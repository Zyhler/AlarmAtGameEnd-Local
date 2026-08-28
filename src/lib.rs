pub mod alarm;
pub mod app;
pub mod counter_strike;
pub mod crash;
pub mod game;
pub mod league;
pub mod notifier;
pub mod sound;
pub mod storage;
pub mod theme;
pub mod worker;

pub const APP_NAME: &str = "Alarm at Game End";

pub fn run() -> eframe::Result {
    let native_options = native_options_from_saved_state();

    eframe::run_native(
        APP_NAME,
        native_options,
        Box::new(|cc| Ok(Box::new(app::AlarmApp::new(cc)))),
    )
}

fn native_options_from_saved_state() -> eframe::NativeOptions {
    let mut native_options = eframe::NativeOptions::default();

    if let Ok(state) = storage::load_state() {
        apply_graphics_backend_preference(&mut native_options, state.graphics_backend_preference);
    }

    native_options
}

pub fn graphics_backend_renderer(
    preference: storage::PersistedGraphicsBackendPreference,
) -> Option<eframe::Renderer> {
    match preference {
        storage::PersistedGraphicsBackendPreference::Auto => None,
        storage::PersistedGraphicsBackendPreference::Wgpu => Some(eframe::Renderer::Wgpu),
        storage::PersistedGraphicsBackendPreference::OpenGl => Some(eframe::Renderer::Glow),
    }
}

fn apply_graphics_backend_preference(
    native_options: &mut eframe::NativeOptions,
    preference: storage::PersistedGraphicsBackendPreference,
) {
    if let Some(renderer) = graphics_backend_renderer(preference) {
        native_options.renderer = renderer;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::PersistedGraphicsBackendPreference;

    #[test]
    fn auto_graphics_backend_keeps_native_default_renderer() {
        assert_eq!(
            graphics_backend_renderer(PersistedGraphicsBackendPreference::Auto),
            None
        );
    }

    #[test]
    fn explicit_graphics_backend_maps_to_renderer() {
        assert_eq!(
            graphics_backend_renderer(PersistedGraphicsBackendPreference::Wgpu),
            Some(eframe::Renderer::Wgpu)
        );
        assert_eq!(
            graphics_backend_renderer(PersistedGraphicsBackendPreference::OpenGl),
            Some(eframe::Renderer::Glow)
        );
    }
}
