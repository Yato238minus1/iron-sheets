//! Mermaid diagrams: the job and result types, shared by the image cache and the diagram viewers. The renderer
//! (mermaid-rs-renderer + resvg) is `mermaid_render.rs`, compiled with the cargo feature `mermaid`.
use egui::ColorImage;
use std::sync::Arc;


/// One diagram to render. `w_pts` is the column width the picture is shrunk to, `ppp` the pixels per point.
#[derive(Clone, Debug)]
pub struct Job {
    pub source: String,
    pub dark: bool,
    pub w_pts: f32,
    pub ppp: f32,
    /// Zoomed raster for the viewers: this many pixels wide (capped), whatever `w_pts` says.
    pub px_w: Option<f32>,
}

/// What a click on a diagram label opens.
#[derive(Clone, Debug, PartialEq)]
pub enum Link {
    /// `click A "url"` (http, https, mailto, or a relative note path like in Markdown links).
    Url(String),
    /// `class A internal-link;`: the label is a note name.
    Note(String),
}

/// One `<text>` of the SVG: box in SVG units, its text lines joined with `\n`, font size, link of the node it labels
/// and, for a linked label, the bounds of the whole node (its `<g id="node-..">`) so the shape is clickable too.
#[derive(Clone, Debug)]
pub struct Label {
    pub rect: egui::Rect,
    pub text: String,
    pub size: f32,
    pub link: Option<Link>,
    pub shape: Option<egui::Rect>,
}

/// The selectable text of a diagram, in reading order (top to bottom, left to right).
#[derive(Debug)]
pub struct DiagramText {
    /// SVG size in units.
    pub size: [f32; 2],
    pub labels: Vec<Label>,
}

#[derive(Debug)]
pub struct Rendered {
    /// Size in points at scale 1 (what layout uses).
    pub dims: [u32; 2],
    pub image: ColorImage,
    pub text: Arc<DiagramText>,
}

/// `transient`: the cause is load, not the diagram, so a later layout may try again.
#[derive(Debug)]
pub struct Fail {
    pub msg: String,
    pub transient: bool,
}

pub(super) fn fail(msg: impl ToString) -> Fail {
    Fail { msg: msg.to_string(), transient: false }
}


/// Renders one diagram. Without the `mermaid` feature every job fails with a message (the layout never queues one:
/// `render_mermaid` is forced off, so a ```mermaid block stays a code block).
#[cfg(feature = "mermaid")]
pub use super::mermaid_render::render;
#[cfg(not(feature = "mermaid"))]
pub fn render(_: &Job) -> Result<Rendered, Fail> {
    Err(fail("Mermaid is not built in (cargo feature `mermaid`)"))
}

/// First line of an error, short enough for a one-line note.
pub fn short(e: &str) -> String {
    let l = e.lines().find(|l| !l.trim().is_empty()).unwrap_or(e).trim();
    if l.chars().count() > 70 { l.chars().take(70).collect::<String>() + "..." } else { l.to_string() }
}
