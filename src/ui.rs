use crate::code_fonts::CodeFont;
use crate::{
    code_view::{CodeDocument, CodeTheme},
    icons::{self, Icon},
    navigation::{self, Node, Target},
    search::{
        self, CompiledQuery, SearchHit, SearchQuery, SearchSummary, SearchTarget, SearchUpdate,
    },
    search_window::{SearchMode, SearchWindow},
    settings::{SearchPreferences, Settings, SettingsStore},
    usages::{self, UsageMode, UsageSummary, UsageUpdate},
    usages_window::UsagesWindow,
};
use eframe::egui;
use rdx::{
    apk::{Archive, Preview},
    engine::{DecompiledCode, DecompilerEngine, NativeEngine, NavigationResult, Project},
    plugin::Plugin,
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

enum Event {
    CallGraph(
        u64,
        NativeEngine,
        Result<rdx::call_graph::CallGraph, String>,
    ),
    Archive(u64, Arc<Archive>),
    Opened(u64, Result<(NativeEngine, Project), String>),
    Source(
        u64,
        String,
        Option<usize>,
        NativeEngine,
        Result<DecompiledCode, String>,
    ),
    Navigated(
        u64,
        JumpLocation,
        Option<NativeEngine>,
        Result<NavigationResult, String>,
    ),
    Asset(u64, usize, Result<Preview, String>),
    Resource(u64, usize, NativeEngine, Result<DecompiledCode, String>),
    ResourceRefresh(u64, usize, NativeEngine, Result<DecompiledCode, String>),
    Plugin(u64, Result<String, String>),
    Exported(u64, Option<NativeEngine>, Result<PathBuf, String>),
    SearchUpdate(u64, u64, SearchUpdate),
    SearchDone(u64, u64, NativeEngine, SearchSummary),
    SearchMetadata(
        u64,
        String,
        String,
        Option<NativeEngine>,
        Result<DecompiledCode, String>,
    ),
    InteractiveReady(u64, NativeEngine),
    UsagesUpdate(u64, u64, UsageUpdate),
    UsagesDone(u64, u64, NativeEngine, UsageSummary),
}

enum InteractiveRequest {
    Metadata(String, String),
    Navigate(String, usize, String),
    NavigateClass(String, JumpLocation),
}

fn replace_pending_navigation(requests: &mut Vec<InteractiveRequest>, request: InteractiveRequest) {
    requests.retain(|pending| {
        !matches!(
            pending,
            InteractiveRequest::Navigate(..) | InteractiveRequest::NavigateClass(..)
        )
    });
    requests.push(request);
}

impl InteractiveRequest {
    fn execute(
        self,
        engine: &mut NativeEngine,
        tx: &mpsc::Sender<Event>,
        generation: u64,
        ctx: &egui::Context,
    ) {
        let event = match self {
            Self::Metadata(name, hash) => {
                let result = engine
                    .decompile_with_metadata(&name)
                    .map_err(|e| format!("{e:#}"));
                Event::SearchMetadata(generation, name, hash, None, result)
            }
            Self::Navigate(class, position, hash) => {
                let result = engine
                    .navigate(&class, position, &hash)
                    .map_err(|e| format!("{e:#}"));
                Event::Navigated(
                    generation,
                    JumpLocation {
                        target: Target::Class(class),
                        position,
                        source_hash: Some(hash),
                    },
                    None,
                    result,
                )
            }
            Self::NavigateClass(class, origin) => {
                let result = engine.navigate_class(&class).map_err(|e| format!("{e:#}"));
                Event::Navigated(generation, origin, None, result)
            }
        };
        let _ = tx.send(event);
        ctx.request_repaint();
    }
}
enum Content {
    Text(Box<CodeDocument>),
    Image {
        texture: egui::TextureHandle,
        width: u32,
        height: u32,
    },
}
#[derive(Clone)]
struct JumpLocation {
    source_hash: Option<String>,
    target: Target,
    position: usize,
}
struct HistoryJump {
    forward: bool,
    destination: JumpLocation,
    origin: Option<JumpLocation>,
}
#[derive(Clone, Copy)]
enum TabAction {
    TogglePin,
    UnpinAll,
    ToggleBookmark,
    UnbookmarkAll,
    Close,
    CloseOthers,
    CloseAll,
}
struct Tab {
    pinned: bool,
    bookmarked: bool,
    loading_navigation: bool,
    source_hash: Option<String>,
    target: Target,
    name: String,
    content: Content,
    note: Option<String>,
}
impl Tab {
    fn reuse_search_hit(&mut self, hit: &SearchHit) -> Result<bool, String> {
        let snapshot = &hit.document;
        let target = match &snapshot.target {
            SearchTarget::Class(name) => Target::Class(name.clone()),
            SearchTarget::Resource(index) => Target::File(*index),
        };
        if self.target != target
            || snapshot.source_hash.is_none()
            || self.source_hash != snapshot.source_hash
        {
            return Ok(false);
        }
        let Content::Text(document) = &mut self.content else {
            return Ok(false);
        };
        if document.text() != snapshot.source {
            return Ok(false);
        }
        document.jump_to_range(hit.start, hit.end)?;
        Ok(true)
    }
    fn attach_search_metadata(
        &mut self,
        expected_hash: &str,
        code: DecompiledCode,
    ) -> Result<(), String> {
        let Content::Text(document) = &mut self.content else {
            return Err("Navigation metadata requires a source tab".into());
        };
        if self.source_hash.as_deref() != Some(expected_hash) {
            return Err("Navigation response belongs to an older tab.".into());
        }
        if code.source_hash != expected_hash {
            let relocated = document.refresh_search_source(code.source, code.links);
            self.source_hash = Some(code.source_hash);
            self.loading_navigation = false;
            self.note = (!relocated).then(|| "Source updated. Navigation is ready; the previous search match could not be uniquely relocated.".into());
            return Ok(());
        }
        if document.text() != code.source {
            return Err("Navigation response has inconsistent source identity.".into());
        }
        // Attach links in place: never replace the matched text, selection or scroll position.
        document.set_links(code.links);
        self.note = None;
        self.loading_navigation = false;
        Ok(())
    }
    fn retained_bytes(&self) -> usize {
        match &self.content {
            Content::Text(document) => document.retained_bytes(),
            Content::Image { width, height, .. } => *width as usize * *height as usize * 4,
        }
    }
    fn text(&self) -> Option<&str> {
        match &self.content {
            Content::Text(document) => Some(document.text()),
            _ => None,
        }
    }
}

pub struct App {
    mcp_server: Option<rdx::mcp::Server>,
    show_mcp: bool,
    mcp_error: String,
    mcp_auto_start: bool,
    search: SearchWindow,
    usages: UsagesWindow,
    call_graph: crate::call_graph_window::CallGraphWindow,
    usages_cancel: Option<Arc<AtomicBool>>,
    usages_id: u64,
    search_cache: Arc<Mutex<search::SearchCache>>,
    search_cancel: Option<Arc<AtomicBool>>,
    search_id: u64,
    pending_search_metadata: Vec<(String, String)>,
    interactive_requests: Arc<Mutex<Vec<InteractiveRequest>>>,
    settings_store: SettingsStore,
    last_preferences: Settings,
    tx: mpsc::Sender<Event>,
    rx: mpsc::Receiver<Event>,
    generation: u64,
    engine: Option<NativeEngine>,
    project: Option<Project>,
    archive: Option<Arc<Archive>>,
    tree: Node,
    path: Option<PathBuf>,
    initial: Option<PathBuf>,
    busy: bool,
    loading_project: bool,
    loading_progress: f32,
    asset_busy: bool,
    export_busy: bool,
    status: String,
    filter: String,
    tabs: Vec<Tab>,
    selected: usize,
    revealed_tab: Option<Target>,
    #[cfg(test)]
    tab_ui_controls: Vec<(String, egui::Rect)>,
    history: Vec<JumpLocation>,
    future: Vec<JumpLocation>,
    pending_history: Option<HistoryJump>,
    cancelled_history: Option<Target>,
    deferred_target: Option<Target>,
    theme: CodeTheme,
    font_size: f32,
    code_font: CodeFont,
    word_wrap: bool,
    plugins: Vec<(Plugin, bool)>,
    show_plugins: bool,
    plugin_busy: bool,
    plugin_output: String,
    diagnostics: Vec<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        let settings_store = SettingsStore::new();
        let (preferences, settings_error) = match settings_store.load() {
            Ok(preferences) => (preferences, None),
            Err(error) => (Settings::default(), Some(error)),
        };
        let appearance = restored_interface_theme(&preferences);
        let theme = restored_code_theme(&preferences);
        let font_size = preferences.font_size;
        let code_font = CodeFont::from_preference(&preferences.code_font).unwrap_or_default();
        crate::code_fonts::install(&cc.egui_ctx);
        let word_wrap = preferences.word_wrap;
        cc.egui_ctx.set_theme(appearance);
        let mut search = SearchWindow::default();
        search.restore_preferences(&preferences.search);
        let mut usages = UsagesWindow::default();
        usages.keep_open = preferences.usages_keep_open;
        let last_preferences = snapshot_preferences(
            &cc.egui_ctx,
            theme,
            font_size,
            code_font,
            word_wrap,
            search.preferences(),
            usages.keep_open,
        );
        cc.egui_ctx.all_styles_mut(|style| {
            style
                .text_styles
                .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
            style
                .text_styles
                .insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
        });
        let (tx, rx) = mpsc::channel();
        Self {
            mcp_server: None,
            show_mcp: false,
            mcp_error: String::new(),
            mcp_auto_start: std::env::var_os("RDX_MCP_AUTO_START").is_some(),
            search,
            usages,
            call_graph: crate::call_graph_window::CallGraphWindow::default(),
            usages_cancel: None,
            usages_id: 0,
            search_cache: Arc::new(Mutex::new(search::SearchCache::default())),
            search_cancel: None,
            search_id: 0,
            pending_search_metadata: Vec::new(),
            interactive_requests: Arc::new(Mutex::new(Vec::new())),
            settings_store,
            last_preferences,
            tx,
            rx,
            generation: 0,
            engine: None,
            project: None,
            archive: None,
            tree: Node::default(),
            path: None,
            initial,
            busy: false,
            loading_project: false,
            loading_progress: 0.0,
            asset_busy: false,
            export_busy: false,
            status: if settings_error.is_some() {
                "Settings could not be restored — using defaults. See Diagnostics.".into()
            } else {
                "Ready — open an APK or DEX".into()
            },
            filter: String::new(),
            tabs: Vec::new(),
            selected: 0,
            revealed_tab: None,
            #[cfg(test)]
            tab_ui_controls: Vec::new(),
            history: Vec::new(),
            future: Vec::new(),
            pending_history: None,
            cancelled_history: None,
            deferred_target: None,
            theme,
            font_size,
            code_font,
            word_wrap,
            plugins: Vec::new(),
            show_plugins: false,
            plugin_busy: false,
            plugin_output: String::new(),
            diagnostics: settings_error.into_iter().collect(),
        }
    }
    fn start_mcp(&mut self) {
        if let Some(path) = self.path.clone() {
            match rdx::mcp::Server::start(path) {
                Ok(server) => {
                    self.mcp_server = Some(server);
                    self.mcp_error.clear();
                }
                Err(error) => self.mcp_error = format!("{error:#}"),
            }
        }
    }

    fn mcp_panel(&mut self, ctx: &egui::Context) {
        if !self.show_mcp {
            return;
        }
        let mut open = self.show_mcp;
        egui::Window::new("MCP Server").open(&mut open).default_width(540.0).show(ctx, |ui| {
            ui.label("Connect agents to this RDX instance. Each window has its own project.");
            if let Some(server) = &self.mcp_server {
                let status = server.status();
                ui.label(format!("Status: {}", status.state));
                ui.label(format!("Instance: {}", server.instance_id));
                ui.label(format!("Project: {}", status.project_id));
                ui.label(format!("APK SHA-256: {}", status.apk_sha256));
                ui.label(format!("Requests served: {}", status.requests));
                if !status.active.is_empty() { ui.label(format!("Current request: {}", status.active)); if ui.button("Cancel current request").clicked() { server.cancel_request(); } }
                if !status.last_error.is_empty() { ui.colored_label(ui.visuals().error_fg_color, &status.last_error); }
                if ui.button("Stop server").clicked() { self.mcp_server = None; }
                ctx.request_repaint_after(std::time::Duration::from_millis(300));
            } else {
                ui.label("Status: Stopped");
                if ui.add_enabled(self.project.is_some() && !self.loading_project, egui::Button::new("Start server")).clicked() { self.start_mcp(); }
                if self.project.is_none() { ui.label("Open an APK or DEX to enable this instance."); }
            }
            if ui.button("Copy MCP client configuration").clicked() {
                match rdx::mcp::client_config() { Ok(config) => ui.ctx().copy_text(config), Err(e) => self.mcp_error=e.to_string() }
            }
            if !self.mcp_error.is_empty() { ui.colored_label(ui.visuals().error_fg_color, &self.mcp_error); }
            ui.separator();
            ui.label("Available: classes, source, DEX, methods, fields, resource strings, DEX strings, manifest and direct subclasses.");
            ui.label("Opening or reloading a project stops its server. Start it again to share the new project.");
        });
        self.show_mcp = open;
    }

    fn open_file_find(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.selected)
            && let Content::Text(document) = &mut tab.content
        {
            document.open_find();
        }
    }

    fn persist_preferences(&mut self, ctx: &egui::Context) {
        let preferences = snapshot_preferences(
            ctx,
            self.theme,
            self.font_size,
            self.code_font,
            self.word_wrap,
            self.search.preferences(),
            self.usages.keep_open,
        );
        if preferences == self.last_preferences {
            return;
        }
        // Commit a font drag when released rather than syncing disk for every pixel moved.
        if preferences.font_size != self.last_preferences.font_size
            && ctx.input(|input| input.pointer.primary_down())
        {
            return;
        }
        // Write only when a preference changes, never on ordinary repaint frames.
        self.last_preferences = preferences.clone();
        if let Err(error) = self.settings_store.save(&preferences) {
            self.error(format!("Settings could not be saved: {error}"));
        }
    }
    fn rebuild_tree(&mut self) {
        self.tree = navigation::build(
            self.project.as_ref().map_or(&[], |p| p.classes.as_slice()),
            self.archive.as_ref().map_or(&[], |a| a.entries.as_slice()),
            &self.filter,
        );
    }
    fn stop(&mut self) {
        self.pending_search_metadata.clear();
        self.interactive_requests = Arc::new(Mutex::new(Vec::new()));
        if let Some(cancel) = self.usages_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        if self.usages.running {
            self.usages
                .finish("Results search stopped — partial results".into(), vec![]);
        }
        if let Some(cancel) = self.search_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        if self.search.running {
            self.search.running = false;
            self.search.status = "Search stopped".into();
        }
        if let Some(cancel) = &self.call_graph.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        self.call_graph = crate::call_graph_window::CallGraphWindow::default();
        self.generation += 1;
        self.search_cache = Arc::new(Mutex::new(search::SearchCache::default()));
        self.engine = None;
        self.busy = false;
        self.loading_project = false;
        self.loading_progress = 0.0;
        self.asset_busy = false;
        self.export_busy = false;
    }
    fn open(&mut self, path: PathBuf, ctx: &egui::Context) {
        self.mcp_server = None;
        self.stop();
        self.project = None;
        self.archive = None;
        self.search.results.clear();
        let keep_open = self.usages.keep_open;
        self.usages = UsagesWindow::default();
        self.usages.keep_open = keep_open;
        self.search.status.clear();
        self.tree = Node::default();
        self.tabs.clear();
        self.history.clear();
        self.future.clear();
        self.pending_history = None;
        self.cancelled_history = None;
        self.deferred_target = None;
        self.selected = 0;
        self.revealed_tab = None;
        self.filter.clear();
        self.plugin_output.clear();
        self.path = Some(path.clone());
        self.busy = true;
        self.loading_project = true;
        self.loading_progress = 0.05;
        self.status = "Indexing APK/DEX with native Rust…".into();
        let (tx, generation, ctx) = (self.tx.clone(), self.generation, ctx.clone());
        thread::spawn(move || {
            let result = (|| {
                if path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("apk"))
                {
                    let archive = Arc::new(Archive::open(&path)?);
                    let _ = tx.send(Event::Archive(generation, archive));
                    ctx.request_repaint();
                }
                let mut engine = NativeEngine::start()?;
                ctx.request_repaint();
                let project = engine.open(&path)?;
                Ok((engine, project))
            })()
            .map_err(|e: anyhow::Error| format!("{e:#}"));
            let _ = tx.send(Event::Opened(generation, result));
            ctx.request_repaint();
        });
    }
    fn choose_target(&mut self, target: Target, ctx: &egui::Context) {
        if let Some(pending) = self.pending_history.take() {
            if pending.destination.target == target {
                self.pending_history = Some(pending);
                return;
            }
            self.cancelled_history = Some(pending.destination.target);
        }
        if self.cancelled_history.is_some() {
            if let Some(index) = self.tabs.iter().position(|tab| tab.target == target) {
                self.selected = index;
                self.deferred_target = None;
            } else {
                self.deferred_target = Some(target);
            }
            return;
        }
        if let Some(index) = self.tabs.iter().position(|tab| tab.target == target) {
            self.selected = index;
            return;
        }
        match target {
            Target::Class(name) => self.decompile(name, None, ctx),
            Target::File(index) => self.open_asset(index, ctx),
        }
    }
    fn decompile(&mut self, name: String, position: Option<usize>, ctx: &egui::Context) {
        let Some(mut engine) = self.engine.take() else {
            return;
        };
        self.busy = true;
        self.status = format!("Decompiling {name}…");
        let (tx, generation, ctx) = (self.tx.clone(), self.generation, ctx.clone());
        thread::spawn(move || {
            let result = engine
                .decompile_with_metadata(&name)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Event::Source(generation, name, position, engine, result));
            ctx.request_repaint();
        });
    }
    fn display_code(
        &mut self,
        name: String,
        code: DecompiledCode,
        position: Option<usize>,
    ) -> Result<(), String> {
        let resource = name.starts_with(rdx::resource_table::PREFIX);
        let mut document = CodeDocument::new(code.source, if resource { "xml" } else { "java" });
        document.set_links(code.links);
        if let Some(position) = position {
            document.jump_to(position)?;
        }
        self.push_tab(Tab {
            pinned: false,
            bookmarked: false,
            loading_navigation: false,
            source_hash: Some(code.source_hash),
            target: Target::Class(name.clone()),
            name,
            content: Content::Text(Box::new(document)),
            note: None,
        })
    }
    fn build_call_graph(
        &mut self,
        class: String,
        offset: usize,
        hash: String,
        ctx: &egui::Context,
    ) {
        self.call_graph.visible = true;
        let Some(mut engine) = self.engine.take() else {
            self.call_graph.status = "Wait for the current engine operation, then retry.".into();
            return;
        };
        self.busy = true;
        self.call_graph.origin = Some((class.clone(), offset, hash.clone()));
        self.call_graph.graph = None;
        self.call_graph.status = "Building call graph…".into();
        let cancel = Arc::new(AtomicBool::new(false));
        self.call_graph.cancel = Some(cancel.clone());
        let (tx, generation, ctx, depth) = (
            self.tx.clone(),
            self.generation,
            ctx.clone(),
            self.call_graph.depth,
        );
        thread::spawn(move || {
            let result = engine
                .call_graph_at(&class, offset, &hash, depth, &cancel)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Event::CallGraph(generation, engine, result));
            ctx.request_repaint();
            ctx.request_repaint_of(crate::call_graph_window::CallGraphWindow::viewport_id());
        });
    }
    fn navigate_graph_method(&mut self, method: String, ctx: &egui::Context) {
        let Some(mut engine) = self.engine.take() else {
            self.call_graph.status =
                "Wait for the current engine operation, then click the method again.".into();
            return;
        };
        self.busy = true;
        let origin = self.current_location().unwrap_or(JumpLocation {
            target: Target::Class(rdx::call_graph::owner(&method).unwrap_or_default().into()),
            position: 0,
            source_hash: None,
        });
        let (tx, generation, ctx) = (self.tx.clone(), self.generation, ctx.clone());
        thread::spawn(move || {
            let result = engine
                .navigate_graph_method(&method)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Event::Navigated(generation, origin, Some(engine), result));
            ctx.request_repaint();
        });
    }
    fn find_usages(
        &mut self,
        class: String,
        offset: usize,
        hash: String,
        mode: UsageMode,
        ctx: &egui::Context,
    ) {
        let Some(mut engine) = self.engine.take() else {
            self.usages.visible = true;
            self.usages
                .error("Wait for the current engine operation".into());
            return;
        };
        self.busy = true;
        match mode {
            UsageMode::Subclasses => self.usages.begin_subclasses(class.clone()),
            UsageMode::Implementations => self.usages.begin_implementations(class.clone()),
            UsageMode::Usages => self.usages.begin(class.clone()),
            UsageMode::Callers | UsageMode::Callees => {
                self.usages.begin_method_xrefs(class.clone(), mode)
            }
        }
        self.status = self.usages.status.clone();
        self.usages_id += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        self.usages_cancel = Some(cancel.clone());
        let (tx, generation, id, ctx) = (
            self.tx.clone(),
            self.generation,
            self.usages_id,
            ctx.clone(),
        );
        thread::spawn(move || {
            let emit = |update| {
                let _ = tx.send(Event::UsagesUpdate(generation, id, update));
                ctx.request_repaint();
                ctx.request_repaint_of(UsagesWindow::viewport_id());
            };
            let summary = match mode {
                UsageMode::Subclasses => {
                    usages::collect_subclasses(&mut engine, &class, offset, &hash, &cancel, emit)
                }
                UsageMode::Implementations => usages::collect_implementations(
                    &mut engine,
                    &class,
                    offset,
                    &hash,
                    &cancel,
                    emit,
                ),
                UsageMode::Usages => {
                    usages::collect(&mut engine, &class, offset, &hash, &cancel, emit)
                }
                UsageMode::Callers | UsageMode::Callees => usages::collect_method_xrefs(
                    &mut engine,
                    &class,
                    offset,
                    &hash,
                    &cancel,
                    mode,
                    emit,
                ),
            };
            let _ = tx.send(Event::UsagesDone(generation, id, engine, summary));
            ctx.request_repaint();
        });
    }
    fn navigate(&mut self, class: String, position: usize, hash: String, ctx: &egui::Context) {
        let Some(mut engine) = self.engine.take() else {
            if self.search_cancel.is_some() {
                let mut requests = self
                    .interactive_requests
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                // Keep at most one pending jump: the most recent user action wins.
                replace_pending_navigation(
                    &mut requests,
                    InteractiveRequest::Navigate(class, position, hash),
                );
                self.status = "Resolving declaration after the current search batch…".into();
            }
            return;
        };
        self.busy = true;
        self.status = "Resolving declaration…".into();
        let (tx, generation, ctx) = (self.tx.clone(), self.generation, ctx.clone());
        thread::spawn(move || {
            let result = engine
                .navigate(&class, position, &hash)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Event::Navigated(
                generation,
                JumpLocation {
                    target: Target::Class(class),
                    position,
                    source_hash: Some(hash),
                },
                Some(engine),
                result,
            ));
            ctx.request_repaint();
        });
    }
    fn navigate_manifest(&mut self, class: String, origin: JumpLocation, ctx: &egui::Context) {
        let Some(mut engine) = self.engine.take() else {
            if self.search_cancel.is_some() {
                let mut requests = self
                    .interactive_requests
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                // Class and source jumps share one latest-user-navigation slot.
                replace_pending_navigation(
                    &mut requests,
                    InteractiveRequest::NavigateClass(class, origin),
                );
                self.status = "Opening manifest component after the current search batch…".into();
            }
            return;
        };
        self.busy = true;
        self.status = format!("Opening {class}…");
        let (tx, generation, ctx) = (self.tx.clone(), self.generation, ctx.clone());
        thread::spawn(move || {
            let result = engine.navigate_class(&class).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Event::Navigated(generation, origin, Some(engine), result));
            ctx.request_repaint();
        });
    }
    fn refresh_manifest_links(&mut self) {
        let classes = self
            .project
            .as_ref()
            .map_or(&[][..], |p| p.classes.as_slice());
        for tab in &mut self.tabs {
            if tab.name == "AndroidManifest.xml"
                && let Content::Text(document) = &mut tab.content
            {
                let mut links = document.resource_links();
                links.extend(crate::manifest_links::links(document.text(), classes));
                document.set_links(links);
                document.set_exported_components(crate::manifest_links::exported_components(
                    document.text(),
                ));
            }
        }
    }
    fn discard_cancelled_history(&mut self, target: &Target, ctx: &egui::Context) -> bool {
        if self.cancelled_history.as_ref() != Some(target) {
            return false;
        }
        self.cancelled_history = None;
        if let Some(target) = self.deferred_target.take() {
            self.choose_target(target, ctx);
        }
        true
    }
    fn current_location(&self) -> Option<JumpLocation> {
        let tab = self.tabs.get(self.selected)?;
        Some(JumpLocation {
            target: tab.target.clone(),
            source_hash: tab.source_hash.clone(),
            position: match &tab.content {
                Content::Text(document) => document.navigation_position(),
                Content::Image { .. } => 0,
            },
        })
    }
    fn can_history(&self, forward: bool) -> bool {
        !self.busy
            && !self.asset_busy
            && self.pending_history.is_none()
            && self.cancelled_history.is_none()
            && !(if forward { &self.future } else { &self.history }).is_empty()
    }
    fn go_back(&mut self, ctx: &egui::Context) {
        self.go_history(false, ctx);
    }
    fn go_forward(&mut self, ctx: &egui::Context) {
        self.go_history(true, ctx);
    }
    fn finish_history(&mut self) {
        let Some(pending) = self.pending_history.take() else {
            return;
        };
        let Some(tab) = self.tabs.get_mut(self.selected) else {
            return;
        };
        if tab.target != pending.destination.target
            || tab.source_hash != pending.destination.source_hash
        {
            self.error(
                "Source changed after cache eviction; reopen the reference before navigating"
                    .into(),
            );
            return;
        }
        if let Content::Text(document) = &mut tab.content
            && !document.text().is_empty()
            && let Err(error) = document.jump_to(pending.destination.position)
        {
            self.error(error);
            return;
        }
        let (from, to) = if pending.forward {
            (&mut self.future, &mut self.history)
        } else {
            (&mut self.history, &mut self.future)
        };
        from.pop();
        if let Some(origin) = pending.origin {
            to.push(origin);
            if to.len() > 100 {
                to.remove(0);
            }
        }
    }
    fn go_history(&mut self, forward: bool, ctx: &egui::Context) {
        if !self.can_history(forward) {
            return;
        }
        let location = (if forward { &self.future } else { &self.history })
            .last()
            .unwrap()
            .clone();
        let cached = self.tabs.iter().position(|tab| {
            tab.target == location.target && tab.source_hash == location.source_hash
        });
        if cached.is_none()
            && match &location.target {
                Target::Class(_) => self.engine.is_none(),
                Target::File(_) => self.archive.is_none(),
            }
        {
            return;
        }
        self.pending_history = Some(HistoryJump {
            forward,
            destination: location.clone(),
            origin: self.current_location(),
        });
        if let Some(index) = cached {
            self.selected = index;
            self.finish_history();
        } else {
            match location.target {
                Target::Class(class) => self.decompile(class, Some(location.position), ctx),
                Target::File(index) => self.open_asset(index, ctx),
            }
        }
    }
    fn export_target(&mut self, target: Target, ctx: &egui::Context) {
        if self.export_busy {
            return;
        }
        let cached = self
            .tabs
            .iter()
            .find(|tab| tab.target == target)
            .and_then(|tab| tab.text())
            .map(str::to_owned);
        if matches!(target, Target::Class(_)) && cached.is_none() && self.engine.is_none() {
            self.error(
                "Wait for the engine or reload the project before exporting this class".into(),
            );
            return;
        }
        let Some(directory) = rfd::FileDialog::new()
            .set_title("Choose export folder")
            .pick_folder()
        else {
            return;
        };
        let mut engine = if matches!(target, Target::Class(_)) && cached.is_none() {
            self.engine.take()
        } else {
            None
        };
        if engine.is_some() {
            self.busy = true;
        }
        self.export_busy = true;
        self.status = "Exporting…".into();
        let archive = self.archive.clone();
        let (tx, generation, ctx) = (self.tx.clone(), self.generation, ctx.clone());
        thread::spawn(move || {
            let result = (|| -> anyhow::Result<PathBuf> {
                match target {
                    Target::File(index) => archive
                        .ok_or_else(|| anyhow::anyhow!("APK archive is unavailable"))?
                        .export(index, &directory),
                    Target::Class(name) => {
                        let source = match cached {
                            Some(source) => source,
                            None => engine
                                .as_mut()
                                .ok_or_else(|| anyhow::anyhow!("Decompiler is unavailable"))?
                                .decompile(&name)?,
                        };
                        let filename = if name.starts_with(rdx::resource_table::PREFIX) {
                            let last = name.rsplit('/').next().unwrap_or("resource");
                            if last.ends_with(".xml") {
                                last.to_owned()
                            } else {
                                format!("{last}.xml")
                            }
                        } else if name.starts_with("dex://") {
                            format!("{}.smali", name.rsplit('.').next().unwrap_or(&name))
                        } else {
                            format!("{}.java", name.rsplit('.').next().unwrap_or(&name))
                        };
                        rdx::export::write_source(&directory, &filename, &source)
                    }
                }
            })()
            .map_err(|error| format!("{error:#}"));
            let _ = tx.send(Event::Exported(generation, engine, result));
            ctx.request_repaint();
        });
    }
    fn start_search(&mut self, query: SearchQuery, ctx: &egui::Context) {
        let query = match CompiledQuery::new(query) {
            Ok(query) => query,
            Err(error) => {
                self.search.error(error);
                return;
            }
        };
        let Some(project) = &self.project else {
            self.search.error("Open an APK or DEX first".into());
            return;
        };
        let Some(mut engine) = self.engine.take() else {
            self.search
                .error("Wait for the current engine operation".into());
            return;
        };
        let classes = project.classes.clone();
        let archive = self.archive.clone();
        self.search_id += 1;
        self.search.begin();
        self.busy = true;
        self.status = "Searching project…".into();
        let search_cache = self.search_cache.clone();
        let interactive_requests = self.interactive_requests.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.search_cancel = Some(cancel.clone());
        let (tx, generation, search_id, ctx) = (
            self.tx.clone(),
            self.generation,
            self.search_id,
            ctx.clone(),
        );
        thread::spawn(move || {
            let mut cache = search_cache
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let summary = search::run_search_interactive(
                &mut engine,
                &mut cache,
                generation,
                &classes,
                archive.as_deref(),
                &query,
                &cancel,
                |update| {
                    let _ = tx.send(Event::SearchUpdate(generation, search_id, update));
                    SearchWindow::wake(&ctx);
                },
                |engine| {
                    let request = interactive_requests
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .pop();
                    if let Some(request) = request {
                        request.execute(engine, &tx, generation, &ctx);
                    }
                },
            );
            let _ = tx.send(Event::SearchDone(generation, search_id, engine, summary));
            SearchWindow::wake(&ctx);
        });
    }
    fn open_search_hit(&mut self, hit: SearchHit) {
        if let Some(pending) = self.pending_history.take() {
            self.cancelled_history = Some(pending.destination.target);
        }
        self.deferred_target = None;
        let mut reused = false;
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            match tab.reuse_search_hit(&hit) {
                Ok(true) => {
                    self.selected = index;
                    self.future.clear();
                    self.status = format!("Search match · {}:{}", hit.document.name, hit.line);
                    if tab.note.is_none() {
                        return;
                    }
                    reused = true;
                    break;
                }
                Ok(false) => {}
                Err(error) => {
                    self.error(error);
                    return;
                }
            }
        }
        let snapshot = &hit.document;
        let needs_metadata = (!snapshot.metadata_complete || snapshot.links.is_empty())
            && matches!(snapshot.target, SearchTarget::Class(_));
        if needs_metadata && let Some(hash) = &snapshot.source_hash {
            self.pending_search_metadata
                .retain(|(name, _)| name != &snapshot.name);
            self.pending_search_metadata
                .push((snapshot.name.clone(), hash.clone()));
            if self.pending_search_metadata.len() > 8 {
                self.pending_search_metadata.remove(0);
            }
        }
        if reused {
            return;
        }
        let mut document = CodeDocument::new(snapshot.source.clone(), &snapshot.syntax);
        document.set_links(snapshot.links.clone());
        if let Err(error) = document.jump_to_range(hit.start, hit.end) {
            self.error(error);
            return;
        }
        let target = match &snapshot.target {
            SearchTarget::Class(name) => Target::Class(name.clone()),
            SearchTarget::Resource(index) => Target::File(*index),
        };
        if let Err(error) = self.push_tab(Tab {
            pinned: false,
            bookmarked: false,
            loading_navigation: needs_metadata,
            source_hash: snapshot.source_hash.clone(),
            target,
            name: snapshot.name.clone(),
            content: Content::Text(Box::new(document)),
            note: None,
        }) {
            self.error(error);
            return;
        }
        self.future.clear();
        self.status = if needs_metadata {
            format!("Opening result · {}", snapshot.name)
        } else {
            format!("Search match · {}:{}", snapshot.name, hit.line)
        };
    }
    fn load_pending_search_metadata(&mut self, ctx: &egui::Context) {
        if self.busy && self.search_cancel.is_none() {
            return;
        }
        while let Some((name, hash)) = self.pending_search_metadata.pop() {
            if self.tabs.iter().any(|tab| {
                tab.target == Target::Class(name.clone()) && tab.source_hash.as_ref() == Some(&hash)
            }) {
                let mut requests = self
                    .interactive_requests
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                requests.retain(|r| !matches!(r, InteractiveRequest::Metadata(n, _) if n == &name));
                if requests.len() >= 9 {
                    requests.remove(0);
                }
                requests.push(InteractiveRequest::Metadata(name, hash));
            }
        }
        if self.engine.is_none() {
            return;
        }
        let request = self
            .interactive_requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop();
        let Some(request) = request else {
            return;
        };
        let mut engine = self.engine.take().expect("checked above");
        self.busy = true;
        let (tx, generation, ctx) = (self.tx.clone(), self.generation, ctx.clone());
        thread::spawn(move || {
            request.execute(&mut engine, &tx, generation, &ctx);
            let _ = tx.send(Event::InteractiveReady(generation, engine));
            ctx.request_repaint();
        });
    }
    fn entry_name(&self, index: usize) -> Option<String> {
        self.archive
            .as_ref()?
            .entries
            .iter()
            .find(|e| e.index == index)
            .map(|e| e.path.clone())
    }
    fn open_asset(&mut self, index: usize, ctx: &egui::Context) {
        let Some(archive) = self.archive.clone() else {
            return;
        };
        self.asset_busy = true;
        let (tx, generation, ctx) = (self.tx.clone(), self.generation, ctx.clone());
        thread::spawn(move || {
            let result = archive.preview(index).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Event::Asset(generation, index, result));
            ctx.request_repaint();
        });
    }
    fn decode_resource(&mut self, index: usize, ctx: &egui::Context) {
        self.decode_resource_mode(index, ctx, false);
    }
    fn decode_resource_mode(&mut self, index: usize, ctx: &egui::Context, refresh: bool) {
        let Some(name) = self.entry_name(index) else {
            return;
        };
        let Some(mut engine) = self.engine.take() else {
            return;
        };
        self.busy = true;
        self.status = format!("Decoding {name}…");
        let (tx, generation, ctx) = (self.tx.clone(), self.generation, ctx.clone());
        thread::spawn(move || {
            let result = engine
                .read_resource_with_metadata(&name)
                .map_err(|e| format!("{e:#}"));
            let event = if refresh {
                Event::ResourceRefresh(generation, index, engine, result)
            } else {
                Event::Resource(generation, index, engine, result)
            };
            let _ = tx.send(event);
            ctx.request_repaint();
        });
    }
    fn select_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        if let Some(pending) = self.pending_history.take() {
            self.cancelled_history = Some(pending.destination.target);
        }
        self.deferred_target = None;
        self.selected = index;
    }
    fn tab_action(&mut self, index: usize, action: TabAction) {
        if index >= self.tabs.len() {
            return;
        }
        match action {
            TabAction::TogglePin => self.tabs[index].pinned = !self.tabs[index].pinned,
            TabAction::UnpinAll => self.tabs.iter_mut().for_each(|tab| tab.pinned = false),
            TabAction::ToggleBookmark => self.tabs[index].bookmarked = !self.tabs[index].bookmarked,
            TabAction::UnbookmarkAll => self.tabs.iter_mut().for_each(|tab| tab.bookmarked = false),
            TabAction::Close | TabAction::CloseOthers | TabAction::CloseAll => {
                if let Some(pending) = self.pending_history.take() {
                    self.cancelled_history = Some(pending.destination.target);
                }
                self.deferred_target = None;
                let selected = self.tabs.get(self.selected).map(|tab| tab.target.clone());
                let context = self.tabs[index].target.clone();
                self.tabs.retain(|tab| match action {
                    TabAction::Close => tab.target != context,
                    TabAction::CloseOthers => tab.pinned || tab.target == context,
                    TabAction::CloseAll => tab.pinned,
                    _ => unreachable!(),
                });
                self.selected = selected
                    .and_then(|selected| self.tabs.iter().position(|tab| tab.target == selected))
                    .unwrap_or(self.selected.min(self.tabs.len().saturating_sub(1)));
            }
        }
    }
    fn push_tab(&mut self, mut tab: Tab) -> Result<(), String> {
        const BUDGET: usize = 64 * 1024 * 1024;
        let existing = self.tabs.iter().position(|old| old.target == tab.target);
        if let Some(index) = existing {
            tab.pinned = self.tabs[index].pinned;
            tab.bookmarked = self.tabs[index].bookmarked;
        }
        if tab.retained_bytes() > BUDGET {
            return Err(
                "View exceeds the 64 MiB tab budget; export it to inspect externally".into(),
            );
        }
        // Plan before mutating: pin-blocked admission preserves every tab and selection.
        let mut retained: usize = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(index, _)| Some(*index) != existing)
            .map(|(_, tab)| tab.retained_bytes())
            .sum();
        let mut count = self.tabs.len() - usize::from(existing.is_some());
        let mut evict = Vec::new();
        for (index, old) in self.tabs.iter().enumerate() {
            if count < 8 && retained + tab.retained_bytes() <= BUDGET {
                break;
            }
            if Some(index) != existing && !old.pinned {
                count -= 1;
                retained -= old.retained_bytes();
                evict.push(index);
            }
        }
        if count >= 8 || retained + tab.retained_bytes() > BUDGET {
            return Err("Pinned views fill the tab budget (8 views / 64 MiB). Unpin or close a view before opening another".into());
        }
        if let Some(index) = existing {
            evict.push(index);
        }
        evict.sort_unstable();
        for index in evict.into_iter().rev() {
            self.tabs.remove(index);
        }
        self.tabs.push(tab);
        self.selected = self.tabs.len() - 1;
        self.refresh_manifest_links();
        if self.pending_history.as_ref().is_some_and(|pending| {
            matches!(pending.destination.target, Target::File(_))
                && self.tabs[self.selected].target == pending.destination.target
        }) {
            self.finish_history();
        }
        Ok(())
    }
    fn tab_bar(&mut self, ui: &mut egui::Ui) -> Option<Target> {
        let mut export = None;
        #[cfg(test)]
        self.tab_ui_controls.clear();
        let mut action = None;
        let mut selected = None;
        let selected_target = self.tabs.get(self.selected).map(|tab| tab.target.clone());
        let reveal = self.revealed_tab != selected_target;
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let views = ui
                    .menu_button("▾", |ui| {
                        let width = (ui.ctx().screen_rect().width() - 32.0).clamp(120.0, 420.0);
                        ui.set_min_width(width.min(260.0));
                        ui.set_max_width(width);
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                        egui::ScrollArea::vertical()
                            .max_height((ui.ctx().screen_rect().height() - 80.0).max(80.0))
                            .show(ui, |ui| {
                                for (index, tab) in self.tabs.iter().enumerate() {
                                    ui.horizontal(|ui| {
                                        if tab.pinned {
                                            icons::small(ui, Icon::Pin);
                                        }
                                        if tab.bookmarked {
                                            icons::small(ui, Icon::Bookmark);
                                        }
                                        let label = format!(
                                            "{}{}",
                                            if index == self.selected { "✓ " } else { "" },
                                            tab.name
                                        );
                                        let response = ui
                                            .selectable_label(index == self.selected, label)
                                            .on_hover_text(&tab.name);
                                        #[cfg(test)]
                                        self.tab_ui_controls
                                            .push((format!("view:{}", tab.name), response.rect));
                                        if response.clicked() {
                                            selected = Some(index);
                                            ui.close_menu();
                                        }
                                    });
                                }
                            });
                    })
                    .response
                    .on_hover_text("Open views — all tabs, including offscreen views");
                #[cfg(test)]
                self.tab_ui_controls.push(("views".into(), views.rect));
                #[cfg(not(test))]
                let _ = views;
                egui::ScrollArea::horizontal()
                    .id_salt("tabs")
                    .max_width(ui.available_width())
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for (i, tab) in self.tabs.iter().enumerate() {
                                if tab.pinned {
                                    icons::small(ui, Icon::Pin);
                                }
                                if tab.bookmarked {
                                    icons::small(ui, Icon::Bookmark);
                                }
                                let label = match tab.target {
                                    Target::Class(_)
                                        if tab.name.starts_with(rdx::resource_table::PREFIX) =>
                                    {
                                        tab.name.rsplit('/').next()
                                    }
                                    Target::Class(_) => tab.name.rsplit('.').next(),
                                    Target::File(_) => tab.name.rsplit('/').next(),
                                }
                                .unwrap_or(&tab.name);
                                let label = if tab.name.starts_with("dex://") {
                                    format!("{label} [DEX]")
                                } else {
                                    label.to_owned()
                                };
                                let response = ui
                                    .selectable_label(self.selected == i, label)
                                    .on_hover_text(&tab.name);
                                if reveal && self.selected == i {
                                    response.scroll_to_me(Some(egui::Align::Center));
                                }
                                if response.clicked() {
                                    selected = Some(i);
                                }
                                response.context_menu(|ui| {
                                    if ui.button("Copy Name").clicked() {
                                        ui.ctx().copy_text(tab.name.clone());
                                        ui.close_menu();
                                    }
                                    ui.separator();
                                    for (label, operation) in [
                                        (
                                            if tab.pinned { "Unpin" } else { "Pin" },
                                            TabAction::TogglePin,
                                        ),
                                        ("Unpin All", TabAction::UnpinAll),
                                        (
                                            if tab.bookmarked {
                                                "Unbookmark"
                                            } else {
                                                "Bookmark"
                                            },
                                            TabAction::ToggleBookmark,
                                        ),
                                        ("Unbookmark All", TabAction::UnbookmarkAll),
                                    ] {
                                        if ui.button(label).clicked() {
                                            action = Some((i, operation));
                                            ui.close_menu();
                                        }
                                    }
                                    ui.separator();
                                    for (label, operation) in [
                                        ("Close", TabAction::Close),
                                        ("Close Others", TabAction::CloseOthers),
                                        ("Close All", TabAction::CloseAll),
                                    ] {
                                        if ui.button(label).clicked() {
                                            action = Some((i, operation));
                                            ui.close_menu();
                                        }
                                    }
                                    ui.separator();
                                    if ui
                                        .add_enabled(
                                            !self.export_busy,
                                            egui::Button::new("Export…"),
                                        )
                                        .clicked()
                                    {
                                        export = Some(tab.target.clone());
                                        ui.close_menu();
                                    }
                                });
                                if ui.small_button("×").clicked() {
                                    action = Some((i, TabAction::Close));
                                }
                                ui.separator();
                            }
                        });
                    });
            });
        });
        self.revealed_tab = selected_target;
        if let Some(index) = selected {
            self.select_tab(index);
        }
        if let Some((index, action)) = action {
            self.tab_action(index, action);
        }
        export
    }
    fn error(&mut self, error: String) {
        self.status = error
            .lines()
            .next()
            .unwrap_or("Native engine failed")
            .to_owned();
        self.diagnostics.push(error);
        if self.diagnostics.len() > 50 {
            self.diagnostics.remove(0);
        }
    }
    fn events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Archive(generation, archive) if generation == self.generation => {
                    self.archive = Some(archive);
                    self.loading_progress = self.loading_progress.max(0.25);
                    self.rebuild_tree();
                }
                Event::Opened(generation, result) if generation == self.generation => {
                    self.busy = false;
                    self.loading_project = false;
                    self.loading_progress = 1.0;
                    match result {
                        Ok((engine, project)) => {
                            self.status = format!(
                                "{} top-level classes · {} archive files · Decompilation engine: RDX Native DEX (alpha)",
                                project.classes.len(),
                                self.archive.as_ref().map_or(0, |a| a.entries.len())
                            );
                            if let Some(error) = engine.resource_error() {
                                self.diagnostics.push(error.to_owned());
                                self.status
                                    .push_str(" · resource names unavailable (see diagnostics)");
                            }
                            self.engine = Some(engine);
                            self.project = Some(project);
                            self.refresh_manifest_links();
                            self.rebuild_tree();
                        }
                        Err(error) => {
                            self.error(error);
                        }
                    }
                }
                Event::Source(generation, name, position, engine, result)
                    if generation == self.generation =>
                {
                    self.busy = false;
                    self.engine = Some(engine);
                    if self.discard_cancelled_history(&Target::Class(name.clone()), ctx) {
                        continue;
                    }
                    match result {
                        Ok(code)
                            if position.is_some()
                                && self.pending_history.as_ref().is_some_and(|pending| {
                                    pending.destination.source_hash.as_ref()
                                        != Some(&code.source_hash)
                                }) =>
                        {
                            self.pending_history = None;
                            self.error("Source changed after cache eviction; reopen the reference before navigating".into());
                        }
                        Ok(code) => match self.display_code(name, code, position) {
                            Ok(()) => {
                                if self.pending_history.is_some() {
                                    self.finish_history();
                                }
                                self.status =
                                    "Source ready · double-click a linked symbol to jump".into();
                            }
                            Err(error) => {
                                self.pending_history = None;
                                self.error(error);
                            }
                        },
                        Err(error) => {
                            self.pending_history = None;
                            self.error(format!("{error}. Reload if the engine stopped."));
                        }
                    }
                }
                Event::Navigated(generation, origin, engine, result)
                    if generation == self.generation =>
                {
                    if let Some(engine) = engine {
                        self.busy = false;
                        self.engine = Some(engine);
                    }
                    match result {
                        Ok(target) => match self.display_code(
                            target.class,
                            target.code,
                            Some(target.position),
                        ) {
                            Ok(()) => {
                                self.future.clear();
                                self.history.push(origin);
                                if self.history.len() > 100 {
                                    self.history.remove(0);
                                }
                                self.status =
                                    "Declaration ready · Back returns to the reference".into();
                            }
                            Err(error) => self.error(error),
                        },
                        Err(error) => self.error(error),
                    }
                }
                Event::Asset(generation, index, result) if generation == self.generation => {
                    self.asset_busy = false;
                    if self.discard_cancelled_history(&Target::File(index), ctx) {
                        continue;
                    }
                    let Some(name) = self.entry_name(index) else {
                        self.pending_history = None;
                        continue;
                    };
                    match result {
                        Ok(preview) => {
                            // Compiled Android XML is decoded natively when its index is ready.
                            if xml_preview_needs_resolution(&name, &preview)
                                && self.engine.as_ref().is_some_and(|engine| {
                                    !engine.resource_table().entries.is_empty()
                                })
                            {
                                self.decode_resource(index, ctx);
                                continue;
                            }
                            let (content, note) = match preview {
                                Preview::Text { text, syntax, note } => (
                                    Content::Text(Box::new(CodeDocument::new(text, &syntax))),
                                    note,
                                ),
                                Preview::Binary { text, note } => (
                                    Content::Text(Box::new(CodeDocument::new(text, "text"))),
                                    Some(note),
                                ),
                                Preview::Image {
                                    rgba,
                                    width,
                                    height,
                                } => {
                                    let image = egui::ColorImage::from_rgba_unmultiplied(
                                        [width as usize, height as usize],
                                        &rgba,
                                    );
                                    let texture = ctx.load_texture(
                                        format!("asset-{}-{index}", self.generation),
                                        image,
                                        egui::TextureOptions::LINEAR,
                                    );
                                    (
                                        Content::Image {
                                            texture,
                                            width,
                                            height,
                                        },
                                        None,
                                    )
                                }
                            };
                            self.status = if self.busy {
                                "Asset ready · native indexing is still processing".into()
                            } else {
                                "Asset ready".into()
                            };
                            if let Err(error) = self.push_tab(Tab {
                                pinned: false,
                                bookmarked: false,
                                loading_navigation: false,
                                source_hash: None,
                                target: Target::File(index),
                                name,
                                content,
                                note,
                            }) {
                                self.pending_history = None;
                                self.error(error);
                            }
                        }
                        Err(error) => {
                            self.pending_history = None;
                            self.error(format!("{name}: {error}"));
                        }
                    }
                }
                Event::ResourceRefresh(generation, index, engine, result)
                    if generation == self.generation =>
                {
                    self.busy = false;
                    self.engine = Some(engine);
                    if let Some(tab) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.target == Target::File(index))
                    {
                        match result {
                            Ok(code) => {
                                let mut document = CodeDocument::new(code.source, "xml");
                                document.set_links(code.links);
                                tab.content = Content::Text(Box::new(document));
                                tab.note = Some("Android XML decoded natively".into());
                            }
                            Err(error) => {
                                tab.note = Some(format!("Android XML decode failed. {error}"));
                            }
                        }
                    }
                    self.refresh_manifest_links();
                }
                Event::Resource(generation, index, engine, result)
                    if generation == self.generation =>
                {
                    self.busy = false;
                    self.engine = Some(engine);
                    if self.discard_cancelled_history(&Target::File(index), ctx) {
                        continue;
                    }
                    match result {
                        Ok(code) => {
                            let Some(name) = self.entry_name(index) else {
                                self.pending_history = None;
                                continue;
                            };
                            let mut document = CodeDocument::new(code.source, "xml");
                            let mut links = code.links;
                            if name == "AndroidManifest.xml" {
                                links.extend(crate::manifest_links::links(
                                    document.text(),
                                    &self
                                        .project
                                        .as_ref()
                                        .map(|p| p.classes.clone())
                                        .unwrap_or_default(),
                                ));
                            }
                            document.set_links(links);
                            if let Err(error) = self.push_tab(Tab {
                                pinned: false,
                                bookmarked: false,
                                loading_navigation: false,
                                source_hash: None,
                                target: Target::File(index),
                                name,
                                content: Content::Text(Box::new(document)),
                                note: Some("Android XML decoded natively".into()),
                            }) {
                                self.pending_history = None;
                                self.error(error);
                                continue;
                            }
                            self.status = "Android XML ready".into();
                        }
                        Err(error) => {
                            self.error(error);
                            // Preserve a useful raw preview when a resource cannot be decoded.
                            if let Some(archive) = self.archive.clone() {
                                let tx = self.tx.clone();
                                let ctx = ctx.clone();
                                self.asset_busy = true;
                                thread::spawn(move || {
                                    let result = archive
                                        .preview(index)
                                        .map(|p| match p {
                                            Preview::Binary { text, note } => Preview::Text {
                                                text,
                                                syntax: "text".into(),
                                                note: Some(format!(
                                                    "Android XML decode failed. {note}"
                                                )),
                                            },
                                            Preview::Text { text, syntax, .. } => Preview::Text {
                                                text, syntax,
                                                note: Some("Android XML decode failed. Showing archive preview.".into()),
                                            },
                                            p => p,
                                        })
                                        .map_err(|e| format!("{e:#}"));
                                    let _ = tx.send(Event::Asset(generation, index, result));
                                    ctx.request_repaint();
                                });
                            }
                        }
                    }
                }
                Event::Exported(generation, engine, result) if generation == self.generation => {
                    self.export_busy = false;
                    if let Some(engine) = engine {
                        self.engine = Some(engine);
                        self.busy = false;
                    }
                    match result {
                        Ok(path) => self.status = format!("Exported {}", path.display()),
                        Err(error) => self.error(format!("Export failed: {error}")),
                    }
                }
                Event::CallGraph(generation, engine, result) if generation == self.generation => {
                    self.engine = Some(engine);
                    self.busy = false;
                    self.call_graph.cancel = None;
                    match result {
                        Ok(graph) => {
                            self.call_graph.status = format!(
                                "{} methods · {} edges · click a method to open it",
                                graph.nodes.len(),
                                graph.edges.len()
                            );
                            self.call_graph.graph = Some(graph);
                        }
                        Err(error) => self.call_graph.status = error,
                    }
                }
                Event::UsagesUpdate(generation, id, update)
                    if generation == self.generation && id == self.usages_id =>
                {
                    match update {
                        UsageUpdate::Target(label) => self.usages.label = label,
                        UsageUpdate::Batch(hits) => self.usages.append(hits),
                        UsageUpdate::Progress(status) => self.usages.status = status,
                    }
                }
                Event::UsagesDone(generation, id, engine, summary)
                    if generation == self.generation && id == self.usages_id =>
                {
                    self.busy = false;
                    self.engine = Some(engine);
                    self.usages_cancel = None;
                    self.usages.finish(summary.status, summary.errors);
                    self.status = self.usages.status.clone();
                }
                Event::SearchUpdate(generation, id, update)
                    if generation == self.generation && id == self.search_id =>
                {
                    self.search.update(update);
                }
                Event::SearchMetadata(generation, name, hash, engine, result)
                    if generation == self.generation =>
                {
                    if let Some(engine) = engine {
                        self.busy = false;
                        self.engine = Some(engine);
                    }
                    if let Some(tab) = self.tabs.iter_mut().find(|tab| {
                        tab.target == Target::Class(name.clone())
                            && tab.source_hash.as_ref() == Some(&hash)
                    }) {
                        match result {
                            Ok(code) => {
                                if let Err(error) = tab.attach_search_metadata(&hash, code) {
                                    tab.note = Some(error);
                                }
                            }
                            Err(error) => {
                                tab.note = Some(format!("Navigation unavailable: {error}"))
                            }
                        }
                    }
                }
                Event::SearchDone(generation, id, engine, summary)
                    if generation == self.generation && id == self.search_id =>
                {
                    self.busy = false;
                    self.engine = Some(engine);
                    self.search_cancel = None;
                    self.search.finish(summary);
                    self.status = self.search.status.clone();
                    ctx.request_repaint_of(SearchWindow::viewport_id());
                }
                Event::InteractiveReady(generation, engine) if generation == self.generation => {
                    self.busy = false;
                    self.engine = Some(engine);
                }
                Event::Plugin(generation, result) => {
                    self.plugin_busy = false;
                    if generation != self.generation {
                        continue;
                    }
                    match result {
                        Ok(output) => self.plugin_output = output,
                        Err(error) => self.error(error),
                    }
                }
                _ => {}
            }
        }
        // Upgrade previews opened while the DEX/resource index was still loading.
        // Keep tab selection and history intact, and never retry failed decodes each frame.
        if !self.busy
            && !self.asset_busy
            && self
                .engine
                .as_ref()
                .is_some_and(|engine| !engine.resource_table().entries.is_empty())
            && let Some(index) = self.tabs.iter().find_map(|tab| {
                if is_android_xml(&tab.name)
                    && !tab.note.as_deref().is_some_and(|note| {
                        note == "Android XML decoded natively"
                            || note.starts_with("Android XML decode failed.")
                    })
                    && let Target::File(index) = tab.target
                {
                    return Some(index);
                }
                None
            })
        {
            self.decode_resource_mode(index, ctx, true);
        }
    }
    fn plugins_window(&mut self, ctx: &egui::Context) {
        let mut visible = self.show_plugins;
        egui::Window::new("Plugins")
            .open(&mut visible)
            .default_width(600.0)
            .show(ctx, |ui| {
                ui.label("Local executable plugins · protocol v1");
                ui.label(
                    "Enabled plugins run with your user permissions. Only enable code you trust.",
                );
                if ui.button("Load plugin manifest…").clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("Plugin manifest", &["json"])
                        .pick_file()
                {
                    match Plugin::load(&path) {
                        Ok(plugin) if !self.plugins.iter().any(|(p, _)| p.id == plugin.id) => {
                            self.plugins.push((plugin, false))
                        }
                        Ok(_) => self.error("This plugin is already loaded".into()),
                        Err(error) => self.error(format!("Plugin manifest: {error:#}")),
                    }
                }
                ui.separator();
                let active_class = self
                    .tabs
                    .get(self.selected)
                    .filter(|t| matches!(t.target, Target::Class(_)));
                for (plugin, enabled) in &mut self.plugins {
                    ui.horizontal(|ui| {
                        ui.checkbox(enabled, &plugin.name);
                        ui.small(&plugin.id);
                        if ui
                            .add_enabled(
                                *enabled && !self.plugin_busy && active_class.is_some(),
                                egui::Button::new("Run on active class"),
                            )
                            .clicked()
                        {
                            let tab = active_class.unwrap();
                            let (plugin, name, source, tx, generation, ctx) = (
                                plugin.clone(),
                                tab.name.clone(),
                                tab.text().unwrap().to_owned(),
                                self.tx.clone(),
                                self.generation,
                                ctx.clone(),
                            );
                            self.plugin_busy = true;
                            thread::spawn(move || {
                                let result = plugin
                                    .analyze(&name, &source)
                                    .and_then(|v| Ok(serde_json::to_string_pretty(&v)?))
                                    .map_err(|e| format!("Plugin failed: {e:#}"));
                                let _ = tx.send(Event::Plugin(generation, result));
                                ctx.request_repaint();
                            });
                        }
                    });
                }
                if self.plugins.is_empty() {
                    ui.weak("Example: plugins/source-stats/plugin.json");
                }
                if self.plugin_busy {
                    ui.spinner();
                }
                egui::ScrollArea::vertical()
                    .max_height(260.0)
                    .show(ui, |ui| {
                        ui.monospace(&self.plugin_output);
                    });
                ui.separator();
                ui.weak("JADX Java/GUI plugin compatibility is not implemented in this preview.");
            });
        self.show_plugins = visible;
    }
}

fn xml_preview_needs_resolution(name: &str, preview: &Preview) -> bool {
    is_android_xml(name)
        && match preview {
            Preview::Text { note, .. } => !note
                .as_deref()
                .is_some_and(|n| n.starts_with("Android XML decode failed.")),
            Preview::Binary { .. } => true,
            Preview::Image { .. } => false,
        }
}

fn is_android_xml(name: &str) -> bool {
    name == "AndroidManifest.xml" || (name.starts_with("res/") && name.ends_with(".xml"))
}

fn tree_entry(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    selected: bool,
    enabled: bool,
) -> egui::Response {
    ui.add_enabled_ui(enabled, |ui| {
        let font = egui::TextStyle::Body.resolve(ui.style());
        let galley =
            ui.painter()
                .layout_no_wrap(label.to_owned(), font, egui::Color32::PLACEHOLDER);
        let size = egui::vec2(galley.size().x + 28.0, galley.size().y.max(18.0) + 4.0);
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
        let visuals = ui.style().interact_selectable(&response, selected);
        if selected || response.hovered() || response.has_focus() {
            ui.painter().rect_filled(rect, 4.0, visuals.bg_fill);
        }
        icons::paint(
            ui,
            egui::Rect::from_center_size(
                egui::pos2(rect.left() + 11.0, rect.center().y),
                egui::vec2(18.0, 18.0),
            ),
            icon,
        );
        ui.painter().galley(
            egui::pos2(rect.left() + 25.0, rect.center().y - galley.size().y / 2.0),
            galley,
            visuals.text_color(),
        );
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, enabled, selected, label)
        });
        response
    })
    .inner
}
fn group_icon(name: &str, path: &str) -> Icon {
    if !path.is_empty() {
        return if path.starts_with("/Classes") {
            Icon::Package
        } else {
            Icon::Folder
        };
    }
    match name {
        "Classes" => Icon::Classes,
        "Assets" => Icon::Assets,
        "Resources" => Icon::Resources,
        "Manifest" => Icon::Manifest,
        "Libraries" => Icon::Libraries,
        "DEX bytecode" => Icon::Dex,
        "Signatures & metadata" => Icon::Signature,
        _ => Icon::Folder,
    }
}
fn file_icon(name: &str, target: &Target) -> Icon {
    if matches!(target, Target::Class(_)) {
        return Icon::Classes;
    }
    if name == "AndroidManifest.xml" {
        return Icon::Manifest;
    }
    match name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "svg" => Icon::Image,
        "dex" => Icon::Dex,
        "so" => Icon::Libraries,
        "xml" | "json" | "js" | "html" | "css" => Icon::Code,
        "rsa" | "sf" | "mf" => Icon::Signature,
        _ => Icon::File,
    }
}
#[derive(Default)]
struct TreeActions {
    selected: Option<Target>,
    export: Option<Target>,
}
fn draw_tree(
    ui: &mut egui::Ui,
    node: &Node,
    path: &str,
    active: Option<&Target>,
    ready: (bool, bool),
    actions: &mut TreeActions,
) {
    for (name, child) in &node.children {
        let key = format!("{path}/{name}");
        if !child.children.is_empty() {
            let id = ui.make_persistent_id(&key);
            let mut clicked = false;
            let mut header = egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                id,
                false,
            )
            .show_header(ui, |ui| {
                clicked = tree_entry(
                    ui,
                    group_icon(name, path),
                    &format!("{name} ({})", child.count),
                    false,
                    true,
                )
                .clicked();
            });
            if clicked {
                header.toggle();
            }
            header.body(|ui| draw_tree(ui, child, &key, active, ready, actions));
        }
        for target in &child.targets {
            let enabled = match target {
                Target::Class(_) => ready.0,
                Target::File(_) => ready.1,
            };
            let response = tree_entry(
                ui,
                file_icon(name, target),
                name,
                active == Some(target),
                enabled,
            )
            .on_hover_text(&key);
            if response.clicked() {
                actions.selected = Some(target.clone());
            }
            response.context_menu(|ui| {
                if ui.button("Export…").clicked() {
                    actions.export = Some(target.clone());
                    ui.close_menu();
                }
            });
        }
    }
}

/// Scope desktop menu styling to the labels; popup menus and toolbar controls
/// keep their usual visuals. A shared scope also preserves menu-to-menu hovering.
fn plain_menu_bar(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    ui.scope(|ui| {
        let style = ui.style_mut();
        style.spacing.button_padding = egui::vec2(6.0, 2.0);
        let widgets = &mut style.visuals.widgets;
        widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
        widgets.inactive.bg_fill = egui::Color32::TRANSPARENT;
        for visual in [
            &mut widgets.inactive,
            &mut widgets.hovered,
            &mut widgets.active,
            &mut widgets.open,
        ] {
            visual.bg_stroke = egui::Stroke::NONE;
            visual.corner_radius = egui::CornerRadius::same(2);
            visual.expansion = 0.0;
        }
        add_contents(ui);
    });
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.events(ctx);
        if self.mcp_auto_start && self.project.is_some() && !self.loading_project {
            self.mcp_auto_start = false;
            self.start_mcp();
            self.show_mcp = true;
        }
        self.mcp_panel(ctx);
        if let Some(path) = self.initial.take() {
            self.open(path, ctx);
        }
        if let Some(path) = ctx.input(|i| i.raw.dropped_files.first().and_then(|f| f.path.clone()))
        {
            self.open(path, ctx);
        }
        if ctx.input_mut(|input| {
            input.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::F,
            )
        }) {
            self.search.open_mode(SearchMode::Code);
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::F)) {
            self.open_file_find();
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::ALT, egui::Key::ArrowLeft)) {
            self.go_back(ctx);
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::ALT, egui::Key::ArrowRight)) {
            self.go_forward(ctx);
        }
        let mut choose_file =
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::O));
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.add_space(5.0);
            ui.horizontal_wrapped(|ui| {
                let accent = if ui.visuals().dark_mode {
                    egui::Color32::from_rgb(105, 195, 230)
                } else {
                    egui::Color32::from_rgb(0, 91, 125)
                };
                ui.heading(egui::RichText::new("RDX").color(accent));
                ui.separator();
                plain_menu_bar(ui, |ui| {
                    ui.menu_button("File", |ui| {
                        if ui.button("Open APK / DEX…").clicked() {
                            choose_file = true;
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(
                                self.path.is_some() && !self.busy,
                                egui::Button::new("Reload"),
                            )
                            .clicked()
                        {
                            self.open(self.path.clone().unwrap(), ctx);
                            ui.close_menu();
                        }
                    });
                    ui.menu_button("View", |ui| {
                        if ui.checkbox(&mut self.word_wrap, "Word wrap").clicked() {
                            ui.close_menu();
                        }
                        ui.separator();
                        ui.menu_button("Settings", |ui| {
                            ui.horizontal(|ui| {
                                icons::small(ui, Icon::Settings);
                                ui.strong("Preferences");
                            });
                            ui.separator();
                            ui.menu_button("Interface theme", |ui| {
                                let mut appearance =
                                    ctx.options(|options| options.theme_preference);
                                for (value, label) in [
                                    (egui::ThemePreference::System, "System"),
                                    (egui::ThemePreference::Light, "Light"),
                                    (egui::ThemePreference::Dark, "Dark"),
                                ] {
                                    if ui.selectable_value(&mut appearance, value, label).clicked()
                                    {
                                        ctx.set_theme(appearance);
                                        ui.close_menu();
                                    }
                                }
                            });
                            ui.menu_button("Code theme", |ui| {
                                for (light, label) in [(true, "Light"), (false, "Dark")] {
                                    if !light { ui.separator(); }
                                    ui.weak(label);
                                    for theme in CodeTheme::ALL.into_iter().filter(|theme| theme.is_light() == light) {
                                        if ui.selectable_value(&mut self.theme, theme, theme.label()).clicked() {
                                            ui.close_menu();
                                        }
                                    }
                                }
                            });
                            ui.menu_button("Code font", |ui| {
                                for font in CodeFont::ALL {
                                    if ui.selectable_value(&mut self.code_font, font, font.label()).clicked() {
                                        ui.close_menu();
                                    }
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.label("Code font size");
                                ui.add(
                                    egui::DragValue::new(&mut self.font_size)
                                        .range(10.0..=28.0)
                                        .speed(0.5)
                                        .suffix(" px"),
                                );
                            });
                            ui.separator();
                            ui.weak("Changes save automatically");
                        });
                    });
                    ui.menu_button("Search", |ui| {
                        let can_find = self.tabs.get(self.selected)
                            .is_some_and(|tab| matches!(tab.content, Content::Text(_)));
                        if ui.add_enabled(can_find, egui::Button::new("Find in file…")
                            .shortcut_text("Cmd/Ctrl+F")).clicked() {
                            self.open_file_find();
                            ui.close_menu();
                        }
                        ui.separator();
                        for mode in SearchMode::ALL {
                            if ui.button(mode.label()).clicked() {
                                self.search.open_mode(mode);
                                ui.close_menu();
                            }
                        }
                    });
                    ui.menu_button("Tools", |ui| {
                        if ui.button("MCP Server…").clicked() {
                            self.show_mcp = true;
                            ui.close_menu();
                        }
                        if ui.button("Plugins…").clicked() {
                            self.show_plugins = true;
                            ui.close_menu();
                        }
                    });
                    ui.menu_button("Navigate", |ui| {
                        if ui
                            .add_enabled(
                                self.can_history(false),
                                egui::Button::new("Back").shortcut_text("Alt+Left"),
                            )
                            .clicked()
                        {
                            self.go_back(ctx);
                            ui.close_menu();
                        }
                        if ui.add_enabled(self.can_history(true), egui::Button::new("Forward").shortcut_text("Alt+Right")).clicked() {
                            self.go_forward(ctx);
                            ui.close_menu();
                        }
                    });
                });
                ui.separator();
                choose_file |=
                    icons::button(ui, Icon::Open, "Open APK / DEX (Cmd/Ctrl+O)", true).clicked();
                if icons::button(
                    ui,
                    Icon::Reload,
                    "Reload project",
                    self.path.is_some() && !self.busy,
                )
                .clicked()
                {
                    self.open(self.path.clone().unwrap(), ctx);
                }
                if icons::button(
                    ui,
                    Icon::Back,
                    "Back to previous reference (Alt+Left)",
                    self.can_history(false),
                )
                .clicked()
                {
                    self.go_back(ctx);
                }
                if icons::button(ui, Icon::Forward, "Forward to next reference (Alt+Right)", self.can_history(true)).clicked() {
                    self.go_forward(ctx);
                }
                if icons::button(ui, Icon::Plugin, "Manage plugins", true).clicked() {
                    self.show_plugins = true;
                }
                if icons::button(ui, Icon::Search, "Code search (Cmd/Ctrl+Shift+F)", true).clicked()
                {
                    self.search.open_mode(SearchMode::Code);
                }
                ui.separator();
                ui.label("Decompilation engine: RDX Native DEX (alpha)");
                if self.busy {
                    if self.loading_project {
                        let percent = (self.loading_progress * 100.0).round() as u8;
                        ui.add(
                            egui::ProgressBar::new(self.loading_progress)
                                .animate(true)
                                .desired_width(220.0)
                                .text(format!("Loading / indexing… {percent}%")),
                        )
                        .on_hover_text(
                            "Completed native loading stages; the current indexing stage is still running.",
                        );
                    } else {
                        ui.spinner();
                    }
                }
                if self.asset_busy {
                    ui.spinner();
                    ui.label("Reading asset…");
                }
            });
            if let Some(path) = &self.path {
                ui.weak(path.display().to_string());
            }
            ui.add_space(4.0);
        });
        if choose_file
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("Android bytecode", &["apk", "dex"])
                .pick_file()
        {
            self.open(path, ctx);
        }
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.label(&self.status);
            if !self.diagnostics.is_empty() {
                egui::CollapsingHeader::new(format!("Diagnostics ({})", self.diagnostics.len()))
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(120.0)
                            .show(ui, |ui| {
                                for message in &self.diagnostics {
                                    ui.label(message);
                                }
                            });
                    });
            }
        });
        let mut tree_actions = TreeActions::default();
        egui::SidePanel::left("project")
            .default_width(300.0)
            .width_range(200.0..=440.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.heading("Project");
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .hint_text("Filter classes and files…"),
                    )
                    .changed()
                {
                    self.rebuild_tree();
                }
                ui.separator();
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if self.tree.count == 0 {
                            ui.weak(if self.filter.is_empty() {
                                "No contents loaded"
                            } else {
                                "No matching classes or files"
                            });
                        }
                        ui.push_id(("project_tree", self.generation), |ui| {
                            draw_tree(
                                ui,
                                &self.tree,
                                "",
                                self.tabs.get(self.selected).map(|t| &t.target),
                                (self.engine.is_some() && !self.busy, !self.asset_busy),
                                &mut tree_actions,
                            );
                        });
                    });
            });
        if let Some(target) = tree_actions.selected {
            self.choose_target(target, ctx);
        }
        let mut export = tree_actions.export;
        let mut decode = None;
        let mut jump = None;
        let mut manifest_jump = None;
        let mut usage_request = None;
        let mut graph_request = None;
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.tabs.is_empty() {
                ui.add_space(75.0);
                ui.vertical_centered(|ui| {
                    ui.heading("Inspect code and APK contents");
                    ui.label("Select a class, resource, or asset from the project tree.");
                    ui.weak("Native Rust · alpha Java / DEX disassembly · assets");
                });
            } else {
                if let Some(target) = self.tab_bar(ui) {
                    export = Some(target);
                }
                if let Some(tab) = self.tabs.get_mut(self.selected) {
                    ui.horizontal_wrapped(|ui| {
                        ui.weak(&tab.name);
                        if let Some(text) = tab.text()
                            && ui.button("Copy source").clicked()
                        {
                            ui.ctx().copy_text(text.to_owned());
                        }
                        if let Target::File(index) = tab.target {
                            if let Some(entry) = self
                                .archive
                                .as_ref()
                                .and_then(|a| a.entries.iter().find(|e| e.index == index))
                            {
                                ui.weak(format!("{} bytes", entry.size));
                            }
                            if is_android_xml(&tab.name)
                                && ui
                                    .add_enabled(
                                        self.engine.is_some() && !self.busy,
                                        egui::Button::new("Decode Android XML"),
                                    )
                                    .clicked()
                            {
                                decode = Some(index);
                            }
                        }
                    });
                    if let Some(owner) = tab.name.strip_prefix("dex://") {
                        ui.horizontal_wrapped(|ui| {
                            ui.label("X-Refs: exact DEX call-site view, not Java source.");
                            if ui
                                .add_enabled(
                                    !self.busy && self.engine.is_some(),
                                    egui::Button::new("Open Java source"),
                                )
                                .clicked()
                            {
                                let position = match &tab.content {
                                    Content::Text(document) => document.navigation_position(),
                                    _ => 0,
                                };
                                manifest_jump = Some((
                                    owner.to_owned(),
                                    JumpLocation {
                                        target: tab.target.clone(),
                                        position,
                                        source_hash: tab.source_hash.clone(),
                                    },
                                ));
                            }
                        });
                    }
                    if let Some(note) = &tab.note {
                        ui.label(note);
                    }
                    ui.separator();
                    if tab.loading_navigation {
                        if tab.note.is_none() {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label("Opening result… preparing navigation");
                            });
                        } else if ui.button("Retry opening result").clicked()
                            && let Some(hash) = &tab.source_hash
                        {
                            self.pending_search_metadata
                                .push((tab.name.clone(), hash.clone()));
                            tab.note = None;
                        }
                        return;
                    }
                    ui.push_id((&tab.name, self.generation), |ui| match &mut tab.content {
                        Content::Text(document) => {
                            document.set_word_wrap(self.word_wrap);
                            document.set_code_font(self.code_font);
                            document.set_usages_enabled(
                                tab.source_hash.is_some() && self.engine.is_some() && !self.busy,
                            );
                            if let Some(position) = document.show(ui, self.theme, self.font_size)
                                && (!self.busy || self.search_cancel.is_some())
                            {
                                if let Some(hash) = &tab.source_hash {
                                    jump = Some((tab.name.clone(), position, hash.clone()));
                                } else if let Some(link) = document.link_at(position)
                                    && link.label.starts_with(rdx::resource_table::PREFIX)
                                {
                                    manifest_jump = Some((
                                        link.label.split(" | ").next().unwrap().to_owned(),
                                        JumpLocation {
                                            target: tab.target.clone(),
                                            position,
                                            source_hash: None,
                                        },
                                    ));
                                } else if tab.name == "AndroidManifest.xml"
                                    && let Some(link) = document.link_at(position)
                                {
                                    manifest_jump = Some((
                                        link.label.clone(),
                                        JumpLocation {
                                            target: tab.target.clone(),
                                            position,
                                            source_hash: None,
                                        },
                                    ));
                                }
                            }
                            if let Some(offset) = document.take_usages_request()
                                && let Some(hash) = &tab.source_hash
                            {
                                usage_request = Some((
                                    tab.name.clone(),
                                    offset,
                                    hash.clone(),
                                    UsageMode::Usages,
                                ));
                            }
                            if let Some(offset) = document.take_subclasses_request()
                                && let Some(hash) = &tab.source_hash
                            {
                                usage_request = Some((
                                    tab.name.clone(),
                                    offset,
                                    hash.clone(),
                                    UsageMode::Subclasses,
                                ));
                            }
                            if let Some(offset) = document.take_implementations_request()
                                && let Some(hash) = &tab.source_hash
                            {
                                usage_request = Some((
                                    tab.name.clone(),
                                    offset,
                                    hash.clone(),
                                    UsageMode::Implementations,
                                ));
                            }
                            if let Some(offset) = document.take_call_graph_request()
                                && let Some(hash) = &tab.source_hash
                            {
                                graph_request = Some((tab.name.clone(), offset, hash.clone()));
                            }
                            if let Some((offset, callers)) = document.take_method_xrefs_request()
                                && let Some(hash) = &tab.source_hash
                            {
                                usage_request = Some((
                                    tab.name.clone(),
                                    offset,
                                    hash.clone(),
                                    if callers {
                                        UsageMode::Callers
                                    } else {
                                        UsageMode::Callees
                                    },
                                ));
                            }
                            if document.take_export_request() {
                                export = Some(tab.target.clone());
                            }
                        }
                        Content::Image {
                            texture,
                            width,
                            height,
                        } => {
                            ui.label(format!("{width} × {height} pixels"));
                            egui::ScrollArea::both().show(ui, |ui| {
                                let available = ui.available_size();
                                let scale = (available.x / *width as f32)
                                    .min(available.y / *height as f32)
                                    .clamp(0.01, 1.0);
                                ui.image((
                                    texture.id(),
                                    egui::vec2(*width as f32 * scale, *height as f32 * scale),
                                ))
                                .context_menu(|ui| {
                                    if ui.button("Export…").clicked() {
                                        export = Some(tab.target.clone());
                                        ui.close_menu();
                                    }
                                });
                            });
                        }
                    });
                }
            }
        });
        if let Some((class, offset, hash)) = graph_request {
            self.build_call_graph(class, offset, hash, ctx);
        }
        if let Some((class, offset, hash, mode)) = usage_request {
            self.find_usages(class, offset, hash, mode, ctx);
        }
        if let Some((class, origin)) = manifest_jump {
            self.navigate_manifest(class, origin, ctx);
        }
        if let Some((class, position, hash)) = jump {
            self.navigate(class, position, hash, ctx);
        }
        if let Some(index) = decode {
            self.decode_resource(index, ctx);
        }
        // Layout caches are created lazily; include them when enforcing tab eviction.
        while self.tabs.len() > 1
            && self.tabs.iter().map(Tab::retained_bytes).sum::<usize>() > 64 * 1024 * 1024
        {
            let Some(index) = self
                .tabs
                .iter()
                .enumerate()
                .position(|(index, tab)| index != self.selected && !tab.pinned)
            else {
                // Pinned source remains open; shed inactive layout caches rather than tabs.
                for (index, tab) in self.tabs.iter_mut().enumerate() {
                    if index != self.selected
                        && let Content::Text(document) = &mut tab.content
                    {
                        document.discard_layout_cache();
                    }
                }
                break;
            };
            self.tabs.remove(index);
            if index < self.selected {
                self.selected -= 1;
            }
        }
        self.plugins_window(ctx);
        let actions = self.search.show(
            ctx,
            self.engine.is_some() && self.project.is_some() && !self.busy,
        );
        if actions.cancel
            && let Some(cancel) = &self.search_cancel
        {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(query) = actions.start {
            self.start_search(query, ctx);
        }
        if let Some(hit) = actions.open {
            self.open_search_hit(hit);
        }
        let actions = self.usages.show(ctx);
        if actions.cancel
            && let Some(cancel) = &self.usages_cancel
        {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(hit) = actions.open {
            let location = format!("{} · {}:{}", self.usages.mode.title(), hit.kind, hit.line);
            self.open_search_hit(hit);
            self.status = location;
        }
        let (method, rebuild) = self.call_graph.show(ctx);
        if let Some(method) = method {
            self.navigate_graph_method(method, ctx);
        }
        if rebuild && let Some((class, offset, hash)) = self.call_graph.origin.clone() {
            self.build_call_graph(class, offset, hash, ctx);
        }
        self.persist_preferences(ctx);
        if let Some(target) = export {
            self.export_target(target, ctx);
        }
        self.load_pending_search_metadata(ctx);
    }
}
impl Drop for App {
    fn drop(&mut self) {
        self.stop();
    }
}

fn restored_interface_theme(settings: &Settings) -> egui::ThemePreference {
    match settings.interface_theme.as_str() {
        "light" => egui::ThemePreference::Light,
        "dark" => egui::ThemePreference::Dark,
        _ => egui::ThemePreference::System,
    }
}
fn restored_code_theme(settings: &Settings) -> CodeTheme {
    CodeTheme::from_preference(&settings.code_theme).unwrap_or_default()
}
fn snapshot_preferences(
    ctx: &egui::Context,
    theme: CodeTheme,
    font_size: f32,
    code_font: CodeFont,
    word_wrap: bool,
    search: SearchPreferences,
    usages_keep_open: bool,
) -> Settings {
    Settings {
        interface_theme: match ctx.options(|options| options.theme_preference) {
            egui::ThemePreference::System => "system",
            egui::ThemePreference::Light => "light",
            egui::ThemePreference::Dark => "dark",
        }
        .into(),
        code_theme: theme.preference_key().into(),
        font_size,
        code_font: code_font.preference_key().into(),
        word_wrap,
        usages_keep_open,
        search_keep_open: search.keep_open,
        search,
    }
}

#[cfg(test)]
mod settings_tests {
    #[test]
    fn mcp_panel_exposes_controls_without_starting_a_service() {
        let mut app = navigation_test_app();
        app.show_mcp = true;
        let ctx = egui::Context::default();
        let mut labels = String::new();
        for _ in 0..2 {
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 820.0),
                    )),
                    ..Default::default()
                },
                |ctx| app.mcp_panel(ctx),
            );
            for shape in output.shapes {
                if let egui::Shape::Text(text) = shape.shape {
                    labels.push_str(&text.galley.job.text);
                    labels.push('\n');
                }
            }
        }
        assert!(labels.contains("MCP Server"), "{labels}");
        assert!(labels.contains("Start server"), "{labels}");
        assert!(labels.contains("Copy MCP client configuration"), "{labels}");
        assert!(app.mcp_server.is_none());
    }

    #[test]
    fn decoded_manifest_preview_still_needs_resource_resolution() {
        let preview = rdx::apk::Preview::Text {
            text: "<manifest/>".into(),
            syntax: "xml".into(),
            note: None,
        };
        assert!(super::xml_preview_needs_resolution(
            "AndroidManifest.xml",
            &preview
        ));
        assert!(super::xml_preview_needs_resolution(
            "res/layout/main.xml",
            &preview
        ));
        assert!(!super::xml_preview_needs_resolution("notes.txt", &preview));
        let fallback = rdx::apk::Preview::Text {
            text: "<manifest/>".into(),
            syntax: "xml".into(),
            note: Some("Android XML decode failed. Showing archive preview.".into()),
        };
        assert!(!super::xml_preview_needs_resolution(
            "AndroidManifest.xml",
            &fallback
        ));
    }

    use super::*;
    #[test]
    #[ignore = "Set RDX_ZOOM_APK to the Zoom APK"]
    fn manifest_resources_resolve_before_and_after_index_ready() {
        let fixture = PathBuf::from(std::env::var_os("RDX_ZOOM_APK").unwrap());
        for manifest_first in [false, true] {
            let ctx = egui::Context::default();
            let archive = Arc::new(Archive::open(&fixture).unwrap());
            let index = archive
                .entries
                .iter()
                .find(|e| e.path == "AndroidManifest.xml")
                .unwrap()
                .index;
            let preview = archive.preview(index).unwrap();
            assert!(matches!(preview, Preview::Text { .. }));
            let mut engine = NativeEngine::start().unwrap();
            let project = engine.open(&fixture).unwrap();
            let mut app = navigation_test_app();
            app.archive = Some(archive);
            if manifest_first {
                app.tx
                    .send(Event::Asset(0, index, Ok(preview.clone())))
                    .unwrap();
                app.events(&ctx);
                assert_eq!(app.tabs.len(), 1);
            }
            app.tx
                .send(Event::Opened(0, Ok((engine, project))))
                .unwrap();
            app.events(&ctx);
            if !manifest_first {
                app.tx.send(Event::Asset(0, index, Ok(preview))).unwrap();
                app.events(&ctx);
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while app.busy && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
                app.events(&ctx);
            }
            assert!(!app.busy, "resource refresh did not finish");
            assert_eq!(app.tabs.len(), 1);
            assert!(app.history.is_empty());
            let Content::Text(document) = &app.tabs[app.selected].content else {
                panic!("missing manifest")
            };
            assert!(
                document
                    .text()
                    .contains("@us.zoom.videomeetings:style/ZMTheme.MainWindow")
            );
            assert!(
                document
                    .resource_links()
                    .iter()
                    .any(|link| link.label.starts_with("resource://7f1306f1"))
            );
            let offset = document
                .text()
                .find("com.zipow.videobox.IMActivity")
                .unwrap();
            assert!(
                document
                    .link_at(document.text()[..offset].chars().count())
                    .is_some()
            );
        }
    }

    fn navigation_test_app() -> App {
        let (tx, rx) = mpsc::channel();
        App {
            mcp_server: None,
            show_mcp: false,
            mcp_error: String::new(),
            mcp_auto_start: false,
            search: SearchWindow::default(),
            usages: UsagesWindow::default(),
            call_graph: crate::call_graph_window::CallGraphWindow::default(),
            usages_cancel: None,
            usages_id: 0,
            search_cache: Arc::new(Mutex::new(search::SearchCache::default())),
            search_cancel: None,
            search_id: 0,
            pending_search_metadata: Vec::new(),
            interactive_requests: Arc::new(Mutex::new(Vec::new())),
            settings_store: SettingsStore::new(),
            last_preferences: Settings::default(),
            tx,
            rx,
            generation: 0,
            engine: None,
            project: None,
            archive: None,
            tree: Node::default(),
            path: None,
            initial: None,
            busy: false,
            loading_project: false,
            loading_progress: 0.0,
            asset_busy: false,
            export_busy: false,
            status: String::new(),
            filter: String::new(),
            tabs: Vec::new(),
            selected: 0,
            revealed_tab: None,
            #[cfg(test)]
            tab_ui_controls: Vec::new(),
            history: Vec::new(),
            future: Vec::new(),
            pending_history: None,
            cancelled_history: None,
            deferred_target: None,
            theme: CodeTheme::Ocean,
            font_size: 14.0,
            code_font: CodeFont::default(),
            word_wrap: false,
            plugins: Vec::new(),
            show_plugins: false,
            plugin_busy: false,
            plugin_output: String::new(),
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn manifest_jump_and_back_work_before_after_loading_and_during_search_completion() {
        for manifest_first in [false, true] {
            for searching in [false, true] {
                let ctx = egui::Context::default();
                let fixture =
                    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk");
                let archive = Arc::new(Archive::open(&fixture).unwrap());
                let index = archive
                    .entries
                    .iter()
                    .find(|e| e.path == "AndroidManifest.xml")
                    .unwrap()
                    .index;
                let mut preview = archive.preview(index).unwrap();
                if let Preview::Text { text, .. } = &mut preview {
                    *text = text.replace("<application", "<!-- 🦀 -->\n<application");
                }
                let mut app = navigation_test_app();
                app.archive = Some(archive);
                let mut engine = NativeEngine::start().unwrap();
                let project = engine.open(&fixture).unwrap();
                if manifest_first {
                    app.tx
                        .send(Event::Asset(0, index, Ok(preview.clone())))
                        .unwrap();
                    app.events(&ctx);
                }
                app.tx
                    .send(Event::Opened(0, Ok((engine, project))))
                    .unwrap();
                app.events(&ctx);
                if !manifest_first {
                    app.tx.send(Event::Asset(0, index, Ok(preview))).unwrap();
                    app.events(&ctx);
                }
                let Content::Text(document) = &app.tabs[app.selected].content else {
                    panic!("missing manifest")
                };
                let byte = document.text().find(".Target").unwrap();
                let position = document.text()[..byte].chars().count();
                let class = document
                    .link_at(position)
                    .expect("manifest link not attached")
                    .label
                    .clone();
                let origin = JumpLocation {
                    target: Target::File(index),
                    position,
                    source_hash: None,
                };
                let held_engine = if searching {
                    app.busy = true;
                    app.search_cancel = Some(Arc::new(AtomicBool::new(false)));
                    app.engine.take()
                } else {
                    None
                };
                app.navigate_manifest(class, origin.clone(), &ctx);
                if let Some(engine) = held_engine {
                    assert_eq!(
                        app.interactive_requests.lock().unwrap().len(),
                        1,
                        "manifest jump was dropped while search owned the engine"
                    );
                    // Search ends after its last interactive poll: the UI must
                    // still drain this final queued request when ownership returns.
                    app.tx
                        .send(Event::SearchDone(
                            0,
                            0,
                            engine,
                            search::SearchSummary::default(),
                        ))
                        .unwrap();
                    app.events(&ctx);
                    app.load_pending_search_metadata(&ctx);
                }
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                loop {
                    app.events(&ctx);
                    if app.tabs[app.selected].target == Target::Class("sample.Target".into())
                        && app.engine.is_some()
                    {
                        break;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "jump did not finish: {}",
                        app.status
                    );
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                assert_eq!(app.history.len(), 1);
                assert_eq!(app.history[0].target, origin.target);
                assert_eq!(app.history[0].position, position);
                app.go_back(&ctx);
                assert_eq!(app.tabs[app.selected].target, Target::File(index));
                assert!(app.history.is_empty());
                let Content::Text(document) = &app.tabs[app.selected].content else {
                    panic!("manifest lost")
                };
                assert_eq!(document.link_at(position).unwrap().label, "sample.Target");
                assert_eq!(app.future.len(), 1);
                app.go_forward(&ctx);
                assert_eq!(
                    app.tabs[app.selected].target,
                    Target::Class("sample.Target".into())
                );
                assert!(app.future.is_empty());
                assert_eq!(app.history.len(), 1);
                assert!(app.diagnostics.is_empty(), "{:?}", app.diagnostics);
            }
        }
    }

    fn sample_tab(index: usize, bytes: usize) -> Tab {
        Tab {
            pinned: false,
            bookmarked: false,
            loading_navigation: false,
            source_hash: None,
            target: Target::File(index),
            name: format!("assets/very-long-view-name-{index}.txt"),
            content: Content::Text(Box::new(CodeDocument::new("x".repeat(bytes), "text"))),
            note: None,
        }
    }
    #[test]
    fn pinned_tab_admission_is_transactional_and_refresh_keeps_flags() {
        let mut app = navigation_test_app();
        for index in 0..8 {
            let mut tab = sample_tab(index, 64);
            tab.pinned = true;
            app.push_tab(tab).unwrap();
        }
        app.selected = 3;
        let error = app.push_tab(sample_tab(8, 64)).unwrap_err();
        assert!(error.contains("Unpin or close"));
        assert_eq!(app.tabs.len(), 8);
        assert_eq!(app.selected, 3);
        assert!(app.tabs.iter().all(|tab| tab.pinned));
        app.tabs[3].bookmarked = true;
        app.push_tab(sample_tab(3, 128)).unwrap();
        assert!(app.tabs[app.selected].pinned && app.tabs[app.selected].bookmarked);
        app.tab_action(0, TabAction::TogglePin);
        app.push_tab(sample_tab(8, 64)).unwrap();
        assert_eq!(app.tabs.len(), 8);
        assert!(!app.tabs.iter().any(|tab| tab.target == Target::File(0)));
    }
    #[test]
    fn pinned_memory_budget_rejects_without_evicting_or_changing_selection() {
        let mut app = navigation_test_app();
        for index in 0..2 {
            let mut tab = sample_tab(index, 24 * 1024 * 1024);
            tab.pinned = true;
            app.push_tab(tab).unwrap();
        }
        let selected = app.selected;
        assert!(app.push_tab(sample_tab(2, 24 * 1024 * 1024)).is_err());
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.selected, selected);
        assert!(app.tabs.iter().map(Tab::retained_bytes).sum::<usize>() <= 64 * 1024 * 1024);
    }
    #[test]
    fn tab_context_actions_preserve_pins_selection_and_reference_history() {
        let mut app = navigation_test_app();
        for index in 0..4 {
            app.push_tab(sample_tab(index, 64)).unwrap();
        }
        app.select_tab(2);
        app.history.push(app.current_location().unwrap());
        app.tab_action(0, TabAction::TogglePin);
        app.tab_action(1, TabAction::ToggleBookmark);
        assert!(app.tabs[1].bookmarked);
        app.tab_action(1, TabAction::UnbookmarkAll);
        assert!(app.tabs.iter().all(|tab| !tab.bookmarked));
        app.tab_action(1, TabAction::Close);
        assert_eq!(app.tabs[app.selected].target, Target::File(2));
        app.tab_action(app.selected, TabAction::CloseOthers);
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.tabs[app.selected].target, Target::File(2));
        app.tab_action(app.selected, TabAction::CloseAll);
        assert_eq!(app.tabs.len(), 1);
        assert!(app.tabs[0].pinned);
        assert_eq!(app.history.len(), 1, "closed references remain reopenable");
        app.tab_action(0, TabAction::Close);
        assert!(app.tabs.is_empty(), "explicit close can close pinned tabs");
        assert_eq!(app.selected, 0);
        app.push_tab(sample_tab(4, 64)).unwrap();
        app.tab_action(0, TabAction::TogglePin);
        app.tab_action(0, TabAction::UnpinAll);
        assert!(!app.tabs[0].pinned);
    }
    #[test]
    fn overflow_menu_lists_full_names_and_selects_offscreen_tab() {
        let ctx = egui::Context::default();
        let mut app = navigation_test_app();
        for index in 0..8 {
            app.push_tab(sample_tab(index, 64)).unwrap();
        }
        app.selected = 0;
        let render = |app: &mut App, events| {
            let mut height = 0.0;
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(320.0, 600.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let start = ui.cursor().min.y;
                        app.tab_bar(ui);
                        height = ui.cursor().min.y - start;
                    });
                },
            );
            assert!(height < 60.0, "tab strip wrapped: {height}");
        };
        let click = |position| {
            vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        render(&mut app, vec![]);
        let button = app
            .tab_ui_controls
            .iter()
            .find(|(name, _)| name == "views")
            .unwrap()
            .1;
        assert!(
            button.max.x < 320.0 && button.min.x > 250.0,
            "dropdown must stay at the right edge: {button:?}"
        );
        render(&mut app, click(button.center()));
        render(&mut app, vec![]); // Allow the popup to settle its measured size.
        assert_eq!(
            app.tab_ui_controls
                .iter()
                .filter(|(name, _)| name.starts_with("view:"))
                .count(),
            8
        );
        let last = app
            .tab_ui_controls
            .iter()
            .find(|(name, _)| name == "view:assets/very-long-view-name-7.txt")
            .unwrap()
            .1;
        render(&mut app, click(last.center()));
        assert_eq!(app.selected, 7);
    }

    fn wait_for_history(app: &mut App, ctx: &egui::Context) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while app.pending_history.is_some() || app.cancelled_history.is_some() {
            app.events(ctx);
            assert!(
                std::time::Instant::now() < deadline,
                "history stalled: {}",
                app.status
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    fn history_fixture() -> (App, usize, PathBuf) {
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk");
        let archive = Arc::new(Archive::open(&fixture).unwrap());
        let index = archive
            .entries
            .iter()
            .find(|e| e.path == "AndroidManifest.xml")
            .unwrap()
            .index;
        let mut engine = NativeEngine::start().unwrap();
        let project = engine.open(&fixture).unwrap();
        let code = engine.decompile_with_metadata("sample.Target").unwrap();
        let mut app = navigation_test_app();
        app.archive = Some(archive);
        app.project = Some(project);
        app.engine = Some(engine);
        app.display_code("sample.Target".into(), code, Some(7))
            .unwrap();
        app.history.push(JumpLocation {
            target: Target::File(index),
            source_hash: None,
            position: 10,
        });
        (app, index, fixture)
    }
    #[test]
    fn forward_restores_evicted_class_and_back_restores_asset_position() {
        let ctx = egui::Context::default();
        let (mut app, index, _) = history_fixture();
        app.go_back(&ctx);
        assert!(app.pending_history.is_some());
        assert_eq!(
            app.history.len(),
            1,
            "pending action must not consume history"
        );
        wait_for_history(&mut app, &ctx);
        assert_eq!(app.current_location().unwrap().position, 10);
        assert_eq!(app.current_location().unwrap().target, Target::File(index));
        assert_eq!(app.future.len(), 1);
        app.tabs.retain(|tab| tab.target == Target::File(index));
        app.selected = 0;
        app.go_forward(&ctx);
        wait_for_history(&mut app, &ctx);
        assert_eq!(
            app.current_location().unwrap().target,
            Target::Class("sample.Target".into())
        );
        assert_eq!(app.current_location().unwrap().position, 7);
        assert!(app.future.is_empty());
        assert_eq!(app.history.len(), 1);
        assert!(app.diagnostics.is_empty(), "{:?}", app.diagnostics);
    }
    #[test]
    fn failed_history_display_preserves_stacks_and_allows_retry() {
        let ctx = egui::Context::default();
        let (mut app, _, _) = history_fixture();
        let mut location = app.current_location().unwrap();
        location.position = usize::MAX;
        app.future.push(location.clone());
        let mut engine = app.engine.take().unwrap();
        let code = engine.decompile_with_metadata("sample.Target").unwrap();
        app.pending_history = Some(HistoryJump {
            forward: true,
            destination: location,
            origin: app.current_location(),
        });
        app.busy = true;
        app.tx
            .send(Event::Source(
                0,
                "sample.Target".into(),
                Some(usize::MAX),
                engine,
                Ok(code),
            ))
            .unwrap();
        app.events(&ctx);
        assert!(app.pending_history.is_none());
        assert_eq!(app.future.len(), 1);
        assert!(app.can_history(true));
        assert!(!app.diagnostics.is_empty());
    }
    #[test]
    fn new_reference_clears_forward_and_project_reload_clears_both() {
        let ctx = egui::Context::default();
        let (mut app, _, fixture) = history_fixture();
        app.go_back(&ctx);
        wait_for_history(&mut app, &ctx);
        let origin = app.current_location().unwrap();
        let mut engine = app.engine.take().unwrap();
        let target = engine.navigate_class("sample.Target").unwrap();
        app.tx
            .send(Event::Navigated(0, origin, Some(engine), Ok(target)))
            .unwrap();
        app.events(&ctx);
        assert!(app.future.is_empty());
        assert_eq!(app.history.len(), 1);
        app.future.push(app.current_location().unwrap());
        app.open(fixture, &ctx);
        assert!(app.future.is_empty() && app.history.is_empty());
        assert!(app.pending_history.is_none());
    }
    #[test]
    fn choosing_another_cached_tab_discards_pending_history_completion() {
        let ctx = egui::Context::default();
        let (mut app, _, _) = history_fixture();
        app.go_back(&ctx);
        app.choose_target(Target::Class("sample.Target".into()), &ctx);
        wait_for_history(&mut app, &ctx);
        assert_eq!(
            app.current_location().unwrap().target,
            Target::Class("sample.Target".into())
        );
        assert_eq!(app.history.len(), 1);
        assert!(app.future.is_empty());
    }

    #[test]
    fn deferred_manifest_navigation_replaces_older_jump_and_preserves_origin() {
        let origin = JumpLocation {
            source_hash: None,
            target: Target::File(7),
            position: 19,
        };
        let mut requests = vec![
            InteractiveRequest::Metadata("sample.Hello".into(), "hash".into()),
            InteractiveRequest::Navigate("sample.Hello".into(), 3, "old".into()),
        ];
        replace_pending_navigation(
            &mut requests,
            InteractiveRequest::NavigateClass("sample.Target".into(), origin.clone()),
        );
        assert_eq!(requests.len(), 2, "metadata plus the latest navigation");
        assert!(matches!(requests[0], InteractiveRequest::Metadata(..)));

        let request = requests.pop().unwrap();
        let mut engine = NativeEngine::start().unwrap();
        let fixture =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.dex");
        engine.open(&fixture).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        request.execute(&mut engine, &tx, 11, &egui::Context::default());
        let event = rx.recv().unwrap();
        let Event::Navigated(generation, returned_origin, None, result) = event else {
            panic!("deferred class jump emitted the wrong event");
        };
        assert_eq!(generation, 11);
        assert_eq!(returned_origin.target, origin.target);
        assert_eq!(returned_origin.position, origin.position);
        let target = result.unwrap();
        assert_eq!(target.class, "sample.Target");
    }

    #[test]
    fn repeated_search_hit_preserves_existing_navigation_document() {
        let source = "return target;";
        let mut document = CodeDocument::new(source.into(), "java");
        document.set_links(vec![rdx::engine::CodeLink {
            start: 7,
            end: 13,
            label: "sample.Target".into(),
        }]);
        let mut tab = Tab {
            pinned: false,
            bookmarked: false,
            loading_navigation: false,
            source_hash: Some("original".into()),
            target: Target::Class("sample.Hello".into()),
            name: "sample.Hello".into(),
            content: Content::Text(Box::new(document)),
            note: None,
        };
        let mut hit = SearchHit {
            document: Arc::new(search::SearchDocument {
                target: SearchTarget::Class("sample.Hello".into()),
                name: "sample.Hello".into(),
                source: source.into(),
                syntax: "java".into(),
                links: Vec::new(),
                source_hash: Some("original".into()),
                metadata_complete: false,
            }),
            start: 7,
            end: 13,
            line: 1,
            kind: "Code".into(),
            preview: source.into(),
            preview_match: 7..13,
        };
        let Content::Text(document) = &tab.content else {
            unreachable!()
        };
        let original_address = document.as_ref() as *const CodeDocument;
        let original_bytes = document.retained_bytes();
        assert!(tab.reuse_search_hit(&hit).unwrap());
        let Content::Text(document) = &tab.content else {
            unreachable!()
        };
        assert_eq!(document.as_ref() as *const CodeDocument, original_address);
        assert_eq!(document.retained_bytes(), original_bytes);
        // The lightweight hit has no links; keeping the allocation and retained
        // metadata proves opening it has not replaced the navigable document.
        assert!(original_bytes > CodeDocument::new(source.into(), "java").retained_bytes());
        assert!(tab.note.is_none());
        Arc::get_mut(&mut hit.document).unwrap().source_hash = Some("changed".into());
        assert!(!tab.reuse_search_hit(&hit).unwrap());
        Arc::get_mut(&mut hit.document).unwrap().source_hash = Some("original".into());
        Arc::get_mut(&mut hit.document).unwrap().source = "return other;".into();
        assert!(!tab.reuse_search_hit(&hit).unwrap());
        Arc::get_mut(&mut hit.document).unwrap().source = source.into();
        Arc::get_mut(&mut hit.document).unwrap().target =
            SearchTarget::Class("sample.Other".into());
        assert!(!tab.reuse_search_hit(&hit).unwrap());
        assert_eq!(tab.text(), Some(source));
    }
    #[test]
    fn deferred_search_metadata_refreshes_changed_source_and_rejects_stale_requests() {
        let mut tab = Tab {
            pinned: false,
            bookmarked: false,
            loading_navigation: true,
            source_hash: Some("original".into()),
            target: Target::Class("sample.Hello".into()),
            name: "sample.Hello".into(),
            content: Content::Text(Box::new(CodeDocument::new("return target;".into(), "java"))),
            note: Some("Loading navigation".into()),
        };
        let code = |source: &str, hash: &str| DecompiledCode {
            source: source.into(),
            source_hash: hash.into(),
            definitions: Vec::new(),
            links: vec![rdx::engine::CodeLink {
                start: source.find("target").unwrap_or(7),
                end: source.find("target").unwrap_or(7) + 6,
                label: "sample.Target".into(),
            }],
        };
        assert!(
            tab.attach_search_metadata("original", code("return other;", "original"))
                .is_err()
        );
        assert_eq!(tab.text(), Some("return target;"));
        assert!(tab.loading_navigation, "unannotated source remains hidden");
        if let Content::Text(document) = &mut tab.content {
            document.jump_to_range(7, 13).unwrap();
        }
        tab.attach_search_metadata("original", code("// fresh\nreturn target;", "changed"))
            .unwrap();
        assert_eq!(tab.text(), Some("// fresh\nreturn target;"));
        assert_eq!(tab.source_hash.as_deref(), Some("changed"));
        assert!(!tab.loading_navigation);
        assert!(tab.note.is_none());
        assert!(
            tab.attach_search_metadata("original", code("return target;", "original"))
                .is_err()
        );
        assert_eq!(tab.text(), Some("// fresh\nreturn target;"));
    }
    #[test]
    fn plain_menu_labels_keep_hover_feedback_and_do_not_restyle_toolbar() {
        for visuals in [egui::Visuals::light(), egui::Visuals::dark()] {
            let context = egui::Context::default();
            context.set_visuals(visuals);
            let _ = context.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let original = ui.style().clone();
                    plain_menu_bar(ui, |ui| {
                        let widgets = &ui.visuals().widgets;
                        assert_eq!(widgets.inactive.weak_bg_fill, egui::Color32::TRANSPARENT);
                        assert_eq!(widgets.inactive.bg_stroke, egui::Stroke::NONE);
                        assert_eq!(widgets.hovered.expansion, 0.0);
                        assert_eq!(widgets.hovered.corner_radius, egui::CornerRadius::same(2));
                        assert_eq!(
                            widgets.hovered.weak_bg_fill,
                            original.visuals.widgets.hovered.weak_bg_fill
                        );
                        assert_ne!(widgets.hovered.weak_bg_fill, egui::Color32::TRANSPARENT);
                        assert_eq!(ui.ctx().style().as_ref(), original.as_ref());
                    });
                    assert_eq!(ui.style().as_ref(), original.as_ref());
                });
            });
        }
    }

    #[test]
    fn every_ui_choice_round_trips_to_settings() {
        let context = egui::Context::default();
        for appearance in [
            egui::ThemePreference::System,
            egui::ThemePreference::Light,
            egui::ThemePreference::Dark,
        ] {
            context.set_theme(appearance);
            for theme in CodeTheme::ALL {
                {
                    let settings = snapshot_preferences(
                        &context,
                        theme,
                        19.5,
                        CodeFont::default(),
                        true,
                        SearchPreferences {
                            keep_open: true,
                            ..Default::default()
                        },
                        false,
                    );
                    let encoded = serde_json::to_vec(&settings).unwrap();
                    let mut restored: Settings = serde_json::from_slice(&encoded).unwrap();
                    restored.normalize();
                    assert_eq!(
                        restored, settings,
                        "theme settings must survive normalization"
                    );
                    assert!(settings.word_wrap);
                    assert!(settings.search_keep_open);
                    assert!(!settings.usages_keep_open);
                    assert!(
                        !snapshot_preferences(
                            &context,
                            theme,
                            19.5,
                            CodeFont::default(),
                            false,
                            SearchPreferences::default(),
                            true,
                        )
                        .search_keep_open
                    );
                    assert_eq!(restored_interface_theme(&settings), appearance);
                    assert_eq!(restored_code_theme(&settings), theme);
                    assert_eq!(settings.font_size, 19.5);
                }
            }
        }
    }
    #[test]
    fn resource_navigation_uses_existing_back_forward_history() {
        let ctx = egui::Context::default();
        let (mut app, _, _) = history_fixture();
        let origin = app.current_location().unwrap();
        let source = "<resources><item name=\"title\">Hello</item></resources>".to_owned();
        let target = "resource://7f010000/string/title";
        let code = DecompiledCode {
            source_hash: rdx::engine::source_identity(&source),
            source,
            links: vec![],
            definitions: vec![],
        };
        app.tx
            .send(Event::Navigated(
                0,
                origin.clone(),
                None,
                Ok(NavigationResult {
                    class: target.into(),
                    code,
                    position: 0,
                }),
            ))
            .unwrap();
        app.events(&ctx);
        assert_eq!(app.tabs[app.selected].target, Target::Class(target.into()));
        app.go_back(&ctx);
        wait_for_history(&mut app, &ctx);
        assert_eq!(app.current_location().unwrap().target, origin.target);
        app.go_forward(&ctx);
        wait_for_history(&mut app, &ctx);
        assert_eq!(
            app.current_location().unwrap().target,
            Target::Class(target.into())
        );
        assert!(app.diagnostics.is_empty(), "{:?}", app.diagnostics);
    }
}
