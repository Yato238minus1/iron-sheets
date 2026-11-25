//! The single mutable application state.
use crate::command::Command;
use crate::dialogs::Dialogs;
use crate::docs::DocStore;
use crate::palette_model::PaletteModel;
use crate::perf::Perf;
use crate::platform::Waker;
use crate::services::MathRenderer;
#[cfg(not(feature = "typst"))]
use crate::services::NoMath;
use crate::settings_model::{InsertMode, SettingsUi};
use crate::tabs::Tabs;
use crossbeam_channel::{Receiver, Sender, unbounded};
use iron_sheets_core::config::{AppConfig, ViewMode};
use iron_sheets_core::glob::ExcludeSet;
use iron_sheets_core::index::Indexer;
use iron_sheets_core::paths::AppDirs;
use iron_sheets_core::tree::{TreeCacheSnapshot, TreeModel};
use iron_sheets_core::watch::{FsEvent, Watcher};
use iron_sheets_core::workspace::{Session, TabState, Workspace, WorkspaceStore};
use parking_lot::{Condvar, Mutex};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SidebarView {
    #[default]
    Files,
    Search,
    Outline,
    Links,
    Symbols,
    #[cfg(feature = "git")]
    Git,
    #[cfg(feature = "calendar")]
    Calendar,
}

impl SidebarView {
    /// Every view in the default header order (Git and Calendar only exist with their feature).
    pub const ALL: &'static [SidebarView] = &[
        SidebarView::Files,
        SidebarView::Search,
        SidebarView::Outline,
        SidebarView::Links,
        SidebarView::Symbols,
        #[cfg(feature = "git")]
        SidebarView::Git,
        #[cfg(feature = "calendar")]
        SidebarView::Calendar,
    ];

    /// Stable id: the config key and the suffix of the `view.<id>` command.
    pub const fn id(self) -> &'static str {
        match self {
            SidebarView::Files => "files",
            SidebarView::Search => "search",
            SidebarView::Outline => "outline",
            SidebarView::Links => "links",
            SidebarView::Symbols => "symbols",
            #[cfg(feature = "git")]
            SidebarView::Git => "git",
            #[cfg(feature = "calendar")]
            SidebarView::Calendar => "calendar",
        }
    }
}

pub struct StatusMsg {
    pub text: String,
    pub error: bool,
    pub until: Instant,
}

#[derive(Default)]
pub struct Status {
    pub msgs: Vec<StatusMsg>,
}

fn notify_inner(notice: &mut Option<Notice>, text: String, action: Option<(&'static str, Command)>, secs: u64) {
    *notice = Some(Notice { text, action, until: Instant::now() + Duration::from_secs(secs) });
}

impl<U> AppState<U> {
    /// Show a toast for `secs` seconds, with an optional button that runs `action`.
    pub fn notify(&mut self, text: impl Into<String>, action: Option<(&'static str, Command)>, secs: u64) {
        notify_inner(&mut self.notice, text.into(), action, secs);
    }
}

impl Status {
    pub fn info(&mut self, text: impl Into<String>) {
        self.push(text.into(), false, 4);
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.push(text.into(), true, 8);
    }

    fn push(&mut self, text: String, error: bool, secs: u64) {
        self.msgs.push(StatusMsg { text, error, until: Instant::now() + Duration::from_secs(secs) });
        if self.msgs.len() > 20 {
            self.msgs.remove(0);
        }
    }

    pub fn current(&self) -> Option<&StatusMsg> {
        let now = Instant::now();
        self.msgs.iter().rev().find(|m| m.until > now)
    }

    pub fn next_expiry(&self) -> Option<Duration> {
        let now = Instant::now();
        self.msgs.iter().filter(|m| m.until > now).map(|m| m.until - now).max()
    }
}

/// One toast at the bottom of the window: a text and an optional action button. A new one replaces the old one.
pub struct Notice {
    pub text: String,
    pub action: Option<(&'static str, Command)>,
    pub until: Instant,
}

/// A folder removed from the workspace, kept so it can be put back.
pub struct RemovedRoot {
    pub workspace: String,
    pub idx: usize,
    pub path: PathBuf,
    pub expanded: Vec<PathBuf>,
}

pub struct AppOptions {
    pub dirs: AppDirs,
    pub config: AppConfig,
    pub start: Instant,
    pub paths: Vec<PathBuf>,
    pub workspace: Option<String>,
    pub print_milestones: bool,
    /// How long the indexer waits before its first scan (see `Indexer::spawn_after`); the binary sets a short one so
    /// the first frames are not slowed by its burst, tests leave it at zero so the index is there at once.
    pub index_start_delay: Duration,
    /// Set when the config file existed but could not be used completely; shown in the status bar.
    pub config_error: Option<String>,
}

impl AppOptions {
    pub fn new(dirs: AppDirs) -> Self {
        let (config, config_error) = AppConfig::load_checked(&dirs);
        AppOptions { dirs, config, start: Instant::now(), paths: Vec::new(), workspace: None, print_milestones: false, index_start_delay: Duration::ZERO, config_error }
    }
}

/// The single mutable application state. `U` is the frontend's own module state (`state.ui`); core code never looks into it.
pub struct AppState<U = ()> {
    /// Lets background threads ask the UI to repaint.
    pub waker: Arc<dyn Waker>,
    pub dirs: AppDirs,
    pub config: AppConfig,
    pub store: WorkspaceStore,
    pub workspace: Workspace,
    pub docs: DocStore,
    pub tabs: Tabs,
    pub tree: TreeModel,
    pub indexer: Indexer,
    pub watcher: Option<Watcher>,
    /// Test hook: shows the "watch limit reached" status segment without exhausting the OS limit.
    pub watch_limit_forced: bool,
    pub fs_rx: Receiver<Vec<FsEvent>>,
    /// Results of link rewrites on closed notes (background thread); `sidebar::moves::poll` reports them.
    pub relink_tx: Sender<crate::sidebar::moves::Done>,
    pub relink_rx: Receiver<crate::sidebar::moves::Done>,
    /// Results of background trash operations; `command::poll_trash` reports them.
    pub trash_tx: Sender<(PathBuf, std::io::Result<()>)>,
    pub trash_rx: Receiver<(PathBuf, std::io::Result<()>)>,
    pub pending_trash: usize,
    /// The move the user is being asked about, with its plan.
    pub pending_move: Option<crate::sidebar::moves::Pending>,
    pub commands: Vec<Command>,
    pub status: Status,
    pub notice: Option<Notice>,
    pub removed_root: Option<RemovedRoot>,
    pub dialogs: Dialogs,
    pub sidebar_view: SidebarView,
    pub perf: Perf,
    pub math: Box<dyn MathRenderer>,
    /// Shared Typst engine. Creating it is free: threads and fonts start at first use.
    #[cfg(feature = "typst")]
    pub typst: Arc<iron_sheets_typst::Engine>,
    pub keymap: crate::keymap::Keymap,
    pub settings: SettingsUi,
    /// Quick switcher / command palette model (open flag, query, ranked rows).
    pub palette: PaletteModel,
    /// What clicking a symbol inserts (`symbol_insert_mode` setting).
    pub symbol_mode: InsertMode,
    /// Frontend module states (widgets, layout caches, textures).
    pub ui: U,
    // Requests and bookkeeping.
    /// Set by `Command::GlobalSearch`; the search panel takes it.
    pub search_request: Option<String>,
    /// Set by `Command::FindInNote`; editor features take it.
    pub find_request: bool,
    /// Path the file tree should expand to and scroll to on the next frame; the file tree takes it.
    pub reveal_request: Option<PathBuf>,
    /// Incremented whenever the indexer/watcher/tree are rebuilt; frontends reset per-tree view state when it changes.
    pub runtime_gen: u64,
    pub quit_ok: bool,
    pub session_sig: u64,
    pub session_dirty_at: Option<Instant>,
    pub config_dirty_at: Option<Instant>,
    pub had_focus: bool,
    /// Set after a workspace rename: restart indexer/watcher under the new name.
    pub rename_restart: bool,
    /// Bumped whenever a setting changed that derived values (theme colours, fonts, sizes) are built from. Frontends
    /// rebuild those once per bump and keep reading the precomputed values every frame (see `settings_model::apply`).
    pub settings_gen: u64,
    /// Bumped by every `touch_config`: caches of values derived from the config key on it.
    pub config_gen: u64,
    /// Set when the empty tab should take keyboard focus (a second "New tab" while it is active); its page takes it.
    pub focus_empty_page: bool,
    /// Set by `Command::FocusSidebar` or `Command::SetSidebarView`; the active sidebar panel takes focus next frame.
    pub focus_sidebar: bool,
    /// Set when a sidebar panel returns focus to the editor via `Escape`.
    pub focus_editor: bool,
    pub stats: crate::statusbar_model::StatusBarModel,
    /// Enabled feature modules (empty, no allocation, when all are off).
    pub modules: crate::modules::Registry,
    persist: PersistWorker,
    workspace_list: Vec<String>,
    workspace_list_at: Option<Instant>,
    /// A wiki-link click that found the index busy (a scan holds the write lock); `command::poll_open` retries it.
    pub pending_link: Option<crate::command::PendingLink>,
}

const WORKSPACE_LIST_TTL: Duration = Duration::from_secs(2);

struct PersistInner {
    workspace: Option<(WorkspaceStore, Workspace)>,
    config: Option<(AppDirs, AppConfig)>,
    tree_cache: Option<(String, PathBuf, TreeCacheSnapshot)>,
    in_flight: bool,
    closed: bool,
    errors: Vec<String>,
}

struct PersistShared {
    mu: Mutex<PersistInner>,
    wake_worker: Condvar,
    wake_done: Condvar,
    workspace_saved: AtomicBool,
}

struct PersistWorker {
    shared: Arc<PersistShared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Default for PersistWorker {
    fn default() -> Self {
        PersistWorker {
            shared: Arc::new(PersistShared {
                mu: Mutex::new(PersistInner { workspace: None, config: None, tree_cache: None, in_flight: false, closed: false, errors: Vec::new() }),
                wake_worker: Condvar::new(),
                wake_done: Condvar::new(),
                workspace_saved: AtomicBool::new(false),
            }),
            thread: None,
        }
    }
}

impl Drop for PersistWorker {
    fn drop(&mut self) {
        {
            let mut g = self.shared.mu.lock();
            g.closed = true;
            self.shared.wake_worker.notify_all();
        }
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}

fn save_workspace_if_needed(store: &WorkspaceStore, ws: &Workspace) -> std::io::Result<bool> {
    if !ws.roots.is_empty() || !ws.session.tabs.is_empty() || store.list().contains(&ws.name) {
        store.save(ws)?;
        return Ok(true);
    }
    Ok(false)
}

fn persist_loop(sh: Arc<PersistShared>, waker: Arc<dyn Waker>) {
    loop {
        let (ws, cfg, tc) = {
            let mut g = sh.mu.lock();
            loop {
                if g.workspace.is_some() || g.config.is_some() || g.tree_cache.is_some() {
                    g.in_flight = true;
                    break (g.workspace.take(), g.config.take(), g.tree_cache.take());
                }
                g.in_flight = false;
                sh.wake_done.notify_all();
                if g.closed {
                    return;
                }
                sh.wake_worker.wait(&mut g);
            }
        };
        let mut errs = Vec::new();
        if let Some((store, ws)) = ws {
            match save_workspace_if_needed(&store, &ws) {
                Ok(true) => sh.workspace_saved.store(true, Ordering::Release),
                Ok(false) => {}
                Err(e) => errs.push(format!("Cannot save workspace: {e}")),
            }
        }
        if let Some((dirs, cfg)) = cfg {
            if let Err(e) = cfg.save(&dirs) {
                errs.push(format!("Cannot save settings: {e}"));
            }
        }
        if let Some((_, path, snap)) = tc {
            let _ = TreeModel::save_cache_snapshot(&path, &snap);
        }
        if !errs.is_empty() {
            sh.mu.lock().errors.extend(errs);
            waker.wake();
        }
    }
}

impl PersistWorker {
    fn ensure_started(&mut self, waker: &Arc<dyn Waker>) -> bool {
        if self.thread.is_some() {
            return true;
        }
        let sh = self.shared.clone();
        let w = waker.clone();
        match std::thread::Builder::new().name("iron-sheets-persist".into()).spawn(move || persist_loop(sh, w)) {
            Ok(h) => {
                self.thread = Some(h);
                true
            }
            Err(_) => false,
        }
    }

    fn enqueue_workspace(&mut self, store: WorkspaceStore, ws: Workspace, waker: &Arc<dyn Waker>) {
        if !self.ensure_started(waker) {
            match save_workspace_if_needed(&store, &ws) {
                Ok(true) => self.shared.workspace_saved.store(true, Ordering::Release),
                Ok(false) => {}
                Err(e) => self.shared.mu.lock().errors.push(format!("Cannot save workspace: {e}")),
            }
            return;
        }
        self.shared.mu.lock().workspace = Some((store, ws));
        self.shared.wake_worker.notify_one();
    }

    fn enqueue_config(&mut self, dirs: AppDirs, config: AppConfig, waker: &Arc<dyn Waker>) {
        if !self.ensure_started(waker) {
            if let Err(e) = config.save(&dirs) {
                self.shared.mu.lock().errors.push(format!("Cannot save settings: {e}"));
            }
            return;
        }
        self.shared.mu.lock().config = Some((dirs, config));
        self.shared.wake_worker.notify_one();
    }

    fn enqueue_tree_cache(&mut self, name: String, path: PathBuf, snap: TreeCacheSnapshot, waker: &Arc<dyn Waker>) {
        if !self.ensure_started(waker) {
            let _ = TreeModel::save_cache_snapshot(&path, &snap);
            return;
        }
        self.shared.mu.lock().tree_cache = Some((name, path, snap));
        self.shared.wake_worker.notify_one();
    }

    fn drain_workspace_only(&self, name: &str) {
        let mut g = self.shared.mu.lock();
        if g.workspace.as_ref().is_some_and(|(_, w)| w.name == name) {
            g.workspace = None;
        }
        while g.in_flight || g.workspace.is_some() {
            self.shared.wake_done.wait(&mut g);
        }
    }

    fn take_and_drain_tree_cache(&self, name: &str) -> Option<(String, PathBuf, TreeCacheSnapshot)> {
        let mut g = self.shared.mu.lock();
        let taken = if g.tree_cache.as_ref().is_some_and(|(n, _, _)| n == name) { g.tree_cache.take() } else { None };
        while g.in_flight || g.tree_cache.is_some() {
            self.shared.wake_done.wait(&mut g);
        }
        taken
    }

    fn cancel_and_drain_workspace(&self, name: &str) {
        let mut g = self.shared.mu.lock();
        if g.workspace.as_ref().is_some_and(|(_, w)| w.name == name) {
            g.workspace = None;
        }
        if g.tree_cache.as_ref().is_some_and(|(n, _, _)| n == name) {
            g.tree_cache = None;
        }
        while g.in_flight || g.workspace.is_some() || g.tree_cache.is_some() {
            self.shared.wake_done.wait(&mut g);
        }
    }

    fn wait_idle(&self) {
        let mut g = self.shared.mu.lock();
        while g.in_flight || g.workspace.is_some() || g.config.is_some() || g.tree_cache.is_some() {
            self.shared.wake_done.wait(&mut g);
        }
    }

    fn drain_for_config(&self) {
        let mut g = self.shared.mu.lock();
        g.config = None;
        while g.in_flight || g.config.is_some() {
            self.shared.wake_done.wait(&mut g);
        }
    }

    fn take_updates(&self) -> (bool, Vec<String>) {
        let saved = self.shared.workspace_saved.swap(false, Ordering::AcqRel);
        let mut g = self.shared.mu.lock();
        let errs = std::mem::take(&mut g.errors);
        (saved, errs)
    }
}

pub(crate) fn globs_sig(globs: &[String]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |bytes: &[u8]| {
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    mix(&(globs.len() as u64).to_le_bytes());
    for g in globs {
        mix(&(g.len() as u64).to_le_bytes());
        mix(g.as_bytes());
    }
    h ^= h >> 30;
    h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^ (h >> 31)
}

fn start_runtime(
    waker: &Arc<dyn Waker>,
    dirs_store: &WorkspaceStore,
    ws: &Workspace,
    config: &AppConfig,
    index_delay: Duration,
) -> (TreeModel, Indexer, Option<Watcher>, Receiver<Vec<FsEvent>>) {
    let excludes = ExcludeSet::new(&config.exclude_globs);
    let mut tree = TreeModel::new(ws.roots.clone(), config.show_hidden_files);
    tree.set_filter(config.show_other_files, excludes.clone());
    tree.set_folder_notes(config.folder_notes, config.folder_note_hide);
    tree.set_filter_sig(globs_sig(&config.exclude_globs));
    tree.set_sort(ws.tree_sort);
    let w = waker.clone();
    let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(move || w.wake());
    tree.enable_async(notify.clone());
    for r in &ws.roots {
        tree.set_expanded(r, true);
    }
    tree.restore_expanded(&ws.session.expanded);
    let _ = tree.seed_from_cache(&dirs_store.tree_cache_file(&ws.name));
    tree.prefetch_startup();
    let indexer = Indexer::spawn_after(ws.roots.clone(), Some(dirs_store.index_cache_file(&ws.name)), notify, excludes.clone(), index_delay);
    let (tx, rx) = unbounded();
    let w = waker.clone();
    let watcher = Watcher::new_with_excludes(&ws.roots, excludes, move |evs| {
        let _ = tx.send(evs);
        w.wake();
    });
    (tree, indexer, watcher, rx)
}

pub(crate) struct CoreMut<'a> {
    pub(crate) waker: &'a mut Arc<dyn Waker>,
    pub(crate) dirs: &'a mut AppDirs,
    pub(crate) config: &'a mut AppConfig,
    pub(crate) store: &'a mut WorkspaceStore,
    pub(crate) workspace: &'a mut Workspace,
    pub(crate) docs: &'a mut DocStore,
    pub(crate) tabs: &'a mut Tabs,
    pub(crate) tree: &'a mut TreeModel,
    pub(crate) indexer: &'a mut Indexer,
    pub(crate) watcher: &'a mut Option<Watcher>,
    pub(crate) fs_rx: &'a mut Receiver<Vec<FsEvent>>,
    pub(crate) relink_tx: &'a mut Sender<crate::sidebar::moves::Done>,
    pub(crate) relink_rx: &'a mut Receiver<crate::sidebar::moves::Done>,
    pub(crate) trash_tx: &'a mut Sender<(PathBuf, std::io::Result<()>)>,
    pub(crate) trash_rx: &'a mut Receiver<(PathBuf, std::io::Result<()>)>,
    pub(crate) pending_trash: &'a mut usize,
    pub(crate) pending_move: &'a mut Option<crate::sidebar::moves::Pending>,
    pub(crate) commands: &'a mut Vec<Command>,
    pub(crate) status: &'a mut Status,
    pub(crate) notice: &'a mut Option<Notice>,
    pub(crate) removed_root: &'a mut Option<RemovedRoot>,
    pub(crate) dialogs: &'a mut Dialogs,
    pub(crate) sidebar_view: &'a mut SidebarView,
    pub(crate) perf: &'a mut Perf,
    pub(crate) math: &'a mut Box<dyn MathRenderer>,
    pub(crate) keymap: &'a mut crate::keymap::Keymap,
    pub(crate) settings: &'a mut SettingsUi,
    pub(crate) palette: &'a mut PaletteModel,
    pub(crate) symbol_mode: &'a mut InsertMode,
    pub(crate) search_request: &'a mut Option<String>,
    pub(crate) find_request: &'a mut bool,
    pub(crate) reveal_request: &'a mut Option<PathBuf>,
    pub(crate) runtime_gen: &'a mut u64,
    pub(crate) quit_ok: &'a mut bool,
    pub(crate) session_sig: &'a mut u64,
    pub(crate) session_dirty_at: &'a mut Option<Instant>,
    pub(crate) config_dirty_at: &'a mut Option<Instant>,
    pub(crate) rename_restart: &'a mut bool,
    pub(crate) settings_gen: &'a mut u64,
    pub(crate) config_gen: &'a mut u64,
    pub(crate) focus_empty_page: &'a mut bool,
    pub(crate) focus_sidebar: &'a mut bool,
    pub(crate) modules: &'a mut crate::modules::Registry,
    persist: &'a mut PersistWorker,
    workspace_list: &'a mut Vec<String>,
    workspace_list_at: &'a mut Option<Instant>,
    pub(crate) pending_link: &'a mut Option<crate::command::PendingLink>,
}

fn new_unit(waker: Arc<dyn Waker>, opts: AppOptions) -> AppState<()> {
    let AppOptions { dirs, config, start, paths, workspace, print_milestones, index_start_delay, config_error } = opts;
    let store = WorkspaceStore::new(&dirs);
    let (mut dirs_arg, mut files_arg) = (Vec::new(), Vec::new());
    for p in paths {
        let abs = std::path::absolute(&p).unwrap_or(p);
        if abs.is_dir() { dirs_arg.push(abs) } else { files_arg.push(abs) }
    }
    let ws = if !dirs_arg.is_empty() {
        let name =
            workspace.clone().unwrap_or_else(|| dirs_arg[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Default".into()));
        let mut ws = store.load(&name).unwrap_or_else(|| Workspace::new(name.clone()));
        let mut fresh = Workspace::new(name);
        for d in &dirs_arg {
            fresh.add_root(d);
        }
        if fresh.roots != ws.roots {
            ws = fresh;
        }
        ws
    } else if let Some(name) = workspace {
        store.load(&name).unwrap_or_else(|| Workspace::new(name))
    } else if let Some(ws) = config.last_workspace.as_deref().and_then(|n| store.load(n)) {
        ws
    } else {
        store.load("Default").unwrap_or_else(|| Workspace::new("Default"))
    };
    let mut config = config;
    config.last_workspace = Some(ws.name.clone());
    let (tree, indexer, watcher, fs_rx) = start_runtime(&waker, &store, &ws, &config, index_start_delay);
    let mut perf = Perf::new(start);
    perf.print = print_milestones;
    // `start_runtime` seeded the tree from its cache.
    perf.milestone("tree seeded");
    #[cfg(feature = "typst")]
    let typst = {
        let w = waker.clone();
        Arc::new(iron_sheets_typst::Engine::new(iron_sheets_typst::EngineConfig { system_fonts: config.typst_system_fonts, notify: Arc::new(move || w.wake()) }))
    };
    let untitled_dir = dirs.untitled_dir();
    let (relink_tx, relink_rx) = unbounded();
    let (trash_tx, trash_rx) = unbounded();
    let mut st = AppState {
        waker,
        dirs,
        config,
        store,
        workspace: ws,
        docs: DocStore::new(untitled_dir.clone()),
        tabs: Tabs::new(untitled_dir),
        tree,
        indexer,
        watcher,
        watch_limit_forced: false,
        fs_rx,
        relink_tx,
        relink_rx,
        trash_tx,
        trash_rx,
        pending_trash: 0,
        pending_move: None,
        commands: Vec::new(),
        status: Status::default(),
        notice: None,
        removed_root: None,
        dialogs: Dialogs::default(),
        sidebar_view: SidebarView::Files,
        perf,
        #[cfg(feature = "typst")]
        math: Box::new(crate::services::TypstMath::new(typst.clone())),
        #[cfg(not(feature = "typst"))]
        math: Box::new(NoMath),
        #[cfg(feature = "typst")]
        typst,
        keymap: crate::keymap::Keymap::defaults(),
        settings: Default::default(),
        palette: Default::default(),
        symbol_mode: InsertMode::default(),
        ui: (),
        search_request: None,
        find_request: false,
        reveal_request: None,
        runtime_gen: 0,
        quit_ok: false,
        session_sig: 0,
        session_dirty_at: None,
        config_dirty_at: None,
        had_focus: true,
        rename_restart: false,
        settings_gen: 0,
        config_gen: 0,
        focus_empty_page: false,
        focus_sidebar: false,
        focus_editor: false,
        stats: Default::default(),
        modules: Default::default(),
        persist: PersistWorker::default(),
        workspace_list: Vec::new(),
        workspace_list_at: None,
        pending_link: None,
    };
    st.restore_session();
    // A fresh start (nothing restored) is one empty tab; files given on the command line overwrite it.
    st.tabs.ensure_one();
    if files_arg.is_empty() {
        if let Some(active) = st.tabs.active_path().map(Path::to_path_buf) {
            st.docs.prefetch.request_open(&active, st.waker.clone());
        }
    }
    st.keymap = crate::keymap::Keymap::build(&st.config.keybindings);
    crate::settings_model::apply(&mut st, true);
    st.modules.set_roots(&st.workspace.roots);
    if let Some(e) = config_error {
        st.status.error(e);
    }
    if !st.keymap.warnings.is_empty() {
        let w = st.keymap.warnings.join("; ");
        st.status.error(format!("Ignored keybindings: {w}"));
    }
    for f in files_arg {
        st.commands.push(Command::OpenPath { path: f, new_tab: true, line: None });
    }
    st.session_sig = st.compute_session_sig();
    st.perf.milestone("state built");
    st
}

impl AppState<()> {
    fn with_ui<U>(self, ui: U) -> AppState<U> {
        AppState {
            waker: self.waker,
            dirs: self.dirs,
            config: self.config,
            store: self.store,
            workspace: self.workspace,
            docs: self.docs,
            tabs: self.tabs,
            tree: self.tree,
            indexer: self.indexer,
            watcher: self.watcher,
            watch_limit_forced: self.watch_limit_forced,
            fs_rx: self.fs_rx,
            relink_tx: self.relink_tx,
            relink_rx: self.relink_rx,
            trash_tx: self.trash_tx,
            trash_rx: self.trash_rx,
            pending_trash: self.pending_trash,
            pending_move: self.pending_move,
            commands: self.commands,
            status: self.status,
            notice: self.notice,
            removed_root: self.removed_root,
            dialogs: self.dialogs,
            sidebar_view: self.sidebar_view,
            perf: self.perf,
            math: self.math,
            #[cfg(feature = "typst")]
            typst: self.typst,
            keymap: self.keymap,
            settings: self.settings,
            palette: self.palette,
            symbol_mode: self.symbol_mode,
            ui,
            search_request: self.search_request,
            find_request: self.find_request,
            reveal_request: self.reveal_request,
            runtime_gen: self.runtime_gen,
            quit_ok: self.quit_ok,
            session_sig: self.session_sig,
            session_dirty_at: self.session_dirty_at,
            config_dirty_at: self.config_dirty_at,
            had_focus: self.had_focus,
            rename_restart: self.rename_restart,
            settings_gen: self.settings_gen,
            config_gen: self.config_gen,
            focus_empty_page: self.focus_empty_page,
            focus_sidebar: self.focus_sidebar,
            focus_editor: self.focus_editor,
            stats: self.stats,
            modules: self.modules,
            persist: self.persist,
            workspace_list: self.workspace_list,
            workspace_list_at: self.workspace_list_at,
            pending_link: self.pending_link,
        }
    }
}

fn is_active_tab_loading_parts(tabs: &Tabs, docs: &DocStore) -> bool {
    tabs.active_tab().is_some_and(|t| !t.is_empty() && t.load_error.is_none() && docs.get(&t.path).is_none())
}

fn is_blank_untitled_parts(tabs: &Tabs, docs: &DocStore, path: &Path) -> bool {
    tabs.is_untitled(path) && docs.get(path).is_some_and(|d| d.text.is_empty())
}

fn stored_parts(tabs: &Tabs, docs: &DocStore, t: &crate::tabs::Tab) -> bool {
    !t.is_empty() && !is_blank_untitled_parts(tabs, docs, &t.path)
}

fn session_parts(tabs: &Tabs, docs: &DocStore, tree: &TreeModel) -> Session {
    let active = tabs.active.map(|a| tabs.tabs[..a.min(tabs.tabs.len())].iter().filter(|t| stored_parts(tabs, docs, t)).count());
    Session {
        tabs: tabs
            .tabs
            .iter()
            .filter(|t| stored_parts(tabs, docs, t))
            .map(|t| TabState {
                path: t.path.clone(),
                view: t.view,
                cursor: t.cursor.as_ref().or(t.pending_cursor.as_ref()).map_or(0, |c| c.start as u32),
                scroll: t.editor_scroll.force.filter(|_| t.editor_scroll.offset == 0.0).unwrap_or(t.editor_scroll.offset),
            })
            .collect(),
        active,
        expanded: tree.expanded(),
    }
}

fn compute_session_sig_parts(tabs: &Tabs, docs: &DocStore, tree: &TreeModel) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = rustc_hash::FxHasher::default();
    for t in tabs.tabs.iter().filter(|t| stored_parts(tabs, docs, t)) {
        t.path.hash(&mut h);
        (t.view as u8).hash(&mut h);
    }
    tabs.active.map(|a| tabs.tabs[..a.min(tabs.tabs.len())].iter().filter(|t| stored_parts(tabs, docs, t)).count()).hash(&mut h);
    // O(1) order-independent hash avoids cloning & sorting expanded paths on every frame.
    tree.expanded_hash().hash(&mut h);
    h.finish()
}

fn default_view_for_cfg(config: &AppConfig, path: &Path) -> ViewMode {
    use iron_sheets_core::FileKind::*;
    match iron_sheets_core::FileKind::from_path(path) {
        Image | Excalidraw => ViewMode::Preview,
        Other => ViewMode::Source,
        Markdown | Typst => config.default_view,
    }
}

fn default_note_dir_parts(tabs: &Tabs, workspace: &Workspace) -> Option<PathBuf> {
    if let Some(p) = tabs.active_path() {
        if workspace.root_of(p).is_some() {
            if let Some(d) = p.parent() {
                return Some(d.to_path_buf());
            }
        }
    }
    workspace.roots.first().cloned()
}

impl CoreMut<'_> {
    pub(crate) fn notify(&mut self, text: impl Into<String>, action: Option<(&'static str, Command)>, secs: u64) {
        notify_inner(self.notice, text.into(), action, secs);
    }

    pub(crate) fn load_workspace(&mut self, ws: Workspace) {
        self.flush_session();
        self.flush_tree_cache();
        self.invalidate_workspace_list();
        // An undo offer belongs to the workspace it was made in.
        *self.removed_root = None;
        *self.notice = None;
        let untitled_dir = self.tabs.untitled_dir.clone();
        *self.docs = DocStore::new(untitled_dir.clone());
        *self.tabs = Tabs::new(untitled_dir);
        *self.workspace = ws;
        self.start_runtime();
        self.restore_session();
        self.tabs.ensure_one();
        if let Some(active) = self.tabs.active_path().map(Path::to_path_buf) {
            self.docs.prefetch.request_open(&active, self.waker.clone());
        }
        self.config.last_workspace = Some(self.workspace.name.clone());
        *self.config_dirty_at = Some(Instant::now());
        *self.session_sig = self.compute_session_sig();
    }

    pub(crate) fn start_runtime(&mut self) {
        let (tree, indexer, watcher, rx) = start_runtime(self.waker, self.store, self.workspace, self.config, Duration::ZERO);
        *self.tree = tree;
        *self.indexer = indexer;
        *self.watcher = watcher;
        *self.fs_rx = rx;
        *self.reveal_request = None;
        *self.runtime_gen += 1;
        self.modules.set_roots(&self.workspace.roots);
    }

    pub(crate) fn restart_indexer(&mut self, excludes: ExcludeSet) {
        let w = self.waker.clone();
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(move || w.wake());
        *self.indexer = Indexer::spawn_with(self.workspace.roots.clone(), Some(self.store.index_cache_file(&self.workspace.name)), notify, excludes);
    }

    pub(crate) fn refresh_workspace_list(&mut self) {
        let (saved, errs) = self.persist.take_updates();
        if saved {
            *self.workspace_list_at = None;
        }
        for e in errs {
            self.status.error(e);
        }
        if self.workspace_list_at.is_none_or(|t| t.elapsed() >= WORKSPACE_LIST_TTL) {
            *self.workspace_list = self.store.list();
            *self.workspace_list_at = Some(Instant::now());
        }
    }

    pub(crate) fn invalidate_workspace_list(&mut self) {
        *self.workspace_list_at = None;
    }

    pub(crate) fn is_active_tab_loading(&self) -> bool {
        is_active_tab_loading_parts(self.tabs, self.docs)
    }

    fn restore_session(&mut self) {
        if !self.config.restore_session {
            return;
        }
        let s = self.workspace.session.clone();
        for t in &s.tabs {
            let i = self.tabs.push_tab(t.path.clone(), t.view, true);
            let tab = &mut self.tabs.tabs[i];
            tab.pending_cursor = Some(t.cursor as usize..t.cursor as usize);
            tab.editor_scroll.force = Some(t.scroll);
        }
        self.tabs.active = match s.active {
            Some(a) if a < self.tabs.tabs.len() => Some(a),
            _ if !self.tabs.tabs.is_empty() => Some(0),
            _ => None,
        };
    }

    pub(crate) fn ensure_tab(&mut self) {
        self.tabs.ensure_one();
    }

    pub(crate) fn start_untitled(&mut self, text: &str) -> bool {
        let dir = self.tabs.untitled_dir.clone();
        let tabs = &*self.tabs;
        let n = iron_sheets_core::untitled::next_number(&dir, |n| tabs.is_open(&iron_sheets_core::untitled::path_for(&dir, n)));
        let path = iron_sheets_core::untitled::path_for(&dir, n);
        match self.docs.open(&path) {
            Ok(d) => {
                if !text.is_empty() {
                    d.apply_edit(0..0, text);
                }
            }
            Err(e) => {
                self.status.error(format!("Cannot open a new note: {e}"));
                return false;
            }
        }
        let view = self.default_view_for(&path);
        let i = if self.tabs.active_is_empty() { self.tabs.open_ex(&path, false, false, view, false).index } else { self.tabs.new_untitled(path, view) };
        if !text.is_empty() {
            self.tabs.tabs[i].pending_cursor = Some(text.len()..text.len());
        }
        true
    }

    pub(crate) fn new_untitled(&mut self) -> bool {
        let dir = self.tabs.untitled_dir.clone();
        let tabs = &*self.tabs;
        let n = iron_sheets_core::untitled::next_number(&dir, |n| tabs.is_open(&iron_sheets_core::untitled::path_for(&dir, n)));
        let path = iron_sheets_core::untitled::path_for(&dir, n);
        if let Err(e) = self.docs.open(&path) {
            self.status.error(format!("Cannot open a new note: {e}"));
            return false;
        }
        let view = self.default_view_for(&path);
        self.tabs.new_untitled(path, view);
        true
    }

    pub(crate) fn session(&self) -> Session {
        session_parts(self.tabs, self.docs, self.tree)
    }

    pub(crate) fn compute_session_sig(&self) -> u64 {
        compute_session_sig_parts(self.tabs, self.docs, self.tree)
    }

    pub(crate) fn flush_session(&mut self) {
        self.workspace.session = self.session();
        *self.session_sig = self.compute_session_sig();
        self.persist.drain_workspace_only(&self.workspace.name);
        match save_workspace_if_needed(self.store, self.workspace) {
            Ok(true) => self.invalidate_workspace_list(),
            Ok(false) => {}
            Err(e) => self.status.error(format!("Cannot save workspace: {e}")),
        }
        *self.session_dirty_at = None;
    }

    pub(crate) fn flush_session_async(&mut self) {
        self.workspace.session = self.session();
        *self.session_sig = self.compute_session_sig();
        *self.session_dirty_at = None;
        self.persist.enqueue_workspace(self.store.clone(), self.workspace.clone(), self.waker);
    }

    pub(crate) fn flush_tree_cache(&mut self) {
        if self.workspace.name.is_empty() {
            return;
        }
        let pending = self.persist.take_and_drain_tree_cache(&self.workspace.name);
        self.tree.set_filter_sig(globs_sig(&self.config.exclude_globs));
        if let Some((hash, snap)) = self.tree.cache_snapshot() {
            let path = self.store.tree_cache_file(&self.workspace.name);
            if TreeModel::save_cache_snapshot(&path, &snap).is_ok() {
                self.tree.mark_cache_persisted(hash);
            }
        } else if let Some((_, path, snap)) = pending {
            let _ = TreeModel::save_cache_snapshot(&path, &snap);
        }
    }

    pub(crate) fn flush_tree_cache_async(&mut self) {
        if self.workspace.name.is_empty() {
            return;
        }
        self.tree.set_filter_sig(globs_sig(&self.config.exclude_globs));
        if let Some((hash, snap)) = self.tree.cache_snapshot() {
            self.tree.mark_cache_persisted(hash);
            let path = self.store.tree_cache_file(&self.workspace.name);
            self.persist.enqueue_tree_cache(self.workspace.name.clone(), path, snap, self.waker);
        }
    }

    pub(crate) fn cancel_pending_workspace(&mut self, name: &str) {
        self.persist.cancel_and_drain_workspace(name);
    }

    pub(crate) fn wait_persist_idle(&self) {
        self.persist.wait_idle();
    }

    pub(crate) fn save_config_now(&mut self) {
        self.persist.drain_for_config();
        if let Err(e) = self.config.save(self.dirs) {
            self.status.error(format!("Cannot save settings: {e}"));
        }
        *self.config_dirty_at = None;
    }

    pub(crate) fn save_config_async(&mut self) {
        *self.config_dirty_at = None;
        self.persist.enqueue_config(self.dirs.clone(), self.config.clone(), self.waker);
    }

    pub(crate) fn poll_persist(&mut self) {
        let (saved, errs) = self.persist.take_updates();
        if saved {
            self.invalidate_workspace_list();
        }
        for e in errs {
            self.status.error(e);
        }
    }

    pub(crate) fn touch_config(&mut self) {
        *self.config_gen += 1;
        *self.config_dirty_at = Some(Instant::now());
    }

    pub(crate) fn active_path(&self) -> Option<PathBuf> {
        self.tabs.active_path().map(Path::to_path_buf)
    }

    pub(crate) fn default_view_for(&self, path: &Path) -> ViewMode {
        default_view_for_cfg(self.config, path)
    }

    pub(crate) fn default_note_dir(&self) -> Option<PathBuf> {
        default_note_dir_parts(self.tabs, self.workspace)
    }
}

impl<U> AppState<U> {
    pub(crate) fn core_mut(&mut self) -> CoreMut<'_> {
        CoreMut {
            waker: &mut self.waker,
            dirs: &mut self.dirs,
            config: &mut self.config,
            store: &mut self.store,
            workspace: &mut self.workspace,
            docs: &mut self.docs,
            tabs: &mut self.tabs,
            tree: &mut self.tree,
            indexer: &mut self.indexer,
            watcher: &mut self.watcher,
            fs_rx: &mut self.fs_rx,
            relink_tx: &mut self.relink_tx,
            relink_rx: &mut self.relink_rx,
            trash_tx: &mut self.trash_tx,
            trash_rx: &mut self.trash_rx,
            pending_trash: &mut self.pending_trash,
            pending_move: &mut self.pending_move,
            commands: &mut self.commands,
            status: &mut self.status,
            notice: &mut self.notice,
            removed_root: &mut self.removed_root,
            dialogs: &mut self.dialogs,
            sidebar_view: &mut self.sidebar_view,
            perf: &mut self.perf,
            math: &mut self.math,
            keymap: &mut self.keymap,
            settings: &mut self.settings,
            palette: &mut self.palette,
            symbol_mode: &mut self.symbol_mode,
            search_request: &mut self.search_request,
            find_request: &mut self.find_request,
            reveal_request: &mut self.reveal_request,
            runtime_gen: &mut self.runtime_gen,
            quit_ok: &mut self.quit_ok,
            session_sig: &mut self.session_sig,
            session_dirty_at: &mut self.session_dirty_at,
            config_dirty_at: &mut self.config_dirty_at,
            rename_restart: &mut self.rename_restart,
            settings_gen: &mut self.settings_gen,
            config_gen: &mut self.config_gen,
            focus_empty_page: &mut self.focus_empty_page,
            focus_sidebar: &mut self.focus_sidebar,
            modules: &mut self.modules,
            persist: &mut self.persist,
            workspace_list: &mut self.workspace_list,
            workspace_list_at: &mut self.workspace_list_at,
            pending_link: &mut self.pending_link,
        }
    }

    pub fn new(waker: Arc<dyn Waker>, opts: AppOptions, ui: U) -> AppState<U> {
        new_unit(waker, opts).with_ui(ui)
    }

    /// Replace the workspace: stops the old indexer/watcher, starts new ones, restores the session lazily.
    pub fn load_workspace(&mut self, ws: Workspace) {
        self.core_mut().load_workspace(ws);
    }

    pub fn start_runtime(&mut self) {
        self.core_mut().start_runtime();
    }

    /// The OS ran out of file-watch slots, so changes in some folders go unnoticed (shown in the status bar).
    pub fn watch_limit_hit(&self) -> bool {
        self.watch_limit_forced || self.watcher.as_ref().is_some_and(Watcher::limit_hit)
    }

    pub fn restart_indexer(&mut self, excludes: ExcludeSet) {
        self.core_mut().restart_indexer(excludes);
    }

    /// Cached list of workspace names (refreshed at most once every 2 seconds or after `invalidate_workspace_list`).
    pub fn workspace_list(&mut self) -> &[String] {
        self.core_mut().refresh_workspace_list();
        &self.workspace_list
    }

    pub fn invalidate_workspace_list(&mut self) {
        self.workspace_list_at = None;
    }

    pub fn is_active_tab_loading(&self) -> bool {
        is_active_tab_loading_parts(&self.tabs, &self.docs)
    }

    pub fn has_pending_trash(&self) -> bool {
        self.pending_trash > 0
    }

    /// Tabs are created without reading any file; the active tab's file loads when first shown.
    fn restore_session(&mut self) {
        self.core_mut().restore_session();
    }

    /// An untitled note without text (it has no file): nothing worth restoring.
    pub fn is_blank_untitled(&self, path: &Path) -> bool {
        is_blank_untitled_parts(&self.tabs, &self.docs, path)
    }

    /// There is never a state without tabs: one empty tab is added when the last one is gone.
    pub fn ensure_tab(&mut self) {
        self.tabs.ensure_one();
    }

    /// Start an untitled note holding `text` (possibly empty). The empty tab is overwritten when it is active, else
    /// the note opens in a new tab. The caret goes to the end of the text. Returns false when no name could be opened.
    pub fn start_untitled(&mut self, text: &str) -> bool {
        self.core_mut().start_untitled(text)
    }

    /// Open an empty untitled note in a new tab right after the active one and activate it. Nothing is written: the file
    /// appears with the first autosave that finds text. Returns false when no untitled name could be opened.
    pub fn new_untitled(&mut self) -> bool {
        self.core_mut().new_untitled()
    }

    /// Blank untitled notes and unstored tabs are left out; `active` is the index among the stored tabs.
    pub fn session(&self) -> Session {
        session_parts(&self.tabs, &self.docs, &self.tree)
    }

    pub fn compute_session_sig(&self) -> u64 {
        compute_session_sig_parts(&self.tabs, &self.docs, &self.tree)
    }

    pub fn flush_session(&mut self) {
        self.core_mut().flush_session();
    }

    /// Snapshot and enqueue the workspace/session save on the background persistence thread (coalesced).
    pub fn flush_session_async(&mut self) {
        self.core_mut().flush_session_async();
    }

    /// Synchronously persist the tree cache snapshot when expanded loaded dirs changed.
    pub fn flush_tree_cache(&mut self) {
        self.core_mut().flush_tree_cache();
    }

    /// Snapshot and enqueue the tree cache save on the background persistence thread (coalesced).
    pub fn flush_tree_cache_async(&mut self) {
        self.core_mut().flush_tree_cache_async();
    }

    pub fn cancel_pending_workspace(&mut self, name: &str) {
        self.core_mut().cancel_pending_workspace(name);
    }

    pub fn wait_persist_idle(&self) {
        self.persist.wait_idle();
    }

    pub fn save_config_now(&mut self) {
        self.core_mut().save_config_now();
    }

    /// Enqueue the current config snapshot on the background persistence thread (coalesced).
    pub fn save_config_async(&mut self) {
        self.core_mut().save_config_async();
    }

    pub fn poll_persist(&mut self) {
        self.core_mut().poll_persist();
    }

    pub fn touch_config(&mut self) {
        self.config_gen += 1;
        self.config_dirty_at = Some(Instant::now());
    }

    pub fn active_path(&self) -> Option<PathBuf> {
        self.tabs.active_path().map(Path::to_path_buf)
    }

    pub fn default_view_for(&self, path: &Path) -> ViewMode {
        default_view_for_cfg(&self.config, path)
    }

    /// Directory for new notes: the active file's folder when inside a root, else the first root.
    pub fn default_note_dir(&self) -> Option<PathBuf> {
        default_note_dir_parts(&self.tabs, &self.workspace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn async_persistence_coalesces_respects_sync_barrier_and_preserves_empty_invariant() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let opts = AppOptions::new(AppDirs::at(&home));
        let mut st = AppState::new(Arc::new(crate::test_support::NoopWaker), opts, ());

        // An empty workspace never saved to disk must NOT be created by flush_session_async().
        st.flush_session_async();
        st.wait_persist_idle();
        assert!(st.store.load("Default").is_none());

        // Rapid async writes coalesce, and a sync flush_session drains older async writes so newer state wins.
        let r1 = dir.path().join("r1");
        let r2 = dir.path().join("r2");
        std::fs::create_dir_all(&r1).unwrap();
        std::fs::create_dir_all(&r2).unwrap();
        st.workspace.add_root(&r1);
        st.flush_session_async();
        st.workspace.add_root(&r2);
        st.flush_session_async();
        st.workspace.tree_sort.desc = true;
        st.flush_session();
        let on_disk = st.store.load("Default").expect("saved");
        assert_eq!(on_disk.roots.len(), 2);
        assert!(on_disk.tree_sort.desc);

        // Removing all roots on an ALREADY-saved workspace DOES persist the empty state via async flush.
        st.workspace.roots.clear();
        st.flush_session_async();
        st.wait_persist_idle();
        let emptied = st.store.load("Default").expect("still exists as empty");
        assert!(emptied.roots.is_empty());

        // Async config save + sync barrier.
        st.config.editor_font_size = 21.0;
        st.save_config_async();
        st.config.editor_font_size = 23.0;
        st.save_config_now();
        assert_eq!(AppConfig::load(&st.dirs).editor_font_size, 23.0);

        // Workspace list cache refreshes immediately after invalidation.
        assert_eq!(st.workspace_list(), &["Default".to_string()]);
        st.store.save(&Workspace::new("Alpha")).unwrap();
        // Within TTL, cached value stays without hitting disk.
        assert_eq!(st.workspace_list(), &["Default".to_string()]);
        st.invalidate_workspace_list();
        assert_eq!(st.workspace_list(), &["Alpha".to_string(), "Default".to_string()]);
    }

    #[test]
    fn tree_cache_async_flush_barrier_and_delete_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let root = dir.path().join("vault");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("note.md"), "# Note").unwrap();
        let mut opts = AppOptions::new(AppDirs::at(&home));
        opts.paths = vec![root.clone()];
        opts.workspace = Some("TreeWs".into());
        let mut st = AppState::new(Arc::new(crate::test_support::NoopWaker), opts, ());
        st.tree.wait_idle();

        // Async tree cache flush writes tree-TreeWs.bin via PersistWorker.
        st.flush_tree_cache_async();
        st.wait_persist_idle();
        let cache_file = st.store.tree_cache_file("TreeWs");
        assert!(cache_file.exists(), "tree cache persisted");

        // Second flush when unchanged is a no-op (does not rewrite mtime).
        let past = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        std::fs::OpenOptions::new().write(true).open(&cache_file).unwrap().set_modified(past).unwrap();
        st.flush_tree_cache();
        assert_eq!(std::fs::metadata(&cache_file).unwrap().modified().unwrap(), past);

        // Switching to a fresh workspace flushes pending changes and seeds cleanly on reload.
        std::fs::write(root.join("extra.md"), "# Extra").unwrap();
        st.tree.invalidate(&root);
        st.tree.wait_idle();
        st.load_workspace(Workspace::new("Other"));
        assert_ne!(std::fs::metadata(&cache_file).unwrap().modified().unwrap(), past, "load_workspace flushed old workspace tree cache");

        // DeleteWorkspaceForce cancellation prevents stale async writes from recreating deleted tree cache.
        st.workspace.add_root(&root);
        st.start_runtime();
        st.tree.wait_idle();
        st.flush_tree_cache_async();
        st.cancel_pending_workspace("Other");
        st.store.delete("Other").unwrap();
        st.wait_persist_idle();
        assert!(!st.store.tree_cache_file("Other").exists());
    }
}
