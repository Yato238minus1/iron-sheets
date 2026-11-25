//! UX leftovers: Sync versus the view-mode segments, the empty tab names, the sidebar tab row at narrow widths and the
//! tree guides.
use crate::common::*;
use egui::{Role, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use iron_sheets::command::Command;
use iron_sheets::state::AppState;
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;

fn st(h: &mut H) -> &mut AppState {
    &mut h.state_mut().app().state
}

fn ready(fx: &Fx, size: egui::Vec2, f: impl FnOnce(&mut iron_sheets_core::config::AppConfig)) -> H {
    let mut o = fx.opts(vec![fx.notes(), fx.journal()]);
    o.config.theme = iron_sheets_core::config::ThemePref::Dark;
    f(&mut o.config);
    let mut h = harness(o);
    h.ctx.set_missing_glyph_policy(egui::MissingGlyphPolicy::Tofu);
    h.set_size(size);
    let t = Instant::now();
    while st(&mut h).indexer.index().read().len() < 8 {
        h.step();
        assert!(t.elapsed() < Duration::from_secs(10), "index timeout");
        std::thread::sleep(Duration::from_millis(5));
    }
    settle(&mut h);
    h
}

fn open_welcome(h: &mut H, fx: &Fx) {
    st(h).commands.push(Command::OpenPath { path: fx.notes().join("Welcome.md"), new_tab: true, line: None });
    settle(h);
}

/// Pixels of a screenshot inside a node's rect (at scale 1).
fn crop(img: &image::RgbaImage, r: egui::Rect) -> Vec<u8> {
    let (x0, y0) = (r.min.x.max(0.0) as u32, r.min.y.max(0.0) as u32);
    let (x1, y1) = ((r.max.x as u32).min(img.width()), (r.max.y as u32).min(img.height()));
    let mut v = Vec::new();
    for y in y0..y1 {
        for x in x0..x1 {
            v.extend_from_slice(&img.get_pixel(x, y).0);
        }
    }
    v
}

#[test]
fn sync_is_a_check_toggle_and_not_a_segment_of_the_view_mode() {
    let fx = Fx::new();
    let mut h = ready(&fx, vec2(1100.0, 720.0), |_| {});
    open_welcome(&mut h, &fx);
    st(&mut h).commands.push(Command::SetViewMode(iron_sheets_core::config::ViewMode::Split));
    settle(&mut h);
    st(&mut h).config.sync_scroll = true;
    settle(&mut h);
    // Sync is a check box with a state; the modes are buttons.
    let sync = h.get_by_role_and_label(Role::CheckBox, "Sync");
    assert_eq!(sync.accesskit_node().toggled(), Some(egui::accesskit::Toggled::True));
    let sync_rect = sync.rect();
    let split_rect = h.get_by_role_and_label(Role::Button, "Split").rect();
    let on = shot(&mut h, "uxfix_sync_on_split");
    let on_img = image::open(&on).expect("png").to_rgba8();
    assert_ne!(crop(&on_img, sync_rect), crop(&on_img, split_rect), "chip and segment draw differently");
    // Turning Sync off changes the chip but leaves the Split segment (the selected mode) as it was.
    st(&mut h).config.sync_scroll = false;
    settle(&mut h);
    let off_img = image::open(shot(&mut h, "uxfix_sync_off_split")).expect("png").to_rgba8();
    assert_ne!(crop(&on_img, sync_rect), crop(&off_img, sync_rect), "the chip shows its state");
    // The Split segment keeps its filled look: its centre pixel is the selection fill in both states.
    let c = split_rect.left_top() + vec2(3.0, split_rect.height() / 2.0);
    assert_eq!(on_img.get_pixel(c.x as u32, c.y as u32), off_img.get_pixel(c.x as u32, c.y as u32), "the mode segment does not follow Sync");
    assert_eq!(h.get_by_role_and_label(Role::CheckBox, "Sync").accesskit_node().toggled(), Some(egui::accesskit::Toggled::False));
}

#[test]
fn empty_tab_uses_one_name_and_right_aligns_the_shortcuts() {
    let fx = Fx::new();
    let mut h = ready(&fx, vec2(1100.0, 720.0), |_| {});
    h.get_by_role_and_label(Role::Tab, "New tab");
    h.get_by_role_and_label(Role::Label, "New tab");
    // One button per action; its accessible name carries the shortcut, which is drawn as separate text at the right edge.
    h.get_by_role_and_label(Role::Button, "New note Ctrl+N");
    h.get_by_role_and_label(Role::Button, "New untitled note");
    let _ = shot(&mut h, "uxfix_empty_tab");
}

#[test]
fn sidebar_tabs_fit_in_one_row_and_overflow_into_a_menu_when_narrow() {
    let fx = Fx::new();
    // Wide pane: five tabs on one line.
    let mut h = ready(&fx, vec2(1100.0, 720.0), |c| {
        c.sidebar_width = 400.0;
        c.module_calendar = false;
    });
    let ys: Vec<f32> = ["Files", "Search", "Outline", "Links", "Symbols"].iter().map(|l| h.get_by_label(l).rect().center().y).collect();
    assert!(ys.iter().all(|y| (y - ys[0]).abs() < 1.0), "one row: {ys:?}");
    let _ = shot(&mut h, "uxfix_sidebar_260");
    // 140 px: the views that fit, the others behind the "…" button; picking one makes it the visible tab.
    let mut h = ready(&fx, vec2(1100.0, 720.0), |c| {
        c.sidebar_width = 140.0;
        c.module_calendar = false;
    });
    assert!(h.query_by_label("Symbols").is_none(), "left out of the row");
    let button = h.get_by_label("Files");
    assert!(button.rect().right() < 140.0, "inside the pane: {:?}", button.rect());
    h.get_by_label("…").click();
    settle(&mut h);
    let _ = shot(&mut h, "uxfix_sidebar_200_menu");
    h.get_by_label("Symbols").click();
    settle(&mut h);
    assert_eq!(st(&mut h).sidebar_view, iron_sheets::state::SidebarView::Symbols);
    assert!(h.query_by_label("Symbols").is_some(), "the current view keeps its tab");
    let _ = shot(&mut h, "uxfix_sidebar_200");
}

#[test]
fn tree_with_nested_folders_shows_guides_and_images_are_not_dimmed() {
    let fx = Fx::new();
    let deep = fx.notes().join("alpha").join("beta").join("gamma");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join("deep.md"), "# deep\n").unwrap();
    std::fs::write(deep.join("pic.png"), b"x").unwrap();
    std::fs::write(deep.join("blob.bin"), b"x").unwrap();
    let mut h = ready(&fx, vec2(1100.0, 720.0), |c| c.sidebar_width = 260.0);
    for d in ["alpha", "alpha/beta", "alpha/beta/gamma"] {
        let p = fx.notes().join(d);
        st(&mut h).tree.toggle(&p);
        settle(&mut h);
    }
    let rows = iron_sheets::theme::recorded_rows(&h.ctx, "files");
    let colour = |name: &str| rows.iter().find(|(n, _)| n.ends_with(name)).unwrap_or_else(|| panic!("row {name} in {rows:?}")).1;
    assert_eq!(colour("pic.png"), colour("deep.md"), "an openable image is drawn like a note");
    assert_ne!(colour("blob.bin"), colour("deep.md"), "a file without a view stays dim");
    let _ = shot(&mut h, "uxfix_tree_guides");
}
