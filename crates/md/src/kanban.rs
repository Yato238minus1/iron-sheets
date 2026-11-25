//! Obsidian Kanban boards as plain Markdown: `parse` reads lanes and cards (byte ranges into the text) and every edit
//! returns [`Edit`]s, so the text outside the edit stays byte for byte as it was (front matter, `**Complete**`,
//! the `***` + `## Archive` tail, the `%% kanban:settings` block).
use std::ops::Range;

#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    /// Card text; continuation lines (including nested items and blank lines) have their indent of up to two columns removed.
    pub text: String,
    /// The checkbox is ticked (any character but a space; a bullet without a checkbox is not done).
    pub done: bool,
    /// The whole card: first line plus indented continuation lines, without the final newline.
    pub range: Range<usize>,
    /// Where the text starts (after `- [ ] ` or `- `).
    text_start: usize,
    /// The character inside `[ ]`; `None` for a plain bullet.
    box_char: Option<Range<usize>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Lane {
    pub title: String,
    /// Start of the `## ` heading line; the lane runs from here to the next lane (or the tail).
    start: usize,
    title_range: Range<usize>,
    /// End of the heading line (without newline).
    heading_end: usize,
    /// Start of the `**Complete**` line, if the lane has one: new cards go in before it.
    complete_at: Option<usize>,
    pub cards: Vec<Card>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Board {
    pub lanes: Vec<Lane>,
    /// Start of the `***` / `%% kanban:settings` tail, or the text length.
    end: usize,
}

/// A replacement of a byte range. Lists of edits are ordered back to front, so applying them one by one is valid.
#[derive(Debug, Clone, PartialEq)]
pub struct Edit {
    pub range: Range<usize>,
    pub insert: String,
}

/// True when the (closed) front matter has the key `kanban-plugin` (any value).
pub fn is_board(text: &str) -> bool {
    let Some(rest) = text.strip_prefix("---\n") else { return false };
    let mut found = false;
    for line in rest.lines() {
        if line.trim_end() == "---" {
            return found;
        }
        found |= line.starts_with("kanban-plugin:");
    }
    false
}

/// `- [x] text`, `- [ ]`, `* text`, `+ [?] text`: (done, box char range, text start) for a line starting a card.
fn marker(line: &str) -> Option<(bool, Option<Range<usize>>, usize)> {
    let rest = line.strip_prefix(['-', '*', '+'])?.strip_prefix(' ')?;
    if let Some(inner) = rest.strip_prefix('[') {
        let c = inner.chars().next()?;
        let after = &inner[c.len_utf8()..];
        if after == "]" || after.starts_with("] ") {
            let at = 3;
            return Some((c != ' ', Some(at..at + c.len_utf8()), at + c.len_utf8() + if after == "]" { 1 } else { 2 }));
        }
    }
    Some((false, None, 2))
}

pub fn parse(text: &str) -> Board {
    let mut board = Board { lanes: Vec::new(), end: text.len() };
    let mut pos = 0;
    if let Some(rest) = text.strip_prefix("---\n") {
        pos = 4;
        for line in rest.split_inclusive('\n') {
            pos += line.len();
            if line.trim_end() == "---" {
                break;
            }
        }
    }
    // `open`: the last card can still continue; `blanks`: blank lines seen since its last line.
    let (mut open, mut blanks, mut in_fence) = (false, 0usize, false);
    for line in text[pos..].split_inclusive('\n') {
        let start = pos;
        pos += line.len();
        let body = line.strip_suffix('\n').unwrap_or(line);
        let end = start + body.len();
        if body.trim().is_empty() {
            blanks += usize::from(open);
            continue;
        }
        let indented = body.starts_with([' ', '\t']);
        let t = body.trim_start();
        let fence = t.starts_with("```") || t.starts_with("~~~");
        if !in_fence && !indented {
            open = false;
            blanks = 0;
            if body.trim_end() == "***" || body.starts_with("%% kanban:settings") {
                board.end = start;
                break;
            }
            if let Some(t) = body.strip_prefix('#').map(|r| r.trim_start_matches('#')).and_then(|r| r.strip_prefix(' ')) {
                let ts = end - t.len();
                board.lanes.push(Lane {
                    title: t.trim().to_string(),
                    start,
                    title_range: ts..ts + t.trim_end().len(),
                    heading_end: end,
                    complete_at: None,
                    cards: Vec::new(),
                });
            } else if let Some(lane) = board.lanes.last_mut() {
                if let Some((done, box_char, skip)) = marker(body) {
                    let box_char = box_char.map(|r| start + r.start..start + r.end);
                    lane.cards.push(Card { text: body[skip..].to_string(), done, range: start..end, text_start: start + skip, box_char });
                    open = true;
                } else if body.trim() == "**Complete**" {
                    lane.complete_at.get_or_insert(start);
                } else {
                    in_fence = fence;
                }
            }
            continue;
        }
        if fence {
            in_fence = !in_fence;
        }
        let Some(c) = board.lanes.last_mut().and_then(|l| l.cards.last_mut()).filter(|_| open && indented) else {
            open = open && indented;
            continue;
        };
        c.text.push_str(&"\n".repeat(blanks + 1));
        c.text.push_str(body.strip_prefix("  ").or_else(|| body.strip_prefix('\t')).unwrap_or(body.trim_start_matches(' ')));
        c.range.end = end;
        blanks = 0;
    }
    board
}

/// Card text as lines of a list item: continuation lines are indented, blank ones stay empty.
fn indent(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for (i, l) in s.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
            if !l.is_empty() {
                out.push_str("  ");
            }
        }
        out.push_str(l);
    }
    out
}

impl Board {
    /// The edit that puts `line` at slot `idx` of `lane` (0..=len; len is the end of the lane, before a `**Complete**` marker).
    fn insertion(&self, lane: usize, idx: usize, line: &str) -> Option<Edit> {
        let l = self.lanes.get(lane)?;
        if let Some(c) = l.cards.get(idx) {
            return Some(Edit { range: c.range.start..c.range.start, insert: format!("{line}\n") });
        }
        // End of the lane: after the last card above a `**Complete**` marker, else at the marker, else under the heading.
        let above = |c: &&Card| l.complete_at.is_none_or(|m| c.range.end <= m);
        Some(match (l.cards.iter().rev().find(above), l.complete_at) {
            (Some(c), _) => Edit { range: c.range.end..c.range.end, insert: format!("\n{line}") },
            (None, Some(m)) => Edit { range: m..m, insert: format!("{line}\n\n") },
            (None, None) => Edit { range: l.heading_end..l.heading_end, insert: format!("\n\n{line}") },
        })
    }

    pub fn add_card(&self, lane: usize, card_text: &str) -> Vec<Edit> {
        let n = self.lanes.get(lane).map_or(0, |l| l.cards.len());
        self.insertion(lane, n, &format!("- [ ] {}", indent(card_text))).into_iter().collect()
    }

    pub fn toggle(&self, lane: usize, idx: usize) -> Vec<Edit> {
        let Some(c) = self.lanes.get(lane).and_then(|l| l.cards.get(idx)) else { return vec![] };
        let mark = if c.done { " " } else { "x" };
        vec![match &c.box_char {
            Some(r) => Edit { range: r.clone(), insert: mark.into() },
            None => Edit { range: c.text_start..c.text_start, insert: "[x] ".into() },
        }]
    }

    pub fn edit_text(&self, text: &str, lane: usize, idx: usize, new_text: &str) -> Vec<Edit> {
        let Some(c) = self.lanes.get(lane).and_then(|l| l.cards.get(idx)) else { return vec![] };
        // `- [ ]` without text has no space after the bracket yet.
        let space = if text[..c.text_start].ends_with(']') { " " } else { "" };
        vec![Edit { range: c.text_start..c.range.end, insert: format!("{space}{}", indent(new_text)) }]
    }

    /// Moves a card to slot `to.1` (0..=len, counted before the move) of lane `to.0`.
    pub fn move_card(&self, text: &str, from: (usize, usize), to: (usize, usize)) -> Vec<Edit> {
        let Some(c) = self.lanes.get(from.0).and_then(|l| l.cards.get(from.1)) else { return vec![] };
        if from.0 == to.0 && (to.1 == from.1 || to.1 == from.1 + 1) {
            return vec![];
        }
        let Some(ins) = self.insertion(to.0, to.1, &text[c.range.clone()]) else { return vec![] };
        // A card at the very end of the file has no newline of its own: take the one before it.
        let del = if c.range.end < text.len() { c.range.start..c.range.end + 1 } else { c.range.start.saturating_sub(1)..c.range.end };
        let mut v = vec![ins, Edit { range: del, insert: String::new() }];
        v.sort_by_key(|e| std::cmp::Reverse((e.range.start, e.range.end)));
        v
    }

    /// A new title is one line; an empty one is refused (no edit).
    pub fn rename_lane(&self, lane: usize, title: &str) -> Vec<Edit> {
        let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        match self.lanes.get(lane) {
            Some(l) if !title.is_empty() => vec![Edit { range: l.title_range.clone(), insert: title }],
            _ => vec![],
        }
    }

    /// A new empty lane after lane `after` (`None`: at the start).
    pub fn add_lane(&self, text: &str, after: Option<usize>, title: &str) -> Vec<Edit> {
        let next = after.map_or(0, |i| i + 1);
        let at = self.lanes.get(next).map_or(self.end, |l| l.title_range.start - heading_hashes(text, l.title_range.start));
        let pre = if at < text.len() || text.ends_with("\n\n") || at == 0 {
            ""
        } else if text.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        let post = if at < text.len() { "\n" } else { "" };
        vec![Edit { range: at..at, insert: format!("{pre}## {title}\n{post}") }]
    }

    /// Byte range of a whole lane: its heading, `**Complete**` marker and cards, and the blank lines after them.
    fn lane_range(&self, lane: usize) -> Option<Range<usize>> {
        let l = self.lanes.get(lane)?;
        Some(l.start..self.lanes.get(lane + 1).map_or(self.end, |n| n.start))
    }

    /// True when the lane is only its heading line: no cards, marker, paragraphs or code (nothing to lose by deleting it).
    pub fn lane_is_empty(&self, text: &str, lane: usize) -> bool {
        self.lane_range(lane).is_some_and(|r| !text[r].trim().contains('\n'))
    }

    /// Removes the lane with all its cards.
    pub fn delete_lane(&self, lane: usize) -> Vec<Edit> {
        self.lane_range(lane).map(|range| Edit { range, insert: String::new() }).into_iter().collect()
    }

    /// Moves a lane before lane `to` (0..=len, counted before the move; len is after the last lane).
    pub fn move_lane(&self, text: &str, from: usize, to: usize) -> Vec<Edit> {
        let Some(range) = self.lane_range(from) else { return vec![] };
        if to > self.lanes.len() || to == from || to == from + 1 {
            return vec![];
        }
        let at = self.lane_range(to).map_or(self.end, |r| r.start);
        // The lane keeps its text; one blank line after it separates it from the lane that follows, and a file that
        // ended without a newline gets one before the moved text.
        let pre = if at > 0 && !text[..at].ends_with('\n') { "\n" } else { "" };
        let chunk = format!("{pre}{}\n\n", text[range.clone()].trim_end());
        let mut v = vec![Edit { range: at..at, insert: chunk }, Edit { range, insert: String::new() }];
        v.sort_by_key(|e| std::cmp::Reverse((e.range.start, e.range.end)));
        v
    }
}

/// Length of the `## ` prefix in front of byte `title_start`.
fn heading_hashes(text: &str, title_start: usize) -> usize {
    let line = text[..title_start].rfind('\n').map_or(0, |i| i + 1);
    title_start - line
}

/// True when the edits are ordered back to front, do not overlap and lie on char boundaries inside `text`: then applying
/// them one by one cannot fail half way.
pub fn valid(text: &str, edits: &[Edit]) -> bool {
    let mut limit = text.len();
    edits.iter().all(|e| {
        let ok = e.range.start <= e.range.end && e.range.end <= limit && text.is_char_boundary(e.range.start) && text.is_char_boundary(e.range.end);
        limit = e.range.start;
        ok
    })
}

/// Adds `new` to `all` (kept back to front) unless one of its ranges touches an edit already there; returns whether it was added.
pub fn merge(all: &mut Vec<Edit>, new: Vec<Edit>) -> bool {
    let clash = |a: &Range<usize>, b: &Range<usize>| a.start == b.start || (a.start < b.end && b.start < a.end);
    if new.iter().any(|n| all.iter().any(|a| clash(&a.range, &n.range))) {
        return false;
    }
    all.extend(new);
    all.sort_by_key(|e| std::cmp::Reverse((e.range.start, e.range.end)));
    true
}

pub fn apply(text: &str, edits: &[Edit]) -> String {
    let mut s = text.to_string();
    for e in edits {
        s.replace_range(e.range.clone(), &e.insert);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const EX: &str = "---\nkanban-plugin: basic\n---\n\n## Todo\n\n- [ ] Write spec\n- [ ] Review #urgent\n\n**Complete**\n\n- [x] Set up repo\n\n## Doing\n\n- [ ] Parser\n  more text\n  - nested\n- [ ] Last\n\n***\n\n## Archive\n\n- [x] Old card\n\n%% kanban:settings\n```\n{\"kanban-plugin\":\"basic\"}\n```\n%%\n";

    fn titles(b: &Board) -> Vec<Vec<String>> {
        b.lanes.iter().map(|l| l.cards.iter().map(|c| c.text.clone()).collect()).collect()
    }

    #[test]
    fn detects_and_parses() {
        assert!(is_board(EX));
        assert!(!is_board("# hi\nkanban-plugin: basic"));
        assert!(!is_board("---\ntitle: x\n---\nkanban-plugin: basic\n"));
        let b = parse(EX);
        assert_eq!(b.lanes.iter().map(|l| l.title.as_str()).collect::<Vec<_>>(), ["Todo", "Doing"]);
        assert_eq!(titles(&b)[0], ["Write spec", "Review #urgent", "Set up repo"]);
        assert!(b.lanes[0].cards[2].done);
        assert_eq!(titles(&b)[1], ["Parser\nmore text\n- nested", "Last"]);
        assert_eq!(&EX[b.end..b.end + 3], "***");
    }

    #[test]
    fn toggle_and_edit_keep_the_rest() {
        let b = parse(EX);
        let t = apply(EX, &b.toggle(0, 0));
        assert_eq!(t, EX.replacen("- [ ] Write spec", "- [x] Write spec", 1));
        assert_eq!(apply(&t, &parse(&t).toggle(0, 0)), EX);
        let t = apply(EX, &b.edit_text(EX, 1, 0, "New\nline"));
        assert_eq!(t, EX.replace("Parser\n  more text\n  - nested", "New\n  line"));
    }

    #[test]
    fn add_card_and_lane() {
        let b = parse(EX);
        assert_eq!(apply(EX, &b.add_card(1, "X")), EX.replace("- [ ] Last\n", "- [ ] Last\n- [ ] X\n"));
        let t = apply(EX, &b.add_lane(EX, Some(1), "Done"));
        assert_eq!(t, EX.replace("***", "## Done\n\n***"));
        let t = apply(EX, &b.add_lane(EX, Some(0), "Mid"));
        assert_eq!(t, EX.replace("## Doing", "## Mid\n\n## Doing"));
        assert_eq!(apply(EX, &b.rename_lane(0, "Backlog")), EX.replace("## Todo", "## Backlog"));
        let empty = "---\nkanban-plugin: basic\n---\n\n## A\n";
        let e = parse(empty);
        let t = apply(empty, &e.add_card(0, "c"));
        assert_eq!(t, "---\nkanban-plugin: basic\n---\n\n## A\n\n- [ ] c\n");
        let t2 = apply(&t, &parse(&t).add_lane(&t, Some(0), "B"));
        assert_eq!(titles(&parse(&t2)), [vec!["c".to_string()], vec![]]);
    }

    #[test]
    fn moves_cards() {
        let b = parse(EX);
        // within a lane
        let t = apply(EX, &b.move_card(EX, (0, 0), (0, 2)));
        assert_eq!(titles(&parse(&t))[0], ["Review #urgent", "Write spec", "Set up repo"]);
        // no-ops
        assert!(b.move_card(EX, (0, 1), (0, 1)).is_empty() && b.move_card(EX, (0, 1), (0, 2)).is_empty());
        // across lanes, multi-line card to the front, then to the end of the other lane
        let t = apply(EX, &b.move_card(EX, (1, 0), (0, 0)));
        assert_eq!(titles(&parse(&t)), [vec!["Parser\nmore text\n- nested", "Write spec", "Review #urgent", "Set up repo"], vec!["Last"]]);
        let t = apply(EX, &b.move_card(EX, (0, 0), (1, 2)));
        assert_eq!(titles(&parse(&t)), [vec!["Review #urgent", "Set up repo"], vec!["Parser\nmore text\n- nested", "Last", "Write spec"]]);
        assert!(t.ends_with("%%\n") && t.contains("## Archive"));
        // card at EOF without newline
        let eof = "---\nkanban-plugin: basic\n---\n\n## A\n\n- [ ] a\n\n## B\n\n- [ ] b";
        let e = parse(eof);
        let t = apply(eof, &e.move_card(eof, (1, 0), (0, 0)));
        assert_eq!(titles(&parse(&t)), [vec!["b", "a"], vec![]]);
        let t = apply(eof, &e.move_card(eof, (0, 0), (1, 1)));
        assert_eq!(titles(&parse(&t)), [vec![], vec!["b", "a"]]);
    }

    #[test]
    fn deletes_and_moves_lanes() {
        let names = |t: &str| parse(t).lanes.iter().map(|l| l.title.clone()).collect::<Vec<_>>();
        let b = parse(EX);
        // Deleting a lane takes its cards and its `**Complete**` section; the tail stays.
        let t = apply(EX, &b.delete_lane(0));
        assert_eq!(names(&t), ["Doing"]);
        assert!(!t.contains("Write spec") && !t.contains("Set up repo") && t.contains("Parser") && t.contains("***\n\n## Archive"));
        let t = apply(EX, &b.delete_lane(1));
        assert_eq!(names(&t), ["Todo"]);
        assert!(t.contains("**Complete**\n\n- [x] Set up repo\n\n***\n") && t.contains("## Archive") && !t.contains("Parser"));
        assert!(b.delete_lane(9).is_empty());
        // Moving: to the end, to the front; neighbours and itself are no-ops.
        let t = apply(EX, &b.move_lane(EX, 0, 2));
        assert_eq!(names(&t), ["Doing", "Todo"]);
        assert_eq!(titles(&parse(&t))[1], ["Write spec", "Review #urgent", "Set up repo"]);
        assert!(parse(&t).lanes[1].complete_at.is_some() && t.contains("***\n\n## Archive") && t.ends_with("%%\n"));
        let t = apply(EX, &b.move_lane(EX, 1, 0));
        assert_eq!(names(&t), ["Doing", "Todo"]);
        assert!(b.move_lane(EX, 0, 0).is_empty() && b.move_lane(EX, 0, 1).is_empty() && b.move_lane(EX, 1, 2).is_empty() && b.move_lane(EX, 0, 3).is_empty());
        // Three lanes, the last without a trailing newline: every move keeps all lanes and cards.
        let eof = format!("{HEAD}## A\n\n- [ ] a\n\n## B\n\n- [ ] b\n\n## C\n\n- [ ] c");
        let e = parse(&eof);
        for (from, to, want) in [(2, 0, "CAB"), (0, 3, "BCA"), (1, 3, "ACB"), (2, 1, "ACB"), (0, 2, "BAC")] {
            let t = apply(&eof, &e.move_lane(&eof, from, to));
            let p = parse(&t);
            let got: String = p.lanes.iter().map(|l| l.title.as_str()).collect();
            assert_eq!(got, want, "{from}->{to}: {t:?}");
            assert!(p.lanes.iter().all(|l| l.cards.len() == 1 && l.cards[0].text.to_lowercase() == l.title.to_lowercase()), "{t:?}");
            assert!(valid(&eof, &e.move_lane(&eof, from, to)));
        }
        let t = apply(&eof, &e.delete_lane(2));
        assert_eq!(names(&t), ["A", "B"]);
    }

    #[test]
    fn only_a_bare_heading_is_an_empty_lane() {
        let t = format!("{HEAD}## A\n\n## B\n\n**Complete**\n\n## C\n\nsome text\n\n## D\n\n```\ncode\n```\n\n## E\n\n- [ ] x\n");
        let b = parse(&t);
        let e: Vec<bool> = (0..5).map(|i| b.lane_is_empty(&t, i)).collect();
        assert_eq!(e, [true, false, false, false, false]);
        assert!(!b.lane_is_empty(&t, 9));
    }

    const HEAD: &str = "---\nkanban-plugin: basic\n---\n\n";

    #[test]
    fn blank_lines_stay_in_the_card() {
        let t = format!("{HEAD}## A\n\n- [ ] one\n\n  para two\n- [ ] next\n");
        let b = parse(&t);
        assert_eq!(titles(&b)[0], ["one\n\npara two", "next"]);
        // writing a multi-paragraph text gives empty lines, which parse back as the same card
        let t2 = apply(&t, &b.edit_text(&t, 0, 0, "a\n\nb"));
        assert_eq!(t2, format!("{HEAD}## A\n\n- [ ] a\n\n  b\n- [ ] next\n"));
        assert_eq!(titles(&parse(&t2))[0], ["a\n\nb", "next"]);
        let t3 = apply(&t2, &parse(&t2).add_card(0, "x\n\ny"));
        assert_eq!(titles(&parse(&t3))[0], ["a\n\nb", "next", "x\n\ny"]);
        // a move takes the whole loose card
        let m = apply(&t2, &parse(&t2).move_card(&t2, (0, 0), (0, 2)));
        assert_eq!(titles(&parse(&m))[0], ["next", "a\n\nb"]);
        // a blank line followed by unindented text ends the card
        let u = format!("{HEAD}## A\n\n- [ ] one\n\nstray\n");
        assert_eq!(parse(&u).lanes[0].cards[0].range.end, u.find("one").map(|i| i + 3).unwrap_or(0));
    }

    #[test]
    fn new_cards_go_before_the_complete_marker() {
        let b = parse(EX);
        let t = apply(EX, &b.add_card(0, "N"));
        assert!(t.contains("- [ ] Review #urgent\n- [ ] N\n\n**Complete**"), "{t}");
        let t = apply(EX, &b.move_card(EX, (1, 0), (0, 3)));
        assert!(t.contains("- [ ] N") || t.contains("- [ ] Parser\n  more text\n  - nested\n\n**Complete**"), "{t}");
        let only = format!("{HEAD}## A\n\n**Complete**\n\n- [x] d\n");
        let o = parse(&only);
        assert_eq!(apply(&only, &o.add_card(0, "n")), format!("{HEAD}## A\n\n- [ ] n\n\n**Complete**\n\n- [x] d\n"));
    }

    #[test]
    fn card_markers() {
        let t = format!("{HEAD}## A\n\n- plain\n* star [ ]\n+ [x] plus\n- [\u{2713}] tick \u{1F600}\n- [ ]\n- [link](x)\n");
        let b = parse(&t);
        let c = &b.lanes[0].cards;
        assert_eq!(
            c.iter().map(|c| (c.text.as_str(), c.done)).collect::<Vec<_>>(),
            [("plain", false), ("star [ ]", false), ("plus", true), ("tick \u{1F600}", true), ("", false), ("[link](x)", false)]
        );
        // toggling: plain bullet gets a checkbox, multibyte tick becomes a space, an empty box ticks
        assert!(apply(&t, &b.toggle(0, 0)).contains("- [x] plain\n"));
        assert!(apply(&t, &b.toggle(0, 3)).contains("- [ ] tick \u{1F600}\n"));
        assert!(apply(&t, &b.toggle(0, 4)).contains("- [x]\n"));
        // editing: emoji text, and text into an empty box
        let e = apply(&t, &b.edit_text(&t, 0, 4, "\u{1F680} go"));
        assert!(e.contains("- [ ] \u{1F680} go\n- [link]"), "{e}");
        let e = apply(&t, &b.edit_text(&t, 0, 3, "d\u{e9}j\u{e0}"));
        assert!(e.contains("- [\u{2713}] d\u{e9}j\u{e0}\n"));
        // moving a multibyte card keeps its bytes
        let m = apply(&t, &b.move_card(&t, (0, 3), (0, 0)));
        assert_eq!(titles(&parse(&m))[0][0], "tick \u{1F600}");
    }

    #[test]
    fn headings_in_code_and_at_eof() {
        let t = format!("{HEAD}## A\n\n- [ ] a\n\n```\n## not a lane\n- [ ] not a card\n```\n\n## B");
        let b = parse(&t);
        assert_eq!(b.lanes.iter().map(|l| l.title.as_str()).collect::<Vec<_>>(), ["A", "B"]);
        assert_eq!(titles(&b)[0], ["a"]);
        let t2 = apply(&t, &b.add_card(1, "c"));
        assert_eq!(t2, format!("{t}\n\n- [ ] c"));
        let t3 = apply(&t, &b.add_lane(&t, Some(1), "C"));
        assert!(t3.ends_with("## B\n\n## C\n"), "{t3}");
    }

    #[test]
    fn rename_and_detection_rules() {
        let b = parse(EX);
        assert!(b.rename_lane(0, "  \n ").is_empty());
        assert_eq!(apply(EX, &b.rename_lane(0, " Back\nlog ")), EX.replace("## Todo", "## Back log"));
        assert!(!is_board("---\nkanban-plugin: basic\n"));
        assert!(is_board("---\nkanban-plugin: x\n---"));
        // an empty board takes a first lane
        let e = HEAD.to_string();
        let t = apply(&e, &parse(&e).add_lane(&e, None, "New lane"));
        assert_eq!(titles(&parse(&t)).len(), 1);
    }

    #[test]
    fn edits_are_validated_and_merged() {
        let t = "a\u{e9}b";
        let ok = vec![Edit { range: 3..3, insert: "x".into() }, Edit { range: 0..1, insert: String::new() }];
        assert!(valid(t, &ok));
        assert!(!valid(t, &[Edit { range: 2..3, insert: String::new() }]), "inside a char");
        assert!(!valid(t, &[Edit { range: 0..9, insert: String::new() }]), "past the end");
        assert!(!valid(t, &[Edit { range: 0..1, insert: String::new() }, Edit { range: 3..3, insert: String::new() }]), "not back to front");
        let mut all = Vec::new();
        assert!(merge(&mut all, vec![Edit { range: 0..2, insert: String::new() }]));
        assert!(!merge(&mut all, vec![Edit { range: 1..1, insert: "x".into() }]));
        assert!(merge(&mut all, vec![Edit { range: 3..3, insert: "y".into() }]));
        assert_eq!(all[0].range, 3..3);
    }
}
