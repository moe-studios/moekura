//! Colour themes. The default theme is built into `css/main.css`; every
//! other one is a `themes/<name>.css` among the static files, built in or
//! added through `paths.static_override`. A theme's file only sets colour
//! tokens, so it loads after the main stylesheet.

use minijinja::{Value, context};
use moekura_core::user_settings::{DEFAULT_THEME, is_theme_name};

use crate::assets::Assets;

/// The site's themes' names, the default first and the rest alphabetically.
pub fn names(assets: &Assets) -> Vec<&str> {
    let mut names: Vec<&str> = assets
        .logical_paths()
        .filter_map(|path| path.strip_prefix("themes/")?.strip_suffix(".css"))
        .filter(|name| is_theme_name(name) && *name != DEFAULT_THEME)
        .collect();
    names.sort_unstable();
    names.insert(0, DEFAULT_THEME);
    names
}

/// `high-contrast` → `High contrast`.
pub fn label(name: &str) -> String {
    let mut chars = name.chars();
    chars.next().map_or_else(String::new, |first| {
        first
            .to_uppercase()
            .chain(chars)
            .collect::<String>()
            .replace('-', " ")
    })
}

/// The theme to show: the user's choice, else the site's default, else the
/// built-in one, skipping any the site doesn't have.
pub fn resolve<'a>(assets: &'a Assets, chosen: Option<&str>, site_default: &str) -> &'a str {
    let names = names(assets);
    [chosen, Some(site_default)]
        .into_iter()
        .flatten()
        .find_map(|wanted| names.iter().find(|name| **name == wanted).copied())
        .unwrap_or(DEFAULT_THEME)
}

/// The themes as `{ name, label }` for templates.
pub fn choices(assets: &Assets) -> Vec<Value> {
    names(assets)
        .into_iter()
        .map(|name| context! { name => name, label => label(name) })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn lists_built_in_and_added_themes() {
        let dir = std::env::temp_dir().join(format!("moekura-themes-{}", std::process::id()));
        fs::create_dir_all(dir.join("themes")).unwrap();
        fs::write(dir.join("themes/aurora.css"), ":root{}").unwrap();
        fs::write(dir.join("themes/Bad Name.css"), ":root{}").unwrap();
        fs::write(dir.join("themes/dark.css"), ":root{}").unwrap();
        let assets = Assets::load(Some(&dir)).unwrap();
        fs::remove_dir_all(dir).unwrap();

        let names = names(&assets);
        assert_eq!(names[0], "default");
        for name in ["aurora", "forest", "ocean", "sakura", "wisteria"] {
            assert!(names.contains(&name), "{names:?}");
        }
        assert!(!names.contains(&"dark"), "mode names aren't themes");
        assert!(names.iter().all(|n| is_theme_name(n)), "{names:?}");
        assert!(names[1..].is_sorted());
    }

    #[test]
    fn falls_back_to_themes_the_site_has() {
        let assets = Assets::load(None).unwrap();
        assert_eq!(resolve(&assets, Some("ocean"), "sakura"), "ocean");
        assert_eq!(resolve(&assets, Some("gone"), "sakura"), "sakura");
        assert_eq!(resolve(&assets, None, "gone"), "default");
    }

    #[test]
    fn labels_names() {
        assert_eq!(label("sakura"), "Sakura");
        assert_eq!(label("high-contrast"), "High contrast");
    }
}
