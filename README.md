# iron sheets

The name is put together from iron, because of rust, and sheets, because it's a note editor.

A native Markdown (and Typst) note editor written in Rust (egui/eframe), inspired by editors like [Obsidian](https://obsidian.md/) and [ZenNotes](https://zennotes.org/). Built for speed: fast start, fast note opening, smooth typing, thousands of notes.

This project has Typst support, so inline math supports the Typst Syntax. There is an auto detection for what you are using, but if you want to specify it directly take a look in the settings under preview. 

Another differentiating feature is the synchronized scrolling in the split view, which works like a charm.

## Build and run

Needs a stable Rust toolchain. Targets: primaly Linux with Wayland, but X11 and Windows are supported (although not thoroughly tested).

```sh
cargo run --release
# other options -> you dont really need them if you just use the settings
cargo run -p iron-sheets --release -- [FOLDER...] [NOTE...]
cargo run -p iron-sheets --release -- --workspace Work ~/notes ~/projects/docs
cargo run -p iron-sheets --no-default-features --features ui-egui,wayland,x11   # minimal: no Typst, no optional modules (smaller; add `accesskit` to keep screen-reader support)
cargo run -p iron-sheets --no-default-features --features ui-egui,typst,font-lilex,git,excalidraw   # all but Kanban
cargo test --workspace
```

Cargo features of `iron-sheets` (all on by default; `--no-default-features --features ui-egui,...` picks a subset):

| Feature | What it adds | Without it |
|---|---|---|
| `ui-egui` | the egui frontend (required) | — |
| `typst` | `.typ` notes, Typst math, symbol panel, Typst fonts | math stays source text |
| `pdf` | Typst PDF export (needs `typst`; drops the krilla PDF stack) | no Export PDF command; the keymap entry says it is not built in |
| `typst-packages` | `@preview/...` Typst packages downloaded over HTTPS (needs `typst`; drops ureq, native-tls and the libssl runtime dependency) | only packages already on disk (`@local`, the Typst cache) resolve; a missing `@preview` package is a diagnostic in the preview |
| `mermaid` | ` ```mermaid ` blocks as diagrams (Settings toggles; mermaid-rs-renderer; resvg is shared with `excalidraw`) | a mermaid block is a plain code block, no Settings rows for it |
| `codehl` | syntax colours in fenced code blocks (syntect, two-face; Settings toggle; free when `typst` is on, which links the same syntax dump) | code blocks are plain text, no Settings row |
| `font-lilex` | the embedded Lilex editor font | DejaVu Sans Mono |
| `kanban` | Kanban boards (Settings toggle) | `kanban-plugin` notes are plain Markdown |
| `git` | branch/dirty count in the status bar, tree badges, diff gutter, a Git panel (stage, unstage, discard, commit, push, pull, fetch, branches, remotes, init; Settings toggle) | no Git code and no `git` processes |
| `calendar` | Calendar sidebar tab and daily notes (Settings toggle) | no calendar code |
| `wayland`, `x11` | Linux windowing backends (Windows ignores them); a Linux build needs at least one | no window on Linux without one |
| `accesskit` | the screen-reader tree (AccessKit, drops the zbus stack on Linux) | widgets still carry names, but no assistive technology sees them |
| `image-extra` | gif, webp and bmp images | those images show the "could not load" state (Typst and resvg link the decoders anyway when on) |
| `excalidraw` | `.excalidraw` / `.excalidraw.md` viewer and `![[drawing]]` embeds | `.excalidraw` files are other files, `.excalidraw.md` plain Markdown |
| `system-fonts` | lazy system fallback fonts: CJK, Arabic, Hebrew, Indic, ... are found among the installed fonts when a note first shows such a character (not before, never for emoji; a missing font shows one status message and tofu; Linux needs e.g. Noto CJK installed; adds fontique) | those characters show as boxes |

A config file that sets `module_kanban`, `module_git` or `module_calendar` still loads in a build without that feature; the setting is ignored.

Folders given on the command line become roots of the current workspace; files open as tabs. Config, workspaces and the session live in the platform config folder (override with `IRON_SHEETS_HOME`).

## Cool Features

### Workspaces
instead of a vault:
- first added folder becomes root
- you can put in folders from multiple locations without symlinks

### Markdown
Full GitHub-flavoured support

### Wiki links
Instead of the usual `()[]`, `[[]]` is also supported. Hovering with CTRL also shows a popup to the linked file.

### Split view
(source | preview, also source-only and preview-only) with **synchronized scrolling** in both directions, block-accurate.

### Typst
`.typ` notes with a live page preview (incremental compile, diagnostics strip that jumps to the line, click on the page jumps to the source, PDF export). `$...$` and `$$...$$` in Markdown are **Typst math** by default (setting "Math syntax in Markdown": LaTeX is available too), rendered as images in the preview. `$x + y $` and `$ x + y $` count as math; prices like `$5 and $10` stay text (rule in `crates/md/src/math.rs`).

### Draw a symbol
sidebar panel where you draw a symbol and get the matching Typst symbols; search by name; insert as name, `#sym.name` or character depending on context.

### Editor
syntax highlighting (Markdown and Typst), list continuation, Tab/Shift+Tab indent, Ctrl+Shift+B/I/K/\` formatting, find and replace (F3) with an optional regular-expression mode (`.*` toggle; `^`/`$` per line, `$1`/`${name}` in the replacement), move line (Ctrl+Shift+Up/Down or Alt+Up/Down), copy line up/down (Alt+Shift+Up/Down), Alt+X / Alt+C cut / copy.

### Optional syntaxes
code blocks are colored for the languages that ship with syntect (Rust, Bash, Python and many more). any other language is strictly opt-in and never bundled by default, since grammars are third-party files: you build a dump from a `.sublime-syntax`, drop it into `crates/ui-egui/assets/syntax/` (gitignored) and rebuild. how to do that: [docs/optional-syntaxes.md](docs/optional-syntaxes.md)

### Calendar

Sidebar tab "Calendar" (module, Settings, Modules): a month grid with prev/next and Today; days that have a daily note carry a dot, today is outlined. Click a day or press Enter to open its note, creating it when missing; arrows move the day, Page Up/Down the month, Esc returns to the editor. Palette: 
- "Open today's daily note" (`calendar.today`)
- "Show calendar"
Settings:
- folder (default `Daily`, under the first workspace folder)
- name pattern (default `YYYY-MM-DD`, tokens `YYYY`, `MM`, `DD`; `/` makes subfolders)
- optional template note (empty = `# <name>`). 
Weeks start on Monday; word-count and open-task dots are not there yet.

### Settings
Ctrl+,, or `--settings` at start

### Accessibility

all controls and list rows carry accessible names, roles and selection state (AccessKit); the file tree and lists work with the keyboard; focus is visible; text colours meet WCAG AA contrast in dark, light and high-contrast themes (checked by a test); state is never shown by colour alone.

## Themes, professional mode, new-tab page

**Themes.** Settings > General > Theme lists System, Light, Dark, two built-in themes (High contrast dark, Sepia) and every `*.toml` in `<config folder>/themes/` ("Open themes folder" opens it, palette: `view.themes_folder`). The folder is read when the choice is opened, and at startup only the one chosen file; a theme file is read when it is chosen or at startup, so edits apply after choosing the theme again (or restarting). The choice is stored by id (the file name without `.toml`) in `theme_file`; a file with the id of a built-in replaces it. A file that does not parse falls back to the base theme and the status bar names the file and line. Colour keys are the ones of Settings > Colors; your own colour overrides stay on top of the theme's.

```toml
name = "Solarized-ish"   # shown in the list; default: the file name
base = "light"           # "dark" (default) or "light": the theme everything else comes from

[colors]                 # all optional: accent editor_bg editor_text link heading emphasis strong code quote
accent = "#eee8d5"       # marker comment keyword string number tag math
editor_bg = "#fdf6e3"
editor_text = "#586e75"
link = "#268bd2"
```

**Professional mode** (palette `view.professional`, Settings > General, "Professional mode" in the status bar while on; click it to turn off). Renders with defaults without touching the saved settings (UI zoom and high contrast stay, they are accessibility settings; the font-size, font and preview-font commands do nothing and say so): the OS theme, default fonts and sizes, no colour overrides or theme file, no caret trail, no F12 overlay and no new-tab extras. It is one function, `app_core::effective::effective`, that maps the stored config to the config the UI reads (no copy at all while the mode is off). The state persists (`professional` in the config).

**New-tab page.** An empty tab shows a random quote at the bottom (picked once per tab; Settings > Interface: switch, "Quotes file" = a text file with one quote per line, optional ` — author`, `#` lines are comments; empty = ten built-in public-domain quotes; a relative path for the file or the image is relative to the config folder) and, when "New-tab background image" is set, that image scaled to cover and faded into the page colour. The image is decoded on a worker thread through the preview's image cache; nothing extra is drawn until it is ready, a failure is one status error.

Don't worry you can easily take that out yourself and it will become optional very soon.


## Shortcuts

| Keys | Action |
|---|---|
| Ctrl+O | Quick open (type `>` for commands, `#` for tags) |
| Ctrl+P | Command palette |
| Ctrl+N | New note |
| Ctrl+S | Save |
| Ctrl+W / Ctrl+Shift+T | Close tab / reopen closed tab |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous tab |
| Alt+Left / Alt+Right | Navigate back / forward |
| Ctrl+E | Cycle view mode (source, split, preview) |
| Ctrl+B or Ctrl+\ | Toggle sidebar |
| F2 | Rename file |
| Ctrl+F / F3 / Shift+F3 | Find in note / next / previous |
| Ctrl+H | Find and replace (focus in the replace field); Enter replaces and goes on, Shift+Enter goes back, Ctrl+Alt+Enter replaces all |
| Mouse back / forward button | Navigate back / forward |
| Ctrl+Shift+F | Search in workspace |
| Ctrl+Shift+B, Ctrl+I, Ctrl+K, Ctrl+` | Bold, italic, link, inline code |
| Alt+Up / Alt+Down (also Ctrl+Shift+Up/Down) | Move line(s) up / down |
| Alt+Shift+Up / Alt+Shift+Down | Copy line(s) up / down |
| Alt+X / Alt+C | Cut / copy the selection (like Ctrl+X / Ctrl+C) |
| Ctrl+click | Follow link under the cursor (Shift: new tab) |
| Middle-click | Open a note or link in a new tab; on a tab: close it |
| Ctrl+Plus or Ctrl+= / Ctrl+- / Ctrl+0 | UI zoom in / out / reset zoom and font sizes |
| Ctrl+mouse wheel | Editor and preview font size (over a Typst page: page zoom) |
| F12 | Performance overlay |
| Ctrl+, | Settings (all shortcuts above can be changed there) |

## Layout


|crate|name|description|
|-|-|-|
|core|iron-sheets-core|config, workspaces, file tree, index, link resolution, search, fuzzy match, watcher (no GUI)|
|md|iron-sheets-md|Markdown -> flat block model, editor highlighter, editing helpers|
|symrec|iron-sheets-symrec|hand-drawn symbol recognizer|
|typst-engine|iron-sheets-typst|Typst world, compile/render workers, math snippets, symbol table|
|app-core|iron-sheets-app-core|framework-free state, commands, models|
|ui-egui|iron-sheets-ui-egui|the egui frontend|
|app|iron-sheets|thin binary: picks the frontend by cargo feature|

Headless UI tests with screenshots live in `crates/app/tests/` (they render headless through kittest and write PNGs to `target/ui-shots/`).

## Usage of AI

This project has used a lot of AI for its coding and probably has flaws, however every feature is thoroughly tested and the code is supervised by a human.
The plans are written by AI, the roadmap and the majority of the README by human.


## License

iron sheets is licensed under the [GNU General Public License v3.0](LICENSE) (`GPL-3.0-only`). Anyone who distributes a binary must also offer its complete corresponding source code under the same license.

Every binary distribution must ship [THIRD-PARTY-LICENSES.txt](THIRD-PARTY-LICENSES.txt) next to the executable. It contains the licenses and notices of the bundled Rust crates, fonts (Typst and egui) and syntax definitions.
