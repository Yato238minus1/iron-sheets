//! The flat block model produced by [`crate::parse`].

use smallvec::SmallVec;
use std::hash::{Hash, Hasher};
use std::ops::{BitOr, BitOrAssign, Range};

/// A parsed note: a flat, sorted list of blocks.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Document {
    pub blocks: Vec<Block>,
    /// Number of editor lines: `'\n'` count + 1 (so `""` has 1 line, `"a\n"` has 2).
    pub line_count: u32,
}

impl Document {
    /// Headings as `(level, text, 0-based source line)`.
    pub fn outline(&self) -> impl Iterator<Item = (u8, &str, u32)> {
        self.blocks.iter().filter_map(|b| match &b.kind {
            BlockKind::Heading { level, content, .. } => Some((*level, content.text.as_str(), b.lines.start)),
            _ => None,
        })
    }

    /// Index of the first heading whose text equals `name` (case-insensitive) or whose
    /// GitHub slug equals `name` (or the slug of `name`).
    pub fn find_heading(&self, name: &str) -> Option<usize> {
        let name = name.trim();
        let lower = name.to_lowercase();
        let name_slug = comrak::Anchorizer::new().anchorize(name);
        self.blocks.iter().position(|b| match &b.kind {
            BlockKind::Heading { content, slug, .. } => {
                let t = content.text.replace('\u{FFFC}', "");
                t.trim().to_lowercase() == lower || *slug == lower || *slug == name_slug || slug_base(slug) == name_slug
            }
            _ => false,
        })
    }

    /// Index of the block whose line range contains `line` (binary search).
    pub fn block_at_line(&self, line: u32) -> Option<usize> {
        let i = self.blocks.partition_point(|b| b.lines.start <= line);
        let i = i.checked_sub(1)?;
        (line < self.blocks[i].lines.end).then_some(i)
    }
}

/// `foo-1` -> `foo` (duplicate-heading suffix removed).
fn slug_base(slug: &str) -> &str {
    match slug.rsplit_once('-') {
        Some((base, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => base,
        _ => slug,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub kind: BlockKind,
    /// 0-based source lines `[start, end)`.
    pub lines: Range<u32>,
    /// Enclosing containers, outermost first.
    pub containers: SmallVec<[Container; 2]>,
    pub spacing: Spacing,
    /// Hash of everything that affects rendering and nothing positional. `spacing` is
    /// deliberately not part of it (it is the gap *before* the block, applied by the app).
    pub hash: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Spacing {
    None,
    Tight,
    Normal,
    Loose,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    /// Unique per container instance within one parse (not stable across parses).
    pub id: u32,
    pub kind: ContainerKind,
    /// This block is the first block inside that instance.
    pub first: bool,
}

impl Hash for Container {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.kind.hash(state);
        self.first.hash(state);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ContainerKind {
    Quote,
    /// GitHub alerts and Obsidian callouts.
    Alert {
        kind: AlertKind,
        title: Option<String>,
    },
    ListItem {
        ordered: Option<u32>,
        task: Option<Task>,
    },
    Footnote {
        label: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AlertKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
    /// Any other Obsidian callout type, normalized (lowercase, hyphenated): `info`, `todo`...
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Task {
    pub checked: bool,
    /// Byte offset in the source of the char between `[` and `]`.
    pub offset: u32,
}

impl Hash for Task {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.checked.hash(state); // not the offset: positional
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BlockKind {
    Paragraph(Inlines),
    Heading { level: u8, content: Inlines, slug: String },
    Code { lang: String, text: String },
    Math { source: String },
    Typst { source: String },
    Table { aligns: Vec<Align>, header: Vec<Inlines>, rows: Vec<Vec<Inlines>> },
    Rule,
    Image { src: String, alt: String, title: Option<String>, wiki: bool },
    Embed { target: String },
    Html { text: String },
    FrontMatter { text: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

/// Inline content of a block: text plus style spans, links and inline objects.
///
/// `spans` are sorted, non-overlapping and cover `text` completely. A soft break is `' '`,
/// a hard break `'\n'`. Each [`InlineObject`] sits on one `U+FFFC` placeholder char.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Inlines {
    pub text: String,
    pub spans: Vec<Span>,
    pub links: Vec<Link>,
    pub objects: Vec<InlineObject>,
}

impl Inlines {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.objects.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Span {
    pub range: Range<u32>,
    pub style: Style,
    pub link: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Link {
    pub target: LinkTarget,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LinkTarget {
    Url(String),
    Wiki { note: String, heading: Option<String>, block: Option<String> },
    Footnote(String),
    Tag(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InlineObject {
    /// Byte offset of a single U+FFFC placeholder char in `text`.
    pub at: u32,
    pub kind: ObjectKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    Math { source: String },
    Image { src: String, alt: String, title: Option<String>, wiki: bool },
}

/// Inline style flags (bitflags-like).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Style(u16);

impl Style {
    pub const EMPTY: Style = Style(0);
    pub const BOLD: Style = Style(1 << 0);
    pub const ITALIC: Style = Style(1 << 1);
    pub const STRIKE: Style = Style(1 << 2);
    pub const CODE: Style = Style(1 << 3);
    pub const SUP: Style = Style(1 << 4);
    pub const SUB: Style = Style(1 << 5);
    pub const UNDERLINE: Style = Style(1 << 6);
    pub const HIGHLIGHT: Style = Style(1 << 7);
    pub const KBD: Style = Style(1 << 8);
    pub const TAG: Style = Style(1 << 9);
    pub const DIM: Style = Style(1 << 10);
    pub(crate) const COUNT: usize = 11;

    const NAMES: [(Style, &'static str); Self::COUNT] = [
        (Style::BOLD, "BOLD"),
        (Style::ITALIC, "ITALIC"),
        (Style::STRIKE, "STRIKE"),
        (Style::CODE, "CODE"),
        (Style::SUP, "SUP"),
        (Style::SUB, "SUB"),
        (Style::UNDERLINE, "UNDERLINE"),
        (Style::HIGHLIGHT, "HIGHLIGHT"),
        (Style::KBD, "KBD"),
        (Style::TAG, "TAG"),
        (Style::DIM, "DIM"),
    ];

    pub const fn bits(self) -> u16 {
        self.0
    }
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
    pub const fn contains(self, other: Style) -> bool {
        self.0 & other.0 == other.0
    }
    pub const fn intersects(self, other: Style) -> bool {
        self.0 & other.0 != 0
    }
    pub fn insert(&mut self, other: Style) {
        self.0 |= other.0;
    }
    pub fn remove(&mut self, other: Style) {
        self.0 &= !other.0;
    }
}

impl BitOr for Style {
    type Output = Style;
    fn bitor(self, rhs: Style) -> Style {
        Style(self.0 | rhs.0)
    }
}

impl BitOrAssign for Style {
    fn bitor_assign(&mut self, rhs: Style) {
        self.0 |= rhs.0;
    }
}

impl std::fmt::Debug for Style {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_empty() {
            return f.write_str("(none)");
        }
        let mut first = true;
        for (s, n) in Self::NAMES {
            if self.contains(s) {
                if !first {
                    f.write_str("|")?;
                }
                f.write_str(n)?;
                first = false;
            }
        }
        Ok(())
    }
}
