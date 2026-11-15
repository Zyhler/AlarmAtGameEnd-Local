use crate::alarm::{
    Alarm, AlarmId, AlarmStatus, AlarmTimeError, PendingAlarmSnapshot, next_alarm_time,
    normalize_alarm_time_text,
};
use crate::alarm_coordination::AlarmCoordination;
use crate::counter_strike::{
    COUNTER_STRIKE_2_GAME_NAME, Cs2GsiConfigStatus, counter_strike_2_gsi_config_status,
    install_counter_strike_2_gsi_config,
};
use crate::discord_bridge::{self, DiscordBridgeSettings, InviteStatus};
use crate::discord_session::{
    DiscordSession, MessageKind, SessionTime, discord_invite_user_label, local_datetime_from_unix,
};
use crate::game::{GameDetectionConfig, GameStatus};
use crate::league::GAME_NAME as LEAGUE_OF_LEGENDS_GAME_NAME;
use crate::sound::{SoundHandle, SoundPlaybackSettings, sound_path_from_text};
use crate::storage::{
    self, PersistedGraphicsBackendPreference, PersistedLogEntry, PersistedLogLevel, PersistedState,
    PersistedThemePreference,
};
use crate::worker::{MonitorCommand, MonitorEvent, MonitorHandle};
use chrono::{DateTime, Local};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const INLINE_DETECTED_GAME_LIMIT: usize = 4;
const IDLE_REPAINT_INTERVAL: Duration = Duration::from_millis(1000);
const ACCENT_BUTTON_PRESSED_SHRINK: f32 = 1.0;
const MAX_LOG_ENTRIES: usize = 500;
const PAGE_ROW_SPACING_Y: f32 = 8.0;
const PAGE_CONTROL_HEIGHT: f32 = 32.0;
const TEXT_INPUT_HORIZONTAL_MARGIN: i8 = 4;
const TEXT_INPUT_MIN_VERTICAL_MARGIN: i8 = 2;
const MAX_APP_NOTICES: usize = 3;
const APP_NOTICE_TTL: Duration = Duration::from_secs(4);
const APP_NOTICE_REPAINT_INTERVAL: Duration = Duration::from_millis(250);
const APP_NOTICE_WIDTH: f32 = 340.0;
const ALARM_VOLUME_SLIDER_WIDTH: f32 = 240.0;
const ALARM_VOLUME_VALUE_WIDTH: f32 = 56.0;

pub struct AlarmApp {
    state: PersistedState,
    state_path: PathBuf,
    state_writes_enabled: bool,
    monitor: MonitorHandle,
    alarms: AlarmCoordination,
    alarm_popups: Vec<AlarmPopup>,
    next_alarm_popup_id: u64,
    test_sound: Option<SoundHandle>,
    logs: Vec<LogEntry>,
    app_notices: Vec<AppNotice>,
    current_page: AppPage,
    accent_color: egui::Color32,
    discord: DiscordSession,
    new_allowed_requester_id: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AppPage {
    Alarm,
    Games,
    Discord,
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

#[derive(Clone, Debug)]
struct AppNotice {
    level: LogLevel,
    message: String,
    created_at: Instant,
}

#[derive(Debug)]
struct AlarmPopup {
    id: u64,
    label: String,
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

#[derive(Clone, Copy, Debug)]
struct AlarmPopupPalette {
    shell_fill: egui::Color32,
    card_fill: egui::Color32,
    border: egui::Color32,
    accent: egui::Color32,
    title: egui::Color32,
    body: egui::Color32,
    muted: egui::Color32,
}

#[derive(Clone, Copy, Debug)]
struct AppNoticePalette {
    fill: egui::Color32,
    border: egui::Color32,
    accent: egui::Color32,
    title: egui::Color32,
    body: egui::Color32,
}

impl AlarmApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut app = Self::from_state_path(storage::state_path(), MonitorHandle::spawn);
        crate::theme::apply_app_style(&cc.egui_ctx, app.state.theme_preference, app.accent_color);
        if let Ok(Some(notice)) = crate::crash::latest_crash_notice()
            && app.state.last_reported_crash_fingerprint.as_deref()
                != Some(notice.fingerprint.as_str())
        {
            app.state.last_reported_crash_fingerprint = Some(notice.fingerprint);
            app.log_error(format!(
                "Previous crash recorded: {}. Crash log: {}",
                notice.summary,
                notice.path.display()
            ));
        }
        app
    }

    fn from_state_path(
        state_path: PathBuf,
        spawn_monitor: impl FnOnce(
            GameDetectionConfig,
            SoundPlaybackSettings,
            Vec<PendingAlarmSnapshot>,
        ) -> MonitorHandle,
    ) -> Self {
        let (mut state, load_error) = match storage::load_state_from_path(&state_path) {
            Ok(state) => (state, None),
            Err(error) => (
                PersistedState::default(),
                Some(format!("Could not load saved state: {error}")),
            ),
        };
        let logs = log_entries_from_persisted(&state.logs);
        let (alarms, missed_alarms) =
            AlarmCoordination::restore(std::mem::take(&mut state.alarms), Local::now());
        state.alarms = alarms.snapshot().alarm.alarms.clone();
        let restored_alarm_count = state.alarms.len();
        let accent_color = crate::theme::system_accent_color();
        let monitor = spawn_monitor(
            game_detection_config_from_state(&state),
            sound_playback_settings_from_state(&state),
            state.alarms.clone(),
        );

        let discord = DiscordSession::new(std::mem::take(&mut state.discord_bridge));
        let mut app = Self {
            state,
            state_path,
            state_writes_enabled: load_error.is_none(),
            monitor,
            alarms,
            alarm_popups: Vec::new(),
            next_alarm_popup_id: 1,
            test_sound: None,
            logs,
            app_notices: Vec::new(),
            current_page: AppPage::Alarm,
            accent_color,
            discord,
            new_allowed_requester_id: String::new(),
        };

        app.log_info("Ready");
        app.log_info(format!(
            "Graphics backend setting: {}",
            app.state.graphics_backend_preference.label()
        ));

        if restored_alarm_count > 0 {
            app.log_info(format!("Restored {restored_alarm_count} pending alarm(s)"));
        }

        for missed_alarm in missed_alarms {
            app.show_missed_alarm(missed_alarm);
        }

        if let Some(error) = load_error {
            app.notify_error(format!(
                "{error}. Automatic saving is paused to preserve {}. Restore the file and restart to resume saving.",
                app.state_path.display()
            ));
        }

        app
    }

    fn drain_monitor_events(&mut self, ctx: &egui::Context) -> bool {
        let events = self.monitor.drain_events();
        let had_events = !events.is_empty();

        for event in events {
            match event {
                MonitorEvent::Snapshot(snapshot) => {
                    if self.alarms.apply_snapshot(snapshot) {
                        self.persist_current_state();
                    }
                }
                MonitorEvent::AlarmFired {
                    id,
                    label,
                    delayed_by_game,
                    fired_at,
                    sound,
                } => {
                    self.alarms.record_fired(id, label.clone(), fired_at);

                    let suffix = if delayed_by_game {
                        " after the game ended"
                    } else {
                        ""
                    };
                    let message = format!(
                        "Alarm fired at {}{suffix}: {label}",
                        format_datetime(fired_at)
                    );
                    self.log_at(LogLevel::Info, fired_at, message);
                    self.push_alarm_popup(label, fired_at, delayed_by_game, false, sound);
                    request_alarm_attention(ctx);
                }
                MonitorEvent::TestAlarmPopup {
                    label,
                    fired_at,
                    sound,
                } => {
                    self.log_at(
                        LogLevel::Info,
                        fired_at,
                        format!("Test alarm popup shown at {}", format_time(fired_at)),
                    );
                    self.push_alarm_popup(label, fired_at, false, false, sound);
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
        label: String,
        fired_at: DateTime<Local>,
        delayed_by_game: bool,
        missed: bool,
        sound: Option<SoundHandle>,
    ) {
        let id = self.next_alarm_popup_id;
        self.next_alarm_popup_id = self.next_alarm_popup_id.checked_add(1).unwrap_or(1);

        self.alarm_popups.push(AlarmPopup {
            id,
            label,
            fired_at,
            delayed_by_game,
            missed,
            sound,
        });
    }

    fn show_missed_alarm(&mut self, alarm: PendingAlarmSnapshot) {
        let label = alarm.alarm.label;
        let scheduled_for = alarm.alarm.scheduled_for;

        self.log_at(
            LogLevel::Error,
            Local::now(),
            format!(
                "Missed alarm from {} while the app was not running: {label}",
                format_datetime(scheduled_for)
            ),
        );
        self.push_alarm_popup(label, scheduled_for, alarm.delayed_by_game, true, None);
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

    fn notify_info(&mut self, message: impl Into<String>) {
        self.notify(LogLevel::Info, message);
    }

    fn notify_error(&mut self, message: impl Into<String>) {
        self.notify(LogLevel::Error, message);
    }

    fn notify(&mut self, level: LogLevel, message: impl Into<String>) {
        let message = message.into();
        self.push_app_notice(level, message.clone());
        self.log_at(level, Local::now(), message);
    }

    fn show_app_notice(&mut self, level: LogLevel, message: impl Into<String>) {
        self.push_app_notice(level, message);
    }

    fn push_app_notice(&mut self, level: LogLevel, message: impl Into<String>) {
        self.app_notices.push(AppNotice {
            level,
            message: message.into(),
            created_at: Instant::now(),
        });

        if self.app_notices.len() > MAX_APP_NOTICES {
            let overflow_count = self.app_notices.len() - MAX_APP_NOTICES;
            self.app_notices.drain(..overflow_count);
        }
    }

    fn clear_expired_app_notices(&mut self) -> bool {
        let previous_len = self.app_notices.len();
        self.app_notices
            .retain(|notice| notice.created_at.elapsed() < APP_NOTICE_TTL);

        self.app_notices.len() != previous_len
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
        if !self.state_writes_enabled {
            return false;
        }
        // The persisted field is a serialization snapshot; the session owns live Discord state.
        self.state.discord_bridge = self.discord.settings().clone();
        self.state.logs = persisted_log_entries_from_logs(&self.logs);
        self.state.alarms = self.alarms.snapshot().alarm.alarms.clone();

        match storage::save_state_to_path(&self.state, &self.state_path) {
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
            self.notify_error(error.to_string());
            return;
        }

        let label = alarm_label_from_text(&self.state.label);

        match self.schedule_alarm_from_values(label, self.state.alarm_time.clone()) {
            Ok(scheduled_for) => {
                self.notify_info(format!(
                    "Alarm added for {}",
                    format_datetime(scheduled_for)
                ));
            }
            Err(error) => {
                self.notify_error(error.to_string());
            }
        }
    }

    fn schedule_alarm_from_values(
        &mut self,
        label: String,
        alarm_time: String,
    ) -> Result<DateTime<Local>, AlarmTimeError> {
        let scheduled_for = next_alarm_time(&alarm_time, Local::now())?;
        let alarm = Alarm::with_sound(
            label,
            scheduled_for,
            self.state.delay_for_games,
            sound_path_from_text(&self.state.sound_path),
        );
        let command = self.alarms.schedule(alarm);
        self.monitor.send(command);
        Ok(scheduled_for)
    }

    fn save_settings(&mut self) -> bool {
        self.monitor.send(MonitorCommand::SetGameDetectionConfig(
            game_detection_config_from_state(&self.state),
        ));
        self.apply_sound_playback_settings();

        self.persist_current_state()
    }

    fn cancel_alarm(&mut self, id: AlarmId, label: &str) {
        let command = self.alarms.cancel(id);
        self.monitor.send(command);
        self.notify_info(format!("Alarm cancelled: {label}"));
    }

    fn cancel_all_alarms(&mut self) {
        let command = self.alarms.cancel_all();
        self.monitor.send(command);
        self.notify_info("All alarms cancelled");
    }

    fn apply_sound_playback_settings(&self) {
        self.monitor.send(MonitorCommand::SetSoundPlaybackSettings(
            sound_playback_settings_from_state(&self.state),
        ));
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
                        self.notify_info(format!("CS2 GSI config installed at {}", path.display()));
                    }
                    Err(error) => {
                        self.notify_error(error.to_string());
                    }
                }
            }
            None => {
                self.notify_error("CS2 GSI config path is blank");
            }
        }
    }

    fn apply_discord_changes(&mut self, ctx: &egui::Context) -> bool {
        let changes = self.discord.take_changes();
        if changes.persist {
            self.persist_current_state();
        }
        self.discord.start_pending_tasks();
        for message in changes.messages {
            match message.kind {
                MessageKind::NotifyInfo => self.notify_info(message.text),
                MessageKind::NotifyError => self.notify_error(message.text),
                MessageKind::NoticeInfo => self.show_app_notice(LogLevel::Info, message.text),
                MessageKind::NoticeError => self.show_app_notice(LogLevel::Error, message.text),
                MessageKind::LogInfo => self.log_info(message.text),
                MessageKind::LogError => self.log_error(message.text),
            }
        }
        if !changes.new_invite_labels.is_empty() {
            self.current_page = AppPage::Discord;
            request_alarm_attention(ctx);
            self.log_info(format!(
                "Pending Discord access invite: {}",
                changes.new_invite_labels.join(", ")
            ));
        }
        changes.repaint
    }

    fn update_discord(&mut self, ctx: &egui::Context) -> bool {
        self.discord.advance(SessionTime::now());
        let mut changed = self.apply_discord_changes(ctx);
        while let Some(request) = self.discord.next_alarm_request(Instant::now()) {
            changed |= self.apply_discord_changes(ctx);
            let result = self
                .schedule_alarm_from_values(
                    alarm_label_from_text(request.label()),
                    request.time_text(),
                )
                .map_err(|error| error.to_string());
            self.discord.finish_alarm_request(request, result);
            changed |= self.apply_discord_changes(ctx);
        }
        changed | self.apply_discord_changes(ctx)
    }

    fn alarm_summary(&self) -> String {
        let alarm_count = self.alarms.snapshot().alarm.alarms.len();

        if let Some(next_alarm) = self.alarms.snapshot().alarm.alarms.first() {
            let next_alarm_text = format!(
                "{} at {}",
                next_alarm.alarm.label,
                format_datetime(next_alarm.alarm.scheduled_for)
            );

            if alarm_count == 1 {
                next_alarm_text
            } else {
                format!("{alarm_count} alarms scheduled, next: {next_alarm_text}")
            }
        } else if let Some(fired_at) = self.alarms.snapshot().alarm.fired_at {
            match &self.alarms.snapshot().alarm.last_fired_label {
                Some(label) => {
                    format!("Last fired at {}: {label}", format_datetime(fired_at))
                }
                None => format!("Last fired at {}", format_datetime(fired_at)),
            }
        } else {
            "No alarm scheduled".to_owned()
        }
    }

    fn draw_alarm_list(&mut self, ui: &mut egui::Ui) {
        let alarms = self.alarms.snapshot().alarm.alarms.clone();

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
                ui.strong("Label");
                ui.strong("Status");
                ui.label("");
                ui.end_row();

                for pending_alarm in alarms {
                    copyable_label(
                        ui,
                        format_alarm_list_datetime(pending_alarm.alarm.scheduled_for),
                    );
                    copyable_label(ui, pending_alarm.alarm.label.clone());
                    copyable_label(ui, pending_alarm.status.as_str());

                    if accent_button(ui, "Cancel", self.accent_color).clicked() {
                        self.cancel_alarm(pending_alarm.id, &pending_alarm.alarm.label);
                    }

                    ui.end_row();
                }
            });
    }

    fn alarm_status_label(&self) -> &'static str {
        match self.alarms.snapshot().alarm.status {
            AlarmStatus::Idle
            | AlarmStatus::Scheduled
            | AlarmStatus::WaitingForGameEnd
            | AlarmStatus::Fired => self.alarms.snapshot().alarm.status.as_str(),
        }
    }

    fn draw_navigation(&mut self, ui: &mut egui::Ui) {
        let discord_label = if self.discord.pending_invites().is_empty() {
            "Discord".to_owned()
        } else {
            format!("Discord ({})", self.discord.pending_invites().len())
        };

        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.current_page, AppPage::Alarm, "Alarm");
            ui.selectable_value(&mut self.current_page, AppPage::Games, "Games");
            ui.selectable_value(&mut self.current_page, AppPage::Discord, discord_label);
            ui.selectable_value(&mut self.current_page, AppPage::Log, "Log");
            ui.selectable_value(&mut self.current_page, AppPage::Settings, "Settings");
        });
    }

    fn draw_detected_games_summary(&mut self, ui: &mut egui::Ui) {
        let statuses = self.alarms.snapshot().game_status.statuses.clone();

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
                copyable_label(ui, self.alarm_status_label());
                ui.end_row();

                ui.label("Label");
                copyable_label(ui, self.alarm_summary());
                ui.end_row();

                ui.label("Detected games");
                self.draw_detected_games_summary(ui);
                ui.end_row();

                ui.label("Checked");
                copyable_label(ui, format_datetime(self.alarms.snapshot().last_checked));
                ui.end_row();
            });
    }

    fn draw_alarm_panel(&mut self, ui: &mut egui::Ui) {
        self.draw_status_panel(ui);
        ui.separator();

        ui.horizontal(|ui| {
            ui.label("Alarm label");
            let response = text_input(
                ui,
                self.accent_color,
                "label_input",
                &mut self.state.label,
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
                "HH.MM, HH:MM, HH,MM, 125, or 1554",
            );
        });
        if readable_checkbox(
            ui,
            &mut self.state.delay_for_games,
            self.accent_color,
            "Delay while a tracked game is active",
        )
        .changed()
            && self.save_settings()
        {
            self.log_info("Game delay setting saved");
        }

        ui.horizontal(|ui| {
            if accent_button(ui, "Set Alarm", self.accent_color).clicked() {
                self.schedule_alarm();
            }

            if accent_button(ui, "Cancel All", self.accent_color).clicked() {
                self.cancel_all_alarms();
            }
        });

        self.draw_alarm_list(ui);
    }

    fn draw_settings_panel(&mut self, ui: &mut egui::Ui) {
        if !self.state_writes_enabled {
            ui.colored_label(crate::theme::error_text_color(ui.visuals()),
                "Saving is paused because the settings file could not be loaded. Restore the file and restart; see Log for its location.");
            ui.separator();
        }
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
            ui.label("Alarm volume");
            let mut volume_percent = i32::from(clamped_sound_volume_percent(
                self.state.sound_volume_percent,
            ));
            let response = alarm_volume_control(ui, self.accent_color, &mut volume_percent);

            if response.changed() {
                self.state.sound_volume_percent = volume_percent.clamp(0, 100) as u8;
                self.persist_current_state();
                self.apply_sound_playback_settings();
            }
        });

        if readable_checkbox(
            ui,
            &mut self.state.sound_fade_in,
            self.accent_color,
            "Fade in for the sound",
        )
        .changed()
        {
            self.persist_current_state();
            self.apply_sound_playback_settings();
        }
        ui.label(
            egui::RichText::new(
                "Note: Gradually increases the alarm volume over the first 5 seconds until it reaches your selected volume. Turn this off if the audio stutters, as it may use more CPU.",
            )
            .color(crate::theme::muted_text_color(ui.visuals())),
        );

        ui.horizontal(|ui| {
            if accent_button(ui, "Test Alarm Popup", self.accent_color).clicked() {
                if let Some(sound) = self.test_sound.take() {
                    sound.stop();
                }

                self.notify_info("Starting test alarm popup");
                self.monitor
                    .send(MonitorCommand::TestAlarmPopup(sound_path_from_text(
                        &self.state.sound_path,
                    )));
            }

            if accent_button(ui, "Test Sound", self.accent_color).clicked() {
                if let Some(sound) = self.test_sound.take() {
                    sound.stop();
                }

                self.notify_info("Starting test sound");
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

                self.notify_info("Test sound stopped");
            }
        });

        ui.separator();
        if accent_button(ui, "Save Settings", self.accent_color).clicked() {
            if self.save_settings() {
                self.notify_info("Settings saved");
            } else {
                self.show_app_notice(LogLevel::Error, "Could not save settings");
            }
        }
    }

    fn draw_discord_bridge_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Discord Settings");

        ui.separator();
        ui.heading("Bot Link");
        let mut bridge_url = self.discord.settings().bridge_url.clone();

        ui.horizontal(|ui| {
            ui.label("Bridge URL");
            let response = text_input(
                ui,
                self.accent_color,
                "discord_bridge_url",
                &mut bridge_url,
                360.0,
                "https://alarmbot.example.com",
            );

            if response.changed() {
                self.discord.set_bridge_url(bridge_url.clone());
                self.apply_discord_changes(ui.ctx());
            }
        });

        ui.horizontal_wrapped(|ui| {
            ui.label("Status");
            copyable_label(ui, self.discord.status().to_owned());
        });

        ui.separator();
        ui.heading("Pairing");

        ui.horizontal(|ui| {
            if accent_button(ui, "Generate Pairing Code", self.accent_color).clicked() {
                self.discord.start_pairing();
                self.apply_discord_changes(ui.ctx());
            }

            if accent_button(ui, "Clear Pairing", self.accent_color).clicked() {
                self.discord.clear_pairing();
                self.apply_discord_changes(ui.ctx());
            }
        });

        if let Some(pairing_code) = self
            .discord
            .visible_pairing_code(Local::now())
            .map(str::to_owned)
        {
            ui.horizontal(|ui| {
                ui.label("Pairing code");
                copyable_monospace_label(ui, pairing_code.clone());

                if accent_button(ui, "Copy", self.accent_color).clicked() {
                    copy_to_clipboard(ui.ctx(), pairing_code.clone());
                    self.discord.pairing_code_copied();
                    self.apply_discord_changes(ui.ctx());
                }
            });

            if let Some(expires_at) = self.discord.settings().pairing_expires_at {
                copyable_label(
                    ui,
                    format!("Expires {}", format_expiry_datetime(expires_at)),
                );
            }
        }

        ui.horizontal(|ui| {
            ui.label("Paired account");
            copyable_label(ui, paired_discord_account_label(self.discord.settings()));
        });

        self.draw_pending_companion_invites(ui);

        ui.separator();
        self.draw_allowed_discord_users(ui);
    }

    fn draw_allowed_discord_users(&mut self, ui: &mut egui::Ui) {
        ui.heading("Allowed Users");

        let users = allowed_discord_user_rows(self.discord.settings());
        if users.is_empty() {
            ui.label("No extra allowed Discord users");
        } else {
            let mut remove_user_id = None;

            egui::Grid::new("allowed_discord_users_grid")
                .num_columns(3)
                .striped(true)
                .show(ui, |ui| {
                    ui.strong("User");
                    ui.strong("Discord ID");
                    ui.end_row();

                    for user in &users {
                        copyable_label(ui, user.display_name.clone());
                        copyable_monospace_label(ui, user.discord_user_id.clone());

                        if accent_button(ui, "Remove", self.accent_color).clicked() {
                            remove_user_id = Some(user.discord_user_id.clone());
                        }

                        ui.end_row();
                    }
                });

            if let Some(user_id) = remove_user_id {
                self.discord.remove_allowed_requester(&user_id);
                self.apply_discord_changes(ui.ctx());
            }
        }

        ui.add_space(6.0);
        ui.scope(|ui| {
            let input_height = text_input_height_for_min_control_height(ui, row_control_height(ui));
            ui.spacing_mut().interact_size.y = input_height;

            ui.horizontal(|ui| {
                row_label_with_height(ui, "Add by ID", input_height);
                text_input_with_min_height(
                    ui,
                    self.accent_color,
                    "discord_new_allowed_requester_id",
                    &mut self.new_allowed_requester_id,
                    260.0,
                    "Discord ID (numbers)",
                    input_height,
                );

                let add_clicked =
                    accent_button_with_min_height(ui, "Add", self.accent_color, input_height)
                        .clicked();
                if add_clicked {
                    if self
                        .discord
                        .add_allowed_requester(&self.new_allowed_requester_id)
                    {
                        self.new_allowed_requester_id.clear();
                    }
                    self.apply_discord_changes(ui.ctx());
                }
            });
        });
    }

    fn draw_pending_companion_invites(&mut self, ui: &mut egui::Ui) {
        if self.discord.pending_invites().is_empty() {
            return;
        }

        ui.separator();
        ui.heading("Pending Access Invites");

        let mut decision = None;
        for invite in self.discord.pending_invites() {
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    copyable_label(
                        ui,
                        format!(
                            "Allow {} to request alarms",
                            discord_invite_user_label(invite)
                        ),
                    );

                    if accent_button(ui, "Allow", self.accent_color).clicked() {
                        decision = Some((invite.id, InviteStatus::Allowed));
                    }

                    if accent_button(ui, "Reject", self.accent_color).clicked() {
                        decision = Some((invite.id, InviteStatus::Rejected));
                    }
                });

                if let Some(expires_at) = local_datetime_from_unix(invite.expires_at_unix) {
                    copyable_label(
                        ui,
                        format!("Expires {}", format_expiry_datetime(expires_at)),
                    );
                }
            });
        }

        if let Some((invite_id, status)) = decision {
            match status {
                InviteStatus::Allowed => self.discord.allow_invite(invite_id),
                InviteStatus::Rejected => self.discord.reject_invite(invite_id),
            }
            self.apply_discord_changes(ui.ctx());
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
        if accent_button(ui, "Save Game Settings", self.accent_color).clicked() {
            if self.save_settings() {
                self.notify_info("Game settings saved");
            } else {
                self.show_app_notice(LogLevel::Error, "Could not save game settings");
            }
        }
    }

    fn draw_log_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Log");

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if accent_button(ui, "Clear", self.accent_color).clicked() {
                    self.logs.clear();
                    self.persist_current_state();
                    self.show_app_notice(LogLevel::Info, "Log cleared");
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
                    copyable_label(ui, format_datetime(entry.occurred_at));
                    copyable_rich_label(
                        ui,
                        egui::RichText::new(entry.level.as_str())
                            .color(entry.level.color(ui.visuals())),
                        entry.level.as_str(),
                    );
                    copyable_label(ui, entry.message.clone());
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
        self.alarms
            .snapshot()
            .game_status
            .statuses
            .iter()
            .find(|status| game_status_name(status) == game)
    }

    fn draw_current_page(&mut self, ui: &mut egui::Ui) {
        with_page_spacing(ui, |ui| {
            self.draw_navigation(ui);
            ui.separator();

            match self.current_page {
                AppPage::Alarm => self.draw_alarm_panel(ui),
                AppPage::Games => self.draw_games_panel(ui),
                AppPage::Discord => self.draw_discord_bridge_settings(ui),
                AppPage::Log => self.draw_log_panel(ui),
                AppPage::Settings => self.draw_settings_panel(ui),
            }
        });
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
                let shell_palette = alarm_popup_palette(ui.visuals(), false);

                egui::Frame::popup(ui.style())
                    .fill(shell_palette.shell_fill)
                    .stroke(egui::Stroke::new(1.5, shell_palette.border))
                    .corner_radius(8)
                    .show(ui, |ui| {
                        ui.set_width(360.0);

                        ui.horizontal(|ui| {
                            ui.heading(egui::RichText::new("Alarm").color(shell_palette.title));

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if accent_button(ui, "Dismiss all", self.accent_color).clicked()
                                    {
                                        dismiss_all = true;
                                    }
                                },
                            );
                        });

                        ui.separator();

                        for popup in &self.alarm_popups {
                            let palette = alarm_popup_palette(ui.visuals(), popup.missed);
                            let response = egui::Frame::new()
                                .inner_margin(egui::Margin::symmetric(14, 10))
                                .outer_margin(egui::Margin::symmetric(0, 4))
                                .fill(palette.card_fill)
                                .stroke(egui::Stroke::new(1.25, palette.border))
                                .corner_radius(8)
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());

                                    if popup.missed {
                                        ui.label(
                                            egui::RichText::new("Missed alarm")
                                                .strong()
                                                .color(palette.title),
                                        );
                                        copyable_rich_label(
                                            ui,
                                            egui::RichText::new(popup.label.clone())
                                                .strong()
                                                .color(palette.body),
                                            popup.label.clone(),
                                        );

                                        let due_text = format!(
                                            "Was due at {}",
                                            format_datetime(popup.fired_at)
                                        );
                                        copyable_rich_label(
                                            ui,
                                            egui::RichText::new(due_text.clone())
                                                .color(palette.muted),
                                            due_text,
                                        );
                                        copyable_rich_label(
                                            ui,
                                            egui::RichText::new("No sound was played.")
                                                .color(palette.muted),
                                            "No sound was played.",
                                        );
                                    } else {
                                        ui.label(
                                            egui::RichText::new("Alarm")
                                                .strong()
                                                .color(palette.title),
                                        );
                                        copyable_rich_label(
                                            ui,
                                            egui::RichText::new(popup.label.clone())
                                                .strong()
                                                .color(palette.body),
                                            popup.label.clone(),
                                        );

                                        let fired_text =
                                            format!("Fired at {}", format_datetime(popup.fired_at));
                                        copyable_rich_label(
                                            ui,
                                            egui::RichText::new(fired_text.clone())
                                                .color(palette.muted),
                                            fired_text,
                                        );
                                    }

                                    if popup.delayed_by_game {
                                        copyable_rich_label(
                                            ui,
                                            egui::RichText::new("Delayed until the game ended")
                                                .color(palette.muted),
                                            "Delayed until the game ended",
                                        );
                                    }

                                    ui.horizontal(|ui| {
                                        if accent_button(ui, "Dismiss", self.accent_color).clicked()
                                        {
                                            dismissed_alarm = Some(popup.id);
                                        }
                                    });
                                })
                                .response;

                            let stripe_rect = egui::Rect::from_min_max(
                                response.rect.left_top(),
                                egui::pos2(response.rect.left() + 5.0, response.rect.bottom()),
                            );
                            ui.painter().rect_filled(stripe_rect, 3.0, palette.accent);
                        }
                    });
            });

        if dismiss_all {
            self.dismiss_all_alarm_popups();
        } else if let Some(id) = dismissed_alarm {
            self.dismiss_alarm_popup(id);
        }
    }

    fn draw_app_notices(&mut self, ctx: &egui::Context) {
        if self.app_notices.is_empty() {
            return;
        }

        ctx.request_repaint_after(APP_NOTICE_REPAINT_INTERVAL);

        egui::Area::new(egui::Id::new("app_notices"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
            .show(ctx, |ui| {
                ui.set_width(APP_NOTICE_WIDTH);
                ui.spacing_mut().item_spacing.y = 8.0;

                for notice in &self.app_notices {
                    let palette = app_notice_palette(ui.visuals(), notice.level);
                    let response = egui::Frame::new()
                        .inner_margin(egui::Margin::symmetric(14, 10))
                        .fill(palette.fill)
                        .stroke(egui::Stroke::new(1.25, palette.border))
                        .corner_radius(8)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(
                                egui::RichText::new(app_notice_title(notice.level))
                                    .strong()
                                    .color(palette.title),
                            );
                            copyable_rich_label(
                                ui,
                                egui::RichText::new(notice.message.clone()).color(palette.body),
                                notice.message.clone(),
                            );
                        })
                        .response;

                    let stripe_rect = egui::Rect::from_min_max(
                        response.rect.left_top(),
                        egui::pos2(response.rect.left() + 5.0, response.rect.bottom()),
                    );
                    ui.painter().rect_filled(stripe_rect, 3.0, palette.accent);
                }
            });
    }
}

impl eframe::App for AlarmApp {
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let state_changed = self.drain_monitor_events(ctx)
            | self.clear_finished_test_sound()
            | self.update_discord(ctx)
            | self.clear_expired_app_notices();

        if state_changed {
            ctx.request_repaint();
        }

        ctx.request_repaint_after(IDLE_REPAINT_INTERVAL);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let state_changed = self.drain_monitor_events(ui.ctx())
            | self.clear_finished_test_sound()
            | self.update_discord(ui.ctx())
            | self.clear_expired_app_notices();

        if state_changed {
            ui.ctx().request_repaint();
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            self.draw_current_page(ui);
        });

        self.draw_alarm_popups(ui.ctx());
        self.draw_app_notices(ui.ctx());
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

fn sound_playback_settings_from_state(state: &PersistedState) -> SoundPlaybackSettings {
    SoundPlaybackSettings::from_percent(
        clamped_sound_volume_percent(state.sound_volume_percent),
        state.sound_fade_in,
    )
}

fn clamped_sound_volume_percent(volume_percent: u8) -> u8 {
    volume_percent.min(100)
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct AllowedDiscordUserRow {
    discord_user_id: String,
    display_name: String,
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

fn with_page_spacing<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.scope(|ui| {
        let spacing = ui.spacing_mut();
        spacing.item_spacing.y = spacing.item_spacing.y.max(PAGE_ROW_SPACING_Y);
        spacing.interact_size.y = spacing.interact_size.y.max(PAGE_CONTROL_HEIGHT);
        spacing.button_padding.y = spacing.button_padding.y.max(5.0);

        add_contents(ui)
    })
    .inner
}

fn row_control_height(ui: &egui::Ui) -> f32 {
    ui.spacing().interact_size.y.max(PAGE_CONTROL_HEIGHT)
}

fn default_text_input_line_height(ui: &egui::Ui) -> f32 {
    ui.text_style_height(&egui::TextStyle::Body) + ui.spacing().extra_text_line_spacing
}

fn text_input_vertical_margin_for_height(ui: &egui::Ui, min_height: f32) -> i8 {
    let vertical_margin = ((min_height - default_text_input_line_height(ui)) / 2.0)
        .ceil()
        .max(f32::from(TEXT_INPUT_MIN_VERTICAL_MARGIN));
    vertical_margin.min(f32::from(i8::MAX)) as i8
}

fn text_input_height_for_min_control_height(ui: &egui::Ui, min_height: f32) -> f32 {
    let vertical_margin = text_input_vertical_margin_for_height(ui, min_height);
    default_text_input_line_height(ui) + (f32::from(vertical_margin) * 2.0)
}

fn row_label_with_height(ui: &mut egui::Ui, label: &'static str, height: f32) -> egui::Response {
    let label_width = ui
        .painter()
        .layout_no_wrap(
            label.to_owned(),
            egui::TextStyle::Body.resolve(ui.style()),
            ui.visuals().text_color(),
        )
        .size()
        .x
        .ceil();

    ui.allocate_ui_with_layout(
        egui::vec2(label_width, height),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| ui.label(label),
    )
    .inner
}

fn alarm_volume_control(
    ui: &mut egui::Ui,
    accent_color: egui::Color32,
    volume_percent: &mut i32,
) -> egui::Response {
    let control_height = row_control_height(ui);
    let slider_response =
        framed_alarm_volume_slider(ui, accent_color, volume_percent, control_height);
    alarm_volume_value_box(ui, accent_color, *volume_percent, control_height);
    slider_response
}

fn framed_alarm_volume_slider(
    ui: &mut egui::Ui,
    accent_color: egui::Color32,
    volume_percent: &mut i32,
    control_height: f32,
) -> egui::Response {
    with_input_visuals(ui, accent_color, |ui| {
        let fill = ui.visuals().widgets.inactive.bg_fill;
        let stroke = ui.visuals().widgets.inactive.bg_stroke;

        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(8, 0))
            .fill(fill)
            .stroke(stroke)
            .corner_radius(5)
            .show(ui, |ui| {
                ui.spacing_mut().slider_width = ALARM_VOLUME_SLIDER_WIDTH;
                ui.set_min_size(egui::vec2(ALARM_VOLUME_SLIDER_WIDTH, control_height));
                ui.add_sized(
                    egui::vec2(ALARM_VOLUME_SLIDER_WIDTH, control_height),
                    egui::Slider::new(volume_percent, 0..=100).show_value(false),
                )
            })
            .inner
    })
}

fn alarm_volume_value_box(
    ui: &mut egui::Ui,
    accent_color: egui::Color32,
    volume_percent: i32,
    control_height: f32,
) -> egui::Response {
    with_input_visuals(ui, accent_color, |ui| {
        let fill = ui.visuals().widgets.inactive.bg_fill;
        let stroke = ui.visuals().widgets.inactive.bg_stroke;
        let value_text = format!("{}%", volume_percent.clamp(0, 100));

        egui::Frame::new()
            .inner_margin(egui::Margin::same(0))
            .fill(fill)
            .stroke(stroke)
            .corner_radius(5)
            .show(ui, |ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(ALARM_VOLUME_VALUE_WIDTH, control_height),
                    egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                    |ui| {
                        ui.label(egui::RichText::new(value_text).monospace());
                    },
                );
            })
            .response
    })
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
    text_input_with_optional_min_height(
        ui,
        accent_color,
        id_salt,
        value,
        desired_width,
        hint_text,
        None,
    )
}

fn text_input_with_min_height(
    ui: &mut egui::Ui,
    accent_color: egui::Color32,
    id_salt: impl egui::AsIdSalt,
    value: &mut String,
    desired_width: f32,
    hint_text: &'static str,
    min_height: f32,
) -> egui::Response {
    text_input_with_optional_min_height(
        ui,
        accent_color,
        id_salt,
        value,
        desired_width,
        hint_text,
        Some(min_height),
    )
}

fn text_input_with_optional_min_height(
    ui: &mut egui::Ui,
    accent_color: egui::Color32,
    id_salt: impl egui::AsIdSalt,
    value: &mut String,
    desired_width: f32,
    hint_text: &'static str,
    min_height: Option<f32>,
) -> egui::Response {
    with_input_visuals(ui, accent_color, |ui| {
        let id = ui.make_persistent_id(id_salt);
        let has_focus = ui.memory(|memory| memory.has_focus(id));
        let hint_text = if has_focus { "" } else { hint_text };
        let mut text_edit = egui::TextEdit::singleline(value)
            .desired_width(desired_width)
            .id(id)
            .hint_text(hint_text);

        if let Some(min_height) = min_height {
            text_edit = text_edit
                .min_size(egui::vec2(desired_width, min_height))
                .margin(egui::Margin::symmetric(
                    TEXT_INPUT_HORIZONTAL_MARGIN,
                    text_input_vertical_margin_for_height(ui, min_height),
                ));
        }

        ui.add(text_edit)
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
    let min_height = row_control_height(ui);
    accent_button_with_min_height(ui, label, accent_color, min_height)
}

fn accent_button_with_min_height(
    ui: &mut egui::Ui,
    label: &'static str,
    accent_color: egui::Color32,
    min_height: f32,
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

        let button = egui::Button::new(label).min_size(egui::vec2(0.0, min_height));
        ui.add(button)
    })
    .inner
}

fn alarm_popup_palette(visuals: &egui::Visuals, missed: bool) -> AlarmPopupPalette {
    match (visuals.dark_mode, missed) {
        (true, false) => AlarmPopupPalette {
            shell_fill: egui::Color32::from_rgb(36, 28, 26),
            card_fill: egui::Color32::from_rgb(47, 34, 31),
            border: egui::Color32::from_rgb(121, 81, 70),
            accent: egui::Color32::from_rgb(225, 126, 94),
            title: egui::Color32::from_rgb(255, 184, 154),
            body: egui::Color32::from_rgb(248, 244, 242),
            muted: egui::Color32::from_rgb(218, 202, 196),
        },
        (true, true) => AlarmPopupPalette {
            shell_fill: egui::Color32::from_rgb(34, 31, 25),
            card_fill: egui::Color32::from_rgb(45, 39, 28),
            border: egui::Color32::from_rgb(118, 92, 49),
            accent: egui::Color32::from_rgb(211, 156, 70),
            title: egui::Color32::from_rgb(244, 202, 124),
            body: egui::Color32::from_rgb(248, 244, 238),
            muted: egui::Color32::from_rgb(216, 204, 184),
        },
        (false, false) => AlarmPopupPalette {
            shell_fill: egui::Color32::from_rgb(255, 240, 231),
            card_fill: egui::Color32::from_rgb(255, 225, 211),
            border: egui::Color32::from_rgb(220, 132, 96),
            accent: egui::Color32::from_rgb(196, 80, 43),
            title: egui::Color32::from_rgb(125, 49, 24),
            body: egui::Color32::from_rgb(34, 28, 25),
            muted: egui::Color32::from_rgb(75, 57, 49),
        },
        (false, true) => AlarmPopupPalette {
            shell_fill: egui::Color32::from_rgb(255, 246, 224),
            card_fill: egui::Color32::from_rgb(255, 234, 188),
            border: egui::Color32::from_rgb(211, 160, 70),
            accent: egui::Color32::from_rgb(168, 105, 18),
            title: egui::Color32::from_rgb(99, 63, 16),
            body: egui::Color32::from_rgb(38, 32, 24),
            muted: egui::Color32::from_rgb(76, 63, 42),
        },
    }
}

fn app_notice_title(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Info => "Notice",
        LogLevel::Error => "Could not do that",
    }
}

fn app_notice_palette(visuals: &egui::Visuals, level: LogLevel) -> AppNoticePalette {
    match (visuals.dark_mode, level) {
        (true, LogLevel::Info) => AppNoticePalette {
            fill: egui::Color32::from_rgb(24, 37, 37),
            border: egui::Color32::from_rgb(56, 104, 103),
            accent: egui::Color32::from_rgb(74, 178, 169),
            title: egui::Color32::from_rgb(145, 227, 217),
            body: egui::Color32::from_rgb(236, 248, 246),
        },
        (true, LogLevel::Error) => AppNoticePalette {
            fill: egui::Color32::from_rgb(43, 30, 27),
            border: egui::Color32::from_rgb(131, 72, 62),
            accent: egui::Color32::from_rgb(236, 116, 94),
            title: egui::Color32::from_rgb(255, 180, 162),
            body: egui::Color32::from_rgb(250, 239, 236),
        },
        (false, LogLevel::Info) => AppNoticePalette {
            fill: egui::Color32::from_rgb(230, 249, 246),
            border: egui::Color32::from_rgb(84, 162, 156),
            accent: egui::Color32::from_rgb(31, 139, 132),
            title: egui::Color32::from_rgb(18, 86, 82),
            body: egui::Color32::from_rgb(26, 43, 41),
        },
        (false, LogLevel::Error) => AppNoticePalette {
            fill: egui::Color32::from_rgb(255, 239, 234),
            border: egui::Color32::from_rgb(211, 116, 93),
            accent: egui::Color32::from_rgb(197, 75, 48),
            title: egui::Color32::from_rgb(120, 42, 28),
            body: egui::Color32::from_rgb(43, 31, 27),
        },
    }
}

fn copy_to_clipboard(ctx: &egui::Context, text: impl Into<String>) {
    ctx.copy_text(text.into());
}

fn copyable_label(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    let text = text.into();
    copyable_rich_label(ui, egui::RichText::new(text.clone()), text)
}

fn copyable_monospace_label(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    let text = text.into();
    copyable_rich_label(ui, egui::RichText::new(text.clone()).monospace(), text)
}

fn copyable_rich_label(
    ui: &mut egui::Ui,
    label: egui::RichText,
    copy_text: impl Into<String>,
) -> egui::Response {
    let copy_text = copy_text.into();
    let response = ui
        .add(egui::Label::new(label).selectable(true))
        .on_hover_text("Right-click to copy");

    response.context_menu(|ui| {
        if ui.button("Copy").clicked() {
            copy_to_clipboard(ui.ctx(), copy_text);
            ui.close();
        }
    });

    response
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

fn format_alarm_list_datetime(value: DateTime<Local>) -> String {
    value.format("%H:%M %d-%m-%Y").to_string()
}

fn format_expiry_datetime(value: DateTime<Local>) -> String {
    value.format("%H:%M, %d-%m-%Y").to_string()
}

fn format_time(value: DateTime<Local>) -> String {
    value.format("%H:%M:%S").to_string()
}

fn paired_discord_account_label(settings: &DiscordBridgeSettings) -> String {
    let Some(user_id) = settings.paired_discord_user_id.as_deref() else {
        return "-".to_owned();
    };

    let label = settings
        .paired_discord_user_label
        .as_deref()
        .map(str::trim)
        .filter(|label| !label.is_empty());

    if let Some(label) = label {
        format!("{label} (ID: {user_id})")
    } else {
        format!("ID: {user_id}")
    }
}

fn allowed_discord_user_rows(settings: &DiscordBridgeSettings) -> Vec<AllowedDiscordUserRow> {
    discord_bridge::allowed_requester_ids(&settings.allowed_requester_ids)
        .into_iter()
        .map(|discord_user_id| {
            let display_name = settings
                .allowed_requesters
                .iter()
                .find(|requester| requester.discord_user_id == discord_user_id)
                .map(|requester| requester.display_name.trim())
                .filter(|display_name| !display_name.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| "Unknown user".to_owned());

            AllowedDiscordUserRow {
                discord_user_id,
                display_name,
            }
        })
        .collect()
}

fn alarm_label_from_text(value: &str) -> String {
    let trimmed = value.trim();

    if trimmed.is_empty() {
        "Alarm".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::sync::mpsc;

    fn test_state_path() -> PathBuf {
        std::env::temp_dir()
            .join(format!("alarm_app_test_{}", rand::random::<u64>()))
            .join("state.json")
    }

    fn test_app(path: PathBuf) -> (AlarmApp, mpsc::Sender<MonitorEvent>) {
        let (app, events, _commands) = test_app_with_commands(path);
        (app, events)
    }

    fn test_app_with_commands(
        path: PathBuf,
    ) -> (
        AlarmApp,
        mpsc::Sender<MonitorEvent>,
        mpsc::Receiver<MonitorCommand>,
    ) {
        let (monitor, events, commands) = MonitorHandle::test_channels();
        let app = AlarmApp::from_state_path(path, |_, _, _| monitor);
        (app, events, commands)
    }

    fn schedule_test_alarm(app: &mut AlarmApp) {
        let time = (Local::now() + chrono::Duration::hours(1))
            .format("%H:%M")
            .to_string();
        app.schedule_alarm_from_values("Test alarm".to_owned(), time)
            .expect("test alarm should schedule");
        app.persist_current_state();
    }

    #[test]
    fn startup_preserves_a_state_file_that_cannot_be_loaded() {
        let path = test_state_path();
        std::fs::create_dir_all(path.parent().expect("test path has a parent"))
            .expect("test directory should be created");
        let original = r#"{"alarms": [recoverable but invalid JSON]}"#;
        std::fs::write(&path, original).expect("test state should be written");

        let (mut app, _) = test_app(path.clone());
        app.log_info("Another automatic save attempt");

        assert_eq!(
            std::fs::read_to_string(&path).expect("original file should remain"),
            original,
            "startup and logging must preserve state after a load error"
        );
    }

    #[test]
    fn stale_snapshot_does_not_remove_a_new_alarm() {
        let path = test_state_path();
        let (mut app, events) = test_app(path.clone());
        let mut stale = app.alarms.snapshot().clone();
        stale
            .game_status
            .statuses
            .push(GameStatus::active("Test game", None));
        schedule_test_alarm(&mut app);

        events
            .send(MonitorEvent::Snapshot(stale))
            .expect("UI should receive snapshot");
        app.drain_monitor_events(&egui::Context::default());

        assert_eq!(
            app.alarms.snapshot().alarm.alarms.len(),
            1,
            "new alarm must remain visible"
        );
        assert!(
            app.alarms.snapshot().game_status.statuses[0].is_active(),
            "game detection should still update"
        );
        assert_eq!(
            storage::load_state_from_path(path)
                .expect("saved state should load")
                .alarms
                .len(),
            1
        );
    }

    #[test]
    fn acknowledged_snapshot_updates_and_saves_alarm_status() {
        let path = test_state_path();
        let (mut app, events, commands) = test_app_with_commands(path.clone());
        schedule_test_alarm(&mut app);
        let mut acknowledged = app.alarms.snapshot().clone();
        let MonitorCommand::Schedule { revision, .. } = commands.recv().unwrap() else {
            panic!("scheduling should send an alarm edit command");
        };
        acknowledged.applied_alarm_revision = revision;
        acknowledged.alarm.status = AlarmStatus::WaitingForGameEnd;
        acknowledged.alarm.alarms[0].status = AlarmStatus::WaitingForGameEnd;
        acknowledged.alarm.alarms[0].delayed_by_game = true;

        events
            .send(MonitorEvent::Snapshot(acknowledged))
            .expect("UI should receive acknowledgement");
        app.drain_monitor_events(&egui::Context::default());

        assert_eq!(
            app.alarms.snapshot().alarm.status,
            AlarmStatus::WaitingForGameEnd
        );
        let saved = storage::load_state_from_path(path).expect("saved state should load");
        assert_eq!(saved.alarms[0].status, AlarmStatus::WaitingForGameEnd);
        assert!(saved.alarms[0].delayed_by_game);
    }

    #[test]
    fn healthy_state_keeps_saving_and_restores_alarms_on_restart() {
        let path = test_state_path();
        let (mut app, _) = test_app(path.clone());
        schedule_test_alarm(&mut app);
        let alarms = app.alarms.snapshot().alarm.alarms.clone();
        drop(app);

        let (restarted, _) = test_app(path);

        assert_eq!(restarted.alarms.snapshot().alarm.alarms, alarms);
    }

    #[test]
    fn discord_session_settings_keep_the_existing_saved_shape_and_restore_on_restart() {
        let path = test_state_path();
        let (mut app, _) = test_app(path.clone());
        let settings = DiscordBridgeSettings {
            bridge_url: "https://bridge.invalid".to_owned(),
            companion_id: "saved-companion".to_owned(),
            api_token: "saved-token".to_owned(),
            paired_discord_user_id: Some("111".to_owned()),
            allowed_requester_ids: "222".to_owned(),
            ..Default::default()
        };
        app.discord = DiscordSession::new(settings.clone());
        assert!(app.persist_current_state());
        let saved: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&path).expect("saved state should be readable"),
        )
        .expect("saved state should be JSON");
        assert_eq!(
            saved["discord_bridge"],
            serde_json::to_value(&settings).unwrap()
        );
        let (restarted, _) = test_app(path);
        assert_eq!(restarted.discord.settings(), &settings);
    }

    #[test]
    fn discord_poll_schedules_once_and_accepts_even_when_saving_is_paused() {
        use crate::discord_bridge::{PollRequestsResponse, RemoteAlarmRequest};
        use chrono::Timelike;

        for paused in [false, true] {
            let path = test_state_path();
            if paused {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, "recoverable but invalid state").unwrap();
            }
            let (mut app, _) = test_app(path.clone());
            let settings = DiscordBridgeSettings {
                companion_id: "test-companion".to_owned(),
                api_token: "test-token".to_owned(),
                paired_discord_user_id: Some("111".to_owned()),
                ..Default::default()
            };
            let due = Local::now() + chrono::Duration::hours(1);
            let request = RemoteAlarmRequest {
                id: 7,
                requester_id: "111".to_owned(),
                requester_label: None,
                target_id: "111".to_owned(),
                hour: due.hour() as u8,
                minute: due.minute() as u8,
                label: "   ".to_owned(),
                created_at_unix: 1,
                expires_at_unix: 2,
            };
            app.discord = crate::discord_session::tests::queued_poll(
                settings,
                PollRequestsResponse {
                    paired: true,
                    paired_discord_user_id: Some("111".to_owned()),
                    paired_discord_user_label: None,
                    requests: vec![request.clone(), request],
                    invites: Vec::new(),
                },
            );
            app.update_discord(&egui::Context::default());
            assert_eq!(
                app.alarms.snapshot().alarm.alarms.len(),
                1,
                "duplicate must not schedule twice"
            );
            assert_eq!(app.alarms.snapshot().alarm.alarms[0].alarm.label, "Alarm");
            assert_eq!(
                app.logs
                    .iter()
                    .filter(|entry| entry
                        .message
                        .starts_with("Accepted Discord alarm request #7 "))
                    .count(),
                1
            );
            if paused {
                assert_eq!(
                    std::fs::read_to_string(&path).unwrap(),
                    "recoverable but invalid state"
                );
            } else {
                assert_eq!(
                    storage::load_state_from_path(&path).unwrap().alarms,
                    app.alarms.snapshot().alarm.alarms
                );
            }
        }
    }

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
    fn allowed_discord_user_rows_use_labels_when_available() {
        let mut settings = DiscordBridgeSettings {
            allowed_requester_ids: "333\n222".to_owned(),
            ..DiscordBridgeSettings::default()
        };
        settings
            .allowed_requesters
            .push(discord_bridge::AllowedDiscordRequester {
                discord_user_id: "222".to_owned(),
                display_name: "Jane".to_owned(),
            });

        assert_eq!(
            allowed_discord_user_rows(&settings),
            vec![
                AllowedDiscordUserRow {
                    discord_user_id: "222".to_owned(),
                    display_name: "Jane".to_owned(),
                },
                AllowedDiscordUserRow {
                    discord_user_id: "333".to_owned(),
                    display_name: "Unknown user".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn paired_discord_account_label_prefers_name_with_id() {
        let mut settings = DiscordBridgeSettings {
            paired_discord_user_id: Some("123".to_owned()),
            paired_discord_user_label: Some("Zyhler".to_owned()),
            ..Default::default()
        };

        assert_eq!(paired_discord_account_label(&settings), "Zyhler (ID: 123)");

        settings.paired_discord_user_label = None;

        assert_eq!(paired_discord_account_label(&settings), "ID: 123");
    }

    #[test]
    fn blank_alarm_label_defaults_to_alarm() {
        assert_eq!(alarm_label_from_text("   "), "Alarm");
        assert_eq!(alarm_label_from_text(" Laundry "), "Laundry");
    }

    #[test]
    fn alarm_list_datetime_uses_time_then_european_date() {
        let value = Local.with_ymd_and_hms(2026, 9, 1, 7, 5, 30).unwrap();

        assert_eq!(format_alarm_list_datetime(value), "07:05 01-09-2026");
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
