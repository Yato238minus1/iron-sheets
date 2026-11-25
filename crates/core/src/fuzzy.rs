//! Fuzzy subsequence matching.
use smallvec::SmallVec;
use std::cmp::Ordering;
use std::collections::BinaryHeap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatch {
    pub score: i32,
    /// Matched character indices (not bytes) into the candidate.
    pub positions: SmallVec<[u32; 16]>,
}

const NEG: i32 = i32::MIN / 2;
const MATCH: i32 = 16;
const CONSEC: i32 = 6;
const GAP_START: i32 = 3;
const GAP_EXT: i32 = 1;
const MAX_CHARS: usize = 512;

#[inline]
fn fold(c: char) -> char {
    if c.is_ascii() {
        c.to_ascii_lowercase()
    } else {
        let mut l = c.to_lowercase();
        match (l.next(), l.next()) {
            (Some(x), None) => x,
            _ => c,
        }
    }
}

fn is_sep(c: char) -> bool {
    matches!(c, '/' | '\\' | '-' | '_' | ' ' | '.' | ':')
}

fn boundary_bonus(prev: Option<char>, c: char) -> i32 {
    match prev {
        None => 10,
        Some('/') | Some('\\') => 9,
        Some(p) if is_sep(p) => 8,
        Some(p) if p.is_lowercase() && c.is_uppercase() => 7,
        Some(p) if !p.is_alphanumeric() => 6,
        Some(p) if p.is_alphabetic() && c.is_numeric() => 3,
        _ => 0,
    }
}

#[inline]
fn boundary_bonus_ascii(bytes: &[u8], j: usize) -> i32 {
    if j == 0 {
        return 10;
    }
    let p = bytes[j - 1];
    let c = bytes[j];
    match p {
        b'/' | b'\\' => 9,
        b'-' | b'_' | b' ' | b'.' | b':' => 8,
        b'a'..=b'z' if c.is_ascii_uppercase() => 7,
        _ if !p.is_ascii_alphanumeric() => 6,
        _ if p.is_ascii_alphabetic() && c.is_ascii_digit() => 3,
        _ => 0,
    }
}

struct PreparedQuery {
    chars: SmallVec<[char; 32]>,
    ascii: Option<SmallVec<[u8; 32]>>,
    /// Bitmask per folded ASCII query byte (`0x20` for `a..=z`, `0` otherwise) so
    /// `(b | mask) == target` matches `b.to_ascii_lowercase() == target` in one OR.
    masks: SmallVec<[u8; 32]>,
}

impl PreparedQuery {
    fn new(query: &str) -> Self {
        let chars: SmallVec<[char; 32]> = query.chars().filter(|c| !c.is_whitespace()).map(fold).collect();
        let ascii: Option<SmallVec<[u8; 32]>> = if chars.iter().all(|c| c.is_ascii()) { Some(chars.iter().map(|&c| c as u8).collect()) } else { None };
        let masks: SmallVec<[u8; 32]> = match &ascii {
            Some(qb) => qb.iter().map(|&b| if b.is_ascii_lowercase() { 0x20 } else { 0 }).collect(),
            None => SmallVec::new(),
        };
        Self { chars, ascii, masks }
    }
}

/// DP buffers reused across candidates of one `fuzzy_rank` call.
#[derive(Default)]
struct Scratch {
    first: SmallVec<[usize; 32]>,
    last: SmallVec<[usize; 32]>,
    mm: Vec<i32>,
    pp: Vec<i32>,
    orig: Vec<char>,
    lc: Vec<char>,
    bonus: Vec<i32>,
    bonus_tag: Vec<u32>,
    tag: u32,
}

impl Scratch {
    fn new(m: usize) -> Self {
        let mut sc = Self::default();
        sc.first.resize(m, 0);
        sc.last.resize(m, 0);
        sc
    }

    #[inline]
    fn next_tag(&mut self) -> u32 {
        self.tag = self.tag.wrapping_add(1);
        if self.tag == 0 {
            self.bonus_tag.fill(0);
            self.tag = 1;
        }
        self.tag
    }

    #[inline]
    fn unicode_bonus_at(&mut self, j: usize) -> i32 {
        if self.bonus_tag[j] != self.tag {
            self.bonus_tag[j] = self.tag;
            self.bonus[j] = boundary_bonus(if j == 0 { None } else { Some(self.orig[j - 1]) }, self.orig[j]);
        }
        self.bonus[j]
    }
}

trait CandidateView {
    type RowQuery: Copy;
    fn row_query(&self, i: usize) -> Self::RowQuery;
    fn matches(&self, rq: Self::RowQuery, j: usize) -> bool;
    fn bonus_at(&mut self, j: usize) -> i32;
}

struct AsciiView<'a> {
    qb: &'a [u8],
    masks: &'a [u8],
    bytes: &'a [u8],
}

impl CandidateView for AsciiView<'_> {
    type RowQuery = (u8, u8);

    #[inline]
    fn row_query(&self, i: usize) -> (u8, u8) {
        (self.qb[i], self.masks[i])
    }

    #[inline]
    fn matches(&self, rq: (u8, u8), j: usize) -> bool {
        (self.bytes[j] | rq.1) == rq.0
    }

    #[inline]
    fn bonus_at(&mut self, j: usize) -> i32 {
        boundary_bonus_ascii(self.bytes, j)
    }
}

struct UnicodeView<'a> {
    q: &'a [char],
    orig: &'a [char],
    lc: &'a [char],
    bonus: &'a mut [i32],
    bonus_tag: &'a mut [u32],
    tag: u32,
}

impl CandidateView for UnicodeView<'_> {
    type RowQuery = char;

    #[inline]
    fn row_query(&self, i: usize) -> char {
        self.q[i]
    }

    #[inline]
    fn matches(&self, rq: char, j: usize) -> bool {
        self.lc[j] == rq
    }

    #[inline]
    fn bonus_at(&mut self, j: usize) -> i32 {
        if self.bonus_tag[j] != self.tag {
            self.bonus_tag[j] = self.tag;
            self.bonus[j] = boundary_bonus(if j == 0 { None } else { Some(self.orig[j - 1]) }, self.orig[j]);
        }
        self.bonus[j]
    }
}

enum MatchPath {
    Single(usize),
    Matrix { bj: usize, base: usize, w: usize, is_ascii: bool },
}

#[inline]
fn dp_score_single_ascii(target: u8, mask: u8, bytes: &[u8]) -> Option<(i32, MatchPath)> {
    let n = bytes.len();
    let (mut bj, mut bs) = (usize::MAX, NEG);
    for (j, &b) in bytes.iter().enumerate() {
        if (b | mask) == target {
            let mut s = MATCH + boundary_bonus_ascii(bytes, j) * 2;
            if j == 0 {
                s += 6;
            }
            if s > bs {
                bs = s;
                bj = j;
            }
        }
    }
    if bj == usize::MAX { None } else { Some((bs - (n as i32) / 4, MatchPath::Single(bj))) }
}

#[inline]
fn dp_score_single_unicode(target: char, candidate: &str) -> Option<(i32, MatchPath)> {
    let mut prev: Option<char> = None;
    let mut n = 0usize;
    let (mut bj, mut bs) = (usize::MAX, NEG);
    for c in candidate.chars().take(MAX_CHARS) {
        if fold(c) == target {
            let mut s = MATCH + boundary_bonus(prev, c) * 2;
            if n == 0 {
                s += 6;
            }
            if s > bs {
                bs = s;
                bj = n;
            }
        }
        prev = Some(c);
        n += 1;
    }
    if bj == usize::MAX { None } else { Some((bs - (n as i32) / 4, MatchPath::Single(bj))) }
}

fn dp_solve<V: CandidateView>(
    view: &mut V,
    m: usize,
    n: usize,
    first: &[usize],
    last: &[usize],
    mm: &mut Vec<i32>,
    pp: &mut Vec<i32>,
) -> Option<(i32, usize, usize, usize)> {
    // Only columns in first[0]..=last[m - 1] can participate in any valid completion.
    let base = first[0];
    let w = last[m - 1] - base + 1;
    let need = m * w;
    if mm.len() < need {
        mm.resize(need, NEG);
        pp.resize(need, NEG);
    }

    // Row 0: columns first[0]..last[1] (m >= 2).
    {
        let rq = view.row_query(0);
        let col_start = first[0];
        let col_end = last[1];
        let k_end = col_end - base;
        let mm_row = &mut mm[..k_end];
        let pp_row = &mut pp[..k_end];
        let mut prev_gap = NEG;
        for j in col_start..col_end {
            let k = j - base;
            let mut best = NEG;
            if view.matches(rq, j) {
                best = MATCH + view.bonus_at(j) * 2;
                if j == 0 {
                    best += 6; // prefix
                }
            }
            mm_row[k] = best;
            let mut g = if prev_gap > NEG { prev_gap - GAP_EXT } else { NEG };
            if best > NEG {
                g = g.max(best - GAP_START);
            }
            pp_row[k] = g;
            prev_gap = g;
        }
    }

    // Rows 1..m: only columns first[i]..row_end need to be evaluated.
    // For any j < row_end where lc[j] == q[i], j <= last[i], so j - 1 lies in
    // first[i - 1]..row_end(i - 1) and was initialized by the previous row.
    for i in 1..m {
        let rq = view.row_query(i);
        let col_start = first[i];
        let col_end = if i + 1 < m { last[i + 1] } else { last[i] + 1 };
        let k_end = col_end - base;
        let (mm_head, mm_tail) = mm[..(i + 1) * w].split_at_mut(i * w);
        let mm_prev = &mm_head[(i - 1) * w..][..k_end];
        let mm_cur = &mut mm_tail[..k_end];
        let (pp_head, pp_tail) = pp[..(i + 1) * w].split_at_mut(i * w);
        let pp_prev = &pp_head[(i - 1) * w..][..k_end];
        let pp_cur = &mut pp_tail[..k_end];

        // pp[i, first[i] - 1] is always NEG because no match of q[0..=i] ends before first[i].
        let mut prev_gap = NEG;
        for j in col_start..col_end {
            let k = j - base;
            let mut best = NEG;
            if view.matches(rq, j) {
                let b = view.bonus_at(j);
                let diag = mm_prev[k - 1];
                if diag > NEG {
                    best = diag + MATCH + b.max(CONSEC);
                }
                let gap = pp_prev[k - 1];
                if gap > NEG {
                    best = best.max(gap + MATCH + b);
                }
            }
            mm_cur[k] = best;
            let mut g = if prev_gap > NEG { prev_gap - GAP_EXT } else { NEG };
            if best > NEG {
                g = g.max(best - GAP_START);
            }
            pp_cur[k] = g;
            prev_gap = g;
        }
    }

    let last_row = &mm[(m - 1) * w..m * w];
    let (mut bj, mut bs) = (usize::MAX, NEG);
    for j in first[m - 1]..=last[m - 1] {
        let s = last_row[j - base];
        if s > bs {
            bs = s;
            bj = j;
        }
    }
    if bj == usize::MAX {
        return None;
    }
    Some((bs - (n as i32) / 4, bj, base, w))
}

fn dp_score(pq: &PreparedQuery, candidate: &str, sc: &mut Scratch) -> Option<(i32, MatchPath)> {
    let m = pq.chars.len();
    // Checking only the first MAX_CHARS bytes for ASCII is exact because any multi-byte
    // UTF-8 sequence starting before char MAX_CHARS puts a byte >= 0x80 inside this prefix.
    let raw = candidate.as_bytes();
    let prefix_ascii = raw[..raw.len().min(MAX_CHARS)].is_ascii();

    if prefix_ascii {
        let Some(qb) = pq.ascii.as_deref() else {
            // Query has a non-ASCII folded char that cannot match any ASCII character.
            return None;
        };
        let n = raw.len().min(MAX_CHARS);
        if m > n {
            return None;
        }
        let bytes = &raw[..n];
        let masks = &pq.masks;
        if m == 1 {
            return dp_score_single_ascii(qb[0], masks[0], bytes);
        }

        let first = &mut sc.first[..m];
        let mut qi = 0usize;
        let mut target = qb[0];
        let mut mask = masks[0];
        for (j, &b) in bytes.iter().enumerate() {
            if (b | mask) == target {
                first[qi] = j;
                qi += 1;
                if qi == m {
                    break;
                }
                target = qb[qi];
                mask = masks[qi];
            }
        }
        if qi < m {
            return None;
        }

        let last = &mut sc.last[..m];
        let mut qi = m - 1;
        let mut target = qb[qi];
        let mut mask = masks[qi];
        let start = first[0];
        for j in (start..n).rev() {
            if (bytes[j] | mask) == target {
                last[qi] = j;
                if qi == 0 {
                    break;
                }
                qi -= 1;
                target = qb[qi];
                mask = masks[qi];
            }
        }

        let mut view = AsciiView { qb, masks, bytes };
        let (score, bj, base, w) = dp_solve(&mut view, m, n, first, last, &mut sc.mm, &mut sc.pp)?;
        Some((score, MatchPath::Matrix { bj, base, w, is_ascii: true }))
    } else {
        if m == 1 {
            return dp_score_single_unicode(pq.chars[0], candidate);
        }

        // Pre-filter on streaming chars before touching sc.orig/sc.lc.
        let mut chars = candidate.chars().take(MAX_CHARS);
        let mut qi = 0usize;
        let mut target = pq.chars[0];
        let mut n = 0usize;
        {
            let first = &mut sc.first[..m];
            while let Some(c) = chars.next() {
                n += 1;
                if fold(c) == target {
                    first[qi] = n - 1;
                    qi += 1;
                    if qi == m {
                        n += chars.count();
                        break;
                    }
                    target = pq.chars[qi];
                }
            }
        }
        if qi < m {
            return None;
        }

        sc.orig.clear();
        sc.lc.clear();
        for c in candidate.chars().take(MAX_CHARS) {
            sc.orig.push(c);
            sc.lc.push(fold(c));
        }
        if sc.bonus.len() < n {
            sc.bonus.resize(n, 0);
            sc.bonus_tag.resize(n, 0);
        }
        let tag = sc.next_tag();

        let first = &sc.first[..m];
        let last = &mut sc.last[..m];
        let mut qi = m - 1;
        let mut target = pq.chars[qi];
        let start = first[0];
        for j in (start..n).rev() {
            if sc.lc[j] == target {
                last[qi] = j;
                if qi == 0 {
                    break;
                }
                qi -= 1;
                target = pq.chars[qi];
            }
        }

        let mut view = UnicodeView { q: &pq.chars, orig: &sc.orig, lc: &sc.lc, bonus: &mut sc.bonus, bonus_tag: &mut sc.bonus_tag, tag };
        let (score, bj, base, w) = dp_solve(&mut view, m, n, first, last, &mut sc.mm, &mut sc.pp)?;
        Some((score, MatchPath::Matrix { bj, base, w, is_ascii: false }))
    }
}

fn backtrack(path: MatchPath, m: usize, candidate: &str, sc: &mut Scratch) -> SmallVec<[u32; 16]> {
    let (bj, base, w, is_ascii) = match path {
        MatchPath::Single(bj) => {
            let mut pos = SmallVec::new();
            pos.push(bj as u32);
            return pos;
        }
        MatchPath::Matrix { bj, base, w, is_ascii } => (bj, base, w, is_ascii),
    };
    let ascii_bytes = if is_ascii {
        let raw = candidate.as_bytes();
        &raw[..raw.len().min(MAX_CHARS)]
    } else {
        &[]
    };
    let mut pos: SmallVec<[u32; 16]> = SmallVec::new();
    let (mut i, mut j) = (m - 1, bj);
    loop {
        pos.push(j as u32);
        if i == 0 {
            break;
        }
        let cur = sc.mm[i * w + (j - base)];
        let prev_row = (i - 1) * w;
        let diag = sc.mm[prev_row + (j - 1 - base)];
        let b = if is_ascii { boundary_bonus_ascii(ascii_bytes, j) } else { sc.unicode_bonus_at(j) };
        if diag > NEG && diag + MATCH + b.max(CONSEC) == cur {
            j -= 1;
        } else {
            let floor = sc.first[i - 1];
            let mut t = j - 1;
            loop {
                let idx = prev_row + (t - base);
                let mv = sc.mm[idx];
                if mv > NEG && sc.pp[idx] == mv - GAP_START {
                    break;
                }
                if t == floor {
                    break;
                }
                t -= 1;
            }
            j = t;
        }
        i -= 1;
    }
    pos.reverse();
    pos
}

pub fn fuzzy_match(query: &str, candidate: &str) -> Option<FuzzyMatch> {
    let pq = PreparedQuery::new(query);
    let m = pq.chars.len();
    if m == 0 {
        return Some(FuzzyMatch { score: 0, positions: SmallVec::new() });
    }
    if m > MAX_CHARS {
        return None;
    }
    let mut sc = Scratch::new(m);
    let (score, path) = dp_score(&pq, candidate, &mut sc)?;
    let positions = backtrack(path, m, candidate, &mut sc);
    Some(FuzzyMatch { score, positions })
}

struct HeapItem<T> {
    score: i32,
    idx: usize,
    item: T,
    positions: SmallVec<[u32; 16]>,
}

impl<T> PartialEq for HeapItem<T> {
    fn eq(&self, other: &Self) -> bool {
        self.score == other.score && self.idx == other.idx
    }
}

impl<T> Eq for HeapItem<T> {}

impl<T> PartialOrd for HeapItem<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for HeapItem<T> {
    /// Inverted relative to output rank order so `BinaryHeap::peek()` holds the worst of the top-`limit`
    /// items (lowest score, or largest input index on tie) and `into_sorted_vec()` emits best-first.
    fn cmp(&self, other: &Self) -> Ordering {
        other.score.cmp(&self.score).then_with(|| self.idx.cmp(&other.idx))
    }
}

/// Best first; ties keep input order. An empty query keeps the input order.
pub fn fuzzy_rank<'a, T>(query: &str, items: impl IntoIterator<Item = (T, &'a str)>, limit: usize) -> Vec<(T, FuzzyMatch)> {
    if limit == 0 {
        return Vec::new();
    }
    let pq = PreparedQuery::new(query);
    let m = pq.chars.len();
    if m == 0 {
        return items.into_iter().take(limit).map(|(t, _)| (t, FuzzyMatch { score: 0, positions: SmallVec::new() })).collect();
    }
    if m > MAX_CHARS {
        return Vec::new();
    }
    let mut sc = Scratch::new(m);
    let mut heap: BinaryHeap<HeapItem<T>> = BinaryHeap::with_capacity(limit.min(1024));
    for (idx, (t, s)) in items.into_iter().enumerate() {
        let Some((score, path)) = dp_score(&pq, s, &mut sc) else {
            continue;
        };
        if heap.len() == limit {
            // Because `idx` increases monotonically, an equal score always loses tie-breaking.
            if let Some(top) = heap.peek()
                && score <= top.score
            {
                continue;
            }
            let positions = backtrack(path, m, s, &mut sc);
            if let Some(mut top) = heap.peek_mut() {
                *top = HeapItem { score, idx, item: t, positions };
            }
        } else {
            let positions = backtrack(path, m, s, &mut sc);
            heap.push(HeapItem { score, idx, item: t, positions });
        }
    }
    heap.into_sorted_vec().into_iter().map(|h| (h.item, FuzzyMatch { score: h.score, positions: h.positions })).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(q: &str, c: &str) -> i32 {
        fuzzy_match(q, c).unwrap().score
    }

    #[test]
    fn subsequence_only() {
        assert!(fuzzy_match("abc", "a_b_c").is_some());
        assert!(fuzzy_match("abc", "acb").is_none());
        assert!(fuzzy_match("abcd", "abc").is_none());
        assert!(fuzzy_match("", "x").unwrap().positions.is_empty());
        assert!(fuzzy_match("ÄB", "äxb").is_some());
    }

    #[test]
    fn positions_are_correct() {
        let m = fuzzy_match("rdm", "docs/readme.md").unwrap();
        let chars: Vec<char> = "docs/readme.md".chars().collect();
        assert_eq!(m.positions.len(), 3);
        for (p, q) in m.positions.iter().zip("rdm".chars()) {
            assert_eq!(chars[*p as usize].to_ascii_lowercase(), q);
        }
        let m = fuzzy_match("read", "docs/readme.md").unwrap();
        assert_eq!(m.positions.as_slice(), &[5, 6, 7, 8]);
    }

    #[test]
    fn ordering_sanity() {
        // consecutive beats scattered
        assert!(score("note", "notes.md") > score("note", "n_o_t_e.md"));
        // word start beats mid-word
        assert!(score("fb", "foo-bar") > score("fb", "afoxbar"));
        // prefix beats later
        assert!(score("not", "note") > score("not", "a-note-b-extra-long"));
        // camelCase boundary
        assert!(score("fb", "fooBar") > score("fb", "foobar"));
        // shorter wins at equal quality
        assert!(score("a", "a") > score("a", "a-very-long-name"));
        // path separator bonus
        assert!(score("r", "docs/readme") > score("r", "docsareadme"));
    }

    #[test]
    fn rank() {
        let items = vec![(0, "src/main.rs"), (1, "README.md"), (2, "docs/maintenance.md"), (3, "zzz")];
        let r = fuzzy_rank("main", items.clone(), 10);
        assert_eq!(r.iter().map(|x| x.0).collect::<Vec<_>>(), vec![0, 2]);
        let all = fuzzy_rank("", items.clone(), 3);
        assert_eq!(all.iter().map(|x| x.0).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert_eq!(fuzzy_rank("m", items, 1).len(), 1);
    }

    /// Reference implementation matching the original naive full-matrix DP.
    fn naive_fuzzy_match_q(q: &[char], candidate: &str) -> Option<FuzzyMatch> {
        let m = q.len();
        if m == 0 {
            return Some(FuzzyMatch { score: 0, positions: SmallVec::new() });
        }
        let orig: Vec<char> = candidate.chars().take(MAX_CHARS).collect();
        let n = orig.len();
        if m > n {
            return None;
        }
        let lc: Vec<char> = orig.iter().map(|&c| fold(c)).collect();
        let mut qi = 0;
        for &c in &lc {
            if c == q[qi] {
                qi += 1;
                if qi == m {
                    break;
                }
            }
        }
        if qi < m {
            return None;
        }
        let bonus: Vec<i32> = (0..n).map(|j| boundary_bonus(if j == 0 { None } else { Some(orig[j - 1]) }, orig[j])).collect();
        let mut mm = vec![NEG; m * n];
        let mut pp = vec![NEG; m * n];
        for i in 0..m {
            for j in i..n {
                let mut best = NEG;
                if lc[j] == q[i] {
                    if i == 0 {
                        best = MATCH + bonus[j] * 2;
                        if j == 0 {
                            best += 6;
                        }
                    } else if j > 0 {
                        let diag = mm[(i - 1) * n + j - 1];
                        if diag > NEG {
                            best = diag + MATCH + bonus[j].max(CONSEC);
                        }
                        let gap = pp[(i - 1) * n + j - 1];
                        if gap > NEG {
                            best = best.max(gap + MATCH + bonus[j]);
                        }
                    }
                }
                mm[i * n + j] = best;
                let prev_gap = if j > 0 { pp[i * n + j - 1] } else { NEG };
                let mut g = if prev_gap > NEG { prev_gap - GAP_EXT } else { NEG };
                if best > NEG {
                    g = g.max(best - GAP_START);
                }
                pp[i * n + j] = g;
            }
        }
        let last = (m - 1) * n;
        let (mut bj, mut bs) = (usize::MAX, NEG);
        for j in (m - 1)..n {
            let s = mm[last + j];
            if s > bs {
                bs = s;
                bj = j;
            }
        }
        if bj == usize::MAX {
            return None;
        }
        let mut pos: SmallVec<[u32; 16]> = SmallVec::new();
        let (mut i, mut j) = (m - 1, bj);
        loop {
            pos.push(j as u32);
            if i == 0 {
                break;
            }
            let cur = mm[i * n + j];
            let diag = mm[(i - 1) * n + j - 1];
            if diag > NEG && diag + MATCH + bonus[j].max(CONSEC) == cur {
                j -= 1;
            } else {
                let mut t = j - 1;
                loop {
                    let mv = mm[(i - 1) * n + t];
                    if mv > NEG && pp[(i - 1) * n + t] == mv - GAP_START {
                        break;
                    }
                    if t == 0 {
                        break;
                    }
                    t -= 1;
                }
                j = t;
            }
            i -= 1;
        }
        pos.reverse();
        Some(FuzzyMatch { score: bs - (n as i32) / 4, positions: pos })
    }

    fn naive_fuzzy_match(query: &str, candidate: &str) -> Option<FuzzyMatch> {
        let q: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).map(fold).collect();
        naive_fuzzy_match_q(&q, candidate)
    }

    fn naive_fuzzy_rank<'a, T>(query: &str, items: impl IntoIterator<Item = (T, &'a str)>, limit: usize) -> Vec<(T, FuzzyMatch)> {
        if query.chars().all(char::is_whitespace) {
            return items.into_iter().take(limit).map(|(t, _)| (t, FuzzyMatch { score: 0, positions: SmallVec::new() })).collect();
        }
        let q: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).map(fold).collect();
        let mut v: Vec<(usize, T, FuzzyMatch)> =
            items.into_iter().enumerate().filter_map(|(i, (t, s))| naive_fuzzy_match_q(&q, s).map(|m| (i, t, m))).collect();
        v.sort_by(|a, b| b.2.score.cmp(&a.2.score).then(a.0.cmp(&b.0)));
        v.truncate(limit);
        v.into_iter().map(|(_, t, m)| (t, m)).collect()
    }

    #[test]
    fn ascii_fast_helpers_exact() {
        for c in 0u8..128 {
            assert_eq!(boundary_bonus_ascii(&[c], 0), boundary_bonus(None, c as char));
            for p in 0u8..128 {
                assert_eq!(boundary_bonus_ascii(&[p, c], 1), boundary_bonus(Some(p as char), c as char), "mismatch at prev={p:?} cur={c:?}");
            }
            let target = (c as char).to_ascii_lowercase() as u8;
            let mask = if target.is_ascii_lowercase() { 0x20 } else { 0 };
            for b in 0u8..=255 {
                assert_eq!((b | mask) == target, b.to_ascii_lowercase() == target, "mask mismatch target={target} b={b}");
            }
        }
    }

    #[test]
    fn equivalence_against_naive_dp() {
        let long_ascii_511 = "a".repeat(510) + "z";
        let long_ascii_512 = "a".repeat(511) + "z";
        let long_ascii_513 = "a".repeat(512) + "z";
        let long_ascii_600 = format!("{}/note123_camelCaseEnd", "x/".repeat(290));
        let long_utf8_prefix_ascii = format!("{}ä", "a".repeat(512));
        let long_utf8_mid = format!("{}ä/{}z", "a".repeat(120), "b".repeat(420));
        let long_query_512 = "a".repeat(512);
        let long_query_513 = "a".repeat(513);

        let candidates: Vec<&str> = vec![
            "",
            "a",
            "Z",
            "ab",
            "axxb",
            "axbbc",
            "axbbxc",
            "ababab",
            "a_b_c",
            "a/b\\c-d_e f.g:h",
            "fooBarBaz",
            "fooBar2Baz3",
            "docs/readme.md",
            "docs/README.md",
            "folder00/sub00/note0-design-notes.md",
            "folder12/sub04/note19-design-notes.md",
            "folder12/sub04/note190-design-notes.md",
            "tied_one/item",
            "tied_two/item",
            "tied_three/item",
            "äxb",
            "ÄÖÜ/café-résumé_über.md",
            "kelvin/K_test",
            "kelvin/k_test",
            "turkish/İ_dot",
            "turkish/i\u{307}_dot",
            "sharp/ß_strasse",
            "cjk/日本語ノート/設計メモ.md",
            "emoji/🦀_rust-notes.typ",
            &long_ascii_511,
            &long_ascii_512,
            &long_ascii_513,
            &long_ascii_600,
            &long_utf8_prefix_ascii,
            &long_utf8_mid,
        ];

        let queries: Vec<&str> = vec![
            "",
            "   \t\n ",
            "a",
            "z",
            "Ä",
            "äb",
            "K",
            "İ",
            "ß",
            "🦀",
            "日本語",
            "ab",
            "abc",
            "fb",
            "fbb",
            "rdm",
            "note",
            "n o t e",
            "f12n19",
            "tied",
            "item",
            "cafe",
            "café",
            "camelCase",
            "az",
            &long_query_512,
            &long_query_513,
        ];

        for &q in &queries {
            for &c in &candidates {
                let got = fuzzy_match(q, c);
                let exp = naive_fuzzy_match(q, c);
                assert_eq!(got, exp, "fuzzy_match mismatch for query={q:?}, candidate={c:?}");
            }
            for limit in [0, 1, 2, 5, 20, 100] {
                let got = fuzzy_rank(q, candidates.iter().enumerate().map(|(i, &s)| (i, s)), limit);
                let exp = naive_fuzzy_rank(q, candidates.iter().enumerate().map(|(i, &s)| (i, s)), limit);
                assert_eq!(got, exp, "fuzzy_rank mismatch for query={q:?}, limit={limit}");
            }
        }
    }
}
