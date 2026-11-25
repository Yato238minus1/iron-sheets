#![allow(clippy::collapsible_if)]
#![allow(dead_code)]
use egui_kittest::Harness;
use iron_sheets::{App, AppOptions};
use iron_sheets_core::config::AppConfig;
use iron_sheets_core::paths::AppDirs;
use std::path::{Path, PathBuf};

/// Temporary IRON_SHEETS_HOME plus a sample vault with two root folders.
pub struct Fx {
    pub dir: tempfile::TempDir,
}

pub struct Slot {
    pub opts: Option<AppOptions>,
    pub app: Option<App>,
}

impl Slot {
    pub fn app(&mut self) -> &mut App {
        self.app.as_mut().expect("app created on first frame")
    }
}

const NOTES: &[(&str, &str)] = &[
    ("Welcome.md", "# Welcome\n\nStart with [[Projects/Plan]] and [[Ideas]].\n\n- [ ] task one\n- [x] task two\n"),
    ("Ideas.md", "# Ideas\n\nSee [[Welcome]] and [[Reading#Books]].\n\n> a quote\n"),
    ("Reading.md", "# Reading\n\n## Books\n\nA list with a [link](https://example.org).\n\n## Papers\n\nNone yet.\n"),
    ("Projects/Plan.md", "# Plan\n\nBack to [[Welcome]].\n\n| a | b |\n|---|---|\n| 1 | 2 |\n"),
    ("Projects/Tasks.md", "# Tasks\n\n1. first\n2. second\n\nSee [[Plan]].\n"),
    ("Projects/Archive/Old.md", "# Old\n\nOld stuff, see [[Plan]].\n"),
    ("Projects/Archive/Older.md", "# Older\n\nEven older.\n"),
    ("Math.md", "# Math\n\nInline $a^2 + b^2 = c^2$ and\n\n$$\\int_0^1 x dx$$\n"),
];

const JOURNAL: &[(&str, &str)] = &[
    ("2024-01-01.md", "# New year\n\nResolutions: [[Ideas]].\n"),
    ("2024-01-02.md", "# Day two\n\nWorked on [[Plan]].\n"),
    ("2024-01-03.md", "# Day three\n\nRead [[Reading]].\n"),
    ("Report.typ", "= Report\n\nSome *Typst* text with $x^2$.\n"),
];

pub fn make_vault(base: &Path) -> (PathBuf, PathBuf) {
    let (notes, journal) = (base.join("notes"), base.join("journal"));
    for (root, files) in [(&notes, NOTES), (&journal, JOURNAL)] {
        for (rel, text) in files {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            std::fs::write(p, text).expect("write note");
        }
    }
    let mut img = image::RgbaImage::new(64, 48);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgba([(x * 4) as u8, (y * 5) as u8, 160, 255]);
    }
    img.save(journal.join("picture.png")).expect("png");
    (notes, journal)
}

impl Fx {
    pub fn new() -> Fx {
        let dir = tempfile::tempdir().expect("tempdir");
        make_vault(dir.path());
        Fx { dir }
    }
    pub fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }
    pub fn notes(&self) -> PathBuf {
        self.dir.path().join("notes")
    }
    pub fn journal(&self) -> PathBuf {
        self.dir.path().join("journal")
    }
    pub fn dirs(&self) -> AppDirs {
        AppDirs::at(&self.home())
    }
    pub fn opts(&self, paths: Vec<PathBuf>) -> AppOptions {
        let mut o = AppOptions::new(self.dirs());
        o.config = AppConfig { autosave_delay_ms: 50, ..AppConfig::default() };
        // Perf A/B: IRON_MODULES_OFF=1 starts with every module disabled.
        if std::env::var_os("IRON_MODULES_OFF").is_some() {
            (o.config.module_kanban, o.config.module_codehl, o.config.module_ligatures, o.config.module_git, o.config.module_calendar) = (false, false, false, false, false);
        }
        o.paths = paths;
        o.workspace = Some("test".into());
        o
    }
}

pub fn harness(opts: AppOptions) -> Harness<'static, Slot> {
    harness_dt(opts, 0.25)
}

/// Like `harness`, with the simulated time per frame; a short one (1/60) lets a test see the frames of an animation.
pub fn harness_dt(opts: AppOptions, step_dt: f32) -> Harness<'static, Slot> {
    // The app records the colour of tab and tree rows only when asked (it allocates per row per frame).
    iron_sheets::theme::set_row_recording(true);
    let mut h = Harness::builder().with_size(egui::vec2(1100.0, 720.0)).with_step_dt(step_dt).with_accessibility_check(false).build_ui_state(
        |ui, slot: &mut Slot| {
            if slot.app.is_none() {
                if let Some(o) = slot.opts.take() {
                    slot.app = Some(App::new(ui.ctx(), o));
                }
            }
            if let Some(a) = slot.app.as_mut() {
                a.frame(ui);
            }
        },
        Slot { opts: Some(opts), app: None },
    );
    settle(&mut h);
    h
}

/// Wait for pending async tree loads and run a few frames so queued commands run and layouts settle.
pub fn settle(h: &mut Harness<'static, Slot>) {
    let start = std::time::Instant::now();
    let mut settled = 0usize;
    while settled < 6 {
        h.step();
        let pending = h.state().app.as_ref().is_some_and(|a| {
            a.state.tree.has_pending_loads()
                || a.state.is_active_tab_loading()
                || a.state.dialogs.is_folder_browser_loading()
                || a.state.has_pending_trash()
        });
        if pending && start.elapsed() < std::time::Duration::from_secs(10) {
            settled = 0;
            std::thread::sleep(std::time::Duration::from_millis(2));
        } else {
            settled += 1;
        }
    }
}

/// wgpu creates a Vulkan instance per harness on its first render; creating them from several test threads at once
/// crashes inside the Vulkan loader (SIGSEGV in `vkEnumerateInstanceExtensionProperties`, seen in 1 of 5 runs of the
/// layout tests), so renders are serialised.
pub static GPU_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn shot(h: &mut Harness<'static, Slot>, name: &str) -> PathBuf {
    let dir =
        std::env::var_os("IRON_SHEETS_SHOTS_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-shots"));
    std::fs::create_dir_all(&dir).expect("shots dir");
    let img = {
        let _g = GPU_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        h.render().expect("render")
    };
    let p = dir.join(format!("{name}.png"));
    img.save(&p).expect("save png");
    p
}

/// True when the last frame asked for another frame at once (an animation, a pending command, a spinner, ...).
pub fn wants_immediate_repaint<S>(h: &Harness<'_, S>) -> bool {
    h.output().viewport_output.values().any(|v| v.repaint_delay == std::time::Duration::ZERO)
}
