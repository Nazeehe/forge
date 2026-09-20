//! Telegram settings dialog: the `[telegram]` section as a form.
//!
//! Tui-realm inputs mirror [`crate::create::CreateDialog`]: Tab cycles
//! rows, typing edits the focused input, Enter submits (or activates the
//! focused action), Esc cancels. Validation errors stay in a fixed slot
//! so the layout never shifts. Submitting never touches the network:
//! the token writes to its file atomically (`0600`) and the section
//! saves to `config.toml` through [`crate::config::LoadedConfig`].

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use tui_realm_stdlib::components::{Input, Radio, Select};
use tuirealm::command::{Cmd, Direction};
use tuirealm::component::Component;
use tuirealm::state::{State, StateValue};

use crate::config::TelegramConfig;

/// Focus rows in Tab order. The token lives at
/// [`crate::telegram::TELEGRAM_TOKEN_FILE`] and has no row.
const FOCUS_ENABLED: usize = 0;
const FOCUS_TOKEN: usize = 1;
const FOCUS_ALLOWED: usize = 2;
const FOCUS_NOTIFY: usize = 3;
const FOCUS_POLL: usize = 4;
const FOCUS_BMIN: usize = 5;
const FOCUS_BMAX: usize = 6;
const FOCUS_ACTIONS: usize = 7;
const FIELD_COUNT: usize = 8;

/// Validated dialog output: the section plus the new token
/// (empty keeps the existing file).
#[derive(Clone, Debug)]
pub struct TelegramForm {
    pub config: TelegramConfig,
    pub token: String,
}

/// Dialog result after one input: still open, cancelled, submitted,
/// or a connection-test request (which also stays open).
#[derive(Debug)]
pub enum TelegramOutcome {
    Pending,
    Cancelled,
    Submitted(TelegramForm),
    Test { token: String },
}

/// Settings form: one select, five text inputs, and a three-way action
/// row. Numeric rows start empty and fall back to blueprint defaults on
/// submit; placeholders show those defaults. The token file path is
/// fixed ([`crate::telegram::TELEGRAM_TOKEN_FILE`]), never a row.
pub struct TelegramDialog {
    enabled: Select,
    token: Input,
    allowed: Input,
    notify: Input,
    poll: Input,
    bmin: Input,
    bmax: Input,
    token_file: String,
    poll_default: u64,
    bmin_default: u64,
    bmax_default: u64,
    actions: Radio,
    pills: bool,
    focus: usize,
    error: Option<String>,
    testing: bool,
    test_result: Option<(bool, String)>,
}

impl TelegramDialog {
    /// Prefill from the live section. The token row always starts empty
    /// (blank keeps the file); numeric rows start empty and show their
    /// defaults as placeholders.
    pub fn new(cfg: &TelegramConfig, pills: bool) -> Self {
        let enabled = Select::default()
            .choices(vec!["Off".to_string(), "On".to_string()])
            .value(usize::from(cfg.enabled))
            .rewind(true);
        let allowed = Input::default().title("Allowed IDs").value(
            cfg.allowed_user_ids
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(", "),
        );
        let notify = Input::default()
            .title("Notify chat")
            .value(if cfg.notify_chat_id == 0 {
                String::new()
            } else {
                cfg.notify_chat_id.to_string()
            });
        let actions = Radio::default()
            .choices(["Save", "Test", "Cancel"])
            .value(0)
            .rewind(true);
        TelegramDialog {
            enabled,
            token: Input::default().title("Token"),
            allowed,
            notify,
            poll: Input::default().title("Poll secs"),
            bmin: Input::default().title("Backoff min"),
            bmax: Input::default().title("Backoff max"),
            token_file: if cfg.token_file.is_empty() {
                crate::telegram::TELEGRAM_TOKEN_FILE.to_string()
            } else {
                cfg.token_file.clone()
            },
            poll_default: cfg.poll_seconds,
            bmin_default: cfg.backoff_min_seconds,
            bmax_default: cfg.backoff_max_seconds,
            actions,
            pills,
            focus: 0,
            error: None,
            testing: false,
            test_result: None,
        }
    }

    #[cfg(test)]
    pub fn focus(&self) -> usize {
        self.focus
    }

    #[cfg(test)]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    #[cfg(test)]
    pub fn test_result(&self) -> Option<(bool, String)> {
        self.test_result.clone()
    }

    pub fn set_testing(&mut self, testing: bool) {
        self.testing = testing;
    }

    pub fn set_error(&mut self, error: String) {
        self.error = Some(error);
    }

    /// Paste bracketed-paste text into the focused text row, one char
    /// at a time like typing. Option rows ignore it.
    pub fn paste(&mut self, text: &str) {
        let field = match self.focus {
            FOCUS_TOKEN => &mut self.token,
            FOCUS_ALLOWED => &mut self.allowed,
            FOCUS_NOTIFY => &mut self.notify,
            FOCUS_POLL => &mut self.poll,
            FOCUS_BMIN => &mut self.bmin,
            FOCUS_BMAX => &mut self.bmax,
            _ => return,
        };
        for ch in text.chars() {
            field.perform(Cmd::Type(ch));
        }
        self.error = None;
        self.test_result = None;
    }

    pub fn set_test_result(&mut self, ok: bool, detail: String) {
        self.testing = false;
        self.test_result = Some((ok, detail));
    }

    fn text_of(input: &Input) -> String {
        match Component::state(input) {
            State::Single(StateValue::String(s)) => s,
            _ => String::new(),
        }
    }

    fn submit(&mut self) -> TelegramOutcome {
        let enabled = self.enabled.states.selected == 1;
        let token = Self::text_of(&self.token);
        if !token.is_empty() && token.len() < crate::telegram::TOKEN_MIN_LEN {
            self.error = Some("token too short".to_string());
            return TelegramOutcome::Pending;
        }
        let allowed = match parse_ids(&Self::text_of(&self.allowed)) {
            Ok(ids) => ids,
            Err(e) => {
                self.error = Some(e);
                return TelegramOutcome::Pending;
            }
        };
        let notify_chat_id = match parse_opt_i64(&Self::text_of(&self.notify)) {
            Ok(v) => v,
            Err(e) => {
                self.error = Some(format!("notify chat {e}"));
                return TelegramOutcome::Pending;
            }
        };
        let poll_seconds = match parse_opt_u64(&Self::text_of(&self.poll), self.poll_default) {
            Ok(v) if v >= 1 => v,
            Ok(_) => {
                self.error = Some("poll seconds must be at least 1".to_string());
                return TelegramOutcome::Pending;
            }
            Err(_) => {
                self.error = Some("poll seconds must be a number".to_string());
                return TelegramOutcome::Pending;
            }
        };
        let backoff_min_seconds = match parse_opt_u64(&Self::text_of(&self.bmin), self.bmin_default) {
            Ok(v) => v,
            Err(_) => {
                self.error = Some("backoff seconds must be numbers".to_string());
                return TelegramOutcome::Pending;
            }
        };
        let backoff_max_seconds = match parse_opt_u64(&Self::text_of(&self.bmax), self.bmax_default) {
            Ok(v) => v,
            Err(_) => {
                self.error = Some("backoff seconds must be numbers".to_string());
                return TelegramOutcome::Pending;
            }
        };
        if backoff_min_seconds > backoff_max_seconds {
            self.error = Some("backoff min must not exceed backoff max".to_string());
            return TelegramOutcome::Pending;
        }
        self.error = None;
        TelegramOutcome::Submitted(TelegramForm {
            config: TelegramConfig {
                enabled,
                token_file: self.token_file.clone(),
                allowed_user_ids: allowed,
                notify_chat_id,
                poll_seconds,
                backoff_min_seconds,
                backoff_max_seconds,
            },
            token,
        })
    }

    /// One key: Tab/BackTab/Up/Down cycle rows, Left/Right drives the
    /// enabled cycler and the action radio (or the text cursor),
    /// typing edits the focused input, Enter submits (or runs the
    /// focused action), Esc cancels.
    pub fn key(&mut self, key: &KeyEvent) -> TelegramOutcome {
        match key.code {
            KeyCode::Esc => return TelegramOutcome::Cancelled,
            KeyCode::Tab => {
                self.focus = (self.focus + 1) % FIELD_COUNT;
                return TelegramOutcome::Pending;
            }
            KeyCode::BackTab => {
                self.focus = (self.focus + FIELD_COUNT - 1) % FIELD_COUNT;
                return TelegramOutcome::Pending;
            }
            KeyCode::Up => {
                self.focus = (self.focus + FIELD_COUNT - 1) % FIELD_COUNT;
                return TelegramOutcome::Pending;
            }
            KeyCode::Down => {
                self.focus = (self.focus + 1) % FIELD_COUNT;
                return TelegramOutcome::Pending;
            }
            KeyCode::Enter => {
                if self.focus == FOCUS_ACTIONS {
                    return self.fire_action(self.actions.states.choice);
                }
                return self.submit();
            }
            _ => {}
        }
        match self.focus {
            FOCUS_ENABLED => match key.code {
                KeyCode::Left => cycle_select(&mut self.enabled, -1),
                KeyCode::Right => cycle_select(&mut self.enabled, 1),
                _ => {}
            },
            FOCUS_ACTIONS => match key.code {
                KeyCode::Left => {
                    self.actions.perform(Cmd::Move(Direction::Left));
                }
                KeyCode::Right => {
                    self.actions.perform(Cmd::Move(Direction::Right));
                }
                _ => {}
            },
            _ => {
                let field = match self.focus {
                    FOCUS_TOKEN => &mut self.token,
                    FOCUS_ALLOWED => &mut self.allowed,
                    FOCUS_NOTIFY => &mut self.notify,
                    FOCUS_POLL => &mut self.poll,
                    FOCUS_BMIN => &mut self.bmin,
                    _ => &mut self.bmax,
                };
                match key.code {
                    KeyCode::Left => {
                        field.perform(Cmd::Move(Direction::Left));
                    }
                    KeyCode::Right => {
                        field.perform(Cmd::Move(Direction::Right));
                    }
                    KeyCode::Backspace => {
                        field.perform(Cmd::Delete);
                        self.error = None;
                        self.test_result = None;
                    }
                    KeyCode::Char(ch)
                        if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                    {
                        field.perform(Cmd::Type(ch));
                        self.error = None;
                        self.test_result = None;
                    }
                    _ => {}
                }
            }
        }
        TelegramOutcome::Pending
    }

    /// Fire one action button: Save submits, Test requests a
    /// connection check, anything else cancels. Shared by Enter and
    /// mouse so both paths agree.
    fn fire_action(&mut self, index: usize) -> TelegramOutcome {
        match index {
            0 => self.submit(),
            1 => {
                self.error = None;
                TelegramOutcome::Test {
                    token: Self::text_of(&self.token),
                }
            }
            _ => TelegramOutcome::Cancelled,
        }
    }

    /// Action-row spans the view paints: pill buttons with the accent
    /// default and `>` chosen-marker, or the legacy bracket labels
    /// without pills. [`Self::click`] hit-tests these same spans, so
    /// the mouse can never desync from the paint.
    fn action_spans(&self) -> Vec<ratatui::text::Span<'static>> {
        use ratatui::style::{Color, Style};
        use ratatui::text::Span;
        use crate::theme::{Role, focus_row, style};
        let text = style(Role::Text);
        let choice = self.actions.states.choice;
        let focused = self.focus == FOCUS_ACTIONS;
        if !self.pills {
            let button = |label: &str, index: usize, default: bool| {
                let tag = if default { format!("[{label}*]") } else { format!("[{label}]") };
                if focused && choice == index {
                    Span::styled(format!(">{tag}"), focus_row())
                } else if default {
                    Span::styled(tag, style(Role::Brand))
                } else {
                    Span::styled(tag, text)
                }
            };
            return vec![
                button("Save", 0, true),
                Span::styled("  ", text),
                button("Test", 1, false),
                Span::styled("  ", text),
                button("Cancel", 2, false),
            ];
        }
        let frame = |glyph: char, color: Color| {
            Span::styled(glyph.to_string(), Style::default().fg(color))
        };
        let mut spans = Vec::new();
        for (index, label) in ["Save", "Test", "Cancel"].iter().enumerate() {
            if index > 0 {
                spans.push(Span::styled("  ", text));
            }
            let default = index == 0;
            let chosen = focused && choice == index;
            let hot = default || chosen;
            if chosen {
                spans.push(Span::styled(">", focus_row()));
            }
            let (fill, left_cap, right_cap) = crate::theme::button_chrome(
                hot,
                style(Role::TabActive),
                style(Role::TabInactive),
                Color::DarkGray,
            );
            let tag = if default { format!("{label}*") } else { label.to_string() };
            spans.push(frame(crate::theme::pill_left(), left_cap));
            spans.push(Span::styled(" ", fill));
            spans.push(Span::styled(tag, fill));
            spans.push(Span::styled(" ", fill));
            spans.push(frame(crate::theme::pill_right(), right_cap));
        }
        spans
    }

    /// Left-click dispatch inside the modal rect the view paints:
    /// form rows take focus, action buttons fire at once. `None` is
    /// dead space or outside; the caller still swallows everything
    /// while the modal is open.
    pub fn click(&mut self, col: u16, row: u16, area: Rect) -> Option<TelegramOutcome> {
        use ratatui::widgets::{Block, Borders};
        if col < area.x || col >= area.right() || row < area.y || row >= area.bottom() {
            return None;
        }
        let inner = Block::default()
            .borders(Borders::ALL)
            .border_type(crate::theme::border_type())
            .inner(area);
        if inner.height < 14 || inner.width < 44 {
            return None;
        }
        let status_row = inner.y + inner.height.saturating_sub(2);
        for index in 0..FOCUS_ACTIONS {
            let r = inner.y + 1 + index as u16;
            if r >= status_row {
                break;
            }
            if row == r {
                self.focus = index;
                self.error = None;
                return Some(TelegramOutcome::Pending);
            }
        }
        let action_row = inner.y + 1 + FOCUS_ACTIONS as u16;
        if row != action_row || action_row >= status_row {
            return None;
        }
        let spans = self.action_spans();
        let cx = inner.x + 2;
        let cw = inner.width.saturating_sub(4);
        let width: u16 = spans.iter().map(|s| s.width() as u16).sum::<u16>().min(cw);
        // Buttons are the span groups between the `"  "` separators
        // (the `>` marker rides with its button); separators and the
        // clipped tail belong to nothing.
        let mut x = cx + cw.saturating_sub(width) / 2;
        let end = x + width;
        let mut index = 0usize;
        for span in &spans {
            let w = span.width() as u16;
            if x >= end {
                break;
            }
            if span.content == "  " {
                index += 1;
            } else if index < 3 && col >= x && col < x + w {
                self.actions.states.choice = index;
                self.focus = FOCUS_ACTIONS;
                self.error = None;
                return Some(self.fire_action(index));
            }
            x += w;
        }
        None
    }

    /// Centered modal: the seven-row form, one action row, one fixed
    /// status slot (error, test result, or testing), and a pinned key
    /// hint. Content keeps two cells of border padding.
    pub fn view(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        use ratatui::text::{Line, Span};
        use ratatui::widgets::{Block, Borders, Clear, Paragraph};
        use crate::theme::{Role, focus_row, style};
        frame.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
                .border_type(crate::theme::border_type())
            .title(" Telegram ")
            .style(crate::theme::modal_fill())
            .border_style(style(Role::BorderModal));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height < 14 || inner.width < 44 {
            return;
        }
        let text = style(Role::Text);
        let end = inner.y + inner.height;
        let cx = inner.x + 2;
        let cw = inner.width.saturating_sub(4);
        let hint_row = end.saturating_sub(1);
        let status_row = end.saturating_sub(2);
        let mut row = inner.y + 1;
        let label_w = 11u16;
        let content_w = cw.saturating_sub(2 + label_w) as usize;
        let line_at = |label: &str, focused: bool, control: Vec<Span<'static>>| {
            let row_style = if focused { focus_row() } else { text };
            Line::from(
                std::iter::once(if focused {
                    Span::styled("> ", focus_row())
                } else {
                    Span::raw("  ")
                })
                .chain(std::iter::once(Span::styled(
                    format!("{label:<11}"),
                    row_style,
                )))
                .chain(control)
                .collect::<Vec<_>>(),
            )
        };
        // `[value]` box, padded to fill; empty shows the dim placeholder.
        let field = |value: &str, focused: bool, placeholder: &str| -> Vec<Span> {
            let room = content_w.saturating_sub(2);
            let shown = if value.is_empty() { placeholder } else { value };
            let cut = fit(shown, room);
            let pad = room.saturating_sub(cut.chars().count());
            if focused {
                vec![Span::styled(format!("[{cut}{}]", " ".repeat(pad)), focus_row())]
            } else if value.is_empty() {
                vec![
                    Span::styled("[", text),
                    Span::styled(cut, style(Role::Muted)),
                    Span::styled(format!("{}]", " ".repeat(pad)), text),
                ]
            } else {
                vec![Span::styled(format!("[{cut}{}]", " ".repeat(pad)), text)]
            }
        };
        let select = |value: &str, focused: bool| -> Vec<Span> {
            let cut = fit(value, content_w.saturating_sub(4));
            let chev = style(Role::Muted);
            vec![
                Span::styled("< ", if focused { focus_row() } else { chev }),
                Span::styled(cut, if focused { focus_row() } else { text }),
                Span::styled(" >", if focused { focus_row() } else { chev }),
            ]
        };
        let token_shown = Self::text_of(&self.token);
        let token_display = if token_shown.is_empty() || self.focus == FOCUS_TOKEN {
            token_shown.clone()
        } else {
            "•".repeat(token_shown.chars().count().min(32))
        };
        let rows: Vec<(&str, Vec<Span>)> = vec![
            (
                "Enabled",
                select(
                    self.enabled.states.choices.get(self.enabled.states.selected).map(String::as_str).unwrap_or("Off"),
                    self.focus == FOCUS_ENABLED,
                ),
            ),
            ("Token", field(&token_display, self.focus == FOCUS_TOKEN, "keep current")),
            ("Allowed IDs", field(&Self::text_of(&self.allowed), self.focus == FOCUS_ALLOWED, "11, 22")),
            ("Notify chat", field(&Self::text_of(&self.notify), self.focus == FOCUS_NOTIFY, "unset")),
            ("Poll secs", field(&Self::text_of(&self.poll), self.focus == FOCUS_POLL, "20")),
            ("Backoff min", field(&Self::text_of(&self.bmin), self.focus == FOCUS_BMIN, "60")),
            ("Backoff max", field(&Self::text_of(&self.bmax), self.focus == FOCUS_BMAX, "900")),
        ];
        for (index, (label, control)) in rows.into_iter().enumerate() {
            if row >= status_row {
                return;
            }
            let focused = index == self.focus;
            frame.render_widget(
                Paragraph::new(line_at(label, focused, control)),
                Rect::new(cx, row, cw, 1),
            );
            row += 1;
        }
        // Centered action row: pill buttons (or bracket labels
        // without pills), the `>` marker tracking the chosen button.
        {
            let spans = self.action_spans();
            let width: u16 = spans.iter().map(|s| s.width() as u16).sum::<u16>().min(cw);
            let ax = cx + cw.saturating_sub(width) / 2;
            frame.render_widget(
                Paragraph::new(Line::from(spans)),
                Rect::new(ax, row, width, 1),
            );
            row += 1;
            let _ = row;
        }
        // One fixed status slot: error, test result, testing spinner
        // text, or blank. State changes never move surrounding rows.
        let status = if let Some(err) = &self.error {
            Line::from(Span::styled(fit(err, cw as usize), style(Role::Danger)))
        } else if let Some((ok, detail)) = &self.test_result {
            let role = if *ok { Role::Success } else { Role::Danger };
            let mark = if *ok { "✓" } else { "!" };
            Line::from(Span::styled(
                fit(&format!("{mark} test {detail}"), cw as usize),
                style(role),
            ))
        } else if self.testing {
            Line::from(Span::styled("… testing", style(Role::Muted)))
        } else {
            Line::from("")
        };
        frame.render_widget(Paragraph::new(status), Rect::new(cx, status_row, cw, 1));
        // Pinned one-line key hint.
        let key_style = style(Role::KeyHint);
        let hint = Line::from(vec![
            Span::styled("Tab next   ", key_style),
            Span::styled("Enter choose   ", key_style),
            Span::styled("Esc cancel", key_style),
        ]);
        frame.render_widget(Paragraph::new(hint), Rect::new(cx, hint_row, cw, 1));
    }
}

/// Centered dialog box: 78 wide, 18 tall for the eight-row form plus
/// action, status, and hint rows; clamped into tiny terminals.
pub fn telegram_area(term: Rect) -> Rect {
    let (w, h) = (78.min(term.width), 18.min(term.height));
    Rect::new(
        term.x + term.width.saturating_sub(w) / 2,
        term.y + term.height.saturating_sub(h) / 2,
        w,
        h,
    )
}

/// Cycle a closed select with wraparound; empty choice lists are a no-op.
fn cycle_select(select: &mut Select, dir: i32) {
    let len = select.states.choices.len() as i32;
    if len == 0 {
        return;
    }
    let next = (select.states.selected as i32 + dir).rem_euclid(len) as usize;
    select.states.select(next);
}

/// Truncate to `room` chars at the end.
fn fit(s: &str, room: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= room {
        s.to_string()
    } else {
        chars[..room].iter().collect()
    }
}

/// Comma/space-separated Telegram user IDs. Empty means none (delivery
/// fails closed until the operator configures users).
fn parse_ids(raw: &str) -> Result<Vec<i64>, String> {
    let mut out = Vec::new();
    for part in raw.split([',', ' ', '\t', '\n']) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        out.push(
            part.parse::<i64>()
                .map_err(|_| "allowed user IDs must be integers like \"11, 22\"".to_string())?,
        );
    }
    Ok(out)
}

/// Optional integer row: blank falls back to `default`.
fn parse_opt_i64(raw: &str) -> Result<i64, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(0);
    }
    raw.parse::<i64>()
        .map_err(|_| "must be an integer".to_string())
}

/// Optional non-negative integer row: blank falls back to `default`.
fn parse_opt_u64(raw: &str, default: u64) -> Result<u64, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(default);
    }
    raw.parse::<u64>()
        .map_err(|_| "must be a number".to_string())
}

/// Expand a leading `~/` against `home`; anything else passes through
/// (absolute stays absolute, bare relative resolves against the
/// process working directory at read time).
pub fn expand_token_path(raw: &str, home: &Path) -> String {
    if let Some(rest) = raw.strip_prefix("~/") {
        return home.join(rest).to_string_lossy().into_owned();
    }
    raw.to_string()
}

/// Resolve a token-file path for tests and the save path.
#[allow(dead_code)]
fn token_path_buf(raw: &str, home: &Path) -> PathBuf {
    PathBuf::from(expand_token_path(raw, home))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn fresh() -> TelegramDialog {
        TelegramDialog::new(&crate::config::TelegramConfig::default(), true)
    }

    fn typed(dialog: &mut TelegramDialog, text: &str) {
        for ch in text.chars() {
            let outcome = dialog.key(&key(KeyCode::Char(ch)));
            assert!(matches!(outcome, TelegramOutcome::Pending), "typing {ch:?}");
        }
    }

    fn tab(dialog: &mut TelegramDialog, n: usize) {
        for _ in 0..n {
            assert!(matches!(dialog.key(&key(KeyCode::Tab)), TelegramOutcome::Pending));
        }
    }

    fn fill_valid(dialog: &mut TelegramDialog) {
        // Focus 0 is Enabled: Right turns it On. Token (row 1) stays
        // empty to keep the file; Allowed IDs is two tabs on.
        assert!(matches!(dialog.key(&key(KeyCode::Right)), TelegramOutcome::Pending));
        // IDs, notify chat, poll, backoff rows in order.
        tab(dialog, 2);
        typed(dialog, "11, 22");
        tab(dialog, 1);
        typed(dialog, "42");
        tab(dialog, 1);
        typed(dialog, "20");
        tab(dialog, 1);
        typed(dialog, "60");
        tab(dialog, 1);
        typed(dialog, "900");
    }

    #[test]
    fn esc_cancels_and_tab_cycles_all_rows() {
        let mut d = fresh();
        assert_eq!(d.focus(), 0);
        tab(&mut d, FIELD_COUNT);
        assert_eq!(d.focus(), 0, "tabs wrap the whole form");
        assert!(matches!(d.key(&key(KeyCode::Esc)), TelegramOutcome::Cancelled));
    }

    #[test]
    fn save_with_bad_ids_stays_open_with_error() {
        let mut d = fresh();
        tab(&mut d, 2);
        typed(&mut d, "abc");
        tab(&mut d, 5);
        let outcome = d.key(&key(KeyCode::Enter));
        assert!(matches!(outcome, TelegramOutcome::Pending), "bad IDs never submit");
        assert!(d.error().is_some_and(|e| e.contains("integer")), "error: {:?}", d.error());
    }

    #[test]
    fn valid_submit_parses_config() {
        let mut d = fresh();
        fill_valid(&mut d);
        tab(&mut d, 1);
        match d.key(&key(KeyCode::Enter)) {
            TelegramOutcome::Submitted(form) => {
                assert!(form.config.enabled);
                assert_eq!(form.config.token_file, crate::telegram::TELEGRAM_TOKEN_FILE);
                assert_eq!(form.config.allowed_user_ids, vec![11, 22]);
                assert_eq!(form.config.notify_chat_id, 42);
                assert_eq!(form.config.poll_seconds, 20);
                assert_eq!(form.config.backoff_min_seconds, 60);
                assert_eq!(form.config.backoff_max_seconds, 900);
                assert!(form.token.is_empty(), "empty token keeps the file");
            }
            other => panic!("expected submit, got {other:?}"),
        }
    }

    #[test]
    fn saving_preserves_configured_timing_values() {
        let cfg = crate::config::TelegramConfig {
            token_file: "/run/custom-forge.token".to_string(),
            poll_seconds: 7,
            backoff_min_seconds: 9,
            backoff_max_seconds: 33,
            ..crate::config::TelegramConfig::default()
        };
        let mut d = TelegramDialog::new(&cfg, true);
        tab(&mut d, 7);
        match d.key(&key(KeyCode::Enter)) {
            TelegramOutcome::Submitted(form) => {
                assert_eq!(form.config.poll_seconds, 7);
                assert_eq!(form.config.backoff_min_seconds, 9);
                assert_eq!(form.config.backoff_max_seconds, 33);
                assert_eq!(form.config.token_file, "/run/custom-forge.token");
            }
            other => panic!("expected submit, got {other:?}"),
        }
    }

    #[test]
    fn short_token_and_bad_timing_reject() {
        let mut d = fresh();
        tab(&mut d, 1);
        typed(&mut d, "tiny");
        tab(&mut d, 6);
        assert!(matches!(d.key(&key(KeyCode::Enter)), TelegramOutcome::Pending));
        assert!(d.error().is_some_and(|e| e.contains("short")), "error: {:?}", d.error());

        let mut d = fresh();
        fill_valid(&mut d);
        // Back on the min row ("60") and grow it past the max from
        // any cursor position.
        assert!(matches!(d.key(&key(KeyCode::BackTab)), TelegramOutcome::Pending));
        typed(&mut d, "9999");
        tab(&mut d, 2);
        assert!(matches!(d.key(&key(KeyCode::Enter)), TelegramOutcome::Pending));
        assert!(d.error().is_some_and(|e| e.contains("backoff")), "error: {:?}", d.error());
    }

    #[test]
    fn test_action_requests_without_closing() {
        let mut d = fresh();
        tab(&mut d, 1);
        typed(&mut d, "0123456789abcdef0123456789abcdef");
        tab(&mut d, 6);
        assert!(matches!(d.key(&key(KeyCode::Right)), TelegramOutcome::Pending));
        match d.key(&key(KeyCode::Enter)) {
            TelegramOutcome::Test { token, .. } => {
                assert_eq!(token, "0123456789abcdef0123456789abcdef");
            }
            other => panic!("expected test, got {other:?}"),
        }
        assert!(d.error().is_none(), "requesting a test is not an error");
    }

    #[test]
    fn paste_types_into_focused_text_row() {
        let mut d = fresh();
        d.paste("ignored-on-select");
        tab(&mut d, 2);
        d.paste("11");
        tab(&mut d, 5);
        match d.key(&key(KeyCode::Enter)) {
            TelegramOutcome::Submitted(form) => {
                assert!(!form.config.enabled, "select untouched by paste");
                assert_eq!(form.config.token_file, crate::telegram::TELEGRAM_TOKEN_FILE);
                assert_eq!(form.config.allowed_user_ids, vec![11]);
            }
            other => panic!("expected submit, got {other:?}"),
        }
    }

    #[test]
    fn expand_token_path_handles_home() {
        let home = std::path::Path::new("/h");
        assert_eq!(expand_token_path("~/t.token", home), "/h/t.token");
        assert_eq!(expand_token_path("/abs/t.token", home), "/abs/t.token");
    }

    #[test]
    fn render_centers_title_and_pins_hint() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut d = fresh();
        let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
        terminal.draw(|f| d.view(f, crate::telegram_dialog::telegram_area(f.area()))).unwrap();
        let buf = terminal.backend().buffer();
        let area = crate::telegram_dialog::telegram_area(ratatui::layout::Rect::new(0, 0, 100, 40));
        assert_eq!(area.x, (100 - area.width) / 2, "horizontally centered");
        let title: String = (area.x..area.x + area.width).map(|x| buf[(x, area.y)].symbol()).collect();
        assert!(title.contains("Telegram"), "title: {title:?}");
        // Hint pins to the bottom inner row; the error slot above it is
        // blank with no error set.
        let hint_y = area.y + area.height - 2;
        let hint: String = (area.x..area.x + area.width).map(|x| buf[(x, hint_y)].symbol()).collect();
        assert!(hint.contains("Esc"), "hint: {hint:?}");
        let slot_y = hint_y - 1;
        let slot: String = (area.x + 3..area.x + area.width - 3)
            .map(|x| buf[(x, slot_y)].symbol())
            .collect();
        assert!(slot.trim().is_empty(), "empty error slot: {slot:?}");
    }

    fn action_row_text(buf: &ratatui::buffer::Buffer, area: ratatui::layout::Rect) -> (u16, String) {
        // Seven form rows paint from the second inner row; the action
        // row follows immediately.
        let y = area.y + 2 + 7;
        let text: String = (area.x..area.x + area.width)
            .map(|x| buf[(x, y)].symbol())
            .collect();
        (y, text)
    }

    fn click_text(d: &mut TelegramDialog, area: ratatui::layout::Rect, y: u16, needle: &str) -> Option<TelegramOutcome> {
        use ratatui::{backend::TestBackend, Terminal};
        let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
        terminal.draw(|f| d.view(f, area)).unwrap();
        let buf = terminal.backend().buffer();
        // Cell columns, not byte offsets: pill caps are multi-byte.
        let cells: Vec<String> = (area.x..area.x + area.width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect();
        let start = (0..cells.len())
            .find(|&i| cells[i..].concat().starts_with(needle))
            .unwrap_or_else(|| panic!("{needle:?} visible: {:?}", cells.concat()));
        let mid = area.x + start as u16 + (needle.chars().count() as u16) / 2;
        d.click(mid, y, area)
    }

    #[test]
    fn action_row_uses_pill_buttons() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut d = TelegramDialog::new(&crate::config::TelegramConfig::default(), true);
        let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
        let area = crate::telegram_dialog::telegram_area(ratatui::layout::Rect::new(0, 0, 100, 40));
        terminal.draw(|f| d.view(f, area)).unwrap();
        let (_, text) = action_row_text(terminal.backend().buffer(), area);
        assert!(text.contains(crate::theme::pill_left()), "pill caps: {text:?}");
        assert!(text.contains("Save"), "save button: {text:?}");
    }

    #[test]
    fn click_save_submits_with_hardcoded_token_file() {
        let mut d = TelegramDialog::new(&crate::config::TelegramConfig::default(), true);
        assert!(matches!(d.key(&key(KeyCode::Right)), TelegramOutcome::Pending));
        let area = crate::telegram_dialog::telegram_area(ratatui::layout::Rect::new(0, 0, 100, 40));
        let y = area.y + 2 + 7;
        match click_text(&mut d, area, y, "Save") {
            Some(TelegramOutcome::Submitted(form)) => {
                assert_eq!(form.config.token_file, crate::telegram::TELEGRAM_TOKEN_FILE);
            }
            other => panic!("clicking Save submits, got {other:?}"),
        }
    }

    #[test]
    fn click_test_and_cancel_fire() {
        let mut d = TelegramDialog::new(&crate::config::TelegramConfig::default(), true);
        let area = crate::telegram_dialog::telegram_area(ratatui::layout::Rect::new(0, 0, 100, 40));
        let y = area.y + 2 + 7;
        assert!(matches!(click_text(&mut d, area, y, "Test"), Some(TelegramOutcome::Test { .. })));
        let mut d = TelegramDialog::new(&crate::config::TelegramConfig::default(), true);
        assert!(matches!(click_text(&mut d, area, y, "Cancel"), Some(TelegramOutcome::Cancelled)));
    }

    #[test]
    fn click_row_focuses_and_outside_is_ignored() {
        let mut d = TelegramDialog::new(&crate::config::TelegramConfig::default(), true);
        let area = crate::telegram_dialog::telegram_area(ratatui::layout::Rect::new(0, 0, 100, 40));
        // Allowed IDs is the third form row (Enabled, Token, Allowed).
        let row = area.y + 2 + 2;
        assert!(matches!(d.click(area.x + 5, row, area), Some(TelegramOutcome::Pending)));
        assert_eq!(d.focus(), 2);
        assert!(d.click(0, 0, area).is_none(), "outside the modal");
    }
}
