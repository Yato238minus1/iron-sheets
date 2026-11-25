//! Preview stability while images and math load: frames until the first image shows and how many block heights
//! change after the first frame (layout jumps). Run with `-- --nocapture` for the numbers.
use crate::common::*;
use egui_kittest::Harness;
use iron_sheets::command::Command;
use iron_sheets_core::config::{ThemePref, ViewMode};
use std::time::Duration;

type H = Harness<'static, Slot>;

fn prev(h: &mut H) -> &mut iron_sheets::preview::markdown::MarkdownPreview {
    &mut h.state_mut().app().state.ui.md_preview
}

/// 30 sections: text, an image (sizes vary), inline math, every third also display math.
fn note(fx: &Fx) -> String {
    let mut s = String::from("# Stable\n\n");
    for i in 0..30u32 {
        // Noise does not compress: a decode takes a few ms, like a photo.
        let (w, h) = (1200 + (i % 5) * 100, 700 + (i % 4) * 60);
        let mut r = 0x9E37_79B9u32 ^ i;
        let img = image::RgbaImage::from_fn(w, h, |_, _| {
            r ^= r << 13;
            r ^= r >> 17;
            r ^= r << 5;
            image::Rgba([r as u8, (r >> 8) as u8, (r >> 16) as u8, 255])
        });
        img.save(fx.notes().join(format!("img{i}.png"))).expect("png");
        s.push_str(&format!("Section {i} has inline $a_{i}^2 + b$ math and some text to fill the line.\n\n![[img{i}.png]]\n\n"));
        if i % 3 == 0 {
            s.push_str(&format!("$$ sum_(k=1)^n k^{i} = (n(n+1))/2 $$\n\n"));
        }
    }
    s
}

#[test]
fn images_and_math_load_without_jumping() {
    let fx = Fx::new();
    let text = note(&fx);
    let p = fx.notes().join("Host.md");
    std::fs::write(&p, &text).expect("write");
    let mut o = fx.opts(vec![fx.notes()]);
    o.config.theme = ThemePref::Light;
    let mut h = harness_dt(o, 0.016);
    {
        let st = &mut h.state_mut().app().state;
        st.commands.push(Command::OpenPath { path: p.clone(), new_tab: true, line: None });
        st.commands.push(Command::SetViewMode(ViewMode::Preview));
    }
    h.event(egui::Event::PointerMoved(egui::pos2(700.0, 400.0)));
    let (mut seen_changes, mut frames_to_image) = (0usize, None);
    let mut seen: std::collections::HashSet<usize> = Default::default();
    let mut last: Vec<f32> = Vec::new();
    // Frames 0..: until the first image shows (capped), then 100 wheel steps through the whole note and a few more frames.
    for f in 0..900 {
        let scrolling = frames_to_image.is_some_and(|s| (s + 5..s + 105).contains(&f));
        if scrolling {
            h.event(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -250.0),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Unknown,
                modifiers: egui::Modifiers::NONE,
            });
        }
        h.step();
        let m = prev(&mut h);
        if frames_to_image.is_none() && m.hits.iter().any(|(_, l)| l == "image") {
            frames_to_image = Some(f);
        }
        let tops = m.tops_of(&p).map(<[f32]>::to_vec).unwrap_or_default();
        if !last.is_empty() {
            for &b in &seen {
                if b + 1 < last.len().min(tops.len()) && ((last[b + 1] - last[b]) - (tops[b + 1] - tops[b])).abs() > 0.01 {
                    seen_changes += 1;
                    println!("frame {f}: block {b} {:.1} -> {:.1}", last[b + 1] - last[b], tops[b + 1] - tops[b]);
                }
            }
        }
        seen.extend(m.block_rects.iter().map(|(b, _)| *b));
        last = tops;
        if frames_to_image.is_some_and(|s| f > s + 140) {
            break;
        }
        std::thread::sleep(Duration::from_millis(3));
    }
    shot(&mut h, "preview-stable-end");
    println!("first image after {frames_to_image:?} frames; height changes of blocks already seen on screen: {seen_changes}");
    assert!(frames_to_image.is_some(), "an image is painted");
    // Measured 2-3 (math landing on different frames under load); before: 10.
    assert!(seen_changes <= 5, "{seen_changes} blocks changed height after they were on screen");
}

#[test]
fn images_just_below_the_viewport_decode_before_they_are_scrolled_to() {
    let fx = Fx::new();
    let text = note(&fx);
    let p = fx.notes().join("Host.md");
    std::fs::write(&p, &text).expect("write");
    let mut h = harness_dt(fx.opts(vec![fx.notes()]), 0.016);
    {
        let st = &mut h.state_mut().app().state;
        st.commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
        st.commands.push(Command::SetViewMode(ViewMode::Preview));
    }
    for _ in 0..600 {
        h.step();
        if prev(&mut h).hits.iter().any(|(_, l)| l == "image") {
            break;
        }
        std::thread::sleep(Duration::from_millis(3));
    }
    for _ in 0..60 {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    let m = prev(&mut h);
    let visible = m.hits.iter().filter(|(_, l)| l == "image").count();
    // Each image is about 460 px; the 720 px window shows one or two, the margin adds one viewport below (and none above).
    assert!(m.images_started() > visible, "started {} images, {visible} on screen", m.images_started());
    assert!(m.images_started() < 15, "far images are not decoded ahead: {}", m.images_started());
}
