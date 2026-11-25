//! Workspaces: a named set of root folders plus the UI session.
use crate::config::ViewMode;
use crate::paths::AppDirs;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TabState {
    pub path: PathBuf,
    pub view: ViewMode,
    pub cursor: u32,
    pub scroll: f32,
}

impl Default for TabState {
    fn default() -> Self {
        TabState { path: PathBuf::new(), view: ViewMode::Split, cursor: 0, scroll: 0.0 }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub tabs: Vec<TabState>,
    pub active: Option<usize>,
    pub expanded: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Workspace {
    pub name: String,
    pub roots: Vec<PathBuf>,
    pub session: Session,
    pub tree_sort: crate::tree::TreeSort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddRoot {
    Added,
    AlreadyPresent,
    /// Inside or containing an existing root.
    Nested,
    NotADirectory,
}

/// Lexically normalise (drop `.`, resolve `..`) without touching the filesystem.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

impl Workspace {
    pub fn new(name: impl Into<String>) -> Self {
        Workspace { name: name.into(), roots: Vec::new(), session: Session::default(), tree_sort: Default::default() }
    }

    pub fn add_root(&mut self, path: &Path) -> AddRoot {
        let Ok(abs) = std::path::absolute(path) else {
            return AddRoot::NotADirectory;
        };
        let abs = normalize(&abs);
        if !abs.is_dir() {
            return AddRoot::NotADirectory;
        }
        for r in &self.roots {
            if *r == abs {
                return AddRoot::AlreadyPresent;
            }
            if abs.starts_with(r) || r.starts_with(&abs) {
                return AddRoot::Nested;
            }
        }
        self.roots.push(abs);
        AddRoot::Added
    }

    pub fn remove_root(&mut self, idx: usize) {
        if idx < self.roots.len() {
            self.roots.remove(idx);
        }
    }

    /// Put a removed root back at `idx` (clamped), with the same checks as [`Self::add_root`] (the folder may have
    /// gone, or an overlapping one been added, since it was removed).
    pub fn insert_root(&mut self, idx: usize, path: &Path) -> AddRoot {
        let r = self.add_root(path);
        if r == AddRoot::Added {
            if let Some(p) = self.roots.pop() {
                self.roots.insert(idx.min(self.roots.len()), p);
            }
        }
        r
    }

    /// Index of the (deepest) root containing `path`.
    pub fn root_of(&self, path: &Path) -> Option<usize> {
        self.roots.iter().enumerate().filter(|(_, r)| path.starts_with(r)).max_by_key(|(_, r)| r.components().count()).map(|(i, _)| i)
    }

    fn folder_name(p: &Path) -> String {
        p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
    }

    /// Folder name, with the parent folder name prepended when two roots share it.
    pub fn root_label(&self, idx: usize) -> String {
        let Some(root) = self.roots.get(idx) else {
            return String::new();
        };
        let name = Self::folder_name(root);
        let clash = self.roots.iter().enumerate().any(|(i, r)| i != idx && Self::folder_name(r) == name);
        if clash {
            if let Some(parent) = root.parent().and_then(|p| p.file_name()) {
                return format!("{}/{}", parent.to_string_lossy(), name);
            }
        }
        name
    }

    /// `Label/rel/path.md` for files inside a root, the full path otherwise.
    pub fn display_path(&self, path: &Path) -> String {
        let Some(i) = self.root_of(path) else {
            return path.display().to_string();
        };
        let mut s = self.root_label(i);
        if let Ok(rel) = path.strip_prefix(&self.roots[i]) {
            for c in rel.components() {
                s.push('/');
                s.push_str(&c.as_os_str().to_string_lossy());
            }
        }
        s
    }
}

/// Reads and writes `<config>/workspaces/<slug>.json`.
#[derive(Debug, Clone)]
pub struct WorkspaceStore {
    dir: PathBuf,
    cache: PathBuf,
}

/// Lossless, filesystem-safe encoding of a workspace name.
fn slug(name: &str) -> String {
    if name.is_empty() {
        return "_empty".into();
    }
    let mut s = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

fn unslug(s: &str) -> Option<String> {
    if s == "_empty" {
        return Some(String::new());
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

impl WorkspaceStore {
    pub fn new(dirs: &AppDirs) -> Self {
        WorkspaceStore { dir: dirs.config.join("workspaces"), cache: dirs.cache.clone() }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{}.json", slug(name)))
    }

    /// Workspace names, sorted case-insensitively.
    pub fn list(&self) -> Vec<String> {
        let mut v: Vec<String> = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let p = e.path();
                    if p.extension().and_then(|x| x.to_str()) != Some("json") {
                        return None;
                    }
                    unslug(p.file_stem()?.to_str()?)
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        v.sort_by_key(|s| s.to_lowercase());
        v
    }

    /// `None` when missing or corrupt.
    pub fn load(&self, name: &str) -> Option<Workspace> {
        let bytes = std::fs::read(self.file(name)).ok()?;
        let mut ws: Workspace = serde_json::from_slice(&bytes).ok()?;
        ws.name = name.to_string();
        Some(ws)
    }

    pub fn save(&self, ws: &Workspace) -> io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let json = serde_json::to_vec_pretty(ws).map_err(io::Error::other)?;
        crate::textio::atomic_write(&self.file(&ws.name), &json)
    }

    pub fn delete(&self, name: &str) -> io::Result<()> {
        match std::fs::remove_file(self.file(name)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => {
                let _ = std::fs::remove_file(self.index_cache_file(name));
                let _ = std::fs::remove_file(self.tree_cache_file(name));
                Ok(())
            }
        }
    }

    /// Fails with `AlreadyExists` when `new` is taken, `NotFound`/`InvalidData` when `old` is unusable.
    pub fn rename(&self, old: &str, new: &str) -> io::Result<()> {
        if old == new {
            return Ok(());
        }
        if self.file(new).exists() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "workspace exists"));
        }
        let mut ws = self.load(old).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "workspace missing or corrupt"))?;
        ws.name = new.to_string();
        self.save(&ws)?;
        let _ = std::fs::rename(self.index_cache_file(old), self.index_cache_file(new));
        let _ = std::fs::rename(self.tree_cache_file(old), self.tree_cache_file(new));
        self.delete(old)
    }

    pub fn index_cache_file(&self, name: &str) -> PathBuf {
        self.cache.join(format!("index-{}.bin", slug(name)))
    }

    pub fn tree_cache_file(&self, name: &str) -> PathBuf {
        self.cache.join(format!("tree-{}.bin", slug(name)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_root_restores_position() {
        let t = tempfile::tempdir().unwrap();
        let d = |n: &str| {
            let p = normalize(&std::path::absolute(t.path().join(n)).unwrap());
            std::fs::create_dir_all(&p).unwrap();
            p
        };
        let (a, b, c, e) = (d("a"), d("b"), d("c"), d("e"));
        let mut ws = Workspace::new("w");
        ws.roots = vec![a.clone(), c.clone()];
        assert_eq!(ws.insert_root(1, &b), AddRoot::Added);
        assert_eq!(ws.insert_root(0, &b), AddRoot::AlreadyPresent);
        assert_eq!(ws.insert_root(0, &b.join("sub")), AddRoot::NotADirectory);
        assert_eq!(ws.insert_root(99, &e), AddRoot::Added);
        assert_eq!(ws.insert_root(0, &d("a/x")), AddRoot::Nested);
        assert_eq!(ws.roots, [a, b, c, e]);
    }

    #[test]
    fn roots_nested_and_labels() {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("a/notes");
        let b = t.path().join("b/notes");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(b.join("sub")).unwrap();
        std::fs::write(t.path().join("file"), "").unwrap();
        let mut ws = Workspace::new("w");
        assert_eq!(ws.add_root(&a), AddRoot::Added);
        assert_eq!(ws.add_root(&a), AddRoot::AlreadyPresent);
        assert_eq!(ws.add_root(&a.join("..").join("notes")), AddRoot::AlreadyPresent);
        assert_eq!(ws.add_root(&t.path().join("file")), AddRoot::NotADirectory);
        assert_eq!(ws.add_root(&t.path().join("nope")), AddRoot::NotADirectory);
        assert_eq!(ws.add_root(&t.path().join("a")), AddRoot::Nested);
        std::fs::create_dir_all(a.join("inner")).unwrap();
        assert_eq!(ws.add_root(&a.join("inner")), AddRoot::Nested);
        assert_eq!(ws.add_root(&b), AddRoot::Added);
        assert_eq!(ws.add_root(&b.join("sub")), AddRoot::Nested);
        assert_eq!(ws.roots.len(), 2);
        assert_eq!(ws.root_of(&b.join("sub/x.md")), Some(1));
        assert_eq!(ws.root_of(&t.path().join("zzz")), None);
        assert_eq!(ws.root_label(0), "a/notes");
        assert_eq!(ws.root_label(1), "b/notes");
        assert_eq!(ws.display_path(&b.join("sub").join("x.md")), "b/notes/sub/x.md");
        ws.remove_root(0);
        assert_eq!(ws.root_label(0), "notes");
        ws.remove_root(9);
    }

    #[test]
    fn store_roundtrip_corrupt_rename() {
        let t = tempfile::tempdir().unwrap();
        let dirs = AppDirs::at(t.path());
        let store = WorkspaceStore::new(&dirs);
        assert!(store.list().is_empty());
        assert!(store.load("x").is_none());
        let mut ws = Workspace::new("My Notes/ü");
        ws.roots.push(t.path().to_path_buf());
        ws.session.tabs.push(TabState { path: t.path().join("a.md"), view: ViewMode::Preview, cursor: 5, scroll: 2.5 });
        ws.session.active = Some(0);
        store.save(&ws).unwrap();
        store.save(&Workspace::new("alpha")).unwrap();
        assert_eq!(store.list(), vec!["alpha".to_string(), "My Notes/ü".to_string()]);
        assert_eq!(store.load("My Notes/ü").unwrap(), ws);
        std::fs::write(store.file("alpha"), "garbage{").unwrap();
        assert!(store.load("alpha").is_none());
        store.rename("My Notes/ü", "Beta").unwrap();
        assert!(store.load("My Notes/ü").is_none());
        assert_eq!(store.load("Beta").unwrap().name, "Beta");
        assert!(store.rename("Beta", "alpha").is_err());
        store.delete("Beta").unwrap();
        store.delete("Beta").unwrap();
        assert_eq!(store.list(), vec!["alpha".to_string()]);
        assert!(store.index_cache_file("alpha").starts_with(&dirs.cache));
        assert!(store.tree_cache_file("alpha").starts_with(&dirs.cache));

        // Tree cache files follow workspace rename and delete.
        store.save(&Workspace::new("Gamma")).unwrap();
        std::fs::create_dir_all(&dirs.cache).unwrap();
        std::fs::write(store.tree_cache_file("Gamma"), b"tree-bytes").unwrap();
        store.rename("Gamma", "Delta").unwrap();
        assert!(!store.tree_cache_file("Gamma").exists());
        assert_eq!(std::fs::read(store.tree_cache_file("Delta")).unwrap(), b"tree-bytes");
        store.delete("Delta").unwrap();
        assert!(!store.tree_cache_file("Delta").exists());
    }
}
