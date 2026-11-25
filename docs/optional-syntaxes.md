# Optional syntax highlighting

Code blocks in the preview (and the source view) are coloured with
[syntect](https://github.com/trishume/syntect). Out of the box you get every language of the bundled
[two-face](https://github.com/CosmicHorrorDev/two-face) set: Rust, Bash, Python, JavaScript, JSON, TOML, SQL, C,
HTML, CSS and many more.

Anything beyond that is **strictly opt-in and never ships by default**. Reason: extra grammars are third-party
files with their own licenses, so the repository and a fresh build contain none of them. You add a language
locally, and only for yourself.

## Adding a language

1. Find a Sublime Text grammar for it — a `.sublime-syntax` file. Good sources are GitHub and
   [Package Control](https://packagecontrol.io/); example: [wozjac/abap-sublime-syntax](https://github.com/wozjac/abap-sublime-syntax)
   for ABAP.
2. Build the precompiled dump from the repository root:

   ```sh
   cargo run -p iron-sheets-ui-egui --features codehl --example mksyntax -- <token> /path/to/Lang.sublime-syntax
   ```

   `<token>` is the language string you use in fenced blocks, so ```` ```token ```` — it becomes both the dump
   filename (`crates/ui-egui/assets/syntax/<token>.packdump`) and the lookup key. Pick the obvious one (`abap`,
   `plsql`, …).
3. Force one rebuild — cargo does not notice files that appear later:

   ```sh
   touch crates/ui-egui/build.rs
   cargo run --release
   ```

That is all. ```` ```token ```` blocks (and only those) are now coloured.

## What happens under the hood

`build.rs` embeds every `*.packdump` in `crates/ui-egui/assets/syntax/` into the binary; the highlighter picks
a set by exact token match. Each dump loads lazily on the worker thread when its language is first used (about
0.02 ms — that is why dumps are precompiled; parsing the YAML at runtime would cost about 90 ms). With no dumps
in the folder the whole lookup is compiled out: a normal build pays nothing for this feature. Everything here needs the cargo feature `codehl` (on by default).

## Removing a language

Delete the `.packdump`. Cargo notices removals on its own; rebuild and the language is gone.

## After a syntect upgrade

Dumps are tied to the syntect version. After upgrading syntect, regenerate each dump with the `mksyntax`
command above; a unit test in `crates/ui-egui/src/preview/md/codehl.rs` fails the build when a dump no longer
loads.

## Licensing — the two rules

- `crates/ui-egui/assets/syntax/` is gitignored: **never commit** grammars, notices or dumps (third-party
  copyright).
- If you distribute a binary that contains a grammar, add its license to `THIRD-PARTY-LICENSES.txt` first.

## Troubleshooting

- Errors about a stale `bundled_syntaxes.rs` (usually an editor caching an older build-script run):
  `cargo clean -p iron-sheets-ui-egui`, then rebuild. If rust-analyzer keeps showing old errors, restart the
  server.
- A new dump seems ignored: you skipped the `touch crates/ui-egui/build.rs` step.
- A block stays plain although the dump is there: check the token — the fenced info string must match the dump
  filename exactly.
