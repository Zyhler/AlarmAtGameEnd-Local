use crate::storage::PersistedThemePreference;
use eframe::egui;

pub const FALLBACK_ACCENT_COLOR: egui::Color32 = egui::Color32::from_rgb(75, 125, 245);

pub fn apply_app_style(
    ctx: &egui::Context,
    theme_preference: PersistedThemePreference,
    accent_color: egui::Color32,
) {
    ctx.set_theme(egui_theme_preference(theme_preference));
    apply_style_for_theme(ctx, egui::Theme::Dark, accent_color);
    apply_style_for_theme(ctx, egui::Theme::Light, accent_color);
    ctx.request_repaint();
}

pub fn system_accent_color() -> egui::Color32 {
    platform_accent_color().unwrap_or(FALLBACK_ACCENT_COLOR)
}

pub fn egui_theme_preference(theme_preference: PersistedThemePreference) -> egui::ThemePreference {
    match theme_preference {
        PersistedThemePreference::System => egui::ThemePreference::System,
        PersistedThemePreference::Dark => egui::ThemePreference::Dark,
        PersistedThemePreference::Light => egui::ThemePreference::Light,
    }
}

pub fn muted_text_color(visuals: &egui::Visuals) -> egui::Color32 {
    if visuals.dark_mode {
        egui::Color32::from_gray(205)
    } else {
        egui::Color32::from_gray(70)
    }
}

pub fn main_text_color(visuals: &egui::Visuals) -> egui::Color32 {
    if visuals.dark_mode {
        egui::Color32::from_gray(235)
    } else {
        egui::Color32::from_gray(30)
    }
}

pub fn error_text_color(visuals: &egui::Visuals) -> egui::Color32 {
    if visuals.dark_mode {
        egui::Color32::from_rgb(245, 115, 105)
    } else {
        egui::Color32::from_rgb(170, 40, 35)
    }
}

pub fn contrast_text_color(background: egui::Color32) -> egui::Color32 {
    if relative_luminance(background) > 0.46 {
        egui::Color32::BLACK
    } else {
        egui::Color32::WHITE
    }
}

pub fn accent_hover_color(accent_color: egui::Color32, dark_mode: bool) -> egui::Color32 {
    if dark_mode {
        mix_colors(accent_color, egui::Color32::WHITE, 0.16)
    } else {
        mix_colors(accent_color, egui::Color32::BLACK, 0.08)
    }
}

pub fn accent_active_color(accent_color: egui::Color32, dark_mode: bool) -> egui::Color32 {
    if dark_mode {
        mix_colors(accent_color, egui::Color32::WHITE, 0.08)
    } else {
        mix_colors(accent_color, egui::Color32::BLACK, 0.16)
    }
}

pub fn color_from_windows_argb(value: u32) -> egui::Color32 {
    let red = ((value >> 16) & 0xff) as u8;
    let green = ((value >> 8) & 0xff) as u8;
    let blue = (value & 0xff) as u8;

    egui::Color32::from_rgb(red, green, blue)
}

fn apply_style_for_theme(ctx: &egui::Context, theme: egui::Theme, accent_color: egui::Color32) {
    ctx.style_mut_of(theme, |style| {
        if let Some(font_id) = style.text_styles.get_mut(&egui::TextStyle::Small) {
            font_id.size = 10.0;
        }
        if let Some(font_id) = style.text_styles.get_mut(&egui::TextStyle::Body) {
            font_id.size = 14.0;
        }
        if let Some(font_id) = style.text_styles.get_mut(&egui::TextStyle::Button) {
            font_id.size = 14.0;
        }
        if let Some(font_id) = style.text_styles.get_mut(&egui::TextStyle::Heading) {
            font_id.size = 19.0;
        }
        if let Some(font_id) = style.text_styles.get_mut(&egui::TextStyle::Monospace) {
            font_id.size = 13.5;
        }

        let dark_mode = theme == egui::Theme::Dark;
        let text_color = if dark_mode {
            egui::Color32::from_gray(235)
        } else {
            egui::Color32::from_gray(30)
        };
        let hovered_text_color = if dark_mode {
            egui::Color32::WHITE
        } else {
            egui::Color32::BLACK
        };

        style.spacing.button_padding = egui::vec2(8.0, 4.0);

        let visuals = &mut style.visuals;
        visuals.weak_text_color = Some(if dark_mode {
            egui::Color32::from_gray(205)
        } else {
            egui::Color32::from_gray(70)
        });
        visuals.selection.bg_fill = accent_color;
        visuals.selection.stroke = egui::Stroke::new(1.0, contrast_text_color(accent_color));
        visuals.hyperlink_color = accent_color;
        visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, text_color);
        visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, text_color);
        visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, hovered_text_color);
        visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, hovered_text_color);
        visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, accent_color);
        visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, accent_color);
        visuals.widgets.noninteractive.expansion = 0.0;
        visuals.widgets.inactive.expansion = 0.0;
        visuals.widgets.hovered.expansion = 0.0;
        visuals.widgets.active.expansion = 0.0;
        visuals.widgets.open.expansion = 0.0;
    });
}

fn mix_colors(left: egui::Color32, right: egui::Color32, right_amount: f32) -> egui::Color32 {
    let right_amount = right_amount.clamp(0.0, 1.0);
    let left_amount = 1.0 - right_amount;
    let [left_red, left_green, left_blue, left_alpha] = left.to_array();
    let [right_red, right_green, right_blue, right_alpha] = right.to_array();

    egui::Color32::from_rgba_unmultiplied(
        mix_channel(left_red, right_red, left_amount, right_amount),
        mix_channel(left_green, right_green, left_amount, right_amount),
        mix_channel(left_blue, right_blue, left_amount, right_amount),
        mix_channel(left_alpha, right_alpha, left_amount, right_amount),
    )
}

fn mix_channel(left: u8, right: u8, left_amount: f32, right_amount: f32) -> u8 {
    ((left as f32 * left_amount) + (right as f32 * right_amount)).round() as u8
}

fn relative_luminance(color: egui::Color32) -> f32 {
    let [red, green, blue, _] = color.to_array();
    0.2126 * linear_channel(red) + 0.7152 * linear_channel(green) + 0.0722 * linear_channel(blue)
}

fn linear_channel(value: u8) -> f32 {
    let value = value as f32 / 255.0;

    if value <= 0.03928 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(windows)]
fn platform_accent_color() -> Option<egui::Color32> {
    use windows_sys::Win32::Graphics::Dwm::DwmGetColorizationColor;

    let mut color = 0_u32;
    let mut opaque_blend = 0;
    let result = unsafe { DwmGetColorizationColor(&mut color, &mut opaque_blend) };

    if result < 0 {
        None
    } else {
        Some(color_from_windows_argb(color))
    }
}

#[cfg(not(windows))]
fn platform_accent_color() -> Option<egui::Color32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_argb_color_uses_rgb_channels() {
        assert_eq!(
            color_from_windows_argb(0xff_12_34_56),
            egui::Color32::from_rgb(0x12, 0x34, 0x56)
        );
    }

    #[test]
    fn contrast_text_color_uses_dark_text_on_light_background() {
        assert_eq!(
            contrast_text_color(egui::Color32::from_rgb(240, 240, 240)),
            egui::Color32::BLACK
        );
    }

    #[test]
    fn contrast_text_color_uses_light_text_on_dark_background() {
        assert_eq!(
            contrast_text_color(egui::Color32::from_rgb(20, 30, 40)),
            egui::Color32::WHITE
        );
    }
}
