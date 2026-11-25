//! User themes: TOML files in `<config>/themes/*.toml` plus two built-in ones. A theme is a base variant and colour
//! overrides keyed like `AppConfig::colors_dark` (`accent`, `editor_bg`, ...); unknown keys and bad values are ignored
//! where the colours are applied. The id of a theme is its file name without `.toml`.
use crate::paths::AppDirs;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub struct ThemeFile {
    pub id: String,
    pub name: String,
    pub dark: bool,
    pub colors: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Raw {
    name: Option<String>,
    base: Option<String>,
    #[serde(default)]
    colors: BTreeMap<String, String>,
}

/// (id, file text) of the themes compiled into the binary; a file with the same id wins.
pub const BUILTIN: [(&str, &str); 2] = [
    (
        "high-contrast-dark",
        "name = \"High contrast dark\"\nbase = \"dark\"\n\n[colors]\naccent = \"#1f4e8c\"\neditor_bg = \"#000000\"\neditor_text = \"#ffffff\"\nlink = \"#6cb6ff\"\nheading = \"#ffd866\"\nemphasis = \"#ff9de2\"\nstrong = \"#ffb86c\"\ncode = \"#ff8fa3\"\nquote = \"#b8e0b8\"\nmarker = \"#a0a0a0\"\ncomment = \"#a8d8a8\"\nkeyword = \"#e0a0ff\"\nstring = \"#9fe8a8\"\nnumber = \"#ffc070\"\ntag = \"#80e8e8\"\nmath = \"#ffd080\"\n",
    ),
    (
        "sepia",
        "name = \"Sepia\"\nbase = \"light\"\n\n[colors]\naccent = \"#e0c890\"\neditor_bg = \"#f4ecd8\"\neditor_text = \"#3b2f20\"\nlink = \"#0b4a8f\"\nheading = \"#7a3e00\"\nemphasis = \"#7a3a6a\"\nstrong = \"#8a3b00\"\ncode = \"#9a2a44\"\nquote = \"#4f6a4f\"\nmarker = \"#7a6f60\"\ncomment = \"#4f6a4f\"\nkeyword = \"#8a2a8a\"\nstring = \"#1a6a2a\"\nnumber = \"#9a4a00\"\ntag = \"#006a6a\"\nmath = \"#7a4a00\"\n",
    ),
];

pub fn themes_dir(dirs: &AppDirs) -> PathBuf {
    dirs.config.join("themes")
}

/// Parse one theme file. The error names the file and carries the parser's line information.
pub fn parse(id: &str, text: &str) -> Result<ThemeFile, String> {
    let raw: Raw = toml::from_str(text).map_err(|e| format!("{id}.toml: {}", e.to_string().lines().next().unwrap_or("invalid")))?;
    let dark = match raw.base.as_deref().unwrap_or("dark") {
        "dark" => true,
        "light" => false,
        other => return Err(format!("{id}.toml: base must be \"dark\" or \"light\", not \"{other}\"")),
    };
    Ok(ThemeFile { id: id.to_string(), name: raw.name.unwrap_or_else(|| id.to_string()), dark, colors: raw.colors })
}

/// The theme `id`: its file in the themes folder (one file is read), else the built-in of that id.
pub fn load(dirs: &AppDirs, id: &str) -> Result<ThemeFile, String> {
    // An id is a file stem: never a path.
    let plain = !id.is_empty() && !id.contains(['/', '\\']) && id != "." && id != "..";
    if plain {
        match std::fs::read_to_string(themes_dir(dirs).join(format!("{id}.toml"))) {
            Ok(text) => return parse(id, &text),
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(format!("{id}.toml: {e}")),
            Err(_) => {}
        }
    }
    match BUILTIN.iter().find(|(b, _)| *b == id) {
        Some((b, text)) => parse(b, text),
        None => Err(format!("theme \"{id}\" not found")),
    }
}

/// (id, display name) of every theme: built-ins, then the files of the themes folder (read now). A file that does not
/// parse is listed under its id.
pub fn list(dirs: &AppDirs) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = BUILTIN.iter().filter_map(|(id, t)| parse(id, t).ok()).map(|t| (t.id, t.name)).collect();
    if let Ok(rd) = std::fs::read_dir(themes_dir(dirs)) {
        let mut files: Vec<(String, String)> = rd
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                let id = p.file_stem()?.to_str()?.to_string();
                (p.extension()? == "toml").then(|| {
                    let name = std::fs::read_to_string(&p).ok().and_then(|t| parse(&id, &t).ok()).map_or_else(|| id.clone(), |t| t.name);
                    (id, name)
                })
            })
            .collect();
        files.sort();
        for f in files {
            match out.iter_mut().find(|(id, _)| *id == f.0) {
                Some(slot) => *slot = f,
                None => out.push(f),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_base_name_and_colors() {
        let t = parse("x", "name = \"X\"\nbase = \"light\"\n[colors]\naccent = \"#112233\"\n").unwrap();
        assert!(!t.dark);
        assert_eq!((t.name.as_str(), t.colors["accent"].as_str()), ("X", "#112233"));
        let d = parse("y", "").unwrap();
        assert!(d.dark && d.name == "y" && d.colors.is_empty(), "everything is optional");
    }

    #[test]
    fn errors_name_file_and_line() {
        let e = parse("bad", "name = \"x\"\nbase = = 1\n").unwrap_err();
        assert!(e.starts_with("bad.toml: ") && e.contains("line 2"), "{e}");
        assert!(parse("b", "base = \"blue\"").unwrap_err().contains("base must be"));
    }

    #[test]
    fn builtins_parse_and_files_win_and_list() {
        for (id, t) in BUILTIN {
            assert!(parse(id, t).is_ok(), "{id}");
        }
        let tmp = tempfile::tempdir().unwrap();
        let dirs = AppDirs::at(tmp.path());
        assert_eq!(load(&dirs, "sepia").unwrap().name, "Sepia");
        assert!(load(&dirs, "nope").is_err() && load(&dirs, "../x").is_err());
        std::fs::create_dir_all(themes_dir(&dirs)).unwrap();
        std::fs::write(themes_dir(&dirs).join("sepia.toml"), "name = \"My sepia\"").unwrap();
        std::fs::write(themes_dir(&dirs).join("mine.toml"), "base = \"light\"").unwrap();
        std::fs::write(themes_dir(&dirs).join("broken.toml"), "= =").unwrap();
        std::fs::write(themes_dir(&dirs).join("readme.txt"), "x").unwrap();
        assert_eq!(load(&dirs, "sepia").unwrap().name, "My sepia");
        let ids: Vec<_> = list(&dirs).into_iter().map(|(i, _)| i).collect();
        assert_eq!(ids, ["high-contrast-dark", "sepia", "broken", "mine"]);
    }
}
