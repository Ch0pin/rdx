//! Persistent preferences in an independent native window.
use crate::{code_fonts::CodeFont, code_view::CodeTheme};
use eframe::egui;

pub fn viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("rdx_preferences")
}

#[derive(Default, PartialEq, Eq, Clone, Copy)]
enum Tab {
    #[default]
    Appearance,
    Decompilation,
    Scanning,
    General,
}

#[derive(Default)]
pub struct PreferencesWindow {
    pub visible: bool,
    pub adjusting: bool,
    tab: Tab,
}

pub struct Controls<'a> {
    pub theme: &'a mut CodeTheme,
    pub font: &'a mut CodeFont,
    pub font_size: &'a mut f32,
    pub fallback: &'a mut bool,
    pub workers: &'a mut usize,
    pub tabs_auto_hide: &'a mut bool,
    pub menu_auto_hide: &'a mut bool,
    pub busy: bool,
}

impl PreferencesWindow {
    pub fn show(&mut self, ctx: &egui::Context, mut controls: Controls<'_>) {
        if !self.visible {
            return;
        }
        ctx.show_viewport_immediate(
            viewport_id(),
            egui::ViewportBuilder::default()
                .with_title("RDX — Preferences")
                .with_inner_size([640.0, 410.0])
                .with_min_inner_size([520.0, 320.0]),
            |ctx, _| self.contents(ctx, &mut controls),
        );
    }

    fn contents(&mut self, ctx: &egui::Context, controls: &mut Controls<'_>) {
        if ctx.input(|i| i.viewport().close_requested()) {
            self.visible = false;
            self.adjusting = false;
            ctx.request_repaint_of(egui::ViewportId::ROOT);
            return;
        }
        let was_adjusting = self.adjusting;
        self.adjusting = ctx.input(|i| i.pointer.primary_down());
        let before = (
            *controls.theme,
            *controls.font,
            *controls.font_size,
            *controls.fallback,
            *controls.workers,
            *controls.tabs_auto_hide,
            *controls.menu_auto_hide,
            ctx.options(|o| o.theme_preference),
        );
        egui::TopBottomPanel::top("preferences_tabs").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                crate::icons::small(ui, crate::icons::Icon::Settings);
                ui.strong("Preferences");
            });
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                for (tab, label) in [
                    (Tab::Appearance, "Appearance"),
                    (Tab::Decompilation, "Decompilation"),
                    (Tab::Scanning, "Search & Scanning"),
                    (Tab::General, "General"),
                ] {
                    ui.selectable_value(&mut self.tab, tab, label);
                }
            });
            ui.add_space(8.0);
        });
        egui::TopBottomPanel::bottom("preferences_footer").show(ctx, |ui| {
            ui.add_space(5.0);
            ui.weak("Changes save automatically.");
            ui.add_space(5.0);
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(8.0);
                match self.tab {
                    Tab::Appearance => {
                        ui.heading("Appearance");
                        ui.weak("Changes apply immediately to open views.");
                        ui.add_space(12.0);
                        egui::Grid::new("appearance_preferences")
                            .spacing([24.0, 14.0])
                            .show(ui, |ui| {
                                ui.label("Interface theme");
                                let mut appearance = ctx.options(|o| o.theme_preference);
                                egui::ComboBox::from_id_salt("interface_theme")
                                    .selected_text(match appearance {
                                        egui::ThemePreference::System => "System",
                                        egui::ThemePreference::Light => "Light",
                                        egui::ThemePreference::Dark => "Dark",
                                    })
                                    .show_ui(ui, |ui| {
                                        for (value, label) in [
                                            (egui::ThemePreference::System, "System"),
                                            (egui::ThemePreference::Light, "Light"),
                                            (egui::ThemePreference::Dark, "Dark"),
                                        ] {
                                            if ui.selectable_value(&mut appearance, value, label).changed() {
                                                ctx.set_theme(appearance);
                                            }
                                        }
                                    });
                                ui.end_row();
                                ui.label("Code theme");
                                egui::ComboBox::from_id_salt("code_theme")
                                    .selected_text(controls.theme.label())
                                    .show_ui(ui, |ui| {
                                        for (light, label) in [(true, "Light"), (false, "Dark")] {
                                            ui.weak(label);
                                            for theme in CodeTheme::ALL.into_iter().filter(|t| t.is_light() == light) {
                                                ui.selectable_value(controls.theme, theme, theme.label());
                                            }
                                        }
                                    });
                                ui.end_row();
                                ui.label("Code font");
                                egui::ComboBox::from_id_salt("code_font")
                                    .selected_text(controls.font.label())
                                    .show_ui(ui, |ui| {
                                        for font in CodeFont::ALL {
                                            ui.selectable_value(controls.font, font, font.label());
                                        }
                                    });
                                ui.end_row();
                                ui.label("Code font size");
                                ui.add(egui::DragValue::new(controls.font_size).range(10.0..=28.0).speed(0.5).suffix(" px"));
                                ui.end_row();
                            });
                    }
                    Tab::Decompilation => {
                        ui.heading("Decompilation");
                        ui.add_space(12.0);
                        ui.add_enabled(!controls.busy, egui::Checkbox::new(controls.fallback, "Androguard fallback"))
                            .on_hover_ui(|ui| {
                                ui.set_max_width(400.0);
                                ui.label(rdx::alternate_source::availability());
                                ui.label("Automatically tries Androguard for failed methods while browsing classes. Each attempt stops after 30 seconds.");
                                if let Some(path) = rdx::alternate_source::helper_path() {
                                    ui.small(format!("Decompiler executable: {}", path.display()));
                                }
                            });
                        ui.weak("Changing fallback refreshes the current class.");
                        if controls.busy {
                            ui.weak("Available when the current loading operation finishes.");
                        }
                    }
                    Tab::Scanning => {
                        ui.heading("Search & Scanning");
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            ui.label("Code search workers");
                            ui.add(crate::ui::search_workers_control(controls.workers));
                        });
                        ui.weak(format!("Auto uses {} workers on this machine. Applies to the next search.", rdx::search::resolve_workers(0)));
                        ui.add_space(8.0);
                        ui.label("More workers can speed up code searches while using more CPU and memory.");
                    }
                    Tab::General => {
                        ui.heading("General");
                        ui.weak("Changes apply immediately.");
                        ui.add_space(12.0);
                        ui.checkbox(controls.tabs_auto_hide, "Autohide tabs");
                        ui.checkbox(controls.menu_auto_hide, "Autohide menu");
                        ui.weak("Move the pointer to the top of the main window to reveal hidden controls.");
                    }
                }
            });
        });
        // Preference edits must redraw the main code view as well as this window.
        let after = (
            *controls.theme,
            *controls.font,
            *controls.font_size,
            *controls.fallback,
            *controls.workers,
            *controls.tabs_auto_hide,
            *controls.menu_auto_hide,
            ctx.options(|o| o.theme_preference),
        );
        if before != after || was_adjusting != self.adjusting {
            ctx.request_repaint_of(egui::ViewportId::ROOT);
        }
    }
}
