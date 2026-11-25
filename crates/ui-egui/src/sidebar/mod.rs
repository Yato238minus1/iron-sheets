//! Left panel: view buttons and the active view.
#[cfg(feature = "calendar")]
pub mod calendar;
pub mod files;
#[cfg(feature = "git")]
pub mod git;
pub mod links;
pub mod outline;
pub mod search;
pub mod symbols;
pub use iron_sheets_app_core::sidebar::{links_model, outline_model, search_model};

use crate::command::Command;
use crate::state::{AppState, SidebarView};

/// The pane never changes size on its own: the views are laid out in a child clipped to the panel's current content
/// rect, and exactly that rect is handed back to the panel. Content wider than the pane is cut off instead of widening it
/// (a panel grows to the min_rect of what it holds). Only the resize drag changes the width.
/// While the pane slides in or out, `shift` is the part still outside: the views keep their full width and are moved left.
pub fn show(ui: &mut egui::Ui, state: &mut AppState, shift: f32) {
    let rect = ui.max_rect();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(egui::Rect::from_min_max(rect.min - egui::vec2(shift, 0.0), rect.max)));
    child.shrink_clip_rect(rect);
    show_views(&mut child, state);
    ui.allocate_rect(rect, egui::Sense::hover());
}

type View = (&'static str, &'static str, SidebarView);

const ALL_VIEWS: &[View] = &[
    ("Files", "File tree", SidebarView::Files),
    ("Search", "Search the workspace", SidebarView::Search),
    ("Outline", "Headings of the open note", SidebarView::Outline),
    ("Links", "Links and backlinks of the open note", SidebarView::Links),
    ("Symbols", "Find a Typst symbol", SidebarView::Symbols),
    #[cfg(feature = "git")]
    ("Git", "Changes, commit, push and pull", SidebarView::Git),
    #[cfg(feature = "calendar")]
    ("Calendar", "Month grid and daily notes", SidebarView::Calendar),
];

/// The view tabs in the configured order: the Git tab only while Git is on and a workspace is open, Calendar only
/// while it is on, hidden views only while current. Rebuilt when the config, the current view or those two change.
fn views(ui: &egui::Ui, state: &AppState) -> std::sync::Arc<[View]> {
    let git = {
        #[cfg(feature = "git")]
        {
            iron_sheets_app_core::git::GitModule::panel_shown(state)
        }
        #[cfg(not(feature = "git"))]
        false
    };
    let cal = {
        #[cfg(feature = "calendar")]
        {
            state.modules.enabled(iron_sheets_app_core::modules::CALENDAR)
        }
        #[cfg(not(feature = "calendar"))]
        false
    };
    type Cached = ((u64, SidebarView, bool, bool), std::sync::Arc<[View]>);
    let (id, key) = (egui::Id::NULL.with("sidebar_views"), (state.config_gen, state.sidebar_view, git, cal));
    if let Some((k, v)) = ui.data(|d| d.get_temp::<Cached>(id)) {
        if k == key {
            return v;
        }
    }
    let v: std::sync::Arc<[View]> = iron_sheets_app_core::settings_model::sidebar_layout(&state.config.sidebar_views)
        .into_iter()
        .filter(|&(v, shown)| {
            (shown || v == state.sidebar_view)
                && match v {
                    #[cfg(feature = "git")]
                    SidebarView::Git => git,
                    #[cfg(feature = "calendar")]
                    SidebarView::Calendar => cal,
                    _ => true,
                }
        })
        .filter_map(|(v, _)| ALL_VIEWS.iter().find(|a| a.2 == v).copied())
        .collect();
    ui.data_mut(|d| d.insert_temp(id, (key, v.clone())));
    v
}
/// The header label of `v`.
pub fn label(v: SidebarView) -> &'static str {
    ALL_VIEWS.iter().find(|a| a.2 == v).map_or("", |a| a.0)
}

fn icon(v: SidebarView) -> crate::icons::Icon {
    use crate::icons::Icon;
    match v {
        SidebarView::Files => Icon::Folder,
        SidebarView::Search => Icon::Search,
        SidebarView::Outline => Icon::List,
        SidebarView::Links => Icon::Link,
        SidebarView::Symbols => Icon::Sigma,
        #[cfg(feature = "git")]
        SidebarView::Git => Icon::GitBranch,
        #[cfg(feature = "calendar")]
        SidebarView::Calendar => Icon::Calendar,
    }
}

const TAB_PAD_X: f32 = 4.0;
const TAB_GAP: f32 = 1.0;
const MORE: &str = "…";

/// Which view buttons fit into `avail`: bit `i` of the result is set when view `i` gets a button, the others go into
/// the "…" menu (`more` wide). `cur` always keeps its button; the rest keep their order and fill from the front.
/// Pure, like `statusbar::fit_optional`.
pub fn fit_views(avail: f32, widths: &[f32], gap: f32, more: f32, cur: usize) -> u32 {
    let all = widths.iter().sum::<f32>() + gap * widths.len().saturating_sub(1) as f32;
    if all <= avail {
        return (1u32 << widths.len()) - 1;
    }
    let mut mask = 1u32 << cur;
    let mut used = more + gap + widths[cur];
    for (i, &w) in widths.iter().enumerate().filter(|&(i, _)| i != cur) {
        if used + gap + w > avail {
            break;
        }
        used += gap + w;
        mask |= 1 << i;
    }
    mask
}

/// Width of each view's tab (text plus padding) and of the "…" button, measured when the font generation changes.
fn tab_widths(ui: &egui::Ui, font_gen: u64) -> ([f32; ALL_VIEWS.len()], f32) {
    let id = egui::Id::NULL.with("sidebar_tab_w");
    if let Some((g, w, m)) = ui.data(|d| d.get_temp::<(u64, [f32; ALL_VIEWS.len()], f32)>(id)) {
        if g == font_gen {
            return (w, m);
        }
    }
    let font = egui::TextStyle::Button.resolve(ui.style());
    let measure = |t: &str| ui.painter().layout_no_wrap(t.to_string(), font.clone(), egui::Color32::WHITE).size().x + 2.0 * TAB_PAD_X;
    let mut w = [0.0; ALL_VIEWS.len()];
    for (w, (l, ..)) in w.iter_mut().zip(ALL_VIEWS) {
        *w = measure(l);
    }
    let m = measure(MORE);
    ui.data_mut(|d| d.insert_temp(id, (font_gen, w, m)));
    (w, m)
}

fn show_views(ui: &mut egui::Ui, state: &mut AppState) {
    let mut chosen = None;
    let views = views(ui, state);
    let views = &views[..];
    let (all_w, more_w) = tab_widths(ui, state.ui.theme.generation);
    let mut widths = [0.0; ALL_VIEWS.len()];
    for (w, v) in widths.iter_mut().zip(views) {
        *w = all_w[ALL_VIEWS.iter().position(|a| a.2 == v.2).unwrap_or(0)];
    }
    let cur = views.iter().position(|v| v.2 == state.sidebar_view).unwrap_or(0);
    let (avail, gap) = (ui.available_width(), TAB_GAP);
    // Labels while they all fit; below that icon-only tabs (tooltip and accessible name carry the label), and only
    // when even the icons do not fit the rest goes into the "…" menu.
    let labels = fit_views(avail, &widths[..views.len()], gap, more_w, cur) == (1u32 << views.len()) - 1;
    let icon_w = crate::icons::tab_width(ui);
    let mask = if labels { (1u32 << views.len()) - 1 } else { fit_views(avail, &[icon_w; ALL_VIEWS.len()][..views.len()], gap, more_w, cur) };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(gap, 2.0);
        ui.spacing_mut().button_padding.x = TAB_PAD_X;
        let tip = |ui: &mut egui::Ui, tip: &str, v: SidebarView| {
            ui.label(crate::tabstrip::tip_with_key(&state.keymap, tip, &format!("view.{}", v.id())));
        };
        for &(label, hint, v) in views.iter().enumerate().filter(|&(i, _)| mask >> i & 1 == 1).map(|(_, v)| v) {
            let r = if labels { ui.selectable_label(state.sidebar_view == v, label) } else { crate::icons::tab(ui, icon(v), label, state.sidebar_view == v) };
            if r.on_hover_ui(|ui| tip(ui, if labels { hint } else { label }, v)).clicked() {
                chosen = Some(v);
            }
        }
        if mask.count_ones() < views.len() as u32 {
            crate::anim::menu_button(ui, egui::Id::NULL.with("sidebar_view_menu"), MORE, false, |ui| {
                for &(label, hint, v) in views.iter().enumerate().filter(|&(i, _)| mask >> i & 1 == 0).map(|(_, v)| v) {
                    if ui.selectable_label(state.sidebar_view == v, label).on_hover_ui(|ui| tip(ui, hint, v)).clicked() {
                        chosen = Some(v);
                        ui.close();
                    }
                }
            })
            .0
            .on_hover_text("More views");
        }
    });
    if let Some(v) = chosen {
        state.commands.push(Command::SetSidebarView(v));
    }
    ui.separator();
    if state.sidebar_view != SidebarView::Files {
        files::finish_close(state);
    }
    match state.sidebar_view {
        SidebarView::Files => files::show(ui, state),
        SidebarView::Search => {
            let mut p = std::mem::take(&mut state.ui.search);
            p.show(ui, state);
            state.ui.search = p;
        }
        SidebarView::Outline => {
            let mut p = std::mem::take(&mut state.ui.outline);
            p.show(ui, state);
            state.ui.outline = p;
        }
        SidebarView::Links => {
            let mut p = std::mem::take(&mut state.ui.links);
            p.show(ui, state);
            state.ui.links = p;
        }
        SidebarView::Symbols => {
            let mut p = std::mem::take(&mut state.ui.symbols);
            p.show(ui, state);
            state.ui.symbols = p;
        }
        #[cfg(feature = "git")]
        SidebarView::Git => {
            let mut p = std::mem::take(&mut state.ui.git);
            p.show(ui, state);
            state.ui.git = p;
        }
        #[cfg(feature = "calendar")]
        SidebarView::Calendar => calendar::show(ui, state),
    }
}

#[cfg(test)]
mod tests {
    use super::fit_views;

    #[test]
    fn everything_fits() {
        assert_eq!(fit_views(200.0, &[50.0, 50.0, 50.0], 1.0, 20.0, 0), 0b111);
    }

    #[test]
    fn overflow_keeps_the_front_and_the_current_view() {
        // 4 views of 50 (+1 gap): 203 needed, 120 available, "..." 20: Files + one more fit (20+50+1 + 1+50 = 122 > 120 -> only one).
        let w = [50.0; 4];
        assert_eq!(fit_views(120.0, &w, 1.0, 20.0, 0), 0b0001);
        assert_eq!(fit_views(130.0, &w, 1.0, 20.0, 0), 0b0011);
        // The current view is kept even when it is last, and takes the room from the front.
        assert_eq!(fit_views(130.0, &w, 1.0, 20.0, 3), 0b1001);
        assert_eq!(fit_views(40.0, &w, 1.0, 20.0, 2), 0b0100, "too narrow for anything else");
    }
}
