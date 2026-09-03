use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player};
use std::fmt;
use std::fs::File;
use std::io::{Cursor, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;

const DEFAULT_ALARM_SOUND_BYTES: &[u8] = include_bytes!("../sounds/universfield-alarm.mp3");
const ALARM_SOUND_FADE_IN_DURATION: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SoundPlaybackSettings {
    pub volume: f32,
    pub fade_in: bool,
}

impl SoundPlaybackSettings {
    pub fn from_percent(volume_percent: u8, fade_in: bool) -> Self {
        Self {
            volume: f32::from(volume_percent.min(100)) / 100.0,
            fade_in,
        }
    }
}

impl Default for SoundPlaybackSettings {
    fn default() -> Self {
        Self::from_percent(100, false)
    }
}

pub trait SoundPlayer: Send {
    fn play(
        &self,
        sound_path: Option<PathBuf>,
        settings: SoundPlaybackSettings,
    ) -> Result<Option<SoundHandle>, SoundError>;
}

pub struct SoundHandle {
    stop: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    finished: Arc<AtomicBool>,
}

impl SoundHandle {
    pub fn new(stop: impl FnOnce() + Send + 'static, finished: Arc<AtomicBool>) -> Self {
        Self {
            stop: Mutex::new(Some(Box::new(stop))),
            finished,
        }
    }

    pub fn stop(&self) {
        if let Ok(mut stop) = self.stop.lock()
            && let Some(stop) = stop.take()
        {
            stop();
        }
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Relaxed)
    }
}

impl Drop for SoundHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

impl fmt::Debug for SoundHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SoundHandle")
            .field("finished", &self.is_finished())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Default)]
pub struct AlarmSoundPlayer;

impl SoundPlayer for AlarmSoundPlayer {
    fn play(
        &self,
        sound_path: Option<PathBuf>,
        settings: SoundPlaybackSettings,
    ) -> Result<Option<SoundHandle>, SoundError> {
        let Some(path) = sound_path else {
            return Ok(None);
        };

        if is_default_sound_path(&path) {
            return Ok(Some(play_default_sound(settings)?));
        }

        let file = File::open(&path).map_err(|source| SoundError::Open {
            path: path.clone(),
            source,
        })?;

        Ok(Some(play_file(file, settings)?))
    }
}

fn play_file(file: File, settings: SoundPlaybackSettings) -> Result<SoundHandle, SoundError> {
    let source =
        Decoder::try_from(file).map_err(|source| SoundError::Decode(source.to_string()))?;
    play_source(source, settings)
}

fn play_default_sound(settings: SoundPlaybackSettings) -> Result<SoundHandle, SoundError> {
    let source = Decoder::try_from(Cursor::new(DEFAULT_ALARM_SOUND_BYTES))
        .map_err(|source| SoundError::Decode(source.to_string()))?;
    play_source(source, settings)
}

fn play_source<R>(
    source: Decoder<R>,
    settings: SoundPlaybackSettings,
) -> Result<SoundHandle, SoundError>
where
    R: Read + Seek + Send + Sync + 'static,
{
    let mut stream = DeviceSinkBuilder::open_default_sink()
        .map_err(|source| SoundError::OutputDevice(source.to_string()))?;
    stream.log_on_drop(false);

    let player = Player::connect_new(stream.mixer());
    let volume = normalized_volume(settings.volume);
    player.set_volume(if settings.fade_in { 0.0 } else { volume });
    player.append(source);
    Ok(spawn_playback_thread(
        player,
        stream,
        volume,
        settings.fade_in,
    ))
}

fn is_default_sound_path(path: &Path) -> bool {
    path == Path::new(crate::storage::DEFAULT_SOUND_PATH)
}

fn spawn_playback_thread(
    player: Player,
    stream: MixerDeviceSink,
    volume: f32,
    fade_in: bool,
) -> SoundHandle {
    let (stop_sender, stop_receiver) = mpsc::channel();
    let finished = Arc::new(AtomicBool::new(false));
    let thread_finished = Arc::clone(&finished);

    thread::spawn(move || {
        let _stream = stream;
        let fade_started_at = Instant::now();
        let mut fade_active = fade_in && volume > 0.0;

        while !player.empty() {
            if stop_receiver.try_recv().is_ok() {
                player.stop();
                break;
            }

            if fade_active {
                let progress = fade_started_at.elapsed().as_secs_f32()
                    / ALARM_SOUND_FADE_IN_DURATION.as_secs_f32();

                if progress >= 1.0 {
                    player.set_volume(volume);
                    fade_active = false;
                } else {
                    player.set_volume(volume * progress);
                }
            }

            thread::sleep(Duration::from_millis(50));
        }

        thread_finished.store(true, Ordering::Relaxed);
    });

    SoundHandle::new(
        move || {
            let _ = stop_sender.send(());
        },
        finished,
    )
}

fn normalized_volume(volume: f32) -> f32 {
    if volume.is_finite() {
        volume.clamp(0.0, 1.0)
    } else {
        1.0
    }
}

pub fn sound_path_from_text(value: &str) -> Option<PathBuf> {
    let trimmed = value.trim();

    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

#[derive(Debug, Error)]
pub enum SoundError {
    #[error("could not open alarm sound file {path:?}: {source}")]
    Open {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not open default audio output: {0}")]
    OutputDevice(String),
    #[error("could not decode alarm sound file: {0}")]
    Decode(String),
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    pub struct RecordingSoundPlayer {
        paths: Arc<Mutex<Vec<Option<PathBuf>>>>,
        settings: Arc<Mutex<Vec<SoundPlaybackSettings>>>,
        stopped_paths: Arc<Mutex<Vec<PathBuf>>>,
    }

    impl RecordingSoundPlayer {
        pub fn paths(&self) -> Vec<Option<PathBuf>> {
            self.paths.lock().expect("paths lock").clone()
        }

        pub fn stopped_paths(&self) -> Vec<PathBuf> {
            self.stopped_paths
                .lock()
                .expect("stopped paths lock")
                .clone()
        }

        pub fn settings(&self) -> Vec<SoundPlaybackSettings> {
            self.settings.lock().expect("settings lock").clone()
        }
    }

    impl SoundPlayer for RecordingSoundPlayer {
        fn play(
            &self,
            sound_path: Option<PathBuf>,
            settings: SoundPlaybackSettings,
        ) -> Result<Option<SoundHandle>, SoundError> {
            self.paths
                .lock()
                .expect("paths lock")
                .push(sound_path.clone());
            self.settings.lock().expect("settings lock").push(settings);

            let Some(sound_path) = sound_path else {
                return Ok(None);
            };

            let stopped_paths = Arc::clone(&self.stopped_paths);
            Ok(Some(SoundHandle::new(
                move || {
                    stopped_paths
                        .lock()
                        .expect("stopped paths lock")
                        .push(sound_path);
                },
                Arc::new(AtomicBool::new(false)),
            )))
        }
    }

    #[test]
    fn blank_sound_path_disables_alarm_sound() {
        assert_eq!(sound_path_from_text("   "), None);
    }

    #[test]
    fn sound_path_keeps_entered_path() {
        assert_eq!(
            sound_path_from_text("C:/sounds/alarm.mp3"),
            Some(Path::new("C:/sounds/alarm.mp3").to_path_buf())
        );
    }

    #[test]
    fn bundled_default_sound_path_is_recognized() {
        assert!(is_default_sound_path(Path::new(
            crate::storage::DEFAULT_SOUND_PATH
        )));
        assert!(!is_default_sound_path(Path::new("C:/sounds/alarm.mp3")));
    }

    #[test]
    fn playback_settings_use_percent_volume() {
        assert_eq!(
            SoundPlaybackSettings::from_percent(65, true),
            SoundPlaybackSettings {
                volume: 0.65,
                fade_in: true,
            }
        );
        assert_eq!(SoundPlaybackSettings::from_percent(150, false).volume, 1.0);
    }

    #[test]
    fn normalized_volume_rejects_invalid_values() {
        assert_eq!(normalized_volume(-0.5), 0.0);
        assert_eq!(normalized_volume(0.4), 0.4);
        assert_eq!(normalized_volume(2.0), 1.0);
        assert_eq!(normalized_volume(f32::NAN), 1.0);
    }
}
