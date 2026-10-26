use crate::alarm::{Alarm, AlarmEngine, AlarmId, AlarmSnapshot, PendingAlarmSnapshot};
use crate::game::{GameDetectionConfig, GameMonitor, GameMonitorStatus};
use crate::notifier::{DesktopNotifier, Notifier};
use crate::sound::{AlarmSoundPlayer, SoundHandle, SoundPlaybackSettings, SoundPlayer};
use chrono::{DateTime, Local};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Clone, Debug)]
pub enum MonitorCommand {
    Schedule {
        id: AlarmId,
        alarm: Alarm,
        revision: u64,
    },
    CancelAlarm {
        id: AlarmId,
        revision: u64,
    },
    Cancel {
        revision: u64,
    },
    SetGameDetectionConfig(GameDetectionConfig),
    SetSoundPlaybackSettings(SoundPlaybackSettings),
    TestSound(Option<PathBuf>),
    TestAlarmPopup(Option<PathBuf>),
    Shutdown,
}

#[derive(Debug)]
pub enum MonitorEvent {
    Snapshot(MonitorSnapshot),
    AlarmFired {
        id: AlarmId,
        label: String,
        delayed_by_game: bool,
        fired_at: DateTime<Local>,
        sound: Option<SoundHandle>,
    },
    TestSoundStarted {
        sound: Option<SoundHandle>,
        started_at: DateTime<Local>,
    },
    TestAlarmPopup {
        label: String,
        fired_at: DateTime<Local>,
        sound: Option<SoundHandle>,
    },
    NotificationError(String),
    SoundError(String),
}

#[derive(Clone, Debug)]
pub struct MonitorSnapshot {
    pub alarm: AlarmSnapshot,
    /// The last UI alarm edit processed before this snapshot was taken.
    pub applied_alarm_revision: u64,
    pub game_status: GameMonitorStatus,
    pub last_checked: DateTime<Local>,
}

impl Default for MonitorSnapshot {
    fn default() -> Self {
        Self {
            alarm: AlarmEngine::default().snapshot(),
            applied_alarm_revision: 0,
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
        sound_settings: SoundPlaybackSettings,
        restored_alarms: Vec<PendingAlarmSnapshot>,
    ) -> Self {
        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let join_handle = thread::spawn(move || {
            run_monitor_loop(
                command_receiver,
                event_sender,
                restored_alarms,
                sound_settings,
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

    #[cfg(test)]
    pub(crate) fn test_channels() -> (Self, Sender<MonitorEvent>, Receiver<MonitorCommand>) {
        let (commands, received_commands) = mpsc::channel();
        let (sent_events, events) = mpsc::channel();
        (
            Self {
                commands,
                events,
                join_handle: None,
            },
            sent_events,
            received_commands,
        )
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
    mut sound_settings: SoundPlaybackSettings,
    mut game_monitor: GameMonitor,
    notifier: Box<dyn Notifier>,
    sound_player: Box<dyn SoundPlayer>,
) {
    let mut engine = AlarmEngine::from_pending_alarms(restored_alarms);
    let mut applied_alarm_revision = 0;

    loop {
        while let Ok(command) = commands.try_recv() {
            match command {
                MonitorCommand::Schedule {
                    id,
                    alarm,
                    revision,
                } => {
                    engine.schedule_with_id(id, alarm);
                    applied_alarm_revision = revision;
                }
                MonitorCommand::CancelAlarm { id, revision } => {
                    engine.cancel_alarm(id);
                    applied_alarm_revision = revision;
                }
                MonitorCommand::Cancel { revision } => {
                    engine.cancel();
                    applied_alarm_revision = revision;
                }
                MonitorCommand::SetGameDetectionConfig(config) => {
                    game_monitor.apply_config(&config)
                }
                MonitorCommand::SetSoundPlaybackSettings(settings) => {
                    sound_settings = settings;
                }
                MonitorCommand::TestSound(path) => match sound_player.play(path, sound_settings) {
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
                    let sound =
                        play_alarm_sound(path, sound_settings, &events, sound_player.as_ref());
                    let _ = events.send(MonitorEvent::TestAlarmPopup {
                        label: "Test alarm notification".to_owned(),
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
            if let Err(error) = notifier.alarm(&event.label) {
                let _ = events.send(MonitorEvent::NotificationError(error.to_string()));
            }

            let sound = play_alarm_sound(
                event.sound_path,
                sound_settings,
                &events,
                sound_player.as_ref(),
            );

            let _ = events.send(MonitorEvent::AlarmFired {
                id: event.id,
                label: event.label,
                delayed_by_game: event.delayed_by_game,
                fired_at: event.fired_at,
                sound,
            });
        }

        let _ = events.send(MonitorEvent::Snapshot(MonitorSnapshot {
            alarm: engine.snapshot(),
            applied_alarm_revision,
            game_status,
            last_checked: now,
        }));

        thread::sleep(Duration::from_millis(750));
    }
}

fn play_alarm_sound(
    sound_path: Option<PathBuf>,
    sound_settings: SoundPlaybackSettings,
    events: &Sender<MonitorEvent>,
    sound_player: &dyn SoundPlayer,
) -> Option<SoundHandle> {
    match sound_player.play(sound_path, sound_settings) {
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
    use crate::notifier::tests::RecordingNotifier;
    use crate::sound::tests::RecordingSoundPlayer;

    #[test]
    fn worker_acknowledges_scheduling_and_both_cancellation_commands() {
        let (commands, command_receiver) = mpsc::channel();
        let (events, event_receiver) = mpsc::channel();
        let thread = thread::spawn(move || {
            run_monitor_loop(
                command_receiver,
                events,
                Vec::new(),
                SoundPlaybackSettings::default(),
                GameMonitor::new(Vec::new()),
                Box::new(RecordingNotifier::default()),
                Box::new(RecordingSoundPlayer::default()),
            )
        });
        let receive_snapshot = |revision| {
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            loop {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                match event_receiver.recv_timeout(remaining) {
                    Ok(MonitorEvent::Snapshot(snapshot))
                        if snapshot.applied_alarm_revision >= revision =>
                    {
                        break Ok(snapshot);
                    }
                    Ok(_) => {}
                    Err(error) => break Err(error),
                }
            }
        };
        let alarm = Alarm::new("Test", Local::now() + chrono::Duration::hours(1), false);
        commands
            .send(MonitorCommand::Schedule {
                id: 1,
                alarm: alarm.clone(),
                revision: 1,
            })
            .expect("worker should accept scheduling");
        let scheduled = receive_snapshot(1);
        commands
            .send(MonitorCommand::CancelAlarm { id: 1, revision: 2 })
            .expect("worker should accept cancellation");
        let canceled = receive_snapshot(2);
        commands
            .send(MonitorCommand::Schedule {
                id: 2,
                alarm,
                revision: 3,
            })
            .expect("worker should accept another alarm");
        commands
            .send(MonitorCommand::Cancel { revision: 4 })
            .expect("worker should accept Cancel All");
        let canceled_all = receive_snapshot(4);
        commands
            .send(MonitorCommand::Shutdown)
            .expect("worker should accept shutdown");
        thread.join().expect("test worker should exit cleanly");

        assert_eq!(
            scheduled
                .expect("schedule must be acknowledged")
                .alarm
                .alarms
                .len(),
            1
        );
        assert!(
            canceled
                .expect("cancellation must be acknowledged")
                .alarm
                .alarms
                .is_empty()
        );
        assert!(
            canceled_all
                .expect("Cancel All must be acknowledged")
                .alarm
                .alarms
                .is_empty()
        );
    }

    #[test]
    fn play_alarm_sound_forwards_configured_path() {
        let player = RecordingSoundPlayer::default();
        let (events, _received_events) = mpsc::channel();
        let sound_path = PathBuf::from("C:/sounds/alarm.mp3");
        let settings = SoundPlaybackSettings::from_percent(65, true);

        let sound = play_alarm_sound(Some(sound_path.clone()), settings, &events, &player);

        assert_eq!(player.paths(), vec![Some(sound_path)]);
        assert_eq!(player.settings(), vec![settings]);
        assert!(sound.is_some());
    }

    #[test]
    fn returned_sound_handle_stops_playback() {
        let player = RecordingSoundPlayer::default();
        let (events, _received_events) = mpsc::channel();
        let sound_path = PathBuf::from("C:/sounds/alarm.mp3");

        let sound = play_alarm_sound(
            Some(sound_path.clone()),
            SoundPlaybackSettings::default(),
            &events,
            &player,
        )
        .expect("sound handle");
        sound.stop();

        assert_eq!(player.stopped_paths(), vec![sound_path]);
    }
}
