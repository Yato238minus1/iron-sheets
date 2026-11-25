//! `Edit`, `Transaction` and `Origin`: the one way text changes (`Doc::apply`).
use super::selection::Selections;
use std::ops::Range;

/// Replace `range` (coordinates of the text BEFORE the transaction) with `insert`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub range: Range<usize>,
    pub insert: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Typing,
    Ime,
    Paste,
    Command,
    Replace,
    Undo,
    Remote(u32),
}

/// Which side a position sticks to when text is inserted exactly at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assoc {
    Before,
    After,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// Edits are not sorted, or overlap.
    Unsorted,
    OutOfBounds,
    NotCharBoundary,
    /// The result would not fit the `u32` line index.
    TooLarge,
    ReadOnly,
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            EditError::Unsorted => "edits overlap or are out of order",
            EditError::OutOfBounds => "edit outside the text",
            EditError::NotCharBoundary => "edit inside a character",
            EditError::TooLarge => "text too large",
            EditError::ReadOnly => "document is read-only",
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Transaction {
    /// Sorted and disjoint.
    pub edits: Vec<Edit>,
    /// Selections after the transaction, if the sender wants them set.
    pub select: Option<Selections>,
    pub origin: Origin,
}

impl Transaction {
    pub fn one(range: Range<usize>, insert: impl Into<String>, origin: Origin) -> Transaction {
        Transaction { edits: vec![Edit { range, insert: insert.into() }], select: None, origin }
    }

    /// Err unless every edit is in bounds on char boundaries of `text`, sorted and disjoint.
    pub fn validate(&self, text: &str) -> Result<(), EditError> {
        let mut prev_end = 0;
        for e in &self.edits {
            if e.range.start > e.range.end || e.range.start < prev_end {
                return Err(EditError::Unsorted);
            }
            if e.range.end > text.len() {
                return Err(EditError::OutOfBounds);
            }
            if !text.is_char_boundary(e.range.start) || !text.is_char_boundary(e.range.end) {
                return Err(EditError::NotCharBoundary);
            }
            prev_end = e.range.end;
        }
        Ok(())
    }

    /// Change of the text length in bytes.
    pub fn delta(&self) -> isize {
        self.edits.iter().map(|e| e.insert.len() as isize - e.range.len() as isize).sum()
    }

    /// Where `pos` (a byte offset in the old text) is in the new text. A position inside a replaced range goes by `assoc`;
    /// at its start it stays before the new text, at its end it moves behind it.
    pub fn map(&self, pos: usize, assoc: Assoc) -> usize {
        let mut shift = 0isize;
        for e in &self.edits {
            let (s, t) = (e.range.start, e.range.end);
            if pos < s {
                break;
            }
            if pos > t {
                shift += e.insert.len() as isize - (t - s) as isize;
                continue;
            }
            let after = if s == t {
                assoc == Assoc::After
            } else if pos == s {
                false
            } else if pos == t {
                true
            } else {
                assoc == Assoc::After
            };
            return (s as isize + shift) as usize + if after { e.insert.len() } else { 0 };
        }
        (pos as isize + shift) as usize
    }

    /// The transaction that undoes this one, in the coordinates of the text after it; `text` is the text before it.
    /// Origin `Undo`, no selection (the history stores those).
    pub fn invert(&self, text: &str) -> Transaction {
        let mut shift = 0isize;
        let edits = self
            .edits
            .iter()
            .map(|e| {
                let start = (e.range.start as isize + shift) as usize;
                shift += e.insert.len() as isize - e.range.len() as isize;
                Edit { range: start..start + e.insert.len(), insert: text[e.range.clone()].to_string() }
            })
            .collect();
        Transaction { edits, select: None, origin: Origin::Undo }
    }
}
