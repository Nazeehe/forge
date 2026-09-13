//! Semantic theme: every color in the UI comes from here.
//!
//! No inline RGB or ad-hoc foreground styles anywhere else in the codebase.
//! Green means running/success, amber means active/brand, teal means
//! information/keys, yellow means warning/command, red means danger/exited,
//! muted gray means inactive.

use ratatui::style::{Color, Modifier, Style};

/// Semantic color roles. Variants, not colors: a future theme remaps roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    Text,
    Muted,
    Success,
    Running,
    Starting,
    Exited,
    Warning,
    Danger,
    Info,
    Brand,
    Command,
    Focus,
    BorderFocused,
    BorderUnfocused,
    BorderModal,
    KeyHint,
    KeyDesc,
}

/// Style for a role in the default theme.
pub fn style(role: Role) -> Style {
    match role {
        Role::Text => Style::default().fg(Color::White),
        Role::Muted => Style::default().fg(Color::DarkGray),
        Role::Success | Role::Running => Style::default().fg(Color::Green),
        Role::Starting => Style::default().fg(Color::Yellow),
        Role::Exited | Role::Danger => Style::default().fg(Color::Red),
        Role::Warning | Role::Command => Style::default().fg(Color::LightYellow),
        Role::Info => Style::default().fg(Color::Cyan),
        Role::Brand => Style::default().fg(Color::Yellow),
        Role::Focus => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        Role::BorderFocused => Style::default().fg(Color::Yellow),
        Role::BorderUnfocused => Style::default().fg(Color::DarkGray),
        Role::BorderModal => Style::default().fg(Color::Cyan),
        Role::KeyHint => Style::default().fg(Color::Cyan),
        Role::KeyDesc => Style::default().fg(Color::White),
    }
}

/// Status glyph plus its role: `●` running, `○` starting, `×` exited.
pub fn status_glyph_running() -> (char, Role) {
    ('●', Role::Running)
}

pub fn status_glyph_starting() -> (char, Role) {
    ('○', Role::Starting)
}

pub fn status_glyph_exited() -> (char, Role) {
    ('×', Role::Exited)
}

/// Focused-field marker.
pub fn focus_marker() -> char {
    '▸'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_no_rgb(role: Role) {
        let fg = style(role).fg.expect("every role sets fg");
        assert!(
            !matches!(fg, Color::Rgb(..)),
            "{role:?} must not use inline RGB"
        );
    }

    #[test]
    fn documented_mappings() {
        assert_eq!(style(Role::Success).fg, Some(Color::Green));
        assert_eq!(style(Role::Running).fg, Some(Color::Green));
        assert_eq!(style(Role::Brand).fg, Some(Color::Yellow));
        assert_eq!(style(Role::Info).fg, Some(Color::Cyan));
        assert_eq!(style(Role::Danger).fg, Some(Color::Red));
        assert_eq!(style(Role::Exited).fg, Some(Color::Red));
    }

    #[test]
    fn no_role_uses_inline_rgb() {
        for role in [
            Role::Text,
            Role::Muted,
            Role::Success,
            Role::Running,
            Role::Starting,
            Role::Exited,
            Role::Warning,
            Role::Danger,
            Role::Info,
            Role::Brand,
            Role::Command,
            Role::Focus,
            Role::BorderFocused,
            Role::BorderUnfocused,
            Role::BorderModal,
            Role::KeyHint,
            Role::KeyDesc,
        ] {
            assert_no_rgb(role);
        }
    }

    #[test]
    fn status_glyphs_and_marker() {
        assert_eq!(status_glyph_running(), ('●', Role::Running));
        assert_eq!(status_glyph_starting(), ('○', Role::Starting));
        assert_eq!(status_glyph_exited(), ('×', Role::Exited));
        assert_eq!(focus_marker(), '▸');
        // Key hints alternate two distinct roles.
        assert_ne!(style(Role::KeyHint), style(Role::KeyDesc));
    }
}
