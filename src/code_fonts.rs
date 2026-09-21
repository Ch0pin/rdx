//! Bundled editor fonts. Each named family retains egui's Unicode fallbacks.
use eframe::egui::{self, FontData, FontDefinitions, FontFamily, FontId};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CodeFont {
    #[default]
    Default,
    JetBrainsMono,
    FiraCode,
    SourceCodePro,
}

impl CodeFont {
    pub const ALL: [Self; 4] = [
        Self::Default,
        Self::JetBrainsMono,
        Self::FiraCode,
        Self::SourceCodePro,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "Default monospace",
            Self::JetBrainsMono => "JetBrains Mono",
            Self::FiraCode => "Fira Code",
            Self::SourceCodePro => "Source Code Pro",
        }
    }

    pub fn preference_key(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::JetBrainsMono => "jetbrains-mono",
            Self::FiraCode => "fira-code",
            Self::SourceCodePro => "source-code-pro",
        }
    }

    pub fn from_preference(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|font| font.preference_key() == value)
    }

    pub fn font_id(self, size: f32) -> FontId {
        FontId::new(size, self.family())
    }

    fn family(self) -> FontFamily {
        match self {
            Self::Default => FontFamily::Monospace,
            _ => FontFamily::Name(format!("rdx-code-{}", self.preference_key()).into()),
        }
    }
}

fn definitions() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let fallback = fonts.families[&FontFamily::Monospace].clone();
    let bundled: [(CodeFont, &'static [u8]); 3] = [
        (
            CodeFont::JetBrainsMono,
            include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
        ),
        (
            CodeFont::FiraCode,
            include_bytes!("../assets/fonts/FiraCode-Regular.ttf"),
        ),
        (
            CodeFont::SourceCodePro,
            include_bytes!("../assets/fonts/SourceCodePro-Regular.ttf"),
        ),
    ];
    for (font, bytes) in bundled {
        let name = font.preference_key().to_owned();
        fonts
            .font_data
            .insert(name.clone(), FontData::from_static(bytes).into());
        let mut members = vec![name];
        members.extend(fallback.iter().cloned());
        fonts.families.insert(font.family(), members);
    }
    fonts
}

/// Install once during application creation, before the first frame is rendered.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(definitions());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_round_trip_and_unknown_values_are_rejected() {
        for font in CodeFont::ALL {
            assert_eq!(CodeFont::from_preference(font.preference_key()), Some(font));
        }
        assert_eq!(CodeFont::from_preference("missing-font"), None);
        assert_eq!(CodeFont::default(), CodeFont::Default);
    }

    #[test]
    fn custom_families_retain_default_unicode_fallbacks() {
        let fonts = definitions();
        let fallback = &fonts.families[&FontFamily::Monospace];
        for font in CodeFont::ALL.into_iter().skip(1) {
            let members = &fonts.families[&font.family()];
            assert_eq!(members[0], font.preference_key());
            assert_eq!(&members[1..], fallback.as_slice());
        }
    }

    #[test]
    fn bundled_fonts_parse_and_layout_monospace_source() {
        let ctx = egui::Context::default();
        install(&ctx);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            ctx.fonts(|fonts| {
                for font in CodeFont::ALL {
                    let id = font.font_id(16.0);
                    assert!(fonts.has_glyphs(&id, "public int café_λ = 42;"));
                    assert!(
                        (fonts.glyph_width(&id, 'i') - fonts.glyph_width(&id, 'W')).abs() < 0.01
                    );
                    let galley = fonts.layout_no_wrap(
                        "public int café_λ = 42;".into(),
                        id,
                        egui::Color32::WHITE,
                    );
                    assert!(galley.size().x > 0.0 && galley.size().y > 0.0);
                    assert_eq!(galley.rows.len(), 1);
                }
            });
        });
    }
}
