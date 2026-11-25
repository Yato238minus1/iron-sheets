#![allow(clippy::collapsible_if)]
mod common;

#[path = "it/alt_keys.rs"]
mod alt_keys;
#[path = "it/anim2.rs"]
mod anim2;
#[path = "it/app.rs"]
mod app;
#[path = "it/big_folder_ux.rs"]
mod big_folder_ux;
#[path = "it/bigcost.rs"]
mod bigcost;
#[path = "it/bignote_prefetch.rs"]
mod bignote_prefetch;
#[path = "it/bignote_screens.rs"]
mod bignote_screens;
#[path = "it/bignote_stale.rs"]
mod bignote_stale;
#[path = "it/bugs3.rs"]
mod bugs3;
#[path = "it/caret_anim.rs"]
mod caret_anim;
#[cfg(feature = "codehl")]
#[path = "it/codehl.rs"]
mod codehl;
#[path = "it/dead_links.rs"]
mod dead_links;
#[path = "it/deleted_open.rs"]
mod deleted_open;
#[path = "it/editor.rs"]
mod editor;
#[path = "it/editor_easy.rs"]
mod editor_easy;
#[path = "it/editor_tree.rs"]
mod editor_tree;
#[path = "it/editor_ux.rs"]
mod editor_ux;
#[path = "it/editor_zoom.rs"]
mod editor_zoom;
#[cfg(feature = "excalidraw")]
#[path = "it/excalidraw.rs"]
mod excalidraw;
#[cfg(feature = "calendar")]
#[path = "it/calendar.rs"]
mod calendar;
#[path = "it/find_regex.rs"]
mod find_regex;
#[path = "it/fixes.rs"]
mod fixes;
#[path = "it/folder_notes.rs"]
mod folder_notes;
#[cfg(feature = "git")]
#[path = "it/git.rs"]
mod git;
#[cfg(feature = "git")]
#[path = "it/git_hunks.rs"]
mod git_hunks;
#[cfg(feature = "git")]
#[path = "it/git_v2.rs"]
mod git_v2;
#[path = "it/hl_screens.rs"]
mod hl_screens;
#[path = "it/hover.rs"]
mod hover;
#[path = "it/images.rs"]
mod images;
#[cfg(feature = "kanban")]
#[path = "it/kanban.rs"]
mod kanban;
#[path = "it/layout.rs"]
mod layout;
#[cfg(feature = "font-lilex")]
#[path = "it/ligatures.rs"]
mod ligatures;
#[path = "it/ligatures_preview.rs"]
mod ligatures_preview;
#[path = "it/links_dnd.rs"]
mod links_dnd;
#[path = "it/lists_big.rs"]
mod lists_big;
#[cfg(feature = "typst")]
#[path = "it/math_auto.rs"]
mod math_auto;
#[cfg(feature = "typst")]
#[path = "it/math_syntax.rs"]
mod math_syntax;
#[path = "it/md_preview.rs"]
mod md_preview;
#[path = "it/menus_ctx.rs"]
mod menus_ctx;
#[cfg(feature = "mermaid")]
#[path = "it/merfix.rs"]
mod merfix;
#[cfg(not(feature = "mermaid"))]
#[path = "it/mermaid_off.rs"]
mod mermaid_off;
#[cfg(feature = "mermaid")]
#[path = "it/mermaid.rs"]
mod mermaid;
#[cfg(feature = "mermaid")]
#[path = "it/mermaid_ux.rs"]
mod mermaid_ux;
#[path = "it/middle_click.rs"]
mod middle_click;
#[path = "it/open_big.rs"]
mod open_big;
#[path = "it/open_failed.rs"]
mod open_failed;
#[path = "it/open_lazy.rs"]
mod open_lazy;
#[path = "it/newtab.rs"]
mod newtab;
#[path = "it/panels.rs"]
mod panels;
#[path = "it/preview_bugs.rs"]
mod preview_bugs;
#[path = "it/preview_c2.rs"]
mod preview_c2;
#[cfg(feature = "typst")]
#[path = "it/preview_stable.rs"]
mod preview_stable;
#[path = "it/render.rs"]
mod render;
#[path = "it/restore_prefetch.rs"]
mod restore_prefetch;
#[path = "it/settings.rs"]
mod settings;
#[path = "it/settings_tree.rs"]
mod settings_tree;
#[path = "it/sidebar_width.rs"]
mod sidebar_width;
#[cfg(all(feature = "calendar", feature = "git", feature = "typst"))]
#[path = "it/sidecollapse.rs"]
mod sidecollapse;
#[path = "it/startup_ux.rs"]
mod startup_ux;
#[path = "it/sync_enable.rs"]
mod sync_enable;
#[path = "it/title_bar.rs"]
mod title_bar;
#[path = "it/trash.rs"]
mod trash;
#[path = "it/tree_dnd.rs"]
mod tree_dnd;
#[path = "it/tree_states.rs"]
mod tree_states;
#[path = "it/tree_ux.rs"]
mod tree_ux;
#[cfg(feature = "typst")]
#[path = "it/typst_ui.rs"]
mod typst_ui;
#[cfg(feature = "typst")]
#[path = "it/typst_wait.rs"]
mod typst_wait;
#[path = "it/untitled.rs"]
mod untitled;
#[path = "it/ux.rs"]
mod ux;
#[path = "it/ux_m2_dialogs_find.rs"]
mod ux_m2_dialogs_find;
#[path = "it/ux_m2_polish_a11y.rs"]
mod ux_m2_polish_a11y;
#[path = "it/ux_m2_sidebar.rs"]
mod ux_m2_sidebar;
#[path = "it/ux_w1.rs"]
mod ux_w1;
#[path = "it/ux_w3.rs"]
mod ux_w3;
#[cfg(feature = "typst")]
#[path = "it/ux_w3_symbols.rs"]
mod ux_w3_symbols;
#[path = "it/uxfix.rs"]
mod uxfix;
#[path = "it/uxpolish.rs"]
mod uxpolish;
#[path = "it/viewmenu.rs"]
mod viewmenu;
#[path = "it/viewports.rs"]
mod viewports;
#[path = "it/watch_limit.rs"]
mod watch_limit;
#[path = "it/wiki_nonblock.rs"]
mod wiki_nonblock;
