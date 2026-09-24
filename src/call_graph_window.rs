use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use rdx::call_graph::{CallGraph, Component};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub struct CallGraphWindow {
    pub visible: bool,
    pub graph: Option<CallGraph>,
    pub status: String,
    pub origin: Option<(String, usize, String)>,
    pub cancel: Option<Arc<AtomicBool>>,
    pub depth: usize,
    zoom: f32,
}
impl Default for CallGraphWindow {
    fn default() -> Self {
        Self {
            visible: false,
            graph: None,
            status: String::new(),
            origin: None,
            cancel: None,
            depth: 20,
            zoom: 0.8,
        }
    }
}
fn color(kind: Component, dark: bool) -> Color32 {
    match (kind, dark) {
        (Component::Activity, true) => Color32::from_rgb(110, 200, 255),
        (Component::Activity, false) => Color32::from_rgb(0, 95, 160),
        (Component::Service, true) => Color32::from_rgb(110, 225, 165),
        (Component::Service, false) => Color32::from_rgb(0, 110, 65),
        (Component::Receiver, true) => Color32::from_rgb(255, 195, 100),
        (Component::Receiver, false) => Color32::from_rgb(145, 80, 0),
        (Component::Provider, true) => Color32::from_rgb(215, 160, 255),
        (Component::Provider, false) => Color32::from_rgb(125, 55, 175),
    }
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
    painter.circle_stroke(center, radius, Stroke::new(1.5, tint));
}
impl CallGraphWindow {
    pub fn viewport_id() -> egui::ViewportId {
        egui::ViewportId::from_hash_of("rdx_call_graph")
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
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut self.depth).range(1..=100).prefix("Depth: "));
                ui.add(egui::Slider::new(&mut self.zoom,0.01..=2.0).logarithmic(true).text("Zoom"));
                if ui.add_enabled(self.cancel.is_none(),egui::Button::new("Rebuild")).clicked() { rebuild=true; }
                if let Some(cancel)=&self.cancel { ui.spinner(); if ui.button("Cancel").clicked() { cancel.store(true,Ordering::Relaxed); } }
            });
            ui.label("Outgoing static calls · virtual/interface calls use declared targets · Intent destinations are not inferred.");
            ui.horizontal(|ui| { for kind in [Component::Activity,Component::Service,Component::Receiver,Component::Provider] { ui.colored_label(color(kind,ui.visuals().dark_mode),kind.label()); } });
            ui.label(&self.status);
            let Some(graph)=&self.graph else { return };
            if graph.truncated { ui.colored_label(ui.visuals().warn_fg_color,"Partial graph: reached 5,000 methods / 20,000 edges. All collected calls are displayed."); }
            let paths=graph.component_reachability();
            ui.label("All discovered calls shown · highlighted edges lead to components · drag the canvas to pan");
            if ui.button("Fit graph").clicked() {
                let columns=graph.nodes.iter().map(|n|n.depth).max().unwrap_or(0)+1;
                let mut rows=vec![0;columns];
                for node in &graph.nodes {rows[node.depth]+=1;}
                self.zoom=((ui.available_width()/(columns as f32*280.0)).min(ui.available_height()/(*rows.iter().max().unwrap_or(&1) as f32*155.0))).clamp(0.01,2.0);
            }
            egui::ScrollArea::both().show(ui,|ui| {
                let columns=graph.nodes.iter().map(|n|n.depth).max().unwrap_or(0)+1;
                let mut rows=vec![0usize;columns];
                let mut positions=Vec::new();
                for node in &graph.nodes {
                    let row=rows[node.depth]; rows[node.depth]+=1;
                    positions.push(Some(Vec2::new(node.depth as f32*280.0,row as f32*155.0)*self.zoom));
                }
                let size=Vec2::new(columns as f32*280.0,(*rows.iter().max().unwrap_or(&1) as f32*155.0).max(150.0))*self.zoom;
                let (area,_)=ui.allocate_exact_size(size,Sense::hover());
                let rects:Vec<_>=positions.iter().map(|p|p.map(|p|Rect::from_min_size(area.min+p,Vec2::new(230.0,140.0)*self.zoom))).collect();
                for edge in graph.edges.iter().filter(|e| paths[e.to]==0).chain(graph.edges.iter().filter(|e| paths[e.to]!=0)) {
                    let (Some(from),Some(to))=(rects[edge.from],rects[edge.to]) else {continue};
                    let highlighted=paths[edge.to]!=0;
                    let kind=match paths[edge.to] { 1=>Some(Component::Activity),2=>Some(Component::Service),4=>Some(Component::Receiver),8=>Some(Component::Provider),_=>graph.nodes[edge.to].component };
                    let tint=kind.map(|k|color(k,ui.visuals().dark_mode)).unwrap_or(if highlighted {ui.visuals().selection.stroke.color} else {ui.visuals().weak_text_color()});
                    let start=from.center_top()+Vec2::new(0.0,60.0)*self.zoom;
                    let end=to.center_top()+Vec2::new(0.0,60.0)*self.zoom;
                    let direction=if start==end {Vec2::X} else {(end-start).normalized()};
                    let a=start+direction*52.0*self.zoom;
                    let b=end-direction*52.0*self.zoom;
                    // Offset back-edges above the cards; preserve recursive edges.
                    if edge.to==edge.from || b.x<=a.x {
                        let top=from.top().min(to.top())-12.0;
                        ui.painter().line(vec![a,Pos2::new(a.x+12.0,top),Pos2::new(b.x-12.0,top),b],Stroke::new(if highlighted{2.5}else{0.7},tint));
                    } else {ui.painter().line_segment([a,b],Stroke::new(if highlighted{2.5}else{0.7},tint));}
                    let arrow=direction*10.0*self.zoom;
                    ui.painter().arrow(b-arrow,arrow,Stroke::new(if highlighted{2.0}else{1.0},tint));
                }
                for (i,node) in graph.nodes.iter().enumerate() {
                    let Some(rect)=rects[i] else {continue};
                    if !ui.clip_rect().intersects(rect) {continue;}
                    let tint=node.component.map(|k|color(k,ui.visuals().dark_mode)).unwrap_or(ui.visuals().text_color());
                    let center=rect.center_top()+Vec2::new(0.0,60.0)*self.zoom;
                    let highlighted=paths[i]!=0;
                    let rim=if node.component.is_some(){tint}else if highlighted{ui.visuals().selection.stroke.color}else{ui.visuals().weak_text_color()};
                    sphere(ui.painter(),center,52.0*self.zoom,rim,ui.visuals().dark_mode);
                    let before=node.method.split('(').next().unwrap_or(&node.method);
                    let (owner, method)=before.rsplit_once('.').unwrap_or(("",before));
                    let short_owner=owner.rsplit('.').next().unwrap_or(owner);
                    ui.painter().text(center-Vec2::new(0.0,6.0)*self.zoom,egui::Align2::CENTER_CENTER,abbreviate(method,16),egui::FontId::proportional(12.0*self.zoom),ui.visuals().text_color());
                    let label=node.component.map(Component::label).unwrap_or(if node.available {"Method"}else{"External"});
                    ui.painter().text(center+Vec2::new(0.0,14.0)*self.zoom,egui::Align2::CENTER_CENTER,label,egui::FontId::proportional(10.0*self.zoom),ui.visuals().text_color());
                    ui.painter().text(center+Vec2::new(0.0,66.0)*self.zoom,egui::Align2::CENTER_CENTER,abbreviate(short_owner,32),egui::FontId::proportional(11.0*self.zoom),tint);
                    let response=ui.interact(rect,ui.id().with(i),Sense::click()).on_hover_text(&node.method);
                    if response.clicked() && node.available {selected=Some(node.method.clone());}
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
                graph: Some(CallGraph {
                    nodes: vec![
                        Node {
                            method: "C.run()V".into(),
                            depth: 0,
                            component: None,
                            available: true,
                        },
                        Node {
                            method: "Activity.run()V".into(),
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
                }),
                ..Default::default()
            };
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                window.show_contents(ctx);
            });
            assert!(!output.shapes.is_empty());
            let cancel = Arc::new(AtomicBool::new(false));
            window.cancel = Some(cancel.clone());
            window.visible = false;
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                window.show(ctx);
            });
            assert!(cancel.load(Ordering::Relaxed));
        }
    }
}
