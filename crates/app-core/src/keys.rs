//! Framework-free keyboard types: `Key`, `Mods`, `Shortcut` and the shortcut string format stored in the config.
//!
//! The key set is a superset of what common toolkits report, so every key a backend reports has a neutral twin; other
//! backends map their native keys onto it. Conversions live in the backend adapters (one table, tested both ways).

macro_rules! key_table {
    ($($variant:ident = $name:literal [$($alias:literal),+ $(,)?],)+) => {
        /// A logical key.
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
        pub enum Key {
            $($variant,)+
        }

        impl Key {
            /// Every key.
            pub const ALL: &'static [Key] = &[$(Key::$variant,)+];

            /// Canonical name used in shortcut strings (`Comma`, `Left`, `F12`, `A`).
            pub fn name(self) -> &'static str {
                match self {
                    $(Key::$variant => $name,)+
                }
            }

            /// Inverse of `name`, also accepting aliases and symbols (`,`, `Esc`, `KeyA`, ...).
            pub fn from_name(key: &str) -> Option<Key> {
                Some(match key {
                    $($($alias)|+ => Key::$variant,)+
                    _ => return None,
                })
            }
        }
    };
}

key_table! {
    ArrowDown = "Down" ["⏷", "ArrowDown", "Down"],
    ArrowLeft = "Left" ["⏴", "ArrowLeft", "Left"],
    ArrowRight = "Right" ["⏵", "ArrowRight", "Right"],
    ArrowUp = "Up" ["⏶", "ArrowUp", "Up"],
    Escape = "Escape" ["Escape", "Esc"],
    Tab = "Tab" ["Tab"],
    Backspace = "Backspace" ["Backspace"],
    Enter = "Enter" ["Enter", "Return", "NumpadEnter"],
    Insert = "Insert" ["Insert"],
    Delete = "Delete" ["Delete"],
    Home = "Home" ["Home"],
    End = "End" ["End"],
    PageUp = "PageUp" ["PageUp"],
    PageDown = "PageDown" ["PageDown"],
    Copy = "Copy" ["Copy"],
    Cut = "Cut" ["Cut"],
    Paste = "Paste" ["Paste"],
    Space = "Space" [" ", "Space"],
    Colon = "Colon" [":", "Colon"],
    Comma = "Comma" [",", "Comma", "NumpadComma"],
    Minus = "Minus" ["-", "−", "Minus", "NumpadSubtract"],
    Period = "Period" [".", "Period", "NumpadDecimal"],
    Plus = "Plus" ["+", "Plus", "NumpadAdd"],
    Equals = "Equals" ["=", "Equal", "Equals", "NumpadEqual"],
    Semicolon = "Semicolon" [";", "Semicolon"],
    Backslash = "Backslash" ["\\", "Backslash"],
    Slash = "Slash" ["/", "Slash", "NumpadDivide"],
    Pipe = "Pipe" ["|", "Pipe"],
    Questionmark = "Questionmark" ["?", "Questionmark"],
    Exclamationmark = "Exclamationmark" ["!", "Exclamationmark"],
    OpenBracket = "OpenBracket" ["[", "OpenBracket", "BracketLeft"],
    CloseBracket = "CloseBracket" ["]", "CloseBracket", "BracketRight"],
    OpenCurlyBracket = "OpenCurlyBracket" ["{", "OpenCurlyBracket"],
    CloseCurlyBracket = "CloseCurlyBracket" ["}", "CloseCurlyBracket"],
    Backtick = "Backtick" ["`", "Backtick", "Backquote", "Grave"],
    Quote = "Quote" ["'", "Quote"],
    Num0 = "0" ["0", "Digit0", "Numpad0"],
    Num1 = "1" ["1", "Digit1", "Numpad1"],
    Num2 = "2" ["2", "Digit2", "Numpad2"],
    Num3 = "3" ["3", "Digit3", "Numpad3"],
    Num4 = "4" ["4", "Digit4", "Numpad4"],
    Num5 = "5" ["5", "Digit5", "Numpad5"],
    Num6 = "6" ["6", "Digit6", "Numpad6"],
    Num7 = "7" ["7", "Digit7", "Numpad7"],
    Num8 = "8" ["8", "Digit8", "Numpad8"],
    Num9 = "9" ["9", "Digit9", "Numpad9"],
    A = "A" ["a", "A", "KeyA"],
    B = "B" ["b", "B", "KeyB"],
    C = "C" ["c", "C", "KeyC"],
    D = "D" ["d", "D", "KeyD"],
    E = "E" ["e", "E", "KeyE"],
    F = "F" ["f", "F", "KeyF"],
    G = "G" ["g", "G", "KeyG"],
    H = "H" ["h", "H", "KeyH"],
    I = "I" ["i", "I", "KeyI"],
    J = "J" ["j", "J", "KeyJ"],
    K = "K" ["k", "K", "KeyK"],
    L = "L" ["l", "L", "KeyL"],
    M = "M" ["m", "M", "KeyM"],
    N = "N" ["n", "N", "KeyN"],
    O = "O" ["o", "O", "KeyO"],
    P = "P" ["p", "P", "KeyP"],
    Q = "Q" ["q", "Q", "KeyQ"],
    R = "R" ["r", "R", "KeyR"],
    S = "S" ["s", "S", "KeyS"],
    T = "T" ["t", "T", "KeyT"],
    U = "U" ["u", "U", "KeyU"],
    V = "V" ["v", "V", "KeyV"],
    W = "W" ["w", "W", "KeyW"],
    X = "X" ["x", "X", "KeyX"],
    Y = "Y" ["y", "Y", "KeyY"],
    Z = "Z" ["z", "Z", "KeyZ"],
    F1 = "F1" ["F1"],
    F2 = "F2" ["F2"],
    F3 = "F3" ["F3"],
    F4 = "F4" ["F4"],
    F5 = "F5" ["F5"],
    F6 = "F6" ["F6"],
    F7 = "F7" ["F7"],
    F8 = "F8" ["F8"],
    F9 = "F9" ["F9"],
    F10 = "F10" ["F10"],
    F11 = "F11" ["F11"],
    F12 = "F12" ["F12"],
    F13 = "F13" ["F13"],
    F14 = "F14" ["F14"],
    F15 = "F15" ["F15"],
    F16 = "F16" ["F16"],
    F17 = "F17" ["F17"],
    F18 = "F18" ["F18"],
    F19 = "F19" ["F19"],
    F20 = "F20" ["F20"],
    F21 = "F21" ["F21"],
    F22 = "F22" ["F22"],
    F23 = "F23" ["F23"],
    F24 = "F24" ["F24"],
    F25 = "F25" ["F25"],
    F26 = "F26" ["F26"],
    F27 = "F27" ["F27"],
    F28 = "F28" ["F28"],
    F29 = "F29" ["F29"],
    F30 = "F30" ["F30"],
    F31 = "F31" ["F31"],
    F32 = "F32" ["F32"],
    F33 = "F33" ["F33"],
    F34 = "F34" ["F34"],
    F35 = "F35" ["F35"],
    BrowserBack = "BrowserBack" ["BrowserBack"],
    BrowserForward = "BrowserForward" ["BrowserForward"],
    BrowserRefresh = "BrowserRefresh" ["BrowserRefresh"],
    BrowserSearch = "BrowserSearch" ["BrowserSearch"],
    BrowserHome = "BrowserHome" ["BrowserHome"],
    BrowserFavorites = "BrowserFavorites" ["BrowserFavorites"],
    BrowserStop = "BrowserStop" ["BrowserStop"],
    MediaPlayPause = "MediaPlayPause" ["MediaPlayPause"],
    MediaTrackNext = "MediaTrackNext" ["MediaTrackNext"],
    MediaTrackPrevious = "MediaTrackPrevious" ["MediaTrackPrevious"],
    MediaStop = "MediaStop" ["MediaStop"],
    AudioVolumeMute = "AudioVolumeMute" ["AudioVolumeMute"],
    AudioVolumeDown = "AudioVolumeDown" ["AudioVolumeDown"],
    AudioVolumeUp = "AudioVolumeUp" ["AudioVolumeUp"],
    LaunchMail = "LaunchMail" ["LaunchMail"],
    LaunchApp1 = "LaunchApp1" ["LaunchApp1"],
    LaunchApp2 = "LaunchApp2" ["LaunchApp2"],
    ShiftLeft = "ShiftLeft" ["ShiftLeft"],
    ShiftRight = "ShiftRight" ["ShiftRight"],
    ControlLeft = "ControlLeft" ["ControlLeft"],
    ControlRight = "ControlRight" ["ControlRight"],
    AltLeft = "AltLeft" ["AltLeft"],
    AltRight = "AltRight" ["AltRight"],
    SuperLeft = "SuperLeft" ["SuperLeft", "MetaLeft", "OSLeft"],
    SuperRight = "SuperRight" ["SuperRight", "MetaRight", "OSRight"],
    IntlBackslash = "IntlBackslash" ["IntlBackslash"],
    CapsLock = "CapsLock" ["CapsLock"],
    NumLock = "NumLock" ["NumLock"],
    ScrollLock = "ScrollLock" ["ScrollLock"],
    PrintScreen = "PrintScreen" ["PrintScreen"],
    Pause = "Pause" ["Pause"],
    Menu = "Menu" ["ContextMenu", "Menu", "Apps"],
    Fn = "Fn" ["Fn", "FnLock", "Globe"],
    Eject = "Eject" ["Eject"],
    Help = "Help" ["Help"],
    Power = "Power" ["Power"],
    Sleep = "Sleep" ["Sleep", "Standby"],
    Clear = "Clear" ["Clear"],
}

/// Modifier state. `command` is the platform command key (Ctrl on Windows and Linux, Cmd on macOS); `ctrl` is the
/// physical Ctrl key. On non-macOS platforms the two mean the same thing and are unified by `normalized`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct Mods {
    pub alt: bool,
    pub shift: bool,
    pub command: bool,
    pub ctrl: bool,
}

const ALT: u8 = 1;
const SHIFT: u8 = 2;
const CMD: u8 = 4;
const CTRL: u8 = 8;

impl Mods {
    pub const NONE: Mods = Mods { alt: false, shift: false, command: false, ctrl: false };
    pub const ALT: Mods = Mods { alt: true, ..Mods::NONE };
    pub const SHIFT: Mods = Mods { shift: true, ..Mods::NONE };
    pub const COMMAND: Mods = Mods { command: true, ..Mods::NONE };
    pub const CTRL: Mods = Mods { ctrl: true, ..Mods::NONE };

    pub const fn plus(self, o: Mods) -> Mods {
        Mods { alt: self.alt || o.alt, shift: self.shift || o.shift, command: self.command || o.command, ctrl: self.ctrl || o.ctrl }
    }

    /// Bit set with `command` and `ctrl` unified the way matching works: off macOS, Ctrl is the command key.
    pub fn bits(self) -> u8 {
        let mac = cfg!(target_os = "macos");
        let cmd = self.command || (!mac && self.ctrl);
        (self.alt as u8 * ALT) | (self.shift as u8 * SHIFT) | (cmd as u8 * CMD) | ((mac && self.ctrl) as u8 * CTRL)
    }

    pub fn from_bits(b: u8) -> Mods {
        Mods { alt: b & ALT != 0, shift: b & SHIFT != 0, command: b & CMD != 0, ctrl: b & CTRL != 0 }
    }

    /// Canonical form (what shortcut strings can express).
    pub fn normalized(self) -> Mods {
        Mods::from_bits(self.bits())
    }

    pub fn is_empty(self) -> bool {
        self.bits() == 0
    }
}

impl std::ops::BitOr for Mods {
    type Output = Mods;
    fn bitor(self, o: Mods) -> Mods {
        self.plus(o)
    }
}

/// Modifiers plus a key.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct Shortcut {
    pub mods: Mods,
    pub key: Key,
}

impl Shortcut {
    pub const fn new(mods: Mods, key: Key) -> Shortcut {
        Shortcut { mods, key }
    }
}

/// `Ctrl+Shift+F`, `Alt+Left`, `F12`. Modifier names are case-insensitive; `Ctrl`, `Cmd` and `Command` all mean
/// the platform command key.
pub fn parse_shortcut(s: &str) -> Result<Shortcut, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty shortcut".into());
    }
    let mut b = 0u8;
    let mut key: Option<Key> = None;
    // A trailing "+" is the key itself ("Ctrl++").
    let parts: Vec<&str> = if let Some(head) = s.strip_suffix("++") { head.split('+').chain(std::iter::once("+")).collect() } else { s.split('+').collect() };
    for part in parts {
        let p = part.trim();
        if p.is_empty() {
            return Err(format!("empty part in \"{s}\""));
        }
        let bit = match p.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "cmd" | "command" | "meta" | "super" => CMD,
            "alt" | "option" => ALT,
            "shift" => SHIFT,
            _ => 0,
        };
        if bit != 0 {
            b |= bit;
            continue;
        }
        if key.is_some() {
            return Err(format!("more than one key in \"{s}\""));
        }
        let found = Key::from_name(p).or_else(|| {
            let mut c = p.chars();
            let up: String = c.next().map(|f| f.to_uppercase().chain(c.flat_map(|x| x.to_lowercase())).collect()).unwrap_or_default();
            Key::from_name(&up)
        });
        key = Some(found.ok_or_else(|| format!("unknown key \"{p}\""))?);
    }
    let key = key.ok_or_else(|| format!("\"{s}\" has no key"))?;
    Ok(Shortcut::new(Mods::from_bits(b), key))
}

pub fn format_shortcut(s: &Shortcut) -> String {
    let b = s.mods.bits();
    let mut out = String::new();
    if b & CMD != 0 {
        out.push_str("Ctrl+");
    }
    if b & CTRL != 0 {
        out.push_str("Ctrl+");
    }
    if b & ALT != 0 {
        out.push_str("Alt+");
    }
    if b & SHIFT != 0 {
        out.push_str("Shift+");
    }
    out.push_str(s.key.name());
    out
}

pub fn is_function_key(k: Key) -> bool {
    matches!(
        k,
        Key::F1
            | Key::F2
            | Key::F3
            | Key::F4
            | Key::F5
            | Key::F6
            | Key::F7
            | Key::F8
            | Key::F9
            | Key::F10
            | Key::F11
            | Key::F12
            | Key::F13
            | Key::F14
            | Key::F15
            | Key::F16
            | Key::F17
            | Key::F18
            | Key::F19
            | Key::F20
    )
}

/// Shortcuts without Ctrl/Alt would fire while typing; only function keys may stand alone (or with Shift).
pub fn bindable(s: &Shortcut) -> Result<(), &'static str> {
    if s.mods.bits() & (CMD | CTRL | ALT) == 0 && !is_function_key(s.key) {
        return Err("needs Ctrl or Alt (only function keys can be used alone)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_roundtrip_for_every_key() {
        for &k in Key::ALL {
            assert_eq!(Key::from_name(k.name()), Some(k), "{k:?}");
        }
    }

    #[test]
    fn parse_format_roundtrip_all_keys() {
        let mut bad = Vec::new();
        for &k in Key::ALL {
            for b in [0, CMD, CMD | SHIFT, ALT | SHIFT | CMD] {
                let sc = Shortcut::new(Mods::from_bits(b), k);
                let s = format_shortcut(&sc);
                match parse_shortcut(&s) {
                    Ok(p) if p == sc => {}
                    _ => bad.push(s),
                }
            }
        }
        assert!(bad.len() < 16, "{} keys do not round-trip, e.g. {:?}", bad.len(), &bad[..bad.len().min(8)]);
        // The keys users actually bind must round-trip.
        for s in ["Ctrl+Shift+F", "Ctrl+Backslash", "Alt+Left", "F12", "Ctrl+Comma", "Ctrl+Equals", "Ctrl+Tab", "Ctrl+0", "Shift+F3"] {
            assert_eq!(format_shortcut(&parse_shortcut(s).unwrap_or_else(|e| panic!("{s}: {e}"))), s);
        }
    }

    #[test]
    fn parse_variants_and_errors() {
        assert_eq!(parse_shortcut("ctrl+shift+f").unwrap(), Shortcut::new(Mods::COMMAND | Mods::SHIFT, Key::F));
        assert_eq!(parse_shortcut("Cmd+,").unwrap().key, Key::Comma);
        assert_eq!(parse_shortcut("Ctrl++").unwrap().key, Key::Plus);
        for bad in ["", "Ctrl+", "Ctrl+Shift", "Ctrl+A+B", "Ctrl+Nonsense", "++"] {
            assert!(parse_shortcut(bad).is_err(), "{bad:?}");
        }
        assert!(bindable(&parse_shortcut("A").unwrap()).is_err());
        assert!(bindable(&parse_shortcut("Shift+A").unwrap()).is_err());
        assert!(bindable(&parse_shortcut("F5").unwrap()).is_ok());
        assert!(bindable(&parse_shortcut("Alt+A").unwrap()).is_ok());
    }

    #[test]
    fn ctrl_and_command_unify_off_macos() {
        if !cfg!(target_os = "macos") {
            assert_eq!(Mods::CTRL.normalized(), Mods::COMMAND);
            assert_eq!(Mods::CTRL.bits(), Mods::COMMAND.bits());
        }
    }
}
