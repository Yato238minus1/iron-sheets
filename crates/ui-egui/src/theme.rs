//! egui side of the theme: converts the framework-free `ThemeModel` into egui colours, styles, visuals and fonts.
use crate::theme_model::{EditorColors, PreviewColors, Rgba, ThemeModel, UiColors};
pub use crate::theme_model::{contrast_pairs, contrast_ratio, contrast_report};
use egui::{Color32, FontFamily, FontId};
use iron_sheets_core::config::{AppConfig, ThemePref};
use std::sync::Arc;

/// Style tokens. They are constants (nothing is computed per frame); the colours come from the theme model, which is
/// rebuilt once per `settings_gen`.
pub mod tokens {
    /// Height of a tab and of the buttons next to the tabs.
    pub const TAB_H: f32 = 28.0;
    /// Corner radius of tabs, rows and small controls.
    pub const RADIUS: f32 = 4.0;
    /// Active-tab accent bar thickness.
    pub const TAB_BAR: f32 = 2.0;
    /// Widest tab title (in multiples of the UI font size); longer names get a middle ellipsis.
    pub const TAB_TITLE_EM: f32 = 13.0;
    /// Horizontal space a tab adds around its title: 8 px left, room for the 16 px close button on the right.
    pub const TAB_PAD: f32 = 36.0;
    /// Gap between status bar segments.
    pub const STATUS_GAP: f32 = 12.0;
}

/// Colours of the view-mode segmented control (the selected segment is filled) and of toggle chips such as Sync (an
/// outline and a check box when on), so the two never look alike. Built from the theme model once per `settings_gen`.
#[derive(Debug, Clone, Copy)]
pub struct ControlStyle {
    /// Fill and text of the selected segment.
    pub seg_fill: Color32,
    pub seg_text: Color32,
    /// Frame around the segmented group, and around a toggle that is off.
    pub frame: egui::Stroke,
    /// Outline, check box and text of a toggle that is on.
    pub on_stroke: egui::Stroke,
    pub on_text: Color32,
    /// Fill under the pointer on an unselected segment or an off toggle (the tab strip's hover colour).
    pub hover_fill: Color32,
    /// Fill of the selected segment under the pointer.
    pub seg_hover_fill: Color32,
}

impl ControlStyle {
    pub fn from_model(u: &UiColors) -> ControlStyle {
        ControlStyle {
            seg_fill: color32(u.selection_bg),
            seg_text: color32(u.strong_text),
            frame: egui::Stroke::new(1.0, color32(u.separator)),
            on_stroke: egui::Stroke::new(1.5, color32(u.hyperlink)),
            on_text: color32(u.strong_text),
            hover_fill: color32(u.hovered_bg),
            seg_hover_fill: color32(u.selection_bg).lerp_to_gamma(color32(u.hovered_text), 0.18),
        }
    }
}

/// Size of egui's `Small` text style in points at UI font size 13 (egui's own is 9).
pub const SMALL_TEXT_PT: f32 = 11.0;

pub fn color32(c: Rgba) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), c.a())
}

/// Editor token colours and fonts (used by `editor::highlight`).
#[derive(Debug, Clone, PartialEq)]
pub struct EditorStyle {
    pub font_id: FontId,
    pub text: Color32,
    pub heading: Color32,
    pub emphasis: Color32,
    pub strong: Color32,
    pub code: Color32,
    pub code_bg: Color32,
    pub link: Color32,
    pub quote: Color32,
    /// Markup punctuation (`#`, `*`, `[[`, fences).
    pub marker: Color32,
    pub comment: Color32,
    pub keyword: Color32,
    pub string: Color32,
    pub number: Color32,
    pub tag: Color32,
    pub math: Color32,
    pub selection_bg: Color32,
    pub line_height: f32,
    /// Ligatures: `ligatures` at all, `in_code` also inside code spans and blocks (set by the theme from the settings).
    pub ligatures: bool,
    pub ligatures_in_code: bool,
    /// The editor font can ligate (Lilex, or a font file): otherwise nothing is split.
    pub font_ligates: bool,
}

impl EditorStyle {
    pub fn from_model(m: &EditorColors) -> EditorStyle {
        EditorStyle {
            font_id: FontId::new(m.font_size, FontFamily::Monospace),
            text: color32(m.text),
            heading: color32(m.heading),
            emphasis: color32(m.emphasis),
            strong: color32(m.strong),
            code: color32(m.code),
            code_bg: color32(m.code_bg),
            link: color32(m.link),
            quote: color32(m.quote),
            marker: color32(m.marker),
            comment: color32(m.comment),
            keyword: color32(m.keyword),
            string: color32(m.string),
            number: color32(m.number),
            tag: color32(m.tag),
            math: color32(m.math),
            selection_bg: color32(m.selection_bg),
            line_height: m.line_height,
            ligatures: true,
            ligatures_in_code: false,
            font_ligates: false,
        }
    }
}

/// Colours and sizes for rendered previews.
#[derive(Debug, Clone, PartialEq)]
pub struct PreviewStyle {
    pub body_size: f32,
    pub text: Color32,
    pub heading: Color32,
    pub link: Color32,
    pub quote_bar: Color32,
    pub code_bg: Color32,
    pub rule: Color32,
    pub table_border: Color32,
    pub dark: bool,
    /// Code block colours: comment, string, number, keyword, function, type (the editor's token colours).
    pub syn: [Color32; 6],
    /// Arrows and operators in prose as Unicode glyphs (module "Ligatures" and "Ligatures in preview").
    pub subs: bool,
    /// Family of code spans and blocks: the editor font when it ligates and "Ligatures in code" is on, else Monospace.
    pub code_family: FontFamily,
    /// Code must stay literal although the editor font ligates: bold and italic code use the editor faces.
    pub split_code: bool,
}

/// The editor font can ligate: Lilex (when built in) or a font file.
pub fn font_ligates(c: &AppConfig) -> bool {
    (cfg!(feature = "font-lilex") && c.editor_font == "lilex") || !c.editor_font_file.trim().is_empty()
}

impl PreviewStyle {
    pub fn set_ligatures(&mut self, c: &AppConfig, font_ligates: bool, editor: &FontFamily) {
        self.subs = c.module_ligatures && c.ligatures_in_preview;
        let code = font_ligates && c.module_ligatures && c.ligatures_in_code;
        self.code_family = if code { editor.clone() } else { FontFamily::Monospace };
        self.split_code = font_ligates && !code;
    }

    pub fn code_color(&self, c: iron_sheets_app_core::services::CodeClass) -> Color32 {
        self.syn[c as usize]
    }

    pub fn from_model(m: &PreviewColors, e: &EditorColors) -> PreviewStyle {
        PreviewStyle {
            syn: [e.comment, e.string, e.number, e.keyword, e.link, e.tag].map(color32),
            body_size: m.body_size,
            text: color32(m.text),
            heading: color32(m.heading),
            link: color32(m.link),
            quote_bar: color32(m.quote_bar),
            code_bg: color32(m.code_bg),
            rule: color32(m.rule),
            table_border: color32(m.table_border),
            dark: m.dark,
            subs: false,
            code_family: FontFamily::Monospace,
            split_code: false,
        }
    }
}

/// Notes larger than this zoom without easing: every frame of the transition lays the visible text out again, and with
/// 100 KB in split view a step took 18 ms in `tests/editor_zoom.rs` (20 KB: 9 ms, over a UI-only base of 6 ms).
pub const ZOOM_ANIM_MAX_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone)]
pub struct Theme {
    /// Changes whenever theme, zoom or font sizes change; caches key on it.
    pub generation: u64,
    pub dark: bool,
    pub editor: EditorStyle,
    pub preview: PreviewStyle,
    /// The framework-free source of `editor`, `preview` and the visuals adjustments.
    pub model: ThemeModel,
    pub controls: ControlStyle,
    /// High-contrast colour set (the settings UI may expose a toggle).
    pub high_contrast: bool,
    /// Set by the frame loop each frame: a zoom started now should jump instead of easing (big note, see
    /// `ZOOM_ANIM_MAX_BYTES`, or a preview showing math / diagrams / images, which would be rasterised at every
    /// intermediate scale). Read once, when the target changes.
    pub zoom_instant: bool,
    zoom_to: f32,
    zoom_jump: bool,
    key: Option<(bool, u32, u32, u32, bool, u64)>,
    /// Font choice the context's fonts were built for (`None` until installed).
    fonts: Option<FontChoice>,
    /// Family of the editor text: the named "editor" family once fonts are installed.
    editor_family: FontFamily,
    /// Problems found while building fonts (unreadable font file); the frame loop shows them in the status bar.
    pub font_errors: Vec<String>,
    /// A tiny galley laid out every frame: egui drops its galley cache and glyph atlas on a theme change (the
    /// text options depend on dark mode) or when the atlas fills up, and every galley cached elsewhere then
    /// points into the old atlas and draws nothing. A new `Arc` here means exactly that happened.
    probe: Option<std::sync::Arc<egui::Galley>>,
}

impl Default for Theme {
    fn default() -> Self {
        let model = ThemeModel::build(false, false, 14.0, 15.0, true);
        Theme {
            generation: 0,
            dark: false,
            editor: EditorStyle::from_model(&model.editor),
            preview: PreviewStyle::from_model(&model.preview, &model.editor),
            controls: ControlStyle::from_model(&model.ui),
            model,
            high_contrast: false,
            zoom_instant: false,
            zoom_to: 1.0,
            zoom_jump: false,
            key: None,
            fonts: None,
            editor_family: FontFamily::Monospace,
            font_errors: Vec::new(),
            probe: None,
        }
    }
}

impl Theme {
    /// Apply `config.theme` and `config.ui_zoom` to the context and rebuild styles when anything changed.
    ///
    /// Cheap by construction: every frame this compares a few numbers (`key`). Everything derived from other settings
    /// (colour overrides, fonts, UI text size) is rebuilt only when `settings_gen` (bumped by `settings_model::apply`)
    /// changed, so no config string is read or parsed in an idle frame.
    pub fn refresh(&mut self, ctx: &egui::Context, config: &AppConfig, settings_gen: u64) {
        let pref = match config.theme {
            ThemePref::System => egui::ThemePreference::System,
            ThemePref::Light => egui::ThemePreference::Light,
            ThemePref::Dark => egui::ThemePreference::Dark,
        };
        if ctx.options(|o| o.theme_preference) != pref {
            ctx.set_theme(pref);
        }
        self.high_contrast = config.high_contrast;
        // The zoom eases to its new value over the animation time (instant with the group off).
        let target = config.ui_zoom.clamp(0.5, 3.0);
        if target != self.zoom_to {
            self.zoom_to = target;
            self.zoom_jump = self.zoom_instant;
        }
        let time = if crate::anim::enabled(crate::anim::Group::Zoom) && !self.zoom_jump { ctx.global_style().animation_time } else { 0.0 };
        let zoom = ctx.animate_value_with_time(egui::Id::NULL.with("iron_sheets_ui_zoom"), target, time);
        if (ctx.zoom_factor() - zoom).abs() > 1e-4 {
            ctx.set_zoom_factor(zoom);
        }
        let dark = ctx.global_style().visuals.dark_mode;
        let key = (dark, zoom.to_bits(), config.editor_font_size.to_bits(), config.preview_font_size.to_bits(), self.high_contrast, settings_gen);
        if self.key != Some(key) {
            let gen_changed = self.key.is_none_or(|k| k.5 != settings_gen);
            if self.key.is_none_or(|k| k.4 != self.high_contrast) || gen_changed {
                self.rebuild_styles(ctx, config);
            }
            if gen_changed {
                let want = FontChoice::of(config);
                if self.fonts.as_ref() != Some(&want) {
                    self.font_errors.extend(install_fonts(ctx, &want));
                    self.editor_family = FontFamily::Name("editor".into());
                    self.fonts = Some(want);
                }
            }
            self.key = Some(key);
            self.dark = dark;
            self.model = Self::model_for(config, dark, self.high_contrast);
            self.controls = ControlStyle::from_model(&self.model.ui);
            self.editor = EditorStyle::from_model(&self.model.editor);
            self.editor.font_id.family = self.editor_family.clone();
            (self.editor.ligatures, self.editor.ligatures_in_code) = (config.module_ligatures, config.ligatures_in_code);
            self.editor.font_ligates = font_ligates(config);
            self.preview = PreviewStyle::from_model(&self.model.preview, &self.model.editor);
            self.preview.set_ligatures(config, self.editor.font_ligates, &self.editor_family);
            self.generation += 1;
        }
        // egui resets the atlas in the pass after a theme change, so this must run every frame. Proportional: the first frame needs that
        // face anyway; the monospace one (editor) is loaded only when a note is shown.
        let probe = ctx.fonts_mut(|f| f.layout_no_wrap("m".into(), egui::FontId::proportional(12.0), Color32::WHITE));
        if self.probe.as_ref().is_some_and(|old| !std::sync::Arc::ptr_eq(old, &probe)) {
            self.generation += 1;
        }
        self.probe = Some(probe);
    }

    /// Record fonts installed before the first frame (`App::new`), so the first `refresh` does not install them again.
    pub fn fonts_installed(&mut self, choice: FontChoice, errors: Vec<String>) {
        self.editor_family = FontFamily::Name("editor".into());
        self.fonts = Some(choice);
        self.font_errors.extend(errors);
    }

    /// The model for one variant with the user's overrides on top (stock, then accessibility adjustments, then
    /// high contrast, then the overrides).
    pub fn model_for(config: &AppConfig, dark: bool, high_contrast: bool) -> ThemeModel {
        let mut m = ThemeModel::build(dark, high_contrast, config.editor_font_size.clamp(6.0, 60.0), config.preview_font_size.clamp(6.0, 60.0), true);
        m.apply_overrides(if dark { &config.colors_dark } else { &config.colors_light });
        m
    }

    /// Visuals (both variants) and text sizes of the context.
    fn rebuild_styles(&self, ctx: &egui::Context, config: &AppConfig) {
        let hc = self.high_contrast;
        let scale = config.ui_font_size.clamp(8.0, 32.0) / 13.0;
        ctx.all_styles_mut(|st| {
            let dark = st.visuals.dark_mode;
            let m = Self::model_for(config, dark, hc);
            st.visuals = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
            apply_ui_colors(&mut st.visuals, &m.ui);
            st.text_styles = egui::style::default_text_styles();
            // Small text (help lines, hints) is 9 pt in egui: too small to read; 11 pt is the floor the review asks for.
            if let Some(f) = st.text_styles.get_mut(&egui::TextStyle::Small) {
                f.size = SMALL_TEXT_PT;
            }
            if (scale - 1.0).abs() > 1e-3 {
                for f in st.text_styles.values_mut() {
                    f.size *= scale;
                }
            }
            // One duration for every transition; 0 with "Animations" off also silences egui's own (collapsing headers, scrollbars).
            st.animation_time = crate::anim::duration(config.animations, config.anim_speed);
        });
        crate::anim::set_groups(config);
    }
}

/// Which font families the context's fonts were built for: the family ids and optional font files of the settings.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FontChoice {
    pub editor: String,
    pub editor_file: String,
    pub preview: String,
    pub preview_file: String,
}

impl FontChoice {
    pub fn of(c: &AppConfig) -> FontChoice {
        FontChoice {
            editor: c.editor_font.clone(),
            editor_file: c.editor_font_file.clone(),
            preview: c.preview_font.clone(),
            preview_file: c.preview_font_file.clone(),
        }
    }
}

/// Font families registered by [`install_fonts`].
#[derive(Debug, Clone)]
pub struct Families {
    pub mono_bold: FontFamily,
    pub mono_italic: FontFamily,
    pub mono_bold_italic: FontFamily,
    pub preview: FontFamily,
    pub preview_bold: FontFamily,
    pub preview_italic: FontFamily,
    pub preview_bold_italic: FontFamily,
    pub math: FontFamily,
}

pub fn families() -> Families {
    let n = |s: &str| FontFamily::Name(s.into());
    Families {
        mono_bold: n("mono-bold"),
        mono_italic: n("mono-italic"),
        mono_bold_italic: n("mono-bold-italic"),
        preview: n("preview"),
        preview_bold: n("preview-bold"),
        preview_italic: n("preview-italic"),
        preview_bold_italic: n("preview-bold-italic"),
        math: n("math"),
    }
}

/// Largest font file accepted (a CJK font is about 20 MB).
const MAX_FONT_FILE: u64 = 64 * 1024 * 1024;

/// Read a user's font file. Only called when a path is configured. Errors are texts for the status bar.
pub fn load_font_file(path: &str) -> Result<Arc<egui::FontData>, String> {
    let p = std::path::Path::new(path.trim());
    let meta = std::fs::metadata(p).map_err(|e| format!("Cannot use font file {}: {e}", p.display()))?;
    if !meta.is_file() || meta.len() > MAX_FONT_FILE {
        return Err(format!("Cannot use font file {}: not a file, or larger than 64 MB", p.display()));
    }
    let bytes = std::fs::read(p).map_err(|e| format!("Cannot use font file {}: {e}", p.display()))?;
    // TrueType, OpenType (CFF), TrueType collection: anything else is not a font egui can draw.
    if !matches!(bytes.get(..4), Some([0, 1, 0, 0] | b"OTTO" | b"true" | b"ttcf")) {
        return Err(format!("Cannot use font file {}: not a TTF or OTF font", p.display()));
    }
    Ok(Arc::new(egui::FontData::from_owned(bytes)))
}

/// Register the fonts of the context. Called once at startup and again only when a font setting changed (that drops
/// every layout cache). Only the fonts that are needed are added, and a font file is read only when one is set.
/// Returns the problems found (a font file that cannot be used: the bundled family is used instead).
///
/// Families: `Monospace` and `Proportional` are the toolkit's own (with DejaVu Sans Mono in front when bundled);
/// the editor text uses the named family "editor" and its bold/italic variants, the Markdown preview the "preview*"
/// families. Without the `typst` feature the named families map to the toolkit's defaults.
pub fn install_fonts(ctx: &egui::Context, choice: &FontChoice) -> Vec<String> {
    let mut errors = Vec::new();
    let mut defs = egui::FontDefinitions::default();
    let defaults_prop = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let defaults_mono = defs.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
    #[cfg(feature = "typst")]
    {
        let fonts = crate::sidebar::symbols::iron_sheets_assets_fonts();
        let mut add = |name: &'static str, idx: usize| {
            if let Some(&bytes) = fonts.get(idx) {
                defs.font_data.insert(name.to_string(), Arc::new(egui::FontData::from_static(bytes)));
            }
        };
        add("dejavu-mono", 16);
        add("dejavu-mono-bold", 13);
        add("dejavu-mono-bold-italic", 14);
        add("dejavu-mono-italic", 15);
        add("preview", 0);
        add("preview-bold", 1);
        add("preview-italic", 2);
        add("preview-bold-italic", 3);
        add("math", 8);
        if choice.preview == "newcm" {
            add("newcm", 9);
            add("newcm-bold", 10);
            add("newcm-italic", 11);
            add("newcm-bold-italic", 12);
        }
    }
    // Lilex (OFL, CFF OTF, calt arrow ligatures) is parsed only when chosen; without the `font-lilex` feature it is not embedded.
    if choice.editor == "lilex" && choice.editor_file.trim().is_empty() {
        #[cfg(feature = "font-lilex")]
        for (name, bytes) in [
            ("lilex", &include_bytes!("../assets/fonts/Lilex-Regular.otf")[..]),
            ("lilex-bold", &include_bytes!("../assets/fonts/Lilex-Bold.otf")[..]),
            ("lilex-italic", &include_bytes!("../assets/fonts/Lilex-Italic.otf")[..]),
            ("lilex-bold-italic", &include_bytes!("../assets/fonts/Lilex-BoldItalic.otf")[..]),
        ] {
            defs.font_data.insert(name.to_string(), Arc::new(egui::FontData::from_static(bytes)));
        }
        #[cfg(not(feature = "font-lilex"))]
        errors.push("Lilex is not built into this binary (feature font-lilex); using DejaVu Sans Mono".into());
    }
    let has = |defs: &egui::FontDefinitions, n: &str| defs.font_data.contains_key(n);
    // Fallbacks behind every family so symbols and emoji still draw.
    let with = |first: &[&str], rest: &[String], defs: &egui::FontDefinitions| -> Vec<String> {
        let mut v: Vec<String> = first.iter().filter(|n| defs.font_data.contains_key(**n)).map(|n| n.to_string()).collect();
        for n in rest {
            if !v.contains(n) {
                v.push(n.clone());
            }
        }
        v
    };
    if has(&defs, "dejavu-mono") {
        defs.families.entry(FontFamily::Monospace).or_default().insert(0, "dejavu-mono".into());
    }
    let mut custom = |defs: &mut egui::FontDefinitions, name: &str, path: &str| -> bool {
        if path.trim().is_empty() {
            return false;
        }
        match load_font_file(path) {
            Ok(d) => {
                defs.font_data.insert(name.to_string(), d);
                true
            }
            Err(e) => {
                errors.push(e);
                false
            }
        }
    };
    // Editor: id "" = DejaVu Sans Mono (bold and italic faces bundled), "hack" = Hack, "sans" = Ubuntu Light.
    let ed_custom = custom(&mut defs, "editor-custom", &choice.editor_file);
    let ed_first: &[&str] = if ed_custom {
        &["editor-custom"]
    } else {
        match choice.editor.as_str() {
            "hack" => &["Hack"],
            "sans" => &["Ubuntu-Light"],
            "lilex" => &["lilex"],
            _ => &["dejavu-mono"],
        }
    };
    let ed_lilex = !ed_custom && choice.editor == "lilex" && has(&defs, "lilex");
    let ed_faces = !ed_custom && !matches!(choice.editor.as_str(), "hack" | "sans");
    let faces = |plain: &[&str], bold: &str, italic: &str, bold_italic: &str, defs: &egui::FontDefinitions, rest: &[String]| {
        [
            with(plain, rest, defs),
            with(&[bold], &with(plain, rest, defs), defs),
            with(&[italic], &with(plain, rest, defs), defs),
            with(&[bold_italic], &with(plain, rest, defs), defs),
        ]
    };
    let ed = if ed_lilex {
        faces(ed_first, "lilex-bold", "lilex-italic", "lilex-bold-italic", &defs, &defaults_mono)
    } else if ed_faces {
        faces(ed_first, "dejavu-mono-bold", "dejavu-mono-italic", "dejavu-mono-bold-italic", &defs, &defaults_mono)
    } else {
        faces(ed_first, "-", "-", "-", &defs, &defaults_mono)
    };
    for (name, list) in ["editor", "mono-bold", "mono-italic", "mono-bold-italic"].into_iter().zip(ed) {
        defs.families.insert(FontFamily::Name(name.into()), list);
    }
    // Preview: id "" = Libertinus Serif, "newcm" = New Computer Modern, "sans" = Ubuntu Light, "mono" = DejaVu Sans Mono.
    let pv_custom = custom(&mut defs, "preview-custom", &choice.preview_file);
    let pv = if pv_custom {
        faces(&["preview-custom"], "-", "-", "-", &defs, &defaults_prop)
    } else {
        match choice.preview.as_str() {
            "newcm" if has(&defs, "newcm") => faces(&["newcm"], "newcm-bold", "newcm-italic", "newcm-bold-italic", &defs, &defaults_prop),
            "sans" => faces(&["Ubuntu-Light"], "-", "-", "-", &defs, &defaults_prop),
            "mono" => faces(&["dejavu-mono"], "dejavu-mono-bold", "dejavu-mono-italic", "dejavu-mono-bold-italic", &defs, &defaults_mono),
            _ => faces(&["preview"], "preview-bold", "preview-italic", "preview-bold-italic", &defs, &defaults_prop),
        }
    };
    for (name, list) in ["preview", "preview-bold", "preview-italic", "preview-bold-italic"].into_iter().zip(pv) {
        defs.families.insert(FontFamily::Name(name.into()), list);
    }
    let math = with(&["math"], &defaults_prop, &defs);
    defs.families.insert(FontFamily::Name("math".into()), math);
    ctx.set_fonts(defs);
    errors
}

/// Tweak egui's stock visuals so every text/background pair reaches WCAG AA and the row colours
/// (text, weak, strong, hover, selection) are opaque. `high_contrast` pushes text towards black/white.
pub fn adjust_visuals(v: &mut egui::Visuals, high_contrast: bool) {
    apply_ui_colors(v, &UiColors::adjusted(v.dark_mode, high_contrast));
}

/// Put a model's chrome colours into egui visuals.
pub fn apply_ui_colors(v: &mut egui::Visuals, u: &UiColors) {
    v.widgets.noninteractive.fg_stroke.color = color32(u.text);
    v.weak_text_color = Some(color32(u.weak_text));
    v.widgets.inactive.weak_bg_fill = color32(u.inactive_bg);
    v.widgets.hovered.weak_bg_fill = color32(u.hovered_bg);
    v.selection.bg_fill = color32(u.selection_bg);
    v.selection.stroke = egui::Stroke::new(u.focus_ring_width, color32(u.focus_ring));
    v.hyperlink_color = color32(u.hyperlink);
    v.warn_fg_color = color32(u.warn);
    v.error_fg_color = color32(u.error);
    v.extreme_bg_color = color32(u.edit_bg);
}

/// What a row in a list or tree represents, for [`row_text_color`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// Notes and folders: grey, brighter on hover.
    Normal,
    /// Files without a preview (images, other): dimmer grey.
    Dim,
    /// The active file / active folder / selected row: strong text colour (white in the dark theme).
    Active,
}

/// Text colour of a row. Active rows use the strong text colour; hovering brightens the grey ones.
pub fn row_text_color(v: &egui::Visuals, kind: RowKind, hovered: bool) -> Color32 {
    match (kind, hovered) {
        (RowKind::Active, _) => v.strong_text_color(),
        (RowKind::Normal, false) => v.text_color(),
        (RowKind::Normal, true) => v.strong_text_color(),
        (RowKind::Dim, false) => v.weak_text_color(),
        (RowKind::Dim, true) => v.text_color(),
    }
}

/// Opaque background of the active/selected row (text on it must use [`row_text_color`] with `Active`).
pub fn selected_bg(v: &egui::Visuals) -> Color32 {
    v.selection.bg_fill
}

/// Give a text field an accessible name (hint text alone is only a placeholder).
pub fn name_field(resp: &egui::Response, name: &str) {
    resp.ctx.accesskit_node_builder(resp.id, |b| b.set_label(name));
}

/// Paint a keyboard focus ring around a custom-drawn widget: one more point than the built-in ring (2 px normally,
/// 3 px in high contrast), inside the rect, with the corner radius of the other controls.
pub fn focus_ring(ui: &egui::Ui, rect: egui::Rect, resp: &egui::Response) {
    if resp.has_focus() {
        let sel = ui.visuals().selection.stroke;
        ui.painter().rect_stroke(rect.shrink(1.0), tokens::RADIUS, egui::Stroke::new((sel.width + 1.0).max(2.0), sel.color), egui::StrokeKind::Inside);
    }
}

static RECORD_ROWS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Turn the row record on (tests only). Off in the app: recording allocates a string per visible row per frame.
pub fn set_row_recording(on: bool) {
    RECORD_ROWS.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// True while rows are recorded; callers can skip building the name.
pub fn row_recording() -> bool {
    RECORD_ROWS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Record the colour a row was drawn with so tests can read it back. A no-op unless [`set_row_recording`] is on.
pub fn record_row(ctx: &egui::Context, key: &'static str, name: String, color: Color32) {
    if !row_recording() {
        return;
    }
    let id = egui::Id::NULL.with(("ux_rows", key));
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Vec<(String, Color32)>>(id).push((name, color)));
}

/// Like [`record_row`], but the name is built only when rows are recorded (no allocation in the app).
pub fn record_row_with(ctx: &egui::Context, key: &'static str, name: impl FnOnce() -> String, color: Color32) {
    if row_recording() {
        record_row(ctx, key, name(), color);
    }
}

/// Start a new frame's record for `key` (call once before the rows are drawn).
pub fn clear_rows(ctx: &egui::Context, key: &'static str) {
    if !row_recording() {
        return;
    }
    let id = egui::Id::NULL.with(("ux_rows", key));
    ctx.data_mut(|d| d.insert_temp::<Vec<(String, Color32)>>(id, Vec::new()));
}

/// Rows recorded by the last frame for `key` ("tabs", "files").
pub fn recorded_rows(ctx: &egui::Context, key: &'static str) -> Vec<(String, Color32)> {
    ctx.data(|d| d.get_temp(egui::Id::NULL.with(("ux_rows", key)))).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The module and its two options decide what the preview does with arrows and code.
    #[test]
    fn preview_ligature_style_follows_the_module_and_options() {
        let editor = FontFamily::Name("editor".into());
        let mut p = PreviewStyle::from_model(&ThemeModel::build(false, false, 14.0, 15.0, true).preview, &ThemeModel::build(false, false, 14.0, 15.0, true).editor);
        let mut c = AppConfig::default();
        p.set_ligatures(&c, true, &editor);
        assert!(p.subs && p.code_family == FontFamily::Monospace && p.split_code, "default: glyphs in prose, literal code");
        c.ligatures_in_code = true;
        p.set_ligatures(&c, true, &editor);
        assert!(p.code_family == editor && !p.split_code);
        p.set_ligatures(&c, false, &editor);
        assert!(p.code_family == FontFamily::Monospace && !p.split_code, "a font that cannot ligate needs nothing");
        c.module_ligatures = false;
        p.set_ligatures(&c, true, &editor);
        assert!(!p.subs && p.code_family == FontFamily::Monospace && p.split_code, "module off: nothing");
        c = AppConfig { ligatures_in_preview: false, ..Default::default() };
        p.set_ligatures(&c, true, &editor);
        assert!(!p.subs);
    }

    fn rgba(c: Color32) -> Rgba {
        let [r, g, b, a] = c.to_srgba_unmultiplied();
        Rgba::new(r, g, b, a)
    }

    #[test]
    fn stock_model_matches_egui_visuals() {
        for dark in [false, true] {
            let v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
            let u = UiColors::stock(dark);
            assert_eq!(rgba(v.text_color()), u.text);
            assert_eq!(rgba(v.weak_text_color()), u.weak_text);
            assert_eq!(rgba(v.strong_text_color()), u.strong_text);
            assert_eq!(rgba(v.hyperlink_color), u.hyperlink);
            assert_eq!(rgba(v.warn_fg_color), u.warn);
            assert_eq!(rgba(v.error_fg_color), u.error);
            assert_eq!(rgba(v.panel_fill), u.panel_fill);
            assert_eq!(rgba(v.extreme_bg_color), u.edit_bg);
            assert_eq!(rgba(v.widgets.inactive.weak_bg_fill), u.inactive_bg);
            assert_eq!(rgba(v.widgets.inactive.text_color()), u.inactive_text);
            assert_eq!(rgba(v.widgets.hovered.weak_bg_fill), u.hovered_bg);
            assert_eq!(rgba(v.widgets.hovered.text_color()), u.hovered_text);
            assert_eq!(rgba(v.selection.bg_fill), u.selection_bg);
            assert_eq!(rgba(v.selection.stroke.color), u.focus_ring);
            assert_eq!(v.selection.stroke.width, u.focus_ring_width);
            assert_eq!(rgba(v.widgets.noninteractive.bg_stroke.color), u.separator);
        }
    }

    #[test]
    fn adjusted_visuals_match_model() {
        for dark in [false, true] {
            for hc in [false, true] {
                let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
                adjust_visuals(&mut v, hc);
                let u = UiColors::adjusted(dark, hc);
                assert_eq!(rgba(v.text_color()), u.text);
                assert_eq!(rgba(v.weak_text_color()), u.weak_text);
                assert_eq!(rgba(v.selection.bg_fill), u.selection_bg);
                assert_eq!(rgba(v.hyperlink_color), u.hyperlink);
                assert_eq!(v.selection.stroke.width, u.focus_ring_width);
                // Untouched by the adjustments.
                assert_eq!(rgba(v.panel_fill), u.panel_fill);
            }
        }
    }

    #[test]
    fn styles_follow_model() {
        let t = Theme::default();
        assert_eq!(t.editor.text, color32(t.model.editor.text));
        assert_eq!(t.editor.font_id.size, 14.0);
        assert_eq!(t.preview.body_size, 15.0);
    }

    #[test]
    fn active_row_is_strong_and_hover_brightens() {
        for dark in [false, true] {
            let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
            adjust_visuals(&mut v, false);
            assert_eq!(row_text_color(&v, RowKind::Active, false), v.strong_text_color());
            assert_ne!(row_text_color(&v, RowKind::Normal, false), v.strong_text_color());
            assert_eq!(row_text_color(&v, RowKind::Normal, true), v.strong_text_color());
            assert!(
                contrast_ratio(rgba(row_text_color(&v, RowKind::Dim, true)), rgba(v.panel_fill))
                    > contrast_ratio(rgba(row_text_color(&v, RowKind::Dim, false)), rgba(v.panel_fill)) - 0.01
                    || !dark
            );
        }
    }

    #[test]
    #[ignore]
    fn report() {
        eprintln!("{}", contrast_report(true, false));
    }
}
