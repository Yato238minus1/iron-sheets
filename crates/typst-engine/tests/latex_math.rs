//! LaTeX formulas converted by `latex_to_typst` must compile with the real Typst engine.
use std::sync::Arc;

use iron_sheets_typst::internals::{MathJob, Shared, render_batch, render_one};

pub const FORMULAS: &[&str] = &[
    "a^2 + b^2 = c^2",
    "ab + cd",
    "\\frac{a}{b} + \\frac12",
    "\\sqrt{x^2+y^2} + \\sqrt[3]{x}",
    "\\sum_{i=1}^{n} \\alpha_i x^i",
    "\\int_0^\\infty e^{-x}\\,dx = 1",
    "\\prod_{k=1}^{n} (1 + x_k)",
    "\\lim_{x \\to 0} \\frac{\\sin x}{x} = 1",
    "\\left( \\frac{a}{b} \\right)^2",
    "\\left[ 0, 1 \\right) \\cup \\left\\{ 2, 3 \\right\\}",
    "\\left\\langle u, v \\right\\rangle \\leq \\|u\\| \\|v\\|",
    "\\mathbb{R}^n \\ni \\mathbf{v}",
    "\\mathrm{d}x + \\mathrm{sin}\\, x + \\operatorname{rank} A",
    "\\text{for all } x \\in \\mathcal{X}",
    "x \\cdot y \\times z \\pm w \\neq 0",
    "A \\subseteq B \\Rightarrow C \\iff D",
    "\\begin{pmatrix} a & b \\\\ c & d \\end{pmatrix}",
    "\\begin{bmatrix} 1 & 0 \\\\ 0 & 1 \\end{bmatrix}",
    "\\begin{vmatrix} a & b \\\\ c & d \\end{vmatrix} = ad - bc",
    "f(x) = \\begin{cases} x^2 & x \\geq 0 \\\\ -x & \\text{otherwise} \\end{cases}",
    "\\begin{aligned} a &= b + c \\\\ &= d \\end{aligned}",
    "\\binom{n}{k} = \\frac{n!}{k!(n-k)!}",
    "\\hat{x} + \\vec{v} + \\bar{y} + \\tilde{z} + \\dot{q} + \\ddot{q}",
    "\\overline{AB} + \\underline{CD}",
    "\\underbrace{a + b}_{n} + \\overbrace{c + d}^{m}",
    "\\partial_t u = \\nabla^2 u",
    "f'(x) + f''(x)",
    "a/b + c|d",
    "x_{i,j}^{(k)}",
    "\\sum\\limits_{i} x_i + \\int\\nolimits_a^b f",
    "\\alpha\\beta\\gamma\\delta\\epsilon\\varepsilon\\zeta\\eta\\theta\\vartheta\\phi\\varphi\\omega\\Omega\\Gamma\\Delta",
    "\\ldots \\cdots \\vdots \\ddots",
    "a \\quad b \\qquad c \\, d \\; e \\: f \\! g",
    "\\forall x \\exists y : x < y",
    "\\hbar \\omega \\ell \\infty \\emptyset",
    "\\color{red} x + y",
    "\\boxed{a = b}",
    "x \\pmod{n} + y \\bmod n",
    "\\overset{!}{=} \\underset{x}{\\min}",
    "1.5 + 2.25 \\times 10^{-3}",
    "\\frac{\\partial^2 f}{\\partial x \\partial y}",
    "\\begin{array}{cc} 1 & 2 \\\\ 3 & 4 \\end{array}",
    "\\mathfrak{g} \\mathsf{A} \\mathtt{t} \\boldsymbol{\\mu}",
    "\"quote\" \\& \\# \\% \\$ \\_ \\{ \\}",
];

fn job(src: &str, latex: bool, display: bool) -> MathJob {
    MathJob { source: src.into(), display, font_size_pt: 14.0, color: [0, 0, 0, 255], pixels_per_pt: 2.0, latex }
}

#[test]
fn every_formula_compiles() {
    let shared = Arc::new(Shared::new());
    let mut bad = Vec::new();
    for (i, f) in FORMULAS.iter().enumerate() {
        for display in [false, true] {
            if let Err(e) = render_one(&shared, &job(f, true, display), i as u64) {
                bad.push(format!("{f}  =>  {} : {e}", iron_sheets_typst::latex_to_typst(f).unwrap_or_default()));
            }
        }
    }
    assert!(bad.is_empty(), "failed:\n{}", bad.join("\n"));
}

#[test]
fn batch_isolates_conversion_and_compile_errors() {
    let shared = Arc::new(Shared::new());
    let jobs: Vec<(MathJob, u64)> = vec![
        (job("a + b", true, false), 1),
        (job("\\foo", true, false), 2),
        (job("\\frac{a}{b}", true, true), 3),
        (job("x^", true, false), 4),
        (job("sum_(i=1)^n i", false, false), 5),
    ];
    let r = render_batch(&shared, &jobs);
    assert!(r[0].is_ok());
    assert!(r[1].as_ref().unwrap_err().contains("unsupported LaTeX command"));
    assert!(r[2].is_ok());
    assert!(r[3].is_err());
    assert!(r[4].is_ok());
}

#[test]
fn latex_and_typst_modes_differ() {
    // `\alpha` is only valid LaTeX; `alpha` as LaTeX is a product of letters.
    let shared = Arc::new(Shared::new());
    assert!(render_one(&shared, &job("\\alpha", false, false), 1).is_err());
    assert!(render_one(&shared, &job("\\alpha", true, false), 1).is_ok());
    assert!(render_one(&shared, &job("alpha", true, false), 1).is_ok());
}

/// Every command of the symbol table, each in its own formula (scripts attached, in a fraction).
#[test]
fn every_table_symbol_compiles() {
    let src = include_str!("../src/latex.rs");
    let region = &src[src.find("fn symbol(").unwrap()..src.find("#[cfg(test)]").unwrap()];
    let mut names: Vec<String> = Vec::new();
    for (pos, _) in region.match_indices("=>") {
        let before = &region[..pos];
        let start = before.rfind(|c: char| !(c.is_ascii_alphabetic() || c == '"' || c == '|' || c == ' ')).map_or(0, |p| p + 1);
        for part in before[start..].split('|') {
            let n = part.trim().trim_matches('"');
            if !n.is_empty() && n.chars().all(|c| c.is_ascii_alphabetic()) {
                names.push(n.to_string());
            }
        }
    }
    names.sort();
    names.dedup();
    assert!(names.len() > 200, "{}", names.len());
    let shared = Arc::new(Shared::new());
    let jobs: Vec<(MathJob, u64)> =
        names.iter().enumerate().map(|(i, n)| (job(&format!("a \\{n}_i^2 b \\frac{{\\{n}}}{{c}}"), true, false), i as u64)).collect();
    let mut bad = Vec::new();
    for (chunk, names) in jobs.chunks(40).zip(names.chunks(40)) {
        for (r, n) in render_batch(&shared, chunk).iter().zip(names) {
            if let Err(e) = r {
                bad.push(format!("\\{n}: {e}"));
            }
        }
    }
    assert!(bad.is_empty(), "failed:\n{}", bad.join("\n"));
}
