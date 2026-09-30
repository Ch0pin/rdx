use eframe::egui;
use std::time::{Duration, SystemTime};

#[derive(Default)]
pub struct RenderRecovery {
    last_frame: Option<SystemTime>,
    was_focused: Option<bool>,
}

impl RenderRecovery {
    pub fn update(&mut self, ctx: &egui::Context) {
        self.update_at(ctx, SystemTime::now());
    }

    fn update_at(&mut self, ctx: &egui::Context, now: SystemTime) {
        let focused = ctx.input(|input| input.focused);
        // Wall time includes laptop sleep on platforms where Instant does not.
        let long_pause = self.last_frame.is_some_and(|last| {
            now.duration_since(last)
                .is_ok_and(|elapsed| elapsed >= Duration::from_secs(60))
        });
        let refocused = self.was_focused == Some(false) && focused;
        self.last_frame = Some(now);
        self.was_focused = Some(focused);
        if long_pause || refocused {
            // Re-upload the CPU copy even if no new glyphs were added. This
            // repairs a stale GPU font texture without changing text or tabs.
            let image = ctx.fonts(|fonts| fonts.image());
            ctx.tex_manager().write().set(
                egui::TextureId::default(),
                egui::epaint::ImageDelta::full(
                    image,
                    egui::epaint::TextureAtlas::texture_options(),
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_full_font_texture_after_pause_or_refocus_only() {
        let ctx = egui::Context::default();
        let mut recovery = RenderRecovery::default();
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let mut frame = |seconds, focused| {
            ctx.run(
                egui::RawInput {
                    focused,
                    ..Default::default()
                },
                |ctx| {
                    recovery.update_at(ctx, start + Duration::from_secs(seconds));
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.label("Already cached glyphs");
                    });
                },
            )
        };
        let full_upload = |output: &egui::FullOutput| {
            output
                .textures_delta
                .set
                .iter()
                .any(|(id, delta)| *id == egui::TextureId::default() && delta.pos.is_none())
        };
        frame(0, true);
        assert!(!full_upload(&frame(1, true)));
        assert!(!full_upload(&frame(2, false)));
        assert!(full_upload(&frame(3, true)));
        assert!(!full_upload(&frame(4, true)));
        assert!(full_upload(&frame(3600, true)));
        assert!(!full_upload(&frame(3601, true)));
        // A backwards wall-clock correction is not a wake event.
        assert!(!full_upload(&frame(3500, true)));
    }
}
