//! Application directories.
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppDirs {
    pub config: PathBuf,
    pub cache: PathBuf,
}

impl AppDirs {
    /// `IRON_SHEETS_HOME=<dir>` gives `<dir>/config` and `<dir>/cache`; otherwise the platform dirs.
    pub fn discover() -> AppDirs {
        if let Some(home) = std::env::var_os("IRON_SHEETS_HOME") {
            if !home.is_empty() {
                return AppDirs::at(Path::new(&home));
            }
        }
        match directories::ProjectDirs::from("", "", "iron-sheets") {
            Some(d) => AppDirs { config: d.config_dir().to_path_buf(), cache: d.cache_dir().to_path_buf() },
            None => AppDirs::at(&std::env::temp_dir().join("iron-sheets")),
        }
    }

    /// Folder of untitled notes (`<config>/untitled`). Not created until a note needs it.
    pub fn untitled_dir(&self) -> PathBuf {
        self.config.join("untitled")
    }

    pub fn at(root: &Path) -> AppDirs {
        AppDirs { config: root.join("config"), cache: root.join("cache") }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_joins() {
        let d = AppDirs::at(Path::new("/x"));
        assert_eq!(d.config, Path::new("/x").join("config"));
        assert_eq!(d.cache, Path::new("/x").join("cache"));
    }
}
