//! Built-in feature modules: switched on and off in Settings, built only while enabled. A disabled module has no struct,
//! thread or channel, and every hook walks an empty `Vec`. See docs/plans/extensions.md 1.2 and 1.4.
//!
//! shortcut: no `decorate`/`overlay`/`epoch` yet (nothing uses them); spell check adds them with the editor hook (slice M3).
//!
//! A module that owns a preview-side service (a code highlighter) does it like `AppState::math`: the frontend-neutral
//! service trait lives in `services`, `settings_model::apply` swaps the real implementation and a no-op one in when
//! `Registry::enabled(ID)` changes (it already diffs `Applied`, which carries the module toggles).
use crate::command::Command;
use crate::platform::Waker;
use iron_sheets_core::config::AppConfig;
use iron_sheets_core::watch::FsEvent;
use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What a module may touch: wake the UI from a worker, queue commands (modules never call each other).
pub struct ModuleCx<'a> {
    pub waker: &'a Arc<dyn Waker>,
    pub commands: &'a mut Vec<Command>,
}

/// All hooks are optional and cheap: heavy work goes to the module's own worker, started in `start`.
pub trait Module: Send {
    fn id(&self) -> &'static str;
    /// Once on the UI thread, after the first frame (or when enabled later). Start workers here, never in the constructor.
    fn start(&mut self, _cx: &mut ModuleCx) {}
    /// The active document changed: another path (tab switch) or a new text version since the last frame. Mark dirty; queue work to a worker.
    fn on_doc_changed(&mut self, _path: &Path, _version: u64) {}
    fn on_save(&mut self, _path: &Path) {}
    fn on_workspace_roots(&mut self, _roots: &[PathBuf]) {}
    fn on_focus_gained(&mut self) {}
    fn on_fs_event(&mut self, _evs: &[FsEvent]) {}
    /// `Command::Module { id, arg }`, offered to every enabled module; the module checks `id`.
    fn on_command(&mut self, _id: &str, _arg: &str, _cx: &mut ModuleCx) {}
    /// Drain the worker channel once per frame; true = repaint.
    fn poll(&mut self) -> bool {
        false
    }
    fn in_flight(&self) -> usize {
        0
    }
    fn as_any(&self) -> Option<&dyn Any> {
        None
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        None
    }
}

/// One built-in module: its settings toggle and its constructor.
pub struct Def {
    pub id: &'static str,
    pub on: fn(&AppConfig) -> bool,
    pub make: fn() -> Box<dyn Module>,
}

#[cfg(feature = "kanban")]
pub const KANBAN: &str = "kanban";

/// Kanban boards: the module only marks the feature as on; the board view checks `Registry::enabled(KANBAN)`.
#[cfg(feature = "kanban")]
struct Kanban;
#[cfg(feature = "kanban")]
impl Module for Kanban {
    fn id(&self) -> &'static str {
        KANBAN
    }
}

#[cfg(feature = "codehl")]
pub const CODEHL: &str = "codehl";

/// Code highlighting: the module only marks the feature as on; the Markdown preview builds the highlighter while
/// `Registry::enabled(CODEHL)` (nothing is loaded before the first fenced block).
#[cfg(feature = "codehl")]
struct CodeHl;
#[cfg(feature = "codehl")]
impl Module for CodeHl {
    fn id(&self) -> &'static str {
        CODEHL
    }
}

pub const LIGATURES: &str = "ligatures";

/// Ligatures: the module only marks the feature as on; the theme reads `Registry::enabled(LIGATURES)` for the editor and the preview.
struct Ligatures;
impl Module for Ligatures {
    fn id(&self) -> &'static str {
        LIGATURES
    }
}

#[cfg(feature = "git")]
pub const GIT: &str = "git";

#[cfg(feature = "calendar")]
pub const CALENDAR: &str = "calendar";

/// Calendar: the module only marks the feature as on; the sidebar view and `Command::OpenDaily` check `Registry::enabled(CALENDAR)`.
#[cfg(feature = "calendar")]
struct Calendar;
#[cfg(feature = "calendar")]
impl Module for Calendar {
    fn id(&self) -> &'static str {
        CALENDAR
    }
}

pub static DEFS: &[Def] = &[
    #[cfg(feature = "kanban")]
    Def { id: KANBAN, on: |c| c.module_kanban, make: || Box::new(Kanban) },
    #[cfg(feature = "codehl")]
    Def { id: CODEHL, on: |c| c.module_codehl, make: || Box::new(CodeHl) },
    Def { id: LIGATURES, on: |c| c.module_ligatures, make: || Box::new(Ligatures) },
    #[cfg(feature = "git")]
    Def { id: GIT, on: |c| c.module_git, make: || Box::new(crate::git::GitModule::new()) },
    #[cfg(feature = "calendar")]
    Def { id: CALENDAR, on: |c| c.module_calendar, make: || Box::new(Calendar) },
];

#[derive(Default)]
pub struct Registry {
    mods: Vec<Box<dyn Module>>,
    /// False until the first frame is on screen: modules built before that are started by `go_live`.
    live: bool,
    last_doc: Option<(PathBuf, u64)>,
    roots: Vec<PathBuf>,
}

impl Registry {
    pub fn any(&self) -> bool {
        !self.mods.is_empty()
    }

    pub fn enabled(&self, id: &str) -> bool {
        self.mods.iter().any(|m| m.id() == id)
    }

    pub fn enable(&mut self, mut m: Box<dyn Module>, cx: &mut ModuleCx) {
        if self.enabled(m.id()) {
            return;
        }
        if !self.roots.is_empty() {
            m.on_workspace_roots(&self.roots);
        }
        if self.live {
            m.start(cx);
        }
        self.mods.push(m);
        // The newcomer has not seen the current document yet.
        self.last_doc = None;
    }

    pub fn disable(&mut self, id: &str) {
        self.mods.retain(|m| m.id() != id);
    }

    /// Build or drop modules so that they match `defs` under `config`; true when something changed.
    pub fn sync(&mut self, defs: &[Def], config: &AppConfig, cx: &mut ModuleCx) -> bool {
        let mut changed = false;
        for d in defs {
            match ((d.on)(config), self.enabled(d.id)) {
                (true, false) => self.enable((d.make)(), cx),
                (false, true) => self.disable(d.id),
                _ => continue,
            }
            changed = true;
        }
        changed
    }

    /// First frame is done: start the modules built so far; later ones start when built.
    pub fn go_live(&mut self, cx: &mut ModuleCx) {
        self.live = true;
        // Roots already reached every module through `enable`/`set_roots`; resending them would rescan.
        self.mods.iter_mut().for_each(|m| m.start(cx));
    }

    pub fn set_roots(&mut self, roots: &[PathBuf]) {
        self.roots = roots.to_vec();
        for m in &mut self.mods {
            m.on_workspace_roots(&self.roots);
        }
    }

    /// Once per frame: drain workers and report a changed active document. True = repaint.
    pub fn frame(&mut self, active_doc: Option<(&Path, u64)>) -> bool {
        if self.mods.is_empty() {
            return false;
        }
        if let Some((p, v)) = active_doc {
            if self.last_doc.as_ref().is_none_or(|(lp, lv)| lp != p || *lv != v) {
                self.last_doc = Some((p.to_path_buf(), v));
                self.mods.iter_mut().for_each(|m| m.on_doc_changed(p, v));
            }
        }
        self.mods.iter_mut().fold(false, |r, m| m.poll() | r)
    }

    pub fn on_save(&mut self, path: &Path) {
        self.mods.iter_mut().for_each(|m| m.on_save(path));
    }

    pub fn on_focus_gained(&mut self) {
        self.mods.iter_mut().for_each(|m| m.on_focus_gained());
    }

    pub fn on_fs_event(&mut self, evs: &[FsEvent]) {
        self.mods.iter_mut().for_each(|m| m.on_fs_event(evs));
    }

    pub fn on_command(&mut self, id: &str, arg: &str, cx: &mut ModuleCx) {
        self.mods.iter_mut().for_each(|m| m.on_command(id, arg, cx));
    }

    pub fn in_flight(&self) -> usize {
        self.mods.iter().map(|m| m.in_flight()).sum()
    }

    pub fn get<M: Module + 'static>(&self, id: &str) -> Option<&M> {
        self.mods.iter().find(|m| m.id() == id).and_then(|m| m.as_any()?.downcast_ref::<M>())
    }

    pub fn get_mut<M: Module + 'static>(&mut self, id: &str) -> Option<&mut M> {
        self.mods.iter_mut().find(|m| m.id() == id).and_then(|m| m.as_any_mut()?.downcast_mut::<M>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    struct NoWake;
    impl Waker for NoWake {
        fn wake(&self) {}
    }

    /// start, doc_changed, fs_event, command, poll
    #[derive(Default)]
    struct Calls([AtomicUsize; 5]);
    impl Calls {
        fn get(&self) -> [usize; 5] {
            std::array::from_fn(|i| self.0[i].load(Relaxed))
        }
    }
    struct Fake(&'static str, Arc<Calls>);
    impl Module for Fake {
        fn id(&self) -> &'static str {
            self.0
        }
        fn start(&mut self, _: &mut ModuleCx) {
            self.1.0[0].fetch_add(1, Relaxed);
        }
        fn on_doc_changed(&mut self, _: &Path, _: u64) {
            self.1.0[1].fetch_add(1, Relaxed);
        }
        fn on_fs_event(&mut self, _: &[FsEvent]) {
            self.1.0[2].fetch_add(1, Relaxed);
        }
        fn on_command(&mut self, id: &str, _: &str, _: &mut ModuleCx) {
            if id == "fake.go" {
                self.1.0[3].fetch_add(1, Relaxed);
            }
        }
        fn poll(&mut self) -> bool {
            self.1.0[4].fetch_add(1, Relaxed) == 0
        }
    }
    fn fake(id: &'static str) -> (Box<dyn Module>, Arc<Calls>) {
        let c = Arc::new(Calls::default());
        (Box::new(Fake(id, c.clone())), c)
    }
    const A: &str = "a.md";
    fn doc(v: u64) -> Option<(&'static Path, u64)> {
        Some((Path::new(A), v))
    }

    #[test]
    fn empty_registry_calls_nothing() {
        let waker: Arc<dyn Waker> = Arc::new(NoWake);
        let mut cmds = Vec::new();
        let mut cx = ModuleCx { waker: &waker, commands: &mut cmds };
        let mut r = Registry::default();
        assert!(!r.any() && !r.frame(doc(1)));
        r.on_fs_event(&[FsEvent::Rescan]);
        r.on_command("fake.go", "", &mut cx);
        r.go_live(&mut cx);
        let (m, c) = fake("fake");
        r.enable(m, &mut cx);
        r.disable("fake");
        assert!(!r.frame(doc(2)));
        assert_eq!(c.get(), [1, 0, 0, 0, 0]);
    }

    #[test]
    fn hooks_reach_enabled_modules_and_start_after_go_live() {
        let waker: Arc<dyn Waker> = Arc::new(NoWake);
        let mut cmds = Vec::new();
        let mut cx = ModuleCx { waker: &waker, commands: &mut cmds };
        let mut r = Registry::default();
        // Enabled before the first frame: built, not started.
        let (m, c) = fake("fake");
        r.enable(m, &mut cx);
        assert!(r.enabled("fake") && c.get()[0] == 0);
        r.go_live(&mut cx);
        assert_eq!(c.get()[0], 1);
        // One doc-changed per new (path, version); poll every frame; its result is the repaint flag.
        assert!(r.frame(doc(1)));
        assert!(!r.frame(doc(1)));
        assert!(!r.frame(doc(2)));
        assert_eq!((c.get()[1], c.get()[4]), (2, 3));
        r.on_fs_event(&[FsEvent::Rescan]);
        r.on_command("fake.go", "", &mut cx);
        r.on_command("other", "", &mut cx);
        assert_eq!((c.get()[2], c.get()[3]), (1, 1));
        r.disable("fake");
        assert!(!r.any());
    }

    #[test]
    fn go_live_does_not_resend_roots() {
        struct Roots(Arc<AtomicUsize>);
        impl Module for Roots {
            fn id(&self) -> &'static str {
                "roots"
            }
            fn on_workspace_roots(&mut self, _: &[PathBuf]) {
                self.0.fetch_add(1, Relaxed);
            }
        }
        let waker: Arc<dyn Waker> = Arc::new(NoWake);
        let mut cmds = Vec::new();
        let mut cx = ModuleCx { waker: &waker, commands: &mut cmds };
        let mut r = Registry::default();
        let n = Arc::new(AtomicUsize::new(0));
        r.set_roots(&[PathBuf::from("/r")]);
        r.enable(Box::new(Roots(n.clone())), &mut cx);
        r.go_live(&mut cx);
        assert_eq!(n.load(Relaxed), 1);
    }

    #[test]
    fn a_module_enabled_later_sees_the_current_document() {
        let waker: Arc<dyn Waker> = Arc::new(NoWake);
        let mut cmds = Vec::new();
        let mut cx = ModuleCx { waker: &waker, commands: &mut cmds };
        let mut r = Registry::default();
        let (m1, c1) = fake("one");
        let (m2, c2) = fake("two");
        r.enable(m1, &mut cx);
        r.frame(doc(1));
        r.enable(m2, &mut cx);
        r.frame(doc(1));
        assert_eq!((c1.get()[1], c2.get()[1]), (2, 1));
    }

    #[cfg(all(feature = "kanban", feature = "codehl", feature = "git", feature = "calendar"))]
    #[test]
    fn sync_follows_the_config_and_kanban_defaults_on() {
        let waker: Arc<dyn Waker> = Arc::new(NoWake);
        let mut cmds = Vec::new();
        let mut cx = ModuleCx { waker: &waker, commands: &mut cmds };
        let mut r = Registry::default();
        let (old, note) = AppConfig::from_json(b"{}", "test");
        assert!(note.is_none() && old.module_kanban, "old config files get the default");
        let mut cfg = AppConfig { module_kanban: false, module_codehl: false, module_ligatures: false, module_git: false, module_calendar: false, ..Default::default() };
        assert!(!r.sync(DEFS, &cfg, &mut cx) && !r.any());
        cfg.module_kanban = true;
        assert!(r.sync(DEFS, &cfg, &mut cx) && r.enabled(KANBAN) && !r.enabled(CODEHL));
        assert!(!r.sync(DEFS, &cfg, &mut cx));
        cfg.module_codehl = true;
        assert!(r.sync(DEFS, &cfg, &mut cx) && r.enabled(CODEHL));
        cfg.module_ligatures = true;
        assert!(r.sync(DEFS, &cfg, &mut cx) && r.enabled(LIGATURES));
        cfg.module_git = true;
        assert!(r.sync(DEFS, &cfg, &mut cx) && r.enabled(GIT));
        cfg.module_calendar = true;
        assert!(r.sync(DEFS, &cfg, &mut cx) && r.enabled(CALENDAR));
        cfg.module_kanban = false;
        cfg.module_codehl = false;
        cfg.module_ligatures = false;
        cfg.module_git = false;
        cfg.module_calendar = false;
        assert!(r.sync(DEFS, &cfg, &mut cx) && !r.any());
    }
}
