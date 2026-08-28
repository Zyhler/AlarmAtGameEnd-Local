use crate::alarm::{
    Alarm, AlarmEngine, AlarmId, AlarmSnapshot, AlarmStatus, AlarmTimeError, PendingAlarmSnapshot,
    next_alarm_id_after, next_alarm_time, normalize_alarm_time_text,
};
use crate::counter_strike::{
    COUNTER_STRIKE_2_GAME_NAME, Cs2GsiConfigStatus, counter_strike_2_gsi_config_status,
    install_counter_strike_2_gsi_config,
};
use crate::game::{GameDetectionConfig, GameStatus};
use crate::league::GAME_NAME as LEAGUE_OF_LEGENDS_GAME_NAME;
use crate::sound::{SoundHandle, sound_path_from_text};
use crate::storage::{
    self, PersistedGraphicsBackendPreference, PersistedLogEntry, PersistedLogLevel, PersistedState,
    PersistedThemePreference,
};
use crate::worker::{MonitorCommand, MonitorEvent, MonitorHandle, MonitorSnapshot};
use chrono::{DateTime, Local};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::time::Duration;

const INLINE_DETECTED_GAME_LIMIT: usize = 4;
const IDLE_REPAINT_INTERVAL: Duration = Duration::from_millis(1000);
const ACCENT_BUTTON_PRESSED_SHRINK: f32 = 1.0;
const MAX_LOG_ENTRIES: usize = 500;

pub struct AlarmApp {
    state: PersistedState,
    monitor: MonitorHandle,
    snapshot: MonitorSnapshot,
    alarm_popups: Vec<AlarmPopup>,
    next_alarm_id: AlarmId,
    next_alarm_popup_id: u64,
    test_sound: Option<SoundHandle>,
    logs: Vec<LogEntry>,
    current_page: AppPage,
    accent_color: egui::Color32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AppPage {
    Alarm,
    Games,
    Log,
    Settings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LogLevel {
    Info,
    Error,
}

impl LogLevel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Info => "Info",
            Self::Error => "Error",
        }
    }

    fn color(self, visuals: &egui::Visuals) -> egui::Color32 {
        match self {
            Self::Info => crate::theme::muted_text_color(visuals),
            Self::Error => crate::theme::error_text_color(visuals),
        }
    }
}

#[derive(Clone, Debug)]
struct LogEntry {
    occurred_at: DateTime<Local>,
    level: LogLevel,
    message: String,
}

#[derive(Debug)]
struct AlarmPopup {
    id: u64,
    reminder: String,
    fired_at: DateTime<Local>,
    delayed_by_game: bool,
    missed: bool,
    sound: Option<SoundHandle>,
}

impl AlarmPopup {
    fn stop_sound(&self) {
        if let Some(sound) = &self.sound {
            sound.stop();
        }
    }
}

impl AlarmApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (mut state, load_error) = match storage::load_state() {
            Ok(state) => (state, None),
            Err(error) => (
                PersistedState::default(),
                Some(format!("Could not load saved state: {error}")),
            ),
        };
        let crash_notice = crate::crash::latest_crash_notice()
            .ok()
            .flatten()
            .filter(|notice| {
                state.last_reported_crash_fingerprint.as_deref()
                    != Some(notice.fingerprint.as_str())
            });
        let logs = log_entries_from_persisted(&state.logs);
        let saved_alarms = std::mem::take(&mut state.alarms);
        let next_alarm_id = next_alarm_id_after(&saved_alarms);
        let now = Local::now();
        let (restored_alarms, missed_alarms) = split_saved_alarms(saved_alarms, now);
        let initial_alarm_snapshot =
            AlarmEngine::from_pending_alarms(restored_alarms.clone()).snapshot();
        state.alarms = restored_alarms.clone();
        let accent_color = crate::theme::system_accent_color();
        crate::theme::apply_app_style(&cc.egui_ctx, state.theme_preference, accent_color);

        let monitor = MonitorHandle::spawn(
            game_detection_config_from_state(&state),
            restored_alarms.clone(),
        );

        let mut app = Self {
            state,
            monitor,
            snapshot: MonitorSnapshot {
                alarm: initial_alarm_snapshot,
                game_status: Default::default(),
                last_checked: now,
            },
            alarm_popups: Vec::new(),
            next_alarm_id,
            next_alarm_popup_id: 1,
            test_sound: None,
            logs,
            current_page: AppPage::Alarm,
            accent_color,
        };

        app.log_info("Ready");
        app.log_info(format!(
            "Graphics backend setting: {}",
            app.state.graphics_backend_preference.label()
        ));

        if !restored_alarms.is_empty() {
            app.log_info(format!(
                "Restored {} pending alarm(s)",
                restored_alarms.len()
            ));
        }

        for missed_alarm in missed_alarms {
            app.show_missed_alarm(missed_alarm);
        }

        if let Some(notice) = crash_notice {
            app.state.last_reported_crash_fingerprint = Some(notice.fingerprint);
            app.log_error(format!(
                "Previous crash recorded: {}. Crash log: {}",
                notice.summary,
                notice.path.display()
            ));
        }

        if let Some(error) = load_error {
            app.log_error(error);
        }

        app
    }

    fn drain_monitor_events(&mut self, ctx: &egui::Context) -> bool {
        let events = self.monitor.drain_events();
        let had_events = !events.is_empty();

        for event in events {
            match event {
                MonitorEvent::Snapshot(snapshot) => {
                    let alarms_changed = self.snapshot.alarm.alarms != snapshot.alarm.alarms;
                    self.snapshot = snapshot;

                    if alarms_changed {
                        self.persist_current_state();
                    }
                }
                MonitorEvent::AlarmFired {
                    id,
                    reminder,
                    delayed_by_game,
                    fired_at,
                    sound,
                } => {
                    self.snapshot.alarm.alarms.retain(|alarm| alarm.id != id);
                    self.snapshot.alarm.fired_at = Some(fired_at);
                    self.snapshot.alarm.last_fired_reminder = Some(reminder.clone());
                    refresh_alarm_snapshot_status(&mut self.snapshot.alarm);

                    let suffix = if delayed_by_game {
                        " after the game ended"
                    } else {
                        ""
                    };
                    let message = format!(
                        "Alarm fired at {}{suffix}: {reminder}",
                        format_datetime(fired_at)
                    );
                    self.log_at(LogLevel::Info, fired_at, message);
                    self.push_alarm_popup(reminder, fired_at, delayed_by_game, false, sound);
                    request_alarm_attention(ctx);
                }
                MonitorEvent::TestAlarmPopup {
                    reminder,
                    fired_at,
                    sound,
                } => {
                    self.log_at(
                        LogLevel::Info,
                        fired_at,
                        format!("Test alarm popup shown at {}", format_time(fired_at)),
                    );
                    self.push_alarm_popup(reminder, fired_at, false, false, sound);
                    request_alarm_attention(ctx);
                }
                MonitorEvent::TestSoundStarted { sound, started_at } => {
                    if let Some(sound) = self.test_sound.take() {
                        sound.stop();
                    }

                    self.test_sound = sound;
                    let message = if self.test_sound.is_some() {
                        format!("Test sound started at {}", format_time(started_at))
                    } else {
                        "Alarm sound is disabled".to_owned()
                    };
                    self.log_at(LogLevel::Info, started_at, message);
                }
                MonitorEvent::NotificationError(error) => {
                    self.log_error(error);
                }
                MonitorEvent::SoundError(error) => {
                    self.log_error(error);
                }
            }
        }

        had_events
    }

    fn push_alarm_popup(
        &mut self,
        reminder: String,
        fired_at: DateTime<Local>,
        delayed_by_game: bool,
        missed: bool,
        sound: Option<SoundHandle>,
    ) {
        let id = self.next_alarm_popup_id;
        self.next_alarm_popup_id = self.next_alarm_popup_id.checked_add(1).unwrap_or(1);

        self.alarm_popups.push(AlarmPopup {
            id,
            reminder,
            fired_at,
            delayed_by_game,
            missed,
            sound,
        });
    }

    fn show_missed_alarm(&mut self, alarm: PendingAlarmSnapshot) {
        let reminder = alarm.alarm.reminder.clone();
        let scheduled_for = alarm.alarm.scheduled_for;

        self.log_at(
            LogLevel::Error,
            Local::now(),
            format!(
                "Missed alarm from {} while the app was not running: {reminder}",
                format_datetime(scheduled_for)
            ),
        );
        self.push_alarm_popup(reminder, scheduled_for, alarm.delayed_by_game, true, None);
    }

    fn clear_finished_test_sound(&mut self) -> bool {
        let finished = self
            .test_sound
            .as_ref()
            .is_some_and(|sound| sound.is_finished());

        if finished {
            self.test_sound = None;
        }

        finished
    }

    fn log_info(&mut self, message: impl Into<String>) {
        self.log_at(LogLevel::Info, Local::now(), message);
    }

    fn log_error(&mut self, message: impl Into<String>) {
        self.log_at(LogLevel::Error, Local::now(), message);
    }

    fn log_at(
        &mut self,
        level: LogLevel,
        occurred_at: DateTime<Local>,
        message: impl Into<String>,
    ) {
        self.push_log(level, occurred_at, message);
        self.persist_current_state();
    }

    fn push_log(
        &mut self,
        level: LogLevel,
        occurred_at: DateTime<Local>,
        message: impl Into<String>,
    ) {
        self.logs.push(LogEntry {
            occurred_at,
            level,
            message: message.into(),
        });

        if self.logs.len() > MAX_LOG_ENTRIES {
            let overflow_count = self.logs.len() - MAX_LOG_ENTRIES;
            self.logs.drain(..overflow_count);
        }
    }

    fn persist_current_state(&mut self) -> bool {
        self.state.logs = persisted_log_entries_from_logs(&self.logs);
        self.state.alarms = self.snapshot.alarm.alarms.clone();

        match storage::save_state(&self.state) {
            Ok(()) => true,
            Err(error) => {
                self.push_log(
                    LogLevel::Error,
                    Local::now(),
                    format!("Could not save app state: {error}"),
                );
                eprintln!("could not save app state: {error}");
                false
            }
        }
    }

    fn apply_appearance(&self, ctx: &egui::Context) {
        crate::theme::apply_app_style(ctx, self.state.theme_preference, self.accent_color);
    }

    fn normalize_alarm_time_field(&mut self) -> Result<(), AlarmTimeError> {
        let normalized = normalize_alarm_time_text(&self.state.alarm_time)?;

        if self.state.alarm_time != normalized {
            self.state.alarm_time = normalized;
            self.persist_current_state();
        }

        Ok(())
    }

    fn schedule_alarm(&mut self) {
        if let Err(error) = self.normalize_alarm_time_field() {
            self.log_error(error.to_string());
            return;
        }

        match next_alarm_time(&self.state.alarm_time, Local::now()) {
            Ok(scheduled_for) => {
                let reminder = if self.state.reminder.trim().is_empty() {
                    "Reminder".to_owned()
                } else {
                    self.state.reminder.trim().to_owned()
                };
                let alarm = Alarm::with_sound(
                    reminder.clone(),
                    scheduled_for,
                    self.state.delay_for_games,
                    sound_path_from_text(&self.state.sound_path),
                );
                let id = self.next_alarm_id;
                self.next_alarm_id = self.next_alarm_id.saturating_add(1);
                self.snapshot.alarm.alarms.push(PendingAlarmSnapshot {
                    id,
                    alarm: alarm.clone(),
                    status: AlarmStatus::Scheduled,
                    delayed_by_game: false,
                });
                sort_alarm_snapshots(&mut self.snapshot.alarm.alarms);
                self.snapshot.alarm.fired_at = None;
                self.snapshot.alarm.last_fired_reminder = None;
                refresh_alarm_snapshot_status(&mut self.snapshot.alarm);

                self.monitor.send(MonitorCommand::Schedule { id, alarm });
                self.log_info(format!(
                    "Alarm added for {}",
                    format_datetime(scheduled_for)
                ));
            }
            Err(error) => {
                self.log_error(error.to_string());
            }
        }
    }

    fn save_settings(&mut self) -> bool {
        self.monitor.send(MonitorCommand::SetGameDetectionConfig(
            game_detection_config_from_state(&self.state),
        ));

        self.persist_current_state()
    }

    fn install_counter_strike_2_gsi_config(&mut self) {
        match gsi_config_path_from_text(&self.state.counter_strike_2_gsi_config_path) {
            Some(path) => {
                match install_counter_strike_2_gsi_config(
                    &path,
                    self.state.counter_strike_2_gsi_port,
                ) {
                    Ok(()) => {
                        self.save_settings();
                        self.log_info(format!("CS2 GSI config installed at {}", path.display()));
                    }
                    Err(error) => {
                        self.log_error(error.to_string());
                    }
                }
            }
            None => {
                self.log_error("CS2 GSI config path is blank");
            }
        }
    }

    fn alarm_summary(&self) -> String {
        let alarm_count = self.snapshot.alarm.alarms.len();

        if let Some(next_alarm) = self.snapshot.alarm.alarms.first() {
            let next_alarm_text = format!(
                "{} at {}",
                next_alarm.alarm.reminder,
                format_datetime(next_alarm.alarm.scheduled_for)
            );

            if alarm_count == 1 {
                next_alarm_text
            } else {
                format!("{alarm_count} alarms scheduled, next: {next_alarm_text}")
            }
        } else if let Some(fired_at) = self.snapshot.alarm.fired_at {
            match &self.snapshot.alarm.last_fired_reminder {
                Some(reminder) => {
                    format!("Last fired at {}: {reminder}", format_datetime(fired_at))
                }
                None => format!("Last fired at {}", format_datetime(fired_at)),
            }
        } else {
            "No alarm scheduled".to_owned()
        }
    }

    fn draw_alarm_list(&mut self, ui: &mut egui::Ui) {
        let alarms = self.snapshot.alarm.alarms.clone();

        ui.separator();
        ui.heading("Scheduled Alarms");

        if alarms.is_empty() {
            ui.label("No alarms scheduled");
            return;
        }

        egui::Grid::new("scheduled_alarms_grid")
            .num_columns(4)
            .striped(true)
            .show(ui, |ui| {
                ui.strong("Time");
                ui.strong("Reminder");
                ui.strong("Status");
                ui.label("");
                ui.end_row();

                for pending_alarm in alarms {
                    ui.label(format_datetime(pending_alarm.alarm.scheduled_for));
                    ui.label(&pending_alarm.alarm.reminder);
                    ui.label(pending_alarm.status.as_str());

                    if accent_button(ui, "Cancel", self.accent_color).clicked() {
                        self.monitor
                            .send(MonitorCommand::CancelAlarm(pending_alarm.id));
                        self.snapshot
                            .alarm
                            .alarms
                            .retain(|alarm| alarm.id != pending_alarm.id);
                        refresh_alarm_snapshot_status(&mut self.snapshot.alarm);
                        self.log_info(format!("Alarm cancelled: {}", pending_alarm.alarm.reminder));
                    }

                    ui.end_row();
                }
            });
    }

    fn alarm_status_label(&self) -> &'static str {
        match self.snapshot.alarm.status {
            AlarmStatus::Idle
            | AlarmStatus::Scheduled
            | AlarmStatus::WaitingForGameEnd
            | AlarmStatus::Fired => self.snapshot.alarm.status.as_str(),
        }
    }

    fn draw_navigation(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.current_page, AppPage::Alarm, "Alarm");
            ui.selectable_value(&mut self.current_page, AppPage::Games, "Games");
            ui.selectable_value(&mut self.current_page, AppPage::Log, "Log");
            ui.selectable_value(&mut self.current_page, AppPage::Settings, "Settings");
        });
    }

    fn draw_detected_games_summary(&mut self, ui: &mut egui::Ui) {
        let statuses = self.snapshot.game_status.statuses.clone();

        if statuses.is_empty() {
            ui.label("No game detectors configured");
            return;
        }

        ui.vertical(|ui| {
            for status in statuses.iter().take(INLINE_DETECTED_GAME_LIMIT) {
                draw_compact_game_status(ui, status);
            }

            if detected_games_overflow_count(statuses.len()).is_some()
                && ui
                    .link(format!("View all {} detected games", statuses.len()))
                    .clicked()
            {
                self.current_page = AppPage::Games;
            }
        });
    }

    fn draw_status_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading(crate::APP_NAME);
        ui.separator();

        egui::Grid::new("status_grid")
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                ui.label("Alarm");
                ui.label(self.alarm_status_label());
                ui.end_row();

                ui.label("Reminder");
                ui.label(self.alarm_summary());
                ui.end_row();

                ui.label("Detected games");
                self.draw_detected_games_summary(ui);
                ui.end_row();

                ui.label("Checked");
                ui.label(format_datetime(self.snapshot.last_checked));
                ui.end_row();
            });
    }

    fn draw_alarm_panel(&mut self, ui: &mut egui::Ui) {
        self.draw_status_panel(ui);
        ui.separator();

        ui.horizontal(|ui| {
            ui.label("Reminder");
            let response = text_input(
                ui,
                self.accent_color,
                "reminder_input",
                &mut self.state.reminder,
                260.0,
                "ex: go get laundry",
            );

            if response.changed() {
                self.persist_current_state();
            }
        });
        ui.horizontal(|ui| {
            ui.label("Time");
            let response = text_input(
                ui,
                self.accent_color,
                "alarm_time_input",
                &mut self.state.alarm_time,
                90.0,
                "ex: 1554",
            );

            if response.changed() {
                self.persist_current_state();
            }

            let enter_pressed =
                response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));

            if enter_pressed {
                self.schedule_alarm();
            } else if response.lost_focus() && !self.state.alarm_time.trim().is_empty() {
                let _ = self.normalize_alarm_time_field();
            }

            ui.colored_label(
                crate::theme::muted_text_color(ui.visuals()),
                "HH.MM, HH:MM, HH,MM, or 1554",
            );
        });
        if readable_checkbox(
            ui,
            &mut self.state.delay_for_games,
            self.accent_color,
            "Delay while a tracked game is active",
        )
        .changed()
        {
            if self.save_settings() {
                self.log_info("Game delay setting saved");
            }
        }

        ui.horizontal(|ui| {
            if accent_button(ui, "Set Alarm", self.accent_color).clicked() {
                self.schedule_alarm();
            }

            if accent_button(ui, "Cancel All", self.accent_color).clicked() {
                self.monitor.send(MonitorCommand::Cancel);
                self.snapshot.alarm.alarms.clear();
                self.snapshot.alarm.fired_at = None;
                self.snapshot.alarm.last_fired_reminder = None;
                refresh_alarm_snapshot_status(&mut self.snapshot.alarm);
                self.log_info("All alarms cancelled");
            }
        });

        self.draw_alarm_list(ui);
    }

    fn draw_settings_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Appearance");
        if self.draw_theme_picker(ui) {
            self.apply_appearance(ui.ctx());
            self.log_info("Appearance setting saved");
        }
        if self.draw_graphics_backend_picker(ui) {
            self.log_info("Graphics backend setting saved; restart required");
        }
        ui.label(
            egui::RichText::new(
                "Changing this requires restarting the app. Try OpenGL if the desktop feels laggy while this app is focused.",
            )
            .color(crate::theme::muted_text_color(ui.visuals())),
        );

        ui.separator();
        ui.heading("Alarm Settings");
        if file_path_row(
            ui,
            self.accent_color,
            "Alarm sound",
            "alarm_sound_path",
            &mut self.state.sound_path,
            FilePickerKind::Audio,
        ) {
            self.persist_current_state();
        }
        ui.label(
            egui::RichText::new(
                "Default sound is bundled. Custom files: MP3, WAV, OGG/Vorbis, FLAC, MP4/M4A/AAC. Leave blank to disable alarm sound.",
            )
            .color(crate::theme::muted_text_color(ui.visuals())),
        );

        ui.horizontal(|ui| {
            if accent_button(ui, "Test Alarm Popup", self.accent_color).clicked() {
                if let Some(sound) = self.test_sound.take() {
                    sound.stop();
                }

                self.log_info("Starting test alarm popup");
                self.monitor
                    .send(MonitorCommand::TestAlarmPopup(sound_path_from_text(
                        &self.state.sound_path,
                    )));
            }

            if accent_button(ui, "Test Sound", self.accent_color).clicked() {
                if let Some(sound) = self.test_sound.take() {
                    sound.stop();
                }

                self.log_info("Starting test sound");
                self.monitor
                    .send(MonitorCommand::TestSound(sound_path_from_text(
                        &self.state.sound_path,
                    )));
            }

            if self.test_sound.is_some()
                && accent_button(ui, "Stop Test Sound", self.accent_color).clicked()
            {
                if let Some(sound) = self.test_sound.take() {
                    sound.stop();
                }

                self.log_info("Test sound stopped");
            }
        });

        ui.separator();
        if accent_button(ui, "Save Settings", self.accent_color).clicked() && self.save_settings() {
            self.log_info("Settings saved");
        }
    }

    fn draw_theme_picker(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;

        ui.horizontal_wrapped(|ui| {
            ui.label("Theme");
            changed |= ui
                .selectable_value(
                    &mut self.state.theme_preference,
                    PersistedThemePreference::System,
                    "Follow Windows",
                )
                .changed();
            changed |= ui
                .selectable_value(
                    &mut self.state.theme_preference,
                    PersistedThemePreference::Dark,
                    "Dark",
                )
                .changed();
            changed |= ui
                .selectable_value(
                    &mut self.state.theme_preference,
                    PersistedThemePreference::Light,
                    "Light",
                )
                .changed();
        });

        changed
    }

    fn draw_graphics_backend_picker(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;

        ui.horizontal_wrapped(|ui| {
            ui.label("Graphics backend");
            changed |= ui
                .selectable_value(
                    &mut self.state.graphics_backend_preference,
                    PersistedGraphicsBackendPreference::Auto,
                    "Auto",
                )
                .changed();
            changed |= ui
                .selectable_value(
                    &mut self.state.graphics_backend_preference,
                    PersistedGraphicsBackendPreference::Wgpu,
                    "WGPU",
                )
                .changed();
            changed |= ui
                .selectable_value(
                    &mut self.state.graphics_backend_preference,
                    PersistedGraphicsBackendPreference::OpenGl,
                    "OpenGL",
                )
                .changed();
        });

        changed
    }

    fn draw_games_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Detected Games");
        self.draw_league_game_section(ui);
        ui.add_space(8.0);
        self.draw_counter_strike_2_game_section(ui);

        ui.separator();
        if accent_button(ui, "Save Game Settings", self.accent_color).clicked()
            && self.save_settings()
        {
            self.log_info("Game settings saved");
        }
    }

    fn draw_log_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Log");

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if accent_button(ui, "Clear", self.accent_color).clicked() {
                    self.logs.clear();
                    self.persist_current_state();
                }
            });
        });

        ui.separator();

        if self.logs.is_empty() {
            ui.label("No log entries");
            return;
        }

        egui::Grid::new("log_grid")
            .num_columns(3)
            .striped(true)
            .show(ui, |ui| {
                ui.strong("Time");
                ui.strong("Level");
                ui.strong("Message");
                ui.end_row();

                for entry in self.logs.iter().rev() {
                    ui.label(format_datetime(entry.occurred_at));
                    ui.colored_label(entry.level.color(ui.visuals()), entry.level.as_str());
                    ui.label(&entry.message);
                    ui.end_row();
                }
            });
    }

    fn draw_league_game_section(&mut self, ui: &mut egui::Ui) {
        let status = self.game_status(LEAGUE_OF_LEGENDS_GAME_NAME).cloned();

        draw_game_section(ui, LEAGUE_OF_LEGENDS_GAME_NAME, status.as_ref(), |ui| {
            if file_path_row(
                ui,
                self.accent_color,
                "Lockfile",
                "league_lockfile_path",
                &mut self.state.lockfile_path,
                FilePickerKind::Any,
            ) {
                self.save_settings();
            }

            ui.horizontal(|ui| {
                ui.label("Gameflow phase");
                ui.label(status_detail_or_dash(status.as_ref()));
            });
        });
    }

    fn draw_counter_strike_2_game_section(&mut self, ui: &mut egui::Ui) {
        let status = self.game_status(COUNTER_STRIKE_2_GAME_NAME).cloned();
        let gsi_config_path =
            gsi_config_path_from_text(&self.state.counter_strike_2_gsi_config_path);
        let gsi_config_status = counter_strike_2_gsi_config_status(
            gsi_config_path.as_deref(),
            self.state.counter_strike_2_gsi_port,
        );
        let gsi_is_working = counter_strike_2_gsi_is_working(status.as_ref());
        let mut install_clicked = false;

        draw_game_section(ui, COUNTER_STRIKE_2_GAME_NAME, status.as_ref(), |ui| {
            if !gsi_is_working
                && gsi_config_status.needs_install_action()
                && accent_button(
                    ui,
                    gsi_config_status.install_button_label(),
                    self.accent_color,
                )
                .clicked()
            {
                install_clicked = true;
            }
            ui.label(counter_strike_2_setup_message(
                gsi_config_status,
                gsi_is_working,
            ));

            ui.horizontal(|ui| {
                ui.label("GSI port");
                let response = number_input(
                    ui,
                    self.accent_color,
                    egui::DragValue::new(&mut self.state.counter_strike_2_gsi_port)
                        .range(1024..=65535)
                        .speed(1.0),
                );

                if response.changed() {
                    self.save_settings();
                }
            });

            if file_path_row(
                ui,
                self.accent_color,
                "GSI config",
                "cs2_gsi_config_path",
                &mut self.state.counter_strike_2_gsi_config_path,
                FilePickerKind::Config,
            ) {
                self.persist_current_state();
            }
        });

        if install_clicked {
            self.install_counter_strike_2_gsi_config();
        }
    }

    fn game_status(&self, game: &'static str) -> Option<&GameStatus> {
        self.snapshot
            .game_status
            .statuses
            .iter()
            .find(|status| game_status_name(status) == game)
    }

    fn draw_current_page(&mut self, ui: &mut egui::Ui) {
        self.draw_navigation(ui);
        ui.separator();

        match self.current_page {
            AppPage::Alarm => self.draw_alarm_panel(ui),
            AppPage::Games => self.draw_games_panel(ui),
            AppPage::Log => self.draw_log_panel(ui),
            AppPage::Settings => self.draw_settings_panel(ui),
        }
    }

    fn dismiss_all_alarm_popups(&mut self) {
        for popup in &self.alarm_popups {
            popup.stop_sound();
        }

        self.alarm_popups.clear();
    }

    fn dismiss_alarm_popup(&mut self, id: u64) {
        if let Some(popup) = self.alarm_popups.iter().find(|popup| popup.id == id) {
            popup.stop_sound();
        }

        self.alarm_popups.retain(|popup| popup.id != id);
    }

    fn draw_alarm_popups(&mut self, ctx: &egui::Context) {
        if self.alarm_popups.is_empty() {
            return;
        }

        let mut dismissed_alarm = None;
        let mut dismiss_all = false;

        egui::Area::new(egui::Id::new("alarm_popups"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-16.0, 16.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(360.0);

                    ui.horizontal(|ui| {
                        ui.heading("Alarm");

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if accent_button(ui, "Dismiss all", self.accent_color).clicked() {
                                dismiss_all = true;
                            }
                        });
                    });

                    ui.separator();

                    for popup in &self.alarm_popups {
                        ui.group(|ui| {
                            ui.set_width(ui.available_width());
                            if popup.missed {
                                ui.strong(format!("Missed alarm: {}", popup.reminder));
                                ui.label(format!("Was due at {}", format_datetime(popup.fired_at)));
                                ui.label("No sound was played.");
                            } else {
                                ui.strong(&popup.reminder);
                                ui.label(format!("Fired at {}", format_datetime(popup.fired_at)));
                            }

                            if popup.delayed_by_game {
                                ui.label("Delayed until the game ended");
                            }

                            ui.horizontal(|ui| {
                                if accent_button(ui, "Dismiss", self.accent_color).clicked() {
                                    dismissed_alarm = Some(popup.id);
                                }
                            });
                        });
                    }
                });
            });

        if dismiss_all {
            self.dismiss_all_alarm_popups();
        } else if let Some(id) = dismissed_alarm {
            self.dismiss_alarm_popup(id);
        }
    }
}

impl eframe::App for AlarmApp {
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let state_changed = self.drain_monitor_events(ctx) | self.clear_finished_test_sound();

        if state_changed {
            ctx.request_repaint();
        }

        ctx.request_repaint_after(IDLE_REPAINT_INTERVAL);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let state_changed = self.drain_monitor_events(ui.ctx()) | self.clear_finished_test_sound();

        if state_changed {
            ui.ctx().request_repaint();
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            self.draw_current_page(ui);
        });

        self.draw_alarm_popups(ui.ctx());
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FilePickerKind {
    Any,
    Audio,
    Config,
}

fn lockfile_override_from_text(value: &str) -> Option<PathBuf> {
    let trimmed = value.trim();

    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

fn gsi_config_path_from_text(value: &str) -> Option<PathBuf> {
    let trimmed = value.trim();

    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

fn game_detection_config_from_state(state: &PersistedState) -> GameDetectionConfig {
    GameDetectionConfig {
        league_lockfile_override: lockfile_override_from_text(&state.lockfile_path),
        counter_strike_2_gsi_port: state.counter_strike_2_gsi_port,
    }
}

fn log_entries_from_persisted(entries: &[PersistedLogEntry]) -> Vec<LogEntry> {
    let mut logs = entries
        .iter()
        .rev()
        .take(MAX_LOG_ENTRIES)
        .map(|entry| LogEntry {
            occurred_at: entry.occurred_at,
            level: entry.level.into(),
            message: entry.message.clone(),
        })
        .collect::<Vec<_>>();
    logs.reverse();
    logs
}

fn persisted_log_entries_from_logs(logs: &[LogEntry]) -> Vec<PersistedLogEntry> {
    logs.iter()
        .rev()
        .take(MAX_LOG_ENTRIES)
        .map(|entry| PersistedLogEntry {
            occurred_at: entry.occurred_at,
            level: entry.level.into(),
            message: entry.message.clone(),
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn split_saved_alarms(
    alarms: Vec<PendingAlarmSnapshot>,
    now: DateTime<Local>,
) -> (Vec<PendingAlarmSnapshot>, Vec<PendingAlarmSnapshot>) {
    let (mut restored, mut missed): (Vec<_>, Vec<_>) = alarms
        .into_iter()
        .partition(|alarm| alarm.alarm.scheduled_for > now);

    restored.retain(|alarm| alarm.status != AlarmStatus::Fired);
    sort_alarm_snapshots(&mut restored);
    sort_alarm_snapshots(&mut missed);

    (restored, missed)
}

fn sort_alarm_snapshots(alarms: &mut [PendingAlarmSnapshot]) {
    alarms.sort_by(|left, right| {
        left.alarm
            .scheduled_for
            .cmp(&right.alarm.scheduled_for)
            .then(left.id.cmp(&right.id))
    });
}

fn refresh_alarm_snapshot_status(snapshot: &mut AlarmSnapshot) {
    snapshot.status = if snapshot
        .alarms
        .iter()
        .any(|alarm| alarm.status == AlarmStatus::WaitingForGameEnd)
    {
        AlarmStatus::WaitingForGameEnd
    } else if !snapshot.alarms.is_empty() {
        AlarmStatus::Scheduled
    } else if snapshot.fired_at.is_some() {
        AlarmStatus::Fired
    } else {
        AlarmStatus::Idle
    };
}

impl From<PersistedLogLevel> for LogLevel {
    fn from(level: PersistedLogLevel) -> Self {
        match level {
            PersistedLogLevel::Info => Self::Info,
            PersistedLogLevel::Error => Self::Error,
        }
    }
}

impl From<LogLevel> for PersistedLogLevel {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Info => Self::Info,
            LogLevel::Error => Self::Error,
        }
    }
}

struct GameStatusRow<'a> {
    game: &'static str,
    state: &'static str,
    detail: Option<&'a str>,
    color: egui::Color32,
}

fn draw_compact_game_status(ui: &mut egui::Ui, status: &GameStatus) {
    let row = game_status_row(status, ui.visuals());

    ui.horizontal_wrapped(|ui| {
        ui.colored_label(row.color, row.state);
        ui.label(row.game);

        if let Some(detail) = row.detail {
            ui.label(format!("({detail})"));
        }
    });
}

fn draw_game_section(
    ui: &mut egui::Ui,
    title: &'static str,
    status: Option<&GameStatus>,
    add_settings: impl FnOnce(&mut egui::Ui),
) {
    ui.group(|ui| {
        ui.set_width(ui.available_width());

        ui.horizontal_wrapped(|ui| {
            ui.heading(title);

            if let Some(status) = status {
                let row = game_status_row(status, ui.visuals());
                ui.colored_label(row.color, row.state);
            }
        });

        if let Some(status) = status {
            let detail = status_detail(status);

            if !detail.is_empty() {
                ui.label(detail);
            }
        }

        ui.separator();
        add_settings(ui);
    });
}

fn game_status_name(status: &GameStatus) -> &'static str {
    match status {
        GameStatus::Active { game, .. }
        | GameStatus::Inactive { game, .. }
        | GameStatus::Unavailable { game, .. } => game,
    }
}

fn status_detail_or_dash(status: Option<&GameStatus>) -> String {
    status
        .map(status_detail)
        .filter(|detail| !detail.is_empty())
        .unwrap_or_else(|| "-".to_owned())
}

fn status_detail(status: &GameStatus) -> String {
    match status {
        GameStatus::Active { detail, .. } | GameStatus::Inactive { detail, .. } => {
            detail.clone().unwrap_or_default()
        }
        GameStatus::Unavailable { message, .. } => message.clone(),
    }
}

fn counter_strike_2_gsi_is_working(status: Option<&GameStatus>) -> bool {
    match status {
        Some(GameStatus::Active { game, .. }) if *game == COUNTER_STRIKE_2_GAME_NAME => true,
        Some(GameStatus::Inactive {
            game,
            detail: Some(detail),
        }) if *game == COUNTER_STRIKE_2_GAME_NAME => {
            detail != "no recent GSI updates" && !detail.starts_with("waiting for GSI updates")
        }
        _ => false,
    }
}

fn counter_strike_2_setup_message(
    config_status: Cs2GsiConfigStatus,
    gsi_is_working: bool,
) -> &'static str {
    if gsi_is_working {
        "CS2 GSI is working; alarms only wait during active matches."
    } else if config_status == Cs2GsiConfigStatus::Installed {
        "CS2 GSI config is installed; restart CS2 if it was already open."
    } else {
        config_status.message()
    }
}

fn game_status_row<'a>(status: &'a GameStatus, visuals: &egui::Visuals) -> GameStatusRow<'a> {
    match status {
        GameStatus::Active { game, detail } => GameStatusRow {
            game,
            state: "Active",
            detail: detail.as_deref(),
            color: egui::Color32::from_rgb(80, 170, 95),
        },
        GameStatus::Inactive { game, detail } => GameStatusRow {
            game,
            state: "Inactive",
            detail: detail.as_deref(),
            color: crate::theme::muted_text_color(visuals),
        },
        GameStatus::Unavailable { game, message } => GameStatusRow {
            game,
            state: "Unavailable",
            detail: Some(message),
            color: egui::Color32::from_rgb(200, 145, 45),
        },
    }
}

fn detected_games_overflow_count(game_count: usize) -> Option<usize> {
    game_count
        .checked_sub(INLINE_DETECTED_GAME_LIMIT)
        .filter(|overflow_count| *overflow_count > 0)
}

fn with_input_visuals<R>(
    ui: &mut egui::Ui,
    accent_color: egui::Color32,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.scope(|ui| {
        let visuals = &mut ui.style_mut().visuals;
        let dark_mode = visuals.dark_mode;
        let (inactive_fill, inactive_stroke, hovered_fill, hovered_stroke, active_fill) =
            if dark_mode {
                (
                    egui::Color32::from_gray(28),
                    egui::Color32::from_gray(95),
                    egui::Color32::from_gray(36),
                    egui::Color32::from_gray(155),
                    egui::Color32::from_gray(40),
                )
            } else {
                (
                    egui::Color32::from_gray(250),
                    egui::Color32::from_gray(165),
                    egui::Color32::from_gray(245),
                    egui::Color32::from_gray(95),
                    egui::Color32::WHITE,
                )
            };
        let text_color = if dark_mode {
            egui::Color32::from_gray(235)
        } else {
            egui::Color32::from_gray(30)
        };

        visuals.text_edit_bg_color = Some(inactive_fill);
        visuals.widgets.inactive.bg_fill = inactive_fill;
        visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, inactive_stroke);
        visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, text_color);
        visuals.widgets.hovered.bg_fill = hovered_fill;
        visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.25, hovered_stroke);
        visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, text_color);
        visuals.widgets.active.bg_fill = active_fill;
        visuals.widgets.active.bg_stroke = egui::Stroke::new(1.5, accent_color);
        visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, text_color);

        add_contents(ui)
    })
    .inner
}

fn file_path_row(
    ui: &mut egui::Ui,
    accent_color: egui::Color32,
    label: &'static str,
    id_salt: &'static str,
    value: &mut String,
    picker_kind: FilePickerKind,
) -> bool {
    let mut changed = false;

    ui.horizontal(|ui| {
        ui.label(label);

        if file_path_text_edit(ui, accent_color, id_salt, value).changed() {
            changed = true;
        }

        if accent_button(ui, "Find file", accent_color).clicked()
            && let Some(path) = browse_for_file(value, picker_kind)
        {
            *value = path.to_string_lossy().into_owned();
            changed = true;
        }
    });

    changed
}

fn text_input(
    ui: &mut egui::Ui,
    accent_color: egui::Color32,
    id_salt: impl egui::AsIdSalt,
    value: &mut String,
    desired_width: f32,
    hint_text: &'static str,
) -> egui::Response {
    with_input_visuals(ui, accent_color, |ui| {
        let id = ui.make_persistent_id(id_salt);
        let has_focus = ui.memory(|memory| memory.has_focus(id));
        let hint_text = if has_focus { "" } else { hint_text };

        ui.add(
            egui::TextEdit::singleline(value)
                .desired_width(desired_width)
                .id(id)
                .hint_text(hint_text),
        )
    })
}

fn file_path_text_edit(
    ui: &mut egui::Ui,
    accent_color: egui::Color32,
    id_salt: impl egui::AsIdSalt,
    value: &mut String,
) -> egui::Response {
    text_input(ui, accent_color, id_salt, value, 360.0, "File path")
}

fn number_input(
    ui: &mut egui::Ui,
    accent_color: egui::Color32,
    value: egui::DragValue<'_>,
) -> egui::Response {
    with_input_visuals(ui, accent_color, |ui| ui.add(value))
}

fn accent_button(
    ui: &mut egui::Ui,
    label: &'static str,
    accent_color: egui::Color32,
) -> egui::Response {
    ui.scope(|ui| {
        let dark_mode = ui.visuals().dark_mode;
        let text_color = crate::theme::contrast_text_color(accent_color);
        let hovered_fill = crate::theme::accent_hover_color(accent_color, dark_mode);
        let active_fill = crate::theme::accent_active_color(accent_color, dark_mode);

        let visuals = &mut ui.style_mut().visuals;
        visuals.widgets.inactive.weak_bg_fill = accent_color;
        visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, hovered_fill);
        visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, text_color);
        visuals.widgets.inactive.expansion = 0.0;
        visuals.widgets.hovered.weak_bg_fill = hovered_fill;
        visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, active_fill);
        visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, text_color);
        visuals.widgets.hovered.expansion = 0.0;
        visuals.widgets.active.weak_bg_fill = active_fill;
        visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, active_fill);
        visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, text_color);
        visuals.widgets.active.expansion = -ACCENT_BUTTON_PRESSED_SHRINK;

        ui.add(egui::Button::new(label).min_size(egui::vec2(0.0, ui.spacing().interact_size.y)))
    })
    .inner
}

fn readable_checkbox(
    ui: &mut egui::Ui,
    value: &mut bool,
    accent_color: egui::Color32,
    label: &'static str,
) -> egui::Response {
    ui.horizontal(|ui| {
        let checkbox_response = with_checkbox_visuals(ui, accent_color, |ui| {
            ui.add(egui::Checkbox::without_text(value))
        });
        let label_response = ui.add(
            egui::Label::new(
                egui::RichText::new(label).color(crate::theme::main_text_color(ui.visuals())),
            )
            .sense(egui::Sense::click()),
        );
        let label_clicked = label_response.clicked();
        let mut response = checkbox_response.union(label_response);

        if label_clicked {
            *value = !*value;
            response.mark_changed();
        }

        response
    })
    .inner
}

fn with_checkbox_visuals<R>(
    ui: &mut egui::Ui,
    accent_color: egui::Color32,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.scope(|ui| {
        let dark_mode = ui.visuals().dark_mode;
        let inactive_fill = if dark_mode {
            egui::Color32::from_gray(28)
        } else {
            egui::Color32::from_gray(250)
        };
        let inactive_stroke = if dark_mode {
            egui::Color32::from_gray(95)
        } else {
            egui::Color32::from_gray(165)
        };
        let hovered_fill = if dark_mode {
            egui::Color32::from_gray(36)
        } else {
            egui::Color32::from_gray(245)
        };
        let active_fill = crate::theme::accent_active_color(accent_color, dark_mode);

        let visuals = &mut ui.style_mut().visuals;
        visuals.widgets.inactive.bg_fill = inactive_fill;
        visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, inactive_stroke);
        visuals.widgets.inactive.fg_stroke = egui::Stroke::new(2.0, accent_color);
        visuals.widgets.hovered.bg_fill = hovered_fill;
        visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.25, accent_color);
        visuals.widgets.hovered.fg_stroke = egui::Stroke::new(2.0, accent_color);
        visuals.widgets.active.bg_fill = active_fill;
        visuals.widgets.active.bg_stroke = egui::Stroke::new(1.25, active_fill);
        visuals.widgets.active.fg_stroke =
            egui::Stroke::new(2.0, crate::theme::contrast_text_color(active_fill));

        add_contents(ui)
    })
    .inner
}

fn browse_for_file(current_value: &str, picker_kind: FilePickerKind) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new();

    if let Some(directory) = existing_dialog_directory(current_value) {
        dialog = dialog.set_directory(directory);
    }

    dialog = match picker_kind {
        FilePickerKind::Any => dialog,
        FilePickerKind::Audio => {
            dialog.add_filter("Audio", &["mp3", "wav", "ogg", "flac", "mp4", "m4a", "aac"])
        }
        FilePickerKind::Config => dialog.add_filter("Config", &["cfg"]),
    };

    dialog.pick_file()
}

fn existing_dialog_directory(value: &str) -> Option<PathBuf> {
    let trimmed = value.trim();

    if trimmed.is_empty() {
        return None;
    }

    let path = Path::new(trimmed);

    if path.is_dir() {
        return Some(path.to_path_buf());
    }

    path.parent()
        .filter(|parent| parent.is_dir())
        .map(Path::to_path_buf)
}

fn request_alarm_attention(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
        egui::UserAttentionType::Critical,
    ));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
}

fn format_datetime(value: DateTime<Local>) -> String {
    value.format("%Y-%m-%d %H:%M:%S").to_string()
}

fn format_time(value: DateTime<Local>) -> String {
    value.format("%H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_lockfile_override_is_ignored() {
        assert_eq!(lockfile_override_from_text("   "), None);
    }

    #[test]
    fn lockfile_override_keeps_entered_path() {
        assert_eq!(
            lockfile_override_from_text("C:/Riot Games/League of Legends/lockfile"),
            Some(PathBuf::from("C:/Riot Games/League of Legends/lockfile"))
        );
    }

    #[test]
    fn blank_gsi_config_path_is_ignored() {
        assert_eq!(gsi_config_path_from_text("   "), None);
    }

    #[test]
    fn gsi_config_path_keeps_entered_path() {
        assert_eq!(
            gsi_config_path_from_text("D:/Steam/cfg/gamestate_integration_alarm_at_game_end.cfg"),
            Some(PathBuf::from(
                "D:/Steam/cfg/gamestate_integration_alarm_at_game_end.cfg"
            ))
        );
    }

    #[test]
    fn detected_games_overflow_starts_after_inline_limit() {
        assert_eq!(detected_games_overflow_count(4), None);
        assert_eq!(detected_games_overflow_count(5), Some(1));
    }

    #[test]
    fn counter_strike_2_gsi_is_working_when_active() {
        assert!(counter_strike_2_gsi_is_working(Some(&GameStatus::active(
            COUNTER_STRIKE_2_GAME_NAME,
            Some("de_dust2 (live)".to_owned())
        ))));
    }

    #[test]
    fn counter_strike_2_gsi_is_not_working_without_recent_updates() {
        assert!(!counter_strike_2_gsi_is_working(Some(
            &GameStatus::inactive(
                COUNTER_STRIKE_2_GAME_NAME,
                Some("waiting for GSI updates on 127.0.0.1:31990".to_owned())
            )
        )));
        assert!(!counter_strike_2_gsi_is_working(Some(
            &GameStatus::inactive(
                COUNTER_STRIKE_2_GAME_NAME,
                Some("no recent GSI updates".to_owned())
            )
        )));
    }

    #[test]
    fn saved_alarms_are_split_into_restored_and_missed() {
        let now = Local::now();
        let past = PendingAlarmSnapshot {
            id: 1,
            alarm: Alarm::new("Past", now - chrono::Duration::minutes(1), true),
            status: AlarmStatus::Scheduled,
            delayed_by_game: false,
        };
        let future = PendingAlarmSnapshot {
            id: 2,
            alarm: Alarm::new("Future", now + chrono::Duration::minutes(1), true),
            status: AlarmStatus::Scheduled,
            delayed_by_game: false,
        };

        let (restored, missed) = split_saved_alarms(vec![future.clone(), past.clone()], now);

        assert_eq!(restored, vec![future]);
        assert_eq!(missed, vec![past]);
    }

    #[test]
    fn persisted_log_entries_keep_last_entries() {
        let now = Local::now();
        let logs = (0..(MAX_LOG_ENTRIES + 5))
            .map(|index| LogEntry {
                occurred_at: now,
                level: LogLevel::Info,
                message: format!("entry {index}"),
            })
            .collect::<Vec<_>>();

        let persisted = persisted_log_entries_from_logs(&logs);

        assert_eq!(persisted.len(), MAX_LOG_ENTRIES);
        assert_eq!(persisted.first().expect("first log").message, "entry 5");
        assert_eq!(
            persisted.last().expect("last log").message,
            format!("entry {}", MAX_LOG_ENTRIES + 4)
        );
    }
}
