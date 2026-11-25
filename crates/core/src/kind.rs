//! File classification by extension.
use std::borrow::Cow;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum FileKind {
    Markdown,
    Typst,
    Excalidraw,
    Image,
    Other,
}

fn strip_excalidraw_md(name: &str) -> Option<&str> {
    const SUFFIX: &str = ".excalidraw.md";
    if cfg!(feature = "excalidraw") && name.len() > SUFFIX.len() && name.as_bytes()[name.len() - SUFFIX.len()..].eq_ignore_ascii_case(SUFFIX.as_bytes()) {
        Some(&name[..name.len() - SUFFIX.len()])
    } else {
        None
    }
}

impl FileKind {
    pub fn is_excalidraw_md(path: &Path) -> bool {
        path.file_name().and_then(|n| n.to_str()).is_some_and(|n| strip_excalidraw_md(n).is_some())
    }

    pub fn note_stem(path: &Path) -> Option<Cow<'_, str>> {
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if let Some(stem) = strip_excalidraw_md(name) {
                return Some(Cow::Borrowed(stem));
            }
        }
        path.file_stem().map(|s| s.to_string_lossy())
    }

    pub fn from_path(path: &Path) -> FileKind {
        if Self::is_excalidraw_md(path) {
            return FileKind::Excalidraw;
        }
        match path.extension().and_then(|e| e.to_str()) {
            Some(e) => Self::from_ext(e),
            None => FileKind::Other,
        }
    }

    pub fn from_ext(ext: &str) -> FileKind {
        let mut buf = [0u8; 16];
        if ext.len() > buf.len() || !ext.is_ascii() {
            return FileKind::Other;
        }
        for (i, b) in ext.bytes().enumerate() {
            buf[i] = b.to_ascii_lowercase();
        }
        match &buf[..ext.len()] {
            b"md" | b"markdown" | b"mdown" | b"mkd" => FileKind::Markdown,
            b"typ" => FileKind::Typst,
            #[cfg(feature = "excalidraw")]
            b"excalidraw" => FileKind::Excalidraw,
            b"png" | b"jpg" | b"jpeg" | b"gif" | b"webp" | b"bmp" | b"svg" => FileKind::Image,
            _ => FileKind::Other,
        }
    }

    pub fn is_note(self) -> bool {
        matches!(self, FileKind::Markdown | FileKind::Typst | FileKind::Excalidraw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(FileKind::from_path(Path::new("a/b.MD")), FileKind::Markdown);
        assert_eq!(FileKind::from_path(Path::new("b.mkd")), FileKind::Markdown);
        assert_eq!(FileKind::from_path(Path::new("b.typ")), FileKind::Typst);
        assert_eq!(FileKind::from_path(Path::new("b.JPeG")), FileKind::Image);
        assert_eq!(FileKind::from_path(Path::new("b.pdf")), FileKind::Other);
        assert_eq!(FileKind::from_path(Path::new("md")), FileKind::Other);
        assert!(FileKind::Typst.is_note());
        assert!(FileKind::Excalidraw.is_note());
        assert!(!FileKind::Image.is_note());
    }

    #[cfg(feature = "excalidraw")]
    #[test]
    fn excalidraw_kinds() {
        assert_eq!(FileKind::from_path(Path::new("arch.excalidraw")), FileKind::Excalidraw);
        assert_eq!(FileKind::from_path(Path::new("arch.EXCALIDRAW")), FileKind::Excalidraw);
        assert_eq!(FileKind::from_path(Path::new("dir/arch.excalidraw.md")), FileKind::Excalidraw);
        assert_eq!(FileKind::from_path(Path::new("dir/arch.EXCALIDRAW.MD")), FileKind::Excalidraw);
        assert_eq!(FileKind::from_path(Path::new(".excalidraw.md")), FileKind::Markdown);
        assert_eq!(FileKind::note_stem(Path::new("dir/arch.excalidraw.md")).as_deref(), Some("arch"));
        assert_eq!(FileKind::note_stem(Path::new("dir/arch.excalidraw")).as_deref(), Some("arch"));
        assert!(FileKind::is_excalidraw_md(Path::new("dir/arch.excalidraw.md")));
        assert!(!FileKind::is_excalidraw_md(Path::new("dir/arch.excalidraw")));
    }
}
