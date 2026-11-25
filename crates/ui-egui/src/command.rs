//! Commands: the framework-free part is `iron_sheets_app_core::command`; this module adds what needs egui (keyboard
//! shortcuts, native pickers, PDF export).
use crate::dialogs;
use crate::state::AppState;
pub use iron_sheets_app_core::command::*;
use iron_sheets_app_core::platform::Platform;

/// Consume pressed global shortcuts and queue their commands. Uses the prebuilt keymap: no work unless a key was pressed.
pub fn collect_shortcuts(ctx: &egui::Context, state: &mut AppState) {
    if crate::settings::handle_recording(state, ctx) {
        return;
    }
    for idx in crate::keys_egui::dispatch(&state.keymap, ctx) {
        if let Some(c) = (all_commands()[idx].make)(&state.tabs) {
            state.commands.push(c);
        }
    }
}

/// Runs a command: the core runs everything framework-free, the commands it hands back are finished here.
pub fn execute(state: &mut AppState, platform: &mut dyn Platform, cmd: Command) {
    let Some(cmd) = iron_sheets_app_core::command::execute(state, platform, cmd) else { return };
    let w = platform.waker();
    match cmd {
        Command::AddRootDialog => dialogs::pick_folder(&mut state.dialogs, &w),
        Command::OpenFileDialog => dialogs::pick_files(&mut state.dialogs, &w),
        Command::SaveAsDialog { path, after } => {
            let name = suggested_name(state, &path);
            dialogs::pick_save(&mut state.dialogs, &w, path, after, &name);
        }
        Command::ExportPdf => crate::preview::typst::export_pdf(state, &w),
        _ => {}
    }
}
