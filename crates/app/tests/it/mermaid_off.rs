//! Without the `mermaid` feature a Mermaid fence stays a plain code block, whatever `render_mermaid` says.
use crate::common::*;
use iron_sheets::command::Command;
use iron_sheets_core::config::ViewMode;

fn block_heights(render_mermaid: bool) -> Vec<f32> {
    let fx = Fx::new();
    let p = fx.notes().join("M.md");
    std::fs::write(&p, "# D\n\n```mermaid\nflowchart LR\n  A --> B\n```\n").expect("write");
    let mut o = fx.opts(vec![fx.notes()]);
    o.config.render_mermaid = render_mermaid;
    let mut h = harness_dt(o, 0.05);
    let st = &mut h.state_mut().app().state;
    st.commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
    st.commands.push(Command::SetViewMode(ViewMode::Preview));
    for _ in 0..10 {
        h.step();
    }
    let mut v: Vec<_> = h.state_mut().app().state.ui.md_preview.block_rects.iter().map(|(i, r)| (*i, r.height())).collect();
    v.sort_by_key(|(i, _)| *i);
    v.into_iter().map(|(_, h)| h).collect()
}

#[test]
fn mermaid_fence_is_a_code_block_without_the_feature() {
    let (on, off) = (block_heights(true), block_heights(false));
    assert_eq!(on.len(), 2, "{on:?}");
    assert_eq!(on, off);
}
