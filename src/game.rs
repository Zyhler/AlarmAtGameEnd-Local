use crate::alarm::GameActivity;
use crate::counter_strike::CounterStrike2Detector;
use crate::league::LeagueDetector;
use std::path::PathBuf;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GameDetectionConfig {
    pub league_lockfile_override: Option<PathBuf>,
    pub counter_strike_2_gsi_port: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub enum GameStatus {
    Active {
        game: &'static str,
        detail: Option<String>,
    },
    Inactive {
        game: &'static str,
        detail: Option<String>,
    },
    Unavailable {
        game: &'static str,
        message: String,
    },
}

impl GameStatus {
    pub fn active(game: &'static str, detail: Option<String>) -> Self {
        Self::Active { game, detail }
    }

    pub fn inactive(game: &'static str, detail: Option<String>) -> Self {
        Self::Inactive { game, detail }
    }

    pub fn unavailable(game: &'static str, message: impl Into<String>) -> Self {
        Self::Unavailable {
            game,
            message: message.into(),
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self, Self::Active { .. })
    }

    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable { .. })
    }

    pub fn activity(&self) -> GameActivity {
        if self.is_active() {
            GameActivity::Active
        } else if self.is_unavailable() {
            GameActivity::Unavailable
        } else {
            GameActivity::Inactive
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Active { game, detail } => match detail {
                Some(detail) => format!("{game} active: {detail}"),
                None => format!("{game} active"),
            },
            Self::Inactive { game, detail } => match detail {
                Some(detail) => format!("{game}: {detail}"),
                None => format!("{game} inactive"),
            },
            Self::Unavailable { game, message } => format!("{game} unavailable: {message}"),
        }
    }
}

pub trait GameDetector: Send {
    fn status(&mut self) -> GameStatus;

    fn apply_config(&mut self, _config: &GameDetectionConfig) {}
}

pub struct GameMonitor {
    detectors: Vec<Box<dyn GameDetector>>,
}

impl GameMonitor {
    pub fn new(detectors: Vec<Box<dyn GameDetector>>) -> Self {
        Self { detectors }
    }

    pub fn default_with_config(config: GameDetectionConfig) -> Self {
        let league_detector = LeagueDetector::new(config.league_lockfile_override.clone())
            .map(|detector| Box::new(detector) as Box<dyn GameDetector>)
            .unwrap_or_else(|error| {
                Box::new(StaticUnavailableDetector {
                    game: crate::league::GAME_NAME,
                    message: error.to_string(),
                }) as Box<dyn GameDetector>
            });

        let counter_strike_2_detector = Box::new(CounterStrike2Detector::new(
            config.counter_strike_2_gsi_port,
        )) as Box<dyn GameDetector>;

        Self::new(vec![league_detector, counter_strike_2_detector])
    }

    pub fn apply_config(&mut self, config: &GameDetectionConfig) {
        for detector in &mut self.detectors {
            detector.apply_config(config);
        }
    }

    pub fn status(&mut self) -> GameMonitorStatus {
        GameMonitorStatus {
            statuses: self
                .detectors
                .iter_mut()
                .map(|detector| detector.status())
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GameMonitorStatus {
    pub statuses: Vec<GameStatus>,
}

impl GameMonitorStatus {
    pub fn activity(&self) -> GameActivity {
        if self.statuses.iter().any(GameStatus::is_active) {
            GameActivity::Active
        } else if self.statuses.iter().any(GameStatus::is_unavailable) {
            GameActivity::Unavailable
        } else {
            GameActivity::Inactive
        }
    }

    pub fn label(&self) -> String {
        let active_count = self
            .statuses
            .iter()
            .filter(|status| status.is_active())
            .count();

        if let Some(active) = self.statuses.iter().find(|status| status.is_active()) {
            if active_count == 1 {
                return active.label();
            }

            return format!("{} (+{} more)", active.label(), active_count - 1);
        }

        match self.statuses.as_slice() {
            [] => "No game detectors configured".to_owned(),
            [status] => status.label(),
            statuses => {
                let unavailable_count = statuses
                    .iter()
                    .filter(|status| status.is_unavailable())
                    .count();

                if unavailable_count == statuses.len() {
                    "Game detection unavailable".to_owned()
                } else if unavailable_count > 0 {
                    format!("No tracked game active ({unavailable_count} detector unavailable)")
                } else {
                    "No tracked game active".to_owned()
                }
            }
        }
    }
}

struct StaticUnavailableDetector {
    game: &'static str,
    message: String,
}

impl GameDetector for StaticUnavailableDetector {
    fn status(&mut self) -> GameStatus {
        GameStatus::unavailable(self.game, self.message.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StaticDetector(GameStatus);

    impl GameDetector for StaticDetector {
        fn status(&mut self) -> GameStatus {
            self.0.clone()
        }
    }

    #[test]
    fn monitor_reports_active_when_any_game_is_active() {
        let mut monitor = GameMonitor::new(vec![
            Box::new(StaticDetector(GameStatus::inactive(
                "League of Legends",
                None,
            ))),
            Box::new(StaticDetector(GameStatus::active(
                "Another Game",
                Some("running".to_owned()),
            ))),
        ]);

        let status = monitor.status();

        assert_eq!(status.activity(), GameActivity::Active);
        assert_eq!(status.label(), "Another Game active: running");
    }

    #[test]
    fn monitor_does_not_block_alarm_when_detector_is_unavailable() {
        let mut monitor = GameMonitor::new(vec![Box::new(StaticDetector(
            GameStatus::unavailable("League of Legends", "not reachable"),
        ))]);

        assert_eq!(monitor.status().activity(), GameActivity::Unavailable);
    }
}
