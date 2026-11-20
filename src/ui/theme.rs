use std::sync::RwLock;

use ratatui::style::Color;

/// Colors of the app's own UI (lists, tabs, dialogs, status bar). Sessions keep the colors
/// the remote programs ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub name: &'static str,
    pub description: &'static str,
    /// What has focus: borders, titles, keys in hints, selected choices.
    pub accent: Color,
    /// Secondary text and unfocused borders.
    pub muted: Color,
    /// Labels and hints, one step above muted.
    pub subtle: Color,
    /// Text that must stand out: picked values, messages.
    pub text: Color,
    /// Text on top of a colored background (badges, buttons, the selected row).
    pub on_color: Color,
    pub folder: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub broadcast: Color,
    pub visual: Color,
    pub directory: Color,
    /// Tab bar and input boxes.
    pub panel: Color,
    /// Focused input box, visual range, secondary badges.
    pub panel_focus: Color,
    pub status_bar: Color,
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

pub const THEMES: &[Theme] = &[
    Theme {
        name: "default",
        description: "your terminal's own palette",
        accent: Color::Cyan,
        muted: Color::DarkGray,
        subtle: Color::Gray,
        text: Color::White,
        on_color: Color::Black,
        folder: Color::Yellow,
        success: Color::Green,
        warning: Color::Yellow,
        error: Color::LightRed,
        broadcast: Color::Red,
        visual: Color::Magenta,
        directory: Color::Blue,
        panel: Color::Indexed(236),
        panel_focus: Color::Indexed(238),
        status_bar: Color::Indexed(235),
    },
    Theme {
        name: "light",
        description: "for terminals with a light background",
        accent: Color::Indexed(25),
        muted: Color::Indexed(246),
        subtle: Color::Indexed(240),
        text: Color::Indexed(232),
        on_color: Color::Indexed(255),
        folder: Color::Indexed(130),
        success: Color::Indexed(28),
        warning: Color::Indexed(130),
        error: Color::Indexed(160),
        broadcast: Color::Indexed(160),
        visual: Color::Indexed(127),
        directory: Color::Indexed(25),
        panel: Color::Indexed(254),
        panel_focus: Color::Indexed(252),
        status_bar: Color::Indexed(253),
    },
    Theme {
        name: "nord",
        description: "cool blues and frost",
        accent: rgb(0x88c0d0),
        muted: rgb(0x616e88),
        subtle: rgb(0xd8dee9),
        text: rgb(0xeceff4),
        on_color: rgb(0x2e3440),
        folder: rgb(0xebcb8b),
        success: rgb(0xa3be8c),
        warning: rgb(0xebcb8b),
        error: rgb(0xbf616a),
        broadcast: rgb(0xbf616a),
        visual: rgb(0xb48ead),
        directory: rgb(0x81a1c1),
        panel: rgb(0x3b4252),
        panel_focus: rgb(0x434c5e),
        status_bar: rgb(0x2e3440),
    },
    Theme {
        name: "gruvbox",
        description: "warm retro colors",
        accent: rgb(0x8ec07c),
        muted: rgb(0x928374),
        subtle: rgb(0xa89984),
        text: rgb(0xebdbb2),
        on_color: rgb(0x282828),
        folder: rgb(0xfabd2f),
        success: rgb(0xb8bb26),
        warning: rgb(0xfe8019),
        error: rgb(0xfb4934),
        broadcast: rgb(0xcc241d),
        visual: rgb(0xd3869b),
        directory: rgb(0x83a598),
        panel: rgb(0x3c3836),
        panel_focus: rgb(0x504945),
        status_bar: rgb(0x32302f),
    },
    Theme {
        name: "dracula",
        description: "purple and pink on dark gray",
        accent: rgb(0xbd93f9),
        muted: rgb(0x6272a4),
        subtle: rgb(0xbfbfbf),
        text: rgb(0xf8f8f2),
        on_color: rgb(0x282a36),
        folder: rgb(0xf1fa8c),
        success: rgb(0x50fa7b),
        warning: rgb(0xffb86c),
        error: rgb(0xff5555),
        broadcast: rgb(0xff5555),
        visual: rgb(0xff79c6),
        directory: rgb(0x8be9fd),
        panel: rgb(0x343746),
        panel_focus: rgb(0x44475a),
        status_bar: rgb(0x21222c),
    },
    Theme {
        name: "catppuccin",
        description: "soft pastels (mocha)",
        accent: rgb(0x89b4fa),
        muted: rgb(0x6c7086),
        subtle: rgb(0xa6adc8),
        text: rgb(0xcdd6f4),
        on_color: rgb(0x1e1e2e),
        folder: rgb(0xf9e2af),
        success: rgb(0xa6e3a1),
        warning: rgb(0xfab387),
        error: rgb(0xf38ba8),
        broadcast: rgb(0xf38ba8),
        visual: rgb(0xcba6f7),
        directory: rgb(0x89b4fa),
        panel: rgb(0x313244),
        panel_focus: rgb(0x45475a),
        status_bar: rgb(0x181825),
    },
    Theme {
        name: "solarized",
        description: "solarized dark",
        accent: rgb(0x2aa198),
        muted: rgb(0x586e75),
        subtle: rgb(0x93a1a1),
        text: rgb(0xeee8d5),
        on_color: rgb(0x002b36),
        folder: rgb(0xb58900),
        success: rgb(0x859900),
        warning: rgb(0xcb4b16),
        error: rgb(0xdc322f),
        broadcast: rgb(0xdc322f),
        visual: rgb(0xd33682),
        directory: rgb(0x268bd2),
        panel: rgb(0x073642),
        panel_focus: rgb(0x0e4a5a),
        status_bar: rgb(0x002b36),
    },
];

static CURRENT: RwLock<Theme> = RwLock::new(THEMES[0]);

pub fn find(name: &str) -> Option<&'static Theme> {
    THEMES.iter().find(|t| t.name.eq_ignore_ascii_case(name))
}

pub fn names() -> String {
    THEMES.iter().map(|t| t.name).collect::<Vec<_>>().join(", ")
}

/// The theme in use.
pub fn colors() -> Theme {
    *CURRENT.read().unwrap_or_else(|e| e.into_inner())
}

/// Switches the UI to `theme`; without truecolor its RGB colors become the nearest of 256.
pub fn apply(theme: &Theme, truecolor: bool) {
    let fit = |c: Color| match c {
        Color::Rgb(r, g, b) if !truecolor => Color::Indexed(crate::term::render::rgb_to_256(r, g, b)),
        c => c,
    };
    let fitted = Theme {
        accent: fit(theme.accent),
        muted: fit(theme.muted),
        subtle: fit(theme.subtle),
        text: fit(theme.text),
        on_color: fit(theme.on_color),
        folder: fit(theme.folder),
        success: fit(theme.success),
        warning: fit(theme.warning),
        error: fit(theme.error),
        broadcast: fit(theme.broadcast),
        visual: fit(theme.visual),
        directory: fit(theme.directory),
        panel: fit(theme.panel),
        panel_focus: fit(theme.panel_focus),
        status_bar: fit(theme.status_bar),
        ..*theme
    };
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = fitted;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_found() {
        for (i, a) in THEMES.iter().enumerate() {
            assert!(THEMES[i + 1..].iter().all(|b| b.name != a.name), "{} twice", a.name);
            assert_eq!(find(&a.name.to_uppercase()).map(|t| t.name), Some(a.name));
        }
        assert!(find("nope").is_none());
    }

    #[test]
    fn rgb_is_fitted_without_truecolor() {
        let nord = find("nord").unwrap();
        apply(nord, false);
        assert!(matches!(colors().accent, Color::Indexed(_)));
        apply(nord, true);
        assert_eq!(colors().accent, nord.accent);
        apply(&THEMES[0], true);
    }
}
