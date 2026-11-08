//! Bounded GUI projections over parsed DEX metadata.
use eframe::egui;
use serde_json::{Value, json};

pub const PAGE_SIZE: usize = 40;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnalysisKind {
    #[default]
    Annotations,
    Services,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub kind: AnalysisKind,
    pub annotation_type: String,
    pub element: String,
    pub package_prefix: String,
    pub offset: usize,
}
impl Query {
    pub fn arguments(&self) -> Value {
        let mut args = json!({"offset":self.offset,"limit":PAGE_SIZE});
        if !self.package_prefix.trim().is_empty() {
            args["package_prefix"] = json!(self.package_prefix.trim());
        }
        match self.kind {
            AnalysisKind::Annotations => {
                args["annotation_type"] =
                    json!(self.annotation_type.trim().trim_start_matches('@'));
                args["element"] = json!(self.element);
            }
            AnalysisKind::Services => args["projection"] = json!("services"),
        }
        args
    }
}

#[derive(Default)]
pub struct Actions {
    pub start: Option<Query>,
    pub cancel: bool,
    pub invalidate: bool,
    pub open_method: Option<String>,
    pub open_class: Option<String>,
}

pub struct AnalysisWindow {
    pub visible: bool,
    pub kind: AnalysisKind,
    pub annotation_type: String,
    pub element: String,
    pub package_prefix: String,
    pub running: bool,
    pub status: String,
    pub page: Option<Value>,
    pub active_query: Option<Query>,
    pub selected_row: Option<usize>,
    previous_offsets: Vec<usize>,
    row_previews: Vec<(String, String)>,
    selected_preview: String,
    cancel_requested: bool,
}
impl Default for AnalysisWindow {
    fn default() -> Self {
        Self {
            visible: false,
            kind: AnalysisKind::Annotations,
            annotation_type: String::new(),
            element: "all".into(),
            package_prefix: String::new(),
            running: false,
            status: "Enter an exact annotation name, or open Retrofit Services.".into(),
            page: None,
            active_query: None,
            selected_row: None,
            previous_offsets: Vec::new(),
            row_previews: Vec::new(),
            selected_preview: String::new(),
            cancel_requested: false,
        }
    }
}
impl AnalysisWindow {
    pub fn viewport_id() -> egui::ViewportId {
        egui::ViewportId::from_hash_of("rdx_parsed_dex_analysis")
    }
    /// Return true when an active worker must be cancelled by App.
    pub fn open(&mut self, kind: AnalysisKind) -> bool {
        let cancel = if self.kind != kind {
            self.clear_query()
        } else {
            false
        };
        self.kind = kind;
        self.visible = true;
        cancel
    }
    pub fn close(&mut self) -> bool {
        let cancel = self.clear_query();
        self.visible = false;
        cancel
    }
    fn clear_query(&mut self) -> bool {
        let cancel = self.running;
        self.running = false;
        self.cancel_requested = false;
        self.page = None;
        self.active_query = None;
        self.selected_row = None;
        self.previous_offsets.clear();
        self.row_previews.clear();
        self.selected_preview.clear();
        self.status = "Run a query for this tab.".into();
        cancel
    }
    pub fn query(&self, offset: usize) -> Result<Query, String> {
        if self.kind == AnalysisKind::Annotations
            && self
                .annotation_type
                .trim()
                .trim_start_matches('@')
                .is_empty()
        {
            return Err("Enter the exact annotation FQCN or DEX descriptor.".into());
        }
        Ok(Query {
            kind: self.kind,
            annotation_type: self.annotation_type.clone(),
            element: self.element.clone(),
            package_prefix: self.package_prefix.clone(),
            offset,
        })
    }
    pub fn begin(&mut self, query: Query) {
        if query.offset == 0 {
            self.previous_offsets.clear();
        } else if let Some(current) = &self.active_query {
            if query.offset > current.offset {
                self.previous_offsets.push(current.offset);
            } else if query.offset < current.offset {
                self.previous_offsets.pop();
            }
        }
        self.kind = query.kind;
        self.active_query = Some(query);
        self.running = true;
        self.cancel_requested = false;
        self.page = None;
        self.selected_row = None;
        self.row_previews.clear();
        self.selected_preview.clear();
        self.status = "Scanning parsed DEX metadata…".into();
    }
    pub fn cancel(&mut self) {
        if self.running {
            self.cancel_requested = true;
            self.status = "Cancelling query…".into();
        }
    }
    pub fn page_query(&self, forward: bool) -> Option<Query> {
        if self.running {
            return None;
        }
        let current = self.active_query.as_ref()?;
        let page = self.page.as_ref()?;
        let offset = if forward {
            page["next_offset"].as_u64()? as usize
        } else {
            *self.previous_offsets.last()?
        };
        Some(Query {
            offset,
            ..current.clone()
        })
    }
    pub fn finish(&mut self, result: Result<Value, String>) {
        if self.active_query.is_none() {
            return;
        }
        if self.cancel_requested {
            self.running = false;
            self.cancel_requested = false;
            self.status = "Query cancelled.".into();
            return;
        }
        self.running = false;
        self.cancel_requested = false;
        match result {
            Ok(page) => {
                let offset = page["offset"].as_u64().unwrap_or(0) as usize;
                let count = page["items"].as_array().map_or(0, Vec::len);
                let total = page["total"]
                    .as_u64()
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| {
                        format!(
                            "at least {}",
                            page["matching_candidates_so_far"]
                                .as_u64()
                                .unwrap_or(count as u64)
                        )
                    });
                let coverage = if page["coverage"]["complete"] == false || page["complete"] == false
                {
                    " · partial coverage"
                } else {
                    " · scan complete"
                };
                self.status = format!(
                    "{}–{} of {} results{}",
                    if count == 0 { 0 } else { offset + 1 },
                    offset + count,
                    total,
                    coverage
                );
                if page["requested_type_present"] == false {
                    self.status
                        .push_str(" · annotation type absent from loaded DEX tables");
                }
                self.row_previews = page["items"]
                    .as_array()
                    .map(|rows| rows.iter().map(|row| row_preview(row, self.kind)).collect())
                    .unwrap_or_default();
                self.page = Some(page);
            }
            Err(error) => self.status = error,
        }
    }
    pub fn stop_project(&mut self) {
        self.clear_query();
        self.status = "Project changed. Run the query for the loaded project.".into();
    }
    pub fn target(row: &Value) -> Option<(bool, String)> {
        if let Some(method) = row["method_id"].as_str() {
            Some((true, method.into()))
        } else {
            row["class_id"].as_str().map(|class| (false, class.into()))
        }
    }
    pub fn show(&mut self, ctx: &egui::Context, ready: bool) -> Actions {
        if !self.visible {
            return Actions::default();
        }
        ctx.show_viewport_immediate(
            Self::viewport_id(),
            egui::ViewportBuilder::default()
                .with_title("RDX — Parsed DEX analysis")
                .with_inner_size([1150.0, 700.0])
                .with_min_inner_size([650.0, 360.0]),
            |ctx, _| self.contents(ctx, ready),
        )
    }
    fn contents(&mut self, ctx: &egui::Context, ready: bool) -> Actions {
        let mut actions = Actions::default();
        if ctx.input(|input| input.viewport().close_requested()) {
            actions.cancel = self.close();
            actions.invalidate = actions.cancel;
            return actions;
        }
        egui::CentralPanel::default().show(ctx,|ui| {
            let previous_kind = self.kind;
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.kind,AnalysisKind::Annotations,"Annotation Search");
                ui.selectable_value(&mut self.kind,AnalysisKind::Services,"Retrofit Services");
            });
            if self.kind != previous_kind {
                actions.cancel = self.clear_query();
                actions.invalidate = actions.cancel;
            }
            ui.separator();
            if self.kind==AnalysisKind::Annotations {
                ui.horizontal(|ui| {
                    ui.label("Annotation");
                    ui.add(egui::TextEdit::singleline(&mut self.annotation_type).desired_width(450.0)
                        .hint_text("retrofit2.http.Url or Lretrofit2/http/Url;"));
                    egui::ComboBox::from_id_salt("annotation_element").selected_text(&self.element)
                        .show_ui(ui,|ui|for value in ["all","class","field","method","parameter"] {
                            ui.selectable_value(&mut self.element,value.into(),value);
                        });
                });
                ui.weak("Exact original DEX name. Recovered renamed Retrofit roles appear in Services.");
            } else {
                ui.weak("Static service declarations and conditional creation evidence. Candidate URLs are not verified runtime destinations.");
            }
            ui.horizontal(|ui| {
                ui.label("Package prefix");
                ui.add(egui::TextEdit::singleline(&mut self.package_prefix).desired_width(300.0).hint_text("optional"));
                if ui.add_enabled(ready && !self.running,egui::Button::new("Search / Refresh")).clicked() {
                    match self.query(0) {Ok(query)=>actions.start=Some(query),Err(error)=>self.status=error}
                }
                if self.running && ui.add_enabled(!self.cancel_requested,egui::Button::new("Cancel")).clicked() {
                    self.cancel(); actions.cancel=true;
                }
            });
            if self.running {ui.spinner();}
            ui.label(&self.status);
            if let Some(page)=&self.page {
                if let Some(note)=page["empty_result_note"].as_str() {ui.label(note);}
                if let Some(scope)=page["scope"].as_str().or_else(||page["coverage"]["scope"].as_str()) {ui.weak(scope);}
                if let Some(warnings)=page["warnings"].as_array() {for warning in warnings.iter().take(3) {
                    if let Some(message)=warning["message"].as_str() {ui.colored_label(ui.visuals().warn_fg_color,message);}
                }}
                if page["coverage"]["complete"]==false {ui.colored_label(ui.visuals().warn_fg_color,format!("Coverage details: {}",page["coverage"]));}
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(self.page_query(false).is_some(),egui::Button::new("Previous page")).clicked() {
                    actions.start=self.page_query(false);
                }
                if ui.add_enabled(self.page_query(true).is_some(),egui::Button::new("Next page")).clicked() {
                    actions.start=self.page_query(true);
                }
            });
            ui.separator();
            egui::ScrollArea::both().id_salt("rdx_analysis_result_rows").auto_shrink([false,false])
                .max_height((ui.available_height()*0.55).max(100.0)).show(ui,|ui| {
                    ui.set_min_width(1050.0);
                    let rows=self.page.as_ref().and_then(|p|p["items"].as_array());
                    if let Some(rows)=rows {
                        if self.kind==AnalysisKind::Services {
                            egui::Grid::new("retrofit_service_rows").striped(true).min_col_width(100.0).show(ui,|ui| {
                                ui.strong("Method"); ui.strong("Verb"); ui.strong("Path / dynamic URL"); ui.strong("Action"); ui.end_row();
                                for (index,row) in rows.iter().enumerate() {
                                    let method=row["method_id"].as_str().unwrap_or("unknown method");
                                    if ui.selectable_label(self.selected_row==Some(index),method).clicked() {
                                        self.selected_row=Some(index);
                                        self.selected_preview=bounded(serde_json::to_string_pretty(row).unwrap_or_default(),16_000);
                                    }
                                    ui.monospace(row["http_verb"].as_str().unwrap_or("?"));
                                    ui.add(egui::Label::new(service_path_summary(row)).selectable(true).wrap_mode(egui::TextWrapMode::Extend));
                                    if ui.small_button("Open owner").clicked() && let Some((method,id))=Self::target(row) {
                                        if method {actions.open_method=Some(id)} else {actions.open_class=Some(id)}
                                    }
                                    ui.end_row();
                                }
                            });
                        } else {
                            for (index,row) in rows.iter().enumerate() {
                                let (title,detail)=&self.row_previews[index];
                                ui.horizontal(|ui| {
                                    if ui.selectable_label(self.selected_row==Some(index),title).clicked() {
                                        self.selected_row=Some(index);
                                        self.selected_preview=bounded(serde_json::to_string_pretty(row).unwrap_or_default(),16_000);
                                    }
                                    if ui.small_button("Open owner").clicked() && let Some((method,id))=Self::target(row) {
                                        if method {actions.open_method=Some(id)} else {actions.open_class=Some(id)}
                                    }
                                    ui.add(egui::Label::new(detail).selectable(true).wrap_mode(egui::TextWrapMode::Extend));
                                });
                            }
                        }
                    }
                });
            if let Some(index)=self.selected_row {
                ui.separator();
                if let Some((_,detail))=self.row_previews.get(index) {
                    ui.add(egui::Label::new(detail).selectable(true).wrap());
                }
                ui.horizontal(|ui| {
                    ui.strong("Selected evidence · preview");
                    if ui.button("Copy full record").clicked()
                        && let Some(row)=self.page.as_ref().and_then(|p|p["items"].as_array()).and_then(|rows|rows.get(index)) {
                            ctx.copy_text(serde_json::to_string_pretty(row).unwrap_or_default());
                    }
                });
                egui::ScrollArea::both().id_salt("rdx_analysis_selected_evidence").auto_shrink([false,false]).show(ui,|ui| {
                    ui.add(egui::Label::new(egui::RichText::new(&self.selected_preview).monospace()).selectable(true)
                        .wrap_mode(egui::TextWrapMode::Extend));
                });
            }
        });
        actions
    }
}

fn bounded(value: String, max: usize) -> String {
    if value.len() <= max {
        return value;
    }
    let mut end = max;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}… [preview truncated; use Copy full record]",
        &value[..end]
    )
}
fn compact(value: &Value) -> String {
    bounded(
        serde_json::to_string(value).unwrap_or_else(|_| "?".into()),
        300,
    )
}
fn display_value(value: &Value) -> String {
    value.as_str().map(str::to_owned).unwrap_or_else(|| {
        if value.is_null() {
            "unknown".into()
        } else {
            compact(value)
        }
    })
}
fn base_url_summary(row: &Value) -> String {
    let candidates: Vec<&str> = row["service_creation_sites"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|site| site["base_url_candidate"].as_str())
        .collect();
    if candidates.is_empty() {
        format!("unknown ({})", display_value(&row["base_url_link_status"]))
    } else {
        format!("candidate: {}", candidates.join(", "))
    }
}
fn row_preview(row: &Value, kind: AnalysisKind) -> (String, String) {
    let owner = row["method_id"]
        .as_str()
        .or_else(|| row["field_id"].as_str())
        .or_else(|| row["class_id"].as_str())
        .unwrap_or("unknown owner");
    let first = if kind == AnalysisKind::Annotations {
        row["annotation_type"].as_str().unwrap_or("annotation")
    } else {
        row["http_verb"].as_str().unwrap_or("HTTP ?")
    };
    let detail = if kind == AnalysisKind::Annotations {
        format!(
            "{}{} · {}",
            row["element"].as_str().unwrap_or(""),
            row["parameter_index"]
                .as_u64()
                .map(|i| format!("[{i}]"))
                .unwrap_or_default(),
            compact(&row["values"])
        )
    } else {
        format!(
            "path {} ({}) · dynamic URL {} · headers {} · query {} · original HTTP annotations {} · creation {} · base URL {}",
            display_value(&row["declared_path"]),
            display_value(&row["path_status"]),
            display_value(&row["dynamic_url_parameter"]),
            compact(&row["header_parameters"]),
            compact(&row["query_parameters"]),
            compact(&row["original_http_annotations"]),
            creation_summary(row),
            base_url_summary(row)
        )
    };
    (
        bounded(format!("{first}  {owner}"), 500),
        bounded(detail, 1000),
    )
}
fn creation_summary(row: &Value) -> String {
    let Some(sites) = row["service_creation_sites"].as_array() else {
        return "unknown".into();
    };
    if sites.is_empty() {
        return "none observed".into();
    }
    format!(
        "{} site(s), first {}",
        sites.len(),
        sites[0]["caller_method_id"]
            .as_str()
            .unwrap_or("unknown caller")
    )
}
fn service_path_summary(row: &Value) -> String {
    bounded(
        format!(
            "{} ({}) · dynamic {}",
            display_value(&row["declared_path"]),
            display_value(&row["path_status"]),
            display_value(&row["dynamic_url_parameter"])
        ),
        400,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn annotation_query_is_exact_and_paged() {
        let w = AnalysisWindow {
            annotation_type: "@Lretrofit2/http/Url;".into(),
            element: "parameter".into(),
            ..Default::default()
        };
        let args = w.query(40).unwrap().arguments();
        assert_eq!(args["annotation_type"], "Lretrofit2/http/Url;");
        assert_eq!(args["element"], "parameter");
        assert_eq!(args["offset"], 40);
        assert_eq!(args["limit"], PAGE_SIZE);
    }
    #[test]
    fn project_change_discards_results_and_selection() {
        let mut w = AnalysisWindow {
            annotation_type: "sample.Annotation".into(),
            ..Default::default()
        };
        w.begin(w.query(0).unwrap());
        w.finish(Ok(
            json!({"items":[{"class_id":"sample.Target"}],"offset":0,"total":1}),
        ));
        w.selected_row = Some(0);
        w.stop_project();
        assert!(w.page.is_none());
        assert!(w.selected_row.is_none());
        assert!(w.active_query.is_none());
    }
    #[test]
    fn owner_navigation_prefers_method() {
        assert_eq!(
            AnalysisWindow::target(
                &json!({"class_id":"sample.Target","method_id":"sample.Target.f()V"})
            ),
            Some((true, "sample.Target.f()V".into()))
        );
        assert_eq!(
            AnalysisWindow::target(&json!({"class_id":"sample.Target"})),
            Some((false, "sample.Target".into()))
        );
    }
    #[test]
    fn cancel_discards_page_and_pagination_uses_backend_cursor() {
        let mut w = AnalysisWindow::default();
        w.open(AnalysisKind::Services);
        w.begin(w.query(0).unwrap());
        w.cancel();
        w.finish(Ok(json!({"items":[{"method_id":"sample.Service.get()V"}],"offset":0,"total":2,"next_offset":1})));
        assert!(w.page.is_none());
        assert_eq!(w.status, "Query cancelled.");
        w.begin(w.query(0).unwrap());
        w.finish(Ok(json!({"items":[{"method_id":"sample.Service.get()V"}],"offset":0,"total":2,"next_offset":1})));
        assert_eq!(w.page_query(true).unwrap().offset, 1);
        assert!(w.page_query(false).is_none());
        w.begin(w.page_query(true).unwrap());
        w.finish(Ok(json!({"items":[{"method_id":"sample.Service.post()V"}],"offset":1,"total":2,"next_offset":null})));
        assert_eq!(w.page_query(false).unwrap().offset, 0);
    }
    #[test]
    fn absent_type_and_partial_coverage_are_explicit() {
        let mut w = AnalysisWindow {
            annotation_type: "retrofit2.http.Url".into(),
            ..Default::default()
        };
        w.begin(w.query(0).unwrap());
        w.finish(Ok(json!({"items":[],"offset":0,"total":0,"requested_type_present":false,"coverage":{"complete":false}})));
        assert!(w.status.contains("partial coverage"));
        assert!(w.status.contains("annotation type absent"));
    }
    #[test]
    fn service_preview_labels_candidate_and_unknown_urls() {
        let row = json!({"class_id":"sample.Service","method_id":"sample.Service.get()V",
            "http_verb":"GET","declared_path":"users","path_status":"explicit_annotation_value",
            "service_creation_sites":[{"caller_method_id":"sample.Factory.create()V","base_url_candidate":"https://example.test/"}]});
        let (_, detail) = row_preview(&row, AnalysisKind::Services);
        assert!(detail.contains("candidate: https://example.test/"));
        assert!(detail.contains("sample.Factory.create()V"));
        let (_, unknown) = row_preview(
            &json!({"class_id":"sample.Service","http_verb":"GET","base_url_link_status":"creation_site_base_url_unresolved"}),
            AnalysisKind::Services,
        );
        assert!(unknown.contains("unknown (creation_site_base_url_unresolved)"));
        assert!(bounded("a".repeat(20_000), 16_000).len() < 17_000);
    }
    #[test]
    fn menu_mode_switch_and_close_cancel_and_reject_late_pages() {
        let mut w = AnalysisWindow {
            annotation_type: "sample.Marker".into(),
            ..Default::default()
        };
        w.begin(w.query(0).unwrap());
        assert!(w.open(AnalysisKind::Services));
        w.finish(Ok(
            json!({"items":[{"annotation_type":"Lsample/Marker;"}],"offset":0,"total":1}),
        ));
        assert!(w.page.is_none());
        assert_eq!(w.kind, AnalysisKind::Services);
        w.begin(w.query(0).unwrap());
        assert!(w.close());
        w.finish(Ok(
            json!({"items":[{"http_verb":"GET"}],"offset":0,"total":1}),
        ));
        assert!(w.page.is_none());
        assert!(!w.visible);
    }
}
