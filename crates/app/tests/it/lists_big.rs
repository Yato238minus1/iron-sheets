//! IS-20 / IS-26: on a 2,000-note workspace the file tree and the search results lay out only the rows in view, scrolling
//! keeps the frame time flat, and opening a hit keeps the search state. Numbers go to stderr (`--nocapture`).
use crate::common::*;
use egui::Event;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;
use iron_sheets::state::SidebarView;
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;
const FOLDERS: usize = 40;
const PER_FOLDER: usize = 50;
/// Tall enough viewport (720 px) holds ~35 rows; anything near the file count would break this bound.
const ROW_BOUND: usize = 60;

fn fixture() -> Fx {
    let fx = Fx::new();
    for d in 0..FOLDERS {
        let dir = fx.notes().join("big").join(format!("d{d:02}"));
        std::fs::create_dir_all(&dir).expect("mkdir");
        for i in 0..PER_FOLDER {
            let n = d * PER_FOLDER + i;
            std::fs::write(dir.join(format!("n{n:04}.md")), format!("# Note {n}\n\nthe needle one\nplain\nthe needle two\nneedle three\n")).expect("write");
        }
    }
    fx
}

/// Wheels the list under `at` by `dy` per step; returns (max rows per frame, slowest step). A step is one harness
/// step, so it can hold several frames: the time is an upper bound for one frame.
fn scroll(h: &mut H, at: egui::Pos2, dy: f32, steps: usize, rows: impl Fn(&mut H) -> usize) -> (usize, Duration) {
    h.event(Event::PointerMoved(at));
    h.step();
    let (mut max_rows, mut max_t) = (0, Duration::ZERO);
    for _ in 0..steps {
        h.event(Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, dy),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
        });
        let t = Instant::now();
        h.step();
        max_t = max_t.max(t.elapsed());
        max_rows = max_rows.max(rows(h));
    }
    (max_rows, max_t)
}

#[test]
fn file_tree_lays_out_only_visible_rows() {
    let fx = fixture();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    {
        let st = &mut h.state_mut().app().state;
        let big = fx.notes().join("big");
        st.tree.set_expanded(&big, true);
    }
    settle(&mut h);
    {
        let st = &mut h.state_mut().app().state;
        for d in 0..FOLDERS {
            st.tree.set_expanded(&fx.notes().join("big").join(format!("d{d:02}")), true);
        }
    }
    settle(&mut h);
    let total = h.state_mut().app().state.tree.rows().len();
    assert!(total > FOLDERS * PER_FOLDER, "all folders expanded: {total} rows");
    let at = egui::pos2(120.0, 400.0);
    let tree_rows = |h: &mut H| h.state_mut().app().state.ui.files.visible_rows();
    let (r1, t1) = scroll(&mut h, at, -900.0, 60, tree_rows);
    // 60 steps x 900 px: well past the first folders, so the top rows are gone and later ones are painted.
    assert!(h.query_by_label("n0000.md").is_none(), "the tree scrolled down");
    shot(&mut h, "lists-tree-middle");
    let (r2, t2) = scroll(&mut h, at, -900.0, 60, tree_rows);
    let (r3, t3) = scroll(&mut h, at, 3000.0, 40, tree_rows);
    let (rows, worst) = (r1.max(r2).max(r3), t1.max(t2).max(t3));
    eprintln!("LISTS tree: {total} rows, max rows laid out per frame {rows}, slowest step {:.2} ms", worst.as_secs_f64() * 1e3);
    assert!((10..=ROW_BOUND).contains(&rows), "rows per frame {rows} of {total}");
    assert!(worst < Duration::from_millis(500), "worst step {worst:?}");
    // Down to the end: the last file is reachable and painted.
    scroll(&mut h, at, -60_000.0, 5, tree_rows);
    h.get_by_label("n1999.md");
    shot(&mut h, "lists-tree-end");
}

#[test]
fn search_results_are_virtualized_and_survive_opening_a_hit() {
    let fx = fixture();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.commands.push(Command::GlobalSearch { query: Some("needle".into()) });
    let start = Instant::now();
    loop {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
        let m = &h.state_mut().app().state.ui.search.model;
        if (m.status_finished() && m.counts().0 > 0) || start.elapsed() > Duration::from_secs(30) {
            break;
        }
    }
    let (hits, files, n_rows) = {
        let m = &h.state_mut().app().state.ui.search.model;
        (m.counts().0, m.counts().1, m.rows().len())
    };
    eprintln!("LISTS search: {hits} hits in {files} files, {n_rows} rows, finished in {:.0} ms", start.elapsed().as_secs_f64() * 1e3);
    assert!(hits >= iron_sheets_app_core::sidebar::search_model::MAX_HITS, "the cap is hit: {hits}");
    assert!(n_rows > 1000);
    shot(&mut h, "lists-search-top");
    let at = egui::pos2(120.0, 400.0);
    let painted = |h: &mut H| h.state_mut().app().state.ui.search.painted_rows;
    let (r1, t1) = scroll(&mut h, at, -900.0, 60, painted);
    shot(&mut h, "lists-search-middle");
    let (r2, t2) = scroll(&mut h, at, 900.0, 60, painted);
    let (rows, worst) = (r1.max(r2), t1.max(t2));
    eprintln!("LISTS search: max rows laid out per frame {rows} of {n_rows}, slowest step {:.2} ms", worst.as_secs_f64() * 1e3);
    assert!((10..=ROW_BOUND).contains(&rows), "rows per frame {rows}");
    assert!(worst < Duration::from_millis(500), "worst frame {worst:?}");

    // Open a hit; the Search view still has its query and results after a round trip through the Files view (IS-26).
    let cmd = {
        let m = &mut h.state_mut().app().state.ui.search.model;
        m.set_selected(Some(1));
        m.open_row(1, false)
    };
    h.state_mut().app().state.commands.extend(cmd);
    settle(&mut h);
    assert!(h.state_mut().app().state.tabs.active_path().is_some_and(|p| p.starts_with(fx.notes().join("big"))), "the hit opened");
    for v in [SidebarView::Files, SidebarView::Search] {
        h.state_mut().app().state.commands.push(Command::SetSidebarView(v));
        settle(&mut h);
    }
    let m = &h.state_mut().app().state.ui.search.model;
    assert_eq!(m.query, "needle");
    assert_eq!((m.counts().0, m.rows().len()), (hits, n_rows), "results kept");
    assert_eq!(m.selected(), Some(1), "selection kept");
    shot(&mut h, "lists-search-after-open");
}
