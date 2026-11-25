//! Modal dialog data: text prompt, confirm, unsaved-changes, built-in folder browser. Widgets and the native pickers
//! (which send `Pick` results through `Dialogs::tx`) live in the frontend.
use crate::command::Command;
use crossbeam_channel::{Receiver, Sender, unbounded};
use parking_lot::{Condvar, Mutex};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// What happens once an untitled note has been saved to its final place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterSave {
    Nothing,
    /// Close the tab (the user chose Save while closing it).
    CloseTab,
    /// Continue quitting (the user chose Save while quitting).
    Quit,
}

#[derive(Debug, Clone)]
pub enum PromptAction {
    /// Save the untitled note `path` as `<dir>/<name>`.
    SaveUntitled {
        path: PathBuf,
        dir: PathBuf,
        after: AfterSave,
    },
    NewNote {
        dir: PathBuf,
    },
    NewFolder {
        parent: PathBuf,
    },
    Rename {
        path: PathBuf,
    },
    NewWorkspace,
    RenameWorkspace,
}

pub enum Dialog {
    Prompt {
        title: String,
        label: String,
        value: String,
        action: PromptAction,
        error: Option<String>,
        focus: bool,
        /// Target folders to choose from (label, path) and the selected one; empty: no choice (the action has its folder).
        dirs: Vec<(String, PathBuf)>,
        dir_sel: usize,
    },
    Confirm {
        title: String,
        message: String,
        ok_label: String,
        on_ok: Vec<Command>,
        /// A second way forward next to OK (label, commands); Cancel still does nothing.
        alt: Option<(String, Vec<Command>)>,
        /// "Do not show again" checkbox: (checked, commands appended to OK when checked).
        dont_ask: Option<(bool, Vec<Command>)>,
        focus: bool,
    },
    /// Save / Discard / Cancel.
    Unsaved {
        message: String,
        save: Vec<Command>,
        discard: Vec<Command>,
    },
    FolderBrowser {
        path_text: String,
        dir: PathBuf,
        error: Option<String>,
        listing: Option<(PathBuf, Vec<(String, PathBuf)>)>,
    },
}

pub enum Pick {
    Folder(PathBuf),
    Files(Vec<PathBuf>),
    /// The native folder dialog could not be shown (no portal); fall back to the built-in browser.
    Unavailable,
    /// The native file picker could not be shown (no portal); fall back to Quick Open / Add Folder.
    FilesUnavailable,
    Cancelled,
}

/// Result of the native "save as" dialog for an untitled note.
#[derive(Debug, Clone)]
pub enum SavePick {
    To {
        from: PathBuf,
        to: PathBuf,
        after: AfterSave,
    },
    /// No native dialog could be shown; use the in-app prompt.
    Unavailable {
        from: PathBuf,
        after: AfterSave,
    },
    Cancelled,
}

pub struct Dialogs {
    pub current: Option<Dialog>,
    pub tx: Sender<Pick>,
    pub rx: Receiver<Pick>,
    pub picking: bool,
    /// Native "save as" results; the frontend turns them into commands.
    pub save_tx: Sender<SavePick>,
    pub save_rx: Receiver<SavePick>,
    /// False once the platform showed it cannot open a native dialog.
    pub native_ok: bool,
    dir_loader: DirLoader,
}

impl Default for Dialogs {
    fn default() -> Self {
        let (tx, rx) = unbounded();
        let (save_tx, save_rx) = unbounded();
        Dialogs { current: None, tx, rx, picking: false, save_tx, save_rx, native_ok: true, dir_loader: DirLoader::default() }
    }
}

pub type DirListing = (PathBuf, Result<Vec<(String, PathBuf)>, String>);

struct DirReq {
    seq: u64,
    dir: PathBuf,
    wake: Arc<dyn Fn() + Send + Sync>,
}

struct DirRes {
    seq: u64,
    dir: PathBuf,
    subdirs: Result<Vec<(String, PathBuf)>, String>,
}

struct DirShared {
    pending: Mutex<Option<DirReq>>,
    wake: Condvar,
    active_seq: AtomicU64,
    closed: AtomicBool,
    paused: AtomicBool,
    res_tx: Sender<DirRes>,
}

struct DirLoader {
    shared: Arc<DirShared>,
    res_rx: Receiver<DirRes>,
    in_flight: Option<(u64, PathBuf)>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Default for DirLoader {
    fn default() -> Self {
        let (res_tx, res_rx) = unbounded();
        DirLoader {
            shared: Arc::new(DirShared {
                pending: Mutex::new(None),
                wake: Condvar::new(),
                active_seq: AtomicU64::new(0),
                closed: AtomicBool::new(false),
                paused: AtomicBool::new(false),
                res_tx,
            }),
            res_rx,
            in_flight: None,
            worker: None,
        }
    }
}

impl Drop for DirLoader {
    fn drop(&mut self) {
        self.shared.active_seq.fetch_add(1, Ordering::AcqRel);
        {
            let mut p = self.shared.pending.lock();
            *p = None;
            self.shared.closed.store(true, Ordering::Release);
            self.shared.wake.notify_all();
        }
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

fn dir_worker(sh: Arc<DirShared>) {
    loop {
        let req = {
            let mut p = sh.pending.lock();
            loop {
                if sh.closed.load(Ordering::Acquire) {
                    return;
                }
                if !sh.paused.load(Ordering::Acquire) {
                    if let Some(r) = p.take() {
                        break r;
                    }
                }
                sh.wake.wait(&mut p);
            }
        };
        if sh.active_seq.load(Ordering::Acquire) != req.seq {
            continue;
        }
        let subdirs = match std::fs::read_dir(&req.dir) {
            Ok(rd) => {
                let mut v = Vec::new();
                let mut cancelled = false;
                for (idx, entry) in rd.enumerate() {
                    if idx.is_multiple_of(64) && (sh.closed.load(Ordering::Acquire) || sh.active_seq.load(Ordering::Acquire) != req.seq) {
                        cancelled = true;
                        break;
                    }
                    let Ok(e) = entry else { continue };
                    if e.file_type().is_ok_and(|t| t.is_dir()) {
                        let name = e.file_name().to_string_lossy().into_owned();
                        if !name.starts_with('.') {
                            v.push((name, e.path()));
                        }
                    }
                }
                if cancelled || sh.active_seq.load(Ordering::Acquire) != req.seq {
                    continue;
                }
                v.sort_by_key(|(n, _)| n.to_lowercase());
                Ok(v)
            }
            Err(e) => Err(e.to_string()),
        };
        if sh.active_seq.load(Ordering::Acquire) == req.seq {
            let _ = sh.res_tx.send(DirRes { seq: req.seq, dir: req.dir, subdirs });
            (req.wake)();
        }
    }
}

impl DirLoader {
    fn request(&mut self, dir: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) {
        let cur_seq = self.shared.active_seq.load(Ordering::Acquire);
        if self.in_flight.as_ref().is_some_and(|(g, p)| *g == cur_seq && *p == dir) {
            return;
        }
        let seq = self.shared.active_seq.fetch_add(1, Ordering::AcqRel) + 1;
        self.in_flight = Some((seq, dir.clone()));
        if self.worker.is_none() {
            let sh = self.shared.clone();
            if let Ok(h) = std::thread::Builder::new().name("iron-sheets-dir".into()).spawn(move || dir_worker(sh)) {
                self.worker = Some(h);
            } else {
                self.in_flight = None;
                let _ = self.shared.res_tx.send(DirRes { seq, dir, subdirs: Err("cannot start directory reader".into()) });
                wake();
                return;
            }
        }
        *self.shared.pending.lock() = Some(DirReq { seq, dir, wake });
        self.shared.wake.notify_one();
    }

    fn cancel(&mut self) {
        self.shared.active_seq.fetch_add(1, Ordering::AcqRel);
        *self.shared.pending.lock() = None;
        self.in_flight = None;
    }

    fn poll(&mut self) -> Option<DirListing> {
        let active = self.shared.active_seq.load(Ordering::Acquire);
        let mut latest = None;
        while let Ok(res) = self.res_rx.try_recv() {
            if res.seq == active {
                latest = Some((res.dir, res.subdirs));
                self.in_flight = None;
            }
        }
        latest
    }
}

impl Dialogs {
    pub fn prompt(&mut self, title: &str, label: &str, value: &str, action: PromptAction) {
        self.current = Some(Dialog::Prompt {
            title: title.into(),
            label: label.into(),
            value: value.into(),
            action,
            error: None,
            focus: true,
            dirs: Vec::new(),
            dir_sel: 0,
        });
    }

    /// Prompt with a choice of target folders (the first one is selected).
    pub fn prompt_in_folders(&mut self, title: &str, label: &str, value: &str, action: PromptAction, dirs: Vec<(String, PathBuf)>) {
        self.current =
            Some(Dialog::Prompt { title: title.into(), label: label.into(), value: value.into(), action, error: None, focus: true, dirs, dir_sel: 0 });
    }

    pub fn confirm(&mut self, title: &str, message: String, ok_label: &str, on_ok: Vec<Command>) {
        self.current = Some(Dialog::Confirm { title: title.into(), message, ok_label: ok_label.into(), on_ok, alt: None, dont_ask: None, focus: true });
    }

    /// Confirm with a "Do not show again" checkbox; `if_checked` runs after `on_ok` when it is ticked.
    pub fn confirm_dont_ask(&mut self, title: &str, message: String, ok_label: &str, on_ok: Vec<Command>, if_checked: Vec<Command>) {
        self.current = Some(Dialog::Confirm {
            title: title.into(),
            message,
            ok_label: ok_label.into(),
            on_ok,
            alt: None,
            dont_ask: Some((false, if_checked)),
            focus: true,
        });
    }

    /// Confirm with a second button `alt_label` that runs `alt`.
    pub fn confirm_alt(&mut self, title: &str, message: String, ok_label: &str, on_ok: Vec<Command>, alt_label: &str, alt: Vec<Command>) {
        self.current = Some(Dialog::Confirm {
            title: title.into(),
            message,
            ok_label: ok_label.into(),
            on_ok,
            alt: Some((alt_label.into(), alt)),
            dont_ask: None,
            focus: true,
        });
    }

    pub fn folder_browser(&mut self, start: &Path) {
        self.dir_loader.cancel();
        let dir = if start.is_dir() { start.to_path_buf() } else { start.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(".")) };
        self.current = Some(Dialog::FolderBrowser { path_text: dir.display().to_string(), dir, error: None, listing: None });
    }

    pub fn request_dir_listing(&mut self, dir: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) {
        self.dir_loader.request(dir, wake);
    }

    pub fn poll_dir_listing(&mut self) -> Option<DirListing> {
        self.dir_loader.poll()
    }

    pub fn cancel_dir_listing(&mut self) {
        self.dir_loader.cancel();
    }

    pub fn is_folder_browser_loading(&self) -> bool {
        self.dir_loader.in_flight.is_some()
    }

    /// Pause or resume background directory listing (for deterministic UI tests).
    #[doc(hidden)]
    pub fn set_dir_listing_paused(&self, paused: bool) {
        let _guard = self.dir_loader.shared.pending.lock();
        self.dir_loader.shared.paused.store(paused, Ordering::Release);
        if !paused {
            self.dir_loader.shared.wake.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn folder_browser_loads_async_and_cancels_on_dir_change() {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("a");
        let b = t.path().join("b");
        std::fs::create_dir_all(a.join("sub_a")).unwrap();
        std::fs::create_dir_all(a.join(".hidden")).unwrap();
        std::fs::create_dir_all(b.join("sub_b")).unwrap();
        std::fs::write(b.join("file.txt"), "x").unwrap();

        let mut d = Dialogs::default();
        let wakes = Arc::new(AtomicU64::new(0));
        let w = wakes.clone();
        d.request_dir_listing(a.clone(), Arc::new(move || { w.fetch_add(1, Ordering::Relaxed); }));
        let w2 = wakes.clone();
        d.request_dir_listing(b.clone(), Arc::new(move || { w2.fetch_add(1, Ordering::Relaxed); }));
        assert!(d.is_folder_browser_loading());

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got = None;
        while Instant::now() < deadline {
            if let Some(res) = d.poll_dir_listing() {
                got = Some(res);
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let (dir, list) = got.expect("dir listing arrived");
        assert_eq!(dir, b);
        let names: Vec<String> = list.unwrap().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["sub_b".to_string()]);
        assert!(!d.is_folder_browser_loading());
        assert!(wakes.load(Ordering::Relaxed) >= 1);
    }
}
