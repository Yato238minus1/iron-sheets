//! Dialog widgets and native pickers (rfd, on threads). The dialog data types are in `iron_sheets_app_core::dialogs`.
use iron_sheets_app_core::command::Command;
pub use iron_sheets_app_core::dialogs::*;
use iron_sheets_app_core::platform::Waker;
use std::path::PathBuf;
use std::sync::Arc;

/// Native folder picker on a background thread.
pub fn pick_folder(d: &mut Dialogs, waker: &Arc<dyn Waker>) {
    if d.picking {
        return;
    }
    d.picking = true;
    let (tx, ctx) = (d.tx.clone(), waker.clone());
    let started = std::time::Instant::now();
    let spawned = std::thread::Builder::new().name("iron-sheets-pick".into()).spawn(move || {
        let r = rfd::FileDialog::new().set_title("Add folder to workspace").pick_folder();
        let msg = match r {
            Some(p) => Pick::Folder(p),
            None if started.elapsed().as_millis() < 150 => Pick::Unavailable,
            None => Pick::Cancelled,
        };
        let _ = tx.send(msg);
        ctx.wake();
    });
    if spawned.is_err() {
        d.picking = false;
    }
}

pub fn classify_pick_files(res: Option<Vec<PathBuf>>, elapsed: std::time::Duration) -> Pick {
    match res {
        Some(v) => Pick::Files(v),
        None if elapsed.as_millis() < 150 => Pick::FilesUnavailable,
        None => Pick::Cancelled,
    }
}

pub fn pick_files(d: &mut Dialogs, waker: &Arc<dyn Waker>) {
    if d.picking {
        return;
    }
    d.picking = true;
    let (tx, ctx) = (d.tx.clone(), waker.clone());
    let started = std::time::Instant::now();
    let spawned = std::thread::Builder::new().name("iron-sheets-pick".into()).spawn(move || {
        let r = rfd::FileDialog::new().set_title("Open file").pick_files();
        let msg = classify_pick_files(r, started.elapsed());
        let _ = tx.send(msg);
        ctx.wake();
    });
    if spawned.is_err() {
        d.picking = false;
        let _ = d.tx.send(Pick::FilesUnavailable);
        waker.wake();
    }
}

/// Native "save as" for the untitled note `from` on a background thread; the answer arrives in `Dialogs::save_rx`.
pub fn pick_save(d: &mut Dialogs, waker: &Arc<dyn Waker>, from: PathBuf, after: AfterSave, name: &str) {
    if d.picking {
        return;
    }
    d.picking = true;
    let (tx, ctx) = (d.save_tx.clone(), waker.clone());
    let name = if name.contains('.') { name.to_string() } else { format!("{name}.md") };
    let started = std::time::Instant::now();
    let spawned = std::thread::Builder::new().name("iron-sheets-pick".into()).spawn(move || {
        let r = rfd::FileDialog::new().set_title("Save note as").set_file_name(name).add_filter("Notes", &["md", "markdown", "typ"]).save_file();
        let msg = match r {
            Some(mut to) => {
                if to.extension().is_none() {
                    to.set_extension("md");
                }
                SavePick::To { from, to, after }
            }
            None if started.elapsed().as_millis() < 150 => SavePick::Unavailable { from, after },
            None => SavePick::Cancelled,
        };
        let _ = tx.send(msg);
        ctx.wake();
    });
    if spawned.is_err() {
        d.picking = false;
    }
}

/// Turn finished native "save as" dialogs into commands.
pub fn poll_save(d: &mut Dialogs, out: &mut Vec<Command>) {
    while let Ok(p) = d.save_rx.try_recv() {
        d.picking = false;
        match p {
            SavePick::To { from, to, after } => out.push(Command::SaveAsTo { from, to, after }),
            SavePick::Unavailable { from, after } => {
                // Use the in-app prompt from now on, for this note too.
                d.native_ok = false;
                out.push(Command::SaveAs { path: from, after });
            }
            SavePick::Cancelled => {}
        }
    }
}

fn folder_browser_go(
    path_text: &str,
    dir: &mut PathBuf,
    error: &mut Option<String>,
    listing: &Option<(PathBuf, Vec<(String, PathBuf)>)>,
) -> bool {
    let p = PathBuf::from(path_text);
    if p.is_dir() {
        let matches_cached = listing.as_ref().is_some_and(|(d_cur, _)| d_cur == &p);
        *dir = p;
        *error = None;
        matches_cached
    } else {
        *error = Some("Not a folder".into());
        false
    }
}

/// Show the current dialog; returns commands to run (and the prompt result via `PromptSubmit`).
pub fn show(ctx: &egui::Context, d: &mut Dialogs, out: &mut Vec<Command>) {
    poll_save(d, out);
    let polled_dir = d.poll_dir_listing();
    let dlg_seen_id = egui::Id::NULL.with("iron_sheets_dlg_seen");
    let fb_sel_id = egui::Id::NULL.with("iron_sheets_fb_sel");
    let fb_scroll_y_id = egui::Id::NULL.with("iron_sheets_fb_scroll_y");
    let fb_view_h_id = egui::Id::NULL.with("iron_sheets_fb_view_h");
    let fb_scroll_to_id = egui::Id::NULL.with("iron_sheets_fb_scroll_to");
    let fb_painted_id = egui::Id::NULL.with("fb_painted_rows");
    let Some(mut dlg) = d.current.take() else {
        ctx.data_mut(|data| {
            data.remove::<u8>(dlg_seen_id);
            data.remove::<usize>(fb_sel_id);
        });
        return;
    };
    if let (Dialog::FolderBrowser { dir, error, listing, .. }, Some((loaded_dir, res))) = (&mut dlg, polled_dir) {
        if loaded_dir == *dir {
            match res {
                Ok(subdirs) => *listing = Some((loaded_dir, subdirs)),
                Err(e) => {
                    *error = Some(e);
                    *listing = Some((loaded_dir, Vec::new()));
                }
            }
        }
    }
    let kind_tag: u8 = match &dlg {
        Dialog::Prompt { .. } => 1,
        Dialog::Confirm { .. } => 2,
        Dialog::Unsaved { .. } => 3,
        Dialog::FolderBrowser { .. } => 4,
    };
    let just_opened = ctx.data(|data| data.get_temp::<u8>(dlg_seen_id)) != Some(kind_tag);
    if just_opened {
        ctx.data_mut(|data| {
            data.insert_temp(dlg_seen_id, kind_tag);
            data.remove::<usize>(fb_sel_id);
        });
    }
    let mut dir_to_load: Option<PathBuf> = None;
    let mut cancel_load = false;
    let mut close = false;
    let title = match &dlg {
        Dialog::Prompt { title, .. } | Dialog::Confirm { title, .. } => title.clone(),
        Dialog::Unsaved { .. } => "Unsaved changes".into(),
        Dialog::FolderBrowser { .. } => "Select folder".into(),
    };
    // Always fully inside the app window, also at tiny sizes.
    let room = ctx.content_rect().shrink(6.0);
    let field_w = (room.width() - 60.0).clamp(80.0, 360.0);
    let list_h = (room.height() - 170.0).clamp(60.0, 260.0);
    let modal_id = egui::Id::NULL.with("iron_sheets_dialog");
    let area = egui::Modal::default_area(modal_id).anchor(egui::Align2::CENTER_CENTER, [0.0, -60.0_f32.min((room.height() / 6.0).max(0.0))]);
    let modal_resp = egui::Modal::new(modal_id).area(area).show(ctx, |ui| {
        ui.set_max_size(egui::vec2((field_w + 80.0).min(room.width()), room.height()));
        ui.heading(&title);
        ui.separator();
        match &mut dlg {
            Dialog::Prompt { label, value, action, error, focus, dirs, dir_sel, .. } => {
                if !dirs.is_empty() {
                    ui.horizontal(|ui| {
                        let folder_lab = ui.label("Folder");
                        egui::ComboBox::from_id_salt("prompt_folder")
                            .width(field_w.min(260.0))
                            .selected_text(dirs.get(*dir_sel).map_or("", |d| d.0.as_str()))
                            .show_ui(ui, |ui| {
                                for (k, (name, path)) in dirs.iter().enumerate() {
                                    ui.selectable_value(dir_sel, k, name.as_str()).on_hover_text(path.display().to_string());
                                }
                            })
                            .response
                            .labelled_by(folder_lab.id);
                    });
                }
                ui.label(label.as_str());
                let r = ui.add(egui::TextEdit::singleline(value).desired_width(field_w.min(320.0)));
                crate::theme::name_field(&r, label.as_str());
                if *focus {
                    r.request_focus();
                    *focus = false;
                }
                if let Some(e) = error {
                    ui.colored_label(ui.visuals().error_fg_color, e.as_str());
                }
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() || enter {
                        if value.trim().is_empty() {
                            *error = Some("Name must not be empty".into());
                        } else {
                            if let (PromptAction::SaveUntitled { dir, .. }, Some((_, chosen))) = (&mut *action, dirs.get(*dir_sel)) {
                                *dir = chosen.clone();
                            }
                            out.push(Command::PromptSubmit(action.clone(), value.trim().to_string()));
                            close = true;
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            }
            Dialog::Confirm { message, ok_label, on_ok, alt, dont_ask, focus, .. } => {
                ui.label(message.as_str());
                if let Some((checked, _)) = dont_ask {
                    ui.checkbox(checked, "Do not show again");
                }
                ui.horizontal(|ui| {
                    let ok_r = ui.button(ok_label.as_str());
                    if *focus {
                        ok_r.request_focus();
                        *focus = false;
                    }
                    if ok_r.clicked() {
                        out.append(on_ok);
                        if let Some((true, cmds)) = dont_ask {
                            out.append(cmds);
                        }
                        close = true;
                    }
                    if let Some((label, cmds)) = alt {
                        if ui.button(label.as_str()).clicked() {
                            out.append(cmds);
                            close = true;
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            }
            Dialog::Unsaved { message, save, discard } => {
                ui.label(message.as_str());
                ui.horizontal(|ui| {
                    let save_r = ui.button("Save");
                    if just_opened {
                        save_r.request_focus();
                    }
                    if save_r.clicked() {
                        out.append(save);
                        close = true;
                    }
                    if ui.button("Discard").clicked() {
                        out.append(discard);
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            }
            Dialog::FolderBrowser { path_text, dir, error, listing } => {
                let path_id = egui::Id::NULL.with("fb_path_input");
                let fb_list_id = egui::Id::NULL.with("fb_list_keys");
                let is_loading = listing.as_ref().is_none_or(|(d_cur, _)| d_cur != dir);
                if is_loading {
                    dir_to_load = Some(dir.clone());
                }
                let subdirs: &[(String, PathBuf)] = if is_loading { &[] } else { listing.as_ref().map_or(&[], |(_, v)| v.as_slice()) };
                let parent_opt = dir.parent().map(std::path::Path::to_path_buf);
                let has_parent = parent_opt.is_some();
                let n = if is_loading { 0 } else { usize::from(has_parent) + subdirs.len() };
                let row_h = (ui.text_style_height(&egui::TextStyle::Body) + 4.0).max(20.0);
                let mut fb_sel: Option<usize> = ctx.data(|data| data.get_temp(fb_sel_id));
                let mut fb_scroll_to: Option<f32> = ctx.data_mut(|data| data.remove_temp::<f32>(fb_scroll_to_id));
                let fb_scroll_y: f32 = ctx.data(|data| data.get_temp(fb_scroll_y_id)).unwrap_or(0.0);
                let fb_view_h: f32 = ctx.data(|data| data.get_temp(fb_view_h_id)).unwrap_or(list_h);
                if n == 0 {
                    fb_sel = None;
                } else if let Some(s) = fb_sel.as_mut() {
                    *s = (*s).min(n - 1);
                }

                let mut move_focus_to_list = false;
                if ui.memory(|m| m.has_focus(path_id)) && !is_loading && n > 0 {
                    ui.memory_mut(|m| m.set_focus_lock_filter(path_id, egui::EventFilter { vertical_arrows: true, ..Default::default() }));
                    let (down, up) = ui.input_mut(|i| {
                        (i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown), i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp))
                    });
                    if down {
                        fb_sel = Some(0);
                        fb_scroll_to = Some(0.0);
                        move_focus_to_list = true;
                    } else if up {
                        fb_sel = Some(n - 1);
                        let top = (n - 1) as f32 * row_h;
                        fb_scroll_to = Some((top + row_h - fb_view_h).max(0.0));
                        move_focus_to_list = true;
                    }
                }

                ui.horizontal(|ui| {
                    let r = ui.add(egui::TextEdit::singleline(path_text).id(path_id).desired_width((field_w - 40.0).max(80.0)));
                    crate::theme::name_field(&r, "Folder path");
                    if just_opened {
                        r.request_focus();
                    }
                    if ((r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) || ui.button("Go").clicked())
                        && folder_browser_go(path_text.as_str(), dir, error, listing)
                    {
                        dir_to_load = None;
                        cancel_load = true;
                    }
                });
                if move_focus_to_list {
                    ui.memory_mut(|m| m.request_focus(fb_list_id));
                }
                if let Some(e) = error {
                    ui.colored_label(ui.visuals().error_fg_color, e.as_str());
                }

                let mut go: Option<PathBuf> = None;
                let list_resp = ui.interact(
                    egui::Rect::from_min_size(ui.cursor().min, egui::vec2(field_w, list_h)),
                    fb_list_id,
                    egui::Sense::focusable_noninteractive(),
                );
                let list_focused = list_resp.has_focus();
                if list_focused && !is_loading && n > 0 {
                    ui.memory_mut(|m| {
                        m.set_focus_lock_filter(fb_list_id, egui::EventFilter { vertical_arrows: true, ..Default::default() });
                    });
                    let page = ((fb_view_h / row_h).floor() as usize).max(1);
                    let (down, up, home, end, pgdn, pgup, enter) = ui.input_mut(|i| {
                        let mut k = |key| i.consume_key(egui::Modifiers::NONE, key);
                        (
                            k(egui::Key::ArrowDown),
                            k(egui::Key::ArrowUp),
                            k(egui::Key::Home),
                            k(egui::Key::End),
                            k(egui::Key::PageDown),
                            k(egui::Key::PageUp),
                            k(egui::Key::Enter),
                        )
                    });
                    let mut moved = false;
                    let cur = fb_sel.unwrap_or(0).min(n - 1);
                    if down {
                        fb_sel = Some((cur + 1).min(n - 1));
                        moved = true;
                    } else if up {
                        if cur == 0 {
                            ui.memory_mut(|m| m.request_focus(path_id));
                        } else {
                            fb_sel = Some(cur - 1);
                            moved = true;
                        }
                    } else if home {
                        fb_sel = Some(0);
                        moved = true;
                    } else if end {
                        fb_sel = Some(n - 1);
                        moved = true;
                    } else if pgdn {
                        fb_sel = Some((cur + page).min(n - 1));
                        moved = true;
                    } else if pgup {
                        fb_sel = Some(cur.saturating_sub(page));
                        moved = true;
                    }
                    if moved {
                        if let Some(row_i) = fb_sel {
                            let top = row_i as f32 * row_h;
                            let vh = fb_view_h.max(row_h);
                            if top < fb_scroll_y {
                                fb_scroll_to = Some(top);
                            } else if top + row_h > fb_scroll_y + vh {
                                fb_scroll_to = Some(top + row_h - vh);
                            }
                        }
                    }
                    if enter {
                        if let Some(row_i) = fb_sel {
                            if has_parent && row_i == 0 {
                                go = parent_opt.clone();
                            } else if let Some((_, p)) = subdirs.get(row_i - usize::from(has_parent)) {
                                go = Some(p.clone());
                            }
                        }
                    }
                }

                let mut area = egui::ScrollArea::vertical().max_height(list_h).id_salt("fb_list").auto_shrink([false, false]);
                if let Some(y) = fb_scroll_to.take() {
                    area = area.vertical_scroll_offset(y);
                }
                if is_loading {
                    ctx.data_mut(|data| data.insert_temp(fb_painted_id, 0usize));
                    area.show(ui, |ui| {
                        if let Some(parent) = &parent_opt {
                            if ui.selectable_label(false, "..").clicked() {
                                go = Some(parent.clone());
                            }
                        }
                        ui.weak("Loading...");
                        let anim_on = ui.style().animation_time > 0.0;
                        let fill = crate::skeleton::fill(ui, anim_on);
                        let avail_w = ui.available_width().max(40.0);
                        for frac in [0.55_f32, 0.72, 0.44] {
                            let (r, _) = ui.allocate_exact_size(egui::vec2((avail_w * frac).max(24.0), 12.0), egui::Sense::hover());
                            ui.painter().rect_filled(r, 3.0, fill);
                            ui.add_space(4.0);
                        }
                    });
                } else {
                    let mut painted_rows = 0usize;
                    let sel_stroke = ui.visuals().selection.stroke;
                    let ring_stroke = egui::Stroke::new((sel_stroke.width + 1.0).max(2.0), sel_stroke.color);
                    let scroll_out = area.show_rows(ui, row_h, n, |ui, range| {
                        painted_rows = range.len();
                        for idx in range {
                            let is_sel = fb_sel == Some(idx);
                            if has_parent && idx == 0 {
                                let resp = ui.selectable_label(is_sel, "..");
                                if list_focused && is_sel {
                                    ui.painter().rect_stroke(resp.rect.shrink(1.0), crate::theme::tokens::RADIUS, ring_stroke, egui::StrokeKind::Inside);
                                }
                                if resp.clicked() {
                                    go = parent_opt.clone();
                                }
                            } else if let Some((name, path)) = subdirs.get(idx - usize::from(has_parent)) {
                                let resp = ui.selectable_label(is_sel, name.as_str());
                                if list_focused && is_sel {
                                    ui.painter().rect_stroke(resp.rect.shrink(1.0), crate::theme::tokens::RADIUS, ring_stroke, egui::StrokeKind::Inside);
                                }
                                if resp.clicked() {
                                    go = Some(path.clone());
                                }
                            }
                        }
                    });
                    ctx.data_mut(|data| {
                        data.insert_temp(fb_painted_id, painted_rows);
                        data.insert_temp(fb_scroll_y_id, scroll_out.state.clamped_offset().y);
                        data.insert_temp(fb_view_h_id, scroll_out.inner_rect.height());
                    });
                }

                if let Some(p) = go {
                    *path_text = p.display().to_string();
                    *dir = p;
                    *error = None;
                    dir_to_load = Some(dir.clone());
                    fb_sel = None;
                    ctx.data_mut(|data| data.insert_temp(fb_scroll_to_id, 0.0f32));
                }
                ctx.data_mut(|data| match fb_sel {
                    Some(s) => {
                        data.insert_temp(fb_sel_id, s);
                    }
                    None => data.remove::<usize>(fb_sel_id),
                });
                ui.horizontal(|ui| {
                    if ui.button("Select this folder").clicked() {
                        out.push(Command::AddRoot(dir.clone()));
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            }
        }
    });
    modal_resp.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Dialog, true, &title));
    // A prompt holds typed text: only Esc or Cancel close it, never a stray click beside it.
    let backdrop_ignored = matches!(dlg, Dialog::Prompt { .. }) && modal_resp.backdrop_response.clicked();
    let esc = !backdrop_ignored && modal_resp.should_close();
    if close || esc {
        if matches!(dlg, Dialog::FolderBrowser { .. }) {
            d.cancel_dir_listing();
        }
        ctx.data_mut(|data| {
            data.remove::<u8>(dlg_seen_id);
            data.remove::<usize>(fb_sel_id);
        });
    } else {
        if let Some(dir) = dir_to_load {
            let wake_ctx = ctx.clone();
            d.request_dir_listing(dir, Arc::new(move || wake_ctx.request_repaint()));
        } else if cancel_load {
            d.cancel_dir_listing();
        }
        d.current = Some(dlg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDirGuard(PathBuf);
    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn folder_browser_go_to_cached_dir_requests_cancel() {
        let base = std::env::temp_dir().join(format!("iron_sheets_fb_go_test_{}", std::process::id()));
        let _guard = TempDirGuard(base.clone());
        let cached = base.join("cached");
        let slow = base.join("slow");
        std::fs::create_dir_all(&cached).unwrap();
        std::fs::create_dir_all(&slow).unwrap();

        let mut dir = slow.clone();
        let mut error = Some("old error".into());
        let listing = Some((cached.clone(), vec![("sub".into(), cached.join("sub"))]));

        // Navigating via Go to an uncached dir clears error and does not cancel.
        assert!(!folder_browser_go(&slow.to_string_lossy(), &mut dir, &mut error, &listing));
        assert_eq!(dir, slow);
        assert_eq!(error, None);

        // Invalid path sets error and keeps current dir.
        assert!(!folder_browser_go(&base.join("missing").to_string_lossy(), &mut dir, &mut error, &listing));
        assert_eq!(dir, slow);
        assert_eq!(error.as_deref(), Some("Not a folder"));

        // Navigating back to the cached dir clears error and requests cancelling in-flight load.
        assert!(folder_browser_go(&cached.to_string_lossy(), &mut dir, &mut error, &listing));
        assert_eq!(dir, cached);
        assert_eq!(error, None);
    }

    #[test]
    fn folder_browser_go_click_cancels_in_flight_listing() {
        let base = std::env::temp_dir().join(format!("iron_sheets_fb_ui_test_{}", std::process::id()));
        let _guard = TempDirGuard(base.clone());
        let cached = base.join("cached");
        let other = base.join("other");
        std::fs::create_dir_all(cached.join("sub")).unwrap();
        std::fs::create_dir_all(&other).unwrap();

        let ctx = egui::Context::default();
        ctx.enable_accesskit();

        let mut d = Dialogs::default();
        d.set_dir_listing_paused(true);
        d.current = Some(Dialog::FolderBrowser {
            path_text: cached.display().to_string(),
            dir: other.clone(),
            error: None,
            listing: Some((cached.clone(), vec![("sub".into(), cached.join("sub"))])),
        });
        let mut out = Vec::new();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
            ..Default::default()
        };

        // Run two frames so the auto-sized Window completes its initial sizing pass, then locate "Go" via AccessKit.
        let mut go_center = None;
        for _ in 0..2 {
            let mut frame = ctx.run_ui(input.clone(), |ui| show(ui.ctx(), &mut d, &mut out));
            frame.textures_delta.clear();
            if let Some(u) = frame.platform_output.accesskit_update.as_ref() {
                if let Some(pos) = u.nodes.iter().find_map(|(_, n)| {
                    (n.label() == Some("Go")).then(|| n.bounds()).flatten().map(|b| {
                        egui::pos2(((b.x0 + b.x1) * 0.5) as f32, ((b.y0 + b.y1) * 0.5) as f32)
                    })
                }) {
                    go_center = Some(pos);
                }
            }
        }
        assert!(d.is_folder_browser_loading(), "other dir load must be active while paused");
        let go_center = go_center.expect("Go button bounds");

        // Click "Go" to navigate back to `cached`.
        let mut click_input = input;
        click_input.events = vec![
            egui::Event::PointerMoved(go_center),
            egui::Event::PointerButton {
                pos: go_center,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos: go_center,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ];
        let mut clicked = ctx.run_ui(click_input, |ui| show(ui.ctx(), &mut d, &mut out));
        clicked.textures_delta.clear();

        assert!(!d.is_folder_browser_loading(), "Go back to cached dir must cancel in-flight load");
        let Some(Dialog::FolderBrowser { dir, error, listing, .. }) = &d.current else {
            panic!("FolderBrowser should remain open");
        };
        assert_eq!(dir, &cached);
        assert!(error.is_none());
        assert_eq!(listing.as_ref().map(|(p, _)| p), Some(&cached));
        d.set_dir_listing_paused(false);
    }

    #[test]
    fn classify_pick_files_detects_unavailable_portal() {
        use std::time::Duration;
        assert!(matches!(classify_pick_files(None, Duration::from_millis(20)), Pick::FilesUnavailable));
        assert!(matches!(classify_pick_files(None, Duration::from_millis(300)), Pick::Cancelled));
        let p = PathBuf::from("note.md");
        assert!(matches!(classify_pick_files(Some(vec![p.clone()]), Duration::from_millis(10)), Pick::Files(v) if v == vec![p]));
    }
}
