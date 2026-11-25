//! `cargo run -p iron-sheets --example sample_vault -- <dir> <count>`: generate a vault of `count` linked notes.
use std::path::PathBuf;

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().unwrap_or_else(|| "vault".into()));
    let count: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(1000);
    let mut seed = 0x9E3779B97F4A7C15u64;
    let mut rnd = move |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n as u64) as usize
    };
    let words = ["alpha", "river", "stone", "garden", "signal", "paper", "orbit", "lantern", "maple", "quartz", "harbor", "ember"];
    for i in 0..count {
        let folder = dir.join(format!("folder {}", i / 50)).join(format!("sub {}", (i / 10) % 5));
        std::fs::create_dir_all(&folder)?;
        let mut t = format!("# Note {i}\n\n");
        for p in 0..(3 + rnd(5)) {
            t.push_str(&format!("## Section {p}\n\n"));
            for _ in 0..(20 + rnd(60)) {
                t.push_str(words[rnd(words.len())]);
                t.push(' ');
            }
            t.push_str(&format!("\n\nSee [[Note {}]] and [[Note {}#Section 1]]. **bold** *em* `code`.\n\n", rnd(count), rnd(count)));
        }
        t.push_str("- item\n- item two\n\n| a | b |\n|---|---|\n| 1 | 2 |\n");
        std::fs::write(folder.join(format!("Note {i}.md")), t)?;
    }
    println!("wrote {count} notes under {}", dir.display());
    Ok(())
}
