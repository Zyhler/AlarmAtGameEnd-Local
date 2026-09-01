use notify_rust::{Notification, Timeout};
use thiserror::Error;

pub trait Notifier: Send {
    fn alarm(&self, label: &str) -> Result<(), NotificationError>;
}

#[derive(Clone, Debug, Default)]
pub struct DesktopNotifier;

impl Notifier for DesktopNotifier {
    fn alarm(&self, label: &str) -> Result<(), NotificationError> {
        show_notification(crate::APP_NAME, label)
    }
}

fn show_notification(summary: &str, body: &str) -> Result<(), NotificationError> {
    Notification::new()
        .summary(summary)
        .body(body)
        .timeout(Timeout::Never)
        .show()
        .map(|_| ())
        .map_err(|error| NotificationError::Show(error.to_string()))
}

#[derive(Debug, Error, PartialEq)]
pub enum NotificationError {
    #[error("could not show system notification: {0}")]
    Show(String),
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    pub struct RecordingNotifier {
        messages: Arc<Mutex<Vec<String>>>,
    }

    impl RecordingNotifier {
        pub fn messages(&self) -> Vec<String> {
            self.messages.lock().expect("messages lock").clone()
        }
    }

    impl Notifier for RecordingNotifier {
        fn alarm(&self, label: &str) -> Result<(), NotificationError> {
            self.messages
                .lock()
                .expect("messages lock")
                .push(label.to_owned());
            Ok(())
        }
    }
}
