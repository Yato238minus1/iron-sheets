//! Framework-free theme: named colour tokens, font sizes and the WCAG contrast checks over them.
//!
//! `theme.rs` turns this model into toolkit styles. All colours here are straight (non-premultiplied) RGBA bytes.

/// An 8-bit RGBA colour (straight alpha).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rgba(pub [u8; 4]);

impl Rgba {
    pub const BLACK: Rgba = Rgba([0, 0, 0, 255]);
    pub const WHITE: Rgba = Rgba([255, 255, 255, 255]);

    pub const fn rgb(r: u8, g: u8, b: u8) -> Rgba {
        Rgba([r, g, b, 255])
    }
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Rgba {
        Rgba([r, g, b, a])
    }
    pub const fn gray(v: u8) -> Rgba {
        Rgba([v, v, v, 255])
    }
    pub const fn r(self) -> u8 {
        self.0[0]
    }
    pub const fn g(self) -> u8 {
        self.0[1]
    }
    pub const fn b(self) -> u8 {
        self.0[2]
    }
    pub const fn a(self) -> u8 {
        self.0[3]
    }
}

/// Editor token colours and sizes (used by the source highlighter).
#[derive(Debug, Clone, PartialEq)]
pub struct EditorColors {
    pub font_size: f32,
    pub text: Rgba,
    pub heading: Rgba,
    pub emphasis: Rgba,
    pub strong: Rgba,
    pub code: Rgba,
    pub code_bg: Rgba,
    pub link: Rgba,
    pub quote: Rgba,
    /// Markup punctuation (`#`, `*`, `[[`, fences).
    pub marker: Rgba,
    pub comment: Rgba,
    pub keyword: Rgba,
    pub string: Rgba,
    pub number: Rgba,
    pub tag: Rgba,
    pub math: Rgba,
    pub selection_bg: Rgba,
    pub line_height: f32,
}

/// Colours and sizes for rendered previews.
#[derive(Debug, Clone, PartialEq)]
pub struct PreviewColors {
    pub body_size: f32,
    pub text: Rgba,
    pub heading: Rgba,
    pub link: Rgba,
    pub quote_bar: Rgba,
    pub code_bg: Rgba,
    pub rule: Rgba,
    pub table_border: Rgba,
    pub dark: bool,
}

/// Chrome colours the app depends on: stock toolkit values plus the app's accessibility adjustments.
#[derive(Debug, Clone, PartialEq)]
pub struct UiColors {
    pub text: Rgba,
    pub weak_text: Rgba,
    pub strong_text: Rgba,
    pub hyperlink: Rgba,
    pub warn: Rgba,
    pub error: Rgba,
    pub panel_fill: Rgba,
    /// Background of text fields and the editor.
    pub edit_bg: Rgba,
    pub inactive_bg: Rgba,
    pub inactive_text: Rgba,
    pub hovered_bg: Rgba,
    pub hovered_text: Rgba,
    pub selection_bg: Rgba,
    pub focus_ring: Rgba,
    pub focus_ring_width: f32,
    pub separator: Rgba,
}

impl UiColors {
    /// The toolkit's stock colours (what the app starts from before adjusting). A test in `theme.rs` keeps these in
    /// sync with the toolkit's own visuals.
    pub fn stock(dark: bool) -> UiColors {
        if dark {
            UiColors {
                text: Rgba::gray(140),
                weak_text: Rgba::new(140, 140, 140, 153),
                strong_text: Rgba::gray(255),
                hyperlink: Rgba::rgb(90, 170, 255),
                warn: Rgba::rgb(255, 143, 0),
                error: Rgba::rgb(255, 0, 0),
                panel_fill: Rgba::gray(27),
                edit_bg: Rgba::gray(10),
                inactive_bg: Rgba::gray(50),
                inactive_text: Rgba::gray(215),
                hovered_bg: Rgba::gray(64),
                hovered_text: Rgba::gray(245),
                selection_bg: Rgba::rgb(0, 92, 128),
                focus_ring: Rgba::rgb(192, 222, 255),
                focus_ring_width: 1.0,
                separator: Rgba::gray(60),
            }
        } else {
            UiColors {
                text: Rgba::gray(80),
                weak_text: Rgba::new(80, 80, 80, 153),
                strong_text: Rgba::gray(0),
                hyperlink: Rgba::rgb(0, 155, 255),
                warn: Rgba::rgb(255, 100, 0),
                error: Rgba::rgb(255, 0, 0),
                panel_fill: Rgba::gray(248),
                edit_bg: Rgba::gray(255),
                inactive_bg: Rgba::gray(230),
                inactive_text: Rgba::gray(60),
                hovered_bg: Rgba::gray(220),
                hovered_text: Rgba::gray(0),
                selection_bg: Rgba::rgb(144, 209, 255),
                focus_ring: Rgba::rgb(0, 83, 125),
                focus_ring_width: 1.0,
                separator: Rgba::gray(190),
            }
        }
    }

    /// Stock colours with the app's changes so every text/background pair reaches WCAG AA and the row colours (text,
    /// weak, strong, hover, selection) are opaque. `high_contrast` pushes text towards black/white.
    pub fn adjusted(dark: bool, high_contrast: bool) -> UiColors {
        let g = Rgba::gray;
        let rgb = Rgba::rgb;
        let mut u = UiColors::stock(dark);
        let width = if high_contrast { 2.0 } else { 1.0 };
        if dark {
            let (text, weak) = if high_contrast { (g(235), g(205)) } else { (g(180), g(150)) };
            u.text = text;
            u.weak_text = weak;
            u.inactive_bg = g(36);
            u.hovered_bg = g(46);
            u.selection_bg = rgb(24, 64, 100);
            u.focus_ring = Rgba::WHITE;
            u.focus_ring_width = width;
            u.hyperlink = rgb(110, 180, 255);
            u.error = rgb(255, 100, 100);
        } else {
            let (text, weak) = if high_contrast { (g(10), g(55)) } else { (g(60), g(95)) };
            u.text = text;
            u.weak_text = weak;
            u.inactive_bg = g(240);
            u.hovered_bg = g(228);
            u.focus_ring = rgb(0, 60, 120);
            u.focus_ring_width = width;
            u.hyperlink = rgb(0, 75, 160);
            u.warn = rgb(150, 75, 0);
            u.error = rgb(180, 0, 0);
        }
        u
    }
}

/// Colours the user can override per theme variant: (key, label, what it colours). Keys are what the config stores.
pub const COLOR_KEYS: &[(&str, &str, &str)] = &[
    ("accent", "Accent", "Selected rows, selection and quote bar"),
    ("editor_bg", "Editor background", "Background of the editor and of text fields"),
    ("editor_text", "Editor text", "Plain text in the editor"),
    ("link", "Link colour", "Links in the editor, the preview and the interface"),
    ("heading", "Heading colour", "Headings in the editor and the preview"),
    ("emphasis", "Emphasis colour", "Italic text in the editor"),
    ("strong", "Strong colour", "Bold text in the editor"),
    ("code", "Code colour", "Inline code and fences in the editor"),
    ("quote", "Quote colour", "Block quotes in the editor"),
    ("marker", "Markup colour", "Markup punctuation in the editor"),
    ("comment", "Comment colour", "Comments in the editor"),
    ("keyword", "Keyword colour", "Keywords in the editor"),
    ("string", "String colour", "Strings in the editor"),
    ("number", "Number colour", "Numbers in the editor"),
    ("tag", "Tag colour", "Tags in the editor"),
    ("math", "Math colour", "Math in the editor"),
];

/// `#rrggbb` (the `#` is optional, case does not matter) as an opaque colour.
pub fn parse_hex(s: &str) -> Option<Rgba> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(Rgba::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// `#rrggbb`.
pub fn format_hex(c: Rgba) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

/// Everything the visual style is computed from, for one theme variant.
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeModel {
    pub dark: bool,
    pub high_contrast: bool,
    pub ui: UiColors,
    pub editor: EditorColors,
    pub preview: PreviewColors,
}

impl ThemeModel {
    /// `adjusted` applies the app's accessibility adjustments to the chrome colours.
    pub fn build(dark: bool, high_contrast: bool, editor_size: f32, preview_size: f32, adjusted: bool) -> ThemeModel {
        ThemeModel {
            dark,
            high_contrast,
            ui: if adjusted { UiColors::adjusted(dark, high_contrast) } else { UiColors::stock(dark) },
            editor: editor_colors(dark, editor_size),
            preview: preview_colors(dark, preview_size),
        }
    }
}

impl ThemeModel {
    /// Put the user's colour overrides (`COLOR_KEYS` -> `#rrggbb`) on top of the colours built so far. Order of
    /// precedence, lowest first: stock toolkit colours, the app's accessibility adjustments, high contrast, the user's
    /// overrides. Unknown keys and values that do not parse are ignored.
    pub fn apply_overrides(&mut self, o: &std::collections::BTreeMap<String, String>) {
        for (key, value) in o {
            let Some(c) = parse_hex(value) else { continue };
            match key.as_str() {
                "accent" => {
                    self.ui.selection_bg = c;
                    self.editor.selection_bg = c;
                    self.preview.quote_bar = c;
                }
                "editor_bg" => self.ui.edit_bg = c,
                "editor_text" => self.editor.text = c,
                "link" => {
                    self.ui.hyperlink = c;
                    self.editor.link = c;
                    self.preview.link = c;
                }
                "heading" => {
                    self.editor.heading = c;
                    self.preview.heading = c;
                }
                "emphasis" => self.editor.emphasis = c,
                "strong" => self.editor.strong = c,
                "code" => self.editor.code = c,
                "quote" => self.editor.quote = c,
                "marker" => self.editor.marker = c,
                "comment" => self.editor.comment = c,
                "keyword" => self.editor.keyword = c,
                "string" => self.editor.string = c,
                "number" => self.editor.number = c,
                "tag" => self.editor.tag = c,
                "math" => self.editor.math = c,
                _ => {}
            }
        }
    }

    /// The colour a key currently has in this model (for the settings swatches).
    pub fn color_of(&self, key: &str) -> Option<Rgba> {
        Some(match key {
            "accent" => self.ui.selection_bg,
            "editor_bg" => self.ui.edit_bg,
            "editor_text" => self.editor.text,
            "link" => self.ui.hyperlink,
            "heading" => self.editor.heading,
            "emphasis" => self.editor.emphasis,
            "strong" => self.editor.strong,
            "code" => self.editor.code,
            "quote" => self.editor.quote,
            "marker" => self.editor.marker,
            "comment" => self.editor.comment,
            "keyword" => self.editor.keyword,
            "string" => self.editor.string,
            "number" => self.editor.number,
            "tag" => self.editor.tag,
            "math" => self.editor.math,
            _ => return None,
        })
    }
}

pub fn editor_colors(dark: bool, size: f32) -> EditorColors {
    let c = |l: [u8; 3], d: [u8; 3]| {
        let v = if dark { d } else { l };
        Rgba::rgb(v[0], v[1], v[2])
    };
    EditorColors {
        font_size: size,
        text: c([30, 30, 30], [210, 210, 210]),
        heading: c([0, 70, 150], [110, 170, 240]),
        emphasis: c([120, 60, 120], [200, 150, 210]),
        strong: c([150, 60, 0], [235, 170, 110]),
        code: c([160, 40, 70], [235, 130, 150]),
        code_bg: c([238, 238, 238], [50, 50, 54]),
        link: c([0, 100, 200], [100, 170, 255]),
        quote: c([90, 110, 90], [150, 180, 150]),
        marker: c([140, 140, 140], [120, 120, 125]),
        comment: c([85, 105, 85], [120, 150, 120]),
        keyword: c([150, 30, 150], [200, 130, 220]),
        string: c([20, 120, 40], [130, 200, 140]),
        number: c([180, 90, 0], [230, 170, 90]),
        tag: c([0, 120, 120], [100, 200, 200]),
        math: c([140, 80, 0], [225, 180, 100]),
        selection_bg: c([180, 205, 240], [50, 80, 120]),
        line_height: size * 1.4,
    }
}

pub fn preview_colors(dark: bool, size: f32) -> PreviewColors {
    let c = |l: [u8; 3], d: [u8; 3]| {
        let v = if dark { d } else { l };
        Rgba::rgb(v[0], v[1], v[2])
    };
    PreviewColors {
        body_size: size,
        text: c([30, 30, 30], [215, 215, 215]),
        heading: c([20, 20, 20], [240, 240, 240]),
        link: c([0, 100, 200], [100, 170, 255]),
        quote_bar: c([190, 190, 190], [90, 90, 95]),
        code_bg: c([240, 240, 240], [45, 45, 50]),
        rule: c([200, 200, 200], [80, 80, 85]),
        table_border: c([200, 200, 200], [80, 80, 85]),
        dark,
    }
}

// ---------------------------------------------------------------------------------------------
// Contrast helpers (WCAG 2.x relative luminance).

fn lin(c: u8) -> f64 {
    let c = c as f64 / 255.0;
    if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn luminance(c: Rgba) -> f64 {
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

/// Composite `fg` over the opaque `bg` (straight alpha result).
fn over(fg: Rgba, bg: Rgba) -> Rgba {
    let a = fg.a() as f32 / 255.0;
    let m = |f: u8, b: u8| (f as f32 * a + b as f32 * (1.0 - a)).round() as u8;
    Rgba::rgb(m(fg.r(), bg.r()), m(fg.g(), bg.g()), m(fg.b(), bg.b()))
}

/// WCAG contrast ratio of `fg` on `bg` (1.0 ..= 21.0). Translucent `fg` is composited over `bg`.
pub fn contrast_ratio(fg: Rgba, bg: Rgba) -> f64 {
    let fg = over(fg, bg);
    let (a, b) = (luminance(fg), luminance(bg));
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

/// Every foreground/background pair the UI draws, with the WCAG AA minimum for it
/// (4.5 normal text, 3.0 UI components and large text). `adjusted` selects the app's colours over the stock ones.
pub fn contrast_pairs(dark: bool, adjusted: bool, high_contrast: bool) -> Vec<(String, Rgba, Rgba, f64)> {
    let m = ThemeModel::build(dark, high_contrast, 14.0, 15.0, adjusted);
    let (v, e, p) = (&m.ui, &m.editor, &m.preview);
    let panel = v.panel_fill;
    let edit_bg = v.edit_bg;
    let hover_bg = v.hovered_bg;
    let sel = v.selection_bg;
    let s = |n: &str| n.to_string();
    let mut out = vec![
        (s("ui text on panel"), v.text, panel, 4.5),
        (s("ui weak text on panel"), v.weak_text, panel, 4.5),
        (s("ui strong text on panel"), v.strong_text, panel, 4.5),
        (s("ui link on panel"), v.hyperlink, panel, 4.5),
        (s("ui warning on panel"), v.warn, panel, 4.5),
        (s("ui error on panel"), v.error, panel, 4.5),
        (s("ui text on hovered row"), v.text, hover_bg, 4.5),
        (s("ui weak text on hovered row"), v.weak_text, hover_bg, 4.5),
        (s("ui strong text on selected row"), v.strong_text, sel, 4.5),
        (s("ui link on selected row"), v.hyperlink, sel, 4.5),
        (s("focus ring on panel"), v.focus_ring, panel, 3.0),
        (s("button text on button"), v.inactive_text, v.inactive_bg, 4.5),
        (s("hovered button text"), v.hovered_text, v.hovered_bg, 4.5),
        (s("separator on panel"), v.separator, panel, 1.0),
        (s("editor text on selection"), e.text, e.selection_bg, 4.5),
        (s("editor code on code bg"), e.code, e.code_bg, 4.5),
        (s("editor marker on editor bg"), e.marker, edit_bg, 3.0),
        (s("preview text on panel"), p.text, panel, 4.5),
        (s("preview heading on panel"), p.heading, panel, 4.5),
        (s("preview link on panel"), p.link, panel, 4.5),
        (s("preview text on code bg"), p.text, p.code_bg, 4.5),
    ];
    for (n, c) in [
        ("text", e.text),
        ("heading", e.heading),
        ("emphasis", e.emphasis),
        ("strong", e.strong),
        ("link", e.link),
        ("quote", e.quote),
        ("comment", e.comment),
        ("keyword", e.keyword),
        ("string", e.string),
        ("number", e.number),
        ("tag", e.tag),
        ("math", e.math),
    ] {
        out.push((format!("editor {n} on editor bg"), c, edit_bg, 4.5));
    }
    out
}

/// Human-readable table of [`contrast_pairs`] (used by the test and the report).
pub fn contrast_report(adjusted: bool, high_contrast: bool) -> String {
    let mut s = String::new();
    for dark in [false, true] {
        for (name, fg, bg, min) in contrast_pairs(dark, adjusted, high_contrast) {
            let r = contrast_ratio(fg, bg);
            s += &format!("{} {name}: {r:.2} (min {min}){}\n", if dark { "dark " } else { "light" }, if r + 1e-9 < min { "  FAIL" } else { "" });
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_is_wcag_aa() {
        for hc in [false, true] {
            let mut fails = String::new();
            for dark in [false, true] {
                for (name, fg, bg, min) in contrast_pairs(dark, true, hc) {
                    let r = contrast_ratio(fg, bg);
                    if r < min {
                        fails += &format!("{} {name}: {r:.2} < {min} (hc={hc})\n", if dark { "dark" } else { "light" });
                    }
                }
            }
            assert!(fails.is_empty(), "contrast below WCAG AA:\n{fails}");
        }
    }

    #[test]
    fn hex_roundtrip_and_rejects() {
        assert_eq!(parse_hex("#336699"), Some(Rgba::rgb(0x33, 0x66, 0x99)));
        assert_eq!(parse_hex(" AbCdEf "), Some(Rgba::rgb(0xab, 0xcd, 0xef)));
        assert_eq!(format_hex(Rgba::rgb(1, 2, 255)), "#0102ff");
        for bad in ["", "#12345", "#1234567", "#gggggg", "#12345é", "red"] {
            assert_eq!(parse_hex(bad), None, "{bad}");
        }
    }

    #[test]
    fn overrides_go_on_top_and_every_key_is_known() {
        use std::collections::BTreeMap;
        let base = ThemeModel::build(true, true, 14.0, 15.0, true);
        let mut m = base.clone();
        m.apply_overrides(&BTreeMap::new());
        assert_eq!(m, base, "no overrides: nothing changes");
        let mut o = BTreeMap::new();
        for (i, (k, _, _)) in COLOR_KEYS.iter().enumerate() {
            o.insert(k.to_string(), format_hex(Rgba::rgb(1 + i as u8, 2, 3)));
        }
        o.insert("nonsense".into(), "#ffffff".into());
        o.insert("accent".into(), "not a colour".into());
        m.apply_overrides(&o);
        // High contrast stays the base; only overridden values moved.
        assert_eq!(m.ui.text, base.ui.text);
        assert_eq!(m.ui.focus_ring_width, base.ui.focus_ring_width);
        assert_eq!(m.ui.edit_bg, Rgba::rgb(2, 2, 3));
        assert_eq!(m.color_of("editor_text"), Some(Rgba::rgb(3, 2, 3)));
        assert_eq!(m.ui.selection_bg, base.ui.selection_bg, "an invalid value is ignored");
        for (k, _, _) in COLOR_KEYS {
            assert!(m.color_of(k).is_some(), "{k}");
        }
    }

    #[test]
    fn ratio_extremes() {
        assert!((contrast_ratio(Rgba::BLACK, Rgba::WHITE) - 21.0).abs() < 1e-6);
        assert!((contrast_ratio(Rgba::WHITE, Rgba::WHITE) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn variants_differ_and_sizes_follow_config() {
        let light = ThemeModel::build(false, false, 14.0, 15.0, true);
        let dark = ThemeModel::build(true, false, 14.0, 15.0, true);
        assert_ne!(light.editor.text, dark.editor.text);
        assert_ne!(light.ui.panel_fill, dark.ui.panel_fill);
        let hc = ThemeModel::build(true, true, 20.0, 18.0, true);
        assert_ne!(hc.ui.text, dark.ui.text);
        assert_eq!(hc.editor.font_size, 20.0);
        assert!((hc.editor.line_height - 28.0).abs() < 1e-4);
        assert_eq!(hc.preview.body_size, 18.0);
        assert!(hc.ui.focus_ring_width > dark.ui.focus_ring_width);
    }
}
