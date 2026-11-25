#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
#[cfg(feature = "ui-egui")]
use iron_sheets::{AppOptions, RunArgs};
#[cfg(feature = "ui-egui")]
use std::path::PathBuf;
#[cfg(feature = "ui-egui")]
use std::time::Instant;

#[cfg(feature = "ui-egui")]
const HELP: &str = "iron sheets - Markdown and Typst notes\n\nUSAGE: iron-sheets [PATH...] [--workspace NAME] [--bench-startup[=full]] [--screenshot FILE] [--settings] [--help] [--version]\n\n  PATH            folder: open as workspace roots; file: open as tab\n  --workspace N   workspace name to open or create\n  --bench-startup print startup milestones and exit after frame 3; =full: wait until index, tree and watcher are done\n  --screenshot F  save a PNG after a few frames and exit\n  --settings      open the Settings window at start\n";

#[cfg(feature = "ui-egui")]
fn main() -> iron_sheets::RunResult {
    let t0 = Instant::now();
    let mut paths: Vec<PathBuf> = Vec::new();
    let (mut workspace, mut bench, mut bench_full, mut screenshot) = (None, false, false, None);
    let mut open_settings = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return Ok(());
            }
            "--version" | "-V" => {
                println!("iron-sheets {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--bench-startup" => bench = true,
            "--bench-startup=full" => (bench, bench_full) = (true, true),
            "--settings" => open_settings = true,
            "--workspace" => workspace = args.next(),
            "--screenshot" => screenshot = args.next().map(PathBuf::from),
            s if s.starts_with("--") => {
                eprintln!("unknown option {s}\n\n{HELP}");
                std::process::exit(2);
            }
            _ => paths.push(PathBuf::from(a)),
        }
    }
    let dirs = iron_sheets_core::paths::AppDirs::discover();
    let (config, config_error) = iron_sheets_core::config::AppConfig::load_checked(&dirs);
    if bench {
        println!("MILESTONE config loaded {:.1}", t0.elapsed().as_secs_f32() * 1000.0);
    }
    let opts = AppOptions { dirs, config, start: t0, paths, workspace, print_milestones: bench, index_start_delay: std::time::Duration::from_millis(30), config_error };
    iron_sheets::run(RunArgs { opts, bench, bench_full, screenshot, open_settings })
}

// Without a frontend the library reports the error; this keeps the binary from adding noise.
#[cfg(not(feature = "ui-egui"))]
fn main() {}
