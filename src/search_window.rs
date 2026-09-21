//! Dedicated project-wide text-search window. Searching runs outside the UI thread.
use crate::search::{SearchHit, SearchQuery, SearchSummary, SearchUpdate};
use crate::settings::SearchPreferences;
use eframe::egui;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchMode {
    Class,
    Code,
    Methods,
    Fields,
}
impl SearchMode {
    pub const ALL: [Self; 4] = [Self::Class, Self::Code, Self::Methods, Self::Fields];
    pub fn label(self) -> &'static str {
        match self {
            Self::Class => "Class",
            Self::Code => "Code",
            Self::Methods => "Methods",
            Self::Fields => "Fields",
        }
    }
    fn apply(self, query: &mut SearchQuery) {
        query.classes = self == Self::Class;
        query.code = self == Self::Code;
        query.methods = self == Self::Methods;
        query.fields = self == Self::Fields;
    }
}

#[derive(Default)]
pub struct SearchActions {
    pub start: Option<SearchQuery>,
    pub cancel: bool,
    pub open: Option<SearchHit>,
}

pub struct SearchWindow {
    pub visible: bool,
    pub keep_open: bool,
    selected_result: Option<usize>,
    pub running: bool,
    pub results: Vec<SearchHit>,
    pub query: SearchQuery,
    pub status: String,
    scanned: usize,
    total: usize,
    hits: usize,
    skipped: usize,
    auto_search: bool,
    changed_at: Option<Instant>,
    cancel_requested: bool,
    errors: Vec<String>,
    was_visible: bool,
    started_at: Option<Instant>,
    node_fraction: f32,
    preview_cache: VecDeque<((usize, bool), egui::text::LayoutJob)>,
    exclusion_input: String,
    mode_changed: bool,
    stale_scope: bool,
}

impl Default for SearchWindow {
    fn default() -> Self {
        Self {
            visible: false,
            keep_open: false,
            selected_result: None,
            running: false,
            results: Vec::new(),
            query: SearchQuery::default(),
            status: "Choose a scope and enter text to search.".into(),
            scanned: 0,
            total: 0,
            hits: 0,
            skipped: 0,
            auto_search: false,
            changed_at: None,
            cancel_requested: false,
            errors: Vec::new(),
            was_visible: false,
            started_at: None,
            node_fraction: 0.45,
            preview_cache: VecDeque::new(),
            exclusion_input: String::new(),
            mode_changed: false,
            stale_scope: false,
        }
    }
}

impl SearchWindow {
    pub fn viewport_id() -> egui::ViewportId {
        egui::ViewportId::from_hash_of("rdx_text_search")
    }

    pub fn wake(ctx: &egui::Context) {
        // Only the root App::update drains worker events. The active viewport
        // can be Search when a background worker asks for a repaint.
        ctx.request_repaint_of(egui::ViewportId::ROOT);
        ctx.request_repaint_of(Self::viewport_id());
    }

    pub fn mode(&self) -> SearchMode {
        if self.query.classes {
            SearchMode::Class
        } else if self.query.methods {
            SearchMode::Methods
        } else if self.query.fields {
            SearchMode::Fields
        } else {
            SearchMode::Code
        }
    }

    pub fn open_mode(&mut self, mode: SearchMode) {
        let before = self.query.clone();
        mode.apply(&mut self.query);
        self.visible = true;
        if before != self.query {
            self.mode_changed = true;
            self.stale_scope = self.running;
            self.results.clear();
            self.preview_cache.clear();
            self.selected_result = None;
            self.errors.clear();
            self.scanned = 0;
            self.total = 0;
            self.hits = 0;
            self.status = format!("{} search — enter text to search.", mode.label());
        }
    }

    pub fn preferences(&self) -> SearchPreferences {
        SearchPreferences {
            classes: self.query.classes,
            methods: self.query.methods,
            fields: self.query.fields,
            code: self.query.code,
            resources: self.query.resources,
            comments: self.query.comments,
            excluded_packages: self.query.excluded_packages.clone(),
            case_sensitive: self.query.case_sensitive,
            regex: self.query.regex,
            auto_search: self.auto_search,
            keep_open: self.keep_open,
        }
    }

    pub fn restore_preferences(&mut self, preferences: &SearchPreferences) {
        self.query.classes = preferences.classes;
        self.query.methods = preferences.methods;
        self.query.fields = preferences.fields;
        self.query.code = preferences.code;
        self.query.resources = preferences.resources;
        self.query.comments = preferences.comments;
        self.query.excluded_packages = preferences.excluded_packages.clone();
        self.query.case_sensitive = preferences.case_sensitive;
        self.query.regex = preferences.regex;
        self.auto_search = preferences.auto_search;
        self.keep_open = preferences.keep_open;
        self.mode().apply(&mut self.query);
    }

    pub fn begin(&mut self) {
        self.stale_scope = false;
        self.mode_changed = false;
        self.running = true;
        self.results.clear();
        self.preview_cache.clear();
        self.selected_result = None;
        self.errors.clear();
        self.cancel_requested = false;
        self.changed_at = None;
        self.started_at = Some(Instant::now());
        self.scanned = 0;
        self.total = 0;
        self.hits = 0;
        self.skipped = 0;
        self.status = "Starting search…".into();
    }

    pub fn update(&mut self, update: SearchUpdate) {
        if self.stale_scope {
            return;
        }
        match update {
            SearchUpdate::Batch(hits) => self.results.extend(hits),
            SearchUpdate::Progress {
                scanned,
                total,
                hits,
                skipped,
            } => {
                self.scanned = scanned;
                self.total = total;
                self.hits = hits;
                self.skipped = skipped;
            }
        }
    }

    pub fn finish(&mut self, summary: SearchSummary) {
        self.running = false;
        self.cancel_requested = false;
        if self.stale_scope {
            self.stale_scope = false;
            self.started_at = None;
            return;
        }
        self.scanned = summary.scanned;
        self.total = summary.total;
        self.hits = summary.hits;
        self.skipped = summary.skipped;
        let state = if summary.cancelled {
            "Cancelled — partial results"
        } else if summary.limited {
            "Search stopped early — partial results"
        } else if summary.skipped > 0 {
            "Search complete — partial coverage"
        } else {
            "Search complete"
        };
        self.status = format!("{state}: {} matches · {} skipped", self.hits, self.skipped);
        if let Some(started) = self.started_at.take() {
            self.status
                .push_str(&format!(" · {:.2} s", started.elapsed().as_secs_f64()));
        }
        self.errors = summary.errors;
        if !self.errors.is_empty() {
            self.status
                .push_str(&format!(" · {} errors", self.errors.len()));
        }
    }

    pub fn error(&mut self, error: String) {
        self.results.clear();
        self.preview_cache.clear();
        self.selected_result = None;
        self.running = false;
        self.cancel_requested = false;
        self.changed_at = None;
        self.started_at = None;
        self.status = format!("Search failed: {error}");
        self.errors = vec![error];
    }

    pub fn show(&mut self, ctx: &egui::Context, can_start: bool) -> SearchActions {
        if !self.visible {
            self.was_visible = false;
            return SearchActions::default();
        }
        ctx.show_viewport_immediate(
            Self::viewport_id(),
            egui::ViewportBuilder::default()
                .with_title(format!("RDX — {} search", self.mode().label()))
                .with_inner_size([1000.0, 600.0])
                .with_min_inner_size([640.0, 300.0]),
            |ctx, _class| self.show_contents(ctx, can_start),
        )
    }

    fn show_contents(&mut self, ctx: &egui::Context, can_start: bool) -> SearchActions {
        if self.running {
            ctx.request_repaint_after_for(Duration::from_millis(100), egui::ViewportId::ROOT);
        }
        let mut actions = SearchActions::default();
        if !self.visible {
            self.was_visible = false;
            return actions;
        }
        let before = self.query.clone();
        let auto_before = self.auto_search;
        let mut visible = self.visible;
        if ctx.input(|input| input.viewport().close_requested()) {
            visible = false;
        }
        egui::TopBottomPanel::bottom("rdx_search_options").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.auto_search, "Auto search")
                    .on_hover_text("Search 600 ms after typing stops. Changes cancel the current search first.");
                ui.checkbox(&mut self.keep_open, "Keep open")
                    .on_hover_text("Keep the search window available behind the code window when selecting a result.");
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            let scope_selected = self.query.classes
                || self.query.methods
                || self.query.fields
                || self.query.code
                || self.query.resources
                || self.query.comments;
            let ready = can_start && !self.query.text.is_empty() && scope_selected;
            ui.horizontal(|ui| {
                ui.label("Search:");
                let input = ui.add_sized(
                    [ui.available_width() - 170.0, 24.0],
                    egui::TextEdit::singleline(&mut self.query.text)
                        .hint_text("Text or regular expression")
                        .id(egui::Id::new("rdx_search_query")),
                );
                let enter =
                    input.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                if !self.was_visible {
                    input.request_focus();
                }
                if (ui
                    .add_enabled(ready && !self.running, egui::Button::new("Search"))
                    .clicked()
                    || (enter && ready && !self.running))
                    && !self.query.text.is_empty()
                {
                    actions.start = Some(self.query.clone());
                }
                if ui
                    .add_enabled(
                        self.running && !self.cancel_requested,
                        egui::Button::new("Cancel"),
                    )
                    .clicked()
                {
                    self.changed_at = None;
                    self.cancel_requested = true;
                    actions.cancel = true;
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("Scope:");
                let mut selected = true;
                ui.add_enabled(
                    false,
                    egui::Checkbox::new(&mut selected, self.mode().label()),
                );
                ui.checkbox(&mut self.query.resources, "Resources");
                ui.checkbox(&mut self.query.comments, "Comments");
            });
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut self.query.case_sensitive, "Case sensitive");
                ui.checkbox(&mut self.query.regex, "Regex");
                ui.menu_button(
                    format!(
                        "Excluded packages ({}) ▾",
                        self.query.excluded_packages.len()
                    ),
                    |ui| {
                        ui.label("Remove a package to include it in searches.");
                        let mut remove = None;
                        egui::ScrollArea::vertical()
                            .max_height(220.0)
                            .show(ui, |ui| {
                                for (index, package) in
                                    self.query.excluded_packages.iter().enumerate()
                                {
                                    ui.push_id(index, |ui| {
                                        ui.horizontal(|ui| {
                                            if ui.small_button("Remove").clicked() {
                                                remove = Some(index);
                                            }
                                            ui.monospace(package);
                                        });
                                    });
                                }
                            });
                        if let Some(index) = remove {
                            self.query.excluded_packages.remove(index);
                        }
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.exclusion_input)
                                    .desired_width(170.0)
                                    .hint_text("com.example.*"),
                            );
                            let candidate =
                                crate::search::normalize_exclusion(&self.exclusion_input);
                            let valid = candidate
                                .as_ref()
                                .is_some_and(|s| !self.query.excluded_packages.contains(s))
                                && self.query.excluded_packages.len() < 128;
                            if ui.add_enabled(valid, egui::Button::new("Add")).clicked() {
                                self.query.excluded_packages.push(candidate.unwrap());
                                self.exclusion_input.clear();
                            }
                        });
                        ui.horizontal(|ui| {
                            if ui.button("Restore defaults").clicked() {
                                self.query.excluded_packages =
                                    crate::settings::default_excluded_packages();
                            }
                            if ui.button("Include all").clicked() {
                                self.query.excluded_packages.clear();
                            }
                        });
                        ui.weak("Package and subpackages; resources are unaffected.");
                    },
                );

                ui.label("Package:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.query.package)
                        .desired_width(220.0)
                        .hint_text("e.g. com.example"),
                )
                .on_hover_text("Limits class-based scopes; resources are unaffected.");
            });
            if !can_start && !self.running {
                ui.label("Open a project, or wait for the current engine operation to finish.");
            } else if !scope_selected {
                ui.label("Select at least one search scope.");
            }
            ui.separator();
            if self.running {
                let fraction = if self.total == 0 {
                    0.0
                } else {
                    (self.scanned as f32 / self.total as f32).clamp(0.0, 1.0)
                };
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .desired_width(ui.available_width())
                        .animate(self.total == 0)
                        .text(format!(
                            "{} {:.0}%",
                            if self.cancel_requested {
                                "Cancelling…"
                            } else {
                                "Searching…"
                            },
                            (fraction * 100.0).floor().min(99.0)
                        )),
                );
            } else {
                ui.label(&self.status);
            }
            if !self.errors.is_empty() {
                egui::CollapsingHeader::new("Search errors").show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(90.0)
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
                .id_salt("rdx_search_results")
                .auto_shrink([false, false])
                .show_rows(ui, row_height, self.results.len(), |ui, rows| {
                    for index in rows {
                        let hit = &self.results[index];
                        ui.push_id(index, |ui| {
                            let title = format!("{}:{}", hit.document.name, hit.line);
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
        }
        if actions.open.is_some() && !self.keep_open {
            visible = false;
        }
        self.visible = visible;
        self.was_visible = visible;
        if !visible {
            ctx.request_repaint_of(egui::ViewportId::ROOT);
            self.changed_at = None;
            if self.running && !self.cancel_requested {
                self.cancel_requested = true;
                actions.cancel = true;
            }
            return actions;
        }
        let mode_changed = std::mem::take(&mut self.mode_changed);
        if before != self.query || mode_changed || (self.auto_search && !auto_before) {
            self.changed_at = self.auto_search.then(Instant::now);
            if self.running && !self.cancel_requested {
                self.cancel_requested = true;
                actions.cancel = true;
            }
        }
        if !self.auto_search {
            self.changed_at = None;
        }
        if let Some(changed) = self.changed_at {
            let elapsed = changed.elapsed();
            if elapsed < Duration::from_millis(600) {
                ctx.request_repaint_after(Duration::from_millis(600) - elapsed);
            } else if !self.running
                && can_start
                && !self.query.text.is_empty()
                && (self.query.classes
                    || self.query.methods
                    || self.query.fields
                    || self.query.code
                    || self.query.resources
                    || self.query.comments)
            {
                actions.start = Some(self.query.clone());
                self.changed_at = None;
            }
        }
        if actions.start.is_some() {
            self.changed_at = None;
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
    fn modes_are_exclusive_preserve_options_and_cancel_old_scope() {
        let ctx = egui::Context::default();
        let mut window = SearchWindow::default();
        window.query.resources = true;
        window.query.comments = true;
        window.query.excluded_packages = vec!["custom.*".into()];
        for mode in SearchMode::ALL {
            window.open_mode(mode);
            assert_eq!(window.mode(), mode);
            assert_eq!(
                [
                    window.query.classes,
                    window.query.code,
                    window.query.methods,
                    window.query.fields
                ]
                .iter()
                .filter(|&&v| v)
                .count(),
                1
            );
            assert!(window.query.resources && window.query.comments);
            assert_eq!(window.query.excluded_packages, vec!["custom.*"]);
        }
        window.begin();
        window.open_mode(SearchMode::Class);
        let mut cancel = false;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            cancel = window.show_contents(ctx, false).cancel;
        });
        assert!(cancel);
        window.update(SearchUpdate::Progress {
            scanned: 99,
            total: 100,
            hits: 10,
            skipped: 0,
        });
        assert_eq!(
            window.scanned, 0,
            "old scope updates must not leak into the new mode"
        );
        window.finish(SearchSummary::default());
        assert!(!window.running);
        assert!(window.status.starts_with("Class search"));
    }

    #[test]
    fn worker_completion_wakes_root_and_search_and_clears_running() {
        let ctx = egui::Context::default();
        let requested = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = requested.clone();
        ctx.set_request_repaint_callback(move |request| {
            captured.lock().unwrap().push(request.viewport_id);
        });
        SearchWindow::wake(&ctx);
        let requested = requested.lock().unwrap();
        assert!(requested.contains(&egui::ViewportId::ROOT));
        assert!(requested.contains(&SearchWindow::viewport_id()));
        let mut window = SearchWindow::default();
        window.begin();
        window.update(SearchUpdate::Progress {
            scanned: 100,
            total: 100,
            hits: 0,
            skipped: 0,
        });
        assert!(window.running);
        let mut summary = SearchSummary::default();
        summary.scanned = 100;
        summary.total = 100;
        window.finish(summary);
        assert!(!window.running);
        assert!(window.status.starts_with("Search complete"));
    }

    #[test]
    fn running_search_draws_progress_without_counts_or_index_hint() {
        let ctx = egui::Context::default();
        let mut window = SearchWindow {
            visible: true,
            ..Default::default()
        };
        window.begin();
        window.update(SearchUpdate::Progress {
            scanned: 25,
            total: 100,
            hits: 3,
            skipped: 2,
        });
        let render = |window: &mut SearchWindow| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 600.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    window.show_contents(ctx, true);
                },
            )
        };
        render(&mut window);
        let output = render(&mut window);
        let labels: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) => Some(text.galley.job.text.as_str()),
                _ => None,
            })
            .collect();
        assert!(labels.contains(&"Searching… 25%"));
        assert!(
            !labels
                .iter()
                .any(|label| label.contains("25/100") || label.contains("First Code/Comments"))
        );
        let progress_center = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.job.text == "Searching… 25%" => {
                    Some(text.pos + text.galley.rect.center().to_vec2())
                }
                _ => None,
            })
            .unwrap();
        let bar = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Rect(rect)
                    if rect.rect.width() > 900.0
                        && rect.rect.height() < 30.0
                        && rect.rect.contains(progress_center) =>
                {
                    Some(rect.rect)
                }
                _ => None,
            })
            .expect("a full-width progress bar is rendered");
        assert!(
            output.shapes.iter().any(|shape| match &shape.shape {
                egui::epaint::Shape::Rect(rect) =>
                    (rect.rect.left() - bar.left()).abs() < 1.0
                        && (rect.rect.top() - bar.top()).abs() < 1.0
                        && (rect.rect.width() - bar.width() * 0.25).abs() < 1.0,
                _ => false,
            }),
            "progress fill reflects the fraction scanned"
        );
        window.cancel_requested = true;
        let output = render(&mut window);
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::epaint::Shape::Text(text) if text.galley.job.text == "Cancelling… 25%")));
        window.begin();
        assert_eq!(
            (window.scanned, window.total, window.hits, window.skipped),
            (0, 0, 0, 0)
        );
        assert!(!window.cancel_requested);
        let output = render(&mut window);
        assert_eq!(
            output.viewport_output[&egui::ViewportId::ROOT].repaint_delay,
            Duration::ZERO,
            "unknown totals animate and request another frame"
        );
    }

    #[test]
    fn preferences_restore_all_flags_without_persisting_query() {
        let preferences = SearchPreferences {
            classes: true,
            methods: false,
            fields: false,
            code: false,
            resources: true,
            comments: true,
            excluded_packages: vec!["custom.lib.*".into()],
            case_sensitive: true,
            regex: true,
            auto_search: true,
            keep_open: true,
        };
        let mut window = SearchWindow::default();
        window.query.text = "transient".into();
        window.query.package = "sample".into();
        window.restore_preferences(&preferences);
        assert_eq!(window.preferences(), preferences);
        window.restore_preferences(&SearchPreferences::default());
        assert_eq!(window.preferences(), SearchPreferences::default());
        assert_eq!(window.query.text, "transient");
        assert_eq!(window.query.package, "sample");
    }

    #[test]
    fn divider_drag_resizes_columns_and_options_stay_at_bottom() {
        let ctx = egui::Context::default();
        let mut window = SearchWindow {
            visible: true,
            ..Default::default()
        };
        let mut render = |events| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 600.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    window.show_contents(ctx, true);
                },
            )
        };
        render(vec![]);
        let output = render(vec![]);
        let text_position = |output: &egui::FullOutput, label| {
            output
                .shapes
                .iter()
                .rev()
                .find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                        Some(text.pos)
                    }
                    _ => None,
                })
                .unwrap()
        };
        let code = text_position(&output, "Code");
        assert!(text_position(&output, "Auto search").y > 550.0);
        assert!(text_position(&output, "Keep open").y > 550.0);
        let divider = egui::pos2(code.x - 8.0, code.y + 5.0);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        render(vec![
            egui::Event::PointerMoved(divider),
            button(divider, true),
        ]);
        let destination = divider + egui::vec2(120.0, 0.0);
        render(vec![egui::Event::PointerMoved(destination)]);
        let output = render(vec![button(destination, false)]);
        assert!(text_position(&output, "Code").x > code.x + 100.0);
        assert!(window.node_fraction > 0.5);
    }

    #[test]
    fn closing_window_cancels_search_and_clears_pending_auto_search() {
        let ctx = egui::Context::default();
        let mut window = SearchWindow {
            visible: true,
            running: true,
            changed_at: Some(Instant::now()),
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
            let actions = window.show_contents(ctx, false);
            assert!(actions.cancel);
            assert!(actions.start.is_none());
        });
        assert!(!window.visible);
        assert!(window.changed_at.is_none());
    }

    #[test]
    fn result_shows_preview_and_opens_on_click() {
        check_result_click(false);
    }

    #[test]
    fn keep_open_preserves_window_after_result_click() {
        check_result_click(true);
    }

    fn check_result_click(keep_open: bool) {
        let ctx = egui::Context::default();
        let mut window = SearchWindow {
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
        let render = |window: &mut SearchWindow, time, events| {
            let mut actions = SearchActions::default();
            let output = ctx.run(
                egui::RawInput {
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    actions = window.show(ctx, true);
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
        let node_x = text_x("sample.Example:1");
        let code_x = text_x("return target;");
        assert!(
            (node_x - text_x("Node")).abs() < 10.0,
            "node text aligns with its header: node={node_x}, header={}",
            text_x("Node")
        );
        let code_header_x = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.job.text == "Code" => {
                    Some(text.pos.x)
                }
                _ => None,
            })
            .min_by(|left, right| (left - code_x).abs().total_cmp(&(right - code_x).abs()))
            .expect("Code header is painted");
        assert!(
            (code_x - code_header_x).abs() < 10.0,
            "code text aligns with its header: code={code_x}, header={code_header_x}",
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
        window.begin();
        assert!(window.preview_cache.is_empty());
    }
}
