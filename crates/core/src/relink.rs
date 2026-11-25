//! Rewrites the links in notes when a file or folder is renamed or moved.
//!
//! `Plan::build` runs on the index before the move: it records where every affected link pointed. `rewrite` then needs
//! no index, so it can run later on any thread. Only Markdown notes are rewritten (Typst notes are not).
use crate::index::Index;
use crate::kind::FileKind;
use crate::links::{LinkSpan, scan_markdown_spans};
use crate::textio;
use rustc_hash::{FxHashMap, FxHashSet};
use std::io;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Default)]
pub struct Plan {
    /// Every file that moves: old path -> new path.
    moves: FxHashMap<PathBuf, PathBuf>,
    /// (note path before the move, link target as written) -> file the link pointed at before the move.
    resolved: FxHashMap<(PathBuf, String), PathBuf>,
    /// New paths whose short name another file (that stays) already has: such a link must become a path.
    taken: FxHashSet<PathBuf>,
    roots: Vec<PathBuf>,
    /// Notes (paths before the move) to rewrite, sorted.
    pub notes: Vec<PathBuf>,
    /// How many of those link to something that moves (the number the user is asked about).
    pub linking: usize,
}

/// Lowercase `s`; short ASCII names (nearly all) need no allocation.
fn lower<'a>(s: &str, buf: &'a mut [u8; 64]) -> std::borrow::Cow<'a, str> {
    if s.len() <= buf.len() && s.is_ascii() {
        buf[..s.len()].copy_from_slice(s.as_bytes());
        buf[..s.len()].make_ascii_lowercase();
        return std::borrow::Cow::Borrowed(std::str::from_utf8(&buf[..s.len()]).unwrap_or_default());
    }
    std::borrow::Cow::Owned(s.to_lowercase())
}

fn lc(s: Option<&std::ffi::OsStr>) -> String {
    s.map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// `to` relative to the directory `from_dir`, with `/` separators.
/// `None` when they share no start (other drive on Windows).
fn rel_path(from_dir: &Path, to: &Path) -> Option<String> {
    let (a, b): (Vec<Component>, Vec<Component>) = (from_dir.components().collect(), to.components().collect());
    let n = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    if n == 0 && a.first() != b.first() {
        return None;
    }
    let ups = std::iter::repeat_n("..".to_string(), a.len() - n);
    Some(ups.chain(b[n..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned())).collect::<Vec<_>>().join("/"))
}

impl Plan {
    /// Plan the move of `from` (a file or a folder) to `to`.
    pub fn build(index: &Index, from: &Path, to: &Path) -> Plan {
        let moves: FxHashMap<PathBuf, PathBuf> = index
            .files()
            .filter(|f| f.path.starts_with(from))
            .map(|f| {
                let new = if f.path == from { to.to_path_buf() } else { to.join(f.path.strip_prefix(from).unwrap_or(Path::new(""))) };
                (f.path.to_path_buf(), new)
            })
            .collect();
        let olds: FxHashSet<PathBuf> = moves.keys().cloned().collect();
        let names: FxHashSet<String> = moves
            .keys()
            .flat_map(|p| [lc(p.file_name()), lc(p.file_stem()), FileKind::note_stem(p).map(|s| s.to_lowercase()).unwrap_or_default()])
            .collect();
        let taken = moves.values().filter(|n| index.name_taken(n, &olds)).cloned().collect();
        let mut plan = Plan { moves, taken, roots: index.roots().to_vec(), ..Default::default() };
        let mut buf = [0u8; 64];
        for n in index.notes().filter(|n| n.kind == FileKind::Markdown) {
            let moved_note = plan.moves.contains_key(&n.path);
            let (mut hit, mut any) = (false, false);
            for l in &n.links {
                // A note that moves keeps all its resolved links: relative ones change with its own place.
                if !moved_note && !names.contains(&*lower(l.target.rsplit(['/', '\\']).next().unwrap_or(""), &mut buf)) {
                    continue;
                }
                let Some(p) = index.resolve_file(&l.target, Some(&n.path)) else { continue };
                let moves_p = plan.moves.contains_key(&p);
                hit |= moves_p && !moved_note && plan.may_change(&l.target, &p);
                if moves_p || moved_note {
                    plan.resolved.insert((n.path.clone(), l.target.clone()), p);
                    any = true;
                }
            }
            plan.linking += hit as usize;
            if any {
                plan.notes.push(n.path.clone());
            }
        }
        plan.notes.sort();
        plan
    }

    /// A cheap guess whether a link to `old` written as `target` has to change (a short wiki link to a file that keeps
    /// its name does not); it only decides whether the user is asked, `rewrite` has the last word.
    fn may_change(&self, target: &str, old: &Path) -> bool {
        let new = self.new_of(old);
        new.file_name() != old.file_name()
            || self.taken.contains(&new)
            || (new.parent() != old.parent() && (target.contains(['/', '\\']) || Path::new(target).extension().is_some()))
    }

    /// Where `p` is after the move.
    pub fn new_of(&self, p: &Path) -> PathBuf {
        self.moves.get(p).cloned().unwrap_or_else(|| p.to_path_buf())
    }

    /// Path relative to its workspace folder, `/` separated.
    fn root_rel(&self, p: &Path) -> Option<String> {
        let root = self.roots.iter().filter(|r| p.starts_with(r)).max_by_key(|r| r.components().count())?;
        Some(p.strip_prefix(root).ok()?.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/"))
    }

    /// The new text of one link target: `Ok(None)` when it stays as it is, `Err` when the new name cannot be written.
    fn retarget(&self, sp: &LinkSpan, w: &str, old: &Path, note_old: &Path, note_new: &Path) -> Result<Option<String>, ()> {
        let new = self.new_of(old);
        let wlc = w.to_lowercase();
        let last = wlc.rsplit(['/', '\\']).next().unwrap_or("");
        let old_note_stem = FileKind::note_stem(old).map(|s| s.to_lowercase()).unwrap_or_default();
        // A link that reached the file through an alias or a title keeps its text.
        if last != lc(old.file_name()) && last != lc(old.file_stem()) && last != old_note_stem {
            return Ok(None);
        }
        let is_old_exc_md = FileKind::is_excalidraw_md(old);
        let old_ext = if is_old_exc_md { "excalidraw.md".to_string() } else { lc(old.extension()) };
        let had_ext = !old_ext.is_empty() && wlc.ends_with(&format!(".{old_ext}"));
        let had_exc_ext = is_old_exc_md && wlc.ends_with(".excalidraw");
        let finish = |s: String| {
            if !had_ext && !had_exc_ext && FileKind::from_path(&new).is_note() {
                if FileKind::is_excalidraw_md(&new) {
                    const SUF: &str = ".excalidraw.md";
                    if s.len() > SUF.len() && s.as_bytes()[s.len() - SUF.len()..].eq_ignore_ascii_case(SUF.as_bytes()) {
                        return s[..s.len() - SUF.len()].to_string();
                    }
                }
                if let Some(e) = new.extension() {
                    return s.strip_suffix(&format!(".{}", e.to_string_lossy())).map(str::to_string).unwrap_or(s);
                }
            } else if had_exc_ext && FileKind::is_excalidraw_md(&new) && s.len() > 3 && s[s.len() - 3..].eq_ignore_ascii_case(".md") {
                return s[..s.len() - 3].to_string();
            }
            s
        };
        let dir = |p: &Path| p.parent().map(Path::to_path_buf).unwrap_or_default();
        let r = if !sp.wiki || w.starts_with("./") || w.starts_with("../") {
            let Some(new_rel) = rel_path(&dir(note_new), &new) else { return Ok(None) };
            if rel_path(&dir(note_old), old).as_deref() == Some(new_rel.as_str()) {
                return Ok(None);
            }
            let mut r = finish(new_rel);
            if w.starts_with("./") && !r.starts_with("../") {
                r.insert_str(0, "./");
            }
            r
        } else {
            if new == old {
                return Ok(None);
            }
            let name = if w.contains(['/', '\\']) || self.taken.contains(&new) {
                match self.root_rel(&new).or_else(|| rel_path(&dir(note_new), &new)) {
                    Some(r) => r,
                    None => return Ok(None),
                }
            } else {
                new.file_name().ok_or(())?.to_string_lossy().into_owned()
            };
            finish(name)
        };
        // These would end or split the link when written inside `[[...]]` or `<...>`.
        let bad: &[char] = if sp.wiki {
            &['#', '|', ']', '[', '^']
        } else if sp.angle {
            &['<', '>']
        } else {
            &[]
        };
        if r.contains(bad) { Err(()) } else { Ok(Some(r)) }
    }
}

/// What `rewrite` did to a note.
#[derive(Debug, Default)]
pub struct Rewrite {
    /// The new text; `None` when no link changed.
    pub text: Option<String>,
    pub links: usize,
    /// Links left unchanged because the new name cannot be written in them (`# | [ ] ^` in a wiki link).
    pub skipped: usize,
}

/// The text with its links rewritten. `note_old` is the path the note had when the plan was built. Never panics: a span
/// that does not fit the text is skipped.
pub fn rewrite(text: &str, note_old: &Path, plan: &Plan) -> Rewrite {
    let scan = scan_markdown_spans(text);
    let note_new = plan.new_of(note_old);
    let mut starts = vec![0usize];
    starts.extend(text.bytes().enumerate().filter(|&(_, b)| b == b'\n').map(|(i, _)| i + 1));
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut skipped = 0;
    for (l, sp) in scan.links.iter().zip(&scan.spans) {
        let Some(old) = plan.resolved.get(&(note_old.to_path_buf(), l.target.clone())) else { continue };
        let Ok(r) = plan.retarget(sp, &l.target, old, note_old, &note_new) else {
            skipped += 1;
            continue;
        };
        let Some(mut r) = r else { continue };
        if !sp.wiki && !sp.angle {
            r = r
                .replace('%', "%25")
                .replace(' ', "%20")
                .replace('(', "%28")
                .replace(')', "%29")
                .replace('#', "%23")
                .replace('?', "%3F")
                .replace('<', "%3C")
                .replace('>', "%3E");
        }
        let Some(at) = starts.get(sp.line as usize) else { continue };
        let (s, e) = (at + sp.start, at + sp.end);
        if text.get(s..e).is_some_and(|old| old != r) {
            edits.push((s, e, r));
        }
    }
    edits.sort_by_key(|e| e.0);
    let mut out = String::with_capacity(text.len() + 32);
    let (mut at, mut links) = (0, 0);
    for (s, e, r) in &edits {
        if *s < at {
            continue;
        }
        out.push_str(&text[at..*s]);
        out.push_str(r);
        at = *e;
        links += 1;
    }
    out.push_str(&text[at..]);
    Rewrite { text: (links > 0).then_some(out), links, skipped }
}

/// Rewrite one closed note on disk (keeping its line endings and BOM). `note_old` as in `rewrite`; the file is read
/// from its place after the move. Returns (changed links, skipped links). Refuses symlinks and read-only files, and
/// gives up when the file changed between reading and writing (the user saved it meanwhile).
pub fn rewrite_file(note_old: &Path, plan: &Plan) -> io::Result<(usize, usize)> {
    let path = plan.new_of(note_old);
    let stamp = |m: &std::fs::Metadata| (m.modified().ok(), m.len());
    let meta = std::fs::symlink_metadata(&path)?;
    if meta.file_type().is_symlink() {
        return Err(io::Error::other("symbolic link"));
    }
    if meta.permissions().readonly() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "read-only file"));
    }
    let l = textio::load_text(&path)?;
    if l.lossy {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not valid UTF-8"));
    }
    let r = rewrite(&l.text, note_old, plan);
    let Some(text) = r.text else { return Ok((0, r.skipped)) };
    if std::fs::symlink_metadata(&path).map(|m| stamp(&m)).ok() != Some(stamp(&meta)) || l.mtime != meta.modified().ok() {
        return Err(io::Error::other("changed meanwhile"));
    }
    textio::save_text(&path, &text, l.line_ending, l.bom)?;
    Ok((r.links, r.skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::links::scan_markdown;

    const R: &str = "/r";

    fn ix(files: &[(&str, &str)]) -> Index {
        let mut ix = Index::new();
        ix.set_roots(vec![PathBuf::from(R)]);
        for (rel, text) in files {
            let path = Path::new(R).join(rel);
            let kind = FileKind::from_path(&path);
            let note = kind.is_note().then(|| crate::index::NoteData { mtime_ms: 1, size: 1, scan: scan_markdown(text) });
            ix.insert(path, 0, rel.to_string(), kind, note);
        }
        ix
    }

    fn p(rel: &str) -> PathBuf {
        Path::new(R).join(rel)
    }

    /// Plan the move of `from` to `to` and rewrite the note `note` (a path relative to the root, before the move).
    fn run(files: &[(&str, &str)], from: &str, to: &str, note: &str) -> Option<(String, usize)> {
        let plan = Plan::build(&ix(files), &p(from), &p(to));
        let text = files.iter().find(|f| f.0 == note).unwrap().1;
        let r = rewrite(text, &p(note), &plan);
        r.text.map(|t| (t, r.links))
    }

    fn out(files: &[(&str, &str)], from: &str, to: &str, note: &str) -> String {
        run(files, from, to, note).map(|r| r.0).unwrap_or_else(|| "UNCHANGED".into())
    }

    #[test]
    fn wiki_forms_keep_alias_heading_embed() {
        let f = [("old.md", ""), ("n.md", "a [[old]] b [[old|Alias]] c [[Old#Head]] d ![[old]] e [[old.md]] f [[old#H|x]]")];
        assert_eq!(
            out(&f, "old.md", "new name.md", "n.md"),
            "a [[new name]] b [[new name|Alias]] c [[new name#Head]] d ![[new name]] e [[new name.md]] f [[new name#H|x]]"
        );
        assert_eq!(run(&f, "old.md", "new.md", "n.md").unwrap().1, 6);
    }

    #[test]
    fn table_escaped_pipe_and_spaces() {
        let f = [("old.md", ""), ("n.md", "| [[ old \\| al ]] |")];
        assert_eq!(out(&f, "old.md", "new.md", "n.md"), "| [[ new \\| al ]] |");
    }

    #[test]
    fn path_style_stays_a_path() {
        let f = [("a/old.md", ""), ("n.md", "[[a/old]] [[a/old|x]] ![[a/old.md]]")];
        assert_eq!(out(&f, "a/old.md", "b/old.md", "n.md"), "[[b/old]] [[b/old|x]] ![[b/old.md]]");
        assert_eq!(out(&f, "a", "c", "n.md"), "[[c/old]] [[c/old|x]] ![[c/old.md]]");
    }

    #[test]
    fn short_name_stays_short_unless_ambiguous() {
        let f = [("a/old.md", ""), ("n.md", "[[old]]")];
        assert_eq!(out(&f, "a/old.md", "b/old.md", "n.md"), "UNCHANGED");
        assert_eq!(out(&f, "a/old.md", "b/new.md", "n.md"), "[[new]]");
        // Another note called "new" stays: the link must say which one.
        let g = [("a/old.md", ""), ("c/new.md", ""), ("n.md", "[[old]]")];
        assert_eq!(out(&g, "a/old.md", "b/new.md", "n.md"), "[[b/new]]");
        // Moving into a folder next to a note of the same name makes the short name ambiguous.
        let h = [("a/old.md", ""), ("c/old.md", ""), ("n.md", "[[a/old]] [[c/old]]")];
        assert_eq!(out(&h, "a/old.md", "b/old.md", "n.md"), "[[b/old]] [[c/old]]");
    }

    #[test]
    fn markdown_links_are_relative_and_encoded() {
        let f = [("d/old.md", ""), ("d/n.md", "[t](old.md) [u](./old.md#h) ![i](<old.md>) [e](https://x/old.md)")];
        assert_eq!(out(&f, "d/old.md", "d/my new.md", "d/n.md"), "[t](my%20new.md) [u](./my%20new.md#h) ![i](<my new.md>) [e](https://x/old.md)");
        assert_eq!(out(&f, "d/old.md", "e/old.md", "d/n.md"), "[t](../e/old.md) [u](../e/old.md#h) ![i](<../e/old.md>) [e](https://x/old.md)");
    }

    #[test]
    fn relative_wiki_links() {
        let f = [("a/old.md", ""), ("b/n.md", "[[../a/old]] [[../a/old.md|x]]")];
        assert_eq!(out(&f, "a/old.md", "a/new.md", "b/n.md"), "[[../a/new]] [[../a/new.md|x]]");
    }

    #[test]
    fn code_is_untouched() {
        let f = [("old.md", ""), ("n.md", "`[[old]]` [[old]]\n\n```\n[[old]]\n```\n\n    [[old]]\n\n``a `[[old]]` b``")];
        assert_eq!(out(&f, "old.md", "new.md", "n.md"), "`[[old]]` [[new]]\n\n```\n[[old]]\n```\n\n    [[old]]\n\n``a `[[old]]` b``");
    }

    #[test]
    fn alias_links_and_other_targets_are_kept() {
        let f = [("old.md", "---\naliases: [Nick]\n---\n"), ("other.md", ""), ("n.md", "[[Nick]] [[other]] [[nothing]]")];
        assert_eq!(out(&f, "old.md", "new.md", "n.md"), "UNCHANGED");
    }

    #[test]
    fn heading_line_and_crlf_offsets() {
        let f = [("old.md", ""), ("n.md", "# See [[old]]\r\ntext [[old]]\r\n")];
        assert_eq!(out(&f, "old.md", "new.md", "n.md"), "# See [[new]]\r\ntext [[new]]\r\n");
    }

    #[test]
    fn files_and_embeds() {
        let f = [("img/pic.png", ""), ("n.md", "![[pic.png]] ![p](img/pic.png)")];
        assert_eq!(out(&f, "img/pic.png", "img/photo.png", "n.md"), "![[photo.png]] ![p](img/photo.png)");
    }

    #[test]
    fn moved_note_fixes_its_own_relative_links() {
        let f = [("a/x.md", "[t](y.md) [[y]] [[../b/z]]"), ("a/y.md", ""), ("b/z.md", "")];
        // x.md moves to c/: relative ones change, the short wiki link stays.
        let plan = Plan::build(&ix(&f), &p("a/x.md"), &p("c/x.md"));
        assert_eq!(rewrite(f[0].1, &p("a/x.md"), &plan).text.unwrap(), "[t](../a/y.md) [[y]] [[../b/z]]");
        // A whole folder moves: links between its notes stay valid, links out of it change.
        let plan = Plan::build(&ix(&f), &p("a"), &p("c/a"));
        assert_eq!(rewrite(f[0].1, &p("a/x.md"), &plan).text.unwrap(), "[t](y.md) [[y]] [[../../b/z]]");
        assert_eq!(plan.linking, 0);
    }

    #[test]
    fn plan_does_not_count_links_that_stay() {
        let f = [("a/old.md", ""), ("n.md", "[[old]]"), ("m.md", "[[a/old]]")];
        let plan = Plan::build(&ix(&f), &p("a/old.md"), &p("b/old.md"));
        assert_eq!(plan.linking, 1);
    }

    #[test]
    fn plan_counts_linking_notes() {
        let f = [("old.md", ""), ("a.md", "[[old]]"), ("b.md", "[[old]] [[old]]"), ("c.md", "[[zzz]]")];
        let plan = Plan::build(&ix(&f), &p("old.md"), &p("new.md"));
        assert_eq!((plan.linking, plan.notes.len()), (2, 2));
    }

    #[test]
    fn rewrite_file_keeps_crlf_and_bom() {
        let t = tempfile::tempdir().unwrap();
        let (old, new) = (t.path().join("old.md"), t.path().join("new.md"));
        let n = t.path().join("n.md");
        std::fs::write(&n, b"\xEF\xBB\xBFa [[old]]\r\nb\r\n").unwrap();
        std::fs::write(&old, "x").unwrap();
        let mut index = Index::new();
        index.set_roots(vec![t.path().to_path_buf()]);
        for path in [&old, &n] {
            let scan = scan_markdown(&std::fs::read_to_string(path).unwrap());
            index.insert(
                path.clone(),
                0,
                path.file_name().unwrap().to_string_lossy().into_owned(),
                FileKind::Markdown,
                Some(crate::index::NoteData { mtime_ms: 1, size: 1, scan }),
            );
        }
        let plan = Plan::build(&index, &old, &new);
        std::fs::rename(&old, &new).unwrap();
        assert_eq!(rewrite_file(&n, &plan).unwrap(), (1, 0));
        assert_eq!(std::fs::read(&n).unwrap(), b"\xEF\xBB\xBFa [[new]]\r\nb\r\n");
        assert_eq!(rewrite_file(&n, &plan).unwrap(), (0, 0));
    }

    #[test]
    fn unwritable_names_are_skipped_and_counted() {
        let f = [("old.md", ""), ("n.md", "[[old]] [t](old.md) ![i](<old.md>)")];
        let plan = Plan::build(&ix(&f), &p("old.md"), &p("a#b|c.md"));
        let r = rewrite(f[1].1, &p("n.md"), &plan);
        assert_eq!((r.skipped, r.links), (1, 2));
        assert_eq!(r.text.unwrap(), "[[old]] [t](a%23b|c.md) ![i](<a#b|c.md>)");
        let plan = Plan::build(&ix(&f), &p("old.md"), &p("a>b.md"));
        let r = rewrite(f[1].1, &p("n.md"), &plan);
        assert_eq!((r.skipped, r.links), (1, 2));
        assert!(r.text.unwrap().contains("[t](a%3Eb.md)"));
    }

    #[test]
    fn rel_path_without_common_start_is_none() {
        assert_eq!(rel_path(Path::new("a/b"), Path::new("a/c")).as_deref(), Some("../c"));
        assert_eq!(rel_path(Path::new("a/b"), Path::new("c")), None);
    }

    #[test]
    fn spans_that_do_not_fit_never_panic() {
        // Plan for a note that is not the text's (line numbers beyond the text) and odd Unicode.
        let f = [("old.md", ""), ("n.md", "[[old]] äöü [[old]]")];
        let plan = Plan::build(&ix(&f), &p("old.md"), &p("new.md"));
        assert_eq!(rewrite("[[old]]", &p("n.md"), &plan).links, 1);
        assert_eq!(rewrite("", &p("n.md"), &plan).links, 0);
        assert_eq!(rewrite("[[old]] äöü [[old]]", &p("other.md"), &plan).links, 0);
    }

    #[test]
    fn rewrite_file_refuses_read_only_and_symlinks() {
        let t = tempfile::tempdir().unwrap();
        let (old, new, n) = (t.path().join("old.md"), t.path().join("new.md"), t.path().join("n.md"));
        std::fs::write(&old, "x").unwrap();
        std::fs::write(&n, "[[old]]").unwrap();
        let mut index = Index::new();
        index.set_roots(vec![t.path().to_path_buf()]);
        for path in [&old, &n] {
            let scan = scan_markdown(&std::fs::read_to_string(path).unwrap());
            index.insert(
                path.clone(),
                0,
                path.file_name().unwrap().to_string_lossy().into_owned(),
                FileKind::Markdown,
                Some(crate::index::NoteData { mtime_ms: 1, size: 1, scan }),
            );
        }
        let plan = Plan::build(&index, &old, &new);
        std::fs::rename(&old, &new).unwrap();
        let mut perm = std::fs::metadata(&n).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&n, perm.clone()).unwrap();
        assert!(rewrite_file(&n, &plan).is_err());
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        std::fs::set_permissions(&n, perm).unwrap();
        assert_eq!(std::fs::read_to_string(&n).unwrap(), "[[old]]");
        #[cfg(unix)]
        {
            let real = t.path().join("real.md");
            std::fs::rename(&n, &real).unwrap();
            std::os::unix::fs::symlink(&real, &n).unwrap();
            assert!(rewrite_file(&n, &plan).is_err());
            assert_eq!(std::fs::read_to_string(&real).unwrap(), "[[old]]");
        }
    }

    /// `cargo test -p iron-sheets-core --release plan_build_speed -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn plan_build_speed() {
        let mut files: Vec<(String, String)> = Vec::new();
        for i in 0..3000 {
            let links: String = (0..20).map(|k| format!("[[note {}]] ", (i * 7 + k * 13) % 3000)).collect();
            files.push((format!("d{}/note {i}.md", i % 30), links));
        }
        let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let index = ix(&refs);
        let t = std::time::Instant::now();
        let plan = Plan::build(&index, &p("d1/note 1.md"), &p("d1/renamed.md"));
        eprintln!("Plan::build file: {:?}, {} linking", t.elapsed(), plan.linking);
        let t = std::time::Instant::now();
        let plan = Plan::build(&index, &p("d1"), &p("e1"));
        eprintln!("Plan::build folder of 100 notes: {:?}, {} notes", t.elapsed(), plan.notes.len());
    }

    #[cfg(feature = "excalidraw")]
    #[test]
    fn relink_excalidraw_stems_and_extensions() {
        let index = ix(&[
            ("diagram.excalidraw.md", ""),
            ("raw.excalidraw", "{}"),
            ("host.md", "![[diagram]] [[diagram.excalidraw]] [[diagram.excalidraw.md]] ![[raw]] ![[raw.excalidraw]]"),
        ]);
        let plan = Plan::build(&index, &p("diagram.excalidraw.md"), &p("arch.excalidraw.md"));
        assert_eq!(plan.notes, vec![p("host.md")]);
        let r = rewrite(
            "![[diagram]] [[diagram.excalidraw]] [[diagram.excalidraw.md]] ![[raw]] ![[raw.excalidraw]]",
            &p("host.md"),
            &plan,
        );
        assert_eq!(
            r.text.as_deref(),
            Some("![[arch]] [[arch.excalidraw]] [[arch.excalidraw.md]] ![[raw]] ![[raw.excalidraw]]")
        );

        let plan_raw = Plan::build(&index, &p("raw.excalidraw"), &p("shape.excalidraw"));
        let r2 = rewrite("![[raw]] ![[raw.excalidraw]]", &p("host.md"), &plan_raw);
        assert_eq!(r2.text.as_deref(), Some("![[shape]] ![[shape.excalidraw]]"));
    }
}
