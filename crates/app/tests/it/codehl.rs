//! Code highlighting module: fenced blocks are coloured in Preview; off (or unknown language) stays plain.
use crate::common::*;
use egui_kittest::Harness;
use iron_sheets::command::Command;
use iron_sheets_core::config::{ThemePref, ViewMode};
use std::time::Duration;

type H = Harness<'static, Slot>;

const NOTE: &str = "# Code\n\n```rust\nfn main() {\n    let s = \"hi\"; // greet\n    println!(\"{}\", 42);\n}\n```\n\n```bash\nif [ -n \"$X\" ]; then\n  echo \"set\" # note\nfi\n```\n\n```abap\nDATA lv_count TYPE i.\n\" counter\nlv_count = 5.\nWRITE 'done'.\n```\n\n```nosuchlang\nplain text here\n```\n";

fn open(on: bool, dark: bool) -> (Fx, H) {
    open_in(on, dark, ViewMode::Preview)
}

fn open_in(on: bool, dark: bool, view: ViewMode) -> (Fx, H) {
    let fx = Fx::new();
    let p = fx.notes().join("C.md");
    std::fs::write(&p, NOTE).expect("write");
    let mut o = fx.opts(vec![fx.notes()]);
    o.config.theme = if dark { ThemePref::Dark } else { ThemePref::Light };
    o.config.module_codehl = on;
    let mut h = harness_dt(o, 0.05);
    let st = &mut h.state_mut().app().state;
    st.commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
    st.commands.push(Command::SetViewMode(view));
    for _ in 0..40 {
        h.step();
        std::thread::sleep(Duration::from_millis(25));
    }
    (fx, h)
}

fn colors(p: &std::path::Path) -> usize {
    let img = image::open(p).expect("png").to_rgba8();
    img.pixels().map(|p| p.0).collect::<std::collections::HashSet<_>>().len()
}

#[test]
fn blocks_are_coloured_and_the_module_switch_works() {
    for dark in [true, false] {
        let name = if dark { "dark" } else { "light" };
        let (_fx, mut on) = open(true, dark);
        let a = colors(&shot(&mut on, &format!("codehl-{name}")));
        let (_fx2, mut off) = open(false, dark);
        let b = colors(&shot(&mut off, &format!("codehl-{name}-off")));
        // Anti-aliased text gives many shades either way; coloured text adds many more.
        assert!(a > b + 50, "{name}: on {a} colours vs off {b}");
    }
}

/// The source view colours the fences like the preview beside it; module off leaves it plain.
#[test]
fn source_view_is_coloured_like_the_preview() {
    let mut counts = Vec::new();
    for on in [true, false] {
        let (_fx, mut h) = open_in(on, true, ViewMode::Split);
        let name = if on { "codehl-source" } else { "codehl-source-off" };
        counts.push(colors(&shot(&mut h, name)));
    }
    assert!(counts[0] > counts[1] + 50, "on {} colours vs off {}", counts[0], counts[1]);
}

/// A note above the highlight limit: the rust block the view is scrolled into is coloured (module on), plain when off.
#[test]
fn big_note_source_view_colours_the_visible_block() {
    let mut note = String::new();
    for i in 0..12000 {
        note.push_str(&format!("Paragraph {i} of filler text to make the note big enough.\n\n"));
        if i % 300 == 150 {
            note.push_str("```rust\nfn block() {\n\n    let s = \"hi\"; // greet\n    println!(\"{}\", 42);\n}\n```\n\n");
        }
    }
    assert!(note.len() > 400_000);
    let mut counts = Vec::new();
    for on in [true, false] {
        let fx = Fx::new();
        let p = fx.notes().join("Big.md");
        std::fs::write(&p, &note).expect("write");
        let mut o = fx.opts(vec![fx.notes()]);
        o.config.module_codehl = on;
        let mut h = harness_dt(o, 0.05);
        let st = &mut h.state_mut().app().state;
        st.commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
        st.commands.push(Command::SetViewMode(ViewMode::Source));
        for _ in 0..10 {
            h.step();
        }
        // Into the middle of the block, below its opener (and blank line), so the opener is off the window's paragraphs.
        let at = note.find("println").expect("block") + 3;
        h.state_mut().app().state.tabs.active_tab_mut().expect("tab").pending_cursor = Some(at..at);
        for _ in 0..40 {
            h.step();
            std::thread::sleep(Duration::from_millis(25));
        }
        counts.push(colors(&shot(&mut h, if on { "codehl-big-source" } else { "codehl-big-source-off" })));
    }
    assert!(counts[0] > counts[1] + 50, "on {} colours vs off {}", counts[0], counts[1]);
}

/// Rust and Python fences in the source editor (screenshot for a human): both are coloured, and the colours arrive after the plain first paint.
#[test]
fn editor_colours_rust_and_python_fences() {
    let fx = Fx::new();
    let p = fx.notes().join("Fences.md");
    std::fs::write(&p, "# Fences\n\n```rust\nfn main() {\n    let s = \"hi\"; // greet\n    println!(\"{}\", 42);\n}\n```\n\n```python\ndef greet(name):\n    # say hello\n    return f\"hi {name}\" + str(42)\n```\n").expect("write");
    let mut h = harness_dt(fx.opts(vec![fx.notes()]), 0.05);
    let st = &mut h.state_mut().app().state;
    st.commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
    st.commands.push(Command::SetViewMode(ViewMode::Source));
    for _ in 0..3 {
        h.step();
    }
    let plain = colors(&shot(&mut h, "codehl-fences-first"));
    for _ in 0..40 {
        h.step();
        std::thread::sleep(Duration::from_millis(25));
    }
    let coloured = colors(&shot(&mut h, "codehl-fences-editor"));
    assert!(coloured > plain + 50, "plain {plain} colours, then {coloured}");
}
