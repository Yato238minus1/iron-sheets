//! `LineIndex`: line starts of a text, kept in step with every edit (see docs/plans/text-widget.md, section 2).
use super::transaction::Edit;

/// Above this many edits in one transaction the index is rebuilt from the new text (one pass) instead of shifted per edit.
const REBUILD_ABOVE: usize = 4;

/// Byte offset of the first byte of every line; line 0 starts at 0, a `\n` at byte `i` starts a line at `i + 1`.
/// shortcut: `u32` limits a note to 4 GB and a shift is O(lines after the edit); upgrade to a chunked/Fenwick index
/// only if the 20 MB keystroke gate fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineIndex {
    starts: Vec<u32>,
}

impl LineIndex {
    pub fn new(text: &str) -> LineIndex {
        let starts = std::iter::once(0).chain(text.bytes().enumerate().filter(|&(_, b)| b == b'\n').map(|(i, _)| i as u32 + 1)).collect();
        LineIndex { starts }
    }

    pub fn lines(&self) -> usize {
        self.starts.len()
    }

    /// Line containing byte `byte` (a position past the end belongs to the last line).
    pub fn line_of(&self, byte: usize) -> usize {
        self.starts.partition_point(|&s| s as usize <= byte).saturating_sub(1)
    }

    /// First byte of `line` (the text length for a line past the end is the caller's business: this clamps to the last line).
    pub fn start_of(&self, line: usize) -> usize {
        self.starts[line.min(self.starts.len() - 1)] as usize
    }

    /// The text changed by `edits` (coordinates of the old text); `new_text` is the result.
    pub(crate) fn apply(&mut self, edits: &[Edit], new_text: &str) {
        if edits.len() > REBUILD_ABOVE {
            *self = LineIndex::new(new_text);
            return;
        }
        // Right to left: an edit does not move the coordinates of the ones before it.
        for e in edits.iter().rev() {
            let lo = self.starts.partition_point(|&s| s as usize <= e.range.start);
            let hi = self.starts.partition_point(|&s| s as usize <= e.range.end);
            let added = e.insert.matches('\n').count();
            let at = e.range.start;
            self.starts.splice(lo..hi, e.insert.match_indices('\n').map(|(i, _)| (at + i + 1) as u32));
            let delta = e.insert.len() as isize - e.range.len() as isize;
            if delta != 0 {
                for s in &mut self.starts[lo + added..] {
                    *s = (*s as isize + delta) as u32;
                }
            }
        }
    }
}
