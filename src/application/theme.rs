//! Loads the Frost stylesheet: a tinted wash, translucent glass panels and the
//! system accent, in a light and a dark palette.
//!
//! `style.css` holds the rules and only names colours; `theme-light.css` and
//! `theme-dark.css` define those names (and libadwaita's) for each scheme.
//! The provider is reloaded with the matching palette whenever libadwaita's
//! dark state changes, which follows the Appearance setting (see
//! `window::apply_color_scheme`) or the system preference. GTK 4.12 has no
//! CSS media queries, hence the swap.
//!
//! The accent is never set here: the palettes derive every accent token from
//! libadwaita's `@accent_bg_color` and `@accent_fg_color`, which libadwaita
//! keeps in step with the system accent.

use gtk::gdk;

const STYLE: &str = include_str!("../../data/style.css");
const LIGHT: &str = include_str!("../../data/theme-light.css");
const DARK: &str = include_str!("../../data/theme-dark.css");

/// Above libadwaita's stylesheet (THEME), so our surfaces and named colours win.
const PRIORITY: u32 = gtk::STYLE_PROVIDER_PRIORITY_USER;

/// The palette followed by the rules, as one sheet so every name resolves.
fn sheet(dark: bool) -> String {
    let palette = if dark { DARK } else { LIGHT };
    let mut s = String::with_capacity(palette.len() + STYLE.len() + 1);
    s.push_str(palette);
    s.push('\n');
    s.push_str(STYLE);
    s
}

/// Installs the stylesheet for `display` and keeps it in step with the
/// colour scheme. Call once at startup, after libadwaita is initialised.
pub fn install(display: &gdk::Display) {
    let css = gtk::CssProvider::new();
    let manager = adw::StyleManager::for_display(display);
    css.load_from_string(&sheet(manager.is_dark()));
    gtk::style_context_add_provider_for_display(display, &css, PRIORITY);
    manager.connect_dark_notify(move |m| css.load_from_string(&sheet(m.is_dark())));
    #[cfg(debug_assertions)]
    debug_accent(display);
}

/// Debug: `FM_ACCENT=#e66100` stands in for the system accent, to check the
/// palettes against other accents without changing the desktop's setting.
#[cfg(debug_assertions)]
fn debug_accent(display: &gdk::Display) {
    let Ok(color) = std::env::var("FM_ACCENT") else { return };
    let Ok(rgba) = gdk::RGBA::parse(color.as_str()) else { return };
    let css = gtk::CssProvider::new();
    css.load_from_string(&format!("@define-color accent_bg_color {rgba};"));
    gtk::style_context_add_provider_for_display(display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    fn defined(css: &str) -> BTreeSet<&str> {
        css.lines()
            .filter_map(|l| l.trim().strip_prefix("@define-color "))
            .filter_map(|l| l.split_whitespace().next())
            .collect()
    }

    /// Both palettes must define the same names, or one scheme falls back to
    /// libadwaita's colours (or an unresolved name) somewhere.
    #[test]
    fn palettes_define_the_same_names() {
        assert_eq!(defined(super::LIGHT), defined(super::DARK));
    }

    /// The accent is the system's: no palette may pin libadwaita's accent.
    #[test]
    fn palettes_leave_the_accent_to_libadwaita() {
        for css in [super::LIGHT, super::DARK] {
            for name in ["accent_bg_color", "accent_fg_color", "accent_color"] {
                assert!(!defined(css).contains(name), "a palette defines @{name}");
            }
            // Built with relative colour syntax; resolves to black from our provider.
            assert!(!css.contains("@accent_color"), "a palette uses @accent_color");
        }
    }

    /// The rules only name colours; every name they use comes from a palette
    /// (or is one of GTK's colour keywords).
    #[test]
    fn rules_use_only_palette_names() {
        let names = defined(super::LIGHT);
        assert!(defined(super::STYLE).is_empty(), "style.css must not define colours");
        for (i, line) in super::STYLE.lines().enumerate() {
            let mut rest = line;
            while let Some(at) = rest.find('@') {
                rest = &rest[at + 1..];
                let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
                if name == "keyframes" || name.is_empty() {
                    continue;
                }
                assert!(names.contains(name.as_str()), "style.css:{}: @{name} is not in the palettes", i + 1);
            }
            let lower = line.to_ascii_lowercase();
            assert!(!lower.contains("rgba(") && !lower.contains("rgb("), "style.css:{}: literal colour", i + 1);
        }
    }
}
