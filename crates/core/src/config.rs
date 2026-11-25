//! Application configuration (JSON). Loading never fails.
use crate::paths::AppDirs;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ThemePref {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ViewMode {
    Source,
    #[default]
    Split,
    Preview,
}

/// Syntax of math inside Markdown notes (`$...$`, `$$...$$`, ```` ```math ````). `.typ` notes always use Typst.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MathSyntax {
    /// Detected per formula (`\name`, `\\`, `^{`, `_{` mean LaTeX), falling back to the other syntax on an error.
    #[default]
    Auto,
    Typst,
    Latex,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowGeom {
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}

impl Default for WindowGeom {
    fn default() -> Self {
        WindowGeom { width: 1200.0, height: 800.0, maximized: false }
    }
}

/// Geometry and open state of an auxiliary OS window (Settings). Plain numbers, no GUI types. `x`/`y` are the outer
/// window position in points; they stay `None` where the platform does not report a position (Wayland).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AuxWindowGeom {
    pub width: f32,
    pub height: f32,
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub open: bool,
}

impl Default for AuxWindowGeom {
    fn default() -> Self {
        AuxWindowGeom { width: 760.0, height: 560.0, x: None, y: None, open: false }
    }
}

/// Junk folders no note lives in; skipped by tree, index and watcher at any depth.
pub const DEFAULT_EXCLUDES: [&str; 2] = ["node_modules", "__pycache__"];

fn no_excludes_rev() -> u32 {
    0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub last_workspace: Option<String>,
    pub theme: ThemePref,
    pub ui_zoom: f32,
    /// Multiplier for mouse-wheel scrolling everywhere (1.0 = the toolkit default).
    pub scroll_speed: f32,
    pub editor_font_size: f32,
    pub preview_font_size: f32,
    pub autosave: bool,
    pub autosave_delay_ms: u32,
    pub sync_scroll: bool,
    /// Markdown preview uses the whole pane width instead of a 900 px column.
    pub preview_full_width: bool,
    /// How `![[note]]` embeds show in the preview: empty = inline, `collapsed` (title row, click to expand), `link`.
    pub embed_mode: String,
    /// Page preview popup on link hover: empty = with Ctrl, `always`, `off`.
    pub page_preview: String,
    /// Embeds inside embeds are drawn down to this depth.
    pub embed_max_depth: u32,
    pub default_view: ViewMode,
    pub sidebar_visible: bool,
    pub sidebar_width: f32,
    /// Sidebar views in header order; an id with a leading `-` is hidden. Empty = the default order, all shown;
    /// unknown ids are ignored and views missing here are appended (see `settings_model::sidebar_layout`).
    pub sidebar_views: Vec<String>,
    pub window: Option<WindowGeom>,
    pub show_hidden_files: bool,
    pub typst_system_fonts: bool,
    /// Only differences from the default keymap: command id -> shortcut strings (empty list = unbound).
    pub keybindings: BTreeMap<String, Vec<String>>,
    pub restore_session: bool,
    /// Rewrite links to a renamed or moved note: empty = ask, `always`, `never`.
    pub update_links: String,
    pub editor_wrap: bool,
    /// Line numbers in the source editor gutter.
    pub show_line_numbers: bool,
    /// Typing an opening bracket or quote also inserts the closing one.
    pub auto_close_brackets: bool,
    /// Markdown/Typst highlighting is skipped above this many KiB.
    pub highlight_limit_kb: u32,
    /// Notes above the size limit: highlight the part around the viewport (Markdown) instead of nothing.
    pub highlight_big_partial: bool,
    pub math_in_markdown: bool,
    /// Fenced ```mermaid blocks render as diagrams in the Markdown preview.
    pub render_mermaid: bool,
    /// When an edited diagram renders again: "" while typing (after a short pause), "save" when the note is saved.
    pub mermaid_render: String,
    /// Module "Kanban boards": `kanban-plugin` notes show as a board in Preview.
    pub module_kanban: bool,
    /// Module "Code highlighting": fenced code blocks in Preview are coloured by language.
    pub module_codehl: bool,
    /// Module "Git integration": branch in status bar, status badges in tree, diff markers in editor gutter.
    pub module_git: bool,
    /// Module "Calendar": month grid in the sidebar and daily notes.
    pub module_calendar: bool,
    /// Daily notes: folder relative to the first workspace root, file name pattern (`YYYY`, `MM`, `DD`; no extension),
    /// and an optional template note (path relative to the first root, or absolute); empty template = `# <name>`.
    pub daily_folder: String,
    pub daily_format: String,
    pub daily_template: String,
    /// Syntax of math in Markdown notes; missing in old config files (then Typst).
    pub math_syntax: MathSyntax,
    /// Show files that are neither notes nor images in the file tree.
    pub show_other_files: bool,
    /// A folder with a note named like it (`Foo/Foo.md`) opens that note when clicked in the tree.
    pub folder_notes: bool,
    /// Leave the folder note itself out of its folder's rows.
    pub folder_note_hide: bool,
    /// Glob patterns kept out of the tree and the index (see `glob::ExcludeSet`).
    pub exclude_globs: Vec<String>,
    /// 0 = file written before the default excludes existed (its empty `exclude_globs` meant "never set").
    #[serde(default = "no_excludes_rev")]
    pub excludes_rev: u32,
    pub links_new_tab: bool,
    /// Ask before moving a file or folder to the trash.
    pub confirm_trash: bool,
    /// "auto", "name", "sym" or "char".
    pub symbol_insert_mode: String,
    pub high_contrast: bool,
    /// Draw the menu bar as the window title bar (no OS decorations). Read at start only.
    pub custom_title_bar: bool,
    /// Settings window: size, position and whether it was open when the app quit. Missing in old config files.
    pub settings_window: Option<AuxWindowGeom>,
    // Status bar segments (all shown by default).
    pub status_counts: bool,
    pub status_position: bool,
    pub status_badge: bool,
    pub status_notes: bool,
    pub status_workspace: bool,
    // Tab strip toolbar buttons (all shown by default); their commands stay in the menu, the palette and the shortcuts.
    pub toolbar_view_buttons: bool,
    pub toolbar_sync_button: bool,
    pub toolbar_new_tab: bool,
    /// Font family ids (see `settings_model::EDITOR_FONTS` / `PREVIEW_FONTS`); empty = the default family.
    pub editor_font: String,
    pub preview_font: String,
    /// Optional TTF/OTF file that replaces the chosen family; empty = none. Read only when set.
    pub editor_font_file: String,
    pub preview_font_file: String,
    /// Module "Ligatures" (the old key `ligatures` loads into it): `->` as an arrow in the editor with a ligature font and
    /// as a glyph in the preview. `ligatures_in_code` also in code spans and blocks, `ligatures_in_preview` in preview prose.
    #[serde(alias = "ligatures")]
    pub module_ligatures: bool,
    pub ligatures_in_code: bool,
    pub ligatures_in_preview: bool,
    /// Where pasted images go: empty = subfolder of the note's folder, `same`, `root` (workspace root of the note), `folder`.
    pub attachment_mode: String,
    /// Subfolder name for the empty `attachment_mode`.
    pub attachment_subfolder: String,
    /// Folder for `attachment_mode` = `folder`.
    pub attachment_folder: String,
    /// Size of the interface text in points (13 is the toolkit default; UI zoom scales everything on top).
    pub ui_font_size: f32,
    /// Master switch for every animation (the app's own and egui's); off = every transition is instant.
    pub animations: bool,
    /// Animation speed multiplier (higher = faster transitions); 1 is the default pace.
    pub anim_speed: f32,
    /// Per-group switches, only read while `animations` is on (see `ui-egui/src/anim.rs` for what each one covers).
    pub anim_menus: bool,
    pub anim_collapse: bool,
    pub anim_slide: bool,
    pub anim_tree: bool,
    pub anim_hover: bool,
    pub anim_zoom: bool,
    pub anim_caret: bool,
    /// Length of the caret glide in ms at speed 1 (also scaled by `anim_speed`).
    pub caret_glide_ms: u32,
    /// A fading trail behind the gliding caret.
    pub caret_trail: bool,
    /// How long the trail's tail lags the head in ms at speed 1 (0 = no trail; clamped to 0-300 where used).
    pub caret_trail_ms: u32,
    /// Trail opacity at the head (0.1-1) and tail thickness as a fraction of the caret height (0-1).
    pub caret_trail_opacity: f32,
    pub caret_trail_width: f32,
    /// Old name of the master switch (inverted): read from old files by `from_json`, never written.
    #[serde(skip_serializing)]
    pub reduce_motion: bool,
    /// Colour overrides per theme variant: colour key -> `#rrggbb`. A missing key means the theme's own colour.
    pub colors_dark: BTreeMap<String, String>,
    pub colors_light: BTreeMap<String, String>,
    /// Id of the chosen user/built-in theme (see `themes`); empty = none, `theme` decides.
    pub theme_file: String,
    /// Professional mode: render with defaults, hiding cosmetic customisation; nothing else is changed.
    pub professional: bool,
    /// New-tab page: a random quote at the bottom (from `newtab_quotes_file`, else the built-in ones) and an optional
    /// background image (path; empty = none).
    pub newtab_quotes: bool,
    pub newtab_quotes_file: String,
    pub newtab_image: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            last_workspace: None,
            theme: ThemePref::System,
            ui_zoom: 1.0,
            scroll_speed: 1.0,
            editor_font_size: 14.0,
            preview_font_size: 15.0,
            autosave: true,
            autosave_delay_ms: 1000,
            sync_scroll: true,
            preview_full_width: false,
            embed_mode: String::new(),
            page_preview: String::new(),
            embed_max_depth: 2,
            default_view: ViewMode::Split,
            sidebar_visible: true,
            sidebar_width: 260.0,
            sidebar_views: Vec::new(),
            window: None,
            show_hidden_files: false,
            typst_system_fonts: true,
            keybindings: BTreeMap::new(),
            restore_session: true,
            update_links: String::new(),
            editor_wrap: true,
            show_line_numbers: false,
            auto_close_brackets: true,
            highlight_limit_kb: 300,
            highlight_big_partial: true,
            math_in_markdown: true,
            render_mermaid: true,
            mermaid_render: String::new(),
            module_kanban: true,
            module_codehl: true,
            module_git: true,
            module_calendar: true,
            daily_folder: "Daily".into(),
            daily_format: "YYYY-MM-DD".into(),
            daily_template: String::new(),
            math_syntax: MathSyntax::Auto,
            show_other_files: true,
            folder_notes: true,
            folder_note_hide: true,
            exclude_globs: DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect(),
            excludes_rev: 1,
            links_new_tab: false,
            confirm_trash: true,
            symbol_insert_mode: "auto".into(),
            high_contrast: false,
            custom_title_bar: false,
            settings_window: None,
            status_counts: true,
            status_position: true,
            status_badge: true,
            status_notes: true,
            status_workspace: true,
            toolbar_view_buttons: true,
            toolbar_sync_button: true,
            toolbar_new_tab: true,
            editor_font: String::new(),
            preview_font: String::new(),
            editor_font_file: String::new(),
            preview_font_file: String::new(),
            module_ligatures: true,
            ligatures_in_code: false,
            ligatures_in_preview: true,
            attachment_mode: String::new(),
            attachment_subfolder: "assets".into(),
            attachment_folder: String::new(),
            ui_font_size: 13.0,
            animations: true,
            anim_speed: 1.0,
            anim_menus: true,
            anim_collapse: true,
            anim_slide: true,
            anim_tree: true,
            anim_hover: true,
            anim_zoom: true,
            anim_caret: true,
            caret_glide_ms: 45,
            caret_trail: true,
            caret_trail_ms: 110,
            caret_trail_opacity: 0.5,
            caret_trail_width: 0.3,
            reduce_motion: false,
            colors_dark: BTreeMap::new(),
            colors_light: BTreeMap::new(),
            theme_file: String::new(),
            professional: false,
            newtab_quotes: true,
            newtab_quotes_file: String::new(),
            newtab_image: String::new(),
        }
    }
}

impl AppConfig {
    pub fn file(dirs: &AppDirs) -> PathBuf {
        dirs.config.join("config.json")
    }

    /// Missing, unreadable or corrupt files yield the defaults.
    pub fn load(dirs: &AppDirs) -> Self {
        Self::load_checked(dirs).0
    }

    /// Like `load`, plus a message when the file existed but could not be used completely.
    /// Unknown keys are ignored. A value of the wrong type only drops that key; a file that is not
    /// JSON at all yields the defaults.
    pub fn load_checked(dirs: &AppDirs) -> (Self, Option<String>) {
        let file = Self::file(dirs);
        let bytes = match std::fs::read(&file) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return (Self::default(), None),
            Err(e) => return (Self::default(), Some(format!("Cannot read {}: {e}; using default settings", file.display()))),
        };
        Self::from_json(&bytes, &file.display().to_string())
    }

    pub fn from_json(bytes: &[u8], what: &str) -> (Self, Option<String>) {
        let (mut c, note) = Self::parse_json(bytes, what);
        c.animations &= !std::mem::take(&mut c.reduce_motion);
        if c.excludes_rev == 0 {
            // Old files saved `[]` for everyone; a user's own list stays, an empty one gets the new defaults.
            if c.exclude_globs.is_empty() {
                c.exclude_globs = Self::default().exclude_globs;
            }
            c.excludes_rev = 1;
        }
        (c, note)
    }

    fn parse_json(bytes: &[u8], what: &str) -> (Self, Option<String>) {
        if let Ok(c) = serde_json::from_slice::<AppConfig>(bytes) {
            return (c, None);
        }
        let Ok(serde_json::Value::Object(map)) = serde_json::from_slice::<serde_json::Value>(bytes) else {
            return (Self::default(), Some(format!("{what} is not valid JSON; using default settings")));
        };
        let mut good = serde_json::Map::new();
        let mut bad = Vec::new();
        for (k, v) in map {
            let mut probe = good.clone();
            probe.insert(k.clone(), v.clone());
            if serde_json::from_value::<AppConfig>(serde_json::Value::Object(probe)).is_ok() {
                good.insert(k, v);
            } else {
                bad.push(k);
            }
        }
        let c = serde_json::from_value(serde_json::Value::Object(good)).unwrap_or_default();
        (c, Some(format!("Ignored invalid settings in {what}: {}", bad.join(", "))))
    }

    pub fn save(&self, dirs: &AppDirs) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        std::fs::create_dir_all(&dirs.config)?;
        crate::textio::atomic_write(&Self::file(dirs), &json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_config_without_trail_options_gets_their_defaults() {
        let (c, note) = AppConfig::from_json(br#"{"caret_trail": false, "caret_glide_ms": 60}"#, "t");
        assert!(note.is_none() && !c.caret_trail && c.caret_glide_ms == 60);
        assert_eq!((c.caret_trail_ms, c.caret_trail_opacity, c.caret_trail_width), (110, 0.5, 0.3));
    }

    #[test]
    fn old_config_without_theme_and_newtab_keys_loads() {
        let (c, note) = AppConfig::from_json(br#"{"theme": "Dark"}"#, "t");
        assert!(note.is_none() && c.theme == ThemePref::Dark);
        assert!(c.theme_file.is_empty() && !c.professional && c.newtab_quotes && c.newtab_image.is_empty());
    }

    #[test]
    fn defaults_and_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        let dirs = AppDirs::at(t.path());
        assert_eq!(AppConfig::load(&dirs), AppConfig::default());
        let c = AppConfig {
            theme: ThemePref::Dark,
            window: Some(WindowGeom { width: 10.0, height: 20.0, maximized: true }),
            last_workspace: Some("w".into()),
            ..Default::default()
        };
        c.save(&dirs).unwrap();
        assert_eq!(AppConfig::load(&dirs), c);
    }

    #[test]
    fn custom_title_bar_defaults_off_and_roundtrips() {
        assert!(!AppConfig::default().custom_title_bar);
        let t = tempfile::tempdir().unwrap();
        let dirs = AppDirs::at(t.path());
        let c = AppConfig { custom_title_bar: true, ..Default::default() };
        c.save(&dirs).unwrap();
        assert!(AppConfig::load(&dirs).custom_title_bar);
    }

    #[test]
    fn old_ligatures_key_loads_into_the_module() {
        let d = AppConfig::default();
        assert!(d.module_ligatures && d.ligatures_in_preview && !d.ligatures_in_code);
        let (c, note) = AppConfig::from_json(br#"{"ligatures": false, "ligatures_in_code": true}"#, "t");
        assert!(note.is_none() && !c.module_ligatures && c.ligatures_in_code);
        assert!(AppConfig::from_json(br#"{"ligatures": true}"#, "t").0.module_ligatures);
        let back = AppConfig::from_json(serde_json::to_string(&c).unwrap().as_bytes(), "t").0;
        assert!(!back.module_ligatures);
    }

    #[test]
    fn confirm_trash_defaults_true_and_roundtrips() {
        assert!(AppConfig::default().confirm_trash);
        assert!(AppConfig::from_json(br#"{"ui_zoom": 1.25}"#, "t").0.confirm_trash);
        let c = AppConfig { confirm_trash: false, ..Default::default() };
        let back = AppConfig::from_json(serde_json::to_string(&c).unwrap().as_bytes(), "t").0;
        assert!(!back.confirm_trash);
    }

    #[test]
    fn settings_window_is_optional_and_roundtrips() {
        let t = tempfile::tempdir().unwrap();
        let dirs = AppDirs::at(t.path());
        std::fs::create_dir_all(&dirs.config).unwrap();
        // an old file without the field
        std::fs::write(AppConfig::file(&dirs), r#"{"ui_zoom": 1.25}"#).unwrap();
        assert_eq!(AppConfig::load(&dirs).settings_window, None);
        // a partial entry fills the rest from the defaults
        std::fs::write(AppConfig::file(&dirs), r#"{"settings_window": {"open": true, "width": 900.0}}"#).unwrap();
        let g = AppConfig::load(&dirs).settings_window.unwrap();
        assert!(g.open && g.width == 900.0 && g.height == 560.0 && g.x.is_none() && g.y.is_none());
        let c =
            AppConfig { settings_window: Some(AuxWindowGeom { width: 800.0, height: 600.0, x: Some(10.0), y: Some(-20.0), open: true }), ..Default::default() };
        c.save(&dirs).unwrap();
        assert_eq!(AppConfig::load(&dirs), c);
    }

    #[test]
    fn new_display_options_default_on_and_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        let dirs = AppDirs::at(t.path());
        std::fs::create_dir_all(&dirs.config).unwrap();
        // An old file has none of them: everything stays as it was.
        std::fs::write(AppConfig::file(&dirs), r#"{"ui_zoom": 1.25}"#).unwrap();
        let c = AppConfig::load(&dirs);
        assert!(c.status_counts && c.status_position && c.status_badge && c.status_notes && c.status_workspace);
        assert!(c.toolbar_view_buttons && c.toolbar_sync_button && c.toolbar_new_tab);
        assert!(c.editor_font.is_empty() && c.editor_font_file.is_empty() && c.colors_dark.is_empty());
        assert_eq!(c.ui_font_size, 13.0);
        let mut c = AppConfig { status_counts: false, toolbar_new_tab: false, editor_font: "hack".into(), ui_font_size: 15.0, ..Default::default() };
        c.colors_light.insert("accent".into(), "#336699".into());
        c.save(&dirs).unwrap();
        assert_eq!(AppConfig::load(&dirs), c);
    }

    #[test]
    fn sidebar_views_default_empty_and_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        let dirs = AppDirs::at(t.path());
        std::fs::create_dir_all(&dirs.config).unwrap();
        std::fs::write(AppConfig::file(&dirs), r#"{"ui_zoom": 1.25}"#).unwrap();
        assert!(AppConfig::load(&dirs).sidebar_views.is_empty());
        let c = AppConfig { sidebar_views: vec!["search".into(), "-outline".into()], ..Default::default() };
        c.save(&dirs).unwrap();
        assert_eq!(AppConfig::load(&dirs), c);
    }

    #[test]
    fn math_syntax_defaults_to_auto_and_keeps_explicit_choices() {
        let t = tempfile::tempdir().unwrap();
        let dirs = AppDirs::at(t.path());
        std::fs::create_dir_all(&dirs.config).unwrap();
        std::fs::write(AppConfig::file(&dirs), r#"{"math_in_markdown": true}"#).unwrap();
        assert_eq!(AppConfig::load(&dirs).math_syntax, MathSyntax::Auto);
        std::fs::write(AppConfig::file(&dirs), r#"{"math_syntax": "Typst"}"#).unwrap();
        assert_eq!(AppConfig::load(&dirs).math_syntax, MathSyntax::Typst);
        std::fs::write(AppConfig::file(&dirs), r#"{"math_syntax": "Latex"}"#).unwrap();
        assert_eq!(AppConfig::load(&dirs).math_syntax, MathSyntax::Latex);
        // an unknown value falls back to the defaults for the whole file
        std::fs::write(AppConfig::file(&dirs), r#"{"math_syntax": "Tex2"}"#).unwrap();
        let _ = AppConfig::load(&dirs);
        let c = AppConfig { math_syntax: MathSyntax::Latex, ..Default::default() };
        c.save(&dirs).unwrap();
        assert_eq!(AppConfig::load(&dirs).math_syntax, MathSyntax::Latex);
    }

    #[test]
    fn partial_unknown_and_corrupt() {
        let t = tempfile::tempdir().unwrap();
        let dirs = AppDirs::at(t.path());
        std::fs::create_dir_all(&dirs.config).unwrap();
        std::fs::write(AppConfig::file(&dirs), r#"{"ui_zoom": 1.5, "future": [1,2], "autosave": false}"#).unwrap();
        let c = AppConfig::load(&dirs);
        assert_eq!(c.ui_zoom, 1.5);
        assert!(!c.autosave);
        assert_eq!(c.editor_font_size, 14.0);
        std::fs::write(AppConfig::file(&dirs), b"{ not json \xff").unwrap();
        assert_eq!(AppConfig::load(&dirs), AppConfig::default());
        let (c, msg) = AppConfig::load_checked(&dirs);
        assert_eq!(c, AppConfig::default());
        assert!(msg.is_some());
    }

    #[test]
    fn wrong_typed_value_drops_only_that_key() {
        let (c, msg) = AppConfig::from_json(br#"{"autosave": "yes", "ui_zoom": 2.0, "keybindings": {"file.save": ["Ctrl+Q"]}}"#, "x");
        assert_eq!(c.ui_zoom, 2.0);
        assert!(c.autosave);
        assert_eq!(c.keybindings["file.save"], vec!["Ctrl+Q".to_string()]);
        assert!(msg.unwrap().contains("autosave"));
    }

    #[test]
    fn old_reduce_motion_switches_animations_off_and_is_not_written_back() {
        assert!(AppConfig::from_json(b"{}", "x").0.animations);
        let (c, _) = AppConfig::from_json(br#"{"reduce_motion": true}"#, "x");
        assert!(!c.animations);
        assert!(!serde_json::to_string(&c).unwrap().contains("reduce_motion"));
        assert!(AppConfig::from_json(br#"{"reduce_motion": false}"#, "x").0.animations);
    }

    #[test]
    fn default_excludes_migration_and_explicit_choices() {
        let t = tempfile::tempdir().unwrap();
        let dirs = AppDirs::at(t.path());
        std::fs::create_dir_all(&dirs.config).unwrap();
        let want: Vec<String> = DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect();
        let load = |json: &str| {
            std::fs::write(AppConfig::file(&dirs), json).unwrap();
            AppConfig::load(&dirs).exclude_globs
        };
        assert_eq!(AppConfig::default().exclude_globs, want);
        assert_eq!(load(r#"{"ui_zoom": 1.0}"#), want); // never touched
        assert_eq!(load(r#"{"exclude_globs": []}"#), want); // written by an old version for everyone
        assert_eq!(load(r#"{"exclude_globs": ["drafts"]}"#), vec!["drafts".to_string()]); // own list kept
        // a user who removes the defaults in settings keeps an empty list through save/load
        let c = AppConfig { exclude_globs: Vec::new(), ..Default::default() };
        c.save(&dirs).unwrap();
        assert!(AppConfig::load(&dirs).exclude_globs.is_empty());
        // and a saved default list stays the default
        AppConfig::default().save(&dirs).unwrap();
        assert_eq!(AppConfig::load(&dirs).exclude_globs, want);
    }

    #[test]
    fn missing_file_is_silent() {
        let t = tempfile::tempdir().unwrap();
        assert!(AppConfig::load_checked(&AppDirs::at(t.path())).1.is_none());
    }

    /// Storage comparison (run with `cargo test -p iron-sheets-core --release -- --ignored --nocapture`).
    #[test]
    #[ignore]
    fn bench_parse() {
        let mut c = AppConfig::default();
        for i in 0..80 {
            c.keybindings.insert(format!("cmd.number_{i}"), vec![format!("Ctrl+Shift+F{}", i % 12 + 1), "Alt+X".into()]);
        }
        let json = serde_json::to_vec_pretty(&c).unwrap();
        let n = 2000;
        let t = std::time::Instant::now();
        for _ in 0..n {
            std::hint::black_box(serde_json::from_slice::<AppConfig>(&json).unwrap());
        }
        println!("json {} bytes: {:.1} us/parse", json.len(), t.elapsed().as_secs_f64() * 1e6 / n as f64);
    }
}
