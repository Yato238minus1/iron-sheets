//! The config the UI renders with: the stored config, with the chosen theme file merged in, or, in professional mode,
//! with every cosmetic customisation replaced by its default. The stored config is never changed.
use iron_sheets_core::config::AppConfig;
use iron_sheets_core::themes::ThemeFile;
use std::borrow::Cow;

/// Borrowed (no copy) unless a theme file is chosen or professional mode is on.
pub fn effective<'a>(c: &'a AppConfig, theme: Option<&ThemeFile>) -> Cow<'a, AppConfig> {
    if c.professional {
        let d = AppConfig::default();
        let mut e = c.clone();
        e.theme = d.theme;
        e.theme_file = String::new();
        (e.editor_font_size, e.preview_font_size, e.ui_font_size) = (d.editor_font_size, d.preview_font_size, d.ui_font_size);
        (e.editor_font, e.preview_font, e.editor_font_file, e.preview_font_file) = (d.editor_font, d.preview_font, d.editor_font_file, d.preview_font_file);
        (e.colors_dark, e.colors_light) = (d.colors_dark, d.colors_light);
        e.caret_trail = false;
        (e.newtab_quotes, e.newtab_image) = (false, String::new());
        return Cow::Owned(e);
    }
    let Some(t) = theme else { return Cow::Borrowed(c) };
    let mut e = c.clone();
    e.theme = if t.dark { iron_sheets_core::config::ThemePref::Dark } else { iron_sheets_core::config::ThemePref::Light };
    // The user's own overrides stay on top of the theme file's.
    let map = if t.dark { &mut e.colors_dark } else { &mut e.colors_light };
    for (k, v) in &t.colors {
        map.entry(k.clone()).or_insert_with(|| v.clone());
    }
    Cow::Owned(e)
}

/// `effective` computed again only when the keys change (config edits, settings bumps), so no frame clones the config.
#[derive(Default)]
pub struct EffCache {
    key: (u64, u64),
    cfg: Option<AppConfig>,
}

impl EffCache {
    pub fn get<'a>(&'a mut self, c: &'a AppConfig, theme: Option<&ThemeFile>, key: (u64, u64)) -> &'a AppConfig {
        if !c.professional && theme.is_none() {
            return c;
        }
        if self.cfg.is_none() || self.key != key {
            self.cfg = Some(effective(c, theme).into_owned());
            self.key = key;
        }
        self.cfg.as_ref().unwrap_or(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iron_sheets_core::config::ThemePref;

    fn busy() -> AppConfig {
        let mut c = AppConfig { theme: ThemePref::Light, editor_font_size: 20.0, caret_trail: true, newtab_image: "a.png".into(), editor_font: "hack".into(), ..Default::default() };
        c.colors_dark.insert("accent".into(), "#010203".into());
        c
    }

    #[test]
    fn professional_ignores_customisations_and_leaves_the_stored_config_alone() {
        let mut c = busy();
        assert!(matches!(effective(&c, None), Cow::Borrowed(_)), "off: no copy");
        c.professional = true;
        let before = c.clone();
        let e = effective(&c, None);
        let d = AppConfig::default();
        assert_eq!((e.theme, e.editor_font_size, e.caret_trail, e.editor_font.as_str()), (d.theme, d.editor_font_size, false, ""));
        assert!(e.colors_dark.is_empty() && e.newtab_image.is_empty() && !e.newtab_quotes);
        assert_eq!(c, before);
        c.professional = false;
        assert_eq!(effective(&c, None).editor_font_size, 20.0, "off again: everything is back");
    }

    #[test]
    fn theme_file_sets_base_and_user_overrides_win() {
        let mut c = AppConfig::default();
        c.colors_light.insert("accent".into(), "#111111".into());
        let t = iron_sheets_core::themes::load(&iron_sheets_core::paths::AppDirs::at(std::path::Path::new("/nonexistent")), "sepia").unwrap();
        let e = effective(&c, Some(&t));
        assert_eq!(e.theme, ThemePref::Light);
        assert_eq!(e.colors_light["accent"], "#111111");
        assert_eq!(e.colors_light["editor_bg"], "#f4ecd8");
        assert!(e.colors_dark.is_empty());
    }
}
