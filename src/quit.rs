//! Quit-confirmation modal: "Are you sure you want to quit?" with
//! Yes/No buttons. No is the default, so Enter or Esc keeps running;
//! only an explicit Yes (or `y`) sets the quit flag.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;

/// Outcome of one key inside the quit confirm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuitOutcome {
    Pending,
    Confirmed,
    Dismissed,
}

/// Yes/No choice; index 1 (No) is the default.
pub struct QuitConfirm {
    choice: usize,
    pills: bool,
}

impl QuitConfirm {
    pub fn new(pills: bool) -> Self {
        QuitConfirm { choice: 1, pills }
    }

    /// Test hook: 0 is Yes, 1 is No.
    #[cfg(test)]
    pub fn choice(&self) -> usize {
        self.choice
    }

    pub fn key(&mut self, key: &KeyEvent) -> QuitOutcome {
        match key.code {
            KeyCode::Esc => QuitOutcome::Dismissed,
            KeyCode::Enter => {
                if self.choice == 0 {
                    QuitOutcome::Confirmed
                } else {
                    QuitOutcome::Dismissed
                }
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                self.choice = 1 - self.choice;
                QuitOutcome::Pending
            }
            KeyCode::Char('h') | KeyCode::Char('l') if key.modifiers.is_empty() => {
                self.choice = 1 - self.choice;
                QuitOutcome::Pending
            }
            KeyCode::Char('y') | KeyCode::Char('Y') if key.modifiers.is_empty() => {
                QuitOutcome::Confirmed
            }
            KeyCode::Char('n') | KeyCode::Char('N') if key.modifiers.is_empty() => {
                QuitOutcome::Dismissed
            }
            _ => QuitOutcome::Pending,
        }
    }

    /// Render the centered modal: opaque, question, Yes/No buttons, hint.
    pub fn view(&self, frame: &mut ratatui::Frame, area: Rect) {
        use ratatui::text::{Line, Span};
        use ratatui::widgets::{Block, Borders, Clear, Paragraph};
        use crate::theme::{Role, modal_fill, style};
        // Opaque: the live grid must not show through the modal.
        frame.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
                .border_type(crate::theme::border_type())
            .title(" Quit ")
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
        let mut lines = vec![
            center(vec![Span::styled(
                "Are you sure you want to quit?",
                style(Role::Text),
            )]),
            Line::from(""),
            center(self.buttons()),
            Line::from(""),
            center(vec![Span::styled(
                "←/→ select • Enter confirm • y quit • n stay • Esc",
                style(Role::Muted),
            )]),
        ];
        lines.truncate(inner.height as usize);
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn buttons(&self) -> Vec<ratatui::text::Span<'static>> {
        use ratatui::text::Span;
        use crate::theme::{Role, focus_row, style};
        let mut spans = Vec::new();
        for (index, label) in ["Yes", "No"].iter().enumerate() {
            if index > 0 {
                spans.push(Span::raw("  "));
            }
            let chosen = index == self.choice;
            if self.pills {
                use ratatui::style::{Color, Style};
                let (fill, left_cap, right_cap) = crate::theme::button_chrome(
                    chosen,
                    style(Role::TabActive),
                    style(Role::TabInactive),
                    Color::DarkGray,
                );
                spans.push(Span::styled(
                    crate::theme::pill_left().to_string(),
                    Style::default().fg(left_cap),
                ));
                spans.push(Span::styled(" ".to_string(), fill));
                spans.push(Span::styled(label.to_string(), fill));
                spans.push(Span::styled(" ".to_string(), fill));
                spans.push(Span::styled(
                    crate::theme::pill_right().to_string(),
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
    use crate::theme::{Role, modal_fill, style};
    // Opaque: the live grid must not show through the modal.
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(crate::theme::border_type())
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
pub fn quit_area(term: Rect) -> Rect {
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
        let mut q = QuitConfirm::new(true);
        assert_eq!(q.choice(), 1, "No is default");
        assert_eq!(q.key(&key(KeyCode::Enter)), QuitOutcome::Dismissed);
        assert_eq!(q.key(&key(KeyCode::Esc)), QuitOutcome::Dismissed);
    }

    #[test]
    fn arrows_toggle_between_yes_and_no() {
        let mut q = QuitConfirm::new(true);
        assert_eq!(q.key(&key(KeyCode::Right)), QuitOutcome::Pending);
        assert_eq!(q.choice(), 0, "Yes");
        assert_eq!(q.key(&key(KeyCode::Enter)), QuitOutcome::Confirmed);
        assert_eq!(q.key(&key(KeyCode::Left)), QuitOutcome::Pending);
        assert_eq!(q.choice(), 1, "back to No");
        assert_eq!(q.key(&key(KeyCode::Enter)), QuitOutcome::Dismissed);
    }

    #[test]
    fn left_highlight_lights_only_the_left_bookend() {
        let theme = crate::theme::parse_external_theme(
            r##"{"name": "tri", "highlight": "left", "buttons": {"left": "[", "right": "]"}}"##,
        )
        .expect("left theme parses");
        let _guard = crate::theme::hold_external_theme(theme);
        let q = QuitConfirm::new(true);
        assert_eq!(q.choice(), 1, "No is default");
        let spans = q.buttons();
        // Yes group (5 spans) + gap + No group: No's left cap, fill, right cap.
        assert_eq!(spans[8].content, "No");
        assert_eq!(
            spans[8].style,
            crate::theme::style(crate::theme::Role::TabInactive),
            "chosen button keeps its rest color"
        );
        assert_eq!(spans[6].style.fg, Some(ratatui::style::Color::Yellow));
        assert_eq!(spans[10].style.fg, Some(ratatui::style::Color::DarkGray));
    }

    #[test]
    fn y_quits_and_n_stays() {
        let mut q = QuitConfirm::new(false);
        assert_eq!(q.key(&key(KeyCode::Char('y'))), QuitOutcome::Confirmed);
        let mut q = QuitConfirm::new(false);
        assert_eq!(q.key(&key(KeyCode::Char('n'))), QuitOutcome::Dismissed);
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
    fn modal_paints_question_and_dim_no_default() {
        use ratatui::{backend::TestBackend, Terminal};
        use ratatui::style::Color;
        let q = QuitConfirm::new(true);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| q.view(f, quit_area(f.area())))
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
