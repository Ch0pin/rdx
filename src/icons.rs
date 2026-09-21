use eframe::egui::{self, Color32, Pos2, Rect, Sense, Shape, Stroke, Vec2};

#[derive(Clone, Copy)]
pub enum Icon {
    Search,
    Open,
    Reload,
    Back,
    Forward,
    Pin,
    Bookmark,
    Settings,
    Classes,
    Package,
    Assets,
    Resources,
    Manifest,
    Libraries,
    Dex,
    Signature,
    Folder,
    File,
    Image,
    Code,
    Plugin,
}

/// A compact, font-independent icon drawn in a shared 24-unit coordinate space.
pub fn paint(ui: &egui::Ui, rect: Rect, icon: Icon) {
    let painter = ui.painter();
    let scale = rect.width().min(rect.height()) / 24.0;
    let origin = rect.center() - Vec2::splat(12.0 * scale);
    let point = |x: f32, y: f32| origin + Vec2::new(x, y) * scale;
    let accent = match icon {
        Icon::Classes | Icon::Code | Icon::Dex => (91, 153, 220),
        Icon::Assets | Icon::Image => (61, 173, 150),
        Icon::Resources | Icon::Folder | Icon::Open => (201, 157, 72),
        Icon::Manifest | Icon::Signature => (104, 166, 109),
        Icon::Libraries | Icon::Plugin => (165, 133, 209),
        _ => (135, 150, 169),
    };
    let tint = if !ui.is_enabled() {
        ui.visuals().weak_text_color().gamma_multiply(0.45)
    } else if ui.visuals().dark_mode {
        Color32::from_rgb(accent.0, accent.1, accent.2)
    } else {
        Color32::from_rgb(
            (f32::from(accent.0) * 0.72) as u8,
            (f32::from(accent.1) * 0.72) as u8,
            (f32::from(accent.2) * 0.72) as u8,
        )
    };
    let stroke = Stroke::new((1.65 * scale).max(1.0), tint);
    let line = |a: (f32, f32), b: (f32, f32)| {
        painter.line_segment([point(a.0, a.1), point(b.0, b.1)], stroke);
    };
    let path = |coords: &[(f32, f32)], closed: bool, filled: bool| {
        let points: Vec<Pos2> = coords.iter().map(|&(x, y)| point(x, y)).collect();
        if filled {
            painter.add(Shape::convex_polygon(
                points,
                tint.gamma_multiply(0.12),
                stroke,
            ));
        } else if closed {
            painter.add(Shape::closed_line(points, stroke));
        } else {
            painter.add(Shape::line(points, stroke));
        }
    };
    let box_at = |x: f32, y: f32, w: f32, h: f32| {
        let r = Rect::from_min_size(point(x, y), Vec2::new(w, h) * scale);
        painter.rect_filled(r, 2.0 * scale, tint.gamma_multiply(0.10));
        painter.rect_stroke(r, 2.0 * scale, stroke, egui::StrokeKind::Inside);
    };
    let document = || {
        path(
            &[(5., 3.), (14., 3.), (19., 8.), (19., 21.), (5., 21.)],
            true,
            false,
        );
        path(&[(14., 3.), (14., 8.), (19., 8.)], false, false);
    };

    match icon {
        Icon::Open | Icon::Folder => {
            path(
                &[
                    (3., 19.),
                    (3., 6.),
                    (9., 6.),
                    (11., 8.),
                    (20., 8.),
                    (20., 11.),
                ],
                false,
                false,
            );
            path(&[(3., 19.), (6., 11.), (22., 11.), (19., 19.)], true, true);
        }
        Icon::Reload => {
            let points = (0..=24)
                .map(|i| {
                    let angle = 0.6 + i as f32 / 24.0 * 4.85;
                    point(12.0 + angle.cos() * 7.5, 12.0 + angle.sin() * 7.5)
                })
                .collect();
            painter.add(Shape::line(points, stroke));
            path(&[(15., 3.), (18., 7.), (13., 8.)], false, false);
        }
        Icon::Back => {
            path(&[(10., 5.), (3., 12.), (10., 19.)], false, false);
            line((3., 12.), (21., 12.));
        }
        Icon::Forward => {
            path(&[(14., 5.), (21., 12.), (14., 19.)], false, false);
            line((3., 12.), (21., 12.));
        }
        Icon::Pin => {
            path(
                &[
                    (8., 3.),
                    (16., 3.),
                    (15., 11.),
                    (19., 15.),
                    (5., 15.),
                    (9., 11.),
                    (8., 3.),
                ],
                true,
                false,
            );
            line((12., 15.), (12., 22.));
        }
        Icon::Bookmark => {
            path(
                &[
                    (6., 3.),
                    (18., 3.),
                    (18., 21.),
                    (12., 16.),
                    (6., 21.),
                    (6., 3.),
                ],
                true,
                false,
            );
        }
        Icon::Settings => {
            for (y, x) in [(6., 8.), (12., 16.), (18., 10.)] {
                line((3., y), (x - 2., y));
                line((x + 2., y), (21., y));
                painter.circle_stroke(point(x, y), 2.3 * scale, stroke);
            }
        }
        Icon::Classes => {
            box_at(3., 3., 13., 14.);
            path(
                &[(19., 7.), (21., 7.), (21., 21.), (8., 21.), (8., 20.)],
                false,
                false,
            );
            path(
                &[(12., 7.), (8., 7.), (6., 10.), (8., 13.), (12., 13.)],
                false,
                false,
            );
        }
        Icon::Package => {
            path(
                &[
                    (12., 3.),
                    (21., 8.),
                    (21., 17.),
                    (12., 22.),
                    (3., 17.),
                    (3., 8.),
                ],
                true,
                true,
            );
            path(&[(3., 8.), (12., 13.), (21., 8.)], false, false);
            line((12., 13.), (12., 22.));
            line((7.5, 5.5), (16.5, 10.5));
        }
        Icon::Assets => {
            box_at(3., 3., 7., 7.);
            box_at(14., 14., 7., 7.);
            painter.circle_stroke(point(17.5, 6.5), 3.5 * scale, stroke);
            path(&[(6.5, 14.), (10., 21.), (3., 21.)], true, true);
        }
        Icon::Resources => {
            for y in [4., 10., 16.] {
                box_at(3., y, 5., 5.);
                line((12., y + 1.), (21., y + 1.));
                line((12., y + 4.), (18., y + 4.));
            }
        }
        Icon::Manifest => {
            document();
            path(&[(8., 11.), (6.5, 13.), (8., 15.)], false, false);
            path(&[(16., 11.), (17.5, 13.), (16., 15.)], false, false);
            line((13., 10.5), (11., 15.5));
        }
        Icon::Libraries => {
            for (x, h) in [(3., 15.), (9., 18.), (15., 13.)] {
                box_at(x, 21. - h, 5., h);
                line((x + 1.5, 17.), (x + 3.5, 17.));
            }
        }
        Icon::Dex => {
            box_at(6., 6., 12., 12.);
            for i in [8., 12., 16.] {
                line((i, 3.), (i, 6.));
                line((i, 18.), (i, 21.));
                line((3., i), (6., i));
                line((18., i), (21., i));
            }
            box_at(10., 10., 4., 4.);
        }
        Icon::Signature => {
            path(
                &[
                    (12., 3.),
                    (20., 6.),
                    (19., 14.),
                    (16., 18.),
                    (12., 21.),
                    (8., 18.),
                    (5., 14.),
                    (4., 6.),
                ],
                true,
                true,
            );
            path(&[(8., 12.), (11., 15.), (16., 9.)], false, false);
        }
        Icon::File => {
            document();
            line((8., 12.), (16., 12.));
            line((8., 16.), (14., 16.));
        }
        Icon::Image => {
            box_at(3., 4., 18., 16.);
            painter.circle_stroke(point(8., 9.), 1.8 * scale, stroke);
            path(
                &[(4., 18.), (9., 13.), (12., 16.), (16., 11.), (20., 16.)],
                false,
                false,
            );
        }
        Icon::Code => {
            path(&[(8., 6.), (2., 12.), (8., 18.)], false, false);
            path(&[(16., 6.), (22., 12.), (16., 18.)], false, false);
            line((14., 4.), (10., 20.));
        }
        Icon::Search => {
            painter.circle_stroke(point(10., 10.), 6.5 * scale, stroke);
            line((15., 15.), (22., 22.));
        }
        Icon::Plugin => {
            box_at(6., 8., 12., 9.);
            line((9., 3.), (9., 8.));
            line((15., 3.), (15., 8.));
            path(&[(12., 17.), (12., 21.), (17., 21.)], false, false);
            line((9., 12.), (15., 12.));
        }
    }
}

pub fn small(ui: &mut egui::Ui, icon: Icon) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
    paint(ui, rect, icon);
}

pub fn button(ui: &mut egui::Ui, icon: Icon, tooltip: &str, enabled: bool) -> egui::Response {
    ui.add_enabled_ui(enabled, |ui| {
        let response = ui.add(egui::Button::new("").min_size(Vec2::splat(28.0)));
        paint(
            ui,
            Rect::from_center_size(response.rect.center(), Vec2::splat(20.0)),
            icon,
        );
        response.on_hover_text(tooltip)
    })
    .inner
}
