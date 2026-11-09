use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use rdx::call_graph::{CallGraph, Component, Exposure, Node, owner};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// Manifest exported state per Java class name, as built by the engine for the open project.
pub type ExposureMap = HashMap<String, Exposure>;

/// Layout constants in unscaled units; the zoom factor scales them to pixels.
const CARD: Vec2 = Vec2::new(230.0, 140.0);
const COLUMN: f32 = 280.0;
const ROW: f32 = 155.0;
const RADIUS: f32 = 52.0;
const SPHERE_DROP: f32 = 60.0;

pub struct CallGraphWindow {
    pub visible: bool,
    pub graph: Option<CallGraph>,
    pub exposure: Option<ExposureMap>,
    pub status: String,
    pub origin: Option<(String, usize, String)>,
    pub cancel: Option<Arc<AtomicBool>>,
    pub depth: usize,
    zoom: f32,
    /// Displacement of each node from its column/row slot, in unscaled units.
    offsets: Vec<Vec2>,
}
impl Default for CallGraphWindow {
    fn default() -> Self {
        Self {
            visible: false,
            graph: None,
            exposure: None,
            status: String::new(),
            origin: None,
            cancel: None,
            depth: 20,
            zoom: 0.8,
            offsets: Vec::new(),
        }
    }
}

/// How a component class can be reached from outside the app, according to the manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reach {
    Exported,
    Internal,
    Unresolved,
    Undeclared,
}
impl Reach {
    const ALL: [Self; 4] = [
        Self::Exported,
        Self::Internal,
        Self::Unresolved,
        Self::Undeclared,
    ];
    /// Bits propagate along reverse edges; the lowest set bit wins when tinting an edge.
    fn bit(self) -> u8 {
        match self {
            Self::Exported => 1,
            Self::Unresolved => 2,
            Self::Internal => 4,
            Self::Undeclared => 8,
        }
    }
    fn from_mask(mask: u8) -> Option<Self> {
        [
            Self::Exported,
            Self::Unresolved,
            Self::Internal,
            Self::Undeclared,
        ]
        .into_iter()
        .find(|reach| mask & reach.bit() != 0)
    }
    fn label(self) -> &'static str {
        match self {
            Self::Exported => "Exported",
            Self::Internal => "Not exported",
            Self::Unresolved => "Unresolved",
            Self::Undeclared => "Undeclared",
        }
    }
    fn description(self) -> &'static str {
        match self {
            Self::Exported => {
                "The manifest exports this component or a declared subclass, so other apps can reach it."
            }
            Self::Internal => "Every manifest declaration of this class denies external access.",
            Self::Unresolved => {
                "The manifest leaves the exported flag unresolved, or no manifest is available."
            }
            Self::Undeclared => {
                "No manifest declaration names this class or a loaded subclass; the system never starts it."
            }
        }
    }
    fn color(self, visuals: &egui::Visuals) -> Color32 {
        match (self, visuals.dark_mode) {
            (Self::Exported, true) => Color32::from_rgb(255, 120, 120),
            (Self::Exported, false) => Color32::from_rgb(190, 30, 30),
            (Self::Internal, true) => Color32::from_rgb(110, 200, 255),
            (Self::Internal, false) => Color32::from_rgb(0, 95, 160),
            (Self::Unresolved, true) => Color32::from_rgb(255, 195, 100),
            (Self::Unresolved, false) => Color32::from_rgb(150, 85, 0),
            (Self::Undeclared, _) => visuals.weak_text_color(),
        }
    }
}
/// Reach of a component node with the manifest lines behind it; plain methods have none.
fn reach(exposure: Option<&ExposureMap>, node: &Node) -> Option<(Reach, String)> {
    node.component?;
    let Some(map) = exposure else {
        return Some((
            Reach::Unresolved,
            "AndroidManifest.xml unavailable; exported state unknown".into(),
        ));
    };
    Some(match map.get(owner(&node.method).unwrap_or_default()) {
        Some(entry) => (
            match entry.exported {
                Some(true) => Reach::Exported,
                Some(false) => Reach::Internal,
                None => Reach::Unresolved,
            },
            entry.details.join("\n"),
        ),
        None => (
            Reach::Undeclared,
            "Not declared in AndroidManifest.xml; no external entry point".into(),
        ),
    })
}
fn abbreviate(text: &str, limit: usize) -> String {
    let mut chars = text.chars();
    let mut label: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        label.push('…');
    }
    label
}
fn sphere(painter: &egui::Painter, center: Pos2, radius: f32, tint: Color32, dark: bool) {
    if radius < 8.0 {
        painter.circle_filled(center, radius, tint);
        return;
    }
    painter.circle_filled(
        center + Vec2::new(radius * 0.08, radius * 0.12),
        radius,
        Color32::from_black_alpha(35),
    );
    let base = if dark {
        Color32::from_rgb(35, 43, 55)
    } else {
        Color32::from_rgb(218, 227, 238)
    };
    let mix = |a: Color32, b: Color32, t: f32| {
        Color32::from_rgb(
            (f32::from(a.r()) * (1.0 - t) + f32::from(b.r()) * t) as u8,
            (f32::from(a.g()) * (1.0 - t) + f32::from(b.g()) * t) as u8,
            (f32::from(a.b()) * (1.0 - t) + f32::from(b.b()) * t) as u8,
        )
    };
    let base = mix(base, tint, if dark { 0.22 } else { 0.12 });
    painter.circle_filled(center, radius, base);
    for layer in 1..=20 {
        let t = layer as f32 / 20.0;
        painter.circle_filled(
            center + Vec2::new(-0.2, -0.25) * radius * t,
            radius * (1.0 - t * 0.65),
            mix(base, Color32::WHITE, t * if dark { 0.22 } else { 0.62 }),
        );
    }
    painter.circle_stroke(center, radius, Stroke::new(1.5_f32, tint));
}
/// Filled triangle whose tip sits at `tip`, pointing along `direction`.
fn arrowhead(painter: &egui::Painter, tip: Pos2, direction: Vec2, length: f32, tint: Color32) {
    let back = tip - direction * length;
    let side = direction.rot90() * length * 0.45;
    painter.add(egui::Shape::convex_polygon(
        vec![tip, back + side, back - side],
        tint,
        Stroke::NONE,
    ));
}
/// Rim-to-rim edge with an arrowhead at the callee. Leftward and recursive edges are routed
/// above the cards so they do not cross the columns between them.
fn draw_edge(
    painter: &egui::Painter,
    [start, end]: [Pos2; 2],
    radius: f32,
    top: f32,
    highlighted: bool,
    tint: Color32,
    zoom: f32,
) {
    let direction = if start == end {
        Vec2::X
    } else {
        (end - start).normalized()
    };
    let a = start + direction * radius;
    let b = end - direction * radius;
    let stroke = Stroke::new(if highlighted { 2.5 } else { 0.9 }, tint);
    let head = (16.0 * zoom).clamp(7.0, 18.0) * if highlighted { 1.15 } else { 1.0 };
    if start == end || b.x <= a.x {
        let top = top - 12.0;
        let corner = Pos2::new(b.x - 12.0, top);
        painter.line(vec![a, Pos2::new(a.x + 12.0, top), corner, b], stroke);
        arrowhead(painter, b, (b - corner).normalized(), head, tint);
    } else {
        painter.line_segment([a, b], stroke);
        arrowhead(painter, b, direction, head, tint);
    }
}
impl CallGraphWindow {
    pub fn viewport_id() -> egui::ViewportId {
        egui::ViewportId::from_hash_of("rdx_call_graph")
    }
    /// Replaces the graph and its manifest context, dropping any manual node placement.
    pub fn set_graph(&mut self, graph: Option<CallGraph>, exposure: Option<ExposureMap>) {
        self.offsets = vec![Vec2::ZERO; graph.as_ref().map_or(0, |g| g.nodes.len())];
        self.graph = graph;
        self.exposure = exposure;
    }
    pub fn show(&mut self, ctx: &egui::Context) -> (Option<String>, bool) {
        if !self.visible {
            if let Some(cancel) = &self.cancel {
                cancel.store(true, Ordering::Relaxed);
            }
            return (None, false);
        }
        ctx.show_viewport_immediate(
            Self::viewport_id(),
            egui::ViewportBuilder::default()
                .with_title("RDX — Method call graph")
                .with_inner_size([1100.0, 750.0])
                .with_min_inner_size([650.0, 400.0]),
            |ctx, _| self.show_contents(ctx),
        )
    }
    fn show_contents(&mut self, ctx: &egui::Context) -> (Option<String>, bool) {
        if ctx.input(|input| input.viewport().close_requested()) {
            self.visible = false;
            if let Some(cancel) = &self.cancel {
                cancel.store(true, Ordering::Relaxed);
            }
            ctx.request_repaint_of(egui::ViewportId::ROOT);
            return (None, false);
        }
        let mut selected = None;
        let mut rebuild = false;
        let mut fit = false;
        let Self {
            graph,
            exposure,
            offsets,
            zoom,
            depth,
            cancel,
            status,
            ..
        } = self;
        if let Some(graph) = graph
            && offsets.len() != graph.nodes.len()
        {
            *offsets = vec![Vec2::ZERO; graph.nodes.len()];
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(depth).range(1..=100).prefix("Depth: "));
                ui.add(
                    egui::Slider::new(zoom, 0.01..=2.0)
                        .logarithmic(true)
                        .text("Zoom"),
                );
                fit = ui
                    .add_enabled(graph.is_some(), egui::Button::new("Fit graph"))
                    .clicked();
                let moved = offsets.iter().any(|offset| *offset != Vec2::ZERO);
                if ui
                    .add_enabled(moved, egui::Button::new("Reset layout"))
                    .on_hover_text("Return every node to its depth column")
                    .clicked()
                {
                    offsets.fill(Vec2::ZERO);
                }
                if ui
                    .add_enabled(cancel.is_none(), egui::Button::new("Rebuild"))
                    .clicked()
                {
                    rebuild = true;
                }
                if let Some(cancel) = cancel {
                    ui.spinner();
                    if ui.button("Cancel").clicked() {
                        cancel.store(true, Ordering::Relaxed);
                    }
                }
            });
            ui.label("Outgoing static calls · virtual/interface calls use declared targets · Intent destinations are not inferred.")
                .on_hover_text("Component colors follow the manifest exported state of the class or of a declared subclass: red components accept intents from other apps, blue ones are declared without external access. Exported state is a manifest property; it does not prove that this method runs for an external intent. Starting a service does not establish execution of an app callback; framework and lifecycle dispatch are not inferred. Constructor/provider callers do not establish active route registration or runtime intent delivery.");
            ui.horizontal(|ui| {
                ui.label("Components:");
                for reach in Reach::ALL {
                    ui.colored_label(reach.color(ui.visuals()), reach.label())
                        .on_hover_text(reach.description());
                }
            });
            ui.label(status.as_str());
            let Some(graph) = graph else { return };
            if graph.truncated {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "Partial graph: reached 5,000 methods / 20,000 edges. All collected calls are displayed.",
                );
            }
            if !graph.dispatch_targets.is_empty() || graph.dispatch_targets_truncated {
                ui.collapsing("Related interface / superclass declarations", |ui| {
                    ui.label("Calls through these declarations may dispatch to this override. They are not proven runtime callers.");
                    for target in &graph.dispatch_targets {
                        if ui.link(target).clicked() {
                            selected = Some(target.clone());
                        }
                    }
                    if graph.dispatch_targets_truncated {
                        ui.label("Ancestor traversal limit reached; list is partial.");
                    }
                });
            }
            let exposure = exposure.as_ref();
            let reaches: Vec<_> = graph.nodes.iter().map(|n| reach(exposure, n)).collect();
            let masks = graph.reachability(|n| reach(exposure, n).map_or(0, |(r, _)| r.bit()));
            ui.label("All discovered calls shown · highlighted edges lead to components · drag a node to move it · drag the canvas to pan");
            let columns = graph.nodes.iter().map(|n| n.depth).max().unwrap_or(0) + 1;
            let mut rows = vec![0usize; columns];
            let slots: Vec<Vec2> = graph
                .nodes
                .iter()
                .map(|node| {
                    let row = rows[node.depth];
                    rows[node.depth] += 1;
                    Vec2::new(node.depth as f32 * COLUMN, row as f32 * ROW)
                })
                .collect();
            let positions: Vec<Vec2> = slots
                .iter()
                .zip(offsets.iter())
                .map(|(slot, offset)| *slot + *offset)
                .collect();
            let extent = positions
                .iter()
                .fold(Vec2::new(COLUMN, ROW), |extent, p| extent.max(*p + CARD));
            if fit {
                let available = ui.available_size();
                *zoom = (available.x / extent.x)
                    .min(available.y / extent.y)
                    .clamp(0.01, 2.0);
            }
            egui::ScrollArea::both().show(ui, |ui| {
                let z = *zoom;
                let dark = ui.visuals().dark_mode;
                let (area, _) = ui.allocate_exact_size(extent * z, Sense::hover());
                let rects: Vec<Rect> = positions
                    .iter()
                    .map(|p| Rect::from_min_size(area.min + *p * z, CARD * z))
                    .collect();
                let centers: Vec<Pos2> = rects
                    .iter()
                    .map(|r| r.center_top() + Vec2::new(0.0, SPHERE_DROP) * z)
                    .collect();
                let plain = graph.edges.iter().filter(|e| masks[e.to] == 0);
                let highlighted = graph.edges.iter().filter(|e| masks[e.to] != 0);
                for edge in plain.chain(highlighted) {
                    let tint = Reach::from_mask(masks[edge.to])
                        .map_or(ui.visuals().weak_text_color(), |r| r.color(ui.visuals()));
                    draw_edge(
                        ui.painter(),
                        [centers[edge.from], centers[edge.to]],
                        RADIUS * z,
                        rects[edge.from].top().min(rects[edge.to].top()),
                        masks[edge.to] != 0,
                        tint,
                        z,
                    );
                }
                for (i, node) in graph.nodes.iter().enumerate() {
                    let rect = rects[i];
                    if !ui.clip_rect().intersects(rect) {
                        continue;
                    }
                    let center = centers[i];
                    let tint = reaches[i]
                        .as_ref()
                        .map_or(ui.visuals().text_color(), |(r, _)| r.color(ui.visuals()));
                    let rim = if node.component.is_some() {
                        tint
                    } else if masks[i] != 0 {
                        ui.visuals().selection.stroke.color
                    } else {
                        ui.visuals().weak_text_color()
                    };
                    sphere(ui.painter(), center, RADIUS * z, rim, dark);
                    let before = node.method.split('(').next().unwrap_or(&node.method);
                    let (owner, method) = before.rsplit_once('.').unwrap_or(("", before));
                    let short_owner = owner.rsplit('.').next().unwrap_or(owner);
                    ui.painter().text(
                        center - Vec2::new(0.0, 6.0) * z,
                        egui::Align2::CENTER_CENTER,
                        abbreviate(method, 16),
                        egui::FontId::proportional(12.0 * z),
                        ui.visuals().text_color(),
                    );
                    let label = node.component.map(Component::label).unwrap_or(if node.available {
                        "Method"
                    } else {
                        "External"
                    });
                    ui.painter().text(
                        center + Vec2::new(0.0, 14.0) * z,
                        egui::Align2::CENTER_CENTER,
                        label,
                        egui::FontId::proportional(10.0 * z),
                        ui.visuals().text_color(),
                    );
                    ui.painter().text(
                        center + Vec2::new(0.0, 66.0) * z,
                        egui::Align2::CENTER_CENTER,
                        abbreviate(short_owner, 32),
                        egui::FontId::proportional(11.0 * z),
                        tint,
                    );
                    let mut hover = node.method.clone();
                    if let Some((reach, detail)) = &reaches[i] {
                        hover.push_str(&format!("\n{}\nManifest: {detail}", reach.label()));
                    }
                    let response = ui
                        .interact(rect, ui.id().with(i), Sense::click_and_drag())
                        .on_hover_cursor(egui::CursorIcon::Grab)
                        .on_hover_text(hover);
                    if response.dragged() {
                        let moved = positions[i] + response.drag_delta() / z;
                        offsets[i] = moved.max(Vec2::ZERO) - slots[i];
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                    }
                    if response.clicked() && node.available {
                        selected = Some(node.method.clone());
                    }
                }
            });
        });
        if !self.visible
            && let Some(cancel) = &self.cancel
        {
            cancel.store(true, Ordering::Relaxed);
        }
        if selected.is_some() || rebuild {
            ctx.request_repaint_of(egui::ViewportId::ROOT);
        }
        if selected.is_some() {
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
        }
        (selected, rebuild)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdx::call_graph::{Edge, Node};
    fn sample_graph() -> CallGraph {
        CallGraph {
            nodes: vec![
                Node {
                    method: "C.run()V".into(),
                    depth: 0,
                    component: None,
                    available: true,
                },
                Node {
                    method: "app.Main.run()V".into(),
                    depth: 20,
                    component: Some(Component::Activity),
                    available: true,
                },
            ],
            edges: vec![
                Edge {
                    from: 0,
                    to: 1,
                    sites: 2,
                },
                Edge {
                    from: 1,
                    to: 1,
                    sites: 1,
                },
            ],
            truncated: false,
            dispatch_targets: vec![],
            dispatch_targets_truncated: false,
        }
    }
    fn arrowheads(output: &egui::FullOutput) -> usize {
        output
            .shapes
            .iter()
            .filter(|clipped| {
                matches!(&clipped.shape, egui::Shape::Path(path) if path.closed && path.points.len() == 3)
            })
            .count()
    }
    #[test]
    fn native_viewport_is_separate_and_close_cancels() {
        assert_ne!(CallGraphWindow::viewport_id(), egui::ViewportId::ROOT);
        assert_ne!(
            CallGraphWindow::viewport_id(),
            crate::usages_window::UsagesWindow::viewport_id()
        );
        let ctx = egui::Context::default();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut window = CallGraphWindow {
            visible: true,
            cancel: Some(cancel.clone()),
            ..Default::default()
        };
        let mut input = egui::RawInput::default();
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .events
            .push(egui::ViewportEvent::Close);
        let _ = ctx.run(input, |ctx| {
            assert_eq!(window.show_contents(ctx), (None, false));
        });
        assert!(!window.visible);
        assert!(cancel.load(Ordering::Relaxed));
    }
    #[test]
    fn graph_renders_in_both_themes_and_closing_cancels_work() {
        assert_eq!(CallGraphWindow::default().depth, 20);
        for dark in [false, true] {
            let ctx = egui::Context::default();
            ctx.set_visuals(if dark {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
            let mut window = CallGraphWindow {
                visible: true,
                ..Default::default()
            };
            window.set_graph(
                Some(sample_graph()),
                Some(HashMap::from([(
                    "app.Main".to_owned(),
                    Exposure {
                        exported: Some(true),
                        details: vec!["activity: true (explicit)".into()],
                    },
                )])),
            );
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                window.show_contents(ctx);
            });
            assert!(!output.shapes.is_empty());
            assert_eq!(arrowheads(&output), 2, "one arrowhead per edge");
            let cancel = Arc::new(AtomicBool::new(false));
            window.cancel = Some(cancel.clone());
            window.visible = false;
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                window.show(ctx);
            });
            assert!(cancel.load(Ordering::Relaxed));
        }
    }
    #[test]
    fn exposure_maps_manifest_state_to_colors_and_edge_tints() {
        let node = |class: &str| Node {
            method: format!("{class}.run()V"),
            depth: 0,
            component: Some(Component::Activity),
            available: true,
        };
        let map = HashMap::from([
            (
                "app.Open".to_owned(),
                Exposure {
                    exported: Some(true),
                    details: vec![],
                },
            ),
            (
                "app.Closed".to_owned(),
                Exposure {
                    exported: Some(false),
                    details: vec![],
                },
            ),
            (
                "app.Maybe".to_owned(),
                Exposure {
                    exported: None,
                    details: vec![],
                },
            ),
        ]);
        let reach_of = |class: &str| reach(Some(&map), &node(class)).unwrap().0;
        assert_eq!(reach_of("app.Open"), Reach::Exported);
        assert_eq!(reach_of("app.Closed"), Reach::Internal);
        assert_eq!(reach_of("app.Maybe"), Reach::Unresolved);
        assert_eq!(reach_of("android.app.Activity"), Reach::Undeclared);
        assert_eq!(reach(None, &node("app.Open")).unwrap().0, Reach::Unresolved);
        let plain = Node {
            component: None,
            ..node("app.Helper")
        };
        assert_eq!(reach(Some(&map), &plain), None);
        // An edge leading to both an exported and an internal component reads as exported.
        let mask = Reach::Exported.bit() | Reach::Internal.bit() | Reach::Undeclared.bit();
        assert_eq!(Reach::from_mask(mask), Some(Reach::Exported));
        assert_eq!(
            Reach::from_mask(Reach::Internal.bit() | Reach::Unresolved.bit()),
            Some(Reach::Unresolved)
        );
        assert_eq!(Reach::from_mask(0), None);
        let light = egui::Visuals::light();
        let dark = egui::Visuals::dark();
        for reach in Reach::ALL {
            assert_ne!(reach.color(&light), reach.color(&dark));
        }
        assert_ne!(Reach::Exported.color(&light), Reach::Internal.color(&light));
    }
    #[test]
    fn nodes_follow_pointer_drags_and_reset_with_a_new_graph() {
        let ctx = egui::Context::default();
        let mut window = CallGraphWindow {
            visible: true,
            ..Default::default()
        };
        window.set_graph(Some(sample_graph()), None);
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            window.show_contents(ctx);
        });
        // The leftmost large circle belongs to the root node's sphere.
        let root = output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Circle(circle) if circle.radius > 40.0 => Some(circle.center),
                _ => None,
            })
            .min_by(|a, b| a.x.total_cmp(&b.x))
            .expect("root sphere");
        let frame = |events: Vec<egui::Event>| egui::RawInput {
            events,
            ..Default::default()
        };
        let press = egui::Event::PointerButton {
            pos: root,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        };
        let _ = ctx.run(frame(vec![egui::Event::PointerMoved(root), press]), |ctx| {
            window.show_contents(ctx);
        });
        for step in 1..=3 {
            let pos = root + Vec2::new(40.0, 25.0) * step as f32;
            let _ = ctx.run(frame(vec![egui::Event::PointerMoved(pos)]), |ctx| {
                window.show_contents(ctx);
            });
        }
        let release = egui::Event::PointerButton {
            pos: root + Vec2::new(120.0, 75.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        };
        let (selected, _) = {
            let mut result = (None, false);
            let _ = ctx.run(frame(vec![release]), |ctx| {
                result = window.show_contents(ctx);
            });
            result
        };
        assert_eq!(selected, None, "a drag is not a click");
        let moved = window.offsets[0];
        assert!(moved.x > 0.0 && moved.y > 0.0, "root node moved: {moved:?}");
        assert_eq!(window.offsets[1], Vec2::ZERO);
        window.set_graph(Some(sample_graph()), None);
        assert!(window.offsets.iter().all(|offset| *offset == Vec2::ZERO));
        window.set_graph(None, None);
        assert!(window.offsets.is_empty());
    }
}
