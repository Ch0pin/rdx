//! Application chrome only. Stored interface choice and syntax palettes stay independent.
use eframe::egui::{self, Color32, CornerRadius, FontId, Stroke, TextStyle};

pub fn install(ctx: &egui::Context) {
    ctx.all_styles_mut(refine);
}

fn refine(style: &mut egui::Style) {
    let dark = style.visuals.dark_mode;
    let rgb = Color32::from_rgb;
    let (panel, window, field, hover, border, text, muted, accent, selection) = if dark {
        (
            rgb(29, 33, 39),
            rgb(33, 38, 45),
            rgb(23, 27, 33),
            rgb(43, 51, 61),
            rgb(56, 64, 75),
            rgb(225, 231, 239),
            rgb(163, 177, 192),
            rgb(93, 198, 231),
            rgb(34, 66, 82),
        )
    } else {
        (
            rgb(246, 248, 251),
            rgb(255, 255, 255),
            rgb(255, 255, 255),
            rgb(231, 237, 244),
            rgb(209, 217, 228),
            rgb(35, 46, 61),
            rgb(89, 105, 123),
            rgb(0, 103, 137),
            rgb(211, 235, 246),
        )
    };
    style
        .text_styles
        .insert(TextStyle::Body, FontId::proportional(14.0));
    style
        .text_styles
        .insert(TextStyle::Button, FontId::proportional(14.0));
    style
        .text_styles
        .insert(TextStyle::Small, FontId::proportional(12.0));
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(8.0, 5.0);
    style.spacing.interact_size.y = 26.0;
    style.spacing.indent = 18.0;
    // No animated transitions or additional redraw requests for decorative chrome.
    style.animation_time = 0.0;
    let v = &mut style.visuals;
    v.panel_fill = panel;
    v.window_fill = window;
    v.extreme_bg_color = field;
    v.faint_bg_color = hover;
    v.hyperlink_color = accent;
    v.window_stroke = Stroke::new(1.0_f32, border);
    v.window_corner_radius = CornerRadius::same(6);
    v.menu_corner_radius = CornerRadius::same(5);
    v.selection.bg_fill = selection;
    v.selection.stroke = Stroke::new(1.0_f32, accent);
    for widget in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        widget.corner_radius = CornerRadius::same(4);
        widget.bg_stroke = Stroke::new(1.0_f32, border);
        widget.fg_stroke = Stroke::new(1.0_f32, text);
        widget.expansion = 0.0;
    }
    v.widgets.noninteractive.bg_fill = panel;
    v.widgets.noninteractive.weak_bg_fill = panel;
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, muted);
    v.widgets.inactive.bg_fill = field;
    v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    v.widgets.hovered.bg_fill = hover;
    v.widgets.hovered.weak_bg_fill = hover;
    v.widgets.active.bg_fill = selection;
    v.widgets.active.weak_bg_fill = selection;
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, accent);
    v.widgets.open.bg_fill = selection;
    v.widgets.open.weak_bg_fill = selection;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contrast(a: Color32, b: Color32) -> f32 {
        let luminance = |c: Color32| {
            let linear = |n: u8| {
                let x = n as f32 / 255.0;
                if x <= 0.04045 {
                    x / 12.92
                } else {
                    ((x + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(c.r()) + 0.7152 * linear(c.g()) + 0.0722 * linear(c.b())
        };
        let a = luminance(a);
        let b = luminance(b);
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn both_palettes_keep_labels_and_selection_readable() {
        for visuals in [egui::Visuals::dark(), egui::Visuals::light()] {
            let mut style = egui::Style {
                visuals,
                ..Default::default()
            };
            refine(&mut style);
            let v = &style.visuals;
            assert!(contrast(v.widgets.inactive.fg_stroke.color, v.panel_fill) >= 4.5);
            assert!(contrast(v.widgets.noninteractive.fg_stroke.color, v.panel_fill) >= 4.5);
            assert!(contrast(v.selection.stroke.color, v.selection.bg_fill) >= 4.5);
        }
    }

    #[test]
    fn styling_preserves_interface_preference_and_code_font_style() {
        let ctx = egui::Context::default();
        ctx.set_theme(egui::ThemePreference::Light);
        let mono = ctx.style().text_styles[&TextStyle::Monospace].clone();
        install(&ctx);
        assert_eq!(ctx.theme(), egui::Theme::Light);
        assert_eq!(ctx.style().text_styles[&TextStyle::Monospace], mono);
    }
}
