//! Minimal glob matching for the "exclude from indexing" setting.
//!
//! A pattern without `/` matches an entry name at any depth (`node_modules`, `*.bak`).
//! A pattern with `/` matches the path relative to the workspace root (`drafts/*`, `**/old/**`).
//! `*` matches within one path segment, `**` across segments, `?` one character. Matching is
//! case-sensitive; a trailing `/` is ignored.

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ExcludeSet {
    name_pats: Vec<String>,
    path_pats: Vec<String>,
}

impl ExcludeSet {
    pub fn new<S: AsRef<str>>(patterns: &[S]) -> Self {
        let mut s = ExcludeSet::default();
        for p in patterns {
            let p = p.as_ref().trim().trim_end_matches('/').trim_start_matches("./");
            if p.is_empty() || p.starts_with('#') {
                continue;
            }
            if p.contains('/') {
                s.path_pats.push(p.trim_start_matches('/').to_string());
            } else {
                s.name_pats.push(p.to_string());
            }
        }
        s
    }

    pub fn is_empty(&self) -> bool {
        self.name_pats.is_empty() && self.path_pats.is_empty()
    }

    /// `rel` uses `/` separators and is relative to the root; `name` is its last segment.
    pub fn matches(&self, rel: &str, name: &str) -> bool {
        self.name_pats.iter().any(|p| glob_match(p, name)) || self.path_pats.iter().any(|p| glob_match(p, rel))
    }
}

/// `*` stops at `/`, `**` does not.
pub fn glob_match(pat: &str, text: &str) -> bool {
    let p = pat.as_bytes();
    let t = text.as_bytes();
    if !p.contains(&b'*') && !p.contains(&b'?') {
        return p == t;
    }
    let n = t.len();
    let mut stack_dp = [false; 256];
    let mut stack_saved = [false; 256];
    let mut heap_dp = Vec::new();
    let mut heap_saved = Vec::new();
    let (dp, saved): (&mut [bool], &mut [bool]) = if n < 256 {
        (&mut stack_dp[..=n], &mut stack_saved[..=n])
    } else {
        heap_dp.resize(n + 1, false);
        heap_saved.resize(n + 1, false);
        (&mut heap_dp, &mut heap_saved)
    };
    dp[0] = true;

    let mut i = 0;
    while i < p.len() {
        if p[i] == b'*' {
            if p.get(i + 1) == Some(&b'*') {
                if p.get(i + 2) == Some(&b'/') {
                    // `**/` matches either zero segments or `**` followed by `/`.
                    saved.copy_from_slice(dp);
                    for j in 1..=n {
                        dp[j] |= dp[j - 1];
                    }
                    let mut any = false;
                    for j in (1..=n).rev() {
                        let ok = (dp[j - 1] && t[j - 1] == b'/') || saved[j];
                        dp[j] = ok;
                        any |= ok;
                    }
                    dp[0] = saved[0];
                    any |= dp[0];
                    if !any {
                        return false;
                    }
                    i += 3;
                } else {
                    if i + 2 == p.len() {
                        return dp.iter().any(|&b| b);
                    }
                    for j in 1..=n {
                        dp[j] |= dp[j - 1];
                    }
                    i += 2;
                }
            } else {
                for j in 1..=n {
                    if t[j - 1] != b'/' {
                        dp[j] |= dp[j - 1];
                    }
                }
                i += 1;
            }
        } else if p[i] == b'?' {
            let mut any = false;
            for j in (1..=n).rev() {
                let ok = dp[j - 1] && t[j - 1] != b'/';
                dp[j] = ok;
                any |= ok;
            }
            dp[0] = false;
            if !any {
                return false;
            }
            i += 1;
        } else {
            let c = p[i];
            let mut any = false;
            for j in (1..=n).rev() {
                let ok = dp[j - 1] && t[j - 1] == c;
                dp[j] = ok;
                any |= ok;
            }
            dp[0] = false;
            if !any {
                return false;
            }
            i += 1;
        }
    }
    dp[n]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ref_glob_match(pat: &str, text: &str) -> bool {
        fn go(p: &[u8], t: &[u8]) -> bool {
            match p.first() {
                None => t.is_empty(),
                Some(b'*') => {
                    let double = p.get(1) == Some(&b'*');
                    let rest = if double { &p[2..] } else { &p[1..] };
                    if double && rest.first() == Some(&b'/') && go(&rest[1..], t) {
                        return true;
                    }
                    let mut i = 0;
                    loop {
                        if go(rest, &t[i..]) {
                            return true;
                        }
                        if i >= t.len() || (!double && t[i] == b'/') {
                            return false;
                        }
                        i += 1;
                    }
                }
                Some(b'?') => !t.is_empty() && t[0] != b'/' && go(&p[1..], &t[1..]),
                Some(c) => t.first() == Some(c) && go(&p[1..], &t[1..]),
            }
        }
        go(pat.as_bytes(), text.as_bytes())
    }

    #[test]
    fn globs() {
        assert!(glob_match("*.bak", "a.bak"));
        assert!(!glob_match("*.bak", "a/b.bak"));
        assert!(glob_match("**/*.bak", "a/b.bak"));
        assert!(glob_match("**/*.bak", "b.bak"));
        assert!(glob_match("a?c", "abc"));
        assert!(glob_match("drafts/**", "drafts/x/y.md"));
        assert!(!glob_match("drafts/*", "drafts/x/y.md"));
    }

    #[test]
    fn matches_reference_on_edge_cases() {
        let pats = [
            "",
            "*",
            "**",
            "**/",
            "**/*",
            "**/**",
            "***",
            "****/",
            "a/**/b",
            "a/**/",
            "a**b",
            "a*b",
            "a?b",
            "**/old/**",
            "**/a/**/b",
            "drafts/**",
            "drafts/*",
            "*.md",
            "**/*.md",
            "plain/path.txt",
        ];
        let texts = [
            "",
            "a",
            "/",
            "a/",
            "a/b",
            "a/x/b",
            "a/x/y/b",
            "ab",
            "old",
            "old/x",
            "x/old/y",
            "x/old",
            "note.md",
            "dir/note.md",
            "drafts/x/y.md",
            "plain/path.txt",
        ];
        for p in pats {
            for t in texts {
                assert_eq!(glob_match(p, t), ref_glob_match(p, t), "pat={p:?} text={t:?}");
            }
        }
        // Long (> 256 byte) text exercises heap fallback cleanly.
        let long = format!("{}/file.md", "seg/".repeat(80));
        assert!(long.len() > 256);
        assert!(glob_match("**/file.md", &long));
        assert!(!glob_match("**/other.md", &long));
    }

    #[test]
    fn adversarial_patterns_do_not_backtrack() {
        let t0 = std::time::Instant::now();
        assert!(!glob_match("**/*/*/*/*/*/*/*/*/*/*/*/*.md", "a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v/w/x.txt"));
        assert!(!glob_match("a*a*a*a*a*a*a*a*a*a*a*a*a*b", &"a".repeat(120)));
        assert!(!glob_match("**/a/**/b/**/c/**/d/**/e/**/z", "a/b/c/d/e/a/b/c/d/e/a/b/c/d/e/x"));
        assert!(glob_match("**/a/**/b/**/c/**/d/**/e/**/z", "x/a/y/b/c/d/w/e/z"));
        assert!(t0.elapsed() < std::time::Duration::from_millis(50));
    }

    #[test]
    fn set() {
        use std::hash::{Hash, Hasher};
        let hash_of = |s: &ExcludeSet| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            s.hash(&mut h);
            h.finish()
        };
        let s = ExcludeSet::new(&["node_modules", "*.tmp", "drafts/", "# c", "", "build/out/**"]);
        let s_same = ExcludeSet::new(&["node_modules", "*.tmp", "drafts", "build/out/**"]);
        let s_diff = ExcludeSet::new(&["node_modules", "*.bak"]);
        assert_eq!(s, s_same);
        assert_eq!(hash_of(&s), hash_of(&s_same));
        assert_ne!(s, s_diff);
        assert!(s.matches("a/node_modules", "node_modules"));
        assert!(s.matches("x.tmp", "x.tmp"));
        assert!(s.matches("build/out/a/b", "b"));
        assert!(!s.matches("src/a.md", "a.md"));
        assert!(ExcludeSet::new::<&str>(&[]).is_empty());
    }
}
