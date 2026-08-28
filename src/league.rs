use crate::alarm::GameActivity;
use crate::game::{GameDetectionConfig, GameDetector, GameStatus};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, HeaderValue};
use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;

const GAMEFLOW_ENDPOINT: &str = "/lol-gameflow/v1/gameflow-phase";
const WINDOWS_RIOT_CLIENT_INSTALLS_FILE: &str =
    r"C:\ProgramData\Riot Games\RiotClientInstalls.json";
pub const GAME_NAME: &str = "League of Legends";

#[derive(Clone, PartialEq)]
pub struct Lockfile {
    pub process_name: String,
    pub pid: u32,
    pub port: u16,
    pub password: String,
    pub protocol: String,
}

impl fmt::Debug for Lockfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Lockfile")
            .field("process_name", &self.process_name)
            .field("pid", &self.pid)
            .field("port", &self.port)
            .field("password", &"<redacted>")
            .field("protocol", &self.protocol)
            .finish()
    }
}

#[derive(Debug, Error)]
pub enum LeagueError {
    #[error("could not read League lockfile")]
    Io(#[from] std::io::Error),
    #[error("invalid League lockfile format")]
    InvalidLockfile,
    #[error("invalid League lockfile pid")]
    InvalidPid,
    #[error("invalid League lockfile port")]
    InvalidPort,
    #[error("could not create League HTTP client")]
    Client(#[from] reqwest::Error),
    #[error("League API returned HTTP {0}")]
    HttpStatus(reqwest::StatusCode),
    #[error("could not build League authorization header")]
    InvalidAuthHeader(#[from] reqwest::header::InvalidHeaderValue),
}

pub struct LeagueDetector {
    lockfile_override: Option<PathBuf>,
    client: Client,
}

impl LeagueDetector {
    pub fn new(lockfile_override: Option<PathBuf>) -> Result<Self, LeagueError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .danger_accept_invalid_certs(true)
            .build()?;

        Ok(Self {
            lockfile_override,
            client,
        })
    }

    fn discover_lockfile(&self) -> Option<PathBuf> {
        if let Some(path) = self
            .lockfile_override
            .as_ref()
            .filter(|path| path.exists())
            .cloned()
        {
            return Some(path);
        }

        candidate_lockfile_paths()
            .into_iter()
            .find(|path| path.exists())
    }

    fn read_lockfile(&self) -> Result<Option<Lockfile>, LeagueError> {
        let Some(path) = self.discover_lockfile() else {
            return Ok(None);
        };

        let contents = fs::read_to_string(path)?;
        parse_lockfile(&contents).map(Some)
    }

    fn request_gameflow_phase(&self, lockfile: &Lockfile) -> Result<String, LeagueError> {
        let url = format!(
            "{}://127.0.0.1:{}{}",
            lockfile.protocol, lockfile.port, GAMEFLOW_ENDPOINT
        );
        let credentials = STANDARD.encode(format!("riot:{}", lockfile.password));
        let auth_header = HeaderValue::from_str(&format!("Basic {credentials}"))?;
        let response = self
            .client
            .get(url)
            .header(AUTHORIZATION, auth_header)
            .send()?;

        if !response.status().is_success() {
            return Err(LeagueError::HttpStatus(response.status()));
        }

        Ok(response.json::<String>()?)
    }
}

impl GameDetector for LeagueDetector {
    fn status(&mut self) -> GameStatus {
        match self.read_lockfile() {
            Ok(Some(lockfile)) => match self.request_gameflow_phase(&lockfile) {
                Ok(phase) => status_from_gameflow_phase(phase),
                Err(error) => GameStatus::unavailable(GAME_NAME, error.to_string()),
            },
            Ok(None) => GameStatus::inactive(GAME_NAME, Some("client not running".to_owned())),
            Err(error) => GameStatus::unavailable(GAME_NAME, error.to_string()),
        }
    }

    fn apply_config(&mut self, config: &GameDetectionConfig) {
        self.lockfile_override = config.league_lockfile_override.clone();
    }
}

pub fn parse_lockfile(contents: &str) -> Result<Lockfile, LeagueError> {
    let parts: Vec<&str> = contents.trim().split(':').collect();
    if parts.len() != 5 {
        return Err(LeagueError::InvalidLockfile);
    }

    Ok(Lockfile {
        process_name: parts[0].to_owned(),
        pid: parts[1].parse().map_err(|_| LeagueError::InvalidPid)?,
        port: parts[2].parse().map_err(|_| LeagueError::InvalidPort)?,
        password: parts[3].to_owned(),
        protocol: parts[4].to_owned(),
    })
}

pub fn classify_gameflow_phase(phase: &str) -> GameActivity {
    match phase {
        "ReadyCheck" | "ChampSelect" | "GameStart" | "InProgress" | "Reconnect" => {
            GameActivity::Active
        }
        _ => GameActivity::Inactive,
    }
}

fn status_from_gameflow_phase(phase: String) -> GameStatus {
    match classify_gameflow_phase(&phase) {
        GameActivity::Active => GameStatus::active(GAME_NAME, Some(phase)),
        GameActivity::Inactive | GameActivity::Unavailable => {
            GameStatus::inactive(GAME_NAME, Some(format!("client open, phase {phase}")))
        }
    }
}

pub fn candidate_lockfile_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Ok(path) = env::var("LEAGUE_LOCKFILE") {
        push_candidate_path(&mut paths, PathBuf::from(path));
    }

    if cfg!(target_os = "windows") {
        push_candidate_path(
            &mut paths,
            Path::new(r"C:\")
                .join("Riot Games")
                .join("League of Legends")
                .join("lockfile"),
        );
        push_windows_candidate(&mut paths, "ProgramFiles");
        push_windows_candidate(&mut paths, "ProgramFiles(x86)");

        if let Ok(system_drive) = env::var("SystemDrive") {
            push_candidate_path(
                &mut paths,
                Path::new(&system_drive)
                    .join("Riot Games")
                    .join("League of Legends")
                    .join("lockfile"),
            );
        }

        for path in windows_riot_client_install_lockfile_paths() {
            push_candidate_path(&mut paths, path);
        }
    } else if cfg!(target_os = "macos") {
        push_candidate_path(
            &mut paths,
            PathBuf::from("/Applications/League of Legends.app/Contents/LoL/lockfile"),
        );
    } else if let Some(home) = dirs_next::home_dir() {
        push_candidate_path(
            &mut paths,
            home.join("Games")
                .join("league-of-legends")
                .join("drive_c")
                .join("Riot Games")
                .join("League of Legends")
                .join("lockfile"),
        );
        push_candidate_path(
            &mut paths,
            home.join(".local")
                .join("share")
                .join("league-of-legends")
                .join("lockfile"),
        );
    }

    paths
}

fn push_windows_candidate(paths: &mut Vec<PathBuf>, env_key: &str) {
    if let Ok(root) = env::var(env_key) {
        push_candidate_path(
            paths,
            Path::new(&root)
                .join("Riot Games")
                .join("League of Legends")
                .join("lockfile"),
        );
    }
}

fn push_candidate_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !path.as_os_str().is_empty() && !paths.iter().any(|candidate| candidate == &path) {
        paths.push(path);
    }
}

fn windows_riot_client_install_lockfile_paths() -> Vec<PathBuf> {
    let Ok(contents) = fs::read_to_string(WINDOWS_RIOT_CLIENT_INSTALLS_FILE) else {
        return Vec::new();
    };

    riot_client_install_lockfile_paths_from_json(&contents)
}

fn riot_client_install_lockfile_paths_from_json(contents: &str) -> Vec<PathBuf> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(contents) else {
        return Vec::new();
    };

    let Some(associated_clients) = value
        .get("associated_client")
        .and_then(serde_json::Value::as_object)
    else {
        return Vec::new();
    };

    associated_clients
        .keys()
        .map(|install_path| Path::new(install_path).join("lockfile"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lockfile_parser_accepts_valid_lcu_shape() {
        let lockfile =
            parse_lockfile("LeagueClient:1234:2999:secret:https").expect("valid lockfile");

        assert_eq!(lockfile.process_name, "LeagueClient");
        assert_eq!(lockfile.pid, 1234);
        assert_eq!(lockfile.port, 2999);
        assert_eq!(lockfile.password, "secret");
        assert_eq!(lockfile.protocol, "https");
    }

    #[test]
    fn lockfile_parser_rejects_missing_fields() {
        assert!(matches!(
            parse_lockfile("LeagueClient:1234:2999"),
            Err(LeagueError::InvalidLockfile)
        ));
    }

    #[test]
    fn active_phases_delay_alarm() {
        for phase in [
            "ReadyCheck",
            "ChampSelect",
            "GameStart",
            "InProgress",
            "Reconnect",
        ] {
            assert_eq!(classify_gameflow_phase(phase), GameActivity::Active);
        }
    }

    #[test]
    fn end_phases_do_not_delay_alarm() {
        for phase in [
            "None",
            "Lobby",
            "Matchmaking",
            "WaitingForStats",
            "PreEndOfGame",
            "EndOfGame",
        ] {
            assert_eq!(classify_gameflow_phase(phase), GameActivity::Inactive);
        }
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn candidate_lockfile_paths_include_default_riot_install() {
        assert!(
            candidate_lockfile_paths()
                .contains(&PathBuf::from(r"C:\Riot Games\League of Legends\lockfile"))
        );
    }

    #[test]
    fn riot_client_installs_json_adds_associated_client_lockfiles() {
        let paths = riot_client_install_lockfile_paths_from_json(
            r#"{
                "associated_client": {
                    "D:/Riot Games/League of Legends/": "D:/Riot Games/Riot Client/RiotClientServices.exe"
                },
                "rc_live": "D:/Riot Games/Riot Client/RiotClientServices.exe"
            }"#,
        );

        assert_eq!(
            paths,
            vec![PathBuf::from("D:/Riot Games/League of Legends/lockfile")]
        );
    }
}
