//! Notes read in the background before they are opened: used when unchanged, ignored when the file changed.
use crate::common::*;
use iron_sheets::command::Command;
use std::time::Duration;

#[test]
fn opening_uses_the_prefetched_text_and_never_a_stale_one() {
    let fx = Fx::new();
    let a = fx.notes().join("A.md");
    let b = fx.notes().join("B.md");
    std::fs::write(&a, "# A\n\nalpha\n").expect("write");
    std::fs::write(&b, "# B\n\nbeta\n").expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    {
        let st = &mut h.state_mut().app().state;
        st.docs.prefetch.request(&a);
        st.docs.prefetch.wait_idle(Duration::from_secs(5));
        st.docs.prefetch.request(&b);
        st.docs.prefetch.wait_idle(Duration::from_secs(5));
        assert!(st.docs.prefetch.has(&a) && st.docs.prefetch.has(&b));
        // B changes on disk after it was read in the background.
        std::fs::write(&b, "# B\n\nbeta changed\n").expect("write");
        st.commands.push(Command::OpenPath { path: a.clone(), new_tab: true, line: None });
        st.commands.push(Command::OpenPath { path: b.clone(), new_tab: true, line: None });
    }
    settle(&mut h);
    let st = &mut h.state_mut().app().state;
    assert_eq!(st.docs.get(&a).map(|d| d.text.as_str()), Some("# A\n\nalpha\n"));
    assert_eq!(st.docs.get(&b).map(|d| d.text.as_str()), Some("# B\n\nbeta changed\n"), "the changed file is read again");
    // A is taken from the hover cache; B's stale entry is dropped and the background open reads it again.
    let (reads, _) = st.docs.prefetch.counters();
    assert_eq!(reads, 3, "A and B prefetched, B read again");
    assert_eq!(st.docs.sync_loads, 0, "nothing is read on the UI thread");
}
