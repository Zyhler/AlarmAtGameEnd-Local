use crate::game::{GameDetectionConfig, GameDetector, GameStatus};
use serde_json::Value;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use thiserror::Error;

pub const COUNTER_STRIKE_2_GAME_NAME: &str = "Counter-Strike 2";
pub const DEFAULT_COUNTER_STRIKE_2_GSI_PORT: u16 = 31990;
pub const COUNTER_STRIKE_2_GSI_CONFIG_FILENAME: &str =
    "gamestate_integration_alarm_at_game_end.cfg";

const GSI_STALE_AFTER: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Cs2GsiConfigStatus {
    BlankPath,
    Missing,
    Installed,
    NeedsUpdate,
    Unreadable,
}

impl Cs2GsiConfigStatus {
    pub fn needs_install_action(self) -> bool {
        matches!(
            self,
            Self::BlankPath | Self::Missing | Self::NeedsUpdate | Self::Unreadable
        )
    }

    pub fn install_button_label(self) -> &'static str {
        match self {
            Self::Installed => "Install CS2 GSI Config",
            Self::NeedsUpdate => "Update CS2 GSI Config",
            Self::BlankPath | Self::Missing | Self::Unreadable => "Install CS2 GSI Config",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::BlankPath => "Choose a CS2 GSI config path before installing.",
            Self::Missing => {
                "Install once and restart CS2 so alarms only wait during active matches."
            }
            Self::Installed => "CS2 GSI config is installed.",
            Self::NeedsUpdate => "Update the CS2 GSI config after changing the port or path.",
            Self::Unreadable => "The CS2 GSI config could not be checked.",
        }
    }
}

pub struct CounterStrike2Detector {
    port: u16,
    server: Result<Cs2GsiServer, String>,
}

impl CounterStrike2Detector {
    pub fn new(port: u16) -> Self {
        let port = normalize_gsi_port(port);

        Self {
            port,
            server: Cs2GsiServer::spawn(port).map_err(|error| error.to_string()),
        }
    }
}

impl GameDetector for CounterStrike2Detector {
    fn status(&mut self) -> GameStatus {
        match &self.server {
            Ok(server) => server.status(),
            Err(error) => GameStatus::unavailable(COUNTER_STRIKE_2_GAME_NAME, error.clone()),
        }
    }

    fn apply_config(&mut self, config: &GameDetectionConfig) {
        let port = normalize_gsi_port(config.counter_strike_2_gsi_port);

        if self.port == port {
            return;
        }

        self.port = port;
        self.server = Cs2GsiServer::spawn(port).map_err(|error| error.to_string());
    }
}

pub fn normalize_gsi_port(port: u16) -> u16 {
    if port == 0 {
        DEFAULT_COUNTER_STRIKE_2_GSI_PORT
    } else {
        port
    }
}

pub fn default_counter_strike_2_gsi_config_path() -> String {
    default_counter_strike_2_cfg_dir()
        .map(|path| path.join(COUNTER_STRIKE_2_GSI_CONFIG_FILENAME))
        .unwrap_or_else(|| PathBuf::from(COUNTER_STRIKE_2_GSI_CONFIG_FILENAME))
        .to_string_lossy()
        .into_owned()
}

pub fn install_counter_strike_2_gsi_config(path: &Path, port: u16) -> Result<(), Cs2GsiError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(path, gsi_config_contents(normalize_gsi_port(port)))?;
    Ok(())
}

pub fn counter_strike_2_gsi_config_status(path: Option<&Path>, port: u16) -> Cs2GsiConfigStatus {
    let Some(path) = path else {
        return Cs2GsiConfigStatus::BlankPath;
    };

    match fs::read_to_string(path) {
        Ok(contents) if gsi_config_matches_current_settings(&contents, port) => {
            Cs2GsiConfigStatus::Installed
        }
        Ok(_) => Cs2GsiConfigStatus::NeedsUpdate,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Cs2GsiConfigStatus::Missing,
        Err(_) => Cs2GsiConfigStatus::Unreadable,
    }
}

fn default_counter_strike_2_cfg_dir() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        for env_key in ["ProgramFiles(x86)", "ProgramFiles"] {
            if let Ok(root) = std::env::var(env_key) {
                return Some(
                    Path::new(&root)
                        .join("Steam")
                        .join("steamapps")
                        .join("common")
                        .join("Counter-Strike Global Offensive")
                        .join("game")
                        .join("csgo")
                        .join("cfg"),
                );
            }
        }
    } else if let Some(home) = dirs_next::home_dir() {
        return Some(
            home.join(".steam")
                .join("steam")
                .join("steamapps")
                .join("common")
                .join("Counter-Strike Global Offensive")
                .join("game")
                .join("csgo")
                .join("cfg"),
        );
    }

    None
}

fn gsi_config_contents(port: u16) -> String {
    format!(
        r#""Alarm at Game End"
{{
    "uri" "http://127.0.0.1:{port}/cs2"
    "timeout" "5.0"
    "buffer" "0.5"
    "throttle" "1.0"
    "heartbeat" "5.0"
    "data"
    {{
        "provider" "1"
        "map" "1"
    }}
}}
"#
    )
}

fn gsi_config_matches_current_settings(contents: &str, port: u16) -> bool {
    contents.contains(&format!(
        r#""uri" "http://127.0.0.1:{}/cs2""#,
        normalize_gsi_port(port)
    )) && contents.contains(r#""map" "1""#)
}

struct Cs2GsiServer {
    port: u16,
    state: Arc<Mutex<Cs2GsiSnapshot>>,
    stop: Sender<()>,
    join_handle: Option<JoinHandle<()>>,
}

impl Cs2GsiServer {
    fn spawn(port: u16) -> Result<Self, Cs2GsiError> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        listener.set_nonblocking(true)?;

        let state = Arc::new(Mutex::new(Cs2GsiSnapshot::default()));
        let thread_state = Arc::clone(&state);
        let (stop, stop_receiver) = mpsc::channel();

        let join_handle = thread::spawn(move || {
            loop {
                if stop_receiver.try_recv().is_ok() {
                    break;
                }

                match listener.accept() {
                    Ok((mut stream, _address)) => {
                        handle_gsi_connection(&mut stream, &thread_state);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(50));
                    }
                    Err(_error) => {
                        thread::sleep(Duration::from_millis(250));
                    }
                }
            }
        });

        Ok(Self {
            port,
            state,
            stop,
            join_handle: Some(join_handle),
        })
    }

    fn status(&self) -> GameStatus {
        let snapshot = self.state.lock().expect("cs2 gsi state lock").clone();

        status_from_gsi_snapshot(self.port, snapshot)
    }
}

impl Drop for Cs2GsiServer {
    fn drop(&mut self) {
        let _ = self.stop.send(());

        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Cs2GsiSnapshot {
    last_payload_at: Option<Instant>,
    map_present: bool,
    map_name: Option<String>,
    map_phase: Option<String>,
}

fn status_from_gsi_snapshot(port: u16, snapshot: Cs2GsiSnapshot) -> GameStatus {
    let Some(last_payload_at) = snapshot.last_payload_at else {
        return GameStatus::inactive(
            COUNTER_STRIKE_2_GAME_NAME,
            Some(format!("waiting for GSI updates on 127.0.0.1:{port}")),
        );
    };

    if last_payload_at.elapsed() > GSI_STALE_AFTER {
        return GameStatus::inactive(
            COUNTER_STRIKE_2_GAME_NAME,
            Some("no recent GSI updates".to_owned()),
        );
    }

    if !snapshot.map_present {
        return GameStatus::inactive(
            COUNTER_STRIKE_2_GAME_NAME,
            Some("client open, not connected to a server".to_owned()),
        );
    }

    if snapshot
        .map_phase
        .as_deref()
        .is_some_and(|phase| phase.eq_ignore_ascii_case("gameover"))
    {
        return GameStatus::inactive(
            COUNTER_STRIKE_2_GAME_NAME,
            Some(map_detail(&snapshot, "match ended")),
        );
    }

    GameStatus::active(
        COUNTER_STRIKE_2_GAME_NAME,
        Some(map_detail(&snapshot, "server connected")),
    )
}

fn map_detail(snapshot: &Cs2GsiSnapshot, fallback: &str) -> String {
    match (&snapshot.map_name, &snapshot.map_phase) {
        (Some(name), Some(phase)) => format!("{name} ({phase})"),
        (Some(name), None) => name.clone(),
        (None, Some(phase)) => phase.clone(),
        (None, None) => fallback.to_owned(),
    }
}

fn handle_gsi_connection(stream: &mut TcpStream, state: &Arc<Mutex<Cs2GsiSnapshot>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));

    if let Ok(Some(body)) = read_http_body(stream) {
        update_gsi_state(&body, state);
    }

    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
}

fn read_http_body(stream: &mut TcpStream) -> Result<Option<Vec<u8>>, std::io::Error> {
    let mut request = Vec::new();
    let mut buffer = [0; 4096];
    let mut header_end = None;
    let mut content_length = None;

    loop {
        let bytes_read = stream.read(&mut buffer)?;

        if bytes_read == 0 {
            break;
        }

        request.extend_from_slice(&buffer[..bytes_read]);

        if request.len() > 128 * 1024 {
            return Ok(None);
        }

        if header_end.is_none() {
            header_end = find_header_end(&request);

            if let Some(end) = header_end {
                content_length = parse_content_length(&request[..end]);
            }
        }

        if let (Some(end), Some(length)) = (header_end, content_length) {
            let body_start = end + 4;

            if request.len() >= body_start + length {
                return Ok(Some(request[body_start..body_start + length].to_vec()));
            }
        }
    }

    Ok(None)
}

fn find_header_end(request: &[u8]) -> Option<usize> {
    request.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_content_length(headers: &[u8]) -> Option<usize> {
    String::from_utf8_lossy(headers).lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;

        if name.trim().eq_ignore_ascii_case("content-length") {
            value.trim().parse().ok()
        } else {
            None
        }
    })
}

fn update_gsi_state(body: &[u8], state: &Arc<Mutex<Cs2GsiSnapshot>>) {
    let Ok(payload) = serde_json::from_slice::<Value>(body) else {
        return;
    };

    let map = payload.get("map").and_then(Value::as_object);
    let snapshot = Cs2GsiSnapshot {
        last_payload_at: Some(Instant::now()),
        map_present: map.is_some(),
        map_name: map
            .and_then(|map| map.get("name"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        map_phase: map
            .and_then(|map| map.get("phase"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
    };

    *state.lock().expect("cs2 gsi state lock") = snapshot;
}

#[derive(Debug, Error)]
pub enum Cs2GsiError {
    #[error("could not access Counter-Strike 2 GSI config")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_gsi_payload_is_inactive() {
        let status = status_from_gsi_snapshot(31990, Cs2GsiSnapshot::default());

        assert_eq!(
            status,
            GameStatus::inactive(
                COUNTER_STRIKE_2_GAME_NAME,
                Some("waiting for GSI updates on 127.0.0.1:31990".to_owned())
            )
        );
    }

    #[test]
    fn payload_without_map_is_inactive() {
        let status = status_from_gsi_snapshot(
            31990,
            Cs2GsiSnapshot {
                last_payload_at: Some(Instant::now()),
                ..Default::default()
            },
        );

        assert_eq!(
            status,
            GameStatus::inactive(
                COUNTER_STRIKE_2_GAME_NAME,
                Some("client open, not connected to a server".to_owned())
            )
        );
    }

    #[test]
    fn live_map_payload_is_active() {
        let status = status_from_gsi_snapshot(
            31990,
            Cs2GsiSnapshot {
                last_payload_at: Some(Instant::now()),
                map_present: true,
                map_name: Some("de_dust2".to_owned()),
                map_phase: Some("live".to_owned()),
            },
        );

        assert_eq!(
            status,
            GameStatus::active(
                COUNTER_STRIKE_2_GAME_NAME,
                Some("de_dust2 (live)".to_owned())
            )
        );
    }

    #[test]
    fn gameover_map_payload_is_inactive() {
        let status = status_from_gsi_snapshot(
            31990,
            Cs2GsiSnapshot {
                last_payload_at: Some(Instant::now()),
                map_present: true,
                map_name: Some("de_nuke".to_owned()),
                map_phase: Some("gameover".to_owned()),
            },
        );

        assert_eq!(
            status,
            GameStatus::inactive(
                COUNTER_STRIKE_2_GAME_NAME,
                Some("de_nuke (gameover)".to_owned())
            )
        );
    }

    #[test]
    fn gsi_payload_updates_snapshot_from_map_data() {
        let state = Arc::new(Mutex::new(Cs2GsiSnapshot::default()));

        update_gsi_state(
            br#"{"provider":{"name":"Counter-Strike: Global Offensive"},"map":{"name":"de_mirage","phase":"warmup"}}"#,
            &state,
        );

        let snapshot = state.lock().expect("cs2 gsi state lock").clone();

        assert!(snapshot.last_payload_at.is_some());
        assert!(snapshot.map_present);
        assert_eq!(snapshot.map_name, Some("de_mirage".to_owned()));
        assert_eq!(snapshot.map_phase, Some("warmup".to_owned()));
    }

    #[test]
    fn gsi_config_points_to_local_listener() {
        let config = gsi_config_contents(31991);

        assert!(config.contains(r#""uri" "http://127.0.0.1:31991/cs2""#));
        assert!(config.contains(r#""map" "1""#));
    }

    #[test]
    fn gsi_config_match_requires_current_port() {
        let config = gsi_config_contents(31991);

        assert!(gsi_config_matches_current_settings(&config, 31991));
        assert!(!gsi_config_matches_current_settings(&config, 31992));
    }
}
