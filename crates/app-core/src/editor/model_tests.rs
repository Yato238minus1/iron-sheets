//! Property tests of the text model against a naive reference (`Vec<char>` text, per-position arithmetic), a seeded loop
//! (no proptest in the tree), plus the grouping table and the 20 MB timing probe.
use super::buffer::LineIndex;
use super::history::History;
use super::selection::{Sel, Selections};
use super::transaction::{Assoc, Edit, EditError, Origin, Transaction};
use crate::docs::{Doc, count_text};
use std::ops::Range;
use std::time::{Duration, Instant};

struct Lcg(u64);
impl Lcg {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n
    }
}

fn piece(r: &mut Lcg) -> String {
    const BITS: [&str; 12] = ["a", "word", " ", "\n", "\n\n", "é", "字", "😀", "\u{301}", "\t", "x y", "ab\ncd"];
    (0..r.next(5)).map(|_| BITS[r.next(BITS.len())]).collect()
}

/// Byte offset of every char position (and the end).
fn offsets(chars: &[char]) -> Vec<usize> {
    let mut v = vec![0];
    for c in chars {
        v.push(v.last().copied().unwrap_or(0) + c.len_utf8());
    }
    v
}

/// A random transaction as char-coordinate edits (sorted, disjoint, adjacent allowed).
fn random_edits(r: &mut Lcg, len: usize) -> Vec<(Range<usize>, String)> {
    let k = if r.next(10) == 0 { 5 + r.next(8) } else { r.next(4) };
    let mut cuts: Vec<usize> = (0..2 * k).map(|_| r.next(len + 1)).collect();
    cuts.sort_unstable();
    cuts.chunks(2).map(|c| (c[0]..c[1], piece(r))).collect()
}

fn to_tx(edits: &[(Range<usize>, String)], offs: &[usize], origin: Origin) -> Transaction {
    Transaction { edits: edits.iter().map(|(r, s)| Edit { range: offs[r.start]..offs[r.end], insert: s.clone() }).collect(), select: None, origin }
}

fn ref_apply(chars: &mut Vec<char>, edits: &[(Range<usize>, String)]) {
    for (r, s) in edits.iter().rev() {
        chars.splice(r.clone(), s.chars());
    }
}

/// Reference `map` in chars (edits are in char coordinates, so this also checks the byte/char bookkeeping).
fn ref_map(edits: &[(Range<usize>, String)], p: usize, assoc: Assoc) -> usize {
    let mut shift = 0isize;
    for (r, s) in edits {
        let l = s.chars().count();
        if p < r.start {
            break;
        }
        if p > r.end {
            shift += l as isize - r.len() as isize;
            continue;
        }
        let after = if r.is_empty() {
            assoc == Assoc::After
        } else if p == r.start {
            false
        } else if p == r.end {
            true
        } else {
            assoc == Assoc::After
        };
        return (r.start as isize + shift) as usize + if after { l } else { 0 };
    }
    (p as isize + shift) as usize
}

/// The incrementally kept line index equals a fresh build.
fn check_index(doc: &mut Doc, ctx: &str) {
    let fresh = LineIndex::new(&doc.text);
    assert_eq!(doc.line_index(), &fresh, "{ctx}");
}

fn naive_apply(text: &str, tx: &Transaction) -> String {
    let mut t = text.to_string();
    for e in tx.edits.iter().rev() {
        t.replace_range(e.range.clone(), &e.insert);
    }
    t
}

#[test]
fn transactions_agree_with_the_reference_model() {
    for seed in 0..80 {
        let mut r = Lcg(seed + 1);
        let init: String = (0..r.next(30)).map(|_| piece(&mut r)).collect();
        let mut doc = Doc::scratch(&init);
        let mut chars: Vec<char> = init.chars().collect();
        assert_eq!(doc.line_index(), &LineIndex::new(&init));
        for step in 0..120 {
            let offs = offsets(&chars);
            let edits = random_edits(&mut r, chars.len());
            let tx = to_tx(&edits, &offs, Origin::Command);
            let old = doc.text.clone();
            let ctx = format!("seed {seed} step {step}");

            // Map: against the reference, monotone, on char boundaries of the new text.
            let mut new_chars = chars.clone();
            ref_apply(&mut new_chars, &edits);
            let new_offs = offsets(&new_chars);
            for assoc in [Assoc::Before, Assoc::After] {
                let mut last = 0;
                for p in 0..=chars.len() {
                    let m = tx.map(offs[p], assoc);
                    assert_eq!(m, new_offs[ref_map(&edits, p, assoc)], "{ctx}: map of char {p} {assoc:?}");
                    assert!(m >= last, "{ctx}: map not monotone");
                    last = m;
                }
            }

            // Selections against a naive endpoint-by-endpoint mapping.
            let sels: Vec<Sel> = (0..1 + r.next(4)).map(|_| Sel::new(offs[r.next(chars.len() + 1)], offs[r.next(chars.len() + 1)])).collect();
            let mapped = Selections::new(sels.clone(), 0).map(&tx);
            let mut expect: Vec<(usize, usize)> = Selections::new(sels, 0)
                .ranges()
                .iter()
                .map(|s| {
                    let rg = s.range();
                    let (a, b) = if rg.is_empty() { (Assoc::After, Assoc::After) } else { (Assoc::Before, Assoc::After) };
                    (tx.map(rg.start, a), tx.map(rg.end, b))
                })
                .collect();
            expect.sort_unstable();
            let mut merged: Vec<(usize, usize)> = Vec::new();
            for (s, e) in expect {
                match merged.last_mut() {
                    Some(l) if s < l.1 || (s == e && l.0 == l.1 && s == l.0) => l.1 = l.1.max(e),
                    _ => merged.push((s, e)),
                }
            }
            let got: Vec<(usize, usize)> = mapped.ranges().iter().map(|s| (s.range().start, s.range().end)).collect();
            assert_eq!(got, merged, "{ctx}: selections");
            assert!(got.contains(&(mapped.primary_range().start, mapped.primary_range().end)), "{ctx}: primary kept");

            // Apply: text, line index, counts, invert round trip.
            doc.apply(tx.clone()).expect("valid transaction");
            let inverse = tx.invert(&old);
            chars = new_chars;
            assert_eq!(doc.text, chars.iter().collect::<String>(), "{ctx}: text");
            assert_eq!(doc.text, naive_apply(&old, &tx), "{ctx}: text (naive)");
            check_index(&mut doc, &format!("{ctx}: line index"));
            assert_eq!(doc.counts(), count_text(&doc.text), "{ctx}: counts");
            assert_eq!(naive_apply(&doc.text, &inverse), old, "{ctx}: invert round trip");
            assert_eq!(inverse.invert(&doc.text).edits, tx.edits, "{ctx}: invert of invert");
        }
    }
}

#[test]
fn invalid_transactions_change_nothing() {
    let mut doc = Doc::scratch("aé\nb");
    let v = doc.version;
    for edits in [
        vec![Edit { range: 2..2, insert: "x".into() }],                                                 // inside é
        vec![Edit { range: 0..9, insert: "x".into() }],                                                 // out of bounds
        vec![Edit { range: 3..4, insert: String::new() }, Edit { range: 1..2, insert: String::new() }], // unsorted
        vec![Edit { range: 0..2, insert: String::new() }, Edit { range: 1..3, insert: String::new() }], // overlap
    ] {
        assert!(doc.apply(Transaction { edits, select: None, origin: Origin::Command }).is_err());
    }
    assert!(matches!(doc.apply(Transaction::one(2..2, "x", Origin::Command)), Err(EditError::NotCharBoundary)));
    assert_eq!((doc.text.as_str(), doc.version), ("aé\nb", v));
    assert_eq!(doc.undo().map(|_| ()), None);
}

#[test]
fn undo_then_redo_is_identity_and_the_bound_holds() {
    for seed in 0..40 {
        let mut r = Lcg(seed + 100);
        let init: String = (0..r.next(20)).map(|_| piece(&mut r)).collect();
        let mut doc = Doc::scratch(&init);
        doc.history = History::with_limits(3000, 40);
        let mut chars: Vec<char> = init.chars().collect();
        let n = 30 + r.next(60);
        for _ in 0..n {
            let offs = offsets(&chars);
            let edits = random_edits(&mut r, chars.len());
            let origin = if r.next(2) == 0 { Origin::Typing } else { Origin::Paste };
            ref_apply(&mut chars, &edits);
            doc.apply(to_tx(&edits, &offs, origin)).expect("valid");
            assert!(doc.history.undo_bytes() <= 3000 || doc.history.undo_groups() == 1, "seed {seed}: bytes {}", doc.history.undo_bytes());
            assert!(doc.history.undo_groups() <= 40);
        }
        let end = doc.text.clone();
        let mut down = vec![end.clone()];
        while doc.undo().is_some() {
            check_index(&mut doc, "");
            assert_eq!(doc.counts(), count_text(&doc.text));
            down.push(doc.text.clone());
        }
        assert_eq!(doc.history.redo_groups(), down.len() - 1, "seed {seed}");
        let mut up = vec![doc.text.clone()];
        while doc.redo().is_some() {
            up.push(doc.text.clone());
        }
        up.reverse();
        assert_eq!(up, down, "seed {seed}: redo walks the same states back");
        assert_eq!(doc.text, end);
        check_index(&mut doc, "");
        // With the bound never hit (the default history) all the way back to the start is the original text.
    }
    let mut doc = Doc::scratch("start");
    for i in 0..50 {
        doc.apply(Transaction::one(i % 5..i % 5, format!("{i}"), Origin::Command)).expect("valid");
    }
    while doc.undo().is_some() {}
    assert_eq!(doc.text, "start");
    assert!(!doc.is_dirty());
    while doc.redo().is_some() {}
    assert!(doc.is_dirty());
}

#[test]
fn an_oversized_edit_clears_the_history_instead_of_keeping_it() {
    let mut doc = Doc::scratch("ab");
    doc.history = History::with_limits(4000, 10);
    doc.apply(Transaction::one(0..0, "x", Origin::Command)).expect("valid");
    doc.apply(Transaction::one(1..3, "y".repeat(100), Origin::Replace)).expect("valid");
    assert_eq!(doc.history.undo_groups(), 2);
    doc.apply(Transaction::one(1..101, "z".repeat(5000), Origin::Replace)).expect("valid"); // above a quarter of the bound
    assert_eq!((doc.history.undo_groups(), doc.history.undo_bytes()), (0, 0));
    assert!(doc.undo().is_none());
    assert_eq!(doc.text.len(), 5001);
}

#[test]
fn no_op_transactions_leave_no_history_and_keep_it_valid() {
    let mut doc = Doc::scratch("ab");
    doc.apply(Transaction::one(1..1, "x", Origin::Command)).expect("valid");
    doc.apply(Transaction::one(0..0, "", Origin::Command)).expect("valid");
    doc.apply(Transaction {
        edits: vec![Edit { range: 0..0, insert: String::new() }, Edit { range: 2..2, insert: String::new() }],
        select: None,
        origin: Origin::Command,
    })
    .expect("valid");
    assert_eq!(doc.history.undo_groups(), 1);
    assert!(doc.undo().is_some());
    assert_eq!(doc.text, "ab");
}

#[test]
fn read_only_documents_refuse_edits_and_undo() {
    let mut doc = Doc::scratch("ab");
    doc.apply(Transaction::one(0..0, "x", Origin::Command)).expect("valid");
    doc.read_only = true;
    assert_eq!(doc.apply(Transaction::one(0..0, "y", Origin::Command)).err(), Some(EditError::ReadOnly));
    assert!(!doc.apply_edit(0..0, "y") && doc.undo().is_none());
    assert_eq!(doc.text, "xab");
}

#[test]
fn undoing_back_to_the_saved_text_is_clean() {
    let mut doc = Doc::scratch("hello");
    assert!(!doc.is_dirty());
    doc.apply(Transaction::one(5..5, "!", Origin::Command)).expect("valid");
    doc.apply(Transaction::one(0..1, "J", Origin::Command)).expect("valid");
    assert!(doc.is_dirty());
    doc.undo();
    assert!(doc.is_dirty());
    doc.undo();
    assert!(!doc.is_dirty(), "the saved text is back");
    doc.redo();
    assert!(doc.is_dirty());
}

#[test]
fn map_of_two_inserts_at_one_position_is_pinned() {
    // Two inserts at byte 1 ("ab"): the first edit takes the position, the second one only moves it.
    let tx =
        Transaction { edits: vec![Edit { range: 1..1, insert: "XX".into() }, Edit { range: 1..1, insert: "Y".into() }], select: None, origin: Origin::Command };
    assert_eq!(tx.map(1, Assoc::Before), 1);
    assert_eq!(tx.map(1, Assoc::After), 3);
    assert_eq!(tx.map(0, Assoc::After), 0);
    assert_eq!(tx.map(2, Assoc::Before), 5);
}

#[test]
fn text_changed_behind_the_history_is_not_undone_into_garbage() {
    let mut doc = Doc::scratch("abc");
    doc.apply(Transaction::one(0..0, "x", Origin::Command)).expect("valid");
    doc.text.push_str("zzz");
    doc.touch();
    assert!(doc.undo().is_none());
    assert_eq!(doc.text, "xabczzz");
    // The line index follows an edit made after the stale change.
    doc.apply(Transaction::one(0..0, "\n", Origin::Command)).expect("valid");
    check_index(&mut doc, "");
}

enum Op {
    Type(usize, &'static str),
    Del(Range<usize>),
    Paste(usize, &'static str),
    Cmd(usize, &'static str),
    Break,
}
use Op::*;

#[test]
fn grouping_rules() {
    // (name, ops with ms since the previous op, expected undo groups)
    type Row = (&'static str, Vec<(Op, u64)>, usize);
    let table: Vec<Row> = vec![
        ("typing a word is one group", vec![(Type(0, "a"), 0), (Type(1, "b"), 100), (Type(2, "c"), 100)], 1),
        ("a pause over 1 s splits", vec![(Type(0, "a"), 0), (Type(1, "b"), 100), (Type(2, "c"), 1500)], 2),
        ("a space joins, then closes the group", vec![(Type(0, "a"), 0), (Type(1, " "), 10), (Type(2, "b"), 10), (Type(3, "c"), 10)], 2),
        ("a newline closes it too", vec![(Type(0, "a"), 0), (Type(1, "\n"), 10), (Type(2, "b"), 10)], 2),
        ("a caret jump splits", vec![(Type(0, "a"), 0), (Type(1, "b"), 10), (Type(0, "c"), 10)], 2),
        ("explicit break splits", vec![(Type(0, "a"), 0), (Break, 0), (Type(1, "b"), 10)], 2),
        ("pastes never join", vec![(Paste(0, "a"), 0), (Paste(1, "b"), 10)], 2),
        ("commands never join", vec![(Cmd(0, "a"), 0), (Cmd(1, "b"), 10)], 2),
        ("a paste ends a typing group", vec![(Type(0, "a"), 0), (Paste(1, "b"), 10), (Type(2, "c"), 10)], 3),
        ("backspaces join", vec![(Type(0, "abcd"), 0), (Break, 0), (Del(3..4), 10), (Del(2..3), 10), (Del(1..2), 10)], 2),
        ("delete key joins", vec![(Type(0, "abcd"), 0), (Break, 0), (Del(1..2), 10), (Del(1..2), 10)], 2),
        ("typing then backspace are two groups", vec![(Type(0, "ab"), 0), (Del(1..2), 10)], 2),
        ("deleting away from the last deletion splits", vec![(Type(0, "abcdef"), 0), (Break, 0), (Del(5..6), 10), (Del(1..2), 10)], 3),
    ];
    for (name, ops, groups) in table {
        let mut doc = Doc::scratch("");
        let mut now = Instant::now();
        for (op, ms) in ops {
            now += Duration::from_millis(ms);
            let tx = match op {
                Type(at, s) => Transaction::one(at..at, s, Origin::Typing),
                Del(r) => Transaction::one(r, "", Origin::Typing),
                Paste(at, s) => Transaction::one(at..at, s, Origin::Paste),
                Cmd(at, s) => Transaction::one(at..at, s, Origin::Command),
                Break => {
                    doc.close_history_group();
                    continue;
                }
            };
            doc.apply_at(tx, None, now).expect("valid");
        }
        assert_eq!(doc.history.undo_groups(), groups, "{name}");
        while doc.undo().is_some() {}
        assert_eq!(doc.text, "", "{name}: all the way back");
    }
}

#[test]
fn undo_restores_the_selection_stored_with_the_edit() {
    let mut doc = Doc::scratch("hello");
    let mut tx = Transaction::one(5..5, "!", Origin::Command);
    tx.select = Some(Selections::caret(6));
    doc.apply_at(tx, Some(Selections::single(1..3)), Instant::now()).expect("valid");
    assert_eq!(doc.undo().and_then(|a| a.select).map(|s| s.primary_range()), Some(1..3));
    assert_eq!(doc.redo().and_then(|a| a.select).map(|s| s.primary_range()), Some(6..6));
}

#[test]
#[ignore = "timing probe: cargo test -p iron-sheets-app-core --release -- --ignored --nocapture apply_at_20mb"]
fn apply_at_20mb() {
    let text = "lorem ipsum dolor sit amet, consectetur\n".repeat(20 * 1024 * 1024 / 40);
    let mut doc = Doc::scratch(&text);
    let t = Instant::now();
    doc.line_index();
    eprintln!("first line index: {:?}", t.elapsed());
    let mut r = Lcg(7);
    let mut times: Vec<Duration> = (0..400)
        .map(|i| {
            let at = r.next(doc.text.len());
            let t = Instant::now();
            doc.apply(Transaction::one(at..at, if i % 7 == 0 { "\n" } else { "a" }, Origin::Typing)).expect("valid");
            t.elapsed()
        })
        .collect();
    times.sort();
    eprintln!("Doc::apply 1 char at {} bytes: median {:?}, p90 {:?}, max {:?}", doc.text.len(), times[200], times[360], times[399]);
    check_index(&mut doc, "");
    assert!(times[200] < Duration::from_micros(1500), "gate: 1.5 ms");
}
