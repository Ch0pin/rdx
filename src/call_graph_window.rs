use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use rdx::call_graph::{CallGraph, Component, Direction, Edge, Exposure, Node, owner};
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
/// Space above the top row for leftward arcs and recursion loops. The curve bounds below
/// keep every route inside it, so no edge leaves the canvas.
const LANE: f32 = 70.0;
/// Largest control offset of a leftward arc; its apex rises three quarters of this.
const ARC_RISE: f32 = 80.0;
/// Largest control offset of a same-column bow; it stays within the card width.
const SIDE_BOW: f32 = 56.0;
/// Dash length of an inferred edge in unscaled units; the gap is six tenths of it.
const DASH: f32 = 9.0;
const LAMBDA_CREATORS_HINT: &str = "Callers graphs only. A method of a synthetic, anonymous or Lambda-named callback class, or of a class extending a Kotlin lambda or continuation base, gains an edge from every method that allocates that class, and the walk continues from those creators. A Runnable handed to a thread, executor or handler shows the method that built it this way. Dashed edges mark these links: the allocation is exact, the callback relationship is inferred. Invoke-custom lambdas, reflection and framework delivery stay out.";

pub struct CallGraphWindow {
    pub visible: bool,
    pub graph: Option<CallGraph>,
    pub exposure: Option<ExposureMap>,
    pub status: String,
    pub origin: Option<(String, usize, String)>,
    pub cancel: Option<Arc<AtomicBool>>,
    pub depth: usize,
    /// Direction requested for the next build; a change rebuilds the graph at once.
    pub direction: Direction,
    /// Follow callback methods back to the methods allocating their class in the next
    /// Callers build; a change rebuilds the graph at once.
    pub lambda_creators: bool,
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
            direction: Direction::Callers,
            lambda_creators: false,
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
/// Rim-to-rim edge with an arrowhead at the callee. A rightward edge is a straight line.
/// An edge within one column bows out beside the cards, a leftward edge arcs over the
/// cards it spans, and a recursive call loops above its own sphere. Arcs and loops rise
/// less than `LANE` above the card, the space the layout reserves over the top row.
/// A dashed edge follows the same route; it marks an inferred link.
fn draw_edge(
    painter: &egui::Painter,
    [start, end]: [Pos2; 2],
    radius: f32,
    highlighted: bool,
    dashed: bool,
    tint: Color32,
    zoom: f32,
) {
    let width = if highlighted { 2.5_f32 } else { 0.9 };
    let stroke = Stroke::new(width, tint);
    let head = (16.0 * zoom).clamp(7.0, 18.0) * if highlighted { 1.15 } else { 1.0 };
    let dash = (DASH * zoom).clamp(4.0, 10.0);
    let dashes = |points: &[Pos2]| {
        painter.extend(egui::Shape::dashed_line(points, stroke, dash, dash * 0.6));
    };
    let curve = |points: [Pos2; 4]| {
        let shape = egui::epaint::CubicBezierShape::from_points_stroke(
            points,
            false,
            Color32::TRANSPARENT,
            stroke,
        );
        if dashed {
            dashes(&shape.flatten(Some(0.25)));
        } else {
            painter.add(shape);
        }
        let into = (points[3] - points[2]).normalized();
        arrowhead(painter, points[3], into, head, tint);
    };
    let delta = end - start;
    if start == end {
        let a = start + Vec2::new(-0.5, -0.866) * radius;
        let b = start + Vec2::new(0.5, -0.866) * radius;
        curve([
            a,
            a + Vec2::new(-0.9, -1.3) * radius,
            b + Vec2::new(0.9, -1.3) * radius,
            b,
        ]);
    } else if delta.x.abs() < radius {
        // Downward edges bow left and upward edges right, so a mutual pair stays apart.
        let side = if delta.y > 0.0 { -1.0 } else { 1.0 };
        let reach = side * (delta.y.abs() * 0.25).clamp(0.5 * radius, SIDE_BOW * zoom);
        let a = start + Vec2::new(side * radius, 0.0);
        let b = end + Vec2::new(side * radius, 0.0);
        curve([a, a + Vec2::new(reach, 0.0), b + Vec2::new(reach, 0.0), b]);
    } else if delta.x < 0.0 {
        let rise = (delta.x.abs() * 0.35).clamp(0.8 * radius, ARC_RISE * zoom);
        let a = start - Vec2::new(0.0, radius);
        let b = end - Vec2::new(0.0, radius);
        curve([a, a - Vec2::new(0.0, rise), b - Vec2::new(0.0, rise), b]);
    } else {
        let direction = delta.normalized();
        let a = start + direction * radius;
        let b = end - direction * radius;
        if dashed {
            dashes(&[a, b]);
        } else {
            painter.line_segment([a, b], stroke);
        }
        arrowhead(painter, b, direction, head, tint);
    }
}
impl CallGraphWindow {
    pub fn viewport_id() -> egui::ViewportId {
        egui::ViewportId::from_hash_of("rdx_call_graph")
    }
    /// Whether the next build follows lambda creators: the option applies to Callers
    /// graphs, so another direction builds declared calls alone.
    pub fn lambda_creators_requested(&self) -> bool {
        self.lambda_creators && self.direction == Direction::Callers
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
            direction,
            lambda_creators,
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
                ui.separator();
                ui.add_enabled_ui(cancel.is_none(), |ui| {
                    for option in Direction::ALL {
                        if ui
                            .selectable_value(direction, option, option.label())
                            .on_hover_text(option.description())
                            .changed()
                        {
                            rebuild = true;
                        }
                    }
                });
                ui.add_enabled_ui(
                    cancel.is_none() && *direction == Direction::Callers,
                    |ui| {
                        if ui
                            .checkbox(lambda_creators, "Lambda creators")
                            .on_hover_text(LAMBDA_CREATORS_HINT)
                            .changed()
                        {
                            rebuild = true;
                        }
                    },
                );
                ui.separator();
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
            let built = graph.as_ref().map_or(*direction, |g| g.direction);
            let flow = match built {
                Direction::Callers => "Incoming",
                Direction::Callees => "Outgoing",
                Direction::Both => "Incoming and outgoing",
            };
            ui.label(format!("{flow} static calls · virtual/interface calls use declared targets · Intent destinations are not inferred."))
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
            if graph.edges.iter().any(Edge::inferred_only) {
                ui.label("Dashed edges: the caller allocates the callback class declaring the callee · the allocation is exact, the callback relationship is inferred")
                    .on_hover_text(LAMBDA_CREATORS_HINT);
            }
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
            // Callee graphs highlight the paths into components, caller graphs the paths out
            // of them towards the selected method; a bidirectional graph shows both.
            let seed = |n: &Node| reach(exposure, n).map_or(0, |(r, _)| r.bit());
            let zeros = || vec![0u8; graph.nodes.len()];
            let leads_to = if graph.direction == Direction::Callers {
                zeros()
            } else {
                graph.reachability(seed)
            };
            let comes_from = if graph.direction == Direction::Callees {
                zeros()
            } else {
                graph.reachable_from(seed)
            };
            let edge_mask = |edge: &Edge| leads_to[edge.to] | comes_from[edge.from];
            let node_mask: Vec<u8> = leads_to
                .iter()
                .zip(&comes_from)
                .map(|(to, from)| to | from)
                .collect();
            let highlight = match graph.direction {
                Direction::Callers => "highlighted edges come from components",
                Direction::Callees => "highlighted edges lead to components",
                Direction::Both => "highlighted edges connect components",
            };
            ui.label(format!("All discovered calls shown · {highlight} · drag a node to move it · drag the canvas to pan"));
            // Callers sit left of the root and callees right of it, so every arrow reads
            // left to right.
            let columns = graph.columns();
            let first = columns.iter().copied().min().unwrap_or(0);
            let span = columns.iter().copied().max().unwrap_or(0) - first + 1;
            let mut rows = vec![0usize; span as usize];
            let slots: Vec<Vec2> = columns
                .iter()
                .map(|&column| {
                    let column = (column - first) as usize;
                    let row = rows[column];
                    rows[column] += 1;
                    Vec2::new(column as f32 * COLUMN, LANE + row as f32 * ROW)
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
                let plain = graph.edges.iter().filter(|e| edge_mask(e) == 0);
                let highlighted = graph.edges.iter().filter(|e| edge_mask(e) != 0);
                for edge in plain.chain(highlighted) {
                    let mask = edge_mask(edge);
                    let tint = Reach::from_mask(mask)
                        .map_or(ui.visuals().weak_text_color(), |r| r.color(ui.visuals()));
                    draw_edge(
                        ui.painter(),
                        [centers[edge.from], centers[edge.to]],
                        RADIUS * z,
                        mask != 0,
                        edge.inferred_only(),
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
                    } else if let Some(reach) = Reach::from_mask(node_mask[i]) {
                        reach.color(ui.visuals())
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
                    } else {
                        if let Some(reach) = Reach::from_mask(leads_to[i]) {
                            hover.push_str(&format!("\nLeads to a component: {}", reach.label()));
                        }
                        if let Some(reach) = Reach::from_mask(comes_from[i]) {
                            hover.push_str(&format!(
                                "\nReached from a component: {}",
                                reach.label()
                            ));
                        }
                    }
                    let response = ui
                        .interact(rect, ui.id().with(i), Sense::click_and_drag())
                        .on_hover_cursor(egui::CursorIcon::Grab)
                        .on_hover_text(hover);
                    if response.dragged() {
                        let moved = positions[i] + response.drag_delta() / z;
                        offsets[i] = moved.max(Vec2::new(0.0, LANE)) - slots[i];
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
                    inferred: 0,
                },
                Edge {
                    from: 1,
                    to: 1,
                    sites: 1,
                    inferred: 0,
                },
            ],
            truncated: false,
            dispatch_targets: vec![],
            dispatch_targets_truncated: false,
            direction: Direction::Callees,
        }
    }
    fn exported_main() -> ExposureMap {
        HashMap::from([(
            "app.Main".to_owned(),
            Exposure {
                exported: Some(true),
                details: vec!["activity: true (explicit)".into()],
            },
        )])
    }
    /// Centers of the node spheres, left to right, taken from their rim strokes.
    fn spheres(output: &egui::FullOutput) -> Vec<Pos2> {
        let mut centers: Vec<Pos2> = output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Circle(circle)
                    if circle.radius > 40.0 && circle.stroke.width > 0.0 =>
                {
                    Some(circle.center)
                }
                _ => None,
            })
            .collect();
        centers.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
        centers
    }
    /// Screen rectangle of the first rendered text equal to `text`.
    fn text_rect(output: &egui::FullOutput, text: &str) -> Rect {
        output
            .shapes
            .iter()
            .find_map(|clipped| match &clipped.shape {
                egui::Shape::Text(shape) if shape.galley.text() == text => {
                    Some(Rect::from_min_size(shape.pos, shape.galley.size()))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("text {text:?} rendered"))
    }
    fn click(
        ctx: &egui::Context,
        window: &mut CallGraphWindow,
        pos: Pos2,
    ) -> (Option<String>, bool) {
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let frame = |events| egui::RawInput {
            events,
            ..Default::default()
        };
        let _ = ctx.run(
            frame(vec![egui::Event::PointerMoved(pos), button(true)]),
            |ctx| {
                window.show_contents(ctx);
            },
        );
        let mut result = (None, false);
        let _ = ctx.run(frame(vec![button(false)]), |ctx| {
            result = window.show_contents(ctx);
        });
        result
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
            window.set_graph(Some(sample_graph()), Some(exported_main()));
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                window.show_contents(ctx);
            });
            assert!(!output.shapes.is_empty());
            assert_eq!(arrowheads(&output), 2, "one arrowhead per edge");
            assert!(output.shapes.iter().any(|clipped| matches!(
                &clipped.shape,
                egui::Shape::Text(shape) if shape.galley.text().starts_with("Outgoing static calls")
            )));
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
    fn callers_flow_into_the_root_from_the_left() {
        assert_eq!(CallGraphWindow::default().direction, Direction::Callers);
        let ctx = egui::Context::default();
        let mut window = CallGraphWindow {
            visible: true,
            ..Default::default()
        };
        let mut graph = sample_graph();
        graph.direction = Direction::Callers;
        graph.nodes[1].depth = 1;
        graph.edges = vec![Edge {
            from: 1,
            to: 0,
            sites: 3,
            inferred: 0,
        }];
        window.set_graph(Some(graph), Some(exported_main()));
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            window.show_contents(ctx);
        });
        let centers = spheres(&output);
        assert_eq!(centers.len(), 2, "{centers:?}");
        let (caller, root) = (centers[0], centers[1]);
        assert!(caller.x < root.x, "the caller sits left of the root");
        let tips: Vec<Pos2> = output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Path(path) if path.closed && path.points.len() == 3 => {
                    Some(path.points[0])
                }
                _ => None,
            })
            .collect();
        assert_eq!(tips.len(), 1);
        assert!(
            tips[0].x > (caller.x + root.x) / 2.0 && tips[0].x < root.x,
            "the arrowhead points into the root: {tips:?}"
        );
        // The exported Activity reaches the root, so both rims carry the exported color.
        let red = Reach::Exported.color(&ctx.style().visuals);
        let rims = output
            .shapes
            .iter()
            .filter(|clipped| matches!(
                &clipped.shape,
                egui::Shape::Circle(circle) if circle.radius > 40.0 && circle.stroke.color == red
            ))
            .count();
        assert_eq!(rims, 2);
        assert!(output.shapes.iter().any(|clipped| matches!(
            &clipped.shape,
            egui::Shape::Text(shape) if shape.galley.text().starts_with("Incoming static calls")
        )));
        // Choosing another direction asks for a rebuild, except while a build is running.
        let (selected, rebuild) = click(&ctx, &mut window, text_rect(&output, "Callees").center());
        assert_eq!((selected, rebuild), (None, true));
        assert_eq!(window.direction, Direction::Callees);
        window.cancel = Some(Arc::new(AtomicBool::new(false)));
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            window.show_contents(ctx);
        });
        let (_, rebuild) = click(&ctx, &mut window, text_rect(&output, "Both").center());
        assert!(!rebuild);
        assert_eq!(window.direction, Direction::Callees);
    }
    #[test]
    fn every_edge_route_stays_inside_the_canvas() {
        let node = |method: &str| Node {
            method: method.into(),
            depth: usize::from(method != "app.Root.run()V"),
            component: None,
            available: true,
        };
        let edge = |from, to| Edge {
            from,
            to,
            sites: 1,
            inferred: 0,
        };
        // Root in column 0; Up, Peer and Third call it from rows 0..2 of column -1; Down
        // is its callee in column +1 and also calls it back and itself.
        let graph = CallGraph {
            nodes: vec![
                node("app.Root.run()V"),
                node("app.Up.run()V"),
                node("app.Peer.run()V"),
                node("app.Third.run()V"),
                node("app.Down.run()V"),
            ],
            edges: vec![
                edge(1, 0),
                edge(2, 0),
                edge(3, 0),
                edge(0, 4),
                edge(1, 3),
                edge(3, 1),
                edge(4, 0),
                edge(4, 4),
            ],
            truncated: false,
            dispatch_targets: vec![],
            dispatch_targets_truncated: false,
            direction: Direction::Both,
        };
        let ctx = egui::Context::default();
        let mut window = CallGraphWindow {
            visible: true,
            ..Default::default()
        };
        window.set_graph(Some(graph), None);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1100.0, 750.0))),
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| {
            window.show_contents(ctx);
        });
        assert_eq!(spheres(&output).len(), 5);
        let canvas = output
            .shapes
            .iter()
            .find_map(|clipped| match &clipped.shape {
                egui::Shape::Circle(circle) if circle.radius > 40.0 => Some(clipped.clip_rect),
                _ => None,
            })
            .expect("sphere clip rect");
        let (mut lines, mut heads, mut curves) = (0, 0, Vec::new());
        for clipped in output.shapes.iter().filter(|c| c.clip_rect == canvas) {
            let points: Vec<Pos2> = match &clipped.shape {
                egui::Shape::LineSegment { points, .. } => {
                    lines += 1;
                    points.to_vec()
                }
                egui::Shape::CubicBezier(curve) => {
                    curves.push(curve.points);
                    (0..=16).map(|i| curve.sample(i as f32 / 16.0)).collect()
                }
                egui::Shape::Path(path) => {
                    heads += usize::from(path.closed && path.points.len() == 3);
                    path.points.clone()
                }
                _ => continue,
            };
            for point in points {
                assert!(
                    canvas.contains(point),
                    "{point:?} is drawn outside the canvas {canvas:?}"
                );
            }
        }
        assert_eq!((lines, curves.len(), heads), (4, 4, 8));
        // The mutual same-column pair bows out on opposite sides; the leftward arc and
        // the recursion loop rise above the spheres and come down into their targets.
        let (bows, arcs): (Vec<&[Pos2; 4]>, Vec<&[Pos2; 4]>) = curves
            .iter()
            .partition(|points| (points[0].x - points[3].x).abs() < 1.0);
        assert_eq!(bows.len(), 2);
        let lefts = bows
            .iter()
            .filter(|points| points[1].x < points[0].x)
            .count();
        assert_eq!(lefts, 1, "{bows:?}");
        assert_eq!(arcs.len(), 2);
        for points in arcs {
            assert!(
                points[1].y < points[0].y && points[2].y < points[3].y,
                "the arc rises over its spheres: {points:?}"
            );
        }
    }
    #[test]
    fn lambda_creators_dash_inferred_edges_and_rebuild_callers_graphs() {
        assert!(!CallGraphWindow::default().lambda_creators);
        let node = |method: &str, depth| Node {
            method: method.into(),
            depth,
            component: None,
            available: true,
        };
        // The root is a lambda's callback: app.Main.start allocates the lambda class and
        // app.Other.call invokes the callback directly.
        let graph = CallGraph {
            nodes: vec![
                node("app.Main$$ExternalSyntheticLambda0.run()V", 0),
                node("app.Main.start()V", 1),
                node("app.Other.call()V", 1),
            ],
            edges: vec![
                Edge {
                    from: 1,
                    to: 0,
                    sites: 0,
                    inferred: 1,
                },
                Edge {
                    from: 2,
                    to: 0,
                    sites: 1,
                    inferred: 0,
                },
            ],
            truncated: false,
            dispatch_targets: vec![],
            dispatch_targets_truncated: false,
            direction: Direction::Callers,
        };
        let ctx = egui::Context::default();
        let mut window = CallGraphWindow {
            visible: true,
            ..Default::default()
        };
        window.set_graph(Some(graph), None);
        let input = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1100.0, 750.0))),
            ..Default::default()
        };
        let output = ctx.run(input(), |ctx| {
            window.show_contents(ctx);
        });
        assert_eq!(spheres(&output).len(), 3);
        let canvas = output
            .shapes
            .iter()
            .find_map(|clipped| match &clipped.shape {
                egui::Shape::Circle(circle) if circle.radius > 40.0 => Some(clipped.clip_rect),
                _ => None,
            })
            .expect("sphere clip rect");
        let segments: Vec<[Pos2; 2]> = output
            .shapes
            .iter()
            .filter(|clipped| clipped.clip_rect == canvas)
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::LineSegment { points, .. } => Some(*points),
                _ => None,
            })
            .collect();
        // The declared call is one solid segment; the inferred link breaks into dashes
        // shorter than a scaled dash length, and both edges keep their arrowheads.
        let solid = segments
            .iter()
            .filter(|[a, b]| (*b - *a).length() > 100.0)
            .count();
        let dashes = segments
            .iter()
            .filter(|[a, b]| (*b - *a).length() <= DASH)
            .count();
        assert_eq!(solid, 1, "{segments:?}");
        assert!(dashes >= 8, "{segments:?}");
        assert_eq!(solid + dashes, segments.len(), "{segments:?}");
        assert_eq!(arrowheads(&output), 2);
        assert!(output.shapes.iter().any(|clipped| matches!(
            &clipped.shape,
            egui::Shape::Text(shape) if shape.galley.text().starts_with("Dashed edges")
        )));
        // The checkbox records the choice and asks for a rebuild.
        let (selected, rebuild) = click(
            &ctx,
            &mut window,
            text_rect(&output, "Lambda creators").center(),
        );
        assert_eq!((selected, rebuild), (None, true));
        assert!(window.lambda_creators);
        assert!(window.lambda_creators_requested());
        // Other directions build declared calls alone and keep the box disabled.
        window.direction = Direction::Callees;
        assert!(!window.lambda_creators_requested());
        let output = ctx.run(input(), |ctx| {
            window.show_contents(ctx);
        });
        let (_, rebuild) = click(
            &ctx,
            &mut window,
            text_rect(&output, "Lambda creators").center(),
        );
        assert!(!rebuild);
        assert!(window.lambda_creators);
        // A running build disables it as well.
        window.direction = Direction::Callers;
        window.cancel = Some(Arc::new(AtomicBool::new(false)));
        let output = ctx.run(input(), |ctx| {
            window.show_contents(ctx);
        });
        let (_, rebuild) = click(
            &ctx,
            &mut window,
            text_rect(&output, "Lambda creators").center(),
        );
        assert!(!rebuild);
        assert!(window.lambda_creators);
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
        // A drag above the lane reserved for arcs and loops stops at the lane edge.
        let center = root + Vec2::new(120.0, 75.0);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let _ = ctx.run(
            frame(vec![
                egui::Event::PointerMoved(center),
                button(center, true),
            ]),
            |ctx| {
                window.show_contents(ctx);
            },
        );
        let far = center - Vec2::new(400.0, 400.0);
        let _ = ctx.run(frame(vec![egui::Event::PointerMoved(far)]), |ctx| {
            window.show_contents(ctx);
        });
        let _ = ctx.run(frame(vec![button(far, false)]), |ctx| {
            window.show_contents(ctx);
        });
        assert_eq!(
            window.offsets[0],
            Vec2::ZERO,
            "the root stops at its slot on the lane edge"
        );
        window.set_graph(Some(sample_graph()), None);
        assert!(window.offsets.iter().all(|offset| *offset == Vec2::ZERO));
        window.set_graph(None, None);
        assert!(window.offsets.is_empty());
    }
}
