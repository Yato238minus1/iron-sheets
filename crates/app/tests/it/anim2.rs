//! Folder open/close and sidebar slide animations, and the switches that turn them off. The harness runs at 60 fps of
//! simulated time (`harness_dt`), so the frames of a 0.12 s animation can be seen one by one.
use crate::common::*;
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;
use iron_sheets::state::AppState;
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;

fn st(h: &mut H) -> &mut AppState {
    &mut h.state_mut().app().state
}

fn ready(fx: &Fx, f: impl FnOnce(&mut iron_sheets_core::config::AppConfig)) -> H {
    let mut o = fx.opts(vec![fx.notes()]);
    f(&mut o.config);
    o.config.theme = iron_sheets_core::config::ThemePref::Dark;
    let mut h = harness_dt(o, 1.0 / 60.0);
    h.set_size(vec2(1100.0, 720.0));
    let t = Instant::now();
    while st(&mut h).indexer.index().read().len() < 8 {
        h.step();
        assert!(t.elapsed() < Duration::from_secs(10), "index timeout");
        std::thread::sleep(Duration::from_millis(5));
    }
    run(&mut h, 40);
    h
}

fn run(h: &mut H, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

fn wants_immediate_repaint(h: &H) -> bool {
    h.output().viewport_output.values().any(|v| v.repaint_delay == Duration::ZERO)
}

/// Top of the first tree row with this label (the tree is the left pane).
fn row_y(h: &H, label: &str) -> f32 {
    h.query_all_by_label(label).find(|n| n.rect().min.x < 300.0).unwrap_or_else(|| panic!("row {label}")).rect().min.y
}

fn click_row(h: &mut H, label: &str) {
    h.query_all_by_label(label).find(|n| n.rect().min.x < 300.0).unwrap_or_else(|| panic!("row {label}")).click();
}

/// Frames until nothing asks for another one; panics when it never settles.
fn frames_until_quiet(h: &mut H) -> usize {
    for i in 0..60 {
        h.step();
        if !wants_immediate_repaint(h) {
            return i;
        }
    }
    panic!("still animating after 60 frames");
}

#[test]
fn folder_opens_and_closes_as_a_sliding_block_then_goes_quiet() {
    let fx = Fx::new();
    let mut h = ready(&fx, |_| {});
    let rows0 = st(&mut h).tree.rows().len();
    let below = |h: &H| row_y(h, "Welcome.md");
    let y0 = below(&h);
    // Open: the rows below the folder move down in several frames, from the old to the final position.
    click_row(&mut h, "Projects");
    let mut ys = vec![];
    for _ in 0..12 {
        h.step();
        ys.push(below(&h));
    }
    eprintln!("ANIM2 open, y of the row below per frame: {ys:?}");
    assert_eq!(st(&mut h).tree.rows().len(), rows0 + 3, "children are in the tree at once");
    // Mid-flight sample: frame 2 of the 0.08 s default transition (at 0.12 s frame 5 still moved).
    assert!(ys[0] < ys[2] && ys[2] < *ys.last().unwrap(), "slides down: {ys:?}");
    assert!(ys[0] < y0 + 20.0, "starts near the old place: {ys:?}");
    let final_y = *ys.last().unwrap();
    assert!(final_y > y0 + 40.0, "three rows further down: {y0} -> {final_y}");
    let _ = frames_until_quiet(&mut h);
    for _ in 0..3 {
        h.step();
        assert!(!wants_immediate_repaint(&h), "quiet after it settled");
    }
    // Close: the children stay in the tree until the block has shrunk, then they are gone.
    click_row(&mut h, "Projects");
    h.step();
    h.step();
    assert_eq!(st(&mut h).tree.rows().len(), rows0 + 3, "rows still there mid-animation");
    let mid = below(&h);
    assert!(mid < final_y && mid > y0, "half way: {mid}");
    let _ = shot(&mut h, "anim2_folder_closing_midway");
    frames_until_quiet(&mut h);
    run(&mut h, 3);
    assert_eq!(st(&mut h).tree.rows().len(), rows0, "closed");
    assert!((below(&h) - y0).abs() < 0.6, "back where it was");
    assert!(!wants_immediate_repaint(&h), "quiet");
}

#[test]
fn animations_off_switches_open_the_folder_in_one_frame() {
    for off in ["master", "tree"] {
        let fx = Fx::new();
        let mut h = ready(&fx, |c| match off {
            "master" => c.animations = false,
            _ => c.anim_tree = false,
        });
        let y0 = row_y(&h, "Welcome.md");
        click_row(&mut h, "Projects");
        run(&mut h, 2);
        let y1 = row_y(&h, "Welcome.md");
        run(&mut h, 2);
        assert_eq!(y1, row_y(&h, "Welcome.md"), "{off}: already at its final place");
        assert!(y1 > y0 + 40.0, "{off}");
        assert!(!wants_immediate_repaint(&h), "{off}: quiet");
    }
}

#[test]
fn sidebar_slides_out_and_in_without_touching_the_stored_width() {
    let fx = Fx::new();
    let mut h = ready(&fx, |_| {});
    let w0 = st(&mut h).config.sidebar_width;
    let edge = |h: &H| h.query_all_by_label_contains("Back").map(|n| n.rect().min.x).fold(f32::INFINITY, f32::min);
    let e0 = edge(&h);
    st(&mut h).commands.push(Command::ToggleSidebar);
    let mut xs = vec![];
    for _ in 0..12 {
        h.step();
        xs.push(edge(&h));
        assert_eq!(st(&mut h).config.sidebar_width, w0, "stored width untouched");
    }
    eprintln!("ANIM2 sidebar closing, edge x per frame: {xs:?}");
    assert!(xs.windows(2).all(|w| w[1] <= w[0] + 0.01), "monotone: {xs:?}");
    assert!(xs.iter().any(|x| *x < e0 - 5.0 && *x > 20.0), "seen in between: {xs:?}");
    assert!(*xs.last().unwrap() < 20.0, "closed");
    assert!(!wants_immediate_repaint(&h), "quiet");
    st(&mut h).commands.push(Command::ToggleSidebar);
    h.step();
    h.step();
    h.step();
    let _ = shot(&mut h, "anim2_sidebar_opening_midway");
    run(&mut h, 20);
    assert!((edge(&h) - e0).abs() < 0.6, "back at {e0}: {}", edge(&h));
    assert_eq!(st(&mut h).config.sidebar_width, w0);
    assert!(!wants_immediate_repaint(&h), "quiet");
}

#[test]
fn sidebar_toggle_is_instant_with_the_slide_switch_off() {
    let fx = Fx::new();
    let mut h = ready(&fx, |c| c.anim_slide = false);
    st(&mut h).commands.push(Command::ToggleSidebar);
    run(&mut h, 2);
    let edge = h.query_all_by_label_contains("Back").map(|n| n.rect().min.x).fold(f32::INFINITY, f32::min);
    assert!(edge < 20.0, "closed at once: {edge}");
}

#[test]
fn settings_list_the_animation_switches() {
    let fx = Fx::new();
    let mut h = ready(&fx, |_| {});
    st(&mut h).settings.open = true;
    st(&mut h).settings.section = iron_sheets::settings::Section::Interface;
    h.set_size(vec2(1100.0, 900.0));
    run(&mut h, 10);
    for label in ["Animations", "Animations: menus", "Animations: file tree"] {
        assert!(h.query_by_label(label).is_some(), "{label}");
    }
    let _ = shot(&mut h, "anim2_settings");
}

/// A folder with 2000 children: frame times while it opens and closes, and the steady frame after.
#[test]
fn big_folder_opens_without_laying_out_its_children() {
    let fx = Fx::new();
    let big = fx.notes().join("Zbig");
    std::fs::create_dir_all(&big).expect("dir");
    for i in 0..2000 {
        std::fs::write(big.join(format!("n{i:04}.md")), "# n\n").expect("write");
    }
    let mut h = ready(&fx, |_| {});
    st(&mut h).indexer.wait_idle(Duration::from_secs(20));
    run(&mut h, 20);
    let frame = |h: &mut H| {
        let t = Instant::now();
        h.step();
        t.elapsed()
    };
    let steady: Vec<_> = (0..30).map(|_| frame(&mut h)).collect();
    click_row(&mut h, "Zbig");
    let open: Vec<_> = (0..14).map(|_| frame(&mut h)).collect();
    let t = Instant::now();
    while st(&mut h).tree.has_pending_loads() && t.elapsed() < Duration::from_secs(10) {
        h.step();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(st(&mut h).tree.rows().len() > 2000, "all children in the tree");
    let steady2: Vec<_> = (0..30).map(|_| frame(&mut h)).collect();
    click_row(&mut h, "Zbig");
    let close: Vec<_> = (0..14).map(|_| frame(&mut h)).collect();
    let ms = |v: &[Duration]| v.iter().map(|d| d.as_secs_f64() * 1000.0).fold(0.0, f64::max);
    let med = |v: &[Duration]| {
        let mut s: Vec<f64> = v.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
        s.sort_by(f64::total_cmp);
        s[s.len() / 2]
    };
    eprintln!(
        "ANIM2 2000 children: steady closed median {:.2} ms; opening slowest {:.2} ms (median {:.2}); steady open median {:.2} ms (slowest {:.2}); closing slowest {:.2} ms",
        med(&steady),
        ms(&open),
        med(&open),
        med(&steady2),
        ms(&steady2),
        ms(&close)
    );
    assert!(ms(&open) < 250.0 && ms(&close) < 250.0);
}

#[test]
fn a_folder_closed_mid_animation_ends_closed_even_when_the_files_view_goes_away() {
    let fx = Fx::new();
    let mut h = ready(&fx, |_| {});
    let rows0 = st(&mut h).tree.rows().len();
    click_row(&mut h, "Projects");
    frames_until_quiet(&mut h);
    click_row(&mut h, "Projects");
    h.step();
    st(&mut h).commands.push(Command::SetSidebarView(iron_sheets::state::SidebarView::Search));
    run(&mut h, 3);
    assert_eq!(st(&mut h).tree.rows().len(), rows0, "the close is applied although the Files view is not drawn");
}

#[test]
fn a_second_click_mid_animation_reverses_from_where_it_is() {
    let fx = Fx::new();
    let mut h = ready(&fx, |_| {});
    let rows0 = st(&mut h).tree.rows().len();
    let y0 = row_y(&h, "Welcome.md");
    click_row(&mut h, "Projects");
    h.step();
    h.step();
    // Reverse while opening: from the frame the click lands the row only moves back up, and the folder ends closed.
    click_row(&mut h, "Projects");
    let mut ys = vec![];
    for _ in 0..14 {
        h.step();
        ys.push(row_y(&h, "Welcome.md"));
    }
    eprintln!("ANIM2 reverse, y per frame: {ys:?}");
    let top = ys.iter().cloned().fold(f32::MIN, f32::max);
    let k = ys.iter().position(|&y| y == top).unwrap_or(0);
    assert!(ys[k..].windows(2).all(|w| w[1] <= w[0] + 0.01), "moves back without restarting: {ys:?}");
    assert!(top < y0 + 66.0, "never fully opened: {ys:?}");
    frames_until_quiet(&mut h);
    run(&mut h, 3);
    assert_eq!(st(&mut h).tree.rows().len(), rows0, "ends closed");
    assert!((row_y(&h, "Welcome.md") - y0).abs() < 0.6);
}

/// Frame cost of the sidebar slide over a 1 MB note in split view, slide on vs off:
/// `cargo test -p iron-sheets --release --test it -- anim2:: --ignored --nocapture slide_cost`.
#[test]
#[ignore]
fn slide_cost_on_a_big_note() {
    let mut note = String::new();
    let mut i = 0;
    while note.len() < 1024 * 1024 {
        note.push_str(&format!("## Section {i}\n\nParagraph {i}: Lorem ipsum dolor sit amet, **consectetur** adipiscing elit, sed do eiusmod tempor [[Ideas]] and `code` and a [link](https://example.org).\n\n- item one {i}\n- [ ] task {i}\n\n"));
        i += 1;
    }
    for slide in [true, false] {
        let fx = Fx::new();
        let p = fx.notes().join("Big.md");
        std::fs::write(&p, &note).expect("write");
        let mut h = ready(&fx, |c| c.anim_slide = slide);
        st(&mut h).commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
        st(&mut h).commands.push(Command::SetViewMode(iron_sheets_core::config::ViewMode::Split));
        run(&mut h, 120);
        let mut steady = vec![];
        for _ in 0..20 {
            let t = Instant::now();
            h.step();
            steady.push(t.elapsed().as_secs_f64() * 1e3);
        }
        for dir in ["close", "open"] {
            st(&mut h).commands.push(Command::ToggleSidebar);
            let mut f = vec![];
            for _ in 0..12 {
                let t = Instant::now();
                h.step();
                f.push(t.elapsed().as_secs_f64() * 1e3);
            }
            let sum: f64 = f.iter().sum();
            eprintln!("SLIDE slide={slide} {dir}: frames {f:.1?} sum {sum:.1} ms");
        }
        steady.sort_by(f64::total_cmp);
        eprintln!("SLIDE slide={slide} steady median {:.2} ms", steady[10]);
    }
}
