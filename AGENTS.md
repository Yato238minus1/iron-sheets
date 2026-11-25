# iron sheets

Native Markdown + Typst note editor written in Rust.

## What the user asked for (acceptance criteria)

- **Performance first, everywhere**: app start, opening notes, typing. Function and layout matter; visual design does not.
- Obsidian-style wiki-links (`[[Note]]`, `[[Note|alias]]`, `[[Note#Heading]]`, `![[embed]]`) and tabs.
- Full GitHub-flavored Markdown.
- Typst: `.typ` notes with live preview, Typst math inside Markdown, and a Tinymist-style "draw a symbol to find it" panel.
- Synchronized scrolling between source and preview.
- Workspaces instead of a vault folder: one workspace holds several root folders from anywhere on disk.
- Hundreds to thousands of files: lazy loading, background indexing, virtualized lists.
- Targets: Linux (Wayland) and Windows.

## Layout

|crate|name|description|
|-|-|-|
|core|iron-sheets-core|no GUI deps: app dirs/config, workspaces, text IO, file tree model, index + cache, link resolution, fuzzy match, search, file watching|
|md|iron-sheets-md|no GUI deps: comrak -> flat block model with source lines, editor syntax highlighter, text-editing helpers
|symrec|iron-sheets-symrec|hand-drawn symbol recognizer (font glyph templates)|
|typst-engine|iron-sheets-typst|Typst world, compile/render workers, math snippets, symbol table, .typ highlighting
|app-core|iron-sheets-app-core|no GUI deps: `AppState<U>`, `Command` + `execute`, docs, tabs, scroll contract, keymap, settings/palette/find/search/links/outline/statusbar/theme models; optional `typst` feature|
|ui-egui|iron-sheets-ui-egui|egui/eframe frontend: frame loop, all widgets, editor, previews, Typst UI, theme-to-egui, native dialogs (rfd), syntect highlighting; optional extra syntaxes via `assets/syntax/*.packdump` (not committed; embedded by `crates/ui-egui/build.rs`, built by `examples/mksyntax.rs`, guide: `docs/optional-syntaxes.md`)|
|app|iron-sheets|thin: binary + lib that selects the frontend by cargo feature (`ui-egui` default, `ui-xilem` placeholder, exactly one) and re-exports it; holds the kittest UI tests. Typst, Kanban, Git, Excalidraw and Calendar are behind the default-on features `typst`, `kanban`, `git`, `excalidraw`, `calendar`|

Crates do not depend on each other except `app-core` (depends on core, md and optionally typst-engine), `ui-egui` (app-core, core, md, symrec, typst-engine) and `app` (app-core, ui-egui). Versions in use: egui/eframe 0.37, Typst 0.15.1, comrak 0.59. Read Crate sources for reading APIs instead of trusting memory, these versions are recent.

## App architecture (crates/app-core, crates/ui-egui, crates/app)

- Renderer: glow. 
- `iron_sheets_app_core::state::AppState<U>` is a plain struct with public fields; `U` is the frontend's module state in `state.ui`. ui-egui defines `EguiUi` (theme, panels, editor, previews) and `type AppState = AppState<EguiUi>`; widgets take `&mut AppState` and read `state.ui.editor` etc. (take a module's state out with `std::mem::take`, call the module, put it back). Model data the core needs (palette model, dialogs, settings state, search/find/reveal requests, symbol insert mode) is a plain core field. No `Deref` wrapper (it would break disjoint borrows).
- Frontend seam for commands: `iron_sheets_app_core::command::execute` runs every framework-free command and hands back the ones that need the frontend (`AddRootDialog`, `OpenFileDialog`, `ExportPdf`) as `Some(cmd)`; `iron_sheets_ui_egui::command::execute` wraps it and finishes those. `collect_shortcuts` (egui key input) lives in ui-egui. `CommandInfo::make` takes `&Tabs`.
- Modules never call each other for side effects. They push `Command`s onto `state.commands`; `command::execute` runs the queue at the start of the next frame.
- Documents: `DocStore` holds one `Doc` per path (text buffer, version counter, save state). Tabs reference docs by path; a tab restored from a session loads its file only when first shown.
- Scrolling contract: each tab has `editor_scroll` and `preview_scroll` of type `PaneScroll` (`offset`, `viewport_h`, `content_h`, `force: Option<f32>`, `user_scrolled`, `map: ScrollMap` = source line -> y in that pane). A pane fills only its own `PaneScroll`; `scrollsync::sync` couples the two. Panes never reach into each other.
- Typst-backed features reach the UI through traits in `services`, so everything except the Typst modules builds and runs without the `typst` feature. `--no-default-features` alone selects no frontend and fails by design (`compile_error!`); use `--no-default-features --features ui-egui`.
- UI verification: tests in `crates/app/tests/` use the helpers in `tests/common` to drive the app headless and write PNG screenshots; look at the screenshots before reporting.

## Rules

Performance
- Nothing slow on the UI thread. File walking, indexing, Typst compiles, image decoding and syntax-set loading run on background threads and report back through channels.
- Nothing runs before the first frame unless that frame needs it. Typst, syntect, extra fonts and the index are initialised lazily.
- Every list or document view is virtualized: only what is visible is laid out and painted.
- Cache by content hash, invalidate narrowly. An edit must not redo work for unchanged blocks.
- Measure before and after any performance claim; numbers go in the README.

Portability
- Must build (use `cargo check`) for `x86_64-unknown-linux-gnu` and `x86_64-pc-windows-msvc`. No Unix-only API without `cfg`. Prefer pure-Rust dependencies over C libraries.
- Use `Path`/`PathBuf` joins, never string concatenation with separators. Do not call `fs::canonicalize` (it yields `\\?\` paths on Windows); use `std::path::absolute`.
- Notes may have CRLF line endings and a BOM: normalise in memory, restore on save.

Code
- No `unwrap()`/`expect()` on anything that depends on user files or input; errors surface in the UI.
- A new capability with a heavy dependency gets a cargo feature (on in `default`, off in the minimal build). A `#[cfg(feature)]` comes with its row in the README Features table, its names in the `ALL` list of the check loop under Commands, and tests that carry the same cfg. Config fields never disappear (old configs must load); only their Settings rows and effect do.
- Keep dependency versions aligned with the ones `typst` already pulls in (see the root `Cargo.toml` comment).
- `iron-sheets-core` stays free of GUI and Typst dependencies and is unit-tested.

## Commands

```sh
cargo run -p iron-sheets                       # dev build (deps optimised, own code opt-level 1)
cargo run -p iron-sheets --release             # optimised build
cargo test --workspace                     # all tests
cargo check -p iron-sheets --target x86_64-pc-windows-msvc   # Windows type-check
cargo check -p iron-sheets --no-default-features --features ui-egui,wayland,x11   # minimal (Linux needs a windowing backend)
cargo check -p iron-sheets-app-core --no-default-features     # core alone
cargo check -p iron-sheets-ui-egui --no-default-features --features wayland   # egui frontend without Typst
# optional features: each one left out of the full set, then each one alone on top of the minimal set
ALL=typst,pdf,typst-packages,font-lilex,kanban,git,excalidraw,calendar,mermaid,codehl,wayland,x11,accesskit,image-extra,system-fonts
for f in ${ALL//,/ }; do
  cargo check -p iron-sheets --no-default-features --features ui-egui,$(echo ",$ALL," | sed "s/,$f,/,/;s/^,//;s/,$//")   # all but $f
done
for f in ${ALL//,/ }; do
  cargo check -p iron-sheets --no-default-features --features ui-egui,wayland,x11,typst,$f   # minimal + typst + $f
done
```

Windows type-check of anything that depends on Typst needs C headers for the `psm`/`stacker` build script. On the dev machine this works:

```sh
CC_x86_64_pc_windows_msvc=clang-cl AR_x86_64_pc_windows_msvc=llvm-lib CFLAGS_x86_64_pc_windows_msvc="-imsvc /usr/include/wine/msvcrt -imsvc /usr/include/wine/windows -D__WINESRC__" cargo check -p iron-sheets --target x86_64-pc-windows-msvc
```

Do not commit unless the user asks.

## Orchestrating feature agents

- Run at most two implementation agents at once, keep orchestrator turns few.
- Agents that edit `crates/app` at the same time each get their own copy of the repository, so a half-edited file never breaks another agent's build: `rsync -a --delete --exclude target --exclude .git ./ target/agents/<name>/repo/`. Build output goes to one of two fixed slots, `CARGO_TARGET_DIR=<repo>/target/agents/slot-a` or `slot-b` (one per concurrently running agent), never a new directory per agent: a full build with tests is 11-14 GB.
- Each agent keeps `target/agents/<name>/PROGRESS.md` and writes its final report to `target/agents/<name>/REPORT.md`. Merge = copy the agent's owned files into the main tree, apply the few listed changes to shared files by hand, then build and test in the main tree.
- Do not run `cargo clean`: `target/agents/` holds agent work copies and reports.

## Working style for implementation agents

- Smallest complete change, reuse what exists, no speculative options. Known limits get a `shortcut: <limit>, <when to upgrade>` comment.
- Work in thin slices, build and test each; test-driven development for new logic and bug fixes (failing test first); measure first for anything on the frame or keystroke path.
- Before reporting, an agent reviews its own diff and simplifies what it wrote; the report lists what the review found and what was fixed.
- Before every merge into main, the orchestrator has the diff reviewed by a fresh-context reviewer and fixes confirmed findings first. One feature branch per wave, `--no-ff` merge, verified gates before each commit.

## Rules for subagents

- You own only the files named in your brief. Do not edit other crates, the root `Cargo.toml`, or `CLAUDE.md`; if you need a change there, say so in your report.
- Several agents build at the same time. Always pass your own target directory and a job limit: `CARGO_TARGET_DIR=target/agents/<your-name> cargo <cmd> -p <your-crate> -j <N>` from the repository root. After editing a `Cargo.toml`, run `cargo metadata --no-deps >/dev/null` at once so a broken manifest never blocks the others.
- Never wait for a process with a loop like `until ! pgrep -f "cargo test"`: `pgrep -f` matches the loop's own command line, so it never ends. Run the long command in the background and wait for its notification. If you must poll a file, give the loop a time limit (`timeout 1800 bash -c 'until ...; do sleep 15; done'`) and start at most one such loop per job.
- No network use other than cargo fetching crates. No system package installs. No commits.
- Verify before reporting: `cargo test -p <crate>`, `cargo clippy -p <crate>` without warnings you introduced, and `cargo check -p <crate> --target x86_64-pc-windows-msvc`.
- The API in your brief is a contract. Keep the names and shapes; add what you need; list every deviation in the report.
- Report format: public API as implemented (signatures only), deviations, test and measurement results with numbers, known gaps. Be exact about what you verified and what you did not.

## UX rules (function & layout only; visual design is out of scope)

Every async crossing (file IO, indexing, Typst compile, decode, highlighting)has a designed state: idle / waiting (lightweight placeholder, final geometry) /ready / empty / failed. No blank panes, no lone spinners, no silent failure.
Render from what you already have: open docs paint from DocStore on the switchframe; preview re-renders keep the previous render visible; staleness is astatusbar cue, never a wait.
Nothing on the keystroke path waits on background work; preview/compile isdebounced; syntax highlighting is incremental (content-hash blocks).
Lists (tree, results, outline, palette, symbols) are virtualized and open withkeyboard-first semantics; first result preselected; Enter runs, Esc cancels.
Errors surface in the UI (statusbar/toast pattern) with a retry or next action;dead wiki-links and unreadable files get designed states; no unwrap/expect.
Repaint only while work is pending or the caret blinks; idle repaints ≈ 0.
A toggleable debug overlay shows fps, frame ms and in-flight jobs; everyperformance change on the frame/keystroke path ships with before/after numbersin the report (and README where AGENTS.md requires).
egui/eframe APIs are verified against the registry sources before use; patterncode is ported from claude-egui-ux-kit/reference/feeling-fast-demo/ (adapted toAppState<U> + state.commands), not reinvented.