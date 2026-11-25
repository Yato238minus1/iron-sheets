#![allow(dead_code)]

use crate::common::{self, Fx, Slot};
use egui::accesskit::Role;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use iron_sheets::{App, AppOptions};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command as SysCommand;
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

pub type H = Harness<'static, Slot>;

/// Protects process-wide environment mutations (e.g. `PATH` in missing-git tests).
/// Every normal `App` / `git` test holds `.read()` for its lifetime; env-mutating tests take `.write()`.
pub static ENV_LOCK: RwLock<()> = RwLock::new(());

pub fn read_env_lock() -> RwLockReadGuard<'static, ()> {
    ENV_LOCK.read().unwrap_or_else(|e| e.into_inner())
}

pub fn write_env_lock() -> RwLockWriteGuard<'static, ()> {
    ENV_LOCK.write().unwrap_or_else(|e| e.into_inner())
}

/// Restores or removes an environment variable on drop.
/// Caller MUST hold `write_env_lock()` for the entire lifetime of this guard.
pub struct EnvVarGuard {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    pub fn set(key: &'static str, val: impl AsRef<std::ffi::OsStr>) -> Self {
        let prev = std::env::var_os(key);
        // SAFETY: callers hold `ENV_LOCK.write()`, serializing against all `ENV_LOCK.read()` tests.
        unsafe { std::env::set_var(key, val) };
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: `ENV_LOCK.write()` is still held until after `EnvVarGuard` drops.
        unsafe {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

pub fn launch(opts: AppOptions) -> H {
    common::harness(opts)
}

pub fn launch_dt(opts: AppOptions, step_dt: f32) -> H {
    common::harness_dt(opts, step_dt)
}

pub fn launch_size(opts: AppOptions, size: Vec2, step_dt: f32) -> H {
    iron_sheets::theme::set_row_recording(true);
    let mut h = Harness::builder()
        .with_size(size)
        .with_step_dt(step_dt)
        .with_accessibility_check(false)
        .build_ui_state(
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
    common::settle(&mut h);
    h
}

pub fn launch_a11y(opts: AppOptions) -> H {
    iron_sheets::theme::set_row_recording(true);
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 720.0))
        .with_step_dt(0.25)
        .with_accessibility_check(true)
        .build_ui_state(
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
    common::settle(&mut h);
    assert_a11y_ok(&mut h);
    h
}

pub fn assert_a11y_ok(h: &mut H) {
    assert!(
        h.run_ok().is_some(),
        "harness exceeded max_steps before running check_accessibility()"
    );
}

pub fn relaunch_same_dirs(mut h: H, make_opts: impl FnOnce() -> AppOptions) -> H {
    h.state_mut().app().on_quit();
    drop(h);
    launch(make_opts())
}

pub fn settle(h: &mut H) {
    common::settle(h);
}

pub fn wants_immediate_repaint(h: &H) -> bool {
    common::wants_immediate_repaint(h)
}

pub fn wait_until(h: &mut H, timeout: Duration, what: &str, mut pred: impl FnMut(&mut H) -> bool) {
    let t0 = Instant::now();
    loop {
        h.step();
        if pred(h) {
            return;
        }
        assert!(t0.elapsed() < timeout, "timed out after {timeout:?} waiting for: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub fn has_label(h: &H, label: &str) -> bool {
    h.query_all_by_label(label).next().is_some()
}

pub fn has_label_contains(h: &H, sub: &str) -> bool {
    h.query_all_by_label_contains(sub).next().is_some()
}

pub fn wait_indexed(h: &mut H) {
    wait_until(h, Duration::from_secs(12), "workspace indexing finished", |h| {
        !has_label_contains(h, "Indexing")
            && !has_label_contains(h, "indexing")
            && has_label_contains(h, " note")
    });
    settle(h);
}

pub fn press_key(h: &mut H, key: Key) {
    h.key_press(key);
    settle(h);
}

pub fn press_shortcut(h: &mut H, mods: Modifiers, key: Key) {
    h.key_press_modifiers(mods, key);
    settle(h);
}

pub fn type_text(h: &mut H, text: &str) {
    h.event(Event::Text(text.into()));
    settle(h);
}

pub fn click_label(h: &mut H, label: &str) {
    h.get_all_by_label(label)
        .next()
        .unwrap_or_else(|| panic!("label not found: {label:?}"))
        .click();
    settle(h);
}

pub fn click_tree_item(h: &mut H, label: &str) {
    h.get_by_role_and_label(Role::TreeItem, label).click();
    settle(h);
}

pub fn click_pos(h: &mut H, pos: Pos2) {
    click_pos_mods(h, pos, Modifiers::NONE);
}

pub fn click_pos_mods(h: &mut H, pos: Pos2, mods: Modifiers) {
    if mods != Modifiers::NONE {
        h.event(Event::ModifiersChanged(mods));
    }
    h.event(Event::PointerMoved(pos));
    h.step();
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: mods,
        });
        h.step();
    }
    if mods != Modifiers::NONE {
        h.event(Event::ModifiersChanged(Modifiers::NONE));
    }
    settle(h);
}

pub fn middle_click_pos(h: &mut H, pos: Pos2) {
    h.event(Event::PointerMoved(pos));
    h.step();
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos,
            button: PointerButton::Middle,
            pressed,
            modifiers: Modifiers::NONE,
        });
        h.step();
    }
    settle(h);
}

pub fn right_click_pos(h: &mut H, pos: Pos2) {
    h.event(Event::PointerMoved(pos));
    h.step();
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos,
            button: PointerButton::Secondary,
            pressed,
            modifiers: Modifiers::NONE,
        });
        h.step();
    }
    settle(h);
}

pub fn double_click_pos(h: &mut H, pos: Pos2) {
    h.event(Event::PointerMoved(pos));
    h.step();
    // Advance simulated time explicitly by 20ms per pointer event so both clicks stay well under
    // egui's 0.3s `max_double_click_delay` regardless of the harness `step_dt`.
    let mut t = h.ctx.input(|i| i.time);
    for _ in 0..2 {
        for pressed in [true, false] {
            t += 0.02;
            h.input_mut().time = Some(t);
            h.event(Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            });
            h.step();
        }
    }
    settle(h);
}

pub fn drag(h: &mut H, from: Pos2, to: Pos2, steps: usize) {
    let steps = steps.max(1);
    h.event(Event::PointerMoved(from));
    h.step();
    h.event(Event::PointerButton {
        pos: from,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.step();
    for i in 1..=steps {
        let p = from + (to - from) * (i as f32 / steps as f32);
        h.event(Event::PointerMoved(p));
        h.step();
    }
    h.event(Event::PointerButton {
        pos: to,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.step();
    settle(h);
}

pub fn drag_path(h: &mut H, pts: &[Pos2]) {
    assert!(!pts.is_empty(), "drag_path requires at least one point");
    h.event(Event::PointerMoved(pts[0]));
    h.step();
    h.event(Event::PointerButton {
        pos: pts[0],
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.step();
    for p in &pts[1..] {
        h.event(Event::PointerMoved(*p));
        h.step();
    }
    let last = *pts.last().expect("pts non-empty");
    h.event(Event::PointerButton {
        pos: last,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.step();
    settle(h);
}

pub fn wheel_at(h: &mut H, pos: Pos2, delta: Vec2) {
    h.event(Event::PointerMoved(pos));
    h.step();
    h.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta,
        modifiers: Modifiers::NONE,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
    });
    for _ in 0..35 {
        h.step();
    }
    settle(h);
}

#[derive(Debug)]
struct TestDroppedFile(PathBuf);

impl egui::DroppedFile for TestDroppedFile {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

pub fn drop_files(h: &mut H, paths: Vec<PathBuf>) {
    h.input_mut().dropped_files = paths
        .into_iter()
        .map(|p| std::sync::Arc::new(TestDroppedFile(p)) as egui::DroppedFileHandle)
        .collect();
    h.step();
    settle(h);
}

pub fn render_pixels(h: &mut H) -> image::RgbaImage {
    let _g = common::GPU_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    h.render().expect("render")
}

pub fn shot(h: &mut H, name: &str) -> PathBuf {
    common::shot(h, name)
}

pub fn editor_text(h: &H) -> String {
    h.get_by_role(Role::MultilineTextInput)
        .value()
        .unwrap_or_default()
}

pub fn editor_rect(h: &H) -> Rect {
    h.get_by_role(Role::MultilineTextInput).rect()
}

pub fn editor_focused(h: &H) -> bool {
    h.get_by_role(Role::MultilineTextInput)
        .accesskit_node()
        .is_focused()
}

pub fn editor_visible_center(h: &H) -> Pos2 {
    let screen = h.ctx.content_rect();
    let r = editor_rect(h);
    egui::pos2(
        r.center().x.clamp(screen.left() + 40.0, screen.right() - 40.0),
        (screen.top() + screen.bottom()) * 0.5,
    )
}

pub fn open_note_via_palette(h: &mut H, query: &str) {
    press_shortcut(h, Modifiers::COMMAND, Key::O);
    type_text(h, query);
    press_key(h, Key::Enter);
}

pub fn switch_sidebar(h: &mut H, label: &str) {
    let clicked = if let Some(btn) = h.query_all_by_label(label).find(|n| {
        let r = n.accesskit_node().role();
        r == Role::Button || r == Role::Tab || r == Role::CheckBox
    }) {
        btn.click();
        true
    } else {
        false
    };
    if clicked {
        settle(h);
        return;
    }
    // If sidebar is collapsed or view is in overflow menu, open via View -> Sidebar menu.
    click_label(h, "View");
    click_label(h, "Sidebar");
    let item = match label {
        "Files" => "Show files",
        "Search" => "Show search",
        "Outline" => "Show outline",
        "Links" => "Show links",
        "Symbols" => "Show symbols",
        "Git" => "Show Git changes",
        "Calendar" => "Show calendar",
        other => other,
    };
    click_label(h, item);
}

/// Scans `area` with pointer hover events until a NEW tooltip containing `tip_substr` appears.
pub fn find_tooltip_pos(h: &mut H, area: Rect, step_x: f32, step_y: f32, tip_substr: &str) -> Option<Pos2> {
    h.event(Event::PointerGone);
    h.step();
    let baseline = h.query_all_by_label_contains(tip_substr).count();
    let mut y = area.top() + 8.0;
    while y < area.bottom() - 4.0 {
        let mut x = area.left() + 8.0;
        while x < area.right() - 4.0 {
            let p = egui::pos2(x, y);
            h.event(Event::PointerMoved(p));
            h.step();
            if h.query_all_by_label_contains(tip_substr).count() > baseline {
                return Some(p);
            }
            x += step_x;
        }
        y += step_y;
    }
    None
}

/// Like `find_tooltip_pos`, but dwells >0.5s (`tooltip_delay`) at each point with a still pointer
/// so delayed `on_hover_text` / `Tooltip::for_widget` popups appear regardless of `step_dt`.
pub fn find_tooltip_pos_dwell(h: &mut H, area: Rect, step_x: f32, step_y: f32, tip_substr: &str) -> Option<Pos2> {
    h.event(Event::PointerGone);
    h.step();
    let baseline = h.query_all_by_label_contains(tip_substr).count();
    let mut y = area.top() + 8.0;
    while y < area.bottom() - 4.0 {
        let mut x = area.left() + 8.0;
        while x < area.right() - 4.0 {
            let p = egui::pos2(x, y);
            h.event(Event::PointerMoved(p));
            h.step();
            let t = h.ctx.input(|i| i.time) + 0.55;
            h.input_mut().time = Some(t);
            h.step();
            if h.query_all_by_label_contains(tip_substr).count() > baseline {
                return Some(p);
            }
            x += step_x;
        }
        y += step_y;
    }
    None
}

/// Counts distinct RGBA colors inside `logical_rect` of a rendered screenshot.
pub fn distinct_colors_in_rect(img: &image::RgbaImage, logical_rect: Rect, ppp: f32) -> usize {
    let x0 = ((logical_rect.left() * ppp).max(0.0) as u32).min(img.width());
    let y0 = ((logical_rect.top() * ppp).max(0.0) as u32).min(img.height());
    let x1 = ((logical_rect.right() * ppp).ceil().max(0.0) as u32).min(img.width());
    let y1 = ((logical_rect.bottom() * ppp).ceil().max(0.0) as u32).min(img.height());
    let mut seen = HashSet::new();
    for y in y0..y1 {
        for x in x0..x1 {
            seen.insert(img.get_pixel(x, y).0);
        }
    }
    seen.len()
}

pub fn rect_pixels_differ(a: &image::RgbaImage, b: &image::RgbaImage, logical_rect: Rect, ppp: f32) -> bool {
    let x0 = ((logical_rect.left() * ppp).max(0.0) as u32).min(a.width().min(b.width()));
    let y0 = ((logical_rect.top() * ppp).max(0.0) as u32).min(a.height().min(b.height()));
    let x1 = ((logical_rect.right() * ppp).ceil().max(0.0) as u32).min(a.width().min(b.width()));
    let y1 = ((logical_rect.bottom() * ppp).ceil().max(0.0) as u32).min(a.height().min(b.height()));
    for y in y0..y1 {
        for x in x0..x1 {
            if a.get_pixel(x, y) != b.get_pixel(x, y) {
                return true;
            }
        }
    }
    false
}

/// Logical preview area when in `Split` or `Preview` mode (bounded by content rect, tabstrip, statusbar, and editor if Split).
pub fn preview_area_rect(h: &H) -> Rect {
    let screen = h.ctx.content_rect();
    let top = 74.0;
    let bottom = screen.bottom() - 28.0;
    let left = if let Some(ed) = h.query_by_role(Role::MultilineTextInput) {
        ed.rect().right() + 12.0
    } else if let Some(tree) = h.query_all_by_role(Role::TreeItem).next() {
        tree.rect().right() + 12.0
    } else {
        screen.left() + 270.0
    };
    Rect::from_min_max(
        egui::pos2(left.min(screen.right() - 20.0), top),
        egui::pos2(screen.right() - 6.0, bottom),
    )
}

pub fn has_git() -> bool {
    SysCommand::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

pub fn run_git(dir: &Path, args: &[&str]) {
    let out = SysCommand::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

pub fn init_git_repo(dir: &Path) {
    run_git(dir, &["init", "-b", "main"]);
    run_git(dir, &["config", "user.email", "test@example.com"]);
    run_git(dir, &["config", "user.name", "Iron Sheets Test"]);
}

pub fn lz_compress_b64(input: &str) -> String {
    const KEY: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    if input.is_empty() {
        return String::new();
    }
    struct Bw {
        out: String,
        val: u32,
        pos: u32,
    }
    impl Bw {
        fn push_bit(&mut self, bit: u32) {
            self.val = (self.val << 1) | (bit & 1);
            if self.pos == 5 {
                self.pos = 0;
                self.out.push(KEY[self.val as usize] as char);
                self.val = 0;
            } else {
                self.pos += 1;
            }
        }
        fn write_bits(&mut self, n: u32, mut v: u32) {
            for _ in 0..n {
                self.push_bit(v & 1);
                v >>= 1;
            }
        }
    }
    let mut dict: HashMap<Vec<u16>, u32> = HashMap::new();
    let mut to_create: HashSet<Vec<u16>> = HashSet::new();
    let mut w: Vec<u16> = Vec::new();
    let (mut enlarge_in, mut dict_size, mut num_bits) = (2u32, 3u32, 2u32);
    let mut bw = Bw {
        out: String::new(),
        val: 0,
        pos: 0,
    };
    for u in input.encode_utf16() {
        let single = vec![u];
        if !dict.contains_key(&single) {
            dict.insert(single.clone(), dict_size);
            dict_size += 1;
            to_create.insert(single);
        }
        let mut wc = w.clone();
        wc.push(u);
        if dict.contains_key(&wc) {
            w = wc;
        } else {
            if to_create.remove(&w) {
                let first = w[0] as u32;
                if first < 256 {
                    bw.write_bits(num_bits, 0);
                    bw.write_bits(8, first);
                } else {
                    bw.write_bits(num_bits, 1);
                    bw.write_bits(16, first);
                }
                enlarge_in -= 1;
                if enlarge_in == 0 {
                    enlarge_in = 1 << num_bits;
                    num_bits += 1;
                }
            } else {
                bw.write_bits(num_bits, dict[&w]);
            }
            enlarge_in -= 1;
            if enlarge_in == 0 {
                enlarge_in = 1 << num_bits;
                num_bits += 1;
            }
            dict.insert(wc, dict_size);
            dict_size += 1;
            w = vec![u];
        }
    }
    if !w.is_empty() {
        if to_create.remove(&w) {
            let first = w[0] as u32;
            if first < 256 {
                bw.write_bits(num_bits, 0);
                bw.write_bits(8, first);
            } else {
                bw.write_bits(num_bits, 1);
                bw.write_bits(16, first);
            }
            enlarge_in -= 1;
            if enlarge_in == 0 {
                enlarge_in = 1 << num_bits;
                num_bits += 1;
            }
        } else {
            bw.write_bits(num_bits, dict[&w]);
        }
        enlarge_in -= 1;
        if enlarge_in == 0 {
            num_bits += 1;
        }
    }
    bw.write_bits(num_bits, 2);
    loop {
        bw.val <<= 1;
        if bw.pos == 5 {
            bw.out.push(KEY[bw.val as usize] as char);
            break;
        } else {
            bw.pos += 1;
        }
    }
    while !bw.out.len().is_multiple_of(4) {
        bw.out.push('=');
    }
    bw.out
}

pub fn obsidian_excalidraw_md(json: &str, text_elements: &str, embedded: Option<(&str, &str)>) -> String {
    let b64 = lz_compress_b64(json);
    let wrapped = b64
        .as_bytes()
        .chunks(256)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join("\n\n");
    let emb = match embedded {
        Some((id, target)) => format!("## Embedded Files\n{id}: [[{target}]]\n"),
        None => String::new(),
    };
    format!(
        "---\nexcalidraw-plugin: parsed\ntags: [excalidraw]\n---\n# Excalidraw Data\n## Text Elements\n{text_elements}\n{emb}%%\n## Drawing\n```compressed-json\n{wrapped}\n```\n%%\n"
    )
}

pub fn new_fx() -> Fx {
    Fx::new()
}
