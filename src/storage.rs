use crate::alarm::PendingAlarmSnapshot;
use crate::discord_bridge::DiscordBridgeSettings;
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use thiserror::Error;

const STATE_DIR: &str = ".alarm_at_game_end";
const STATE_FILE: &str = "state.json";
const CRASH_LOG_FILE: &str = "crash.log";
pub const DEFAULT_SOUND_PATH: &str = "sounds/universfield-alarm.mp3";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PersistedState {
    pub label: String,
    pub alarm_time: String,
    #[serde(default = "default_delay_for_games")]
    pub delay_for_games: bool,
    pub lockfile_path: String,
    #[serde(default = "default_sound_path")]
    pub sound_path: String,
    #[serde(default = "default_counter_strike_2_gsi_port")]
    pub counter_strike_2_gsi_port: u16,
    #[serde(default = "default_counter_strike_2_gsi_config_path")]
    pub counter_strike_2_gsi_config_path: String,
    #[serde(default)]
    pub logs: Vec<PersistedLogEntry>,
    #[serde(default)]
    pub alarms: Vec<PendingAlarmSnapshot>,
    #[serde(default)]
    pub last_reported_crash_fingerprint: Option<String>,
    #[serde(default)]
    pub theme_preference: PersistedThemePreference,
    #[serde(default)]
    pub graphics_backend_preference: PersistedGraphicsBackendPreference,
    #[serde(default)]
    pub discord_bridge: DiscordBridgeSettings,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PersistedLogLevel {
    Info,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PersistedLogEntry {
    pub occurred_at: DateTime<Local>,
    pub level: PersistedLogLevel,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PersistedThemePreference {
    #[default]
    System,
    Dark,
    Light,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PersistedGraphicsBackendPreference {
    #[default]
    Auto,
    Wgpu,
    OpenGl,
}

impl PersistedGraphicsBackendPreference {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Wgpu => "WGPU",
            Self::OpenGl => "OpenGL",
        }
    }
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            label: String::new(),
            alarm_time: "19:30".to_owned(),
            delay_for_games: default_delay_for_games(),
            lockfile_path: String::new(),
            sound_path: default_sound_path(),
            counter_strike_2_gsi_port: default_counter_strike_2_gsi_port(),
            counter_strike_2_gsi_config_path: default_counter_strike_2_gsi_config_path(),
            logs: Vec::new(),
            alarms: Vec::new(),
            last_reported_crash_fingerprint: None,
            theme_preference: PersistedThemePreference::System,
            graphics_backend_preference: PersistedGraphicsBackendPreference::Auto,
            discord_bridge: DiscordBridgeSettings::default(),
        }
    }
}

fn default_delay_for_games() -> bool {
    true
}

fn default_sound_path() -> String {
    DEFAULT_SOUND_PATH.to_owned()
}

fn default_counter_strike_2_gsi_port() -> u16 {
    crate::counter_strike::DEFAULT_COUNTER_STRIKE_2_GSI_PORT
}

fn default_counter_strike_2_gsi_config_path() -> String {
    crate::counter_strike::default_counter_strike_2_gsi_config_path()
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("could not access local app state")]
    Io(#[from] std::io::Error),
    #[error("could not parse local app state")]
    Json(#[from] serde_json::Error),
}

pub fn state_path() -> PathBuf {
    app_config_dir().join(STATE_FILE)
}

pub fn crash_log_path() -> PathBuf {
    app_config_dir().join(CRASH_LOG_FILE)
}

fn app_config_dir() -> PathBuf {
    user_config_dir().unwrap_or_else(|| local_app_dir(STATE_DIR))
}

fn user_config_dir() -> Option<PathBuf> {
    dirs_next::config_dir().map(|path| path.join(crate::APP_NAME))
}

fn local_app_dir(state_dir: &str) -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(state_dir)
}

pub fn load_state() -> Result<PersistedState, StorageError> {
    let path = state_path();

    if !path.exists() {
        return Ok(PersistedState::default());
    }

    load_state_from_path(path)
}

pub fn save_state(state: &PersistedState) -> Result<(), StorageError> {
    let path = state_path();
    save_state_to_path(state, path)
}

pub fn save_state_to_path(state: &PersistedState, path: PathBuf) -> Result<(), StorageError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let contents = serde_json::to_string_pretty(state)?;
    let temp_path = path.with_extension("json.tmp");

    fs::write(&temp_path, contents)?;
    replace_state_file(&temp_path, &path)?;
    Ok(())
}

pub fn load_state_from_str(contents: &str) -> Result<PersistedState, StorageError> {
    Ok(serde_json::from_str(contents)?)
}

pub fn load_state_from_path(path: PathBuf) -> Result<PersistedState, StorageError> {
    let contents = fs::read_to_string(path)?;
    load_state_from_str(&contents)
}

fn replace_state_file(temp_path: &PathBuf, path: &PathBuf) -> Result<(), StorageError> {
    match fs::rename(temp_path, path) {
        Ok(()) => Ok(()),
        Err(_) if path.exists() => {
            fs::remove_file(path)?;
            fs::rename(temp_path, path).map_err(StorageError::Io)
        }
        Err(error) => Err(StorageError::Io(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_has_alarm_settings() {
        let state = PersistedState::default();

        assert!(state.label.is_empty());
        assert_eq!(state.alarm_time, "19:30");
        assert!(state.delay_for_games);
        assert!(state.lockfile_path.is_empty());
        assert_eq!(state.sound_path, DEFAULT_SOUND_PATH);
        assert_eq!(
            state.counter_strike_2_gsi_port,
            default_counter_strike_2_gsi_port()
        );
        assert!(!state.counter_strike_2_gsi_config_path.is_empty());
        assert!(state.logs.is_empty());
        assert!(state.alarms.is_empty());
        assert_eq!(state.last_reported_crash_fingerprint, None);
        assert_eq!(state.theme_preference, PersistedThemePreference::System);
        assert_eq!(
            state.graphics_backend_preference,
            PersistedGraphicsBackendPreference::Auto
        );
        assert_eq!(state.discord_bridge, DiscordBridgeSettings::default());
    }

    #[test]
    fn saved_state_uses_current_defaults_for_missing_fields() {
        let state = load_state_from_str(
            r#"{
                "label": "Stretch",
                "alarm_time": "20:15",
                "lockfile_path": "C:/Riot Games/League of Legends/lockfile"
            }"#,
        )
        .expect("saved state should parse");

        assert_eq!(state.label, "Stretch");
        assert_eq!(state.alarm_time, "20:15");
        assert!(state.delay_for_games);
        assert_eq!(
            state.lockfile_path,
            "C:/Riot Games/League of Legends/lockfile"
        );
        assert_eq!(state.sound_path, DEFAULT_SOUND_PATH);
        assert_eq!(
            state.counter_strike_2_gsi_port,
            default_counter_strike_2_gsi_port()
        );
        assert!(!state.counter_strike_2_gsi_config_path.is_empty());
        assert!(state.logs.is_empty());
        assert!(state.alarms.is_empty());
        assert_eq!(state.theme_preference, PersistedThemePreference::System);
        assert_eq!(
            state.graphics_backend_preference,
            PersistedGraphicsBackendPreference::Auto
        );
        assert_eq!(state.discord_bridge, DiscordBridgeSettings::default());
    }

    #[test]
    fn saved_state_keeps_theme_preference() {
        let state = load_state_from_str(
            r#"{
                "label": "Stretch",
                "alarm_time": "20:15",
                "delay_for_games": true,
                "lockfile_path": "",
                "theme_preference": "light"
            }"#,
        )
        .expect("saved state should parse");

        assert_eq!(state.theme_preference, PersistedThemePreference::Light);
        assert_eq!(
            state.graphics_backend_preference,
            PersistedGraphicsBackendPreference::Auto
        );
    }

    #[test]
    fn saved_state_keeps_graphics_backend_preference() {
        let state = load_state_from_str(
            r#"{
                "label": "Stretch",
                "alarm_time": "20:15",
                "delay_for_games": true,
                "lockfile_path": "",
                "graphics_backend_preference": "open_gl"
            }"#,
        )
        .expect("saved state should parse");

        assert_eq!(
            state.graphics_backend_preference,
            PersistedGraphicsBackendPreference::OpenGl
        );
    }

    #[test]
    fn saved_state_keeps_explicit_blank_sound_path() {
        let state: PersistedState = serde_json::from_str(
            r#"{
                "label": "Stretch",
                "alarm_time": "20:15",
                "delay_for_games": true,
                "lockfile_path": "",
                "sound_path": ""
            }"#,
        )
        .expect("saved state should parse");

        assert!(state.sound_path.is_empty());
        assert_eq!(
            state.counter_strike_2_gsi_port,
            default_counter_strike_2_gsi_port()
        );
        assert!(!state.counter_strike_2_gsi_config_path.is_empty());
    }

    #[test]
    fn saved_state_keeps_counter_strike_2_gsi_settings() {
        let state: PersistedState = serde_json::from_str(
            r#"{
                "label": "Stretch",
                "alarm_time": "20:15",
                "delay_for_games": true,
                "lockfile_path": "",
                "sound_path": "",
                "counter_strike_2_gsi_port": 31991,
                "counter_strike_2_gsi_config_path": "D:/Steam/steamapps/common/Counter-Strike Global Offensive/game/csgo/cfg/gamestate_integration_alarm_at_game_end.cfg"
            }"#,
        )
        .expect("saved state should parse");

        assert_eq!(state.counter_strike_2_gsi_port, 31991);
        assert_eq!(
            state.counter_strike_2_gsi_config_path,
            "D:/Steam/steamapps/common/Counter-Strike Global Offensive/game/csgo/cfg/gamestate_integration_alarm_at_game_end.cfg"
        );
    }

    #[test]
    fn saved_state_keeps_logs_and_alarms() {
        let scheduled_for = Local::now() + chrono::Duration::hours(1);
        let mut state = PersistedState::default();
        state.logs.push(PersistedLogEntry {
            occurred_at: Local::now(),
            level: PersistedLogLevel::Info,
            message: "Ready".to_owned(),
        });
        state.alarms.push(PendingAlarmSnapshot {
            id: 42,
            alarm: crate::alarm::Alarm::new("Stretch", scheduled_for, true),
            status: crate::alarm::AlarmStatus::Scheduled,
            delayed_by_game: false,
        });

        let contents = serde_json::to_string(&state).expect("state serializes");
        let reloaded = load_state_from_str(&contents).expect("state deserializes");

        assert_eq!(reloaded.logs, state.logs);
        assert_eq!(reloaded.alarms, state.alarms);
    }

    #[test]
    fn save_state_to_path_writes_reloadable_state() {
        let path = unique_test_state_path();
        let mut state = PersistedState::default();
        state.label = "Saved label".to_owned();
        state.logs.push(PersistedLogEntry {
            occurred_at: Local::now(),
            level: PersistedLogLevel::Error,
            message: "Example error".to_owned(),
        });

        save_state_to_path(&state, path.clone()).expect("state should save");
        let reloaded = load_state_from_path(path).expect("state should reload");

        assert_eq!(reloaded.label, "Saved label");
        assert_eq!(reloaded.logs, state.logs);
    }

    fn unique_test_state_path() -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos();

        std::env::temp_dir()
            .join(format!("alarm_at_game_end_storage_test_{unique}"))
            .join(STATE_FILE)
    }
}
