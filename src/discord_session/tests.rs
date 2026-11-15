use super::*;
use chrono::TimeZone;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::time::Duration;

#[derive(Debug, PartialEq)]
struct Report {
    id: u64,
    status: RequestStatus,
    reason: Option<String>,
}

#[derive(Default)]
struct Calls {
    registrations: Vec<Sender<Result<PairingRegistrationResponse, String>>>,
    codes: Vec<String>,
    polls: Vec<Sender<Result<PollRequestsResponse, String>>>,
    reports: Vec<Report>,
    invites: Vec<(u64, InviteStatus)>,
}

struct ScriptedTasks(Rc<RefCell<Calls>>);

impl BridgeTasks for ScriptedTasks {
    fn register_pairing(
        &mut self,
        _: BridgeClientConfig,
        code: String,
    ) -> Receiver<Result<PairingRegistrationResponse, String>> {
        let (sender, receiver) = mpsc::channel();
        let mut calls = self.0.borrow_mut();
        calls.registrations.push(sender);
        calls.codes.push(code);
        receiver
    }

    fn poll_requests(
        &mut self,
        _: BridgeClientConfig,
    ) -> Receiver<Result<PollRequestsResponse, String>> {
        let (sender, receiver) = mpsc::channel();
        self.0.borrow_mut().polls.push(sender);
        receiver
    }

    fn report_request(
        &mut self,
        _: BridgeClientConfig,
        id: u64,
        status: RequestStatus,
        reason: Option<String>,
    ) {
        self.0
            .borrow_mut()
            .reports
            .push(Report { id, status, reason });
    }

    fn report_invite(&mut self, _: BridgeClientConfig, id: u64, status: InviteStatus) {
        self.0.borrow_mut().invites.push((id, status));
    }
}

struct Harness {
    session: DiscordSession,
    calls: Rc<RefCell<Calls>>,
    time: SessionTime,
}

impl Harness {
    fn new(settings: DiscordBridgeSettings) -> Self {
        let time = SessionTime {
            wall: Local.with_ymd_and_hms(2026, 9, 1, 12, 0, 0).unwrap(),
            monotonic: Instant::now(),
        };
        let calls = Rc::new(RefCell::new(Calls::default()));
        let session =
            DiscordSession::with_tasks(settings, Box::new(ScriptedTasks(calls.clone())), time);
        Self {
            session,
            calls,
            time,
        }
    }

    fn advance(&mut self, seconds: u32) {
        self.time.monotonic += Duration::from_secs(u64::from(seconds));
        self.time.wall += chrono::Duration::seconds(i64::from(seconds));
        self.session.advance(self.time);
    }

    fn start_poll(&mut self) {
        self.session.advance(self.time);
        assert!(
            self.session
                .next_alarm_request(self.time.monotonic)
                .is_none()
        );
        assert!(!self.calls.borrow().polls.is_empty());
    }

    fn queue_poll(&self, response: PollRequestsResponse) {
        self.calls
            .borrow()
            .polls
            .last()
            .unwrap()
            .send(Ok(response))
            .unwrap();
    }

    fn complete_poll(&mut self, response: PollRequestsResponse) {
        self.queue_poll(response);
        self.session.advance(self.time);
    }
}

fn settings() -> DiscordBridgeSettings {
    DiscordBridgeSettings {
        bridge_url: "https://bridge.invalid".to_owned(),
        companion_id: "companion".to_owned(),
        api_token: "token".to_owned(),
        paired_discord_user_id: Some("111".to_owned()),
        allowed_requester_ids: "222".to_owned(),
        ..Default::default()
    }
}

fn request(id: u64) -> RemoteAlarmRequest {
    RemoteAlarmRequest {
        id,
        requester_id: "222".to_owned(),
        requester_label: None,
        target_id: "111".to_owned(),
        hour: 7,
        minute: 5,
        label: "Laundry".to_owned(),
        // Preserve the desktop's current reliance on bridge-side expiry decisions.
        created_at_unix: 1,
        expires_at_unix: 2,
    }
}

fn invite(id: u64) -> CompanionInvite {
    CompanionInvite {
        id,
        owner_id: "111".to_owned(),
        invitee_id: (id + 200).to_string(),
        invitee_label: format!("Person {id}"),
        created_at_unix: 1,
        expires_at_unix: 2,
    }
}

fn response(
    requests: Vec<RemoteAlarmRequest>,
    invites: Vec<CompanionInvite>,
) -> PollRequestsResponse {
    PollRequestsResponse {
        paired: true,
        paired_discord_user_id: Some("111".to_owned()),
        paired_discord_user_label: Some("Owner".to_owned()),
        requests,
        invites,
    }
}

pub(crate) fn queued_poll(
    settings: DiscordBridgeSettings,
    response: PollRequestsResponse,
) -> DiscordSession {
    let mut harness = Harness::new(settings);
    harness.start_poll();
    harness.queue_poll(response);
    harness.session
}

#[test]
fn clearing_pairing_discards_queued_registration_and_poll_results() {
    let mut registration = Harness::new(DiscordBridgeSettings::default());
    registration.session.start_pairing();
    registration.session.start_pending_tasks();
    registration.calls.borrow().registrations[0]
        .send(Ok(PairingRegistrationResponse {
            pairing_code: "OLD-CODE".to_owned(),
            expires_at_unix: (registration.time.wall + chrono::Duration::minutes(5)).timestamp(),
        }))
        .unwrap();
    registration.session.clear_pairing();
    registration.advance(0);
    assert!(
        registration
            .session
            .next_alarm_request(registration.time.monotonic)
            .is_none()
    );
    assert!(registration.session.settings().pairing_code.is_empty());
    assert!(registration.session.settings().api_token.is_empty());
    assert_eq!(registration.session.status(), "Disconnected");

    let mut poll = Harness::new(settings());
    poll.start_poll();
    poll.queue_poll(response(vec![request(1)], vec![invite(1)]));
    poll.session.clear_pairing();
    poll.advance(0);
    assert!(
        poll.session
            .next_alarm_request(poll.time.monotonic)
            .is_none()
    );
    assert!(poll.session.settings().paired_discord_user_id.is_none());
    assert!(poll.session.pending_invites().is_empty());
    assert!(poll.calls.borrow().reports.is_empty());
}

#[test]
fn url_edit_discards_registration_without_clearing_identity() {
    let mut harness = Harness::new(DiscordBridgeSettings::default());
    harness.session.start_pairing();
    harness.session.start_pending_tasks();
    let mut expected = harness.session.settings().clone();
    expected.bridge_url = "https://replacement.invalid".to_owned();
    harness.calls.borrow().registrations[0]
        .send(Ok(PairingRegistrationResponse {
            pairing_code: "OLD-CODE".to_owned(),
            expires_at_unix: i64::MAX,
        }))
        .unwrap();
    harness.session.set_bridge_url(expected.bridge_url.clone());
    assert_eq!(harness.session.settings(), &expected);
    harness.advance(0);
    assert_ne!(harness.session.settings().pairing_code, "OLD-CODE");
}

#[test]
fn url_edit_discards_poll_and_invites_but_keeps_access_and_duplicate_history() {
    let mut harness = Harness::new(settings());
    harness.start_poll();
    harness.complete_poll(response(vec![request(1)], vec![invite(1)]));
    let alarm = harness
        .session
        .next_alarm_request(harness.time.monotonic)
        .unwrap();
    harness
        .session
        .finish_alarm_request(alarm, Ok(harness.time.wall));
    assert!(
        harness
            .session
            .next_alarm_request(harness.time.monotonic)
            .is_none()
    );
    assert_eq!(harness.session.pending_invites().len(), 1);
    harness.advance(5);
    assert!(
        harness
            .session
            .next_alarm_request(harness.time.monotonic)
            .is_none()
    );
    let mut stale = response(vec![request(2)], vec![invite(2)]);
    stale.paired_discord_user_id = Some("old-owner".to_owned());
    harness.queue_poll(stale);
    let mut expected = harness.session.settings().clone();
    expected.bridge_url = "https://replacement.invalid".to_owned();
    harness.session.set_bridge_url(expected.bridge_url.clone());
    harness.advance(0);
    assert!(
        harness
            .session
            .next_alarm_request(harness.time.monotonic)
            .is_none()
    );
    assert_eq!(harness.session.settings(), &expected);
    assert!(harness.session.pending_invites().is_empty());
    assert_eq!(
        harness.calls.borrow().polls.len(),
        2,
        "preserve the existing deadline"
    );
    harness.advance(5);
    assert!(
        harness
            .session
            .next_alarm_request(harness.time.monotonic)
            .is_none()
    );
    let mut duplicate = request(1);
    duplicate.requester_id = "not-allowed".to_owned();
    harness.complete_poll(response(vec![duplicate], vec![]));
    assert!(
        harness
            .session
            .next_alarm_request(harness.time.monotonic)
            .is_none()
    );
    assert_eq!(
        harness.calls.borrow().reports[1].reason.as_deref(),
        Some("already handled")
    );
}

#[test]
fn pairing_saves_before_launch_and_hides_code_after_pairing_or_expiration() {
    let mut harness = Harness::new(DiscordBridgeSettings::default());
    harness.session.start_pairing();
    let prepared = harness.session.settings().clone();
    assert!(harness.session.take_changes().persist);
    assert!(
        harness.calls.borrow().registrations.is_empty(),
        "saving precedes launching HTTP"
    );
    assert!(
        harness
            .session
            .visible_pairing_code(harness.time.wall)
            .is_none()
    );
    harness.session.start_pending_tasks();
    assert_eq!(harness.calls.borrow().codes[0], prepared.pairing_code);
    harness.session.start_pairing();
    harness.session.start_pending_tasks();
    assert_eq!(harness.calls.borrow().registrations.len(), 1);
    let expires_at = harness.time.wall + chrono::Duration::minutes(5);
    harness.calls.borrow().registrations[0]
        .send(Ok(PairingRegistrationResponse {
            pairing_code: prepared.pairing_code.clone(),
            expires_at_unix: expires_at.timestamp(),
        }))
        .unwrap();
    harness.advance(0);
    assert_eq!(
        harness.session.visible_pairing_code(harness.time.wall),
        Some(prepared.pairing_code.as_str())
    );
    assert!(harness.session.visible_pairing_code(expires_at).is_none());
    assert_eq!(harness.session.settings().api_token, prepared.api_token);
    assert!(
        harness
            .session
            .next_alarm_request(harness.time.monotonic)
            .is_none()
    );
    harness.complete_poll(response(vec![], vec![]));
    assert!(
        harness
            .session
            .next_alarm_request(harness.time.monotonic)
            .is_none()
    );
    assert!(harness.session.settings().pairing_code.is_empty());
    assert_eq!(harness.session.settings().pairing_expires_at, None);
}

#[test]
fn registration_failure_and_disconnection_clear_the_code() {
    for disconnected in [false, true] {
        let mut harness = Harness::new(DiscordBridgeSettings::default());
        harness.session.start_pairing();
        harness.session.start_pending_tasks();
        let sender = harness.calls.borrow_mut().registrations.remove(0);
        if !disconnected {
            sender.send(Err("bridge unavailable".to_owned())).unwrap();
        }
        drop(sender);
        harness.advance(0);
        assert!(harness.session.settings().pairing_code.is_empty());
        assert_eq!(harness.session.status(), "Pairing failed");
        assert!(
            harness
                .session
                .take_changes()
                .messages
                .iter()
                .any(|message| matches!(message.kind, MessageKind::NotifyError))
        );
    }
}

#[test]
fn expiration_clears_saved_pairing_code_without_waiting_for_http() {
    let mut saved = settings();
    saved.paired_discord_user_id = None;
    saved.pairing_code = "ABCD-1234".to_owned();
    saved.pairing_expires_at = Some(Local.with_ymd_and_hms(2026, 9, 1, 12, 1, 0).unwrap());
    let mut harness = Harness::new(saved);
    assert_eq!(
        harness.session.visible_pairing_code(harness.time.wall),
        Some("ABCD-1234")
    );
    harness.advance(60);
    assert!(
        harness
            .session
            .visible_pairing_code(harness.time.wall)
            .is_none()
    );
    assert!(harness.session.settings().pairing_code.is_empty());
    assert!(harness.session.take_changes().persist);
    harness.advance(0);
    assert!(!harness.session.take_changes().persist);
}

#[test]
fn unpaired_poll_clears_owner_and_invites_without_scheduling_requests() {
    let mut harness = Harness::new(settings());
    harness.start_poll();
    harness.complete_poll(response(vec![], vec![invite(1)]));
    assert!(
        harness
            .session
            .next_alarm_request(harness.time.monotonic)
            .is_none()
    );
    assert_eq!(harness.session.pending_invites().len(), 1);
    harness.advance(5);
    assert!(
        harness
            .session
            .next_alarm_request(harness.time.monotonic)
            .is_none()
    );
    let mut unpaired = response(vec![request(7)], vec![invite(2)]);
    unpaired.paired = false;
    harness.complete_poll(unpaired);
    assert!(
        harness
            .session
            .next_alarm_request(harness.time.monotonic)
            .is_none()
    );
    assert!(harness.session.settings().paired_discord_user_id.is_none());
    assert!(harness.session.pending_invites().is_empty());
    assert_eq!(harness.session.status(), "Waiting for /companion pair");
    assert!(harness.session.take_changes().persist);
    assert!(harness.calls.borrow().reports.is_empty());
}

#[test]
fn failed_poll_preserves_pairing_and_retries_at_the_existing_deadline() {
    let mut harness = Harness::new(settings());
    let saved = harness.session.settings().clone();
    harness.start_poll();
    harness.calls.borrow().polls[0]
        .send(Err("bridge offline".to_owned()))
        .unwrap();
    harness.advance(0);
    assert!(
        harness
            .session
            .next_alarm_request(harness.time.monotonic)
            .is_none()
    );
    assert_eq!(harness.session.settings(), &saved);
    assert_eq!(
        harness.session.status(),
        "Bridge poll failed: bridge offline"
    );
    assert_eq!(harness.calls.borrow().polls.len(), 1);
    harness.advance(5);
    assert!(
        harness
            .session
            .next_alarm_request(harness.time.monotonic)
            .is_none()
    );
    assert_eq!(harness.calls.borrow().polls.len(), 2);
}

#[test]
fn polling_uses_credentials_and_request_start_deadlines_despite_legacy_flag() {
    let mut saved = settings();
    saved.poll_enabled = false;
    let mut harness = Harness::new(saved);
    harness.start_poll();
    harness.complete_poll(response(vec![], vec![]));
    assert!(
        harness
            .session
            .next_alarm_request(harness.time.monotonic)
            .is_none()
    );
    harness.advance(4);
    assert!(
        harness
            .session
            .next_alarm_request(harness.time.monotonic)
            .is_none()
    );
    assert_eq!(harness.calls.borrow().polls.len(), 1);
    harness.advance(1);
    assert!(
        harness
            .session
            .next_alarm_request(harness.time.monotonic)
            .is_none()
    );
    assert_eq!(harness.calls.borrow().polls.len(), 2);
    harness.advance(10);
    assert!(
        harness
            .session
            .next_alarm_request(harness.time.monotonic)
            .is_none()
    );
    assert_eq!(harness.calls.borrow().polls.len(), 2, "one poll at a time");
    harness.complete_poll(response(vec![], vec![]));
    assert!(
        harness
            .session
            .next_alarm_request(harness.time.monotonic)
            .is_none()
    );
    assert_eq!(
        harness.calls.borrow().polls.len(),
        3,
        "deadline is measured from request start"
    );
}

#[test]
fn scheduling_result_controls_acceptance_and_same_poll_duplicates() {
    let mut harness = Harness::new(settings());
    harness.start_poll();
    harness.complete_poll(response(vec![request(7), request(7), request(7)], vec![]));
    let first = harness
        .session
        .next_alarm_request(harness.time.monotonic)
        .unwrap();
    assert_eq!(first.label(), "Laundry");
    assert_eq!(first.time_text(), "07.05");
    assert!(
        harness.calls.borrow().reports.is_empty(),
        "no acceptance before scheduling"
    );
    harness
        .session
        .finish_alarm_request(first, Err("invalid alarm time".to_owned()));
    let retry = harness
        .session
        .next_alarm_request(harness.time.monotonic)
        .expect("failed request remains retryable");
    harness
        .session
        .finish_alarm_request(retry, Ok(harness.time.wall));
    assert!(
        harness
            .session
            .next_alarm_request(harness.time.monotonic)
            .is_none(),
        "successful request schedules only once"
    );
    assert_eq!(
        harness.calls.borrow().reports,
        vec![
            Report {
                id: 7,
                status: RequestStatus::Rejected,
                reason: Some("invalid alarm time".to_owned())
            },
            Report {
                id: 7,
                status: RequestStatus::Accepted,
                reason: None
            },
            Report {
                id: 7,
                status: RequestStatus::Accepted,
                reason: Some("already handled".to_owned())
            },
        ]
    );
}

#[test]
fn owner_target_and_requester_are_checked_before_scheduling() {
    for (owner, target, requester, reason) in [
        (None, "111", "222", "companion is not paired"),
        (Some("111"), "999", "222", "target Discord user mismatch"),
        (Some("111"), "111", "999", "requester is not allowed"),
    ] {
        let mut harness = Harness::new(settings());
        harness.start_poll();
        let mut incoming = request(1);
        incoming.target_id = target.to_owned();
        incoming.requester_id = requester.to_owned();
        let mut poll = response(vec![incoming], vec![]);
        poll.paired_discord_user_id = owner.map(str::to_owned);
        harness.complete_poll(poll);
        assert!(
            harness
                .session
                .next_alarm_request(harness.time.monotonic)
                .is_none()
        );
        assert_eq!(
            harness.calls.borrow().reports,
            vec![Report {
                id: 1,
                status: RequestStatus::Rejected,
                reason: Some(reason.to_owned()),
            }]
        );
    }
    let mut harness = Harness::new(settings());
    harness.start_poll();
    let mut incoming = request(2);
    incoming.requester_id = "111".to_owned();
    harness.complete_poll(response(vec![incoming], vec![]));
    assert!(
        harness
            .session
            .next_alarm_request(harness.time.monotonic)
            .is_some(),
        "owner is always allowed"
    );
}

#[test]
fn learned_names_are_saved_before_scheduling_and_blank_names_preserve_them() {
    let mut harness = Harness::new(settings());
    harness.start_poll();
    let mut named = request(1);
    named.requester_label = Some("Jane".to_owned());
    let mut blank = request(2);
    blank.requester_label = Some("   ".to_owned());
    harness.complete_poll(response(vec![named, blank], vec![]));
    harness.session.take_changes();
    let first = harness
        .session
        .next_alarm_request(harness.time.monotonic)
        .unwrap();
    assert!(harness.session.take_changes().persist);
    assert_eq!(
        harness.session.settings().allowed_requesters[0].display_name,
        "Jane"
    );
    harness
        .session
        .finish_alarm_request(first, Err("invalid time".to_owned()));
    let second = harness
        .session
        .next_alarm_request(harness.time.monotonic)
        .unwrap();
    assert!(!harness.session.take_changes().persist);
    assert_eq!(
        harness.session.settings().allowed_requesters[0].display_name,
        "Jane"
    );
    harness
        .session
        .finish_alarm_request(second, Ok(harness.time.wall));
}

#[test]
fn invites_surface_once_and_access_is_saved_before_reporting() {
    let mut harness = Harness::new(settings());
    harness.start_poll();
    harness.complete_poll(response(vec![], vec![invite(1), invite(2)]));
    assert!(
        harness
            .session
            .next_alarm_request(harness.time.monotonic)
            .is_none()
    );
    assert_eq!(
        harness.session.take_changes().new_invite_labels,
        vec!["Person 1", "Person 2"]
    );
    harness.advance(5);
    assert!(
        harness
            .session
            .next_alarm_request(harness.time.monotonic)
            .is_none()
    );
    let mut anonymous = invite(3);
    anonymous.invitee_label.clear();
    harness.complete_poll(response(
        vec![],
        vec![invite(1), invite(2), anonymous.clone()],
    ));
    assert!(
        harness
            .session
            .next_alarm_request(harness.time.monotonic)
            .is_none()
    );
    assert_eq!(
        harness.session.take_changes().new_invite_labels,
        vec!["<@203>"]
    );
    harness.session.allow_invite(1);
    assert!(harness.session.take_changes().persist);
    assert!(harness.calls.borrow().invites.is_empty());
    assert!(discord_bridge::requester_is_allowed(
        "111",
        &harness.session.settings().allowed_requester_ids,
        "201"
    ));
    harness.session.start_pending_tasks();
    harness.session.reject_invite(2);
    harness.session.start_pending_tasks();
    assert_eq!(
        harness.calls.borrow().invites,
        vec![(1, InviteStatus::Allowed), (2, InviteStatus::Rejected)]
    );
    assert_eq!(harness.session.pending_invites(), &[anonymous]);
    assert!(!harness.session.add_allowed_requester("username"));
    assert!(harness.session.add_allowed_requester("<@!333>"));
    assert!(!harness.session.add_allowed_requester("333"));
    harness.session.remove_allowed_requester("<@333>");
    assert!(
        !harness
            .session
            .settings()
            .allowed_requesters
            .iter()
            .any(|user| user.discord_user_id == "333")
    );
}

#[test]
fn duplicate_history_is_bounded_and_is_not_persisted() {
    let mut harness = Harness::new(settings());
    harness.start_poll();
    harness.complete_poll(response((0..501).map(request).collect(), vec![]));
    while let Some(alarm) = harness.session.next_alarm_request(harness.time.monotonic) {
        harness
            .session
            .finish_alarm_request(alarm, Ok(harness.time.wall));
    }
    harness.advance(5);
    assert!(
        harness
            .session
            .next_alarm_request(harness.time.monotonic)
            .is_none()
    );
    harness.complete_poll(response(vec![request(0), request(500)], vec![]));
    let evicted = harness
        .session
        .next_alarm_request(harness.time.monotonic)
        .expect("oldest request should be evicted");
    harness
        .session
        .finish_alarm_request(evicted, Ok(harness.time.wall));
    assert!(
        harness
            .session
            .next_alarm_request(harness.time.monotonic)
            .is_none(),
        "recent request remains remembered"
    );
    let mut restarted = Harness::new(harness.session.settings().clone());
    restarted.start_poll();
    restarted.complete_poll(response(vec![request(500)], vec![]));
    assert!(
        restarted
            .session
            .next_alarm_request(restarted.time.monotonic)
            .is_some(),
        "history is not persisted"
    );
}
