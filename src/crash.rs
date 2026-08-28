use chrono::{DateTime, Local};
use std::backtrace::Backtrace;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::panic::{self, PanicHookInfo};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CrashNotice {
    pub path: PathBuf,
    pub fingerprint: String,
    pub summary: String,
}

struct CrashReport {
    occurred_at: DateTime<Local>,
    thread_name: String,
    message: String,
    location: String,
    backtrace: String,
}

pub fn install_panic_hook() {
    let default_hook = panic::take_hook();

    panic::set_hook(Box::new(move |info| {
        if let Err(error) = write_panic_report(info) {
            eprintln!("could not write crash report: {error}");
        }

        default_hook(info);
    }));
}

pub fn latest_crash_notice() -> Result<Option<CrashNotice>, io::Error> {
    latest_crash_notice_from_path(&crate::storage::crash_log_path())
}

fn write_panic_report(info: &PanicHookInfo<'_>) -> Result<(), io::Error> {
    let current_thread = std::thread::current();
    let report = CrashReport {
        occurred_at: Local::now(),
        thread_name: current_thread.name().unwrap_or("unnamed").to_owned(),
        message: panic_message(info),
        location: info
            .location()
            .map(|location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            })
            .unwrap_or_else(|| "unknown".to_owned()),
        backtrace: Backtrace::force_capture().to_string(),
    };

    write_crash_report_to_path(&crate::storage::crash_log_path(), &report)
}

fn panic_message(info: &PanicHookInfo<'_>) -> String {
    if let Some(message) = info.payload().downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = info.payload().downcast_ref::<String>() {
        message.clone()
    } else {
        "panic payload was not a string".to_owned()
    }
}

fn write_crash_report_to_path(path: &Path, report: &CrashReport) -> Result<(), io::Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut file = OpenOptions::new().create(true).append(true).open(path)?;

    writeln!(file, "=== crash {} ===", report.occurred_at.to_rfc3339())?;
    writeln!(file, "thread: {}", report.thread_name)?;
    writeln!(file, "panic: {}", report.message)?;
    writeln!(file, "location: {}", report.location)?;
    writeln!(file, "backtrace:")?;
    writeln!(file, "{}", report.backtrace)?;
    writeln!(file)?;

    Ok(())
}

fn latest_crash_notice_from_path(path: &Path) -> Result<Option<CrashNotice>, io::Error> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };

    if metadata.len() == 0 {
        return Ok(None);
    }

    let contents = fs::read_to_string(path)?;
    let summary = latest_crash_summary(&contents).unwrap_or_else(|| "panic recorded".to_owned());
    let modified = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|modified| modified.as_nanos())
        .unwrap_or_default();
    let fingerprint = format!("{}:{modified}", metadata.len());

    Ok(Some(CrashNotice {
        path: path.to_path_buf(),
        fingerprint,
        summary,
    }))
}

fn latest_crash_summary(contents: &str) -> Option<String> {
    let latest_report = contents
        .rsplit("=== crash ")
        .find(|section| section.contains("panic:"))?;

    let panic = latest_report
        .lines()
        .find_map(|line| line.strip_prefix("panic: "))?;
    let location = latest_report
        .lines()
        .find_map(|line| line.strip_prefix("location: "))
        .unwrap_or("unknown");

    Some(format!("{panic} at {location}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crash_report_write_can_be_loaded_as_notice() {
        let path = unique_test_crash_path();
        let report = CrashReport {
            occurred_at: Local::now(),
            thread_name: "test-thread".to_owned(),
            message: "simulated crash".to_owned(),
            location: "src/example.rs:12:34".to_owned(),
            backtrace: "fake backtrace".to_owned(),
        };

        write_crash_report_to_path(&path, &report).expect("crash report should write");
        let notice = latest_crash_notice_from_path(&path)
            .expect("notice should load")
            .expect("notice should exist");

        assert!(notice.fingerprint.contains(':'));
        assert_eq!(notice.path, path);
        assert_eq!(notice.summary, "simulated crash at src/example.rs:12:34");
    }

    #[test]
    fn missing_crash_report_has_no_notice() {
        let notice = latest_crash_notice_from_path(&unique_test_crash_path())
            .expect("missing crash report should be ok");

        assert_eq!(notice, None);
    }

    fn unique_test_crash_path() -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos();

        std::env::temp_dir()
            .join(format!("alarm_at_game_end_crash_test_{unique}"))
            .join("crash.log")
    }
}
