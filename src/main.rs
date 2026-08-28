#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

fn main() -> eframe::Result {
    alarm_at_game_end::crash::install_panic_hook();
    alarm_at_game_end::run()
}
