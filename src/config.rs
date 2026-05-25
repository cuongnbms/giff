use std::collections::HashMap;

use serde::Deserialize;

use crate::ui::theme::{Theme, ThemeConfig};
use crate::ui::ViewMode;

#[derive(Default, Deserialize)]
pub struct Config {
    pub theme: Option<String>,
    /// Initial word-wrap state. Defaults to `true` when unset.
    pub wrap: Option<bool>,
    /// Initial diff layout: `"side-by-side"` (default) or `"unified"`.
    pub view_mode: Option<String>,
    #[serde(default)]
    pub themes: HashMap<String, ThemeConfig>,
}

pub fn load_config() -> Config {
    let config_path = match dirs::config_dir() {
        Some(dir) => dir.join("giff").join("config.toml"),
        None => return Config::default(),
    };

    let contents = match std::fs::read_to_string(&config_path) {
        Ok(s) => s,
        Err(_) => return Config::default(),
    };

    match toml::from_str(&contents) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Warning: failed to parse {}: {}", config_path.display(), e);
            Config::default()
        }
    }
}

pub fn resolve_wrap(config: &Config) -> bool {
    config.wrap.unwrap_or(true)
}

pub fn resolve_view_mode(config: &Config) -> ViewMode {
    match config.view_mode.as_deref() {
        Some(s) => match s.trim().to_ascii_lowercase().as_str() {
            "unified" => ViewMode::Unified,
            "side-by-side" | "sidebyside" | "side_by_side" | "split" => ViewMode::SideBySide,
            other => {
                eprintln!(
                    "Warning: unknown view_mode '{}' (expected \"side-by-side\" or \"unified\"), \
                     falling back to side-by-side",
                    other
                );
                ViewMode::SideBySide
            }
        },
        None => ViewMode::SideBySide,
    }
}

pub fn resolve_theme(config: &Config, cli_theme: Option<&str>) -> Theme {
    let theme_name = cli_theme.or(config.theme.as_deref()).unwrap_or("dark");

    if let Some(theme) = Theme::by_name(theme_name) {
        return theme;
    }

    if let Some(theme_config) = config.themes.get(theme_name) {
        return theme_config.to_theme();
    }

    eprintln!(
        "Warning: unknown theme '{}', falling back to dark",
        theme_name
    );
    Theme::dark()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn empty_config() -> Config {
        Config::default()
    }

    fn config_with_theme(name: &str) -> Config {
        Config {
            theme: Some(name.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn resolve_defaults_to_dark() {
        let t = resolve_theme(&empty_config(), None);
        assert!(t.is_dark);
    }

    #[test]
    fn resolve_cli_overrides_config() {
        let config = config_with_theme("dark");
        let t = resolve_theme(&config, Some("light"));
        assert!(!t.is_dark);
    }

    #[test]
    fn resolve_config_file_theme() {
        let config = config_with_theme("light");
        let t = resolve_theme(&config, None);
        assert!(!t.is_dark);
    }

    #[test]
    fn resolve_unknown_falls_back_to_dark() {
        let t = resolve_theme(&empty_config(), Some("nonexistent"));
        assert!(t.is_dark);
    }

    #[test]
    fn resolve_custom_theme_from_config() {
        let mut config = empty_config();
        config.themes.insert(
            "custom".to_string(),
            ThemeConfig {
                base: Some("light".to_string()),
                accent: Some("#FF0000".to_string()),
                ..Default::default()
            },
        );
        let t = resolve_theme(&config, Some("custom"));
        assert!(!t.is_dark); // based on light
        assert_eq!(t.accent, Color::Rgb(255, 0, 0));
    }

    #[test]
    fn resolve_wrap_defaults_true() {
        assert!(resolve_wrap(&empty_config()));
    }

    #[test]
    fn resolve_wrap_honors_config() {
        let cfg = Config {
            wrap: Some(false),
            ..Default::default()
        };
        assert!(!resolve_wrap(&cfg));
    }

    #[test]
    fn resolve_view_mode_defaults_side_by_side() {
        assert!(matches!(
            resolve_view_mode(&empty_config()),
            ViewMode::SideBySide
        ));
    }

    #[test]
    fn resolve_view_mode_parses_unified() {
        let cfg = Config {
            view_mode: Some("unified".to_string()),
            ..Default::default()
        };
        assert!(matches!(resolve_view_mode(&cfg), ViewMode::Unified));
    }

    #[test]
    fn resolve_view_mode_parses_side_by_side_aliases() {
        for alias in ["side-by-side", "SideBySide", "split", "side_by_side"] {
            let cfg = Config {
                view_mode: Some(alias.to_string()),
                ..Default::default()
            };
            assert!(
                matches!(resolve_view_mode(&cfg), ViewMode::SideBySide),
                "alias {} should resolve to SideBySide",
                alias
            );
        }
    }

    #[test]
    fn parses_full_toml_with_ui_defaults() {
        let src = r#"
            theme = "dark"
            wrap = false
            view_mode = "unified"
        "#;
        let cfg: Config = toml::from_str(src).expect("toml should parse");
        assert_eq!(cfg.theme.as_deref(), Some("dark"));
        assert_eq!(cfg.wrap, Some(false));
        assert!(!resolve_wrap(&cfg));
        assert!(matches!(resolve_view_mode(&cfg), ViewMode::Unified));
    }

    #[test]
    fn resolve_view_mode_unknown_falls_back() {
        let cfg = Config {
            view_mode: Some("not-a-mode".to_string()),
            ..Default::default()
        };
        assert!(matches!(resolve_view_mode(&cfg), ViewMode::SideBySide));
    }

    #[test]
    fn resolve_priority_cli_over_config_over_default() {
        // CLI wins over config
        let config = config_with_theme("light");
        let t = resolve_theme(&config, Some("dark"));
        assert!(t.is_dark);

        // Config wins over default
        let config = config_with_theme("light");
        let t = resolve_theme(&config, None);
        assert!(!t.is_dark);

        // Default is dark
        let t = resolve_theme(&empty_config(), None);
        assert!(t.is_dark);
    }
}
