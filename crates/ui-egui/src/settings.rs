//! Settings window widgets: feature options (table driven, searchable) and the keyboard section. The model is in
//! `settings_model.rs`.
use crate::keymap::{Captured, FIXED_KEYS};
use crate::keys::Shortcut;
use crate::keys::format_shortcut;
pub use crate::settings_model::*;
use crate::state::AppState;
use crate::theme_model::{Rgba, contrast_ratio, format_hex};
use egui::{Color32, RichText};
use iron_sheets_core::config::{AppConfig, MathSyntax, ThemePref, ViewMode};

/// While recording: handle the key press instead of dispatching shortcuts. Returns true when recording is active.
///
/// With Settings in its own OS window the keys of that window are not in the root viewport's input: the capture runs
/// inside the Settings window's callback, and the root frame (where `collect_shortcuts` calls this) leaves its keys alone.
pub fn handle_recording(state: &mut AppState, ctx: &egui::Context) -> bool {
    let waiting = kb_search(ctx).waiting;
    if state.settings.recording.is_none() && !waiting {
        return false;
    }
    if crate::aux_window::is_native(ctx) && ctx.viewport_id() == egui::ViewportId::ROOT {
        return false;
    }
    if waiting {
        // "Find by shortcut": the press is the search text, not a command.
        match crate::keys_egui::capture(ctx) {
            Captured::Nothing => {}
            Captured::Cancelled => set_kb_search(ctx, KbSearch::default()),
            Captured::Key(sc) => set_kb_search(ctx, KbSearch { waiting: false, found: Some(sc) }),
        }
        return true;
    }
    handle_captured(state, crate::keys_egui::capture(ctx))
}

/// State of the Keyboard page's "find by shortcut" search; kept in the egui context because it lives only as long as
/// the window does.
#[derive(Clone, Copy, Default)]
struct KbSearch {
    waiting: bool,
    found: Option<Shortcut>,
}

fn kb_search_id() -> egui::Id {
    egui::Id::unique("settings_kb_search")
}

fn kb_search(ctx: &egui::Context) -> KbSearch {
    ctx.data(|d| d.get_temp(kb_search_id())).unwrap_or_default()
}

fn set_kb_search(ctx: &egui::Context, s: KbSearch) {
    ctx.data_mut(|d| d.insert_temp(kb_search_id(), s));
}

fn opened_key() -> egui::Id {
    egui::Id::unique("settings_opened")
}

const SPEC: crate::aux_window::AuxSpec<'static> =
    crate::aux_window::AuxSpec { id: "settings", title: "Settings", default_size: egui::vec2(760.0, 560.0), min_size: egui::vec2(520.0, 320.0) };

/// Keep the persisted open flag in step with the state; only explicit open/close changes it, never a quit.
fn sync_open(state: &mut AppState, open: bool) {
    let cur = state.config.settings_window;
    if cur.is_some_and(|g| g.open) != open {
        let mut g = cur.unwrap_or_default();
        g.open = open;
        state.config.settings_window = Some(g);
        state.touch_config();
    }
}

pub fn show(ctx: &egui::Context, state: &mut AppState) {
    if !state.settings.open {
        sync_open(state, false);
        crate::aux_window::reset(ctx, &SPEC);
        if ctx.data(|d| d.get_temp::<bool>(opened_key())).is_some() {
            ctx.data_mut(|d| d.remove::<bool>(opened_key()));
        }
        if kb_search(ctx).waiting || kb_search(ctx).found.is_some() {
            set_kb_search(ctx, KbSearch::default());
        }
        return;
    }
    sync_open(state, true);
    // The search field takes the keyboard when the window opens.
    let just_opened = ctx.data(|d| d.get_temp::<bool>(opened_key())).is_none();
    if just_opened {
        ctx.data_mut(|d| d.insert_temp(opened_key(), true));
    }
    let focus = std::mem::take(&mut state.settings.focus_request);
    let mut geom = state.config.settings_window.unwrap_or_default();
    let mut ui_state = std::mem::take(&mut state.settings);
    let mut changed = false;
    let shown = crate::aux_window::show(ctx, &SPEC, &mut geom, focus, |ui| {
        if !ui.ctx().embed_viewports() {
            // Own window: the keys typed here are this window's input.
            let mut tmp = std::mem::take(&mut ui_state);
            std::mem::swap(&mut state.settings, &mut tmp);
            handle_recording(state, ui.ctx());
            std::mem::swap(&mut state.settings, &mut tmp);
            ui_state = tmp;
        }
        content(ui, state, &mut ui_state, &mut changed, just_opened);
    });
    if shown.close {
        ui_state.open = false;
        ui_state.recording = None;
        // The root has to run once more without the window so that the OS window goes away.
        ctx.request_repaint();
    }
    state.settings = ui_state;
    if shown.geom_changed {
        state.config.settings_window = Some(geom);
        state.touch_config();
    }
    if changed {
        state.touch_config();
        apply(state, false);
    }
}

fn content(ui: &mut egui::Ui, state: &mut AppState, ui_state: &mut SettingsUi, changed: &mut bool, focus_search: bool) {
    // A blinking cursor repaints the root and this window twice a second for as long as a field has focus (an
    // immediate viewport paints both windows per frame): the cursor stays solid in here.
    ui.visuals_mut().text_cursor.blink = false;
    let clear_on_esc = !ui_state.search.is_empty() && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    if clear_on_esc {
        ui_state.search.clear();
    }
    if state.config.professional {
        ui.label(RichText::new("Professional mode is on: appearance settings apply when you turn it off").weak());
    }
    ui.horizontal(|ui| {
        let l = ui.label("Search settings");
        let r = ui.add(egui::TextEdit::singleline(&mut ui_state.search).hint_text("Search all settings").desired_width(260.0));
        let r = r.labelled_by(l.id);
        if focus_search || clear_on_esc {
            r.request_focus();
        }
        if !ui_state.search.is_empty() && ui.button("Clear").clicked() {
            ui_state.search.clear();
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Reset all").on_hover_text("Reset every option and shortcut to its default").clicked() {
                reset_all(state);
                ui_state.globs_text = None;
                *changed = true;
            }
        });
    });
    ui.separator();
    egui::Panel::left("settings_sections").resizable(false).exact_size(120.0).show(ui, |ui| {
        let sec_focus_id = egui::Id::NULL.with("settings_pending_sec_focus");
        let pending_focus: Option<usize> = ui.data_mut(|d| d.remove_temp(sec_focus_id));
        let mut any_focused = None;
        for (idx, s) in Section::ALL.iter().copied().enumerate() {
            let sel = ui_state.search.trim().is_empty() && ui_state.section == s;
            let resp = ui.selectable_label(sel, s.title());
            if pending_focus == Some(idx) {
                resp.request_focus();
            }
            if resp.has_focus() {
                any_focused = Some(idx);
                ui.memory_mut(|m| {
                    m.set_focus_lock_filter(resp.id, egui::EventFilter { vertical_arrows: true, ..Default::default() });
                });
            }
            if resp.clicked() {
                ui_state.section = s;
                ui_state.search.clear();
                resp.request_focus();
            }
        }
        if let Some(cur) = any_focused {
            let n = Section::ALL.len();
            let (down, up, home, end) = ui.input_mut(|i| {
                let mut k = |key| i.consume_key(egui::Modifiers::NONE, key);
                (k(egui::Key::ArrowDown), k(egui::Key::ArrowUp), k(egui::Key::Home), k(egui::Key::End))
            });
            let next = if down {
                Some((cur + 1).min(n - 1))
            } else if up {
                Some(cur.saturating_sub(1))
            } else if home {
                Some(0)
            } else if end {
                Some(n - 1)
            } else {
                None
            };
            if let Some(target) = next {
                ui_state.section = Section::ALL[target];
                ui_state.search.clear();
                ui.data_mut(|d| d.insert_temp(sec_focus_id, target));
            }
        }
    });
    egui::CentralPanel::default().show(ui, |ui| {
        let searching = !ui_state.search.trim().is_empty();
        // "Find by shortcut" listens only while its page is shown; elsewhere a key press must reach its command.
        if (searching || ui_state.section != Section::Keyboard) && kb_search(ui.ctx()).waiting {
            set_kb_search(ui.ctx(), KbSearch::default());
        }
        if !searching && ui_state.section == Section::Keyboard {
            keyboard_section(ui, state, ui_state);
            return;
        }
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            let mut shown = 0;
            let mut last: Option<Section> = None;
            let sel_section = ui_state.section;
            let visible = visible_items(ui_state, sel_section);
            for it in visible {
                if searching && last != Some(it.section) {
                    ui.add_space(6.0);
                    ui.heading(it.section.title());
                    last = Some(it.section);
                }
                shown += 1;
                *changed |= item_row(ui, state, ui_state, it);
            }
            if searching {
                if shown == 0 {
                    ui.label("No matching option");
                }
                let kb = matching_commands(&ui_state.search);
                if kb > 0 && ui.link(format!("{kb} matching shortcut(s) in Keyboard")).clicked() {
                    ui_state.kb_filter = ui_state.search.trim().to_string();
                    ui_state.section = Section::Keyboard;
                    ui_state.search.clear();
                }
            }
        });
    });
}

/// Label of the option that rescales the whole interface.
const ZOOM_LABEL: &str = "UI zoom";

/// The UI zoom drag value.
///
/// Why it used to misbehave: a `DragValue` turns pointer movement (in points) into a value change. The zoom was applied
/// to the context on every frame of the drag, which changes points-per-pixel: the pointer position in points and
/// the widget under it rescaled while the drag was running, so each frame's delta contained the effect of the previous
/// frame's zoom change (feedback), and the settings window grew and shrank under the pointer. The value is now kept in
/// a local buffer while the drag lasts (the number shown still follows the pointer) and is applied once on release.
/// Typed values and Enter apply immediately.
fn zoom_value(ui: &mut egui::Ui, value: &mut f32, range: std::ops::RangeInclusive<f32>, label: egui::Id) -> bool {
    let buf_id = ui.scope_id().with("ui_zoom_drag");
    let held: Option<f32> = ui.data(|d| d.get_temp(buf_id));
    let mut v = held.unwrap_or(*value);
    let r = ui.add(egui::DragValue::new(&mut v).range(range).speed(0.005).fixed_decimals(2)).labelled_by(label);
    let mut changed = false;
    if r.dragged() {
        // Still dragging: remember, do not touch the config yet.
        ui.data_mut(|d| d.insert_temp(buf_id, v));
    } else {
        if held.is_some() {
            ui.data_mut(|d| d.remove::<f32>(buf_id));
        }
        if held.is_some() || r.changed() {
            let v = (v * 100.0).round() / 100.0;
            if (v - *value).abs() > f32::EPSILON {
                *value = v;
                changed = true;
            }
        }
    }
    changed
}

/// Colour override of one key for both theme variants: a swatch with a picker and a reset button each.
///
/// Dragging inside the picker only changes a pending value; the setting is applied once the pointer is released, because
/// every applied colour change rebuilds the theme and invalidates the cached text layouts.
fn color_row(ui: &mut egui::Ui, cfg: &mut AppConfig, key: &'static str) -> bool {
    let mut changed = false;
    let hc = cfg.high_contrast;
    for dark in [true, false] {
        let name = if dark { "Dark" } else { "Light" };
        let model = crate::theme::Theme::model_for(cfg, dark, hc);
        let Some(shown) = model.color_of(key) else { continue };
        let pend_id = ui.scope_id().with(("color_pending", key, dark));
        let pending: Option<Color32> = ui.data(|d| d.get_temp(pend_id));
        let mut c = pending.unwrap_or_else(|| crate::theme::color32(shown));
        ui.label(name);
        let r = egui::color_picker::color_edit_button_srgba(ui, &mut c, egui::color_picker::Alpha::Opaque);
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{name} colour")));
        if r.changed() {
            ui.data_mut(|d| d.insert_temp(pend_id, c));
        } else if pending.is_some() && !ui.input(|i| i.pointer.any_down()) {
            ui.data_mut(|d| d.remove::<Color32>(pend_id));
            let hex = format_hex(Rgba::rgb(c.r(), c.g(), c.b()));
            let map = if dark { &mut cfg.colors_dark } else { &mut cfg.colors_light };
            if map.get(key) != Some(&hex) {
                map.insert(key.to_string(), hex);
                changed = true;
            }
        }
        let map = if dark { &mut cfg.colors_dark } else { &mut cfg.colors_light };
        if map.contains_key(key) && ui.small_button("Reset").on_hover_text(format!("Back to the {name} theme's own colour")).clicked() {
            map.remove(key);
            changed = true;
        }
        // Text-like colours on the editor background: warn when they would be hard to read.
        if !matches!(key, "accent" | "editor_bg") {
            let m = crate::theme::Theme::model_for(cfg, dark, hc);
            let ratio = contrast_ratio(m.color_of(key).unwrap_or(Rgba::BLACK), m.ui.edit_bg);
            if ratio < 3.0 {
                ui.colored_label(ui.visuals().warn_fg_color, format!("low contrast ({ratio:.1}:1)"));
            }
        }
        ui.add_space(8.0);
    }
    changed
}

/// Chevron of a choice box: points right when closed and turns down while its list is open.
fn combo_icon(salt: &'static str) -> impl FnOnce(&egui::Ui, egui::Rect, &egui::style::WidgetVisuals, bool) + 'static {
    move |ui, rect, visuals, open| {
        let t = crate::anim::bool_t(ui, egui::Id::NULL.with(("combo_chevron", salt)), open, crate::anim::Group::Menus);
        crate::anim::arrow(ui.painter(), rect.center(), t, visuals.fg_stroke.color);
    }
}

fn item_row(ui: &mut egui::Ui, state: &mut AppState, ui_state: &mut SettingsUi, it: &Item) -> bool {
    let mut changed = false;
    let cfg = &mut state.config;
    ui.horizontal(|ui| {
        let label = if it.restart { format!("{} (applies after restart)", it.label) } else { it.label.to_string() };
        let lab = match &it.kind {
            Kind::Bool(f) => {
                let r = ui.checkbox(f(cfg), label).on_hover_text(it.help);
                changed |= r.changed();
                None
            }
            _ => Some(ui.label(label)),
        };
        let Some(lab) = lab else { return };
        let lab = lab.on_hover_text(it.help);
        match &it.kind {
            Kind::Bool(_) => {}
            Kind::F32(f, lo, hi) if it.label == ZOOM_LABEL => {
                changed |= zoom_value(ui, f(cfg), *lo..=*hi, lab.id);
            }
            Kind::F32(f, lo, hi) => {
                let r = ui.add(egui::DragValue::new(f(cfg)).range(*lo..=*hi).speed(((*hi - *lo) / 200.0).max(0.005)).max_decimals(2)).labelled_by(lab.id);
                changed |= r.changed();
            }
            Kind::U32(f, lo, hi) => {
                let speed = ((*hi - *lo) as f64 / 200.0).max(1.0);
                let r = ui.add(egui::DragValue::new(f(cfg)).range(*lo..=*hi).speed(speed)).labelled_by(lab.id);
                changed |= r.changed();
            }
            Kind::Theme => {
                // The theme folder is read when the choice is opened, not at startup.
                let cur = if cfg.theme_file.is_empty() { format!("{:?}", cfg.theme) } else { ui_state.theme.as_ref().map_or_else(|| cfg.theme_file.clone(), |t| t.name.clone()) };
                let r = egui::ComboBox::from_id_salt("opt_theme").icon(combo_icon("opt_theme")).selected_text(cur).show_ui(ui, |ui| {
                    let list = ui_state.theme_list.get_or_insert_with(|| iron_sheets_core::themes::list(&state.dirs));
                    for t in [ThemePref::System, ThemePref::Light, ThemePref::Dark] {
                        if ui.selectable_label(cfg.theme_file.is_empty() && cfg.theme == t, format!("{t:?}")).clicked() {
                            (cfg.theme, cfg.theme_file) = (t, String::new());
                            changed = true;
                        }
                    }
                    for (id, name) in list.iter() {
                        if ui.selectable_label(cfg.theme_file == *id, name).clicked() {
                            cfg.theme_file = id.clone();
                            changed = true;
                        }
                    }
                });
                if r.inner.is_none() {
                    ui_state.theme_list = None;
                }
                r.response.labelled_by(lab.id);
                if ui.small_button("Folder").on_hover_text("Open themes folder").clicked() {
                    state.commands.push(iron_sheets_app_core::command::Command::OpenThemesFolder);
                }
            }
            Kind::View => {
                let cur = cfg.default_view;
                egui::ComboBox::from_id_salt("opt_view")
                    .icon(combo_icon("opt_view"))
                    .selected_text(format!("{cur:?}"))
                    .show_ui(ui, |ui| {
                        for v in [ViewMode::Source, ViewMode::Split, ViewMode::Preview] {
                            changed |= ui.selectable_value(&mut cfg.default_view, v, format!("{v:?}")).changed();
                        }
                    })
                    .response
                    .labelled_by(lab.id);
            }
            Kind::MathSyntax => {
                egui::ComboBox::from_id_salt("opt_mathsyntax")
                    .icon(combo_icon("opt_mathsyntax"))
                    .selected_text(format!("{:?}", cfg.math_syntax))
                    .show_ui(ui, |ui| {
                        for v in [MathSyntax::Auto, MathSyntax::Typst, MathSyntax::Latex] {
                            changed |= ui.selectable_value(&mut cfg.math_syntax, v, format!("{v:?}")).changed();
                        }
                    })
                    .response
                    .labelled_by(lab.id);
            }
            Kind::SymbolMode => {
                let cur = SYMBOL_MODES[symbol_mode_from(&cfg.symbol_insert_mode)].1;
                egui::ComboBox::from_id_salt("opt_symmode")
                    .icon(combo_icon("opt_symmode"))
                    .selected_text(cur)
                    .show_ui(ui, |ui| {
                        for (v, name) in SYMBOL_MODES {
                            if ui.selectable_label(cfg.symbol_insert_mode == v, name).clicked() {
                                cfg.symbol_insert_mode = v.to_string();
                                changed = true;
                            }
                        }
                    })
                    .response
                    .labelled_by(lab.id);
            }
            Kind::Choice(f, list) => {
                let cur = f(cfg).clone();
                // Lilex is offered only when it is built in (feature `font-lilex`); a config that names it falls back to the first entry.
                let list: Vec<_> = list.iter().filter(|(id, _)| cfg!(feature = "font-lilex") || *id != "lilex").collect();
                let text = list.iter().find(|(id, _)| *id == cur).map_or(list[0].1, |(_, l)| *l);
                egui::ComboBox::from_id_salt(it.label)
                    .icon(combo_icon(it.label))
                    .selected_text(text)
                    .show_ui(ui, |ui| {
                        for (id, label) in list.iter().copied() {
                            if ui.selectable_label(cur == *id, *label).clicked() && cur != *id {
                                *f(cfg) = id.to_string();
                                changed = true;
                            }
                        }
                    })
                    .response
                    .labelled_by(lab.id);
            }
            Kind::Text(f) => {
                let r = ui.add(egui::TextEdit::singleline(f(cfg)).hint_text("none").desired_width(260.0)).labelled_by(lab.id);
                // Reading a font file is the costly part: apply when editing ends, not per keystroke.
                if r.lost_focus() {
                    changed = true;
                }
            }
            Kind::Color(key) => {
                changed |= color_row(ui, cfg, key);
            }
            Kind::SidebarViews => {
                ui.vertical(|ui| {
                    let layout = iron_sheets_app_core::settings_model::sidebar_layout(&cfg.sidebar_views);
                    for (i, &(v, shown)) in layout.iter().enumerate() {
                        let name = crate::sidebar::label(v);
                        ui.horizontal(|ui| {
                            let mut on = shown;
                            if ui.add_enabled(v != crate::state::SidebarView::Files, egui::Checkbox::new(&mut on, name)).changed() {
                                iron_sheets_app_core::settings_model::set_sidebar_view_shown(&mut cfg.sidebar_views, v, on);
                                changed = true;
                            }
                            for (delta, icon, dir, ok) in [(-1, crate::icons::Icon::Up, "up", i > 0), (1, crate::icons::Icon::Down, "down", i + 1 < layout.len())] {
                                let r = ui.add_enabled_ui(ok, |ui| crate::icons::button(ui, icon, &format!("Move {name} {dir}"))).inner;
                                if r.clicked() {
                                    iron_sheets_app_core::settings_model::move_sidebar_view(&mut cfg.sidebar_views, v, delta);
                                    changed = true;
                                }
                            }
                        });
                    }
                });
            }
            Kind::Globs => {
                let text = ui_state.globs_text.get_or_insert_with(|| cfg.exclude_globs.join("\n"));
                let r = ui.add(egui::TextEdit::multiline(text).desired_rows(3).desired_width(260.0)).labelled_by(lab.id);
                if r.changed() {
                    cfg.exclude_globs = text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
                }
                // Re-indexing is expensive: apply when editing ends.
                if r.lost_focus() {
                    changed = true;
                }
            }
        }
    });
    if !it.help.is_empty() {
        ui.label(RichText::new(it.help).small().weak());
    }
    ui.add_space(4.0);
    changed
}

/// Widths of the Keyboard table's columns: command | shortcuts | record + reset. Rows and header use the same ones.
const COL_CMD: f32 = 220.0;
const COL_KEYS: f32 = 250.0;
const COL_REC: f32 = 170.0;

/// One table cell of exactly `w` x `h` (a child `Ui` only advances by what it used, so the minimum size is set).
fn cell(ui: &mut egui::Ui, w: f32, h: f32, add: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(egui::vec2(w, h), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_size(egui::vec2(w, h));
        add(ui);
    });
}

fn keyboard_section(ui: &mut egui::Ui, state: &mut AppState, ui_state: &mut SettingsUi) {
    let mut search = kb_search(ui.ctx());
    ui.horizontal(|ui| {
        let l = ui.label("Filter commands");
        ui.add(egui::TextEdit::singleline(&mut ui_state.kb_filter).desired_width(220.0)).labelled_by(l.id);
        let text = if search.waiting { "Press a shortcut (Esc cancels)" } else { "Find by shortcut" };
        if ui.selectable_label(search.waiting, text).on_hover_text("Press a key combination to list the commands bound to it").clicked() {
            search = KbSearch { waiting: !search.waiting, found: None };
        }
        if ui.button("Reset all shortcuts").clicked() {
            reset_all_bindings(state);
            ui_state.note = "All shortcuts reset to their defaults".into();
        }
    });
    if let Some(sc) = search.found {
        ui.horizontal(|ui| {
            ui.label(format!("Commands bound to {}", format_shortcut(&sc)));
            if ui.small_button("Clear").clicked() {
                search.found = None;
            }
        });
    }
    set_kb_search(ui.ctx(), search);
    if !state.keymap.conflicts.is_empty() {
        let km = &state.keymap;
        for (sc, ids) in &km.conflicts {
            let names: Vec<&str> = ids.iter().map(|&i| km.entries[i].title).collect();
            ui.colored_label(Color32::from_rgb(220, 140, 30), format!("Conflict: {sc} is used by {}", names.join(", ")));
        }
    }
    if !ui_state.note.is_empty() {
        ui.label(RichText::new(&ui_state.note).small());
    }
    for (k, what) in FIXED_KEYS {
        ui.label(RichText::new(format!("Fixed: {k} - {what}")).small().weak());
    }
    ui.separator();
    let rows = match search.found {
        Some(sc) => state.keymap.others_with(&sc, usize::MAX),
        None => keyboard_rows(&state.keymap, &ui_state.kb_filter),
    };
    let row_h = ui.spacing().interact_size.y + 6.0;
    ui.horizontal(|ui| {
        for (w, t) in [(COL_CMD, "Command"), (COL_KEYS, "Shortcut"), (COL_REC, "")] {
            cell(ui, w, ui.spacing().interact_size.y, |ui| {
                ui.label(RichText::new(t).strong());
            });
        }
    });
    if rows.is_empty() {
        ui.label("No matching command");
    }
    let mut remove: Option<(usize, usize)> = None;
    let mut reset: Option<usize> = None;
    let mut record: Option<usize> = None;
    egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, row_h, rows.len(), |ui, range| {
        for &i in &rows[range] {
            let e = &state.keymap.entries[i];
            ui.horizontal(|ui| {
                ui.set_min_height(row_h);
                cell(ui, COL_CMD, row_h, |ui| {
                    ui.add(egui::Label::new(e.title).truncate());
                });
                let conflict = state.keymap.in_conflict(i);
                cell(ui, COL_KEYS, row_h, |ui| {
                    for (bi, b) in state.keymap.bindings[i].iter().enumerate() {
                        let t = format_shortcut(b);
                        let txt = if conflict { RichText::new(&t).color(Color32::from_rgb(220, 140, 30)) } else { RichText::new(&t) };
                        ui.label(txt);
                        if crate::icons::button(ui, crate::icons::Icon::Close, &format!("Remove {t} from {}", e.title)).clicked() {
                            remove = Some((i, bi));
                        }
                    }
                    if state.keymap.bindings[i].is_empty() {
                        ui.label(RichText::new("unbound").weak());
                    }
                });
                cell(ui, COL_REC, row_h, |ui| {
                    let recording = ui_state.recording == Some(i);
                    let rb = ui.selectable_label(recording, if recording { "Press keys (Esc cancels)" } else { "Record" });
                    if rb.on_hover_text(format!("Record a shortcut for {}", e.title)).clicked() {
                        record = Some(i);
                    }
                    if !state.keymap.is_default(i) && ui.small_button("Reset").on_hover_text(format!("Reset {} to its default", e.title)).clicked() {
                        reset = Some(i);
                    }
                });
            });
        }
    });
    if let Some(i) = record {
        toggle_recording(ui_state, i);
    }
    if let Some((i, bi)) = remove {
        remove_binding(state, i, bi);
    }
    if let Some(i) = reset {
        reset_binding(state, i);
    }
}
