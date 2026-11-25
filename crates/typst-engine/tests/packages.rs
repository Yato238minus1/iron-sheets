//! Package resolution from Typst's data/cache directories (no network).
//! Own test binary: it changes environment variables before the engine starts.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use iron_sheets_typst::{Engine, EngineConfig, PageState};

#[test]
fn preview_package_from_cache_dir_resolves() {
    let base = std::env::temp_dir().join(format!("iron-sheets-typst-pkg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let pkg = base.join("typst").join("packages").join("preview").join("tinylib").join("0.1.0");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(pkg.join("typst.toml"), "[package]\nname = \"tinylib\"\nversion = \"0.1.0\"\nentrypoint = \"lib.typ\"\n").unwrap();
    std::fs::write(pkg.join("lib.typ"), "#let shout(body) = upper(body)\n").unwrap();
    // `dirs` reads these on Linux; on other platforms this test is skipped.
    if !cfg!(target_os = "linux") {
        return;
    }
    // SAFETY: single test in this binary, no other threads exist yet.
    unsafe { std::env::set_var("XDG_CACHE_HOME", &base) };

    let pair = Arc::new((Mutex::new(0u64), Condvar::new()));
    let p = pair.clone();
    let engine = Engine::new(EngineConfig {
        system_fonts: false,
        notify: Arc::new(move || {
            *p.0.lock().unwrap() += 1;
            p.1.notify_all();
        }),
    });
    engine.set_document(1, None, None, "#import \"@preview/tinylib:0.1.0\": shout\n#shout[hello]".to_string(), 1);
    let deadline = Instant::now() + Duration::from_secs(30);
    let snap = loop {
        if let Some(s) = engine.document(1) {
            break s;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(snap.ok, "{:?}", snap.diagnostics);
    assert_eq!(snap.pages.len(), 1);
    loop {
        if let PageState::Ready(_) = engine.page(1, 0, 1.0) {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = std::fs::remove_dir_all(&base);
}
