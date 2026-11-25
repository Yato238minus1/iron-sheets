//! File operations used by the sidebar.
use std::io::{self, Write};
use std::path::{Path, PathBuf};

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.to_string())
}

fn check_name(name: &str) -> io::Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
        return Err(invalid("invalid file name"));
    }
    Ok(())
}

/// Create `<stem>[ N].<ext>` in `dir` with `contents`; never overwrites.
pub fn create_note(dir: &Path, stem: &str, ext: &str, contents: &str) -> io::Result<PathBuf> {
    check_name(stem)?;
    for _ in 0..1000 {
        let p = crate::textio::unique_path(dir, stem, ext);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(mut f) => {
                f.write_all(contents.as_bytes())?;
                return Ok(p);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "could not find a free file name"))
}

pub fn create_dir(parent: &Path, name: &str) -> io::Result<PathBuf> {
    check_name(name)?;
    let p = parent.join(name);
    std::fs::create_dir(&p)?;
    Ok(p)
}

/// Whether `a` and `b` are one directory entry seen under two spellings. On Windows names that differ only in case
/// (checked by the caller) are always one entry; elsewhere the inode decides.
fn same_entry(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (std::fs::symlink_metadata(a), std::fs::symlink_metadata(b)) {
            (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        true
    }
}

/// Rename or move; fails if `to` exists, except for a case-only rename of the same entry in the same folder.
pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
    if from == to {
        return Ok(());
    }
    if std::fs::symlink_metadata(to).is_ok() {
        let lc = |p: &Path| p.file_name().map(|n| n.to_string_lossy().to_lowercase());
        let case_only = from.parent() == to.parent() && lc(from) == lc(to) && same_entry(from, to);
        if !case_only {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "destination exists"));
        }
    }
    std::fs::rename(from, to)
}

/// Move to the system trash / recycle bin. Never deletes permanently.
pub fn trash(path: &Path) -> io::Result<()> {
    trash::delete(path).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_rename() {
        let t = tempfile::tempdir().unwrap();
        let a = create_note(t.path(), "Untitled", "md", "hi").unwrap();
        let b = create_note(t.path(), "Untitled", "md", "").unwrap();
        assert_eq!(a.file_name().unwrap(), "Untitled.md");
        assert_eq!(b.file_name().unwrap(), "Untitled 1.md");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "hi");
        assert!(create_note(t.path(), "a/b", "md", "").is_err());
        let d = create_dir(t.path(), "sub").unwrap();
        assert!(create_dir(t.path(), "sub").is_err());
        assert!(create_dir(t.path(), "..").is_err());
        assert!(rename(&a, &b).is_err());
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "hi");
        let moved = d.join("m.md");
        rename(&a, &moved).unwrap();
        assert!(moved.exists() && !a.exists());
        assert!(rename(&t.path().join("none"), &t.path().join("x")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rename_never_replaces_another_file() {
        let t = tempfile::tempdir().unwrap();
        let (a, b) = (create_dir(t.path(), "A").unwrap(), create_dir(t.path(), "a").unwrap());
        std::fs::write(a.join("x.md"), "upper").unwrap();
        std::fs::write(b.join("x.md"), "lower").unwrap();
        assert!(rename(&a.join("x.md"), &b.join("x.md")).is_err());
        // Same folder, names differing in case, two different files.
        std::fs::write(a.join("Y.md"), "1").unwrap();
        std::fs::write(a.join("y.md"), "2").unwrap();
        assert!(rename(&a.join("Y.md"), &a.join("y.md")).is_err());
        assert_eq!(std::fs::read_to_string(a.join("y.md")).unwrap(), "2");
        // Renaming to itself is a no-op.
        rename(&a.join("y.md"), &a.join("y.md")).unwrap();
        assert!(a.join("y.md").exists());
    }
}
