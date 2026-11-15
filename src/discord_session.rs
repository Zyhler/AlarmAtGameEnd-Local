//! Desktop Discord pairing, access rules, and background-result coordination.

mod transport;

use crate::discord_bridge::{
    self, BridgeClientConfig, CompanionInvite, DiscordBridgeSettings, InviteStatus,
    PairingRegistrationResponse, PollRequestsResponse, RemoteAlarmRequest, RequestStatus,
};
use chrono::{DateTime, Local, Utc};
use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver};
use std::time::Instant;
use transport::{BridgeTasks, HttpBridgeTasks};

#[derive(Clone, Copy, Debug)]
pub(crate) struct SessionTime {
    pub wall: DateTime<Local>,
    pub monotonic: Instant,
}

impl SessionTime {
    pub(crate) fn now() -> Self {
        Self {
            wall: Local::now(),
            monotonic: Instant::now(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum MessageKind {
    NotifyInfo,
    NotifyError,
    NoticeInfo,
    NoticeError,
    LogInfo,
    LogError,
}

#[derive(Debug)]
pub(crate) struct SessionMessage {
    pub kind: MessageKind,
    pub text: String,
}

#[derive(Debug, Default)]
pub(crate) struct SessionChanges {
    pub persist: bool,
    pub repaint: bool,
    pub messages: Vec<SessionMessage>,
    pub new_invite_labels: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct PreparedAlarm {
    request: RemoteAlarmRequest,
}

impl PreparedAlarm {
    pub(crate) fn label(&self) -> &str {
        &self.request.label
    }
    pub(crate) fn time_text(&self) -> String {
        self.request.time_text()
    }
}

struct PendingPoll {
    requests: VecDeque<RemoteAlarmRequest>,
    invites: Vec<CompanionInvite>,
    request_count: usize,
}

pub(crate) struct DiscordSession {
    settings: DiscordBridgeSettings,
    tasks: Box<dyn BridgeTasks>,
    registration: Option<Receiver<Result<PairingRegistrationResponse, String>>>,
    poll: Option<Receiver<Result<PollRequestsResponse, String>>>,
    pending_registration: Option<(BridgeClientConfig, String)>,
    pending_invite_reports: Vec<(BridgeClientConfig, u64, InviteStatus)>,
    pending_poll: Option<PendingPoll>,
    awaiting_alarm: Option<u64>,
    next_poll_at: Instant,
    time: SessionTime,
    status: String,
    handled_request_ids: Vec<u64>,
    pending_invites: Vec<CompanionInvite>,
    changes: SessionChanges,
}

impl DiscordSession {
    pub(crate) fn new(settings: DiscordBridgeSettings) -> Self {
        Self::with_tasks(settings, Box::new(HttpBridgeTasks), SessionTime::now())
    }

    fn with_tasks(
        settings: DiscordBridgeSettings,
        tasks: Box<dyn BridgeTasks>,
        time: SessionTime,
    ) -> Self {
        Self {
            settings,
            tasks,
            registration: None,
            poll: None,
            pending_registration: None,
            pending_invite_reports: Vec::new(),
            pending_poll: None,
            awaiting_alarm: None,
            next_poll_at: time.monotonic,
            time,
            status: "Disconnected".to_owned(),
            handled_request_ids: Vec::new(),
            pending_invites: Vec::new(),
            changes: SessionChanges::default(),
        }
    }

    pub(crate) fn settings(&self) -> &DiscordBridgeSettings {
        &self.settings
    }
    pub(crate) fn status(&self) -> &str {
        &self.status
    }
    pub(crate) fn pending_invites(&self) -> &[CompanionInvite] {
        &self.pending_invites
    }
    pub(crate) fn visible_pairing_code(&self, now: DateTime<Local>) -> Option<&str> {
        visible_pairing_code(&self.settings, now)
    }

    pub(crate) fn set_bridge_url(&mut self, url: String) {
        self.settings.bridge_url = url;
        self.invalidate_tasks();
        self.pending_invites.clear();
        self.changes.persist = true;
    }

    pub(crate) fn pairing_code_copied(&mut self) {
        self.message(MessageKind::NoticeInfo, "Pairing code copied to clipboard");
        self.set_status("Pairing code copied to clipboard");
    }

    fn invalidate_tasks(&mut self) {
        // Dropping receivers discards old results; already-running HTTP is left alone.
        self.registration = None;
        self.poll = None;
        self.pending_registration = None;
        self.pending_poll = None;
        self.awaiting_alarm = None;
    }

    pub(crate) fn take_changes(&mut self) -> SessionChanges {
        std::mem::take(&mut self.changes)
    }

    /// Starts work deferred until the app has attempted to save session changes.
    pub(crate) fn start_pending_tasks(&mut self) {
        if let Some((config, code)) = self.pending_registration.take() {
            self.registration = Some(self.tasks.register_pairing(config, code));
        }
        for (config, id, status) in self.pending_invite_reports.drain(..) {
            self.tasks.report_invite(config, id, status);
        }
    }

    fn queue_invite_report(&mut self, id: u64, status: InviteStatus) {
        if let Ok(config) = self.settings.client_config() {
            self.pending_invite_reports.push((config, id, status));
        }
    }

    fn report_request(&mut self, id: u64, status: RequestStatus, reason: Option<String>) {
        if let Ok(config) = self.settings.client_config() {
            self.tasks.report_request(config, id, status, reason);
        }
    }

    fn reject_request(&mut self, id: u64, reason: &str) {
        self.report_request(id, RequestStatus::Rejected, Some(reason.to_owned()));
    }

    fn message(&mut self, kind: MessageKind, text: impl Into<String>) {
        self.changes.messages.push(SessionMessage {
            kind,
            text: text.into(),
        });
    }
    pub(crate) fn start_pairing(&mut self) {
        if self.registration.is_some() || self.pending_registration.is_some() {
            self.set_status("Pairing registration already in progress");
            self.message(
                MessageKind::NoticeInfo,
                "Pairing registration already in progress",
            );
            return;
        }

        if self
            .settings
            .paired_discord_user_id
            .as_deref()
            .is_some_and(|user_id| !user_id.trim().is_empty())
        {
            self.set_status("Clear pairing before generating a new code");
            self.message(
                MessageKind::NoticeError,
                "Clear pairing before generating a new code",
            );
            return;
        }

        if self.settings.bridge_url.trim().is_empty() {
            self.message(MessageKind::NotifyError, "Discord bridge URL is blank");
            return;
        }

        if discord_bridge::ensure_credentials(&mut self.settings) {
            self.changes.persist = true;
        }

        let pairing_code = discord_bridge::generate_pairing_code();
        self.settings.pairing_code = pairing_code.clone();
        self.settings.pairing_expires_at = None;
        self.settings.poll_enabled = true;
        self.changes.persist = true;

        let config = match self.settings.client_config() {
            Ok(config) => config,
            Err(error) => {
                self.message(
                    MessageKind::NotifyError,
                    format!("Discord bridge is not ready: {error}"),
                );
                return;
            }
        };
        self.pending_registration = Some((config, pairing_code));
        self.set_status("Registering pairing code");
        self.message(MessageKind::NoticeInfo, "Generating pairing code");
    }

    pub(crate) fn clear_pairing(&mut self) {
        self.invalidate_tasks();
        self.settings.companion_id.clear();
        self.settings.api_token.clear();
        self.settings.pairing_code.clear();
        self.settings.pairing_expires_at = None;
        self.settings.paired_discord_user_id = None;
        self.settings.paired_discord_user_label = None;
        self.settings.allowed_requester_ids.clear();
        self.settings.allowed_requesters.clear();
        self.settings.poll_enabled = false;
        self.handled_request_ids.clear();
        self.pending_invites.clear();
        self.changes.persist = true;
        self.set_status("Disconnected");
        self.message(MessageKind::NotifyInfo, "Discord pairing cleared");
    }

    pub(crate) fn allow_invite(&mut self, invite_id: u64) {
        let Some(invite) = self
            .pending_invites
            .iter()
            .find(|invite| invite.id == invite_id)
            .cloned()
        else {
            return;
        };

        self.settings.allowed_requester_ids = discord_bridge::add_allowed_requester(
            &self.settings.allowed_requester_ids,
            &invite.invitee_id,
        );
        discord_bridge::remember_allowed_requester(
            &mut self.settings,
            &invite.invitee_id,
            &invite.invitee_label,
        );
        self.pending_invites.retain(|invite| invite.id != invite_id);
        self.changes.persist = true;
        self.queue_invite_report(invite_id, InviteStatus::Allowed);
        self.message(
            MessageKind::NotifyInfo,
            format!(
                "Allowed {} to request alarms",
                discord_invite_user_label(&invite)
            ),
        );
    }

    pub(crate) fn reject_invite(&mut self, invite_id: u64) {
        let Some(invite) = self
            .pending_invites
            .iter()
            .find(|invite| invite.id == invite_id)
            .cloned()
        else {
            return;
        };

        self.pending_invites.retain(|invite| invite.id != invite_id);
        self.queue_invite_report(invite_id, InviteStatus::Rejected);
        self.message(
            MessageKind::NotifyInfo,
            format!(
                "Rejected Discord access invite for {}",
                discord_invite_user_label(&invite)
            ),
        );
    }

    pub(crate) fn add_allowed_requester(&mut self, input: &str) -> bool {
        let input = input.trim();

        if input.is_empty() {
            self.message(
                MessageKind::NotifyError,
                "Enter a Discord user ID before clicking Add",
            );
            return false;
        }

        let Some(added_id) = discord_bridge::normalize_allowed_requester_id(input) else {
            self.message(MessageKind::NotifyError,
                "Could not add that Discord user. Use a numeric Discord ID or copied @mention; usernames alone do not work here.",
            );
            return false;
        };

        let previous_ids =
            discord_bridge::allowed_requester_ids(&self.settings.allowed_requester_ids);
        if previous_ids.iter().any(|id| id == &added_id) {
            self.message(
                MessageKind::NotifyInfo,
                format!("Discord user {added_id} is already allowed"),
            );
            return false;
        }

        self.settings.allowed_requester_ids =
            discord_bridge::add_allowed_requester(&self.settings.allowed_requester_ids, &added_id);
        discord_bridge::remember_allowed_requester(&mut self.settings, &added_id, "");
        self.changes.persist = true;
        self.message(
            MessageKind::NotifyInfo,
            format!("Allowed Discord user {added_id} to request alarms"),
        );
        true
    }

    pub(crate) fn remove_allowed_requester(&mut self, user_id: &str) {
        self.settings.allowed_requester_ids =
            discord_bridge::remove_allowed_requester(&self.settings.allowed_requester_ids, user_id);
        discord_bridge::forget_allowed_requester(&mut self.settings, user_id);
        self.changes.persist = true;
        self.message(
            MessageKind::NotifyInfo,
            format!("Removed Discord user {user_id} from allowed users"),
        );
    }

    fn remember_request_id(&mut self, request_id: u64) {
        if self.handled_request_ids.contains(&request_id) {
            return;
        }

        self.handled_request_ids.push(request_id);

        if self.handled_request_ids.len() > 500 {
            let overflow_count = self.handled_request_ids.len() - 500;
            self.handled_request_ids.drain(..overflow_count);
        }
    }

    fn set_status(&mut self, status: impl Into<String>) -> bool {
        let status = status.into();

        if self.status == status {
            false
        } else {
            self.status = status;
            self.changes.repaint = true;
            true
        }
    }
    /// Drains completed work. Consume requests sequentially with `next_alarm_request` afterwards.
    pub(crate) fn advance(&mut self, time: SessionTime) {
        assert!(
            self.awaiting_alarm.is_none(),
            "finish the outstanding alarm before advancing the session"
        );
        self.time = time;
        if self.registration.is_none()
            && self.pending_registration.is_none()
            && clear_inactive_pairing_code(&mut self.settings, time.wall)
        {
            self.changes.persist = true;
            self.changes.repaint = true;
        }
        if let Some(result) = completed(
            self.registration.as_ref(),
            "Pairing registration worker stopped",
        ) {
            self.registration = None;
            self.changes.repaint = true;
            match result {
                Ok(response) => {
                    self.settings.pairing_code = response.pairing_code;
                    self.settings.pairing_expires_at =
                        local_datetime_from_unix(response.expires_at_unix);
                    self.changes.persist = true;
                    self.message(MessageKind::NotifyInfo, "Discord pairing code registered");
                    self.set_status("Waiting for /companion pair");
                    self.next_poll_at = time.monotonic;
                }
                Err(error) => {
                    self.settings.pairing_code.clear();
                    self.settings.pairing_expires_at = None;
                    self.changes.persist = true;
                    self.message(
                        MessageKind::NotifyError,
                        format!("Could not register Discord pairing code: {error}"),
                    );
                    self.set_status("Pairing failed");
                }
            }
        }
        if let Some(result) = completed(self.poll.as_ref(), "Discord bridge polling worker stopped")
        {
            self.poll = None;
            self.changes.repaint = true;
            match result {
                Ok(response) => self.begin_poll_response(response),
                Err(error) => {
                    self.set_status(format!("Bridge poll failed: {error}"));
                }
            }
        }
    }

    fn begin_poll_response(&mut self, response: PollRequestsResponse) {
        if !response.paired {
            self.settings.paired_discord_user_id = None;
            self.settings.paired_discord_user_label = None;
            self.pending_invites.clear();
            self.changes.persist = true;
            self.set_status("Waiting for /companion pair");
            return;
        }
        if self.settings.paired_discord_user_id != response.paired_discord_user_id
            || self.settings.paired_discord_user_label != response.paired_discord_user_label
        {
            self.settings.paired_discord_user_id = response.paired_discord_user_id;
            self.settings.paired_discord_user_label = response.paired_discord_user_label;
            self.changes.persist = true;
        }
        if clear_inactive_pairing_code(&mut self.settings, self.time.wall) {
            self.changes.persist = true;
        }
        self.pending_poll = Some(PendingPoll {
            request_count: response.requests.len(),
            requests: response.requests.into(),
            invites: response.invites,
        });
    }

    /// Yields one eligible request; finish it before asking for the next.
    /// This keeps duplicate IDs within the same poll from scheduling twice.
    pub(crate) fn next_alarm_request(&mut self, now: Instant) -> Option<PreparedAlarm> {
        assert!(
            self.awaiting_alarm.is_none(),
            "finish the outstanding alarm before requesting another"
        );
        loop {
            let Some(mut response) = self.pending_poll.take() else {
                self.maybe_start_poll(now);
                return None;
            };
            let Some(request) = response.requests.pop_front() else {
                self.changes.new_invite_labels =
                    new_companion_invite_labels(&self.pending_invites, &response.invites);
                self.pending_invites = response.invites;
                let invite_count = self.pending_invites.len();
                if response.request_count == 0 && invite_count == 0 {
                    self.set_status("Connected");
                } else {
                    self.set_status(format!(
                        "Connected; processed {} request(s), {invite_count} invite(s) pending",
                        response.request_count
                    ));
                }
                self.maybe_start_poll(now);
                return None;
            };
            self.pending_poll = Some(response);
            if self.handled_request_ids.contains(&request.id) {
                self.report_request(
                    request.id,
                    RequestStatus::Accepted,
                    Some("already handled".to_owned()),
                );
                continue;
            }
            let Some(owner) = self.settings.paired_discord_user_id.as_deref() else {
                self.reject_request(request.id, "companion is not paired");
                continue;
            };
            if request.target_id != owner {
                self.reject_request(request.id, "target Discord user mismatch");
                continue;
            }
            if !discord_bridge::requester_is_allowed(
                owner,
                &self.settings.allowed_requester_ids,
                &request.requester_id,
            ) {
                self.reject_request(request.id, "requester is not allowed");
                self.message(
                    MessageKind::LogError,
                    format!(
                        "Rejected Discord alarm request #{} from {}",
                        request.id, request.requester_id
                    ),
                );
                continue;
            }
            if remember_requester_label_from_request(&mut self.settings, &request) {
                self.changes.persist = true;
            }
            self.awaiting_alarm = Some(request.id);
            return Some(PreparedAlarm { request });
        }
    }

    pub(crate) fn finish_alarm_request(
        &mut self,
        alarm: PreparedAlarm,
        result: Result<DateTime<Local>, String>,
    ) {
        let id = alarm.request.id;
        assert_eq!(
            self.awaiting_alarm.take(),
            Some(id),
            "finish only the session's outstanding alarm request {id}"
        );
        match result {
            Ok(scheduled_for) => {
                self.remember_request_id(id);
                self.report_request(id, RequestStatus::Accepted, None);
                self.message(
                    MessageKind::LogInfo,
                    format!(
                        "Accepted Discord alarm request #{id} for {}",
                        scheduled_for.format("%Y-%m-%d %H:%M:%S")
                    ),
                );
            }
            Err(error) => self.reject_request(id, &error),
        }
    }

    fn maybe_start_poll(&mut self, now: Instant) {
        if !self.settings.can_poll()
            || self.poll.is_some()
            || self.registration.is_some()
            || self.pending_registration.is_some()
            || now < self.next_poll_at
        {
            return;
        }
        match self.settings.client_config() {
            Ok(config) => {
                self.next_poll_at = now + discord_bridge::POLL_INTERVAL;
                self.poll = Some(self.tasks.poll_requests(config));
            }
            Err(error) => {
                self.set_status(format!("Discord bridge is not ready: {error}"));
            }
        }
    }
}

fn completed<T>(
    receiver: Option<&Receiver<Result<T, String>>>,
    disconnected: &str,
) -> Option<Result<T, String>> {
    receiver.and_then(|receiver| match receiver.try_recv() {
        Ok(result) => Some(result),
        Err(mpsc::TryRecvError::Empty) => None,
        Err(mpsc::TryRecvError::Disconnected) => Some(Err(disconnected.to_owned())),
    })
}

fn visible_pairing_code(settings: &DiscordBridgeSettings, now: DateTime<Local>) -> Option<&str> {
    let pairing_code = settings.pairing_code.trim();

    if pairing_code.is_empty()
        || settings
            .paired_discord_user_id
            .as_deref()
            .is_some_and(|user_id| !user_id.trim().is_empty())
        || match settings.pairing_expires_at {
            Some(expires_at) => expires_at <= now,
            None => true,
        }
    {
        None
    } else {
        Some(pairing_code)
    }
}

fn clear_inactive_pairing_code(settings: &mut DiscordBridgeSettings, now: DateTime<Local>) -> bool {
    if settings.pairing_code.trim().is_empty() && settings.pairing_expires_at.is_none() {
        return false;
    }

    if visible_pairing_code(settings, now).is_some() {
        return false;
    }

    settings.pairing_code.clear();
    settings.pairing_expires_at = None;
    true
}

fn discord_user_label(user_id: &str) -> String {
    format!("<@{user_id}>")
}

pub(crate) fn discord_invite_user_label(invite: &CompanionInvite) -> String {
    let label = invite.invitee_label.trim();

    if label.is_empty() {
        discord_user_label(&invite.invitee_id)
    } else {
        label.to_owned()
    }
}

fn new_companion_invite_labels(
    existing_invites: &[CompanionInvite],
    incoming_invites: &[CompanionInvite],
) -> Vec<String> {
    incoming_invites
        .iter()
        .filter(|incoming_invite| {
            !existing_invites
                .iter()
                .any(|existing_invite| existing_invite.id == incoming_invite.id)
        })
        .map(discord_invite_user_label)
        .collect()
}

fn remember_requester_label_from_request(
    settings: &mut DiscordBridgeSettings,
    request: &RemoteAlarmRequest,
) -> bool {
    let Some(display_name) = request
        .requester_label
        .as_deref()
        .map(str::trim)
        .filter(|display_name| !display_name.is_empty())
    else {
        return false;
    };

    let previous_requesters = settings.allowed_requesters.clone();
    discord_bridge::remember_allowed_requester(settings, &request.requester_id, display_name);

    settings.allowed_requesters != previous_requesters
}

pub(crate) fn local_datetime_from_unix(timestamp: i64) -> Option<DateTime<Local>> {
    DateTime::<Utc>::from_timestamp(timestamp, 0).map(|value| value.with_timezone(&Local))
}

#[cfg(test)]
pub(crate) mod tests;
