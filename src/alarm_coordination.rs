//! Immediate desktop alarm edits and reconciliation with worker acknowledgements.

use crate::alarm::{
    Alarm, AlarmEngine, AlarmId, AlarmStatus, PendingAlarmSnapshot, next_alarm_id_after,
    sort_pending_alarms,
};
use crate::worker::{MonitorCommand, MonitorSnapshot};
use chrono::{DateTime, Local};

#[derive(Debug)]
pub(crate) struct AlarmCoordination {
    snapshot: MonitorSnapshot,
    revision: u64,
    next_alarm_id: AlarmId,
}

impl AlarmCoordination {
    pub(crate) fn restore(
        saved: Vec<PendingAlarmSnapshot>,
        now: DateTime<Local>,
    ) -> (Self, Vec<PendingAlarmSnapshot>) {
        // Reserve IDs from the entire saved list, including missed and discarded alarms.
        let next_alarm_id = next_alarm_id_after(&saved);
        let (mut restored, mut missed): (Vec<_>, Vec<_>) = saved
            .into_iter()
            .partition(|alarm| alarm.alarm.scheduled_for > now);
        restored.retain(|alarm| alarm.status != AlarmStatus::Fired);
        sort_pending_alarms(&mut restored);
        sort_pending_alarms(&mut missed);
        let alarm = AlarmEngine::from_pending_alarms(restored).snapshot();

        (
            Self {
                snapshot: MonitorSnapshot {
                    alarm,
                    applied_alarm_revision: 0,
                    game_status: Default::default(),
                    last_checked: now,
                },
                revision: 0,
                next_alarm_id,
            },
            missed,
        )
    }

    pub(crate) fn snapshot(&self) -> &MonitorSnapshot {
        &self.snapshot
    }

    /// Updates the immediate view and returns the matching command for the worker.
    pub(crate) fn schedule(&mut self, alarm: Alarm) -> MonitorCommand {
        let id = self.next_alarm_id;
        self.next_alarm_id = self.next_alarm_id.saturating_add(1);
        self.snapshot.alarm.alarms.push(PendingAlarmSnapshot {
            id,
            alarm: alarm.clone(),
            status: AlarmStatus::Scheduled,
            delayed_by_game: false,
        });
        sort_pending_alarms(&mut self.snapshot.alarm.alarms);
        self.snapshot.alarm.fired_at = None;
        self.snapshot.alarm.last_fired_label = None;
        self.snapshot.alarm.refresh_status();

        MonitorCommand::Schedule {
            id,
            alarm,
            revision: self.next_revision(),
        }
    }

    pub(crate) fn cancel(&mut self, id: AlarmId) -> MonitorCommand {
        self.snapshot.alarm.alarms.retain(|alarm| alarm.id != id);
        self.snapshot.alarm.refresh_status();
        MonitorCommand::CancelAlarm {
            id,
            revision: self.next_revision(),
        }
    }

    pub(crate) fn cancel_all(&mut self) -> MonitorCommand {
        self.snapshot.alarm.alarms.clear();
        self.snapshot.alarm.fired_at = None;
        self.snapshot.alarm.last_fired_label = None;
        self.snapshot.alarm.refresh_status();
        MonitorCommand::Cancel {
            revision: self.next_revision(),
        }
    }

    /// Accepts detection updates immediately, retaining local alarm edits until acknowledged.
    /// Returns whether the pending alarm list changed and should be saved.
    pub(crate) fn apply_snapshot(&mut self, snapshot: MonitorSnapshot) -> bool {
        if snapshot.applied_alarm_revision < self.revision {
            self.snapshot.game_status = snapshot.game_status;
            self.snapshot.last_checked = snapshot.last_checked;
            return false;
        }
        let alarms_changed = self.snapshot.alarm.alarms != snapshot.alarm.alarms;
        self.snapshot = snapshot;
        alarms_changed
    }

    pub(crate) fn record_fired(&mut self, id: AlarmId, label: String, fired_at: DateTime<Local>) {
        self.snapshot.alarm.alarms.retain(|alarm| alarm.id != id);
        self.snapshot.alarm.fired_at = Some(fired_at);
        self.snapshot.alarm.last_fired_label = Some(label);
        self.snapshot.alarm.refresh_status();
    }

    fn next_revision(&mut self) -> u64 {
        self.revision = self
            .revision
            .checked_add(1)
            .expect("a single app session cannot exhaust u64 alarm revisions");
        self.revision
    }
}

#[cfg(test)]
mod tests;
