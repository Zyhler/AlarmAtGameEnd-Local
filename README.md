# Alarm at Game End

A small Rust desktop reminder app. It lets you set a reminder, watches tracked games in the background, delays while a game is active, and sends a system notification with an optional custom sound when the game ends.

## Quick Start

Rust is installed on this machine at `C:\Users\Zyhler\.cargo\bin`. That folder has been added to the Windows user PATH, so new terminals can run Cargo directly:

```powershell
cargo run
```

Useful commands:

```powershell
cargo fmt
cargo test
cargo run
```

The app saves settings, logs, and pending alarms under the user's config directory. On Windows this is `%APPDATA%\Alarm at Game End\state.json`. If the OS config directory is unavailable, it falls back to a local `.alarm_at_game_end/state.json`.

Crash reports are appended to `%APPDATA%\Alarm at Game End\crash.log` on Windows. On the next launch, the Log page records a short summary and the crash log path.

## Sharing

The default alarm sound is embedded in the release executable, so the app can be shared as a single `.exe` unless you want to include extra custom sounds.

Build a Windows release:

```powershell
cargo build --release
```

When you are ready to share a new build, create a small zip containing `Alarm at Game End.exe`:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\package-windows.ps1
```

The packaged app is written to `dist\Alarm at Game End-windows-x64.zip`.

## Features

- Schedule a local alarm from `HH:MM`, `HH.MM`, `HH,MM`, or compact `HHMM` input.
- Keep multiple pending alarms and cancel them individually.
- Keep alarm setup, game status, app settings, and logs on separate pages.
- Keep the shared game-delay toggle on the front Alarm page.
- Show detected games as a short front-page list, with a full Games page when the list grows.
- Keep action feedback and errors on the Log page.
- Persist settings, the last 500 log entries, and pending alarms across restarts.
- Show overdue restored alarms as missed popups without playing sound.
- Append Rust panic details to a local crash log and report the latest unreported crash on startup.
- Hide the CS2 GSI install button once the config is installed or the app is already receiving CS2 GSI updates.
- Send desktop notifications through `notify-rust`.
- Log test alarm popup and test sound results.
- Show a persistent in-app alarm popup until dismissed, and stop the alarm sound when it is dismissed.
- Request native window attention when an alarm fires.
- Switch between automatic, WGPU, and OpenGL graphics backends from Settings when a restart is acceptable.
- Play the bundled default alarm sound, or an optional custom alarm sound file through `rodio`.
- Test and stop the configured alarm sound from Settings.
- Poll game detectors from a background worker thread.
- Delay the alarm while any tracked game is active.
- Auto-save reminder text, alarm time, delay preference, optional sound path, optional League lockfile override, Counter-Strike 2 GSI settings, logs, and pending alarms.

## Game Detection

Game detection is routed through a generic detector interface. League of Legends and Counter-Strike 2 are currently built in; to add another game, implement `GameDetector` and register it in `GameMonitor::default_with_config`.

League detection is automatic. The app looks for a League lockfile in common locations and also accepts a manual lockfile path override in the GUI. It reads the lockfile only while running and does not store the password from it.

The League Client API is a local, unsupported API. The app still polls the gameflow phase instead of treating "LCU is up" as an active game, because the League client can be open while you are only in menus or lobby. If detection is unavailable when the alarm is due, the alarm fires instead of waiting forever.

Counter-Strike 2 detection uses Game State Integration. The app listens on a local HTTP port and CS2 sends map state to that local listener after `gamestate_integration_alarm_at_game_end.cfg` is installed in the CS2 config folder. A recent GSI payload with map data counts as active; no map data counts as inactive, so sitting in the CS2 client or lobby does not delay alarms.
