//! Git integration (G1 status/branch, G2 diff gutter against HEAD, G3 stage/commit, G4 push/pull) via system `git` on a background worker.
use crate::command::Command as AppCommand;
use crate::modules::{GIT, Module, ModuleCx};
use crate::platform::Waker;
use crate::state::AppState;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use iron_sheets_core::glob::ExcludeSet;
use iron_sheets_core::textio;
use iron_sheets_core::watch::FsEvent;
use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet, FxHasher};
use std::any::Any;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::hash::{Hash, Hasher};
use std::io;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, OnceLock};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

/// Notes larger than this (2 MiB) skip diff calculation against `HEAD` to keep the editor fast.
pub const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;
const DIFF_DEBOUNCE: Duration = Duration::from_millis(150);
const STATUS_DEBOUNCE: Duration = Duration::from_millis(120);
const STATUS_MAX_LATENCY: Duration = Duration::from_millis(500);
const GIT_META_POLL: Duration = Duration::from_millis(400);
const MAX_MYERS_D: usize = 2048;
const MAX_SYMLINK_HOPS: usize = 16;
/// How many folder levels below a workspace folder are searched for repositories, and how many are kept.
const NESTED_DEPTH: usize = 4;
/// Folders visited by one nested-repo walk, and cached diffs (marks plus `HEAD` text) kept.
const MAX_WALK_DIRS: usize = 20_000;
const MAX_CACHED_DIFFS: usize = 32;
/// Focus gains search for nested repos again only after this long.
const REWALK_AFTER: Duration = Duration::from_secs(30);
const MAX_NESTED: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GitFileStatus {
    Modified,
    Added,
    Untracked,
    Deleted,
    Conflict,
    Ignored,
}

impl GitFileStatus {
    fn folder_rank(self) -> u8 {
        match self {
            GitFileStatus::Conflict => 5,
            GitFileStatus::Modified => 4,
            GitFileStatus::Added => 3,
            GitFileStatus::Deleted => 2,
            GitFileStatus::Untracked => 1,
            GitFileStatus::Ignored => 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GitAvailability {
    #[default]
    NotARepo,
    Ready,
    Unavailable,
}

/// One changed path of `git status` (ignored files excluded): porcelain `XY` letters, `?`/`?` for untracked, `U`/`U` for conflicts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeEntry {
    /// Path relative to the repo root, `/` separated (collapsed untracked directories keep their trailing `/`).
    pub rel: String,
    pub x: u8,
    pub y: u8,
}

impl ChangeEntry {
    pub fn staged(&self) -> bool {
        !matches!(self.x, b'.' | b'?' | b'U')
    }

    pub fn unstaged(&self) -> bool {
        self.y != b'.'
    }

    /// Status letter of the staged (`true`) or working-tree side.
    pub fn letter(&self, staged: bool) -> char {
        let c = if staged { self.x } else { self.y };
        c as char
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSnapshot {
    pub repo_root: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
    pub branch: String,
    pub detached: bool,
    pub unborn: bool,
    pub head_oid: Option<String>,
    pub dirty_count: usize,
    pub files: FxHashMap<PathBuf, GitFileStatus>,
    pub dirs: FxHashMap<PathBuf, GitFileStatus>,
    /// Changed paths in `git status` order, for the Git panel.
    pub changes: Vec<ChangeEntry>,
    /// The branch's upstream (`origin/main`) and how far the branch is ahead of / behind it; `None` and 0 without one.
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    /// Pairs `(ws_norm, real_subdir)` for workspace roots reaching a repo subdir via symlinks/junctions.
    pub aliases: Vec<(PathBuf, PathBuf)>,
}

impl RepoSnapshot {
    pub fn branch_display(&self) -> String {
        if self.detached {
            match &self.head_oid {
                Some(oid) => format!("HEAD ({})", &oid[..oid.len().min(7)]),
                None => "HEAD (detached)".into(),
            }
        } else if self.branch.is_empty() {
            "HEAD".into()
        } else {
            self.branch.clone()
        }
    }

    /// `ahead 2, behind 1` (only the nonzero sides; git's own words, the UI font has no arrow glyphs), empty when in sync
    /// or without an upstream.
    pub fn sync_text(&self) -> String {
        let parts = [(self.ahead, "ahead"), (self.behind, "behind")];
        parts.iter().filter(|(n, _)| *n > 0).map(|(n, w)| format!("{w} {n}")).collect::<Vec<_>>().join(", ")
    }

    fn match_len(&self, path: &Path) -> Option<usize> {
        let mut best = path.starts_with(&self.repo_root).then(|| self.repo_root.as_os_str().len());
        for (alias, _) in &self.aliases {
            if path.starts_with(alias) {
                let len = alias.as_os_str().len();
                if best.is_none_or(|b| len > b) {
                    best = Some(len);
                }
            }
        }
        best
    }

    fn translate_to_repo<'a>(&self, path: &'a Path) -> Option<Cow<'a, Path>> {
        let mut best_len = if path.starts_with(&self.repo_root) { Some(self.repo_root.as_os_str().len()) } else { None };
        let mut best_alias: Option<&(PathBuf, PathBuf)> = None;
        for pair @ (alias, _) in &self.aliases {
            if path.starts_with(alias) {
                let len = alias.as_os_str().len();
                if best_len.is_none_or(|b| len > b) {
                    best_len = Some(len);
                    best_alias = Some(pair);
                }
            }
        }
        best_len?;
        let Some((alias, target)) = best_alias else {
            return Some(Cow::Borrowed(path));
        };
        let rel = path.strip_prefix(alias).ok()?;
        if rel.as_os_str().is_empty() { Some(Cow::Owned(target.clone())) } else { Some(Cow::Owned(target.join(rel))) }
    }

    fn status_of(&self, path: &Path) -> Option<GitFileStatus> {
        let translated = self.translate_to_repo(path)?;
        let path = translated.as_ref();
        if let Some(&st) = self.files.get(path) {
            return Some(st);
        }
        // Collapsed untracked/ignored directories (`? dir/`, `! secret/`) propagate to all descendants.
        let mut cur = path.parent();
        while let Some(p) = cur {
            if !p.starts_with(&self.repo_root) {
                break;
            }
            if let Some(&st) = self.files.get(p) {
                if matches!(st, GitFileStatus::Untracked | GitFileStatus::Ignored) {
                    return Some(st);
                }
            }
            if p == self.repo_root {
                break;
            }
            cur = p.parent();
        }
        self.dirs.get(path).copied()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitSnapshot {
    pub availability: GitAvailability,
    pub repos: Vec<RepoSnapshot>,
    pub root_to_repo: FxHashMap<PathBuf, usize>,
}

impl GitSnapshot {
    pub fn status_of(&self, path: &Path) -> Option<GitFileStatus> {
        let repo = self.repo_for_path(path)?;
        repo.status_of(path)
    }

    pub fn repo_for_path(&self, path: &Path) -> Option<&RepoSnapshot> {
        self.repos.iter().filter_map(|r| r.match_len(path).map(|len| (len, r))).max_by_key(|(len, _)| *len).map(|(_, r)| r)
    }

    pub fn active_repo(&self, active_path: Option<&Path>, roots: &[PathBuf]) -> Option<&RepoSnapshot> {
        if let Some(p) = active_path {
            if let Some(r) = self.repo_for_path(p) {
                return Some(r);
            }
        }
        for r in roots {
            if let Some(&idx) = self.root_to_repo.get(r) {
                if let Some(repo) = self.repos.get(idx) {
                    return Some(repo);
                }
            }
        }
        self.repos.first()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffMark {
    /// 0-based line range in the buffer (`start < end` for Added/Modified; `start == end` for Deleted before `start`).
    pub start: u32,
    pub end: u32,
    /// The `HEAD` lines this change replaces (0-based, `old_start == old_end` for Added), in the same `split('\n')` line model.
    pub old_start: u32,
    pub old_end: u32,
    pub kind: DiffKind,
}

#[derive(Clone)]
struct CachedDiff {
    repo_root: PathBuf,
    version: u64,
    head_oid: Option<String>,
    marks: Arc<[DiffMark]>,
    /// The `HEAD` text (LF) the marks were computed against, for the hunk popup; empty without marks.
    old: Arc<str>,
    /// Order of insertion, to drop the oldest entries ([`MAX_CACHED_DIFFS`]).
    seq: u64,
}

#[derive(Default)]
struct DiffState {
    seq: u64,
    cache: FxHashMap<PathBuf, CachedDiff>,
    in_flight: FxHashMap<PathBuf, (u64, Option<String>)>,
}

/// A write operation on one repo, run on the git worker (network ones on a thread of their own).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitOp {
    Stage(String),
    Unstage(String),
    StageAll,
    Commit(String),
    Push,
    Pull,
    /// `git init` in a workspace folder that is in no repository (the op's "repo" is that folder).
    Init,
    /// Working-tree changes of a file back to the index state (untracked: delete it), after a backup copy.
    Discard(String),
    /// Puts a discard's backup back (the stamp from the toast; `None` = the newest one of the repo).
    UndoDiscard(Option<String>),
    Switch(String),
    NewBranch(String),
    DeleteBranch(String),
    RemoteAdd { name: String, url: String },
    RemoteRemove(String),
    RemoteRename { from: String, to: String },
    /// One remote, or all of them (`None`).
    Fetch(Option<String>),
    /// Stage (or unstage) one change of the buffer `text` in the index, without touching the working tree.
    Hunk { rel: String, mark: DiffMark, text: String, stage: bool },
}

impl GitOp {
    pub fn label(&self) -> &'static str {
        match self {
            GitOp::Stage(_) => "Stage",
            GitOp::Unstage(_) => "Unstage",
            GitOp::StageAll => "Stage all",
            GitOp::Commit(_) => "Commit",
            GitOp::Push => "Push",
            GitOp::Pull => "Pull",
            GitOp::Init => "Init",
            GitOp::Discard(_) => "Discard",
            GitOp::UndoDiscard(_) => "Undo discard",
            GitOp::Switch(_) => "Switch branch",
            GitOp::NewBranch(_) => "New branch",
            GitOp::DeleteBranch(_) => "Delete branch",
            GitOp::RemoteAdd { .. } => "Add remote",
            GitOp::RemoteRemove(_) => "Remove remote",
            GitOp::RemoteRename { .. } => "Rename remote",
            GitOp::Fetch(_) => "Fetch",
            GitOp::Hunk { stage: true, .. } => "Stage hunk",
            GitOp::Hunk { stage: false, .. } => "Unstage hunk",
        }
    }
}

/// The long operations: one at a time, the panel shows them as busy.
fn is_long(label: &str) -> bool {
    matches!(label, "Commit" | "Push" | "Pull" | "Fetch" | "Init")
}

/// Operations whose success needs no toast (the panel shows the result).
fn is_quiet(label: &str) -> bool {
    matches!(label, "Stage" | "Unstage" | "Stage all")
}

/// A local branch for the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    pub current: bool,
    /// Subject of the branch's last commit.
    pub subject: String,
}

/// A remote with its fetch URL (user info removed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteInfo {
    pub name: String,
    pub url: String,
}

/// Which dialog of the frontend a `git.branches` / `git.remotes` command opens, for the repo at the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitDialog {
    Branches(PathBuf),
    Remotes(PathBuf),
}

/// Which change of the note the hunk popup should show (`git.hunk`, `git.hunk_next`, `git.hunk_prev`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HunkNav {
    /// The one at the caret, else the next one after it.
    At,
    Next,
    Prev,
}

/// What a finished [`GitOp`] reports: `Ok` text for success, git's first error line (or our own message) for failure.
#[derive(Debug, Clone)]
pub struct OpOutcome {
    pub serial: u64,
    pub repo_root: PathBuf,
    pub op: &'static str,
    pub result: Result<String, String>,
}

enum WorkerCmd {
    SetRoots { roots: Vec<PathBuf>, scan_gen: u64 },
    /// `paths`: the files that changed (only the repos holding them are scanned again); `None` = scan every repo.
    Refresh { force_rediscover: bool, scan_gen: u64, paths: Option<Vec<PathBuf>> },
    SetGitBin { bin: Option<String>, scan_gen: u64 },
    /// The folders kept out of the workspace; the nested repos are searched again.
    SetExcludes { excludes: ExcludeSet, scan_gen: u64 },
    /// Search the nested repos again with the next scan (a repo may have been cloned or deleted meanwhile).
    Rewalk,
    ComputeDiff { repo_root: PathBuf, path: PathBuf, rel_posix: String, version: u64, head_oid: Option<String>, is_added: bool, text: String },
    Op { repo_root: PathBuf, op: GitOp },
    Remote { repo_root: PathBuf },
    Branches { repo_root: PathBuf },
    RemoteList { repo_root: PathBuf },
}

enum WorkerMsg {
    Snapshot { snap: GitSnapshot, scan_gen: u64 },
    ScanAck(u64),
    DiffDone { path: PathBuf, version: u64, head_oid: Option<String>, marks: Arc<[DiffMark]>, old: Arc<str> },
    OpDone { repo_root: PathBuf, op: &'static str, result: Result<String, String> },
    Remote { repo_root: PathBuf, url: Option<String> },
    Branches { repo_root: PathBuf, list: Result<Vec<Branch>, String> },
    RemoteList { repo_root: PathBuf, list: Result<Vec<RemoteInfo>, String> },
}

struct DecOnDrop(Arc<AtomicUsize>);
impl Drop for DecOnDrop {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub struct GitModule {
    snapshot: Arc<GitSnapshot>,
    diff_state: Mutex<DiffState>,
    tx: Option<Sender<WorkerCmd>>,
    rx: Option<Receiver<WorkerMsg>>,
    roots: Vec<PathBuf>,
    excludes: ExcludeSet,
    /// When the nested repos were last searched (asked for), so focus gains do not search again within 30 s.
    last_walk: Instant,
    scanning_flag: Arc<AtomicBool>,
    pending_diffs: Arc<AtomicUsize>,
    git_bin_override: Option<String>,
    scan_req_gen: u64,
    scan_done_gen: u64,
    /// Operations sent to the worker and not finished (also counted by `in_flight`).
    pending_ops: Arc<AtomicUsize>,
    /// The commit, push or pull in progress, per repo (nothing else runs on that repo meanwhile).
    busy: FxHashMap<PathBuf, &'static str>,
    /// Display URL of each repo's remote, and the branch it was asked for.
    remotes: FxHashMap<PathBuf, String>,
    remote_asked: FxHashMap<PathBuf, String>,
    /// Branch and remote lists per repo, as last listed (asked for when a dialog opens, and after each op that changes them).
    branches: FxHashMap<PathBuf, Result<Arc<[Branch]>, String>>,
    remote_list: FxHashMap<PathBuf, Result<Arc<[RemoteInfo]>, String>>,
    /// A dialog the frontend should open (taken with [`GitModule::take_dialog`]).
    dialog: Option<GitDialog>,
    /// A hunk popup the frontend should open for a note (taken with [`GitModule::take_hunk_request`]).
    hunk_request: Option<(PathBuf, HunkNav)>,
    last: Option<OpOutcome>,
    /// Outcomes not yet shown in the status bar (see [`report`]).
    unreported: Vec<OpOutcome>,
    serial: u64,
}

impl Default for GitModule {
    fn default() -> Self {
        Self::new()
    }
}

impl GitModule {
    pub fn new() -> Self {
        GitModule {
            snapshot: Arc::new(GitSnapshot::default()),
            diff_state: Mutex::new(DiffState::default()),
            tx: None,
            rx: None,
            roots: Vec::new(),
            excludes: ExcludeSet::default(),
            last_walk: Instant::now(),
            scanning_flag: Arc::new(AtomicBool::new(false)),
            pending_diffs: Arc::new(AtomicUsize::new(0)),
            git_bin_override: None,
            scan_req_gen: 0,
            scan_done_gen: 0,
            pending_ops: Arc::new(AtomicUsize::new(0)),
            busy: FxHashMap::default(),
            remotes: FxHashMap::default(),
            remote_asked: FxHashMap::default(),
            branches: FxHashMap::default(),
            remote_list: FxHashMap::default(),
            dialog: None,
            hunk_request: None,
            last: None,
            unreported: Vec::new(),
            serial: 0,
        }
    }

    /// True when Git is on and a workspace is open: the Git panel tab is shown, with or without a repository (no repo =
    /// "Initialize repository" rows, no git program = its own state).
    pub fn panel_shown<U>(state: &AppState<U>) -> bool {
        Self::of(state).is_some() && !state.workspace.roots.is_empty()
    }

    /// The commit, push or pull in progress in the repo at `root` ("Commit", "Push", "Pull").
    pub fn busy(&self, root: &Path) -> Option<&'static str> {
        self.busy.get(root).copied()
    }

    /// Where Push and Pull of the repo at `root` go (user info removed); asked for with the `git.remote` command.
    pub fn remote_of(&self, root: &Path) -> Option<&str> {
        self.remotes.get(root).map(String::as_str)
    }

    /// The local branches of the repo at `root` as last listed (`None` while the first listing runs).
    pub fn branches(&self, root: &Path) -> Option<Result<Arc<[Branch]>, String>> {
        self.branches.get(root).cloned()
    }

    pub fn remote_list(&self, root: &Path) -> Option<Result<Arc<[RemoteInfo]>, String>> {
        self.remote_list.get(root).cloned()
    }

    /// The dialog a command asked for, once.
    pub fn take_dialog(&mut self) -> Option<GitDialog> {
        self.dialog.take()
    }

    /// The hunk popup a command asked for, once.
    pub fn take_hunk_request(&mut self) -> Option<(PathBuf, HunkNav)> {
        self.hunk_request.take()
    }

    /// True until the first scan (and every later one) has reported back.
    pub fn scanning(&self) -> bool {
        self.scan_pending()
    }

    /// The last finished operation, for the panel's failed/ready line.
    pub fn last_outcome(&self) -> Option<&OpOutcome> {
        self.last.as_ref()
    }

    fn record(&mut self, repo_root: PathBuf, op: &'static str, result: Result<String, String>) {
        self.serial += 1;
        let o = OpOutcome { serial: self.serial, repo_root, op, result };
        self.unreported.push(o.clone());
        self.last = Some(o);
    }

    /// `arg` is a path inside the repo, or `<path>\0<payload>`. The repo is the one containing that path (panel commands pass
    /// the repo root or a file); an empty path (palette without an open note) or a miss is refused, never guessed.
    /// `init` is the exception: its path is a workspace folder in no repo.
    fn start_op(&mut self, kind: &str, arg: &str) {
        let (path, payload) = arg.split_once('\0').unwrap_or((arg, ""));
        let path = Path::new(path);
        let snap = Arc::clone(&self.snapshot);
        let label = match kind {
            "stage" => "Stage",
            "unstage" => "Unstage",
            "stage_all" => "Stage all",
            "commit" => "Commit",
            "push" => "Push",
            "pull" => "Pull",
            "remote" => "Remote",
            "init" => "Init",
            "discard" => "Discard",
            "undo_discard" => "Undo discard",
            "switch" => "Switch branch",
            "branch_new" => "New branch",
            "branch_delete" => "Delete branch",
            "remote_add" => "Add remote",
            "remote_remove" => "Remove remote",
            "remote_rename" => "Rename remote",
            "fetch" => "Fetch",
            "branches" => "Branches",
            "remotes" => "Remotes",
            _ => return,
        };
        if kind == "init" {
            // The folder is a workspace root (the one holding `path`), and in no repository.
            let root = self.roots.iter().filter(|r| path.starts_with(r)).max_by_key(|r| r.as_os_str().len()).cloned();
            match root {
                None => self.record(PathBuf::new(), label, Err("Open a note in the folder first".into())),
                Some(r) if snap.repo_for_path(&r).is_some() => self.record(r, label, Err("This folder is already in a Git repository".into())),
                Some(r) => self.send_op(r, label, GitOp::Init),
            }
            return;
        }
        let repo = if path.as_os_str().is_empty() {
            Err("Open a note inside a repository first")
        } else {
            snap.repo_for_path(path).ok_or("This folder is not in a Git repository")
        };
        let repo = match repo {
            Ok(r) => r,
            Err(e) => {
                if kind != "remote" {
                    self.record(PathBuf::new(), label, Err(e.into()));
                }
                return;
            }
        };
        let root = repo.repo_root.clone();
        if kind == "remote" {
            if let Some(tx) = &self.tx {
                if self.remote_asked.get(&root) != Some(&repo.branch) {
                    self.remote_asked.insert(root.clone(), repo.branch.clone());
                    let _ = tx.send(WorkerCmd::Remote { repo_root: root });
                }
            }
            return;
        }
        if matches!(kind, "branches" | "remotes") {
            let Some(tx) = &self.tx else { return self.record(root, label, Err("Git is not running".into())) };
            let _ = tx.send(if kind == "branches" { WorkerCmd::Branches { repo_root: root.clone() } } else { WorkerCmd::RemoteList { repo_root: root.clone() } });
            self.dialog = Some(if kind == "branches" { GitDialog::Branches(root) } else { GitDialog::Remotes(root) });
            return;
        }
        let rel = || repo.translate_to_repo(path).and_then(|p| path_to_posix_rel(&repo.repo_root, &p));
        let (a, b) = payload.split_once('\0').unwrap_or((payload, ""));
        let op = match kind {
            "stage" => rel().map(GitOp::Stage),
            "unstage" => rel().map(GitOp::Unstage),
            "stage_all" => Some(GitOp::StageAll),
            "commit" => Some(GitOp::Commit(payload.to_string())),
            "push" => Some(GitOp::Push),
            // Only what the snapshot lists as changed in the working tree.
            "discard" => rel().filter(|r| repo.changes.iter().any(|c| &c.rel == r && c.unstaged())).map(GitOp::Discard),
            "undo_discard" => Some(GitOp::UndoDiscard((!payload.is_empty()).then(|| payload.to_string()))),
            "switch" => Some(GitOp::Switch(payload.to_string())),
            "branch_new" => Some(GitOp::NewBranch(payload.to_string())),
            "branch_delete" => Some(GitOp::DeleteBranch(payload.to_string())),
            "remote_add" => Some(GitOp::RemoteAdd { name: a.to_string(), url: b.to_string() }),
            "remote_remove" => Some(GitOp::RemoteRemove(payload.to_string())),
            "remote_rename" => Some(GitOp::RemoteRename { from: a.to_string(), to: b.to_string() }),
            "fetch" => Some(GitOp::Fetch((!payload.is_empty()).then(|| payload.to_string()))),
            _ => Some(GitOp::Pull),
        };
        match op {
            Some(op) => self.send_op(root, label, op),
            None => self.record(root, label, Err(if kind == "discard" { "That file has no changes to discard" } else { "That file is not inside the repository" }.into())),
        }
    }

    /// Stage or unstage one change of the buffer `text` of the note at `path` (a mark from [`GitModule::diff_marks`]); the worker
    /// checks the mark against `text` and refuses a stale one.
    pub fn hunk_op(&mut self, path: &Path, mark: DiffMark, text: String, stage: bool) {
        let label = if stage { "Stage hunk" } else { "Unstage hunk" };
        let snap = Arc::clone(&self.snapshot);
        let Some(repo) = snap.repo_for_path(path) else { return self.record(PathBuf::new(), label, Err("This note is not in a Git repository".into())) };
        match repo.translate_to_repo(path).and_then(|p| path_to_posix_rel(&repo.repo_root, &p)) {
            Some(rel) => self.send_op(repo.repo_root.clone(), label, GitOp::Hunk { rel, mark, text, stage }),
            None => self.record(repo.repo_root.clone(), label, Err("That file is not inside the repository".into())),
        }
    }

    fn send_op(&mut self, root: PathBuf, label: &'static str, op: GitOp) {
        let err = match &op {
            // Index and ref locks: one operation per repo at a time while a long one runs.
            _ if self.busy.contains_key(&root) => Some(format!("Wait for the running {} to finish", self.busy[&root])),
            GitOp::Commit(m) if m.trim().is_empty() => Some("Enter a commit message first".into()),
            _ if self.tx.is_none() => Some("Git is not running".into()),
            _ => None,
        };
        if let (None, Some(tx)) = (&err, &self.tx) {
            if is_long(label) {
                self.busy.insert(root.clone(), label);
            }
            self.pending_ops.fetch_add(1, Ordering::AcqRel);
            if tx.send(WorkerCmd::Op { repo_root: root.clone(), op }).is_err() {
                self.pending_ops.fetch_sub(1, Ordering::AcqRel);
                self.busy.remove(&root);
                self.record(root, label, Err("Git is not running".into()));
            }
        } else if let Some(e) = err {
            self.record(root, label, Err(e));
        }
    }

    fn bump_gen(&mut self) -> u64 {
        self.scan_req_gen = self.scan_req_gen.wrapping_add(1);
        self.scan_req_gen
    }

    fn scan_pending(&self) -> bool {
        self.scan_done_gen < self.scan_req_gen || self.scanning_flag.load(Ordering::Acquire)
    }

    pub fn of<U>(state: &AppState<U>) -> Option<&GitModule> {
        state.modules.get::<GitModule>(GIT)
    }

    pub fn of_mut<U>(state: &mut AppState<U>) -> Option<&mut GitModule> {
        state.modules.get_mut::<GitModule>(GIT)
    }

    pub fn snapshot(&self) -> &Arc<GitSnapshot> {
        &self.snapshot
    }

    /// The workspace's exclude patterns (as in Settings): excluded folders are not searched for nested repositories.
    pub fn set_excludes(&mut self, globs: &[String]) {
        let excludes = ExcludeSet::new(globs);
        if excludes == self.excludes {
            return;
        }
        self.excludes = excludes.clone();
        if self.tx.is_some() {
            let scan_gen = self.bump_gen();
            if let Some(tx) = &self.tx {
                let _ = tx.send(WorkerCmd::SetExcludes { excludes, scan_gen });
            }
        }
    }

    /// Test hook: override the git executable name/path without mutating process-wide `PATH`.
    pub fn set_git_bin_override(&mut self, bin: Option<String>) {
        self.git_bin_override = bin.clone();
        if self.tx.is_some() {
            let scan_gen = self.bump_gen();
            if let Some(tx) = &self.tx {
                let _ = tx.send(WorkerCmd::SetGitBin { bin, scan_gen });
            }
        }
    }

    /// Returns the delay after which `diff_marks` should be re-queried after an in-frame edit.
    pub fn diff_repaint_delay(&self, path: &Path, text_len: usize, is_dirty: bool, last_edit: Instant) -> Option<Duration> {
        if text_len > MAX_DIFF_BYTES || self.snapshot.availability != GitAvailability::Ready {
            return None;
        }
        let repo = self.snapshot.repo_for_path(path)?;
        let status = repo.status_of(path);
        if matches!(status, Some(GitFileStatus::Untracked | GitFileStatus::Ignored)) {
            return None;
        }
        if is_dirty {
            return Some(DIFF_DEBOUNCE.saturating_sub(last_edit.elapsed()));
        }
        if status.is_some() || self.diff_state.lock().cache.contains_key(path) {
            return Some(Duration::ZERO);
        }
        None
    }

    /// The marks cached for `path`, the `HEAD` text they were computed against and the document version they are for (for the
    /// hunk popup, which acts only on marks of the current version); queues no work.
    pub fn hunk_data(&self, path: &Path) -> Option<(Arc<[DiffMark]>, Arc<str>, u64)> {
        self.diff_state.lock().cache.get(path).filter(|c| !c.marks.is_empty()).map(|c| (Arc::clone(&c.marks), Arc::clone(&c.old), c.version))
    }

    /// Returns `(marks, repaint_after)` for the editor gutter. Debounced while `is_dirty`; cached per `(path, version, head_oid)`.
    pub fn diff_marks(
        &self,
        path: &Path,
        version: u64,
        text_len: usize,
        is_dirty: bool,
        last_edit: Instant,
        text: impl FnOnce() -> String,
    ) -> (Arc<[DiffMark]>, Option<Duration>) {
        let empty = || Arc::from([]) as Arc<[DiffMark]>;
        if text_len > MAX_DIFF_BYTES || self.snapshot.availability != GitAvailability::Ready {
            let mut ds = self.diff_state.lock();
            ds.cache.remove(path);
            ds.in_flight.remove(path);
            return (empty(), None);
        }
        let Some(repo) = self.snapshot.repo_for_path(path) else {
            return (empty(), None);
        };
        let status = repo.status_of(path);
        if matches!(status, Some(GitFileStatus::Untracked | GitFileStatus::Ignored)) {
            let mut ds = self.diff_state.lock();
            ds.cache.remove(path);
            ds.in_flight.remove(path);
            return (empty(), None);
        }
        if !is_dirty && status.is_none() {
            if !self.scan_pending() {
                let mut ds = self.diff_state.lock();
                ds.cache.remove(path);
                ds.in_flight.remove(path);
                return (empty(), None);
            }
            let ds = self.diff_state.lock();
            let marks =
                ds.cache.get(path).filter(|c| c.repo_root == repo.repo_root && c.head_oid == repo.head_oid).map(|c| Arc::clone(&c.marks)).unwrap_or_else(empty);
            return (marks, None);
        }
        let head_oid = repo.head_oid.clone();
        let mut ds = self.diff_state.lock();
        if let Some(c) = ds.cache.get(path) {
            if c.version == version && c.head_oid == head_oid && c.repo_root == repo.repo_root {
                return (Arc::clone(&c.marks), None);
            }
        }
        let prev_marks = ds.cache.get(path).map(|c| Arc::clone(&c.marks)).unwrap_or_else(empty);
        if is_dirty {
            let elapsed = last_edit.elapsed();
            if elapsed < DIFF_DEBOUNCE {
                return (prev_marks, Some(DIFF_DEBOUNCE - elapsed));
            }
        }
        if ds.in_flight.get(path).is_some_and(|(v, h)| *v == version && *h == head_oid) {
            return (prev_marks, None);
        }
        let Some(rel_posix) = repo.translate_to_repo(path).and_then(|p| path_to_posix_rel(&repo.repo_root, &p)) else {
            return (empty(), None);
        };
        let Some(tx) = &self.tx else {
            return (prev_marks, None);
        };
        let is_added = status == Some(GitFileStatus::Added) || (repo.unborn && status.is_some());
        let text_owned = text();
        ds.in_flight.insert(path.to_path_buf(), (version, head_oid.clone()));
        self.pending_diffs.fetch_add(1, Ordering::AcqRel);
        if tx
            .send(WorkerCmd::ComputeDiff {
                repo_root: repo.repo_root.clone(),
                path: path.to_path_buf(),
                rel_posix,
                version,
                head_oid,
                is_added,
                text: text_owned,
            })
            .is_err()
        {
            self.pending_diffs.fetch_sub(1, Ordering::AcqRel);
            ds.in_flight.remove(path);
        }
        (prev_marks, None)
    }
}

impl Module for GitModule {
    fn id(&self) -> &'static str {
        GIT
    }

    fn start(&mut self, cx: &mut ModuleCx) {
        // The worker is never joined (that would block the UI): it exits once `tx` drops and its current git call ends.
        if self.tx.is_some() {
            return;
        }
        let (cmd_tx, cmd_rx) = unbounded();
        let (msg_tx, msg_rx) = unbounded();
        let waker = cx.waker.clone();
        let scanning_flag = self.scanning_flag.clone();
        let pending_diffs = self.pending_diffs.clone();
        let pending_ops = self.pending_ops.clone();
        let initial_roots = self.roots.clone();
        let initial_excludes = self.excludes.clone();
        let initial_bin = self.git_bin_override.clone();
        let initial_gen = if !initial_roots.is_empty() { self.bump_gen() } else { 0 };
        match std::thread::Builder::new()
            .name("iron-sheets-git".into())
            .spawn(move || worker_loop(cmd_rx, msg_tx, waker, scanning_flag, pending_diffs, pending_ops, initial_roots, initial_excludes, initial_bin, initial_gen))
        {
            Ok(_) => {
                self.tx = Some(cmd_tx);
                self.rx = Some(msg_rx);
            }
            Err(_) => {
                self.scan_done_gen = self.scan_req_gen;
                self.snapshot = Arc::new(GitSnapshot { availability: GitAvailability::Unavailable, ..Default::default() });
            }
        }
    }

    /// `git.hunk|hunk_next|hunk_prev` (arg: the note's path) only record a request for the frontend's popup.
    /// `git.stage|unstage|stage_all|commit|push|pull|init|discard|undo_discard|switch|branch_new|branch_delete|remote_add|
    /// remote_remove|remote_rename|fetch|branches|remotes`: `arg` is a path in the repo, plus `\0` and the payload (commit
    /// message, branch or remote name, `name\0url`). shortcut: paths travel as lossy UTF-8, fine for notes.
    fn on_command(&mut self, id: &str, arg: &str, _cx: &mut ModuleCx) {
        match id.strip_prefix("git.") {
            Some(k @ ("hunk" | "hunk_next" | "hunk_prev")) if !arg.is_empty() => {
                let nav = match k {
                    "hunk" => HunkNav::At,
                    "hunk_next" => HunkNav::Next,
                    _ => HunkNav::Prev,
                };
                self.hunk_request = Some((PathBuf::from(arg), nav));
            }
            Some(kind) => self.start_op(kind, arg),
            None => {}
        }
    }

    fn on_save(&mut self, path: &Path) {
        if !self.roots.is_empty() && self.tx.is_some() {
            let scan_gen = self.bump_gen();
            if let Some(tx) = &self.tx {
                let _ = tx.send(WorkerCmd::Refresh { force_rediscover: false, scan_gen, paths: Some(vec![path.to_path_buf()]) });
            }
        }
    }

    fn on_workspace_roots(&mut self, roots: &[PathBuf]) {
        self.roots = roots.to_vec();
        if self.tx.is_some() {
            let scan_gen = self.bump_gen();
            if let Some(tx) = &self.tx {
                let _ = tx.send(WorkerCmd::SetRoots { roots: self.roots.clone(), scan_gen });
            }
        }
    }

    fn on_focus_gained(&mut self) {
        if !self.roots.is_empty() && self.tx.is_some() {
            let scan_gen = self.bump_gen();
            if let Some(tx) = &self.tx {
                // A clone or delete in a terminal changes the set of nested repos (the watcher skips `.git`).
                if self.last_walk.elapsed() > REWALK_AFTER {
                    self.last_walk = Instant::now();
                    let _ = tx.send(WorkerCmd::Rewalk);
                }
                let _ = tx.send(WorkerCmd::Refresh { force_rediscover: false, scan_gen, paths: None });
            }
        }
    }

    fn on_fs_event(&mut self, evs: &[FsEvent]) {
        if evs.is_empty() {
            return;
        }
        let force_rediscover = evs.iter().any(|e| matches!(e, FsEvent::Rescan));
        if !self.roots.is_empty() && self.tx.is_some() {
            let scan_gen = self.bump_gen();
            if let Some(tx) = &self.tx {
                let paths = evs.iter().map(|e| match e {
                    FsEvent::Changed(p) | FsEvent::Removed(p) => Some(p.clone()),
                    FsEvent::Rescan => None,
                });
                let _ = tx.send(WorkerCmd::Refresh { force_rediscover, scan_gen, paths: paths.collect() });
            }
        }
    }

    fn poll(&mut self) -> bool {
        let Some(rx) = self.rx.clone() else { return false };
        let mut changed = false;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                WorkerMsg::Snapshot { snap, scan_gen } => {
                    self.scan_done_gen = self.scan_done_gen.max(scan_gen);
                    if *self.snapshot != snap {
                        // Invalidate diff cache entries for repos whose HEAD OID changed or that disappeared.
                        let mut ds = self.diff_state.lock();
                        ds.cache.retain(|_, c| {
                            snap.availability == GitAvailability::Ready && snap.repos.iter().any(|r| r.repo_root == c.repo_root && r.head_oid == c.head_oid)
                        });
                        self.snapshot = Arc::new(snap);
                        changed = true;
                    }
                }
                WorkerMsg::ScanAck(scan_gen) => {
                    self.scan_done_gen = self.scan_done_gen.max(scan_gen);
                    if self.diff_state.lock().cache.keys().any(|p| self.snapshot.status_of(p).is_none()) {
                        changed = true;
                    }
                }
                WorkerMsg::Remote { repo_root, url } => {
                    match url {
                        Some(u) => self.remotes.insert(repo_root, u),
                        None => self.remotes.remove(&repo_root),
                    };
                    changed = true;
                }
                WorkerMsg::Branches { repo_root, list } => {
                    self.branches.insert(repo_root, list.map(Arc::from));
                    changed = true;
                }
                WorkerMsg::RemoteList { repo_root, list } => {
                    self.remote_list.insert(repo_root, list.map(Arc::from));
                    changed = true;
                }
                WorkerMsg::OpDone { repo_root, op, result } => {
                    if is_long(op) {
                        self.busy.remove(&repo_root);
                    }
                    // Lists a dialog shows follow what the op changed; a fetch moves the upstream, so ahead/behind needs a scan.
                    if let Some(tx) = &self.tx {
                        if self.branches.contains_key(&repo_root) {
                            let _ = tx.send(WorkerCmd::Branches { repo_root: repo_root.clone() });
                        }
                        if self.remote_list.contains_key(&repo_root) {
                            let _ = tx.send(WorkerCmd::RemoteList { repo_root: repo_root.clone() });
                        }
                    }
                    if op == "Fetch" {
                        let scan_gen = self.bump_gen();
                        if let Some(tx) = &self.tx {
                            let _ = tx.send(WorkerCmd::Refresh { force_rediscover: false, scan_gen, paths: None });
                        }
                    }
                    self.record(repo_root, op, result);
                    changed = true;
                }
                WorkerMsg::DiffDone { path, version, head_oid, marks, old } => {
                    let repo_root = self.snapshot.repo_for_path(&path).map(|r| r.repo_root.clone()).unwrap_or_default();
                    let mut ds = self.diff_state.lock();
                    if ds.in_flight.get(&path).is_some_and(|(v, h)| *v == version && *h == head_oid) {
                        ds.in_flight.remove(&path);
                    }
                    let prev_same = ds.cache.get(&path).is_some_and(|c| c.marks == marks);
                    ds.seq += 1;
                    let seq = ds.seq;
                    ds.cache.insert(path, CachedDiff { repo_root, version, head_oid, marks, old, seq });
                    while ds.cache.len() > MAX_CACHED_DIFFS {
                        let Some(oldest) = ds.cache.iter().min_by_key(|(_, c)| c.seq).map(|(p, _)| p.clone()) else { break };
                        ds.cache.remove(&oldest);
                    }
                    if !prev_same {
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    fn in_flight(&self) -> usize {
        usize::from(self.scanning_flag.load(Ordering::Acquire)) + self.pending_diffs.load(Ordering::Acquire) + self.pending_ops.load(Ordering::Acquire)
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

#[derive(Clone, PartialEq, Eq)]
struct MetaSig {
    path: PathBuf,
    sig: Option<(Option<SystemTime>, u64)>,
}

fn stat_sig(path: &Path) -> Option<(Option<SystemTime>, u64)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok(), m.len()))
}

fn collect_repo_meta_sigs(repo: &RepoSnapshot) -> Vec<MetaSig> {
    let mut paths = vec![repo.git_dir.join("HEAD"), repo.git_dir.join("index"), repo.common_dir.join("packed-refs"), repo.repo_root.join(".gitignore")];
    if let Ok(head) = std::fs::read_to_string(repo.git_dir.join("HEAD")) {
        if let Some(ref_rel) = head.trim().strip_prefix("ref: ") {
            let ref_path = posix_rel_to_path(&repo.common_dir, ref_rel.trim());
            paths.push(ref_path);
        }
    }
    paths.into_iter().map(|path| MetaSig { sig: stat_sig(&path), path }).collect()
}

fn collect_all_meta_sigs(repos: &[RepoSnapshot]) -> Vec<Vec<MetaSig>> {
    repos.iter().map(collect_repo_meta_sigs).collect()
}

fn meta_changed(repos: &[RepoSnapshot], prev: &mut Vec<Vec<MetaSig>>) -> bool {
    if repos.len() != prev.len() {
        *prev = collect_all_meta_sigs(repos);
        return true;
    }
    for (repo, old_list) in repos.iter().zip(prev.iter()) {
        for old in old_list {
            if stat_sig(&old.path) != old.sig {
                *prev = collect_all_meta_sigs(repos);
                return true;
            }
        }
        // Also detect if HEAD symbolic ref target switched.
        let cur = collect_repo_meta_sigs(repo);
        if &cur != old_list {
            *prev = collect_all_meta_sigs(repos);
            return true;
        }
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn worker_loop(
    cmd_rx: Receiver<WorkerCmd>,
    msg_tx: Sender<WorkerMsg>,
    waker: Arc<dyn Waker>,
    scanning_flag: Arc<AtomicBool>,
    pending_diffs: Arc<AtomicUsize>,
    pending_ops: Arc<AtomicUsize>,
    initial_roots: Vec<PathBuf>,
    initial_excludes: ExcludeSet,
    initial_bin: Option<String>,
    initial_gen: u64,
) {
    let mut roots = initial_roots;
    let mut excludes = initial_excludes;
    // Repos below the workspace folders; `None` = search again with the next scan (folder set, excludes, focus, rescan).
    let mut nested: Option<Vec<PathBuf>> = None;
    let mut git_bin = initial_bin.unwrap_or_else(|| "git".into());
    let mut last_snap = GitSnapshot::default();
    let mut meta_sigs: Vec<Vec<MetaSig>> = Vec::new();
    let mut unavailable_latched = false;
    let mut dirty_first: Option<Instant> = None;
    let mut dirty_last: Option<Instant> = None;
    let mut pending_immediate = !roots.is_empty();
    let mut latest_gen = initial_gen;
    let mut sent_gen = 0u64;
    // What the next debounced scan has to cover: everything, or the repos holding the files that changed.
    let mut full = true;
    let mut touched: Vec<PathBuf> = Vec::new();
    let mut from_debounce = false;

    loop {
        if pending_immediate {
            pending_immediate = false;
            let touched_repos: Option<FxHashSet<PathBuf>> = (from_debounce && !full && !touched.is_empty())
                .then(|| touched.iter().map(|p| last_snap.repo_for_path(p).map(|r| r.repo_root.clone())).collect::<Option<_>>())
                .flatten();
            (from_debounce, full) = (false, false);
            touched.clear();
            dirty_first = None;
            dirty_last = None;
            let gen_for_scan = latest_gen;
            scanning_flag.store(true, Ordering::Release);
            if nested.is_none() && !unavailable_latched {
                nested = Some(find_nested_repos(&roots, &excludes));
            }
            let snap = scan_roots(&git_bin, &roots, nested.as_deref().unwrap_or_default(), touched_repos.as_ref().map(|t| (&last_snap, t)), &mut unavailable_latched);
            scanning_flag.store(false, Ordering::Release);
            meta_sigs = collect_all_meta_sigs(&snap.repos);
            if snap != last_snap {
                last_snap = snap.clone();
                sent_gen = gen_for_scan;
                let _ = msg_tx.send(WorkerMsg::Snapshot { snap, scan_gen: gen_for_scan });
                waker.wake();
            } else if gen_for_scan > sent_gen {
                sent_gen = gen_for_scan;
                let _ = msg_tx.send(WorkerMsg::ScanAck(gen_for_scan));
                waker.wake();
            }
        }

        let cmd = if let (Some(first), Some(last)) = (dirty_first, dirty_last) {
            let now = Instant::now();
            let deb = STATUS_DEBOUNCE.saturating_sub(now.saturating_duration_since(last));
            let max_rem = STATUS_MAX_LATENCY.saturating_sub(now.saturating_duration_since(first));
            let wait = deb.min(max_rem);
            match cmd_rx.recv_timeout(wait) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => {
                    (pending_immediate, from_debounce) = (true, true);
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        } else if last_snap.availability == GitAvailability::Ready && !last_snap.repos.is_empty() {
            match cmd_rx.recv_timeout(GIT_META_POLL) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => {
                    if !unavailable_latched && meta_changed(&last_snap.repos, &mut meta_sigs) {
                        pending_immediate = true;
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match cmd_rx.recv() {
                Ok(c) => Some(c),
                Err(_) => return,
            }
        };

        if let Some(first_cmd) = cmd {
            let mut batch = vec![first_cmd];
            while let Ok(more) = cmd_rx.try_recv() {
                batch.push(more);
            }
            for c in batch {
                match c {
                    WorkerCmd::SetRoots { roots: r, scan_gen } => {
                        latest_gen = latest_gen.max(scan_gen);
                        roots = r;
                        nested = None;
                        unavailable_latched = false;
                        pending_immediate = true;
                    }
                    WorkerCmd::SetExcludes { excludes: e, scan_gen } => {
                        latest_gen = latest_gen.max(scan_gen);
                        excludes = e;
                        nested = None;
                        pending_immediate = true;
                    }
                    WorkerCmd::Rewalk => nested = None,
                    WorkerCmd::SetGitBin { bin: b, scan_gen } => {
                        latest_gen = latest_gen.max(scan_gen);
                        git_bin = b.unwrap_or_else(|| "git".into());
                        unavailable_latched = false;
                        pending_immediate = true;
                    }
                    WorkerCmd::Refresh { force_rediscover, scan_gen, paths } => {
                        latest_gen = latest_gen.max(scan_gen);
                        match paths {
                            Some(p) => touched.extend(p),
                            None => full = true,
                        }
                        if force_rediscover {
                            unavailable_latched = false;
                            nested = None;
                            pending_immediate = true;
                        } else if !unavailable_latched && !roots.is_empty() {
                            let now = Instant::now();
                            dirty_first.get_or_insert(now);
                            dirty_last = Some(now);
                        } else if latest_gen > sent_gen {
                            sent_gen = latest_gen;
                            let _ = msg_tx.send(WorkerMsg::ScanAck(latest_gen));
                            waker.wake();
                        }
                    }
                    WorkerCmd::Remote { repo_root } => {
                        let url = remote_display(&git_bin, &repo_root);
                        let _ = msg_tx.send(WorkerMsg::Remote { repo_root, url });
                        waker.wake();
                    }
                    WorkerCmd::Branches { repo_root } => {
                        let list = list_branches(&git_bin, &repo_root);
                        let _ = msg_tx.send(WorkerMsg::Branches { repo_root, list });
                        waker.wake();
                    }
                    WorkerCmd::RemoteList { repo_root } => {
                        let list = list_remotes(&git_bin, &repo_root);
                        let _ = msg_tx.send(WorkerMsg::RemoteList { repo_root, list });
                        waker.wake();
                    }
                    WorkerCmd::Op { repo_root, op } => {
                        let (label, bin, tx, wk, dec) = (op.label(), git_bin.clone(), msg_tx.clone(), waker.clone(), DecOnDrop(pending_ops.clone()));
                        let root = repo_root.clone();
                        let job = move || {
                            let _dec = dec;
                            let result = run_op(&bin, &root, &op);
                            let _ = tx.send(WorkerMsg::OpDone { repo_root: root, op: label, result });
                            wk.wake();
                        };
                        if matches!(label, "Push" | "Pull" | "Fetch") {
                            // The network can take minutes: not on the worker, which keeps status and diffs flowing.
                            if let Err(e) = std::thread::Builder::new().name("iron-sheets-git-net".into()).spawn(job) {
                                let _ = msg_tx.send(WorkerMsg::OpDone { repo_root, op: label, result: Err(format!("could not start git: {e}")) });
                                waker.wake();
                            }
                        } else {
                            job();
                            pending_immediate = true;
                        }
                    }
                    WorkerCmd::ComputeDiff { repo_root, path, rel_posix, version, head_oid, is_added, text } => {
                        let _dec = DecOnDrop(pending_diffs.clone());
                        let (marks, old) = compute_diff_for_file(&git_bin, &repo_root, &rel_posix, head_oid.as_deref(), is_added, &text);
                        let _ = msg_tx.send(WorkerMsg::DiffDone { path, version, head_oid, marks: Arc::from(marks), old: old.into() });
                        waker.wake();
                    }
                }
            }
        }
    }
}

fn git_cmd(git_bin: &str, cwd: &Path) -> Command {
    let mut cmd = Command::new(git_bin);
    cmd.arg("-C")
        .arg(cwd)
        // A root may be an untrusted folder: its `.git/config` must not get commands run (see `filter_overrides`).
        .args(["--no-optional-locks", "-c", "core.quotePath=false", "-c", "core.fsmonitor=false"])
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("LC_ALL", "C");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

/// A hung git (network filesystem, lock, credential prompt) is killed so the worker never stalls for good.
const GIT_TIMEOUT: Duration = if cfg!(test) { Duration::from_secs(2) } else { Duration::from_secs(10) };
/// Stage, unstage and commit (a signing prompt or a big `add -A` may take a while).
const WRITE_TIMEOUT: Duration = if cfg!(test) { Duration::from_secs(2) } else { Duration::from_secs(60) };
/// Push and pull talk to the network.
const NET_TIMEOUT: Duration = if cfg!(test) { Duration::from_secs(2) } else { Duration::from_secs(120) };
/// After git exits, how long to wait for its pipes to close (a daemonised ssh may keep them open).
const PIPE_GRACE: Duration = Duration::from_secs(2);

/// `Command::output` with [`GIT_TIMEOUT`]; stderr is discarded.
fn output(cmd: &mut Command) -> io::Result<Output> {
    run(cmd, GIT_TIMEOUT, false)
}

fn drain(r: Option<impl Read + Send + 'static>) -> Receiver<Vec<u8>> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    if let Some(mut r) = r {
        // Read on a thread so a full pipe cannot deadlock `try_wait`; left detached if git hangs.
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = r.read_to_end(&mut buf);
            let _ = tx.send(buf);
        });
    }
    rx
}

/// Kills git and, on Unix, its whole process group.
/// shortcut: via the `kill` program (no libc dependency); Windows kills only git itself, so an ssh child may outlive a
/// timeout there. Upgrade: a Job Object (`windows-sys`) if that ever shows up.
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill").args(["-KILL", "--"]).arg(format!("-{}", child.id())).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    let _ = child.kill();
}

/// Runs `cmd` for at most `timeout` (then kills it); stderr is captured only when `stderr` is set.
fn run(cmd: &mut Command, timeout: Duration, stderr: bool) -> io::Result<Output> {
    run_with(cmd, timeout, stderr, None)
}

/// [`run`] with `input` on git's stdin (written from a thread, so a full pipe cannot deadlock).
fn run_with(cmd: &mut Command, timeout: Duration, stderr: bool, input: Option<Vec<u8>>) -> io::Result<Output> {
    if input.is_some() {
        cmd.stdin(Stdio::piped());
    }
    #[cfg(unix)]
    {
        // Own process group, so a timeout can take down what git started (ssh, a credential helper), not just git.
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.stdout(Stdio::piped()).stderr(if stderr { Stdio::piped() } else { Stdio::null() }).spawn()?;
    if let (Some(mut stdin), Some(data)) = (child.stdin.take(), input) {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&data);
        });
    }
    let (out_rx, err_rx) = (drain(child.stdout.take()), drain(child.stderr.take()));
    let t0 = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if t0.elapsed() > timeout {
            kill_tree(&mut child);
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, "git timed out"));
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    Ok(Output { status, stdout: out_rx.recv_timeout(PIPE_GRACE).unwrap_or_default(), stderr: err_rx.recv_timeout(PIPE_GRACE).unwrap_or_default() })
}

/// First useful line of a failed git run: the first `fatal:`/`error:` line, else the first stderr line, else a stdout hint.
fn error_line(o: &Output) -> String {
    let err = String::from_utf8_lossy(&o.stderr);
    let mut lines = err.lines().map(str::trim).filter(|l| !l.is_empty());
    if let Some(l) = lines.clone().find(|l| l.starts_with("fatal:") || l.starts_with("error:")).or_else(|| lines.next()) {
        return l.to_string();
    }
    let out = String::from_utf8_lossy(&o.stdout);
    let mut lines = out.lines().map(str::trim).filter(|l| !l.is_empty());
    if lines.clone().any(|l| l.contains("nothing to commit") || l.contains("no changes added")) {
        return "Nothing staged to commit".into();
    }
    lines.next().map_or_else(|| format!("git exited with {}", o.status), str::to_string)
}

/// Runs a write command; `Ok` is the first line of stdout.
fn exec(cmd: &mut Command, timeout: Duration) -> Result<String, String> {
    capture(cmd, timeout).map(|out| out.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default().to_string())
}

/// Runs a command; `Ok` is all of stdout, `Err` git's first error line.
fn capture(cmd: &mut Command, timeout: Duration) -> Result<String, String> {
    capture_with(cmd, timeout, None)
}

fn capture_with(cmd: &mut Command, timeout: Duration, input: Option<Vec<u8>>) -> Result<String, String> {
    match run_with(cmd, timeout, true, input) {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
        Ok(o) => Err(error_line(&o)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err("git was not found".into()),
        Err(e) if e.kind() == io::ErrorKind::TimedOut => Err("git timed out".into()),
        Err(e) => Err(e.to_string()),
    }
}

/// An empty folder as `core.hooksPath`: a repo's hooks are scripts from the folder itself (a downloaded repo is not trusted).
fn hooks_dir() -> Result<&'static Path, String> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let d = iron_sheets_core::paths::AppDirs::discover().cache.join("git-no-hooks");
        std::fs::create_dir_all(&d).ok().map(|_| d)
    })
    .as_deref()
    .ok_or_else(|| "could not create the empty hooks folder in the cache".to_string())
}

/// `user.name` and `user.email` must be configured (we never set them); the error says what to run.
fn identity_error(git_bin: &str, repo: &Path) -> Option<String> {
    ["user.name", "user.email"].into_iter().find(|key| {
        !output(git_cmd(git_bin, repo).args(["config", "--get", key])).is_ok_and(|o| o.status.success() && !o.stdout.trim_ascii().is_empty())
    })
    .map(|key| {
        format!("Git does not know who you are ({key} is not set). Run in a terminal: git config --global user.name \"Your Name\" and git config --global user.email you@example.com")
    })
}

/// Config keys that make git run a program or reach somewhere else; from the repo's own config they are refused.
fn risky_key(k: &str) -> bool {
    const EXACT: [&str; 10] = ["core.alternaterefscommand", "submodule.recurse", "core.sshcommand", "core.askpass", "core.gitproxy", "core.hookspath", "core.fsmonitor", "commit.gpgsign", "tag.gpgsign", "merge.verifysignatures"];
    const PREFIX: [&str; 6] = ["credential.", "protocol.", "filter.", "gpg.", "include.", "includeif."];
    let remote_var = k.starts_with("remote.") && ["uploadpack", "receivepack", "proxy", "vcs"].iter().any(|v| k.ends_with(&format!(".{v}")));
    let url_var = k.starts_with("url.") && (k.ends_with(".insteadof") || k.ends_with(".pushinsteadof"));
    let submodule_cmd = k.starts_with("submodule.") && k.ends_with(".update");
    submodule_cmd || EXACT.contains(&k) || PREFIX.iter().any(|p| k.starts_with(p)) || remote_var || url_var || k == "diff.external"
}

fn is_exec_url(u: &str) -> bool {
    let l = u.trim().to_ascii_lowercase();
    l.starts_with("ext::") || l.starts_with("fd::")
}

/// Why `repo` must not be operated on from here: its own config (scope local or worktree, includes followed) sets a key that
/// runs programs, or a remote URL runs a command. The user can still run git in a terminal.
fn config_refusal(git_bin: &str, repo: &Path) -> Option<String> {
    let Ok(out) = output(git_cmd(git_bin, repo).args(["config", "--list", "--show-scope"])) else {
        return Some("Git: could not read this repository's config".into());
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some((scope, rest)) = line.split_once('\t') else { continue };
        if scope != "local" && scope != "worktree" {
            continue;
        }
        let key = rest.split_once('=').map_or(rest, |(k, _)| k).to_ascii_lowercase();
        if risky_key(&key) {
            return Some(format!("Git: this repository's own config sets {key}; run git in a terminal"));
        }
    }
    let urls = output(git_cmd(git_bin, repo).args(["config", "--get-regexp", r"^remote\..*\.(url|pushurl)$"])).ok()?;
    String::from_utf8_lossy(&urls.stdout)
        .lines()
        .find(|l| l.split_once(' ').is_some_and(|(_, u)| is_exec_url(u)))
        .map(|l| format!("Git: the remote {} runs a command; run git in a terminal", l.split_once(' ').map_or("", |(k, _)| k)))
}

/// `https://user:token@host/x` -> `https://host/x`.
fn strip_userinfo(u: &str) -> String {
    let Some(i) = u.find("://") else { return u.to_string() };
    let rest = &u[i + 3..];
    let auth_end = rest.find('/').unwrap_or(rest.len());
    match rest[..auth_end].rfind('@') {
        Some(at) => format!("{}{}", &u[..i + 3], &rest[at + 1..]),
        None => u.to_string(),
    }
}

/// The URL Push and Pull go to: the upstream's remote, else `origin`.
fn remote_display(git_bin: &str, repo: &Path) -> Option<String> {
    let up = output(git_cmd(git_bin, repo).args(["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])).ok().filter(|o| o.status.success());
    let up = up.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    let name = up.split_once('/').map_or("origin", |(n, _)| n);
    let o = output(git_cmd(git_bin, repo).args(["remote", "get-url", name])).ok().filter(|o| o.status.success())?;
    let url = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (!url.is_empty()).then(|| strip_userinfo(&url))
}

fn run_op(git_bin: &str, repo: &Path, op: &GitOp) -> Result<String, String> {
    if matches!(op, GitOp::Init) {
        // No repository yet, so no config to refuse; the template folder is empty, so no hook is copied in.
        if has_dot_git_ancestor(repo) || output(git_cmd(git_bin, repo).args(["rev-parse", "--git-dir"])).is_ok_and(|o| o.status.success()) {
            return Err("This folder is already in a Git repository".into());
        }
        let template = format!("--template={}", hooks_dir()?.display());
        return exec(git_cmd(git_bin, repo).args(["init", &template]), WRITE_TIMEOUT);
    }
    if let Some(e) = config_refusal(git_bin, repo) {
        return Err(e);
    }
    let hooks = format!("core.hooksPath={}", hooks_dir()?.display());
    let git = || {
        let mut c = git_cmd(git_bin, repo);
        c.args(["-c", &hooks, "--literal-pathspecs"]);
        c
    };
    // No ext:: or fd:: transports, whatever the config says.
    let net = || {
        let mut c = git();
        c.env("GIT_ALLOW_PROTOCOL", "file:git:http:https:ssh");
        c
    };
    let io_err = |e: io::Error| e.to_string();
    match op {
        GitOp::Stage(rel) => exec(git().args(["add", "--"]).arg(rel), WRITE_TIMEOUT).map(|_| format!("Staged {rel}")),
        GitOp::StageAll => exec(git().args(["add", "-A"]), WRITE_TIMEOUT).map(|_| "Staged all changes".into()),
        GitOp::Unstage(rel) => {
            let filters = filter_overrides(git_bin, repo).map_err(io_err)?;
            exec(git().args(filters).args(["reset", "-q", "--"]).arg(rel), WRITE_TIMEOUT).map(|_| format!("Unstaged {rel}"))
        }
        GitOp::Commit(msg) => {
            if let Some(e) = identity_error(git_bin, repo) {
                return Err(e);
            }
            let filters = filter_overrides(git_bin, repo).map_err(io_err)?;
            exec(git().args(filters).args(["commit", "--no-gpg-sign", "--no-verify", "-m"]).arg(msg), WRITE_TIMEOUT)
        }
        // No credential handling of our own: `GIT_TERMINAL_PROMPT=0` (in `git_cmd`) makes a password request fail fast.
        GitOp::Push => exec(net().arg("push"), NET_TIMEOUT).map(|_| "Pushed".into()),
        GitOp::Pull => {
            // No filter blanking: the repo's own `filter.*` is refused before any op, and blanking the user's global ones
            // (Git LFS) would leave pointer files behind.
            exec(net().args(["-c", "merge.verifySignatures=false", "-c", "pull.rebase=false", "pull", "--ff-only", "--no-recurse-submodules"]), NET_TIMEOUT)
        }
        GitOp::Fetch(remote) => {
            let mut c = net();
            c.args(["fetch", "--no-recurse-submodules"]);
            match remote {
                Some(r) => c.arg("--").arg(non_option(r)?),
                None => c.arg("--all"),
            };
            exec(&mut c, NET_TIMEOUT).map(|_| "Fetched".into())
        }
        GitOp::Discard(rel) => discard(repo, rel, &discard_backup_folder(), &git),
        GitOp::UndoDiscard(stamp) => undo_discard(repo, &discard_backup_folder(), stamp.as_deref()),
        // Smudge filters (the user's own, e.g. LFS) run here; the repo's own are refused above, its hooks are off.
        GitOp::Switch(name) => exec(git().args(["switch", "--no-guess", "--no-recurse-submodules", "--"]).arg(non_option(name)?), WRITE_TIMEOUT).map(|_| format!("Switched to {name}")),
        GitOp::NewBranch(name) => {
            exec(git().args(["check-ref-format", "--branch"]).arg(non_option(name)?), GIT_TIMEOUT)?;
            exec(git().args(["switch", "--no-recurse-submodules", "-c"]).arg(name), WRITE_TIMEOUT).map(|_| format!("Created and switched to {name}"))
        }
        // `-d`: only a branch that is merged; git's refusal line comes back as is.
        GitOp::DeleteBranch(name) => exec(git().args(["branch", "-d", "--"]).arg(non_option(name)?), WRITE_TIMEOUT).map(|_| format!("Deleted branch {name}")),
        GitOp::RemoteAdd { name, url } => {
            remote_name_error(name).map_or(Ok(()), |e| Err(e.to_string()))?;
            remote_url_error(url).map_or(Ok(()), |e| Err(e.to_string()))?;
            exec(git().args(["remote", "add", "--"]).arg(name).arg(url.trim()), WRITE_TIMEOUT).map(|_| format!("Added remote {name}"))
        }
        GitOp::RemoteRemove(name) => exec(git().args(["remote", "remove", "--"]).arg(non_option(name)?), WRITE_TIMEOUT).map(|_| format!("Removed remote {name}")),
        GitOp::RemoteRename { from, to } => {
            remote_name_error(to).map_or(Ok(()), |e| Err(e.to_string()))?;
            exec(git().args(["remote", "rename", "--"]).arg(non_option(from)?).arg(to), WRITE_TIMEOUT).map(|_| format!("Renamed remote {from} to {to}"))
        }
        GitOp::Hunk { rel, mark, text, stage } => hunk_op(&git, rel, mark, text, *stage),
        GitOp::Init => Err("Init runs before the repository checks".into()),
    }
}

/// A blob's bytes, no filters (`cat-file`), at most [`MAX_DIFF_BYTES`].
fn blob(git: &dyn Fn() -> Command, spec: &str) -> Result<Vec<u8>, String> {
    let o = run(git().args(["cat-file", "blob", spec]), GIT_TIMEOUT, true).map_err(|e| e.to_string())?;
    match (o.status.success(), o.stdout.len() > MAX_DIFF_BYTES) {
        (false, _) => Err(error_line(&o)),
        (_, true) => Err("This note is too large for hunk staging; stage the whole file".into()),
        _ => Ok(o.stdout),
    }
}

/// Stage or unstage one change of `text`: compose the new index blob from the index blob (and `HEAD`'s) and the buffer
/// (`git_hunks`), store it unfiltered (`hash-object --no-filters`, so no clean filter runs) and point the index entry at it
/// (`update-index --cacheinfo`). Like Zed; `git apply` is not used. Nothing here touches the working tree.
fn hunk_op(git: &dyn Fn() -> Command, rel: &str, mark: &DiffMark, text: &str, stage: bool) -> Result<String, String> {
    use crate::git_hunks::{FileStyle, stage_blob, unstage_blob};
    if text.len() > MAX_DIFF_BYTES {
        return Err("This note is too large for hunk staging; stage the whole file".into());
    }
    // The index entry: one regular file at stage 0 (a conflicted file has several, a link or submodule is no text).
    let listing = capture(git().args(["ls-files", "-s", "-z", "--"]).arg(rel), GIT_TIMEOUT)?;
    let mut entries = listing.split('\0').filter(|e| !e.is_empty());
    let (Some(entry), None) = (entries.next(), entries.next()) else { return Err("This file is not in the index, or it has conflicts".into()) };
    let mut meta = entry.split_once('\t').map_or("", |(m, _)| m).split(' ');
    let (Some(mode @ ("100644" | "100755")), Some(oid), Some("0")) = (meta.next(), meta.next(), meta.next()) else {
        return Err("Only regular files that are not in conflict can be staged by hunk".into());
    };
    let index = blob(git, oid)?;
    let new = if stage {
        // Lines are written in the index blob's style, not the working file's: with `core.autocrlf` the blob is LF while the
        // file is CRLF, and `git add` would store LF. (The file is in the index, so no .gitattributes guess is needed.)
        let head = blob(git, &format!("HEAD:{rel}")).ok();
        stage_blob(&index, head.as_deref(), text, FileStyle::of(&index), mark)?
    } else {
        let head = blob(git, &format!("HEAD:{rel}")).map_err(|_| "This file is not in the last commit: unstage the whole file".to_string())?;
        unstage_blob(&index, &head, text, mark)?
    };
    let new_oid = capture_with(git().args(["hash-object", "-w", "--stdin", "--no-filters"]), WRITE_TIMEOUT, Some(new))?;
    exec(git().args(["update-index", "--cacheinfo", mode]).arg(new_oid.trim()).arg(rel), WRITE_TIMEOUT)?;
    Ok(format!("{} hunk in {rel}", if stage { "Staged" } else { "Unstaged" }))
}

/// A branch or remote name from the user is an argument, never an option and never blank.
fn non_option(n: &str) -> Result<&str, String> {
    if n.is_empty() || n.starts_with('-') || n.chars().any(|c| c.is_whitespace() || c.is_control()) { Err(format!("'{n}' is not a valid name")) } else { Ok(n) }
}

/// Why `name` cannot be a remote name (`None` = fine). shortcut: ASCII letters, digits and `-_.`, narrower than git; upgrade on request.
pub fn remote_name_error(name: &str) -> Option<&'static str> {
    let ok = name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()) && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) && !name.contains("..");
    if name.is_empty() { Some("Enter a name") } else if ok { None } else { Some("Use letters, digits, - _ and . only") }
}

/// Why `url` cannot be a remote URL (`None` = fine): nothing that runs a command (`ext::`, `fd::`, any `helper::` form).
pub fn remote_url_error(url: &str) -> Option<&'static str> {
    let u = url.trim();
    let helper = u.split_once("::").is_some_and(|(h, _)| !h.is_empty() && h.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '+' | '.')));
    if u.is_empty() {
        Some("Enter a URL or a path")
    } else if u.starts_with('-') || u.chars().any(char::is_control) {
        Some("This is not a URL")
    } else if is_exec_url(u) || helper {
        Some("This URL would run a command")
    } else {
        None
    }
}

fn list_branches(git_bin: &str, repo: &Path) -> Result<Vec<Branch>, String> {
    let out = capture(git_cmd(git_bin, repo).args(["for-each-ref", "--count=2000", "--format=%(HEAD)\t%(refname:short)\t%(contents:subject)", "refs/heads"]), GIT_TIMEOUT)?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let mut f = l.splitn(3, '\t');
            let (head, name) = (f.next()?, f.next()?);
            Some(Branch { name: name.to_string(), current: head == "*", subject: f.next().unwrap_or_default().to_string() })
        })
        .collect())
}

fn list_remotes(git_bin: &str, repo: &Path) -> Result<Vec<RemoteInfo>, String> {
    let out = capture(git_cmd(git_bin, repo).args(["remote", "-v"]), GIT_TIMEOUT)?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let (name, rest) = l.split_once('\t')?;
            let url = rest.strip_suffix(" (fetch)")?;
            Some(RemoteInfo { name: name.to_string(), url: strip_userinfo(url) })
        })
        .collect())
}

/// Where discarded files are kept: `<cache>/git-discard-backup/<repo hash>/<stamp>/<path in the repo>`, never inside the repo.
pub fn discard_backup_folder() -> PathBuf {
    iron_sheets_core::paths::AppDirs::discover().cache.join("git-discard-backup")
}

const KEEP_BACKUPS: usize = 20;
const MAX_BACKUP_BYTES: u64 = 50 * 1024 * 1024;

fn backup_dir(base: &Path, repo: &Path) -> PathBuf {
    let mut h = FxHasher::default();
    std::path::absolute(repo).unwrap_or_else(|_| repo.to_path_buf()).hash(&mut h);
    base.join(format!("{:016x}", h.finish()))
}

/// `rel` (from our snapshot) joined onto the repo, refusing anything that could leave it, and anything reached through a
/// symlink (a write or delete there would land outside the repo).
fn repo_file(repo: &Path, rel: &str) -> Result<PathBuf, String> {
    let bad = rel.is_empty() || rel.split('/').any(|s| s == ".." || s.is_empty()) || Path::new(rel).is_absolute() || rel.contains('\0');
    if bad {
        return Err(format!("'{rel}' is not a path inside the repository"));
    }
    let mut p = repo.to_path_buf();
    for seg in rel.split('/') {
        p.push(seg);
        if std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(format!("{rel} is reached through a symbolic link; use a terminal"));
        }
    }
    Ok(p)
}

fn dir_size(d: &Path) -> u64 {
    std::fs::read_dir(d).into_iter().flatten().flatten().map(|e| e.metadata().map_or(0, |m| if m.is_dir() { dir_size(&e.path()) } else { m.len() })).sum()
}

/// What a file holds, for telling whether it changed since a discard: a hash of its bytes, `absent` when missing.
fn content_sig(p: &Path) -> String {
    match std::fs::read(p) {
        Ok(b) => {
            let mut h = FxHasher::default();
            b.hash(&mut h);
            format!("{:016x}", h.finish())
        }
        Err(_) => "absent".into(),
    }
}

/// Stamp folders of a repo's backups in creation order (names are strictly increasing, see [`backup_file`]).
fn stamps_of(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    v.sort();
    v
}

/// Copies the regular file to a new stamp folder (`<stamp>/files/<rel>`) and drops the oldest ones beyond [`KEEP_BACKUPS`] /
/// [`MAX_BACKUP_BYTES`] (the new one stays). The stamp is the clock in ms but always above the newest existing one, so
/// name order is creation order even when the clock steps back.
fn backup_file(base: &Path, repo: &Path, rel: &str, file: &Path) -> Result<PathBuf, String> {
    let dir = backup_dir(base, repo);
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not make the backup folder: {e}"))?;
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let newest = stamps_of(&dir).last().and_then(|p| p.file_name()?.to_str()?.parse::<u128>().ok());
    let stamp_dir = dir.join(format!("{:020}", newest.map_or(now, |n| now.max(n + 1))));
    let dest = posix_rel_to_path(&stamp_dir.join("files"), rel);
    let copied = dest.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::copy(file, &dest));
    if let Err(e) = copied {
        let _ = std::fs::remove_dir_all(&stamp_dir);
        return Err(format!("could not back up {rel}: {e}; nothing was discarded"));
    }
    let mut stamps = stamps_of(&dir);
    let mut total: u64 = stamps.iter().map(|d| dir_size(d)).sum();
    while stamps.len() > 1 && (stamps.len() > KEEP_BACKUPS || total > MAX_BACKUP_BYTES) {
        let old = stamps.remove(0);
        total = total.saturating_sub(dir_size(&old));
        let _ = std::fs::remove_dir_all(old);
    }
    Ok(dest)
}

/// The stamp of the backup a "Discarded ... Backup: <path>" line names (`<base>/<repo hash>/<stamp>/files/...`).
fn backup_stamp(text: &str, base: &Path) -> Option<String> {
    let path = Path::new(text.split("Backup: ").nth(1)?);
    path.strip_prefix(base).ok()?.components().nth(1).map(|c| c.as_os_str().to_string_lossy().into_owned())
}

/// Discards the working-tree changes of one file: tracked files go back to the index state (`restore --worktree`),
/// untracked ones are deleted. A copy of what is lost is kept first (see [`discard_backup_folder`]); no backup, no discard.
/// Only regular files: a symlink, a FIFO or a device is refused, and so is a path through a symlinked folder.
fn discard(repo: &Path, rel: &str, base: &Path, git: &dyn Fn() -> Command) -> Result<String, String> {
    let file = repo_file(repo, rel)?;
    let meta = std::fs::symlink_metadata(&file).ok();
    if let Some(m) = &meta {
        if m.is_dir() {
            return Err("Discarding a folder is not supported; use the file tree".into());
        }
        if !m.is_file() {
            return Err(format!("{rel} is not a regular file; use a terminal"));
        }
        if m.len() > MAX_BACKUP_BYTES {
            return Err(format!("{rel} is over 50 MB, too large to back up; discard it in a terminal"));
        }
    }
    // An empty answer means untracked; an error means unknown, and unknown never deletes.
    let tracked = !capture(git().args(["ls-files", "--"]).arg(rel), GIT_TIMEOUT)?.trim().is_empty();
    let backup = meta.map(|_| backup_file(base, repo, rel, &file)).transpose()?;
    let result = if tracked { exec(git().args(["restore", "--worktree", "--"]).arg(rel), WRITE_TIMEOUT).map(|_| ()) } else { std::fs::remove_file(&file).map_err(|e| e.to_string()) };
    // Nothing was lost on failure, so nothing is left to undo.
    let stamp_dir = backup.as_ref().and_then(|b| b.ancestors().nth(rel.split('/').count() + 1));
    if let Err(e) = result {
        if let Some(d) = stamp_dir {
            let _ = std::fs::remove_dir_all(d);
        }
        return Err(e);
    }
    if let Some(d) = stamp_dir {
        // What the discard left, so an undo can tell whether the file changed since.
        let _ = std::fs::write(d.join("after"), content_sig(&file));
    }
    Ok(match backup {
        Some(b) => format!("Discarded {rel}. Backup: {}", b.display()),
        None => format!("Discarded {rel}"),
    })
}

/// Puts a backup back: the one with `stamp`, else the newest of the repo. Refuses when the file changed since the discard.
fn undo_discard(repo: &Path, base: &Path, stamp: Option<&str>) -> Result<String, String> {
    let dir = backup_dir(base, repo);
    let newest = match stamp {
        Some(s) if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) => return Err("Not a backup of this repository".into()),
        Some(s) => Some(dir.join(s)).filter(|p| p.is_dir()),
        None => stamps_of(&dir).pop(),
    };
    let Some(newest) = newest else { return Err("No discarded file to restore".into()) };
    let mut files = Vec::new();
    let mut todo = vec![newest.join("files")];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() { todo.push(p) } else { files.push(p) }
        }
    }
    let Some(backup) = files.first() else {
        let _ = std::fs::remove_dir_all(&newest);
        return Err("No discarded file to restore".into());
    };
    let rel = backup.strip_prefix(newest.join("files")).map_err(|e| e.to_string())?.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
    let target = repo_file(repo, &rel)?;
    if std::fs::read_to_string(newest.join("after")).is_ok_and(|a| a != content_sig(&target)) {
        return Err(format!("{rel} changed since it was discarded; not overwriting it. The copy is {}", backup.display()));
    }
    target.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::copy(backup, &target)).map_err(|e| format!("could not restore {rel}: {e}"))?;
    let _ = std::fs::remove_dir_all(&newest);
    Ok(format!("Restored {rel}"))
}

/// Shows finished operations in the status bar: errors always (push and pull say to run it in a terminal), successes of
/// the long ones only (a stage click does not need a toast). Called once per frame by the frontend.
pub fn report<U>(state: &mut AppState<U>) {
    let Some(g) = GitModule::of_mut(state) else { return };
    if g.unreported.is_empty() {
        return;
    }
    for o in std::mem::take(&mut g.unreported) {
        match o.result {
            Ok(t) if matches!(o.op, "Commit" | "Push" | "Pull" | "Init") => state.status.info(format!("{}: {t}", o.op)),
            Ok(t) if o.op == "Discard" => {
                let stamp = backup_stamp(&t, &discard_backup_folder()).unwrap_or_default();
                let undo = AppCommand::Module { id: "git.undo_discard", arg: format!("{}\0{stamp}", o.repo_root.display()) };
                state.notify(t, Some(("Undo", undo)), 12);
            }
            Ok(_) if is_quiet(o.op) => {}
            Ok(t) => state.status.info(t),
            Err(e) if matches!(o.op, "Push" | "Pull") => {
                let dir = o.repo_root.file_name().map_or_else(String::new, |n| format!(" in {}", n.to_string_lossy()));
                state.status.error(format!("{} failed: {}. Run `git {}` in a terminal{dir}.", o.op, e.trim_end_matches('.'), o.op.to_lowercase()));
            }
            Err(e) => state.status.error(format!("{} failed: {e}", o.op)),
        }
    }
}

fn strip_verbatim_prefix(p: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        use std::path::Prefix;
        let mut out = PathBuf::new();
        let mut comps = p.components();
        if let Some(Component::Prefix(pre)) = comps.next() {
            match pre.kind() {
                Prefix::VerbatimDisk(d) => out.push(format!("{}:", d as char)),
                Prefix::VerbatimUNC(srv, share) => {
                    let mut unc = OsString::from(r"\\");
                    unc.push(srv);
                    unc.push(r"\");
                    unc.push(share);
                    out.push(unc);
                }
                Prefix::Verbatim(s) => out.push(s),
                _ => out.push(pre.as_os_str()),
            }
        }
        for c in comps {
            out.push(c.as_os_str());
        }
        out
    }
    #[cfg(not(windows))]
    {
        p.to_path_buf()
    }
}

fn resolve_symlinks(path: &Path) -> PathBuf {
    let base = if path.is_absolute() { path.to_path_buf() } else { std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()) };
    let mut queue: VecDeque<OsString> = base.components().map(|c| c.as_os_str().to_os_string()).collect();
    let mut resolved = PathBuf::new();
    let mut hops = 0usize;
    while let Some(part) = queue.pop_front() {
        if part == "." {
            continue;
        }
        if part == ".." {
            if matches!(resolved.components().next_back(), Some(Component::Normal(_))) {
                resolved.pop();
            } else if !resolved.has_root() {
                resolved.push(&part);
            }
            continue;
        }
        resolved.push(&part);
        let is_normal = {
            let mut cs = Path::new(&part).components();
            matches!(cs.next(), Some(Component::Normal(_))) && cs.next().is_none()
        };
        if is_normal && hops < MAX_SYMLINK_HOPS {
            if let Ok(target) = std::fs::read_link(&resolved) {
                hops += 1;
                let target = strip_verbatim_prefix(&target);
                if target.is_absolute() {
                    resolved.clear();
                } else {
                    resolved.pop();
                }
                for c in target.components().rev() {
                    queue.push_front(c.as_os_str().to_os_string());
                }
            }
        }
    }
    resolved
}

fn has_dot_git_ancestor(root: &Path) -> bool {
    if root.ancestors().any(|anc| anc.join(".git").exists()) {
        return true;
    }
    let resolved = resolve_symlinks(root);
    resolved != root && resolved.ancestors().any(|anc| anc.join(".git").exists())
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push(comp);
                }
            }
            c => out.push(c),
        }
    }
    out
}

pub fn posix_rel_to_path(repo_root: &Path, rel_posix: &str) -> PathBuf {
    let mut p = repo_root.to_path_buf();
    for seg in rel_posix.split('/') {
        if !seg.is_empty() && seg != "." {
            p.push(seg);
        }
    }
    p
}

pub(crate) fn path_to_posix_rel(repo_root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(repo_root).ok()?;
    let mut out = String::new();
    for comp in rel.components() {
        if let Component::Normal(s) = comp {
            if !out.is_empty() {
                out.push('/');
            }
            out.push_str(&s.to_string_lossy());
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

pub(crate) fn derive_repo_root(ws_root: &Path, show_toplevel: &str, show_prefix: &str) -> PathBuf {
    let ws_norm = normalize_path(ws_root);
    let prefix_segs: Vec<&str> = show_prefix.trim_end_matches('/').split('/').filter(|s| !s.is_empty()).collect();
    if prefix_segs.is_empty() {
        return ws_norm;
    }
    let comps: Vec<_> = ws_norm.components().collect();
    if comps.len() >= prefix_segs.len() {
        let tail = &comps[comps.len() - prefix_segs.len()..];
        let matches = tail.iter().zip(&prefix_segs).all(|(c, seg)| matches!(c, Component::Normal(os) if os.to_string_lossy() == *seg));
        if matches {
            let mut root = ws_norm;
            for _ in 0..prefix_segs.len() {
                root.pop();
            }
            if resolve_symlinks(&root) == resolve_symlinks(Path::new(show_toplevel)) {
                return root;
            }
        }
    }
    normalize_path(Path::new(show_toplevel))
}

enum ProbeResult {
    Repo { repo_root: PathBuf, git_dir: PathBuf, common_dir: PathBuf, alias: Option<(PathBuf, PathBuf)> },
    NotARepo,
    GitMissing,
}

fn probe_repo(git_bin: &str, ws_root: &Path) -> ProbeResult {
    if std::env::var_os("GIT_DIR").is_none() && !has_dot_git_ancestor(ws_root) {
        return ProbeResult::NotARepo;
    }
    let out = match output(git_cmd(git_bin, ws_root).args(["rev-parse", "--show-toplevel", "--absolute-git-dir", "--git-common-dir", "--show-prefix"])) {
        Ok(o) => o,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return ProbeResult::GitMissing,
        Err(_) => return ProbeResult::NotARepo,
    };
    if !out.status.success() {
        return ProbeResult::NotARepo;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut lines = stdout.lines().map(|l| l.trim_end_matches('\r'));
    let Some(toplevel) = lines.next() else { return ProbeResult::NotARepo };
    let Some(abs_git_dir) = lines.next() else { return ProbeResult::NotARepo };
    let Some(common_raw) = lines.next() else { return ProbeResult::NotARepo };
    let show_prefix = lines.next().unwrap_or("");
    let resolved_ws = resolve_symlinks(ws_root);
    let repo_root = derive_repo_root(ws_root, toplevel, show_prefix);
    let ws_norm = normalize_path(ws_root);
    let real_subdir = posix_rel_to_path(&repo_root, show_prefix);
    let alias = (ws_norm != real_subdir).then_some((ws_norm, real_subdir));
    let git_dir = normalize_path(Path::new(abs_git_dir));
    let common_path = Path::new(common_raw);
    let common_dir = if common_path.is_relative() { normalize_path(&resolved_ws.join(common_path)) } else { normalize_path(common_path) };
    ProbeResult::Repo { repo_root, git_dir, common_dir, alias }
}

/// Folders below the workspace folders (not the folders themselves) that hold a `.git`, found like Zed does: a bounded walk
/// that skips hidden and excluded folders and links, stops at the repositories it finds, and visits at most 20,000 folders.
pub(crate) fn find_nested_repos(roots: &[PathBuf], excludes: &ExcludeSet) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut visited = 0usize;
    for root in roots {
        let mut stack = vec![(root.clone(), String::new(), 0usize)];
        while let Some((dir, rel, depth)) = stack.pop() {
            visited += 1;
            if visited > MAX_WALK_DIRS {
                break;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for e in entries.flatten() {
                // `file_type` does not follow links: a symlinked folder is neither entered nor a loop.
                if !e.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                let name = e.file_name().to_string_lossy().into_owned();
                let child_rel = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
                if name.starts_with('.') || excludes.matches(&child_rel, &name) {
                    continue;
                }
                let child = e.path();
                if std::fs::symlink_metadata(child.join(".git")).is_ok() {
                    // shortcut: a repo is not searched for repos inside it (they would need their own walk of a possibly huge
                    // working tree); upgrade by walking only the folders git itself lists as untracked.
                    found.push(child);
                } else if depth + 1 < NESTED_DEPTH {
                    stack.push((child, child_rel, depth + 1));
                }
            }
        }
    }
    found.sort(); // `read_dir` order is arbitrary; the panel lists repos in a steady order
    found.truncate(MAX_NESTED);
    found
}

/// `reuse`: the previous snapshot and the repos to scan again; every other repo is taken over from it without a git call.
fn scan_roots(git_bin: &str, roots: &[PathBuf], nested: &[PathBuf], reuse: Option<(&GitSnapshot, &FxHashSet<PathBuf>)>, unavailable_latched: &mut bool) -> GitSnapshot {
    let mut repos: Vec<RepoSnapshot> = Vec::new();
    let mut root_to_repo: FxHashMap<PathBuf, usize> = FxHashMap::default();
    let mut repo_index: FxHashMap<PathBuf, usize> = FxHashMap::default();

    for (r, is_root) in roots.iter().map(|r| (r, true)).chain(nested.iter().map(|r| (r, false))) {
        if let Some((prev, touched)) = reuse {
            let at = if is_root { prev.root_to_repo.get(r).copied() } else { prev.repos.iter().position(|x| x.repo_root == *r) };
            if let Some(old) = at.and_then(|i| prev.repos.get(i)).filter(|o| !touched.contains(&o.repo_root)) {
                let idx = *repo_index.entry(old.repo_root.clone()).or_insert_with(|| {
                    repos.push(old.clone());
                    repos.len() - 1
                });
                if is_root {
                    root_to_repo.insert(r.clone(), idx);
                }
                continue;
            }
        }
        match probe_repo(git_bin, r) {
            ProbeResult::GitMissing => {
                *unavailable_latched = true;
                return GitSnapshot { availability: GitAvailability::Unavailable, ..Default::default() };
            }
            ProbeResult::NotARepo => {}
            ProbeResult::Repo { repo_root, git_dir, common_dir, alias } => {
                if let Some(&idx) = repo_index.get(&repo_root) {
                    if let Some(a) = alias {
                        if !repos[idx].aliases.contains(&a) {
                            repos[idx].aliases.push(a);
                        }
                    }
                    if is_root {
                        root_to_repo.insert(r.clone(), idx);
                    }
                    continue;
                }
                match run_status(git_bin, &repo_root, &git_dir, &common_dir) {
                    Ok(mut snap) => {
                        if let Some(a) = alias {
                            snap.aliases.push(a);
                        }
                        let idx = repos.len();
                        repo_index.insert(repo_root, idx);
                        if is_root {
                            root_to_repo.insert(r.clone(), idx);
                        }
                        repos.push(snap);
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        *unavailable_latched = true;
                        return GitSnapshot { availability: GitAvailability::Unavailable, ..Default::default() };
                    }
                    Err(_) => {}
                }
            }
        }
    }

    // A nested repo shows up in its parent's status as one untracked folder: its content is its own, not a change of the parent.
    let roots_of: Vec<PathBuf> = repos.iter().map(|r| r.repo_root.clone()).collect();
    for parent in &mut repos {
        for inner in roots_of.iter().filter(|i| **i != parent.repo_root) {
            if let Some(rel) = path_to_posix_rel(&parent.repo_root, inner) {
                let before = parent.changes.len();
                parent.changes.retain(|c| c.rel.trim_end_matches('/') != rel);
                parent.dirty_count = parent.dirty_count.saturating_sub(before - parent.changes.len());
            }
        }
    }

    let availability = if repos.is_empty() { GitAvailability::NotARepo } else { GitAvailability::Ready };
    GitSnapshot { availability, repos, root_to_repo }
}

/// `-c` arguments that blank every configured clean/process filter: `git status` runs the clean filter on
/// stat-dirty files, so a repo's own config could otherwise execute commands. Smudge/textconv are not reached
/// by `status`, `rev-parse` or `show <blob>`.
fn filter_overrides(git_bin: &str, repo_root: &Path) -> io::Result<Vec<String>> {
    let out = output(git_cmd(git_bin, repo_root).args(["config", "--name-only", "-z", "--get-regexp", r"^filter\..*\.(clean|process|required)$"]))?;
    let mut args = Vec::new();
    for key in String::from_utf8_lossy(&out.stdout).split('\0').filter(|k| !k.is_empty()) {
        let Some(name) = key.strip_prefix("filter.").and_then(|k| k.rsplit_once('.')).map(|(n, _)| n) else { continue };
        if name.contains('=') {
            // `-c` splits at the first `=`, so this driver could not be overridden.
            return Err(io::Error::other("unsupported filter name"));
        }
        for (var, val) in [("clean", ""), ("process", ""), ("required", "false")] {
            args.extend(["-c".to_string(), format!("filter.{name}.{var}={val}")]);
        }
    }
    Ok(args)
}

fn run_status(git_bin: &str, repo_root: &Path, git_dir: &Path, common_dir: &Path) -> io::Result<RepoSnapshot> {
    let out = output(
        git_cmd(git_bin, repo_root)
            .args(filter_overrides(git_bin, repo_root)?)
            // Submodules would be scanned by child gits with their own, unblanked filters.
            .args(["status", "--porcelain=v2", "-z", "--branch", "--ignored=matching", "--ignore-submodules=all"]),
    )?;
    if !out.status.success() {
        return Err(io::Error::other("git status failed"));
    }
    Ok(parse_porcelain_v2(repo_root, git_dir, common_dir, &out.stdout))
}

fn map_xy(x: u8, y: u8) -> GitFileStatus {
    if x == b'U' || y == b'U' || (x == b'A' && y == b'A') || (x == b'D' && y == b'D') {
        GitFileStatus::Conflict
    } else if y == b'D' || (x == b'D' && y == b'.') {
        GitFileStatus::Deleted
    } else if x == b'A' {
        GitFileStatus::Added
    } else {
        GitFileStatus::Modified
    }
}

pub(crate) fn parse_porcelain_v2(repo_root: &Path, git_dir: &Path, common_dir: &Path, bytes: &[u8]) -> RepoSnapshot {
    let mut branch = String::new();
    let mut detached = false;
    let mut unborn = false;
    let mut head_oid: Option<String> = None;
    let (mut upstream, mut ahead, mut behind) = (None, 0u32, 0u32);
    let mut files: FxHashMap<PathBuf, GitFileStatus> = FxHashMap::default();
    let mut dirs: FxHashMap<PathBuf, GitFileStatus> = FxHashMap::default();
    let mut dirty_count = 0usize;
    let mut changes: Vec<ChangeEntry> = Vec::new();

    let insert_file = |rel_posix: &str,
                       st: GitFileStatus,
                       files: &mut FxHashMap<PathBuf, GitFileStatus>,
                       dirs: &mut FxHashMap<PathBuf, GitFileStatus>,
                       dirty_count: &mut usize| {
        let rel_trimmed = rel_posix.trim_end_matches('/');
        if rel_trimmed.is_empty() {
            return;
        }
        let abs = posix_rel_to_path(repo_root, rel_trimmed);
        files.insert(abs.clone(), st);
        if st != GitFileStatus::Ignored {
            *dirty_count += 1;
            let mut cur = abs.parent();
            while let Some(p) = cur {
                if !p.starts_with(repo_root) {
                    break;
                }
                let replace = dirs.get(p).is_none_or(|prev| st.folder_rank() > prev.folder_rank());
                if replace {
                    dirs.insert(p.to_path_buf(), st);
                }
                if p == repo_root {
                    break;
                }
                cur = p.parent();
            }
        }
    };

    let mut iter = bytes.split(|&b| b == 0);
    while let Some(raw) = iter.next() {
        if raw.is_empty() {
            continue;
        }
        let s = String::from_utf8_lossy(raw);
        if let Some(rest) = s.strip_prefix("# branch.oid ") {
            if rest == "(initial)" {
                unborn = true;
                head_oid = None;
            } else {
                unborn = false;
                head_oid = Some(rest.to_string());
            }
        } else if let Some(rest) = s.strip_prefix("# branch.head ") {
            if rest == "(detached)" {
                detached = true;
                branch = "HEAD".into();
            } else {
                detached = false;
                branch = rest.to_string();
            }
        } else if let Some(rest) = s.strip_prefix("# branch.upstream ") {
            upstream = Some(rest.to_string());
        } else if let Some(rest) = s.strip_prefix("# branch.ab ") {
            let n = |t: Option<&str>, sign: char| t.and_then(|t| t.strip_prefix(sign)).and_then(|t| t.parse().ok()).unwrap_or(0);
            let mut it = rest.split(' ');
            (ahead, behind) = (n(it.next(), '+'), n(it.next(), '-'));
        } else if s.starts_with('#') {
            continue;
        } else if let Some(rest) = s.strip_prefix("1 ") {
            let mut parts = rest.splitn(8, ' ');
            let Some(xy) = parts.next() else { continue };
            for _ in 0..6 {
                parts.next();
            }
            let Some(path) = parts.next() else { continue };
            let b = xy.as_bytes();
            let st = if b.len() >= 2 { map_xy(b[0], b[1]) } else { GitFileStatus::Modified };
            if b.len() >= 2 {
                changes.push(ChangeEntry { rel: path.to_string(), x: b[0], y: b[1] });
            }
            insert_file(path, st, &mut files, &mut dirs, &mut dirty_count);
        } else if let Some(rest) = s.strip_prefix("2 ") {
            let mut parts = rest.splitn(9, ' ');
            let Some(xy) = parts.next() else {
                let _ = iter.next();
                continue;
            };
            for _ in 0..7 {
                parts.next();
            }
            let path = parts.next();
            // Consume the extra NUL-terminated origPath token.
            let _orig = iter.next();
            if let Some(path) = path {
                let b = xy.as_bytes();
                let st = if b.len() >= 2 { map_xy(b[0], b[1]) } else { GitFileStatus::Modified };
                if b.len() >= 2 {
                    // shortcut: a rename is listed under its new path only; unstaging it leaves the old path's deletion staged.
                    changes.push(ChangeEntry { rel: path.to_string(), x: b[0], y: b[1] });
                }
                insert_file(path, st, &mut files, &mut dirs, &mut dirty_count);
            }
        } else if let Some(rest) = s.strip_prefix("u ") {
            let mut parts = rest.splitn(10, ' ');
            for _ in 0..9 {
                parts.next();
            }
            if let Some(path) = parts.next() {
                changes.push(ChangeEntry { rel: path.to_string(), x: b'U', y: b'U' });
                insert_file(path, GitFileStatus::Conflict, &mut files, &mut dirs, &mut dirty_count);
            }
        } else if let Some(path) = s.strip_prefix("? ") {
            changes.push(ChangeEntry { rel: path.to_string(), x: b'?', y: b'?' });
            insert_file(path, GitFileStatus::Untracked, &mut files, &mut dirs, &mut dirty_count);
        } else if let Some(path) = s.strip_prefix("! ") {
            insert_file(path, GitFileStatus::Ignored, &mut files, &mut dirs, &mut dirty_count);
        }
    }

    RepoSnapshot {
        repo_root: repo_root.to_path_buf(),
        git_dir: git_dir.to_path_buf(),
        common_dir: common_dir.to_path_buf(),
        branch,
        detached,
        unborn,
        head_oid,
        dirty_count,
        files,
        dirs,
        changes,
        upstream,
        ahead,
        behind,
        aliases: Vec::new(),
    }
}

/// The marks of the buffer against `HEAD`, and the `HEAD` text they were computed against (empty without marks).
fn compute_diff_for_file(git_bin: &str, repo_root: &Path, rel_posix: &str, head_oid: Option<&str>, is_added: bool, new_text: &str) -> (Vec<DiffMark>, String) {
    if new_text.len() > MAX_DIFF_BYTES {
        return Default::default();
    }
    if is_added || head_oid.is_none() {
        if is_added {
            let lines = new_text.split('\n').count() as u32;
            if lines > 0 {
                return (vec![DiffMark { start: 0, end: lines, old_start: 0, old_end: 0, kind: DiffKind::Added }], String::new());
            }
        }
        return Default::default();
    }
    let spec = format!("HEAD:{rel_posix}");
    let out = match output(git_cmd(git_bin, repo_root).args(["show", &spec])) {
        Ok(o) if o.status.success() => o,
        _ => return Default::default(),
    };
    if out.stdout.len() > MAX_DIFF_BYTES {
        return Default::default();
    }
    let Some(old_text) = textio::decode_text_lf(out.stdout) else {
        return Default::default();
    };
    if old_text == new_text {
        return Default::default();
    }
    (diff_lines(&old_text, new_text), old_text)
}

#[derive(Clone, Copy)]
struct LineItem<'a> {
    hash: u64,
    text: &'a str,
}

impl<'a> LineItem<'a> {
    fn new(text: &'a str) -> Self {
        let mut h = FxHasher::default();
        text.hash(&mut h);
        LineItem { hash: h.finish(), text }
    }
}

impl PartialEq for LineItem<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash && self.text == other.text
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffOp {
    Equal(usize),
    Delete(usize),
    Insert(usize),
}

/// Computes sorted, non-overlapping [`DiffMark`]s transforming `old` into `new` (both LF-normalized).
pub fn diff_lines(old: &str, new: &str) -> Vec<DiffMark> {
    if old == new {
        return Vec::new();
    }
    let a: Vec<LineItem<'_>> = old.split('\n').map(LineItem::new).collect();
    let b: Vec<LineItem<'_>> = new.split('\n').map(LineItem::new).collect();
    let mut ops = Vec::new();
    myers_diff(&a, &b, &mut ops);
    coalesce_ops(&ops)
}

fn coalesce_ops(ops: &[DiffOp]) -> Vec<DiffMark> {
    let mut marks = Vec::new();
    let (mut new_line, mut old_line): (u32, u32) = (0, 0);
    let mut del_run: usize = 0;
    let mut ins_run: usize = 0;
    let (mut ins_start, mut old_start): (u32, u32) = (0, 0);

    let flush = |marks: &mut Vec<DiffMark>, del_run: &mut usize, ins_run: &mut usize, ins_start: u32, old_start: u32| {
        let (start, end, old_end) = (ins_start, ins_start + *ins_run as u32, old_start + *del_run as u32);
        let kind = match (*del_run > 0, *ins_run > 0) {
            (true, true) => Some(DiffKind::Modified),
            (false, true) => Some(DiffKind::Added),
            (true, false) => Some(DiffKind::Deleted),
            _ => None,
        };
        marks.extend(kind.map(|kind| DiffMark { start, end, old_start, old_end, kind }));
        *del_run = 0;
        *ins_run = 0;
    };

    for &op in ops {
        match op {
            DiffOp::Equal(n) => {
                if n > 0 {
                    flush(&mut marks, &mut del_run, &mut ins_run, ins_start, old_start);
                    new_line += n as u32;
                    old_line += n as u32;
                    ins_start = new_line;
                }
            }
            DiffOp::Delete(n) => {
                if del_run == 0 && ins_run == 0 {
                    ins_start = new_line;
                    old_start = old_line;
                }
                del_run += n;
                old_line += n as u32;
            }
            DiffOp::Insert(n) => {
                if del_run == 0 && ins_run == 0 {
                    ins_start = new_line;
                    old_start = old_line;
                }
                ins_run += n;
                new_line += n as u32;
            }
        }
    }
    flush(&mut marks, &mut del_run, &mut ins_run, ins_start, old_start);
    marks
}

fn push_op(ops: &mut Vec<DiffOp>, op: DiffOp) {
    match (ops.last_mut(), op) {
        (Some(DiffOp::Equal(a)), DiffOp::Equal(b)) => *a += b,
        (Some(DiffOp::Delete(a)), DiffOp::Delete(b)) => *a += b,
        (Some(DiffOp::Insert(a)), DiffOp::Insert(b)) => *a += b,
        (_, DiffOp::Equal(0) | DiffOp::Delete(0) | DiffOp::Insert(0)) => {}
        _ => ops.push(op),
    }
}

fn myers_diff(a: &[LineItem<'_>], b: &[LineItem<'_>], ops: &mut Vec<DiffOp>) {
    let mut a0 = 0usize;
    let mut a1 = a.len();
    let mut b0 = 0usize;
    let mut b1 = b.len();

    while a0 < a1 && b0 < b1 && a[a0] == b[b0] {
        a0 += 1;
        b0 += 1;
    }
    let mut suffix = 0usize;
    while a0 < a1 && b0 < b1 && a[a1 - 1] == b[b1 - 1] {
        a1 -= 1;
        b1 -= 1;
        suffix += 1;
    }

    push_op(ops, DiffOp::Equal(a0));

    let a_mid = &a[a0..a1];
    let b_mid = &b[b0..b1];
    let n = a_mid.len();
    let m = b_mid.len();

    if n == 0 {
        push_op(ops, DiffOp::Insert(m));
    } else if m == 0 {
        push_op(ops, DiffOp::Delete(n));
    } else if let Some((x, y, u, v)) = find_middle_snake(a_mid, b_mid) {
        myers_diff(&a_mid[..x], &b_mid[..y], ops);
        push_op(ops, DiffOp::Equal(u - x));
        myers_diff(&a_mid[u..], &b_mid[v..], ops);
    } else {
        // Fallback when edit distance exceeds MAX_MYERS_D.
        push_op(ops, DiffOp::Delete(n));
        push_op(ops, DiffOp::Insert(m));
    }

    push_op(ops, DiffOp::Equal(suffix));
}

/// Linear-space Myers middle snake: returns `(x, y, u, v)` where `a[x..u] == b[y..v]` is on an optimal edit path.
fn find_middle_snake(a: &[LineItem<'_>], b: &[LineItem<'_>]) -> Option<(usize, usize, usize, usize)> {
    let n = a.len() as isize;
    let m = b.len() as isize;
    let max_d = ((n + m + 1) / 2).min(MAX_MYERS_D as isize);
    let delta = n - m;
    let odd = (delta & 1) != 0;
    let offset = max_d + 1;
    let size = (2 * max_d + 3) as usize;
    let mut vf = vec![0isize; size];
    let mut vb = vec![0isize; size];

    for d in 0..=max_d {
        // Forward step.
        let mut k = -d;
        while k <= d {
            let idx = (k + offset) as usize;
            let mut x = if k == -d || (k != d && vf[idx - 1] < vf[idx + 1]) { vf[idx + 1] } else { vf[idx - 1] + 1 };
            let mut y = x - k;
            let (x0, y0) = (x, y);
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            vf[idx] = x;
            let kb = delta - k;
            if odd && kb.abs() < d && x <= n && y <= m && x0 >= 0 && y0 >= 0 {
                let b_idx = (kb + offset) as usize;
                let x_rev = vb[b_idx];
                let y_rev = x_rev - kb;
                if x_rev >= 0 && y_rev >= 0 && x_rev <= n && y_rev <= m && x >= n - x_rev {
                    return Some((x0 as usize, y0 as usize, x as usize, y as usize));
                }
            }
            k += 2;
        }

        // Backward step.
        let mut k = -d;
        while k <= d {
            let idx = (k + offset) as usize;
            let mut x_rev = if k == -d || (k != d && vb[idx - 1] < vb[idx + 1]) { vb[idx + 1] } else { vb[idx - 1] + 1 };
            let mut y_rev = x_rev - k;
            let (u, v) = (n - x_rev, m - y_rev);
            while x_rev < n && y_rev < m && a[(n - 1 - x_rev) as usize] == b[(m - 1 - y_rev) as usize] {
                x_rev += 1;
                y_rev += 1;
            }
            vb[idx] = x_rev;
            let kf = delta - k;
            if !odd && kf.abs() <= d && x_rev <= n && y_rev <= m && u >= 0 && v >= 0 {
                let f_idx = (kf + offset) as usize;
                let fx = vf[f_idx];
                let fy = fx - kf;
                if fx >= 0 && fy >= 0 && fx <= n && fy <= m && fx >= n - x_rev {
                    return Some(((n - x_rev) as usize, (m - y_rev) as usize, u as usize, v as usize));
                }
            }
            k += 2;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::{DEFS, Registry};
    use iron_sheets_core::config::AppConfig;

    struct NoWake;
    impl Waker for NoWake {
        fn wake(&self) {}
    }

    fn apply_ops_to_lines<'a>(a: &[&'a str], b: &[&'a str], ops: &[DiffOp]) -> Vec<&'a str> {
        let mut out = Vec::new();
        let mut ia = 0usize;
        let mut ib = 0usize;
        for &op in ops {
            match op {
                DiffOp::Equal(n) => {
                    for _ in 0..n {
                        assert_eq!(a[ia], b[ib]);
                        out.push(a[ia]);
                        ia += 1;
                        ib += 1;
                    }
                }
                DiffOp::Delete(n) => {
                    ia += n;
                }
                DiffOp::Insert(n) => {
                    for _ in 0..n {
                        out.push(b[ib]);
                        ib += 1;
                    }
                }
            }
        }
        assert_eq!(ia, a.len());
        assert_eq!(ib, b.len());
        out
    }

    #[test]
    fn porcelain_v2_parses_branch_unborn_detached_renames_conflicts_and_ignored() {
        let repo = Path::new("/tmp/repo");
        let gd = Path::new("/tmp/repo/.git");
        let mut raw = Vec::new();
        for tok in [
            "# branch.oid 0123456789abcdef0123456789abcdef01234567",
            "# branch.head main",
            "# branch.upstream origin/main",
            "# branch.ab +2 -1",
            "1 .M N... 100644 100644 100644 111 111 notes/mod.md",
            "1 A. N... 000000 100644 100644 000 111 notes/added.md",
            "1 .D N... 100644 100644 000000 111 000 notes/del.md",
            "2 R. N... 100644 100644 100644 111 111 R100 notes/new name.md",
            "notes/old name.md",
            "u UU N... 100644 100644 100644 100644 111 222 333 conflict/both.md",
            "? untracked dir/",
            "! secret/",
            "! tmp/ignored.tmp",
        ] {
            raw.extend_from_slice(tok.as_bytes());
            raw.push(0);
        }
        let snap = parse_porcelain_v2(repo, gd, gd, &raw);
        assert_eq!(snap.branch_display(), "main");
        assert!(!snap.detached && !snap.unborn);
        assert_eq!((snap.upstream.as_deref(), snap.ahead, snap.behind, snap.sync_text().as_str()), (Some("origin/main"), 2, 1, "ahead 2, behind 1"));
        assert_eq!(snap.head_oid.as_deref(), Some("0123456789abcdef0123456789abcdef01234567"));
        // 4 tracked changes + 1 conflict + 1 untracked dir = 6; ignored excluded!
        assert_eq!(snap.dirty_count, 6);
        assert_eq!(snap.status_of(&repo.join("notes/mod.md")), Some(GitFileStatus::Modified));
        assert_eq!(snap.status_of(&repo.join("notes/added.md")), Some(GitFileStatus::Added));
        assert_eq!(snap.status_of(&repo.join("notes/del.md")), Some(GitFileStatus::Deleted));
        assert_eq!(snap.status_of(&repo.join("notes/new name.md")), Some(GitFileStatus::Modified));
        // Rename origPath must have been consumed, not parsed as a record.
        assert_eq!(snap.status_of(&repo.join("notes/old name.md")), None);
        assert_eq!(snap.status_of(&repo.join("conflict/both.md")), Some(GitFileStatus::Conflict));
        assert_eq!(snap.status_of(&repo.join("untracked dir/child.md")), Some(GitFileStatus::Untracked));
        assert_eq!(snap.status_of(&repo.join("secret/key.txt")), Some(GitFileStatus::Ignored));
        assert_eq!(snap.status_of(&repo.join("tmp/ignored.tmp")), Some(GitFileStatus::Ignored));
        // Folder bubble-up: conflict > modified, ignored excluded from dirs.
        assert_eq!(snap.status_of(&repo.join("notes")), Some(GitFileStatus::Modified));
        assert_eq!(snap.status_of(&repo.join("conflict")), Some(GitFileStatus::Conflict));
        assert_eq!(snap.status_of(repo), Some(GitFileStatus::Conflict));
        assert_eq!(snap.status_of(&repo.join("tmp")), None);

        // Unborn + detached HEAD cases.
        let mut unborn_raw = Vec::new();
        for tok in ["# branch.oid (initial)", "# branch.head main"] {
            unborn_raw.extend_from_slice(tok.as_bytes());
            unborn_raw.push(0);
        }
        let unborn = parse_porcelain_v2(repo, gd, gd, &unborn_raw);
        assert!(unborn.unborn && !unborn.detached && unborn.head_oid.is_none());
        assert_eq!(unborn.branch_display(), "main");

        let mut det_raw = Vec::new();
        for tok in ["# branch.oid abcdef1234567890", "# branch.head (detached)"] {
            det_raw.extend_from_slice(tok.as_bytes());
            det_raw.push(0);
        }
        let det = parse_porcelain_v2(repo, gd, gd, &det_raw);
        assert!(det.detached && !det.unborn);
        assert_eq!(det.branch_display(), "HEAD (abcdef1)");
    }

    #[test]
    fn line_diff_handles_adds_modifies_deletes_crlf_and_random_inputs() {
        assert!(diff_lines("a\nb\n", "a\nb\n").is_empty());
        assert_eq!(diff_lines("a\nb\n", "x\na\nb\n"), vec![DiffMark { start: 0, end: 1, old_start: 0, old_end: 0, kind: DiffKind::Added }]);
        assert_eq!(diff_lines("a\nb\nc", "a\nX\nc"), vec![DiffMark { start: 1, end: 2, old_start: 1, old_end: 2, kind: DiffKind::Modified }]);
        assert_eq!(diff_lines("a\nb\nc", "a\nc"), vec![DiffMark { start: 1, end: 1, old_start: 1, old_end: 2, kind: DiffKind::Deleted }]);
        assert_eq!(diff_lines("a\nb\nc", "b\nc"), vec![DiffMark { start: 0, end: 0, old_start: 0, old_end: 1, kind: DiffKind::Deleted }]);
        assert_eq!(diff_lines("a\nb", "a"), vec![DiffMark { start: 1, end: 1, old_start: 1, old_end: 2, kind: DiffKind::Deleted }]);

        // BOM + CRLF HEAD blob normalizes to match LF Doc::text.
        let head_norm = textio::decode_text_lf(b"\xEF\xBB\xBFone\r\ntwo\r\nthree\r\n".to_vec()).unwrap();
        assert!(diff_lines(&head_norm, "one\ntwo\nthree\n").is_empty());
        assert_eq!(diff_lines(&head_norm, "one\nTWO\nthree\n"), vec![DiffMark { start: 1, end: 2, old_start: 1, old_end: 2, kind: DiffKind::Modified }]);
        assert!(textio::decode_text_lf(b"abc\0def".to_vec()).is_none());

        // 100 random multi-line pairs verifying exact Myers reconstruction.
        let vocab = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta"];
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next_u32 = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 32) as u32
        };
        for _ in 0..500 {
            let la = (next_u32() % 40) as usize;
            let lb = (next_u32() % 40) as usize;
            let va: Vec<&str> = (0..la).map(|_| vocab[(next_u32() as usize) % vocab.len()]).collect();
            let vb: Vec<&str> = (0..lb).map(|_| vocab[(next_u32() as usize) % vocab.len()]).collect();
            let ia: Vec<LineItem<'_>> = va.iter().copied().map(LineItem::new).collect();
            let ib: Vec<LineItem<'_>> = vb.iter().copied().map(LineItem::new).collect();
            let mut ops = Vec::new();
            myers_diff(&ia, &ib, &mut ops);
            let reconstructed = apply_ops_to_lines(&va, &vb, &ops);
            assert_eq!(reconstructed, vb);
        }
    }

    #[test]
    fn path_normalization_and_derive_repo_root() {
        let root = Path::new("/tmp/symlink_ws/notes");
        let p = posix_rel_to_path(root, "sub/dir/file.md");
        assert_eq!(p, root.join("sub").join("dir").join("file.md"));
        assert_eq!(path_to_posix_rel(root, &p).as_deref(), Some("sub/dir/file.md"));

        // Non-matching parent with matching suffix must fall back to show_toplevel.
        let fallback = derive_repo_root(Path::new("/tmp/non_matching_ws/sub/notes"), "/real/target/repo", "sub/notes/");
        assert_eq!(fallback, PathBuf::from("/real/target/repo"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let tmp = tempfile::tempdir().unwrap();
            let real_repo = tmp.path().join("real_repo");
            std::fs::create_dir_all(real_repo.join("sub/notes")).unwrap();
            let link_parent = tmp.path().join("symlink_ws");
            symlink(&real_repo, &link_parent).unwrap();
            let real_str = real_repo.to_string_lossy();

            // Symlinked parent resolving to show_toplevel keeps the symlinked repo root.
            let derived = derive_repo_root(&link_parent.join("sub/notes"), &real_str, "sub/notes/");
            assert_eq!(derived, normalize_path(&link_parent));

            // Direct symlink to repo root (empty show_prefix) keeps ws_norm cleanly.
            let direct = derive_repo_root(&link_parent, &real_str, "");
            assert_eq!(direct, normalize_path(&link_parent));

            // Same-name symlinked subdir (`outside/sub1 -> ../real_repo/sub1`) must fall back to real_repo.
            let outside = tmp.path().join("outside");
            std::fs::create_dir_all(&outside).unwrap();
            let outside_sub = outside.join("notes");
            symlink(real_repo.join("sub/notes"), &outside_sub).unwrap();
            let same_leaf = derive_repo_root(&outside_sub, &real_str, "sub/notes/");
            assert_eq!(same_leaf, normalize_path(&real_repo));
        }
    }

    #[test]
    fn disabled_module_costs_nothing_and_config_defaults_true() {
        assert!(AppConfig::default().module_git);
        let (legacy, _) = AppConfig::from_json(b"{}", "test");
        assert!(legacy.module_git);

        let waker: Arc<dyn Waker> = Arc::new(NoWake);
        let mut cmds = Vec::new();
        let mut cx = ModuleCx { waker: &waker, commands: &mut cmds };
        let mut reg = Registry::default();
        let cfg = AppConfig { module_kanban: false, module_codehl: false, module_ligatures: false, module_git: false, module_calendar: false, ..Default::default() };
        assert!(!reg.sync(DEFS, &cfg, &mut cx));
        assert!(!reg.any());
        assert_eq!(reg.in_flight(), 0);
    }

    #[test]
    fn real_temp_repo_status_diff_commit_and_missing_git() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let non_git = tmp.path().join("plain");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&non_git).unwrap();

        let run_git = |args: &[&str]| {
            let out = Command::new("git").current_dir(&repo).args(args).output().unwrap();
            assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
        };
        run_git(&["init", "-b", "main"]);
        run_git(&["config", "user.email", "test@example.com"]);
        run_git(&["config", "user.name", "Test"]);
        let note = repo.join("note.md");
        std::fs::write(&note, "line 1\nline 2\nline 3\n").unwrap();
        run_git(&["add", "note.md"]);
        run_git(&["commit", "-m", "init"]);

        std::fs::write(&note, "line 1\nline 2 modified\nline 3\nline 4\n").unwrap();
        let untracked = repo.join("new.md");
        std::fs::write(&untracked, "hello\n").unwrap();

        let waker: Arc<dyn Waker> = Arc::new(NoWake);
        let mut cmds = Vec::new();
        let mut cx = ModuleCx { waker: &waker, commands: &mut cmds };
        let mut m = GitModule::new();
        m.on_workspace_roots(&[repo.clone(), non_git.clone()]);
        m.start(&mut cx);

        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(5) {
            m.poll();
            if m.snapshot().availability == GitAvailability::Ready && m.snapshot().repos.first().is_some_and(|r| r.dirty_count == 2) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let snap = m.snapshot();
        assert_eq!(snap.availability, GitAvailability::Ready);
        assert_eq!(snap.repos.len(), 1);
        assert_eq!(snap.repos[0].branch, "main");
        assert_eq!(snap.status_of(&note), Some(GitFileStatus::Modified));
        assert_eq!(snap.status_of(&untracked), Some(GitFileStatus::Untracked));
        assert_eq!(snap.status_of(&non_git), None);

        // Diff marks against HEAD.
        let past = Instant::now() - Duration::from_secs(1);
        let txt = std::fs::read_to_string(&note).unwrap();
        let _ = m.diff_marks(&note, 1, txt.len(), false, past, || txt.clone());
        let t1 = Instant::now();
        let mut got_marks = Arc::from([]) as Arc<[DiffMark]>;
        while t1.elapsed() < Duration::from_secs(5) {
            if m.poll() {
                let (marks, _) = m.diff_marks(&note, 1, txt.len(), false, past, || txt.clone());
                if !marks.is_empty() {
                    got_marks = marks;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(&*got_marks, &[DiffMark { start: 1, end: 2, old_start: 1, old_end: 2, kind: DiffKind::Modified }, DiffMark { start: 3, end: 4, old_start: 3, old_end: 3, kind: DiffKind::Added },]);
        assert_eq!(m.in_flight(), 0);

        // External commit refreshes via .git metadata polling.
        run_git(&["add", "-A"]);
        run_git(&["commit", "-m", "second"]);
        let t2 = Instant::now();
        while t2.elapsed() < Duration::from_secs(5) {
            m.poll();
            if m.snapshot().repos[0].dirty_count == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(m.snapshot().repos[0].dirty_count, 0);

        // Missing git binary degrades cleanly to Unavailable without panics.
        m.set_git_bin_override(Some("/nonexistent/git-binary-for-test".into()));
        let t3 = Instant::now();
        while t3.elapsed() < Duration::from_secs(5) {
            m.poll();
            if m.snapshot().availability == GitAvailability::Unavailable {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(m.snapshot().availability, GitAvailability::Unavailable);
    }

    #[test]
    fn save_after_edit_preserves_diff_marks_until_scan_finishes_and_clears_on_revert() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let run_git = |args: &[&str]| {
            let out = Command::new("git").current_dir(&repo).args(args).output().unwrap();
            assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
        };
        run_git(&["init", "-b", "main"]);
        run_git(&["config", "user.email", "test@example.com"]);
        run_git(&["config", "user.name", "Test"]);
        let note = repo.join("note.md");
        let orig = "line 1\nline 2\n";
        let edited = "line 1\nline 2 edited\n";
        std::fs::write(&note, orig).unwrap();
        run_git(&["add", "note.md"]);
        run_git(&["commit", "-m", "init"]);

        let waker: Arc<dyn Waker> = Arc::new(NoWake);
        let mut cmds = Vec::new();
        let mut cx = ModuleCx { waker: &waker, commands: &mut cmds };
        let mut m = GitModule::new();
        m.on_workspace_roots(std::slice::from_ref(&repo));
        m.start(&mut cx);

        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(5) {
            m.poll();
            if m.snapshot().availability == GitAvailability::Ready {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(m.snapshot().availability, GitAvailability::Ready);
        assert_eq!(m.snapshot().status_of(&note), None);

        // Cache diff marks while dirty in memory (`status` is still None).
        let past = Instant::now() - Duration::from_secs(1);
        let _ = m.diff_marks(&note, 1, edited.len(), true, past, || edited.into());
        let t1 = Instant::now();
        let mut cached = Arc::from([]) as Arc<[DiffMark]>;
        while t1.elapsed() < Duration::from_secs(5) {
            if m.poll() {
                let (marks, _) = m.diff_marks(&note, 1, edited.len(), true, past, || edited.into());
                if !marks.is_empty() {
                    cached = marks;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(&*cached, &[DiffMark { start: 1, end: 2, old_start: 1, old_end: 2, kind: DiffKind::Modified }]);

        // Save to disk -> `is_dirty = false` before 120ms scan finishes: cached marks must remain intact!
        std::fs::write(&note, edited).unwrap();
        m.on_save(&note);
        let (immediate_marks, _) = m.diff_marks(&note, 1, edited.len(), false, past, || edited.into());
        assert_eq!(&*immediate_marks, &[DiffMark { start: 1, end: 2, old_start: 1, old_end: 2, kind: DiffKind::Modified }]);

        // Wait for post-save scan to finish (`status` becomes Modified).
        let t2 = Instant::now();
        while t2.elapsed() < Duration::from_secs(5) {
            m.poll();
            if m.snapshot().status_of(&note) == Some(GitFileStatus::Modified) && !m.scan_pending() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(m.snapshot().status_of(&note), Some(GitFileStatus::Modified));
        let (post_scan_marks, _) = m.diff_marks(&note, 1, edited.len(), false, past, || edited.into());
        assert_eq!(&*post_scan_marks, &[DiffMark { start: 1, end: 2, old_start: 1, old_end: 2, kind: DiffKind::Modified }]);

        // Revert file back to HEAD on disk and save -> once scan finishes, marks clear to [].
        std::fs::write(&note, orig).unwrap();
        m.on_save(&note);
        let t3 = Instant::now();
        while t3.elapsed() < Duration::from_secs(5) {
            m.poll();
            if m.snapshot().status_of(&note).is_none() && !m.scan_pending() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(m.snapshot().status_of(&note), None);
        let (reverted_marks, _) = m.diff_marks(&note, 2, orig.len(), false, past, || orig.into());
        assert!(reverted_marks.is_empty());

        // Also verify `ScanAck` when `snap == last_snap` (clean status unchanged, cached marks cleared).
        let _ = m.diff_marks(&note, 3, edited.len(), true, past, || edited.into());
        let t4 = Instant::now();
        while t4.elapsed() < Duration::from_secs(5) {
            if m.poll() {
                let (marks, _) = m.diff_marks(&note, 3, edited.len(), true, past, || edited.into());
                if !marks.is_empty() {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        m.on_save(&note); // Disk already equals HEAD, so scan returns identical clean snapshot (`ScanAck`).
        let mut ack_changed = false;
        let t5 = Instant::now();
        while t5.elapsed() < Duration::from_secs(5) {
            if m.poll() {
                ack_changed = true;
            }
            if !m.scan_pending() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ack_changed, "ScanAck must report changed=true when stale diff cache entry needs clearing");
        let (cleared_marks, _) = m.diff_marks(&note, 3, orig.len(), false, past, || orig.into());
        assert!(cleared_marks.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_subdir_workspace_roots_and_loop_bound() {
        use std::os::unix::fs::symlink;
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();

        // Symlink loop must terminate cleanly within MAX_SYMLINK_HOPS without hanging.
        let loop_a = base.join("loop_a");
        let loop_b = base.join("loop_b");
        symlink("loop_b", &loop_a).unwrap();
        symlink("loop_a", &loop_b).unwrap();
        let _ = resolve_symlinks(&loop_a);

        // Real repo with multiple subdirectories.
        let repo_a = base.join("repoA");
        let sub1 = repo_a.join("sub1");
        let sub2 = repo_a.join("sub2");
        let deep_notes = repo_a.join("deep/notes");
        let secret = sub1.join("secret");
        std::fs::create_dir_all(&secret).unwrap();
        std::fs::create_dir_all(&sub2).unwrap();
        std::fs::create_dir_all(&deep_notes).unwrap();

        let run_git = |args: &[&str]| {
            let out = Command::new("git").current_dir(&repo_a).args(args).output().unwrap();
            assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
        };
        run_git(&["init", "-b", "main"]);
        run_git(&["config", "user.email", "test@example.com"]);
        run_git(&["config", "user.name", "Test"]);
        std::fs::write(repo_a.join(".gitignore"), "secret/\n").unwrap();
        std::fs::write(sub1.join("a.md"), "one\ntwo\n").unwrap();
        std::fs::write(sub2.join("b.md"), "alpha\nbeta\n").unwrap();
        std::fs::write(deep_notes.join("n.md"), "deep1\ndeep2\n").unwrap();
        run_git(&["add", "."]);
        run_git(&["commit", "-m", "init"]);

        let edited_a = "one\nTWO\n";
        let edited_n = "deep1\nDEEP2\n";
        std::fs::write(sub1.join("a.md"), edited_a).unwrap();
        std::fs::write(sub2.join("b.md"), "alpha\nBETA\n").unwrap();
        std::fs::write(deep_notes.join("n.md"), edited_n).unwrap();
        std::fs::write(secret.join("k.md"), "ignored").unwrap();

        // Relative symlink + chained symlink to `repoA/sub1`.
        let link_rel = base.join("link_rel");
        let link_chain = base.join("link_chain");
        symlink(Path::new("repoA/sub1"), &link_rel).unwrap();
        symlink(&link_rel, &link_chain).unwrap();

        // Same leaf name symlink (`outside/sub1 -> ../repoA/sub1`) and multi-component same suffix (`outside2/deep/notes -> ../../repoA/deep/notes`).
        let outside = base.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let outside_sub1 = outside.join("sub1");
        symlink(Path::new("../repoA/sub1"), &outside_sub1).unwrap();

        let outside2_deep = base.join("outside2/deep");
        std::fs::create_dir_all(&outside2_deep).unwrap();
        let outside2_notes = outside2_deep.join("notes");
        symlink(Path::new("../../repoA/deep/notes"), &outside2_notes).unwrap();

        let waker: Arc<dyn Waker> = Arc::new(NoWake);
        let mut cmds = Vec::new();
        let mut cx = ModuleCx { waker: &waker, commands: &mut cmds };
        let mut m = GitModule::new();
        let roots = vec![link_chain.clone(), outside_sub1.clone(), outside2_notes.clone(), sub2.clone()];
        m.on_workspace_roots(&roots);
        m.start(&mut cx);

        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(5) {
            m.poll();
            if m.snapshot().availability == GitAvailability::Ready {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let snap = m.snapshot();
        assert_eq!(snap.availability, GitAvailability::Ready);
        assert_eq!(snap.repos.len(), 1, "all symlinked subdirs and sibling subdir must deduplicate to 1 RepoSnapshot");
        assert_eq!(snap.status_of(&link_chain.join("a.md")), Some(GitFileStatus::Modified));
        assert_eq!(snap.status_of(&link_chain), Some(GitFileStatus::Modified));
        assert_eq!(snap.status_of(&link_chain.join("secret/k.md")), Some(GitFileStatus::Ignored));
        assert_eq!(snap.status_of(&outside_sub1.join("a.md")), Some(GitFileStatus::Modified));
        assert_eq!(snap.status_of(&outside_sub1), Some(GitFileStatus::Modified));
        assert_eq!(snap.status_of(&outside_sub1.join("secret/k.md")), Some(GitFileStatus::Ignored));
        assert_eq!(snap.status_of(&outside2_notes.join("n.md")), Some(GitFileStatus::Modified));
        assert_eq!(snap.status_of(&outside2_notes), Some(GitFileStatus::Modified));
        assert_eq!(snap.status_of(&sub2.join("b.md")), Some(GitFileStatus::Modified));
        assert!(snap.active_repo(Some(&link_chain.join("a.md")), &roots).is_some());
        assert!(snap.active_repo(Some(&outside_sub1.join("a.md")), &roots).is_some());
        assert!(snap.active_repo(Some(&outside2_notes.join("n.md")), &roots).is_some());

        // Diff marks through chained, same-leaf, and multi-component same-suffix symlinked workspace roots.
        let past = Instant::now() - Duration::from_secs(1);
        for (path, edited) in [
            (link_chain.join("a.md"), edited_a),
            (outside_sub1.join("a.md"), edited_a),
            (outside2_notes.join("n.md"), edited_n),
        ] {
            let _ = m.diff_marks(&path, 1, edited.len(), false, past, || edited.into());
            let t1 = Instant::now();
            let mut got = Arc::from([]) as Arc<[DiffMark]>;
            while t1.elapsed() < Duration::from_secs(5) {
                if m.poll() {
                    let (marks, _) = m.diff_marks(&path, 1, edited.len(), false, past, || edited.into());
                    if !marks.is_empty() {
                        got = marks;
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(&*got, &[DiffMark { start: 1, end: 2, old_start: 1, old_end: 2, kind: DiffKind::Modified }], "diff_marks failed for {}", path.display());
        }

        // Direct symlink to repo root (`show_prefix = ""`) keeps ws_norm cleanly and reports Ready + status + diff_marks.
        let link_to_repo = base.join("link_to_repo");
        symlink(Path::new("repoA"), &link_to_repo).unwrap();
        let mut m2 = GitModule::new();
        m2.on_workspace_roots(std::slice::from_ref(&link_to_repo));
        m2.start(&mut cx);
        let t2 = Instant::now();
        while t2.elapsed() < Duration::from_secs(5) {
            m2.poll();
            if m2.snapshot().availability == GitAvailability::Ready {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let snap2 = m2.snapshot();
        assert_eq!(snap2.availability, GitAvailability::Ready);
        assert_eq!(snap2.repos.len(), 1);
        assert_eq!(snap2.repos[0].repo_root, normalize_path(&link_to_repo));
        let a_via_repo_link = link_to_repo.join("sub1/a.md");
        assert_eq!(snap2.status_of(&a_via_repo_link), Some(GitFileStatus::Modified));
        let _ = m2.diff_marks(&a_via_repo_link, 1, edited_a.len(), false, past, || edited_a.into());
        let t3 = Instant::now();
        let mut got2 = Arc::from([]) as Arc<[DiffMark]>;
        while t3.elapsed() < Duration::from_secs(5) {
            if m2.poll() {
                let (marks, _) = m2.diff_marks(&a_via_repo_link, 1, edited_a.len(), false, past, || edited_a.into());
                if !marks.is_empty() {
                    got2 = marks;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(&*got2, &[DiffMark { start: 1, end: 2, old_start: 1, old_end: 2, kind: DiffKind::Modified }]);
    }

    /// A root's own `.git/config` must not make a scan or diff run commands (fsmonitor hook, clean/process filters).
    #[cfg(unix)]
    #[test]
    fn repo_config_cannot_execute_commands_during_scan_or_diff() {
        use std::os::unix::fs::PermissionsExt;
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let run_git = |args: &[&str]| {
            let out = Command::new("git").current_dir(&repo).args(args).output().unwrap();
            assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
        };
        run_git(&["init", "-b", "main"]);
        run_git(&["config", "user.email", "test@example.com"]);
        run_git(&["config", "user.name", "Test"]);
        let note = repo.join("note.md");
        std::fs::write(&note, "a\n").unwrap();
        run_git(&["add", "note.md"]);
        run_git(&["commit", "-m", "init"]);

        let marker = tmp.path().join("pwned");
        let script = tmp.path().join("evil.sh");
        std::fs::write(&script, format!("#!/bin/sh\ntouch '{}'\ncat\n", marker.display())).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let s = script.to_str().unwrap();
        run_git(&["config", "core.fsmonitor", s]);
        run_git(&["config", "filter.evil.clean", s]);
        run_git(&["config", "filter.evil.required", "true"]);
        std::fs::write(repo.join(".git/info/attributes"), "* filter=evil\n").unwrap();
        // Same size, new mtime: status must re-hash the file, which is where a clean filter would run.
        let f = std::fs::OpenOptions::new().write(true).open(&note).unwrap();
        f.set_modified(SystemTime::now() + Duration::from_secs(5)).unwrap();
        drop(f);

        let mut latched = false;
        let snap = scan_roots("git", std::slice::from_ref(&repo), &[], None, &mut latched);
        assert_eq!(snap.availability, GitAvailability::Ready);
        let head = snap.repos[0].head_oid.clone();
        let _ = compute_diff_for_file("git", &repo, "note.md", head.as_deref(), false, "b\n");
        assert!(!marker.exists(), "a command from the repo's own config ran");
    }

    /// A hung git is killed after `GIT_TIMEOUT`, and dropping the module (Git off, quit) never waits for it.
    #[cfg(unix)]
    #[test]
    fn hung_git_times_out_and_drop_does_not_block() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let fake = tmp.path().join("git");
        std::fs::write(&fake, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let t0 = Instant::now();
        let err = output(&mut Command::new(&fake)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(t0.elapsed() < Duration::from_secs(5));

        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let waker: Arc<dyn Waker> = Arc::new(NoWake);
        let mut cmds = Vec::new();
        let mut cx = ModuleCx { waker: &waker, commands: &mut cmds };
        let mut m = GitModule::new();
        m.set_git_bin_override(Some(fake.to_string_lossy().into_owned()));
        m.on_workspace_roots(std::slice::from_ref(&repo));
        m.start(&mut cx);
        let t1 = Instant::now();
        while m.in_flight() == 0 && t1.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(m.in_flight() > 0, "scan should be running the fake git");
        let t2 = Instant::now();
        drop(m);
        assert!(t2.elapsed() < Duration::from_millis(1000), "drop must not wait for the 2 s git timeout");
    }

    #[test]
    fn head_blob_over_max_diff_bytes_skips_diff() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let run_git = |args: &[&str]| {
            let out = Command::new("git").current_dir(&repo).args(args).output().unwrap();
            assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
        };
        run_git(&["init", "-b", "main"]);
        run_git(&["config", "user.email", "test@example.com"]);
        run_git(&["config", "user.name", "Test"]);

        let big = repo.join("big.md");
        let big_content = "0123456789abcdef0123456789abcdef\n".repeat((MAX_DIFF_BYTES / 33) + 100);
        assert!(big_content.len() > MAX_DIFF_BYTES);
        std::fs::write(&big, &big_content).unwrap();
        run_git(&["add", "big.md"]);
        run_git(&["commit", "-m", "big"]);

        let small = "small\n";
        std::fs::write(&big, small).unwrap();

        let waker: Arc<dyn Waker> = Arc::new(NoWake);
        let mut cmds = Vec::new();
        let mut cx = ModuleCx { waker: &waker, commands: &mut cmds };
        let mut m = GitModule::new();
        m.on_workspace_roots(std::slice::from_ref(&repo));
        m.start(&mut cx);

        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(5) {
            m.poll();
            if m.snapshot().availability == GitAvailability::Ready {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(m.snapshot().status_of(&big), Some(GitFileStatus::Modified));

        let past = Instant::now() - Duration::from_secs(1);
        let _ = m.diff_marks(&big, 1, small.len(), false, past, || small.into());
        let t1 = Instant::now();
        while t1.elapsed() < Duration::from_secs(5) {
            m.poll();
            if m.in_flight() == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let (marks, _) = m.diff_marks(&big, 1, small.len(), false, past, || small.into());
        assert!(marks.is_empty(), ">2 MiB HEAD blob must skip diff even when buffer is small");
    }

    /// A `git` that ignores the user's real config, so identity and hooks in these tests are what the test sets.
    #[cfg(unix)]
    fn isolated_git(dir: &Path) -> Option<String> {
        use std::os::unix::fs::PermissionsExt;
        Command::new("git").arg("--version").output().ok()?;
        let wrapper = dir.join("git-isolated");
        let script = "#!/bin/sh\nexport GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 XDG_CONFIG_HOME=/nonexistent HOME=/nonexistent\n\
                      unset GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL EMAIL\nexec git \"$@\"\n";
        std::fs::write(&wrapper, script).ok()?;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).ok()?;
        // A parallel test forking while the script was open for writing makes the first exec fail with "Text file busy".
        for _ in 0..100 {
            match Command::new(&wrapper).arg("--version").output() {
                Err(e) if e.kind() == io::ErrorKind::ExecutableFileBusy => std::thread::sleep(Duration::from_millis(10)),
                _ => break,
            }
        }
        Some(wrapper.to_string_lossy().into_owned())
    }

    #[cfg(unix)]
    fn g(bin: &str, dir: &Path, args: &[&str]) -> String {
        let out = Command::new(bin).current_dir(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[cfg(unix)]
    fn new_repo(bin: &str, path: &Path, identity: bool) {
        std::fs::create_dir_all(path).unwrap();
        g(bin, path, &["init", "-b", "main"]);
        if identity {
            g(bin, path, &["config", "user.name", "Test"]);
            g(bin, path, &["config", "user.email", "test@example.com"]);
        }
    }

    #[cfg(unix)]
    fn changes(bin: &str, repo: &Path) -> Vec<(String, bool, bool)> {
        let mut latched = false;
        let snap = scan_roots(bin, std::slice::from_ref(&repo.to_path_buf()), &[], None, &mut latched);
        snap.repos[0].changes.iter().map(|c| (c.rel.clone(), c.staged(), c.unstaged())).collect()
    }

    #[cfg(unix)]
    #[test]
    fn stage_unstage_commit_on_an_unborn_repo_and_odd_names() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(bin) = isolated_git(tmp.path()) else { return };
        let repo = tmp.path().join("repo");
        new_repo(&bin, &repo, true);
        std::fs::write(repo.join("[x] a.md"), "one\n").unwrap();
        std::fs::write(repo.join("b.md"), "two\n").unwrap();

        assert_eq!(changes(&bin, &repo), [("[x] a.md".into(), false, true), ("b.md".into(), false, true)]);
        run_op(&bin, &repo, &GitOp::Stage("[x] a.md".into())).unwrap();
        assert_eq!(changes(&bin, &repo)[0], ("[x] a.md".into(), true, false), "a literal pathspec, not a glob");
        run_op(&bin, &repo, &GitOp::Unstage("[x] a.md".into())).unwrap();
        assert_eq!(changes(&bin, &repo)[0], ("[x] a.md".into(), false, true), "unstaging works before the first commit");

        run_op(&bin, &repo, &GitOp::StageAll).unwrap();
        let line = run_op(&bin, &repo, &GitOp::Commit("-first\n\nbody".into())).unwrap();
        assert!(line.starts_with("[main"), "{line}");
        assert_eq!(g(&bin, &repo, &["log", "-1", "--format=%B"]).trim(), "-first\n\nbody");
        assert!(changes(&bin, &repo).is_empty());
        assert_eq!(run_op(&bin, &repo, &GitOp::Commit("again".into())), Err("Nothing staged to commit".into()));

        // Staged and unstaged at once (MM) shows on both sides.
        std::fs::write(repo.join("b.md"), "two\nthree\n").unwrap();
        run_op(&bin, &repo, &GitOp::Stage("b.md".into())).unwrap();
        std::fs::write(repo.join("b.md"), "two\nthree\nfour\n").unwrap();
        assert_eq!(changes(&bin, &repo), [("b.md".into(), true, true)]);
    }

    #[cfg(unix)]
    #[test]
    fn commit_is_refused_without_identity_and_never_runs_repo_hooks() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let Some(bin) = isolated_git(tmp.path()) else { return };
        let repo = tmp.path().join("repo");
        new_repo(&bin, &repo, false);
        std::fs::write(repo.join("a.md"), "x\n").unwrap();
        run_op(&bin, &repo, &GitOp::StageAll).unwrap();
        let err = run_op(&bin, &repo, &GitOp::Commit("msg".into())).unwrap_err();
        assert!(err.contains("user.name") && err.contains("git config --global"), "{err}");
        assert!(Command::new(&bin).current_dir(&repo).args(["rev-parse", "--verify", "HEAD"]).output().unwrap().status.code() != Some(0), "nothing was committed");
        let cfg = Command::new(&bin).current_dir(&repo).args(["config", "--get", "user.name"]).output().unwrap();
        assert!(cfg.stdout.is_empty(), "we never set the identity");

        g(&bin, &repo, &["config", "user.name", "T"]);
        let err = run_op(&bin, &repo, &GitOp::Commit("msg".into())).unwrap_err();
        assert!(err.contains("user.email"), "{err}");
        g(&bin, &repo, &["config", "user.email", "t@example.com"]);
        let marker = tmp.path().join("hook-ran");
        let hook = repo.join(".git/hooks/pre-commit");
        std::fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        run_op(&bin, &repo, &GitOp::Commit("msg".into())).unwrap();
        assert!(!marker.exists(), "a hook from the repo ran");
    }

    #[cfg(unix)]
    #[test]
    fn staging_refuses_a_clean_filter_from_the_repo_config() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(bin) = isolated_git(tmp.path()) else { return };
        let repo = tmp.path().join("repo");
        new_repo(&bin, &repo, true);
        std::fs::write(repo.join("a.md"), "x\n").unwrap();
        g(&bin, &repo, &["config", "filter.evil.clean", "touch pwned; cat"]);
        let err = run_op(&bin, &repo, &GitOp::Stage("a.md".into())).unwrap_err();
        assert!(err.contains("filter.evil.clean"), "{err}");
        assert!(run_op(&bin, &repo, &GitOp::StageAll).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn push_and_pull_against_a_local_bare_repo_report_git_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(bin) = isolated_git(tmp.path()) else { return };
        let bare = tmp.path().join("remote.git");
        std::fs::create_dir_all(&bare).unwrap();
        g(&bin, &bare, &["init", "--bare", "-b", "main"]);
        let a = tmp.path().join("a");
        new_repo(&bin, &a, true);
        std::fs::write(a.join("n.md"), "1\n").unwrap();
        run_op(&bin, &a, &GitOp::StageAll).unwrap();
        run_op(&bin, &a, &GitOp::Commit("one".into())).unwrap();
        g(&bin, &a, &["remote", "add", "origin", bare.to_str().unwrap()]);
        let err = run_op(&bin, &a, &GitOp::Push).unwrap_err();
        assert!(err.starts_with("fatal:") && err.contains("upstream"), "first error line of git: {err}");
        g(&bin, &a, &["push", "-u", "origin", "main"]);

        let b = tmp.path().join("b");
        g(&bin, tmp.path(), &["clone", bare.to_str().unwrap(), b.to_str().unwrap()]);
        g(&bin, &b, &["config", "user.name", "B"]);
        g(&bin, &b, &["config", "user.email", "b@example.com"]);
        std::fs::write(a.join("n.md"), "1\n2\n").unwrap();
        run_op(&bin, &a, &GitOp::StageAll).unwrap();
        run_op(&bin, &a, &GitOp::Commit("two".into())).unwrap();
        assert_eq!(run_op(&bin, &a, &GitOp::Push), Ok("Pushed".into()));
        run_op(&bin, &b, &GitOp::Pull).unwrap();
        assert_eq!(std::fs::read_to_string(b.join("n.md")).unwrap(), "1\n2\n");
        assert_eq!(run_op(&bin, &b, &GitOp::Pull), Ok("Already up to date.".into()));

        // Diverged: `--ff-only` refuses and git's own line comes back.
        std::fs::write(a.join("a-only.md"), "a\n").unwrap();
        run_op(&bin, &a, &GitOp::StageAll).unwrap();
        run_op(&bin, &a, &GitOp::Commit("a".into())).unwrap();
        run_op(&bin, &a, &GitOp::Push).unwrap();
        std::fs::write(b.join("b-only.md"), "b\n").unwrap();
        run_op(&bin, &b, &GitOp::StageAll).unwrap();
        run_op(&bin, &b, &GitOp::Commit("b".into())).unwrap();
        let err = run_op(&bin, &b, &GitOp::Pull).unwrap_err();
        assert!(err.starts_with("fatal:") && err.contains("fast-forward"), "{err}");
        let err = run_op(&bin, &b, &GitOp::Push).unwrap_err();
        assert!(err.starts_with("error:") && err.contains("failed to push"), "{err}");

        // A remote that cannot be reached fails with a line, never a hang or a prompt.
        g(&bin, &b, &["remote", "set-url", "origin", tmp.path().join("missing.git").to_str().unwrap()]);
        assert!(run_op(&bin, &b, &GitOp::Pull).unwrap_err().starts_with("fatal:"));
    }

    #[cfg(unix)]
    #[test]
    fn module_runs_ops_on_the_worker_one_long_op_at_a_time() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(bin) = isolated_git(tmp.path()) else { return };
        let repo = tmp.path().join("repo");
        new_repo(&bin, &repo, true);
        let note = repo.join("n.md");
        std::fs::write(&note, "x\n").unwrap();
        let waker: Arc<dyn Waker> = Arc::new(NoWake);
        let mut cmds = Vec::new();
        let mut cx = ModuleCx { waker: &waker, commands: &mut cmds };
        let mut m = GitModule::new();
        m.set_git_bin_override(Some(bin.clone()));
        m.on_workspace_roots(std::slice::from_ref(&repo));
        m.start(&mut cx);
        let wait = |m: &mut GitModule, f: &dyn Fn(&GitModule) -> bool| {
            let t0 = Instant::now();
            while !f(m) && t0.elapsed() < Duration::from_secs(10) {
                m.poll();
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(f(m), "timeout");
        };
        wait(&mut m, &|m| m.snapshot().availability == GitAvailability::Ready);

        m.on_command("git.commit", &format!("{}\0   ", repo.display()), &mut cx);
        assert_eq!(m.last_outcome().unwrap().result, Err("Enter a commit message first".into()));
        assert!(m.busy(&repo).is_none() && m.in_flight() == 0, "nothing was sent");

        m.on_command("git.stage", &note.to_string_lossy(), &mut cx);
        wait(&mut m, &|m| m.snapshot().repos[0].changes.first().is_some_and(|c| c.staged()));
        m.on_command("git.commit", &format!("{}\0first", repo.display()), &mut cx);
        assert_eq!(m.busy(&repo), Some("Commit"));
        for k in ["git.push", "git.stage", "git.stage_all"] {
            m.on_command(k, &note.to_string_lossy(), &mut cx);
            assert_eq!(m.last_outcome().unwrap().result, Err("Wait for the running Commit to finish".into()), "{k} on a repo with a commit running");
        }
        wait(&mut m, &|m| m.busy(&repo).is_none());
        let o = m.last_outcome().unwrap().clone();
        assert_eq!((o.op, o.result.is_ok()), ("Commit", true));
        wait(&mut m, &|m| m.snapshot().repos[0].changes.is_empty() && m.in_flight() == 0);
        assert!(!m.unreported.is_empty());
        // Discard only touches what the snapshot lists as changed.
        m.on_command("git.discard", &note.to_string_lossy(), &mut cx);
        assert_eq!(m.last_outcome().unwrap().result, Err("That file has no changes to discard".into()));

        // No remote: push fails with git's line, busy clears.
        m.on_command("git.push", &repo.to_string_lossy(), &mut cx);
        wait(&mut m, &|m| m.busy(&repo).is_none());
        assert!(m.last_outcome().unwrap().result.as_ref().is_err_and(|e| e.starts_with("fatal:")));

        // Never a guessed repo: an empty path or one outside every repo is refused.
        m.on_command("git.push", "", &mut cx);
        assert_eq!(m.last_outcome().unwrap().result, Err("Open a note inside a repository first".into()));
        m.on_command("git.stage", &tmp.path().join("elsewhere/x.md").to_string_lossy(), &mut cx);
        assert_eq!(m.last_outcome().unwrap().result, Err("This folder is not in a Git repository".into()));
        assert_eq!(m.in_flight(), 0);

        // The hover URL of Push and Pull, without the credentials in it.
        g(&bin, &repo, &["remote", "add", "origin", "https://user:secret@example.com/x.git"]);
        m.on_command("git.remote", &repo.to_string_lossy(), &mut cx);
        wait(&mut m, &|m| m.remote_of(&repo).is_some());
        assert_eq!(m.remote_of(&repo), Some("https://example.com/x.git"));
    }

    #[test]
    fn risky_keys_and_urls() {
        for k in [
            "core.sshcommand", "core.askpass", "core.gitproxy", "core.hookspath", "core.fsmonitor", "credential.helper", "credential.https://x.y.helper",
            "remote.origin.uploadpack", "remote.o.receivepack", "remote.o.proxy", "remote.o.vcs", "url.x.insteadof", "url.x.pushinsteadof", "protocol.ext.allow",
            "filter.lfs.smudge", "core.alternaterefscommand", "submodule.recurse", "submodule.x.update", "gpg.program", "gpg.ssh.program", "commit.gpgsign", "tag.gpgsign", "merge.verifysignatures", "diff.external", "include.path",
            "includeif.gitdir:/x.path",
        ] {
            assert!(risky_key(k), "{k}");
        }
        for k in ["user.name", "core.editor", "submodule.x.url", "remote.origin.url", "remote.origin.fetch", "branch.main.remote", "url.x.other"] {
            assert!(!risky_key(k), "{k}");
        }
        assert!(is_exec_url(" EXT::sh -c x") && is_exec_url("fd::3") && !is_exec_url("https://h/ext::x"));
        assert_eq!(strip_userinfo("https://u:p@h.com/a@b"), "https://h.com/a@b");
        assert_eq!(strip_userinfo("git@host:x/y.git"), "git@host:x/y.git");
    }

    /// Each class of repo-config trick: the op is refused, and the marker command never runs.
    #[cfg(unix)]
    #[test]
    fn ops_refuse_configs_that_would_run_commands() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let Some(bin) = isolated_git(tmp.path()) else { return };
        let marker = tmp.path().join("pwned");
        let script = tmp.path().join("evil.sh");
        std::fs::write(&script, format!("#!/bin/sh\ntouch '{}'\ncat\n", marker.display())).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let evil = script.to_str().unwrap();
        let included = tmp.path().join("included.cfg");
        std::fs::write(&included, format!("[core]\n\tsshCommand = {evil}\n")).unwrap();
        let hunk = |stage| GitOp::Hunk { rel: "a.md".into(), mark: DiffMark { start: 0, end: 1, old_start: 0, old_end: 1, kind: DiffKind::Modified }, text: "y\n".into(), stage };
        let cases: Vec<(&str, Vec<&str>, GitOp, &str)> = vec![
            ("core.sshCommand", vec![evil], GitOp::Push, "core.sshcommand"),
            ("filter.x.smudge", vec![evil], GitOp::Pull, "filter.x.smudge"),
            ("commit.gpgsign", vec!["true"], GitOp::Commit("m".into()), "commit.gpgsign"),
            ("gpg.program", vec![evil], GitOp::Commit("m".into()), "gpg.program"),
            ("credential.helper", vec!["!evil.sh"], GitOp::Push, "credential.helper"),
            ("include.path", vec![included.to_str().unwrap()], GitOp::Stage("a.md".into()), "include.path"),
            ("core.fsmonitor", vec![evil], GitOp::StageAll, "core.fsmonitor"),
            ("url.ext::sh -c touch pwned.insteadOf", vec!["https://x/"], GitOp::Pull, "insteadof"),
            ("core.sshCommand", vec![evil], GitOp::Fetch(None), "core.sshcommand"),
            ("remote.origin.uploadpack", vec![evil], GitOp::Fetch(Some("origin".into())), "uploadpack"),
            ("filter.x.smudge", vec![evil], GitOp::Discard("a.md".into()), "filter.x.smudge"),
            ("filter.x.smudge", vec![evil], GitOp::Switch("main".into()), "filter.x.smudge"),
            ("core.hooksPath", vec![tmp.path().to_str().unwrap()], GitOp::NewBranch("b".into()), "core.hookspath"),
            ("core.sshCommand", vec![evil], GitOp::RemoteAdd { name: "o".into(), url: "https://x/y".into() }, "core.sshcommand"),
            ("filter.x.clean", vec![evil], hunk(true), "filter.x.clean"),
            ("core.hooksPath", vec![tmp.path().to_str().unwrap()], hunk(false), "core.hookspath"),
            ("include.path", vec![included.to_str().unwrap()], hunk(true), "include.path"),
        ];
        for (i, (key, val, op, want)) in cases.into_iter().enumerate() {
            let repo = tmp.path().join(format!("r{i}"));
            new_repo(&bin, &repo, true);
            std::fs::write(repo.join("a.md"), "x\n").unwrap();
            g(&bin, &repo, &["config", key, val[0]]);
            let err = run_op(&bin, &repo, &op).unwrap_err();
            assert!(err.starts_with("Git: this repository's own config sets") && err.contains(want) && err.ends_with("run git in a terminal"), "{key}: {err}");
            assert!(!marker.exists(), "{key}: the marker command ran");
        }
        // An exec transport as the remote URL (config.worktree is read through `--show-scope` the same way).
        let repo = tmp.path().join("rext");
        new_repo(&bin, &repo, true);
        g(&bin, &repo, &["remote", "add", "origin", &format!("ext::{evil}")]);
        for op in [GitOp::Push, GitOp::Pull, GitOp::Fetch(None), GitOp::Fetch(Some("origin".into())), GitOp::Switch("main".into())] {
            let err = run_op(&bin, &repo, &op).unwrap_err();
            assert!(err.contains("remote.origin.url") && err.contains("runs a command"), "{err}");
        }
        assert!(!marker.exists());
    }

    /// The `git` builder `run_op` uses, for calling `discard` directly with a backup folder of the test's own.
    #[cfg(unix)]
    fn discard_with(bin: &str, repo: &Path, rel: &str, base: &Path) -> Result<String, String> {
        let hooks = format!("core.hooksPath={}", hooks_dir().unwrap().display());
        let git = || {
            let mut c = git_cmd(bin, repo);
            c.args(["-c", &hooks, "--literal-pathspecs"]);
            c
        };
        discard(repo, rel, base, &git)
    }

    #[cfg(unix)]
    fn commit_all(bin: &str, repo: &Path, msg: &str) {
        run_op(bin, repo, &GitOp::StageAll).unwrap();
        run_op(bin, repo, &GitOp::Commit(msg.into())).unwrap();
    }

    #[cfg(unix)]
    fn wait_for(m: &mut GitModule, f: &dyn Fn(&GitModule) -> bool) {
        let t0 = Instant::now();
        while !f(m) && t0.elapsed() < Duration::from_secs(10) {
            m.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(f(m), "timeout");
    }

    #[cfg(unix)]
    #[test]
    fn init_makes_a_repo_with_no_template_hooks_and_refuses_inside_one() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(bin) = isolated_git(tmp.path()) else { return };
        let dir = tmp.path().join("notes");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let line = run_op(&bin, &dir, &GitOp::Init).unwrap();
        assert!(line.starts_with("Initialized empty Git repository"), "{line}");
        assert!(dir.join(".git/HEAD").exists());
        assert_eq!(std::fs::read_dir(dir.join(".git/hooks")).map_or(0, |d| d.count()), 0, "the empty template copies no hook");
        for d in [&dir, &dir.join("sub")] {
            assert_eq!(run_op(&bin, d, &GitOp::Init), Err("This folder is already in a Git repository".into()));
        }
        assert!(!dir.join("sub/.git").exists());
    }

    #[cfg(unix)]
    #[test]
    fn module_initialises_only_workspace_folders_and_the_snapshot_follows() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(bin) = isolated_git(tmp.path()) else { return };
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        new_repo(&bin, &b, true);
        std::fs::create_dir_all(&a).unwrap();
        let waker: Arc<dyn Waker> = Arc::new(NoWake);
        let mut cmds = Vec::new();
        let mut cx = ModuleCx { waker: &waker, commands: &mut cmds };
        let mut m = GitModule::new();
        m.set_git_bin_override(Some(bin));
        m.on_workspace_roots(&[a.clone(), b.clone()]);
        m.start(&mut cx);
        wait_for(&mut m, &|m| m.snapshot().availability == GitAvailability::Ready);
        m.on_command("git.init", &b.to_string_lossy(), &mut cx);
        assert_eq!(m.last_outcome().unwrap().result, Err("This folder is already in a Git repository".into()));
        m.on_command("git.init", &tmp.path().join("elsewhere").to_string_lossy(), &mut cx);
        assert_eq!(m.last_outcome().unwrap().result, Err("Open a note in the folder first".into()));
        m.on_command("git.init", &a.to_string_lossy(), &mut cx);
        assert_eq!(m.busy(&a), Some("Init"));
        wait_for(&mut m, &|m| m.snapshot().repos.len() == 2 && m.busy(&a).is_none());
        assert!(m.last_outcome().unwrap().result.is_ok());
    }

    fn dirs_with_git(root: &Path, repos: &[&str]) {
        for r in repos {
            std::fs::create_dir_all(root.join(r).join(".git")).unwrap();
        }
    }

    #[test]
    fn nested_repo_search_is_bounded_skips_hidden_excluded_and_linked_folders_and_nests() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("ws");
        // Levels below the root: a = 1, q/r/s = 3, x/y/z/w = 4 (kept), x/y/z/w/v = 5 (too deep); a/b/c is inside the repo a.
        dirs_with_git(&root, &["a", "a/b/c", "q/r/s", "x/y/z/w", "x/y/z/w/v", "node_modules/p", ".hidden/q", "skip/me", "dir/plain/../ok"]);
        std::fs::create_dir_all(root.join("plain/deeper")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("a"), root.join("link")).unwrap();
        let ex = ExcludeSet::new(&["node_modules", "skip/me"]);
        let mut got: Vec<String> = find_nested_repos(std::slice::from_ref(&root), &ex).iter().map(|p| p.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/")).collect();
        got.sort();
        assert_eq!(got, ["a", "dir/ok", "q/r/s", "x/y/z/w"], "the root itself, hidden, excluded (by name and by path), too deep, linked and repo-internal folders are left out");
    }

    #[cfg(unix)]
    #[test]
    fn nested_repos_have_their_own_status_and_leave_the_parent_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(bin) = isolated_git(tmp.path()) else { return };
        let root = tmp.path().join("ws");
        new_repo(&bin, &root, true);
        std::fs::write(root.join("top.md"), "t\n").unwrap();
        for sub in ["notes/proj", "other"] {
            new_repo(&bin, &root.join(sub), true);
            std::fs::write(root.join(sub).join("n.md"), "1\n").unwrap();
            g(&bin, &root.join(sub), &["add", "."]);
            g(&bin, &root.join(sub), &["commit", "-qm", "c"]);
            std::fs::write(root.join(sub).join("n.md"), "2\n").unwrap();
        }
        std::fs::create_dir_all(root.join("plain")).unwrap();
        std::fs::write(root.join("plain/p.md"), "p\n").unwrap();
        let mut latched = false;
        let nested = find_nested_repos(std::slice::from_ref(&root), &ExcludeSet::default());
        let snap = scan_roots(&bin, std::slice::from_ref(&root), &nested, None, &mut latched);
        assert_eq!(snap.availability, GitAvailability::Ready);
        let mut roots: Vec<_> = snap.repos.iter().map(|r| r.repo_root.strip_prefix(&root).unwrap().to_string_lossy().into_owned()).collect();
        roots.sort();
        assert_eq!(roots, ["", "notes/proj", "other"]);
        assert_eq!(snap.root_to_repo.len(), 1, "only the workspace folder maps to a repo");
        // A file inside a nested repo takes that repo's status.
        for (f, repo) in [("notes/proj/n.md", "notes/proj"), ("other/n.md", "other")] {
            assert_eq!(snap.repo_for_path(&root.join(f)).unwrap().repo_root, root.join(repo));
            assert_eq!(snap.status_of(&root.join(f)), Some(GitFileStatus::Modified), "{f}");
        }
        assert_eq!(snap.status_of(&root.join("plain/p.md")), Some(GitFileStatus::Untracked));
        let parent = &snap.repos[0];
        let rels: Vec<&str> = parent.changes.iter().map(|c| c.rel.as_str()).collect();
        // `other/` is a nested repo itself and no change of the parent; `notes/` also holds other untracked content for git.
        assert_eq!(rels, ["notes/", "plain/", "top.md"]);
        assert_eq!(parent.dirty_count, 3, "the count follows the list");
    }

    #[cfg(unix)]
    #[test]
    fn a_workspace_with_no_repo_at_the_top_still_lists_the_repos_below_and_a_gitfile_counts() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(bin) = isolated_git(tmp.path()) else { return };
        let (root, main) = (tmp.path().join("ws"), tmp.path().join("main"));
        new_repo(&bin, &main, true);
        std::fs::write(main.join("a.md"), "a\n").unwrap();
        g(&bin, &main, &["add", "."]);
        g(&bin, &main, &["commit", "-qm", "c"]);
        std::fs::create_dir_all(&root).unwrap();
        g(&bin, &main, &["worktree", "add", "-q", root.join("wt").to_str().unwrap()]);
        assert!(root.join("wt/.git").is_file(), "a linked worktree has a .git file");
        let nested = find_nested_repos(std::slice::from_ref(&root), &ExcludeSet::default());
        assert_eq!(nested, [root.join("wt")]);
        let mut latched = false;
        let snap = scan_roots(&bin, std::slice::from_ref(&root), &nested, None, &mut latched);
        assert_eq!(snap.availability, GitAvailability::Ready);
        assert_eq!(snap.repos.len(), 1);
        assert_eq!(snap.repos[0].repo_root, root.join("wt"));
        assert!(snap.root_to_repo.is_empty());
        // Without the walk the workspace is not a repo.
        assert_eq!(scan_roots(&bin, std::slice::from_ref(&root), &[], None, &mut latched).availability, GitAvailability::NotARepo);
    }

    #[cfg(unix)]
    #[test]
    fn module_finds_nested_repos_after_start_honours_excludes_and_looks_again_on_focus() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(bin) = isolated_git(tmp.path()) else { return };
        let root = tmp.path().join("ws");
        for sub in ["one", "skipme/two"] {
            new_repo(&bin, &root.join(sub), true);
        }
        let waker: Arc<dyn Waker> = Arc::new(NoWake);
        let mut cmds = Vec::new();
        let mut cx = ModuleCx { waker: &waker, commands: &mut cmds };
        let mut m = GitModule::new();
        m.set_git_bin_override(Some(bin.clone()));
        m.set_excludes(&["skipme".to_string()]);
        m.on_workspace_roots(std::slice::from_ref(&root));
        m.start(&mut cx);
        wait_for(&mut m, &|m| m.snapshot().repos.len() == 1);
        assert_eq!(m.snapshot().repos[0].repo_root, root.join("one"));
        // A repo cloned in a terminal shows up when the window gets the focus back; a changed exclude list searches again.
        new_repo(&bin, &root.join("three"), true);
        m.on_focus_gained();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(m.snapshot().repos.len(), 1, "a focus gain within 30 s does not search again");
        m.last_walk = Instant::now().checked_sub(Duration::from_secs(31)).unwrap_or_else(Instant::now);
        m.on_focus_gained();
        wait_for(&mut m, &|m| m.snapshot().repos.len() == 2);
        m.set_excludes(&[]);
        wait_for(&mut m, &|m| m.snapshot().repos.len() == 3);
    }

    #[cfg(unix)]
    #[test]
    fn a_partial_scan_runs_git_only_for_the_touched_repos() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(bin) = isolated_git(tmp.path()) else { return };
        let root = tmp.path().join("ws");
        for sub in ["a", "b"] {
            new_repo(&bin, &root.join(sub), true);
            std::fs::write(root.join(sub).join("n.md"), "1\n").unwrap();
            g(&bin, &root.join(sub), &["add", "."]);
            g(&bin, &root.join(sub), &["commit", "-qm", "c"]);
        }
        let nested = find_nested_repos(std::slice::from_ref(&root), &ExcludeSet::default());
        let mut latched = false;
        let first = scan_roots(&bin, &[], &nested, None, &mut latched);
        assert!(first.repos.iter().all(|r| r.changes.is_empty()));
        for sub in ["a", "b"] {
            std::fs::write(root.join(sub).join("n.md"), "2\n").unwrap();
        }
        let touched: FxHashSet<PathBuf> = [root.join("a")].into_iter().collect();
        let second = scan_roots(&bin, &[], &nested, Some((&first, &touched)), &mut latched);
        let dirty = |s: &GitSnapshot, n: &str| s.repos.iter().find(|r| r.repo_root == root.join(n)).map(|r| r.changes.len());
        assert_eq!((dirty(&second, "a"), dirty(&second, "b")), (Some(1), Some(0)), "only a was asked again");
        assert_eq!(dirty(&scan_roots(&bin, &[], &nested, None, &mut latched), "b"), Some(1), "a full scan sees both");
    }

    #[test]
    fn the_diff_cache_keeps_the_newest_entries_only() {
        let mut m = GitModule::new();
        let (tx, rx) = unbounded();
        m.rx = Some(rx);
        let mark = DiffMark { start: 0, end: 1, old_start: 0, old_end: 1, kind: DiffKind::Modified };
        for i in 0..40 {
            tx.send(WorkerMsg::DiffDone { path: PathBuf::from(format!("/n{i}.md")), version: 1, head_oid: None, marks: Arc::from([mark]), old: "x".into() }).unwrap();
        }
        m.poll();
        let ds = m.diff_state.lock();
        assert_eq!(ds.cache.len(), MAX_CACHED_DIFFS);
        assert!(!ds.cache.contains_key(Path::new("/n0.md")) && ds.cache.contains_key(Path::new("/n39.md")));
    }

    /// Measured, not claimed: the walk over a big tree runs on the worker (numbers in the README).
    #[test]
    fn nested_repo_search_over_five_thousand_folders_is_fast() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("ws");
        // 5 000 folders: 10 x 10 x 49 leaves plus their parents, three levels deep, a repo every 500th.
        let mut n = 0;
        for a in 0..10 {
            for b in 0..10 {
                for c in 0..49 {
                    let d = root.join(format!("a{a}/b{b}/c{c}"));
                    std::fs::create_dir_all(&d).unwrap();
                    std::fs::write(d.join("n.md"), "x").unwrap();
                    n += 1;
                    if n % 500 == 0 {
                        std::fs::create_dir(d.join(".git")).unwrap();
                    }
                }
            }
        }
        let t = Instant::now();
        let found = find_nested_repos(std::slice::from_ref(&root), &ExcludeSet::new(&["node_modules", "__pycache__"]));
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        eprintln!("nested repo walk: {} folders, {} repos, {ms:.1} ms", 10 + 100 + 4900, found.len());
        assert_eq!(found.len(), 9);
        assert!(ms < 500.0, "{ms} ms");
    }

    /// `text` with CRLF when `crlf`.
    #[cfg(unix)]
    fn eol(text: &str, crlf: bool) -> Vec<u8> {
        if crlf { text.replace('\n', "\r\n").into_bytes() } else { text.as_bytes().to_vec() }
    }

    #[cfg(unix)]
    fn index_oid(bin: &str, repo: &Path, file: &str) -> String {
        g(bin, repo, &["rev-parse", &format!(":{file}")]).trim().to_string()
    }

    /// `git add -p` answering `y` for hunk `k` and `n` for the others (no `core.autocrlf`, no attributes in the isolated git).
    #[cfg(unix)]
    fn git_add_p(bin: &str, repo: &Path, file: &str, k: usize, hunks: usize) {
        let answers: String = (0..hunks).map(|i| if i == k { "y\n" } else { "n\n" }).collect();
        let mut child = Command::new(bin).current_dir(repo).args(["add", "-p", "--", file]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(answers.as_bytes()).unwrap();
        assert!(child.wait().unwrap().success());
    }

    #[cfg(unix)]
    fn stage_hunk_now(bin: &str, repo: &Path, file: &str, mark: &DiffMark, text: &str, stage: bool) -> Result<String, String> {
        run_op(bin, repo, &GitOp::Hunk { rel: file.into(), mark: *mark, text: text.into(), stage })
    }

    /// Staging one hunk gives the index blob `git add -p` makes, for LF and CRLF files, at the start, middle and end of a
    /// file and with or without a final newline; unstaging goes back to `HEAD`'s blob; staging all of them gives the file.
    #[cfg(unix)]
    #[test]
    fn stage_and_unstage_hunk_match_git_add_p_for_lf_and_crlf_files() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(bin) = isolated_git(tmp.path()) else { return };
        let base: String = (1..=40).map(|i| format!("line {i}\n")).collect();
        let edit = |f: &dyn Fn(&mut Vec<String>)| {
            let mut v: Vec<String> = base.lines().map(str::to_string).collect();
            f(&mut v);
            v.join("\n") + "\n"
        };
        let variants: Vec<(&str, String)> = vec![
            ("modify, delete, add at the end", edit(&|v| {
                v[4] = "five!".into();
                v.remove(19);
                v.push("tail".into());
            })),
            ("add at the start, modify the last line", edit(&|v| {
                v.insert(0, "head".into());
                *v.last_mut().unwrap() = "last!".into();
            })),
            ("delete the first and the last line", edit(&|v| {
                v.remove(0);
                v.pop();
            })),
            ("no final newline in the buffer", edit(&|v| v[9] = "ten!".into()).trim_end_matches('\n').to_string()),
            ("two lines in the middle replaced by three", edit(&|v| {
                v.splice(14..16, ["a".to_string(), "b".into(), "c".into()]);
                v[34] = "late".into();
            })),
        ];
        for crlf in [false, true] {
            for (n, (what, text)) in variants.iter().enumerate() {
                let repo = tmp.path().join(format!("r{n}{crlf}"));
                new_repo(&bin, &repo, true);
                std::fs::write(repo.join("f.md"), eol(&base, crlf)).unwrap();
                g(&bin, &repo, &["add", "."]);
                g(&bin, &repo, &["commit", "-qm", "base"]);
                let head_oid = index_oid(&bin, &repo, "f.md");
                std::fs::write(repo.join("f.md"), eol(text, crlf)).unwrap();
                let marks = diff_lines(&base, text);
                assert!(marks.len() >= 2, "{what}: {marks:?}");
                for (k, m) in marks.iter().enumerate() {
                    let ctx = format!("{what}, crlf {crlf}, hunk {k} {m:?}");
                    git_add_p(&bin, &repo, "f.md", k, marks.len());
                    let want = index_oid(&bin, &repo, "f.md");
                    g(&bin, &repo, &["reset", "-q"]);
                    stage_hunk_now(&bin, &repo, "f.md", m, text, true).unwrap();
                    assert_eq!(index_oid(&bin, &repo, "f.md"), want, "{ctx}: same blob as git add -p");
                    assert!(stage_hunk_now(&bin, &repo, "f.md", m, text, true).is_err(), "{ctx}: staged twice");
                    stage_hunk_now(&bin, &repo, "f.md", m, text, false).unwrap();
                    assert_eq!(index_oid(&bin, &repo, "f.md"), head_oid, "{ctx}: unstaged is HEAD again");
                    assert_eq!(stage_hunk_now(&bin, &repo, "f.md", m, text, false), Err("That change is not staged".into()), "{ctx}");
                }
                for m in &marks {
                    stage_hunk_now(&bin, &repo, "f.md", m, text, true).unwrap();
                }
                g(&bin, &repo, &["diff", "--quiet"]);
                let staged = g(&bin, &repo, &["diff", "--cached", "--numstat"]);
                assert!(!staged.is_empty(), "{what}");
                // Unstage them again in the other order.
                for m in marks.iter().rev() {
                    stage_hunk_now(&bin, &repo, "f.md", m, text, false).unwrap();
                }
                assert_eq!(index_oid(&bin, &repo, "f.md"), head_oid, "{what}, crlf {crlf}: all unstaged");
            }
        }
    }

    /// `core.autocrlf=true` (Git for Windows): LF blobs, CRLF files. A staged hunk stays LF, like `git add -p` makes it.
    #[cfg(unix)]
    #[test]
    fn stage_hunk_follows_the_index_line_endings_under_autocrlf() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(bin) = isolated_git(tmp.path()) else { return };
        let repo = tmp.path().join("r");
        new_repo(&bin, &repo, true);
        g(&bin, &repo, &["config", "core.autocrlf", "true"]);
        let base: String = (1..=30).map(|i| format!("l{i}\n")).collect();
        std::fs::write(repo.join("f.md"), eol(&base, true)).unwrap();
        g(&bin, &repo, &["add", "."]);
        g(&bin, &repo, &["commit", "-qm", "base"]);
        assert!(!g(&bin, &repo, &["cat-file", "-p", "HEAD:f.md"]).contains('\r'), "the blob is LF");
        let text = base.replace("l3\n", "L3\n").replace("l25\n", "L25\n");
        std::fs::write(repo.join("f.md"), eol(&text, true)).unwrap();
        let marks = diff_lines(&base, &text);
        for (k, m) in marks.iter().enumerate() {
            git_add_p(&bin, &repo, "f.md", k, marks.len());
            let want = index_oid(&bin, &repo, "f.md");
            g(&bin, &repo, &["reset", "-q"]);
            stage_hunk_now(&bin, &repo, "f.md", m, &text, true).unwrap();
            assert_eq!(index_oid(&bin, &repo, "f.md"), want, "hunk {k}");
            g(&bin, &repo, &["reset", "-q"]);
        }
    }

    #[cfg(unix)]
    #[test]
    fn stage_hunk_keeps_the_mode_works_on_partially_staged_and_unborn_files_and_refuses_stale_marks_and_unsuitable_files() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let Some(bin) = isolated_git(tmp.path()) else { return };
        let repo = tmp.path().join("r");
        new_repo(&bin, &repo, true);
        let base: String = (1..=30).map(|i| format!("l{i}\n")).collect();
        let text: String = base.replace("l3\n", "L3\n").replace("l25\n", "L25\n");
        std::fs::write(repo.join("x.sh"), &base).unwrap();
        std::fs::set_permissions(repo.join("x.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        // Unborn: the whole file is one "added" change; the index entry exists (git add), so staging it takes the buffer.
        g(&bin, &repo, &["add", "x.sh"]);
        std::fs::write(repo.join("x.sh"), &text).unwrap();
        let whole = DiffMark { start: 0, end: 31, old_start: 0, old_end: 0, kind: DiffKind::Added };
        stage_hunk_now(&bin, &repo, "x.sh", &whole, &text, true).unwrap();
        assert_eq!(index_oid(&bin, &repo, "x.sh"), g(&bin, &repo, &["hash-object", "x.sh"]).trim());
        assert_eq!(stage_hunk_now(&bin, &repo, "x.sh", &whole, &text, false), Err("This file is not in the last commit: unstage the whole file".into()));
        // Born: commit the base, then a half-staged state (first change staged), then stage the second only.
        std::fs::write(repo.join("x.sh"), &base).unwrap();
        g(&bin, &repo, &["add", "x.sh"]);
        g(&bin, &repo, &["commit", "-qm", "base"]);
        std::fs::write(repo.join("x.sh"), &text).unwrap();
        let marks = diff_lines(&base, &text);
        assert_eq!(marks.len(), 2);
        stage_hunk_now(&bin, &repo, "x.sh", &marks[0], &text, true).unwrap();
        stage_hunk_now(&bin, &repo, "x.sh", &marks[1], &text, true).unwrap();
        assert_eq!(g(&bin, &repo, &["ls-files", "-s", "x.sh"]).split(' ').next(), Some("100755"), "the exec bit stays");
        assert_eq!(index_oid(&bin, &repo, "x.sh"), g(&bin, &repo, &["hash-object", "x.sh"]).trim());
        // Unstage the first one only: the index keeps the second.
        stage_hunk_now(&bin, &repo, "x.sh", &marks[0], &text, false).unwrap();
        let cached = g(&bin, &repo, &["diff", "--cached", "-U0"]);
        assert!(cached.contains("+L25") && !cached.contains("+L3\n"), "{cached}");
        // A mark of an older buffer is refused, nothing changes.
        let before = index_oid(&bin, &repo, "x.sh");
        assert_eq!(stage_hunk_now(&bin, &repo, "x.sh", &marks[0], &base, false), Err(crate::git_hunks::STALE.into()));
        let bogus = DiffMark { start: 9, end: 10, old_start: 9, old_end: 10, kind: DiffKind::Modified };
        assert!(stage_hunk_now(&bin, &repo, "x.sh", &bogus, &text, true).is_err());
        assert_eq!(index_oid(&bin, &repo, "x.sh"), before);
        // Untracked, linked and conflicting files are no targets.
        std::fs::write(repo.join("new.md"), "n\n").unwrap();
        assert!(stage_hunk_now(&bin, &repo, "new.md", &marks[0], "n\n", true).unwrap_err().contains("not in the index"));
        std::os::unix::fs::symlink("x.sh", repo.join("ln")).unwrap();
        g(&bin, &repo, &["add", "ln"]);
        assert!(stage_hunk_now(&bin, &repo, "ln", &marks[0], "x.sh", true).unwrap_err().contains("regular files"));
    }

    #[cfg(unix)]
    #[test]
    fn module_stages_a_hunk_on_the_worker_and_reports_a_stale_one() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(bin) = isolated_git(tmp.path()) else { return };
        let repo = tmp.path().join("r");
        new_repo(&bin, &repo, true);
        let base: String = (1..=20).map(|i| format!("l{i}\n")).collect();
        std::fs::write(repo.join("n.md"), &base).unwrap();
        g(&bin, &repo, &["add", "."]);
        g(&bin, &repo, &["commit", "-qm", "base"]);
        let text = base.replace("l2\n", "two\n");
        std::fs::write(repo.join("n.md"), &text).unwrap();
        let waker: Arc<dyn Waker> = Arc::new(NoWake);
        let mut cmds = Vec::new();
        let mut cx = ModuleCx { waker: &waker, commands: &mut cmds };
        let mut m = GitModule::new();
        m.set_git_bin_override(Some(bin.clone()));
        m.on_workspace_roots(std::slice::from_ref(&repo));
        m.start(&mut cx);
        wait_for(&mut m, &|m| m.snapshot().repos.len() == 1);
        let note = repo.join("n.md");
        let mark = diff_lines(&base, &text)[0];
        m.hunk_op(&note, mark, text.clone(), true);
        wait_for(&mut m, &|m| m.last_outcome().is_some_and(|o| o.op == "Stage hunk"));
        assert_eq!(m.last_outcome().unwrap().result, Ok("Staged hunk in n.md".into()));
        wait_for(&mut m, &|m| m.snapshot().repos[0].changes.iter().any(|c| c.staged()));
        m.hunk_op(&note, mark, text.replace("l3\n", "three\n"), false); // l2 and l3 now change together: the mark is stale
        wait_for(&mut m, &|m| m.last_outcome().is_some_and(|o| o.op == "Unstage hunk"));
        assert_eq!(m.last_outcome().unwrap().result, Err(crate::git_hunks::STALE.into()));
        m.hunk_op(&tmp.path().join("elsewhere.md"), mark, text, true);
        assert_eq!(m.last_outcome().unwrap().result, Err("This note is not in a Git repository".into()));
    }

    #[cfg(unix)]
    #[test]
    fn discard_backs_up_first_restores_and_undo_puts_it_back() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(bin) = isolated_git(tmp.path()) else { return };
        let (repo, base) = (tmp.path().join("repo"), tmp.path().join("backups"));
        new_repo(&bin, &repo, true);
        // CRLF + BOM bytes must survive the round trip untouched.
        let odd: &[u8] = b"\xEF\xBB\xBFone\r\ntwo\r\n";
        for f in ["[x] a.md", "b.md", "gone.md"] {
            std::fs::write(repo.join(f), "orig\n").unwrap();
        }
        commit_all(&bin, &repo, "one");
        std::fs::write(repo.join("[x] a.md"), odd).unwrap();
        std::fs::write(repo.join("untracked.md"), "mine\n").unwrap();
        std::fs::remove_file(repo.join("gone.md")).unwrap();

        let msg = discard_with(&bin, &repo, "[x] a.md", &base).unwrap();
        assert_eq!(std::fs::read_to_string(repo.join("[x] a.md")).unwrap(), "orig\n", "a literal path, back to HEAD");
        let backup = msg.split("Backup: ").nth(1).unwrap();
        assert!(Path::new(backup).starts_with(&base) && !Path::new(backup).starts_with(&repo), "outside the repo: {backup}");
        assert_eq!(std::fs::read(backup).unwrap(), odd);
        discard_with(&bin, &repo, "untracked.md", &base).unwrap();
        assert!(!repo.join("untracked.md").exists());
        assert_eq!(discard_with(&bin, &repo, "gone.md", &base), Ok("Discarded gone.md".into()), "a deleted file needs no backup");
        assert!(repo.join("gone.md").exists());
        assert!(changes(&bin, &repo).is_empty());

        // Undo goes back through the backups, newest first.
        assert_eq!(undo_discard(&repo, &base, None), Ok("Restored untracked.md".into()));
        assert_eq!(std::fs::read_to_string(repo.join("untracked.md")).unwrap(), "mine\n");
        assert_eq!(undo_discard(&repo, &base, None), Ok("Restored [x] a.md".into()));
        assert_eq!(std::fs::read(repo.join("[x] a.md")).unwrap(), odd);
        assert_eq!(undo_discard(&repo, &base, None), Err("No discarded file to restore".into()));

        // Refusals: a folder, a path leaving the repo; nothing is lost.
        std::fs::create_dir_all(repo.join("dir")).unwrap();
        assert!(discard_with(&bin, &repo, "dir", &base).unwrap_err().contains("folder"));
        assert!(discard_with(&bin, &repo, "../x", &base).unwrap_err().contains("not a path inside"));
        assert!(discard_with(&bin, &repo, "/etc/passwd", &base).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn discard_failure_keeps_no_backup_and_old_backups_are_pruned() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(bin) = isolated_git(tmp.path()) else { return };
        let (repo, base) = (tmp.path().join("repo"), tmp.path().join("backups"));
        new_repo(&bin, &repo, true);
        std::fs::write(repo.join("a.md"), "x\n").unwrap();
        commit_all(&bin, &repo, "one");
        // `restore` fails on an unmerged path: the file stays, and no backup is left that undo could resurrect.
        g(&bin, &repo, &["checkout", "-q", "-b", "side"]);
        std::fs::write(repo.join("a.md"), "side\n").unwrap();
        commit_all(&bin, &repo, "side");
        g(&bin, &repo, &["checkout", "-q", "main"]);
        std::fs::write(repo.join("a.md"), "main\n").unwrap();
        commit_all(&bin, &repo, "main");
        let _ = Command::new(&bin).current_dir(&repo).args(["merge", "side"]).output();
        assert!(discard_with(&bin, &repo, "a.md", &base).is_err());
        assert!(std::fs::read_to_string(repo.join("a.md")).unwrap().contains("<<<<<<<"));
        assert_eq!(undo_discard(&repo, &base, None), Err("No discarded file to restore".into()));

        g(&bin, &repo, &["merge", "--abort"]);
        for i in 0..(KEEP_BACKUPS + 5) {
            std::fs::write(repo.join("a.md"), format!("edit {i}\n")).unwrap();
            discard_with(&bin, &repo, "a.md", &base).unwrap();
        }
        assert_eq!(std::fs::read_dir(backup_dir(&base, &repo)).unwrap().count(), KEEP_BACKUPS);
        assert_eq!(undo_discard(&repo, &base, None), Ok("Restored a.md".into()));
        assert_eq!(std::fs::read_to_string(repo.join("a.md")).unwrap(), format!("edit {}\n", KEEP_BACKUPS + 4), "the newest backup comes back first");
    }

    #[cfg(unix)]
    #[test]
    fn discard_refuses_links_special_files_and_symlinked_parents_and_undo_keeps_newer_edits() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let Some(bin) = isolated_git(tmp.path()) else { return };
        let (repo, base, outside) = (tmp.path().join("repo"), tmp.path().join("backups"), tmp.path().join("outside"));
        new_repo(&bin, &repo, true);
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret"), "secret\n").unwrap();
        std::fs::write(repo.join("a.md"), "one\n").unwrap();
        commit_all(&bin, &repo, "one");

        // A symlink (even to a device), a FIFO and a path through a linked folder are refused, nothing is copied or deleted.
        symlink("/dev/zero", repo.join("zero")).unwrap();
        symlink(&outside, repo.join("linked")).unwrap();
        let fifo = repo.join("fifo");
        assert!(Command::new("mkfifo").arg(&fifo).status().unwrap().success());
        for rel in ["zero", "fifo", "linked/secret"] {
            let err = discard_with(&bin, &repo, rel, &base).unwrap_err();
            assert!(err.contains("not a regular file") || err.contains("symbolic link"), "{rel}: {err}");
        }
        assert_eq!(std::fs::read_to_string(outside.join("secret")).unwrap(), "secret\n");
        assert!(!base.exists(), "no backup was made");
        assert!(repo_file(&repo, "linked/new.md").is_err(), "undo would write through the link too");

        // Undo refuses to overwrite what changed after the discard; the named stamp is the one restored.
        std::fs::write(repo.join("a.md"), "first edit\n").unwrap();
        let m1 = discard_with(&bin, &repo, "a.md", &base).unwrap();
        std::fs::write(repo.join("a.md"), "second edit\n").unwrap();
        discard_with(&bin, &repo, "a.md", &base).unwrap();
        let s1 = backup_stamp(&m1, &base).unwrap();
        assert_eq!(undo_discard(&repo, &base, Some(&s1)), Ok("Restored a.md".into()), "the first discard's backup, not the newest");
        assert_eq!(std::fs::read_to_string(repo.join("a.md")).unwrap(), "first edit\n");
        std::fs::write(repo.join("a.md"), "typed after\n").unwrap();
        let err = undo_discard(&repo, &base, None).unwrap_err();
        assert!(err.contains("changed since") && err.contains("backups"), "{err}");
        assert_eq!(std::fs::read_to_string(repo.join("a.md")).unwrap(), "typed after\n");
        assert!(undo_discard(&repo, &base, Some("../x")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn backup_stamps_keep_creation_order_when_the_clock_steps_back() {
        let tmp = tempfile::tempdir().unwrap();
        let (repo, base) = (tmp.path().join("repo"), tmp.path().join("b"));
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("f"), "x").unwrap();
        let dir = backup_dir(&base, &repo);
        // A stamp from the far future, as if the clock was later set back.
        std::fs::create_dir_all(dir.join(format!("{:020}", u64::MAX as u128))).unwrap();
        let b = backup_file(&base, &repo, "f", &repo.join("f")).unwrap();
        let newest = stamps_of(&dir).pop().unwrap();
        assert!(b.starts_with(&newest) && newest.file_name().unwrap().to_str().unwrap().parse::<u128>().unwrap() > u64::MAX as u128, "the new stamp sorts last");
    }

    #[cfg(unix)]
    #[test]
    fn switch_create_delete_branches_and_git_refuses_a_dirty_switch() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let Some(bin) = isolated_git(tmp.path()) else { return };
        let repo = tmp.path().join("repo");
        new_repo(&bin, &repo, true);
        std::fs::write(repo.join("a.md"), "main\n").unwrap();
        commit_all(&bin, &repo, "first commit");
        let names = |r: &[Branch]| r.iter().map(|b| (b.name.clone(), b.current)).collect::<Vec<_>>();
        assert_eq!(names(&list_branches(&bin, &repo).unwrap()), [("main".into(), true)]);
        assert_eq!(list_branches(&bin, &repo).unwrap()[0].subject, "first commit");

        assert_eq!(run_op(&bin, &repo, &GitOp::NewBranch("feat/x".into())), Ok("Created and switched to feat/x".into()));
        std::fs::write(repo.join("a.md"), "feat\n").unwrap();
        commit_all(&bin, &repo, "feat");
        assert_eq!(names(&list_branches(&bin, &repo).unwrap()), [("feat/x".into(), true), ("main".into(), false)]);

        // Dirty tree: git's own first error line, and nothing changed.
        std::fs::write(repo.join("a.md"), "dirty\n").unwrap();
        let err = run_op(&bin, &repo, &GitOp::Switch("main".into())).unwrap_err();
        assert!(err.starts_with("error:") && err.contains("would be overwritten"), "{err}");
        assert_eq!(std::fs::read_to_string(repo.join("a.md")).unwrap(), "dirty\n");
        g(&bin, &repo, &["checkout", "--", "a.md"]);

        // A post-checkout hook of the repo does not run on a switch or a discard.
        let marker = tmp.path().join("hook-ran");
        let hook = repo.join(".git/hooks/post-checkout");
        std::fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(run_op(&bin, &repo, &GitOp::Switch("main".into())), Ok("Switched to main".into()));
        assert_eq!(std::fs::read_to_string(repo.join("a.md")).unwrap(), "main\n");
        std::fs::write(repo.join("a.md"), "again\n").unwrap();
        run_op(&bin, &repo, &GitOp::Discard("a.md".into())).unwrap();
        run_op(&bin, &repo, &GitOp::NewBranch("other".into())).unwrap();
        assert!(!marker.exists(), "a post-checkout hook from the repo ran");
        run_op(&bin, &repo, &GitOp::Switch("main".into())).unwrap();

        // Invalid names never reach git as options; delete is `-d` only and never the current branch.
        for bad in ["", "-x", "--detach", "a b", "x\ny"] {
            assert!(run_op(&bin, &repo, &GitOp::Switch(bad.into())).unwrap_err().contains("not a valid name"), "{bad:?}");
            assert!(run_op(&bin, &repo, &GitOp::NewBranch(bad.into())).is_err(), "{bad:?}");
            assert!(run_op(&bin, &repo, &GitOp::DeleteBranch(bad.into())).is_err(), "{bad:?}");
        }
        assert!(run_op(&bin, &repo, &GitOp::NewBranch("a..b".into())).unwrap_err().contains("not a valid branch name"));
        assert!(run_op(&bin, &repo, &GitOp::DeleteBranch("main".into())).unwrap_err().starts_with("error:"));
        assert_eq!(run_op(&bin, &repo, &GitOp::DeleteBranch("other".into())), Ok("Deleted branch other".into()));
        let err = run_op(&bin, &repo, &GitOp::DeleteBranch("feat/x".into())).unwrap_err();
        assert!(err.contains("not fully merged"), "unmerged branches need a terminal: {err}");
        assert_eq!(names(&list_branches(&bin, &repo).unwrap()), [("feat/x".into(), false), ("main".into(), true)]);
    }

    #[test]
    fn remote_names_and_urls_are_validated() {
        for n in ["origin", "up-stream", "a_b.c", "R2"] {
            assert_eq!(remote_name_error(n), None, "{n}");
        }
        for n in ["", "-x", ".x", "a b", "a..b", "a/b", "ü"] {
            assert!(remote_name_error(n).is_some(), "{n:?}");
        }
        for u in ["https://h/x.git", "git@host:x/y.git", "/srv/my repo.git", "file:///x", "ssh://[::1]/x", "C:\\x\\y"] {
            assert_eq!(remote_url_error(u), None, "{u}");
        }
        for u in ["", "  ", "-oProxyCommand=x", "ext::sh -c x", "FD::3", "hg::http://x", "a\nb"] {
            assert!(remote_url_error(u).is_some(), "{u:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn remotes_add_rename_remove_fetch_and_ahead_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(bin) = isolated_git(tmp.path()) else { return };
        let bare = tmp.path().join("remote.git");
        std::fs::create_dir_all(&bare).unwrap();
        g(&bin, &bare, &["init", "--bare", "-b", "main"]);
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        new_repo(&bin, &a, true);
        std::fs::write(a.join("n.md"), "1\n").unwrap();
        commit_all(&bin, &a, "one");
        assert_eq!(list_remotes(&bin, &a).unwrap(), []);
        let url = bare.to_str().unwrap();
        run_op(&bin, &a, &GitOp::RemoteAdd { name: "origin".into(), url: url.into() }).unwrap();
        assert!(run_op(&bin, &a, &GitOp::RemoteAdd { name: "origin".into(), url: url.into() }).unwrap_err().contains("already exists"));
        // A remote we add sets no key that runs commands.
        let cfg = g(&bin, &a, &["config", "--local", "--list"]);
        assert!(cfg.lines().any(|l| l.starts_with("remote.origin.url=")) && !cfg.lines().any(|l| risky_key(l.split('=').next().unwrap_or(""))), "{cfg}");
        for (name, url, want) in [("-x", url, "Use letters"), ("o2", "ext::sh -c touch pwned", "run a command"), ("o2", "-oFoo", "not a URL")] {
            let err = run_op(&bin, &a, &GitOp::RemoteAdd { name: name.into(), url: url.into() }).unwrap_err();
            assert!(err.contains(want), "{err}");
        }
        assert_eq!(list_remotes(&bin, &a).unwrap().len(), 1);
        g(&bin, &a, &["remote", "set-url", "origin", "https://user:secret@example.com/x.git"]);
        assert_eq!(list_remotes(&bin, &a).unwrap(), [RemoteInfo { name: "origin".into(), url: "https://example.com/x.git".into() }], "no credentials shown");
        g(&bin, &a, &["remote", "set-url", "origin", url]);
        g(&bin, &a, &["push", "-u", "origin", "main"]);

        g(&bin, tmp.path(), &["clone", url, b.to_str().unwrap()]);
        g(&bin, &b, &["config", "user.name", "B"]);
        g(&bin, &b, &["config", "user.email", "b@example.com"]);
        std::fs::write(a.join("n.md"), "1\n2\n").unwrap();
        commit_all(&bin, &a, "two");
        run_op(&bin, &a, &GitOp::Push).unwrap();
        std::fs::write(b.join("local.md"), "l\n").unwrap();
        commit_all(&bin, &b, "local");
        let sync = |repo: &Path| {
            let mut latched = false;
            let r = scan_roots(&bin, std::slice::from_ref(&repo.to_path_buf()), &[], None, &mut latched).repos.remove(0);
            (r.upstream.clone(), r.ahead, r.behind, r.sync_text())
        };
        assert_eq!(sync(&b), (Some("origin/main".into()), 1, 0, "ahead 1".into()), "behind is unknown until a fetch");
        assert_eq!(run_op(&bin, &b, &GitOp::Fetch(None)), Ok("Fetched".into()));
        assert_eq!(sync(&b), (Some("origin/main".into()), 1, 1, "ahead 1, behind 1".into()));
        assert_eq!(run_op(&bin, &b, &GitOp::Fetch(Some("origin".into()))), Ok("Fetched".into()));
        assert!(run_op(&bin, &b, &GitOp::Fetch(Some("nope".into()))).unwrap_err().starts_with("fatal:"));
        assert!(run_op(&bin, &b, &GitOp::Fetch(Some("--all".into()))).unwrap_err().contains("not a valid name"));
        g(&bin, &b, &["remote", "set-url", "origin", tmp.path().join("missing.git").to_str().unwrap()]);
        assert!(run_op(&bin, &b, &GitOp::Fetch(None)).unwrap_err().starts_with("fatal:"), "an unreachable remote fails with a line");

        run_op(&bin, &a, &GitOp::RemoteRename { from: "origin".into(), to: "up".into() }).unwrap();
        assert_eq!(list_remotes(&bin, &a).unwrap()[0].name, "up");
        assert_eq!(sync(&a).0.as_deref(), Some("up/main"), "branch tracking followed the rename");
        assert!(run_op(&bin, &a, &GitOp::RemoteRename { from: "up".into(), to: "bad name".into() }).is_err());
        run_op(&bin, &a, &GitOp::RemoteRemove("up".into())).unwrap();
        assert_eq!(list_remotes(&bin, &a).unwrap(), []);
        assert!(run_op(&bin, &a, &GitOp::RemoteRemove("up".into())).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn module_lists_branches_and_remotes_for_the_dialogs() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(bin) = isolated_git(tmp.path()) else { return };
        let repo = tmp.path().join("repo");
        new_repo(&bin, &repo, true);
        std::fs::write(repo.join("n.md"), "x\n").unwrap();
        commit_all(&bin, &repo, "one");
        let waker: Arc<dyn Waker> = Arc::new(NoWake);
        let mut cmds = Vec::new();
        let mut cx = ModuleCx { waker: &waker, commands: &mut cmds };
        let mut m = GitModule::new();
        m.set_git_bin_override(Some(bin));
        m.on_workspace_roots(std::slice::from_ref(&repo));
        m.start(&mut cx);
        wait_for(&mut m, &|m| m.snapshot().availability == GitAvailability::Ready);
        assert_eq!(m.take_dialog(), None);
        m.on_command("git.branches", &repo.to_string_lossy(), &mut cx);
        assert_eq!(m.take_dialog(), Some(GitDialog::Branches(repo.clone())));
        wait_for(&mut m, &|m| m.branches(&repo).is_some());
        // An op that changes branches refreshes the list the dialog shows.
        m.on_command("git.branch_new", &format!("{}\0topic", repo.display()), &mut cx);
        wait_for(&mut m, &|m| matches!(m.branches(&repo), Some(Ok(l)) if l.iter().any(|b| b.name == "topic" && b.current)));
        assert_eq!(m.last_outcome().unwrap().op, "New branch");
        m.on_command("git.remotes", &repo.to_string_lossy(), &mut cx);
        assert_eq!(m.take_dialog(), Some(GitDialog::Remotes(repo.clone())));
        m.on_command("git.remote_add", &format!("{}\0origin\0https://example.com/x.git", repo.display()), &mut cx);
        wait_for(&mut m, &|m| matches!(m.remote_list(&repo), Some(Ok(l)) if l.len() == 1));
        m.on_command("git.fetch", &format!("{}\0origin", repo.display()), &mut cx);
        assert_eq!(m.busy(&repo), Some("Fetch"), "fetch is a long op");
        wait_for(&mut m, &|m| m.busy(&repo).is_none());
        assert!(m.last_outcome().unwrap().result.is_err(), "no network in the test, git's line comes back");
    }

    /// A timeout takes down what git started, and a child that holds the pipes does not stall the worker.
    #[cfg(unix)]
    #[test]
    fn timeout_kills_the_process_group() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let fake = tmp.path().join("git");
        let pidfile = tmp.path().join("pid");
        std::fs::write(&fake, format!("#!/bin/sh\nsleep 30 &\necho $! > '{}'\nwait\n", pidfile.display())).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = run(&mut Command::new(&fake), Duration::from_millis(500), true).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        let pid = std::fs::read_to_string(&pidfile).unwrap().trim().to_string();
        let t0 = Instant::now();
        while Command::new("kill").args(["-0", &pid]).stderr(Stdio::null()).status().unwrap().success() && t0.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!Command::new("kill").args(["-0", &pid]).stderr(Stdio::null()).status().unwrap().success(), "the grandchild survived the timeout");
    }
}
