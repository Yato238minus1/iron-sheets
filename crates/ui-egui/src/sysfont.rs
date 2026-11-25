//! Fallback fonts from the system, found lazily and off the UI thread. Registered at start as a cheap starter; the first
//! char no bundled font has makes it build the real provider (system font list, fontconfig) on a background thread and
//! draws tofu meanwhile. When the provider is ready it is added with `add_font_provider`, which resets egui's font
//! caches (they also cache "no font" per char), so the next layout picks the glyph up. Emoji never start it.
//! Needs the `system-fonts` feature; without it nothing is installed.
use parking_lot::Mutex;
use std::sync::Arc;

/// Scripts no system font covers; the frame turns each into one status message.
pub type Missing = Arc<Mutex<Vec<&'static str>>>;

#[cfg(any(test, feature = "system-fonts"))]
mod lazy {
    use super::Missing;
    use egui::{FallbackRequest, FontFamily, FontInsert, FontProvider};
    use parking_lot::Mutex;
    use std::sync::{Arc, atomic::{AtomicBool, Ordering::Relaxed}};
    use unicode_script::{Script, UnicodeScript};

    /// Colour emoji took 396 ms on the UI thread (egui-0.37-text.md), so emoji and symbol blocks stay tofu.
    /// shortcut: block ranges, also drops plain monochrome symbols; upgrade if a monochrome symbol font matters.
    fn is_emoji(c: char) -> bool {
        matches!(c,
            '\u{A9}' | '\u{AE}' | '\u{2000}'..='\u{206F}' | '\u{2122}' | '\u{2139}' | '\u{2190}'..='\u{21FF}'
            | '\u{2300}'..='\u{23FF}' | '\u{25A0}'..='\u{25FF}' | '\u{2600}'..='\u{27BF}' | '\u{2B00}'..='\u{2BFF}'
            | '\u{3030}' | '\u{303D}' | '\u{3297}' | '\u{3299}' | '\u{1F000}'..='\u{1FAFF}')
    }

    /// Registered at start: on the first uncovered, non-emoji char builds the provider on a thread.
    pub struct Starter<P> {
        pub make: fn() -> P,
        pub started: AtomicBool,
        pub ctx: egui::Context,
        pub missing: Missing,
    }

    impl<P: FontProvider + 'static> FontProvider for Starter<P> {
        fn font_for(&self, request: &FallbackRequest<'_>) -> Option<FontInsert> {
            let c = request.cluster.chars().next()?;
            if is_emoji(c) || self.started.swap(true, Relaxed) {
                return None;
            }
            let (make, ctx, missing) = (self.make, self.ctx.clone(), self.missing.clone());
            // A failed spawn leaves tofu, same as no font.
            let _ = std::thread::Builder::new().name("system-fonts".into()).spawn(move || {
                let inner = make();
                // The first request waits for the provider's own font enumeration: do it here, not in layout.
                inner.font_for(&FallbackRequest { cluster: "a", family: &FontFamily::Proportional });
                ctx.add_font_provider(Arc::new(Ready { inner, seen: Mutex::default(), missing }));
            });
            None
        }
    }

    /// The built provider: skips emoji and records scripts it has no font for.
    pub struct Ready<P> {
        pub inner: P,
        pub seen: Mutex<Vec<Script>>,
        pub missing: Missing,
    }

    impl<P: FontProvider> FontProvider for Ready<P> {
        fn font_for(&self, request: &FallbackRequest<'_>) -> Option<FontInsert> {
            let c = request.cluster.chars().next()?;
            if is_emoji(c) {
                return None;
            }
            let found = self.inner.font_for(request);
            let script = c.script();
            if found.is_none() && !matches!(script, Script::Common | Script::Inherited | Script::Unknown) {
                let mut seen = self.seen.lock();
                if !seen.contains(&script) {
                    seen.push(script);
                    self.missing.lock().push(script.full_name());
                }
            }
            found
        }
    }
}

/// Registers the starter (one allocation; nothing is read until an uncovered char is laid out) and returns the list of
/// scripts the system has no font for.
pub fn install(ctx: &egui::Context) -> Missing {
    let missing = Missing::default();
    #[cfg(feature = "system-fonts")]
    ctx.add_font_provider(Arc::new(lazy::Starter {
        make: egui_system_font_provider::SystemFontProvider::new,
        started: Default::default(),
        ctx: ctx.clone(),
        missing: missing.clone(),
    }));
    #[cfg(not(feature = "system-fonts"))]
    let _ = ctx;
    missing
}

#[cfg(test)]
mod tests {
    use super::lazy::Starter;
    use super::*;
    use egui::{FallbackRequest, FontInsert, FontProvider};
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
    use std::time::{Duration, Instant};

    /// Counts the requests for '日' that reach the built provider (only one test uses it); never has a font.
    static HITS: AtomicUsize = AtomicUsize::new(0);
    struct Slow;
    impl FontProvider for Slow {
        fn font_for(&self, r: &FallbackRequest<'_>) -> Option<FontInsert> {
            if r.cluster == "日" {
                HITS.fetch_add(1, Relaxed);
            }
            None
        }
    }

    /// A provider that takes 200 ms to build.
    fn starter_ctx() -> (egui::Context, Arc<Starter<Slow>>) {
        let ctx = egui::Context::default();
        let starter = Arc::new(Starter {
            make: || {
                std::thread::sleep(Duration::from_millis(200));
                Slow
            },
            started: Default::default(),
            ctx: ctx.clone(),
            missing: Missing::default(),
        });
        ctx.add_font_provider(starter.clone());
        (ctx, starter)
    }

    fn layout(ctx: &egui::Context, text: &str) {
        let mut out = ctx.run_ui(Default::default(), |ui| {
            ui.label(text);
        });
        out.textures_delta.clear();
    }

    #[test]
    fn latin_and_emoji_do_not_start_the_provider() {
        let (ctx, starter) = starter_ctx();
        layout(&ctx, "plain text, 123 \u{1F600} \u{2705} \u{2192} \u{201C}");
        assert!(!starter.started.load(Relaxed));
    }

    #[test]
    fn first_uncovered_char_does_not_wait_and_the_next_layout_sees_the_provider() {
        let (ctx, starter) = starter_ctx();
        let hits = &HITS;
        let t = Instant::now();
        layout(&ctx, "日本語 \u{0627}\u{0644}\u{0639}\u{0631}\u{0628}\u{064A}\u{0629}");
        assert!(t.elapsed() < Duration::from_millis(150), "layout waited for the provider: {:?}", t.elapsed());
        assert!(starter.started.load(Relaxed));
        assert_eq!(hits.load(Relaxed), 0);
        // Ready: the repaint request is what wakes the app; here poll by laying out again.
        let t = Instant::now();
        while hits.load(Relaxed) == 0 && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
            layout(&ctx, "日本語 \u{0627}\u{0644}\u{0639}\u{0631}\u{0628}\u{064A}\u{0629}");
        }
        assert_eq!(hits.load(Relaxed), 1, "egui did not ask the ready provider (stale 'no font' cache)");
        layout(&ctx, "日本語");
        assert_eq!(hits.load(Relaxed), 1, "negative answer asked again");
        assert_eq!(*starter.missing.lock(), ["Han", "Arabic"]);
    }

    /// With a real system font: CJK and Arabic get glyphs once the provider is ready. Skips without such fonts.
    #[cfg(feature = "system-fonts")]
    #[test]
    fn system_fonts_cover_cjk_and_arabic() {
        let ctx = egui::Context::default();
        let missing = install(&ctx);
        let font_id = egui::FontId::proportional(14.0);
        layout(&ctx, "日\u{0639}");
        let t = Instant::now();
        let has = |c: char| {
            let mut has = false;
            let mut out = ctx.run_ui(Default::default(), |ui| ui.fonts_mut(|f| has = f.has_glyph(&font_id, c)));
            out.textures_delta.clear();
            has
        };
        while !(has('日') && has('\u{0639}')) && t.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        for (c, script) in [('日', "Han"), ('\u{0639}', "Arabic")] {
            if missing.lock().contains(&script) {
                eprintln!("skipped {script}: no system font installed");
            } else {
                assert!(has(c), "{script} has a font but no glyph");
            }
        }
    }

    /// `cargo test -p iron-sheets-ui-egui --features system-fonts --release sysfont -- --ignored --nocapture`
    #[cfg(feature = "system-fonts")]
    #[test]
    #[ignore = "prints timings, depends on the installed fonts"]
    fn first_use_cost() {
        let ctx = egui::Context::default();
        install(&ctx);
        let text = "日本語のテキスト 100 KB".repeat(4000);
        for text in ["warm up", "日", &text] {
            let t = Instant::now();
            layout(&ctx, text);
            eprintln!("{} bytes, first layout: {:.1} ms", text.len(), t.elapsed().as_secs_f32() * 1e3);
        }
        std::thread::sleep(Duration::from_secs(1));
        let t = Instant::now();
        layout(&ctx, &text);
        eprintln!("relayout after ready: {:.1} ms", t.elapsed().as_secs_f32() * 1e3);
        let t = Instant::now();
        layout(&ctx, &text);
        eprintln!("again (cached): {:.1} ms", t.elapsed().as_secs_f32() * 1e3);
    }
}
