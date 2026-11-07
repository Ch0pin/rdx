//! Region screenshots of the viewer.
//!
//! The user holds the platform command modifier (Cmd on macOS, Ctrl elsewhere),
//! presses the right mouse button over the content area and drags a rectangle.
//! On release RDX asks eframe for a frame capture, crops it to the selection and
//! offers a save dialog named after the class or file shown in the active tab.

use eframe::egui::{self, Color32, ColorImage, Pos2, Rect};
use std::io::Cursor;

/// Selections narrower or shorter than this (in points) are treated as an
/// accidental click and ignored.
pub const MIN_SELECTION_POINTS: f32 = 4.0;

/// The gesture modifier: the platform command key (Cmd on macOS, Ctrl elsewhere),
/// with plain Ctrl also accepted on macOS so the shortcut reads the same everywhere.
pub fn modifier_held(modifiers: &egui::Modifiers) -> bool {
    modifiers.command || modifiers.mac_cmd || modifiers.ctrl
}

/// macOS has no guaranteed right button: one-button mice and Ctrl+click are the
/// platform's secondary click, but winit reports both as the primary button.
/// Rewrites a primary press made with the gesture modifier into a secondary
/// press (and its matching release), so the gesture works with any pointer.
/// `remapped` carries the press/release pairing across frames.
pub fn remap_modifier_primary(events: &mut [egui::Event], remapped: &mut bool) {
    if !cfg!(target_os = "macos") {
        return;
    }
    for event in events {
        let egui::Event::PointerButton {
            button,
            pressed,
            modifiers,
            ..
        } = event
        else {
            continue;
        };
        if *button != egui::PointerButton::Primary {
            continue;
        }
        if *pressed {
            if modifier_held(modifiers) {
                *button = egui::PointerButton::Secondary;
                *remapped = true;
            }
        } else if *remapped {
            *button = egui::PointerButton::Secondary;
            *remapped = false;
        }
    }
}

/// Gesture progress across frames.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum RegionCapture {
    #[default]
    Idle,
    /// Right button is held with the modifier; `anchor` is where it was pressed.
    Dragging { anchor: Pos2 },
    /// A frame capture was requested; waiting for `egui::Event::Screenshot`.
    Pending,
}

/// Attached to the viewport screenshot command so unrelated screenshot replies
/// are ignored.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// Selected area in UI points.
    pub rect: Rect,
    /// File stem proposed in the save dialog.
    pub stem: String,
}

/// Builds the selection rectangle from the press anchor and the current pointer,
/// clamped to `bounds`. Returns `None` when the selection is too small to be a
/// deliberate drag.
pub fn selection(anchor: Pos2, current: Pos2, bounds: Rect) -> Option<Rect> {
    let rect = Rect::from_two_pos(anchor, current).intersect(bounds);
    (rect.is_positive()
        && rect.width() >= MIN_SELECTION_POINTS
        && rect.height() >= MIN_SELECTION_POINTS)
        .then_some(rect)
}

/// Height of the caption bar appended to every capture, in UI points.
pub const CAPTION_HEIGHT: f32 = 22.0;
/// Horizontal padding of the caption text inside the bar.
pub const CAPTION_PADDING: f32 = 6.0;

/// The class or file name shown in the caption: the tab name without the
/// `dex://` / resource scheme and without any ` | ` suffix.
pub fn display_name(tab_name: &str) -> String {
    let name = tab_name
        .strip_prefix("dex://")
        .or_else(|| tab_name.strip_prefix(rdx::resource_table::PREFIX))
        .unwrap_or(tab_name);
    name.split(" | ").next().unwrap_or(name).trim().to_owned()
}

/// Places the caption bar next to `selection`: below it when there is room,
/// otherwise above, otherwise over its bottom edge. Returns
/// `(capture_rect, caption_rect)`, where `capture_rect` covers both.
pub fn caption_layout(selection: Rect, screen: Rect) -> (Rect, Rect) {
    let below = Rect::from_min_size(
        selection.left_bottom(),
        egui::vec2(selection.width(), CAPTION_HEIGHT),
    );
    if below.max.y <= screen.max.y {
        return (selection.union(below), below);
    }
    let above = Rect::from_min_size(
        selection.left_top() - egui::vec2(0.0, CAPTION_HEIGHT),
        egui::vec2(selection.width(), CAPTION_HEIGHT),
    );
    if above.min.y >= screen.min.y {
        return (selection.union(above), above);
    }
    let height = CAPTION_HEIGHT.min(selection.height());
    let inside = Rect::from_min_size(
        egui::pos2(selection.min.x, selection.max.y - height),
        egui::vec2(selection.width(), height),
    );
    (selection, inside)
}

/// Caption bar colours as `(background, text)`.
pub fn caption_colors(dark_mode: bool) -> (Color32, Color32) {
    if dark_mode {
        (
            Color32::from_rgb(30, 30, 30),
            Color32::from_rgb(230, 230, 230),
        )
    } else {
        (
            Color32::from_rgb(235, 235, 235),
            Color32::from_rgb(25, 25, 25),
        )
    }
}

/// Derives a filesystem-safe file stem from a tab name such as
/// `com.example.LoginActivity`, `dex://com.example.Foo` or `res/layout/main.xml`.
pub fn file_stem(tab_name: &str) -> String {
    let mut stem: String = display_name(tab_name)
        .trim_matches(|c| c == '/' || c == '\\')
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    if stem.len() > 120 {
        let mut end = 120;
        while !stem.is_char_boundary(end) {
            end -= 1;
        }
        stem.truncate(end);
    }
    let stem = stem.trim_matches(|c| c == '.' || c == ' ' || c == '_');
    if stem.is_empty() {
        "screenshot".to_owned()
    } else {
        stem.to_owned()
    }
}

/// Crops a full-window capture to `rect` (UI points) at `pixels_per_point`.
/// Coordinates are clamped to the image, so this never panics; returns `None`
/// if nothing remains.
pub fn crop(image: &ColorImage, rect: Rect, pixels_per_point: f32) -> Option<ColorImage> {
    let scale = if pixels_per_point.is_finite() && pixels_per_point > 0.0 {
        pixels_per_point
    } else {
        1.0
    };
    let [width, height] = image.size;
    let clamp_x = |v: f32| ((v * scale).round().max(0.0) as usize).min(width);
    let clamp_y = |v: f32| ((v * scale).round().max(0.0) as usize).min(height);
    let (x0, x1) = (clamp_x(rect.min.x), clamp_x(rect.max.x));
    let (y0, y1) = (clamp_y(rect.min.y), clamp_y(rect.max.y));
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (w, h) = (x1 - x0, y1 - y0);
    let mut pixels = Vec::with_capacity(w * h);
    for y in y0..y1 {
        let start = y * width + x0;
        pixels.extend_from_slice(&image.pixels[start..start + w]);
    }
    Some(ColorImage {
        size: [w, h],
        pixels,
    })
}

/// Encodes the capture as PNG. Screenshots are opaque, so the alpha channel is
/// forced to 255 to avoid premultiplication artefacts in the output.
pub fn encode_png(image: &ColorImage) -> anyhow::Result<Vec<u8>> {
    let [width, height] = image.size;
    let mut rgba = Vec::with_capacity(width * height * 4);
    for pixel in &image.pixels {
        let [r, g, b, _] = pixel.to_srgba_unmultiplied();
        rgba.extend_from_slice(&[r, g, b, 255]);
    }
    let buffer = image::RgbaImage::from_raw(width as u32, height as u32, rgba)
        .ok_or_else(|| anyhow::anyhow!("screenshot dimensions do not match pixel data"))?;
    let mut out = Cursor::new(Vec::new());
    buffer.write_to(&mut out, image::ImageFormat::Png)?;
    Ok(out.into_inner())
}

/// Overlay colours for the in-progress selection.
pub fn overlay_colors(dark_mode: bool) -> (Color32, Color32) {
    if dark_mode {
        (
            Color32::from_rgba_unmultiplied(90, 170, 255, 40),
            Color32::from_rgb(120, 190, 255),
        )
    } else {
        (
            Color32::from_rgba_unmultiplied(30, 110, 220, 40),
            Color32::from_rgb(30, 110, 220),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::pos2;

    #[test]
    fn selection_is_normalised_clamped_and_rejects_tiny_drags() {
        let bounds = Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 50.0));
        let rect = selection(pos2(80.0, 40.0), pos2(20.0, 10.0), bounds).unwrap();
        assert_eq!(rect, Rect::from_min_max(pos2(20.0, 10.0), pos2(80.0, 40.0)));
        let clamped = selection(pos2(-10.0, -10.0), pos2(500.0, 500.0), bounds).unwrap();
        assert_eq!(clamped, bounds);
        assert!(selection(pos2(10.0, 10.0), pos2(12.0, 40.0), bounds).is_none());
        assert!(selection(pos2(10.0, 10.0), pos2(10.0, 10.0), bounds).is_none());
        assert!(selection(pos2(200.0, 200.0), pos2(300.0, 300.0), bounds).is_none());
    }

    #[test]
    fn modifier_accepts_command_or_ctrl_only() {
        assert!(modifier_held(&egui::Modifiers::COMMAND));
        assert!(modifier_held(&egui::Modifiers::CTRL));
        assert!(modifier_held(&egui::Modifiers::MAC_CMD));
        assert!(!modifier_held(&egui::Modifiers::NONE));
        assert!(!modifier_held(&egui::Modifiers::SHIFT));
        assert!(!modifier_held(&egui::Modifiers::ALT));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn modifier_primary_press_and_release_become_secondary() {
        use egui::{Event, Modifiers, PointerButton};
        let click = |button, pressed, modifiers| Event::PointerButton {
            pos: pos2(1.0, 1.0),
            button,
            pressed,
            modifiers,
        };
        let button_of = |event: &Event| match event {
            Event::PointerButton { button, .. } => *button,
            _ => unreachable!(),
        };
        let mut remapped = false;
        // Press with the modifier is remapped; the release is remapped even
        // though the modifier was let go first.
        let mut events = vec![click(PointerButton::Primary, true, Modifiers::CTRL)];
        remap_modifier_primary(&mut events, &mut remapped);
        assert_eq!(button_of(&events[0]), PointerButton::Secondary);
        assert!(remapped);
        let mut events = vec![click(PointerButton::Primary, false, Modifiers::NONE)];
        remap_modifier_primary(&mut events, &mut remapped);
        assert_eq!(button_of(&events[0]), PointerButton::Secondary);
        assert!(!remapped);
        // Plain clicks and real right clicks are untouched.
        let mut events = vec![
            click(PointerButton::Primary, true, Modifiers::NONE),
            click(PointerButton::Primary, false, Modifiers::NONE),
            click(PointerButton::Secondary, true, Modifiers::COMMAND),
        ];
        remap_modifier_primary(&mut events, &mut remapped);
        assert_eq!(button_of(&events[0]), PointerButton::Primary);
        assert_eq!(button_of(&events[1]), PointerButton::Primary);
        assert_eq!(button_of(&events[2]), PointerButton::Secondary);
        assert!(!remapped);
        // Cmd works too.
        let mut events = vec![click(PointerButton::Primary, true, Modifiers::MAC_CMD)];
        remap_modifier_primary(&mut events, &mut remapped);
        assert_eq!(button_of(&events[0]), PointerButton::Secondary);
    }

    #[test]
    fn display_name_strips_schemes_and_suffixes() {
        assert_eq!(
            display_name("dex://com.example.Foo$Bar"),
            "com.example.Foo$Bar"
        );
        assert_eq!(
            display_name(&format!(
                "{}res/layout/main.xml",
                rdx::resource_table::PREFIX
            )),
            "res/layout/main.xml"
        );
        assert_eq!(display_name("a.b.C | smali"), "a.b.C");
        assert_eq!(
            display_name("  AndroidManifest.xml "),
            "AndroidManifest.xml"
        );
    }

    #[test]
    fn caption_goes_below_then_above_then_inside() {
        let screen = Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 600.0));
        let selection = Rect::from_min_max(pos2(100.0, 100.0), pos2(300.0, 250.0));
        let (capture, bar) = caption_layout(selection, screen);
        assert_eq!(
            bar,
            Rect::from_min_max(pos2(100.0, 250.0), pos2(300.0, 272.0))
        );
        assert_eq!(
            capture,
            Rect::from_min_max(pos2(100.0, 100.0), pos2(300.0, 272.0))
        );
        // No room below: the bar goes above.
        let low = Rect::from_min_max(pos2(100.0, 400.0), pos2(300.0, 590.0));
        let (capture, bar) = caption_layout(low, screen);
        assert_eq!(
            bar,
            Rect::from_min_max(pos2(100.0, 378.0), pos2(300.0, 400.0))
        );
        assert_eq!(
            capture,
            Rect::from_min_max(pos2(100.0, 378.0), pos2(300.0, 590.0))
        );
        // Full-height selection: the bar overlays the bottom edge.
        let tall = Rect::from_min_max(pos2(100.0, 10.0), pos2(300.0, 590.0));
        let (capture, bar) = caption_layout(tall, screen);
        assert_eq!(capture, tall);
        assert_eq!(
            bar,
            Rect::from_min_max(pos2(100.0, 568.0), pos2(300.0, 590.0))
        );
        // A selection shorter than the bar never produces a bar taller than itself.
        let short = Rect::from_min_max(pos2(100.0, 0.0), pos2(300.0, 600.0));
        let (_, bar) = caption_layout(
            short,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 600.0)),
        );
        assert!(bar.height() <= CAPTION_HEIGHT);
    }

    #[test]
    fn file_stem_handles_classes_resources_and_garbage() {
        assert_eq!(
            file_stem("com.example.LoginActivity"),
            "com.example.LoginActivity"
        );
        assert_eq!(
            file_stem("dex://com.example.Foo$Bar"),
            "com.example.Foo$Bar"
        );
        assert_eq!(file_stem("res/layout/main.xml"), "res_layout_main.xml");
        assert_eq!(file_stem("AndroidManifest.xml"), "AndroidManifest.xml");
        assert_eq!(
            file_stem(&format!(
                "{}res/values/strings.xml",
                rdx::resource_table::PREFIX
            )),
            "res_values_strings.xml"
        );
        assert_eq!(file_stem("a:b*c?\"d<e>f|g"), "a_b_c__d_e_f_g");
        assert_eq!(file_stem("   "), "screenshot");
        assert_eq!(file_stem("..."), "screenshot");
        assert!(file_stem(&"x".repeat(500)).len() <= 120);
    }

    #[test]
    fn crop_scales_by_pixels_per_point_and_clamps_to_image() {
        let mut image = ColorImage::new([8, 6], Color32::BLACK);
        // Mark pixel (4, 2) so we can confirm the crop origin.
        image.pixels[2 * 8 + 4] = Color32::RED;
        let crop = crop(
            &image,
            Rect::from_min_max(pos2(2.0, 1.0), pos2(4.0, 3.0)),
            2.0,
        )
        .unwrap();
        assert_eq!(crop.size, [4, 4]);
        assert_eq!(crop.pixels[0], Color32::RED);
        let clamped = crop_outside_is_clamped(&image);
        assert_eq!(clamped.size, [8, 6]);
        assert!(
            super::crop(
                &image,
                Rect::from_min_max(pos2(20.0, 20.0), pos2(30.0, 30.0)),
                1.0
            )
            .is_none()
        );
        assert!(
            super::crop(
                &image,
                Rect::from_min_max(pos2(1.0, 1.0), pos2(1.0, 5.0)),
                1.0
            )
            .is_none()
        );
        // Non-finite scale falls back to 1.0 instead of panicking.
        assert!(
            super::crop(
                &image,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(2.0, 2.0)),
                f32::NAN
            )
            .is_some()
        );
    }

    fn crop_outside_is_clamped(image: &ColorImage) -> ColorImage {
        crop(
            image,
            Rect::from_min_max(pos2(-5.0, -5.0), pos2(50.0, 50.0)),
            1.0,
        )
        .unwrap()
    }

    #[test]
    fn png_round_trips_pixels_as_opaque() {
        let mut image = ColorImage::new([3, 2], Color32::from_rgb(10, 20, 30));
        image.pixels[4] = Color32::from_rgba_premultiplied(0, 0, 0, 0);
        let png = encode_png(&image).unwrap();
        let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (3, 2));
        assert_eq!(decoded.get_pixel(0, 0).0, [10, 20, 30, 255]);
        assert_eq!(decoded.get_pixel(1, 1).0, [0, 0, 0, 255]);
    }
}
