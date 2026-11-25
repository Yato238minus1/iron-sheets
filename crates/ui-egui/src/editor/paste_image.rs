//! Pasting an image into the source editor: the clipboard image (egui's `Event::PasteImage`) is encoded as PNG and
//! written into the attachment folder on a worker; when the file exists, `![[name]]` is inserted at the caret.
use crossbeam_channel::{Receiver, Sender, unbounded};
use egui::ColorImage;
use iron_sheets_core::config::AppConfig;
use std::io::Write;
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// A finished paste. `caret` and `version` are those of the note when the image was pasted.
pub struct Done {
    pub note: PathBuf,
    pub dir: PathBuf,
    pub caret: Range<usize>,
    pub version: u64,
    pub res: Result<String, String>,
}

pub struct Pasting {
    tx: Sender<Done>,
    pub rx: Receiver<Done>,
}

impl Default for Pasting {
    fn default() -> Self {
        let (tx, rx) = unbounded();
        Pasting { tx, rx }
    }
}

impl Pasting {
    /// Encode and write on a worker thread; the result arrives on `rx` and wakes the UI.
    pub fn start(&self, ctx: &egui::Context, img: Arc<ColorImage>, dir: PathBuf, note: PathBuf, caret: Range<usize>, version: u64) {
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let res = write_png(&dir, &img, now_stamp());
            let _ = tx.send(Done { note, dir, caret, version, res });
            ctx.request_repaint();
        });
    }
}

/// Folder for pasted images of `note` per the settings, and a message when a bad setting made it fall back to
/// `<note dir>/assets`. `roots` are the workspace root folders.
pub fn attachment_dir(cfg: &AppConfig, note: &Path, roots: &[PathBuf]) -> (PathBuf, Option<&'static str>) {
    let base = note.parent().unwrap_or_else(|| Path::new(""));
    let default = base.join("assets");
    match cfg.attachment_mode.as_str() {
        "same" => (base.to_path_buf(), None),
        "root" => (roots.iter().find(|r| note.starts_with(r)).cloned().unwrap_or_else(|| base.to_path_buf()), None),
        "folder" => {
            let f = Path::new(cfg.attachment_folder.trim());
            if f.is_absolute() { (f.to_path_buf(), None) } else { (default, Some("The attachment folder must be an absolute path; using assets")) }
        }
        _ => {
            let sub = cfg.attachment_subfolder.trim();
            let p = Path::new(sub);
            if sub.is_empty() {
                (default, None)
            } else if p.components().all(|c| matches!(c, Component::Normal(_))) {
                (base.join(p), None)
            } else {
                (default, Some("Invalid attachment subfolder name; using assets"))
            }
        }
    }
}

/// `YYYYMMDDHHMMSS` in local time, like Obsidian.
fn now_stamp() -> String {
    chrono::Local::now().format("%Y%m%d%H%M%S").to_string()
}

/// Writes `img` as `Pasted image <stamp>[ n].png` into `dir` (created); never overwrites. Returns the file name.
fn write_png(dir: &Path, img: &ColorImage, stamp: String) -> Result<String, String> {
    let mut png = Vec::new();
    let [w, h] = img.size;
    let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
    let buf = image::RgbaImage::from_raw(w as u32, h as u32, rgba).ok_or("bad image size")?;
    buf.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for n in 0..1000 {
        let name = if n == 0 { format!("Pasted image {stamp}.png") } else { format!("Pasted image {stamp} {n}.png") };
        match std::fs::OpenOptions::new().write(true).create_new(true).open(dir.join(&name)) {
            Ok(mut f) => {
                let r = f.write_all(&png);
                drop(f);
                if let Err(e) = r {
                    let _ = std::fs::remove_file(dir.join(&name)); // no partial file
                    return Err(e.to_string());
                }
                return Ok(name);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("{}: {e}", dir.join(&name).display())),
        }
    }
    Err("no free file name".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_has_14_digits() {
        let t = now_stamp();
        assert!(t.len() == 14 && t.bytes().all(|b| b.is_ascii_digit()), "{t}");
    }

    #[test]
    fn dir_follows_the_setting() {
        let note = Path::new("/ws/a/sub/n.md");
        let roots = [PathBuf::from("/other"), PathBuf::from("/ws/a")];
        let mut c = AppConfig::default();
        assert_eq!(attachment_dir(&c, note, &roots).0, Path::new("/ws/a/sub/assets"));
        c.attachment_mode = "same".into();
        assert_eq!(attachment_dir(&c, note, &roots).0, Path::new("/ws/a/sub"));
        c.attachment_mode = "root".into();
        assert_eq!(attachment_dir(&c, note, &roots).0, Path::new("/ws/a"));
        c.attachment_mode = "folder".into();
        c.attachment_folder = "/x/y".into();
        assert_eq!(attachment_dir(&c, note, &roots).0, Path::new("/x/y"));
        // Bad settings fall back to assets with a message.
        c.attachment_folder = "rel/dir".into();
        let (d, msg) = attachment_dir(&c, note, &roots);
        assert!(d == Path::new("/ws/a/sub/assets") && msg.is_some());
        c.attachment_mode = String::new();
        c.attachment_subfolder = "../up".into();
        let (d, msg) = attachment_dir(&c, note, &roots);
        assert!(d == Path::new("/ws/a/sub/assets") && msg.is_some());
        c.attachment_subfolder = "img/pasted".into();
        assert_eq!(attachment_dir(&c, note, &roots).0, Path::new("/ws/a/sub/img/pasted"));
    }

    #[test]
    fn never_overwrites() {
        let dir = std::env::temp_dir().join(format!("iron-sheets-paste-{}", std::process::id())).join("assets");
        let img = ColorImage::filled([2, 2], egui::Color32::RED);
        let a = write_png(&dir, &img, "1".into()).expect("a");
        let b = write_png(&dir, &img, "1".into()).expect("b");
        assert_eq!((a.as_str(), b.as_str()), ("Pasted image 1.png", "Pasted image 1 1.png"));
        assert!(image::open(dir.join(&a)).is_ok());
        let _ = std::fs::remove_dir_all(dir.parent().unwrap_or(&dir));
    }
}
