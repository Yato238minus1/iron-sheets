use crate::common::*;
use egui::{Event, Key, Modifiers, PointerButton, Pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;
use iron_sheets_core::config::ViewMode;
use std::time::Instant;

const BOARD: &str = "---\nkanban-plugin: basic\n---\n\n## Todo\n\n- [ ] Write **spec** for [[Ideas]]\n- [ ] Review `code` #urgent\n\n**Complete**\n\n- [x] Set up repo\n\n## Doing\n\n- [ ] Parser with a rather long title that has to wrap over several lines of the card\n  second line\n- [ ] Last one\n\n## Empty\n\n***\n\n## Archive\n\n- [x] Old card\n\n%% kanban:settings\n```\n{\"kanban-plugin\":\"basic\"}\n```\n%%\n";

fn open(fx: &Fx, text: &str, view: ViewMode) -> Harness<'static, Slot> {
    let mut h = harness_dt(fx.opts(vec![fx.notes()]), 1.0 / 60.0);
    let p = fx.notes().join("board.md");
    std::fs::write(&p, text).expect("write");
    {
        let st = &mut h.state_mut().app().state;
        st.commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
        st.commands.push(Command::SetViewMode(view));
    }
    settle(&mut h);
    h
}

fn text(h: &mut Harness<'static, Slot>) -> String {
    let st = &mut h.state_mut().app().state;
    let p = st.tabs.tabs[st.tabs.active.expect("tab")].path.clone();
    st.docs.get(&p).expect("doc").text.clone()
}

fn center(h: &Harness<'static, Slot>, label: &str) -> Pos2 {
    h.query_all_by_label_contains(label).next().unwrap_or_else(|| panic!("no {label}")).rect().center()
}

fn press(h: &mut Harness<'static, Slot>, pos: Pos2, pressed: bool) {
    h.event(Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
    h.step();
}

fn click(h: &mut Harness<'static, Slot>, pos: Pos2) {
    h.event(Event::PointerMoved(pos));
    h.step();
    press(h, pos, true);
    press(h, pos, false);
}

#[test]
fn board_renders_toggles_and_edits() {
    let fx = Fx::new();
    let mut h = open(&fx, BOARD, ViewMode::Preview);
    shot(&mut h, "kanban_board");
    // The checkbox of the first card toggles done in the text only.
    let cb = center(&h, "Write");
    click(&mut h, cb);
    settle(&mut h);
    assert_eq!(text(&mut h), BOARD.replacen("- [ ] Write", "- [x] Write", 1));
    // A double click edits, Enter saves.
    let p = center(&h, "Last one") + egui::vec2(80.0, 0.0);
    // Clicks of an earlier gesture within 0.3 s would make this a triple click.
    for _ in 0..30 {
        h.step();
    }
    click(&mut h, p);
    click(&mut h, p);
    settle(&mut h);
    shot(&mut h, "kanban_editing");
    h.event(Event::Text(" edited".into()));
    h.step();
    h.event(Event::Key { key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
    settle(&mut h);
    assert!(text(&mut h).contains("- [ ] Last one edited\n"), "{}", text(&mut h));
}

#[test]
fn drag_moves_a_card_between_lanes() {
    let fx = Fx::new();
    let mut h = open(&fx, BOARD, ViewMode::Preview);
    let from = center(&h, "Review") + egui::vec2(80.0, 0.0);
    let to = center(&h, "Last one") + egui::vec2(80.0, 0.0);
    h.event(Event::PointerMoved(from));
    h.step();
    press(&mut h, from, true);
    for k in 1..=10 {
        h.event(Event::PointerMoved(from + (to - from) * (k as f32 / 10.0) + egui::vec2(0.0, -8.0)));
        h.step();
    }
    shot(&mut h, "kanban_drag");
    press(&mut h, to + egui::vec2(0.0, -8.0), false);
    settle(&mut h);
    let t = text(&mut h);
    assert!(t.contains("- [ ] Review `code` #urgent\n- [ ] Last one"), "{t}");
    assert!(t.ends_with("%%\n") && t.contains("## Archive"));
}

#[test]
fn big_board_frames_stay_cheap() {
    let mut s = String::from("---\nkanban-plugin: basic\n---\n");
    for l in 0..5 {
        s += &format!("\n## Lane {l}\n\n");
        for c in 0..200 {
            s += &format!("- [ ] card {l}-{c} with some **text** to wrap around the width of the lane\n");
        }
    }
    let fx = Fx::new();
    let mut h = open(&fx, &s, ViewMode::Preview);
    let t = Instant::now();
    for _ in 0..100 {
        h.step();
    }
    eprintln!("steady frame: {:?}", t.elapsed() / 100);
    let from = center(&h, "card 0-1 ") + egui::vec2(80.0, 0.0);
    h.event(Event::PointerMoved(from));
    h.step();
    press(&mut h, from, true);
    let t = Instant::now();
    for k in 0..50 {
        h.event(Event::PointerMoved(from + egui::vec2(k as f32 * 4.0, k as f32)));
        h.step();
    }
    eprintln!("drag frame: {:?}", t.elapsed() / 50);
    shot(&mut h, "kanban_big");
}

#[test]
fn empty_board_offers_a_lane() {
    let fx = Fx::new();
    let mut h = open(&fx, "---\nkanban-plugin: basic\n---\n", ViewMode::Preview);
    h.get_by_label("+ Add lane").click();
    settle(&mut h);
    assert!(text(&mut h).ends_with("## New lane\n"), "{}", text(&mut h));
}

#[test]
fn editing_is_dropped_when_the_text_changed_underneath() {
    let fx = Fx::new();
    let mut h = open(&fx, BOARD, ViewMode::Preview);
    for _ in 0..30 {
        h.step();
    }
    let p = center(&h, "Last one") + egui::vec2(80.0, 0.0);
    click(&mut h, p);
    click(&mut h, p);
    settle(&mut h);
    // Another pane changes the note: card indices no longer mean the same cards.
    let changed = {
        let st = &mut h.state_mut().app().state;
        let path = st.tabs.tabs[st.tabs.active.expect("tab")].path.clone();
        let d = st.docs.get_mut(&path).expect("doc");
        let at = d.text.find("## Todo").expect("lane");
        d.apply_edit(at..at, "## New\n\n- [ ] inserted\n\n");
        d.text.clone()
    };
    h.event(Event::Text(" typed".into()));
    h.event(Event::Key { key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
    settle(&mut h);
    assert_eq!(text(&mut h), changed);
}

#[test]
fn drag_released_outside_the_window_changes_nothing() {
    let fx = Fx::new();
    let mut h = open(&fx, BOARD, ViewMode::Preview);
    let from = center(&h, "Review") + egui::vec2(80.0, 0.0);
    h.event(Event::PointerMoved(from));
    h.step();
    press(&mut h, from, true);
    for k in 1..=5 {
        h.event(Event::PointerMoved(from + egui::vec2(k as f32 * 20.0, 0.0)));
        h.step();
    }
    h.event(Event::PointerGone);
    press(&mut h, from, false);
    settle(&mut h);
    assert_eq!(text(&mut h), BOARD);
}

#[test]
fn module_off_shows_a_board_note_as_plain_markdown_and_on_brings_the_board_back() {
    let fx = Fx::new();
    let mut o = fx.opts(vec![fx.notes()]);
    o.config.module_kanban = false;
    o.config.module_codehl = false;
    let mut h = harness_dt(o, 1.0 / 60.0);
    let p = fx.notes().join("board.md");
    std::fs::write(&p, "---\nkanban-plugin: basic\n---\n").expect("write");
    {
        let st = &mut h.state_mut().app().state;
        assert!(!st.modules.enabled(iron_sheets_app_core::modules::KANBAN), "a disabled module is not built");
        st.commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
        st.commands.push(Command::SetViewMode(ViewMode::Preview));
    }
    settle(&mut h);
    assert!(h.query_by_label("+ Add lane").is_none(), "no board view while the module is off");
    {
        let st = &mut h.state_mut().app().state;
        st.config.module_kanban = true;
        iron_sheets::settings_model::apply(st, false);
        assert!(st.modules.enabled(iron_sheets_app_core::modules::KANBAN));
    }
    settle(&mut h);
    h.get_by_label("+ Add lane");
}

#[test]
fn turning_the_module_off_drops_an_open_board() {
    let fx = Fx::new();
    let mut h = open(&fx, "---\nkanban-plugin: basic\n---\n", ViewMode::Preview);
    h.get_by_label("+ Add lane");
    {
        let st = &mut h.state_mut().app().state;
        st.config.module_kanban = false;
        iron_sheets::settings_model::apply(st, false);
        assert!(!st.modules.enabled(iron_sheets_app_core::modules::KANBAN));
    }
    settle(&mut h);
    assert!(h.query_by_label("+ Add lane").is_none());
}

fn menu_item(h: &mut Harness<'static, Slot>, lane: usize, item: &str) {
    let m = h.get_all_by_label("Lane menu").nth(lane).expect("lane menu").rect().center();
    click(h, m);
    settle(h);
    let it = center(h, item);
    click(h, it);
    settle(h);
}

#[test]
fn lanes_are_deleted_from_their_menu_and_ask_first_when_they_have_cards() {
    let fx = Fx::new();
    // An empty lane goes at once.
    let small = "---\nkanban-plugin: basic\n---\n\n## Empty\n\n## Full\n\n- [ ] one\n";
    let mut h = open(&fx, small, ViewMode::Preview);
    menu_item(&mut h, 0, "Delete lane");
    assert_eq!(text(&mut h), small.replacen("## Empty\n\n", "", 1));
    let fx = Fx::new();
    let mut h = open(&fx, BOARD, ViewMode::Preview);
    // A lane with cards asks; Cancel keeps it.
    let before = text(&mut h);
    menu_item(&mut h, 1, "Delete lane");
    shot(&mut h, "polish2-kanban-confirm");
    assert!(h.query_by_label_contains("and its 2 cards").is_some());
    assert_eq!(text(&mut h), before);
    let c = center(&h, "Cancel");
    click(&mut h, c);
    settle(&mut h);
    assert!(h.query_by_label_contains("and its 2 cards").is_none() && text(&mut h) == before);
    // Delete removes the lane and its cards, the archive tail stays.
    menu_item(&mut h, 1, "Delete lane");
    let d = h.get_by_label("Delete").rect().center();
    click(&mut h, d);
    settle(&mut h);
    let t = text(&mut h);
    assert!(!t.contains("## Doing") && !t.contains("Last one") && t.contains("## Todo") && t.contains("***\n\n## Archive"), "{t}");
    assert!(t.ends_with("%%\n"));
}

fn drag_to(h: &mut Harness<'static, Slot>, from: Pos2, to: Pos2) {
    h.event(Event::PointerMoved(from));
    h.step();
    press(h, from, true);
    for k in 1..=10 {
        h.event(Event::PointerMoved(from + (to - from) * (k as f32 / 10.0)));
        h.step();
    }
    shot(h, "polish2-kanban-lane-drag");
    press(h, to, false);
    settle(h);
}

#[test]
fn lanes_are_reordered_by_dragging_their_header() {
    let fx = Fx::new();
    let mut h = open(&fx, BOARD, ViewMode::Preview);
    let order = |t: &str| t.lines().filter(|l| l.starts_with("## ")).map(|l| l[3..].to_string()).collect::<Vec<_>>();
    assert_eq!(order(&text(&mut h)), ["Todo", "Doing", "Empty", "Archive"]);
    // Todo's title dragged right, past the middle of Doing and Empty: it lands before Archive's old place, after Empty.
    let from = center(&h, "Todo") + egui::vec2(60.0, 0.0);
    let to = center(&h, "Empty") + egui::vec2(120.0, 40.0);
    drag_to(&mut h, from, to);
    let t = text(&mut h);
    assert_eq!(order(&t), ["Doing", "Empty", "Todo", "Archive"], "{t}");
    assert!(t.contains("## Todo\n\n- [ ] Write **spec** for [[Ideas]]\n- [ ] Review `code` #urgent\n\n**Complete**\n\n- [x] Set up repo\n\n***"), "{t}");
    assert!(t.ends_with("%%\n"));
    // Back to the front.
    let from = center(&h, "Todo") + egui::vec2(60.0, 0.0);
    let to = center(&h, "Doing") - egui::vec2(120.0, 0.0);
    drag_to(&mut h, from, to);
    assert_eq!(order(&text(&mut h)), ["Todo", "Doing", "Empty", "Archive"]);
}

fn exact(h: &Harness<'static, Slot>, label: &str) -> Pos2 {
    h.get_by_label(label).rect().center()
}

fn wide_board(lanes: usize, cards: usize) -> String {
    let mut s = String::from("---\nkanban-plugin: basic\n---\n");
    for l in 0..lanes {
        s += &format!("\n## Lane {l}\n\n");
        for c in 0..cards {
            s += &format!("- [ ] card {l}-{c}\n");
        }
    }
    s
}

#[test]
fn dragging_near_an_edge_scrolls_the_board() {
    let fx = Fx::new();
    let mut h = open(&fx, &wide_board(6, 60), ViewMode::Preview);
    let screen = h.ctx.content_rect();
    // Sideways: a card held near the right edge brings the lanes beyond it into view.
    let x0 = center(&h, "Lane 0").x;
    let from = exact(&h, "card 0-1") + egui::vec2(60.0, 0.0);
    h.event(Event::PointerMoved(from));
    h.step();
    press(&mut h, from, true);
    let edge = egui::pos2(screen.right() - 10.0, from.y);
    h.event(Event::PointerMoved(edge));
    for _ in 0..40 {
        h.step();
    }
    let x1 = center(&h, "Lane 0").x;
    assert!(x1 < x0 - 100.0, "board scrolled left: {x0} -> {x1}");
    shot(&mut h, "polish2-kanban-autoscroll-x");
    // Back at the left edge it scrolls the other way.
    h.event(Event::PointerMoved(egui::pos2(screen.left() + 10.0, from.y)));
    for _ in 0..40 {
        h.step();
    }
    assert!(center(&h, "Lane 0").x > x1 + 100.0, "scrolled back");
    press(&mut h, egui::pos2(screen.left() + 10.0, from.y), false);
    settle(&mut h);
    // Down: a card held near the bottom of its lane scrolls the lane's list.
    let first = exact(&h, "card 0-3");
    let from = first + egui::vec2(60.0, 0.0);
    h.event(Event::PointerMoved(from));
    h.step();
    press(&mut h, from, true);
    let low = egui::pos2(from.x, screen.bottom() - 60.0);
    h.event(Event::PointerMoved(low));
    for _ in 0..60 {
        h.step();
    }
    assert!(h.query_by_label("card 0-3").is_none_or(|n| n.rect().center().y < first.y - 100.0), "list scrolled down");
    shot(&mut h, "polish2-kanban-autoscroll-y");
    h.event(Event::PointerGone);
    press(&mut h, low, false);
}

#[test]
fn a_lane_with_other_content_asks_before_it_is_deleted() {
    let fx = Fx::new();
    let t = "---\nkanban-plugin: basic\n---\n\n## Notes\n\nsome paragraph\n\n## Two\n\n- [ ] x\n";
    let mut h = open(&fx, t, ViewMode::Preview);
    menu_item(&mut h, 0, "Delete lane");
    assert_eq!(text(&mut h), t, "no cards, but a paragraph: still asks");
    assert!(h.query_by_label_contains("and its 0 cards").is_some());
}

#[test]
fn lane_delete_and_move_are_one_undo_step_each() {
    use egui::accesskit::Role;
    let fx = Fx::new();
    let mut h = open(&fx, BOARD, ViewMode::Split);
    let before = text(&mut h);
    let undo = |h: &mut Harness<'static, Slot>| {
        h.get_by_role(Role::MultilineTextInput).click();
        settle(h);
        h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
        settle(h);
    };
    // Move Todo after Doing by dragging its header.
    let from = center(&h, "Todo") + egui::vec2(60.0, 0.0);
    let to = center(&h, "Doing") + egui::vec2(100.0, 40.0);
    drag_to(&mut h, from, to);
    let moved = text(&mut h);
    assert_ne!(moved, before);
    for _ in 0..30 {
        h.step();
    }
    undo(&mut h);
    assert_eq!(text(&mut h), before, "one Ctrl+Z undoes the move");
    // Delete the (empty-looking is not; it has cards) first lane through the confirm row.
    menu_item(&mut h, 0, "Delete lane");
    let d = h.get_by_label("Delete").rect().center();
    click(&mut h, d);
    settle(&mut h);
    assert_ne!(text(&mut h), before);
    for _ in 0..30 {
        h.step();
    }
    undo(&mut h);
    assert_eq!(text(&mut h), before, "one Ctrl+Z undoes the delete");
}
