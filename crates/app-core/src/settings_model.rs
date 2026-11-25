//! Settings model: what is configurable (table of options), defaults, applying a changed config, and the keymap
//! override editing with its conflict logic. Widgets live in `settings.rs`.
use crate::command::all_commands;
use crate::keymap::{Captured, Keymap};
use crate::keys::{Shortcut, bindable, format_shortcut};
use crate::state::{AppState, CoreMut, SidebarView};
use iron_sheets_core::config::{AppConfig, MathSyntax};
use iron_sheets_core::glob::ExcludeSet;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Section {
    #[default]
    General,
    Interface,
    Editor,
    Preview,
    Colors,
    Typst,
    Files,
    Modules,
    Keyboard,
}

impl Section {
    pub const ALL: [Section; 9] = [
        Section::General,
        Section::Interface,
        Section::Editor,
        Section::Preview,
        Section::Colors,
        Section::Typst,
        Section::Files,
        Section::Modules,
        Section::Keyboard,
    ];
    pub fn title(self) -> &'static str {
        match self {
            Section::General => "General",
            Section::Interface => "Interface",
            Section::Colors => "Colors",
            Section::Editor => "Editor",
            Section::Preview => "Preview",
            Section::Typst => "Typst",
            Section::Files => "Files",
            Section::Modules => "Modules",
            Section::Keyboard => "Keyboard",
        }
    }
}

pub enum Kind {
    Bool(fn(&mut AppConfig) -> &mut bool),
    F32(fn(&mut AppConfig) -> &mut f32, f32, f32),
    U32(fn(&mut AppConfig) -> &mut u32, u32, u32),
    Theme,
    View,
    SymbolMode,
    MathSyntax,
    Globs,
    /// Order and visibility of the sidebar views (`sidebar_layout`).
    SidebarViews,
    /// One of a fixed list of (id, label) choices stored as a string; the first entry is the default (empty id).
    Choice(fn(&mut AppConfig) -> &mut String, &'static [(&'static str, &'static str)]),
    /// A free text (a font file path).
    Text(fn(&mut AppConfig) -> &mut String),
    /// A colour override per theme variant, by `theme_model::COLOR_KEYS` key.
    Color(&'static str),
}

/// Editor font families that ship with the app: (id, label). The empty id is the default.
pub const EDITOR_FONTS: &[(&str, &str)] = &[("", "DejaVu Sans Mono"), ("hack", "Hack"), ("lilex", "Lilex (ligatures)"), ("sans", "Ubuntu (sans-serif)")];
/// Preview font families that ship with the app: (id, label). The empty id is the default.
pub const PREVIEW_FONTS: &[(&str, &str)] =
    &[("", "Libertinus Serif"), ("newcm", "New Computer Modern"), ("sans", "Ubuntu (sans-serif)"), ("mono", "DejaVu Sans Mono")];

/// Choices of "Embeds": (id, label); the empty id is inline.
pub const EMBED_MODES: &[(&str, &str)] = &[("", "Inline"), ("collapsed", "Collapsed"), ("link", "Link only")];

/// Choices of "Page preview on hover"; the empty id is with Ctrl.
pub const PAGE_PREVIEWS: &[(&str, &str)] = &[("", "With Ctrl"), ("always", "Always"), ("off", "Off")];

/// Choices of "Re-render Mermaid diagrams": (id, label); the empty id is while typing.
#[cfg(feature = "mermaid")]
pub const MERMAID_RENDER: &[(&str, &str)] = &[("", "While typing"), ("save", "On save")];

/// Choices of "Update links on rename": (id, label); the empty id asks every time.
pub const UPDATE_LINKS: &[(&str, &str)] = &[("", "Ask"), ("always", "Always"), ("never", "Never")];

/// Choices of "Attachment location" (pasted images): (id, label); the empty id is a subfolder of the note's folder.
pub const ATTACHMENT_MODES: &[(&str, &str)] = &[
    ("", "Subfolder of the note's folder"),
    ("same", "Same folder as the note"),
    ("root", "Workspace root folder of the note"),
    ("folder", "Specific folder"),
];

pub struct Item {
    pub section: Section,
    pub label: &'static str,
    pub help: &'static str,
    /// Takes effect only after a restart (or when a workspace is opened).
    pub restart: bool,
    pub kind: Kind,
}

const fn item(section: Section, label: &'static str, help: &'static str, restart: bool, kind: Kind) -> Item {
    Item { section, label, help, restart, kind }
}

use Section as S;
pub static ITEMS: &[Item] = &[
    item(S::General, "Theme", "Dark, light, or follow the system", false, Kind::Theme),
    item(
        S::General,
        "Professional mode",
        "Present the app clean: default theme, fonts, font sizes, no caret trail, perf overlay or new-tab extras. Your settings stay saved; UI zoom and high contrast (accessibility) are kept",
        false,
        Kind::Bool(|c| &mut c.professional),
    ),
    item(S::General, "High contrast", "Stronger text and border colours", false, Kind::Bool(|c| &mut c.high_contrast)),
    item(S::General, "Scroll speed", "Mouse-wheel scrolling in all panes (1 = default)", false, Kind::F32(|c| &mut c.scroll_speed, 0.1, 10.0)),
    item(S::General, "UI zoom", "Scale factor of the whole interface", false, Kind::F32(|c| &mut c.ui_zoom, 0.5, 3.0)),
    item(S::General, "Autosave", "Save changed notes automatically", false, Kind::Bool(|c| &mut c.autosave)),
    item(S::General, "Autosave delay (ms)", "Idle time before a note is saved", false, Kind::U32(|c| &mut c.autosave_delay_ms, 100, 600_000)),
    item(S::General, "Restore session", "Reopen the tabs of the last session when a workspace opens", true, Kind::Bool(|c| &mut c.restore_session)),
    item(S::General, "Confirm before moving to trash", "Ask first; the dialog's checkbox turns this off", false, Kind::Bool(|c| &mut c.confirm_trash)),
    item(S::General, "Default view mode", "View used when a note is opened", false, Kind::View),
    item(S::General, "Synchronized scrolling", "Scroll source and preview together", false, Kind::Bool(|c| &mut c.sync_scroll)),
    item(S::General, "Open links in a new tab", "Shift flips the choice for one click", false, Kind::Bool(|c| &mut c.links_new_tab)),
    item(S::Interface, "New-tab quotes", "Show a random quote at the bottom of an empty tab", false, Kind::Bool(|c| &mut c.newtab_quotes)),
    item(S::Interface, "Quotes file", "Text file for the new-tab quotes, one per line, optional ' — author'; empty = built-in quotes", false, Kind::Text(|c| &mut c.newtab_quotes_file)),
    item(S::Interface, "New-tab background image", "Image path shown dimmed behind an empty tab; empty = none", false, Kind::Text(|c| &mut c.newtab_image)),
    item(
        S::Interface,
        "UI font size",
        "Text size of the interface in points; UI zoom scales everything on top",
        false,
        Kind::F32(|c| &mut c.ui_font_size, 8.0, 32.0),
    ),
    item(
        S::Interface,
        "Animations",
        "Master switch: off makes every transition instant, whatever the switches below say",
        false,
        Kind::Bool(|c| &mut c.animations),
    ),
    item(
        S::Interface,
        "Animation speed",
        "Higher makes every transition faster, lower slower (1 = default)",
        false,
        Kind::F32(|c| &mut c.anim_speed, 0.5, 3.0),
    ),
    item(S::Interface, "Animations: menus", "Dropdown arrows and the fade-in of menus", false, Kind::Bool(|c| &mut c.anim_menus)),
    item(
        S::Interface,
        "Animations: toolbar buttons",
        "The toolbar's view buttons gliding into one button when the window is narrow",
        false,
        Kind::Bool(|c| &mut c.anim_collapse),
    ),
    item(S::Interface, "Animations: sidebar slide", "The sidebar sliding out and in with Ctrl+B", false, Kind::Bool(|c| &mut c.anim_slide)),
    item(S::Interface, "Animations: file tree", "Folders opening and closing, with the arrow turning", false, Kind::Bool(|c| &mut c.anim_tree)),
    item(S::Interface, "Animations: hover", "Fades of hovered tabs", false, Kind::Bool(|c| &mut c.anim_hover)),
    item(S::Interface, "Animations: zoom", "The interface growing or shrinking to a new UI zoom", false, Kind::Bool(|c| &mut c.anim_zoom)),
    item(S::Interface, "Animations: caret", "The text caret gliding to its new place and fading as it blinks", false, Kind::Bool(|c| &mut c.anim_caret)),
    item(S::Interface, "Caret glide (ms)", "How long the caret takes to glide to its new place (lower is snappier, scaled by Animation speed)", false, Kind::U32(|c| &mut c.caret_glide_ms, 10, 200)),
    item(S::Interface, "Caret trail", "A smear behind the moving caret, like Zed or Neovide", false, Kind::Bool(|c| &mut c.caret_trail)),
    item(S::Interface, "Trail length (ms)", "How long the smear's tail lags behind the caret (0 = no trail, scaled by Animation speed)", false, Kind::U32(|c| &mut c.caret_trail_ms, 0, 300)),
    item(S::Interface, "Trail opacity", "How solid the smear is next to the caret", false, Kind::F32(|c| &mut c.caret_trail_opacity, 0.1, 1.0)),
    item(S::Interface, "Trail width (fraction of caret height)", "Thickness of the smear's tail relative to the caret height, 0 to 1", false, Kind::F32(|c| &mut c.caret_trail_width, 0.0, 1.0)),
    item(
        S::Interface,
        "Custom title bar",
        "The menu bar becomes the window title bar, without the system frame. Applies after a restart",
        false,
        Kind::Bool(|c| &mut c.custom_title_bar),
    ),
    item(S::Interface, "Status bar: word and character count", "Shown for the active note", false, Kind::Bool(|c| &mut c.status_counts)),
    item(S::Interface, "Status bar: cursor position", "Line and column", false, Kind::Bool(|c| &mut c.status_position)),
    item(S::Interface, "Status bar: save state", "Saved, Unsaved, Draft, Read-only", false, Kind::Bool(|c| &mut c.status_badge)),
    item(S::Interface, "Status bar: note count", "Number of indexed notes (indexing progress always shows)", false, Kind::Bool(|c| &mut c.status_notes)),
    item(S::Interface, "Status bar: workspace name", "The workspace button; the Workspace menu stays", false, Kind::Bool(|c| &mut c.status_workspace)),
    item(
        S::Interface,
        "Toolbar: view mode buttons",
        "Source, Split and Preview; the View menu and the palette stay",
        false,
        Kind::Bool(|c| &mut c.toolbar_view_buttons),
    ),
    item(S::Interface, "Toolbar: sync button", "Synchronized scrolling; the View menu and the palette stay", false, Kind::Bool(|c| &mut c.toolbar_sync_button)),
    item(
        S::Interface,
        "Sidebar views",
        "Order and visibility of the views in the sidebar header; hidden views stay in the palette and the View menu, Files cannot be hidden",
        false,
        Kind::SidebarViews,
    ),
    item(S::Interface, "Toolbar: new tab button", "The + next to the tabs; Ctrl+T stays", false, Kind::Bool(|c| &mut c.toolbar_new_tab)),
    item(S::Editor, "Editor font size", "Points", false, Kind::F32(|c| &mut c.editor_font_size, 6.0, 60.0)),
    item(S::Editor, "Editor font", "Families that ship with the app", false, Kind::Choice(|c| &mut c.editor_font, EDITOR_FONTS)),
    item(
        S::Editor,
        "Editor font file",
        "Optional TTF or OTF file used instead; read only when set. Bold and italic use the same file",
        false,
        Kind::Text(|c| &mut c.editor_font_file),
    ),
    item(S::Editor, "Wrap long lines", "Off gives a horizontal scroll bar", false, Kind::Bool(|c| &mut c.editor_wrap)),
    item(S::Editor, "Show line numbers", "A gutter left of the source editor", false, Kind::Bool(|c| &mut c.show_line_numbers)),
    item(S::Editor, "Auto-close brackets and quotes", "Typing ( [ { \" ' ` or $ adds the closing one", false, Kind::Bool(|c| &mut c.auto_close_brackets)),
    item(
        S::Editor,
        "Highlighting size limit (KiB)",
        "Larger notes are highlighted only around the viewport (or not at all, see below)",
        false,
        Kind::U32(|c| &mut c.highlight_limit_kb, 1, 100_000),
    ),
    item(
        S::Editor,
        "Highlight the visible part of big notes",
        "Notes above the size limit: highlighted around the viewport only (Markdown). Off shows them as plain text",
        false,
        Kind::Bool(|c| &mut c.highlight_big_partial),
    ),
    item(S::Preview, "Preview font size", "Points", false, Kind::F32(|c| &mut c.preview_font_size, 6.0, 60.0)),
    item(S::Preview, "Preview full width", "Markdown preview fills the pane instead of a 900 px column", false, Kind::Bool(|c| &mut c.preview_full_width)),
    item(
        S::Preview,
        "Embeds",
        "How ![[note]] shows: inline, collapsed to its title row (click to expand), or as a link only",
        false,
        Kind::Choice(|c| &mut c.embed_mode, EMBED_MODES),
    ),
    item(
        S::Preview,
        "Page preview on hover",
        "Hovering a link to a note shows that note in a popup: only while Ctrl is held, always, or never",
        false,
        Kind::Choice(|c| &mut c.page_preview, PAGE_PREVIEWS),
    ),
    item(S::Preview, "Maximum embed depth", "Embeds inside embeds are drawn down to this depth", false, Kind::U32(|c| &mut c.embed_max_depth, 1, 8)),
    item(S::Preview, "Preview font", "Families that ship with the app", false, Kind::Choice(|c| &mut c.preview_font, PREVIEW_FONTS)),
    item(
        S::Preview,
        "Preview font file",
        "Optional TTF or OTF file used instead; read only when set. Bold and italic use the same file",
        false,
        Kind::Text(|c| &mut c.preview_font_file),
    ),
    item(S::Preview, "Render math in Markdown", "Typst math in $...$ and $$...$$", false, Kind::Bool(|c| &mut c.math_in_markdown)),
    #[cfg(feature = "mermaid")]
    item(S::Preview, "Render Mermaid diagrams", "```mermaid blocks as images; off shows the code", false, Kind::Bool(|c| &mut c.render_mermaid)),
    #[cfg(feature = "mermaid")]
    item(
        S::Preview,
        "Re-render Mermaid diagrams",
        "While typing: after a short pause. On save: the last picture stays until the note is saved (autosave counts). A syntax error keeps the last picture and shows the message below it",
        false,
        Kind::Choice(|c| &mut c.mermaid_render, MERMAID_RENDER),
    ),
    item(
        S::Preview,
        "Math syntax",
        "Auto detects LaTeX or Typst per formula; inside $...$, $$...$$ and ```math; .typ notes stay Typst",
        false,
        Kind::MathSyntax,
    ),
    item(S::Colors, "Accent", "Selected rows, selection and quote bar", false, Kind::Color("accent")),
    item(S::Colors, "Editor background", "Background of the editor and of text fields", false, Kind::Color("editor_bg")),
    item(S::Colors, "Editor text", "Plain text in the editor", false, Kind::Color("editor_text")),
    item(S::Colors, "Link colour", "Links in the editor, the preview and the interface", false, Kind::Color("link")),
    item(S::Colors, "Heading colour", "Headings in the editor and the preview", false, Kind::Color("heading")),
    item(S::Colors, "Emphasis colour", "Italic text in the editor", false, Kind::Color("emphasis")),
    item(S::Colors, "Strong colour", "Bold text in the editor", false, Kind::Color("strong")),
    item(S::Colors, "Code colour", "Inline code and fences in the editor", false, Kind::Color("code")),
    item(S::Colors, "Quote colour", "Block quotes in the editor", false, Kind::Color("quote")),
    item(S::Colors, "Markup colour", "Markup punctuation in the editor", false, Kind::Color("marker")),
    item(S::Colors, "Comment colour", "Comments in the editor", false, Kind::Color("comment")),
    item(S::Colors, "Keyword colour", "Keywords in the editor", false, Kind::Color("keyword")),
    item(S::Colors, "String colour", "Strings in the editor", false, Kind::Color("string")),
    item(S::Colors, "Number colour", "Numbers in the editor", false, Kind::Color("number")),
    item(S::Colors, "Tag colour", "Tags in the editor", false, Kind::Color("tag")),
    item(S::Colors, "Math colour", "Math in the editor", false, Kind::Color("math")),
    item(S::Typst, "Symbol panel insert mode", "What clicking a symbol inserts", false, Kind::SymbolMode),
    item(S::Typst, "Use system fonts", "Also load installed fonts for Typst", true, Kind::Bool(|c| &mut c.typst_system_fonts)),
    item(
        S::Files,
        "Update links on rename",
        "Rewrite links to a note or folder that is renamed or moved",
        false,
        Kind::Choice(|c| &mut c.update_links, UPDATE_LINKS),
    ),
    item(S::Files, "Attachment location", "Where pasted images are saved", false, Kind::Choice(|c| &mut c.attachment_mode, ATTACHMENT_MODES)),
    item(
        S::Files,
        "Attachment subfolder name",
        "Used by \"Subfolder of the note's folder\"; default assets",
        false,
        Kind::Text(|c| &mut c.attachment_subfolder),
    ),
    item(S::Files, "Attachment specific folder", "Used by \"Specific folder\": a path on disk", false, Kind::Text(|c| &mut c.attachment_folder)),
    item(S::Files, "Show hidden files", "Dot files and folders in the tree", false, Kind::Bool(|c| &mut c.show_hidden_files)),
    item(S::Files, "Show non-note files", "Files that are not Markdown, Typst or images", false, Kind::Bool(|c| &mut c.show_other_files)),
    item(S::Files, "Folder notes", "Clicking a folder with a note named like it (Foo/Foo.md) opens that note", false, Kind::Bool(|c| &mut c.folder_notes)),
    item(S::Files, "Hide folder notes", "Leave the folder note out of its folder's rows", false, Kind::Bool(|c| &mut c.folder_note_hide)),
    item(S::Files, "Exclude patterns", "One glob per line: node_modules, *.bak, drafts/**", false, Kind::Globs),
    #[cfg(feature = "kanban")]
    item(
        S::Modules,
        "Kanban boards",
        "Notes with front matter kanban-plugin show as a board in Preview; off shows them as plain Markdown",
        false,
        Kind::Bool(|c| &mut c.module_kanban),
    ),
    #[cfg(feature = "codehl")]
    item(
        S::Modules,
        "Code highlighting",
        "Colours fenced code blocks in Preview by language (Rust, Bash, and many more); off shows them as plain text",
        false,
        Kind::Bool(|c| &mut c.module_codehl),
    ),
    item(
        S::Modules,
        "Ligatures",
        "Arrows and operators as one glyph: with a ligature font such as Lilex in the editor, and as Unicode glyphs in preview text; off draws every character on its own",
        false,
        Kind::Bool(|c| &mut c.module_ligatures),
    ),
    item(S::Modules, "    Ligatures in code", "Ligatures: also in code spans and code blocks (editor and preview; the preview needs the editor font to ligate)", false, Kind::Bool(|c| &mut c.ligatures_in_code)),
    item(S::Modules, "    Ligatures in preview", "Ligatures: show -> => <= >= != <-> <=> in preview text as arrows and operators; copying gives the typed characters", false, Kind::Bool(|c| &mut c.ligatures_in_preview)),
    #[cfg(feature = "git")]
    item(
        S::Modules,
        "Git integration",
        "Show the active branch and dirty count in the status bar, file status badges in the tree, and diff markers in the editor gutter (uses system git)",
        false,
        Kind::Bool(|c| &mut c.module_git),
    ),
    #[cfg(feature = "calendar")]
    item(
        S::Modules,
        "Calendar",
        "Month grid in the sidebar; clicking a day opens or creates its daily note (command: Open today's daily note)",
        false,
        Kind::Bool(|c| &mut c.module_calendar),
    ),
    #[cfg(feature = "calendar")]
    item(S::Modules, "    Daily notes folder", "Calendar: folder under the first workspace folder; empty = the folder itself", false, Kind::Text(|c| &mut c.daily_folder)),
    #[cfg(feature = "calendar")]
    item(S::Modules, "    Daily note name", "Calendar: file name without .md; YYYY, MM and DD stand for year, month and day (all three needed; / makes subfolders)", false, Kind::Text(|c| &mut c.daily_format)),
    #[cfg(feature = "calendar")]
    item(S::Modules, "    Daily note template", "Calendar: note whose text starts a new daily note (path under the first workspace folder); empty = a heading with the date", false, Kind::Text(|c| &mut c.daily_template)),
];

/// What clicking a symbol in the symbol panel inserts (setting `symbol_insert_mode`; the panel can change it too).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum InsertMode {
    /// `.typ`: the name in math, `#sym.name` elsewhere. Markdown: the name inside `$...$`, the character elsewhere.
    #[default]
    Auto,
    Name,
    SymName,
    Character,
}

/// Default size above which the editor shows text without syntax highlighting.
pub const DEFAULT_HIGHLIGHT_LIMIT: usize = 300 * 1024;
static HIGHLIGHT_LIMIT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(DEFAULT_HIGHLIGHT_LIMIT);

/// Size in bytes above which highlighting is skipped (setting `highlight_limit_kb`, applied by `apply`).
pub fn highlight_limit() -> usize {
    HIGHLIGHT_LIMIT.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_highlight_limit(bytes: usize) {
    HIGHLIGHT_LIMIT.store(bytes, std::sync::atomic::Ordering::Relaxed);
}

pub const SYMBOL_MODES: [(&str, &str); 4] = [("auto", "Auto"), ("name", "Name"), ("sym", "#sym.name"), ("char", "Character")];

#[derive(Clone, PartialEq, Default)]
pub(crate) struct Applied {
    show_hidden: bool,
    show_other: bool,
    folder_notes: bool,
    folder_note_hide: bool,
    excludes: Vec<String>,
    symbol_mode: String,
    highlight_limit_kb: u32,
    highlight_big_partial: bool,
    math_syntax: MathSyntax,
    module_kanban: bool,
    module_codehl: bool,
    module_git: bool,
    module_calendar: bool,
    // Derived by the frontend's theme (colours, fonts, text size): any change bumps `settings_gen`.
    ui_font_size: u32,
    /// Master switch, the six group switches, and the speed multiplier.
    anim: [bool; 8],
    anim_speed: u32,
    fonts: [String; 4],
    ligatures: [bool; 3],
    colors: [std::collections::BTreeMap<String, String>; 2],
    theme_file: String,
    professional: bool,
}

impl Applied {
    fn of(c: &AppConfig) -> Applied {
        Applied {
            show_hidden: c.show_hidden_files,
            show_other: c.show_other_files,
            folder_notes: c.folder_notes,
            folder_note_hide: c.folder_note_hide,
            excludes: c.exclude_globs.clone(),
            symbol_mode: c.symbol_insert_mode.clone(),
            highlight_limit_kb: c.highlight_limit_kb,
            highlight_big_partial: c.highlight_big_partial,
            math_syntax: c.math_syntax,
            module_kanban: c.module_kanban,
            module_codehl: c.module_codehl,
            module_git: c.module_git,
            module_calendar: c.module_calendar,
            ui_font_size: c.ui_font_size.to_bits(),
            anim: [c.animations, c.anim_menus, c.anim_collapse, c.anim_slide, c.anim_tree, c.anim_hover, c.anim_zoom, c.anim_caret],
            anim_speed: c.anim_speed.to_bits(),
            fonts: [c.editor_font.clone(), c.editor_font_file.clone(), c.preview_font.clone(), c.preview_font_file.clone()],
            ligatures: [c.module_ligatures, c.ligatures_in_code, c.ligatures_in_preview],
            colors: [c.colors_dark.clone(), c.colors_light.clone()],
            theme_file: c.theme_file.clone(),
            professional: c.professional,
        }
    }
}

#[derive(Default)]
pub struct SettingsUi {
    pub open: bool,
    pub section: Section,
    pub search: String,
    pub kb_filter: String,
    /// Entry index being recorded.
    pub recording: Option<usize>,
    /// One-line message about the last recorded binding (rejections, conflicts).
    pub note: String,
    pub(crate) applied: Applied,
    pub globs_text: Option<String>,
    /// Set when the user asks for Settings while it is already open: the frontend raises and focuses the window.
    pub focus_request: bool,
    /// The chosen theme file, read when the choice changes (and at startup only when one is chosen).
    pub theme: Option<iron_sheets_core::themes::ThemeFile>,
    /// (id, name) of the themes, read when the theme choice opens and dropped when it closes.
    pub theme_list: Option<Vec<(String, String)>>,
}

impl SettingsUi {
    pub fn matches(&self, it: &Item) -> bool {
        self.matches_q(&self.search.trim().to_lowercase(), it)
    }

    /// `matches` with the lowered query already computed (loops lower it once, not per item).
    fn matches_q(&self, q: &str, it: &Item) -> bool {
        q.is_empty() || it.label.to_lowercase().contains(q) || it.help.to_lowercase().contains(q) || it.section.title().to_lowercase().contains(q)
    }
}

/// Number of option rows that match a search (for tests and the search summary).
pub fn matching_items(query: &str) -> usize {
    let s = SettingsUi { search: query.to_string(), ..Default::default() };
    let q = query.trim().to_lowercase();
    ITEMS.iter().filter(|i| s.matches_q(&q, i)).count()
}

pub fn symbol_mode_from(s: &str) -> usize {
    SYMBOL_MODES.iter().position(|(v, _)| *v == s).unwrap_or(0)
}

/// Push config values that live outside `AppConfig` readers (tree filter, symbol panel, indexer).
/// Called at startup and after each change; cheap when nothing changed.
pub fn apply<U>(state: &mut AppState<U>, force: bool) {
    apply_inner(&mut state.core_mut(), force);
}

pub(crate) fn apply_inner(state: &mut CoreMut<'_>, force: bool) {
    let now = Applied::of(state.config);
    let old = std::mem::replace(&mut state.settings.applied, now.clone());
    if !force && old == now {
        return;
    }
    if force
        || old.ui_font_size != now.ui_font_size
        || old.anim != now.anim
        || old.anim_speed != now.anim_speed
        || old.fonts != now.fonts
        || old.ligatures != now.ligatures
        || old.colors != now.colors
        || old.theme_file != now.theme_file
        || old.professional != now.professional
    {
        // The frontend rebuilds its derived values (visuals, fonts, colours, layout caches) once for this bump.
        *state.settings_gen += 1;
    }
    if force || old.theme_file != now.theme_file {
        state.settings.theme = None;
        if !now.theme_file.is_empty() {
            match iron_sheets_core::themes::load(state.dirs, &now.theme_file) {
                Ok(t) => state.settings.theme = Some(t),
                // Falls back to the base theme (`config.theme`).
                Err(e) => state.status.error(format!("Theme: {e}")),
            }
        }
    }
    set_highlight_limit(now.highlight_limit_kb as usize * 1024);
    crate::docs::set_highlight_big_partial(now.highlight_big_partial);
    if force || old.math_syntax != now.math_syntax {
        state.math.set_syntax(now.math_syntax);
    }
    if force || old.module_kanban != now.module_kanban || old.module_codehl != now.module_codehl || old.module_git != now.module_git || old.module_calendar != now.module_calendar || old.ligatures[0] != now.ligatures[0] {
        let cx = &mut crate::modules::ModuleCx { waker: state.waker, commands: state.commands };
        state.modules.sync(crate::modules::DEFS, state.config, cx);
    }
    state.tree.set_show_hidden(now.show_hidden);
    state.tree.set_folder_notes(now.folder_notes, now.folder_note_hide);
    // Also when the module was just switched on: it starts without the patterns.
    #[cfg(feature = "git")]
    if force || old.module_git != now.module_git || old.excludes != now.excludes {
        if let Some(g) = state.modules.get_mut::<crate::git::GitModule>(crate::modules::GIT) {
            g.set_excludes(&now.excludes);
        }
    }
    if force || old.show_other != now.show_other || old.excludes != now.excludes {
        let excludes = ExcludeSet::new(&now.excludes);
        state.tree.set_filter(now.show_other, excludes.clone());
        if force || old.excludes != now.excludes {
            state.tree.set_filter_sig(crate::state::globs_sig(&now.excludes));
        }
        if !force && old.excludes != now.excludes {
            if let Some(w) = state.watcher.as_mut() {
                w.set_excludes(excludes.clone());
            }
            // The indexer filters while walking: respawn it under the new patterns without tearing down the tree or watcher.
            state.restart_indexer(excludes);
            state.flush_tree_cache_async();
        }
    }
    *state.symbol_mode = [InsertMode::Auto, InsertMode::Name, InsertMode::SymName, InsertMode::Character][symbol_mode_from(&now.symbol_mode)];
}

/// Replace the bindings of one entry; stores only differences from the defaults.
pub fn set_bindings<U>(state: &mut AppState<U>, entry: usize, bindings: Vec<Shortcut>) {
    set_bindings_inner(&mut state.core_mut(), entry, bindings);
}

fn set_bindings_inner(state: &mut CoreMut<'_>, entry: usize, bindings: Vec<Shortcut>) {
    let id = state.keymap.entries[entry].id;
    if bindings.as_slice() == state.keymap.entries[entry].default.as_slice() {
        state.config.keybindings.remove(id);
    } else {
        state.config.keybindings.insert(id.to_string(), bindings.iter().map(format_shortcut).collect());
    }
    *state.keymap = Keymap::build(&state.config.keybindings);
    state.touch_config();
}

pub fn reset_all_bindings<U>(state: &mut AppState<U>) {
    reset_all_bindings_inner(&mut state.core_mut());
}

fn reset_all_bindings_inner(state: &mut CoreMut<'_>) {
    state.config.keybindings.clear();
    *state.keymap = Keymap::build(&state.config.keybindings);
    state.touch_config();
}

/// The sidebar views in header order with their visibility, from `AppConfig::sidebar_views`. Unknown ids and repeats
/// are ignored, views the list lacks come last and visible, Files is always visible.
pub fn sidebar_layout(list: &[String]) -> Vec<(SidebarView, bool)> {
    let mut out: Vec<(SidebarView, bool)> = Vec::with_capacity(SidebarView::ALL.len());
    for e in list {
        let (id, shown) = e.strip_prefix('-').map_or((e.as_str(), true), |id| (id, false));
        if let Some(&v) = SidebarView::ALL.iter().find(|v| v.id() == id) {
            if !out.iter().any(|o| o.0 == v) {
                out.push((v, shown || v == SidebarView::Files));
            }
        }
    }
    for &v in SidebarView::ALL {
        if !out.iter().any(|o| o.0 == v) {
            out.push((v, true));
        }
    }
    out
}

fn store_layout(l: &[(SidebarView, bool)]) -> Vec<String> {
    l.iter().map(|&(v, shown)| if shown { v.id().to_string() } else { format!("-{}", v.id()) }).collect()
}

/// Move `v` one place up (`-1`) or down (`1`) in the header; the list is written out in full.
pub fn move_sidebar_view(list: &mut Vec<String>, v: SidebarView, delta: isize) {
    let mut l = sidebar_layout(list);
    if let Some(i) = l.iter().position(|o| o.0 == v) {
        if let Some(j) = i.checked_add_signed(delta).filter(|&j| j < l.len()) {
            l.swap(i, j);
        }
    }
    *list = store_layout(&l);
}

/// Show or hide `v` in the header (Files stays).
pub fn set_sidebar_view_shown(list: &mut Vec<String>, v: SidebarView, shown: bool) {
    let mut l = sidebar_layout(list);
    if let Some(o) = l.iter_mut().find(|o| o.0 == v) {
        o.1 = shown || v == SidebarView::Files;
    }
    *list = store_layout(&l);
}

/// Reset every option and shortcut to its default (window geometry, workspace and sidebar stay).
pub fn reset_all<U>(state: &mut AppState<U>) {
    reset_all_inner(&mut state.core_mut());
}

fn reset_all_inner(state: &mut CoreMut<'_>) {
    let keep = AppConfig {
        last_workspace: state.config.last_workspace.clone(),
        window: state.config.window,
        settings_window: state.config.settings_window,
        sidebar_visible: state.config.sidebar_visible,
        sidebar_width: state.config.sidebar_width,
        ..Default::default()
    };
    *state.config = keep;
    *state.keymap = Keymap::build(&state.config.keybindings);
    state.touch_config();
}

/// Apply a captured key press while recording (the toolkit adapter produces `captured`). Returns true when recording
/// is active, i.e. the press must not be dispatched as a shortcut.
pub fn handle_captured<U>(state: &mut AppState<U>, captured: Captured) -> bool {
    handle_captured_inner(&mut state.core_mut(), captured)
}

fn handle_captured_inner(state: &mut CoreMut<'_>, captured: Captured) -> bool {
    let Some(entry) = state.settings.recording else { return false };
    match captured {
        Captured::Nothing => {}
        Captured::Cancelled => {
            state.settings.recording = None;
            state.settings.note = "Recording cancelled".into();
        }
        Captured::Key(sc) => {
            state.settings.recording = None;
            match bindable(&sc) {
                Err(m) => state.settings.note = format!("{} not set: {m}", format_shortcut(&sc)),
                Ok(()) => {
                    let mut v = state.keymap.bindings[entry].clone();
                    if !v.contains(&sc) {
                        v.push(sc);
                    }
                    let others = state.keymap.others_with(&sc, entry);
                    let text = format_shortcut(&sc);
                    set_bindings_inner(state, entry, v);
                    state.settings.note = if others.is_empty() {
                        format!("{text} set for {}", state.keymap.entries[entry].title)
                    } else {
                        let names: Vec<&str> = others.iter().map(|&i| state.keymap.entries[i].title).collect();
                        format!("Conflict: {text} is also used by {}", names.join(", "))
                    };
                }
            }
        }
    }
    true
}

/// Indices of the keymap entries shown for a filter (title, id or any bound shortcut contains it).
pub fn keyboard_rows(keymap: &Keymap, filter: &str) -> Vec<usize> {
    let q = filter.trim().to_lowercase();
    (0..keymap.entries.len())
        .filter(|&i| {
            let e = &keymap.entries[i];
            q.is_empty()
                || e.title.to_lowercase().contains(&q)
                || e.id.contains(&q)
                || keymap.bindings[i].iter().any(|b| format_shortcut(b).to_lowercase().contains(&q))
        })
        .collect()
}

/// Number of commands whose title contains the search text (the "matching shortcuts" hint).
pub fn matching_commands(query: &str) -> usize {
    let q = query.trim().to_lowercase();
    all_commands().iter().filter(|c| c.title.to_lowercase().contains(&q)).count()
}

/// Options shown for a search text, or for `section` when the search is empty.
pub fn visible_items(search: &SettingsUi, section: Section) -> Vec<&'static Item> {
    let q = search.search.trim().to_lowercase();
    let searching = !q.is_empty();
    ITEMS.iter().filter(|i| if searching { search.matches_q(&q, i) } else { i.section == section }).collect()
}

/// Remove the `binding`-th shortcut of an entry.
pub fn remove_binding<U>(state: &mut AppState<U>, entry: usize, binding: usize) {
    remove_binding_inner(&mut state.core_mut(), entry, binding);
}

fn remove_binding_inner(state: &mut CoreMut<'_>, entry: usize, binding: usize) {
    let mut v = state.keymap.bindings[entry].clone();
    if binding < v.len() {
        v.remove(binding);
    }
    set_bindings_inner(state, entry, v);
}

/// Restore an entry's default shortcut(s).
pub fn reset_binding<U>(state: &mut AppState<U>, entry: usize) {
    reset_binding_inner(&mut state.core_mut(), entry);
}

fn reset_binding_inner(state: &mut CoreMut<'_>, entry: usize) {
    let d: Vec<Shortcut> = state.keymap.entries[entry].default.into_iter().collect();
    set_bindings_inner(state, entry, d);
}

/// Start or stop recording a binding for an entry.
pub fn toggle_recording(ui: &mut SettingsUi, entry: usize) {
    ui.recording = if ui.recording == Some(entry) { None } else { Some(entry) };
    ui.note.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{Key, Mods};

    #[test]
    fn rows_of_compiled_out_features_are_absent() {
        let has = |label: &str| ITEMS.iter().any(|i| i.label == label);
        assert_eq!(has("Code highlighting"), cfg!(feature = "codehl"));
        assert_eq!(has("Render Mermaid diagrams"), cfg!(feature = "mermaid"));
        assert_eq!(has("Re-render Mermaid diagrams"), cfg!(feature = "mermaid"));
    }

    #[test]
    fn sidebar_layout_defaults_ignores_unknown_and_appends_missing() {
        let all: Vec<_> = SidebarView::ALL.iter().map(|&v| (v, true)).collect();
        assert_eq!(sidebar_layout(&[]), all);
        let l = sidebar_layout(&["outline".into(), "bogus".into(), "-search".into(), "outline".into(), "-files".into()]);
        assert_eq!(l[0], (SidebarView::Outline, true), "first, a repeat is ignored");
        assert_eq!(l[1], (SidebarView::Search, false));
        assert_eq!(l[2], (SidebarView::Files, true), "Files cannot be hidden");
        assert_eq!(l.len(), all.len(), "the missing views are appended");
        assert!(l[3..].iter().all(|o| o.1));
    }

    #[test]
    fn sidebar_views_move_and_hide() {
        let mut l: Vec<String> = Vec::new();
        move_sidebar_view(&mut l, SidebarView::Search, -1);
        assert_eq!(sidebar_layout(&l)[0].0, SidebarView::Search);
        move_sidebar_view(&mut l, SidebarView::Search, -1);
        assert_eq!(sidebar_layout(&l)[0].0, SidebarView::Search, "stays at the front");
        move_sidebar_view(&mut l, SidebarView::Search, 1);
        assert_eq!(sidebar_layout(&l)[1].0, SidebarView::Search);
        set_sidebar_view_shown(&mut l, SidebarView::Outline, false);
        assert!(sidebar_layout(&l).contains(&(SidebarView::Outline, false)));
        set_sidebar_view_shown(&mut l, SidebarView::Files, false);
        assert!(sidebar_layout(&l).contains(&(SidebarView::Files, true)));
        set_sidebar_view_shown(&mut l, SidebarView::Outline, true);
        assert!(sidebar_layout(&l).iter().all(|o| o.1));
    }

    #[test]
    fn search_matches_label_help_and_section() {
        assert_eq!(matching_items("zzzz"), 0);
        assert!(matching_items("") >= 18);
        assert!(matching_items("autosave") >= 2);
        assert!(matching_items("Typst") >= 2);
    }

    #[test]
    fn every_item_has_a_label_and_valid_range() {
        for it in ITEMS {
            assert!(!it.label.is_empty());
            match &it.kind {
                Kind::F32(_, lo, hi) => assert!(lo < hi, "{}", it.label),
                Kind::U32(_, lo, hi) => assert!(lo < hi, "{}", it.label),
                _ => {}
            }
        }
        let mut labels: Vec<_> = ITEMS.iter().map(|i| i.label).collect();
        labels.sort();
        labels.dedup();
        assert_eq!(labels.len(), ITEMS.len(), "duplicate labels");
        // Every section except the keyboard one has options.
        for s in Section::ALL {
            if s != Section::Keyboard {
                assert!(ITEMS.iter().any(|i| i.section == s), "{s:?}");
            }
        }
    }

    #[test]
    fn highlight_limit_alone_is_a_change() {
        let a = AppConfig::default();
        let b = AppConfig { highlight_limit_kb: a.highlight_limit_kb + 1, ..AppConfig::default() };
        assert!(Applied::of(&a) != Applied::of(&b));
    }

    #[test]
    fn highlight_limit_applies_when_it_is_the_only_change() {
        // The limit is process-global and other tests build states (which reset it), so retry on a lost race.
        let (_dir, mut st) = crate::test_support::state_with(&[("a.md", "x")]);
        apply(&mut st, true);
        let mut ok = false;
        for kb in 100..200u32 {
            st.config.highlight_limit_kb = kb;
            apply(&mut st, false);
            if highlight_limit() == kb as usize * 1024 {
                ok = true;
                break;
            }
        }
        assert!(ok, "changing only highlight_limit_kb must reach the global limit");
    }

    #[test]
    fn math_syntax_is_an_option_and_applies_live() {
        use std::sync::{Arc, Mutex};
        assert!(ITEMS.iter().any(|i| matches!(i.kind, Kind::MathSyntax)));
        let a = AppConfig::default();
        let b = AppConfig { math_syntax: MathSyntax::Latex, ..AppConfig::default() };
        assert!(Applied::of(&a) != Applied::of(&b));

        struct Rec(Arc<Mutex<MathSyntax>>);
        impl crate::services::MathRenderer for Rec {
            fn math(&self, _: &str, _: bool, _: f32, _: [u8; 4], _: f32) -> crate::services::MathResult {
                crate::services::MathResult::Unavailable
            }
            fn set_syntax(&self, syntax: MathSyntax) {
                *self.0.lock().unwrap() = syntax;
            }
        }
        let seen = Arc::new(Mutex::new(MathSyntax::Typst));
        let (_dir, mut st) = crate::test_support::state_with(&[("a.md", "x")]);
        st.math = Box::new(Rec(seen.clone()));
        apply(&mut st, true);
        assert_eq!(*seen.lock().unwrap(), MathSyntax::Auto);
        st.config.math_syntax = MathSyntax::Latex;
        apply(&mut st, false);
        assert_eq!(*seen.lock().unwrap(), MathSyntax::Latex, "switching the syntax must reach the renderer without a restart");
        st.config.math_syntax = MathSyntax::Typst;
        apply(&mut st, false);
        assert_eq!(*seen.lock().unwrap(), MathSyntax::Typst);
    }

    #[test]
    fn derived_settings_bump_the_generation_once_and_others_do_not() {
        let (_dir, mut st) = crate::test_support::state_with(&[("a.md", "x")]);
        apply(&mut st, true);
        let g = st.settings_gen;
        apply(&mut st, false);
        assert_eq!(st.settings_gen, g, "nothing changed");
        st.config.status_counts = false;
        st.config.toolbar_new_tab = false;
        apply(&mut st, false);
        assert_eq!(st.settings_gen, g, "plain bools are read directly, no rebuild");
        for change in [
            |c: &mut AppConfig| c.editor_font = "hack".into(),
            |c: &mut AppConfig| c.preview_font_file = "x.ttf".into(),
            |c: &mut AppConfig| c.ui_font_size = 15.0,
            |c: &mut AppConfig| {
                c.colors_light.insert("accent".into(), "#112233".into());
            },
        ] {
            let before = st.settings_gen;
            change(&mut st.config);
            apply(&mut st, false);
            assert_eq!(st.settings_gen, before + 1);
            apply(&mut st, false);
            assert_eq!(st.settings_gen, before + 1);
        }
    }

    #[test]
    fn colour_items_match_the_colour_table() {
        let items: Vec<&str> = ITEMS.iter().filter_map(|i| if let Kind::Color(k) = i.kind { Some(k) } else { None }).collect();
        let keys: Vec<&str> = crate::theme_model::COLOR_KEYS.iter().map(|k| k.0).collect();
        assert_eq!(items, keys);
        for (it, k) in ITEMS.iter().filter(|i| matches!(i.kind, Kind::Color(_))).zip(crate::theme_model::COLOR_KEYS) {
            assert_eq!(it.label, k.1);
        }
        assert!(matching_items("status bar") >= 5 && matching_items("font") >= 5 && matching_items("colour") >= 10);
    }

    #[test]
    fn symbol_mode_lookup_falls_back_to_auto() {
        assert_eq!(symbol_mode_from("char"), 3);
        assert_eq!(symbol_mode_from("bogus"), 0);
    }

    #[test]
    fn keyboard_filter_matches_title_id_and_shortcut() {
        let k = Keymap::defaults();
        assert_eq!(keyboard_rows(&k, "").len(), k.entries.len());
        let save = k.index_of("file.save").expect("id");
        assert!(keyboard_rows(&k, "ctrl+s").contains(&save));
        assert!(keyboard_rows(&k, "file.save").contains(&save));
        assert!(keyboard_rows(&k, "zzzz").is_empty());
    }

    #[test]
    fn recording_toggles_and_clears_note() {
        let mut ui = SettingsUi { note: "x".into(), ..Default::default() };
        toggle_recording(&mut ui, 3);
        assert_eq!(ui.recording, Some(3));
        assert!(ui.note.is_empty());
        toggle_recording(&mut ui, 3);
        assert_eq!(ui.recording, None);
    }

    #[test]
    fn unbindable_shortcut_is_rejected_text() {
        assert!(bindable(&Shortcut::new(Mods::NONE, Key::A)).is_err());
    }

    #[test]
    fn exclude_globs_updates_watcher_and_indexer_without_blocking() {
        let (_dir, mut st) = crate::test_support::state_with(&[("keep.md", "ok"), ("drafts/skip.md", "no")]);
        let gen0 = st.runtime_gen;
        apply(&mut st, true);
        st.config.exclude_globs = vec!["drafts".into()];
        apply(&mut st, false);
        assert_eq!(st.runtime_gen, gen0, "tree and watcher stay alive in place");
        assert!(st.indexer.wait_idle(std::time::Duration::from_secs(10)));
        let idx = st.indexer.index();
        let g = idx.read();
        assert!(g.by_path(&st.vault_path("keep.md")).is_some());
        assert!(g.by_path(&st.vault_path("drafts/skip.md")).is_none(), "excluded file dropped from index");
    }

    #[test]
    fn exclude_globs_updates_tree_cache_config_hash_and_persists_cosmetic_edits() {
        let (_dir, mut st) = crate::test_support::state_with(&[("keep.md", "ok"), ("drafts/skip.md", "no")]);
        apply(&mut st, true);
        st.tree.wait_idle();
        st.flush_tree_cache();
        let cache_file = st.store.tree_cache_file(&st.workspace.name);
        assert!(cache_file.exists());

        // Real exclude change updates filter_sig & cache_snapshot().config_hash() immediately (test with workspace.name cleared so flush_tree_cache_async does not consume it).
        let ws_name = std::mem::take(&mut st.workspace.name);
        let (_, snap0) = {
            let mut probe = iron_sheets_core::tree::TreeModel::new(st.workspace.roots.clone(), st.config.show_hidden_files);
            probe.set_expanded(&st.workspace.roots[0], true);
            probe.set_filter_sig(crate::state::globs_sig(&st.config.exclude_globs));
            probe.rows();
            probe.cache_snapshot().expect("baseline snap")
        };
        st.config.exclude_globs = vec!["drafts".into()];
        apply(&mut st, false);
        st.tree.wait_idle();
        let (_, snap1) = st.tree.cache_snapshot().expect("updated snap");
        assert_ne!(snap1.config_hash(), snap0.config_hash(), "apply must update tree cache config_hash");

        // Cosmetic exclude_globs edit (comment added) keeps ExcludeSet equal, updates filter_sig immediately, and flushes cache to disk.
        st.workspace.name = ws_name;
        st.flush_tree_cache();
        st.config.exclude_globs = vec!["drafts".into(), "# cosmetic".into()];
        apply(&mut st, false);
        st.wait_persist_idle();
        assert!(st.tree.cache_snapshot().is_none(), "cosmetic edit flushed asynchronously");

        let mut fresh_new = iron_sheets_core::tree::TreeModel::new(st.workspace.roots.clone(), st.config.show_hidden_files);
        fresh_new.set_filter(st.config.show_other_files, ExcludeSet::new(&st.config.exclude_globs));
        fresh_new.set_filter_sig(crate::state::globs_sig(&st.config.exclude_globs));
        fresh_new.set_expanded(&st.workspace.roots[0], true);
        assert_eq!(fresh_new.seed_from_cache(&cache_file), 1, "persisted cache matches updated filter_sig");

        let mut fresh_old = iron_sheets_core::tree::TreeModel::new(st.workspace.roots.clone(), st.config.show_hidden_files);
        fresh_old.set_filter(st.config.show_other_files, ExcludeSet::new(&["drafts"]));
        fresh_old.set_filter_sig(crate::state::globs_sig(&["drafts".into()]));
        fresh_old.set_expanded(&st.workspace.roots[0], true);
        assert_eq!(fresh_old.seed_from_cache(&cache_file), 0, "old filter_sig no longer matches persisted cache");
    }
}
