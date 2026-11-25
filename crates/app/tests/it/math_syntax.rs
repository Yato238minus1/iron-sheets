//! Math in Markdown previews: LaTeX option, the relaxed `$ x $` rule, math colour in both themes.
use crate::common::*;
use egui_kittest::Harness;
use iron_sheets::command::Command;
use iron_sheets_core::config::{MathSyntax, ThemePref, ViewMode};
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;

const TYPST_NOTE: &str = "# Typst math\n\nBody text for comparison: the quick brown fox.\n\nInline $a^2 + b^2 = c^2$ strict, space before $x + y $ closing, Typst style $ sum_(i=1)^n i $ here, and costs $5 and $10 stay text.\n\n$$ integral_0^1 x dif x $$\n";
const LATEX_NOTE: &str = "# LaTeX math\n\nBody text for comparison: the quick brown fox.\n\nInline $a^2 + b^2 = c^2$ strict, space before $\\alpha + \\beta $ closing, spaced $ \\sum_{i=1}^n i $ here, and costs $5 and $10 stay text.\n\n$$\\int_0^1 \\frac{x}{2} \\, dx = \\begin{pmatrix} a & b \\\\ c & d \\end{pmatrix}$$\n\n```math\n\\sqrt{x^2 + y^2} \\leq \\left( \\sum_k a_k \\right)\n```\n\nBroken: $\\foo + 1$\n";

fn st(h: &mut H) -> &mut iron_sheets::AppState {
    &mut h.state_mut().app().state
}

fn prev(h: &mut H) -> &mut iron_sheets::preview::markdown::MarkdownPreview {
    &mut h.state_mut().app().state.ui.md_preview
}

fn open(fx: &Fx, theme: ThemePref, syntax: MathSyntax, name: &str, text: &str) -> H {
    let p = fx.notes().join(name);
    std::fs::write(&p, text).expect("write");
    let mut o = fx.opts(vec![fx.notes()]);
    o.config.theme = theme;
    o.config.math_syntax = syntax;
    let mut h = harness(o);
    st(&mut h).commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
    st(&mut h).commands.push(Command::SetViewMode(ViewMode::Preview));
    settle(&mut h);
    h
}

fn wait_math(h: &mut H, textures: usize) {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(40) && prev(h).textures_painted < textures {
        h.step();
        std::thread::sleep(Duration::from_millis(15));
    }
    settle(h);
}

#[test]
fn typst_syntax_relaxed_dollar_and_currency() {
    for (theme, tag) in [(ThemePref::Light, "light"), (ThemePref::Dark, "dark")] {
        let fx = Fx::new();
        let mut h = open(&fx, theme, MathSyntax::Typst, "T.md", TYPST_NOTE);
        // strict, relaxed-close, spaced, display = 4 formulas
        wait_math(&mut h, 4);
        let n = prev(&mut h).textures_painted;
        let p = shot(&mut h, &format!("math-typst-{tag}"));
        eprintln!("{}: textures {n}", p.display());
        assert_eq!(n, 4, "four formulas, currency stays text");
    }
}

#[test]
fn latex_syntax_renders_and_reports_errors() {
    for (theme, tag) in [(ThemePref::Light, "light"), (ThemePref::Dark, "dark")] {
        let fx = Fx::new();
        let mut h = open(&fx, theme, MathSyntax::Latex, "L.md", LATEX_NOTE);
        // 3 inline + display + ```math block = 5 images; `\foo` is an error shown as text
        wait_math(&mut h, 5);
        let n = prev(&mut h).textures_painted;
        let p = shot(&mut h, &format!("math-latex-{tag}"));
        eprintln!("{}: textures {n}", p.display());
        assert_eq!(n, 5);
    }
}

#[test]
fn latex_source_is_an_error_in_typst_mode() {
    let fx = Fx::new();
    let mut h = open(&fx, ThemePref::Light, MathSyntax::Typst, "L.md", "Text $\\alpha + \\beta$ end.\n");
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(20) {
        h.step();
        std::thread::sleep(Duration::from_millis(15));
    }
    assert_eq!(prev(&mut h).textures_painted, 0, "LaTeX is not valid Typst");
    shot(&mut h, "math-latex-in-typst-mode");
}

#[test]
fn toggling_the_syntax_changes_results_live() {
    let fx = Fx::new();
    let mut h = open(&fx, ThemePref::Light, MathSyntax::Typst, "L.md", "Text $\\alpha + \\beta$ end.\n");
    for _ in 0..40 {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(prev(&mut h).textures_painted, 0);
    // Switch in the settings the way the UI does: change the config, then `apply`.
    st(&mut h).config.math_syntax = MathSyntax::Latex;
    iron_sheets::core::settings_model::apply(st(&mut h), false);
    wait_math(&mut h, 1);
    assert_eq!(prev(&mut h).textures_painted, 1, "after switching to LaTeX the same note renders");
    shot(&mut h, "math-toggled-to-latex");
    st(&mut h).config.math_syntax = MathSyntax::Typst;
    iron_sheets::core::settings_model::apply(st(&mut h), false);
    for _ in 0..40 {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(prev(&mut h).textures_painted, 0, "back to Typst: stale LaTeX image must not stay");
}

/// Math uses the text colour of the theme: its most saturated ink pixel equals the body text
/// colour (it only looks lighter than the serif body text because the math font's strokes are thin).
#[test]
fn math_ink_has_the_theme_text_colour() {
    for (theme, tag) in [(ThemePref::Light, "light"), (ThemePref::Dark, "dark")] {
        let fx = Fx::new();
        let mut h = open(&fx, theme, MathSyntax::Typst, "C.md", "$$ integral_0^1 x dif x $$\n\n$a^2 + b^2$\n");
        wait_math(&mut h, 2);
        let want = st(&mut h).ui.theme.preview.text;
        shot(&mut h, &format!("math-colour-{tag}"));
        let img = h.render().expect("render");
        let (w, hh) = (img.width(), img.height());
        let (mut best, mut best_d) = ([0u8; 3], 255i32 * 3);
        for y in 80..(hh.min(300)) {
            for x in 290..w.min(1080) {
                let p = img.get_pixel(x, y).0;
                let d = (p[0] as i32 - want.r() as i32).abs() + (p[1] as i32 - want.g() as i32).abs() + (p[2] as i32 - want.b() as i32).abs();
                if d < best_d {
                    (best, best_d) = ([p[0], p[1], p[2]], d);
                }
            }
        }
        eprintln!("{tag}: text colour {want:?}, closest ink pixel {best:?} (distance {best_d})");
        assert!(best_d <= 12, "{tag}: no pixel with the text colour (closest {best:?}, want {want:?})");
    }
}
