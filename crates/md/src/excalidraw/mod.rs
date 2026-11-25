//! Pure-Rust Excalidraw (`.excalidraw` and `.excalidraw.md`) parser and sanitized SVG emitter.

pub mod extract;
pub mod lzstring;
pub mod schema;
pub mod svg;

pub use extract::{DrawingFormat, EmbeddedFileRef, ExtractedDrawing, extract_drawing};
pub use schema::{ExcalidrawAppState, ExcalidrawElement, ExcalidrawFile, ExcalidrawScene, FontFamily, Roundness};
pub use svg::{RenderOptions, RenderedSvg, scene_to_svg};

/// Maximum accepted raw `.excalidraw` or `.excalidraw.md` file size (16 MiB).
pub const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;
/// Maximum decompressed UTF-16 code units from `lz-string` (32M units = 64 MiB UTF-16).
pub const MAX_DECOMPRESSED_UTF16_UNITS: usize = 32 * 1024 * 1024;
/// Maximum elements permitted in a single Excalidraw scene.
pub const MAX_ELEMENTS: usize = 20_000;
/// Maximum points permitted in a single line/arrow/freedraw element.
pub const MAX_POINTS_PER_ELEMENT: usize = 100_000;
/// Maximum total points across all elements in a scene.
pub const MAX_TOTAL_POINTS: usize = 500_000;
/// Maximum emitted SVG output size in bytes (16 MiB).
pub const MAX_SVG_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExcalidrawError {
    TooLarge,
    InvalidFormat(String),
    DecompressionFailed(String),
    DecompressionLimit,
    InvalidJson(String),
    TooManyElements,
    TooManyPoints,
    EmptyScene,
    SvgTooLarge,
}

impl std::fmt::Display for ExcalidrawError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => write!(f, "Excalidraw file exceeds {} MB limit", MAX_INPUT_BYTES / (1024 * 1024)),
            Self::InvalidFormat(msg) => write!(f, "Invalid Excalidraw format: {msg}"),
            Self::DecompressionFailed(msg) => write!(f, "Failed to decompress Excalidraw data: {msg}"),
            Self::DecompressionLimit => write!(f, "Decompressed Excalidraw data exceeds safety limit"),
            Self::InvalidJson(msg) => write!(f, "Invalid Excalidraw JSON: {msg}"),
            Self::TooManyElements => write!(f, "Excalidraw scene exceeds {MAX_ELEMENTS} element limit"),
            Self::TooManyPoints => write!(f, "Excalidraw scene exceeds point count limit"),
            Self::EmptyScene => write!(f, "Empty Excalidraw drawing"),
            Self::SvgTooLarge => write!(f, "Generated SVG exceeds size limit"),
        }
    }
}

impl std::error::Error for ExcalidrawError {}

#[derive(Debug, Clone)]
pub struct ParsedDrawing {
    pub scene: ExcalidrawScene,
    pub embedded_files: Vec<EmbeddedFileRef>,
}

/// Extract, decompress (if needed), and parse an `.excalidraw` or `.excalidraw.md` document.
pub fn parse_drawing(input: &str) -> Result<ParsedDrawing, ExcalidrawError> {
    let extracted = extract_drawing(input)?;
    let owned_json;
    let json_str = match extracted.format {
        DrawingFormat::RawJson(s) => s,
        DrawingFormat::JsonFence(s) => s,
        DrawingFormat::CompressedJson(b64) => {
            owned_json = lzstring::decompress_from_base64(b64, MAX_DECOMPRESSED_UTF16_UNITS)?;
            &owned_json
        }
    };
    let scene = schema::parse_scene_json(json_str)?;
    Ok(ParsedDrawing {
        scene,
        embedded_files: extracted.embedded_files,
    })
}

/// Parse an `.excalidraw` or `.excalidraw.md` document into a scene.
pub fn parse_scene(input: &str) -> Result<ExcalidrawScene, ExcalidrawError> {
    parse_drawing(input).map(|d| d.scene)
}

/// Parse an `.excalidraw` or `.excalidraw.md` document and emit a sanitized SVG.
pub fn render_to_svg(input: &str, opts: &RenderOptions) -> Result<RenderedSvg, ExcalidrawError> {
    let parsed = parse_drawing(input)?;
    let mut rendered = scene_to_svg(&parsed.scene, &parsed.embedded_files, opts)?;
    rendered.embedded_files = parsed.embedded_files;
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_raw_and_compressed_obsidian_md() {
        let json = r##"{
            "type": "excalidraw",
            "version": 2,
            "elements": [
                {"id": "r1", "type": "rectangle", "x": 10, "y": 20, "width": 120, "height": 60, "strokeColor": "#1e1e1e", "backgroundColor": "#a5d8ff", "fillStyle": "hachure", "seed": 42},
                {"id": "t1", "type": "text", "x": 20, "y": 35, "width": 100, "height": 25, "text": "Hello <World> & Co", "fontSize": 20, "fontFamily": 1, "seed": 7}
            ],
            "appState": { "viewBackgroundColor": "#ffffff" }
        }"##;

        let raw_svg = render_to_svg(json, &RenderOptions::default()).unwrap();
        assert!(raw_svg.svg.contains("<svg "));
        assert!(raw_svg.svg.contains("Hello &lt;World&gt; &amp; Co"));
        assert!(!raw_svg.svg.contains("<World>"));

        let compressed = lzstring::compress_to_base64(json);
        let wrapped = compressed
            .as_bytes()
            .chunks(256)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join("\n\n");
        let md = format!(
            "\u{feff}---\r\nexcalidraw-plugin: parsed\r\n---\r\n# Excalidraw Data\r\n## Text Elements\r\nHello ^t1\r\n## Embedded Files\r\nabc123: [[assets/photo.png]]\r\n%%\r\n## Drawing\r\n```compressed-json\r\n{wrapped}\r\n```\r\n%%\r\n"
        );
        let md_svg = render_to_svg(&md, &RenderOptions::default()).unwrap();
        assert_eq!(raw_svg.svg, md_svg.svg);
        assert_eq!(md_svg.embedded_files, vec![EmbeddedFileRef { file_id: "abc123".into(), target: "assets/photo.png".into() }]);
    }

    #[test]
    fn performance_5000_elements_within_budget() {
        use std::fmt::Write;
        let mut json = String::from(r#"{"type":"excalidraw","version":2,"elements":["#);
        for i in 0..5000 {
            if i > 0 {
                json.push(',');
            }
            let x = (i % 100) * 40;
            let y = (i / 100) * 40;
            let _ = write!(
                json,
                r##"{{"id":"e{i}","type":"rectangle","x":{x},"y":{y},"width":30,"height":30,"strokeColor":"#1e1e1e","backgroundColor":"transparent","seed":{}}}"##,
                i + 1
            );
        }
        json.push_str(r##"],"appState":{"viewBackgroundColor":"#ffffff"}}"##);

        let start = std::time::Instant::now();
        let out = render_to_svg(&json, &RenderOptions::default()).unwrap();
        let elapsed = start.elapsed();
        assert_eq!(out.element_count, 5000);
        assert!(elapsed.as_millis() < 500, "5000-element render took {:?}", elapsed);
    }
}
