//! New-tab page content: the quote at the bottom.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quote {
    pub text: String,
    pub author: Option<String>,
}

/// Public-domain quotes shown when no quotes file is set.
pub const BUILTIN_QUOTES: [(&str, &str); 10] = [
    ("The only thing we have to fear is fear itself.", "Franklin D. Roosevelt"),
    ("It is not that we have a short time to live, but that we waste a lot of it.", "Seneca"),
    ("He who has a why to live can bear almost any how.", "Friedrich Nietzsche"),
    ("The unexamined life is not worth living.", "Socrates"),
    ("We are what we repeatedly do. Excellence, then, is not an act, but a habit.", "Will Durant"),
    ("Simplicity is the ultimate sophistication.", "Leonardo da Vinci"),
    ("Well begun is half done.", "Aristotle"),
    ("Knowing yourself is the beginning of all wisdom.", "Aristotle"),
    ("A journey of a thousand miles begins with a single step.", "Lao Tzu"),
    ("What we think, we become.", "Buddha"),
];

/// One quote per line; ` — ` (or ` -- `) splits off the author. Blank lines and `#` comments are skipped.
pub fn parse_quotes(text: &str) -> Vec<Quote> {
    text.trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| match l.rsplit_once(" — ").or_else(|| l.rsplit_once(" -- ")) {
            Some((q, a)) if !q.trim().is_empty() && !a.trim().is_empty() => Quote { text: q.trim().into(), author: Some(a.trim().into()) },
            _ => Quote { text: l.into(), author: None },
        })
        .collect()
}

/// A quote for a new tab: from the file when it holds any, else a built-in one. `seed` picks. The error is the
/// reason the file was not used (the built-ins are used then).
pub fn pick(file: &str, seed: u64) -> (Option<Quote>, Option<String>) {
    let mut err = None;
    let mut list = Vec::new();
    if !file.trim().is_empty() {
        match std::fs::read_to_string(file.trim()) {
            Ok(t) => list = parse_quotes(&t),
            Err(e) => err = Some(format!("Quotes file {file}: {e}")),
        }
        if list.is_empty() && err.is_none() {
            err = Some(format!("Quotes file {file} has no quotes"));
        }
    }
    if list.is_empty() {
        list = BUILTIN_QUOTES.iter().map(|(q, a)| Quote { text: (*q).into(), author: Some((*a).into()) }).collect();
    }
    let i = (seed % list.len() as u64) as usize;
    (Some(list.swap_remove(i)), err)
}

/// Different on every call (nanosecond clock); no RNG crate for one pick per new tab.
pub fn seed() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64 / 7)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lines_authors_and_skips_junk() {
        let q = parse_quotes("# c\n\nPlain one\nWise words — Someone\n — \nA -- B\n");
        assert_eq!(q.len(), 4);
        assert_eq!(parse_quotes("\u{feff}First — A")[0].text, "First", "a BOM is not part of the quote");
        assert_eq!(q[0], Quote { text: "Plain one".into(), author: None });
        assert_eq!(q[1], Quote { text: "Wise words".into(), author: Some("Someone".into()) });
        assert_eq!(q[2].author, None, "a lone dash line stays a quote");
        assert_eq!(q[3].author.as_deref(), Some("B"));
    }

    #[test]
    fn pick_uses_file_else_builtins_and_reports_problems() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("q.txt");
        std::fs::write(&f, "Only — Me\n").unwrap();
        let (q, e) = pick(f.to_str().unwrap(), 5);
        assert_eq!((q.unwrap().text, e), ("Only".into(), None));
        let (q, e) = pick("/no/such/file", 3);
        assert!(q.is_some() && e.unwrap().contains("/no/such/file"));
        std::fs::write(&f, "\n#x\n").unwrap();
        assert!(pick(f.to_str().unwrap(), 1).1.is_some());
        assert!(pick("", 99).1.is_none());
    }
}
