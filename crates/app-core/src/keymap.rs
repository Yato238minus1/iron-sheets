//! Keymap: shortcut strings, the built-in defaults plus user overrides, and per-frame dispatch.
//!
//! Dispatch cost: with no key pressed one pass over the (usually empty) event list; with a key
//! pressed at most four hash lookups, most specific modifier combination first.
use crate::command::{CommandInfo, all_commands};
use crate::keys::{Key, Mods, Shortcut};
pub use crate::keys::{bindable, format_shortcut, parse_shortcut};
use rustc_hash::FxHashMap;
use std::collections::BTreeMap;

/// Editor-internal actions: handled by the editor, but rebindable and listed like commands.
pub struct EditorKey {
    pub id: &'static str,
    pub title: &'static str,
    pub default: Shortcut,
    /// Optional second default.
    pub extra: Option<Shortcut>,
}

const C: Mods = Mods::COMMAND;
pub static EDITOR_KEYS: &[EditorKey] = &[
    EditorKey { id: "editor.bold", title: "Editor: bold", default: Shortcut::new(C.plus(Mods::SHIFT), Key::B), extra: None },
    EditorKey { id: "editor.italic", title: "Editor: italic", default: Shortcut::new(C, Key::I), extra: None },
    EditorKey { id: "editor.link", title: "Editor: insert link", default: Shortcut::new(C, Key::K), extra: None },
    EditorKey { id: "editor.code", title: "Editor: inline code", default: Shortcut::new(C, Key::Backtick), extra: None },
    EditorKey { id: "editor.line_up", title: "Editor: move line up", default: Shortcut::new(C.plus(Mods::SHIFT), Key::ArrowUp), extra: Some(Shortcut::new(Mods::ALT, Key::ArrowUp)) },
    EditorKey { id: "editor.line_down", title: "Editor: move line down", default: Shortcut::new(C.plus(Mods::SHIFT), Key::ArrowDown), extra: Some(Shortcut::new(Mods::ALT, Key::ArrowDown)) },
    EditorKey { id: "editor.dup_up", title: "Editor: copy line up", default: Shortcut::new(Mods::ALT.plus(Mods::SHIFT), Key::ArrowUp), extra: None },
    EditorKey { id: "editor.dup_down", title: "Editor: copy line down", default: Shortcut::new(Mods::ALT.plus(Mods::SHIFT), Key::ArrowDown), extra: None },
    EditorKey { id: "editor.cut", title: "Editor: cut selection (Alt)", default: Shortcut::new(Mods::ALT, Key::X), extra: None },
    EditorKey { id: "editor.copy", title: "Editor: copy selection (Alt)", default: Shortcut::new(Mods::ALT, Key::C), extra: None },
    EditorKey { id: "editor.find_next", title: "Editor: find next", default: Shortcut::new(Mods::NONE, Key::F3), extra: None },
    EditorKey { id: "editor.replace", title: "Editor: find and replace", default: Shortcut::new(C, Key::H), extra: None },
    EditorKey { id: "editor.replace_all", title: "Find bar: replace all", default: Shortcut::new(C.plus(Mods::ALT), Key::Enter), extra: None },
    EditorKey { id: "editor.find_prev", title: "Editor: find previous", default: Shortcut::new(Mods::SHIFT, Key::F3), extra: None },
];

/// Keys that stay fixed (documented in the settings UI).
pub const FIXED_KEYS: &[(&str, &str)] = &[
    ("Enter", "Continue list / quote, or accept autocomplete"),
    ("Enter / Shift+Enter in the replace field", "Replace and go to the next match / go to the previous match"),
    ("Tab / Shift+Tab", "Indent / outdent list item"),
    ("Ctrl+click", "Follow link under the pointer (Shift flips the new-tab choice)"),
];

pub fn editor_index(id: &str) -> Option<usize> {
    EDITOR_KEYS.iter().position(|e| e.id == id)
}

// ---- keymap ----

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    /// Index into `all_commands()`.
    Command(usize),
    /// Index into `EDITOR_KEYS`.
    Editor(usize),
}

/// Up to two default shortcuts of an entry (`Copy`, so it can be read out of a shared `Entry`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Defaults {
    items: [Shortcut; 2],
    len: u8,
}

impl Defaults {
    pub fn new(first: Option<Shortcut>, second: Option<Shortcut>) -> Defaults {
        let filler = Shortcut::new(Mods::NONE, Key::Escape);
        match (first, second) {
            (Some(a), Some(b)) => Defaults { items: [a, b], len: 2 },
            (Some(a), None) | (None, Some(a)) => Defaults { items: [a, filler], len: 1 },
            (None, None) => Defaults { items: [filler, filler], len: 0 },
        }
    }

    pub fn as_slice(&self) -> &[Shortcut] {
        &self.items[..self.len as usize]
    }
}

impl IntoIterator for Defaults {
    type Item = Shortcut;
    type IntoIter = std::iter::Take<std::array::IntoIter<Shortcut, 2>>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter().take(self.len as usize)
    }
}

pub struct Entry {
    pub id: &'static str,
    pub title: &'static str,
    pub scope: Scope,
    pub default: Defaults,
}

pub struct Keymap {
    pub entries: Vec<Entry>,
    /// Effective bindings per entry (parallel to `entries`).
    pub bindings: Vec<Vec<Shortcut>>,
    lookup: FxHashMap<(u8, Key), (usize, Shortcut)>,
    /// Display text of the first binding per entry.
    hints: Vec<String>,
    /// Groups of entry indices sharing a shortcut.
    pub conflicts: Vec<(String, Vec<usize>)>,
    /// Messages for override strings that could not be used.
    pub warnings: Vec<String>,
}

impl Keymap {
    pub fn defaults() -> Keymap {
        Self::build(&BTreeMap::new())
    }

    pub fn build(overrides: &BTreeMap<String, Vec<String>>) -> Keymap {
        let mut entries: Vec<Entry> = all_commands()
            .iter()
            .enumerate()
            .map(|(i, c): (usize, &CommandInfo)| Entry {
                id: c.id,
                title: c.title,
                scope: Scope::Command(i),
                default: Defaults::new(c.shortcut, c.extra_shortcut),
            })
            .collect();
        entries.extend(EDITOR_KEYS.iter().enumerate().map(|(i, e)| Entry {
            id: e.id,
            title: e.title,
            scope: Scope::Editor(i),
            default: Defaults::new(Some(e.default), e.extra),
        }));
        let mut warnings = Vec::new();
        let mut bindings: Vec<Vec<Shortcut>> = Vec::with_capacity(entries.len());
        for e in &entries {
            let b = match overrides.get(e.id) {
                Some(list) => {
                    let mut v = Vec::new();
                    for s in list {
                        match parse_shortcut(s).and_then(|k| bindable(&k).map(|_| k).map_err(String::from)) {
                            Ok(k) if !v.contains(&k) => v.push(k),
                            Ok(_) => {}
                            Err(m) => warnings.push(format!("{}: {m}", e.id)),
                        }
                    }
                    v
                }
                None => e.default.into_iter().collect(),
            };
            bindings.push(b);
        }
        for k in overrides.keys() {
            if !entries.iter().any(|e| e.id == k) {
                warnings.push(format!("unknown command \"{k}\" in keybindings"));
            }
        }
        let mut lookup: FxHashMap<(u8, Key), (usize, Shortcut)> = FxHashMap::default();
        let mut by_shortcut: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, bs) in bindings.iter().enumerate() {
            for s in bs {
                let g = by_shortcut.entry(format_shortcut(s)).or_default();
                if !g.contains(&i) {
                    g.push(i);
                }
                // Editor entries too: they stop an extra-Shift fallback (Ctrl+Shift+B must not hit Ctrl+B).
                lookup.entry((s.mods.bits(), s.key)).or_insert((i, *s));
            }
        }
        let conflicts = by_shortcut.into_iter().filter(|(_, v)| v.len() > 1).collect();
        let hints = bindings.iter().map(|b| b.first().map(format_shortcut).unwrap_or_default()).collect();
        Keymap { entries, bindings, lookup, hints, conflicts, warnings }
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.id == id)
    }

    /// Display text for the first shortcut of `all_commands()[cmd_index]`.
    pub fn hint(&self, cmd_index: usize) -> &str {
        self.hints.get(cmd_index).map_or("", String::as_str)
    }

    pub fn is_default(&self, entry: usize) -> bool {
        self.bindings[entry].as_slice() == self.entries[entry].default.as_slice()
    }

    pub fn in_conflict(&self, entry: usize) -> bool {
        self.conflicts.iter().any(|(_, v)| v.contains(&entry))
    }

    /// Other entries bound to the same shortcut as `s`.
    pub fn others_with(&self, s: &Shortcut, except: usize) -> Vec<usize> {
        let b = s.mods.bits();
        (0..self.entries.len()).filter(|&i| i != except && self.bindings[i].iter().any(|x| x.mods.bits() == b && x.key == s.key)).collect()
    }

    /// Effective bindings of the editor-scope action `id` (empty when unknown).
    pub fn editor_bindings(&self, id: &str) -> &[Shortcut] {
        match editor_index(id).map(|i| all_commands().len() + i) {
            Some(i) => &self.bindings[i],
            None => &[],
        }
    }

    /// The global command bound to a key press (`mods` are the raw modifiers at press time), if any. Most specific
    /// modifier combination first: extra Shift/Alt do not prevent a match.
    pub fn lookup(&self, key: Key, mods: Mods) -> Option<Hit> {
        let b = mods.bits();
        let cmd = b & (CMD | CTRL);
        // Windows reports AltGr as Ctrl+Alt (it types `}`, `~`, `@` on German layouts): there, never fall back to a
        // binding without Alt. Elsewhere an extra Alt still matches (Ctrl+Alt+F finds).
        let altgr = cfg!(windows) && b & CMD != 0 && b & ALT != 0;
        for extra in [ALT | SHIFT, ALT, SHIFT, 0] {
            if extra & !b != 0 || (altgr && extra & ALT == 0) {
                continue;
            }
            if let Some((idx, sc)) = self.lookup.get(&(extra | cmd, key)).copied() {
                if let Scope::Command(c) = self.entries[idx].scope {
                    return Some(Hit { command: c, binding: sc });
                }
                return None;
            }
        }
        None
    }

    /// Global shortcuts matched by the pressed keys (`mods` raw). With nothing pressed this is a single empty-slice check.
    pub fn dispatch(&self, presses: &[Shortcut]) -> Vec<Hit> {
        presses.iter().filter_map(|p| self.lookup(p.key, p.mods)).collect()
    }
}

const ALT: u8 = 1;
const SHIFT: u8 = 2;
const CMD: u8 = 4;
const CTRL: u8 = 8;

/// A matched global shortcut: the `all_commands()` index and the binding that matched (backends consume its key).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub command: usize,
    pub binding: Shortcut,
}

/// While recording a binding: the next key press, consumed.
pub enum Captured {
    Nothing,
    Cancelled,
    Key(Shortcut),
}

impl Captured {
    /// Interpret a key press (`mods` raw) while recording: plain Escape cancels.
    pub fn from_press(press: Shortcut) -> Captured {
        let sc = Shortcut::new(press.mods.normalized(), press.key);
        if press.key == Key::Escape && sc.mods.is_empty() { Captured::Cancelled } else { Captured::Key(sc) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_have_no_conflicts_or_warnings() {
        let k = Keymap::defaults();
        assert!(k.conflicts.is_empty(), "{:?}", k.conflicts);
        assert!(k.warnings.is_empty());
        let mut ids: Vec<_> = k.entries.iter().map(|e| e.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), k.entries.len(), "duplicate command ids");
    }

    #[test]
    fn overrides_conflicts_and_bad_strings() {
        let mut o = BTreeMap::new();
        o.insert("file.save".to_string(), vec!["Ctrl+Q".to_string(), "bogus+".to_string()]);
        o.insert("file.save_all".to_string(), vec!["Ctrl+Q".to_string()]);
        o.insert("nope".to_string(), vec![]);
        o.insert("view.perf".to_string(), vec![]);
        let k = Keymap::build(&o);
        assert_eq!(k.conflicts.len(), 1);
        assert_eq!(k.conflicts[0].0, "Ctrl+Q");
        assert_eq!(k.warnings.len(), 2);
        let perf = k.index_of("view.perf").unwrap();
        assert!(k.bindings[perf].is_empty() && !k.is_default(perf));
        assert!(!k.lookup.contains_key(&(CMD, Key::S)));
    }

    #[test]
    fn every_default_binding_roundtrips_through_its_string() {
        let k = Keymap::defaults();
        let mut n = 0;
        for (i, e) in k.entries.iter().enumerate() {
            for b in &k.bindings[i] {
                let text = format_shortcut(b);
                assert_eq!(parse_shortcut(&text).as_ref(), Ok(&Shortcut::new(b.mods.normalized(), b.key)), "{} {text}", e.id);
                assert!(bindable(b).is_ok(), "{} {text} not bindable", e.id);
                n += 1;
            }
        }
        assert!(n >= 20, "{n} default bindings");
        let save = k.index_of("file.save").unwrap();
        assert_eq!(k.hint(save), "Ctrl+S");
    }

    fn press(key: Key, mods: Mods) -> Shortcut {
        Shortcut::new(mods, key)
    }

    #[test]
    fn lookup_finds_commands_most_specific_first() {
        let k = Keymap::defaults();
        let idx = |id: &str| all_commands().iter().position(|c| c.id == id).unwrap();
        // Ctrl+S saves, also when the platform reports both ctrl and command.
        assert_eq!(k.lookup(Key::S, Mods::COMMAND).map(|h| h.command), Some(idx("file.save")));
        assert_eq!(k.lookup(Key::S, Mods::COMMAND | Mods::CTRL).map(|h| h.command), Some(idx("file.save")));
        // Ctrl+Shift+F is search, Ctrl+F is find; extra Alt does not break a match.
        assert_eq!(k.lookup(Key::F, Mods::COMMAND | Mods::SHIFT).map(|h| h.command), Some(idx("edit.search")));
        assert_eq!(k.lookup(Key::F, Mods::COMMAND).map(|h| h.command), Some(idx("edit.find")));
        assert_eq!(k.lookup(Key::F, Mods::COMMAND | Mods::ALT).map(|h| h.command), Some(idx("edit.find")));
        assert_eq!(k.lookup(Key::F12, Mods::NONE).map(|h| h.command), Some(idx("view.perf")));
        // Unbound, plain letters and editor-scope keys do not dispatch globally.
        assert!(k.lookup(Key::S, Mods::NONE).is_none());
        assert!(k.lookup(Key::B, Mods::COMMAND | Mods::SHIFT).is_none());
        // Ctrl+B and Ctrl+\ toggle the sidebar (bold is Ctrl+Shift+B); F2 renames.
        assert_eq!(k.lookup(Key::B, Mods::COMMAND).map(|h| h.command), Some(idx("view.sidebar")));
        assert_eq!(k.lookup(Key::Backslash, Mods::COMMAND).map(|h| h.command), Some(idx("view.sidebar")));
        assert_eq!(k.lookup(Key::F2, Mods::NONE).map(|h| h.command), Some(idx("file.rename")));
        assert!(k.dispatch(&[]).is_empty());
        let hits = k.dispatch(&[press(Key::S, Mods::COMMAND), press(Key::N, Mods::COMMAND)]);
        assert_eq!(hits.iter().map(|h| h.command).collect::<Vec<_>>(), vec![idx("file.save"), idx("file.new_note")]);
        assert_eq!(hits[0].binding, press(Key::S, Mods::COMMAND));
    }

    #[test]
    fn zoom_defaults_on_german_and_us_keyboards() {
        let k = Keymap::defaults();
        let idx = |id: &str| all_commands().iter().position(|c| c.id == id).unwrap();
        let (zin, zout, zres) = (idx("view.zoom_in"), idx("view.zoom_out"), idx("view.zoom_reset"));
        // German layout: "+" is its own key, so Ctrl+Plus zooms in. US layout: "+" is Shift+=, so Ctrl+Shift+Plus
        // arrives as Plus with Shift, and a plain Ctrl+= (no shift) zooms in as well.
        assert_eq!(k.lookup(Key::Plus, Mods::COMMAND).map(|h| h.command), Some(zin));
        assert_eq!(k.lookup(Key::Plus, Mods::COMMAND | Mods::SHIFT).map(|h| h.command), Some(zin));
        assert_eq!(k.lookup(Key::Equals, Mods::COMMAND).map(|h| h.command), Some(zin));
        assert_eq!(k.lookup(Key::Equals, Mods::COMMAND | Mods::SHIFT).map(|h| h.command), Some(zin));
        assert_eq!(k.lookup(Key::Equals, Mods::CTRL | Mods::SHIFT).map(|h| h.command), Some(zin));
        assert_eq!(k.lookup(Key::Minus, Mods::COMMAND).map(|h| h.command), Some(zout));
        // Ctrl+0 resets zoom and font sizes; Ctrl+= no longer does.
        assert_eq!(k.lookup(Key::Num0, Mods::COMMAND).map(|h| h.command), Some(zres));
        // The matching binding is what gets consumed from the input.
        assert_eq!(k.lookup(Key::Equals, Mods::COMMAND | Mods::SHIFT).map(|h| h.binding), Some(press(Key::Equals, Mods::COMMAND)));
        assert_eq!(k.bindings[zin], vec![press(Key::Plus, Mods::COMMAND), press(Key::Equals, Mods::COMMAND)]);
        assert_eq!(k.bindings[zres], vec![press(Key::Num0, Mods::COMMAND)]);
        assert!(k.is_default(zin) && k.is_default(zres));
        assert_eq!(k.hint(zres), "Ctrl+0");
        // AltGr arrives as Ctrl+Alt on Windows: AltGr+0 types `}` and AltGr+Plus types `~`; neither may zoom there.
        let altgr0 = k.lookup(Key::Num0, Mods::CTRL | Mods::ALT).map(|h| h.command);
        assert_eq!(altgr0, if cfg!(windows) { None } else { Some(zres) });
        assert_eq!(k.hint(zin), "Ctrl+Plus");
        assert!(k.conflicts.is_empty());
    }

    #[test]
    fn ctrl_t_opens_a_new_tab_and_ctrl_shift_t_reopens() {
        let k = Keymap::defaults();
        let idx = |id: &str| all_commands().iter().position(|c| c.id == id).unwrap();
        assert_eq!(k.lookup(Key::T, Mods::COMMAND).map(|h| h.command), Some(idx("tab.new")));
        assert_eq!(k.lookup(Key::T, Mods::COMMAND | Mods::SHIFT).map(|h| h.command), Some(idx("tab.reopen")));
    }

    #[test]
    fn editor_bindings_and_others_with() {
        let k = Keymap::defaults();
        assert_eq!(k.editor_bindings("editor.bold"), &[press(Key::B, Mods::COMMAND | Mods::SHIFT)]);
        assert!(k.editor_bindings("nope").is_empty());
        let save = k.index_of("file.save").unwrap();
        let bold = k.index_of("editor.bold").unwrap();
        assert_eq!(k.others_with(&press(Key::B, Mods::COMMAND | Mods::SHIFT), save), vec![bold]);
        assert!(k.others_with(&press(Key::B, Mods::COMMAND | Mods::SHIFT), bold).is_empty());
    }

    #[test]
    fn alt_editor_keys_have_defaults_and_never_reach_global_commands() {
        let k = Keymap::defaults();
        assert!(k.conflicts.is_empty(), "{:?}", k.conflicts);
        let (a, as_) = (Mods::ALT, Mods::ALT | Mods::SHIFT);
        assert_eq!(k.editor_bindings("editor.line_up"), &[press(Key::ArrowUp, Mods::COMMAND | Mods::SHIFT), press(Key::ArrowUp, a)]);
        assert_eq!(k.editor_bindings("editor.line_down"), &[press(Key::ArrowDown, Mods::COMMAND | Mods::SHIFT), press(Key::ArrowDown, a)]);
        assert_eq!(k.editor_bindings("editor.dup_up"), &[press(Key::ArrowUp, as_)]);
        assert_eq!(k.editor_bindings("editor.dup_down"), &[press(Key::ArrowDown, as_)]);
        assert_eq!(k.editor_bindings("editor.cut"), &[press(Key::X, a)]);
        assert_eq!(k.editor_bindings("editor.copy"), &[press(Key::C, a)]);
        // The editor entries stop the extra-modifier fallback: no global command fires, also with an extra Shift/Alt.
        for key in [Key::ArrowUp, Key::ArrowDown, Key::X, Key::C] {
            for m in [a, as_] {
                assert!(k.lookup(key, m).is_none(), "{key:?} {m:?}");
            }
        }
        // Alt+Left/Right and Alt+Z keep their global meaning.
        let idx = |id: &str| all_commands().iter().position(|c| c.id == id).unwrap();
        assert_eq!(k.lookup(Key::ArrowLeft, a).map(|h| h.command), Some(idx("nav.back")));
        assert_eq!(k.lookup(Key::Z, a).map(|h| h.command), Some(idx("view.wrap")));
    }

    #[test]
    fn capture_cancels_on_plain_escape_only() {
        assert!(matches!(Captured::from_press(press(Key::Escape, Mods::NONE)), Captured::Cancelled));
        assert!(matches!(Captured::from_press(press(Key::Escape, Mods::SHIFT)), Captured::Key(_)));
        assert!(matches!(Captured::from_press(press(Key::K, Mods::COMMAND)), Captured::Key(s) if s == press(Key::K, Mods::COMMAND)));
    }
}
