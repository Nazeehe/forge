//! Generic Yes/No confirmation modal (quit forge, kill a session,
//! ...): No is the default, so Enter or Esc keeps running; only an
//! explicit Yes (or `y`) confirms.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;

/// Outcome of one key inside the confirm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmOutcome {
    Pending,
    Confirmed,
    Dismissed,
    /// Dirty mode: save the named files, then proceed.
    SaveDirty,
    /// Dirty mode: proceed without saving.
    DiscardDirty,
}

/// What the confirm modal is asking about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmKind {
    QuitForge,
    KillSession(crate::session::SessionId),
}

impl ConfirmKind {
    fn question(&self) -> &'static str {
        match self {
            ConfirmKind::QuitForge => "Are you sure you want to quit?",
            ConfirmKind::KillSession(_) => "Are you sure you want to kill this session?",
        }
    }
}

/// Yes/No choice; index 1 (No) is the default. With dirty files the
/// choice is Save/Discard/Cancel and index 2 (Cancel) is default,
/// so Enter never loses work.
pub struct Confirm {
    kind: ConfirmKind,
    choice: usize,
    pills: bool,
    dirty: Vec<String>,
}

impl Confirm {
    pub fn new(kind: ConfirmKind, pills: bool) -> Self {
        Confirm { kind, choice: 1, pills, dirty: Vec::new() }
    }

    /// Dirty variant: names the unsaved files and offers
    /// Save/Discard/Cancel instead of Yes/No.
    pub fn with_dirty(kind: ConfirmKind, pills: bool, dirty: Vec<String>) -> Self {
        let choice = if dirty.is_empty() { 1 } else { 2 };
        Confirm { kind, choice, pills, dirty }
    }

    /// Unsaved files this confirm guards, if any.
    pub fn dirty_files(&self) -> &[String] {
        &self.dirty
    }

    pub fn kind(&self) -> ConfirmKind {
        self.kind
    }

    /// Test hook: 0 is Yes, 1 is No.
    #[cfg(test)]
    pub fn choice(&self) -> usize {
        self.choice
    }

    pub fn key(&mut self, key: &KeyEvent) -> ConfirmOutcome {
        if !self.dirty.is_empty() {
            return self.dirty_key(key);
        }
        match key.code {
            KeyCode::Esc => ConfirmOutcome::Dismissed,
            KeyCode::Enter => {
                if self.choice == 0 {
                    ConfirmOutcome::Confirmed
                } else {
                    ConfirmOutcome::Dismissed
                }
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                self.choice = 1 - self.choice;
                ConfirmOutcome::Pending
            }
            KeyCode::Char('h') | KeyCode::Char('l') if key.modifiers.is_empty() => {
                self.choice = 1 - self.choice;
                ConfirmOutcome::Pending
            }
            KeyCode::Char('y') | KeyCode::Char('Y') if key.modifiers.is_empty() => {
                ConfirmOutcome::Confirmed
            }
            KeyCode::Char('n') | KeyCode::Char('N') if key.modifiers.is_empty() => {
                ConfirmOutcome::Dismissed
            }
            _ => ConfirmOutcome::Pending,
        }
    }

    /// Dirty-mode keys: three choices (Save/Discard/Cancel, Cancel
    /// default), s/d/c shortcuts; y/n stay meaningless and are
    /// ignored rather than guessed.
    fn dirty_key(&mut self, key: &KeyEvent) -> ConfirmOutcome {
        match key.code {
            KeyCode::Esc => ConfirmOutcome::Dismissed,
            KeyCode::Enter => self.outcome_for(self.choice),
            KeyCode::Left => {
                self.choice = (self.choice + 2) % 3;
                ConfirmOutcome::Pending
            }
            KeyCode::Right | KeyCode::Tab => {
                self.choice = (self.choice + 1) % 3;
                ConfirmOutcome::Pending
            }
            KeyCode::Char('s') | KeyCode::Char('S') if key.modifiers.is_empty() => {
                self.choice = 0;
                ConfirmOutcome::SaveDirty
            }
            KeyCode::Char('d') | KeyCode::Char('D') if key.modifiers.is_empty() => {
                self.choice = 1;
                ConfirmOutcome::DiscardDirty
            }
            KeyCode::Char('c') | KeyCode::Char('C') if key.modifiers.is_empty() => {
                self.choice = 2;
                ConfirmOutcome::Dismissed
            }
            _ => ConfirmOutcome::Pending,
        }
    }

    /// Outcome for a button index: Yes/No clean, Save/Discard/Cancel
    /// dirty. Shared by keys and clicks so they can never disagree.
    fn outcome_for(&self, choice: usize) -> ConfirmOutcome {
        if self.dirty.is_empty() {
            if choice == 0 {
                ConfirmOutcome::Confirmed
            } else {
                ConfirmOutcome::Dismissed
            }
        } else {
            match choice {
                0 => ConfirmOutcome::SaveDirty,
                1 => ConfirmOutcome::DiscardDirty,
                _ => ConfirmOutcome::Dismissed,
            }
        }
    }

    /// Fire the button under a cell, if any: sets the choice and
    /// returns its outcome. Shared rect math with the paint (the
    /// button row is always the third content row, centered the same
    /// way), so clicks can never desync from what is on screen.
    pub fn click(&mut self, col: u16, row: u16, area: Rect) -> Option<ConfirmOutcome> {
        use ratatui::widgets::{Block, Borders};
        let inner = Block::default().borders(Borders::ALL).inner(area);
        if row != inner.y.saturating_add(2) || inner.width < 10 {
            return None;
        }
        let labels = self.button_labels();
        let widths: Vec<usize> = labels
            .iter()
            .map(|label| self.button_width(label))
            .collect();
        let total: usize = widths.iter().sum::<usize>() + 2 * labels.len().saturating_sub(1);
        let mut x = inner.x.saturating_add(inner.width.saturating_sub(total as u16) / 2);
        for (index, width) in widths.iter().enumerate() {
            if col >= x && col < x.saturating_add(*width as u16) {
                self.choice = index;
                return Some(self.outcome_for(index));
            }
            x = x.saturating_add(*width as u16 + 2);
        }
        None
    }

    /// Button labels per mode.
    fn button_labels(&self) -> Vec<&'static str> {
        if self.dirty.is_empty() {
            vec!["Yes", "No"]
        } else {
            vec!["Save", "Discard", "Cancel"]
        }
    }

    /// Painted cell width of one button, both pill styles.
    fn button_width(&self, label: &str) -> usize {
        if self.pills {
            // Caps plus padded label, mirroring buttons() below.
            label.chars().count() + 2 + 2
        } else {
            // `>` marker plus `[label]`, mirroring buttons() below.
            1 + label.chars().count() + 2
        }
    }

    /// Render the centered modal: opaque, question, Yes/No buttons, hint.
    pub fn view(&self, frame: &mut ratatui::Frame, area: Rect) {
        use ratatui::text::{Line, Span};
        use ratatui::widgets::{Block, Borders, Clear, Paragraph};
        use crate::ui::theme::{Role, modal_fill, style};
        // Opaque: the live grid must not show through the modal.
        frame.render_widget(Clear, area);
        let title = match self.kind {
            ConfirmKind::QuitForge => " Quit ",
            ConfirmKind::KillSession(_) => " Kill session ",
        };
        let block = Block::default()
            .borders(Borders::ALL)
                .border_type(crate::ui::theme::border_type())
            .title(title)
            .style(modal_fill())
            .border_style(style(Role::BorderModal));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height < 5 || inner.width < 30 {
            return;
        }
        let center = |spans: Vec<Span<'static>>| -> Line<'static> {
            let width: usize = spans.iter().map(|s| s.width()).sum();
            let pad = inner.width.saturating_sub(width as u16) / 2;
            let mut padded = vec![Span::raw(" ".repeat(pad as usize))];
            padded.extend(spans);
            Line::from(padded)
        };
        let head = if self.dirty.is_empty() {
            self.kind.question().to_string()
        } else {
            format!("Unsaved changes in {}:", dirty_display(&self.dirty))
        };
        let hint = if self.dirty.is_empty() {
            "←/→ select • Enter confirm • y yes • n no • Esc"
        } else {
            "←/→ select • Enter confirm • s save • d discard • Esc"
        };
        let mut lines = vec![
            center(vec![Span::styled(head, style(Role::Text))]),
            Line::from(""),
            center(self.buttons()),
            Line::from(""),
            center(vec![Span::styled(hint, style(Role::Muted))]),
        ];
        lines.truncate(inner.height as usize);
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn buttons(&self) -> Vec<ratatui::text::Span<'static>> {
        use ratatui::text::Span;
        use crate::ui::theme::{Role, focus_row, style};
        let mut spans = Vec::new();
        for (index, label) in self.button_labels().iter().enumerate() {
            if index > 0 {
                spans.push(Span::raw("  "));
            }
            let chosen = index == self.choice;
            if self.pills {
                use ratatui::style::{Color, Style};
                let (fill, left_cap, right_cap) = crate::ui::theme::button_chrome(
                    chosen,
                    style(Role::TabActive),
                    style(Role::TabInactive),
                    Color::DarkGray,
                );
                spans.push(Span::styled(
                    crate::ui::theme::pill_left().to_string(),
                    Style::default().fg(left_cap),
                ));
                spans.push(Span::styled(" ".to_string(), fill));
                spans.push(Span::styled(label.to_string(), fill));
                spans.push(Span::styled(" ".to_string(), fill));
                spans.push(Span::styled(
                    crate::ui::theme::pill_right().to_string(),
                    Style::default().fg(right_cap),
                ));
            } else if chosen {
                spans.push(Span::styled(">".to_string(), focus_row()));
                spans.push(Span::styled(format!("[{label}]"), focus_row()));
            } else {
                spans.push(Span::styled(format!(" [{label}]"), style(Role::Text)));
            }
        }
        spans
    }
}

/// File list for the dirty-mode head row: up to three names (each
/// shortened past 32 chars), then `+N more`. Bounded, so the modal
/// never grows with the session count.
fn dirty_display(dirty: &[String]) -> String {
    const MAX_NAMES: usize = 3;
    const MAX_NAME: usize = 32;
    let short = |name: &str| {
        if name.chars().count() > MAX_NAME {
            format!("{}…", name.chars().take(MAX_NAME - 1).collect::<String>())
        } else {
            name.to_string()
        }
    };
    let mut shown: Vec<String> = dirty.iter().take(MAX_NAMES).map(|n| short(n)).collect();
    if dirty.len() > MAX_NAMES {
        shown.push(format!("+{} more", dirty.len() - MAX_NAMES));
    }
    shown.join(", ")
}

/// Centered "Saving sessions..." box, clamped into tiny terminals.
pub fn saving_area(term: Rect) -> Rect {
    let (w, h) = (40.min(term.width), 5.min(term.height));
    Rect::new(
        term.x + term.width.saturating_sub(w) / 2,
        term.y + term.height.saturating_sub(h) / 2,
        w,
        h,
    )
}

/// Render the saving modal: opaque, one centered line, no input.
pub fn view_saving(frame: &mut ratatui::Frame, area: Rect) {
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Clear, Paragraph};
    use crate::ui::theme::{Role, modal_fill, style};
    // Opaque: the live grid must not show through the modal.
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(crate::ui::theme::border_type())
        .title(" Saving ")
        .style(modal_fill())
        .border_style(style(Role::BorderModal));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 3 || inner.width < 20 {
        return;
    }
    let width: usize = "Saving sessions...".len();
    let pad = inner.width.saturating_sub(width as u16) / 2;
    let mut padded = vec![Span::raw(" ".repeat(pad as usize))];
    padded.push(Span::styled("Saving sessions...", style(Role::Text)));
    let mut lines = vec![Line::from(""), Line::from(padded), Line::from("")];
    lines.truncate(inner.height as usize);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Centered confirm box, clamped into tiny terminals.
pub fn confirm_area(term: Rect) -> Rect {
    let (w, h) = (52.min(term.width), 7.min(term.height));
    Rect::new(
        term.x + term.width.saturating_sub(w) / 2,
        term.y + term.height.saturating_sub(h) / 2,
        w,
        h,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn no_is_default_so_enter_stays() {
        let mut q = Confirm::new(ConfirmKind::QuitForge, true);
        assert_eq!(q.choice(), 1, "No is default");
        assert_eq!(q.key(&key(KeyCode::Enter)), ConfirmOutcome::Dismissed);
        assert_eq!(q.key(&key(KeyCode::Esc)), ConfirmOutcome::Dismissed);
    }

    #[test]
    fn arrows_toggle_between_yes_and_no() {
        let mut q = Confirm::new(ConfirmKind::QuitForge, true);
        assert_eq!(q.key(&key(KeyCode::Right)), ConfirmOutcome::Pending);
        assert_eq!(q.choice(), 0, "Yes");
        assert_eq!(q.key(&key(KeyCode::Enter)), ConfirmOutcome::Confirmed);
        assert_eq!(q.key(&key(KeyCode::Left)), ConfirmOutcome::Pending);
        assert_eq!(q.choice(), 1, "back to No");
        assert_eq!(q.key(&key(KeyCode::Enter)), ConfirmOutcome::Dismissed);
    }

    #[test]
    fn left_highlight_lights_only_the_left_bookend() {
        let theme = crate::ui::theme::parse_external_theme(
            r##"{"name": "tri", "highlight": "left", "buttons": {"left": "[", "right": "]"}}"##,
        )
        .expect("left theme parses");
        let _guard = crate::ui::theme::hold_external_theme(theme);
        let q = Confirm::new(ConfirmKind::QuitForge, true);
        assert_eq!(q.choice(), 1, "No is default");
        let spans = q.buttons();
        // Yes group (5 spans) + gap + No group: No's left cap, fill, right cap.
        assert_eq!(spans[8].content, "No");
        assert_eq!(
            spans[8].style,
            crate::ui::theme::style(crate::ui::theme::Role::TabInactive),
            "chosen button keeps its rest color"
        );
        assert_eq!(spans[6].style.fg, Some(ratatui::style::Color::Yellow));
        assert_eq!(spans[10].style.fg, Some(ratatui::style::Color::DarkGray));
    }

    #[test]
    fn y_quits_and_n_stays() {
        let mut q = Confirm::new(ConfirmKind::QuitForge, false);
        assert_eq!(q.key(&key(KeyCode::Char('y'))), ConfirmOutcome::Confirmed);
        let mut q = Confirm::new(ConfirmKind::QuitForge, false);
        assert_eq!(q.key(&key(KeyCode::Char('n'))), ConfirmOutcome::Dismissed);
    }

    #[test]
    fn saving_modal_paints_saving_sessions_centered() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| view_saving(f, saving_area(f.area())))
            .unwrap();
        let buf = terminal.backend().buffer();
        let text: String = buf
            .content
            .iter()
            .map(|c| c.symbol().to_string())
            .collect();
        assert!(text.contains("Saving sessions..."));
        // Box is 40x5 at x20/y9: border, blank, text (row 11), blank, border.
        let row: String = (0..80)
            .map(|x| buf[(x, 11)].symbol().to_string())
            .collect();
        let byte = row.find("Saving sessions...").expect("saving line");
        let x = row[..byte].chars().count() as u16;
        assert_eq!((x, buf[(x, 11)].symbol()), (31, "S"), "text centered");
    }

    #[test]
    fn kill_question_paints_kill_session_text() {
        use ratatui::{backend::TestBackend, Terminal};
        let id = crate::session::SessionId::fresh();
        let dialog = Confirm::new(ConfirmKind::KillSession(id), true);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| dialog.view(f, confirm_area(f.area())))
            .unwrap();
        let buf = terminal.backend().buffer();
        let text: String = buf
            .content
            .iter()
            .map(|c| c.symbol().to_string())
            .collect();
        assert!(text.contains("Are you sure you want to kill this session?"));
        assert!(text.contains("Yes"));
        assert!(text.contains("No"));
    }

    #[test]
    fn modal_paints_question_and_dim_no_default() {
        use ratatui::{backend::TestBackend, Terminal};
        use ratatui::style::Color;
        let q = Confirm::new(ConfirmKind::QuitForge, true);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| q.view(f, confirm_area(f.area())))
            .unwrap();
        let buf = terminal.backend().buffer();
        let text: String = buf
            .content
            .iter()
            .map(|c| c.symbol().to_string())
            .collect();
        assert!(text.contains("Are you sure you want to quit?"));
        assert!(text.contains("Yes"));
        assert!(text.contains("No"));
        // No (default) carries the accent fill, Yes rests dim.
        // Box is 7 rows at y8: border, question, blank, buttons (row 11).
        let row: String = (0..80)
            .map(|x| buf[(x, 11)].symbol().to_string())
            .collect();
        let no_byte = row.find("No").expect("No button");
        let no_x = row[..no_byte].chars().count() as u16;
        assert_eq!(buf[(no_x, 11)].bg, Color::Yellow, "default No filled");
        let yes_byte = row.find("Yes").expect("Yes button");
        let yes_x = row[..yes_byte].chars().count() as u16;
        assert_eq!(buf[(yes_x, 11)].bg, Color::DarkGray, "Yes rests dim");
    }
}

#[cfg(test)]
mod dirty_tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn dirty_quit() -> Confirm {
        Confirm::with_dirty(
            ConfirmKind::QuitForge,
            true,
            vec!["notes/a.md".to_string(), "notes/b.md".to_string()],
        )
    }

    #[test]
    fn dirty_mode_defaults_to_cancel() {
        let mut q = dirty_quit();
        assert_eq!(q.key(&key(KeyCode::Enter)), ConfirmOutcome::Dismissed);
    }

    #[test]
    fn dirty_keys_fire_save_discard_cancel() {
        use crossterm::event::KeyEvent as KE;
        let mut q = dirty_quit();
        assert_eq!(
            q.key(&KE::new(KeyCode::Char('s'), KeyModifiers::NONE)),
            ConfirmOutcome::SaveDirty
        );
        let mut q = dirty_quit();
        assert_eq!(
            q.key(&KE::new(KeyCode::Char('d'), KeyModifiers::NONE)),
            ConfirmOutcome::DiscardDirty
        );
        let mut q = dirty_quit();
        assert_eq!(
            q.key(&KE::new(KeyCode::Char('c'), KeyModifiers::NONE)),
            ConfirmOutcome::Dismissed
        );
    }

    #[test]
    fn dirty_arrows_cycle_three_choices() {
        let mut q = dirty_quit();
        assert_eq!(q.key(&key(KeyCode::Left)), ConfirmOutcome::Pending);
        assert_eq!(q.key(&key(KeyCode::Enter)), ConfirmOutcome::DiscardDirty);
        assert_eq!(q.key(&key(KeyCode::Left)), ConfirmOutcome::Pending);
        assert_eq!(q.key(&key(KeyCode::Enter)), ConfirmOutcome::SaveDirty);
    }

    #[test]
    fn dirty_view_names_files_and_three_buttons() {
        use ratatui::{backend::TestBackend, Terminal};
        let q = dirty_quit();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| q.view(f, confirm_area(f.area())))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("notes/a.md"), "names files: {text:?}");
        assert!(text.contains("notes/b.md"), "names files: {text:?}");
        assert!(text.contains("Save"), "save pill");
        assert!(text.contains("Discard"), "discard pill");
        assert!(text.contains("Cancel"), "cancel pill");
    }

    #[test]
    fn click_fires_the_button_under_the_cell() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let area = confirm_area(ratatui::layout::Rect::new(0, 0, 80, 24));
        let mut q = dirty_quit();
        terminal.draw(|f| q.view(f, area)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let save_x = (0..80)
            .find(|x| {
                let row: String = (*x..(*x + 4).min(80))
                    .map(|xx| buf[(xx, area.y + 3)].symbol())
                    .collect();
                row == "Save"
            })
            .expect("Save painted");
        assert_eq!(
            q.click(save_x, area.y + 3, area),
            Some(ConfirmOutcome::SaveDirty)
        );
    }
}
