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

/// Builtin pill bookends (Nerd Font half circles). External themes may
/// replace these with any single character (e.g. `[`/`]` for square
/// buttons, `(`/`)` for round ones, `│` for a flat bar look).
pub const BUILTIN_PILL_LEFT: char = '\u{e0b6}';
pub const BUILTIN_PILL_RIGHT: char = '\u{e0b4}';

/// Per-role override from a `theme.json` file. `None` fields keep the
/// builtin value, so minimal themes only name the roles they change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoleOverride {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub modifiers: Option<Modifier>,
}

/// How a selected button shows its selection. `Full` fills the whole
/// button with the accent container (today's pill look); `Left` keeps
/// the button in its rest color and lights only the left bookend —
/// for shaped themes (triangles, wedges) where a full fill would
/// drown the shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonHighlight {
    #[default]
    Full,
    Left,
}

/// One external theme: a name plus role overrides, button bookends,
/// the modal/chrome border style, and the button highlight behavior.
#[derive(Clone, Debug)]
pub struct ExternalTheme {
    pub name: String,
    pub overrides: std::collections::HashMap<Role, RoleOverride>,
    pub button_left: char,
    pub button_right: char,
    pub borders: ratatui::widgets::BorderType,
    pub highlight: ButtonHighlight,
}

impl ExternalTheme {
    /// The builtin look as a theme value: no overrides, Nerd Font pill
    /// caps, plain borders, full-button highlight. The theme picker
    /// always lists this first under the name `default`.
    pub fn builtin() -> Self {
        ExternalTheme {
            name: "default".to_string(),
            overrides: std::collections::HashMap::new(),
            button_left: BUILTIN_PILL_LEFT,
            button_right: BUILTIN_PILL_RIGHT,
            borders: ratatui::widgets::BorderType::Plain,
            highlight: ButtonHighlight::Full,
        }
    }
}

/// Highlight behavior of the active theme (`Full` when no theme is
/// applied).
pub fn highlight_mode() -> ButtonHighlight {
    ACTIVE_THEME.with(|active| {
        active
            .borrow()
            .as_ref()
            .map(|t| t.highlight)
            .unwrap_or(ButtonHighlight::Full)
    })
}

/// Fill plus cap colors for one button: `accent`/`Yellow` when the
/// button carries the emphasis (selected, default, open), `rest`/
/// `rest_cap` otherwise. Under `Left` highlight an emphasized button
/// keeps its rest fill and only the left bookend lights up.
pub fn button_chrome(
    emphasized: bool,
    accent: Style,
    rest: Style,
    rest_cap: Color,
) -> (Style, Color, Color) {
    match (highlight_mode(), emphasized) {
        (ButtonHighlight::Left, true) => (rest, Color::Yellow, rest_cap),
        (_, true) => (accent, Color::Yellow, Color::Yellow),
        (_, false) => (rest, rest_cap, rest_cap),
    }
}

thread_local! {
    static ACTIVE_THEME: RefCell<Option<ExternalTheme>> = RefCell::new(None);
}

/// Install `theme` on this thread. The render loop owns one thread, so
/// this is the runtime-apply path; tests should prefer
/// [`hold_external_theme`] so parallel threads never leak state.
pub fn apply_external_theme(theme: ExternalTheme) {
    ACTIVE_THEME.with(|active| *active.borrow_mut() = Some(theme));
}

/// Drop back to the builtin look on this thread.
pub fn clear_external_theme() {
    ACTIVE_THEME.with(|active| *active.borrow_mut() = None);
}

/// Active theme name on this thread: `default` when no external theme
/// is applied.
pub fn active_theme_name() -> String {
    ACTIVE_THEME.with(|active| {
        active
            .borrow()
            .as_ref()
            .map(|t| t.name.clone())
            .unwrap_or_else(|| "default".to_string())
    })
}

/// Left button bookend for the active theme (builtin Nerd Font cap
/// when no theme is applied).
pub fn pill_left() -> char {
    ACTIVE_THEME.with(|active| {
        active
            .borrow()
            .as_ref()
            .map(|t| t.button_left)
            .unwrap_or(BUILTIN_PILL_LEFT)
    })
}

/// Right button bookend for the active theme.
pub fn pill_right() -> char {
    ACTIVE_THEME.with(|active| {
        active
            .borrow()
            .as_ref()
            .map(|t| t.button_right)
            .unwrap_or(BUILTIN_PILL_RIGHT)
    })
}

/// Border style for modal/chrome blocks under the active theme.
pub fn border_type() -> ratatui::widgets::BorderType {
    ACTIVE_THEME.with(|active| {
        active
            .borrow()
            .as_ref()
            .map(|t| t.borders)
            .unwrap_or(ratatui::widgets::BorderType::Plain)
    })
}

/// Hold `theme` on the calling thread; restores the previous theme on
/// drop (even on panic), mirroring [`hold_delta`] for hermetic tests.
pub struct ThemeGuard {
    prev: Option<ExternalTheme>,
}

impl Drop for ThemeGuard {
    fn drop(&mut self) {
        ACTIVE_THEME.with(|active| *active.borrow_mut() = self.prev.take());
    }
}

/// Test helper: hold `theme` for this thread until the guard drops.
pub fn hold_external_theme(theme: ExternalTheme) -> ThemeGuard {
    ACTIVE_THEME.with(|active| {
        let prev = active.borrow().clone();
        *active.borrow_mut() = Some(theme);
        ThemeGuard { prev }
    })
}

/// Map a snake_case role name to its [`Role`]. Dashes are accepted too
/// (`tab-active` reads as `tab_active`).
pub fn role_from_name(name: &str) -> Option<Role> {
    match name.to_ascii_lowercase().replace('-', "_").as_str() {
        "text" => Some(Role::Text),
        "muted" => Some(Role::Muted),
        "success" => Some(Role::Success),
        "running" => Some(Role::Running),
        "starting" => Some(Role::Starting),
        "exited" => Some(Role::Exited),
        "warning" => Some(Role::Warning),
        "danger" => Some(Role::Danger),
        "info" => Some(Role::Info),
        "brand" => Some(Role::Brand),
        "command" => Some(Role::Command),
        "focus" => Some(Role::Focus),
        "tab_active" => Some(Role::TabActive),
        "tab_inactive" => Some(Role::TabInactive),
        "border_focused" => Some(Role::BorderFocused),
        "border_unfocused" => Some(Role::BorderUnfocused),
        "border_modal" => Some(Role::BorderModal),
        "key_hint" => Some(Role::KeyHint),
        "key_desc" => Some(Role::KeyDesc),
        _ => None,
    }
}

/// Parse one color value: a named ANSI color (case-insensitive, `grey`
/// accepted for `gray`) or a `#rrggbb` / `#rgb` hex string.
pub fn parse_color(text: &str) -> Result<Color, String> {
    let t = text.trim();
    if let Some(hex) = t.strip_prefix('#') {
        return parse_hex_color(hex).ok_or_else(|| {
            format!("bad color {text:?}: expected #rrggbb or #rgb hex")
        });
    }
    match t.to_ascii_lowercase().as_str() {
        "black" => Ok(Color::Black),
        "red" => Ok(Color::Red),
        "green" => Ok(Color::Green),
        "yellow" => Ok(Color::Yellow),
        "blue" => Ok(Color::Blue),
        "magenta" => Ok(Color::Magenta),
        "cyan" => Ok(Color::Cyan),
        "gray" | "grey" => Ok(Color::Gray),
        "darkgray" | "darkgrey" | "dark-gray" | "dark-grey" => Ok(Color::DarkGray),
        "lightred" | "light-red" => Ok(Color::LightRed),
        "lightgreen" | "light-green" => Ok(Color::LightGreen),
        "lightyellow" | "light-yellow" => Ok(Color::LightYellow),
        "lightblue" | "light-blue" => Ok(Color::LightBlue),
        "lightmagenta" | "light-magenta" => Ok(Color::LightMagenta),
        "lightcyan" | "light-cyan" => Ok(Color::LightCyan),
        "white" => Ok(Color::White),
        _ => Err(format!(
            "bad color {text:?}: expected a named ANSI color or #rrggbb hex"
        )),
    }
}

fn parse_hex_color(hex: &str) -> Option<Color> {
    if hex.len() == 3 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        let mut it = hex.chars();
        let r = it.next()?.to_digit(16)? as u8;
        let g = it.next()?.to_digit(16)? as u8;
        let b = it.next()?.to_digit(16)? as u8;
        return Some(Color::Rgb(r * 17, g * 17, b * 17));
    }
    if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        let v = u32::from_str_radix(hex, 16).ok()?;
        return Some(Color::Rgb(
            ((v >> 16) & 0xff) as u8,
            ((v >> 8) & 0xff) as u8,
            (v & 0xff) as u8,
        ));
    }
    None
}

fn parse_modifier(name: &str) -> Option<Modifier> {
    match name.to_ascii_lowercase().replace('-', "_").as_str() {
        "bold" => Some(Modifier::BOLD),
        "italic" => Some(Modifier::ITALIC),
        "underlined" | "underline" => Some(Modifier::UNDERLINED),
        "reversed" | "reverse" => Some(Modifier::REVERSED),
        "dim" => Some(Modifier::DIM),
        "blink" => Some(Modifier::SLOW_BLINK),
        "crossedout" | "crossed_out" | "strikethrough" => Some(Modifier::CROSSED_OUT),
        "hidden" => Some(Modifier::HIDDEN),
        _ => None,
    }
}

fn reject_unknown(
    obj: &serde_json::Map<String, serde_json::Value>,
    known: &[&str],
    ctx: &str,
) -> Result<(), String> {
    for key in obj.keys() {
        if !known.contains(&key.as_str()) {
            return Err(format!("unknown {ctx} field {key:?}"));
        }
    }
    Ok(())
}

/// Parse one `theme.json` document. Manual over `serde_json::Value`
/// (no derive in this tree), mirroring `agents.rs`: unknown fields are
/// rejected so typos fail loudly instead of silently doing nothing.
pub fn parse_external_theme(text: &str) -> Result<ExternalTheme, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}"))?;
    let top = value
        .as_object()
        .ok_or_else(|| "top level must be an object".to_string())?;
    reject_unknown(
        top,
        &["name", "colors", "buttons", "borders", "highlight"],
        "top level",
    )?;
    let name = match top.get("name") {
        None => "theme".to_string(),
        Some(v) => v
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_string())
            .ok_or_else(|| "name must be a non-empty string".to_string())?,
    };
    let mut overrides = std::collections::HashMap::new();
    if let Some(colors) = top.get("colors") {
        let map = colors
            .as_object()
            .ok_or_else(|| "colors must be an object".to_string())?;
        for (key, raw) in map {
            let Some(role) = role_from_name(key) else {
                return Err(format!("unknown color role {key:?}"));
            };
            overrides.insert(role, parse_role_value(raw, key)?);
        }
    }
    let (button_left, button_right) = match top.get("buttons") {
        None => (BUILTIN_PILL_LEFT, BUILTIN_PILL_RIGHT),
        Some(v) => {
            let map = v
                .as_object()
                .ok_or_else(|| "buttons must be an object".to_string())?;
            reject_unknown(map, &["left", "right"], "buttons")?;
            (parse_cap(map.get("left"), BUILTIN_PILL_LEFT, "buttons.left")?,
             parse_cap(map.get("right"), BUILTIN_PILL_RIGHT, "buttons.right")?)
        }
    };
    let borders = match top.get("borders") {
        None => ratatui::widgets::BorderType::Plain,
        Some(v) => {
            let s = v
                .as_str()
                .ok_or_else(|| "borders must be a string".to_string())?;
            parse_border_type(s)?
        }
    };
    let highlight = match top.get("highlight") {
        None => ButtonHighlight::Full,
        Some(v) => {
            let s = v
                .as_str()
                .ok_or_else(|| "highlight must be a string".to_string())?;
            parse_highlight(s)?
        }
    };
    Ok(ExternalTheme {
        name,
        overrides,
        button_left,
        button_right,
        borders,
        highlight,
    })
}

fn parse_role_value(raw: &serde_json::Value, role: &str) -> Result<RoleOverride, String> {
    if let Some(s) = raw.as_str() {
        return Ok(RoleOverride {
            fg: Some(parse_color(s)?),
            bg: None,
            modifiers: None,
        });
    }
    let map = raw
        .as_object()
        .ok_or_else(|| format!("colors.{role} must be a color string or object"))?;
    reject_unknown(map, &["fg", "bg", "modifiers"], &format!("colors.{role}"))?;
    let fg = match map.get("fg") {
        None => None,
        Some(v) => Some(parse_color(
            v.as_str()
                .ok_or_else(|| format!("colors.{role}.fg must be a string"))?,
        )?),
    };
    let bg = match map.get("bg") {
        None => None,
        Some(v) => Some(parse_color(
            v.as_str()
                .ok_or_else(|| format!("colors.{role}.bg must be a string"))?,
        )?),
    };
    let modifiers = match map.get("modifiers") {
        None => None,
        Some(v) => {
            let arr = v
                .as_array()
                .ok_or_else(|| format!("colors.{role}.modifiers must be a list"))?;
            let mut out = Modifier::empty();
            for item in arr {
                let s = item
                    .as_str()
                    .ok_or_else(|| format!("colors.{role}.modifiers must be strings"))?;
                out |= parse_modifier(s)
                    .ok_or_else(|| format!("unknown modifier {s:?} for colors.{role}"))?;
            }
            Some(out)
        }
    };
    Ok(RoleOverride { fg, bg, modifiers })
}

fn parse_cap(
    raw: Option<&serde_json::Value>,
    fallback: char,
    ctx: &str,
) -> Result<char, String> {
    let Some(v) = raw else { return Ok(fallback) };
    let s = v
        .as_str()
        .ok_or_else(|| format!("{ctx} must be a single-character string"))?;
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Ok(c),
        _ => Err(format!("{ctx} must be a single-character string")),
    }
}

fn parse_highlight(s: &str) -> Result<ButtonHighlight, String> {
    match s.to_ascii_lowercase().as_str() {
        "full" => Ok(ButtonHighlight::Full),
        "left" => Ok(ButtonHighlight::Left),
        _ => Err(format!(
            "bad highlight {s:?}: expected full or left"
        )),
    }
}

fn parse_border_type(s: &str) -> Result<ratatui::widgets::BorderType, String> {
    use ratatui::widgets::BorderType as B;
    match s.to_ascii_lowercase().replace('-', "_").as_str() {
        "plain" | "single" => Ok(B::Plain),
        "rounded" => Ok(B::Rounded),
        "double" => Ok(B::Double),
        "thick" => Ok(B::Thick),
        _ => Err(format!(
            "bad borders {s:?}: expected plain, rounded, double or thick"
        )),
    }
}

/// Theme files under `dir`: `theme.json` at the top, any `*.json`
/// directly inside, and any `*/theme.json` one level down. Sorted and
/// capped so a hostile directory cannot grow the picker without bound.
pub fn discover_theme_files(dir: &Path) -> Vec<std::path::PathBuf> {
    const CAP: usize = 256;
    let mut out = Vec::new();
    let top = dir.join("theme.json");
    if top.is_file() {
        out.push(top);
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        if out.len() >= CAP {
            break;
        }
        let path = entry.path();
        if path.is_file()
            && path.extension().is_some_and(|e| e == "json")
            && path.file_name().is_some_and(|n| n != "theme.json")
        {
            out.push(path);
        } else if path.is_dir() {
            let nested = path.join("theme.json");
            if nested.is_file() {
                out.push(nested);
            }
        }
    }
    out.sort();
    out.truncate(CAP);
    out
}

/// Read and parse one theme file. A missing `name` falls back to the
/// file stem (`tokyo-night.json`) or the parent directory name for
/// `*/theme.json`, so minimal files stay one `colors` object.
pub fn load_external_theme_file(path: &Path) -> Result<ExternalTheme, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("{}: cannot read theme file: {e}", path.display()))?;
    let mut theme =
        parse_external_theme(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if theme.name == "theme" {
        if let Some(stem) = fallback_theme_name(path) {
            theme.name = stem;
        }
    }
    Ok(theme)
}

fn fallback_theme_name(path: &Path) -> Option<String> {
    if path.file_name().is_some_and(|n| n == "theme.json") {
        if let Some(parent) = path.parent().and_then(|p| p.file_name()) {
            let s = parent.to_string_lossy().trim().to_string();
            if !s.is_empty() && s != "themes" {
                return Some(s);
            }
        }
        return None;
    }
    path.file_stem()
        .map(|s| s.to_string_lossy().trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Every valid theme under `dir`, sorted by name (case-insensitive).
/// Invalid files are skipped so one broken JSON never hides the rest;
/// the builtin `default` is always index 0.
pub fn list_external_themes(dir: &Path) -> Vec<ExternalTheme> {
    let mut themes = vec![ExternalTheme::builtin()];
    for path in discover_theme_files(dir) {
        if let Ok(theme) = load_external_theme_file(&path) {
            if themes.iter().all(|t| t.name != theme.name) {
                themes.push(theme);
            }
        }
    }
    themes[1..].sort_by(|a, b| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()));
    themes
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

/// Style for a role: builtin, plus any active external theme, plus
/// any ambient OS-theme override (which still wins for absolute roles
/// so light terminals stay readable).
pub fn style(role: Role) -> Style {
    let mut base = builtin_style(role);
    ACTIVE_THEME.with(|active| {
        if let Some(theme) = active.borrow().as_ref() {
            if let Some(ov) = theme.overrides.get(&role) {
                if let Some(fg) = ov.fg {
                    base = base.fg(fg);
                }
                if let Some(bg) = ov.bg {
                    base = base.bg(bg);
                }
                if let Some(mods) = ov.modifiers {
                    base = Style {
                        fg: base.fg,
                        bg: base.bg,
                        underline_color: base.underline_color,
                        add_modifier: mods,
                        sub_modifier: base.sub_modifier,
                    };
                }
            }
        }
    });
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
    fn external_theme_parses_colors_and_button_caps() {
        let text = r##"{
            "name": "demo",
            "colors": {"brand": "#ff0000", "info": "cyan"},
            "buttons": {"left": "[", "right": "]"},
            "borders": "rounded"
        }"##;
        let theme = parse_external_theme(text).expect("valid theme parses");
        assert_eq!(theme.name, "demo");
        assert_eq!(theme.button_left, '[');
        assert_eq!(theme.button_right, ']');
        let _guard = hold_external_theme(theme);
        assert_eq!(style(Role::Brand).fg, Some(Color::Rgb(255, 0, 0)));
        assert_eq!(pill_left(), '[');
        assert_eq!(pill_right(), ']');
    }

    #[test]
    fn external_theme_parses_highlight_modes() {
        let left = parse_external_theme(r##"{"name": "t", "highlight": "left"}"##)
            .expect("left parses");
        assert_eq!(left.highlight, ButtonHighlight::Left);
        let full = parse_external_theme(r##"{"name": "t", "highlight": "full"}"##)
            .expect("full parses");
        assert_eq!(full.highlight, ButtonHighlight::Full);
        let dflt = parse_external_theme(r##"{"name": "t"}"##).expect("default parses");
        assert_eq!(dflt.highlight, ButtonHighlight::Full);
        assert!(
            parse_external_theme(r##"{"highlight": "glow"}"##).is_err(),
            "bad highlight"
        );
    }

    #[test]
    fn button_chrome_splits_highlight_per_mode() {
        let accent = style(Role::TabActive);
        let rest = style(Role::TabInactive);
        // Full: the whole button highlights.
        let _guard = hold_external_theme(ExternalTheme::builtin());
        assert_eq!(
            button_chrome(true, accent, rest, Color::DarkGray),
            (accent, Color::Yellow, Color::Yellow)
        );
        assert_eq!(
            button_chrome(false, accent, rest, Color::DarkGray),
            (rest, Color::DarkGray, Color::DarkGray)
        );
        // Left: only the left bookend carries the selection; the
        // button keeps its rest color.
        let theme =
            parse_external_theme(r##"{"name": "t", "highlight": "left"}"##).unwrap();
        let _guard = hold_external_theme(theme);
        assert_eq!(
            button_chrome(true, accent, rest, Color::DarkGray),
            (rest, Color::Yellow, Color::DarkGray)
        );
        assert_eq!(
            button_chrome(false, accent, rest, Color::DarkGray),
            (rest, Color::DarkGray, Color::DarkGray)
        );
    }

    #[test]
    fn external_theme_rejects_bad_fields() {
        assert!(parse_external_theme(r##"{"name": "x", "bogus": 1}"##).is_err(), "unknown top field");
        assert!(parse_external_theme(r##"{"colors": {"frob": "red"}}"##).is_err(), "unknown role");
        assert!(parse_external_theme(r##"{"colors": {"brand": "chartreuse"}}"##).is_err(), "bad color");
        assert!(parse_external_theme(r##"{"buttons": {"left": "ab"}}"##).is_err(), "multi-char cap");
        assert!(parse_external_theme(r##"{"buttons": {"left": ""}}"##).is_err(), "empty cap");
        assert!(parse_external_theme(r##"{"borders": "groovy"}"##).is_err(), "bad borders");
        assert!(parse_external_theme("not json").is_err(), "garbage");
    }

    #[test]
    fn external_theme_object_form_sets_bg_and_modifiers() {
        let text = r##"{
            "name": "obj",
            "colors": {"tab_active": {"fg": "black", "bg": "#ffaf00", "modifiers": ["bold"]}}
        }"##;
        let theme = parse_external_theme(text).expect("object form parses");
        let _guard = hold_external_theme(theme);
        let s = style(Role::TabActive);
        assert_eq!(s.fg, Some(Color::Black));
        assert_eq!(s.bg, Some(Color::Rgb(255, 175, 0)));
        assert!(s.add_modifier.contains(Modifier::BOLD));
        // Unmentioned roles keep the builtin look.
        assert_eq!(style(Role::Brand).fg, Some(Color::Yellow));
    }

    #[test]
    fn external_theme_names_fall_back_to_file_stem() {
        let dir = std::env::temp_dir().join(format!(
            "forge-ext-theme-{}-stem",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("tokyo-night.json"),
            r##"{"colors": {"brand": "cyan"}}"##,
        )
        .unwrap();
        std::fs::write(dir.join("broken.json"), "nope [[[").unwrap();
        let themes = list_external_themes(&dir);
        assert_eq!(themes[0].name, "default", "builtin first");
        assert!(themes.iter().any(|t| t.name == "tokyo-night"), "stem name: {:?}", themes.iter().map(|t| &t.name).collect::<Vec<_>>());
        assert!(!themes.iter().any(|t| t.name == "broken"), "invalid skipped");
        let files = discover_theme_files(&dir);
        assert_eq!(files.len(), 2, "both files discovered");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn packaged_example_themes_parse() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/themes");
        let mut count = 0;
        for entry in std::fs::read_dir(&dir).expect("assets/themes exists") {
            let path = entry.expect("dir entry").path();
            if path.extension().is_some_and(|e| e == "json") {
                let theme = load_external_theme_file(&path)
                    .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                assert!(!theme.name.trim().is_empty(), "named theme");
                count += 1;
            }
        }
        assert!(count >= 3, "ships square, round, tokyo-night");
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
