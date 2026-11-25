//! Background note writer: one worker thread runs the atomic writes (tmp file, fsync, rename) and the hash of the
//! written text, so a save never blocks the UI thread. `DocStore` owns it and applies the results.
use crate::platform::Waker;
use crossbeam_channel::{Receiver, Sender, TryRecvError, unbounded};
use iron_sheets_core::textio::{self, LineEnding};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

/// What to write: a snapshot of a document taken on the UI thread.
pub(crate) struct SaveJob {
    pub path: PathBuf,
    pub text: String,
    pub ending: LineEnding,
    pub bom: bool,
    pub version: u64,
    /// Create the parent folder first (untitled notes).
    pub mkdir: bool,
    /// An untitled note without text has no file: delete it instead of writing.
    pub remove: bool,
}

pub(crate) struct SaveDone {
    pub path: PathBuf,
    pub version: u64,
    /// Length and hash of the written text, for `Doc`'s "edited back to the saved text" check.
    pub len: usize,
    pub hash: u64,
    pub result: io::Result<Option<SystemTime>>,
}

pub(crate) fn hash_text(s: &str) -> u64 {
    use std::hash::Hasher;
    let mut h = rustc_hash::FxHasher::default();
    h.write(s.as_bytes());
    h.finish()
}

/// Does the write; runs on the worker, or inline for saves that must finish before the caller goes on.
pub(crate) fn run(job: SaveJob) -> SaveDone {
    let result = if job.remove {
        match std::fs::remove_file(&job.path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(None),
        }
    } else {
        let dir = job.path.parent().filter(|_| job.mkdir);
        dir.map_or(Ok(()), std::fs::create_dir_all).and_then(|()| textio::save_text(&job.path, &job.text, job.ending, job.bom))
    };
    SaveDone { len: job.text.len(), hash: hash_text(&job.text), path: job.path, version: job.version, result }
}

#[derive(Default)]
pub(crate) struct Saver {
    tx: Option<Sender<SaveJob>>,
    rx: Option<Receiver<SaveDone>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Saver {
    /// Queues `job` on the worker (started on first use). Hands the job back when no worker can be started.
    pub fn send(&mut self, job: SaveJob, waker: &Arc<dyn Waker>) -> Result<(), SaveJob> {
        if self.tx.is_none() {
            let (tx, jobs) = unbounded::<SaveJob>();
            let (done_tx, rx) = unbounded();
            let w = waker.clone();
            let spawned = std::thread::Builder::new().name("iron-sheets-save".into()).spawn(move || {
                for job in jobs {
                    if done_tx.send(run(job)).is_err() {
                        return;
                    }
                    w.wake();
                }
            });
            match spawned {
                Ok(h) => (self.tx, self.rx, self.thread) = (Some(tx), Some(rx), Some(h)),
                Err(_) => return Err(job),
            }
        }
        let Some(tx) = &self.tx else { return Err(job) };
        tx.send(job).map_err(|e| e.0)
    }

    pub fn try_recv(&self) -> Result<SaveDone, TryRecvError> {
        self.rx.as_ref().map_or(Err(TryRecvError::Empty), |rx| rx.try_recv())
    }

    /// Blocks for the next result; `None` when the worker is gone.
    pub fn recv(&self) -> Option<SaveDone> {
        self.rx.as_ref()?.recv().ok()
    }
}

impl Drop for Saver {
    fn drop(&mut self) {
        // Closing the job channel ends the worker after the write in progress.
        self.tx = None;
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}
