//! Human-readable plugin presentation in an independent desktop viewport.
use eframe::egui;
use serde_json::Value;

pub fn manager_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("rdx_plugin_manager")
}
pub fn results_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("rdx_plugin_results")
}
#[derive(Default)]
pub struct ResultsWindow {
    pub visible: bool,
    pub previous_offsets: Vec<usize>,
}
#[derive(Default)]
pub struct Actions {
    pub method: Option<String>,
    pub next: Option<usize>,
    pub previous: bool,
    pub cancel: bool,
}
impl ResultsWindow {
    pub fn open(&mut self) {
        self.visible = true;
        self.previous_offsets.clear();
    }

    pub fn show(&mut self, ctx: &egui::Context, page: &Value) -> Actions {
        if !self.visible {
            return Actions::default();
        }
        ctx.show_viewport_immediate(
            results_id(),
            egui::ViewportBuilder::default()
                .with_title("RDX — Plugin results")
                .with_inner_size([900.0, 700.0])
                .with_min_inner_size([500.0, 320.0]),
            |ctx, _| self.contents(ctx, page),
        )
    }
    fn contents(&mut self, ctx: &egui::Context, page: &Value) -> Actions {
        let mut actions = Actions::default();
        if ctx.input(|i| i.viewport().close_requested()) {
            self.visible = false;
            return actions;
        }
        egui::TopBottomPanel::bottom("plugin_results_footer").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                actions.previous = ui
                    .add_enabled(
                        page["offset"].as_u64().unwrap_or(0) > 0,
                        egui::Button::new("Previous page"),
                    )
                    .clicked();
                if let Some(next) = page["next_offset"].as_u64()
                    && ui.button("Next page").clicked()
                {
                    actions.next = Some(next as usize);
                }
                if ui.button("Copy JSON for an agent").clicked() {
                    ctx.copy_text(serde_json::to_string_pretty(page).unwrap_or_default());
                }
            });
        });
        egui::CentralPanel::default().show(ctx,|ui| {
            if page["status"]=="running" {
                ui.heading("Analyzing the loaded project…");ui.spinner();
                ui.label("You can keep browsing source while the analysis runs.");
                actions.cancel=ui.button("Cancel analysis").clicked();
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
                ctx.request_repaint_after_for(std::time::Duration::from_millis(200),egui::ViewportId::ROOT);
                return;
            }
            if page["status"]=="cancelled" {ui.heading("Analysis cancelled");return;}
            if let Some(error)=page["error"].as_str() {ui.heading("Analysis could not finish");ui.label(error);return;}
            let Some(result)=page.get("result") else {ui.label("Select an analysis from the Plugins window.");return;};
            ui.heading(result["title"].as_str().unwrap_or("Analysis results"));
            if let Some(summary)=result["summary"].as_str() {ui.label(summary);}
            if let Some(note)=result["human_note"].as_str() {ui.weak(note);}
            if let Some(note)=result["scan_note"].as_str() {ui.label(note);}
            ui.separator();
            egui::ScrollArea::vertical().id_salt("plugin_human_results").auto_shrink([false,false]).show(ui,|ui| {
                if let Some(items)=result["items"].as_array() {
                    if items.is_empty() {ui.label("No matching references were found in the inspected scope. See Scan details for coverage.");}
                    for (index,item) in items.iter().enumerate() {
                        ui.push_id(item["group_id"].as_str().unwrap_or("item"),|ui| {
                            egui::Frame::group(ui.style()).show(ui,|ui| {
                                ui.set_width(ui.available_width());
                                let human=&item["human"];
                                ui.label(egui::RichText::new(format!("{}. {}",page["offset"].as_u64().unwrap_or(0)+index as u64+1,
                                    human["title"].as_str().or_else(||item["label"].as_str()).unwrap_or("Analysis item")))
                                    .strong().color(ui.visuals().hyperlink_color));
                                if let Some(explanation)=human["explanation"].as_str().or_else(||item["description"].as_str()) {ui.label(explanation);}
                                ui.add_space(5.0);
                                if let Some(facts)=human["observations"].as_array().or_else(||item["facts"].as_array()) {
                                    for fact in facts {
                                        ui.horizontal_wrapped(|ui| {ui.strong(fact["label"].as_str().unwrap_or(""));ui.label(fact["value"].as_str().unwrap_or(""));});
                                    }
                                }
                                ui.collapsing("Show code locations",|ui| {
                                    if let Some(locations)=human["locations"].as_array() {
                                        for location in locations {
                                            if let Some(method)=location["method_id"].as_str()
                                                && ui.link(location["label"].as_str().unwrap_or(method)).on_hover_text(method).clicked() {actions.method=Some(method.into());}
                                        }
                                    } else if let Some(method)=item["method_id"].as_str() && ui.link(method).clicked() {actions.method=Some(method.into());}
                                });
                                ui.collapsing("Technical evidence",|ui| {
                                    egui::ScrollArea::horizontal().id_salt("evidence_horizontal").show(ui,|ui| {ui.monospace(serde_json::to_string_pretty(item).unwrap_or_default());});
                                });
                            });
                            ui.add_space(8.0);
                        });
                    }
                }
                ui.collapsing("Scan details",|ui| {
                    ui.label("These limits describe what the analysis checked. A missing connection means it was not established by this scan.");
                    egui::ScrollArea::horizontal().id_salt("coverage_horizontal").show(ui,|ui| {ui.monospace(serde_json::to_string_pretty(&result["coverage"]).unwrap_or_default());});
                });
            });
        });
        if actions.method.is_some() {
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_windows_have_distinct_ids_and_closing_preserves_results() {
        assert_ne!(manager_id(), egui::ViewportId::ROOT);
        assert_ne!(results_id(), egui::ViewportId::ROOT);
        assert_ne!(manager_id(), results_id());
        assert_ne!(
            results_id(),
            crate::search_window::SearchWindow::viewport_id()
        );
        let ctx = egui::Context::default();
        let mut window = ResultsWindow {
            visible: true,
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
            let a = window.contents(ctx, &serde_json::json!({"status":"complete"}));
            assert!(!a.cancel);
        });
        assert!(!window.visible);
    }
    #[test]
    fn cards_render_human_text_without_raw_terms_by_default() {
        let ctx = egui::Context::default();
        let mut window = ResultsWindow {
            visible: true,
            ..Default::default()
        };
        let page = serde_json::json!({"status":"complete","offset":0,"result":{"title":"WebView inventory","items":[{"group_id":"x","label":"call_result","human":{"title":"demo.Screen.create()","explanation":"The app uses the returned WebView.","observations":[{"label":"Creation","value":"Not determined from the inspected code"}]},"receiver_origin":{"kind":"unresolved"}}]}});
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            window.contents(ctx, &page);
        });
        fn text(shape: &egui::epaint::Shape, out: &mut String) {
            match shape {
                egui::epaint::Shape::Text(t) => out.push_str(t.galley.text()),
                egui::epaint::Shape::Vec(v) => {
                    for s in v {
                        text(s, out)
                    }
                }
                _ => {}
            }
        }
        let mut labels = String::new();
        for s in output.shapes {
            text(&s.shape, &mut labels);
        }
        assert!(labels.contains("demo.Screen.create()"), "{labels}");
        assert!(!labels.contains("call_result"), "{labels}");
        assert!(!labels.contains("unresolved"), "{labels}");
    }
}
