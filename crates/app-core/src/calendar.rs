//! Calendar module: date math, daily-note paths, the month grid model and `Command::OpenDaily`.
//! The grid marks days whose note is in the index (31 hash lookups per month or index change, no file IO);
//! only creating a missing note touches the disk.
//!
//! shortcut: weeks start on Monday, no word-count or open-task dots (needs `NoteMeta` fields from the indexer),
//! no `{{date}}` placeholders in templates; add them when asked.
use crate::command::open_created;
use crate::state::CoreMut;
use iron_sheets_core::config::AppConfig;
use iron_sheets_core::index::Index;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ymd {
    pub y: i32,
    pub m: u32,
    pub d: u32,
}

pub fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

pub fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        2 if is_leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl Ymd {
    pub fn today() -> Ymd {
        use chrono::Datelike;
        let n = chrono::Local::now().date_naive();
        Ymd { y: n.year(), m: n.month(), d: n.day() }
    }

    /// Seconds until the next local midnight (+1), for one scheduled repaint that moves the today highlight.
    pub fn secs_to_midnight() -> u64 {
        use chrono::Timelike;
        86_401 - u64::from(chrono::Local::now().num_seconds_from_midnight()).min(86_400)
    }

    /// Days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
    fn days(self) -> i64 {
        let y = i64::from(if self.m <= 2 { self.y - 1 } else { self.y });
        let (era, yoe) = (y.div_euclid(400), y.rem_euclid(400));
        let doy = (153 * ((i64::from(self.m) + 9) % 12) + 2) / 5 + i64::from(self.d) - 1;
        era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
    }

    fn from_days(z: i64) -> Ymd {
        let z = z + 719_468;
        let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        Ymd { y: (yoe + era * 400) as i32 + i32::from(m <= 2), m, d }
    }

    pub fn add_days(self, n: i64) -> Ymd {
        Ymd::from_days(self.days() + n)
    }

    /// The same day `n` months away, clamped to the length of that month.
    pub fn add_months(self, n: i32) -> Ymd {
        let t = self.y * 12 + self.m as i32 - 1 + n;
        let (y, m) = (t.div_euclid(12), t.rem_euclid(12) as u32 + 1);
        Ymd { y, m, d: self.d.min(days_in_month(y, m)) }
    }

    /// 0 = Monday.
    pub fn weekday(self) -> u32 {
        // 1970-01-01 was a Thursday.
        (self.days() + 3).rem_euclid(7) as u32
    }
}

pub const DEFAULT_FORMAT: &str = "YYYY-MM-DD";

/// `YYYY`, `MM` and `DD` replaced; a pattern that lacks one of them (every day would be the same file) falls back to the default.
pub fn format_name(fmt: &str, d: Ymd) -> String {
    let fmt = if ["YYYY", "MM", "DD"].iter().all(|t| fmt.contains(t)) { fmt } else { DEFAULT_FORMAT };
    fmt.replace("YYYY", &format!("{:04}", d.y)).replace("MM", &format!("{:02}", d.m)).replace("DD", &format!("{:02}", d.d))
}

/// Pushes the components of a relative setting one by one (so separators are native and the keys equal the walker's);
/// `..`, `.`, empty components, roots and prefixes (`C:`) are errors naming the setting. `allow_empty`: "" = nothing.
fn push_rel(p: &mut PathBuf, setting: &str, v: &str, allow_empty: bool) -> Result<(), String> {
    let v = v.trim();
    if v.is_empty() && allow_empty {
        return Ok(());
    }
    for c in v.split(['/', '\\']) {
        if c.is_empty() || c == "." || c == ".." || c.contains(':') {
            return Err(format!("{setting}: \"{v}\" must be a relative path inside the first workspace folder"));
        }
        p.push(c);
    }
    Ok(())
}

/// Path of the daily note of `d`: `<root>/<folder>/<name>.md`, always inside `root`.
pub fn daily_path(cfg: &AppConfig, root: &Path, d: Ymd) -> Result<PathBuf, String> {
    let mut p = root.to_path_buf();
    push_rel(&mut p, "Daily notes folder", &cfg.daily_folder, true)?;
    let name = format_name(&cfg.daily_format, d);
    let mut file = PathBuf::new();
    push_rel(&mut file, "Daily note name", &name, false)?;
    let mut file = file.into_os_string();
    file.push(".md");
    p.push(file);
    Ok(p)
}

const TEMPLATE_MAX: u64 = 1 << 20;

/// Template text: a regular file inside the root, at most 1 MiB.
fn read_template(cfg: &AppConfig, root: &Path) -> Result<String, String> {
    use std::io::Read;
    let mut p = root.to_path_buf();
    push_rel(&mut p, "Daily note template", &cfg.daily_template, false)?;
    let f = std::fs::File::open(&p).map_err(|e| format!("Daily note template: {e}"))?;
    if !f.metadata().is_ok_and(|m| m.is_file()) {
        return Err("Daily note template: not a file".into());
    }
    let mut text = String::new();
    f.take(TEMPLATE_MAX + 1).read_to_string(&mut text).map_err(|e| format!("Daily note template: {e}"))?;
    if text.len() as u64 > TEMPLATE_MAX {
        return Err("Daily note template: larger than 1 MiB".into());
    }
    Ok(text)
}

/// The month grid: shown month, selected day, and which days of the shown month have a note.
pub struct CalendarModel {
    pub month: (i32, u32),
    pub sel: Ymd,
    /// Bit `d` set = day `d` of `month` has a note.
    marks: u32,
    /// What `marks` was built from: month, index generation, config generation.
    key: Option<((i32, u32), u64, u64)>,
}

impl Default for CalendarModel {
    fn default() -> Self {
        CalendarModel::new(Ymd::today())
    }
}

impl CalendarModel {
    pub fn new(today: Ymd) -> Self {
        CalendarModel { month: (today.y, today.m), sel: today, marks: 0, key: None }
    }

    pub fn has_note(&self, day: u32) -> bool {
        self.marks & (1 << day) != 0
    }

    /// Moves the selection (and the shown month with it).
    pub fn select(&mut self, d: Ymd) {
        self.sel = d;
        self.month = (d.y, d.m);
    }

    pub fn move_days(&mut self, n: i64) {
        self.select(self.sel.add_days(n));
    }

    pub fn move_months(&mut self, n: i32) {
        self.select(self.sel.add_months(n));
    }

    /// Rebuilds the marks when the month, the index or the settings changed; false = the index was busy (retry next frame).
    pub fn update(&mut self, idx: &parking_lot::RwLock<Index>, cfg: &AppConfig, config_gen: u64, root: Option<&Path>) -> bool {
        let Some(g) = idx.try_read() else { return false };
        let key = Some((self.month, g.generation(), config_gen));
        if self.key == key {
            return true;
        }
        self.marks = 0;
        if let Some(root) = root {
            for d in 1..=days_in_month(self.month.0, self.month.1) {
                let path = daily_path(cfg, root, Ymd { y: self.month.0, m: self.month.1, d });
                if path.is_ok_and(|p| g.by_path(&p).is_some()) {
                    self.marks |= 1 << d;
                }
            }
        }
        self.key = key;
        true
    }
}

/// `Command::OpenDaily`: opens the daily note of `date` (today when `None`), creating it first when it is missing.
pub(crate) fn open_daily(state: &mut CoreMut<'_>, date: Option<Ymd>) {
    if !state.modules.enabled(crate::modules::CALENDAR) {
        state.status.error("Calendar is off (Settings, Modules)");
        return;
    }
    let Some(root) = state.workspace.roots.first().cloned() else {
        state.status.error("Add a folder to the workspace first");
        return;
    };
    let date = date.unwrap_or_else(Ymd::today);
    let path = match daily_path(state.config, &root, date) {
        Ok(p) => p,
        Err(e) => {
            state.status.error(e);
            return;
        }
    };
    if !path.exists() {
        let name = format_name(&state.config.daily_format, date);
        let text = if state.config.daily_template.trim().is_empty() {
            format!("# {name}\n")
        } else {
            read_template(state.config, &root).unwrap_or_else(|e| {
                state.status.error(e);
                format!("# {name}\n")
            })
        };
        // shortcut: create_dir_all and create_new run on the UI thread (tiny writes, but a network share can block); move to a worker if that hurts.
        let made = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => f.write_all(text.as_bytes()),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
                Err(e) => Err(e),
            }
        });
        if let Err(e) = made {
            state.status.error(format!("Cannot create {}: {e}", path.display()));
            return;
        }
        state.tree.invalidate(&root);
        if let Some(dir) = path.parent() {
            state.tree.invalidate(dir);
        }
        state.indexer.file_changed(path.clone());
        state.modules.on_save(&path);
    }
    open_created(state, path, false);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Command, execute};

    fn ymd(y: i32, m: u32, d: u32) -> Ymd {
        Ymd { y, m, d }
    }

    #[test]
    fn leap_years_and_month_lengths() {
        assert!(is_leap(2024) && is_leap(2000) && !is_leap(1900) && !is_leap(2023));
        assert_eq!((days_in_month(2024, 2), days_in_month(2023, 2), days_in_month(2023, 4), days_in_month(2023, 12)), (29, 28, 30, 31));
    }

    #[test]
    fn weekdays_and_day_arithmetic() {
        // Monday = 0.
        assert_eq!(ymd(1970, 1, 1).weekday(), 3);
        assert_eq!(ymd(2024, 2, 29).weekday(), 3);
        assert_eq!(ymd(2026, 11, 1).weekday(), 6);
        assert_eq!(ymd(2000, 3, 1).add_days(-1), ymd(2000, 2, 29));
        assert_eq!(ymd(2023, 12, 31).add_days(1), ymd(2024, 1, 1));
        assert_eq!(ymd(1969, 12, 31).add_days(1), ymd(1970, 1, 1));
        assert_eq!(ymd(2026, 11, 24).add_days(400).add_days(-400), ymd(2026, 11, 24));
        assert_eq!(ymd(2024, 1, 31).add_months(1), ymd(2024, 2, 29));
        assert_eq!(ymd(2024, 1, 15).add_months(-1), ymd(2023, 12, 15));
        assert_eq!(ymd(2024, 12, 15).add_months(1), ymd(2025, 1, 15));
    }

    #[test]
    fn names_and_paths() {
        let d = ymd(2026, 3, 7);
        assert_eq!(format_name("YYYY-MM-DD", d), "2026-03-07");
        assert_eq!(format_name("DD.MM.YYYY", d), "07.03.2026");
        assert_eq!(format_name("YYYY/MM/YYYY-MM-DD", d), "2026/03/2026-03-07");
        assert_eq!(format_name("daily", d), "2026-03-07", "a pattern without all tokens falls back");
        let cfg = AppConfig { daily_folder: "Journal\\Sub/x".into(), ..Default::default() };
        assert_eq!(daily_path(&cfg, Path::new("r"), d), Ok(Path::new("r").join("Journal").join("Sub").join("x").join("2026-03-07.md")));
        let cfg = AppConfig { daily_folder: String::new(), ..Default::default() };
        assert_eq!(daily_path(&cfg, Path::new("r"), d), Ok(Path::new("r").join("2026-03-07.md")));
        for bad in ["../x", "a/../b", "/abs", "a//b", "a/", "C:\\x", "C:x", "."] {
            let cfg = AppConfig { daily_folder: bad.into(), ..Default::default() };
            assert!(daily_path(&cfg, Path::new("r"), d).is_err_and(|e| e.contains("Daily notes folder")), "{bad}");
        }
        for bad in ["YYYY-MM-DD/", "../YYYY-MM-DD", "/YYYY-MM-DD", "YYYY//MM-DD"] {
            let cfg = AppConfig { daily_format: bad.into(), ..Default::default() };
            assert!(daily_path(&cfg, Path::new("r"), d).is_err_and(|e| e.contains("Daily note name")), "{bad}");
        }
    }

    struct P;
    impl crate::platform::Platform for P {
        fn waker(&self) -> std::sync::Arc<dyn crate::platform::Waker> {
            std::sync::Arc::new(crate::test_support::NoopWaker)
        }
        fn clipboard_set(&mut self, _: &str) {}
        fn close_window(&mut self) {}
        fn set_title(&mut self, _: &str) {}
    }

    #[test]
    fn creating_a_daily_note_uses_folder_format_and_template() {
        let (_d, mut st) = crate::test_support::state_with(&[("tpl.md", "## Plan\n")]);
        st.config.daily_folder = "Logs/Daily".into();
        st.config.daily_format = "DD.MM.YYYY".into();
        st.config.daily_template = "tpl.md".into();
        st.config.module_calendar = true;
        let cx = &mut crate::modules::ModuleCx { waker: &st.waker, commands: &mut st.commands };
        st.modules.sync(crate::modules::DEFS, &st.config, cx);
        let day = ymd(2026, 11, 24);
        let _ = execute(&mut st, &mut P, Command::OpenDaily(Some(day)));
        let p = st.vault_path("Logs/Daily/24.11.2026.md");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "## Plan\n");
        assert_eq!(st.tabs.active_path(), Some(p.as_path()));
        // An existing note is opened, not overwritten.
        std::fs::write(&p, "mine").unwrap();
        let _ = execute(&mut st, &mut P, Command::OpenDaily(Some(day)));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "mine");
        // Without a template: a heading with the name.
        st.config.daily_template.clear();
        let _ = execute(&mut st, &mut P, Command::OpenDaily(Some(ymd(2026, 11, 25))));
        assert_eq!(std::fs::read_to_string(st.vault_path("Logs/Daily/25.11.2026.md")).unwrap(), "# 25.11.2026\n");
    }

    #[test]
    fn bad_settings_create_nothing_and_report() {
        let (_d, mut st) = crate::test_support::state_with(&[("tpl.md", "x")]);
        let cx = &mut crate::modules::ModuleCx { waker: &st.waker, commands: &mut st.commands };
        st.modules.sync(crate::modules::DEFS, &st.config, cx);
        for (folder, fmt, tpl) in [("../out", "YYYY-MM-DD", ""), ("Daily", "YYYY-MM-DD/", ""), ("Daily", "YYYY-MM-DD", "../tpl.md")] {
            st.config.daily_folder = folder.into();
            st.config.daily_format = fmt.into();
            st.config.daily_template = tpl.into();
            let _ = execute(&mut st, &mut P, Command::OpenDaily(Some(ymd(2026, 1, 2))));
            assert!(st.status.current().is_some_and(|m| m.error));
        }
        assert!(!st.vault_path("../out").exists() && !st.vault_path("Daily/2026-01-02/.md").exists());
    }

    #[test]
    fn template_must_be_a_small_file() {
        let (_d, st) = crate::test_support::state_with(&[("big.md", "x")]);
        let root = st.workspace.roots[0].clone();
        std::fs::write(root.join("big.md"), vec![b'a'; (TEMPLATE_MAX + 1) as usize]).unwrap();
        std::fs::create_dir(root.join("dir")).unwrap();
        let cfg = |t: &str| AppConfig { daily_template: t.into(), ..Default::default() };
        assert!(read_template(&cfg("big.md"), &root).is_err_and(|e| e.contains("1 MiB")));
        assert!(read_template(&cfg("dir"), &root).is_err());
        assert!(read_template(&cfg("nope.md"), &root).is_err());
    }

    #[test]
    fn marks_follow_the_index() {
        let (_d, st) = crate::test_support::state_with(&[("Daily/2026-11-03.md", "x"), ("Daily/2026-12-03.md", "x"), ("Daily/Sub/2026-11-05.md", "x")]);
        let mut m = CalendarModel::new(ymd(2026, 11, 24));
        let root = st.workspace.roots.first().cloned();
        assert!(m.update(&st.indexer.index(), &st.config, st.config_gen, root.as_deref()));
        assert!((1..=30).all(|d| m.has_note(d) == (d == 3)));
        m.move_months(1);
        assert!(m.update(&st.indexer.index(), &st.config, st.config_gen, root.as_deref()));
        assert!(m.has_note(3) && !m.has_note(4));
        // A nested folder written with mixed separators finds the walker's keys.
        let cfg = AppConfig { daily_folder: "Daily\\Sub".into(), ..Default::default() };
        m.move_months(-1);
        assert!(m.update(&st.indexer.index(), &cfg, st.config_gen + 1, root.as_deref()));
        assert!((1..=30).all(|d| m.has_note(d) == (d == 5)));
    }
}
