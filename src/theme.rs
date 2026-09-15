//! Semantic theme: every color in the UI comes from here.
//!
//! No inline RGB or ad-hoc foreground styles anywhere else in the codebase.
//! Green means running/success, amber means active/brand, teal means
//! information/keys, yellow means warning/command, red means danger/exited,
//! muted gray means inactive.
//!
//! OS themes (Omarchy `colors.toml`) only ever override *absolute*
//! black/white roles: hue roles (yellow, cyan, …) already follow the OS
//! theme live because the terminal redefines those palette slots itself.
//! The override lives in a thread-local — the render loop runs on one
//! thread, so parallel tests each start on builtin with no shared
//! mutable state.

use std::cell::RefCell;
use std::path::Path;

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
    /// Pill session tab: bright container, dark label.
    TabActive,
    /// Pill tab at rest: dim solid container, dark label.
    TabInactive,
    BorderFocused,
    BorderUnfocused,
    BorderModal,
    KeyHint,
    KeyDesc,
}

/// Absolute-color overrides from the OS theme. Hue roles are absent on
/// purpose: the terminal already maps those slots per OS theme.
#[derive(Clone, Copy, Debug, Default)]
pub struct AbsoluteDelta {
    text: Option<Color>,
    key_desc: Option<Color>,
    modal_bg: Option<Color>,
}

impl AbsoluteDelta {
    /// Builtin look: no overrides (dark-terminal correct).
    pub const BUILTIN: Self = Self { text: None, key_desc: None, modal_bg: None };
    /// Light OS theme: black text, opaque light modal fill.
    pub const LIGHT: Self = Self {
        text: Some(Color::Black),
        key_desc: Some(Color::Black),
        modal_bg: Some(Color::White),
    };
}

thread_local! {
    static ACTIVE_DELTA: RefCell<AbsoluteDelta> =
        RefCell::new(AbsoluteDelta::BUILTIN);
}

/// Watches one Omarchy state dir for theme switches. Explicit state
/// (not ambient) so tests stay hermetic; the render loop owns one.
pub struct ThemeWatcher {
    state_dir: std::path::PathBuf,
    last_name: String,
}

impl ThemeWatcher {
    /// Production watcher: `$HOME/.local/state/omarchy`.
    pub fn omarchy() -> Self {
        let dir = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        Self::at(std::path::PathBuf::from(dir).join(".local/state/omarchy").as_path())
    }

    /// Watcher against an explicit state dir (tests point at a tempdir).
    pub fn at(state_dir: &Path) -> Self {
        ThemeWatcher { state_dir: state_dir.to_path_buf(), last_name: String::new() }
    }

    /// Poll for a theme switch. True when the ambient map changed and
    /// the frame must repaint. Never fails: an unreadable or
    /// half-written state keeps the current map (and retries next tick).
    pub fn poll(&mut self) -> bool {
        let name = match std::fs::read_to_string(self.state_dir.join("current/theme.name")) {
            Ok(name) => name.trim().to_string(),
            Err(_) => return false,
        };
        if name == self.last_name {
            return false;
        }
        // theme.name is written after the theme dir swap, so colors.toml
        // is already the new theme's. A torn read resolves next tick.
        let mode = std::fs::read_to_string(self.state_dir.join("current/theme/colors.toml"))
            .ok()
            .and_then(|text| parse_omarchy_mode(&text));
        let Some(mode) = mode else { return false };
        ACTIVE_DELTA.with(|active| *active.borrow_mut() = delta_for(mode.dark));
        self.last_name = name;
        true
    }
}

/// Style for a role: builtin, plus any ambient OS-theme override.
pub fn style(role: Role) -> Style {
    let base = builtin_style(role);
    ACTIVE_DELTA.with(|active| {
        let delta = active.borrow();
        match role {
            Role::Text => delta.text.map(|fg| base.fg(fg)).unwrap_or(base),
            Role::KeyDesc => delta.key_desc.map(|fg| base.fg(fg)).unwrap_or(base),
            _ => base,
        }
    })
}

/// Opaque fill for modal bodies. Transparent on builtin/dark setups
/// (today's look); solid on light OS themes.
pub fn modal_fill() -> Style {
    ACTIVE_DELTA.with(|active| match active.borrow().modal_bg {
        Some(bg) => Style::default().bg(bg),
        None => Style::default(),
    })
}

/// Style for a role in the default theme.
fn builtin_style(role: Role) -> Style {
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
        Role::TabActive => Style::default()
            .fg(Color::Black)
            .bg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        Role::TabInactive => Style::default()
            .fg(Color::Black)
            .bg(Color::DarkGray),
        Role::BorderFocused => Style::default().fg(Color::Yellow),
        Role::BorderUnfocused => Style::default().fg(Color::DarkGray),
        Role::BorderModal => Style::default().fg(Color::Cyan),
        Role::KeyHint => Style::default().fg(Color::Cyan),
        Role::KeyDesc => Style::default().fg(Color::White),
    }
}

/// What forge needs from an Omarchy `colors.toml`: dark or light.
/// Only absolute roles depend on it (see [`AbsoluteDelta`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OmarchyMode {
    pub dark: bool,
}

/// Parse the mode out of `colors.toml` text. Honors an explicit
/// `mode` key, else falls back to background luminance; `None` when
/// neither exists or the text is not TOML.
pub fn parse_omarchy_mode(text: &str) -> Option<OmarchyMode> {
    let table: toml::Table = text.parse().ok()?;
    if let Some(mode) = table.get("mode").and_then(|v| v.as_str()) {
        if mode.eq_ignore_ascii_case("dark") {
            return Some(OmarchyMode { dark: true });
        }
        if mode.eq_ignore_ascii_case("light") {
            return Some(OmarchyMode { dark: false });
        }
    }
    let bg = table.get("background").and_then(|v| v.as_str())?;
    Some(OmarchyMode { dark: !is_light_hex(bg) })
}

fn is_light_hex(hex: &str) -> bool {
    let h = hex.strip_prefix('#').unwrap_or(hex);
    if h.len() != 6 {
        return false;
    }
    let v = u32::from_str_radix(h, 16).unwrap_or(0);
    let (r, g, b) = ((v >> 16) & 0xff, (v >> 8) & 0xff, v & 0xff);
    (0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64) / 255.0 > 0.5
}

/// Override for a mode: dark setups already match builtin.
fn delta_for(dark: bool) -> AbsoluteDelta {
    if dark {
        AbsoluteDelta::BUILTIN
    } else {
        AbsoluteDelta::LIGHT
    }
}

/// Hold an override on the calling thread; restores on drop (even on
/// panic), so pooled test threads never leak it into another test.
pub struct DeltaGuard {
    prev: AbsoluteDelta,
}

impl Drop for DeltaGuard {
    fn drop(&mut self) {
        ACTIVE_DELTA.with(|active| *active.borrow_mut() = self.prev);
    }
}

/// Test helper: hold `delta` for this thread until the guard drops.
pub fn hold_delta(delta: AbsoluteDelta) -> DeltaGuard {
    ACTIVE_DELTA.with(|active| {
        let prev = *active.borrow();
        *active.borrow_mut() = delta;
        DeltaGuard { prev }
    })
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

/// Keyboard-focus row inside dialogs: cyan plus reverse video, per the
/// UI guidance. `>` marks the focused row; yellow stays reserved for
/// selection marks and the default action.
pub fn focus_row() -> Style {
    style(Role::Info).add_modifier(Modifier::REVERSED)
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
        // Focus rows are cyan plus reverse: never hue-alone, never yellow.
        assert_eq!(focus_row().fg, Some(Color::Cyan));
        assert!(focus_row().add_modifier.contains(Modifier::REVERSED));
        // Key hints alternate two distinct roles.
        assert_ne!(style(Role::KeyHint), style(Role::KeyDesc));
    }

    #[test]
    fn omarchy_mode_parses_explicit_dark_and_light() {
        assert_eq!(parse_omarchy_mode("mode = \"dark\"\n"), Some(OmarchyMode { dark: true }));
        assert_eq!(parse_omarchy_mode("mode = \"Light\"\n"), Some(OmarchyMode { dark: false }));
    }

    #[test]
    fn omarchy_mode_ignores_unknown_keys() {
        let text = "mode = \"dark\"\naccent = \"#7aa2f7\"\nbackground = \"#1a1b26\"\nred = \"#f7768e\"\n";
        assert_eq!(parse_omarchy_mode(text), Some(OmarchyMode { dark: true }));
    }

    #[test]
    fn omarchy_mode_falls_back_to_background_luminance() {
        assert_eq!(
            parse_omarchy_mode("background = \"#1a1b26\"\n"),
            Some(OmarchyMode { dark: true })
        );
        assert_eq!(
            parse_omarchy_mode("background = \"#eff1f5\"\n"),
            Some(OmarchyMode { dark: false })
        );
        // Unknown mode value falls back to luminance too.
        assert_eq!(
            parse_omarchy_mode("mode = \"sepia\"\nbackground = \"#ffffff\"\n"),
            Some(OmarchyMode { dark: false })
        );
        assert_eq!(parse_omarchy_mode("accent = \"#7aa2f7\"\n"), None, "no mode, no background");
        assert_eq!(parse_omarchy_mode("not toml [[["), None, "garbage");
    }

    #[test]
    fn light_delta_swaps_absolute_roles_only() {
        let _guard = hold_delta(AbsoluteDelta::LIGHT);
        assert_eq!(style(Role::Text).fg, Some(Color::Black));
        assert_eq!(style(Role::KeyDesc).fg, Some(Color::Black));
        assert_eq!(modal_fill().bg, Some(Color::White));
        // Hue roles keep tracking the terminal palette.
        assert_eq!(style(Role::Brand).fg, Some(Color::Yellow));
        assert_eq!(style(Role::Info).fg, Some(Color::Cyan));
        assert_eq!(modal_fill(), Style::default().bg(Color::White));
    }

    #[test]
    fn builtin_modal_fill_stays_transparent() {
        assert_eq!(modal_fill(), Style::default());
    }

    fn theme_state_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("forge-theme-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("current/theme")).unwrap();
        dir
    }

    fn write_theme(dir: &std::path::Path, name: &str, colors: &str) {
        std::fs::write(dir.join("current/theme.name"), format!("{name}\n")).unwrap();
        std::fs::write(dir.join("current/theme/colors.toml"), colors).unwrap();
    }

    #[test]
    fn watcher_applies_switch_and_ignores_repeats() {
        let _guard = hold_delta(AbsoluteDelta::BUILTIN);
        let dir = theme_state_dir("switch");
        let mut watcher = ThemeWatcher::at(&dir);
        assert!(!watcher.poll(), "no state yet");
        write_theme(&dir, "tokyo-night", "mode = \"dark\"\n");
        assert!(watcher.poll(), "first switch applies");
        assert_eq!(style(Role::Text).fg, Some(Color::White));
        assert!(!watcher.poll(), "repeat polls quiet");
        write_theme(&dir, "catppuccin-latte", "mode = \"light\"\n");
        assert!(watcher.poll(), "second switch applies");
        assert_eq!(style(Role::Text).fg, Some(Color::Black));
        assert_eq!(modal_fill().bg, Some(Color::White));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn watcher_keeps_map_on_torn_state() {
        let _guard = hold_delta(AbsoluteDelta::BUILTIN);
        let dir = theme_state_dir("torn");
        let mut watcher = ThemeWatcher::at(&dir);
        write_theme(&dir, "tokyo-night", "mode = \"dark\"\n");
        assert!(watcher.poll());
        // Name flips but the palette never lands: the map stays and the
        // next tick retries instead of sticking on a torn read.
        std::fs::write(dir.join("current/theme.name"), "half-staged\n").unwrap();
        std::fs::remove_file(dir.join("current/theme/colors.toml")).unwrap();
        assert!(!watcher.poll());
        assert_eq!(style(Role::Text).fg, Some(Color::White), "map unchanged");
        std::fs::write(dir.join("current/theme/colors.toml"), "mode = \"light\"\n").unwrap();
        assert!(watcher.poll(), "applies once the palette lands");
        assert_eq!(style(Role::Text).fg, Some(Color::Black));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
