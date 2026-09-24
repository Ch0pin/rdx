//! Bounded, selectable source previews. Original source is never normalized or edited.
use crate::code_fonts::CodeFont;
use rdx::engine::CodeLink;
mod palettes;
use std::{
    ops::Range,
    sync::{Arc, OnceLock},
};

use eframe::egui::{self, Color32, FontId, TextFormat, text::LayoutJob};
use syntect::{
    easy::HighlightLines, highlighting::ThemeSet, parsing::SyntaxSet, util::LinesWithEndings,
};

const LONG_LINE: usize = 4_000;

fn exported_component_tint(theme: CodeTheme) -> Color32 {
    if theme.is_light() {
        Color32::from_rgba_unmultiplied(130, 65, 195, 45)
    } else {
        Color32::from_rgba_unmultiplied(190, 145, 255, 65)
    }
}

fn jump_accent(theme: CodeTheme) -> Color32 {
    if theme.is_light() {
        Color32::from_rgb(38, 91, 161)
    } else {
        Color32::from_rgb(125, 185, 245)
    }
}
fn jump_gutter_tint(theme: CodeTheme) -> Color32 {
    if theme.is_light() {
        Color32::from_rgba_unmultiplied(220, 70, 70, 40)
    } else {
        Color32::from_rgba_unmultiplied(255, 135, 135, 55)
    }
}
fn jump_tint(theme: CodeTheme) -> Color32 {
    let c = jump_accent(theme);
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 48)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CodeTheme {
    #[default]
    Ocean,
    Eighties,
    SolarizedDark,
    SolarizedLight,
    AtomOneLight,
    QuietLight,
    OneDark,
    Dracula,
}
impl CodeTheme {
    pub const ALL: [Self; 8] = [
        Self::Ocean,
        Self::Eighties,
        Self::SolarizedDark,
        Self::SolarizedLight,
        Self::AtomOneLight,
        Self::QuietLight,
        Self::OneDark,
        Self::Dracula,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Ocean => "Ocean dark",
            Self::Eighties => "Eighties dark",
            Self::SolarizedDark => "Solarized dark",
            Self::SolarizedLight => "Solarized light",
            Self::AtomOneLight => "Atom One Light",
            Self::QuietLight => "Quiet Light",
            Self::OneDark => "One Dark",
            Self::Dracula => "Dracula",
        }
    }
    pub fn is_light(self) -> bool {
        matches!(
            self,
            Self::SolarizedLight | Self::AtomOneLight | Self::QuietLight
        )
    }
    pub fn preference_key(self) -> &'static str {
        match self {
            Self::Ocean => "ocean",
            Self::Eighties => "eighties",
            Self::SolarizedDark => "solarized_dark",
            Self::SolarizedLight => "solarized_light",
            Self::AtomOneLight => "atom-one-light",
            Self::QuietLight => "quiet-light",
            Self::OneDark => "one-dark",
            Self::Dracula => "dracula",
        }
    }
    pub fn from_preference(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|theme| theme.preference_key() == key)
    }
    fn key(self) -> &'static str {
        match self {
            Self::Ocean => "base16-ocean.dark",
            Self::Eighties => "base16-eighties.dark",
            Self::SolarizedDark => "Solarized (dark)",
            Self::SolarizedLight => "Solarized (light)",
            Self::AtomOneLight => "Atom One Light",
            Self::QuietLight => "Quiet Light",
            Self::OneDark => "One Dark",
            Self::Dracula => "Dracula",
        }
    }
}
fn syntax_set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}
fn themes() -> &'static ThemeSet {
    static SET: OnceLock<ThemeSet> = OnceLock::new();
    SET.get_or_init(|| {
        let mut set = ThemeSet::load_defaults();
        palettes::extend(&mut set);
        set
    })
}
fn color(c: syntect::highlighting::Color) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}

/// Syntax-colored, bounded one-line search preview with an exact UTF-8 match range.
pub fn search_preview(text: &str, syntax: &str, matched: Range<usize>, dark: bool) -> LayoutJob {
    let set = syntax_set();
    let theme = if dark {
        CodeTheme::Ocean
    } else {
        CodeTheme::SolarizedLight
    };
    let palette = &themes().themes[theme.key()];
    let syntax = set
        .find_syntax_by_extension(syntax)
        .unwrap_or_else(|| set.find_syntax_plain_text());
    let mut highlighter = HighlightLines::new(syntax, palette);
    let parts = highlighter.highlight_line(text, set).unwrap_or_default();
    let mut job = LayoutJob::default();
    let mut offset = 0;
    for (style, part) in parts {
        let end = offset + part.len();
        let mut cuts = vec![offset, end];
        for point in [matched.start, matched.end] {
            if point > offset && point < end && text.is_char_boundary(point) {
                cuts.push(point);
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        for pair in cuts.windows(2) {
            let selected = pair[0] >= matched.start && pair[0] < matched.end;
            job.append(
                &text[pair[0]..pair[1]],
                0.0,
                TextFormat {
                    font_id: FontId::monospace(13.0),
                    color: if selected {
                        Color32::BLACK
                    } else {
                        color(style.foreground)
                    },
                    background: if selected {
                        Color32::from_rgb(255, 210, 65)
                    } else {
                        Color32::TRANSPARENT
                    },
                    ..Default::default()
                },
            );
        }
        offset = end;
    }
    job
}

struct Cache {
    theme: CodeTheme,
    size: f32,
    pixels_per_point: f32,
    galley: Arc<egui::Galley>,
    retained_bytes: usize,
    wrap_width: f32,
}

#[derive(Default)]
struct FileFind {
    open: bool,
    focus: bool,
    query: String,
    case_sensitive: bool,
    pattern: Option<regex::Regex>,
    total: usize,
    current: Option<(usize, Range<usize>)>, // byte offsets; converted only for display/navigation
}

pub struct CodeDocument {
    selection_key: Option<(usize, usize, Option<Range<usize>>)>,
    selection_occurrences: Vec<Range<usize>>,
    clicked_word: Option<(usize, Range<usize>)>,
    find: FileFind,
    word_wrap: bool,
    code_font: CodeFont,
    source: String,
    syntax: String,
    preview_end: usize,
    preview_start: usize,
    char_start: usize,
    line_start: usize,
    links: Vec<CodeLink>,
    exported_components: Vec<Range<usize>>,
    jump: Option<usize>,
    navigation_position: usize,
    last_navigation_cursor: Option<usize>,
    highlight: Option<Range<usize>>,
    export_requested: bool,
    usages_requested: Option<usize>,
    subclasses_requested: Option<usize>,
    implementations_requested: Option<usize>,
    call_graph_requested: Option<usize>,
    method_xrefs_requested: Option<(usize, bool)>,
    usages_enabled: bool,
    menu_link: Option<CodeLink>,
    menu_selection: Option<Range<usize>>,
    #[cfg(test)]
    menu_items: Vec<(String, egui::Rect, bool)>,
    #[cfg(test)]
    last_galley_pos: Option<egui::Pos2>,
    #[cfg(test)]
    last_editor_id: Option<egui::Id>,
    plain: bool,
    cache: Option<Cache>,
}
impl CodeDocument {
    pub fn new(text: String, syntax: &str) -> Self {
        let end = text.len();
        let plain = text[..end].split('\n').any(|line| line.len() > LONG_LINE);
        Self {
            selection_key: None,
            selection_occurrences: Vec::new(),
            clicked_word: None,
            find: FileFind::default(),
            word_wrap: false,
            code_font: CodeFont::default(),
            source: text,
            syntax: syntax.to_owned(),
            preview_end: end,
            preview_start: 0,
            char_start: 0,
            line_start: 1,
            links: Vec::new(),
            exported_components: Vec::new(),
            jump: None,
            navigation_position: 0,
            last_navigation_cursor: None,
            highlight: None,
            export_requested: false,
            usages_requested: None,
            subclasses_requested: None,
            implementations_requested: None,
            call_graph_requested: None,
            method_xrefs_requested: None,
            usages_enabled: true,
            menu_link: None,
            menu_selection: None,
            #[cfg(test)]
            menu_items: Vec::new(),
            #[cfg(test)]
            last_galley_pos: None,
            #[cfg(test)]
            last_editor_id: None,
            plain,
            cache: None,
        }
    }
    pub fn open_find(&mut self) {
        self.find.open = true;
        self.find.focus = true;
        if self.find.current.is_some() {
            self.reveal_find();
        }
    }

    fn update_selection_occurrences(&mut self, selected: Option<Range<usize>>) {
        let key = (self.preview_start, self.preview_end, selected);
        if self.selection_key.as_ref() == Some(&key) {
            return;
        }
        let preview = &self.source[self.preview_start..self.preview_end];
        self.selection_occurrences = key
            .2
            .clone()
            .and_then(|range| crate::word_occurrences::selected_word(preview, range))
            .map_or_else(Vec::new, |word| {
                crate::word_occurrences::occurrences(preview, &word)
            });
        self.selection_key = Some(key);
    }

    pub fn set_code_font(&mut self, font: CodeFont) {
        if self.code_font != font {
            self.code_font = font;
            self.cache = None;
        }
    }
    pub fn set_word_wrap(&mut self, enabled: bool) {
        if self.word_wrap != enabled {
            self.word_wrap = enabled;
            self.cache = None;
        }
    }

    fn update_find(&mut self) {
        self.find.pattern = if self.find.query.is_empty() {
            None
        } else {
            regex::RegexBuilder::new(&regex::escape(&self.find.query))
                .case_insensitive(!self.find.case_sensitive)
                .build()
                .ok()
        };
        self.find.total = 0;
        self.find.current = None;
        if let Some(pattern) = &self.find.pattern {
            // Scan the entire source, not just the bounded visible window. Keep
            // only one result so a common letter cannot allocate millions of ranges.
            for matched in pattern.find_iter(&self.source) {
                if self.find.current.is_none() {
                    self.find.current = Some((0, matched.range()));
                }
                self.find.total += 1;
            }
        }
        self.reveal_find();
    }

    fn reveal_find(&mut self) {
        if let Some((_, matched)) = &self.find.current {
            let start = self.source[..matched.start].chars().count();
            let end = start + self.source[matched.clone()].chars().count();
            let _ = self.jump_to_range(start, end);
        } else {
            self.highlight = None;
        }
    }

    fn step_find(&mut self, backwards: bool) {
        let (Some(pattern), Some((index, current))) = (&self.find.pattern, &self.find.current)
        else {
            return;
        };
        let next = if backwards {
            pattern
                .find_iter(&self.source)
                .take_while(|m| m.start() < current.start)
                .last()
                .map(|m| (index.saturating_sub(1), m.range()))
                .or_else(|| {
                    pattern
                        .find_iter(&self.source)
                        .last()
                        .map(|m| (self.find.total - 1, m.range()))
                })
        } else {
            pattern
                .find_at(&self.source, current.end)
                .map(|m| (index + 1, m.range()))
                .or_else(|| pattern.find(&self.source).map(|m| (0, m.range())))
        };
        self.find.current = next;
        self.reveal_find();
    }

    fn find_bar(&mut self, ui: &mut egui::Ui) {
        if !self.find.open {
            return;
        }
        let mut previous = false;
        let mut next = false;
        let mut close = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        ui.horizontal_wrapped(|ui| {
            ui.label("Find in file");
            let id = ui.make_persistent_id("find_in_file_query");
            if ui.memory(|m| m.has_focus(id)) {
                previous =
                    ui.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::Enter));
                next = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
            }
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.find.query)
                    .id(id)
                    .hint_text("Text in this file…")
                    .desired_width(220.0),
            );
            if self.find.focus {
                response.request_focus();
                self.find.focus = false;
            }
            let changed = ui
                .checkbox(&mut self.find.case_sensitive, "Match case")
                .changed();
            if response.changed() || changed {
                self.update_find();
            }
            let position = self.find.current.as_ref().map_or(0, |(i, _)| i + 1);
            ui.label(format!("{position} / {}", self.find.total));
            previous |= ui
                .add_enabled(self.find.total > 0, egui::Button::new("Previous"))
                .clicked();
            next |= ui
                .add_enabled(self.find.total > 0, egui::Button::new("Next"))
                .clicked();
            close |= ui.button("Close").clicked();
        });
        if previous {
            self.step_find(true);
        }
        if next {
            self.step_find(false);
        }
        if close {
            self.find.open = false;
            self.highlight = None;
        }
        ui.separator();
    }
    pub fn take_export_request(&mut self) -> bool {
        std::mem::take(&mut self.export_requested)
    }
    pub fn take_usages_request(&mut self) -> Option<usize> {
        self.usages_requested.take()
    }
    pub fn take_method_xrefs_request(&mut self) -> Option<(usize, bool)> {
        self.method_xrefs_requested.take()
    }
    pub fn take_call_graph_request(&mut self) -> Option<usize> {
        self.call_graph_requested.take()
    }
    pub fn take_implementations_request(&mut self) -> Option<usize> {
        self.implementations_requested.take()
    }
    pub fn take_subclasses_request(&mut self) -> Option<usize> {
        self.subclasses_requested.take()
    }
    pub fn set_usages_enabled(&mut self, enabled: bool) {
        self.usages_enabled = enabled;
    }
    pub fn link_at(&self, position: usize) -> Option<&CodeLink> {
        self.links
            .iter()
            .find(|link| link.start <= position && position < link.end)
    }

    pub fn resource_links(&self) -> Vec<CodeLink> {
        self.links
            .iter()
            .filter(|link| link.label.starts_with(rdx::resource_table::PREFIX))
            .cloned()
            .collect()
    }
    pub fn set_links(&mut self, mut links: Vec<CodeLink>) {
        let count = self.source.chars().count();
        links.retain(|link| link.start < link.end && link.end <= count);
        links.sort_by_key(|link| link.start);
        self.links = links;
    }

    pub fn set_exported_components(&mut self, mut ranges: Vec<Range<usize>>) {
        let count = self.source.chars().count();
        ranges.retain(|range| range.start < range.end && range.end <= count);
        ranges.sort_by_key(|range| (range.start, range.end));
        ranges.dedup();
        self.exported_components = ranges;
    }

    /// Replace a stale search snapshot with the annotated current source. Never
    /// transfer old offsets unless the entire matched line block is unique.
    pub fn refresh_search_source(&mut self, source: String, links: Vec<CodeLink>) -> bool {
        let relocated = self.highlight.as_ref().and_then(|range| {
            let start = self.source.char_indices().nth(range.start)?.0;
            let end = if range.end == self.source.chars().count() {
                self.source.len()
            } else {
                self.source.char_indices().nth(range.end)?.0
            };
            let line_start = self.source[..start].rfind('\n').map_or(0, |i| i + 1);
            let line_end = self.source[end..]
                .find('\n')
                .map_or(self.source.len(), |i| end + i);
            let anchor = &self.source[line_start..line_end];
            if anchor.is_empty() {
                return None;
            }
            let mut candidates = source.match_indices(anchor).filter(|(offset, _)| {
                (*offset == 0 || source.as_bytes()[offset - 1] == b'\n')
                    && (*offset + anchor.len() == source.len()
                        || source.as_bytes()[offset + anchor.len()] == b'\n')
            });
            let offset = candidates.next()?.0;
            if candidates.next().is_some() {
                return None;
            }
            let new_start = source[..offset + start - line_start].chars().count();
            Some(new_start..new_start + range.end - range.start)
        });
        let mut current = Self::new(source, &self.syntax);
        current.word_wrap = self.word_wrap;
        current.code_font = self.code_font;
        current.find = std::mem::take(&mut self.find);
        current.set_links(links);
        let matched =
            relocated.is_some_and(|range| current.jump_to_range(range.start, range.end).is_ok());
        if current.find.open {
            current.update_find();
        }
        *self = current;
        matched
    }
    pub fn navigation_position(&self) -> usize {
        self.navigation_position
            .min(self.source.chars().count().saturating_sub(1))
    }

    /// Positions are Unicode scalar offsets, matching the engine protocol.
    pub fn jump_to(&mut self, position: usize) -> Result<(), String> {
        self.source
            .chars()
            .nth(position)
            .ok_or_else(|| "Declaration position is outside the source".to_owned())?;
        self.navigation_position = position;
        self.jump = Some(position);
        self.highlight = Some(self.symbol_range(position));
        Ok(())
    }
    pub fn jump_to_range(&mut self, start: usize, end: usize) -> Result<(), String> {
        if start >= end || end > self.source.chars().count() {
            return Err("Search match is outside the source".into());
        }
        self.jump_to(start)?;
        self.highlight = Some(start..end);
        Ok(())
    }
    fn symbol_range(&self, position: usize) -> Range<usize> {
        let next = self.links.partition_point(|link| link.start <= position);
        if let Some(link) = next.checked_sub(1).and_then(|index| self.links.get(index))
            && position < link.end
        {
            return link.start..link.end;
        }
        // Some declarations do not have a usage annotation. Expand their actual
        // identifier in scalar coordinates, never bytes or UTF-16 code units.
        let identifier = |c: char| {
            c.is_alphanumeric()
                || c == '_'
                || c == '$'
                || matches!(c, '\u{0300}'..='\u{036f}' | '\u{1ab0}'..='\u{1aff}'
                    | '\u{1dc0}'..='\u{1dff}' | '\u{20d0}'..='\u{20ff}'
                    | '\u{fe20}'..='\u{fe2f}')
        };
        let byte = self
            .source
            .char_indices()
            .nth(position)
            .map(|(i, _)| i)
            .unwrap_or(self.source.len());
        if !self.source[byte..].chars().next().is_some_and(identifier) {
            return position..position + 1;
        }
        let before = self.source[..byte]
            .chars()
            .rev()
            .take_while(|c| identifier(*c))
            .count();
        let after = self.source[byte..]
            .chars()
            .take_while(|c| identifier(*c))
            .count();
        position - before..position + after
    }

    fn glyph_at_pointer(galley: &egui::Galley, position: egui::Vec2) -> Option<usize> {
        let nearest = galley.cursor_from_pos(position).ccursor.index;
        for index in [Some(nearest), nearest.checked_sub(1)]
            .into_iter()
            .flatten()
        {
            let begin = galley.pos_from_ccursor(egui::text::CCursor::new(index));
            let end = galley.pos_from_ccursor(egui::text::CCursor::new(index + 1));
            if begin.top() != end.top() {
                continue;
            }
            let bounds =
                egui::Rect::from_min_max(begin.min, egui::pos2(end.left(), begin.bottom()));
            if bounds.contains(position.to_pos2()) && bounds.width() > 0.0 {
                return Some(index);
            }
        }
        None
    }

    fn link_at_pointer(&self, galley: &egui::Galley, position: egui::Vec2) -> Option<&CodeLink> {
        let global = self.char_start + Self::glyph_at_pointer(galley, position)?;
        let next = self.links.partition_point(|link| link.start <= global);
        next.checked_sub(1)
            .and_then(|index| self.links.get(index))
            .filter(|link| global < link.end)
    }
    pub fn text(&self) -> &str {
        &self.source
    }
    /// Release rendered glyphs without losing source, selection, or navigation.
    pub fn discard_layout_cache(&mut self) {
        self.cache = None;
    }

    /// Conservative accounting includes source, styled text, glyphs, and mesh buffers.
    pub fn retained_bytes(&self) -> usize {
        let base = self.source.capacity()
            + self.syntax.capacity()
            + self.find.query.capacity()
            + self.selection_occurrences.capacity() * std::mem::size_of::<Range<usize>>()
            + self.exported_components.capacity() * std::mem::size_of::<Range<usize>>()
            + self.links.capacity() * std::mem::size_of::<CodeLink>()
            + self
                .links
                .iter()
                .map(|link| link.label.capacity())
                .sum::<usize>();
        base.saturating_add(self.cache.as_ref().map_or(0, |cache| cache.retained_bytes))
    }
    fn job(&self, theme: CodeTheme, size: f32) -> LayoutJob {
        let text = &self.source[self.preview_start..self.preview_end];
        let palette = &themes().themes[theme.key()];
        let foreground = palette
            .settings
            .foreground
            .map(color)
            .unwrap_or(Color32::LIGHT_GRAY);
        let format = |foreground| TextFormat {
            font_id: self.code_font.font_id(size),
            color: foreground,
            line_height: Some(size * 1.45),
            ..Default::default()
        };
        let mut job = LayoutJob::default();
        job.wrap.max_width = f32::INFINITY;
        let set = syntax_set();
        let extension = match self.syntax.to_lowercase().as_str() {
            "javascript" => "js",
            "typescript" => "ts",
            "text" | "plain" => "txt",
            _ => self.syntax.trim_start_matches('.'),
        };
        let syntax = set
            .find_syntax_by_extension(extension)
            .or_else(|| set.find_syntax_by_name(&self.syntax));
        if !self.plain
            && let Some(syntax) = syntax
        {
            let mut highlighter = HighlightLines::new(syntax, palette);
            for line in LinesWithEndings::from(text) {
                match highlighter.highlight_line(line, set) {
                    Ok(parts) => {
                        for (style, part) in parts {
                            job.append(part, 0.0, format(color(style.foreground)));
                        }
                    }
                    Err(_) => job.append(line, 0.0, format(foreground)),
                }
            }
        } else {
            job.append(text, 0.0, format(foreground));
        }
        job
    }
    pub fn show(&mut self, ui: &mut egui::Ui, theme: CodeTheme, font_size: f32) -> Option<usize> {
        self.find_bar(ui);
        let mut clicked = None;
        let size = font_size.clamp(10.0, 28.0);
        let lines = self.source[self.preview_start..self.preview_end]
            .bytes()
            .filter(|b| *b == b'\n')
            .count()
            + 1;
        let digits = (self.line_start + lines).max(1).ilog10() + 1;
        let gutter_width = (digits as f32 + 1.0) * size * 0.65;
        let wrap_width = if self.word_wrap {
            (ui.available_width() - 16.0 - gutter_width - ui.spacing().item_spacing.x - 18.0)
                .max(40.0)
        } else {
            f32::INFINITY
        };
        let pixels_per_point = ui.ctx().pixels_per_point();
        if self.cache.as_ref().is_none_or(|cache| {
            cache.theme != theme
                || cache.size != size
                || cache.pixels_per_point != pixels_per_point
                || cache.wrap_width != wrap_width
        }) {
            let mut job = self.job(theme, size);
            job.wrap.max_width = wrap_width;
            let galley = ui.fonts(|fonts| fonts.layout_job(job));
            let retained_bytes = galley.job.text.capacity()
                + galley.job.sections.capacity() * std::mem::size_of::<egui::text::LayoutSection>()
                + galley.rows.capacity() * std::mem::size_of::<egui::epaint::text::Row>()
                + galley
                    .rows
                    .iter()
                    .map(|row| {
                        row.glyphs.capacity() * std::mem::size_of::<egui::epaint::text::Glyph>()
                            + row.visuals.mesh.vertices.capacity()
                                * std::mem::size_of::<egui::epaint::Vertex>()
                            + row.visuals.mesh.indices.capacity() * std::mem::size_of::<u32>()
                    })
                    .sum::<usize>();
            self.cache = Some(Cache {
                theme,
                size,
                pixels_per_point,
                galley,
                retained_bytes,
                wrap_width,
            });
        }

        if self.plain {
            ui.label("Very long lines: syntax coloring disabled to keep the viewer responsive.");
        }
        let palette = &themes().themes[theme.key()];
        let background = palette
            .settings
            .background
            .map(color)
            .unwrap_or(Color32::BLACK);
        let foreground = palette
            .settings
            .foreground
            .map(color)
            .unwrap_or(Color32::LIGHT_GRAY);
        let galley = Arc::clone(&self.cache.as_ref().expect("cache populated").galley);
        egui::Frame::NONE
            .fill(background)
            .inner_margin(8)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                ui.visuals_mut().override_text_color = Some(foreground);
                ui.visuals_mut().selection.bg_fill = if theme.is_light() {
                    Color32::from_rgb(180, 211, 221)
                } else {
                    Color32::from_rgb(62, 83, 112)
                };
                let mut scroll =
                    egui::ScrollArea::new([!self.word_wrap, true]).auto_shrink([false, false]);
                if let Some(position) = self.jump {
                    let target = galley.pos_from_ccursor(egui::text::CCursor::new(position));
                    scroll = scroll.vertical_scroll_offset(
                        (target.center().y - ui.available_height() * 0.5).max(0.0),
                    );
                }
                scroll.show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        // Paint numbers at the actual laid-out row positions, avoiding font drift.
                        let width = gutter_width;
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(width, galley.size().y),
                            egui::Sense::hover(),
                        );
                        let gutter_highlight = ui.painter().add(egui::Shape::Noop);
                        let mut line = self.line_start;
                        let mut line_beginning = true;
                        for row in &galley.rows {
                            let point = rect.min + egui::vec2(width - 4.0, row.rect.min.y);
                            if line_beginning
                                && ui.clip_rect().intersects(egui::Rect::from_min_size(
                                    point,
                                    egui::vec2(1.0, size * 1.45),
                                ))
                            {
                                ui.painter().text(
                                    point,
                                    egui::Align2::RIGHT_TOP,
                                    line.to_string(),
                                    self.code_font.font_id(size),
                                    foreground.gamma_multiply(0.65),
                                );
                            }
                            line_beginning = row.ends_with_newline;
                            if row.ends_with_newline {
                                line += 1;
                            }
                        }
                        let mut source = &self.source[self.preview_start..self.preview_end];
                        let mut layouter = |_: &egui::Ui, _: &str, _: f32| Arc::clone(&galley);
                        let editor_id = ui.make_persistent_id("source_editor");
                        let preserve_selection = ui.input(|input| {
                            input.pointer.button_pressed(egui::PointerButton::Secondary)
                        });
                        let previous_selection =
                            egui::text_edit::TextEditState::load(ui.ctx(), editor_id)
                                .and_then(|state| state.cursor.char_range());
                        // Reserve a background slot before TextEdit paints selection and
                        // glyphs. Highlights must never cover the user's selection.
                        let highlight_layer = ui.painter().add(egui::Shape::Noop);
                        let mut highlight_shapes = Vec::new();
                        let mut output = egui::TextEdit::multiline(&mut source)
                            .id(editor_id)
                            .font(self.code_font.font_id(size))
                            .code_editor()
                            .frame(false)
                            .margin(0)
                            .desired_width(if self.word_wrap {
                                wrap_width
                            } else {
                                galley.size().x.max(ui.clip_rect().width() - width).max(1.0)
                            })
                            .min_size(egui::vec2(0.0, ui.clip_rect().height()))
                            .layouter(&mut layouter)
                            .show(ui);
                        if preserve_selection {
                            output.state.cursor.set_char_range(previous_selection);
                            output.state.clone().store(ui.ctx(), editor_id);
                        }
                        if output.response.secondary_clicked() {
                            self.menu_link = ui
                                .ctx()
                                .pointer_interact_pos()
                                .filter(|pointer| output.text_clip_rect.contains(*pointer))
                                .and_then(|pointer| {
                                    self.link_at_pointer(
                                        &output.galley,
                                        pointer - output.galley_pos,
                                    )
                                })
                                .cloned();
                            self.menu_selection = output
                                .state
                                .cursor
                                .char_range()
                                .map(|range| {
                                    let [start, end] = range.sorted();
                                    start.index..end.index
                                })
                                .filter(|range| !range.is_empty())
                                .map(|range| {
                                    range.start + self.char_start..range.end + self.char_start
                                });
                        }
                        output.response.context_menu(|ui| {
                            // Popup text follows the interface theme, independently of code colors.
                            ui.visuals_mut().override_text_color = None;
                            #[cfg(test)]
                            self.menu_items.clear();
                            ui.weak("Navigation");
                            let go = ui.add_enabled(
                                self.menu_link.is_some(),
                                egui::Button::new("Go to declaration"),
                            );
                            #[cfg(test)]
                            self.menu_items.push((
                                "Go to declaration".into(),
                                go.rect,
                                go.enabled(),
                            ));
                            if go.clicked() {
                                clicked = self.menu_link.as_ref().map(|link| link.start);
                                ui.close_menu();
                            }
                            ui.separator();
                            ui.weak("X-Refs");
                            let usages = ui.add_enabled(
                                self.usages_enabled && self.menu_link.as_ref().is_some_and(|link| !link.label.starts_with(rdx::resource_table::PREFIX)),
                                egui::Button::new("Find usages"),
                            );
                            #[cfg(test)]
                            self.menu_items.push((
                                "Find usages".into(),
                                usages.rect,
                                usages.enabled(),
                            ));
                            if usages.clicked() {
                                self.usages_requested =
                                    self.menu_link.as_ref().map(|link| link.start);
                                ui.close_menu();
                            }
                            let subclasses = ui.add_enabled(
                                self.usages_enabled && self.menu_link.as_ref()
                                    .is_some_and(|link| !link.label.contains(['(', ':'])),
                                egui::Button::new("Find direct subclasses"),
                            ).on_hover_text("Find classes whose immediate superclass is this type; excludes grandchildren and interface implementations.");
                            #[cfg(test)]
                            self.menu_items.push(("Find direct subclasses".into(), subclasses.rect, subclasses.enabled()));
                            if subclasses.clicked() {
                                self.subclasses_requested = self.menu_link.as_ref().map(|link| link.start);
                                ui.close_menu();
                            }
                            let implementations = ui.add_enabled(
                                self.usages_enabled && self.menu_link.as_ref().is_some_and(|link|
                                    !link.label.contains(':') && !link.label.contains(".<init>(") && !link.label.contains(".<clinit>(")),
                                egui::Button::new("Find implementations"),
                            ).on_hover_text("Find concrete implementing classes or overriding method declarations across the loaded hierarchy.");
                            #[cfg(test)]
                            self.menu_items.push(("Find implementations".into(), implementations.rect, implementations.enabled()));
                            if implementations.clicked() {
                                self.implementations_requested = self.menu_link.as_ref().map(|link| link.start);
                                ui.close_menu();
                            }
                            let method_enabled = self.usages_enabled && self.menu_link.as_ref()
                                .is_some_and(|link| link.label.contains('(') && !link.label.starts_with(rdx::resource_table::PREFIX));
                            ui.add_enabled_ui(method_enabled, |ui| {
                                let submenu = ui.menu_button("Method references", |ui| {
                                    let graph_action = ui.button("Call graph…");
                                    #[cfg(test)]
                                    self.menu_items.push(("Call graph…".into(), graph_action.rect, graph_action.enabled()));
                                    if graph_action.clicked() {
                                        self.call_graph_requested = self.menu_link.as_ref().map(|link| link.start);
                                        ui.close_menu();
                                    }
                                    for (label, callers) in [("Callers", true), ("Callees", false)] {
                                        let response = ui.button(label);
                                        #[cfg(test)]
                                        self.menu_items.push((label.into(), response.rect, response.enabled()));
                                        if response.clicked() {
                                            self.method_xrefs_requested = self.menu_link.as_ref().map(|link| (link.start, callers));
                                            ui.close_menu();
                                        }
                                    }
                                });
                                #[cfg(test)]
                                self.menu_items.push(("Method references".into(), submenu.response.rect, submenu.response.enabled()));
                                #[cfg(not(test))]
                                let _ = submenu;
                            });
                            ui.separator();
                            ui.weak("Copy");
                            let symbol = ui.add_enabled(
                                self.menu_link.is_some(),
                                egui::Button::new("Copy symbol name"),
                            );
                            #[cfg(test)]
                            self.menu_items.push((
                                "Copy symbol name".into(),
                                symbol.rect,
                                symbol.enabled(),
                            ));
                            if symbol.clicked() {
                                if let Some(link) = &self.menu_link {
                                    ui.ctx().copy_text(link.label.clone());
                                }
                                ui.close_menu();
                            }
                            let snippet = self.menu_link.as_ref().and_then(|link| rdx::frida_snippet::for_method(&link.label));
                            let frida = ui.add_enabled(snippet.is_some(), egui::Button::new("Copy as Frida snippet"));
                            #[cfg(test)]
                            self.menu_items.push(("Copy as Frida snippet".into(), frida.rect, frida.enabled()));
                            if frida.clicked() {
                                if let Some(snippet) = snippet { ui.ctx().copy_text(snippet); }
                                ui.close_menu();
                            }
                            ui.separator();
                            let selection = ui.add_enabled(
                                self.menu_selection.is_some(),
                                egui::Button::new("Copy selection"),
                            );
                            #[cfg(test)]
                            self.menu_items.push((
                                "Copy selection".into(),
                                selection.rect,
                                selection.enabled(),
                            ));
                            if selection.clicked() {
                                if let Some(range) = &self.menu_selection {
                                    ui.ctx().copy_text(
                                        self.source
                                            .chars()
                                            .skip(range.start)
                                            .take(range.len())
                                            .collect(),
                                    );
                                }
                                ui.close_menu();
                            }
                            let export = ui.button("Export…");
                            #[cfg(test)]
                            self.menu_items
                                .push(("Export…".into(), export.rect, export.enabled()));
                            if export.clicked() {
                                self.export_requested = true;
                                ui.close_menu();
                            }
                            let source = ui.button("Copy source");
                            #[cfg(test)]
                            self.menu_items.push((
                                "Copy source".into(),
                                source.rect,
                                source.enabled(),
                            ));
                            if source.clicked() {
                                ui.ctx().copy_text(self.source.clone());
                                ui.close_menu();
                            }
                        });
                        #[cfg(test)]
                        {
                            self.last_galley_pos = Some(output.galley_pos);
                            self.last_editor_id = Some(editor_id);
                        }
                        if let Some(cursor) = output.state.cursor.char_range() {
                            let position = self.char_start + cursor.primary.index;
                            if self.jump.is_none() && self.last_navigation_cursor != Some(position)
                            {
                                self.navigation_position = position;
                            }
                            self.last_navigation_cursor = Some(position);
                        }
                        let selected = output
                            .state
                            .cursor
                            .char_range()
                            .map(|range| {
                                let [start, end] = range.sorted();
                                start.index..end.index
                            })
                            .filter(|range| !range.is_empty());
                        let caret = output
                            .state
                            .cursor
                            .char_range()
                            .map(|range| range.primary.index);
                        if selected.is_some()
                            || self
                                .clicked_word
                                .as_ref()
                                .is_some_and(|(position, _)| Some(*position) != caret)
                        {
                            self.clicked_word = None;
                        }
                        if selected.is_none()
                            && output.response.clicked_by(egui::PointerButton::Primary)
                        {
                            self.clicked_word = output
                                .response
                                .interact_pointer_pos()
                                .filter(|point| {
                                    output
                                        .text_clip_rect
                                        .intersect(ui.clip_rect())
                                        .contains(*point)
                                })
                                .and_then(|point| {
                                    Self::glyph_at_pointer(
                                        &output.galley,
                                        point - output.galley_pos,
                                    )
                                })
                                .and_then(|index| {
                                    crate::word_occurrences::word_at(
                                        &self.source[self.preview_start..self.preview_end],
                                        index,
                                    )
                                })
                                .zip(caret)
                                .map(|(range, position)| (position, range));
                        }
                        let moved_focus = output.response.has_focus()
                            && ui.input(|input| {
                                input.pointer.primary_down()
                                    || input.events.iter().any(|event| {
                                        matches!(event, egui::Event::Key { pressed: true, .. })
                                    })
                            });
                        if self.jump.is_none()
                            && moved_focus
                            && let (Some(caret), Some(target)) = (caret, &self.highlight)
                        {
                            let line =
                                |at| self.source.chars().take(at).filter(|c| *c == '\n').count();
                            if line(self.char_start + caret) != line(target.start) {
                                self.highlight = None;
                            }
                        }
                        let occurrence_word = selected
                            .clone()
                            .or_else(|| self.clicked_word.as_ref().map(|(_, range)| range.clone()));
                        self.update_selection_occurrences(occurrence_word);
                        let painter = ui
                            .painter()
                            .with_clip_rect(output.text_clip_rect.intersect(ui.clip_rect()));
                        let tint = if theme.is_light() {
                            Color32::from_rgba_unmultiplied(40, 120, 180, 45)
                        } else {
                            Color32::from_rgba_unmultiplied(100, 190, 230, 55)
                        };
                        let mut hovered_exported = false;
                        let mut row_start = 0;
                        for row in &galley.rows {
                            let row_end = row_start + row.glyphs.len();
                            if painter
                                .clip_rect()
                                .intersects(row.rect.translate(output.galley_pos.to_vec2()))
                            {
                                let global_start = self.char_start + row_start;
                                let global_end = self.char_start + row_end;
                                let first = self
                                    .exported_components
                                    .partition_point(|range| range.end <= global_start);
                                for range in self.exported_components[first..]
                                    .iter()
                                    .take_while(|range| range.start < global_end)
                                {
                                    let left =
                                        row.x_offset(range.start.saturating_sub(global_start));
                                    let right =
                                        row.x_offset(range.end.min(global_end) - global_start);
                                    let rect = egui::Rect::from_min_max(
                                        egui::pos2(left, row.rect.top()),
                                        egui::pos2(right, row.rect.bottom()),
                                    )
                                    .translate(output.galley_pos.to_vec2());
                                    highlight_shapes.push(egui::Shape::rect_filled(
                                        rect,
                                        2.0,
                                        exported_component_tint(theme),
                                    ));
                                    hovered_exported |=
                                        ui.ctx().pointer_hover_pos().is_some_and(|pointer| {
                                            rect.intersect(painter.clip_rect()).contains(pointer)
                                        });
                                }
                                let first = self
                                    .selection_occurrences
                                    .partition_point(|range| range.end <= row_start);
                                for range in self.selection_occurrences[first..]
                                    .iter()
                                    .take_while(|range| range.start < row_end)
                                {
                                    if selected.as_ref() == Some(range) {
                                        continue;
                                    }
                                    let left = row.x_offset(range.start.saturating_sub(row_start));
                                    let right = row.x_offset(range.end.min(row_end) - row_start);
                                    let rect = egui::Rect::from_min_max(
                                        egui::pos2(left, row.rect.top()),
                                        egui::pos2(right, row.rect.bottom()),
                                    )
                                    .translate(output.galley_pos.to_vec2());
                                    highlight_shapes
                                        .push(egui::Shape::rect_filled(rect, 1.0, tint));
                                }
                            }
                            row_start = row_end + usize::from(row.ends_with_newline);
                        }
                        if let Some(target) = &self.highlight {
                            let local_start = target.start.saturating_sub(self.char_start);
                            let local_end = target
                                .end
                                .saturating_sub(self.char_start)
                                .min(galley.job.text.chars().count());
                            let first = output
                                .galley
                                .pos_from_ccursor(egui::text::CCursor::new(local_start));
                            let last = output
                                .galley
                                .pos_from_ccursor(egui::text::CCursor::new(local_end));
                            for row in &output.galley.rows {
                                if row.rect.bottom() <= first.top() || row.rect.top() > last.top() {
                                    continue;
                                }
                                let left = if row.rect.top() <= first.top() {
                                    first.left()
                                } else {
                                    row.rect.left()
                                };
                                let right = if row.rect.top() >= last.top() {
                                    last.left()
                                } else {
                                    row.rect.right()
                                };
                                if right > left {
                                    let rect = egui::Rect::from_min_max(
                                        egui::pos2(left, row.rect.top()),
                                        egui::pos2(right, row.rect.bottom()),
                                    )
                                    .translate(output.galley_pos.to_vec2());
                                    highlight_shapes.push(egui::Shape::rect_filled(
                                        rect,
                                        1.0,
                                        jump_tint(theme),
                                    ));
                                }
                            }
                            // Mark the logical destination line, including wrapped lines.
                            let line_start = galley
                                .job
                                .text
                                .chars()
                                .take(local_start)
                                .enumerate()
                                .filter(|(_, c)| *c == '\n')
                                .map(|(i, _)| i + 1)
                                .last()
                                .unwrap_or(0);
                            let line_rect =
                                galley.pos_from_ccursor(egui::text::CCursor::new(line_start));
                            let marker = egui::Rect::from_min_max(
                                egui::pos2(rect.left(), rect.top() + line_rect.top()),
                                egui::pos2(rect.right(), rect.top() + line_rect.bottom()),
                            );
                            ui.painter().set(
                                gutter_highlight,
                                egui::Shape::rect_filled(marker, 2.0, jump_gutter_tint(theme)),
                            );
                            if self.jump.take().is_some() {
                                ui.scroll_to_rect_animation(
                                    first.translate(output.galley_pos.to_vec2()),
                                    Some(egui::Align::Center),
                                    egui::style::ScrollAnimation::none(),
                                );
                            }
                        }
                        ui.painter()
                            .with_clip_rect(painter.clip_rect())
                            .set(highlight_layer, egui::Shape::Vec(highlight_shapes));
                        if let Some(pointer) = ui.ctx().pointer_hover_pos()
                            && output.response.rect.contains(pointer)
                            && output.text_clip_rect.contains(pointer)
                            && let Some(link) =
                                self.link_at_pointer(&output.galley, pointer - output.galley_pos)
                        {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            let first = output.galley.pos_from_ccursor(egui::text::CCursor::new(
                                link.start.saturating_sub(self.char_start),
                            ));
                            let last = output.galley.pos_from_ccursor(egui::text::CCursor::new(
                                link.end.saturating_sub(self.char_start),
                            ));
                            if first.top() == last.top() {
                                ui.painter().line_segment(
                                    [
                                        output.galley_pos
                                            + egui::vec2(first.left(), first.bottom()),
                                        output.galley_pos + egui::vec2(last.left(), last.bottom()),
                                    ],
                                    egui::Stroke::new(1.0_f32, foreground),
                                );
                            }
                            if output.response.double_clicked() {
                                clicked = Some(link.start);
                            }
                            let label = if hovered_exported {
                                format!("{}\nExported Android component", link.label)
                            } else {
                                link.label.split_once(" | ").map_or_else(|| link.label.clone(), |(_, preview)| preview.to_owned())
                            };
                            output.response.on_hover_text(label);
                        } else if hovered_exported {
                            output.response.on_hover_text("Exported Android component");
                        }
                    });
                });
            });
        clicked
    }
}

#[cfg(test)]
mod tests {
    fn painted_shapes(output: &egui::FullOutput) -> Vec<egui::epaint::ClippedShape> {
        fn append(
            shape: &egui::Shape,
            clip: egui::Rect,
            out: &mut Vec<egui::epaint::ClippedShape>,
        ) {
            if let egui::Shape::Vec(shapes) = shape {
                for shape in shapes {
                    append(shape, clip, out);
                }
            } else {
                out.push(egui::epaint::ClippedShape {
                    clip_rect: clip,
                    shape: shape.clone(),
                });
            }
        }
        let mut shapes = vec![];
        for shape in &output.shapes {
            append(&shape.shape, shape.clip_rect, &mut shapes);
        }
        shapes
    }

    #[test]
    fn jump_markers_are_under_selection_and_clear_on_other_line_in_every_theme() {
        for theme in CodeTheme::ALL {
            let context = egui::Context::default();
            let mut doc =
                CodeDocument::new("// header\nclass Target {}\nnext line\n".into(), "java");
            doc.jump_to_range(16, 22).unwrap();
            let render = |doc: &mut CodeDocument, events| {
                context.run(
                    egui::RawInput {
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            doc.show(ui, theme, 14.0);
                        });
                    },
                )
            };
            render(&mut doc, vec![]);
            let id = doc.last_editor_id.unwrap();
            let mut state = egui::text_edit::TextEditState::load(&context, id).unwrap();
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(16),
                    egui::text::CCursor::new(22),
                )));
            state.store(&context, id);
            context.memory_mut(|memory| memory.request_focus(id));
            let output = render(&mut doc, vec![]);
            let shapes = painted_shapes(&output);
            let jump = shapes
                .iter()
                .position(|shape| {
                    matches!(&shape.shape,
                egui::Shape::Rect(rect) if rect.fill == super::jump_tint(theme))
                })
                .unwrap();
            let selection_color = if theme.is_light() {
                Color32::from_rgb(180, 211, 221)
            } else {
                Color32::from_rgb(62, 83, 112)
            };
            let selection = shapes
                .iter()
                .position(|shape| {
                    matches!(&shape.shape,
                egui::Shape::Text(text) if text.galley.rows.iter().any(|row|
                            row.visuals.mesh.vertices.iter().any(|vertex| vertex.color == selection_color)))
                })
                .unwrap();
            assert!(
                jump < selection,
                "selection must paint over jump: {theme:?}"
            );
            assert!(shapes.iter().any(|shape| matches!(&shape.shape,
                egui::Shape::Rect(rect) if rect.fill == super::jump_gutter_tint(theme)
                    && rect.rect.width() > 14.0 && rect.rect.height() >= 14.0)));
            assert!(
                doc.highlight.is_some(),
                "same-line selection preserves destination"
            );
            let output = render(
                &mut doc,
                vec![egui::Event::Key {
                    key: egui::Key::ArrowDown,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            assert!(
                doc.highlight.is_none(),
                "other line clears destination: {theme:?}"
            );
            assert!(
                !painted_shapes(&output)
                    .iter()
                    .any(|shape| matches!(&shape.shape,
                egui::Shape::Rect(rect) if rect.fill == super::jump_gutter_tint(theme)
                    || rect.fill == super::jump_tint(theme)))
            );
        }
    }

    #[test]
    fn selected_word_highlights_other_occurrences_with_fonts_and_wrapping() {
        let context = egui::Context::default();
        crate::code_fonts::install(&context);
        let source = format!("café caféteria\n{}café\ncafé", "word ".repeat(30));
        let mut doc = super::CodeDocument::new(source, "java");
        doc.set_word_wrap(true);
        let render = |doc: &mut super::CodeDocument| {
            context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(420.0, 700.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        doc.show(ui, super::CodeTheme::QuietLight, 14.0);
                    });
                },
            )
        };
        for font in crate::code_fonts::CodeFont::ALL {
            doc.set_code_font(font);
            render(&mut doc);
            let id = doc.last_editor_id.unwrap();
            let mut state = egui::text_edit::TextEditState::load(&context, id).unwrap();
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(4),
                )));
            state.store(&context, id);
            let output = render(&mut doc);
            assert_eq!(doc.selection_occurrences.len(), 3);
            assert_eq!(
                doc.cache.as_ref().unwrap().galley.job.sections[0]
                    .format
                    .font_id,
                font.font_id(14.0)
            );
            let tint = egui::Color32::from_rgba_unmultiplied(40, 120, 180, 45);
            let marks = painted_shapes(&output)
                .into_iter()
                .filter(|s| matches!(&s.shape, egui::Shape::Rect(rect) if rect.fill == tint))
                .count();
            assert_eq!(marks, 2, "other whole words should be visibly marked");
            let mut state = egui::text_edit::TextEditState::load(&context, id).unwrap();
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(1),
                )));
            state.store(&context, id);
            render(&mut doc);
            assert!(doc.selection_occurrences.is_empty());
        }
    }
    #[test]
    fn exported_component_highlight_tracks_unicode_wrapping_and_themes() {
        let name = "sample.AVeryLongExportedComponentName";
        let source =
            format!("<!-- 🦀 -->\n<activity android:name=\"{name}\" android:exported=\"true\"/>");
        let start = source[..source.find(name).unwrap()].chars().count();
        let range = start..start + name.chars().count();
        for theme in CodeTheme::ALL {
            for wrap in [false, true] {
                let mut doc = CodeDocument::new(source.clone(), "xml");
                doc.set_word_wrap(wrap);
                doc.set_links(vec![CodeLink {
                    start: range.start,
                    end: range.end,
                    label: name.into(),
                }]);
                doc.set_exported_components(vec![
                    range.clone(),
                    range.clone(),
                    0..0,
                    0..usize::MAX,
                ]);
                assert_eq!(doc.exported_components, vec![range.clone()]);
                let context = egui::Context::default();
                let output = context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(260.0, 500.0),
                        )),
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            doc.show(ui, theme, 14.0);
                        });
                    },
                );
                let painted: Vec<_> = painted_shapes(&output)
                    .into_iter()
                    .filter_map(|shape| match &shape.shape {
                        egui::epaint::Shape::Rect(rect)
                            if rect.fill == super::exported_component_tint(theme) =>
                        {
                            Some(rect.clone())
                        }
                        _ => None,
                    })
                    .collect();
                assert!(!painted.is_empty(), "{theme:?}, wrap={wrap}");
                assert!(painted.iter().all(|rect| rect.rect.width() > 0.0));
                if wrap {
                    assert!(painted.len() > 1, "long name should wrap");
                }
                assert_eq!(doc.link_at(start).unwrap().label, name);
                assert_eq!(doc.text(), source);
                doc.set_links(vec![]);
                assert_eq!(
                    doc.exported_components,
                    vec![range.clone()],
                    "highlight is independent of available classes"
                );
            }
        }
    }

    #[test]
    fn exported_highlights_remain_visible_after_full_document_jumps() {
        let prefix = "\n".repeat(6_000);
        let mut doc = CodeDocument::new(format!("{prefix}sample.Target"), "xml");
        doc.set_exported_components(std::iter::once(6_000..6_013).collect());
        let ctx = egui::Context::default();
        let draw = |doc: &mut CodeDocument| {
            ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    doc.show(ui, CodeTheme::Ocean, 14.0);
                });
            })
        };
        let count = |out: &egui::FullOutput| {
            painted_shapes(out).into_iter().filter(|shape| matches!(&shape.shape,
            egui::epaint::Shape::Rect(rect) if rect.fill == super::exported_component_tint(CodeTheme::Ocean))).count()
        };
        assert_eq!(count(&draw(&mut doc)), 0);
        doc.jump_to(6_000).unwrap();
        draw(&mut doc); // Apply the scroll request before checking the visible frame.
        let result = draw(&mut doc);

        assert_eq!(count(&result), 1);
    }

    #[test]
    fn file_find_is_literal_unicode_aware_and_cycles_both_directions() {
        let mut doc = super::CodeDocument::new("🦀 CAFÉ café a.b axb".into(), "txt");
        doc.find.query = "café".into();
        doc.update_find();
        assert_eq!(doc.find.total, 2);
        assert_eq!(doc.highlight, Some(2..6));
        doc.step_find(false);
        assert_eq!(doc.highlight, Some(7..11));
        doc.step_find(false);
        assert_eq!(doc.highlight, Some(2..6));
        doc.step_find(true);
        assert_eq!(doc.highlight, Some(7..11));
        doc.find.case_sensitive = true;
        doc.update_find();
        assert_eq!(doc.find.total, 1);
        doc.find.query = "a.b".into();
        doc.update_find();
        assert_eq!(doc.find.total, 1, "literal dots must not act as regex");
        doc.find.query = "missing".into();
        doc.update_find();
        assert_eq!(doc.find.total, 0);
        assert!(doc.highlight.is_none());
        doc.step_find(true);
    }

    #[test]
    fn file_find_reaches_the_full_document_and_refreshes_source() {
        let source = format!("{}needle", "λ\n".repeat(90_000));
        let mut doc = super::CodeDocument::new(source, "txt");
        doc.open_find();
        doc.find.query = "needle".into();
        doc.update_find();
        assert_eq!(doc.preview_start, 0);
        assert_eq!(doc.preview_end, doc.source.len());
        assert_eq!(doc.highlight, Some(180_000..180_006));
        doc.set_word_wrap(true);
        doc.refresh_search_source("🦀 needle".into(), vec![]);
        assert!(doc.word_wrap && doc.find.open);
        assert_eq!(doc.find.total, 1);
        assert_eq!(doc.highlight, Some(2..8));
    }

    #[test]
    fn wrapping_reflows_on_resize_without_changing_source_or_link_offsets() {
        let source = format!("{}sample.Target\nsecond line", "word ".repeat(35));
        let start = source.find("sample.Target").unwrap();
        let mut doc = super::CodeDocument::new(source.clone(), "java");
        doc.set_links(vec![super::CodeLink {
            start,
            end: start + 13,
            label: "sample.Target".into(),
        }]);
        doc.set_exported_components(std::iter::once(start..start + 13).collect());
        let context = egui::Context::default();
        let render = |doc: &mut super::CodeDocument, width| {
            let _ = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 700.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        doc.show(ui, super::CodeTheme::Ocean, 14.0);
                    });
                },
            );
        };
        doc.set_word_wrap(true);
        render(&mut doc, 340.0);
        let cache = doc.cache.as_ref().unwrap();
        let narrow_rows = cache.galley.rows.len();
        assert!(narrow_rows > 2);
        assert!(cache.galley.size().x <= cache.wrap_width + 1.0);
        assert_eq!(
            cache
                .galley
                .rows
                .iter()
                .filter(|r| r.ends_with_newline)
                .count(),
            1
        );
        let galley = &cache.galley;
        let a = galley.pos_from_ccursor(egui::text::CCursor::new(start + 1));
        let b = galley.pos_from_ccursor(egui::text::CCursor::new(start + 2));
        assert_eq!(
            doc.link_at_pointer(
                galley,
                egui::vec2((a.left() + b.left()) / 2.0, a.center().y)
            )
            .unwrap()
            .start,
            start
        );
        render(&mut doc, 650.0);
        assert!(doc.cache.as_ref().unwrap().galley.rows.len() < narrow_rows);
        doc.set_word_wrap(false);
        render(&mut doc, 340.0);
        assert_eq!(doc.cache.as_ref().unwrap().galley.rows.len(), 2);
        assert_eq!(doc.text(), source);
    }

    #[test]
    fn find_bar_keyboard_enter_shift_enter_and_escape() {
        let context = egui::Context::default();
        let mut doc = super::CodeDocument::new("word word".into(), "txt");
        doc.open_find();
        let render = |doc: &mut super::CodeDocument, events| {
            let _ = context.run(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        doc.show(ui, super::CodeTheme::Ocean, 14.0);
                    });
                },
            );
        };
        render(&mut doc, vec![]);
        render(&mut doc, vec![egui::Event::Text("word".into())]);
        assert_eq!(doc.find.total, 2);
        let key = |key, modifiers| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        render(&mut doc, vec![key(egui::Key::Enter, egui::Modifiers::NONE)]);
        assert_eq!(doc.highlight, Some(5..9));
        render(
            &mut doc,
            vec![key(egui::Key::Enter, egui::Modifiers::SHIFT)],
        );
        assert_eq!(doc.highlight, Some(0..4));
        render(
            &mut doc,
            vec![key(egui::Key::Escape, egui::Modifiers::NONE)],
        );
        assert!(!doc.find.open);
        doc.open_find();
        assert_eq!(doc.highlight, Some(0..4));
    }
    use super::*;

    #[test]
    fn refreshed_search_source_relocates_unique_unicode_lines_without_guessing() {
        let old = "// old\nreturn café;\n";
        let start = old[..old.find("café").unwrap()].chars().count();
        let mut doc = CodeDocument::new(old.into(), "java");
        doc.jump_to_range(start, start + 4).unwrap();
        let fresh = "// new 🦀\n// inserted\nreturn café;\n";
        let current_start = fresh[..fresh.find("café").unwrap()].chars().count();
        assert!(doc.refresh_search_source(
            fresh.into(),
            vec![CodeLink {
                start: current_start,
                end: current_start + 4,
                label: "sample.café".into(),
            }]
        ));
        assert_eq!(doc.highlight, Some(current_start..current_start + 4));
        assert_eq!(
            doc.symbol_range(current_start),
            current_start..current_start + 4
        );
        assert!(!doc.refresh_search_source("return café;\nreturn café;\n".into(), vec![]));
        assert!(
            doc.highlight.is_none(),
            "ambiguous repeated lines must not inherit stale offsets"
        );
        doc.jump_to_range(7, 11).unwrap();
        assert!(!doc.refresh_search_source("return different;\n".into(), vec![]));
        assert!(doc.highlight.is_none());
    }
    #[test]
    fn context_menu_navigates_copies_and_preserves_selection() {
        for (action, blank, metadata, usages_enabled) in [
            ("Go to declaration", false, true, true),
            ("Find usages", false, true, true),
            ("Callers", false, true, true),
            ("Callees", false, true, true),
            ("Call graph…", false, true, true),
            ("Find direct subclasses", false, true, true),
            ("Find implementations", false, true, true),
            ("Copy symbol name", false, true, true),
            ("Copy as Frida snippet", false, true, true),
            ("Copy selection", false, true, true),
            ("Copy source", true, true, true),
            ("Copy source", false, false, true),
            ("Copy source", false, true, false),
            ("Export…", false, true, true),
        ] {
            let context = egui::Context::default();
            let mut doc = CodeDocument::new(
                "🎯 café = 1; // café\n".into(),
                if metadata { "java" } else { "txt" },
            );
            let label = match action {
                "Find usages" | "Callers" | "Callees" | "Call graph…" | "Copy as Frida snippet" => {
                    "pkg.Target.run()V"
                }
                "Copy symbol name" => "pkg.Target.café:I",
                _ => "pkg.Target",
            };
            if metadata {
                doc.set_links(vec![CodeLink {
                    start: 2,
                    end: 6,
                    label: label.into(),
                }]);
            }
            doc.set_usages_enabled(usages_enabled);
            let render = |doc: &mut CodeDocument, time: f64, events| {
                let mut result = None;
                let output = context.run(
                    egui::RawInput {
                        time: Some(time),
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            result = doc.show(ui, CodeTheme::Ocean, 14.0);
                        });
                    },
                );
                (result, output)
            };
            render(&mut doc, 0.0, vec![]);
            let id = doc.last_editor_id.unwrap();
            let mut state = egui::text_edit::TextEditState::load(&context, id).unwrap();
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(2),
                    egui::text::CCursor::new(6),
                )));
            state.store(&context, id);
            let galley = &doc.cache.as_ref().unwrap().galley;
            let a = galley.pos_from_ccursor(egui::text::CCursor::new(3));
            let b = galley.pos_from_ccursor(egui::text::CCursor::new(4));
            let point = if blank {
                egui::pos2(500.0, 300.0)
            } else {
                doc.last_galley_pos.unwrap() + egui::vec2((a.left() + b.left()) / 2.0, a.center().y)
            };
            let button = |pos, button, pressed| egui::Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            render(
                &mut doc,
                0.1,
                vec![
                    egui::Event::PointerMoved(point),
                    button(point, egui::PointerButton::Secondary, true),
                ],
            );
            render(
                &mut doc,
                0.2,
                vec![button(point, egui::PointerButton::Secondary, false)],
            );
            render(&mut doc, 0.3, vec![]);
            assert_eq!(doc.menu_items.len(), 10);
            assert_eq!(doc.menu_items[0].2, !blank && metadata);
            assert_eq!(doc.menu_items[1].2, !blank && metadata && usages_enabled);
            assert_eq!(
                doc.menu_items[2].2,
                !blank && metadata && usages_enabled && !label.contains(['(', ':'])
            );
            assert_eq!(
                doc.menu_items[3].2,
                !blank && metadata && usages_enabled && !label.contains(':')
            );
            let frida = doc
                .menu_items
                .iter()
                .find(|item| item.0 == "Copy as Frida snippet")
                .unwrap();
            assert_eq!(
                frida.2,
                !blank && metadata && rdx::frida_snippet::for_method(label).is_some()
            );
            assert_eq!(doc.menu_selection, Some(2..6));
            let submenu = doc
                .menu_items
                .iter()
                .find(|item| item.0 == "Method references")
                .unwrap();
            assert_eq!(
                submenu.2,
                !blank && metadata && usages_enabled && label.contains('(')
            );
            if matches!(action, "Callers" | "Callees" | "Call graph…") {
                let pos = submenu.1.center();
                render(&mut doc, 0.31, vec![egui::Event::PointerMoved(pos)]);
                render(&mut doc, 0.39, vec![]);
            }
            let item = doc
                .menu_items
                .iter()
                .find(|item| item.0 == action)
                .unwrap()
                .1
                .center();
            render(
                &mut doc,
                0.4,
                vec![
                    egui::Event::PointerMoved(item),
                    button(item, egui::PointerButton::Primary, true),
                ],
            );
            let (result, output) = render(
                &mut doc,
                0.5,
                vec![button(item, egui::PointerButton::Primary, false)],
            );
            if action == "Go to declaration" {
                assert_eq!(result, Some(2));
            } else if matches!(action, "Callers" | "Callees") {
                assert_eq!(
                    doc.take_method_xrefs_request(),
                    Some((2, action == "Callers"))
                );
                assert_eq!(doc.take_method_xrefs_request(), None);
            } else if action == "Call graph…" {
                assert_eq!(doc.take_call_graph_request(), Some(2));
                assert_eq!(doc.take_call_graph_request(), None);
            } else if action == "Find usages" {
                assert_eq!(result, None);
                assert_eq!(doc.take_usages_request(), Some(2));
                assert_eq!(doc.take_usages_request(), None);
            } else if action == "Find direct subclasses" {
                assert_eq!(result, None);
                assert_eq!(doc.take_subclasses_request(), Some(2));
                assert_eq!(doc.take_subclasses_request(), None);
                assert_eq!(doc.take_usages_request(), None);
            } else if action == "Find implementations" {
                assert_eq!(doc.take_implementations_request(), Some(2));
                assert_eq!(doc.take_implementations_request(), None);
            } else if action == "Copy as Frida snippet" {
                let expected = rdx::frida_snippet::for_method(label).unwrap();
                assert!(output.platform_output.commands.iter().any(|command| matches!(command, egui::OutputCommand::CopyText(text) if text == &expected)));
            } else if action == "Export…" {
                assert!(doc.take_export_request());
                assert!(!doc.take_export_request());
            } else {
                let expected = match action {
                    "Copy symbol name" => label,
                    "Copy selection" => "café",
                    _ => doc.text(),
                };
                assert!(output.platform_output.commands.iter().any(|command| matches!(command, egui::OutputCommand::CopyText(text) if text == expected)), "{action}");
            }
        }
    }
    #[test]
    fn search_ranges_highlight_exact_unicode_and_multiple_lines() {
        let context = egui::Context::default();
        let mut doc = CodeDocument::new("α first\nsecond tail".into(), "text");
        doc.jump_to_range(2, 14).unwrap();
        assert_eq!(doc.highlight, Some(2..14));
        let output = context.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                doc.show(ui, CodeTheme::Ocean, 14.0);
            });
        });
        let highlights = painted_shapes(&output).into_iter().filter(|shape| matches!(&shape.shape, egui::epaint::Shape::Rect(rect) if rect.fill == super::jump_tint(CodeTheme::Ocean))).count();
        assert_eq!(highlights, 2);
        assert!(doc.jump_to_range(4, 4).is_err());
        assert!(doc.jump_to_range(0, 100).is_err());
    }
    #[test]
    fn jumps_highlight_complete_metadata_symbol_at_glyph_width() {
        let context = egui::Context::default();
        let mut doc = CodeDocument::new("🎯 class LongComponentName {}".into(), "java");
        doc.set_links(vec![CodeLink {
            start: 8,
            end: 25,
            label: "LongComponentName".into(),
        }]);
        doc.jump_to(10).unwrap();
        assert_eq!(doc.highlight, Some(8..25));
        let output = context.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                doc.show(ui, CodeTheme::Ocean, 14.0);
            });
        });
        let galley = &doc.cache.as_ref().unwrap().galley;
        let expected_width = galley.pos_from_ccursor(egui::text::CCursor::new(25)).left()
            - galley.pos_from_ccursor(egui::text::CCursor::new(8)).left();
        let painted = painted_shapes(&output)
            .into_iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Rect(rect)
                    if rect.fill == super::jump_tint(CodeTheme::Ocean) =>
                {
                    Some(rect.clone())
                }
                _ => None,
            })
            .expect("destination highlight must be painted");
        assert!((painted.rect.width() - expected_width).abs() < 0.01);
        assert!(painted.rect.width() > 100.0);
    }
    #[test]
    fn declaration_fallback_expands_unicode_identifier_without_punctuation() {
        let mut doc = CodeDocument::new("🎯 void $Cafe\u{0301}_方法42() {}".into(), "java");
        doc.jump_to(9).unwrap();
        assert_eq!(doc.highlight, Some(7..18));
        doc.jump_to(18).unwrap();
        assert_eq!(doc.highlight, Some(18..19));
    }
    #[test]
    fn single_click_highlights_whole_words_without_selecting_or_navigating() {
        for wrap in [false, true] {
            let context = egui::Context::default();
            let source = format!("🎯 café caféteria; {}café", "word ".repeat(20));
            let mut doc = CodeDocument::new(source, "java");
            doc.set_word_wrap(wrap);
            doc.set_links(vec![CodeLink {
                start: 2,
                end: 6,
                label: "café".into(),
            }]);
            let render = |doc: &mut CodeDocument, time: f64, events| {
                let mut navigation = None;
                let output = context.run(
                    egui::RawInput {
                        time: Some(time),
                        events,
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(320.0, 600.0),
                        )),
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            navigation = doc.show(ui, CodeTheme::QuietLight, 14.0);
                        });
                    },
                );
                assert_eq!(navigation, None, "single click must not navigate");
                output
            };
            render(&mut doc, 0.0, vec![]);
            let point_at = |doc: &CodeDocument, index| {
                let galley = &doc.cache.as_ref().unwrap().galley;
                let a = galley.pos_from_ccursor(egui::text::CCursor::new(index));
                let b = galley.pos_from_ccursor(egui::text::CCursor::new(index + 1));
                doc.last_galley_pos.unwrap() + egui::vec2((a.left() + b.left()) / 2.0, a.center().y)
            };
            let event = |pos, pressed| egui::Event::PointerButton {
                pos,
                pressed,
                button: egui::PointerButton::Primary,
                modifiers: egui::Modifiers::NONE,
            };
            let point = point_at(&doc, 4);
            render(
                &mut doc,
                0.1,
                vec![egui::Event::PointerMoved(point), event(point, true)],
            );
            let output = render(&mut doc, 0.15, vec![event(point, false)]);
            assert_eq!(doc.selection_occurrences.len(), 2);
            let state = egui::text_edit::TextEditState::load(&context, doc.last_editor_id.unwrap())
                .unwrap();
            assert!(
                state.cursor.char_range().unwrap().primary
                    == state.cursor.char_range().unwrap().secondary,
                "highlight must not alter selection"
            );
            let tint = Color32::from_rgba_unmultiplied(40, 120, 180, 45);
            assert_eq!(
                painted_shapes(&output)
                    .into_iter()
                    .filter(|s| matches!(&s.shape, egui::Shape::Rect(rect) if rect.fill == tint))
                    .count(),
                2
            );
            render(&mut doc, 0.3, vec![]);
            assert_eq!(
                doc.selection_occurrences.len(),
                2,
                "persist across idle frames"
            );
            let punctuation = point_at(&doc, 16);
            render(
                &mut doc,
                1.0,
                vec![
                    egui::Event::PointerMoved(punctuation),
                    event(punctuation, true),
                ],
            );
            render(&mut doc, 1.1, vec![event(punctuation, false)]);
            assert!(
                doc.selection_occurrences.is_empty(),
                "punctuation must clear occurrences"
            );
        }
    }

    #[test]
    fn real_double_click_navigates_only_metadata_links() {
        for (syntax, source, linked) in [
            ("java", "🎯 café(); // café\n", "café"),
            ("java", "if (this.a.e()) {\n}\n", "e"),
            (
                "xml",
                "<!-- 🦀 --><a x=\"sample.Target\"/>",
                "sample.Target",
            ),
        ] {
            let byte = source.find(linked).unwrap();
            let start = source[..byte].chars().count();
            let end = start + linked.chars().count();
            for (index, expected) in [
                (start + usize::from(end - start > 1), Some(start)),
                (start - 1, None),
                (end + 1, None),
            ] {
                let context = egui::Context::default();
                let mut doc = CodeDocument::new(source.into(), syntax);
                doc.set_usages_enabled(syntax != "xml");
                doc.set_links(vec![CodeLink {
                    start,
                    end,
                    label: linked.into(),
                }]);
                let render = |doc: &mut CodeDocument, time: f64, events| {
                    let mut result = None;
                    let _ = context.run(
                        egui::RawInput {
                            time: Some(time),
                            events,
                            ..Default::default()
                        },
                        |ctx| {
                            egui::CentralPanel::default().show(ctx, |ui| {
                                result = doc.show(ui, CodeTheme::Ocean, 14.0);
                            });
                        },
                    );
                    result
                };
                render(&mut doc, 0.0, vec![]);
                let galley = &doc.cache.as_ref().unwrap().galley;
                let a = galley.pos_from_ccursor(egui::text::CCursor::new(index));
                let b = galley.pos_from_ccursor(egui::text::CCursor::new(index + 1));
                let point = doc.last_galley_pos.unwrap()
                    + egui::vec2((a.left() + b.left()) / 2.0, a.center().y);
                let event = |pressed| egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                };
                assert_eq!(
                    render(
                        &mut doc,
                        0.1,
                        vec![egui::Event::PointerMoved(point), event(true)]
                    ),
                    None
                );
                assert_eq!(render(&mut doc, 0.15, vec![event(false)]), None);
                render(&mut doc, 0.2, vec![event(true)]);
                assert_eq!(render(&mut doc, 0.25, vec![event(false)]), expected);
            }
        }
    }
    #[test]
    fn jumps_preserve_full_unicode_source_and_reject_invalid_positions() {
        let source = format!("{}class 🎯Café {{}}", "λ\n".repeat(90_000));
        let target = source.chars().count() - "Café {}".chars().count();
        let mut doc = CodeDocument::new(source, "java");
        doc.jump_to(target).unwrap();
        assert_eq!(doc.preview_start, 0);
        assert_eq!(doc.preview_end, doc.source.len());
        assert_eq!(doc.highlight, Some(target..target + 4));
        assert_eq!(
            doc.source[..doc.preview_start].chars().count(),
            doc.char_start
        );
        assert_eq!(doc.source.chars().nth(target), Some('C'));
        assert_eq!(
            doc.source[doc.preview_start..doc.preview_end]
                .chars()
                .nth(target - doc.char_start),
            Some('C')
        );
        assert!(doc.jump_to(usize::MAX).is_err());
        doc.jump_to(0).unwrap();
        assert_eq!(doc.preview_start, 0);
        assert_eq!(doc.line_start, 1);
    }
    #[test]
    fn metadata_hit_testing_observes_scalar_offsets_and_exact_glyph_bounds() {
        let context = egui::Context::default();
        let mut doc = CodeDocument::new("🎯 café(); // café\n".into(), "java");
        doc.set_links(vec![CodeLink {
            start: 2,
            end: 6,
            label: "café".into(),
        }]);
        let _ = context.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                doc.show(ui, CodeTheme::Ocean, 14.0);
                let galley = &doc.cache.as_ref().unwrap().galley;
                for index in 0..doc.source.chars().count() - 1 {
                    let a = galley.pos_from_ccursor(egui::text::CCursor::new(index));
                    let b = galley.pos_from_ccursor(egui::text::CCursor::new(index + 1));
                    let point = egui::vec2((a.left() + b.left()) / 2.0, a.center().y);
                    assert_eq!(
                        doc.link_at_pointer(galley, point).is_some(),
                        (2..6).contains(&index),
                        "index {index}"
                    );
                }
                assert!(
                    doc.link_at_pointer(galley, egui::vec2(galley.size().x + 50.0, 4.0))
                        .is_none()
                );
            });
        });
    }
    #[test]
    fn embedded_palettes_are_complete_readable_and_highlight_sources() {
        use syntect::highlighting::Color;
        fn luminance(c: Color) -> f64 {
            let linear = |v: u8| {
                let v = f64::from(v) / 255.0;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(c.r) + 0.7152 * linear(c.g) + 0.0722 * linear(c.b)
        }
        for theme in CodeTheme::ALL {
            assert_eq!(
                CodeTheme::from_preference(theme.preference_key()),
                Some(theme)
            );
            assert!(super::themes().themes.contains_key(theme.key()));
        }
        assert_eq!(CodeTheme::from_preference("missing"), None);
        for theme in [
            CodeTheme::AtomOneLight,
            CodeTheme::QuietLight,
            CodeTheme::OneDark,
            CodeTheme::Dracula,
        ] {
            let palette = &super::themes().themes[theme.key()];
            let bg = luminance(palette.settings.background.unwrap());
            assert_eq!(theme.is_light(), bg > 0.5);
            for foreground in std::iter::once(palette.settings.foreground.unwrap()).chain(
                palette
                    .scopes
                    .iter()
                    .filter_map(|scope| scope.style.foreground),
            ) {
                let fg = luminance(foreground);
                let contrast = (fg.max(bg) + 0.05) / (fg.min(bg) + 0.05);
                assert!(contrast >= 4.5, "{} contrast: {contrast}", theme.label());
            }
            for (syntax, text) in [
                (
                    "java",
                    "public class Demo { String s = \"hello\"; int n = 42; } // note",
                ),
                ("xml", "<item enabled=\"true\">hello</item>"),
                ("json", "{\"hello\": 42, \"ok\": true}"),
            ] {
                let doc = CodeDocument::new(text.into(), syntax);
                let job = doc.job(theme, 14.0);
                assert_eq!(job.text, text);
                assert!(
                    job.sections
                        .iter()
                        .any(|section| section.format.color != job.sections[0].format.color)
                );
            }
        }
    }
    #[test]
    fn preserves_unicode_crlf_and_multiline_literals() {
        let source = "class Café {\r\n String s = \"\"\"\r\nλ你好\r\n\"\"\";\r\n}\r\n";
        let doc = CodeDocument::new(source.to_owned(), "java");
        for theme in CodeTheme::ALL {
            assert_eq!(doc.text(), source);
            assert_eq!(doc.job(theme, 14.0).text, source);
        }
    }
    #[test]
    fn java_and_json_have_distinct_token_colors_and_themes() {
        for (syntax, source) in [
            (
                "java",
                "public class Demo { String s = \"hello\"; int n = 42; }",
            ),
            ("json", "{\"hello\": 42, \"ok\": true}"),
        ] {
            let doc = CodeDocument::new(source.into(), syntax);
            let job = doc.job(CodeTheme::Ocean, 14.0);
            let first = job.sections[0].format.color;
            assert!(job.sections.iter().any(|s| s.format.color != first));
            assert_ne!(
                job.sections[0].format.color,
                doc.job(CodeTheme::SolarizedLight, 14.0).sections[0]
                    .format
                    .color
            );
        }
    }
    #[test]
    fn reuses_layout_and_invalidates_on_theme_or_font_change() {
        let context = egui::Context::default();
        let mut doc = CodeDocument::new("class Demo {}".into(), "java");
        let render = |doc: &mut CodeDocument, theme, size| {
            let _ = context.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| doc.show(ui, theme, size));
            });
        };
        render(&mut doc, CodeTheme::Ocean, 14.0);
        let first = Arc::clone(&doc.cache.as_ref().unwrap().galley);
        render(&mut doc, CodeTheme::Ocean, 14.0);
        assert!(Arc::ptr_eq(&first, &doc.cache.as_ref().unwrap().galley));
        render(&mut doc, CodeTheme::SolarizedLight, 14.0);
        assert!(!Arc::ptr_eq(&first, &doc.cache.as_ref().unwrap().galley));
        assert_eq!(doc.cache.as_ref().unwrap().theme, CodeTheme::SolarizedLight);
        render(&mut doc, CodeTheme::SolarizedLight, 20.0);
        assert_eq!(doc.cache.as_ref().unwrap().size, 20.0);
        assert!(doc.retained_bytes() > doc.text().len());
    }
    #[test]
    fn full_document_exceeds_old_byte_and_line_limits() {
        let source = format!("{}\n", "a".repeat(100)).repeat(3_014);
        assert!(source.len() > 128 * 1024);
        let doc = CodeDocument::new(source.clone(), "txt");
        assert_eq!(doc.job(CodeTheme::Ocean, 14.0).text, source);
        assert_eq!(doc.job(CodeTheme::Ocean, 14.0).text.lines().count(), 3_014);
        let source = "λ".repeat(128 * 1024);
        let doc = CodeDocument::new(source.clone(), "java");
        assert_eq!(doc.text(), source);
        assert_eq!(doc.preview_end, source.len());
        assert!(doc.plain);
        assert_eq!(doc.job(CodeTheme::Ocean, 14.0).text, source);
        let doc = CodeDocument::new("x\n".repeat(10_000), "txt");
        assert_eq!(doc.job(CodeTheme::Ocean, 14.0).text.lines().count(), 10_000);
    }
}
