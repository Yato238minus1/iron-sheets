//! Selections: byte offsets on char boundaries, sorted and non-overlapping.
use super::transaction::{Assoc, Transaction};
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sel {
    pub anchor: usize,
    pub head: usize,
    /// Visual column kept while moving up/down.
    pub goal_x: Option<f32>,
}

impl Sel {
    pub fn new(anchor: usize, head: usize) -> Sel {
        Sel { anchor, head, goal_x: None }
    }

    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

/// One element in 99.9 % of cases. Invariant: sorted by start, no two overlap (two empty ones never share a place).
#[derive(Debug, Clone, PartialEq)]
pub struct Selections {
    ranges: Vec<Sel>,
    primary: usize,
}

impl Selections {
    /// Sorts and merges overlapping ranges; `primary` indexes `ranges` as given (an empty list gives a caret at 0).
    pub fn new(ranges: Vec<Sel>, primary: usize) -> Selections {
        let mut tagged: Vec<(Sel, bool)> = ranges.into_iter().enumerate().map(|(i, s)| (s, i == primary)).collect();
        tagged.sort_by_key(|(s, _)| (s.range().start, s.range().end));
        let mut out: Vec<(Sel, bool)> = Vec::with_capacity(tagged.len());
        for (s, p) in tagged {
            match out.last_mut() {
                Some((last, lp)) if s.range().start < last.range().end || (s.is_empty() && last.is_empty() && s.head == last.head) => {
                    let end = last.range().end.max(s.range().end);
                    let start = last.range().start;
                    *last = if s.head >= s.anchor {
                        Sel { anchor: start, head: end, goal_x: s.goal_x }
                    } else {
                        Sel { anchor: end, head: start, goal_x: s.goal_x }
                    };
                    *lp |= p;
                }
                _ => out.push((s, p)),
            }
        }
        if out.is_empty() {
            return Selections { ranges: vec![Sel::new(0, 0)], primary: 0 };
        }
        let primary = out.iter().position(|&(_, p)| p).unwrap_or(0);
        Selections { ranges: out.into_iter().map(|(s, _)| s).collect(), primary }
    }

    pub fn single(range: Range<usize>) -> Selections {
        Selections { ranges: vec![Sel::new(range.start, range.end)], primary: 0 }
    }

    pub fn caret(at: usize) -> Selections {
        Selections::single(at..at)
    }

    pub fn ranges(&self) -> &[Sel] {
        &self.ranges
    }

    pub fn primary(&self) -> &Sel {
        &self.ranges[self.primary]
    }

    pub fn primary_range(&self) -> Range<usize> {
        self.primary().range()
    }

    /// Where the selections are after `tx`: the start of a range sticks before text inserted at it, the end and a caret after it.
    pub fn map(&self, tx: &Transaction) -> Selections {
        let ranges = self
            .ranges
            .iter()
            .map(|s| {
                let r = s.range();
                let (a, b) = if r.is_empty() { (Assoc::After, Assoc::After) } else { (Assoc::Before, Assoc::After) };
                let (lo, hi) = (tx.map(r.start, a), tx.map(r.end, b));
                let (anchor, head) = if s.head >= s.anchor { (lo, hi) } else { (hi, lo) };
                Sel { anchor, head, goal_x: s.goal_x }
            })
            .collect();
        Selections::new(ranges, self.primary)
    }
}
