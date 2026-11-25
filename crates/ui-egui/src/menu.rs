//! Menu bar driven by the command table.
use crate::command::{Command, all_commands};
use crate::state::AppState;

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    let mut out: Vec<Command> = Vec::new();
    egui::MenuBar::new().ui(ui, |ui| {
        for menu in ["File", "Edit", "View", "Workspace", "Help"] {
            crate::anim::menu_button(ui, egui::Id::NULL.with(("menu_bar_menu", menu)), menu, false, |ui| {
                if menu == "Workspace" {
                    ui.menu_button("Switch workspace", |ui| {
                        let cur_ws = state.workspace.name.clone();
                        for n in state.workspace_list() {
                            let cur = *n == cur_ws;
                            if ui.selectable_label(cur, n).clicked() {
                                out.push(Command::SwitchWorkspace(n.clone()));
                                ui.close();
                            }
                        }
                    });
                    ui.menu_button("Remove folder", |ui| {
                        for i in 0..state.workspace.roots.len() {
                            if ui.button(state.workspace.root_label(i)).clicked() {
                                out.push(Command::RemoveRoot(i));
                                ui.close();
                            }
                        }
                        if state.workspace.roots.is_empty() {
                            ui.label("No folders");
                        }
                    });
                    ui.separator();
                }
                let mut item = |ui: &mut egui::Ui, ci: usize, info: &crate::command::CommandInfo| {
                    let cmd = (info.make)(&state.tabs);
                    let mut b = match checked(state, info.id) {
                        Some(on) => egui::Button::selectable(on, info.title),
                        None => egui::Button::new(info.title),
                    };
                    let hint = state.keymap.hint(ci);
                    if !hint.is_empty() {
                        b = b.shortcut_text(hint);
                    }
                    if ui.add_enabled(cmd.is_some(), b).clicked() {
                        out.extend(cmd);
                        ui.close();
                    }
                };
                let cmds = all_commands().iter().enumerate().filter(|(_, c)| c.menu == Some(menu));
                if menu == "View" {
                    // The long View list is grouped into submenus; what is left stays flat.
                    for group in VIEW_GROUPS {
                        ui.menu_button(*group, |ui| {
                            for (ci, info) in all_commands().iter().enumerate().filter(|(_, c)| c.menu == Some(menu) && view_group(c.id) == Some(*group)) {
                                item(ui, ci, info);
                            }
                        });
                    }
                    for (ci, info) in cmds.filter(|(_, c)| view_group(c.id).is_none()) {
                        item(ui, ci, info);
                    }
                } else {
                    for (ci, info) in cmds {
                        item(ui, ci, info);
                    }
                }
                if menu == "View" {
                    // The preview font without a trip to Settings.
                    ui.separator();
                    ui.menu_button("Preview font", |ui| {
                        let cur = if state.config.professional { "" } else { state.config.preview_font.as_str() };
                        for (id, label) in iron_sheets_app_core::settings_model::PREVIEW_FONTS {
                            if ui.selectable_label(cur == *id, *label).clicked() {
                                out.push(Command::SetPreviewFont(id.to_string()));
                                ui.close();
                            }
                        }
                    });
                }
            });
        }
        if state.config.custom_title_bar {
            crate::titlebar::show(ui, &mut out);
        }
    });
    state.commands.extend(out);
}

const VIEW_GROUPS: &[&str] = &["Sidebar", "Editor mode", "Theme", "Zoom"];

/// The View submenu a command belongs to, by id prefix (shortcut: a new group needs a prefix, upgrade: a field on `CommandInfo`).
fn view_group(id: &str) -> Option<&'static str> {
    match id {
        "view.sidebar" | "view.focus_sidebar" | "view.files" | "view.search" | "view.outline" | "view.links" | "view.symbols" | "view.git" | "view.calendar" => {
            Some("Sidebar")
        }
        "view.source" | "view.split" | "view.preview" | "view.cycle" | "view.sync" => Some("Editor mode"),
        _ if id.starts_with("view.theme_") => Some("Theme"),
        _ if id.starts_with("view.zoom_") => Some("Zoom"),
        _ => None,
    }
}

/// `Some(on)` for a menu command that shows a state (current sidebar view, editor mode, theme, toggles).
fn checked(state: &AppState, id: &str) -> Option<bool> {
    use crate::state::SidebarView::*;
    use iron_sheets_core::config::ThemePref;
    use iron_sheets_core::config::ViewMode;
    let mode = |m| state.tabs.active_tab().map(|t| t.view == m);
    let theme = |t| Some(state.config.theme == t);
    match id {
        "view.files" => Some(state.sidebar_view == Files),
        "view.search" => Some(state.sidebar_view == Search),
        "view.outline" => Some(state.sidebar_view == Outline),
        "view.links" => Some(state.sidebar_view == Links),
        "view.symbols" => Some(state.sidebar_view == Symbols),
        #[cfg(feature = "git")]
        "view.git" => Some(state.sidebar_view == Git),
        #[cfg(feature = "calendar")]
        "view.calendar" => Some(state.sidebar_view == Calendar),
        "view.source" => mode(ViewMode::Source),
        "view.split" => mode(ViewMode::Split),
        "view.preview" => mode(ViewMode::Preview),
        "view.sync" => Some(state.config.sync_scroll),
        "view.wrap" => Some(state.config.editor_wrap),
        "view.theme_system" => theme(ThemePref::System),
        "view.theme_light" => theme(ThemePref::Light),
        "view.theme_dark" => theme(ThemePref::Dark),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_view_menu_command_lands_in_a_group_or_stays_flat() {
        let mut grouped = std::collections::BTreeMap::<&str, usize>::new();
        for c in all_commands().iter().filter(|c| c.menu == Some("View")) {
            if let Some(g) = view_group(c.id) {
                *grouped.entry(g).or_default() += 1;
            }
        }
        for g in VIEW_GROUPS {
            assert!(grouped.get(g).copied().unwrap_or(0) >= 3, "{g} has entries: {grouped:?}");
        }
        assert_eq!(view_group("view.wrap"), None);
    }
}
