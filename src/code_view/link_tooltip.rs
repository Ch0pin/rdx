use eframe::egui;

/// Retain the hovered link while the pointer enters its interactive tooltip.
/// The anchor stays at the original pointer position, rather than following it.
#[derive(Default)]
pub(super) struct LinkTooltip {
    active: Option<Active>,
    last_pass: u64,
    #[cfg(test)]
    pub copy_rect: Option<egui::Rect>,
    #[cfg(test)]
    pub text_rect: Option<egui::Rect>,
}

struct Active {
    start: usize,
    label: String,
    token: egui::Rect,
    anchor: egui::Rect,
    origin: egui::Pos2,
    popup: Option<egui::Rect>,
}

impl LinkTooltip {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        editor: egui::Id,
        origin: egui::Pos2,
        hovered: Option<(usize, egui::Rect, String)>,
    ) {
        let pass = ui.ctx().cumulative_pass_nr();
        if pass > self.last_pass + 1
            || self.active.as_ref().is_some_and(|a| a.origin != origin)
            || ui.input(|i| {
                i.key_pressed(egui::Key::Escape) || i.raw_scroll_delta != egui::Vec2::ZERO
            })
        {
            self.active = None;
        }
        self.last_pass = pass;
        #[cfg(test)]
        {
            self.copy_rect = None;
            self.text_rect = None;
        }
        let pointer = ui.ctx().pointer_hover_pos();
        // Include the short gap between the anchor and popup so crossing another
        // source token on the way to Copy doesn't replace the tooltip contents.
        let entering_popup = self.active.as_ref().is_some_and(|a| {
            a.popup
                .is_some_and(|rect| pointer.is_some_and(|p| rect.union(a.anchor).contains(p)))
        });
        if !entering_popup
            && let Some((start, token, label)) = hovered
            && let Some(pointer) = pointer
            && self
                .active
                .as_ref()
                .is_none_or(|a| a.start != start || a.popup.is_none())
        {
            self.active = Some(Active {
                start,
                label,
                token,
                anchor: egui::Rect::from_min_max(
                    pointer - egui::vec2(0.0, 8.0),
                    pointer + egui::vec2(1.0, 8.0),
                ),
                origin,
                popup: None,
            });
        }
        let Some(active) = &mut self.active else {
            return;
        };
        let mut response = ui.interact(
            active.token,
            editor.with(("link-tooltip", active.start)),
            egui::Sense::hover(),
        );
        // Hit testing uses the token; popup placement uses the small, fixed
        // pointer anchor. egui places it below, or above near the screen edge.
        response.rect = active.anchor;
        active.popup = None;
        response.on_hover_ui(|ui| {
            ui.set_max_width(ui.spacing().tooltip_width);
            let text = ui.add(egui::Label::new(&active.label).selectable(true).wrap());
            let copy = ui.button("Copy details");
            if copy.clicked() {
                ui.ctx().copy_text(active.label.clone());
            }
            active.popup = Some(ui.min_rect().expand(8.0));
            #[cfg(test)]
            {
                self.copy_rect = Some(copy.rect);
                self.text_rect = Some(text.rect);
            }
            #[cfg(not(test))]
            let _ = text;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;

    #[test]
    fn ordinary_member_and_class_links_keep_navigation_without_resource_popup() {
        for (source, label) in [
            ("thz1._rdx_c783", "thz1._rdx_c783"),
            ("ExampleClass", "com.example.ExampleClass"),
        ] {
            let ctx = egui::Context::default();
            let mut doc = CodeDocument::new(source.into(), "java");
            doc.set_links(vec![CodeLink {
                start: 0,
                end: source.chars().count(),
                label: label.into(),
            }]);
            let render = |doc: &mut CodeDocument, time, events| {
                let mut navigation = None;
                let _ = ctx.run(
                    egui::RawInput {
                        time: Some(time),
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            navigation = doc.show(ui, CodeTheme::Ocean, 14.0);
                        });
                    },
                );
                navigation
            };
            render(&mut doc, 0.0, vec![]);
            let glyph = doc
                .cache
                .as_ref()
                .unwrap()
                .galley
                .pos_from_ccursor(egui::text::CCursor::new(3));
            let pointer = doc.last_galley_pos.unwrap() + glyph.center().to_vec2();
            render(&mut doc, 0.1, vec![egui::Event::PointerMoved(pointer)]);
            for time in [1.0, 1.1, 1.2] {
                render(&mut doc, time, vec![]);
                assert!(doc.link_tooltip.active.is_none(), "{label}");
                assert!(doc.link_tooltip.copy_rect.is_none(), "{label}");
                assert!(doc.link_tooltip.text_rect.is_none(), "{label}");
            }
            let button = |pressed| egui::Event::PointerButton {
                pos: pointer,
                pressed,
                button: egui::PointerButton::Primary,
                modifiers: egui::Modifiers::NONE,
            };
            assert_eq!(render(&mut doc, 1.3, vec![button(true)]), None);
            assert_eq!(render(&mut doc, 1.35, vec![button(false)]), None);
            render(&mut doc, 1.4, vec![button(true)]);
            assert_eq!(render(&mut doc, 1.45, vec![button(false)]), Some(0));
        }
    }

    #[test]
    fn resource_tooltip_stays_near_pointer_and_supports_copy_and_selection() {
        for top_space in [20.0, 330.0] {
            let ctx = egui::Context::default();
            let mut doc = CodeDocument::new("setContentView(R.layout.splash);".into(), "java");
            let details = "com.airbnb.android:layout.splash\nValue: res/layout/splash.xml\nID 2131623992 / 0x7f0e0038";
            doc.set_links(vec![CodeLink {
                start: 15,
                end: 29,
                label: format!("resource://7f0e0038/layout/splash | {details}"),
            }]);
            let render = |doc: &mut CodeDocument, time, events| {
                ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(640.0, 440.0),
                        )),
                        time: Some(time),
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            ui.add_space(top_space);
                            assert_eq!(doc.show(ui, CodeTheme::Ocean, 14.0), None);
                        });
                    },
                )
            };
            render(&mut doc, 0.0, vec![]);
            let glyph = doc
                .cache
                .as_ref()
                .unwrap()
                .galley
                .pos_from_ccursor(egui::text::CCursor::new(18));
            let pointer = doc.last_galley_pos.unwrap() + glyph.center().to_vec2();
            render(&mut doc, 0.1, vec![egui::Event::PointerMoved(pointer)]);
            for time in [1.0, 1.1, 1.2] {
                render(&mut doc, time, vec![]);
            }
            let text = doc.link_tooltip.text_rect.expect("hover must show details");
            let copy = doc.link_tooltip.copy_rect.unwrap();
            assert!(
                (text.left() - pointer.x).abs() < 16.0,
                "{text:?}, {pointer:?}"
            );
            if top_space < 100.0 {
                assert!((0.0..32.0).contains(&(text.top() - pointer.y)));
            } else {
                assert!(
                    (0.0..32.0).contains(&(pointer.y - copy.bottom())),
                    "{copy:?}, {pointer:?}"
                );
            }
            let button = |pos, pressed| egui::Event::PointerButton {
                pos,
                pressed,
                button: egui::PointerButton::Primary,
                modifiers: egui::Modifiers::NONE,
            };
            let pos = copy.center();
            render(&mut doc, 1.3, vec![egui::Event::PointerMoved(pos)]);
            assert_eq!(
                doc.link_tooltip.copy_rect,
                Some(copy),
                "popup must stay fixed while entering it"
            );
            render(&mut doc, 1.4, vec![button(pos, true)]);
            let output = render(&mut doc, 1.5, vec![button(pos, false)]);
            assert!(
                output
                    .platform_output
                    .commands
                    .iter()
                    .any(|cmd| matches!(cmd, egui::OutputCommand::CopyText(s) if s == details))
            );

            let start = text.left_top() + egui::vec2(2.0, 6.0);
            let end = start + egui::vec2(100.0, 0.0);
            render(&mut doc, 1.6, vec![egui::Event::PointerMoved(start)]);
            render(&mut doc, 1.7, vec![button(start, true)]);
            render(&mut doc, 1.8, vec![egui::Event::PointerMoved(end)]);
            render(&mut doc, 1.9, vec![button(end, false)]);
            let output = render(&mut doc, 2.0, vec![egui::Event::Copy]);
            assert!(output.platform_output.commands.iter().any(|cmd| matches!(cmd, egui::OutputCommand::CopyText(s) if !s.is_empty() && details.contains(s))), "selected text should copy: {:?}", output.platform_output.commands);
            render(
                &mut doc,
                2.1,
                vec![egui::Event::PointerMoved(egui::pos2(620.0, 20.0))],
            );
            render(&mut doc, 2.2, vec![]);
            assert!(
                doc.link_tooltip.copy_rect.is_none(),
                "leaving tooltip dismisses it"
            );
        }
    }
}
