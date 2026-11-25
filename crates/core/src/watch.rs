//! Debounced recursive file watching with background filtered registration.
use crate::glob::ExcludeSet;
use crossbeam_channel::{Receiver, Select, Sender, TryRecvError, unbounded};
#[cfg(target_os = "linux")]
use notify::event::CreateKind;
use notify::event::{ModifyKind, RenameMode};
use notify::{ErrorKind, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};
#[cfg(target_os = "linux")]
use rustc_hash::FxHashSet;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "linux")]
use std::collections::VecDeque;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsEvent {
    Changed(PathBuf),
    Removed(PathBuf),
    /// Events were lost or the backend asked for a full rescan.
    Rescan,
}

const DEBOUNCE: Duration = Duration::from_millis(150);
const MAX_LATENCY: Duration = Duration::from_millis(1000);
#[cfg(target_os = "linux")]
const WALK_CMD_CHECK_INTERVAL: usize = 64;
#[cfg(target_os = "linux")]
const ENTRY_CMD_CHECK_INTERVAL: usize = 256;
#[cfg(target_os = "linux")]
const DYNAMIC_SYNTHETIC_LIMIT: usize = 64;

enum WatchCmd {
    SetRoots(Vec<PathBuf>),
    SetExcludes(ExcludeSet),
    /// Walk the roots again (the user raised the OS limit or wants a fresh registration).
    Resync,
    Shutdown,
}

#[derive(Default)]
struct Pending {
    map: HashMap<PathBuf, bool>, // true = removed
    order: Vec<PathBuf>,
    rescan: bool,
}

impl Pending {
    fn set(&mut self, p: PathBuf, removed: bool) {
        if self.map.insert(p.clone(), removed).is_none() {
            self.order.push(p);
        }
    }

    fn is_empty(&self) -> bool {
        self.map.is_empty() && !self.rescan
    }

    fn prune(&mut self, mut keep: impl FnMut(&Path) -> bool) {
        self.map.retain(|p, _| keep(p));
        self.order.retain(|p| self.map.contains_key(p));
    }

    fn take(&mut self) -> Vec<FsEvent> {
        let mut out = Vec::with_capacity(self.order.len() + 1);
        if std::mem::take(&mut self.rescan) {
            out.push(FsEvent::Rescan);
        }
        for p in self.order.drain(..) {
            if let Some(removed) = self.map.remove(&p) {
                out.push(if removed { FsEvent::Removed(p) } else { FsEvent::Changed(p) });
            }
        }
        self.map.clear();
        out
    }
}

fn matching_root<'a>(path: &'a Path, roots: &'a [PathBuf]) -> Option<(&'a PathBuf, &'a Path)> {
    let mut best: Option<(&PathBuf, &Path, usize)> = None;
    for r in roots {
        if let Ok(rel) = path.strip_prefix(r) {
            let depth = r.components().count();
            if best.is_none_or(|(_, _, d)| depth > d) {
                best = Some((r, rel, depth));
            }
        }
    }
    best.map(|(r, rel, _)| (r, rel))
}

fn ignored_rel(rel: &Path, excludes: &ExcludeSet, rel_buf: &mut String) -> bool {
    rel_buf.clear();
    let has_excludes = !excludes.is_empty();
    for comp in rel.components() {
        if let Component::Normal(os) = comp {
            let name = os.to_string_lossy();
            if name.starts_with('.') {
                return true;
            }
            if has_excludes {
                if !rel_buf.is_empty() {
                    rel_buf.push('/');
                }
                rel_buf.push_str(&name);
                if excludes.matches(rel_buf, &name) {
                    return true;
                }
            }
        }
    }
    false
}

/// True when `path` is outside `roots`, or any component below the matching root starts with '.' or matches `excludes`.
fn should_ignore(path: &Path, roots: &[PathBuf], excludes: &ExcludeSet, rel_buf: &mut String) -> bool {
    matching_root(path, roots).is_none_or(|(_, rel)| ignored_rel(rel, excludes, rel_buf))
}

/// True when any component below the root starts with '.'.
#[cfg(test)]
fn hidden_under_roots(path: &Path, roots: &[PathBuf]) -> bool {
    let mut scratch = String::new();
    should_ignore(path, roots, &ExcludeSet::default(), &mut scratch)
}

#[cfg(target_os = "linux")]
fn is_allowed_dir(dir: &Path, roots: &[PathBuf], excludes: &ExcludeSet, rel_buf: &mut String) -> bool {
    matching_root(dir, roots).is_some_and(|(_, rel)| !ignored_rel(rel, excludes, rel_buf))
}

fn is_max_watches(err: &notify::Error) -> bool {
    matches!(err.kind, ErrorKind::MaxFilesWatch)
}

#[cfg(target_os = "linux")]
fn rel_string(rel: &Path) -> String {
    let mut out = String::new();
    for comp in rel.components() {
        if let Component::Normal(os) = comp {
            if !out.is_empty() {
                out.push('/');
            }
            out.push_str(&os.to_string_lossy());
        }
    }
    out
}

fn absorb_event(
    ev: &notify::Event,
    pending: &mut Pending,
    roots: &[PathBuf],
    excludes: &ExcludeSet,
    rel_buf: &mut String,
) -> bool {
    let mut absorbed = false;
    if ev.need_rescan() {
        pending.rescan = true;
        absorbed = true;
    }
    match ev.kind {
        EventKind::Access(_) | EventKind::Modify(ModifyKind::Metadata(_)) => {}
        EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
            for p in &ev.paths {
                if !should_ignore(p, roots, excludes, rel_buf) {
                    pending.set(p.clone(), true);
                    absorbed = true;
                }
            }
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
            for (i, p) in ev.paths.iter().enumerate() {
                if !should_ignore(p, roots, excludes, rel_buf) {
                    pending.set(p.clone(), i == 0);
                    absorbed = true;
                }
            }
        }
        EventKind::Modify(ModifyKind::Name(_)) => {
            // Unknown rename side: decide by existence.
            for p in &ev.paths {
                if !should_ignore(p, roots, excludes, rel_buf) {
                    let gone = std::fs::symlink_metadata(p).is_err();
                    pending.set(p.clone(), gone);
                    absorbed = true;
                }
            }
        }
        _ => {
            for p in &ev.paths {
                if !should_ignore(p, roots, excludes, rel_buf) {
                    pending.set(p.clone(), false);
                    absorbed = true;
                }
            }
        }
    }
    absorbed
}

struct Engine {
    inner: RecommendedWatcher,
    roots: Vec<PathBuf>,
    excludes: ExcludeSet,
    pending: Pending,
    first: Option<Instant>,
    last: Option<Instant>,
    rel_scratch: String,
    #[cfg(target_os = "linux")]
    watched: FxHashSet<PathBuf>,
    #[cfg(target_os = "linux")]
    completed_roots: FxHashSet<PathBuf>,
    #[cfg(target_os = "linux")]
    max_watches_hit: bool,
    #[cfg(target_os = "linux")]
    needs_sync: bool,
    #[cfg(not(target_os = "linux"))]
    watched_roots: Vec<PathBuf>,
}

impl Engine {
    fn new(inner: RecommendedWatcher, roots: Vec<PathBuf>, excludes: ExcludeSet) -> Self {
        Self {
            inner,
            roots,
            excludes,
            pending: Pending::default(),
            first: None,
            last: None,
            rel_scratch: String::new(),
            #[cfg(target_os = "linux")]
            watched: FxHashSet::default(),
            #[cfg(target_os = "linux")]
            completed_roots: FxHashSet::default(),
            #[cfg(target_os = "linux")]
            max_watches_hit: false,
            #[cfg(target_os = "linux")]
            needs_sync: false,
            #[cfg(not(target_os = "linux"))]
            watched_roots: Vec::new(),
        }
    }

    fn limit_hit(&self) -> bool {
        #[cfg(target_os = "linux")]
        return self.max_watches_hit;
        #[cfg(not(target_os = "linux"))]
        false
    }

    fn prune_pending(&mut self) {
        let roots = &self.roots;
        let excludes = &self.excludes;
        let scratch = &mut self.rel_scratch;
        self.pending.prune(|p| !should_ignore(p, roots, excludes, scratch));
        if self.pending.is_empty() {
            self.first = None;
            self.last = None;
        }
    }

    fn apply_single_cmd(&mut self, cmd: WatchCmd) -> bool {
        match cmd {
            WatchCmd::SetRoots(roots) => {
                self.roots = roots;
                self.prune_pending();
                true
            }
            WatchCmd::SetExcludes(excludes) => {
                self.excludes = excludes;
                self.prune_pending();
                #[cfg(target_os = "linux")]
                self.completed_roots.clear();
                true
            }
            WatchCmd::Resync => {
                #[cfg(target_os = "linux")]
                self.completed_roots.clear();
                true
            }
            WatchCmd::Shutdown => false,
        }
    }

    fn drain_cmds(&mut self, first_cmd: WatchCmd, cmd_rx: &Receiver<WatchCmd>) -> bool {
        if !self.apply_single_cmd(first_cmd) {
            return false;
        }
        loop {
            match cmd_rx.try_recv() {
                Ok(cmd) => {
                    if !self.apply_single_cmd(cmd) {
                        return false;
                    }
                }
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => return false,
            }
        }
    }

    fn poll_cmds(&mut self, cmd_rx: &Receiver<WatchCmd>) -> PollCmd {
        match cmd_rx.try_recv() {
            Ok(cmd) => {
                if self.drain_cmds(cmd, cmd_rx) {
                    PollCmd::Updated
                } else {
                    PollCmd::Shutdown
                }
            }
            Err(TryRecvError::Empty) => PollCmd::None,
            Err(TryRecvError::Disconnected) => PollCmd::Shutdown,
        }
    }

    fn flush_if_max_latency(&mut self, on_events: &dyn Fn(Vec<FsEvent>)) {
        if self.first.is_some_and(|t| t.elapsed() >= MAX_LATENCY) {
            if !self.pending.is_empty() {
                on_events(self.pending.take());
            }
            self.first = None;
            self.last = None;
        }
    }

    fn mark_absorbed(&mut self) {
        let now = Instant::now();
        if self.first.is_none() && !self.pending.is_empty() {
            self.first = Some(now);
        }
        if !self.pending.is_empty() {
            self.last = Some(now);
        }
    }

    #[cfg(target_os = "linux")]
    fn untrack_removed_dir(&mut self, path: &Path) {
        if self.watched.remove(path) {
            self.watched.retain(|d| !d.starts_with(path));
            self.completed_roots.retain(|r| !r.starts_with(path));
            // A slot was freed: re-walk so folders the limit skipped get their watch (and the flag is recomputed).
            if self.max_watches_hit {
                self.needs_sync = true;
            }
            self.max_watches_hit = false;
        }
    }

    #[cfg(target_os = "linux")]
    fn check_walk_interrupt(
        &mut self,
        cmd_rx: &Receiver<WatchCmd>,
        on_events: &dyn Fn(Vec<FsEvent>),
    ) -> WalkInterrupt {
        match self.poll_cmds(cmd_rx) {
            PollCmd::None => {
                self.flush_if_max_latency(on_events);
                WalkInterrupt::None
            }
            PollCmd::Updated => WalkInterrupt::Restart,
            PollCmd::Shutdown => WalkInterrupt::Shutdown,
        }
    }

    #[cfg(target_os = "linux")]
    fn watch_dynamic_dir(
        &mut self,
        start: &Path,
        cmd_rx: &Receiver<WatchCmd>,
        on_events: &dyn Fn(Vec<FsEvent>),
    ) -> bool {
        let Some((root, rel_path)) = matching_root(start, &self.roots) else {
            return true;
        };
        if ignored_rel(rel_path, &self.excludes, &mut self.rel_scratch) {
            return true;
        }
        let root = root.clone();
        let mut queue = VecDeque::new();
        queue.push_back((start.to_path_buf(), rel_string(rel_path)));
        let mut visited = 0usize;
        let mut entries_seen = 0usize;
        let mut synthetic_count = 0usize;

        while let Some((dir, rel)) = queue.pop_front() {
            if visited != 0 && visited.is_multiple_of(WALK_CMD_CHECK_INTERVAL) {
                match self.check_walk_interrupt(cmd_rx, on_events) {
                    WalkInterrupt::None => {}
                    WalkInterrupt::Restart => {
                        self.needs_sync = true;
                        return true;
                    }
                    WalkInterrupt::Shutdown => return false,
                }
            }
            visited += 1;

            if self.max_watches_hit {
                break;
            }
            if !self.watched.contains(&dir) {
                match self.inner.watch(&dir, RecursiveMode::NonRecursive) {
                    Ok(()) => {
                        self.watched.insert(dir.clone());
                    }
                    Err(e) if is_max_watches(&e) => {
                        self.max_watches_hit = true;
                        break;
                    }
                    Err(_) => continue,
                }
            }

            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in rd.flatten() {
                entries_seen += 1;
                if entries_seen.is_multiple_of(ENTRY_CMD_CHECK_INTERVAL) {
                    match self.check_walk_interrupt(cmd_rx, on_events) {
                        WalkInterrupt::None => {}
                        WalkInterrupt::Restart => {
                            self.needs_sync = true;
                            return true;
                        }
                        WalkInterrupt::Shutdown => return false,
                    }
                }
                let name_os = entry.file_name();
                let name = name_os.to_string_lossy();
                if name.starts_with('.') {
                    continue;
                }
                let Ok(ft) = entry.file_type() else {
                    continue;
                };
                let child_rel = if rel.is_empty() { name.as_ref().to_owned() } else { format!("{rel}/{name}") };
                if !self.excludes.is_empty() && self.excludes.matches(&child_rel, &name) {
                    continue;
                }
                let path = entry.path();
                if ft.is_dir() {
                    if synthetic_count < DYNAMIC_SYNTHETIC_LIMIT {
                        self.pending.set(path.clone(), false);
                        synthetic_count += 1;
                    }
                    if path.starts_with(&root) {
                        queue.push_back((path, child_rel));
                    }
                } else if synthetic_count < DYNAMIC_SYNTHETIC_LIMIT && ft.is_file() {
                    // Emit files created in the microsecond race before the new dir's watch attached.
                    self.pending.set(path, false);
                    synthetic_count += 1;
                }
            }
        }
        true
    }

    #[cfg(target_os = "linux")]
    fn sync_watches(
        &mut self,
        cmd_rx: &Receiver<WatchCmd>,
        ev_rx: &Receiver<notify::Result<notify::Event>>,
        on_events: &dyn Fn(Vec<FsEvent>),
    ) -> bool {
        'sync: loop {
            self.needs_sync = false;
            match self.poll_cmds(cmd_rx) {
                PollCmd::None | PollCmd::Updated => {}
                PollCmd::Shutdown => return false,
            }

            let roots = &self.roots;
            let excludes = &self.excludes;
            let scratch = &mut self.rel_scratch;
            let mut unwatched_any = false;
            self.watched.retain(|dir| {
                if is_allowed_dir(dir, roots, excludes, scratch) {
                    true
                } else {
                    let _ = self.inner.unwatch(dir);
                    unwatched_any = true;
                    false
                }
            });
            self.completed_roots.retain(|r| roots.contains(r));
            if unwatched_any {
                self.max_watches_hit = false;
            }

            let mut queue = VecDeque::new();
            let mut walking_roots = Vec::new();
            for r in &self.roots {
                if self.completed_roots.contains(r) {
                    continue;
                }
                if std::fs::metadata(r).is_ok_and(|m| m.is_dir()) {
                    queue.push_back((r.clone(), String::new()));
                    walking_roots.push(r.clone());
                }
            }
            if queue.is_empty() {
                return true;
            }

            self.max_watches_hit = false;
            let mut visited = 0usize;
            let mut entries_seen = 0usize;

            while let Some((dir, rel)) = queue.pop_front() {
                if visited != 0 && visited.is_multiple_of(WALK_CMD_CHECK_INTERVAL) {
                    match self.check_walk_interrupt(cmd_rx, on_events) {
                        WalkInterrupt::None => {}
                        WalkInterrupt::Restart => continue 'sync,
                        WalkInterrupt::Shutdown => return false,
                    }
                    for _ in 0..64 {
                        match ev_rx.try_recv() {
                            Ok(res) => {
                                if !self.handle_event(res, cmd_rx, on_events) {
                                    return false;
                                }
                                if self.needs_sync {
                                    continue 'sync;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    self.flush_if_max_latency(on_events);
                }
                visited += 1;

                if self.max_watches_hit {
                    break;
                }
                if !self.watched.contains(&dir) {
                    match self.inner.watch(&dir, RecursiveMode::NonRecursive) {
                        Ok(()) => {
                            self.watched.insert(dir.clone());
                        }
                        Err(e) if is_max_watches(&e) => {
                            self.max_watches_hit = true;
                            break;
                        }
                        Err(_) => continue,
                    }
                }

                let Ok(rd) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in rd.flatten() {
                    entries_seen += 1;
                    if entries_seen.is_multiple_of(ENTRY_CMD_CHECK_INTERVAL) {
                        match self.check_walk_interrupt(cmd_rx, on_events) {
                            WalkInterrupt::None => {}
                            WalkInterrupt::Restart => continue 'sync,
                            WalkInterrupt::Shutdown => return false,
                        }
                    }
                    let name_os = entry.file_name();
                    let name = name_os.to_string_lossy();
                    if name.starts_with('.') {
                        continue;
                    }
                    let Ok(ft) = entry.file_type() else {
                        continue;
                    };
                    if !ft.is_dir() {
                        continue;
                    }
                    let child_rel = if rel.is_empty() { name.as_ref().to_owned() } else { format!("{rel}/{name}") };
                    if !self.excludes.is_empty() && self.excludes.matches(&child_rel, &name) {
                        continue;
                    }
                    queue.push_back((entry.path(), child_rel));
                }
            }

            if !self.max_watches_hit {
                self.completed_roots.extend(walking_roots);
            }
            return true;
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn sync_watches(
        &mut self,
        cmd_rx: &Receiver<WatchCmd>,
        _ev_rx: &Receiver<notify::Result<notify::Event>>,
        _on_events: &dyn Fn(Vec<FsEvent>),
    ) -> bool {
        'sync: loop {
            match self.poll_cmds(cmd_rx) {
                PollCmd::None => {}
                PollCmd::Updated => {}
                PollCmd::Shutdown => return false,
            }

            let roots = &self.roots;
            let inner = &mut self.inner;
            self.watched_roots.retain(|old| {
                if roots.contains(old) {
                    true
                } else {
                    let _ = inner.unwatch(old);
                    false
                }
            });

            let wanted = self.roots.clone();
            for r in wanted {
                match self.poll_cmds(cmd_rx) {
                    PollCmd::None => {}
                    PollCmd::Updated => continue 'sync,
                    PollCmd::Shutdown => return false,
                }
                if !self.watched_roots.contains(&r) && self.inner.watch(&r, RecursiveMode::Recursive).is_ok() {
                    self.watched_roots.push(r);
                }
            }
            return true;
        }
    }

    fn handle_event(
        &mut self,
        res: notify::Result<notify::Event>,
        cmd_rx: &Receiver<WatchCmd>,
        on_events: &dyn Fn(Vec<FsEvent>),
    ) -> bool {
        let ev = match res {
            Ok(e) => e,
            Err(e) => {
                if is_max_watches(&e) {
                    #[cfg(target_os = "linux")]
                    {
                        self.max_watches_hit = true;
                    }
                } else {
                    self.pending.rescan = true;
                    self.mark_absorbed();
                }
                return true;
            }
        };

        if absorb_event(&ev, &mut self.pending, &self.roots, &self.excludes, &mut self.rel_scratch) {
            self.mark_absorbed();
        }

        #[cfg(target_os = "linux")]
        {
            match ev.kind {
                EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
                    for p in &ev.paths {
                        self.untrack_removed_dir(p);
                    }
                }
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
                    if let Some(old) = ev.paths.first() {
                        self.untrack_removed_dir(old);
                    }
                    if let Some(new_p) = ev.paths.get(1) {
                        if !self.max_watches_hit
                            && !self.watched.contains(new_p)
                            && !should_ignore(new_p, &self.roots, &self.excludes, &mut self.rel_scratch)
                            && std::fs::symlink_metadata(new_p).is_ok_and(|m| m.is_dir())
                        {
                            if !self.watch_dynamic_dir(new_p, cmd_rx, on_events) {
                                return false;
                            }
                            if !self.pending.is_empty() {
                                self.mark_absorbed();
                            }
                        }
                    }
                }
                EventKind::Create(CreateKind::Folder | CreateKind::Any)
                | EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
                    for p in &ev.paths {
                        if self.needs_sync {
                            break;
                        }
                        if !self.max_watches_hit
                            && !self.watched.contains(p)
                            && !should_ignore(p, &self.roots, &self.excludes, &mut self.rel_scratch)
                            && std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
                        {
                            if !self.watch_dynamic_dir(p, cmd_rx, on_events) {
                                return false;
                            }
                            if !self.pending.is_empty() {
                                self.mark_absorbed();
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (cmd_rx, on_events);
        }

        true
    }
}

enum PollCmd {
    None,
    Updated,
    Shutdown,
}

#[cfg(target_os = "linux")]
enum WalkInterrupt {
    None,
    Restart,
    Shutdown,
}

#[allow(clippy::too_many_arguments)]
fn run_loop(
    inner: RecommendedWatcher,
    roots: Vec<PathBuf>,
    excludes: ExcludeSet,
    cmd_rx: Receiver<WatchCmd>,
    ev_rx: Receiver<notify::Result<notify::Event>>,
    on_events: impl Fn(Vec<FsEvent>),
    registered: Arc<AtomicBool>,
    limit: Arc<AtomicBool>,
) {
    let mut engine = Engine::new(inner, roots, excludes);
    if !engine.sync_watches(&cmd_rx, &ev_rx, &on_events) {
        if !engine.pending.is_empty() {
            on_events(engine.pending.take());
        }
        return;
    }
    limit.store(engine.limit_hit(), Ordering::Release);
    registered.store(true, Ordering::Release);
    if engine.limit_hit() {
        on_events(Vec::new()); // wakes the UI so the notice shows without waiting for a repaint
    }

    'outer: loop {
        let hit = engine.limit_hit();
        if limit.swap(hit, Ordering::Relaxed) != hit {
            on_events(Vec::new());
        }
        #[cfg(target_os = "linux")]
        if engine.needs_sync && !engine.sync_watches(&cmd_rx, &ev_rx, &on_events) {
            break 'outer;
        }

        let timeout = match (engine.first, engine.last) {
            (Some(first), Some(last)) => {
                let d_rem = DEBOUNCE.saturating_sub(last.elapsed());
                let m_rem = MAX_LATENCY.saturating_sub(first.elapsed());
                d_rem.min(m_rem)
            }
            _ => Duration::from_secs(3600),
        };

        let mut sel = Select::new_biased();
        let cmd_idx = sel.recv(&cmd_rx);
        let ev_idx = sel.recv(&ev_rx);
        match sel.select_timeout(timeout) {
            Ok(oper) if oper.index() == cmd_idx => match oper.recv(&cmd_rx) {
                Ok(cmd) => {
                    if !engine.drain_cmds(cmd, &cmd_rx) || !engine.sync_watches(&cmd_rx, &ev_rx, &on_events) {
                        break 'outer;
                    }
                }
                Err(_) => break 'outer,
            },
            Ok(oper) if oper.index() == ev_idx => match oper.recv(&ev_rx) {
                Ok(res) => {
                    if !engine.handle_event(res, &cmd_rx, &on_events) {
                        break 'outer;
                    }
                    for _ in 0..64 {
                        #[cfg(target_os = "linux")]
                        if engine.needs_sync {
                            break;
                        }
                        if !cmd_rx.is_empty() {
                            break;
                        }
                        match ev_rx.try_recv() {
                            Ok(next) => {
                                if !engine.handle_event(next, &cmd_rx, &on_events) {
                                    break 'outer;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    engine.flush_if_max_latency(&on_events);
                }
                Err(_) => break 'outer,
            },
            Ok(_) => unreachable!(),
            Err(_) => {
                if !engine.pending.is_empty() {
                    on_events(engine.pending.take());
                }
                engine.first = None;
                engine.last = None;
            }
        }
    }

    if !engine.pending.is_empty() {
        on_events(engine.pending.take());
    }
}

pub struct Watcher {
    cmd_tx: Sender<WatchCmd>,
    registered: Arc<AtomicBool>,
    limit: Arc<AtomicBool>,
}

impl Watcher {
    /// `None` when no backend is available. Events are debounced (~150 ms) and coalesced;
    /// `on_events` runs on a background thread.
    pub fn new(roots: &[PathBuf], on_events: impl Fn(Vec<FsEvent>) + Send + 'static) -> Option<Watcher> {
        Self::new_with_excludes(roots, ExcludeSet::default(), on_events)
    }

    /// Like [`Watcher::new`], also skipping and filtering paths matching `excludes`.
    pub fn new_with_excludes(
        roots: &[PathBuf],
        excludes: ExcludeSet,
        on_events: impl Fn(Vec<FsEvent>) + Send + 'static,
    ) -> Option<Watcher> {
        let (cmd_tx, cmd_rx) = unbounded::<WatchCmd>();
        let (ev_tx, ev_rx) = unbounded::<notify::Result<notify::Event>>();
        let inner = notify::recommended_watcher(move |res| {
            let _ = ev_tx.send(res);
        })
        .ok()?;
        let init_roots = roots.to_vec();
        let registered = Arc::new(AtomicBool::new(false));
        let flag = registered.clone();
        let limit = Arc::new(AtomicBool::new(false));
        let limit_flag = limit.clone();
        std::thread::Builder::new()
            .name("iron-sheets-watch".into())
            .spawn(move || {
                run_loop(inner, init_roots, excludes, cmd_rx, ev_rx, on_events, flag, limit_flag);
            })
            .ok()?;
        Some(Watcher { cmd_tx, registered, limit })
    }

    /// The initial registration of the root folders has finished (startup diagnostics).
    pub fn registered(&self) -> bool {
        self.registered.load(Ordering::Acquire)
    }

    /// The OS ran out of watch slots (Linux `fs.inotify.max_user_watches`): changes in some folders are not detected.
    /// Updated after each registration pass and event batch; always false elsewhere.
    pub fn limit_hit(&self) -> bool {
        self.limit.load(Ordering::Relaxed)
    }

    /// Register the roots again; folders that did not fit before are tried once more.
    pub fn resync(&self) {
        let _ = self.cmd_tx.send(WatchCmd::Resync);
    }

    pub fn set_roots(&mut self, roots: &[PathBuf]) {
        let _ = self.cmd_tx.send(WatchCmd::SetRoots(roots.to_vec()));
    }

    pub fn set_excludes(&mut self, excludes: ExcludeSet) {
        let _ = self.cmd_tx.send(WatchCmd::SetExcludes(excludes));
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(WatchCmd::Shutdown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn wait_for(rx: &mpsc::Receiver<Vec<FsEvent>>, pred: impl Fn(&FsEvent) -> bool, secs: u64) -> bool {
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end {
            if let Ok(v) = rx.recv_timeout(Duration::from_millis(100)) {
                if v.iter().any(&pred) {
                    return true;
                }
            }
        }
        false
    }

    fn drain_events(rx: &mpsc::Receiver<Vec<FsEvent>>) -> Vec<FsEvent> {
        let mut all = Vec::new();
        while let Ok(v) = rx.try_recv() {
            all.extend(v);
        }
        all
    }

    #[test]
    fn pending_coalesces() {
        let mut p = Pending::default();
        p.set(PathBuf::from("a"), false);
        p.set(PathBuf::from("b"), false);
        p.set(PathBuf::from("a"), true);
        assert_eq!(p.take(), vec![FsEvent::Removed("a".into()), FsEvent::Changed("b".into())]);
        assert!(p.is_empty());
    }

    #[test]
    fn hidden_filter() {
        let roots = vec![PathBuf::from("/r")];
        assert!(hidden_under_roots(Path::new("/r/.git/x"), &roots));
        assert!(!hidden_under_roots(Path::new("/r/a/x"), &roots));
    }

    #[test]
    fn ignores_hidden_and_excluded_paths() {
        let roots = vec![PathBuf::from("/tmp/.ws/root")];
        let excludes = ExcludeSet::new(&["node_modules", "*.tmp", "build/out"]);
        let mut scratch = String::new();
        assert!(!should_ignore(Path::new("/tmp/.ws/root/notes/a.md"), &roots, &excludes, &mut scratch));
        assert!(should_ignore(Path::new("/tmp/.ws/root/.git/HEAD"), &roots, &excludes, &mut scratch));
        assert!(should_ignore(Path::new("/tmp/.ws/root/node_modules/pkg/index.js"), &roots, &excludes, &mut scratch));
        assert!(should_ignore(Path::new("/tmp/.ws/root/build/out/a/b.md"), &roots, &excludes, &mut scratch));
        assert!(should_ignore(Path::new("/tmp/.ws/root/notes/draft.tmp"), &roots, &excludes, &mut scratch));
    }

    #[test]
    fn detects_changes() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().to_path_buf();
        let (tx, rx) = mpsc::channel();
        let tx = parking_lot::Mutex::new(tx);
        let Some(mut w) = Watcher::new(std::slice::from_ref(&root), move |e| {
            let _ = tx.lock().send(e);
        }) else {
            eprintln!("no watcher backend; skipping");
            return;
        };
        std::thread::sleep(Duration::from_millis(200));
        let f = root.join("new.md");
        std::fs::write(&f, "x").unwrap();
        assert!(wait_for(&rx, |e| matches!(e, FsEvent::Changed(p) if p.ends_with("new.md")), 5));
        std::fs::remove_file(&f).unwrap();
        assert!(wait_for(&rx, |e| matches!(e, FsEvent::Removed(p) if p.ends_with("new.md")), 5));
        w.set_roots(&[]);
        w.set_roots(&[root]);
    }

    #[test]
    fn new_and_set_roots_do_not_block_on_large_tree() {
        let t = tempfile::tempdir().unwrap();
        let root_a = t.path().join("root_a");
        let root_b = t.path().join("root_b");
        std::fs::create_dir_all(&root_a).unwrap();
        std::fs::create_dir_all(&root_b).unwrap();

        // Build ~360 subdirs in root_a (300 normal + .git + node_modules) and 100 in root_b.
        for parent_idx in 0..15 {
            let dir_a = root_a.join(format!("dir_{parent_idx:02}"));
            for sub_idx in 0..20 {
                std::fs::create_dir_all(dir_a.join(format!("sub_{sub_idx:02}"))).unwrap();
            }
        }
        for parent_idx in 0..10 {
            let dir_b = root_b.join(format!("dir_{parent_idx:02}"));
            for sub_idx in 0..10 {
                std::fs::create_dir_all(dir_b.join(format!("sub_{sub_idx:02}"))).unwrap();
            }
        }
        for i in 0..30 {
            std::fs::create_dir_all(root_a.join(format!(".git/objects/{i:02}"))).unwrap();
            std::fs::create_dir_all(root_a.join(format!("node_modules/pkg_{i:02}"))).unwrap();
        }
        let normal_file = root_a.join("dir_00/sub_00/note.md");
        let excluded_file = root_a.join("node_modules/pkg_00/index.js");
        let hidden_file = root_a.join(".git/HEAD");
        let tmp_file = root_a.join("dir_00/scratch.tmp");
        let sentinel_file = root_b.join("dir_01/sub_01/sentinel.md");
        std::fs::write(&normal_file, "v1").unwrap();
        std::fs::write(&excluded_file, "v1").unwrap();
        std::fs::write(&hidden_file, "ref: refs/heads/main\n").unwrap();

        let excludes = ExcludeSet::new(&["node_modules", "*.tmp"]);
        let (tx, rx) = mpsc::channel();
        let tx = parking_lot::Mutex::new(tx);

        let t_new_start = Instant::now();
        let Some(mut w) = Watcher::new_with_excludes(std::slice::from_ref(&root_a), excludes, move |e| {
            let _ = tx.lock().send(e);
        }) else {
            eprintln!("no watcher backend; skipping");
            return;
        };
        let new_elapsed = t_new_start.elapsed();

        let t_set_start = Instant::now();
        w.set_roots(&[root_a.clone(), root_b.clone()]);
        let set_roots_elapsed = t_set_start.elapsed();

        eprintln!(
            "watcher timings (~360 dirs): Watcher::new_with_excludes={:?}, Watcher::set_roots={:?}",
            new_elapsed, set_roots_elapsed
        );
        assert!(new_elapsed < Duration::from_millis(50), "Watcher::new blocked for {:?}", new_elapsed);
        assert!(set_roots_elapsed < Duration::from_millis(50), "Watcher::set_roots blocked for {:?}", set_roots_elapsed);

        // Give the background thread a moment to finish initial registration over the ~300 dirs.
        std::thread::sleep(Duration::from_millis(250));
        drain_events(&rx);

        // 1. Modifying a normal file delivers FsEvent::Changed.
        std::fs::write(&normal_file, "v2").unwrap();
        assert!(wait_for(&rx, |e| matches!(e, FsEvent::Changed(p) if p == &normal_file), 5));

        // 2. Creating a new subdirectory and modifying a file inside it delivers events.
        let fresh_dir = root_b.join("fresh_sub/nested");
        std::fs::create_dir_all(&fresh_dir).unwrap();
        let fresh_file = fresh_dir.join("fresh.md");
        std::fs::write(&fresh_file, "first").unwrap();
        assert!(wait_for(&rx, |e| matches!(e, FsEvent::Changed(p) if p == &fresh_file), 5));
        std::fs::write(&fresh_file, "second").unwrap();
        assert!(wait_for(&rx, |e| matches!(e, FsEvent::Changed(p) if p == &fresh_file), 5));

        // 3. Modifying excluded/hidden paths produces NO events while sentinel normal file does.
        std::thread::sleep(Duration::from_millis(200));
        drain_events(&rx);
        std::fs::write(&excluded_file, "v2").unwrap();
        std::fs::write(&hidden_file, "ref: refs/heads/other\n").unwrap();
        std::fs::write(&tmp_file, "tmp").unwrap();
        let new_excluded_sub = root_a.join("node_modules/new_pkg");
        std::fs::create_dir_all(&new_excluded_sub).unwrap();
        std::fs::write(new_excluded_sub.join("mod.js"), "export {}").unwrap();
        std::fs::write(&sentinel_file, "ping").unwrap();

        assert!(wait_for(&rx, |e| matches!(e, FsEvent::Changed(p) if p == &sentinel_file), 5));
        std::thread::sleep(Duration::from_millis(250));
        let all = drain_events(&rx);
        for ev in &all {
            if let FsEvent::Changed(p) | FsEvent::Removed(p) = ev {
                assert!(
                    !p.starts_with(root_a.join("node_modules"))
                        && !p.starts_with(root_a.join(".git"))
                        && p != &tmp_file,
                    "unexpected event for excluded/hidden path: {ev:?}"
                );
            }
        }
    }

    #[test]
    fn set_roots_cancels_mid_walk_and_unwatches_removed_roots() {
        let t = tempfile::tempdir().unwrap();
        let r1 = t.path().join("r1");
        let r2 = t.path().join("r2");
        for i in 0..15 {
            for j in 0..20 {
                std::fs::create_dir_all(r1.join(format!("d{i:02}/s{j:02}"))).unwrap();
            }
        }
        std::fs::create_dir_all(&r2).unwrap();
        let f1 = r1.join("d00/s00/a.md");
        let f2 = r2.join("b.md");
        std::fs::write(&f1, "1").unwrap();
        std::fs::write(&f2, "1").unwrap();

        let (tx, rx) = mpsc::channel();
        let tx = parking_lot::Mutex::new(tx);
        let Some(mut w) = Watcher::new(std::slice::from_ref(&r1), move |e| {
            let _ = tx.lock().send(e);
        }) else {
            eprintln!("no watcher backend; skipping");
            return;
        };
        // Immediately switch roots while r1 walk is still running or queued.
        w.set_roots(std::slice::from_ref(&r2));
        std::thread::sleep(Duration::from_millis(250));
        drain_events(&rx);

        std::fs::write(&f1, "2").unwrap();
        std::fs::write(&f2, "2").unwrap();
        assert!(wait_for(&rx, |e| matches!(e, FsEvent::Changed(p) if p == &f2), 5));
        std::thread::sleep(Duration::from_millis(250));
        let extra = drain_events(&rx);
        assert!(
            !extra.iter().any(|e| matches!(e, FsEvent::Changed(p) | FsEvent::Removed(p) if p.starts_with(&r1))),
            "unexpected event from unwatched root r1: {extra:?}"
        );
    }

    #[test]
    fn set_excludes_filters_subsequent_events() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().to_path_buf();
        let drafts = root.join("drafts");
        std::fs::create_dir_all(&drafts).unwrap();
        let draft_note = drafts.join("a.md");
        let visible_note = root.join("visible.md");
        std::fs::write(&draft_note, "1").unwrap();
        std::fs::write(&visible_note, "1").unwrap();

        let (tx, rx) = mpsc::channel();
        let tx = parking_lot::Mutex::new(tx);
        let Some(mut w) = Watcher::new(std::slice::from_ref(&root), move |e| {
            let _ = tx.lock().send(e);
        }) else {
            eprintln!("no watcher backend; skipping");
            return;
        };
        std::thread::sleep(Duration::from_millis(200));
        drain_events(&rx);

        w.set_excludes(ExcludeSet::new(&["drafts"]));
        std::thread::sleep(Duration::from_millis(200));
        drain_events(&rx);

        std::fs::write(&draft_note, "2").unwrap();
        std::fs::write(&visible_note, "2").unwrap();
        assert!(wait_for(&rx, |e| matches!(e, FsEvent::Changed(p) if p == &visible_note), 5));
        std::thread::sleep(Duration::from_millis(250));
        let extra = drain_events(&rx);
        assert!(
            !extra.iter().any(|e| matches!(e, FsEvent::Changed(p) | FsEvent::Removed(p) if p == &draft_note)),
            "unexpected event for newly excluded path: {extra:?}"
        );
    }

    #[test]
    fn max_files_watch_error_does_not_trigger_rescan() {
        let Some(inner) = notify::recommended_watcher(|_| {}).ok() else {
            eprintln!("no watcher backend; skipping");
            return;
        };
        let mut engine = Engine::new(inner, Vec::new(), ExcludeSet::default());
        let (_cmd_tx, cmd_rx) = unbounded();
        let on_events = |_: Vec<FsEvent>| {};
        assert!(engine.handle_event(Err(notify::Error::new(ErrorKind::MaxFilesWatch)), &cmd_rx, &on_events));
        #[cfg(target_os = "linux")]
        assert!(engine.max_watches_hit);
        assert!(engine.pending.is_empty());
        assert!(engine.handle_event(Err(notify::Error::generic("other")), &cmd_rx, &on_events));
        assert_eq!(engine.pending.take(), vec![FsEvent::Rescan]);
    }

    #[test]
    fn set_roots_prunes_pending_and_ignores_late_events_from_removed_roots() {
        let t = tempfile::tempdir().unwrap();
        let r1 = t.path().join("r1");
        let r2 = t.path().join("r2");
        std::fs::create_dir_all(&r1).unwrap();
        std::fs::create_dir_all(&r2).unwrap();

        let mut scratch = String::new();
        assert!(should_ignore(&r1.join("a.md"), &[], &ExcludeSet::default(), &mut scratch));
        assert!(should_ignore(&r1.join("a.md"), std::slice::from_ref(&r2), &ExcludeSet::default(), &mut scratch));
        assert!(!should_ignore(&r2.join("b.md"), std::slice::from_ref(&r2), &ExcludeSet::default(), &mut scratch));

        let Some(inner) = notify::recommended_watcher(|_| {}).ok() else {
            eprintln!("no watcher backend; skipping");
            return;
        };
        let mut engine = Engine::new(inner, vec![r1.clone(), r2.clone()], ExcludeSet::default());
        let (_cmd_tx, cmd_rx) = unbounded();
        let on_events = |_: Vec<FsEvent>| {};

        let ev_r1 = notify::Event::new(EventKind::Modify(ModifyKind::Any)).add_path(r1.join("stale.md"));
        let ev_r2 = notify::Event::new(EventKind::Modify(ModifyKind::Any)).add_path(r2.join("keep.md"));
        assert!(engine.handle_event(Ok(ev_r1), &cmd_rx, &on_events));
        assert!(engine.handle_event(Ok(ev_r2), &cmd_rx, &on_events));
        assert!(!engine.pending.is_empty());

        // Remove r1 while keeping r2: stale.md from r1 is pruned, keep.md stays.
        assert!(engine.apply_single_cmd(WatchCmd::SetRoots(vec![r2.clone()])));
        assert_eq!(engine.pending.take(), vec![FsEvent::Changed(r2.join("keep.md"))]);

        // Late event arriving from removed r1 is filtered out by should_ignore.
        let late_r1 = notify::Event::new(EventKind::Modify(ModifyKind::Any)).add_path(r1.join("late.md"));
        assert!(engine.handle_event(Ok(late_r1), &cmd_rx, &on_events));
        assert!(engine.pending.is_empty());

        // Pending only from r1, then set_roots(&[]) clears pending and timestamps and ignores late r1 events.
        assert!(engine.apply_single_cmd(WatchCmd::SetRoots(vec![r1.clone()])));
        assert!(engine.handle_event(Ok(notify::Event::new(EventKind::Modify(ModifyKind::Any)).add_path(r1.join("only_r1.md"))), &cmd_rx, &on_events));
        assert!(engine.first.is_some() && engine.last.is_some());
        assert!(engine.apply_single_cmd(WatchCmd::SetRoots(Vec::new())));
        assert!(engine.pending.is_empty());
        assert!(engine.first.is_none() && engine.last.is_none());
        assert!(engine.handle_event(Ok(notify::Event::new(EventKind::Modify(ModifyKind::Any)).add_path(r1.join("after_empty.md"))), &cmd_rx, &on_events));
        assert!(engine.pending.is_empty());
    }

    #[test]
    fn run_loop_drops_removed_root_events_on_set_roots() {
        let t = tempfile::tempdir().unwrap();
        let r1 = t.path().join("r1");
        let r2 = t.path().join("r2");
        std::fs::create_dir_all(&r1).unwrap();
        std::fs::create_dir_all(&r2).unwrap();

        let Some(inner) = notify::recommended_watcher(|_| {}).ok() else {
            eprintln!("no watcher backend; skipping");
            return;
        };
        let (cmd_tx, cmd_rx) = unbounded();
        let (ev_tx, ev_rx) = unbounded();
        let (out_tx, out_rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            run_loop(inner, vec![r1.clone()], ExcludeSet::default(), cmd_rx, ev_rx, move |evs| {
                let _ = out_tx.send(evs);
            }, Default::default(), Default::default());
        });

        // Queue a pending event on r1, immediately switch roots to r2, then send a late r1 event and an r2 event.
        let r1_stale = t.path().join("r1/stale.md");
        let r1_late = t.path().join("r1/late.md");
        let r2_keep = r2.join("keep.md");
        ev_tx.send(Ok(notify::Event::new(EventKind::Modify(ModifyKind::Any)).add_path(r1_stale.clone()))).unwrap();
        cmd_tx.send(WatchCmd::SetRoots(vec![r2.clone()])).unwrap();
        ev_tx.send(Ok(notify::Event::new(EventKind::Modify(ModifyKind::Any)).add_path(r1_late.clone()))).unwrap();
        ev_tx.send(Ok(notify::Event::new(EventKind::Modify(ModifyKind::Any)).add_path(r2_keep.clone()))).unwrap();

        assert!(wait_for(&out_rx, |e| matches!(e, FsEvent::Changed(p) if p == &r2_keep), 5));
        cmd_tx.send(WatchCmd::Shutdown).unwrap();
        handle.join().unwrap();

        let all = drain_events(&out_rx);
        assert!(
            !all.iter().any(|e| matches!(e, FsEvent::Changed(p) | FsEvent::Removed(p) if p == &r1_stale || p == &r1_late)),
            "stale/late events from removed root leaked: {all:?}"
        );
    }
}
