use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Local};
use rand::RngCore;
use reqwest::StatusCode;
use reqwest::blocking::Client;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashSet;
use std::time::Duration;
use thiserror::Error;

pub const POLL_INTERVAL: Duration = Duration::from_secs(5);
pub const DEFAULT_BRIDGE_URL: &str = "https://aagedb.zyhlerservers.ddns.net";

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DiscordBridgeSettings {
    #[serde(
        default = "default_bridge_url",
        deserialize_with = "deserialize_bridge_url"
    )]
    pub bridge_url: String,
    pub companion_id: String,
    pub api_token: String,
    pub pairing_code: String,
    pub pairing_expires_at: Option<DateTime<Local>>,
    pub paired_discord_user_id: Option<String>,
    #[serde(default)]
    pub paired_discord_user_label: Option<String>,
    pub allowed_requester_ids: String,
    #[serde(default)]
    pub allowed_requesters: Vec<AllowedDiscordRequester>,
    pub poll_enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct AllowedDiscordRequester {
    pub discord_user_id: String,
    pub display_name: String,
}

impl Default for DiscordBridgeSettings {
    fn default() -> Self {
        Self {
            bridge_url: default_bridge_url(),
            companion_id: String::new(),
            api_token: String::new(),
            pairing_code: String::new(),
            pairing_expires_at: None,
            paired_discord_user_id: None,
            paired_discord_user_label: None,
            allowed_requester_ids: String::new(),
            allowed_requesters: Vec::new(),
            poll_enabled: false,
        }
    }
}

fn default_bridge_url() -> String {
    DEFAULT_BRIDGE_URL.to_owned()
}

fn deserialize_bridge_url<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let bridge_url = String::deserialize(deserializer)?;

    if bridge_url.trim().is_empty() {
        Ok(default_bridge_url())
    } else {
        Ok(bridge_url)
    }
}

impl DiscordBridgeSettings {
    pub fn client_config(&self) -> Result<BridgeClientConfig, BridgeError> {
        BridgeClientConfig::new(
            self.bridge_url.clone(),
            self.companion_id.clone(),
            self.api_token.clone(),
        )
    }

    pub fn can_poll(&self) -> bool {
        !self.bridge_url.trim().is_empty()
            && !self.companion_id.trim().is_empty()
            && !self.api_token.trim().is_empty()
    }
}

#[derive(Clone, Debug)]
pub struct BridgeClientConfig {
    bridge_url: String,
    companion_id: String,
    api_token: String,
}

impl BridgeClientConfig {
    fn new(
        bridge_url: String,
        companion_id: String,
        api_token: String,
    ) -> Result<Self, BridgeError> {
        let bridge_url = bridge_url.trim().trim_end_matches('/').to_owned();
        let companion_id = companion_id.trim().to_owned();
        let api_token = api_token.trim().to_owned();

        if bridge_url.is_empty() {
            return Err(BridgeError::MissingBridgeUrl);
        }

        if companion_id.is_empty() || api_token.is_empty() {
            return Err(BridgeError::MissingCredentials);
        }

        Ok(Self {
            bridge_url,
            companion_id,
            api_token,
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct PairingRegistrationResponse {
    pub pairing_code: String,
    pub expires_at_unix: i64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PollRequestsResponse {
    pub paired: bool,
    pub paired_discord_user_id: Option<String>,
    #[serde(default)]
    pub paired_discord_user_label: Option<String>,
    pub requests: Vec<RemoteAlarmRequest>,
    #[serde(default)]
    pub invites: Vec<CompanionInvite>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct RemoteAlarmRequest {
    pub id: u64,
    pub requester_id: String,
    #[serde(
        default,
        alias = "requester_display_name",
        alias = "requester_name",
        alias = "requesterDisplayName"
    )]
    pub requester_label: Option<String>,
    pub target_id: String,
    pub hour: u8,
    pub minute: u8,
    pub label: String,
    pub created_at_unix: i64,
    pub expires_at_unix: i64,
}

impl RemoteAlarmRequest {
    pub fn time_text(&self) -> String {
        format!("{:02}.{:02}", self.hour, self.minute)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct CompanionInvite {
    pub id: u64,
    pub owner_id: String,
    pub invitee_id: String,
    #[serde(default)]
    pub invitee_label: String,
    pub created_at_unix: i64,
    pub expires_at_unix: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestStatus {
    Accepted,
    Rejected,
}

impl RequestStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InviteStatus {
    Allowed,
    Rejected,
}

impl InviteStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Debug, Serialize)]
struct PairingRegistrationRequest<'a> {
    companion_id: &'a str,
    api_token: &'a str,
    pairing_code: &'a str,
}

#[derive(Debug, Serialize)]
struct RequestStatusReport<'a> {
    status: &'a str,
    reason: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct InviteStatusReport<'a> {
    status: &'a str,
}

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("bridge URL is blank")]
    MissingBridgeUrl,
    #[error("companion credentials are missing")]
    MissingCredentials,
    #[error("bridge request failed")]
    Request(#[from] reqwest::Error),
    #[error("bridge returned {status}: {body}")]
    HttpStatus { status: StatusCode, body: String },
}

pub fn ensure_credentials(settings: &mut DiscordBridgeSettings) -> bool {
    let mut changed = false;

    if settings.companion_id.trim().is_empty() {
        settings.companion_id = generate_companion_id();
        changed = true;
    }

    if settings.api_token.trim().is_empty() {
        settings.api_token = generate_api_token();
        changed = true;
    }

    changed
}

pub fn generate_pairing_code() -> String {
    let mut bytes = [0_u8; 4];
    rand::thread_rng().fill_bytes(&mut bytes);
    display_pairing_code(u32::from_le_bytes(bytes))
}

pub fn register_pairing(
    config: &BridgeClientConfig,
    pairing_code: &str,
) -> Result<PairingRegistrationResponse, BridgeError> {
    let response = client()?
        .post(format!("{}/bridge/pairings", config.bridge_url))
        .json(&PairingRegistrationRequest {
            companion_id: &config.companion_id,
            api_token: &config.api_token,
            pairing_code,
        })
        .send()?;

    parse_json_response(response)
}

pub fn poll_requests(config: &BridgeClientConfig) -> Result<PollRequestsResponse, BridgeError> {
    let response = client()?
        .get(format!("{}/bridge/requests", config.bridge_url))
        .bearer_auth(&config.api_token)
        .send()?;

    parse_json_response(response)
}

pub fn report_request_status(
    config: &BridgeClientConfig,
    request_id: u64,
    status: RequestStatus,
    reason: Option<&str>,
) -> Result<(), BridgeError> {
    let response = client()?
        .post(format!(
            "{}/bridge/requests/{}/status",
            config.bridge_url, request_id
        ))
        .bearer_auth(&config.api_token)
        .json(&RequestStatusReport {
            status: status.as_str(),
            reason,
        })
        .send()?;

    parse_empty_response(response)
}

pub fn report_invite_status(
    config: &BridgeClientConfig,
    invite_id: u64,
    status: InviteStatus,
) -> Result<(), BridgeError> {
    let response = client()?
        .post(format!(
            "{}/bridge/invites/{}/status",
            config.bridge_url, invite_id
        ))
        .bearer_auth(&config.api_token)
        .json(&InviteStatusReport {
            status: status.as_str(),
        })
        .send()?;

    parse_empty_response(response)
}

pub fn requester_is_allowed(
    owner_discord_user_id: &str,
    allowed_requester_ids: &str,
    requester_discord_user_id: &str,
) -> bool {
    requester_discord_user_id == owner_discord_user_id
        || allowed_discord_user_ids(allowed_requester_ids).contains(requester_discord_user_id)
}

fn generate_companion_id() -> String {
    let mut bytes = [0_u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex_bytes(&bytes)
}

fn generate_api_token() -> String {
    let mut bytes = [0_u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn display_pairing_code(value: u32) -> String {
    let raw = format!("{value:08X}");
    format!("{}-{}", &raw[0..4], &raw[4..8])
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

fn client() -> Result<Client, BridgeError> {
    Ok(Client::builder().timeout(HTTP_TIMEOUT).build()?)
}

fn parse_json_response<T: for<'de> Deserialize<'de>>(
    response: reqwest::blocking::Response,
) -> Result<T, BridgeError> {
    if !response.status().is_success() {
        return Err(http_status_error(response));
    }

    Ok(response.json()?)
}

fn parse_empty_response(response: reqwest::blocking::Response) -> Result<(), BridgeError> {
    if !response.status().is_success() {
        return Err(http_status_error(response));
    }

    Ok(())
}

fn http_status_error(response: reqwest::blocking::Response) -> BridgeError {
    let status = response.status();
    let body = response.text().unwrap_or_else(|_| String::new());

    BridgeError::HttpStatus { status, body }
}

fn normalize_discord_user_id(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let mention_body = trimmed
        .strip_prefix("<@")
        .and_then(|value| value.strip_suffix('>'))
        .unwrap_or(trimmed);
    let candidate = mention_body.strip_prefix('!').unwrap_or(mention_body);

    if candidate.is_empty()
        || !candidate
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        None
    } else {
        Some(candidate.to_owned())
    }
}

fn allowed_discord_user_ids(value: &str) -> HashSet<String> {
    value
        .split(|character: char| {
            character == ',' || character == ';' || character.is_ascii_whitespace()
        })
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .filter_map(normalize_discord_user_id)
        .collect()
}

pub fn allowed_requester_ids(value: &str) -> Vec<String> {
    let mut ids = allowed_discord_user_ids(value)
        .into_iter()
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

pub fn normalize_allowed_requester_id(value: &str) -> Option<String> {
    normalize_discord_user_id(value)
}

pub fn add_allowed_requester(existing: &str, user_id: &str) -> String {
    let Some(user_id) = normalize_discord_user_id(user_id) else {
        return allowed_requester_ids(existing).join("\n");
    };

    let mut ids = allowed_requester_ids(existing);

    if !ids.iter().any(|id| id == &user_id) {
        ids.push(user_id);
    }

    ids.sort();
    ids.join("\n")
}

pub fn remove_allowed_requester(existing: &str, user_id: &str) -> String {
    let Some(user_id) = normalize_discord_user_id(user_id) else {
        return allowed_requester_ids(existing).join("\n");
    };

    allowed_requester_ids(existing)
        .into_iter()
        .filter(|id| id != &user_id)
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn remember_allowed_requester(
    settings: &mut DiscordBridgeSettings,
    user_id: &str,
    display_name: &str,
) {
    let Some(user_id) = normalize_discord_user_id(user_id) else {
        return;
    };
    let display_name = display_name.trim();

    if let Some(requester) = settings
        .allowed_requesters
        .iter_mut()
        .find(|requester| requester.discord_user_id == user_id)
    {
        if !display_name.is_empty() {
            requester.display_name = display_name.to_owned();
        }
    } else {
        settings.allowed_requesters.push(AllowedDiscordRequester {
            discord_user_id: user_id,
            display_name: display_name.to_owned(),
        });
    }

    settings
        .allowed_requesters
        .sort_by(|left, right| left.discord_user_id.cmp(&right.discord_user_id));
}

pub fn forget_allowed_requester(settings: &mut DiscordBridgeSettings, user_id: &str) {
    let Some(user_id) = normalize_discord_user_id(user_id) else {
        return;
    };

    settings
        .allowed_requesters
        .retain(|requester| requester.discord_user_id != user_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bridge_url_points_to_configured_server() {
        assert_eq!(
            DiscordBridgeSettings::default().bridge_url,
            DEFAULT_BRIDGE_URL
        );
    }

    #[test]
    fn saved_bridge_settings_without_url_use_default_bridge_url() {
        let settings: DiscordBridgeSettings = serde_json::from_str(
            r#"{
                "companion_id": "",
                "api_token": "",
                "pairing_code": "",
                "pairing_expires_at": null,
                "paired_discord_user_id": null,
                "allowed_requester_ids": "",
                "poll_enabled": false
            }"#,
        )
        .expect("bridge settings should parse");

        assert_eq!(settings.bridge_url, DEFAULT_BRIDGE_URL);
    }

    #[test]
    fn saved_bridge_settings_with_blank_url_use_default_bridge_url() {
        let settings: DiscordBridgeSettings = serde_json::from_str(
            r#"{
                "bridge_url": "   ",
                "companion_id": "",
                "api_token": "",
                "pairing_code": "",
                "pairing_expires_at": null,
                "paired_discord_user_id": null,
                "allowed_requester_ids": "",
                "poll_enabled": false
            }"#,
        )
        .expect("bridge settings should parse");

        assert_eq!(settings.bridge_url, DEFAULT_BRIDGE_URL);
    }

    #[test]
    fn saved_bridge_settings_keep_custom_bridge_url() {
        let settings: DiscordBridgeSettings = serde_json::from_str(
            r#"{
                "bridge_url": "http://127.0.0.1:3000",
                "companion_id": "",
                "api_token": "",
                "pairing_code": "",
                "pairing_expires_at": null,
                "paired_discord_user_id": null,
                "allowed_requester_ids": "",
                "poll_enabled": false
            }"#,
        )
        .expect("bridge settings should parse");

        assert_eq!(settings.bridge_url, "http://127.0.0.1:3000");
    }

    #[test]
    fn ensure_credentials_only_generates_missing_values() {
        let mut settings = DiscordBridgeSettings::default();

        assert!(ensure_credentials(&mut settings));
        assert_eq!(settings.companion_id.len(), 32);
        assert!(settings.api_token.len() >= 32);

        let companion_id = settings.companion_id.clone();
        let api_token = settings.api_token.clone();

        assert!(!ensure_credentials(&mut settings));
        assert_eq!(settings.companion_id, companion_id);
        assert_eq!(settings.api_token, api_token);
    }

    #[test]
    fn can_poll_when_bridge_url_and_credentials_exist() {
        let mut settings = DiscordBridgeSettings {
            bridge_url: "http://127.0.0.1:3000".to_owned(),
            ..Default::default()
        };

        assert!(ensure_credentials(&mut settings));
        assert!(settings.can_poll());
    }

    #[test]
    fn pairing_code_uses_four_four_hex_shape() {
        let code = generate_pairing_code();

        assert_eq!(code.len(), 9);
        assert_eq!(&code[4..5], "-");
        assert!(
            code.chars()
                .filter(|character| *character != '-')
                .all(|character| character.is_ascii_hexdigit())
        );
    }

    #[test]
    fn requester_is_allowed_for_owner_or_allowed_ids() {
        assert!(requester_is_allowed("111", "", "111"));
        assert!(requester_is_allowed("111", "222, 333\n444", "333"));
        assert!(!requester_is_allowed("111", "222", "333"));
    }

    #[test]
    fn allowed_requester_ids_accept_mentions_and_sort() {
        assert_eq!(
            allowed_requester_ids("333, <@222>; <@!444> nope 333"),
            vec!["222".to_owned(), "333".to_owned(), "444".to_owned()]
        );
    }

    #[test]
    fn normalize_allowed_requester_id_accepts_ids_and_mentions_only() {
        assert_eq!(
            normalize_allowed_requester_id("222"),
            Some("222".to_owned())
        );
        assert_eq!(
            normalize_allowed_requester_id("<@!333>"),
            Some("333".to_owned())
        );
        assert_eq!(normalize_allowed_requester_id("thisguysname"), None);
    }

    #[test]
    fn add_allowed_requester_adds_once_and_normalizes_list() {
        assert_eq!(add_allowed_requester("", "222"), "222");
        assert_eq!(add_allowed_requester("333, 222", "222"), "222\n333");
        assert_eq!(add_allowed_requester("abc, 333", "222"), "222\n333");
        assert_eq!(add_allowed_requester("333", "<@222>"), "222\n333");
    }

    #[test]
    fn remove_allowed_requester_removes_normalized_id() {
        assert_eq!(remove_allowed_requester("333\n222", "222"), "333");
        assert_eq!(remove_allowed_requester("333\n222", "<@333>"), "222");
    }

    #[test]
    fn remembered_allowed_requester_stores_latest_display_name() {
        let mut settings = DiscordBridgeSettings::default();

        remember_allowed_requester(&mut settings, "222", "Jane");
        remember_allowed_requester(&mut settings, "222", "Jane D");

        assert_eq!(
            settings.allowed_requesters,
            vec![AllowedDiscordRequester {
                discord_user_id: "222".to_owned(),
                display_name: "Jane D".to_owned()
            }]
        );
    }

    #[test]
    fn request_time_text_matches_alarm_input_format() {
        let request = RemoteAlarmRequest {
            id: 1,
            requester_id: "111".to_owned(),
            requester_label: None,
            target_id: "222".to_owned(),
            hour: 7,
            minute: 5,
            label: "Laundry".to_owned(),
            created_at_unix: 1,
            expires_at_unix: 2,
        };

        assert_eq!(request.time_text(), "07.05");
    }

    #[test]
    fn remote_alarm_request_accepts_optional_requester_label() {
        let request: RemoteAlarmRequest = serde_json::from_str(
            r#"{
                "id": 1,
                "requester_id": "111",
                "requester_label": "Jane",
                "target_id": "222",
                "hour": 7,
                "minute": 5,
                "label": "Laundry",
                "created_at_unix": 1,
                "expires_at_unix": 2
            }"#,
        )
        .expect("remote alarm request should parse");

        assert_eq!(request.requester_label.as_deref(), Some("Jane"));
    }
}
