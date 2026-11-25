//! Wiki-link autocomplete model: context at the cursor, candidate list cached per index generation, ranking only
//! when the query changes, selection and the accepted edit. No UI toolkit here.
use crate::state::AppState;
use iron_sheets_core::FileKind;
use iron_sheets_core::fuzzy::fuzzy_rank;
use iron_sheets_core::index::{Index, Indexer};
use iron_sheets_md::edit::{EditOp, wikilink_context};
use rustc_hash::FxHashMap;
use std::path::Path;
use std::time::{Duration, Instant};

/// Maximum items offered.
pub const LIMIT: usize = 50;

struct Cand {
    stem: String,
    dir: String,
    /// Relative path without extension (used when the stem alone is ambiguous).
    rel_noext: String,
    dup: bool,
}

/// The link a file dropped into a note gets: `[[Name]]` for notes (the relative path without extension when another note has the same stem,
/// like the autocomplete), `![[pic.png]]` for images, `[[doc.pdf]]` for other files (the relative path when the name is not unique).
/// `None` when the name holds a char that breaks wiki-link syntax (`# | ^ [ ]`): the caller tells the user to rename it.
pub fn dropped_link(ix: &Index, path: &Path) -> Option<String> {
    let kind = FileKind::from_path(path);
    let note = kind.is_note();
    let short = if note {
        FileKind::note_stem(path).map(|s| s.into_owned())
    } else {
        path.file_name().map(|n| n.to_string_lossy().into_owned())
    }
    .unwrap_or_default();
    // Same (case-insensitive) rule the resolver uses, so a link never lands on another file.
    let dup = ix.name_taken(path, &std::iter::once(path.to_path_buf()).collect());
    let rel = ix.files().find(|f| dup && f.path == path).map(|f| f.rel.replace('\\', "/"));
    let target = match rel {
        Some(r) if note => strip_rel_note_ext(&r),
        Some(r) => r,
        None => short,
    };
    if target.contains(['#', '|', '^', '[', ']']) {
        return None;
    }
    Some(format!("{}[[{target}]]", if matches!(kind, FileKind::Image | FileKind::Excalidraw) { "!" } else { "" }))
}

fn strip_rel_note_ext(rel: &str) -> String {
    let p = Path::new(rel);
    if FileKind::is_excalidraw_md(p) && rel.len() > 14 {
        return rel[..rel.len() - 14].replace('\\', "/");
    }
    p.with_extension("").to_string_lossy().replace('\\', "/")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    pub dim: String,
    pub insert: String,
}

#[derive(Clone, Copy)]
struct Ctx {
    /// Byte where the query starts (just after `[[` or `#`).
    start: usize,
    /// Cursor byte.
    end: usize,
}

/// Ranking cache key: (index generation, candidate count) plus `ranked_query` / `ranked_note`.
type RankKey = (u64, usize);

#[derive(Default)]
pub struct WikiModel {
    /// Candidate cache key: (index generation, note count).
    gen_key: Option<(u64, usize)>,
    /// The indexer is still filling the index (set by `update`); `cands_at` is when `cands` were last built.
    index_partial: bool,
    cands_at: Option<Instant>,
    cands: Vec<Cand>,
    ranked_for: Option<RankKey>,
    ranked_query: String,
    ranked_note: Option<String>,
    items: Vec<Item>,
    sel: usize,
    ctx: Option<Ctx>,
    close_follows: bool,
    dismissed: Option<usize>,
}

/// Parses `[[Note#head` before the cursor: (start of heading query, note, heading query).
pub fn heading_ctx(text: &str, cursor: usize) -> Option<(usize, &str, &str)> {
    let ls = text[..cursor].rfind('\n').map_or(0, |i| i + 1);
    let seg = &text[ls..cursor];
    let idx = seg.rfind("[[")?;
    let inner = &seg[idx + 2..];
    if inner.contains(['|', ']', '[']) {
        return None;
    }
    let h = inner.find('#')?;
    let q = &inner[h + 1..];
    if q.contains('#') {
        return None;
    }
    Some((ls + idx + 2 + h + 1, &inner[..h], q))
}

impl WikiModel {
    /// A popup should be shown: a context exists and there are items.
    pub fn active(&self) -> bool {
        self.ctx.is_some() && !self.items.is_empty()
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn selected(&self) -> usize {
        self.sel
    }

    /// Moves the selection by `delta` (wrapping).
    pub fn move_sel(&mut self, delta: isize) {
        let n = self.items.len();
        if n > 0 {
            self.sel = (self.sel as isize + delta).rem_euclid(n as isize) as usize;
        }
    }

    /// Escape: hide the popup until the cursor leaves this link.
    pub fn dismiss(&mut self) {
        self.dismissed = self.ctx.map(|c| c.start);
        self.ctx = None;
    }

    fn refresh_cands(&mut self, ix: &Index) {
        let key = (ix.generation(), ix.len());
        if self.gen_key == Some(key) {
            return;
        }
        // Startup fills the index in 50 ms bumps: keep the candidates we have and rebuild at most every 500 ms (the final bump rebuilds at once).
        if self.index_partial && !self.cands.is_empty() && self.cands_at.is_some_and(|t| t.elapsed() < Duration::from_millis(500)) {
            return;
        }
        self.cands_at = Some(Instant::now());
        self.gen_key = Some(key);
        self.ranked_for = None;
        let mut counts: FxHashMap<&str, u32> = FxHashMap::default();
        for n in ix.notes() {
            *counts.entry(n.stem.as_str()).or_default() += 1;
        }
        let mut v: Vec<Cand> = ix
            .notes()
            .map(|n| {
                let p = Path::new(&n.rel);
                let dir = p.parent().map(|d| d.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                let rel_noext = strip_rel_note_ext(&n.rel);
                Cand { stem: n.stem.clone(), dir, rel_noext, dup: counts.get(n.stem.as_str()).copied().unwrap_or(0) > 1 }
            })
            .collect();
        v.sort_by_cached_key(|c| (c.stem.to_lowercase(), c.rel_noext.clone()));
        self.cands = v;
    }

    /// Recomputes the context and items for the cursor. Cheap when nothing changed; never waits for the index
    /// (when a writer holds it, items ranked for this query stay; items for another query are dropped, so Enter
    /// never inserts a stale name, and the next frame retries).
    pub fn update<U>(&mut self, state: &AppState<U>, text: &str, cursor: usize, from: &Path) {
        self.update_parts(&state.indexer, text, cursor, from);
    }

    fn update_parts(&mut self, indexer: &Indexer, text: &str, cursor: usize, from: &Path) {
        let Some((note, query)) = self.enter(text, cursor) else { return };
        let idx = indexer.index();
        self.index_partial = indexer.partial();
        if let Some(g) = idx.try_read() {
            self.rank(&g, note, query, from);
        } else if self.ranked_query != query || self.ranked_note.as_deref() != note {
            self.items.clear();
            self.sel = 0;
        }
    }

    /// `update` over an already locked index.
    pub fn update_with(&mut self, ix: &Index, text: &str, cursor: usize, from: &Path) {
        if let Some((note, query)) = self.enter(text, cursor) {
            self.rank(ix, note, query, from);
        }
    }

    /// Context at the cursor without touching the index: `Some((note, query))` when a popup context is open.
    fn enter<'a>(&mut self, text: &'a str, cursor: usize) -> Option<(Option<&'a str>, &'a str)> {
        let cursor = cursor.min(text.len());
        if !text.is_char_boundary(cursor) {
            self.ctx = None;
            return None;
        }
        self.close_follows = text[cursor..].starts_with("]]");
        let (ctx, note, query): (Ctx, Option<&str>, &str) = if let Some((start, note, q)) = heading_ctx(text, cursor) {
            (Ctx { start, end: cursor }, Some(note.trim()), q)
        } else if let Some(c) = wikilink_context(text, cursor) {
            (Ctx { start: c.start, end: cursor }, None, c.query)
        } else {
            self.ctx = None;
            self.dismissed = None;
            return None;
        };
        if self.dismissed == Some(ctx.start) {
            self.ctx = None;
            return None;
        }
        self.ctx = Some(ctx);
        Some((note, query))
    }

    fn rank(&mut self, ix: &Index, note: Option<&str>, query: &str, from: &Path) {
        if note.is_none() {
            self.refresh_cands(ix);
        }
        let key = (ix.generation(), self.cands.len());
        if self.ranked_for == Some(key) && self.ranked_query == query && self.ranked_note.as_deref() == note {
            return;
        }
        self.ranked_for = Some(key);
        self.ranked_query.clear();
        self.ranked_query.push_str(query);
        self.ranked_note = note.map(str::to_string);
        self.sel = 0;
        self.items.clear();
        match note {
            None => {
                let ranked = fuzzy_rank(query, self.cands.iter().map(|c| (c, c.stem.as_str())), LIMIT);
                for (c, _) in ranked {
                    self.items.push(Item { label: c.stem.clone(), dim: c.dir.clone(), insert: if c.dup { c.rel_noext.clone() } else { c.stem.clone() } });
                }
            }
            Some(n) => {
                let target = if n.is_empty() { None } else { ix.resolve(n, Some(from)) };
                let id = target.or_else(|| ix.by_path(from));
                if let Some(meta) = id.and_then(|i| ix.get(i)) {
                    let ranked = fuzzy_rank(query, meta.headings.iter().map(|h| (h, h.text.as_str())), LIMIT);
                    for (h, _) in ranked {
                        self.items.push(Item { label: h.text.clone(), dim: format!("H{}", h.level), insert: h.text.clone() });
                    }
                }
            }
        }
    }

    /// Accepts item `i`: the edit that completes the link (`Note]]`), or `None` without a context or item.
    pub fn accept(&mut self, i: usize) -> Option<EditOp> {
        let ctx = self.ctx?;
        let item = self.items.get(i)?;
        let end = if self.close_follows { ctx.end + 2 } else { ctx.end };
        let insert = format!("{}]]", item.insert);
        let at = ctx.start + insert.len();
        self.ctx = None;
        self.dismissed = None;
        Some(EditOp { range: ctx.start..end, insert, select: at..at })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{state_with, state_with_many};

    /// `cargo test -p iron-sheets-app-core --lib wiki_cands -- --ignored --nocapture`
    #[test]
    #[ignore = "perf measurement"]
    fn wiki_cands_sort_time_at_50k_notes() {
        let (_d, state) = state_with_many(50_000);
        let idx = state.indexer.index();
        let ix = idx.read();
        let mut m = WikiModel::default();
        let t = std::time::Instant::now();
        m.refresh_cands(&ix);
        let new_ms = t.elapsed().as_secs_f64() * 1000.0;
        // Sort alone, old comparator vs cached key, on the same shuffled input.
        let mut v: Vec<(String, String)> = m.cands.iter().map(|c| (c.stem.clone(), c.rel_noext.clone())).collect();
        v.sort_by_key(|(a, _)| {
            use std::hash::BuildHasher;
            rustc_hash::FxBuildHasher.hash_one(a)
        });
        let mut w = v.clone();
        let t = std::time::Instant::now();
        v.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()).then_with(|| a.1.cmp(&b.1)));
        let old_ms = t.elapsed().as_secs_f64() * 1000.0;
        let t = std::time::Instant::now();
        w.sort_by_cached_key(|c| (c.0.to_lowercase(), c.1.clone()));
        let key_ms = t.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(v, w);
        println!("PERF wiki refresh_cands 50k total {new_ms:.1} ms; shuffled sort alone: old comparator {old_ms:.1} ms, cached key {key_ms:.1} ms");
    }

    const VAULT: &[(&str, &str)] = &[
        ("Welcome.md", "# Welcome\n"),
        ("Reading.md", "# Reading\n\n## Books\n\n## Papers\n\n## Podcasts\n"),
        ("Projects/Plan.md", "# Plan\n"),
        ("Archive/Plan.md", "# Old\n"),
        ("Projects/Tasks.md", "# Tasks\n"),
    ];

    fn update<U>(m: &mut WikiModel, state: &AppState<U>, text: &str, cursor: usize) {
        let from = state.vault_path("Welcome.md");
        m.update(state, text, cursor, &from);
    }

    fn labels(m: &WikiModel) -> Vec<&str> {
        m.items().iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn note_candidates_are_ranked_and_disambiguated() {
        let (_d, state) = state_with(VAULT);
        let mut m = WikiModel::default();
        let text = "see [[ta";
        update(&mut m, &state, text, text.len());
        assert!(m.active());
        assert_eq!(labels(&m)[0], "Tasks");
        assert_eq!(m.items()[0].dim, "Projects");
        assert_eq!(m.items()[0].insert, "Tasks");
        // Empty query lists all notes, sorted by stem; duplicated stems insert the relative path.
        let text = "[[";
        update(&mut m, &state, text, 2);
        assert_eq!(labels(&m), ["Plan", "Plan", "Reading", "Tasks", "Welcome"]);
        let plans: Vec<&str> = m.items().iter().filter(|i| i.label == "Plan").map(|i| i.insert.as_str()).collect();
        assert_eq!(plans, ["Archive/Plan", "Projects/Plan"]);
        assert_eq!(m.selected(), 0);
    }

    #[test]
    fn dropped_files_get_the_shortest_unambiguous_link() {
        let extra = [("img/pic.png", ""), ("a/doc.pdf", ""), ("b/doc.pdf", ""), ("a/Mix.md", ""), ("b/mix.md", ""), ("C# basics.md", "")];
        let files: Vec<(&str, &str)> = VAULT.iter().copied().chain(extra).collect();
        let (_d, state) = state_with(&files);
        let idx = state.indexer.index();
        let ix = idx.read();
        let link = |rel: &str| dropped_link(&ix, &state.vault_path(rel)).unwrap_or_default();
        assert_eq!(link("Reading.md"), "[[Reading]]");
        assert_eq!(link("Projects/Plan.md"), "[[Projects/Plan]]");
        assert_eq!(link("img/pic.png"), "![[pic.png]]");
        assert_eq!(link("a/doc.pdf"), "[[a/doc.pdf]]");
        assert_eq!(link("nowhere/x.txt"), "[[x.txt]]");
        // Names differing only by case still need the path.
        assert_eq!(link("a/Mix.md"), "[[a/Mix]]");
        // A name that would break the link syntax gets none.
        assert_eq!(dropped_link(&ix, &state.vault_path("C# basics.md")), None);
    }

    #[test]
    fn a_busy_index_never_leaves_items_of_another_query() {
        let (_d, state) = state_with(VAULT);
        let mut m = WikiModel::default();
        update(&mut m, &state, "[[ta", 4);
        assert_eq!(labels(&m)[0], "Tasks");
        let idx = state.indexer.index();
        let guard = idx.write();
        // Same query while locked: the ranked items stay.
        update(&mut m, &state, "[[ta", 4);
        assert_eq!(labels(&m)[0], "Tasks");
        // A new link context with another query while locked: no stale items, so Enter inserts nothing.
        let text = "[[Tasks]] and [[re";
        update(&mut m, &state, text, text.len());
        assert!(!m.active() && m.accept(0).is_none());
        drop(guard);
        update(&mut m, &state, text, text.len());
        assert_eq!(labels(&m), ["Reading"]);
    }

    #[test]
    fn heading_candidates() {
        let (_d, state) = state_with(VAULT);
        let mut m = WikiModel::default();
        let text = "[[Reading#Po";
        update(&mut m, &state, text, text.len());
        assert_eq!(labels(&m), ["Podcasts"]);
        assert_eq!(m.items()[0].dim, "H2");
        // Without a note name the headings of the current note are used.
        let text = "[[#We";
        update(&mut m, &state, text, text.len());
        assert_eq!(labels(&m), ["Welcome"]);
        // A closed link or alias is no completion context.
        let text = "[[Reading|al";
        update(&mut m, &state, text, text.len());
        assert!(!m.active());
    }

    #[test]
    fn selection_wraps_and_resets_with_the_query() {
        let (_d, state) = state_with(VAULT);
        let mut m = WikiModel::default();
        update(&mut m, &state, "[[", 2);
        m.move_sel(-1);
        assert_eq!(m.selected(), 4);
        m.move_sel(1);
        assert_eq!(m.selected(), 0);
        m.move_sel(2);
        assert_eq!(m.selected(), 2);
        // Same context again: ranking is cached, the selection stays.
        update(&mut m, &state, "[[", 2);
        assert_eq!(m.selected(), 2);
        update(&mut m, &state, "[[R", 3);
        assert_eq!(m.selected(), 0, "new query resets the selection");
    }

    #[test]
    fn accept_builds_the_edit() {
        let (_d, state) = state_with(VAULT);
        let mut m = WikiModel::default();
        let text = "see [[Rea more";
        update(&mut m, &state, text, "see [[Rea".len());
        let op = m.accept(0).expect("edit");
        assert_eq!(op.range, 6..9);
        assert_eq!(op.insert, "Reading]]");
        assert_eq!(op.select, 15..15);
        assert_eq!(op.apply(text), "see [[Reading]] more");
        assert!(!m.active(), "context consumed");
        assert!(m.accept(0).is_none());
        // Existing closing brackets are replaced, not doubled.
        let text = "[[Rea]]";
        update(&mut m, &state, text, 5);
        let op = m.accept(0).expect("edit");
        assert_eq!(op.apply(text), "[[Reading]]");
        // Duplicate stem inserts the relative path.
        let text = "[[Pl";
        update(&mut m, &state, text, 4);
        let op = m.accept(0).expect("edit");
        assert!(op.insert == "Archive/Plan]]", "{}", op.insert);
    }

    #[test]
    fn dismiss_hides_until_the_cursor_leaves_the_link() {
        let (_d, state) = state_with(VAULT);
        let mut m = WikiModel::default();
        update(&mut m, &state, "[[Re", 4);
        assert!(m.active());
        m.dismiss();
        assert!(!m.active());
        update(&mut m, &state, "[[Rea", 5);
        assert!(!m.active(), "still the same link");
        update(&mut m, &state, "plain text", 5);
        update(&mut m, &state, "[[Rea", 5);
        assert!(m.active(), "dismissal ended when the cursor left");
    }

    #[test]
    fn invalid_cursor_and_heading_ctx_parsing() {
        let (_d, state) = state_with(VAULT);
        let mut m = WikiModel::default();
        update(&mut m, &state, "[[\u{e9}", 3);
        assert!(!m.active(), "cursor inside a char");
        assert_eq!(heading_ctx("a [[Note#he", 11), Some((9, "Note", "he")));
        assert_eq!(heading_ctx("a [[Note#he#x", 13), None);
        assert_eq!(heading_ctx("[[Note]]#x", 10), None);
        assert_eq!(heading_ctx("no link", 7), None);
    }
}
