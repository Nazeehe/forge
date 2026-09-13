//! Permission modal (Phase 3d): tuirealm List driving Allow-once/Deny.
//!
//! Manual decisions are allow-once or deny: an allow answers this request
//! only (never cached), a deny is cached for identical requests. Escape
//! denies (fail-closed for humans). The modal captures all keyboard and
//! mouse input while open; clicks select-and-submit, clicks outside do
//! nothing.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use tui_realm_stdlib::components::List;
use tuirealm::command::{Cmd, Direction};
use tuirealm::component::Component;

/// What the harness asked about, in display-ready pieces.
#[derive(Clone, Debug)]
pub struct PermissionPrompt {
    pub hook: String,
    pub tool: String,
    pub command: String,
}

/// Modal result after one input: still open, or decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalOutcome {
    Pending,
    Decided { allow: bool },
}

/// Choice indices: allow-once first so Enter accepts.
pub const CHOICE_ALLOW_ONCE: usize = 0;
pub const CHOICE_DENY: usize = 1;

/// Centered modal box, clamped into tiny terminals.
pub fn modal_area(term: Rect) -> Rect {
    let (w, h) = (60.min(term.width), 12.min(term.height));
    Rect::new(
        term.x + term.width.saturating_sub(w) / 2,
        term.y + term.height.saturating_sub(h) / 2,
        w,
        h,
    )
}

/// Choice-list rows within a modal box: first row and visible count, shared
/// by rendering and click hit-testing so they can never disagree.
pub fn list_rows(box_area: Rect) -> Option<(u16, u16)> {
    let inner_h = box_area.height.saturating_sub(2);
    if inner_h < 5 {
        return None;
    }
    let mut list_h = (inner_h - 4).min(3);
    if list_h > 2 {
        list_h -= 1; // last line reserved for hints
    }
    Some((box_area.y + 1 + 4, list_h))
}

/// Permission modal: a stdlib List driven directly (our loop owns stdin,
/// so no tuirealm Application event pump). Clicks hit-test to rows.
pub struct PermissionModal {
    prompt: PermissionPrompt,
    list: List,
}

impl PermissionModal {
    pub fn new(prompt: PermissionPrompt) -> Self {
        let list = List::default()
            .rows(["Allow once", "Deny"])
            .rewind(true)
            .scroll(true)
            .selected_line(CHOICE_ALLOW_ONCE)
            .always_active()
            .highlight_str("▸");
        PermissionModal { prompt, list }
    }

    pub fn prompt(&self) -> &PermissionPrompt {
        &self.prompt
    }

    pub fn focused(&self) -> usize {
        self.list
            .states
            .list_index
    }

    /// Keyboard: arrows/Tab/j/k move, Enter submits, Esc denies.
    /// Anything else (typing) never moves focus.
    pub fn key(&mut self, key: &KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Up | KeyCode::Left | KeyCode::Char('k') | KeyCode::Char('h') => {
                self.list.perform(Cmd::Move(Direction::Up));
                ModalOutcome::Pending
            }
            KeyCode::Down
            | KeyCode::Right
            | KeyCode::Tab
            | KeyCode::Char('j')
            | KeyCode::Char('l') => {
                self.list.perform(Cmd::Move(Direction::Down));
                ModalOutcome::Pending
            }
            KeyCode::Enter => self.submit(),
            KeyCode::Esc => ModalOutcome::Decided { allow: false },
            _ => ModalOutcome::Pending,
        }
    }

    /// Click a choice row (0-based within the list): select and submit.
    /// Out-of-range rows do nothing.
    pub fn click(&mut self, row: usize) -> ModalOutcome {
        if row > CHOICE_DENY {
            return ModalOutcome::Pending;
        }
        while self.focused() != row {
            self.list.perform(Cmd::Move(Direction::Down));
        }
        self.submit()
    }

    fn submit(&self) -> ModalOutcome {
        use tuirealm::state::{State, StateValue};
        match self.list.state() {
            State::Single(StateValue::Usize(CHOICE_DENY)) => ModalOutcome::Decided { allow: false },
            State::Single(StateValue::Usize(_)) => ModalOutcome::Decided { allow: true },
            _ => ModalOutcome::Pending,
        }
    }

    /// Render the modal box: title, encoded request, choices, hints.
    pub fn view(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        use ratatui::text::{Line, Text};
        use ratatui::widgets::{Block, Borders, Paragraph};
        let prompt = self.prompt.clone();
        let body = Text::from(vec![
            Line::from(format!("Hook: {}", prompt.hook)),
            Line::from(format!("Tool: {}", prompt.tool)),
            Line::from(format!(
                "Command: {}",
                crate::safe_text::encode_for_display(&prompt.command)
            )),
            Line::from(""),
        ]);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" permission ")
            .style(crate::theme::style(crate::theme::Role::BorderFocused));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let rows: Vec<Line> = body.lines.into_iter().collect();
        let text_h = rows.len().min(inner.height as usize) as u16;
        frame.render_widget(
            Paragraph::new(Text::from(rows)),
            Rect::new(inner.x, inner.y, inner.width, text_h),
        );
        if let Some((list_y, list_h)) = list_rows(area) {
            self.list.view(
                frame,
                Rect::new(inner.x, list_y, inner.width, list_h),
            );
            if list_h < inner.height.saturating_sub(text_h).min(3) {
                frame.render_widget(
                    Paragraph::new("Enter choose • Esc deny"),
                    Rect::new(inner.x, list_y + list_h, inner.width, 1),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn prompt() -> PermissionPrompt {
        PermissionPrompt {
            hook: "PreToolUse".to_string(),
            tool: "Bash".to_string(),
            command: "rm -rf /tmp/x".to_string(),
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn enter_chooses_focused_allow_once_by_default() {
        let mut modal = PermissionModal::new(prompt());
        assert_eq!(modal.focused(), 0);
        assert_eq!(modal.key(&key(KeyCode::Enter)), ModalOutcome::Decided { allow: true });
    }

    #[test]
    fn arrows_and_tab_move_focus_then_enter_decides() {
        let mut modal = PermissionModal::new(prompt());
        assert_eq!(modal.key(&key(KeyCode::Down)), ModalOutcome::Pending);
        assert_eq!(modal.focused(), 1);
        assert_eq!(modal.key(&key(KeyCode::Up)), ModalOutcome::Pending);
        assert_eq!(modal.focused(), 0);
        assert_eq!(modal.key(&key(KeyCode::Tab)), ModalOutcome::Pending);
        assert_eq!(modal.focused(), 1);
        assert_eq!(
            modal.key(&key(KeyCode::Enter)),
            ModalOutcome::Decided { allow: false }
        );
    }

    #[test]
    fn escape_denies_and_letters_are_ignored() {
        let mut modal = PermissionModal::new(prompt());
        assert_eq!(modal.key(&key(KeyCode::Char('x'))), ModalOutcome::Pending);
        assert_eq!(modal.focused(), 0, "typing never moves focus");
        assert_eq!(modal.key(&key(KeyCode::Esc)), ModalOutcome::Decided { allow: false });
    }

    #[test]
    fn click_selects_and_submits() {
        let mut modal = PermissionModal::new(prompt());
        assert_eq!(modal.click(1), ModalOutcome::Decided { allow: false });
    }

    #[test]
    fn modal_area_centers_and_clamps() {
        let area = modal_area(ratatui::layout::Rect::new(0, 0, 120, 40));
        assert_eq!((area.width, area.height), (60, 12));
        assert_eq!((area.x, area.y), (30, 14));
        let tiny = modal_area(ratatui::layout::Rect::new(0, 0, 20, 8));
        assert!(tiny.width <= 20 && tiny.height <= 8);
    }
}
