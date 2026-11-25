//! System fonts for the SVG rasteriser (resvg), shared by Mermaid diagrams and Excalidraw drawings.
use std::sync::{Arc, OnceLock};

/// Loads the system fonts on a thread of their own (once), so the first diagram does not wait for them on its render
/// worker. Call when a note with a diagram is opened.
pub fn warm_fonts() {
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !STARTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        let _ = std::thread::Builder::new().name("iron-sheets-svg-fonts".into()).spawn(|| {
            fonts();
        });
    }
}

pub(crate) fn fonts() -> Arc<resvg::usvg::fontdb::Database> {
    static DB: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = resvg::usvg::fontdb::Database::new();
        db.load_system_fonts();
        let (sans, mono, serif) = {
            let has_family = |name: &str| {
                db.faces()
                    .any(|f| f.families.iter().any(|(fam, _)| fam.eq_ignore_ascii_case(name)))
            };
            let first_loaded = db.faces().next().and_then(|f| f.families.first().map(|(n, _)| n.clone()));
            let pick = |candidates: &[&str]| -> Option<String> {
                candidates
                    .iter()
                    .find(|&&c| has_family(c))
                    .map(|&s| s.to_string())
                    .or_else(|| first_loaded.clone())
            };
            (
                pick(&["Liberation Sans", "DejaVu Sans", "Noto Sans", "Segoe UI", "Arial", "Helvetica", "Cantarell", "Adwaita Sans"]),
                pick(&["Liberation Mono", "DejaVu Sans Mono", "Noto Sans Mono", "Consolas", "Adwaita Mono", "Courier New"]),
                pick(&["Liberation Serif", "DejaVu Serif", "Noto Serif", "Times New Roman"]),
            )
        };
        if let Some(sans) = sans {
            db.set_sans_serif_family(sans.clone());
            db.set_cursive_family(sans);
        }
        if let Some(mono) = mono {
            db.set_monospace_family(mono);
        }
        if let Some(serif) = serif {
            db.set_serif_family(serif);
        }
        Arc::new(db)
    })
    .clone()
}

