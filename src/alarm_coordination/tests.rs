use super::*;
use crate::alarm::GameActivity;
use crate::game::{GameMonitorStatus, GameStatus};
use chrono::TimeZone;

fn now() -> DateTime<Local> {
    Local.with_ymd_and_hms(2026, 9, 1, 12, 0, 0).unwrap()
}

fn coordination() -> AlarmCoordination {
    AlarmCoordination::restore(Vec::new(), now()).0
}

fn alarm(label: &str, minutes: i64) -> Alarm {
    Alarm::new(label, now() + chrono::Duration::minutes(minutes), true)
}

fn revision(command: &MonitorCommand) -> u64 {
    match command {
        MonitorCommand::Schedule { revision, .. }
        | MonitorCommand::CancelAlarm { revision, .. }
        | MonitorCommand::Cancel { revision } => *revision,
        _ => panic!("expected an alarm edit command, got {command:?}"),
    }
}

fn apply(engine: &mut AlarmEngine, command: &MonitorCommand) -> u64 {
    match command {
        MonitorCommand::Schedule { id, alarm, .. } => {
            engine.schedule_with_id(*id, alarm.clone());
        }
        MonitorCommand::CancelAlarm { id, .. } => {
            engine.cancel_alarm(*id);
        }
        MonitorCommand::Cancel { .. } => engine.cancel(),
        _ => panic!("expected an alarm edit command, got {command:?}"),
    }
    revision(command)
}

fn worker_snapshot(engine: &AlarmEngine, revision: u64) -> MonitorSnapshot {
    MonitorSnapshot {
        alarm: engine.snapshot(),
        applied_alarm_revision: revision,
        game_status: GameMonitorStatus::default(),
        last_checked: now(),
    }
}

fn assert_matches_engine(coordination: &AlarmCoordination, engine: &AlarmEngine) {
    let actual = &coordination.snapshot().alarm;
    let expected = engine.snapshot();
    assert_eq!(actual.alarms, expected.alarms, "pending alarms must agree");
    assert_eq!(
        actual.status, expected.status,
        "aggregate status must agree"
    );
    assert_eq!(
        actual.fired_at, expected.fired_at,
        "last firing time must agree"
    );
    assert_eq!(actual.last_fired_label, expected.last_fired_label);
}

#[test]
fn restoration_preserves_pending_state_sorts_missed_alarms_and_reserves_all_saved_ids() {
    let saved = |id, minutes, status| PendingAlarmSnapshot {
        id,
        alarm: alarm(&format!("Alarm {id}"), minutes),
        status,
        delayed_by_game: status == AlarmStatus::WaitingForGameEnd,
    };
    let waiting = saved(9, 1, AlarmStatus::WaitingForGameEnd);
    let future = saved(7, 3, AlarmStatus::Scheduled);
    let missed = saved(50, -1, AlarmStatus::Scheduled);
    let due_now = saved(60, 0, AlarmStatus::Scheduled);
    let fired = saved(100, 2, AlarmStatus::Fired);
    let (mut coordination, missed_alarms) = AlarmCoordination::restore(
        vec![
            future.clone(),
            fired,
            due_now.clone(),
            missed.clone(),
            waiting.clone(),
        ],
        now(),
    );
    assert_eq!(coordination.snapshot().alarm.alarms, vec![waiting, future]);
    assert_eq!(
        coordination.snapshot().alarm.status,
        AlarmStatus::WaitingForGameEnd
    );
    assert_eq!(missed_alarms, vec![missed, due_now]);
    let MonitorCommand::Schedule { id, revision, .. } = coordination.schedule(alarm("Next", 4))
    else {
        panic!("scheduling must return a schedule command");
    };
    assert_eq!(id, 101, "discarded saved IDs must not be reused");
    assert_eq!(revision, 1);
}

#[test]
fn edits_update_the_view_before_worker_delivery_and_keep_engine_order_and_status() {
    let mut coordination = coordination();
    let mut engine = AlarmEngine::default();
    let first = coordination.schedule(alarm("Later", 3));
    apply(&mut engine, &first);
    assert_matches_engine(&coordination, &engine);
    let second = coordination.schedule(alarm("Earlier", 1));
    apply(&mut engine, &second);
    let third = coordination.schedule(alarm("Same time", 1));
    apply(&mut engine, &third);
    assert_matches_engine(&coordination, &engine);
    assert_eq!(
        coordination
            .snapshot()
            .alarm
            .alarms
            .iter()
            .map(|alarm| alarm.alarm.label.as_str())
            .collect::<Vec<_>>(),
        vec!["Earlier", "Same time", "Later"]
    );
    assert_eq!(
        [revision(&first), revision(&second), revision(&third)],
        [1, 2, 3]
    );
    let removed_id = coordination.snapshot().alarm.alarms[1].id;
    let cancel = coordination.cancel(removed_id);
    apply(&mut engine, &cancel);
    assert_matches_engine(&coordination, &engine);
    let cancel_all = coordination.cancel_all();
    apply(&mut engine, &cancel_all);
    assert_matches_engine(&coordination, &engine);
    assert_eq!([revision(&cancel), revision(&cancel_all)], [4, 5]);
}

#[test]
fn stale_snapshot_preserves_a_new_alarm_while_detection_and_heartbeat_update() {
    let mut coordination = coordination();
    let mut stale = coordination.snapshot().clone();
    stale
        .game_status
        .statuses
        .push(GameStatus::active("Test game", None));
    stale.last_checked = now() + chrono::Duration::seconds(1);
    coordination.schedule(alarm("New", 1));
    assert!(!coordination.apply_snapshot(stale));
    assert_eq!(coordination.snapshot().alarm.alarms[0].alarm.label, "New");
    assert!(coordination.snapshot().game_status.statuses[0].is_active());
    assert_eq!(
        coordination.snapshot().last_checked,
        now() + chrono::Duration::seconds(1)
    );
}

#[test]
fn stale_snapshot_cannot_restore_a_canceled_alarm() {
    let mut coordination = coordination();
    let mut engine = AlarmEngine::default();
    let command = coordination.schedule(alarm("Cancel me", 1));
    let revision = apply(&mut engine, &command);
    let stale = worker_snapshot(&engine, revision);
    let id = stale.alarm.alarms[0].id;
    coordination.cancel(id);
    assert!(!coordination.apply_snapshot(stale));
    assert!(coordination.snapshot().alarm.alarms.is_empty());
    assert_eq!(coordination.snapshot().alarm.status, AlarmStatus::Idle);
}

#[test]
fn stale_snapshot_cannot_undo_cancel_all() {
    let mut coordination = coordination();
    let mut engine = AlarmEngine::default();
    apply(&mut engine, &coordination.schedule(alarm("First", 1)));
    let revision = apply(&mut engine, &coordination.schedule(alarm("Second", 2)));
    let stale = worker_snapshot(&engine, revision);
    coordination.cancel_all();
    assert!(!coordination.apply_snapshot(stale));
    assert!(coordination.snapshot().alarm.alarms.is_empty());
    assert_eq!(coordination.snapshot().alarm.status, AlarmStatus::Idle);
}

#[test]
fn partial_acknowledgement_cannot_undo_later_edits_and_full_acknowledgement_is_saved() {
    let mut coordination = coordination();
    let mut engine = AlarmEngine::default();
    let first = coordination.schedule(alarm("First", 1));
    let second = coordination.schedule(alarm("Second", 3));
    let partial_revision = apply(&mut engine, &first);
    engine.tick(now() + chrono::Duration::minutes(2), GameActivity::Active);
    assert!(!coordination.apply_snapshot(worker_snapshot(&engine, partial_revision)));
    assert_eq!(coordination.snapshot().alarm.alarms.len(), 2);
    assert_eq!(coordination.snapshot().alarm.status, AlarmStatus::Scheduled);
    let full_revision = apply(&mut engine, &second);
    let acknowledged = worker_snapshot(&engine, full_revision);
    assert!(
        coordination.apply_snapshot(acknowledged.clone()),
        "accepted status changes need saving"
    );
    assert_matches_engine(&coordination, &engine);
    assert_eq!(
        coordination.snapshot().alarm.status,
        AlarmStatus::WaitingForGameEnd
    );
    assert!(
        !coordination.apply_snapshot(acknowledged),
        "unchanged alarm lists need no extra save"
    );
}

#[test]
fn game_end_firing_and_later_edits_share_engine_status_rules() {
    let mut coordination = coordination();
    let mut engine = AlarmEngine::default();
    let command = coordination.schedule(alarm("Due", 1));
    let revision = apply(&mut engine, &command);
    assert!(
        engine
            .tick(now() + chrono::Duration::minutes(2), GameActivity::Active)
            .is_empty()
    );
    coordination.apply_snapshot(worker_snapshot(&engine, revision));
    assert_eq!(
        coordination.snapshot().alarm.status,
        AlarmStatus::WaitingForGameEnd
    );
    for event in engine.tick(now() + chrono::Duration::minutes(3), GameActivity::Inactive) {
        coordination.record_fired(event.id, event.label, event.fired_at);
    }
    assert_matches_engine(&coordination, &engine);
    assert_eq!(coordination.snapshot().alarm.status, AlarmStatus::Fired);
    let command = coordination.schedule(alarm("New", 4));
    apply(&mut engine, &command);
    assert_matches_engine(&coordination, &engine);
    assert!(coordination.snapshot().alarm.fired_at.is_none());
    let command = coordination.cancel_all();
    apply(&mut engine, &command);
    assert_matches_engine(&coordination, &engine);
}

#[test]
fn firing_during_unacknowledged_edits_keeps_the_remaining_alarm_and_last_firing() {
    let mut coordination = coordination();
    let mut engine = AlarmEngine::default();
    let first = coordination.schedule(alarm("Due", 1));
    let first_revision = apply(&mut engine, &first);
    let later = coordination.schedule(alarm("Later", 4));
    for event in engine.tick(now() + chrono::Duration::minutes(2), GameActivity::Inactive) {
        coordination.record_fired(event.id, event.label, event.fired_at);
    }
    assert!(!coordination.apply_snapshot(worker_snapshot(&engine, first_revision)));
    assert_eq!(coordination.snapshot().alarm.alarms[0].alarm.label, "Later");
    assert_eq!(
        coordination.snapshot().alarm.last_fired_label.as_deref(),
        Some("Due")
    );
    assert_eq!(coordination.snapshot().alarm.status, AlarmStatus::Scheduled);
    let full_revision = apply(&mut engine, &later);
    coordination.apply_snapshot(worker_snapshot(&engine, full_revision));
    assert_matches_engine(&coordination, &engine);
}

#[test]
fn canceling_the_last_pending_alarm_preserves_firing_history_but_cancel_all_clears_it() {
    let mut coordination = coordination();
    coordination.schedule(alarm("Pending", 4));
    let id = coordination.snapshot().alarm.alarms[0].id;
    coordination.record_fired(999, "Already fired".to_owned(), now());
    assert_eq!(coordination.snapshot().alarm.status, AlarmStatus::Scheduled);
    coordination.cancel(id);
    assert_eq!(coordination.snapshot().alarm.status, AlarmStatus::Fired);
    assert_eq!(
        coordination.snapshot().alarm.last_fired_label.as_deref(),
        Some("Already fired")
    );
    coordination.cancel_all();
    assert_eq!(coordination.snapshot().alarm.status, AlarmStatus::Idle);
    assert!(coordination.snapshot().alarm.fired_at.is_none());
    assert!(coordination.snapshot().alarm.last_fired_label.is_none());
}
