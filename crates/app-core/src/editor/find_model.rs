//! Find / replace model: match scan, cache, current match, replace operations. No UI toolkit here.
//! Plain text (ASCII case switch) or regular expressions (`regex` crate); no whole-word.
use crate::docs::DocStore;
use crate::state::AppState;
use crate::tabs::Tabs;
use regex::{Regex, RegexBuilder};
use std::borrow::Cow;
use std::ops::Range;

/// Upper bound on matches kept for one document.
pub const MAX_MATCHES: usize = 200_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub b: Range<usize>,
    /// Same range in chars (galley coordinates).
    pub c: Range<usize>,
}

/// All non-overlapping matches. Case-insensitive mode folds ASCII only (keeps byte offsets).
pub fn scan(text: &str, query: &str, case: bool) -> Vec<Match> {
    let mut out = Vec::new();
    if query.is_empty() {
        return out;
    }
    let (hay, needle);
    let (h, n): (&str, &str) = if case {
        (text, query)
    } else {
        hay = text.to_ascii_lowercase();
        needle = query.to_ascii_lowercase();
        (&hay, &needle)
    };
    let (mut last_b, mut last_c) = (0usize, 0usize);
    for (b, _) in h.match_indices(n) {
        if out.len() >= MAX_MATCHES {
            break;
        }
        last_c += text[last_b..b].chars().count();
        last_b = b;
        let len_c = text[b..b + n.len()].chars().count();
        out.push(Match { b: b..b + n.len(), c: last_c..last_c + len_c });
    }
    out
}

/// All non-empty, non-overlapping matches of `re`.
pub fn scan_regex(text: &str, re: &Regex) -> Vec<Match> {
    let mut out = Vec::new();
    let (mut last_b, mut last_c) = (0usize, 0usize);
    for m in re.find_iter(text).filter(|m| !m.is_empty()).take(MAX_MATCHES) {
        last_c += text[last_b..m.start()].chars().count();
        last_b = m.start();
        let len_c = m.as_str().chars().count();
        out.push(Match { b: m.range(), c: last_c..last_c + len_c });
    }
    out
}

/// Last line of a regex error ("error: ..." detail) as the short message.
fn short_error(full: &str) -> String {
    let l = full.lines().last().unwrap_or(full);
    l.strip_prefix("error: ").unwrap_or(l).to_string()
}

/// Compiled pattern for `(query, case)`; the error is `(short, full)`.
struct Compiled {
    query: String,
    case: bool,
    re: Result<Regex, (String, String)>,
}

#[derive(Default)]
pub struct FindModel {
    pub open: bool,
    pub show_replace: bool,
    tab: u64,
    pub query: String,
    pub replace: String,
    pub case: bool,
    /// Query is a regular expression (`^`/`$` per line).
    pub regex: bool,
    compiled: Option<Compiled>,
    #[cfg(test)]
    compiles: usize,
    matches: Vec<Match>,
    cur: usize,
    /// Cache key of `matches`: (document version, case, regex) plus `key_query`.
    key: Option<(u64, bool, bool)>,
    key_query: String,
}

impl FindModel {
    /// Opens (or re-targets) the bar for tab `tab_id` in Find-only mode; a one-line `selection` becomes the query when newly opened.
    pub fn open_for(&mut self, tab_id: u64, selection: Option<&str>) {
        if !self.open || self.tab != tab_id {
            if let Some(s) = selection.filter(|s| !s.is_empty() && !s.contains('\n')) {
                self.query = s.to_string();
            }
        }
        self.open = true;
        self.show_replace = false;
        self.tab = tab_id;
        self.key = None;
    }

    /// Opens (or re-targets) the bar with the Replace controls visible (`Ctrl+H`).
    pub fn open_replace_for(&mut self, tab_id: u64, selection: Option<&str>) {
        self.open_for(tab_id, selection);
        self.show_replace = true;
    }

    pub fn shown_for(&self, tab_id: u64) -> bool {
        self.open && self.tab == tab_id
    }

    pub fn close(&mut self) {
        self.open = false;
        self.show_replace = false;
        self.matches.clear();
        self.key = None;
    }

    /// Forces a rescan on the next `refresh`.
    pub fn invalidate(&mut self) {
        self.key = None;
    }

    pub fn toggle_case(&mut self) {
        self.case = !self.case;
        self.key = None;
    }

    pub fn toggle_regex(&mut self) {
        self.regex = !self.regex;
        self.key = None;
    }

    /// Short message of an invalid pattern (regex mode only).
    pub fn error(&self) -> Option<&str> {
        self.compile_error().map(|e| e.0.as_str())
    }

    /// The full compiler message, for a tooltip.
    pub fn error_detail(&self) -> Option<&str> {
        self.compile_error().map(|e| e.1.as_str())
    }

    /// The compile error of the current query only (not of an earlier one, e.g. after the query was cleared).
    fn compile_error(&self) -> Option<&(String, String)> {
        let c = self.compiled.as_ref().filter(|c| self.regex && !self.query.is_empty() && c.query == self.query && c.case == self.case)?;
        c.re.as_ref().err()
    }

    fn regex_ref(&self) -> Option<&Regex> {
        self.compiled.as_ref()?.re.as_ref().ok()
    }

    pub fn matches(&self) -> &[Match] {
        &self.matches
    }

    /// Index of the current match (meaningful when `matches()` is not empty).
    pub fn current_index(&self) -> usize {
        self.cur
    }

    pub fn current(&self) -> Option<&Match> {
        self.matches.get(self.cur)
    }

    /// Rescans when the document version, query or case flag changed. On the first scan (or when the current index
    /// fell off the end) the current match becomes the first one at or after byte `from`.
    // shortcut: the scan runs on the UI thread (plain and regex), move it to a worker when large notes lag.
    pub fn refresh(&mut self, text: &str, version: u64, from: usize) {
        if self.key == Some((version, self.case, self.regex)) && self.key_query == self.query {
            return;
        }
        let first = self.key.is_none();
        self.key = Some((version, self.case, self.regex));
        self.key_query.clear();
        self.key_query.push_str(&self.query);
        self.matches = if !self.regex {
            scan(text, &self.query, self.case)
        } else if self.query.is_empty() {
            Vec::new()
        } else {
            self.compile();
            self.regex_ref().map(|re| scan_regex(text, re)).unwrap_or_default()
        };
        if first || self.cur >= self.matches.len() {
            self.cur = self.matches.iter().position(|m| m.b.start >= from).unwrap_or(0);
        }
    }

    /// Compiles the query unless the cached pattern is for the same (query, case).
    fn compile(&mut self) {
        if self.compiled.as_ref().is_some_and(|c| c.query == self.query && c.case == self.case) {
            return;
        }
        #[cfg(test)]
        {
            self.compiles += 1;
        }
        let re = RegexBuilder::new(&self.query)
            .case_insensitive(!self.case)
            .multi_line(true)
            // Bounds compile time of pathological patterns (they show as "bad regex").
            .size_limit(1 << 20)
            .dfa_size_limit(1 << 20)
            .build()
            .map_err(|e| (short_error(&e.to_string()), e.to_string()));
        self.compiled = Some(Compiled { query: self.query.clone(), case: self.case, re });
    }

    /// Replacement text for match `m` of `text`: literal in plain mode; in regex mode `$1`/`${name}`/`$$` expanded.
    /// None when the match no longer sits where it was found (stale).
    fn replacement(&self, text: &str, m: &Match) -> Option<Cow<'_, str>> {
        if !self.regex {
            return Some(Cow::Borrowed(&self.replace));
        }
        let caps = self.regex_ref()?.captures_at(text, m.b.start)?;
        if caps.get(0)?.range() != m.b {
            return None;
        }
        let mut s = String::new();
        caps.expand(&self.replace, &mut s);
        Some(Cow::Owned(s))
    }

    /// Moves the current match by `step` (wrapping) and returns its byte range.
    pub fn go(&mut self, step: isize) -> Option<Range<usize>> {
        let n = self.matches.len();
        if n == 0 {
            return None;
        }
        self.cur = (self.cur as isize + step).rem_euclid(n as isize) as usize;
        self.current().map(|m| m.b.clone())
    }

    /// "cur/total" label of the bar.
    pub fn label(&self) -> String {
        if self.query.is_empty() {
            "-/-".to_string()
        } else if self.error().is_some() {
            "bad regex".to_string()
        } else if self.matches.is_empty() {
            "0/0".to_string()
        } else {
            format!("{}/{}", self.cur + 1, self.matches.len())
        }
    }

    /// Selects the current match in tab `ti`. Returns true when a selection was requested.
    pub fn select_current<U>(&self, state: &mut AppState<U>, ti: usize) -> bool {
        self.select_current_parts(&mut state.tabs, ti)
    }

    fn select_current_parts(&self, tabs: &mut Tabs, ti: usize) -> bool {
        if let (Some(m), Some(tab)) = (self.matches.get(self.cur), tabs.tabs.get_mut(ti)) {
            tab.pending_cursor = Some(m.b.clone());
            return true;
        }
        false
    }

    /// Replaces the current match in tab `ti`; the cursor goes behind the replacement. True when the text changed.
    pub fn replace_current<U>(&self, state: &mut AppState<U>, ti: usize) -> bool {
        self.replace_current_parts(&mut state.tabs, &mut state.docs, ti)
    }

    fn replace_current_parts(&self, tabs: &mut Tabs, docs: &mut DocStore, ti: usize) -> bool {
        let Some(m) = self.matches.get(self.cur).cloned() else { return false };
        let Some(path) = tabs.tabs.get(ti).map(|t| t.path.clone()) else { return false };
        let Some(doc) = docs.get_mut(&path).filter(|d| !d.read_only) else { return false };
        // Stale match (doc moved on): refuse; the next frame rescans.
        let Some(rep) = self.replacement(&doc.text, &m) else { return false };
        let at = m.b.start + rep.len();
        let ok = doc.apply_edit(m.b.clone(), &rep);
        if ok {
            tabs.tabs[ti].pending_cursor = Some(at..at);
        }
        ok
    }

    /// Replaces the current match, then rescans and selects the first match behind the replacement (wrapping).
    pub fn replace_and_select_next<U>(&mut self, state: &mut AppState<U>, ti: usize) -> bool {
        self.replace_and_select_next_parts(&mut state.tabs, &mut state.docs, ti)
    }

    fn replace_and_select_next_parts(&mut self, tabs: &mut Tabs, docs: &mut DocStore, ti: usize) -> bool {
        if !self.replace_current_parts(tabs, docs, ti) {
            return false;
        }
        let tab = &tabs.tabs[ti];
        let at = tab.pending_cursor.as_ref().map_or(0, |r| r.start);
        if let Some(d) = docs.get(&tab.path) {
            self.key = None;
            self.refresh(&d.text, d.version, at);
            self.select_current_parts(tabs, ti);
        }
        true
    }

    /// Replaces every match in tab `ti` with one edit; the cursor goes to the start of the replaced span.
    pub fn replace_all<U>(&self, state: &mut AppState<U>, ti: usize) -> bool {
        self.replace_all_parts(&mut state.tabs, &mut state.docs, ti)
    }

    fn replace_all_parts(&self, tabs: &mut Tabs, docs: &mut DocStore, ti: usize) -> bool {
        let (Some(first), Some(last)) = (self.matches.first(), self.matches.last()) else { return false };
        let (s, e) = (first.b.start, last.b.end);
        let Some(path) = tabs.tabs.get(ti).map(|t| t.path.clone()) else { return false };
        let Some(doc) = docs.get_mut(&path) else { return false };
        if doc.read_only {
            return false;
        }
        let mut new = String::with_capacity(e - s);
        let mut at = s;
        for m in &self.matches {
            let Some(rep) = self.replacement(&doc.text, m) else { return false };
            new.push_str(&doc.text[at..m.b.start]);
            new.push_str(&rep);
            at = m.b.end;
        }
        if doc.apply_edit(s..e, &new) {
            tabs.tabs[ti].pending_cursor = Some(s..s);
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_counts_chars_and_bytes() {
        let m = scan("aé ab AB", "ab", false);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].b, 4..6);
        assert_eq!(m[0].c, 3..5);
        assert_eq!(scan("aé ab AB", "ab", true).len(), 1);
    }

    #[test]
    fn scan_edge_cases() {
        assert!(scan("abc", "", false).is_empty());
        assert_eq!(scan("aaaa", "aa", true).len(), 2, "non-overlapping");
        assert!(scan("abc", "x", false).is_empty());
        // Case folding is ASCII only: non-ASCII letters match exactly.
        assert_eq!(scan("\u{c9}", "\u{e9}", false).len(), 0);
    }

    #[test]
    fn refresh_current_and_wrap() {
        let mut f = FindModel { query: "ab".into(), ..Default::default() };
        let text = "ab x ab y AB";
        f.refresh(text, 1, 3);
        assert_eq!(f.matches().len(), 3);
        assert_eq!(f.current_index(), 1, "first match at or after the cursor");
        assert_eq!(f.label(), "2/3");
        assert_eq!(f.go(1), Some(10..12));
        assert_eq!(f.go(1), Some(0..2), "wraps forward");
        assert_eq!(f.go(-1), Some(10..12), "wraps backward");
        // Same key: no rescan, current kept.
        f.refresh(text, 1, 0);
        assert_eq!(f.current_index(), 2);
        // New version rescans; the index is kept while it is valid.
        f.refresh("ab ab ab", 2, 0);
        assert_eq!(f.matches().len(), 3);
        assert_eq!(f.current_index(), 2);
        f.refresh("ab", 3, 0);
        assert_eq!(f.current_index(), 0, "index past the end resets to the cursor match");
    }

    #[test]
    fn case_toggle_and_labels() {
        let mut f = FindModel::default();
        assert_eq!(f.label(), "-/-");
        f.query = "AB".into();
        f.refresh("ab AB", 1, 0);
        assert_eq!(f.matches().len(), 2);
        f.toggle_case();
        f.refresh("ab AB", 1, 0);
        assert_eq!(f.matches().len(), 1);
        f.query = "zz".into();
        f.refresh("ab AB", 1, 0);
        assert_eq!(f.label(), "0/0");
        assert_eq!(f.go(1), None);
    }

    #[test]
    fn open_for_takes_one_line_selection() {
        let mut f = FindModel::default();
        f.open_for(7, Some("a\nb"));
        assert_eq!(f.query, "");
        f.open_for(7, Some("word"));
        assert_eq!(f.query, "", "already open for this tab: keep the query");
        f.open_for(8, Some("word"));
        assert_eq!(f.query, "word");
        assert!(f.shown_for(8) && !f.shown_for(7));
        f.close();
        assert!(!f.shown_for(8));
        assert!(f.matches().is_empty());
    }

    fn rx(query: &str) -> FindModel {
        FindModel { query: query.into(), regex: true, case: true, ..Default::default() }
    }

    #[test]
    fn regex_offsets_in_bytes_and_chars() {
        let mut f = rx(r"b+");
        f.refresh("\u{e9}\u{e9} bb x b", 1, 0);
        assert_eq!(f.matches()[0], Match { b: 5..7, c: 3..5 });
        assert_eq!(f.matches()[1], Match { b: 10..11, c: 8..9 });
    }

    #[test]
    fn regex_case_multiline_and_empty_matches() {
        let mut f = rx("^ab");
        f.refresh("ab\nxab\nab", 1, 0);
        assert_eq!(f.matches().len(), 2, "^ is per line");
        f.case = false;
        f.query = "AB$".into();
        f.refresh("ab\nabx\nAb", 1, 0);
        assert_eq!(f.matches().len(), 2, "case-insensitive, $ per line");
        let mut f = rx("a*");
        f.refresh("baab", 1, 0);
        assert_eq!(f.matches().len(), 1, "empty matches skipped");
        assert_eq!(f.matches()[0].b, 1..3);
        let mut f = rx("^");
        f.refresh("a\nb", 1, 0);
        assert!(f.matches().is_empty());
        assert_eq!(f.error(), None);
    }

    #[test]
    fn invalid_regex_reports_and_matches_nothing() {
        let mut f = rx("(ab");
        f.refresh("(ab ab", 1, 0);
        assert!(f.matches().is_empty());
        assert!(f.error().is_some_and(|e| !e.contains('\n')));
        assert!(f.error_detail().is_some());
        assert_eq!(f.label(), "bad regex");
        f.query.clear();
        f.refresh("(ab ab", 1, 0);
        assert_eq!(f.error(), None, "a cleared query shows no stale error");
        f.query = "(ab".into();
        f.refresh("(ab ab", 1, 0);
        f.toggle_regex();
        f.refresh("(ab ab", 1, 0);
        assert_eq!((f.error(), f.matches().len()), (None, 1), "plain mode: literal");
    }

    #[test]
    fn compiled_regex_is_reused_across_versions() {
        let mut f = rx("a+");
        f.refresh("aa", 1, 0);
        f.refresh("aa a", 2, 0);
        assert_eq!((f.compiles, f.matches().len()), (1, 2));
        f.toggle_case();
        f.refresh("aa a", 2, 0);
        assert_eq!(f.compiles, 2);
    }

    mod with_state {
        use super::*;
        use crate::test_support::{activate_tab, state_with};

        fn setup(text: &str) -> (tempfile::TempDir, AppState) {
            let (d, mut state) = state_with(&[("n.md", text)]);
            activate_tab(&mut state, "n.md");
            let p = state.vault_path("n.md");
            state.docs.open(&p).expect("open");
            (d, state)
        }

        fn text<U>(state: &AppState<U>) -> String {
            state.docs.get(&state.vault_path("n.md")).expect("doc").text.clone()
        }

        fn model<U>(state: &AppState<U>, query: &str, replace: &str) -> FindModel {
            let mut f = FindModel { query: query.into(), replace: replace.into(), ..Default::default() };
            let doc = state.docs.get(&state.vault_path("n.md")).expect("doc");
            f.refresh(&doc.text, doc.version, 0);
            f
        }

        #[test]
        fn select_and_replace_current() {
            let (_d, mut state) = setup("one two one two");
            let f = model(&state, "two", "2");
            assert!(f.select_current(&mut state, 0));
            assert_eq!(state.tabs.tabs[0].pending_cursor, Some(4..7));
            assert!(f.replace_current(&mut state, 0));
            assert_eq!(text(&state), "one 2 one two");
            assert_eq!(state.tabs.tabs[0].pending_cursor, Some(5..5));
            assert!(!f.select_current(&mut state, 9), "no such tab");
            assert!(!f.replace_current(&mut state, 9));
        }

        #[test]
        fn replace_and_select_next_moves_on() {
            let (_d, mut state) = setup("a a a");
            let mut f = model(&state, "a", "aa");
            assert!(f.replace_and_select_next(&mut state, 0));
            assert_eq!(text(&state), "aa a a");
            // Skips the replacement text itself: next match is the old second `a` (now at byte 3).
            assert_eq!(state.tabs.tabs[0].pending_cursor, Some(3..4));
            assert!(!f.replace_and_select_next(&mut state, 9));
        }

        #[test]
        fn replace_all_is_one_edit() {
            let (_d, mut state) = setup("a-b-a-b-a");
            let f = model(&state, "a", "xyz");
            let v0 = state.docs.get(&state.vault_path("n.md")).expect("doc").version;
            assert!(f.replace_all(&mut state, 0));
            assert_eq!(text(&state), "xyz-b-xyz-b-xyz");
            let v1 = state.docs.get(&state.vault_path("n.md")).expect("doc").version;
            assert_eq!(v1, v0 + 1, "a single version bump, a single undo step");
            assert_eq!(state.tabs.tabs[0].pending_cursor, Some(0..0));
            let none = model(&state, "nothing", "x");
            assert!(!none.replace_all(&mut state, 0));
        }

        fn rmodel<U>(state: &AppState<U>, query: &str, replace: &str, regex: bool) -> FindModel {
            let mut f = FindModel { query: query.into(), replace: replace.into(), regex, case: true, ..Default::default() };
            let doc = state.docs.get(&state.vault_path("n.md")).expect("doc");
            f.refresh(&doc.text, doc.version, 0);
            f
        }

        #[test]
        fn regex_replace_expands_groups() {
            let (_d, mut state) = setup("k1=v1 k2=v2");
            let mut f = rmodel(&state, r"(?P<k>k\d)=(v\d)", "$2=${k} $$", true);
            assert!(f.replace_current(&mut state, 0));
            assert_eq!(text(&state), "v1=k1 $ k2=v2");
            f.refresh(&text(&state), 99, 0);
            let v0 = state.docs.get(&state.vault_path("n.md")).expect("doc").version;
            assert!(f.replace_all(&mut state, 0));
            assert_eq!(text(&state), "v1=k1 $ v2=k2 $");
            assert_eq!(state.docs.get(&state.vault_path("n.md")).expect("doc").version, v0 + 1);
        }

        #[test]
        fn regex_replace_next_and_stale_match() {
            let (_d, mut state) = setup("a1 a2");
            let mut f = rmodel(&state, r"a(\d)", "<$1>", true);
            assert!(f.replace_and_select_next(&mut state, 0));
            assert_eq!(text(&state), "<1> a2");
            assert_eq!(state.tabs.tabs[0].pending_cursor, Some(4..6));
            // The doc changed behind the model's back: refuse instead of replacing the wrong range.
            let p = state.vault_path("n.md");
            state.docs.get_mut(&p).expect("doc").apply_edit(0..0, "zz");
            assert!(!f.replace_current(&mut state, 0));
            assert_eq!(text(&state), "zz<1> a2");
        }

        #[test]
        fn plain_replace_keeps_dollar_literal() {
            let (_d, mut state) = setup("(a)");
            let f = rmodel(&state, "(a)", "$1", false);
            assert!(f.replace_all(&mut state, 0));
            assert_eq!(text(&state), "$1");
        }

        #[test]
        fn read_only_documents_are_not_edited() {
            let (_d, mut state) = setup("aaa");
            let f = model(&state, "a", "b");
            let p = state.vault_path("n.md");
            state.docs.get_mut(&p).expect("doc").read_only = true;
            assert!(!f.replace_current(&mut state, 0));
            assert!(!f.replace_all(&mut state, 0));
            assert_eq!(text(&state), "aaa");
        }
    }
}
