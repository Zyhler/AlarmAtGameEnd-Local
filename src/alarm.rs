use chrono::{DateTime, Local, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GameActivity {
    Active,
    Inactive,
    Unavailable,
}

impl GameActivity {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Active)
    }
}

pub type AlarmId = u64;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Alarm {
    pub reminder: String,
    pub scheduled_for: DateTime<Local>,
    pub delay_for_games: bool,
    pub sound_path: Option<PathBuf>,
}

impl Alarm {
    pub fn new(
        reminder: impl Into<String>,
        scheduled_for: DateTime<Local>,
        delay_for_games: bool,
    ) -> Self {
        Self::with_sound(reminder, scheduled_for, delay_for_games, None)
    }

    pub fn with_sound(
        reminder: impl Into<String>,
        scheduled_for: DateTime<Local>,
        delay_for_games: bool,
        sound_path: Option<PathBuf>,
    ) -> Self {
        Self {
            reminder: reminder.into(),
            scheduled_for,
            delay_for_games,
            sound_path,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AlarmStatus {
    Idle,
    Scheduled,
    WaitingForGameEnd,
    Fired,
}

impl AlarmStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Scheduled => "Scheduled",
            Self::WaitingForGameEnd => "Waiting for game end",
            Self::Fired => "Fired",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AlarmEvent {
    pub id: AlarmId,
    pub reminder: String,
    pub scheduled_for: DateTime<Local>,
    pub fired_at: DateTime<Local>,
    pub delayed_by_game: bool,
    pub sound_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PendingAlarmSnapshot {
    pub id: AlarmId,
    pub alarm: Alarm,
    pub status: AlarmStatus,
    pub delayed_by_game: bool,
}

#[derive(Clone, Debug)]
pub struct AlarmSnapshot {
    pub status: AlarmStatus,
    pub alarms: Vec<PendingAlarmSnapshot>,
    pub fired_at: Option<DateTime<Local>>,
    pub last_fired_reminder: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AlarmEngine {
    alarms: Vec<PendingAlarm>,
    fired_at: Option<DateTime<Local>>,
    last_fired_reminder: Option<String>,
    next_alarm_id: AlarmId,
}

#[derive(Clone, Debug)]
struct PendingAlarm {
    id: AlarmId,
    alarm: Alarm,
    status: AlarmStatus,
    delayed_by_game: bool,
}

impl Default for AlarmEngine {
    fn default() -> Self {
        Self {
            alarms: Vec::new(),
            fired_at: None,
            last_fired_reminder: None,
            next_alarm_id: 1,
        }
    }
}

impl AlarmEngine {
    pub fn from_pending_alarms(alarms: Vec<PendingAlarmSnapshot>) -> Self {
        let mut engine = Self {
            next_alarm_id: next_alarm_id_after(&alarms),
            ..Self::default()
        };

        engine.alarms = alarms
            .into_iter()
            .filter(|alarm| alarm.status != AlarmStatus::Fired)
            .map(|alarm| PendingAlarm {
                id: alarm.id,
                alarm: alarm.alarm,
                status: alarm.status,
                delayed_by_game: alarm.delayed_by_game,
            })
            .collect();
        engine.sort_alarms();
        engine
    }

    pub fn schedule(&mut self, alarm: Alarm) -> AlarmId {
        let id = self.next_alarm_id;
        self.next_alarm_id = self.next_alarm_id.saturating_add(1);
        self.schedule_with_id(id, alarm)
    }

    pub fn schedule_with_id(&mut self, id: AlarmId, alarm: Alarm) -> AlarmId {
        self.next_alarm_id = self.next_alarm_id.max(id.saturating_add(1));
        self.alarms.push(PendingAlarm {
            id,
            alarm,
            status: AlarmStatus::Scheduled,
            delayed_by_game: false,
        });
        self.sort_alarms();
        self.fired_at = None;
        self.last_fired_reminder = None;
        id
    }

    pub fn cancel(&mut self) {
        self.alarms.clear();
        self.fired_at = None;
        self.last_fired_reminder = None;
    }

    pub fn cancel_alarm(&mut self, id: AlarmId) -> bool {
        let original_count = self.alarms.len();
        self.alarms.retain(|alarm| alarm.id != id);

        original_count != self.alarms.len()
    }

    pub fn tick(&mut self, now: DateTime<Local>, game_activity: GameActivity) -> Vec<AlarmEvent> {
        let mut pending_alarms = Vec::with_capacity(self.alarms.len());
        let mut events = Vec::new();

        for mut pending_alarm in std::mem::take(&mut self.alarms) {
            match pending_alarm.status {
                AlarmStatus::Scheduled if now >= pending_alarm.alarm.scheduled_for => {
                    if pending_alarm.alarm.delay_for_games && game_activity.is_active() {
                        pending_alarm.status = AlarmStatus::WaitingForGameEnd;
                        pending_alarm.delayed_by_game = true;
                        pending_alarms.push(pending_alarm);
                    } else {
                        events.push(self.fire(now, pending_alarm));
                    }
                }
                AlarmStatus::WaitingForGameEnd if !game_activity.is_active() => {
                    events.push(self.fire(now, pending_alarm));
                }
                _ => pending_alarms.push(pending_alarm),
            }
        }

        self.alarms = pending_alarms;
        self.sort_alarms();
        events
    }

    pub fn snapshot(&self) -> AlarmSnapshot {
        AlarmSnapshot {
            status: self.status(),
            alarms: self
                .alarms
                .iter()
                .map(|pending_alarm| PendingAlarmSnapshot {
                    id: pending_alarm.id,
                    alarm: pending_alarm.alarm.clone(),
                    status: pending_alarm.status,
                    delayed_by_game: pending_alarm.delayed_by_game,
                })
                .collect(),
            fired_at: self.fired_at,
            last_fired_reminder: self.last_fired_reminder.clone(),
        }
    }

    fn status(&self) -> AlarmStatus {
        if self
            .alarms
            .iter()
            .any(|alarm| alarm.status == AlarmStatus::WaitingForGameEnd)
        {
            AlarmStatus::WaitingForGameEnd
        } else if !self.alarms.is_empty() {
            AlarmStatus::Scheduled
        } else if self.fired_at.is_some() {
            AlarmStatus::Fired
        } else {
            AlarmStatus::Idle
        }
    }

    fn sort_alarms(&mut self) {
        self.alarms.sort_by(|left, right| {
            left.alarm
                .scheduled_for
                .cmp(&right.alarm.scheduled_for)
                .then(left.id.cmp(&right.id))
        });
    }

    fn fire(&mut self, fired_at: DateTime<Local>, pending_alarm: PendingAlarm) -> AlarmEvent {
        self.fired_at = Some(fired_at);
        self.last_fired_reminder = Some(pending_alarm.alarm.reminder.clone());

        AlarmEvent {
            id: pending_alarm.id,
            reminder: pending_alarm.alarm.reminder,
            scheduled_for: pending_alarm.alarm.scheduled_for,
            fired_at,
            delayed_by_game: pending_alarm.delayed_by_game,
            sound_path: pending_alarm.alarm.sound_path,
        }
    }
}

pub fn next_alarm_id_after(alarms: &[PendingAlarmSnapshot]) -> AlarmId {
    alarms
        .iter()
        .map(|alarm| alarm.id)
        .max()
        .map(|id| id.saturating_add(1))
        .unwrap_or(1)
}

#[derive(Debug, Error, PartialEq)]
pub enum AlarmTimeError {
    #[error("Use 24-hour time, for example 19:30, 19.30, 19,30, or 1930")]
    InvalidFormat,
    #[error("That local time does not exist on this date")]
    InvalidLocalTime,
    #[error("Could not calculate tomorrow's date")]
    DateOverflow,
}

pub fn next_alarm_time(
    input: &str,
    now: DateTime<Local>,
) -> Result<DateTime<Local>, AlarmTimeError> {
    let time = parse_alarm_time(input)?;
    let today = now.date_naive();
    let mut candidate = local_datetime(today, time)?;

    if candidate <= now {
        let tomorrow = today.succ_opt().ok_or(AlarmTimeError::DateOverflow)?;
        candidate = local_datetime(tomorrow, time)?;
    }

    Ok(candidate)
}

pub fn normalize_alarm_time_text(input: &str) -> Result<String, AlarmTimeError> {
    let time = parse_alarm_time(input)?;

    Ok(time.format("%H.%M").to_string())
}

fn parse_alarm_time(input: &str) -> Result<NaiveTime, AlarmTimeError> {
    let trimmed = input.trim();

    if trimmed.len() == 4 && trimmed.chars().all(|value| value.is_ascii_digit()) {
        let (hour, minute) = trimmed.split_at(2);
        return parse_alarm_time_parts(hour, minute);
    }

    let separator = time_separator(trimmed)?;
    let (hour, minute) = trimmed
        .split_once(separator)
        .ok_or(AlarmTimeError::InvalidFormat)?;

    parse_alarm_time_parts(hour, minute)
}

fn parse_alarm_time_parts(hour: &str, minute: &str) -> Result<NaiveTime, AlarmTimeError> {
    if hour.is_empty()
        || hour.len() > 2
        || minute.len() != 2
        || !hour.chars().all(|value| value.is_ascii_digit())
        || !minute.chars().all(|value| value.is_ascii_digit())
    {
        return Err(AlarmTimeError::InvalidFormat);
    }

    let hour = hour.parse().map_err(|_| AlarmTimeError::InvalidFormat)?;
    let minute = minute.parse().map_err(|_| AlarmTimeError::InvalidFormat)?;

    NaiveTime::from_hms_opt(hour, minute, 0).ok_or(AlarmTimeError::InvalidFormat)
}

fn time_separator(input: &str) -> Result<char, AlarmTimeError> {
    let mut separators = input
        .chars()
        .filter(|value| matches!(value, ':' | '.' | ','));
    let separator = separators.next().ok_or(AlarmTimeError::InvalidFormat)?;

    if separators.next().is_some() {
        return Err(AlarmTimeError::InvalidFormat);
    }

    Ok(separator)
}

fn local_datetime(date: NaiveDate, time: NaiveTime) -> Result<DateTime<Local>, AlarmTimeError> {
    match Local.from_local_datetime(&NaiveDateTime::new(date, time)) {
        LocalResult::Single(value) => Ok(value),
        LocalResult::Ambiguous(earlier, _) => Ok(earlier),
        LocalResult::None => Err(AlarmTimeError::InvalidLocalTime),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn inactive_alarm_fires_when_due() {
        let now = Local::now();
        let mut engine = AlarmEngine::default();
        engine.schedule(Alarm::new("Laundry", now - Duration::seconds(1), true));

        let event = engine
            .tick(now, GameActivity::Inactive)
            .pop()
            .expect("alarm should fire");

        assert_eq!(event.reminder, "Laundry");
        assert!(!event.delayed_by_game);
        assert_eq!(engine.snapshot().status, AlarmStatus::Fired);
    }

    #[test]
    fn active_game_delays_alarm_until_game_is_inactive() {
        let now = Local::now();
        let mut engine = AlarmEngine::default();
        engine.schedule(Alarm::new("Laundry", now - Duration::seconds(1), true));

        assert!(engine.tick(now, GameActivity::Active).is_empty());
        assert_eq!(engine.snapshot().status, AlarmStatus::WaitingForGameEnd);

        let event = engine
            .tick(now + Duration::seconds(10), GameActivity::Inactive)
            .pop()
            .expect("alarm should fire when the game ends");

        assert!(event.delayed_by_game);
        assert_eq!(event.reminder, "Laundry");
    }

    #[test]
    fn unavailable_detection_does_not_block_alarm_before_game_was_seen() {
        let now = Local::now();
        let mut engine = AlarmEngine::default();
        engine.schedule(Alarm::new("Laundry", now - Duration::seconds(1), true));

        let event = engine
            .tick(now, GameActivity::Unavailable)
            .pop()
            .expect("alarm should fire");

        assert_eq!(event.reminder, "Laundry");
        assert!(!event.delayed_by_game);
    }

    #[test]
    fn fired_alarm_includes_configured_sound_path() {
        let now = Local::now();
        let sound_path = PathBuf::from("C:/sounds/alarm.mp3");
        let mut engine = AlarmEngine::default();
        engine.schedule(Alarm::with_sound(
            "Laundry",
            now - Duration::seconds(1),
            true,
            Some(sound_path.clone()),
        ));

        let event = engine
            .tick(now, GameActivity::Inactive)
            .pop()
            .expect("alarm should fire");

        assert_eq!(event.sound_path, Some(sound_path));
    }

    #[test]
    fn scheduling_multiple_alarms_keeps_each_alarm() {
        let now = Local::now();
        let mut engine = AlarmEngine::default();

        let later_id = engine.schedule(Alarm::new("Later", now + Duration::hours(2), true));
        let earlier_id = engine.schedule(Alarm::new("Earlier", now + Duration::hours(1), true));

        let snapshot = engine.snapshot();

        assert_eq!(snapshot.status, AlarmStatus::Scheduled);
        assert_eq!(snapshot.alarms.len(), 2);
        assert_eq!(snapshot.alarms[0].id, earlier_id);
        assert_eq!(snapshot.alarms[1].id, later_id);
    }

    #[test]
    fn cancel_alarm_removes_only_selected_alarm() {
        let now = Local::now();
        let mut engine = AlarmEngine::default();
        let keep_id = engine.schedule(Alarm::new("Keep", now + Duration::hours(1), true));
        let cancel_id = engine.schedule(Alarm::new("Cancel", now + Duration::hours(2), true));

        assert!(engine.cancel_alarm(cancel_id));

        let snapshot = engine.snapshot();
        assert_eq!(snapshot.alarms.len(), 1);
        assert_eq!(snapshot.alarms[0].id, keep_id);
        assert_eq!(snapshot.alarms[0].alarm.reminder, "Keep");
    }

    #[test]
    fn restoring_pending_alarms_keeps_ids_and_next_id() {
        let now = Local::now();
        let mut engine = AlarmEngine::from_pending_alarms(vec![PendingAlarmSnapshot {
            id: 7,
            alarm: Alarm::new("Restored", now + Duration::hours(1), true),
            status: AlarmStatus::Scheduled,
            delayed_by_game: false,
        }]);

        let new_id = engine.schedule(Alarm::new("New", now + Duration::hours(2), true));
        let snapshot = engine.snapshot();

        assert_eq!(new_id, 8);
        assert_eq!(snapshot.alarms[0].id, 7);
        assert_eq!(snapshot.alarms[1].id, 8);
    }

    #[test]
    fn multiple_due_alarms_fire_together() {
        let now = Local::now();
        let mut engine = AlarmEngine::default();
        engine.schedule(Alarm::new("One", now - Duration::seconds(2), true));
        engine.schedule(Alarm::new("Two", now - Duration::seconds(1), true));

        let events = engine.tick(now, GameActivity::Inactive);

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].reminder, "One");
        assert_eq!(events[1].reminder, "Two");
        assert!(engine.snapshot().alarms.is_empty());
    }

    #[test]
    fn next_alarm_time_accepts_hh_mm() {
        let now = Local::now();
        let input = (now + Duration::hours(2)).format("%H:%M").to_string();

        let scheduled = next_alarm_time(&input, now).expect("valid alarm time");

        assert!(scheduled > now);
    }

    #[test]
    fn next_alarm_time_accepts_dot_separator() {
        let now = Local::now();
        let input = (now + Duration::hours(2)).format("%H.%M").to_string();

        let scheduled = next_alarm_time(&input, now).expect("valid alarm time");

        assert!(scheduled > now);
    }

    #[test]
    fn next_alarm_time_accepts_comma_separator() {
        let now = Local::now();
        let input = (now + Duration::hours(2)).format("%H,%M").to_string();

        let scheduled = next_alarm_time(&input, now).expect("valid alarm time");

        assert!(scheduled > now);
    }

    #[test]
    fn next_alarm_time_accepts_four_digits() {
        let now = Local::now();
        let input = (now + Duration::hours(2)).format("%H%M").to_string();

        let scheduled = next_alarm_time(&input, now).expect("valid alarm time");

        assert!(scheduled > now);
    }

    #[test]
    fn alarm_time_normalizes_to_dot_separator() {
        assert_eq!(
            normalize_alarm_time_text("1554").expect("valid alarm time"),
            "15.54"
        );
        assert_eq!(
            normalize_alarm_time_text("15:54").expect("valid alarm time"),
            "15.54"
        );
        assert_eq!(
            normalize_alarm_time_text("15,50").expect("valid alarm time"),
            "15.50"
        );
    }

    #[test]
    fn next_alarm_time_rejects_other_formats() {
        let error = next_alarm_time("7pm", Local::now()).expect_err("format should be rejected");

        assert_eq!(error, AlarmTimeError::InvalidFormat);
    }

    #[test]
    fn alarm_time_rejects_short_digit_input() {
        let error = normalize_alarm_time_text("955").expect_err("format should be rejected");

        assert_eq!(error, AlarmTimeError::InvalidFormat);
    }

    #[test]
    fn alarm_time_rejects_invalid_four_digits() {
        let error = normalize_alarm_time_text("2460").expect_err("format should be rejected");

        assert_eq!(error, AlarmTimeError::InvalidFormat);
    }
}
