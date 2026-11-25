//! Prints highlighter spans of a file or stdin: `cargo run -p iron-sheets-md --example hl -- note.md`

use std::io::Read;

fn main() {
    let mut text = String::new();
    match std::env::args().nth(1) {
        Some(p) => text = std::fs::read_to_string(p).expect("read file"),
        None => {
            std::io::stdin().read_to_string(&mut text).expect("read stdin");
        }
    }
    let mut out = Vec::new();
    iron_sheets_md::highlight(&text, &mut out);
    for s in &out {
        println!("{:>6}..{:<6} {:<12} {:?}", s.range.start, s.range.end, format!("{:?}", s.tok), &text[s.range.clone()]);
    }
}
