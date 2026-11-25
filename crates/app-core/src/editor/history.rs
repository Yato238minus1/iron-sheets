//! Undo/redo of one document: deltas, not snapshots; typing is grouped (docs/plans/text-widget.md, section 3).
use super::selection::Selections;
use super::transaction::{Edit, Origin, Transaction};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Typing within this time of the previous edit may join its group.
const GROUP_WINDOW: Duration = Duration::from_secs(1);
const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_GROUPS: usize = 10_000;
/// Cost of an edit besides its text.
const EDIT_COST: usize = 64;

#[derive(Debug, Clone)]
struct Entry {
    /// Applying it moves the text one step in this stack's direction.
    tx: Transaction,
    /// Selections to set after applying `tx`, and (for the opposite stack) after undoing that again.
    restore: Option<Selections>,
    other: Option<Selections>,
    origin: Origin,
    at: Instant,
    /// No later typing may join this group.
    closed: bool,
    bytes: usize,
}

fn cost(tx: &Transaction) -> usize {
    tx.edits.iter().map(|e| e.insert.len() + e.range.len() + EDIT_COST).sum()
}

/// If `new` (the inverse of a typing edit) continues the group of `last`, the merged inverse edit.
fn merge(last: &Transaction, new: &Transaction) -> Option<Edit> {
    let (Some(p), Some(n)) = (last.edits.first().filter(|_| last.edits.len() == 1), new.edits.first().filter(|_| new.edits.len() == 1)) else {
        return None;
    };
    // Typed right after the previous typed text.
    if n.insert.is_empty() && !n.range.is_empty() && !p.range.is_empty() && n.range.start == p.range.end {
        return Some(Edit { range: p.range.start..n.range.end, insert: p.insert.clone() });
    }
    // Deleting: the new deletion is a pure removal next to the previous pure removal.
    if n.range.is_empty() && !n.insert.is_empty() && p.range.is_empty() {
        let at = n.range.start;
        if at + n.insert.len() == p.range.start {
            return Some(Edit { range: at..at, insert: format!("{}{}", n.insert, p.insert) }); // backspace
        }
        if at == p.range.start {
            return Some(Edit { range: at..at, insert: format!("{}{}", p.insert, n.insert) }); // delete key
        }
    }
    None
}

#[derive(Debug)]
pub struct History {
    undo: VecDeque<Entry>,
    redo: VecDeque<Entry>,
    undo_bytes: usize,
    redo_bytes: usize,
    max_bytes: usize,
    max_groups: usize,
}

impl Default for History {
    fn default() -> History {
        History::with_limits(MAX_BYTES, MAX_GROUPS)
    }
}

impl History {
    pub fn with_limits(max_bytes: usize, max_groups: usize) -> History {
        History { undo: VecDeque::new(), redo: VecDeque::new(), undo_bytes: 0, redo_bytes: 0, max_bytes, max_groups }
    }

    pub fn undo_groups(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_groups(&self) -> usize {
        self.redo.len()
    }

    pub fn undo_bytes(&self) -> usize {
        self.undo_bytes
    }

    pub fn redo_bytes(&self) -> usize {
        self.redo_bytes
    }

    pub fn clear(&mut self) {
        *self = History::with_limits(self.max_bytes, self.max_groups);
    }

    /// The next typing edit starts a new group (caret jump, save, ...).
    pub fn close_group(&mut self) {
        if let Some(e) = self.undo.back_mut() {
            e.closed = true;
        }
    }

    /// Record that `forward` was applied and `inverse` (from `Transaction::invert`) undoes it. Drops the redo stack.
    pub fn record(&mut self, forward: &Transaction, inverse: Transaction, before: Option<Selections>, now: Instant) {
        self.redo.clear();
        self.redo_bytes = 0;
        // shortcut: one edit above a quarter of the bound (replace-all on a huge note) is not undoable, so the document
        // does not keep that much text; upgrade when the history can spill to disk.
        if cost(&inverse) > self.max_bytes / 4 {
            self.clear();
            return;
        }
        let typing = forward.origin == Origin::Typing;
        let breaks = forward.edits.iter().any(|e| e.insert.chars().any(char::is_whitespace));
        if let Some(last) = self.undo.back_mut().filter(|l| typing && l.origin == Origin::Typing && !l.closed && now.duration_since(l.at) < GROUP_WINDOW) {
            if let Some(edit) = merge(&last.tx, &inverse) {
                last.tx.edits = vec![edit];
                last.other = forward.select.clone();
                last.at = now;
                last.closed = breaks;
                let bytes = cost(&last.tx);
                self.undo_bytes = self.undo_bytes - last.bytes + bytes;
                last.bytes = bytes;
                self.trim();
                return;
            }
        }
        let bytes = cost(&inverse);
        self.undo_bytes += bytes;
        self.undo.push_back(Entry {
            tx: inverse,
            restore: before,
            other: forward.select.clone(),
            origin: forward.origin,
            at: now,
            closed: breaks || !typing,
            bytes,
        });
        self.trim();
    }

    /// Take the newest entry of the undo (or redo) stack, run `apply` on its transaction (it returns the inverse of what it
    /// applied, or None on failure) and move the entry to the other stack. Returns the selections to restore.
    pub fn step(&mut self, redo: bool, now: Instant, apply: impl FnOnce(&Transaction) -> Option<Transaction>) -> Option<Option<Selections>> {
        let e = if redo { self.redo.pop_back() } else { self.undo.pop_back() }?;
        if redo {
            self.redo_bytes -= e.bytes;
        } else {
            self.undo_bytes -= e.bytes;
        }
        let inverse = apply(&e.tx)?;
        let bytes = cost(&inverse);
        let back = Entry { tx: inverse, restore: e.other, other: e.restore.clone(), origin: Origin::Undo, at: now, closed: true, bytes };
        if redo {
            self.undo_bytes += bytes;
            self.undo.push_back(back);
        } else {
            self.redo_bytes += bytes;
            self.redo.push_back(back);
        }
        self.trim();
        Some(e.restore)
    }

    /// Drop the oldest groups above the bounds; the newest group stays even when alone above the byte bound.
    fn trim(&mut self) {
        for (q, bytes) in [(&mut self.undo, &mut self.undo_bytes), (&mut self.redo, &mut self.redo_bytes)] {
            while q.len() > self.max_groups || (*bytes > self.max_bytes && q.len() > 1) {
                if let Some(e) = q.pop_front() {
                    *bytes -= e.bytes;
                }
            }
        }
    }
}
