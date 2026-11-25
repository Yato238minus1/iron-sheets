//! Commands: the only way modules cause side effects. `execute` runs them at the start of a frame.
use crate::dialogs::{AfterSave, Dialog, PromptAction};
use crate::editor::selection::Selections;
use crate::editor::transaction::{Edit, Origin, Transaction};
use crate::docs::DocStore;
use crate::keys::{Key, Mods, Shortcut};
use crate::palette_model::PaletteMode;
use crate::platform::{Platform, Waker};
use crate::state::{AppState, CoreMut, RemovedRoot, SidebarView};
use crate::tabs::Tabs;
use iron_sheets_core::FileKind;
use iron_sheets_core::config::{ThemePref, ViewMode};
use iron_sheets_core::index::Indexer;
use iron_sheets_core::ops;
use iron_sheets_core::workspace::{AddRoot, Workspace};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum Command {
    /// A command of a feature module (`id` names it, `arg` is free text); offered to every enabled module.
    Module {
        id: &'static str,
        arg: String,
    },
    /// `line` is 0-based.
    OpenPath {
        path: PathBuf,
        new_tab: bool,
        line: Option<u32>,
    },
    OpenLink {
        note: String,
        heading: Option<String>,
        from: Option<PathBuf>,
        new_tab: bool,
    },
    /// Calendar: opens the daily note of a day (today when `None`), creating it when it is missing.
    #[cfg(feature = "calendar")]
    OpenDaily(Option<crate::calendar::Ymd>),
    /// Creates a missing note targeted by a wiki-link (after confirmation) or Quick Switcher and opens it.
    CreateLinkedNote {
        note: String,
        from: Option<PathBuf>,
        new_tab: bool,
    },
    OpenUrl(String),
    /// Open a new empty tab (no editor, no preview). While an empty tab is already active it only takes the focus.
    NewTab,
    /// Start an untitled note with `text` (typed on the empty tab, or empty from its "New untitled note" action). It
    /// overwrites the active empty tab, else opens in a new tab.
    StartUntitled {
        text: String,
    },
    CloseTab(usize),
    CloseActiveTab,
    /// Tries again to load the file of tab `.0` that failed to open (clears its `load_error`).
    RetryOpen(usize),
    CloseOtherTabs(usize),
    /// Closes every tab right of tab `.0`.
    CloseTabsToRight(usize),
    CloseAllTabs,
    ActivateTab(usize),
    NextTab,
    PrevTab,
    ReopenClosedTab,
    HistoryBack,
    HistoryForward,
    Save,
    /// Save the untitled note `path` to a name and folder chosen by the user (in-app prompt; native dialog without roots).
    SaveAs {
        path: PathBuf,
        after: AfterSave,
    },
    /// The native "save as" dialog for `path`; handed to the frontend by `execute`.
    SaveAsDialog {
        path: PathBuf,
        after: AfterSave,
    },
    /// Move the untitled note `from` to `to` (a full path) and retarget its tab.
    SaveAsTo {
        from: PathBuf,
        to: PathBuf,
        after: AfterSave,
    },
    SaveAll,
    NewNote {
        dir: Option<PathBuf>,
    },
    NewFolder {
        parent: Option<PathBuf>,
    },
    Rename {
        path: PathBuf,
    },
    /// Rename or move a file or folder to a new full path; see `sidebar::moves::mv` for `links` and `dnd`.
    Move {
        from: PathBuf,
        to: PathBuf,
        links: Option<bool>,
        dnd: bool,
    },
    Trash {
        path: PathBuf,
    },
    RevealInFileManager(PathBuf),
    OpenExternal(PathBuf),
    CopyPath(PathBuf),
    ToggleSidebar,
    FocusSidebar,
    ToggleWrap,
    SetSidebarView(SidebarView),
    SetViewMode(ViewMode),
    CycleViewMode,
    ToggleSyncScroll,
    QuickOpen,
    CommandPalette,
    GlobalSearch {
        query: Option<String>,
    },
    FindInNote,
    AddRootDialog,
    AddRootByPath,
    AddRoot(PathBuf),
    RemoveRoot(usize),
    UndoRemoveRoot,
    SetTreeSort(iron_sheets_core::tree::TreeSort),
    CollapseTree,
    SwitchWorkspace(String),
    /// Switch without the unsaved-changes check (after Discard).
    SwitchWorkspaceForce(String),
    NewWorkspace,
    RenameWorkspace,
    DeleteWorkspace,
    Rescan,
    InsertText(String),
    /// Sugar for `Apply` with one edit; the caret goes to `select`, or behind the inserted text.
    ApplyEdit {
        path: PathBuf,
        range: Range<usize>,
        insert: String,
        select: Option<Range<usize>>,
    },
    /// Apply a transaction to the document of `path` (ignored for read-only documents).
    Apply(PathBuf, Transaction),
    /// Stage (`stage`) or unstage one change of the note at `path` in the Git index; the note's text goes to the Git worker.
    #[cfg(feature = "git")]
    GitHunk {
        path: PathBuf,
        mark: crate::git::DiffMark,
        stage: bool,
    },
    ScrollToLine(u32),
    SetTheme(ThemePref),
    /// Whole interface zoom (and, for reset, the editor and preview font sizes back to their defaults).
    ZoomIn,
    ZoomOut,
    ZoomReset,
    /// Editor and preview font size together, in whole points (Ctrl+wheel).
    FontZoom(i32),
    /// Preview font family id (see `settings_model::PREVIEW_FONTS`).
    SetPreviewFont(String),
    TogglePerfOverlay,
    ToggleProfessional,
    OpenThemesFolder,
    Quit,
    // Internal / extra.
    OpenFileDialog,
    /// Export the active Typst note as PDF (save dialog, then the engine writes it).
    ExportPdf,
    About,
    OpenSettings,
    PromptSubmit(PromptAction, String),
    TrashConfirmed(PathBuf),
    SetConfirmTrash(bool),
    DeleteWorkspaceConfirmed(String),
    DeleteWorkspaceForce(String),
    SaveDoc(PathBuf),
    CloseTabByPath {
        path: PathBuf,
        discard: bool,
    },
    ReloadDoc(PathBuf),
    KeepMine(PathBuf),
    /// Writes a note whose file was deleted on disk back (the "Save again" action).
    SaveAgain(PathBuf),
    QuitNow,
}

pub struct CommandInfo {
    pub id: &'static str,
    pub title: &'static str,
    /// Menu bar menu, if any.
    pub menu: Option<&'static str>,
    pub shortcut: Option<Shortcut>,
    /// Second default binding (for example zoom reset on both Ctrl+= and Ctrl+0).
    pub extra_shortcut: Option<Shortcut>,
    /// Builds the command for the current state; `None` when not applicable (menu item disabled).
    pub make: fn(&Tabs) -> Option<Command>,
}

const fn sc(m: Mods, k: Key) -> Option<Shortcut> {
    Some(Shortcut::new(m, k))
}

const C: Mods = Mods::COMMAND;
const CS: Mods = Mods::COMMAND.plus(Mods::SHIFT);

/// A Git module command for the repo of the active note (empty = the first root's repo).
#[cfg(feature = "git")]
fn git_cmd(id: &'static str, s: &Tabs) -> Command {
    Command::Module { id, arg: s.active_path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default() }
}

#[cfg(feature = "pdf")]
fn active(s: &Tabs) -> Option<PathBuf> {
    s.active_path().map(Path::to_path_buf)
}

/// Active document that lives in the workspace or elsewhere on disk, not an untitled note in the app data folder.
fn active_real(s: &Tabs) -> Option<PathBuf> {
    s.active_path().filter(|p| !s.is_untitled(p)).map(Path::to_path_buf)
}

fn active_untitled(s: &Tabs) -> Option<PathBuf> {
    s.active_path().filter(|p| s.is_untitled(p)).map(Path::to_path_buf)
}

macro_rules! cmd {
    ($id:expr, $title:expr, $menu:expr, $sc:expr, $make:expr) => {
        CommandInfo { id: $id, title: $title, menu: $menu, shortcut: $sc, extra_shortcut: None, make: $make }
    };
    ($id:expr, $title:expr, $menu:expr, $sc:expr, $extra:expr, $make:expr) => {
        CommandInfo { id: $id, title: $title, menu: $menu, shortcut: $sc, extra_shortcut: $extra, make: $make }
    };
}

/// Range of the editor and preview font size in points (the settings use the same).
pub const FONT_MIN: f32 = 6.0;
pub const FONT_MAX: f32 = 60.0;

static COMMANDS: &[CommandInfo] = &[
    cmd!("file.new_note", "New note", Some("File"), sc(C, Key::N), |_| Some(Command::NewNote { dir: None })),
    cmd!("file.new_folder", "New folder", Some("File"), None, |_| Some(Command::NewFolder { parent: None })),
    cmd!("file.open", "Open file...", Some("File"), None, |_| Some(Command::OpenFileDialog)),
    cmd!("app.settings", "Settings...", Some("File"), sc(C, Key::Comma), |_| Some(Command::OpenSettings)),
    cmd!("file.quick_open", "Quick open", Some("File"), sc(C, Key::O), |_| Some(Command::QuickOpen)),
    cmd!("file.save", "Save", Some("File"), sc(C, Key::S), |s| s.active.is_some().then_some(Command::Save)),
    #[cfg(feature = "pdf")]
    cmd!("file.export_pdf", "Export PDF...", Some("File"), None, |s| active(s)
        .filter(|p| matches!(iron_sheets_core::FileKind::from_path(p), iron_sheets_core::FileKind::Typst))
        .map(|_| Command::ExportPdf)),
    cmd!("file.save_as", "Save as...", Some("File"), sc(CS, Key::S), |s| active_untitled(s).map(|path| Command::SaveAs { path, after: AfterSave::Nothing })),
    cmd!("file.save_all", "Save all", Some("File"), None, |_| Some(Command::SaveAll)),
    cmd!("file.rename", "Rename file...", Some("File"), sc(Mods::NONE, Key::F2), |s| active_real(s).map(|path| Command::Rename { path })),
    cmd!("file.trash", "Move file to trash", Some("File"), None, |s| active_real(s).map(|path| Command::Trash { path })),
    cmd!("file.reveal", "Reveal in file manager", Some("File"), None, |s| active_real(s).map(Command::RevealInFileManager)),
    cmd!("file.open_external", "Open in default app", Some("File"), None, |s| active_real(s).map(Command::OpenExternal)),
    cmd!("file.copy_path", "Copy path", Some("File"), None, |s| active_real(s).map(Command::CopyPath)),
    cmd!("tab.new", "New tab", Some("File"), sc(C, Key::T), |_| Some(Command::NewTab)),
    cmd!("file.new_untitled", "New untitled note", Some("File"), None, |_| Some(Command::StartUntitled { text: String::new() })),
    cmd!("tab.close", "Close tab", Some("File"), sc(C, Key::W), |s| s.active.map(|_| Command::CloseActiveTab)),
    cmd!("tab.close_all", "Close all tabs", None, None, |s| (!s.tabs.is_empty()).then_some(Command::CloseAllTabs)),
    cmd!("tab.close_right", "Close tabs to the right", None, None, |s| s.active.filter(|&i| i + 1 < s.tabs.len()).map(Command::CloseTabsToRight)),
    cmd!("tab.reopen", "Reopen closed tab", Some("File"), sc(CS, Key::T), |s| (!s.closed.is_empty()).then_some(Command::ReopenClosedTab)),
    cmd!("app.quit", "Quit", Some("File"), None, |_| Some(Command::Quit)),
    cmd!("edit.find", "Find in note", Some("Edit"), sc(C, Key::F), |s| s.active.is_some().then_some(Command::FindInNote)),
    cmd!("edit.search", "Search in workspace", Some("Edit"), sc(CS, Key::F), |_| Some(Command::GlobalSearch { query: None })),
    cmd!("edit.palette", "Command palette", Some("Edit"), sc(C, Key::P), |_| Some(Command::CommandPalette)),
    cmd!("nav.back", "Navigate back", Some("Edit"), sc(Mods::ALT, Key::ArrowLeft), |s| s.active_tab().filter(|t| t.can_back()).map(|_| Command::HistoryBack)),
    cmd!("nav.forward", "Navigate forward", Some("Edit"), sc(Mods::ALT, Key::ArrowRight), |s| s
        .active_tab()
        .filter(|t| t.can_forward())
        .map(|_| Command::HistoryForward)),
    cmd!("tab.next", "Next tab", Some("Edit"), sc(Mods::CTRL, Key::Tab), |_| Some(Command::NextTab)),
    cmd!("tab.prev", "Previous tab", Some("Edit"), sc(Mods::CTRL.plus(Mods::SHIFT), Key::Tab), |_| Some(Command::PrevTab)),
    cmd!("view.sidebar", "Toggle sidebar", Some("View"), sc(C, Key::B), sc(C, Key::Backslash), |_| Some(Command::ToggleSidebar)),
    cmd!("view.focus_sidebar", "Focus sidebar", Some("View"), sc(CS, Key::E), |_| Some(Command::FocusSidebar)),
    cmd!("view.wrap", "Toggle line wrap", Some("View"), sc(Mods::ALT, Key::Z), |_| Some(Command::ToggleWrap)),
    cmd!("view.files", "Show files", Some("View"), None, |_| Some(Command::SetSidebarView(SidebarView::Files))),
    cmd!("view.search", "Show search", Some("View"), None, |_| Some(Command::SetSidebarView(SidebarView::Search))),
    cmd!("view.outline", "Show outline", Some("View"), None, |_| Some(Command::SetSidebarView(SidebarView::Outline))),
    cmd!("view.links", "Show links", Some("View"), None, |_| Some(Command::SetSidebarView(SidebarView::Links))),
    cmd!("view.symbols", "Show symbols", Some("View"), None, |_| Some(Command::SetSidebarView(SidebarView::Symbols))),
    #[cfg(feature = "calendar")]
    cmd!("view.calendar", "Show calendar", Some("View"), None, |_| Some(Command::SetSidebarView(SidebarView::Calendar))),
    #[cfg(feature = "calendar")]
    cmd!("calendar.today", "Open today's daily note", Some("File"), None, |_| Some(Command::OpenDaily(None))),
    #[cfg(feature = "git")]
    cmd!("view.git", "Show Git changes", Some("View"), None, |_| Some(Command::SetSidebarView(SidebarView::Git))),
    #[cfg(feature = "git")]
    cmd!("git.push", "Git: push", None, None, |s| Some(git_cmd("git.push", s))),
    #[cfg(feature = "git")]
    cmd!("git.pull", "Git: pull (fast-forward only)", None, None, |s| Some(git_cmd("git.pull", s))),
    #[cfg(feature = "git")]
    cmd!("git.fetch", "Git: fetch", None, None, |s| Some(git_cmd("git.fetch", s))),
    #[cfg(feature = "git")]
    cmd!("git.branches", "Git: switch branch...", None, None, |s| Some(git_cmd("git.branches", s))),
    #[cfg(feature = "git")]
    cmd!("git.remotes", "Git: manage remotes...", None, None, |s| Some(git_cmd("git.remotes", s))),
    #[cfg(feature = "git")]
    cmd!("git.init", "Git: initialize repository", None, None, |s| Some(git_cmd("git.init", s))),
    #[cfg(feature = "git")]
    cmd!("git.hunk", "Git: show the change at the caret", Some("Edit"), sc(Mods::ALT, Key::H), |s| Some(git_cmd("git.hunk", s))),
    #[cfg(feature = "git")]
    cmd!("git.hunk_next", "Git: next change", Some("Edit"), sc(Mods::ALT, Key::CloseBracket), |s| Some(git_cmd("git.hunk_next", s))),
    #[cfg(feature = "git")]
    cmd!("git.hunk_prev", "Git: previous change", Some("Edit"), sc(Mods::ALT, Key::OpenBracket), |s| Some(git_cmd("git.hunk_prev", s))),
    #[cfg(feature = "git")]
    cmd!("git.undo_discard", "Git: undo last discard", None, None, |s| Some(git_cmd("git.undo_discard", s))),
    cmd!("view.source", "Source mode", Some("View"), None, |_| Some(Command::SetViewMode(ViewMode::Source))),
    cmd!("view.split", "Split mode", Some("View"), None, |_| Some(Command::SetViewMode(ViewMode::Split))),
    cmd!("view.preview", "Preview mode", Some("View"), None, |_| Some(Command::SetViewMode(ViewMode::Preview))),
    cmd!("view.cycle", "Cycle view mode", Some("View"), sc(C, Key::E), |_| Some(Command::CycleViewMode)),
    cmd!("view.sync", "Toggle synchronized scrolling", Some("View"), None, |_| Some(Command::ToggleSyncScroll)),
    cmd!("view.theme_system", "Theme: system", Some("View"), None, |_| Some(Command::SetTheme(ThemePref::System))),
    cmd!("view.theme_light", "Theme: light", Some("View"), None, |_| Some(Command::SetTheme(ThemePref::Light))),
    cmd!("view.theme_dark", "Theme: dark", Some("View"), None, |_| Some(Command::SetTheme(ThemePref::Dark))),
    // Ctrl+= is zoom in too: on a US layout "+" is Shift+=, and Ctrl+0 resets.
    cmd!("view.zoom_in", "Zoom in", Some("View"), sc(C, Key::Plus), sc(C, Key::Equals), |_| Some(Command::ZoomIn)),
    cmd!("view.zoom_out", "Zoom out", Some("View"), sc(C, Key::Minus), |_| Some(Command::ZoomOut)),
    cmd!("view.zoom_reset", "Reset zoom and font sizes", Some("View"), sc(C, Key::Num0), |_| Some(Command::ZoomReset)),
    cmd!("view.font_up", "Larger editor and preview font", None, None, |_| Some(Command::FontZoom(1))),
    cmd!("view.font_down", "Smaller editor and preview font", None, None, |_| Some(Command::FontZoom(-1))),
    cmd!("view.perf", "Toggle performance overlay", Some("View"), sc(Mods::NONE, Key::F12), |_| Some(Command::TogglePerfOverlay)),
    cmd!("view.professional", "Toggle professional mode", Some("View"), None, |_| Some(Command::ToggleProfessional)),
    cmd!("view.themes_folder", "Open themes folder", Some("View"), None, |_| Some(Command::OpenThemesFolder)),
    cmd!("ws.add_folder", "Add folder to workspace...", Some("Workspace"), None, |_| Some(Command::AddRootDialog)),
    cmd!("ws.add_folder_path", "Add folder by path...", Some("Workspace"), None, |_| Some(Command::AddRootByPath)),
    // Always listed: `make` sees only the tabs; with nothing to undo the command says so.
    cmd!("ws.undo_remove", "Undo remove folder", Some("Workspace"), None, |_| Some(Command::UndoRemoveRoot)),
    cmd!("ws.new", "New workspace...", Some("Workspace"), None, |_| Some(Command::NewWorkspace)),
    cmd!("ws.rename", "Rename workspace...", Some("Workspace"), None, |_| Some(Command::RenameWorkspace)),
    cmd!("ws.delete", "Delete workspace...", Some("Workspace"), None, |_| Some(Command::DeleteWorkspace)),
    cmd!("ws.rescan", "Rescan workspace", Some("Workspace"), None, |_| Some(Command::Rescan)),
    cmd!("help.about", "About iron sheets", Some("Help"), None, |_| Some(Command::About)),
];

pub fn all_commands() -> &'static [CommandInfo] {
    COMMANDS
}

/// Apply `tx` to the document of `path` (nothing happens when it is read-only or the edits are invalid) and move the tab's caret to the transaction's primary selection, if it has one.
fn apply_transaction(state: &mut CoreMut<'_>, path: &Path, tx: Transaction) {
    let caret = tx.select.as_ref().map(Selections::primary_range);
    let Some(doc) = state.docs.get_mut(path) else { return };
    if doc.apply(tx).is_ok() {
        if let (Some(i), Some(sel)) = (state.tabs.find(path), caret) {
            state.tabs.tabs[i].pending_cursor = Some(sel);
        }
    }
}

fn line_start(text: &str, line: u32) -> usize {
    if line == 0 {
        return 0;
    }
    let mut seen = 0;
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            seen += 1;
            if seen == line {
                return i + 1;
            }
        }
    }
    text.len()
}

fn parent_of(p: &Path) -> PathBuf {
    p.parent().map(Path::to_path_buf).unwrap_or_default()
}

/// Called after a doc was written: tell the indexer to re-read the file.
pub fn after_save<U>(state: &mut AppState<U>, path: &Path) {
    after_save_parts(&mut state.docs, &mut state.indexer, &mut state.modules, path);
}

fn after_save_parts(docs: &mut DocStore, indexer: &mut Indexer, modules: &mut crate::modules::Registry, path: &Path) {
    if let Some(d) = docs.get_mut(path) {
        d.indexed_version = d.version;
    }
    indexer.file_changed(path.to_path_buf());
    modules.on_save(path);
}

pub fn save_path<U>(state: &mut AppState<U>, path: &Path, announce: bool) -> bool {
    save_path_inner(&mut state.core_mut(), path, announce)
}

fn save_path_inner(state: &mut CoreMut<'_>, path: &Path, announce: bool) -> bool {
    match state.docs.save(path) {
        Ok(true) => {
            after_save_parts(state.docs, state.indexer, state.modules, path);
            if announce {
                state.status.info("Saved");
            }
            true
        }
        Ok(false) => true,
        Err(e) => {
            state.status.error(format!("Cannot save {}: {e}", path.display()));
            false
        }
    }
}

/// Writes `path` on the background worker; the result is reported by `poll_saves`. Overwrites a file that changed on
/// disk, like `save_path`. For saves nothing else waits for.
pub fn save_path_async<U>(state: &mut AppState<U>, path: &Path, announce: bool) {
    save_path_async_parts(&mut state.docs, &state.waker, path, announce);
}

fn save_path_async_parts(docs: &mut DocStore, waker: &Arc<dyn Waker>, path: &Path, announce: bool) {
    docs.save_async(path, true, announce, waker);
}

/// After a doc's text was replaced from outside the editor, pull stale cursors back onto char boundaries.
pub fn fix_cursors<U>(state: &mut AppState<U>, path: &Path) {
    fix_cursors_parts(&state.docs, &mut state.tabs, path);
}

fn fix_cursors_parts(docs: &DocStore, tabs: &mut Tabs, path: &Path) {
    let Some(doc) = docs.get(path) else { return };
    let text = &doc.text;
    let fix = |r: &mut Option<std::ops::Range<usize>>| {
        if let Some(r) = r {
            r.start = crate::editor::floor_boundary(text, r.start);
            r.end = crate::editor::floor_boundary(text, r.end).max(r.start);
        }
    };
    for t in tabs.tabs.iter_mut().filter(|t| t.path == path) {
        fix(&mut t.cursor);
        fix(&mut t.pending_cursor);
    }
}

/// Autosave: like `save_path` but never overwrites a file that changed on disk.
pub fn autosave_path<U>(state: &mut AppState<U>, path: &Path) -> bool {
    autosave_path_inner(&mut state.core_mut(), path)
}

fn autosave_path_inner(state: &mut CoreMut<'_>, path: &Path) -> bool {
    if state.docs.get(path).is_some_and(|d| d.autosave_blocked()) {
        return true;
    }
    save_path_inner(state, path, false)
}

/// `autosave_path` on the background worker, for autosave timers, focus loss and tab switches.
pub fn autosave_async<U>(state: &mut AppState<U>, path: &Path) {
    autosave_async_parts(&mut state.docs, &state.waker, path);
}

fn autosave_async_parts(docs: &mut DocStore, waker: &Arc<dyn Waker>, path: &Path) {
    if !docs.get(path).is_some_and(|d| d.autosave_blocked()) {
        docs.save_async(path, false, false, waker);
    }
}

/// `save_all` on the background worker (focus loss).
pub fn save_all_async<U>(state: &mut AppState<U>) {
    save_all_async_inner(&mut state.core_mut());
}

fn save_all_async_inner(state: &mut CoreMut<'_>) {
    for p in state.docs.dirty_paths() {
        autosave_async_parts(state.docs, state.waker, &p);
    }
}

/// Reports the background saves that finished: index update, "Saved", errors.
pub fn poll_saves<U>(state: &mut AppState<U>) {
    poll_saves_inner(&mut state.core_mut());
}

fn poll_saves_inner(state: &mut CoreMut<'_>) {
    for f in state.docs.poll_saves() {
        match f.error {
            None => {
                if f.reloaded {
                    fix_cursors_parts(state.docs, state.tabs, &f.path);
                }
                // Edited meanwhile: the follow-up save (and the idle indexing of the text) takes care of it.
                if !state.docs.get(&f.path).is_some_and(|d| d.is_dirty()) {
                    after_save_parts(state.docs, state.indexer, state.modules, &f.path);
                }
                if f.announce {
                    state.status.info("Saved");
                }
            }
            Some(e) => state.status.error(format!("Cannot save {}: {e}", f.path.display())),
        }
    }
}

/// Save every dirty doc except those with an unresolved external change, and wait for the writes.
pub fn save_all<U>(state: &mut AppState<U>) {
    save_all_inner(&mut state.core_mut());
}

fn save_all_inner(state: &mut CoreMut<'_>) {
    for p in state.docs.dirty_paths() {
        autosave_path_inner(state, &p);
    }
}

/// Explicit "Save all": also overwrites externally changed files.
pub fn save_all_force<U>(state: &mut AppState<U>) {
    save_all_force_inner(&mut state.core_mut());
}

fn save_all_force_inner(state: &mut CoreMut<'_>) {
    for p in state.docs.dirty_paths() {
        save_path_inner(state, &p, false);
    }
}

fn release_doc(state: &mut CoreMut<'_>, path: &Path) {
    let tabs = &*state.tabs;
    state.docs.close_if_unused(path, |p| tabs.is_open(p));
}

/// Forget an untitled note for good: its document and its file in the app data folder. Only called for notes the user
/// closed or discarded, or that have no text.
fn drop_untitled(state: &mut CoreMut<'_>, path: &Path) {
    if state.tabs.is_open(path) || !state.tabs.is_untitled(path) {
        return;
    }
    state.docs.remove(path);
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            state.status.error(format!("Cannot delete {}: {e}", path.display()));
        }
    }
}

/// Closes the tabs whose index `pick`s; dirty ones and untitled ones with text stay open and are reported.
fn close_tabs(state: &mut CoreMut<'_>, pick: impl Fn(usize) -> bool) {
    let mut skipped = 0;
    for j in (0..state.tabs.tabs.len()).rev().filter(|&j| pick(j)) {
        let p = state.tabs.tabs[j].path.clone();
        if state.tabs.is_untitled(&p) {
            // Blank ones just go; one with text (or still loading) needs the user's decision, so it stays.
            if untitled_is_empty(state, &p) || (state.docs.get(&p).is_none() && state.tabs.tabs[j].load_error.is_some()) {
                do_close(state, j, true);
            } else {
                if state.docs.get(&p).is_none() {
                    state.docs.prefetch.request_open(&p, state.waker.clone());
                }
                skipped += 1;
            }
            continue;
        }
        if state.config.autosave {
            autosave_path_inner(state, &p);
        }
        if state.docs.get(&p).is_some_and(|d| d.is_dirty()) {
            skipped += 1;
        } else {
            do_close(state, j, false);
        }
    }
    if skipped > 0 {
        state.status.info(format!("{skipped} tab(s) with unsaved changes were kept"));
    }
}

/// True when `path` is an untitled note whose text is already loaded or prefetched and empty (non-blocking).
fn untitled_is_empty(state: &mut CoreMut<'_>, path: &Path) -> bool {
    if !state.tabs.is_untitled(path) {
        return false;
    }
    if state.docs.get(path).is_none()
        && let Some(Err(e)) = state.docs.take_open(path)
    {
        // Only the active tab gets this from `poll_open`; a background tab would otherwise never be closable.
        let msg = e.to_string();
        state.tabs.tabs.iter_mut().filter(|t| t.path == path).for_each(|t| t.load_error = Some(msg.clone()));
    }
    state.docs.get(path).is_some_and(|d| d.text.is_empty())
}

/// Opens `path` in a tab. The click frame never reads the file: a note that is neither open nor prefetched gets its tab
/// at once and loads on the prefetch worker (`poll_open` installs it); `line` waits in `Tab::pending_line` until then.
fn open_path(state: &mut CoreMut<'_>, path: PathBuf, new_tab: bool, line: Option<u32>) {
    open_path_ex(state, path, new_tab, line, false);
}

/// Like `open_path`, but reads a file that was just created here (tiny) on the spot, so no loading frame shows.
pub(crate) fn open_created(state: &mut CoreMut<'_>, path: PathBuf, new_tab: bool) {
    open_path_ex(state, path, new_tab, None, true);
}

fn open_path_ex(state: &mut CoreMut<'_>, path: PathBuf, new_tab: bool, line: Option<u32>, sync: bool) {
    let path = std::path::absolute(&path).unwrap_or(path);
    if sync && path.is_dir() {
        state.status.error(format!("{} is a folder", path.display()));
        return;
    }
    // Autosave on tab switch.
    if state.config.autosave {
        if let Some(a) = state.active_path() {
            autosave_async_parts(state.docs, state.waker, &a);
        }
    }
    let ready = if sync {
        state.docs.open(&path).map(|_| true)
    } else {
        // Already open, or prefetched (hover, quick-open dwell, tab hover): installs without a read.
        state.docs.take_open(&path).map_or(Ok(false), |r| r.map(|_| true))
    };
    let ready_ok = matches!(ready, Ok(true));
    match ready {
        Err(e) => {
            state.status.error(format!("Cannot open {}: {e}", path.display()));
            return;
        }
        Ok(false) => state.docs.prefetch.request_open(&path, state.waker.clone()),
        Ok(true) => {}
    }
    // An empty untitled note is replaced by the next file; one with text is never replaced.
    let active = state.tabs.active_path().map(Path::to_path_buf);
    let take_over = active.as_deref().is_some_and(|a| untitled_is_empty(state, a));
    let can_replace = match active.as_deref().and_then(|p| state.docs.get(p)) {
        Some(d) => !(d.is_dirty() && !state.config.autosave) && !(state.tabs.is_untitled(&d.path) && !d.text.is_empty()),
        None => true,
    };
    let view = state.default_view_for(&path);
    let old_view = state.tabs.active_tab().map(|t| t.view);
    let o = state.tabs.open_ex(&path, new_tab, can_replace, view, take_over);
    if let (false, false, Some(old), Some(v)) = (ready_ok, take_over, o.released.clone(), old_view) {
        // The load can still fail: the tab goes back to this note then (see `poll_open`).
        state.tabs.tabs[o.index].replaced = Some((old, v));
    }
    if let Some(old) = o.released {
        if state.tabs.is_untitled(&old) && !state.tabs.is_open(&old) && take_over {
            drop_untitled(state, &old);
        } else {
            release_doc(state, &old);
        }
    }
    if let Some(l) = line {
        match state.docs.get(&path) {
            Some(doc) => {
                let at = line_start(&doc.text, l);
                state.tabs.tabs[o.index].pending_cursor = Some(at..at);
            }
            None => state.tabs.tabs[o.index].pending_line = Some(l),
        }
    }
}

/// Reads the restored tabs that are not shown yet in the background, one at a time while the worker is idle (so the
/// active tab's own load always goes first). True while there is more to do: the caller repaints soon to continue.
pub fn prefetch_restored_tabs<U>(state: &mut AppState<U>) -> bool {
    prefetch_restored_tabs_inner(&mut state.core_mut())
}

fn prefetch_restored_tabs_inner(state: &mut CoreMut<'_>) -> bool {
    if state.is_active_tab_loading() || !state.docs.prefetch.is_idle() {
        return state.tabs.tabs.iter().any(|t| !t.is_empty() && state.docs.get(&t.path).is_none());
    }
    state.tabs.tabs.iter().filter(|t| !t.is_empty() && state.docs.get(&t.path).is_none()).any(|t| state.docs.prefetch.request_once(&t.path))
}

/// Installs the active tab's document once its background load is done (requesting the load when needed) and applies
/// the tab's pending line. A failed load keeps a new tab with `load_error` set and reports the error; true then, so the caller repaints.
pub fn poll_open<U>(state: &mut AppState<U>) -> bool {
    poll_open_inner(&mut state.core_mut())
}

fn poll_open_inner(state: &mut CoreMut<'_>) -> bool {
    if let Some(l) = state.pending_link.take() {
        if l.since.elapsed() >= LINK_GIVE_UP {
            state.status.error(format!("Index busy: link to \"{}\" not opened, click it again", l.note));
        } else {
            open_link(state, &l.note, l.heading.as_deref(), l.from, l.new_tab);
            if let Some(p) = state.pending_link.as_mut() {
                p.since = l.since;
            }
        }
    }
    let Some(i) = state.tabs.active.filter(|&i| state.tabs.tabs.get(i).is_some_and(|t| !t.is_empty())) else { return false };
    if state.tabs.tabs[i].load_error.is_some() {
        return false;
    }
    let path = state.tabs.tabs[i].path.clone();
    if state.docs.get(&path).is_none() {
        match state.docs.take_open(&path) {
            Some(Ok(_)) => {}
            Some(Err(e)) => {
                state.status.error(format!("Cannot open {}: {e}", path.display()));
                // A history navigation that failed rolls back without dropping the target from `history`.
                if let Some((old_pos, old, view)) = state.tabs.tabs[i].replaced_hist.take() {
                    state.tabs.tabs[i].restore_history_entry(old_pos, old, view);
                    return true;
                }
                // A tab that replaced a note goes back to it (its text loads again like a restored tab).
                if let Some((old, view)) = state.tabs.tabs[i].replaced.take() {
                    let t = &mut state.tabs.tabs[i];
                    t.pop_replaced_history();
                    t.path = old;
                    t.view = view;
                    t.reset_panes();
                    return true;
                }
                // A new tab stays, showing why (Retry / Reveal / Close).
                state.tabs.tabs[i].load_error = Some(e.to_string());
                return true;
            }
            None => {
                state.docs.prefetch.request_open(&path, state.waker.clone());
                return false;
            }
        }
    }
    state.tabs.tabs[i].replaced = None;
    state.tabs.tabs[i].replaced_hist = None;
    if let (Some(l), Some(doc)) = (state.tabs.tabs[i].pending_line.take(), state.docs.get(&path)) {
        let at = line_start(&doc.text, l);
        state.tabs.tabs[i].pending_cursor = Some(at..at);
    } else if state.tabs.tabs[i].pending_cursor.is_some() {
        fix_cursors_parts(state.docs, state.tabs, &path);
    }
    false
}

/// A wiki-link click waiting for the index (see `AppState::pending_link`).
#[derive(Debug, Clone)]
pub struct PendingLink {
    note: String,
    heading: Option<String>,
    from: Option<PathBuf>,
    new_tab: bool,
    since: std::time::Instant,
}

/// A link click waiting this long for the index is dropped with an error instead of keeping the UI repainting.
const LINK_GIVE_UP: std::time::Duration = std::time::Duration::from_secs(3);

/// Resolves the link under one read lock of the index. While a scan holds the write lock the click is kept as
/// `pending_link` and retried by `poll_open` (never blocking the UI); the status bar shows "Opening link…" meanwhile.
fn open_link(state: &mut CoreMut<'_>, note: &str, heading: Option<&str>, from: Option<PathBuf>, new_tab: bool) {
    let mut found: Option<(PathBuf, Option<u32>)> = None;
    if note.trim().is_empty() {
        found = from.clone().map(|f| (f, None));
    }
    if !note.trim().is_empty() || (found.is_some() && heading.is_some()) {
        let idx = state.indexer.index();
        let Some(g) = idx.try_read() else {
            *state.pending_link = Some(PendingLink {
                note: note.to_string(),
                heading: heading.map(str::to_string),
                from,
                new_tab,
                since: std::time::Instant::now(),
            });
            return;
        };
        if !note.trim().is_empty() {
            if let Some(id) = g.resolve(note, from.as_deref()) {
                if let Some(meta) = g.get(id) {
                    found = Some((meta.path.clone(), None));
                }
            } else if let Some(p) = g.resolve_file(note, from.as_deref()) {
                if FileKind::from_path(&p) == FileKind::Other {
                    drop(g);
                    let _ = open::that_detached(&p);
                    return;
                }
                found = Some((p, None));
            }
        }
        if let (Some((path, line)), Some(h)) = (found.as_mut(), heading) {
            if let Some(meta) = g.by_path(path).and_then(|id| g.get(id)) {
                let want = h.trim().to_lowercase();
                *line = meta.headings.iter().find(|x| x.text.trim().to_lowercase() == want).map(|x| x.line);
            }
        }
    }
    match found {
        Some((path, line)) => open_path(state, path, new_tab, line),
        None => {
            let name = note.rsplit(['/', '\\']).next().unwrap_or(note).trim();
            if name.is_empty() {
                return;
            }
            let dir = from.as_deref().map(parent_of).filter(|d| d.is_dir()).or_else(|| state.workspace.roots.first().cloned());
            if dir.is_none() {
                state.status.error("Add a folder to the workspace first");
                return;
            }
            state.dialogs.confirm(
                "Create note",
                format!("Note '{name}' does not exist."),
                "Create",
                vec![Command::CreateLinkedNote { note: note.to_string(), from, new_tab }],
            );
        }
    }
}

fn create_linked_note(state: &mut CoreMut<'_>, note: &str, from: Option<PathBuf>, new_tab: bool) {
    let name = note.rsplit(['/', '\\']).next().unwrap_or(note).trim();
    if name.is_empty() {
        return;
    }
    let dir = from.as_deref().map(parent_of).filter(|d| d.is_dir()).or_else(|| state.workspace.roots.first().cloned());
    let Some(dir) = dir else {
        state.status.error("Add a folder to the workspace first");
        return;
    };
    match ops::create_note(&dir, name, "md", "") {
        Ok(p) => {
            state.tree.invalidate(&dir);
            state.indexer.file_changed(p.clone());
            state.status.info(format!("Created {}", p.display()));
            open_created(state, p, new_tab);
        }
        Err(e) => state.status.error(format!("Cannot create note: {e}")),
    }
}

/// Close tab `i`, asking first when its document has unsaved changes that cannot be autosaved.
fn request_close(state: &mut CoreMut<'_>, i: usize) {
    let Some(path) = state.tabs.tabs.get(i).map(|t| t.path.clone()) else { return };
    if path.as_os_str().is_empty() {
        // The only empty tab stays (there is always a tab); one among others just goes.
        if state.tabs.tabs.len() > 1 {
            do_close(state, i, false);
        }
        return;
    }
    if state.tabs.is_untitled(&path) {
        // Nothing to lose without text; with text the user decides, even when autosave already wrote the file.
        if untitled_is_empty(state, &path) {
            do_close(state, i, true);
        } else if state.docs.get(&path).is_none() {
            if state.tabs.tabs[i].load_error.is_some() {
                do_close(state, i, true);
            } else {
                state.docs.prefetch.request_open(&path, state.waker.clone());
                state.status.info("Still loading note…");
            }
        } else {
            state.dialogs.current = Some(Dialog::Unsaved {
                message: format!("Save \"{}\" before closing?", untitled_title(&path)),
                save: vec![Command::SaveAs { path: path.clone(), after: AfterSave::CloseTab }],
                discard: vec![Command::CloseTabByPath { path, discard: true }],
            });
        }
        return;
    }
    if state.config.autosave {
        autosave_path_inner(state, &path);
    }
    if state.docs.get(&path).is_some_and(|d| d.is_dirty()) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        state.dialogs.current = Some(Dialog::Unsaved {
            message: format!("Save changes to {name}?"),
            save: vec![Command::CloseTabByPath { path: path.clone(), discard: false }],
            discard: vec![Command::CloseTabByPath { path, discard: true }],
        });
        return;
    }
    do_close(state, i, false);
}

/// "Untitled-3" for `.../Untitled-3.md`.
pub fn untitled_title(path: &Path) -> String {
    path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn do_close(state: &mut CoreMut<'_>, i: usize, discard: bool) {
    if let Some(path) = state.tabs.close(i).filter(|p| !p.as_os_str().is_empty()) {
        if state.tabs.is_untitled(&path) {
            // Only blank or discarded untitled notes get here: the file goes with the tab.
            drop_untitled(state, &path);
            return;
        }
        if discard && !state.tabs.is_open(&path) {
            state.docs.remove(&path);
        }
        release_doc(state, &path);
    }
}

/// Name suggested when saving an untitled note: its first line without markup, else "Untitled".
pub fn suggested_name<U>(state: &AppState<U>, path: &Path) -> String {
    suggested_name_parts(&state.docs, path)
}

fn suggested_name_parts(docs: &DocStore, path: &Path) -> String {
    let first = docs.get(path).and_then(|d| d.text.lines().map(str::trim).find(|l| !l.is_empty())).unwrap_or("");
    let cleaned: String = first
        .trim_start_matches(['#', '>', '-', '*', ' '])
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .take(40)
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.').trim().to_string();
    if cleaned.is_empty() { "Untitled".into() } else { cleaned }
}

/// Resolve the name typed in the save prompt (a file name, or a path below the chosen folder) to the target path.
fn save_target(dir: &Path, value: &str) -> Result<PathBuf, String> {
    use std::path::Component;
    let value = value.trim().replace('\\', "/");
    let mut target = dir.to_path_buf();
    let parts: Vec<&str> = value.split('/').collect();
    for (k, part) in parts.iter().enumerate() {
        let part = part.trim();
        if part.is_empty() || part == "." || part == ".." || part.contains('\0') || !matches!(Path::new(part).components().next(), Some(Component::Normal(_))) {
            return Err("Invalid name".into());
        }
        if k + 1 < parts.len() {
            target.push(part);
        } else {
            let p = Path::new(part);
            let ext_ok = p.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "md" | "markdown" | "typ"));
            target.push(if ext_ok { part.to_string() } else { format!("{part}.md") });
        }
    }
    if !target.parent().is_some_and(Path::is_dir) {
        return Err("The folder does not exist".into());
    }
    Ok(target)
}

/// Move the untitled note `from` to `to`, retarget its tab and document, and index it.
fn save_untitled_as(state: &mut CoreMut<'_>, from: PathBuf, to: PathBuf, after: AfterSave) {
    if !state.tabs.is_untitled(&from) {
        return;
    }
    if std::fs::symlink_metadata(&to).is_ok() {
        state.status.error(format!("{} already exists; pick another name", to.display()));
        return;
    }
    let Ok(doc) = state.docs.open(&from) else {
        state.status.error("Cannot read the untitled note");
        return;
    };
    if doc.text.is_empty() {
        // No file to move: make the empty target, never replacing anything.
        let made = std::fs::OpenOptions::new().write(true).create_new(true).open(&to);
        if let Err(e) = made {
            state.status.error(format!("Cannot save {}: {e}", to.display()));
            return;
        }
        let _ = std::fs::remove_file(&from);
    } else {
        if !save_path_inner(state, &from, false) {
            return;
        }
        if let Err(e) = iron_sheets_core::untitled::move_file(&from, &to) {
            state.status.error(format!("Cannot save {}: {e}", to.display()));
            return;
        }
    }
    state.docs.rename(&from, &to);
    state.tabs.rename_path(&from, &to);
    if let Some(d) = state.docs.get_mut(&to) {
        d.mark_saved();
        d.indexed_version = d.version;
        d.disk_mtime = std::fs::metadata(&to).and_then(|m| m.modified()).ok();
        d.save_error = None;
    }
    let dir = parent_of(&to);
    state.tree.invalidate(&dir);
    state.indexer.file_changed(to.clone());
    state.modules.on_save(&to);
    state.tree.reveal(&to);
    *state.reveal_request = Some(to.clone());
    state.status.info(format!("Saved as {}", to.display()));
    match after {
        AfterSave::Nothing => {}
        AfterSave::CloseTab => {
            if let Some(i) = state.tabs.find(&to) {
                do_close(state, i, false);
            }
        }
        AfterSave::Quit => state.commands.push(Command::Quit),
    }
}

/// Untitled notes with text, in tab order (those a quit without "Restore session" would lose).
fn untitled_with_text(state: &CoreMut<'_>) -> Vec<PathBuf> {
    state
        .tabs
        .tabs
        .iter()
        .filter(|t| state.tabs.is_untitled(&t.path) && state.docs.get(&t.path).is_some_and(|d| !d.text.is_empty()))
        .map(|t| t.path.clone())
        .collect()
}

fn switch_workspace(state: &mut CoreMut<'_>, name: &str, force: bool) {
    if name == state.workspace.name {
        return;
    }
    if !force {
        if state.config.autosave {
            save_all_inner(state);
        }
        if !state.docs.dirty_paths().is_empty() {
            state.dialogs.current = Some(Dialog::Unsaved {
                message: "Save changes before switching workspace?".into(),
                save: vec![Command::SaveAll, Command::SwitchWorkspace(name.into())],
                discard: vec![Command::SwitchWorkspaceForce(name.into())],
            });
            return;
        }
    }
    let ws = if name == "Default" {
        state.store.load(name).unwrap_or_else(|| Workspace::new(name))
    } else {
        match state.store.load(name) {
            Some(w) => w,
            None => {
                state.status.error(format!("Workspace \"{name}\" not found"));
                return;
            }
        }
    };
    state.load_workspace(ws);
    state.status.info(format!("Workspace \"{name}\""));
}

fn add_root(state: &mut CoreMut<'_>, path: &Path) {
    let r = state.workspace.add_root(path);
    match r {
        AddRoot::Added => {
            sync_roots(state);
            if let Some(root) = state.workspace.roots.last().cloned() {
                state.tree.set_expanded(&root, true);
            }
            state.flush_session_async();
            state.config.last_workspace = Some(state.workspace.name.clone());
            state.touch_config();
            state.status.info(format!("Added {}", path.display()));
        }
        AddRoot::AlreadyPresent => state.status.info("Folder is already in the workspace"),
        AddRoot::Nested => state.status.error("Folder overlaps with a folder already in the workspace"),
        AddRoot::NotADirectory => state.status.error(format!("{} is not a folder", path.display())),
    }
}

fn sync_roots(state: &mut CoreMut<'_>) {
    let roots = state.workspace.roots.clone();
    state.tree.set_roots(roots.clone());
    state.indexer.set_roots(roots.clone());
    if let Some(w) = state.watcher.as_mut() {
        w.set_roots(&roots);
    }
    state.modules.set_roots(&roots);
}

fn prompt_submit(state: &mut CoreMut<'_>, action: PromptAction, value: String) {
    match action {
        PromptAction::SaveUntitled { path, dir, after } => match save_target(&dir, &value) {
            Ok(to) => save_untitled_as(state, path, to, after),
            Err(e) => state.status.error(format!("Cannot save: {e}")),
        },
        PromptAction::NewNote { dir } => {
            let p = Path::new(&value);
            let (stem, ext) = match p.extension().and_then(|e| e.to_str()) {
                Some(e) if matches!(e.to_ascii_lowercase().as_str(), "md" | "markdown" | "typ") => {
                    (p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(), e.to_string())
                }
                _ => (value.clone(), "md".to_string()),
            };
            let init = if ext == "typ" { "= Title\n\n" } else { "" };
            match ops::create_note(&dir, &stem, &ext, init) {
                Ok(path) => {
                    state.tree.invalidate(&dir);
                    state.indexer.file_changed(path.clone());
                    state.modules.on_save(&path);
                    open_created(state, path.clone(), false);
                    state.tree.reveal(&path);
                    *state.reveal_request = Some(path);
                }
                Err(e) => state.status.error(format!("Cannot create note: {e}")),
            }
        }
        PromptAction::NewFolder { parent } => match ops::create_dir(&parent, &value) {
            Ok(p) => {
                state.tree.set_expanded(&parent, true);
                state.tree.invalidate(&parent);
                state.modules.on_save(&p);
            }
            Err(e) => state.status.error(format!("Cannot create folder: {e}")),
        },
        PromptAction::Rename { path } => {
            let is_dir = state.docs.get(&path).is_none() && !state.tabs.is_open(&path) && path.is_dir();
            let mut name = value;
            if !is_dir {
                if FileKind::is_excalidraw_md(&path) {
                    if !name.to_ascii_lowercase().ends_with(".excalidraw.md") && Path::new(&name).extension().is_none() {
                        name.push_str(".excalidraw.md");
                    }
                } else if Path::new(&name).extension().is_none()
                    && let Some(e) = path.extension()
                {
                    name = format!("{name}.{}", e.to_string_lossy());
                }
            }
            if name.contains(['/', '\\']) || name == "." || name == ".." {
                state.status.error("Invalid name");
                return;
            }
            let to = parent_of(&path).join(&name);
            crate::sidebar::moves::mv_inner(state, path, to, None, false);
        }
        PromptAction::NewWorkspace => {
            state.wait_persist_idle();
            if state.store.list().contains(&value) || value == state.workspace.name {
                state.status.error("A workspace with that name exists");
                return;
            }
            let ws = Workspace::new(value.clone());
            if let Err(e) = state.store.save(&ws) {
                state.status.error(format!("Cannot create workspace: {e}"));
                return;
            }
            state.invalidate_workspace_list();
            state.commands.push(Command::SwitchWorkspace(value));
        }
        PromptAction::RenameWorkspace => {
            let old = state.workspace.name.clone();
            state.flush_session();
            state.flush_tree_cache();
            match state.store.rename(&old, &value) {
                Ok(()) => {
                    state.workspace.name = value.clone();
                    state.config.last_workspace = Some(value);
                    state.touch_config();
                    state.invalidate_workspace_list();
                    *state.rename_restart = true;
                }
                Err(e) => state.status.error(format!("Cannot rename workspace: {e}")),
            }
        }
    }
}

/// Runs one command. Commands that need the frontend (native pickers, PDF export) are not run but handed back, so
/// the frontend can handle them with its own state: `AddRootDialog`, `OpenFileDialog`, `ExportPdf`.
#[must_use = "a returned command needs the frontend"]
pub fn execute<U>(state: &mut AppState<U>, platform: &mut dyn Platform, cmd: Command) -> Option<Command> {
    execute_inner(&mut state.core_mut(), platform, cmd)
}

fn execute_inner(state: &mut CoreMut<'_>, platform: &mut dyn Platform, cmd: Command) -> Option<Command> {
    match cmd {
        c @ (Command::AddRootDialog | Command::OpenFileDialog | Command::ExportPdf | Command::SaveAsDialog { .. }) => Some(c),
        // Without a folder in the workspace the target is picked in the native dialog.
        Command::SaveAs { path, after } if state.workspace.roots.is_empty() && state.dialogs.native_ok && state.tabs.is_untitled(&path) => {
            Some(Command::SaveAsDialog { path, after })
        }
        cmd => {
            execute_core(state, platform, cmd);
            // Closing the last tab leaves one empty tab.
            state.ensure_tab();
            None
        }
    }
}

fn execute_core(state: &mut CoreMut<'_>, platform: &mut dyn Platform, cmd: Command) {
    use Command::*;
    // A link click that is still waiting for the index must not land on a tab the user has moved on from.
    if matches!(cmd, OpenPath { .. } | OpenLink { .. } | ActivateTab(_) | NextTab | PrevTab | CloseTab(_) | CloseActiveTab | NewTab | HistoryBack | HistoryForward) {
        *state.pending_link = None;
    }
    match cmd {
        Module { id, arg } => state.modules.on_command(id, &arg, &mut crate::modules::ModuleCx { waker: state.waker, commands: state.commands }),
        #[cfg(feature = "git")]
        GitHunk { path, mark, stage } => {
            let text = state.docs.get(&path).map(|d| d.text.clone());
            if let (Some(text), Some(g)) = (text, state.modules.get_mut::<crate::git::GitModule>(crate::modules::GIT)) {
                g.hunk_op(&path, mark, text, stage);
            }
        }
        OpenPath { path, new_tab, line } => open_path(state, path, new_tab, line),
        OpenLink { note, heading, from, new_tab } => {
            let new_tab = new_tab != state.config.links_new_tab;
            open_link(state, &note, heading.as_deref(), from, new_tab)
        }
        #[cfg(feature = "calendar")]
        OpenDaily(date) => crate::calendar::open_daily(state, date),
        CreateLinkedNote { note, from, new_tab } => create_linked_note(state, &note, from, new_tab),
        OpenUrl(url) => {
            let l = url.to_ascii_lowercase();
            if l.starts_with("http://") || l.starts_with("https://") || l.starts_with("mailto:") {
                if let Err(e) = open::that_detached(&url) {
                    state.status.error(format!("Cannot open URL: {e}"));
                }
            } else {
                state.status.error("Only http, https and mailto links can be opened");
            }
        }
        NewTab => {
            if state.tabs.active_is_empty() {
                // Already on an empty tab: no second one, just bring it forward.
                *state.focus_empty_page = true;
            } else {
                if state.config.autosave {
                    if let Some(a) = state.active_path() {
                        autosave_async_parts(state.docs, state.waker, &a);
                    }
                }
                state.tabs.new_empty();
                *state.focus_empty_page = true;
            }
        }
        StartUntitled { text } => {
            if state.config.autosave {
                if let Some(a) = state.active_path() {
                    autosave_async_parts(state.docs, state.waker, &a);
                }
            }
            state.start_untitled(&text);
        }
        CloseTab(i) => request_close(state, i),
        CloseActiveTab => {
            if let Some(i) = state.tabs.active {
                request_close(state, i);
            }
        }
        CloseOtherTabs(keep) => close_tabs(state, |j| j != keep),
        CloseTabsToRight(i) => close_tabs(state, |j| j > i),
        CloseAllTabs => close_tabs(state, |_| true),
        ActivateTab(i) => {
            if i < state.tabs.tabs.len() && state.tabs.active != Some(i) {
                if state.config.autosave {
                    if let Some(a) = state.active_path() {
                        autosave_async_parts(state.docs, state.waker, &a);
                    }
                }
                state.tabs.active = Some(i);
            }
        }
        NextTab | PrevTab => {
            let n = state.tabs.tabs.len();
            if n > 1 {
                let a = state.tabs.active.unwrap_or(0);
                let next = if matches!(cmd, NextTab) { (a + 1) % n } else { (a + n - 1) % n };
                execute_core(state, platform, ActivateTab(next));
            }
        }
        _ => execute2(state, platform, cmd),
    }
}

fn execute2(state: &mut CoreMut<'_>, platform: &mut dyn Platform, cmd: Command) {
    use Command::*;
    match cmd {
        ReopenClosedTab => {
            if let Some(p) = state.tabs.closed.pop() {
                open_path(state, p, true, None);
            }
        }
        HistoryBack | HistoryForward => {
            let back = matches!(cmd, HistoryBack);
            let Some((old_pos, old_view, target)) = state.tabs.active_tab().and_then(|t| {
                let target = if back { t.hist_pos.checked_sub(1).and_then(|i| t.history.get(i)) } else { t.history.get(t.hist_pos + 1) };
                target.cloned().map(|p| (t.hist_pos, t.view, p))
            }) else {
                return;
            };
            if state.config.autosave {
                if let Some(a) = state.active_path() {
                    autosave_async_parts(state.docs, state.waker, &a);
                }
            }
            let ready_ok = match state.docs.take_open(&target) {
                Some(Ok(_)) => true,
                Some(Err(e)) => {
                    state.status.error(format!("Cannot open {}: {e}", target.display()));
                    return;
                }
                None => {
                    state.docs.prefetch.request_open(&target, state.waker.clone());
                    false
                }
            };
            if let Some((idx, old, _)) = state.tabs.go_history(back) {
                if ready_ok {
                    fix_cursors_parts(state.docs, state.tabs, &target);
                } else {
                    state.tabs.tabs[idx].replaced_hist = Some((old_pos, old.clone(), old_view));
                }
                release_doc(state, &old);
            }
        }
        Save => {
            if let Some(p) = state.active_path() {
                if state.tabs.is_untitled(&p) {
                    // An untitled note is saved by giving it a name and a folder.
                    state.commands.push(SaveAs { path: p, after: AfterSave::Nothing });
                } else if state.docs.get(&p).is_some_and(|d| d.read_only) {
                    state.status.error("This file was opened read-only (invalid UTF-8)");
                } else {
                    if let Some(d) = state.docs.get_mut(&p) {
                        d.mark_unsaved_if_clean_when_deleted();
                    }
                    save_path_async_parts(state.docs, state.waker, &p, true);
                }
            }
        }
        SaveAs { path, after } => {
            if !state.tabs.is_untitled(&path) {
                return;
            }
            if let Some(i) = state.tabs.find(&path) {
                execute_core(state, platform, ActivateTab(i));
            }
            let name = suggested_name_parts(state.docs, &path);
            let dirs: Vec<(String, PathBuf)> = if state.workspace.roots.is_empty() {
                // No folder in the workspace and no native dialog: start from the working directory.
                vec![("Current folder".into(), std::env::current_dir().unwrap_or_default())]
            } else {
                (0..state.workspace.roots.len()).map(|i| (state.workspace.root_label(i), state.workspace.roots[i].clone())).collect()
            };
            let dir = dirs.first().map(|d| d.1.clone()).unwrap_or_default();
            state.dialogs.prompt_in_folders("Save note as", "Name (add .typ for a Typst note)", &name, PromptAction::SaveUntitled { path, dir, after }, dirs);
        }
        SaveAsTo { from, to, after } => save_untitled_as(state, from, to, after),
        SaveAll => save_all_force_inner(state),
        SaveDoc(p) => {
            save_path_inner(state, &p, false);
        }
        NewNote { dir } => match dir.or_else(|| state.default_note_dir()) {
            Some(dir) => state.dialogs.prompt("New note", "Name (add .typ for a Typst note)", "Untitled", PromptAction::NewNote { dir }),
            None => state.status.error("Add a folder to the workspace first"),
        },
        NewFolder { parent } => match parent.or_else(|| state.default_note_dir()) {
            Some(parent) => state.dialogs.prompt("New folder", "Folder name", "New folder", PromptAction::NewFolder { parent }),
            None => state.status.error("Add a folder to the workspace first"),
        },
        // A dialog is already open (F2 again): keep what was typed.
        Rename { .. } if state.dialogs.current.is_some() => {}
        Rename { path } => {
            let is_dir = state.docs.get(&path).is_none() && !state.tabs.is_open(&path) && path.is_dir();
            let v = if is_dir {
                path.file_name().map(|s| s.to_string_lossy().into_owned())
            } else {
                FileKind::note_stem(&path).map(|s| s.into_owned())
            };
            let v = v.unwrap_or_default();
            state.dialogs.prompt("Rename", "New name", &v, PromptAction::Rename { path });
        }
        Move { from, to, links, dnd } => crate::sidebar::moves::mv_inner(state, from, to, links, dnd),
        Trash { path } => {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if state.config.confirm_trash {
                let msg = format!("Move \"{name}\" to the trash?");
                state.dialogs.confirm_dont_ask("Move to trash", msg, "Move to trash", vec![TrashConfirmed(path)], vec![SetConfirmTrash(false)]);
            } else {
                state.commands.push(TrashConfirmed(path));
            }
        }
        SetConfirmTrash(on) => {
            state.config.confirm_trash = on;
            state.touch_config();
        }
        TrashConfirmed(path) => {
            state.docs.wait_saved_under(&path);
            for j in (0..state.tabs.tabs.len()).rev() {
                if state.tabs.tabs[j].path.starts_with(&path) {
                    let p = state.tabs.tabs[j].path.clone();
                    do_close(state, j, true);
                    state.docs.remove(&p);
                }
            }
            *state.pending_trash += 1;
            let tx = state.trash_tx.clone();
            let waker = state.waker.clone();
            let p = path.clone();
            let spawned = std::thread::Builder::new().name("iron-sheets-trash".into()).spawn(move || {
                let res = ops::trash(&p);
                let _ = tx.send((p, res));
                waker.wake();
            });
            if spawned.is_err() {
                *state.pending_trash = state.pending_trash.saturating_sub(1);
                let res = ops::trash(&path);
                apply_trash_result(state, path, res);
            }
        }
        RetryOpen(i) => {
            if let Some(t) = state.tabs.tabs.get_mut(i) {
                t.load_error = None;
            }
        }
        RevealInFileManager(p) => {
            let dir = if p.is_dir() { p } else { parent_of(&p) };
            if let Err(e) = open::that_detached(dir) {
                state.status.error(format!("Cannot open file manager: {e}"));
            }
        }
        OpenExternal(p) => {
            if let Err(e) = open::that_detached(&p) {
                state.status.error(format!("Cannot open {}: {e}", p.display()));
            }
        }
        CopyPath(p) => {
            platform.clipboard_set(&p.display().to_string());
            state.status.info("Path copied");
        }
        ToggleWrap => {
            state.config.editor_wrap = !state.config.editor_wrap;
            state.touch_config();
        }
        ToggleSidebar => {
            state.config.sidebar_visible = !state.config.sidebar_visible;
            state.touch_config();
        }
        FocusSidebar => {
            state.config.sidebar_visible = true;
            *state.focus_sidebar = true;
            state.touch_config();
        }
        SetSidebarView(v) => {
            *state.sidebar_view = v;
            state.config.sidebar_visible = true;
            *state.focus_sidebar = true;
            state.touch_config();
        }
        SetViewMode(m) => {
            if let Some(t) = state.tabs.active_tab_mut() {
                t.view = m;
            }
        }
        CycleViewMode => {
            if let Some(t) = state.tabs.active_tab_mut() {
                t.view = match t.view {
                    ViewMode::Source => ViewMode::Split,
                    ViewMode::Split => ViewMode::Preview,
                    ViewMode::Preview => ViewMode::Source,
                };
            }
        }
        ToggleSyncScroll => {
            state.config.sync_scroll = !state.config.sync_scroll;
            for t in &mut state.tabs.tabs {
                t.resync = state.config.sync_scroll;
            }
            state.touch_config();
        }
        QuickOpen => state.palette.begin(PaletteMode::Files),
        CommandPalette => state.palette.begin(PaletteMode::Commands),
        GlobalSearch { query } => {
            *state.sidebar_view = SidebarView::Search;
            state.config.sidebar_visible = true;
            *state.search_request = Some(query.unwrap_or_default());
        }
        FindInNote => *state.find_request = true,
        AddRootByPath => {
            let start = state.default_note_dir().unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            state.dialogs.folder_browser(&start);
        }
        AddRoot(p) => add_root(state, &p),
        RemoveRoot(i) => {
            let Some(path) = state.workspace.roots.get(i).cloned() else { return };
            let expanded = state.tree.expanded().into_iter().filter(|p| p.starts_with(&path)).collect();
            let name = state.workspace.root_label(i);
            state.workspace.remove_root(i);
            *state.removed_root = Some(RemovedRoot { workspace: state.workspace.name.clone(), idx: i, path, expanded });
            sync_roots(state);
            state.flush_session_async();
            state.notify(format!("Removed {name} from workspace"), Some(("Undo", UndoRemoveRoot)), 8);
        }
        UndoRemoveRoot => {
            *state.notice = None;
            let Some(r) = state.removed_root.take().filter(|r| r.workspace == state.workspace.name) else {
                return state.status.info("Nothing to undo");
            };
            match state.workspace.insert_root(r.idx, &r.path) {
                iron_sheets_core::workspace::AddRoot::Added => {
                    sync_roots(state);
                    // The root's own entry is in `expanded` only if it was open.
                    state.tree.set_expanded(&r.path, r.expanded.contains(&r.path));
                    for p in &r.expanded {
                        state.tree.set_expanded(p, true);
                    }
                    state.flush_session_async();
                }
                iron_sheets_core::workspace::AddRoot::AlreadyPresent => state.status.info("The folder is already in the workspace"),
                iron_sheets_core::workspace::AddRoot::Nested => state.status.error("Folder overlaps with a folder already in the workspace"),
                iron_sheets_core::workspace::AddRoot::NotADirectory => state.status.error(format!("{} is no longer a folder", r.path.display())),
            }
        }
        SetTreeSort(sort) => {
            state.workspace.tree_sort = sort;
            state.tree.set_sort(sort);
            state.flush_session_async();
        }
        CollapseTree => state.tree.collapse_all(),
        SwitchWorkspace(n) => switch_workspace(state, &n, false),
        SwitchWorkspaceForce(n) => switch_workspace(state, &n, true),
        NewWorkspace => state.dialogs.prompt("New workspace", "Workspace name", "", PromptAction::NewWorkspace),
        RenameWorkspace => {
            let n = state.workspace.name.clone();
            state.dialogs.prompt("Rename workspace", "New name", &n, PromptAction::RenameWorkspace);
        }
        DeleteWorkspace => {
            let n = state.workspace.name.clone();
            state.dialogs.confirm(
                "Delete workspace",
                format!("Delete workspace \"{n}\"? Your files are not touched."),
                "Delete",
                vec![DeleteWorkspaceConfirmed(n)],
            );
        }
        DeleteWorkspaceConfirmed(n) => {
            if n == state.workspace.name {
                if state.config.autosave {
                    save_all_inner(state);
                }
                if !state.docs.dirty_paths().is_empty() {
                    state.dialogs.current = Some(Dialog::Unsaved {
                        message: "Save changes before deleting this workspace?".into(),
                        save: vec![SaveAll, DeleteWorkspaceConfirmed(n.clone())],
                        discard: vec![DeleteWorkspaceForce(n)],
                    });
                    return;
                }
            }
            state.commands.push(DeleteWorkspaceForce(n));
        }
        DeleteWorkspaceForce(n) => {
            state.cancel_pending_workspace(&n);
            if let Err(e) = state.store.delete(&n) {
                state.status.error(format!("Cannot delete workspace: {e}"));
                return;
            }
            state.invalidate_workspace_list();
            if n == state.workspace.name {
                let next = state.store.list().into_iter().next().and_then(|x| state.store.load(&x)).unwrap_or_else(|| Workspace::new("Default"));
                state.workspace.name = String::new(); // force a real switch
                state.load_workspace(next);
            }
        }
        Rescan => {
            state.indexer.rescan();
            if let Some(w) = &*state.watcher {
                w.resync();
            }
            state.tree.invalidate_all();
            state.modules.on_fs_event(&[iron_sheets_core::watch::FsEvent::Rescan]);
            state.status.info("Rescanning...");
        }
        InsertText(text) => {
            let Some(tab) = state.tabs.active_tab() else { return };
            let (path, sel) = (tab.path.clone(), tab.cursor.clone());
            let Some(doc) = state.docs.get(&path) else { return };
            let sel = sel.unwrap_or(doc.text.len()..doc.text.len());
            let end = sel.start + text.len();
            state.commands.insert(0, ApplyEdit { path, range: sel, insert: text, select: Some(end..end) });
        }
        ApplyEdit { path, range, insert, select } => {
            let caret = select.unwrap_or_else(|| (range.start + insert.len())..(range.start + insert.len()));
            let tx = Transaction { edits: vec![Edit { range, insert }], select: Some(Selections::single(caret)), origin: Origin::Command };
            apply_transaction(state, &path, tx);
        }
        Apply(path, tx) => apply_transaction(state, &path, tx),
        ScrollToLine(l) => {
            if let Some(t) = state.tabs.active {
                let path = state.tabs.tabs[t].path.clone();
                if let Some(d) = state.docs.get(&path) {
                    let at = line_start(&d.text, l);
                    state.tabs.tabs[t].pending_cursor = Some(at..at);
                }
            }
        }
        SetTheme(p) => {
            state.config.theme = p;
            state.config.theme_file.clear();
            state.touch_config();
            crate::settings_model::apply_inner(state, false);
        }
        // Appearance changes would not show: these only touch values that professional mode renders as defaults.
        ZoomReset | FontZoom(_) | SetPreviewFont(_) if state.config.professional => state.status.info("Professional mode is on"),
        ZoomIn | ZoomOut | ZoomReset => {
            let reset = matches!(cmd, ZoomReset);
            let z = match cmd {
                ZoomIn => state.config.ui_zoom + 0.1,
                ZoomOut => state.config.ui_zoom - 0.1,
                _ => 1.0,
            };
            state.config.ui_zoom = ((z * 10.0).round() / 10.0).clamp(0.5, 3.0);
            if reset {
                // Reset means "as after a fresh install": zoom 100 % and the default editor and preview sizes.
                let d = iron_sheets_core::config::AppConfig::default();
                state.config.editor_font_size = d.editor_font_size;
                state.config.preview_font_size = d.preview_font_size;
            }
            state.status.info(format!("Zoom {:.0} %", state.config.ui_zoom * 100.0));
            state.touch_config();
        }
        FontZoom(steps) => {
            let c = &mut state.config;
            let ed = (c.editor_font_size + steps as f32).round().clamp(FONT_MIN, FONT_MAX);
            let pv = (c.preview_font_size + steps as f32).round().clamp(FONT_MIN, FONT_MAX);
            // At the limit the wheel keeps sending steps: no config write, message or repaint for nothing.
            if ed != c.editor_font_size || pv != c.preview_font_size {
                (c.editor_font_size, c.preview_font_size) = (ed, pv);
                state.status.info(format!("Font {ed:.0} pt"));
                state.touch_config();
            }
        }
        SetPreviewFont(id) => {
            if state.config.preview_font != id {
                state.config.preview_font = id;
                state.touch_config();
                crate::settings_model::apply_inner(state, false);
            }
        }
        TogglePerfOverlay => state.perf.overlay = !state.perf.overlay,
        ToggleProfessional => {
            state.config.professional = !state.config.professional;
            state.touch_config();
            crate::settings_model::apply_inner(state, false);
        }
        OpenThemesFolder => {
            let dir = iron_sheets_core::themes::themes_dir(state.dirs);
            if let Err(e) = std::fs::create_dir_all(&dir).map_err(|e| e.to_string()).and_then(|_| open::that_detached(&dir).map_err(|e| e.to_string())) {
                state.status.error(format!("Cannot open themes folder: {e}"));
            }
        }
        OpenSettings => {
            state.settings.open = true;
        }
        About => state.status.info(format!("iron sheets {}", env!("CARGO_PKG_VERSION"))),
        PromptSubmit(a, v) => prompt_submit(state, a, v),
        CloseTabByPath { path, discard } => {
            if !discard {
                if !save_path_inner(state, &path, false) {
                    return;
                }
            }
            if let Some(i) = state.tabs.find(&path) {
                do_close(state, i, discard);
            }
        }
        ReloadDoc(p) => {
            if let Err(e) = state.docs.reload(&p) {
                state.status.error(format!("Cannot reload: {e}"));
            } else {
                fix_cursors_parts(state.docs, state.tabs, &p);
                state.indexer.file_changed(p);
            }
        }
        SaveAgain(p) => match state.docs.get_mut(&p) {
            Some(d) if d.read_only || d.kind == iron_sheets_core::FileKind::Image => state.status.error("This file cannot be written back"),
            Some(d) => {
                // `deleted` stays until the write succeeds (see `DocStore::apply_done`).
                d.mark_unsaved_if_clean();
                state.docs.save_async(&p, true, true, state.waker);
            }
            None => {}
        },
        KeepMine(p) => {
            if let Some(d) = state.docs.get_mut(&p) {
                d.external_change = false;
                // Remember the disk version so the same change does not prompt again.
                d.disk_mtime = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
            }
        }
        Quit => {
            if state.config.autosave {
                save_all_inner(state);
            }
            // With "Restore session" untitled notes are kept (hot exit); without it, those with text are the user's call.
            let keep_untitled = state.config.restore_session;
            let at_risk = if keep_untitled { Vec::new() } else { untitled_with_text(state) };
            let unsaved = state.docs.dirty_paths().iter().any(|p| !state.tabs.is_untitled(p));
            if !unsaved && at_risk.is_empty() {
                state.commands.push(QuitNow);
            } else {
                let mut save = vec![SaveAll];
                // Saving an untitled note needs a name: one at a time, then the quit continues.
                save.push(match at_risk.first() {
                    Some(p) => SaveAs { path: p.clone(), after: AfterSave::Quit },
                    None => Quit,
                });
                let message = if at_risk.is_empty() {
                    "You have unsaved changes. Save before quitting?"
                } else {
                    "Untitled notes are not kept because \"Restore session\" is off. Save before quitting?"
                };
                state.dialogs.current = Some(Dialog::Unsaved { message: message.into(), save, discard: vec![QuitNow] });
            }
        }
        QuitNow => {
            let untitled: Vec<usize> = (0..state.tabs.tabs.len()).filter(|&i| state.tabs.is_untitled(&state.tabs.tabs[i].path)).collect();
            if state.config.restore_session {
                // Hot exit: the session lists them, their files hold the text.
                for i in untitled {
                    let p = state.tabs.tabs[i].path.clone();
                    save_path_inner(state, &p, false);
                }
            } else {
                for i in untitled.into_iter().rev() {
                    do_close(state, i, true);
                }
            }
            *state.quit_ok = true;
            platform.close_window();
        }
        _ => {}
    }
}

fn apply_trash_result(state: &mut CoreMut<'_>, path: PathBuf, res: std::io::Result<()>) {
    match res {
        Ok(()) => {
            state.tree.invalidate(&parent_of(&path));
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            state.indexer.file_changed(path.clone());
            state.modules.on_save(&path);
            // An Undo offer still on screen wins over this plain message.
            if state.notice.as_ref().is_none_or(|n| n.action.is_none()) {
                state.notify(format!("Moved {name} to trash"), None, 4);
            }
        }
        Err(e) => state.status.error(format!("Cannot move to trash: {e}")),
    }
}

/// Polls completed background trash operations and updates tree, index, and status/toast.
pub fn poll_trash<U>(state: &mut AppState<U>) {
    poll_trash_inner(&mut state.core_mut());
}

fn poll_trash_inner(state: &mut CoreMut<'_>) {
    while let Ok((path, res)) = state.trash_rx.try_recv() {
        *state.pending_trash = state.pending_trash.saturating_sub(1);
        apply_trash_result(state, path, res);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_pdf_command_exists_only_with_the_pdf_feature() {
        assert_eq!(all_commands().iter().any(|c| c.id == "file.export_pdf"), cfg!(feature = "pdf"));
    }

    #[test]
    fn save_target_names() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("sub")).unwrap();
        let d = t.path();
        assert_eq!(save_target(d, "idea").unwrap(), d.join("idea.md"));
        assert_eq!(save_target(d, "idea.typ").unwrap(), d.join("idea.typ"));
        assert_eq!(save_target(d, "v1.2").unwrap(), d.join("v1.2.md"));
        assert_eq!(save_target(d, "sub/idea").unwrap(), d.join("sub").join("idea.md"));
        assert_eq!(save_target(d, "sub\\idea").unwrap(), d.join("sub").join("idea.md"));
        assert!(save_target(d, "../idea").is_err());
        assert!(save_target(d, "/idea").is_err());
        assert!(save_target(d, "missing/idea").is_err());
        assert!(save_target(d, "sub/").is_err());
    }

    #[test]
    fn a_link_waiting_too_long_for_the_index_is_dropped_with_an_error() {
        let (_d, mut state) = crate::test_support::state_with(&[("a.md", "x")]);
        let idx = state.indexer.index();
        let _busy = idx.write();
        open_link(&mut state.core_mut(), "a", None, None, true);
        let p = state.pending_link.as_mut().expect("pending while the index is busy");
        p.since -= LINK_GIVE_UP;
        poll_open(&mut state);
        assert!(state.pending_link.is_none(), "no endless retry");
        assert!(state.status.msgs.iter().any(|m| m.error && m.text.contains("Index busy")));
    }

    #[test]
    fn suggested_names_come_from_the_first_line() {
        let (_d, mut state) = crate::test_support::state_with(&[]);
        let p = iron_sheets_core::untitled::path_for(&state.tabs.untitled_dir, 1);
        state.docs.open(&p).unwrap().apply_edit(0..0, "\n## My: plan / v2?\nbody");
        assert_eq!(suggested_name(&state, &p), "My plan  v2");
        state.docs.get_mut(&p).unwrap().apply_edit(0..0, "");
        let q = iron_sheets_core::untitled::path_for(&state.tabs.untitled_dir, 2);
        state.docs.open(&q).unwrap();
        assert_eq!(suggested_name(&state, &q), "Untitled");
    }

    fn four_tabs() -> (tempfile::TempDir, crate::state::AppState) {
        let (d, mut state) = crate::test_support::state_with(&[("a.md", "a"), ("b.md", "b"), ("c.md", "c"), ("d.md", "d")]);
        state.config.autosave = false;
        for f in ["a.md", "b.md", "c.md", "d.md"] {
            crate::test_support::activate_tab(&mut state, f);
        }
        (d, state)
    }

    fn names(state: &crate::state::AppState) -> Vec<String> {
        state.tabs.tabs.iter().map(|t| t.path.file_name().unwrap().to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn close_to_the_right_keeps_dirty_tabs_and_a_sane_active_tab() {
        let (_d, mut state) = four_tabs();
        let c = state.vault_path("c.md");
        state.docs.open(&c).unwrap().apply_edit(0..0, "x");
        state.tabs.active = Some(3);
        let _ = execute(&mut state, &mut TestPlatform, Command::CloseTabsToRight(1));
        assert_eq!(names(&state), ["a.md", "b.md", "c.md"], "d closed, dirty c kept");
        assert!(state.tabs.active.is_some_and(|i| i < 3));
        assert!(state.status.msgs.iter().any(|m| m.text.contains("1 tab(s) with unsaved")));
        // Nothing to the right of the last tab.
        let _ = execute(&mut state, &mut TestPlatform, Command::CloseTabsToRight(2));
        assert_eq!(names(&state), ["a.md", "b.md", "c.md"]);
    }

    #[test]
    fn close_all_keeps_dirty_and_untitled_with_text_but_drops_blank_ones() {
        let (_d, mut state) = four_tabs();
        let b = state.vault_path("b.md");
        state.docs.open(&b).unwrap().apply_edit(0..0, "x");
        let _ = execute(&mut state, &mut TestPlatform, Command::StartUntitled { text: "keep me".into() });
        let _ = execute(&mut state, &mut TestPlatform, Command::StartUntitled { text: String::new() });
        assert_eq!(state.tabs.tabs.len(), 6);
        let _ = execute(&mut state, &mut TestPlatform, Command::CloseAllTabs);
        let n = names(&state);
        assert_eq!(n.len(), 2, "{n:?}");
        assert!(n.contains(&"b.md".to_string()) && n.iter().any(|x| x.starts_with("Untitled")), "{n:?}");
        assert!(state.tabs.active.is_some_and(|i| i < 2));
    }

    struct TestPlatform;
    impl Platform for TestPlatform {
        fn waker(&self) -> std::sync::Arc<dyn crate::platform::Waker> {
            std::sync::Arc::new(crate::test_support::NoopWaker)
        }
        fn clipboard_set(&mut self, _: &str) {}
        fn close_window(&mut self) {}
        fn set_title(&mut self, _: &str) {}
    }

    fn doc_text(state: &crate::state::AppState, p: &Path) -> String {
        state.docs.get(p).map(|d| d.text.clone()).unwrap_or_default()
    }

    #[test]
    fn apply_commands_respect_read_only_and_bad_ranges_and_set_the_caret() {
        let (_d, mut state) = crate::test_support::state_with(&[("a.md", "héllo")]);
        crate::test_support::activate_tab(&mut state, "a.md");
        let p = state.vault_path("a.md");
        state.docs.open(&p).unwrap();
        let edit = |range: Range<usize>, insert: &str| Command::ApplyEdit { path: p.clone(), range, insert: insert.into(), select: None };
        let run = |state: &mut crate::state::AppState, c: Command| {
            let _ = execute(state, &mut TestPlatform, c);
        };
        // Invalid range (inside the é) and out of bounds: nothing changes.
        run(&mut state, edit(2..2, "x"));
        run(&mut state, edit(0..99, "x"));
        assert_eq!(doc_text(&state, &p), "héllo");
        assert!(state.tabs.tabs[0].pending_cursor.is_none());
        // Success sets the caret behind the inserted text.
        run(&mut state, edit(0..0, "ab"));
        assert_eq!(doc_text(&state, &p), "abhéllo");
        assert_eq!(state.tabs.tabs[0].pending_cursor, Some(2..2));
        // Transactions go through `Apply` too, with their own selection.
        let mut tx = Transaction::one(0..2, "", Origin::Command);
        tx.select = Some(Selections::single(1..3));
        state.tabs.tabs[0].pending_cursor = None;
        run(&mut state, Command::Apply(p.clone(), tx));
        assert_eq!((doc_text(&state, &p).as_str(), state.tabs.tabs[0].pending_cursor.clone()), ("héllo", Some(1..3)));
        // Read-only: no change, no caret change, undo refused too.
        state.tabs.tabs[0].pending_cursor = None;
        state.docs.get_mut(&p).unwrap().read_only = true;
        run(&mut state, edit(0..0, "zz"));
        assert_eq!(doc_text(&state, &p), "héllo");
        assert!(state.tabs.tabs[0].pending_cursor.is_none());
        assert!(state.docs.get_mut(&p).unwrap().undo().is_none());
    }

    #[test]
    fn trash_asks_unless_disabled() {
        let (_d, mut state) = crate::test_support::state_with(&[("a.md", "hello")]);
        let p = state.vault_path("missing.md");
        let _ = execute(&mut state, &mut TestPlatform, Command::Trash { path: p.clone() });
        assert!(matches!(state.dialogs.current, Some(crate::dialogs::Dialog::Confirm { dont_ask: Some((false, _)), .. })));
        assert!(!state.has_pending_trash());
        state.dialogs.current = None;
        let _ = execute(&mut state, &mut TestPlatform, Command::SetConfirmTrash(false));
        assert!(!state.config.confirm_trash && state.config_dirty_at.is_some());
        let _ = execute(&mut state, &mut TestPlatform, Command::Trash { path: p });
        assert!(state.dialogs.current.is_none());
        let queued = state.commands.drain(..).any(|c| matches!(c, Command::TrashConfirmed(_)));
        assert!(queued, "goes straight to the trash");
    }

    #[test]
    fn trash_confirmed_runs_in_background_and_polls_completion() {
        let (_d, mut state) = crate::test_support::state_with(&[("a.md", "hello")]);
        let missing = state.vault_path("missing.md");
        state.tabs.push_tab(missing.clone(), ViewMode::Source, true);
        let _ = execute(&mut state, &mut TestPlatform, Command::TrashConfirmed(missing.clone()));
        assert!(!state.tabs.is_open(&missing), "matching tab closes immediately");
        assert!(state.has_pending_trash(), "trash runs on background thread");
        let t0 = std::time::Instant::now();
        while state.has_pending_trash() {
            assert!(t0.elapsed() < std::time::Duration::from_secs(5), "trash worker timeout");
            poll_trash(&mut state);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(state.status.current().is_some_and(|m| m.error && m.text.contains("Cannot move to trash")));
    }

    fn wait_saves(state: &mut crate::state::AppState, p: &Path) {
        let t0 = std::time::Instant::now();
        while state.docs.is_saving(p) {
            assert!(t0.elapsed() < std::time::Duration::from_secs(5), "save worker timeout");
            poll_saves(state);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        poll_saves(state);
    }

    fn dirty_doc(files: &[(&str, &str)], rel: &str, insert: &str) -> (tempfile::TempDir, crate::state::AppState, PathBuf) {
        let (d, mut state) = crate::test_support::state_with(files);
        let p = state.vault_path(rel);
        state.docs.open(&p).unwrap().apply_edit(0..0, insert);
        (d, state, p)
    }

    #[test]
    fn autosave_runs_on_the_worker_and_marks_clean_on_completion() {
        let (_d, mut state, p) = dirty_doc(&[("a.md", "hello")], "a.md", "X");
        autosave_async(&mut state, &p);
        assert!(state.docs.is_saving(&p), "returns with the write in flight");
        assert!(state.docs.get(&p).unwrap().is_dirty());
        wait_saves(&mut state, &p);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "Xhello");
        let d = state.docs.get(&p).unwrap();
        assert!(!d.is_dirty() && d.save_error.is_none());
        assert!(!state.docs.changed_on_disk(&p), "our own write is not an external change");
    }

    #[test]
    fn explicit_save_announces_when_done() {
        let (_d, mut state, p) = dirty_doc(&[("a.md", "hello")], "a.md", "X");
        save_path_async(&mut state, &p, true);
        wait_saves(&mut state, &p);
        assert!(state.status.current().is_some_and(|m| !m.error && m.text == "Saved"));
    }

    #[test]
    fn watcher_event_during_a_write_is_not_an_external_change() {
        let (_d, mut state, p) = dirty_doc(&[("a.md", "hello")], "a.md", "X");
        autosave_async(&mut state, &p);
        // The old mtime is still in the doc until the result arrives.
        assert!(!state.docs.changed_on_disk(&p));
        wait_saves(&mut state, &p);
        assert!(!state.docs.changed_on_disk(&p));
    }

    #[test]
    fn edit_during_a_write_stays_dirty_and_the_next_save_writes_it() {
        let (_d, mut state, p) = dirty_doc(&[("a.md", "hello")], "a.md", "X");
        autosave_async(&mut state, &p);
        state.docs.get_mut(&p).unwrap().apply_edit(0..0, "Y");
        autosave_async(&mut state, &p); // coalesced behind the write in flight
        wait_saves(&mut state, &p);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "YXhello");
        assert!(!state.docs.get(&p).unwrap().is_dirty());
    }

    #[test]
    fn edit_during_a_write_without_a_follow_up_request_stays_dirty() {
        let (_d, mut state, p) = dirty_doc(&[("a.md", "hello")], "a.md", "X");
        autosave_async(&mut state, &p);
        state.docs.get_mut(&p).unwrap().apply_edit(0..0, "Y");
        wait_saves(&mut state, &p);
        assert!(state.docs.get(&p).unwrap().is_dirty());
        // Editing back to what is on disk counts as clean.
        state.docs.get_mut(&p).unwrap().apply_edit(0..1, "");
        assert!(!state.docs.get(&p).unwrap().is_dirty());
    }

    #[test]
    fn sync_save_waits_for_the_write_in_flight() {
        let (_d, mut state, p) = dirty_doc(&[("a.md", "hello")], "a.md", "X");
        autosave_async(&mut state, &p);
        state.docs.get_mut(&p).unwrap().apply_edit(0..0, "Y");
        assert!(save_path(&mut state, &p, false));
        assert!(!state.docs.is_saving(&p));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "YXhello");
        assert!(!state.docs.get(&p).unwrap().is_dirty());
    }

    #[test]
    fn write_errors_surface_as_before() {
        let (_d, mut state, p) = dirty_doc(&[("sub/a.md", "hello")], "sub/a.md", "X");
        std::fs::remove_dir_all(p.parent().unwrap()).unwrap();
        autosave_async(&mut state, &p);
        wait_saves(&mut state, &p);
        let d = state.docs.get(&p).unwrap();
        assert!(d.save_error.is_some() && d.last_save_fail.is_some() && d.is_dirty());
        assert!(state.status.current().is_some_and(|m| m.error && m.text.contains("Cannot save")));
    }

    #[test]
    fn quit_writes_before_it_returns() {
        let (_d, mut state, p) = dirty_doc(&[("a.md", "hello")], "a.md", "X");
        state.config.autosave = true;
        let _ = execute(&mut state, &mut TestPlatform, Command::Quit);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "Xhello");
        assert!(!state.docs.get(&p).unwrap().is_dirty());
    }

    /// UI-thread time of an autosave of a 1 MB note: before this change `autosave_path` (write and fsync inline).
    #[test]
    fn autosave_ui_thread_time_for_a_big_note() {
        let text = "lorem ipsum dolor sit amet\n".repeat(40_000);
        let (_d, mut state, p) = dirty_doc(&[("big.md", &text)], "big.md", "X");
        autosave_async(&mut state, &p); // starts the worker thread
        wait_saves(&mut state, &p);
        state.docs.get_mut(&p).unwrap().apply_edit(0..0, "Y");
        let t = std::time::Instant::now();
        autosave_async(&mut state, &p);
        let async_us = t.elapsed().as_micros();
        wait_saves(&mut state, &p);
        state.docs.get_mut(&p).unwrap().apply_edit(0..0, "Z");
        let t = std::time::Instant::now();
        assert!(autosave_path(&mut state, &p));
        let sync_us = t.elapsed().as_micros();
        eprintln!("autosave 1 MB: UI thread async {async_us} us, sync {sync_us} us");
    }

    #[test]
    fn undo_to_the_old_text_during_a_write_leaves_the_note_dirty() {
        let (_d, mut state, p) = dirty_doc(&[("a.md", "hello")], "a.md", "X");
        autosave_async(&mut state, &p);
        state.docs.get_mut(&p).unwrap().undo().expect("undo");
        assert_eq!(state.docs.get(&p).unwrap().text, "hello");
        wait_saves(&mut state, &p);
        assert!(state.docs.get(&p).unwrap().is_dirty(), "disk has Xhello, the buffer hello");
        assert!(save_path(&mut state, &p, false));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hello");
        assert!(!state.docs.get(&p).unwrap().is_dirty());
    }

    #[test]
    fn external_write_landing_during_a_flight_is_detected_after_it() {
        let (_d, mut state, p) = dirty_doc(&[("a.md", "hello")], "a.md", "X");
        autosave_async(&mut state, &p);
        assert!(!state.docs.changed_on_disk(&p)); // the watcher event arrives mid-flight
        let t0 = std::time::Instant::now();
        while std::fs::read_to_string(&p).unwrap() != "Xhello" {
            assert!(t0.elapsed() < std::time::Duration::from_secs(5));
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        std::fs::write(&p, "external").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options().write(true).open(&p).unwrap().set_modified(later).unwrap();
        state.docs.get_mut(&p).unwrap().apply_edit(0..0, "Y");
        wait_saves(&mut state, &p);
        assert!(state.docs.get(&p).unwrap().external_change);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "external", "the follow-up save did not overwrite it");
    }

    #[test]
    fn removing_a_doc_waits_for_its_write() {
        let (_d, mut state, p) = dirty_doc(&[("a.md", "hello")], "a.md", "X");
        autosave_async(&mut state, &p);
        state.docs.remove(&p);
        assert!(!state.docs.is_saving(&p));
        std::fs::remove_file(&p).unwrap();
        poll_saves(&mut state);
        assert!(!p.exists(), "no write after the delete");
    }

    #[test]
    fn dead_wikilink_opens_confirmation_without_creating_file() {
        let (_d, mut state) = crate::test_support::state_with(&[("a.md", "hello [[Missing]]")]);
        let a = state.vault_path("a.md");
        let missing = a.parent().unwrap().join("Missing.md");
        let _ = execute(
            &mut state,
            &mut TestPlatform,
            Command::OpenLink { note: "Missing".into(), heading: None, from: Some(a), new_tab: false },
        );
        assert!(!missing.exists(), "clicking dead link must not create file immediately");
        let Some(Dialog::Confirm { message, ok_label, on_ok, focus, .. }) = state.dialogs.current.take() else {
            panic!("expected Confirm dialog");
        };
        assert_eq!(message, "Note 'Missing' does not exist.");
        assert_eq!(ok_label, "Create");
        assert!(focus);
        for cmd in on_ok {
            let _ = execute(&mut state, &mut TestPlatform, cmd);
        }
        assert!(missing.exists(), "confirming Create must create Missing.md");
        assert_eq!(state.tabs.active_path(), Some(missing.as_path()));
    }

    #[test]
    fn dead_wikilink_without_roots_reports_error() {
        let (_d, mut state) = crate::test_support::state_with(&[]);
        state.workspace.roots.clear();
        let _ = execute(
            &mut state,
            &mut TestPlatform,
            Command::OpenLink { note: "Missing".into(), heading: None, from: None, new_tab: false },
        );
        assert!(state.dialogs.current.is_none());
        assert!(state.status.current().is_some_and(|m| m.error && m.text.contains("Add a folder")));
    }

    #[test]
    fn history_back_evicted_note_is_non_blocking_and_restores_cursor_and_scroll() {
        let (_d, mut state) = crate::test_support::state_with(&[("a.md", "alpha\nbeta\ngamma"), ("b.md", "second")]);
        let a = state.vault_path("a.md");
        let b = state.vault_path("b.md");
        let _ = execute(&mut state, &mut TestPlatform, Command::OpenPath { path: a.clone(), new_tab: false, line: None });
        let t0 = std::time::Instant::now();
        while state.is_active_tab_loading() {
            assert!(t0.elapsed() < std::time::Duration::from_secs(5));
            poll_open(&mut state);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        state.tabs.tabs[0].cursor = Some(6..6);
        state.tabs.tabs[0].editor_scroll.offset = 123.5;

        let _ = execute(&mut state, &mut TestPlatform, Command::OpenPath { path: b.clone(), new_tab: false, line: None });
        let t0 = std::time::Instant::now();
        while state.is_active_tab_loading() {
            assert!(t0.elapsed() < std::time::Duration::from_secs(5));
            poll_open(&mut state);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        // Navigating away released `a.md` from DocStore (evicted).
        assert!(state.docs.get(&a).is_none());
        let sync_before = state.docs.sync_loads;

        state.docs.prefetch.set_open_paused(true);
        let _ = execute(&mut state, &mut TestPlatform, Command::HistoryBack);
        assert_eq!(state.docs.sync_loads, sync_before, "HistoryBack must not sync-load");
        assert_eq!(state.tabs.active_path(), Some(a.as_path()));
        assert!(state.is_active_tab_loading());

        state.docs.prefetch.set_open_paused(false);
        let t0 = std::time::Instant::now();
        while state.is_active_tab_loading() {
            assert!(t0.elapsed() < std::time::Duration::from_secs(5));
            poll_open(&mut state);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(state.docs.sync_loads, sync_before, "async poll_open must not sync-load");
        assert_eq!(state.tabs.tabs[0].pending_cursor, Some(6..6));
        assert_eq!(state.tabs.tabs[0].editor_scroll.force, Some(123.5));
    }

    #[test]
    fn history_back_failed_load_rolls_back_without_corrupting_history() {
        let (_d, mut state) = crate::test_support::state_with(&[("a.md", "alpha"), ("b.md", "beta")]);
        let a = state.vault_path("a.md");
        let b = state.vault_path("b.md");
        let _ = execute(&mut state, &mut TestPlatform, Command::OpenPath { path: a.clone(), new_tab: false, line: None });
        while state.is_active_tab_loading() {
            poll_open(&mut state);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let _ = execute(&mut state, &mut TestPlatform, Command::OpenPath { path: b.clone(), new_tab: false, line: None });
        while state.is_active_tab_loading() {
            poll_open(&mut state);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        std::fs::remove_file(&a).unwrap();
        let _ = execute(&mut state, &mut TestPlatform, Command::HistoryBack);
        let t0 = std::time::Instant::now();
        while state.tabs.tabs[0].path == a && t0.elapsed() < std::time::Duration::from_secs(5) {
            poll_open(&mut state);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(state.tabs.tabs[0].path, b);
        assert_eq!(state.tabs.tabs[0].hist_pos, 1);
        assert_eq!(state.tabs.tabs[0].history, vec![a, b]);
        assert!(state.status.current().is_some_and(|m| m.error && m.text.contains("Cannot open")));
    }

    #[test]
    fn reopen_closed_tab_and_unloaded_untitled_do_not_sync_load() {
        let (_d, mut state) = crate::test_support::state_with(&[("a.md", "alpha")]);
        let a = state.vault_path("a.md");
        state.tabs.closed.push(a.clone());
        let sync_before = state.docs.sync_loads;
        state.docs.prefetch.set_open_paused(true);
        let _ = execute(&mut state, &mut TestPlatform, Command::ReopenClosedTab);
        assert_eq!(state.docs.sync_loads, sync_before);
        assert_eq!(state.tabs.active_path(), Some(a.as_path()));
        state.docs.prefetch.set_open_paused(false);

        // Unloaded restored untitled tab must NOT be treated as blank or deleted on CloseTab.
        let u = iron_sheets_core::untitled::path_for(&state.tabs.untitled_dir, 9);
        std::fs::create_dir_all(u.parent().unwrap()).unwrap();
        std::fs::write(&u, "saved untitled content").unwrap();
        let idx = state.tabs.new_untitled(u.clone(), ViewMode::Source);
        assert!(state.docs.get(&u).is_none());
        let _ = execute(&mut state, &mut TestPlatform, Command::CloseTab(idx));
        assert_eq!(state.docs.sync_loads, sync_before, "CloseTab on unloaded untitled must not sync-open");
        assert!(u.exists(), "unloaded untitled file must not be deleted");
        assert!(state.tabs.is_open(&u), "tab stays open until loaded");
    }
}
