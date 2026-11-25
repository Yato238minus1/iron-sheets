//! Traits through which Typst-backed features reach the UI, so everything else builds without them.
use iron_sheets_core::config::MathSyntax;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum MathResult {
    /// No renderer is available (feature off or not initialised yet).
    Unavailable,
    /// Rendering is in flight; ask again later (a repaint is requested when ready).
    Pending,
    Ready(Arc<MathBitmap>),
    Error(Arc<str>),
}

#[derive(Debug, Clone)]
pub struct MathBitmap {
    pub key: u64,
    pub width: u32,
    pub height: u32,
    /// Premultiplied RGBA, `width * height * 4` bytes.
    pub rgba_premul: Vec<u8>,
    pub size_pt: [f32; 2],
    pub baseline_pt: f32,
    /// Rendered from LaTeX (else Typst); shown on hover in Auto mode.
    pub latex: bool,
}

pub trait MathRenderer: Send + Sync {
    fn math(&self, source: &str, display: bool, font_size_pt: f32, color: [u8; 4], pixels_per_pt: f32) -> MathResult;

    /// Selects the syntax of `source` in later `math` calls: LaTeX (converted to Typst on the
    /// worker thread), Typst, or Auto (per formula). Results are cached per syntax. Default: ignored.
    fn set_syntax(&self, _syntax: MathSyntax) {}
}

/// Default renderer: always `Unavailable`.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoMath;

impl MathRenderer for NoMath {
    fn math(&self, _source: &str, _display: bool, _font_size_pt: f32, _color: [u8; 4], _pixels_per_pt: f32) -> MathResult {
        MathResult::Unavailable
    }
}

/// Colour class of a highlighted code span; the frontend maps it to a theme colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeClass {
    Comment,
    Str,
    Num,
    Kw,
    Func,
    Type,
}

/// A styled byte range of a code block's text. Runs are sorted, never overlap, never cross a newline; gaps are plain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeRun {
    pub start: u32,
    pub end: u32,
    pub class: CodeClass,
}

#[derive(Debug, Clone)]
pub enum CodeHl {
    /// Final: no colouring (module off, unknown language, too big).
    Plain,
    /// A job is queued or running; ask again (a repaint is requested when ready). Carries the runs of the text this
    /// one was edited from, shifted onto the new text, so typing does not flash the block plain.
    Pending(Option<Arc<[CodeRun]>>),
    Runs(Arc<[CodeRun]>),
}

/// Syntax colouring of fenced code blocks (module "Code highlighting"). Results are cached by (language, text).
pub trait CodeHighlighter: Send + Sync {
    fn get(&self, lang: &str, text: &str) -> CodeHl;
}

/// Default highlighter: always `Plain`.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoHighlight;

impl CodeHighlighter for NoHighlight {
    fn get(&self, _lang: &str, _text: &str) -> CodeHl {
        CodeHl::Plain
    }
}

/// Math renderer backed by the shared Typst engine.
#[cfg(feature = "typst")]
pub struct TypstMath {
    engine: Arc<iron_sheets_typst::Engine>,
    /// Converted bitmaps by engine key, so a cache hit never copies pixels.
    converted: parking_lot::Mutex<rustc_hash::FxHashMap<u64, Arc<MathBitmap>>>,
    syntax: parking_lot::Mutex<MathSyntax>,
}

#[cfg(feature = "typst")]
impl TypstMath {
    pub fn new(engine: Arc<iron_sheets_typst::Engine>) -> Self {
        TypstMath { engine, converted: Default::default(), syntax: Default::default() }
    }
}

#[cfg(feature = "typst")]
impl TypstMath {
    fn render(&self, source: &str, latex: bool, display: bool, font_size_pt: f32, color: [u8; 4], pixels_per_pt: f32) -> MathResult {
        use iron_sheets_typst::{MathRequest, MathState};
        match self.engine.math(&MathRequest { source, display, font_size_pt, color, pixels_per_pt, latex }) {
            MathState::Pending => MathResult::Pending,
            MathState::Error(e) => MathResult::Error(e),
            MathState::Ready(img) => {
                let mut map = self.converted.lock();
                if map.len() > 4096 {
                    map.clear();
                }
                let bitmap = map.entry(img.key).or_insert_with(|| {
                    Arc::new(MathBitmap {
                        key: img.key,
                        width: img.width,
                        height: img.height,
                        rgba_premul: img.rgba.clone(),
                        size_pt: img.size_pt,
                        baseline_pt: img.baseline_pt,
                        latex,
                    })
                });
                MathResult::Ready(bitmap.clone())
            }
        }
    }
}

#[cfg(feature = "typst")]
/// Auto mode: a formula with a `\name` command is LaTeX for sure, so its error is shown instead of retrying it as
/// Typst (where `\foo` would silently render as an escape plus "oo" = infinity).
fn may_fall_back(source: &str, latex: bool) -> bool {
    !latex || !source.as_bytes().windows(2).any(|w| w[0] == b'\\' && w[1].is_ascii_alphabetic())
}

#[cfg(feature = "typst")]
impl MathRenderer for TypstMath {
    fn set_syntax(&self, syntax: MathSyntax) {
        *self.syntax.lock() = syntax;
    }

    fn math(&self, source: &str, display: bool, font_size_pt: f32, color: [u8; 4], pixels_per_pt: f32) -> MathResult {
        let syntax = *self.syntax.lock();
        let latex = match syntax {
            MathSyntax::Auto => iron_sheets_md::looks_like_latex(source),
            s => s == MathSyntax::Latex,
        };
        let r = self.render(source, latex, display, font_size_pt, color, pixels_per_pt);
        // Auto: a formula that fails in the detected syntax gets one try in the other. Both outcomes are
        // cached by the engine (the syntax is part of its key), so a failing formula costs two lookups.
        if syntax == MathSyntax::Auto && may_fall_back(source, latex) && matches!(r, MathResult::Error(_)) {
            let alt = self.render(source, !latex, display, font_size_pt, color, pixels_per_pt);
            if !matches!(alt, MathResult::Error(_)) {
                return alt;
            }
        }
        r
    }
}

#[cfg(all(test, feature = "typst"))]
mod fallback_tests {
    #[test]
    fn a_latex_command_is_never_retried_as_typst() {
        assert!(!super::may_fall_back(r"\foo + 1", true));
        assert!(super::may_fall_back("x_{i}", true), "LaTeX only by ^{{ / _{{: may still be Typst");
        assert!(super::may_fall_back("x + 1", false), "Typst may fall back to LaTeX");
    }
}
