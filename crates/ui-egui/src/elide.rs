//! Middle ellipsis for file names: `looooong.md` -> `lo…ong.md`.
//!
//! The extension always stays, the stem keeps its first and its last characters (the end of a name carries the
//! distinguishing part more often than the middle: `report-2024-03`, `IMG_0042`). One "…" glyph is used, which is
//! narrower than "..". Text is split on `char` boundaries (UTF-8, CJK, emoji); a ZWJ emoji sequence or a base letter
//! with combining marks can be cut in two, which draws as two glyphs and never panics.
//!
//! [`elide_middle`] is pure. [`elide_cached`] remembers results per (name, width rounded to 4 px, font generation),
//! so a steady frame measures no text at all.
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

const ELLIPSIS: &str = "…";

/// Split a name into (stem, extension with its dot). A leading dot (`.gitignore`) or a very long "extension" is not one.
fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 && name[i..].chars().count() <= 8 => name.split_at(i),
        _ => (name, ""),
    }
}

/// `name` shortened to at most `max_w` (as measured by `measure`), or as short as possible when even `…` plus the
/// extension does not fit. Binary search over the number of kept stem characters: O(log n) measurements.
pub fn elide_middle(name: &str, max_w: f32, measure: impl Fn(&str) -> f32) -> Cow<'_, str> {
    if measure(name) <= max_w {
        return Cow::Borrowed(name);
    }
    let (stem, ext) = split_ext(name);
    // Byte offset of every char start, plus the end: slices at these offsets are always valid.
    let offs: Vec<usize> = stem.char_indices().map(|(i, _)| i).chain(std::iter::once(stem.len())).collect();
    let n = offs.len() - 1;
    // `k` kept characters: head = k / 2, tail = k - head (the tail gets the odd one).
    let build = |k: usize| -> String {
        let (h, t) = (k / 2, k - k / 2);
        let mut s = String::with_capacity(stem.len() + ext.len() + ELLIPSIS.len());
        s.push_str(&stem[..offs[h]]);
        s.push_str(ELLIPSIS);
        s.push_str(&stem[offs[n - t]..]);
        s.push_str(ext);
        s
    };
    // Largest k < n that fits (k = n would be the whole name, which does not fit).
    let (mut lo, mut hi) = (0usize, n.saturating_sub(1));
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if measure(&build(mid)) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    Cow::Owned(build(lo))
}

#[derive(Default)]
struct Cache {
    generation: u64,
    map: HashMap<(u64, u32), Rc<str>>,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache::default());
}

fn stable_hash(name: &str) -> u64 {
    // FNV-1a: cheap, no allocation, deterministic.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in name.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[derive(Default)]
struct Galleys {
    key: (u64, u32),
    map: HashMap<(u64, u32), Arc<egui::Galley>>,
}

thread_local! {
    static GALLEYS: RefCell<Galleys> = RefCell::new(Galleys::default());
}

/// The laid-out (and, when too wide, elided) text of a title: one galley per (name, 4 px width bucket), kept until
/// `font_gen` or the pixel density changes, so a steady frame does no allocation and no text layout. The galley is
/// laid out with `Color32::PLACEHOLDER`: pass the real colour to `Painter::galley`. `font_gen` must change whenever
/// egui may have dropped its glyph atlas (the theme generation does).
pub fn title_galley(painter: &egui::Painter, name: &str, max_w: f32, font_gen: u64, font: &egui::FontId) -> Arc<egui::Galley> {
    let bucket = (max_w / 4.0).floor().max(0.0) as u32;
    let key = (stable_hash(name), bucket);
    let gen_key = (font_gen, painter.pixels_per_point().to_bits());
    let hit = GALLEYS.with(|g| {
        let mut g = g.borrow_mut();
        if g.key != gen_key || g.map.len() > 2048 {
            g.map.clear();
            g.key = gen_key;
        }
        g.map.get(&key).cloned()
    });
    hit.unwrap_or_else(|| {
        let layout = |s: &str| painter.layout_no_wrap(s.to_string(), font.clone(), egui::Color32::PLACEHOLDER);
        let shown = elide_middle(name, bucket as f32 * 4.0, |s| layout(s).size().x);
        let galley = layout(&shown);
        GALLEYS.with(|g| g.borrow_mut().map.insert(key, galley.clone()));
        galley
    })
}

/// [`elide_middle`] with a cache. `font_gen` must change whenever the font (family, size) changes; the width is
/// rounded to 4 px so a window that is dragged smoothly does not fill the cache. Hits cost one hash of the name.
pub fn elide_cached(name: &str, max_w: f32, font_gen: u64, measure: impl Fn(&str) -> f32) -> Rc<str> {
    let bucket = (max_w / 4.0).floor().max(0.0) as u32;
    // Measure against the bucket's lower edge so the result never overflows the real width.
    let w = bucket as f32 * 4.0;
    let key = (stable_hash(name), bucket);
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.generation != font_gen || c.map.len() > 8192 {
            c.map.clear();
            c.generation = font_gen;
        }
        if let Some(r) = c.map.get(&key) {
            return r.clone();
        }
        let r: Rc<str> = Rc::from(elide_middle(name, w, measure).as_ref());
        c.map.insert(key, r.clone());
        r
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One unit per char; "…" counts like any char.
    fn m(s: &str) -> f32 {
        s.chars().count() as f32
    }

    #[test]
    fn fits_unchanged() {
        assert_eq!(elide_middle("note.md", 7.0, m), "note.md");
        assert!(matches!(elide_middle("note.md", 20.0, m), Cow::Borrowed(_)));
    }

    #[test]
    fn keeps_extension_and_splits_the_stem() {
        assert_eq!(elide_middle("looooong.md", 9.0, m), "lo…ong.md");
        assert_eq!(elide_middle("looooong.md", 8.0, m), "lo…ng.md");
        assert_eq!(elide_middle("abcdefghijkl.md", 8.0, m), "ab…kl.md");
    }

    #[test]
    fn longest_result_that_fits() {
        for w in 4..=16 {
            let r = elide_middle("abcdefghijklmn.md", w as f32, m);
            assert_eq!(m(&r), w as f32, "{r} at {w}");
            assert!(r.ends_with(".md") && r.contains('…'));
        }
    }

    #[test]
    fn too_narrow_gives_the_minimum() {
        assert_eq!(elide_middle("looooong.md", 1.0, m), "….md");
    }

    #[test]
    fn no_extension_and_dotfiles() {
        assert_eq!(elide_middle("abcdefghij", 6.0, m), "ab…hij");
        let r = elide_middle(".gitignore-long-name", 8.0, m);
        assert!(r.starts_with('.') && r.contains('…') && m(&r) <= 8.0);
        // A long "extension" is part of the stem.
        let r = elide_middle("archive.verylongext", 10.0, m);
        assert!(r.contains('…') && m(&r) <= 10.0 && !r.ends_with(".verylongext"));
    }

    #[test]
    fn works_on_char_boundaries() {
        assert_eq!(elide_middle("日本語のとても長いファイル名.md", 8.0, m), "日本…ル名.md");
        for name in ["日本語のとても長いファイル名.md", "ünïcödé-nämé-lóng.md", "🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀.typ", "a\u{301}b\u{301}c\u{301}d\u{301}e\u{301}f.md"]
        {
            for w in 0..=20 {
                let r = elide_middle(name, w as f32, m);
                if m(name) > w as f32 {
                    assert!(r.contains('…'), "{r}");
                    assert!(m(&r) <= (w as f32).max(m("….md")) + 1.0);
                }
            }
        }
    }

    #[test]
    fn uses_log_n_measurements() {
        let name = format!("{}.md", "x".repeat(5000));
        let calls = std::cell::Cell::new(0);
        let r = elide_middle(&name, 40.0, |s| {
            calls.set(calls.get() + 1);
            m(s)
        });
        assert!(m(&r) <= 40.0 && m(&r) >= 39.0);
        assert!(calls.get() < 30, "{} measurements", calls.get());
    }

    #[test]
    fn cache_hits_measure_nothing() {
        let calls = std::cell::Cell::new(0);
        let meas = |s: &str| {
            calls.set(calls.get() + 1);
            m(s)
        };
        let a = elide_cached("looooooooooong.md", 41.0, 1, meas);
        let n = calls.get();
        assert!(n > 0);
        let b = elide_cached("looooooooooong.md", 42.5, 1, meas);
        assert_eq!(calls.get(), n, "same 4 px bucket: no measuring");
        assert!(Rc::ptr_eq(&a, &b));
        let _ = elide_cached("looooooooooong.md", 41.0, 2, meas);
        assert!(calls.get() > n, "a new font generation measures again");
    }

    #[test]
    fn title_galley_is_reused_and_fits() {
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let font = egui::FontId::proportional(13.0);
            let name = "a-rather-long-note-file-name-number-07-with-a-long-tail-2024.md";
            let a = title_galley(ui.painter(), name, 120.0, 1, &font);
            let b = title_galley(ui.painter(), name, 121.0, 1, &font);
            assert!(Arc::ptr_eq(&a, &b), "same bucket: same galley");
            assert!(a.size().x <= 120.0 && a.text().contains('…') && a.text().ends_with(".md"), "{}", a.text());
            // A new generation empties the cache (egui's own cache may still hand back the same galley).
            title_galley(ui.painter(), name, 120.0, 2, &font);
            assert_eq!(GALLEYS.with(|g| g.borrow().map.len()), 1);
        });
        out.textures_delta.clear();
    }
}
