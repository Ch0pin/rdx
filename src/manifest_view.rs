//! Interactive manifest declarations, backed by the same parser as the report.
use eframe::egui;
use rdx::manifest_summary::{self, ManifestComponent};

pub struct ManifestView {
    rows: Result<Vec<ManifestComponent>, String>,
    components: bool,
    kind: String,
    query: String,
    only_exported: bool,
    limit: usize,
}
#[derive(Default)]
pub struct Action {
    pub class: Option<String>,
    pub summary: bool,
    pub show_xml: bool,
}
impl ManifestView {
    pub fn new(xml: &str) -> Self {
        Self {
            rows: manifest_summary::components(xml),
            components: false,
            kind: "All components".into(),
            query: String::new(),
            only_exported: false,
            limit: 100,
        }
    }
    pub fn select_xml(&mut self) {
        self.components = false;
    }
    pub fn retained_bytes(&self) -> usize {
        self.rows
            .as_ref()
            .map(|rows| {
                rows.iter()
                    .map(|r| {
                        std::mem::size_of::<ManifestComponent>()
                            + r.kind.len()
                            + r.name.len()
                            + r.target.len()
                            + r.exported.len()
                            + r.enabled.len()
                            + r.filters.iter().map(String::len).sum::<usize>()
                            + r.details
                                .iter()
                                .map(|(k, v)| k.len() + v.len())
                                .sum::<usize>()
                    })
                    .sum()
            })
            .unwrap_or(0)
    }
    pub fn show(&mut self, ui: &mut egui::Ui, can_open: bool) -> Action {
        let mut action = Action::default();
        ui.horizontal(|ui| {
            action.summary = ui.button("Summary").clicked();
            ui.selectable_value(&mut self.components, true, "Components");
            ui.selectable_value(&mut self.components, false, "XML");
        });
        action.show_xml = !self.components;
        if !self.components {
            return action;
        }
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            let before = (self.kind.clone(), self.only_exported, self.query.clone());
            egui::ComboBox::from_id_salt("component-kind")
                .selected_text(&self.kind)
                .show_ui(ui, |ui| {
                    for kind in [
                        "All components",
                        "activity",
                        "activity-alias",
                        "service",
                        "receiver",
                        "provider",
                    ] {
                        ui.selectable_value(&mut self.kind, kind.to_owned(), kind);
                    }
                });
            ui.checkbox(&mut self.only_exported, "Only exported");
            ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("Filter by name or target…")
                    .desired_width(260.0),
            );
            if before != (self.kind.clone(), self.only_exported, self.query.clone()) {
                self.limit = 100;
            }
        });
        ui.weak("Android API 17+ · Manifest configuration; runtime state and permission checks are separate.");
        let rows = match &self.rows {
            Ok(rows) => rows,
            Err(error) => {
                ui.label(format!("Cannot read components: {error}"));
                return action;
            }
        };
        let query = self.query.to_lowercase();
        let filtered: Vec<_> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                (self.kind == "All components" || r.kind == self.kind)
                    && (!self.only_exported || r.is_exported())
                    && (r.name.to_lowercase().contains(&query)
                        || r.target.to_lowercase().contains(&query))
            })
            .collect();
        ui.horizontal(|ui| {
            ui.weak(format!(
                "{} of {} matching components · {} total",
                self.limit.min(filtered.len()),
                filtered.len(),
                rows.len()
            ));
            if self.limit < filtered.len() && ui.button("Load more").clicked() {
                self.limit += 100;
            }
        });
        egui::ScrollArea::both().id_salt("manifest-components").show(ui, |ui| {
            let name_width = (ui.available_width() - 670.0).max(330.0);
            let widths = [name_width, 110.0, 95.0, 85.0, 85.0, 235.0];
            let total = widths.iter().sum::<f32>() + ui.spacing().item_spacing.x * 5.0;
            ui.set_min_width(total);
            ui.horizontal_top(|ui| {
                for (label, width) in ["Name", "Type", "Exported", "Enabled", "Launcher", "Intent filters"].into_iter().zip(widths) {
                    cell(ui, width, |ui| { ui.strong(label); });
                }
            });
            ui.separator();
            for (index, row) in filtered.into_iter().take(self.limit) {
                ui.push_id(index, |ui| {
                    let fill = if row.is_exported() {
                        egui::Color32::from_rgba_unmultiplied(45, 155, 85, 28)
                    } else { egui::Color32::TRANSPARENT };
                    egui::Frame::new().fill(fill).show(ui, |ui| {
                        ui.set_min_width(total);
                        ui.horizontal_top(|ui| {
                            cell(ui, widths[0], |ui| {
                                let response = ui.add_enabled(can_open && !row.target.is_empty(), egui::Button::new(&row.name).wrap().frame(false));
                                if response.clicked() { action.class = Some(row.target.clone()); }
                                response.on_hover_text(if row.kind == "activity-alias" { format!("Open target activity: {}", row.target) } else { "Open component class".into() });
                                egui::CollapsingHeader::new("Details").show(ui, |ui| {
                                    for (key, value) in &row.details { ui.label(format!("{key}: {value}")); }
                                });
                            });
                            cell(ui, widths[1], |ui| { ui.label(&row.kind); });
                            cell(ui, widths[2], |ui| {
                                let label = if row.is_exported() { "true" } else if row.exported.starts_with("false") { "false" } else { "unresolved" };
                                ui.label(label).on_hover_text(&row.exported);
                            });
                            cell(ui, widths[3], |ui| { ui.label(&row.enabled).on_hover_text("Combines application and component enabled flags"); });
                            cell(ui, widths[4], |ui| { ui.label(if row.launcher { "yes" } else { "no" }).on_hover_text("MAIN and LAUNCHER / LEANBACK_LAUNCHER in the same declared filter"); });
                            cell(ui, widths[5], |ui| {
                                if row.filters.is_empty() { ui.weak("None"); }
                                for (number, filter) in row.filters.iter().enumerate() {
                                    egui::CollapsingHeader::new(format!("Filter {}", number + 1)).show(ui, |ui| { ui.label(filter); });
                                }
                            });
                        });
                    });
                    ui.separator();
                });
            }
            if rows.is_empty() { ui.label("No components declared in this manifest."); }
        });
        action
    }
}
fn cell(ui: &mut egui::Ui, width: f32, content: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, 0.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_width(width);
            content(ui);
        },
    );
}
