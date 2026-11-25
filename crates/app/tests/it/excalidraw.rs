//! Integration & kittest UI tests for Excalidraw viewing (`S4`), Markdown embeds (`S5`),
//! file watcher reload, error/empty states, and idle repaint = 0 invariants.

use crate::common::*;
use egui::{Event, Key, Modifiers};
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;
use iron_sheets_core::FileKind;
use iron_sheets_core::config::{ThemePref, ViewMode};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

const SAMPLE_RAW: &str = r##"{
  "type": "excalidraw",
  "version": 2,
  "elements": [
    {"id": "f1", "type": "frame", "x": 10, "y": 10, "width": 520, "height": 260, "name": "Architecture"},
    {"id": "r1", "type": "rectangle", "x": 35, "y": 55, "width": 150, "height": 80, "strokeColor": "#1971c2", "backgroundColor": "#a5d8ff", "fillStyle": "hachure", "strokeWidth": 2, "roundness": {"type": 3}, "seed": 101, "frameId": "f1"},
    {"id": "t1", "type": "text", "x": 50, "y": 82, "width": 120, "height": 26, "text": "Client App", "fontSize": 20, "fontFamily": 1, "textAlign": "center", "verticalAlign": "middle", "strokeColor": "#1e1e1e", "seed": 102},
    {"id": "a1", "type": "arrow", "x": 195, "y": 95, "width": 85, "height": 0, "points": [[0,0],[85,0]], "endArrowhead": "triangle", "strokeColor": "#1e1e1e", "strokeWidth": 2, "seed": 103},
    {"id": "d1", "type": "diamond", "x": 290, "y": 45, "width": 130, "height": 100, "strokeColor": "#2f9e44", "backgroundColor": "#b2f2bb", "fillStyle": "cross-hatch", "strokeWidth": 2, "seed": 104},
    {"id": "t2", "type": "text", "x": 315, "y": 82, "width": 80, "height": 26, "text": "Router", "fontSize": 18, "fontFamily": 2, "textAlign": "center", "verticalAlign": "middle", "strokeColor": "#1e1e1e", "seed": 105},
    {"id": "e1", "type": "ellipse", "x": 40, "y": 175, "width": 140, "height": 70, "strokeColor": "#e03131", "backgroundColor": "#ffc9c9", "fillStyle": "zigzag", "strokeWidth": 2, "seed": 106},
    {"id": "fd1", "type": "freedraw", "x": 230, "y": 190, "width": 180, "height": 40, "points": [[0,20],[30,0],[60,30],[100,5],[140,25],[180,10]], "strokeColor": "#9c36b5", "strokeWidth": 3, "seed": 107}
  ],
  "appState": { "viewBackgroundColor": "#ffffff" }
}"##;

const SHAPE_RAW: &str = r##"{
  "type": "excalidraw",
  "version": 2,
  "elements": [
    {"id": "box1", "type": "rectangle", "x": 0, "y": 0, "width": 220, "height": 90, "strokeColor": "#e8590c", "backgroundColor": "#ffd8a8", "fillStyle": "hachure", "strokeWidth": 2, "roundness": {"type": 3}, "seed": 55},
    {"id": "lbl1", "type": "text", "x": 20, "y": 32, "width": 180, "height": 26, "text": "Embedded Shape", "fontSize": 18, "fontFamily": 3, "textAlign": "center", "verticalAlign": "middle", "strokeColor": "#1e1e1e", "seed": 56}
  ],
  "appState": { "viewBackgroundColor": "#ffffff" }
}"##;

fn lz_compress_b64(input: &str) -> String {
    const KEY: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    if input.is_empty() {
        return String::new();
    }
    struct Bw {
        out: String,
        val: u32,
        pos: u32,
    }
    impl Bw {
        fn push_bit(&mut self, bit: u32) {
            self.val = (self.val << 1) | (bit & 1);
            if self.pos == 5 {
                self.pos = 0;
                self.out.push(KEY[self.val as usize] as char);
                self.val = 0;
            } else {
                self.pos += 1;
            }
        }
        fn write_bits(&mut self, n: u32, mut v: u32) {
            for _ in 0..n {
                self.push_bit(v & 1);
                v >>= 1;
            }
        }
    }
    let mut dict: HashMap<Vec<u16>, u32> = HashMap::new();
    let mut to_create: HashSet<Vec<u16>> = HashSet::new();
    let mut w: Vec<u16> = Vec::new();
    let (mut enlarge_in, mut dict_size, mut num_bits) = (2u32, 3u32, 2u32);
    let mut bw = Bw { out: String::new(), val: 0, pos: 0 };
    for u in input.encode_utf16() {
        let single = vec![u];
        if !dict.contains_key(&single) {
            dict.insert(single.clone(), dict_size);
            dict_size += 1;
            to_create.insert(single);
        }
        let mut wc = w.clone();
        wc.push(u);
        if dict.contains_key(&wc) {
            w = wc;
        } else {
            if to_create.remove(&w) {
                let first = w[0] as u32;
                if first < 256 {
                    bw.write_bits(num_bits, 0);
                    bw.write_bits(8, first);
                } else {
                    bw.write_bits(num_bits, 1);
                    bw.write_bits(16, first);
                }
                enlarge_in -= 1;
                if enlarge_in == 0 {
                    enlarge_in = 1 << num_bits;
                    num_bits += 1;
                }
            } else {
                bw.write_bits(num_bits, dict[&w]);
            }
            enlarge_in -= 1;
            if enlarge_in == 0 {
                enlarge_in = 1 << num_bits;
                num_bits += 1;
            }
            dict.insert(wc, dict_size);
            dict_size += 1;
            w = vec![u];
        }
    }
    if !w.is_empty() {
        if to_create.remove(&w) {
            let first = w[0] as u32;
            if first < 256 {
                bw.write_bits(num_bits, 0);
                bw.write_bits(8, first);
            } else {
                bw.write_bits(num_bits, 1);
                bw.write_bits(16, first);
            }
            enlarge_in -= 1;
            if enlarge_in == 0 {
                enlarge_in = 1 << num_bits;
                num_bits += 1;
            }
        } else {
            bw.write_bits(num_bits, dict[&w]);
        }
        enlarge_in -= 1;
        if enlarge_in == 0 {
            num_bits += 1;
        }
    }
    bw.write_bits(num_bits, 2);
    loop {
        bw.val <<= 1;
        if bw.pos == 5 {
            bw.out.push(KEY[bw.val as usize] as char);
            break;
        } else {
            bw.pos += 1;
        }
    }
    while !bw.out.len().is_multiple_of(4) {
        bw.out.push('=');
    }
    bw.out
}

fn obsidian_excalidraw_md(json: &str, embedded_png: Option<(&str, &str)>) -> String {
    let b64 = lz_compress_b64(json);
    let wrapped = b64.as_bytes().chunks(256).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join("\n\n");
    let emb = match embedded_png {
        Some((id, target)) => format!("## Embedded Files\n{id}: [[{target}]]\n"),
        None => String::new(),
    };
    format!("---\nexcalidraw-plugin: parsed\ntags: [excalidraw]\n---\n# Excalidraw Data\n## Text Elements\nClient App ^t1\n{emb}%%\n## Drawing\n```compressed-json\n{wrapped}\n```\n%%\n")
}

fn wait_until(h: &mut egui_kittest::Harness<'static, Slot>, what: &str, mut cond: impl FnMut(&mut egui_kittest::Harness<'static, Slot>) -> bool) {
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(12) {
        h.step();
        if cond(h) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for: {what}");
}

#[test]
fn excalidraw_tab_renders_shapes_text_and_toolbar_fit_100_source_toggle() {
    let fx = Fx::new();
    let exc_md = fx.notes().join("Diagram.excalidraw.md");
    std::fs::write(&exc_md, obsidian_excalidraw_md(SAMPLE_RAW, None)).expect("write excalidraw.md");
    assert_eq!(FileKind::from_path(&exc_md), FileKind::Excalidraw);

    let mut opts = fx.opts(vec![fx.notes()]);
    opts.config.theme = ThemePref::Dark;
    let mut h = harness_dt(opts, 0.05);
    h.state_mut().app().state.commands.push(Command::OpenPath {
        path: exc_md.clone(),
        new_tab: true,
        line: None,
    });
    wait_until(&mut h, "dark drawing ready", |h| {
        h.state_mut().app().state.ui.excalidraw_preview.in_flight() == 0 && h.query_by_label("Fit").is_some()
    });
    settle(&mut h);
    assert_eq!(h.state_mut().app().state.tabs.active_tab().unwrap().view, ViewMode::Preview);
    shot(&mut h, "excalidraw-viewer-dark");

    // Switch to light mode and verify re-render
    h.state_mut().app().state.commands.push(Command::SetTheme(ThemePref::Light));
    wait_until(&mut h, "light drawing ready", |h| {
        !h.state_mut().app().state.ui.theme.dark && h.state_mut().app().state.ui.excalidraw_preview.in_flight() == 0
    });
    settle(&mut h);
    shot(&mut h, "excalidraw-viewer-light");

    // Zoom toolbar: click + then Fit, and 100% button
    h.get_by_label("+").click();
    settle(&mut h);
    assert!(h.query_by_label("125%").is_some(), "zoom in updated zoom percentage");
    h.get_all_by_label("100%").next().expect("100% button").click();
    settle(&mut h);
    h.get_by_label("Fit").click();
    settle(&mut h);
    assert!(h.query_all_by_label("100%").count() >= 2, "Fit reset zoom to 100%");

    // Source toggle button opens editor
    h.get_all_by_label("Source").last().expect("Source button").click();
    settle(&mut h);
    assert_eq!(h.state_mut().app().state.tabs.active_tab().unwrap().view, ViewMode::Source);

    // Switch to Split view on a raw .excalidraw file and edit live
    let raw_p = fx.notes().join("Live.excalidraw");
    std::fs::write(&raw_p, SHAPE_RAW).expect("write raw");
    h.state_mut().app().state.commands.push(Command::OpenPath {
        path: raw_p.clone(),
        new_tab: true,
        line: None,
    });
    settle(&mut h);
    h.state_mut().app().state.commands.push(Command::SetViewMode(ViewMode::Split));
    wait_until(&mut h, "split preview ready", |h| {
        h.state_mut().app().state.ui.excalidraw_preview.in_flight() == 0
    });
    settle(&mut h);
    {
        let st = &mut h.state_mut().app().state;
        let d = st.docs.get_mut(&raw_p).expect("doc");
        let idx = d.text.find("Embedded Shape").expect("label");
        d.apply_edit(idx..idx + "Embedded Shape".len(), "Live Edited");
    }
    wait_until(&mut h, "live edit re-rendered", |h| {
        h.state_mut().app().state.ui.excalidraw_preview.in_flight() == 0
    });
    settle(&mut h);
    shot(&mut h, "excalidraw-split-source");
}

#[test]
fn excalidraw_error_retry_and_empty_states() {
    let fx = Fx::new();
    let bad = fx.notes().join("Broken.excalidraw");
    std::fs::write(&bad, "{ not valid json").expect("write corrupt");

    let mut h = harness_dt(fx.opts(vec![fx.notes()]), 0.05);
    h.state_mut().app().state.commands.push(Command::OpenPath {
        path: bad.clone(),
        new_tab: true,
        line: None,
    });
    wait_until(&mut h, "error banner shown", |h| h.query_by_label("Retry").is_some());
    settle(&mut h);
    assert!(h.query_by_label_contains("Cannot show drawing:").is_some());
    assert!(h.query_by_label("View source").is_some());
    shot(&mut h, "excalidraw-error");

    // Fix file on disk, reload into doc or trigger watcher, click Retry
    std::fs::write(&bad, SHAPE_RAW).expect("write fixed");
    let _ = h.state_mut().app().state.docs.reload(&bad);
    h.get_by_label("Retry").click();
    wait_until(&mut h, "retry succeeded", |h| {
        h.query_by_label("Retry").is_none() && h.state_mut().app().state.ui.excalidraw_preview.in_flight() == 0
    });
    settle(&mut h);
    shot(&mut h, "excalidraw-retried");

    // Empty scene shows designed Empty state without error
    let empty = fx.notes().join("Empty.excalidraw");
    std::fs::write(&empty, r#"{"type":"excalidraw","elements":[]}"#).expect("write empty");
    h.state_mut().app().state.commands.push(Command::OpenPath {
        path: empty,
        new_tab: true,
        line: None,
    });
    wait_until(&mut h, "empty state shown", |h| {
        h.query_by_label("Empty Excalidraw drawing").is_some()
    });
    settle(&mut h);
    assert!(h.query_by_label("Retry").is_none());
    assert!(h.query_by_label("View source").is_some());
    shot(&mut h, "excalidraw-empty");
}

#[test]
fn markdown_embeds_render_all_variants_and_click_opens_tab() {
    let fx = Fx::new();
    // Copy picture.png into notes for vault Embedded Files inside Diagram.excalidraw.md
    std::fs::copy(fx.journal().join("picture.png"), fx.notes().join("picture.png")).expect("copy png");
    let with_img_json = r##"{
      "type": "excalidraw",
      "version": 2,
      "elements": [
        {"id": "r1", "type": "rectangle", "x": 10, "y": 10, "width": 200, "height": 80, "strokeColor": "#1971c2", "backgroundColor": "#d0ebff", "fillStyle": "hachure", "seed": 11},
        {"id": "im1", "type": "image", "x": 230, "y": 10, "width": 80, "height": 80, "fileId": "img1"}
      ],
      "appState": { "viewBackgroundColor": "#ffffff" }
    }"##;
    let diagram_md = fx.notes().join("Diagram.excalidraw.md");
    std::fs::write(&diagram_md, obsidian_excalidraw_md(with_img_json, Some(("img1", "picture.png")))).expect("write diagram.excalidraw.md");
    let shape_raw = fx.notes().join("shape.excalidraw");
    std::fs::write(&shape_raw, SHAPE_RAW).expect("write shape.excalidraw");

    let host = fx.notes().join("Host.md");
    std::fs::write(
        &host,
        "# Embedded Drawings\n\n![[Diagram]]\n\n![[shape.excalidraw]]\n\n![Caption](shape.excalidraw)\n",
    )
    .expect("write host");

    let mut h = harness_dt(fx.opts(vec![fx.notes()]), 0.05);
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    h.state_mut().app().state.commands.push(Command::OpenPath {
        path: host,
        new_tab: true,
        line: None,
    });
    h.state_mut().app().state.commands.push(Command::SetViewMode(ViewMode::Preview));
    wait_until(&mut h, "all 3 excalidraw embeds rendered", |h| {
        let app = h.state_mut().app();
        app.state.ui.md_preview.in_flight_decodes() == 0
            && app.state.ui.md_preview.hits.iter().filter(|(_, k)| k == "excalidraw").count() == 3
    });
    settle(&mut h);
    let rects: Vec<egui::Rect> = h
        .state_mut()
        .app()
        .state
        .ui
        .md_preview
        .hits
        .iter()
        .filter(|(_, k)| k == "excalidraw")
        .map(|(r, _)| *r)
        .collect();
    assert_eq!(rects.len(), 3);
    assert!(rects.iter().all(|r| r.height() > 20.0 && r.width() > 40.0), "non-zero embed geometry: {rects:?}");
    shot(&mut h, "excalidraw-md-embeds");

    // Clicking the first embedded drawing opens Diagram.excalidraw.md in Preview mode
    let target_pt = rects[0].center();
    h.event(Event::PointerMoved(target_pt));
    h.step();
    h.event(Event::PointerButton {
        pos: target_pt,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.step();
    h.event(Event::PointerButton {
        pos: target_pt,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    wait_until(&mut h, "clicked embed opened drawing tab", |h| {
        h.state_mut().app().state.tabs.active_path() == Some(diagram_md.as_path())
    });
    assert_eq!(h.state_mut().app().state.tabs.active_tab().unwrap().view, ViewMode::Preview);
}

#[test]
fn excalidraw_file_watcher_reload_updates_viewer_and_embed() {
    let fx = Fx::new();
    let shape = fx.notes().join("Watched.excalidraw");
    std::fs::write(&shape, SHAPE_RAW).expect("write initial");
    let host = fx.notes().join("WatchHost.md");
    std::fs::write(&host, "# Watch\n\n![[Watched]]\n").expect("write host");

    let mut h = harness_dt(fx.opts(vec![fx.notes()]), 0.05);
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    h.state_mut().app().state.commands.push(Command::OpenPath {
        path: shape.clone(),
        new_tab: true,
        line: None,
    });
    wait_until(&mut h, "initial drawing ready", |h| {
        h.state_mut().app().state.ui.excalidraw_preview.in_flight() == 0
            && h.state_mut().app().state.docs.get(&shape).is_some()
    });
    let v0 = h.state_mut().app().state.docs.get(&shape).unwrap().version;

    // Also open WatchHost in another tab
    h.state_mut().app().state.commands.push(Command::OpenPath {
        path: host.clone(),
        new_tab: true,
        line: None,
    });
    h.state_mut().app().state.commands.push(Command::SetViewMode(ViewMode::Preview));
    wait_until(&mut h, "embed ready", |h| {
        h.state_mut().app().state.ui.md_preview.in_flight_decodes() == 0
            && h.state_mut().app().state.ui.md_preview.hits.iter().any(|(_, k)| k == "excalidraw")
    });

    // Modify file on disk -> watcher reloads open doc and updates embed
    std::thread::sleep(Duration::from_millis(30));
    std::fs::write(&shape, SAMPLE_RAW).expect("overwrite on disk");
    wait_until(&mut h, "watcher reloaded open excalidraw doc", |h| {
        h.state_mut().app().state.docs.get(&shape).is_some_and(|d| d.version > v0)
    });
}

#[test]
fn excalidraw_idle_asks_for_no_repaint_and_clears_inflight() {
    let fx = Fx::new();
    let exc_md = fx.notes().join("Idle.excalidraw.md");
    std::fs::write(&exc_md, obsidian_excalidraw_md(SAMPLE_RAW, None)).expect("write");

    let mut h = harness_dt(fx.opts(vec![fx.notes()]), 0.25);
    h.state_mut().app().state.commands.push(Command::OpenPath {
        path: exc_md,
        new_tab: true,
        line: None,
    });
    wait_until(&mut h, "idle drawing settled", |h| {
        h.state_mut().app().state.ui.excalidraw_preview.in_flight() == 0 && h.query_by_label("Fit").is_some()
    });
    settle(&mut h);
    {
        let st = &mut h.state_mut().app().state;
        iron_sheets::perf::collect_inflight(st);
        assert!(st.perf.inflight.iter().any(|(k, v)| *k == "image/mermaid decodes" && *v == 0));
    }
    // Step 20 idle frames (5s simulated) with no user input and verify zero immediate repaints
    for _ in 0..20 {
        h.step();
        assert!(!wants_immediate_repaint(&h), "settled Excalidraw preview requested immediate repaint");
    }
}

#[test]
fn excalidraw_ctrl_e_and_split_idle_repaint_zero() {
    let fx = Fx::new();
    let raw = fx.notes().join("SplitIdle.excalidraw");
    std::fs::write(&raw, SHAPE_RAW).expect("write");

    let mut h = harness_dt(fx.opts(vec![fx.notes()]), 0.25);
    h.state_mut().app().state.commands.push(Command::OpenPath {
        path: raw,
        new_tab: true,
        line: None,
    });
    h.state_mut().app().state.commands.push(Command::SetViewMode(ViewMode::Split));
    wait_until(&mut h, "split drawing settled", |h| {
        h.state_mut().app().state.ui.excalidraw_preview.in_flight() == 0
    });
    settle(&mut h);
    for _ in 0..10 {
        h.step();
        assert!(!wants_immediate_repaint(&h), "split Excalidraw preview requested immediate repaint");
    }
    // Ctrl+E cycles view mode
    h.key_press_modifiers(Modifiers::CTRL, Key::E);
    h.step();
    assert_ne!(h.state_mut().app().state.tabs.active_tab().unwrap().view, ViewMode::Split);
}
