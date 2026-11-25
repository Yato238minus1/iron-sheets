//! Format detection and payload extraction for `.excalidraw` and `.excalidraw.md`.

use super::{ExcalidrawError, MAX_INPUT_BYTES};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedFileRef {
    pub file_id: String,
    pub target: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawingFormat<'a> {
    RawJson(&'a str),
    CompressedJson(&'a str),
    JsonFence(&'a str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedDrawing<'a> {
    pub format: DrawingFormat<'a>,
    pub embedded_files: Vec<EmbeddedFileRef>,
}

/// Extracts the scene JSON or compressed-json payload (plus Obsidian `## Embedded Files` refs)
/// from either raw `.excalidraw` JSON or Obsidian `.excalidraw.md` Markdown.
pub fn extract_drawing(input: &str) -> Result<ExtractedDrawing<'_>, ExcalidrawError> {
    if input.len() > MAX_INPUT_BYTES {
        return Err(ExcalidrawError::TooLarge);
    }
    let body = input.strip_prefix('\u{feff}').unwrap_or(input);
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err(ExcalidrawError::InvalidFormat("empty file".into()));
    }
    if trimmed.starts_with('{') {
        return Ok(ExtractedDrawing {
            format: DrawingFormat::RawJson(trimmed),
            embedded_files: Vec::new(),
        });
    }

    let after_fm = skip_front_matter(body);
    let embedded_files = extract_embedded_files(after_fm);

    // Prefer a fence inside a `# Drawing` / `## Drawing` section; fall back to first drawing fence.
    let search_from = find_drawing_section(after_fm).unwrap_or(after_fm);
    if let Some(fmt) = find_fenced_block(search_from) {
        return Ok(ExtractedDrawing { format: fmt, embedded_files });
    }
    if search_from.len() != after_fm.len()
        && let Some(fmt) = find_fenced_block(after_fm)
    {
        return Ok(ExtractedDrawing { format: fmt, embedded_files });
    }

    Err(ExcalidrawError::InvalidFormat("no JSON or compressed-json drawing block found".into()))
}

fn skip_front_matter(s: &str) -> &str {
    let first_line_end = s.find('\n').unwrap_or(s.len());
    let first_line = s[..first_line_end].trim_end_matches('\r').trim();
    if first_line != "---" {
        return s;
    }
    let mut pos = if first_line_end < s.len() { first_line_end + 1 } else { return s };
    while pos < s.len() {
        let next_nl = s[pos..].find('\n').map_or(s.len(), |i| pos + i);
        let line = s[pos..next_nl].trim_end_matches('\r').trim();
        if line == "---" || line == "..." {
            return if next_nl < s.len() { &s[next_nl + 1..] } else { "" };
        }
        pos = if next_nl < s.len() { next_nl + 1 } else { s.len() };
    }
    s
}

fn find_drawing_section(s: &str) -> Option<&str> {
    let mut pos = 0;
    while pos < s.len() {
        let next_nl = s[pos..].find('\n').map_or(s.len(), |i| pos + i);
        let line = s[pos..next_nl].trim_end_matches('\r').trim();
        if line == "## Drawing" || line == "# Drawing" {
            return Some(if next_nl < s.len() { &s[next_nl + 1..] } else { "" });
        }
        pos = if next_nl < s.len() { next_nl + 1 } else { s.len() };
    }
    None
}

fn find_fenced_block(s: &str) -> Option<DrawingFormat<'_>> {
    let mut pos = 0;
    while pos < s.len() {
        let next_nl = s[pos..].find('\n').map_or(s.len(), |i| pos + i);
        let line = s[pos..next_nl].trim_end_matches('\r').trim();
        if let Some(info) = line.strip_prefix("```") {
            let lang = info.trim();
            let is_compressed = lang.eq_ignore_ascii_case("compressed-json");
            let is_json = lang.eq_ignore_ascii_case("json");
            if is_compressed || is_json {
                let content_start = if next_nl < s.len() { next_nl + 1 } else { return None };
                let mut scan = content_start;
                while scan < s.len() {
                    let end_nl = s[scan..].find('\n').map_or(s.len(), |i| scan + i);
                    let cline = s[scan..end_nl].trim_end_matches('\r').trim();
                    if cline.starts_with("```") {
                        let payload = s[content_start..scan].trim();
                        return Some(if is_compressed {
                            DrawingFormat::CompressedJson(payload)
                        } else {
                            DrawingFormat::JsonFence(payload)
                        });
                    }
                    scan = if end_nl < s.len() { end_nl + 1 } else { s.len() };
                }
                return None;
            }
        }
        pos = if next_nl < s.len() { next_nl + 1 } else { s.len() };
    }
    None
}

fn extract_embedded_files(s: &str) -> Vec<EmbeddedFileRef> {
    let mut out = Vec::new();
    let mut in_section = false;
    let mut pos = 0;
    while pos < s.len() {
        let next_nl = s[pos..].find('\n').map_or(s.len(), |i| pos + i);
        let line = s[pos..next_nl].trim_end_matches('\r').trim();
        if line == "## Embedded Files" {
            in_section = true;
        } else if in_section {
            if line.starts_with('#') || line.starts_with("```") {
                break;
            }
            if !line.is_empty()
                && line != "%%"
                && let Some((id, rest)) = line.split_once(':')
            {
                let id = id.trim();
                let rest = rest.trim();
                if !id.is_empty()
                    && let Some(inner) = rest.strip_prefix("[[")
                    && let Some(end) = inner.find("]]")
                {
                    let target = inner[..end].split('|').next().unwrap_or("").trim();
                    if !target.is_empty() {
                        out.push(EmbeddedFileRef {
                            file_id: id.to_string(),
                            target: target.to_string(),
                        });
                    }
                }
            }
        }
        pos = if next_nl < s.len() { next_nl + 1 } else { s.len() };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_legacy_json_and_missing_fence_errors() {
        let md = "---\nexcalidraw-plugin: parsed\n---\n# Excalidraw Data\n## Drawing\n```json\n{\"type\":\"excalidraw\",\"elements\":[]}\n```\n";
        let ex = extract_drawing(md).unwrap();
        assert_eq!(ex.format, DrawingFormat::JsonFence("{\"type\":\"excalidraw\",\"elements\":[]}"));

        assert!(matches!(extract_drawing("not json"), Err(ExcalidrawError::InvalidFormat(_))));
        assert!(matches!(extract_drawing(""), Err(ExcalidrawError::InvalidFormat(_))));
    }
}
