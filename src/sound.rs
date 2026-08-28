use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player};
use std::fmt;
use std::fs::File;
use std::io::{Cursor, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;
use thiserror::Error;

const DEFAULT_ALARM_SOUND_BYTES: &[u8] = include_bytes!("../sounds/universfield-alarm.mp3");

pub trait SoundPlayer: Send {
    fn play(&self, sound_path: Option<PathBuf>) -> Result<Option<SoundHandle>, SoundError>;
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
    fn play(&self, sound_path: Option<PathBuf>) -> Result<Option<SoundHandle>, SoundError> {
        let Some(path) = sound_path else {
            return Ok(None);
        };

        if is_default_sound_path(&path) {
            return Ok(Some(play_default_sound()?));
        }

        let file = File::open(&path).map_err(|source| SoundError::Open {
            path: path.clone(),
            source,
        })?;

        Ok(Some(play_file(file)?))
    }
}

fn play_file(file: File) -> Result<SoundHandle, SoundError> {
    let source =
        Decoder::try_from(file).map_err(|source| SoundError::Decode(source.to_string()))?;
    play_source(source)
}

fn play_default_sound() -> Result<SoundHandle, SoundError> {
    let source = Decoder::try_from(Cursor::new(DEFAULT_ALARM_SOUND_BYTES))
        .map_err(|source| SoundError::Decode(source.to_string()))?;
    play_source(source)
}

fn play_source<R>(source: Decoder<R>) -> Result<SoundHandle, SoundError>
where
    R: Read + Seek + Send + Sync + 'static,
{
    let mut stream = DeviceSinkBuilder::open_default_sink()
        .map_err(|source| SoundError::OutputDevice(source.to_string()))?;
    stream.log_on_drop(false);

    let player = Player::connect_new(stream.mixer());
    player.append(source);
    Ok(spawn_playback_thread(player, stream))
}

fn is_default_sound_path(path: &Path) -> bool {
    path == Path::new(crate::storage::DEFAULT_SOUND_PATH)
}

fn spawn_playback_thread(player: Player, stream: MixerDeviceSink) -> SoundHandle {
    let (stop_sender, stop_receiver) = mpsc::channel();
    let finished = Arc::new(AtomicBool::new(false));
    let thread_finished = Arc::clone(&finished);

    thread::spawn(move || {
        let _stream = stream;

        while !player.empty() {
            if stop_receiver.try_recv().is_ok() {
                player.stop();
                break;
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
    }

    impl SoundPlayer for RecordingSoundPlayer {
        fn play(&self, sound_path: Option<PathBuf>) -> Result<Option<SoundHandle>, SoundError> {
            self.paths
                .lock()
                .expect("paths lock")
                .push(sound_path.clone());

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
}
