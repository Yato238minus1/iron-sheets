//! Auto math syntax: each formula is rendered in the syntax it looks like, with a fallback to the other one.
use crate::common::*;
use egui_kittest::Harness;
use iron_sheets::command::Command;
use iron_sheets_core::config::{MathSyntax, ThemePref, ViewMode};
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;

const MIXED: &str = "# Mixed math\n\nTypst $a^2 + b^2 = c^2$ and LaTeX $\\alpha + \\beta$ and Typst $ sum_(i=1)^n i $ and LaTeX $x_{i}^{2}$ here.\n\n$$ integral_0^1 x dif x $$\n\n$$\\int_0^1 \\frac{x}{2} \\, dx$$\n\nBroken: $\\foo + 1$\n";

fn st(h: &mut H) -> &mut iron_sheets::AppState {
    &mut h.state_mut().app().state
}

fn prev(h: &mut H) -> &mut iron_sheets::preview::markdown::MarkdownPreview {
    &mut h.state_mut().app().state.ui.md_preview
}

fn open(fx: &Fx, syntax: MathSyntax, text: &str) -> H {
    let p = fx.notes().join("A.md");
    std::fs::write(&p, text).expect("write");
    let mut o = fx.opts(vec![fx.notes()]);
    o.config.theme = ThemePref::Light;
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

/// A few more frames, so a late fallback render would show up in the count.
fn steps_quiet(h: &mut H) {
    for _ in 0..20 {
        h.step();
        std::thread::sleep(Duration::from_millis(15));
    }
}

#[test]
fn auto_is_the_default_syntax() {
    assert_eq!(iron_sheets_core::config::AppConfig::default().math_syntax, MathSyntax::Auto);
}

#[test]
fn auto_renders_both_syntaxes_in_one_note() {
    let fx = Fx::new();
    let mut h = open(&fx, MathSyntax::Auto, MIXED);
    // 4 inline + 2 display render; `\foo` is a LaTeX command, so it shows its LaTeX error instead of being retried as
    // Typst (where it would silently become an escape plus "oo" = infinity).
    wait_math(&mut h, 6);
    steps_quiet(&mut h);
    let n = prev(&mut h).textures_painted;
    let p = shot(&mut h, "math-auto-mixed");
    eprintln!("{}: textures {n}", p.display());
    assert_eq!(n, 6);
}

#[test]
fn explicit_typst_still_rejects_latex() {
    let fx = Fx::new();
    let mut h = open(&fx, MathSyntax::Typst, MIXED);
    wait_math(&mut h, 6);
    assert!(prev(&mut h).textures_painted < 6, "LaTeX formulas are errors in Typst mode");
}

/// Timing: `cargo test -p iron-sheets --release --test it -- math_auto:: --ignored --nocapture`.
#[test]
#[ignore]
fn auto_vs_explicit_timing_500_formulas() {
    // 10 paragraphs of 50 formulas: 25 Typst-style and 25 LaTeX-style each, all distinct.
    let note = |latex_only: bool, typst_only: bool| {
        let mut s = String::new();
        for p in 0..10 {
            for i in 0..50 {
                let k = p * 50 + i;
                if i % 2 == 0 && !latex_only || typst_only {
                    s.push_str(&format!("t{i} $x_{k} + y^2 + alpha_{i}$ "));
                } else {
                    s.push_str(&format!("l{i} $\\alpha_{{{k}}} + \\beta^2$ "));
                }
            }
            s.push_str("\n\n");
        }
        s
    };
    for (name, syntax, text) in [
        ("auto mixed", MathSyntax::Auto, note(false, false)),
        ("auto typst-only", MathSyntax::Auto, note(false, true)),
        ("typst typst-only", MathSyntax::Typst, note(false, true)),
        ("auto latex-only", MathSyntax::Auto, note(true, false)),
        ("latex latex-only", MathSyntax::Latex, note(true, false)),
    ] {
        let fx = Fx::new();
        let t0 = Instant::now();
        let mut h = open(&fx, syntax, &text);
        let first = t0.elapsed();
        // Visible formulas only (virtualized); wait until the count stops growing.
        let (mut last, mut stable) = (0, 0);
        while stable < 30 && t0.elapsed() < Duration::from_secs(60) {
            h.step();
            let n = prev(&mut h).textures_painted;
            stable = if n == last { stable + 1 } else { 0 };
            last = n;
            std::thread::sleep(Duration::from_millis(10));
        }
        let all = t0.elapsed();
        let mut frames = vec![];
        for _ in 0..30 {
            let t = Instant::now();
            h.step();
            frames.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        frames.sort_by(f64::total_cmp);
        eprintln!(
            "{name:18} open {:6.0} ms, all visible math {:6.0} ms ({last} painted), steady frame median {:.2} ms",
            first.as_secs_f64() * 1000.0,
            all.as_secs_f64() * 1000.0,
            frames[15]
        );
    }
}
