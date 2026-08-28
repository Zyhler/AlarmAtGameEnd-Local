use crate::alarm::{Alarm, AlarmEngine, AlarmId, AlarmSnapshot, PendingAlarmSnapshot};
use crate::game::{GameDetectionConfig, GameMonitor, GameMonitorStatus};
use crate::notifier::{DesktopNotifier, Notifier};
use crate::sound::{AlarmSoundPlayer, SoundHandle, SoundPlayer};
use chrono::{DateTime, Local};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Clone, Debug)]
pub enum MonitorCommand {
    Schedule { id: AlarmId, alarm: Alarm },
    CancelAlarm(AlarmId),
    Cancel,
    SetGameDetectionConfig(GameDetectionConfig),
    TestSound(Option<PathBuf>),
    TestAlarmPopup(Option<PathBuf>),
    Shutdown,
}

#[derive(Debug)]
pub enum MonitorEvent {
    Snapshot(MonitorSnapshot),
    AlarmFired {
        id: AlarmId,
        reminder: String,
        delayed_by_game: bool,
        fired_at: DateTime<Local>,
        sound: Option<SoundHandle>,
    },
    TestSoundStarted {
        sound: Option<SoundHandle>,
        started_at: DateTime<Local>,
    },
    TestAlarmPopup {
        reminder: String,
        fired_at: DateTime<Local>,
        sound: Option<SoundHandle>,
    },
    NotificationError(String),
    SoundError(String),
}

#[derive(Clone, Debug)]
pub struct MonitorSnapshot {
    pub alarm: AlarmSnapshot,
    pub game_status: GameMonitorStatus,
    pub last_checked: DateTime<Local>,
}

impl Default for MonitorSnapshot {
    fn default() -> Self {
        Self {
            alarm: AlarmEngine::default().snapshot(),
            game_status: GameMonitorStatus::default(),
            last_checked: Local::now(),
        }
    }
}

pub struct MonitorHandle {
    commands: Sender<MonitorCommand>,
    events: Receiver<MonitorEvent>,
    join_handle: Option<JoinHandle<()>>,
}

impl MonitorHandle {
    pub fn spawn(
        game_detection_config: GameDetectionConfig,
        restored_alarms: Vec<PendingAlarmSnapshot>,
    ) -> Self {
        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let join_handle = thread::spawn(move || {
            run_monitor_loop(
                command_receiver,
                event_sender,
                restored_alarms,
                GameMonitor::default_with_config(game_detection_config),
                Box::new(DesktopNotifier),
                Box::new(AlarmSoundPlayer),
            );
        });

        Self {
            commands: command_sender,
            events: event_receiver,
            join_handle: Some(join_handle),
        }
    }

    pub fn send(&self, command: MonitorCommand) {
        let _ = self.commands.send(command);
    }

    pub fn drain_events(&self) -> Vec<MonitorEvent> {
        let mut events = Vec::new();

        while let Ok(event) = self.events.try_recv() {
            events.push(event);
        }

        events
    }
}

impl Drop for MonitorHandle {
    fn drop(&mut self) {
        let _ = self.commands.send(MonitorCommand::Shutdown);

        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

fn run_monitor_loop(
    commands: Receiver<MonitorCommand>,
    events: Sender<MonitorEvent>,
    restored_alarms: Vec<PendingAlarmSnapshot>,
    mut game_monitor: GameMonitor,
    notifier: Box<dyn Notifier>,
    sound_player: Box<dyn SoundPlayer>,
) {
    let mut engine = AlarmEngine::from_pending_alarms(restored_alarms);

    loop {
        while let Ok(command) = commands.try_recv() {
            match command {
                MonitorCommand::Schedule { id, alarm } => {
                    engine.schedule_with_id(id, alarm);
                }
                MonitorCommand::CancelAlarm(id) => {
                    engine.cancel_alarm(id);
                }
                MonitorCommand::Cancel => engine.cancel(),
                MonitorCommand::SetGameDetectionConfig(config) => {
                    game_monitor.apply_config(&config)
                }
                MonitorCommand::TestSound(path) => match sound_player.play(path) {
                    Ok(sound) => {
                        let _ = events.send(MonitorEvent::TestSoundStarted {
                            sound,
                            started_at: Local::now(),
                        });
                    }
                    Err(error) => {
                        let _ = events.send(MonitorEvent::SoundError(error.to_string()));
                    }
                },
                MonitorCommand::TestAlarmPopup(path) => {
                    let fired_at = Local::now();
                    let sound = play_alarm_sound(path, events.clone(), sound_player.as_ref());
                    let _ = events.send(MonitorEvent::TestAlarmPopup {
                        reminder: "Test alarm notification".to_owned(),
                        fired_at,
                        sound,
                    });
                }
                MonitorCommand::Shutdown => return,
            }
        }

        let now = Local::now();
        let game_status = game_monitor.status();

        for event in engine.tick(now, game_status.activity()) {
            if let Err(error) = notifier.alarm(&event.reminder) {
                let _ = events.send(MonitorEvent::NotificationError(error.to_string()));
            }

            let sound = play_alarm_sound(
                event.sound_path.clone(),
                events.clone(),
                sound_player.as_ref(),
            );

            let _ = events.send(MonitorEvent::AlarmFired {
                id: event.id,
                reminder: event.reminder,
                delayed_by_game: event.delayed_by_game,
                fired_at: event.fired_at,
                sound,
            });
        }

        let _ = events.send(MonitorEvent::Snapshot(MonitorSnapshot {
            alarm: engine.snapshot(),
            game_status,
            last_checked: now,
        }));

        thread::sleep(Duration::from_millis(750));
    }
}

fn play_alarm_sound(
    sound_path: Option<PathBuf>,
    events: Sender<MonitorEvent>,
    sound_player: &dyn SoundPlayer,
) -> Option<SoundHandle> {
    match sound_player.play(sound_path) {
        Ok(sound) => sound,
        Err(error) => {
            let _ = events.send(MonitorEvent::SoundError(error.to_string()));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sound::tests::RecordingSoundPlayer;

    #[test]
    fn play_alarm_sound_forwards_configured_path() {
        let player = RecordingSoundPlayer::default();
        let (events, _received_events) = mpsc::channel();
        let sound_path = PathBuf::from("C:/sounds/alarm.mp3");

        let sound = play_alarm_sound(Some(sound_path.clone()), events, &player);

        assert_eq!(player.paths(), vec![Some(sound_path)]);
        assert!(sound.is_some());
    }

    #[test]
    fn returned_sound_handle_stops_playback() {
        let player = RecordingSoundPlayer::default();
        let (events, _received_events) = mpsc::channel();
        let sound_path = PathBuf::from("C:/sounds/alarm.mp3");

        let sound =
            play_alarm_sound(Some(sound_path.clone()), events, &player).expect("sound handle");
        sound.stop();

        assert_eq!(player.stopped_paths(), vec![sound_path]);
    }
}
