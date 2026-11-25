//! Untitled notes: real files `Untitled-<n>.md` in the app data folder, created only once they have content.
//! Pure path logic plus the move to a final place; nothing here runs at startup.
use std::io;
use std::path::{Path, PathBuf};

const PREFIX: &str = "Untitled-";
const EXT: &str = "md";

/// Path of untitled note number `n` in `dir`.
pub fn path_for(dir: &Path, n: u32) -> PathBuf {
    dir.join(format!("{PREFIX}{n}.{EXT}"))
}

/// The number of `path` when it is an untitled note of `dir` (exactly `<dir>/Untitled-<n>.md`).
pub fn number_of(dir: &Path, path: &Path) -> Option<u32> {
    if dir.as_os_str().is_empty() || path.parent()? != dir {
        return None;
    }
    let name = path.file_name()?.to_str()?;
    let n = name.strip_prefix(PREFIX)?.strip_suffix(".md")?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse().ok()
}

/// Smallest number that `taken` does not claim and whose file does not exist. Probes single paths (one `stat` each,
/// normally one in total); the folder is never listed.
pub fn next_number(dir: &Path, taken: impl Fn(u32) -> bool) -> u32 {
    let mut n = 1;
    while taken(n) || std::fs::symlink_metadata(path_for(dir, n)).is_ok() {
        n += 1;
    }
    n
}

/// Move `from` to `to`; never overwrites. Falls back to copy + remove when a rename is not possible (other file system).
pub fn move_file(from: &Path, to: &Path) -> io::Result<()> {
    if std::fs::symlink_metadata(to).is_ok() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "a file with that name already exists"));
    }
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    // Copy to the final name without ever replacing something that appeared meanwhile.
    let mut src = std::fs::File::open(from)?;
    let mut dst = std::fs::OpenOptions::new().write(true).create_new(true).open(to)?;
    let res = io::copy(&mut src, &mut dst).and_then(|_| dst.sync_all());
    drop(dst);
    match res {
        Ok(()) => {
            let _ = std::fs::remove_file(from);
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(to);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_paths() {
        let d = Path::new("/data/untitled");
        assert_eq!(path_for(d, 3), d.join("Untitled-3.md"));
        assert_eq!(number_of(d, &d.join("Untitled-12.md")), Some(12));
        assert_eq!(number_of(d, &d.join("Untitled-.md")), None);
        assert_eq!(number_of(d, &d.join("Untitled-1x.md")), None);
        assert_eq!(number_of(d, &d.join("sub").join("Untitled-1.md")), None);
        assert_eq!(number_of(d, Path::new("/elsewhere/Untitled-1.md")), None);
        assert_eq!(number_of(Path::new(""), Path::new("Untitled-1.md")), None);
    }

    #[test]
    fn next_number_skips_taken_and_existing() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(next_number(t.path(), |_| false), 1);
        std::fs::write(path_for(t.path(), 1), "x").unwrap();
        assert_eq!(next_number(t.path(), |_| false), 2);
        assert_eq!(next_number(t.path(), |n| n == 2), 3);
    }

    #[test]
    fn move_never_overwrites() {
        let t = tempfile::tempdir().unwrap();
        let (a, b) = (t.path().join("a.md"), t.path().join("b.md"));
        std::fs::write(&a, "one").unwrap();
        std::fs::write(&b, "two").unwrap();
        assert!(move_file(&a, &b).is_err());
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "two");
        let c = t.path().join("c.md");
        move_file(&a, &c).unwrap();
        assert!(!a.exists());
        assert_eq!(std::fs::read_to_string(&c).unwrap(), "one");
    }
}
