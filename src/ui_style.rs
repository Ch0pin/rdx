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
            rgb(23, 27, 34),
            rgb(29, 34, 43),
            rgb(19, 23, 30),
            rgb(39, 47, 60),
            rgb(53, 63, 78),
            rgb(231, 237, 246),
            rgb(171, 184, 202),
            rgb(138, 180, 255),
            rgb(37, 54, 82),
        )
    } else {
        (
            rgb(247, 249, 252),
            rgb(255, 255, 255),
            rgb(255, 255, 255),
            rgb(235, 240, 248),
            rgb(219, 226, 236),
            rgb(30, 41, 59),
            rgb(88, 103, 124),
            rgb(29, 78, 216),
            rgb(226, 236, 255),
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
    style.spacing.interact_size.y = 28.0;
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
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.selection.bg_fill = selection;
    v.selection.stroke = Stroke::new(1.0_f32, accent);
    for widget in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        widget.corner_radius = CornerRadius::same(6);
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

/// Shared table treatment keeps search and reference views visually consistent.
pub fn result_row(ui: &egui::Ui, rect: egui::Rect, index: usize, selected: bool, hovered: bool) {
    let fill = if selected {
        ui.visuals().selection.bg_fill
    } else if hovered {
        ui.visuals().widgets.hovered.bg_fill
    } else if index.is_multiple_of(2) {
        ui.visuals().window_fill
    } else {
        ui.visuals().panel_fill
    };
    ui.painter().rect_filled(rect, 3.0, fill);
    if selected {
        let marker = egui::Rect::from_min_max(
            rect.min + egui::vec2(0.0, 2.0),
            egui::pos2(rect.left() + 3.0, rect.bottom() - 2.0),
        );
        ui.painter()
            .rect_filled(marker, 1.5, ui.visuals().selection.stroke.color);
    }
}

pub fn result_header(ui: &egui::Ui, rect: egui::Rect) {
    ui.painter()
        .rect_filled(rect, 4.0, ui.visuals().faint_bg_color);
}

/// Flat grouping around toolbar actions; no state or extra repaint work.
pub fn action_group(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(4, 2))
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
            assert!(contrast(v.widgets.inactive.fg_stroke.color, v.selection.bg_fill) >= 4.5);
            assert!(contrast(v.widgets.inactive.fg_stroke.color, v.window_fill) >= 4.5);
            assert!(
                contrast(
                    v.widgets.inactive.fg_stroke.color,
                    v.widgets.hovered.bg_fill
                ) >= 4.5
            );
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
