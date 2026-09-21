//! Symbol-reference results in an independent native window.
use crate::search::SearchHit;
use eframe::egui;
use std::collections::VecDeque;

#[derive(Default)]
pub struct UsagesActions {
    pub open: Option<SearchHit>,
    pub cancel: bool,
}
pub struct UsagesWindow {
    pub visible: bool,
    pub label: String,
    pub status: String,
    pub running: bool,
    pub results: Vec<SearchHit>,
    pub keep_open: bool,
    selected_result: Option<usize>,
    cancel_requested: bool,
    errors: Vec<String>,
    node_fraction: f32,
    preview_cache: VecDeque<((usize, bool), egui::text::LayoutJob)>,
}
impl Default for UsagesWindow {
    fn default() -> Self {
        Self {
            visible: false,
            label: String::new(),
            status: String::new(),
            running: false,
            results: Vec::new(),
            keep_open: true,
            selected_result: None,
            cancel_requested: false,
            errors: Vec::new(),
            node_fraction: 0.45,
            preview_cache: VecDeque::new(),
        }
    }
}
impl UsagesWindow {
    pub fn begin(&mut self, label: String) {
        self.visible = true;
        self.label = label;
        self.running = true;
        self.status = "Finding references…".into();
        self.results.clear();
        self.preview_cache.clear();
        self.errors.clear();
        self.selected_result = None;
        self.cancel_requested = false;
    }
    pub fn append(&mut self, hits: Vec<SearchHit>) {
        let remaining = 1000_usize.saturating_sub(self.results.len());
        if hits.len() > remaining && self.errors.len() < 8 {
            self.errors
                .push("Results limited to 1,000 references; coverage is partial.".into());
        }
        self.results.extend(hits.into_iter().take(remaining));
        self.status = format!("{} references found", self.results.len());
    }
    pub fn finish(&mut self, status: String, errors: Vec<String>) {
        self.running = false;
        self.cancel_requested = false;
        self.status = status;
        self.errors.extend(
            errors
                .into_iter()
                .take(8_usize.saturating_sub(self.errors.len())),
        );
    }
    pub fn error(&mut self, error: String) {
        self.finish(format!("Find usages failed: {error}"), vec![error]);
    }
    pub fn viewport_id() -> egui::ViewportId {
        egui::ViewportId::from_hash_of("rdx_find_usages")
    }
    pub fn show(&mut self, ctx: &egui::Context) -> UsagesActions {
        if !self.visible {
            return UsagesActions::default();
        }
        ctx.show_viewport_immediate(
            Self::viewport_id(),
            egui::ViewportBuilder::default()
                .with_title("RDX — Find usages")
                .with_inner_size([1000.0, 560.0])
                .with_min_inner_size([600.0, 280.0]),
            |ctx, _| self.show_contents(ctx),
        )
    }
    fn show_contents(&mut self, ctx: &egui::Context) -> UsagesActions {
        let mut actions = UsagesActions::default();
        if !self.visible {
            return actions;
        }
        let mut visible = !ctx.input(|input| input.viewport().close_requested());
        egui::TopBottomPanel::bottom("rdx_usages_options").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.keep_open, "Keep open").on_hover_text(
                    "Keep references available behind the code window when opening a result.",
                );
                if self.running
                    && ui
                        .add_enabled(!self.cancel_requested, egui::Button::new("Cancel"))
                        .clicked()
                {
                    self.cancel_requested = true;
                    actions.cancel = true;
                }
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.strong("Usage for:");
                ui.monospace(&self.label);
            });
            ui.horizontal(|ui| {
                if self.running {
                    ui.spinner();
                }
                ui.label(if self.cancel_requested {
                    "Cancelling — waiting for the current document…"
                } else {
                    &self.status
                });
            });
            if !self.errors.is_empty() {
                egui::CollapsingHeader::new("Partial results / errors").show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(80.0)
                        .show(ui, |ui| {
                            for error in &self.errors {
                                ui.label(error);
                            }
                        });
                });
            }
            ui.separator();
            let table_width = ui.available_width();
            let minimum = 140.0_f32.min(table_width / 2.0);
            let node_width =
                (table_width * self.node_fraction).clamp(minimum, table_width - minimum);
            let header_height = 24.0;
            let (header, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), header_height),
                egui::Sense::hover(),
            );
            ui.painter().text(
                header.min + egui::vec2(26.0, 4.0),
                egui::Align2::LEFT_TOP,
                "Node",
                egui::FontId::proportional(14.0),
                ui.visuals().text_color(),
            );
            ui.painter().text(
                header.min + egui::vec2(node_width + 8.0, 4.0),
                egui::Align2::LEFT_TOP,
                "Code",
                egui::FontId::proportional(14.0),
                ui.visuals().text_color(),
            );
            ui.separator();
            let row_height = 25.0;
            egui::ScrollArea::vertical()
                .id_salt("rdx_usages_results")
                .auto_shrink([false, false])
                .show_rows(ui, row_height, self.results.len(), |ui, rows| {
                    for index in rows {
                        let hit = &self.results[index];
                        ui.push_id(index, |ui| {
                            let title = format!(
                                "{}:{}",
                                if hit.kind.is_empty() {
                                    &hit.document.name
                                } else {
                                    &hit.kind
                                },
                                hit.line
                            );
                            let (rect, response) = ui.allocate_exact_size(
                                egui::vec2(
                                    ui.available_width(),
                                    row_height - ui.spacing().item_spacing.y,
                                ),
                                egui::Sense::click(),
                            );
                            if self.selected_result == Some(index) {
                                ui.painter().rect_filled(
                                    rect,
                                    0.0,
                                    ui.visuals().selection.bg_fill.gamma_multiply(0.35),
                                );
                            } else if response.hovered() || response.has_focus() {
                                ui.painter().rect_filled(
                                    rect,
                                    0.0,
                                    ui.visuals().widgets.hovered.bg_fill,
                                );
                            }
                            let icon = match hit.document.target {
                                crate::search::SearchTarget::Class(_) => {
                                    crate::icons::Icon::Classes
                                }
                                crate::search::SearchTarget::Resource(_) => {
                                    crate::icons::Icon::Resources
                                }
                            };
                            crate::icons::paint(
                                ui,
                                egui::Rect::from_min_size(rect.min, egui::vec2(18.0, 18.0)),
                                icon,
                            );
                            let divider = rect.left() + node_width;
                            ui.painter().line_segment(
                                [
                                    egui::pos2(divider, rect.top()),
                                    egui::pos2(
                                        divider,
                                        rect.bottom() + ui.spacing().item_spacing.y,
                                    ),
                                ],
                                ui.visuals().widgets.noninteractive.bg_stroke,
                            );
                            let node_rect = egui::Rect::from_min_max(
                                rect.min + egui::vec2(24.0, 0.0),
                                egui::pos2(divider - 6.0, rect.bottom()),
                            );
                            ui.painter().with_clip_rect(node_rect).text(
                                node_rect.left_center(),
                                egui::Align2::LEFT_CENTER,
                                title,
                                egui::FontId::monospace(13.0),
                                ui.visuals().text_color(),
                            );
                            let key = (index, ui.visuals().dark_mode);
                            let preview = if let Some((_, preview)) =
                                self.preview_cache.iter().find(|(cached, _)| *cached == key)
                            {
                                preview.clone()
                            } else {
                                let preview = crate::code_view::search_preview(
                                    &hit.preview,
                                    &hit.document.syntax,
                                    hit.preview_match.clone(),
                                    key.1,
                                );
                                if self.preview_cache.len() >= 128 {
                                    self.preview_cache.pop_front();
                                }
                                self.preview_cache.push_back((key, preview.clone()));
                                preview
                            };
                            let code_rect = egui::Rect::from_min_max(
                                egui::pos2(divider + 8.0, rect.top()),
                                rect.max,
                            );
                            let galley = ui.fonts(|fonts| fonts.layout_job(preview));
                            let position = egui::pos2(
                                code_rect.left(),
                                code_rect.center().y - galley.size().y / 2.0,
                            );
                            ui.painter().with_clip_rect(code_rect).galley(
                                position,
                                galley,
                                ui.visuals().text_color(),
                            );
                            if response
                                .on_hover_text(format!(
                                    "{} · {}:{}\n{}",
                                    hit.kind, hit.document.name, hit.line, hit.preview
                                ))
                                .clicked()
                            {
                                self.selected_result = Some(index);
                                actions.open = Some(hit.clone());
                            }
                        });
                    }
                });
            let divider_x = header.left() + node_width;
            let divider_rect = egui::Rect::from_min_max(
                egui::pos2(divider_x - 4.0, header.top()),
                egui::pos2(divider_x + 4.0, ui.max_rect().bottom()),
            );
            let resize = ui
                .interact(
                    divider_rect,
                    ui.id().with("column_divider"),
                    egui::Sense::drag(),
                )
                .on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
                .on_hover_text("Drag to resize Node and Code columns");
            if resize.dragged()
                && let Some(pointer) = resize.interact_pointer_pos()
            {
                self.node_fraction = ((pointer.x - header.left())
                    .clamp(minimum, table_width - minimum)
                    / table_width)
                    .clamp(0.0, 1.0);
                ui.ctx().request_repaint();
            }
            ui.painter().line_segment(
                [
                    egui::pos2(divider_x, header.top()),
                    egui::pos2(divider_x, ui.max_rect().bottom()),
                ],
                if resize.hovered() || resize.dragged() {
                    ui.visuals().widgets.hovered.fg_stroke
                } else {
                    ui.visuals().widgets.noninteractive.bg_stroke
                },
            );
        });
        if actions.open.is_some() {
            ctx.request_repaint_of(egui::ViewportId::ROOT);
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
            if !self.keep_open {
                visible = false;
            }
        }
        self.visible = visible;
        if !visible && self.running && !self.cancel_requested {
            self.cancel_requested = true;
            actions.cancel = true;
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{SearchDocument, SearchTarget};
    use std::sync::Arc;
    #[test]
    fn result_click_focuses_code_and_respects_keep_open() {
        check_result_click(true);
        check_result_click(false);
    }
    #[test]
    fn viewport_is_independent_and_closing_cancels() {
        assert_ne!(
            UsagesWindow::viewport_id(),
            egui::ViewportId::from_hash_of("rdx_text_search")
        );
        assert_ne!(UsagesWindow::viewport_id(), egui::ViewportId::ROOT);
        let ctx = egui::Context::default();
        let mut window = UsagesWindow::default();
        window.begin("Target.method()".into());
        assert!(window.keep_open);
        let mut input = egui::RawInput::default();
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .events
            .push(egui::ViewportEvent::Close);
        let _ = ctx.run(input, |ctx| assert!(window.show_contents(ctx).cancel));
        assert!(!window.visible);
    }
    fn check_result_click(keep_open: bool) {
        let ctx = egui::Context::default();
        let mut window = UsagesWindow {
            visible: true,
            keep_open,
            results: vec![SearchHit {
                document: Arc::new(SearchDocument {
                    target: SearchTarget::Class("sample.Example".into()),
                    name: "sample.Example".into(),
                    source: "return target;".into(),
                    syntax: "java".into(),
                    links: Vec::new(),
                    source_hash: None,
                    metadata_complete: true,
                }),
                start: 7,
                end: 13,
                line: 1,
                kind: "code".into(),
                preview: "return target;".into(),
                preview_match: 7..13,
            }],
            ..Default::default()
        };
        let render = |window: &mut UsagesWindow, time, events| {
            let mut actions = UsagesActions::default();
            let output = ctx.run(
                egui::RawInput {
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    actions = window.show(ctx);
                },
            );
            (actions, output)
        };
        render(&mut window, 0.0, vec![]);
        let (_, output) = render(&mut window, 0.1, vec![]);
        assert_eq!(
            window.preview_cache.len(),
            1,
            "repaints reuse highlighted rows"
        );
        let text_x = |needle: &str| {
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) if text.galley.job.text == needle => {
                        Some(text.pos.x)
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{needle} is painted"))
        };
        let node_x = text_x("code:1");
        let code_x = text_x("return target;");
        assert!(
            (node_x - text_x("Node")).abs() < 10.0,
            "node text aligns with its header: node={node_x}, header={}",
            text_x("Node")
        );
        assert!(
            (code_x - text_x("Code")).abs() < 10.0,
            "code text aligns with its header: code={code_x}, header={}",
            text_x("Code")
        );
        let point = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.job.text == "return target;" => {
                    let highlighted: String = text
                        .galley
                        .job
                        .sections
                        .iter()
                        .filter(|section| section.format.background != egui::Color32::TRANSPARENT)
                        .map(|section| &text.galley.job.text[section.byte_range.clone()])
                        .collect();
                    assert_eq!(highlighted, "target");
                    Some(text.pos + text.galley.rect.center().to_vec2())
                }
                _ => None,
            })
            .expect("preview must be visible separately from title");
        let button = |pressed| egui::Event::PointerButton {
            pos: point,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        render(
            &mut window,
            0.2,
            vec![egui::Event::PointerMoved(point), button(true)],
        );
        let (actions, output) = render(&mut window, 0.3, vec![button(false)]);
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::Focus))
        );
        assert_eq!(actions.open.unwrap().start, 7);
        assert_eq!(window.visible, keep_open);
        assert_eq!(window.selected_result, Some(0));
        assert!(!actions.cancel);
        window.begin("target".into());
        assert!(window.preview_cache.is_empty());
    }
}
