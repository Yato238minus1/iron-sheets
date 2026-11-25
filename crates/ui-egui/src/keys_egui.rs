//! egui <-> neutral conversions for keys, modifiers and shortcuts, plus the egui entry points of the keymap
//! (dispatching pressed shortcuts and recording a new binding).
use crate::keymap::{Captured, Keymap};
use crate::keys::{Key, Mods, Shortcut};

macro_rules! key_pairs {
    ($($v:ident),+ $(,)?) => {
        /// Neutral key for an egui key.
        pub fn key_from_egui(k: egui::Key) -> Key {
            match k {
                $(egui::Key::$v => Key::$v,)+
            }
        }

        /// egui key for a neutral key.
        pub fn key_to_egui(k: Key) -> egui::Key {
            match k {
                $(Key::$v => egui::Key::$v,)+
            }
        }
    };
}

key_pairs!(
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    Escape,
    Tab,
    Backspace,
    Enter,
    Insert,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Copy,
    Cut,
    Paste,
    Space,
    Colon,
    Comma,
    Minus,
    Period,
    Plus,
    Equals,
    Semicolon,
    Backslash,
    Slash,
    Pipe,
    Questionmark,
    Exclamationmark,
    OpenBracket,
    CloseBracket,
    OpenCurlyBracket,
    CloseCurlyBracket,
    Backtick,
    Quote,
    Num0,
    Num1,
    Num2,
    Num3,
    Num4,
    Num5,
    Num6,
    Num7,
    Num8,
    Num9,
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    F13,
    F14,
    F15,
    F16,
    F17,
    F18,
    F19,
    F20,
    F21,
    F22,
    F23,
    F24,
    F25,
    F26,
    F27,
    F28,
    F29,
    F30,
    F31,
    F32,
    F33,
    F34,
    F35,
    BrowserBack,
    BrowserForward,
    BrowserRefresh,
    BrowserSearch,
    BrowserHome,
    BrowserFavorites,
    BrowserStop,
    MediaPlayPause,
    MediaTrackNext,
    MediaTrackPrevious,
    MediaStop,
    AudioVolumeMute,
    AudioVolumeDown,
    AudioVolumeUp,
    LaunchMail,
    LaunchApp1,
    LaunchApp2,
    ShiftLeft,
    ShiftRight,
    ControlLeft,
    ControlRight,
    AltLeft,
    AltRight,
    SuperLeft,
    SuperRight,
    IntlBackslash,
    CapsLock,
    NumLock,
    ScrollLock,
    PrintScreen,
    Pause,
    Menu,
    Fn,
    Eject,
    Help,
    Power,
    Sleep,
    Clear
);

pub fn mods_from_egui(m: &egui::Modifiers) -> Mods {
    Mods { alt: m.alt, shift: m.shift, command: m.command || m.mac_cmd, ctrl: m.ctrl }
}

pub fn mods_to_egui(m: Mods) -> egui::Modifiers {
    egui::Modifiers { alt: m.alt, shift: m.shift, command: m.command, ctrl: m.ctrl, mac_cmd: false }
}

pub fn shortcut_to_egui(s: &Shortcut) -> egui::KeyboardShortcut {
    egui::KeyboardShortcut::new(mods_to_egui(s.mods), key_to_egui(s.key))
}

/// Pressed key events of this frame as neutral shortcuts (`mods` are the raw modifiers at press time).
fn presses(i: &egui::InputState) -> Vec<Shortcut> {
    i.events
        .iter()
        .filter_map(|e| {
            if let egui::Event::Key { key, pressed: true, modifiers, .. } = e {
                Some(Shortcut::new(mods_from_egui(modifiers), key_from_egui(*key)))
            } else {
                None
            }
        })
        .collect()
}

/// Consumes pressed global shortcuts and returns the matching `all_commands()` indices.
pub fn dispatch(km: &Keymap, ctx: &egui::Context) -> Vec<usize> {
    ctx.input_mut(|i| {
        if !i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. })) {
            return Vec::new();
        }
        let hits = km.dispatch(&presses(i));
        for h in &hits {
            i.consume_shortcut(&shortcut_to_egui(&h.binding));
        }
        // egui-winit also sends the typed character for Alt+key (it only drops it for Ctrl/Cmd): Alt+[ must not type "[".
        if hits.iter().any(|h| h.binding.mods.alt && !h.binding.mods.command && !h.binding.mods.ctrl) {
            i.events.retain(|e| !matches!(e, egui::Event::Text(_)));
        }
        hits.into_iter().map(|h| h.command).collect()
    })
}

/// Editor-scope shortcut check (consumes the key when pressed).
pub fn editor_pressed(km: &Keymap, input: &mut egui::InputState, id: &str) -> bool {
    km.editor_bindings(id).iter().any(|s| input.consume_shortcut(&shortcut_to_egui(s)))
}

/// While recording a binding: the next key press, consumed.
pub fn capture(ctx: &egui::Context) -> Captured {
    ctx.input_mut(|i| {
        let found = i.events.iter().find_map(|e| if let egui::Event::Key { key, pressed: true, modifiers, .. } = e { Some((*key, *modifiers)) } else { None });
        let Some((key, m)) = found else { return Captured::Nothing };
        let captured = Captured::from_press(Shortcut::new(mods_from_egui(&m), key_from_egui(key)));
        i.consume_key(m, key);
        captured
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_roundtrips_through_egui() {
        assert_eq!(Key::ALL.len(), egui::Key::ALL.len(), "key sets differ in size");
        for &k in Key::ALL {
            assert_eq!(key_from_egui(key_to_egui(k)), k, "{k:?}");
        }
        for &k in egui::Key::ALL {
            assert_eq!(key_to_egui(key_from_egui(k)), k, "{k:?}");
            // Names agree with egui's own, so stored shortcut strings are unchanged.
            assert_eq!(key_from_egui(k).name(), k.name());
        }
    }

    #[test]
    fn names_match_egui_from_name() {
        for s in ["Esc", "KeyA", "Digit3", "NumpadEnter", "⏴", "−", "Backquote", "OSLeft"] {
            assert_eq!(egui::Key::from_name(s).map(key_from_egui), Key::from_name(s), "{s}");
        }
    }

    fn press(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    /// Runs one frame with `events`, then asks `editor_pressed(id)`.
    fn pressed(id: &str, events: Vec<egui::Event>) -> bool {
        let km = Keymap::defaults();
        let ctx = egui::Context::default();
        let mut hit = false;
        let mut out = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| hit = ui.input_mut(|i| editor_pressed(&km, i, id)));
        out.textures_delta.clear();
        hit
    }

    #[test]
    fn alt_editor_keys_ignore_altgr_and_other_modifiers() {
        let alt = egui::Modifiers::ALT;
        // AltGr arrives as Ctrl+Alt (command and ctrl set): Alt-only bindings must not fire.
        let altgr = egui::Modifiers { alt: true, ctrl: true, command: true, ..Default::default() };
        assert!(pressed("editor.copy", vec![press(egui::Key::C, alt)]));
        assert!(pressed("editor.cut", vec![press(egui::Key::X, alt)]));
        assert!(!pressed("editor.copy", vec![press(egui::Key::C, altgr)]));
        assert!(!pressed("editor.cut", vec![press(egui::Key::X, altgr)]));
        assert!(!pressed("editor.copy", vec![press(egui::Key::C, egui::Modifiers::NONE)]));
        // Alt+Shift+Up is the copy key; egui ignores an extra Shift, so features.rs checks it before Alt+Up.
        let alt_shift = egui::Modifiers::ALT | egui::Modifiers::SHIFT;
        assert!(pressed("editor.dup_up", vec![press(egui::Key::ArrowUp, alt_shift)]));
        assert!(!pressed("editor.dup_up", vec![press(egui::Key::ArrowUp, alt)]));
        assert!(pressed("editor.line_up", vec![press(egui::Key::ArrowUp, alt)]));
    }

    #[test]
    fn mods_roundtrip() {
        for b in 0u8..16 {
            let m = Mods::from_bits(b);
            let e = mods_to_egui(m);
            assert_eq!((e.alt, e.shift, e.command, e.ctrl), (m.alt, m.shift, m.command, m.ctrl));
            assert_eq!(mods_from_egui(&e), m);
        }
    }
}
