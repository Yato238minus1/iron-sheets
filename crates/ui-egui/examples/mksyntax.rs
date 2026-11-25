//! Builds `assets/syntax/<token>.packdump`, an optional syntax the binary embeds when present.
//!
//! `assets/syntax/` is not committed (third-party grammars). To enable a language locally, fetch a Sublime Text
//! grammar (`.sublime-syntax`) and build its dump:
//!
//! ```text
//! cargo run -p iron-sheets-ui-egui --example mksyntax -- <token> /path/to/Lang.sublime-syntax
//! ```
//!
//! `<token>` is the fenced-block info string (```` ```token ````) and the dump filename. After adding a dump,
//! force one rebuild (cargo does not notice files that appear later): `touch crates/ui-egui/build.rs`. Nothing
//! else to do: `build.rs` embeds every `*.packdump` it finds and code highlighting picks the set by token.
//!
//! If the compiler or the editor complains about a stale `bundled_syntaxes.rs` (an older build-script run):
//! `cargo clean -p iron-sheets-ui-egui` regenerates it, and restarting rust-analyzer clears its cached
//! build-script diagnostics.
//!
//! Dumps are precompiled because loading one costs about 0.02 ms while parsing the YAML at runtime costs about
//! 90 ms. A dump is tied to the syntect version; regenerate after an upgrade. Keep third-party grammars and
//! their license files out of commits, and add their licenses to `THIRD-PARTY-LICENSES.txt` in any binary you
//! distribute.
use syntect::dumps::dump_to_uncompressed_file;
use syntect::parsing::{SyntaxDefinition, SyntaxSetBuilder};

fn main() {
    let mut args = std::env::args_os().skip(1);
    let (Some(token), Some(grammar)) = (args.next(), args.next()) else {
        eprintln!("usage: cargo run -p iron-sheets-ui-egui --example mksyntax -- <token> <path-to.sublime-syntax>");
        std::process::exit(2);
    };
    let token = token.to_string_lossy();
    if token.is_empty() || token.contains(['/', '\\', '.']) {
        eprintln!("<token> must be a plain name (the fenced-block language), got {token:?}");
        std::process::exit(2);
    }
    let yaml = std::fs::read_to_string(&grammar).expect("read grammar");
    let def = SyntaxDefinition::load_from_str(&yaml, false, Some(&token)).expect("parse grammar");
    let mut b = SyntaxSetBuilder::new();
    b.add(def);
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/syntax");
    std::fs::create_dir_all(&dir).expect("create assets/syntax");
    dump_to_uncompressed_file(&b.build(), dir.join(format!("{token}.packdump"))).expect("write dump");
    println!("wrote {}", dir.join(format!("{token}.packdump")).display());
}
