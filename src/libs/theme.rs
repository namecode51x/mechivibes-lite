use dioxus::prelude::*;
use serde::{Deserialize, Serialize};
use strum::{EnumIter, IntoEnumIterator};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Theme {
    BuiltIn(BuiltInTheme),
    Custom(String), // Custom theme with name
}

/// Defaults to following the OS, matching what a fresh `AppConfig` picks.
/// Having a `Default` lets a damaged `theme` entry in a hand-edited config
/// fall back to this one setting instead of failing the whole document.
///
/// Written out rather than derived: `#[default]` only applies to unit
/// variants, and the default here carries a `BuiltInTheme` payload.
impl Default for Theme {
    fn default() -> Self {
        Theme::BuiltIn(BuiltInTheme::default())
    }
}


#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, EnumIter, Default)]
pub enum BuiltInTheme {
    /// The house theme: dark frosted glass (see `style.css`).
    #[default]
    SuperLite,
    /// The house theme in its other finish: paper white with black ink
    /// (see `style.css`). Same shape tokens as `SuperLite`, so the two read
    /// as one instrument in two finishes. Lowercased, this name is the
    /// daisyUI theme the stylesheet has to define.
    Papel,
    Light,
    Dark,
    System,
    // DaisyUI themes
    Cupcake,
    Bumblebee,
    Emerald,
    Corporate,
    Synthwave,
    Retro,
    Cyberpunk,
    Valentine,
    Halloween,
    Garden,
    Forest,
    Aqua,
    Lofi,
    Pastel,
    Fantasy,
    Wireframe,
    Black,
    Luxury,
    Dracula,
    Cmyk,
    Autumn,
    Business,
    Acid,
    Lemonade,
    Night,
    Coffee,
    Winter,
    Dim,
    Nord,
    Sunset,
    Abyss,
    Silk,
    Caramellatte,
}

impl BuiltInTheme {
    /// Convert to DaisyUI theme name
    pub fn to_daisy_theme(&self) -> String {
        match self {
            BuiltInTheme::System => "light".to_string(), // Default to light for system theme
            _ => format!("{:?}", self).to_lowercase(),
        }
    }
    /// Get all available built-in themes
    pub fn all() -> Vec<BuiltInTheme> {
        BuiltInTheme::iter().collect()
    }
}

impl Theme {
    /// Convert to DaisyUI theme name
    pub fn to_daisy_theme(&self) -> String {
        match self {
            Theme::BuiltIn(builtin) => builtin.to_daisy_theme(),
            Theme::Custom(name) => format!("custom-{}", name),
        }
    }
}

// Global theme context
pub static THEME: GlobalSignal<Theme> = Signal::global(Theme::default);

pub fn use_theme() -> Signal<Theme> {
    THEME.signal()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The picker renders one entry per variant and the webview selects the
    /// theme by its daisyUI name, so a variant added in Rust but never
    /// compiled into `assets/style.css` would show up as selectable and then
    /// paint nothing - silently keeping whatever theme was already active.
    /// The two files are checked against each other here instead: this is
    /// what catches a theme registered in one place and forgotten in the
    /// other, which no amount of clicking in the UI would reliably show.
    #[test]
    fn every_builtin_theme_is_present_in_the_compiled_stylesheet() {
        const STYLESHEET: &str = include_str!("../../assets/style.css");

        for theme in BuiltInTheme::all() {
            let daisy_name = theme.to_daisy_theme();
            assert!(
                STYLESHEET.contains(&format!("[data-theme={daisy_name}]")),
                "{theme:?} maps to data-theme `{daisy_name}`, but assets/style.css \
                 defines no such theme - register it in style.css next to \
                 `superlite` and rebuild the stylesheet (npm run build:css)"
            );
        }
    }
}
