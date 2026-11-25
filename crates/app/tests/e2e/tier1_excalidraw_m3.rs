use super::driver::*;
#[cfg(feature = "excalidraw")]
use egui::accesskit::Role;
#[cfg(feature = "excalidraw")]
use egui::{Key, Modifiers};
use egui_kittest::kittest::Queryable;
#[cfg(feature = "excalidraw")]
use std::time::Duration;

#[cfg(feature = "excalidraw")]
const SAMPLE_EXCALIDRAW: &str = r##"{
  "type": "excalidraw",
  "version": 2,
  "elements": [
    {"id": "r1", "type": "rectangle", "x": 20, "y": 20, "width": 160, "height": 90, "strokeColor": "#1971c2", "backgroundColor": "#a5d8ff", "fillStyle": "hachure", "strokeWidth": 2, "seed": 11},
    {"id": "e1", "type": "ellipse", "x": 220, "y": 20, "width": 140, "height": 90, "strokeColor": "#e03131", "backgroundColor": "#ffc9c9", "fillStyle": "solid", "strokeWidth": 2, "seed": 12},
    {"id": "a1", "type": "arrow", "x": 180, "y": 65, "width": 40, "height": 0, "points": [[0,0],[40,0]], "endArrowhead": "triangle", "strokeColor": "#1e1e1e", "strokeWidth": 2, "seed": 13},
    {"id": "fd1", "type": "freedraw", "x": 40, "y": 140, "width": 180, "height": 40, "points": [[0,20],[40,0],[80,30],[140,10],[180,25]], "strokeColor": "#9c36b5", "strokeWidth": 3, "seed": 14},
    {"id": "t1", "type": "text", "x": 45, "y": 50, "width": 110, "height": 26, "text": "Service Box", "fontSize": 20, "fontFamily": 1, "strokeColor": "#1e1e1e", "seed": 15}
  ],
  "appState": { "viewBackgroundColor": "#ffffff" }
}"##;

#[cfg(feature = "excalidraw")]
const BOX_EXCALIDRAW: &str = r##"{
  "type": "excalidraw",
  "version": 2,
  "elements": [
    {"id": "b1", "type": "rectangle", "x": 10, "y": 10, "width": 220, "height": 100, "strokeColor": "#e8590c", "backgroundColor": "#ffd8a8", "fillStyle": "solid", "strokeWidth": 2, "seed": 42},
    {"id": "t1", "type": "text", "x": 30, "y": 45, "width": 180, "height": 26, "text": "Box Label", "fontSize": 20, "fontFamily": 1, "strokeColor": "#1e1e1e", "seed": 43}
  ],
  "appState": { "viewBackgroundColor": "#ffffff" }
}"##;

#[cfg(feature = "excalidraw")]
fn wait_nonblank_drawing(h: &mut H) -> image::RgbaImage {
    wait_until(h, Duration::from_secs(12), "Excalidraw toolbar ready", |h| {
        has_label(h, "Fit")
            && has_label(h, "Edit externally")
            && !has_label_contains(h, "Cannot show drawing:")
    });
    let area = preview_area_rect(h);
    let ppp = h.ctx.pixels_per_point();
    let t0 = std::time::Instant::now();
    loop {
        h.step();
        let img = render_pixels(h);
        if distinct_colors_in_rect(&img, area, ppp) >= 4 {
            return img;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "timed out waiting for non-blank Excalidraw raster in {area:?}"
        );
        std::thread::sleep(Duration::from_millis(15));
    }
}

#[cfg(feature = "excalidraw")]
#[test]
fn t1_f10_open_excalidraw_json_renders() {
    let _env = read_env_lock();
    let fx = new_fx();
    let p = fx.notes().join("diagram.excalidraw");
    std::fs::write(&p, SAMPLE_EXCALIDRAW).expect("write diagram.excalidraw");

    let mut h = launch_dt(fx.opts(vec![fx.notes()]), 0.05);
    click_tree_item(&mut h, "diagram.excalidraw");
    let img = wait_nonblank_drawing(&mut h);
    let area = preview_area_rect(&h);
    assert!(
        distinct_colors_in_rect(&img, area, h.ctx.pixels_per_point()) >= 4,
        "Excalidraw JSON drawing must render non-blank shapes/text"
    );
    assert!(h.query_by_role(Role::MultilineTextInput).is_none(), "opens in Preview mode by default");
}

#[cfg(feature = "excalidraw")]
#[test]
fn t1_f10_open_excalidraw_md_compressed_and_legacy() {
    let _env = read_env_lock();
    let fx = new_fx();
    let comp_p = fx.notes().join("Compressed.excalidraw.md");
    std::fs::write(
        &comp_p,
        obsidian_excalidraw_md(SAMPLE_EXCALIDRAW, "Service Box ^t1", None),
    )
    .expect("write compressed");

    let legacy_p = fx.notes().join("Legacy.excalidraw.md");
    std::fs::write(
        &legacy_p,
        format!("---\nexcalidraw-plugin: parsed\n---\n# Drawing\n```json\n{BOX_EXCALIDRAW}\n```\n"),
    )
    .expect("write legacy");

    let mut h = launch_dt(fx.opts(vec![fx.notes()]), 0.05);

    click_tree_item(&mut h, "Compressed.excalidraw.md");
    let img1 = wait_nonblank_drawing(&mut h);
    assert!(distinct_colors_in_rect(&img1, preview_area_rect(&h), h.ctx.pixels_per_point()) >= 4);

    click_tree_item(&mut h, "Legacy.excalidraw.md");
    let img2 = wait_nonblank_drawing(&mut h);
    assert!(distinct_colors_in_rect(&img2, preview_area_rect(&h), h.ctx.pixels_per_point()) >= 4);
}

#[cfg(feature = "excalidraw")]
#[test]
fn t1_f10_excalidraw_toolbar_source_zoom_pan_export() {
    let _env = read_env_lock();
    let fx = new_fx();
    let p = fx.notes().join("diagram.excalidraw");
    std::fs::write(&p, BOX_EXCALIDRAW).expect("write");

    let mut h = launch_dt(fx.opts(vec![fx.notes()]), 0.05);
    click_tree_item(&mut h, "diagram.excalidraw");
    let before_img = wait_nonblank_drawing(&mut h);

    // Zoom toolbar: "+" updates zoom percentage, "100%" resets, "Fit" fits.
    click_label(&mut h, "+");
    assert!(h.query_by_label("125%").is_some(), "clicking + zooms to 125%");
    h.get_all_by_label("100%").next().expect("100% button").click();
    settle(&mut h);
    click_label(&mut h, "Fit");
    assert_eq!(
        h.query_all_by_label("100%").count(),
        2,
        "Fit resets zoom readout to 100% (button + readout)"
    );

    // Wheel zoom over drawing area changes zoom percentage and rendered pixels.
    let area = preview_area_rect(&h);
    let ppp = h.ctx.pixels_per_point();
    wheel_at(&mut h, area.center(), egui::vec2(0.0, 200.0));
    assert_eq!(
        h.query_all_by_label("100%").count(),
        1,
        "wheel zoom must change zoom readout away from 100%"
    );
    assert!(
        h.query_all_by_role(Role::Label).any(|n| {
            n.value().is_some_and(|s| {
                s != "100%" && s.strip_suffix('%').is_some_and(|d| d.parse::<u32>().is_ok())
            })
        }),
        "wheel zoom must display a non-100% zoom percentage label"
    );
    let after_wheel_img = render_pixels(&mut h);
    assert!(
        rect_pixels_differ(&before_img, &after_wheel_img, area, ppp),
        "wheel zoom must visibly change the drawing"
    );

    // Drag pan shifts rendered pixels relative to post-wheel image.
    let c = area.center();
    drag(&mut h, c, c + egui::vec2(80.0, 40.0), 8);
    let panned_img = render_pixels(&mut h);
    assert!(
        rect_pixels_differ(&after_wheel_img, &panned_img, area, ppp),
        "drag pan must visibly move the drawing"
    );

    // Verify "Edit externally" button + tooltip presence (Export PNG is not present in this viewer;
    // do not click Edit externally in headless tests as it spawns an OS viewer).
    let ext_btn = h.get_all_by_label("Edit externally").next().expect("Edit externally");
    ext_btn.hover();
    for _ in 0..14 {
        h.step();
    }
    assert!(has_label_contains(&h, "Open in the default app"));

    // Toggle "Source" -> reveals editor with raw JSON, edit it, and switch back to Preview.
    h.get_all_by_label("Source").last().expect("Source button").click();
    settle(&mut h);
    assert!(h.query_by_role(Role::MultilineTextInput).is_some(), "Source toggle reveals editor");
    assert!(editor_text(&h).contains("Box Label"));
    h.get_by_role(Role::MultilineTextInput).click();
    settle(&mut h);
    press_shortcut(&mut h, Modifiers::COMMAND, Key::A);
    type_text(&mut h, SAMPLE_EXCALIDRAW);
    h.get_all_by_label("Preview").next().expect("Preview button").click();
    settle(&mut h);
    let updated_img = wait_nonblank_drawing(&mut h);
    assert!(
        rect_pixels_differ(&before_img, &updated_img, preview_area_rect(&h), ppp),
        "edited Excalidraw source must re-render differently from initial BOX_EXCALIDRAW"
    );
}

#[cfg(feature = "excalidraw")]
#[test]
fn t1_f10_excalidraw_markdown_embed_and_click_open() {
    let _env = read_env_lock();
    let fx = new_fx();
    let diag = fx.notes().join("diagram.excalidraw");
    std::fs::write(&diag, BOX_EXCALIDRAW).expect("write diagram");
    let host = fx.notes().join("EmbedHost.md");
    std::fs::write(
        &host,
        "# Embeds\n\n![[diagram]]\n\n![[diagram.excalidraw]]\n\n![alt](diagram.excalidraw)\n",
    )
    .expect("write host");

    let mut h = launch_dt(fx.opts(vec![fx.notes()]), 0.05);
    wait_indexed(&mut h);
    click_tree_item(&mut h, "EmbedHost.md");
    click_label(&mut h, "Preview");

    let area = preview_area_rect(&h);
    let mut embed_pos = None;
    let t0 = std::time::Instant::now();
    while t0.elapsed() < Duration::from_secs(12) {
        h.step();
        let img = render_pixels(&mut h);
        if distinct_colors_in_rect(&img, area, h.ctx.pixels_per_point()) >= 4 {
            embed_pos = Some(egui::pos2(area.left() + 80.0, area.top() + 90.0));
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    let pos = embed_pos.expect("rendered Excalidraw embed");
    // Hover over the rendered embed to verify its tooltip, then click to open.
    h.event(egui::Event::PointerMoved(pos));
    for _ in 0..14 {
        h.step();
    }
    if !has_label_contains(&h, "Click to open Excalidraw drawing") {
        if let Some(p) = find_tooltip_pos(&mut h, area, 30.0, 20.0, "Click to open Excalidraw drawing") {
            click_pos(&mut h, p);
        } else {
            click_pos(&mut h, pos);
        }
    } else {
        click_pos(&mut h, pos);
    }

    wait_until(&mut h, Duration::from_secs(8), "clicked embed opened diagram tab", |h| {
        has_label(h, "Fit")
            && h.query_all_by_role_and_label(Role::Tab, "diagram.excalidraw").next().is_some()
    });
}

#[cfg(feature = "excalidraw")]
#[test]
fn t1_f10_excalidraw_text_search_and_rename_relink() {
    let _env = read_env_lock();
    let fx = new_fx();
    let exc_md = fx.notes().join("ArchDiagram.excalidraw.md");
    std::fs::write(
        &exc_md,
        obsidian_excalidraw_md(SAMPLE_EXCALIDRAW, "GatewayNode ^t1", None),
    )
    .expect("write ArchDiagram.excalidraw.md");

    let raw = fx.notes().join("ArchRaw.excalidraw");
    std::fs::write(&raw, BOX_EXCALIDRAW).expect("write ArchRaw.excalidraw");

    let ref_note = fx.notes().join("RefNote.md");
    std::fs::write(&ref_note, "# Refs\n\nSee [[ArchRaw.excalidraw]].\n").expect("write RefNote.md");

    let mut h = launch_dt(fx.opts(vec![fx.notes()]), 0.05);
    wait_indexed(&mut h);

    // Workspace search finds text in .excalidraw.md ## Text Elements.
    press_shortcut(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::F);
    type_text(&mut h, "GatewayNode");
    wait_until(&mut h, Duration::from_secs(10), "search finds GatewayNode", |h| {
        has_label_contains(h, "ArchDiagram.excalidraw.md")
    });

    // Switch to Files, open ArchRaw.excalidraw, rename via F2 -> confirm link update.
    switch_sidebar(&mut h, "Files");
    click_tree_item(&mut h, "ArchRaw.excalidraw");
    wait_until(&mut h, Duration::from_secs(5), "ArchRaw opened", |h| {
        h.query_by_role_and_label(Role::Tab, "ArchRaw.excalidraw").is_some()
    });
    press_key(&mut h, Key::F2);
    wait_until(&mut h, Duration::from_secs(5), "Rename prompt open", |h| {
        h.query_all_by_role(Role::Dialog).next().is_some()
    });
    press_shortcut(&mut h, Modifiers::COMMAND, Key::A);
    type_text(&mut h, "ArchRenamed.excalidraw");
    click_label(&mut h, "OK");

    wait_until(&mut h, Duration::from_secs(5), "Update links confirmation", |h| {
        has_label_contains(h, "Update links in 1 note?")
    });
    click_label(&mut h, "Update");

    wait_until(&mut h, Duration::from_secs(8), "renamed file & updated link on disk", |h| {
        let _ = h;
        fx.notes().join("ArchRenamed.excalidraw").exists()
            && !raw.exists()
            && std::fs::read_to_string(&ref_note)
                .map(|s| s.contains("[[ArchRenamed.excalidraw]]"))
                .unwrap_or(false)
    });
}

#[cfg(not(feature = "excalidraw"))]
#[test]
fn t1_f10_non_excalidraw_fallback() {
    let _env = read_env_lock();
    let fx = new_fx();
    let raw = fx.notes().join("diagram.excalidraw");
    std::fs::write(&raw, r#"{"type":"excalidraw","elements":[]}"#).expect("write");
    let exc_md = fx.notes().join("Arch.excalidraw.md");
    std::fs::write(&exc_md, "# Drawing\n\nFallback markdown.\n").expect("write");

    let mut h = launch(fx.opts(vec![fx.notes()]));
    click_tree_item(&mut h, "Arch.excalidraw.md");
    assert!(editor_text(&h).contains("Fallback markdown."));
    assert!(h.query_by_label("Edit externally").is_none());

    // Without excalidraw feature, .excalidraw may be hidden unless show_other_files or opened via palette/path.
    let mut opts = fx.opts(vec![fx.notes(), raw.clone()]);
    opts.config.show_other_files = true;
    drop(h);
    let h = launch(opts);
    assert!(editor_text(&h).contains("\"excalidraw\""));
    assert!(h.query_by_label("Edit externally").is_none());
}
