//! Theme picker modal: lists `~/.forge/themes/` entries plus the
//! builtin `default`, previews each theme's button bookends inline,
//! and applies the selection at runtime on Enter.
//!
//! Up/Down (or `j`/`k`) moves, Enter applies, Esc closes. Rows take
//! focus on left-click; the Apply/Cancel pills fire on left-click.
//! An open modal swallows all other mouse input.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;

use crate::ui::theme::ExternalTheme;

/// Outcome of one key or click inside the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThemeOutcome {
    Pending,
    Applied(String),
    Cancelled,
}

/// Theme picker state: the listed themes (builtin `default` first),
/// the cursor, the currently applied name (marked `(active)`), and
/// the global pills flag for the action buttons.
pub struct ThemeDialog {
    themes: Vec<ExternalTheme>,
    selected: usize,
    active: String,
    pills: bool,
}

impl ThemeDialog {
    pub fn new(themes: Vec<ExternalTheme>, current: &str, pills: bool) -> Self {
        let selected = themes
            .iter()
            .position(|t| t.name == current)
            .unwrap_or(0);
        ThemeDialog {
            themes,
            selected,
            active: current.to_string(),
            pills,
        }
    }

    /// Theme names in list order (builtin first).
    pub fn names(&self) -> Vec<String> {
        self.themes.iter().map(|t| t.name.clone()).collect()
    }

    /// Currently selected theme name.
    pub fn selected_name(&self) -> &str {
        self.themes
            .get(self.selected)
            .map(|t| t.name.as_str())
            .unwrap_or("default")
    }

    fn move_cursor(&mut self, dir: i32) {
        if self.themes.is_empty() {
            return;
        }
        let len = self.themes.len() as i32;
        self.selected = (self.selected as i32 + dir).rem_euclid(len) as usize;
    }

    pub fn key(&mut self, key: &KeyEvent) -> ThemeOutcome {
        match key.code {
            KeyCode::Esc => ThemeOutcome::Cancelled,
            KeyCode::Enter => ThemeOutcome::Applied(self.selected_name().to_string()),
            KeyCode::Up => {
                self.move_cursor(-1);
                ThemeOutcome::Pending
            }
            KeyCode::Down => {
                self.move_cursor(1);
                ThemeOutcome::Pending
            }
            KeyCode::Char('k') if key.modifiers.is_empty() => {
                self.move_cursor(-1);
                ThemeOutcome::Pending
            }
            KeyCode::Char('j') if key.modifiers.is_empty() => {
                self.move_cursor(1);
                ThemeOutcome::Pending
            }
            _ => ThemeOutcome::Pending,
        }
    }

    /// First visible row for the current cursor under `visible` rows.
    fn offset(&self, visible: usize) -> usize {
        if visible == 0 {
            return 0;
        }
        self.selected.saturating_sub(visible.saturating_sub(1))
    }

    /// Render the centered modal. Geometry (header, rows, buttons,
    /// hint) is recomputed identically by [`ThemeDialog::click`], so
    /// clicks never desync from what is on screen.
    pub fn view(&self, frame: &mut ratatui::Frame, area: Rect) {
        use ratatui::text::{Line, Span};
        use ratatui::widgets::Paragraph;
        use crate::ui::theme::{Role, style};
        let Some(inner) =
            super::frame::ModalFrame::new(" Themes ").min_size(30, 8).open(frame, area)
        else {
            return;
        };
        let text = style(Role::Text);
        let muted = style(Role::Muted);
        // Header, rows, buttons, hint all pin to fixed offsets so
        // state changes never shove content around.
        let visible = visible_rows(inner.height, self.themes.len());
        let offset = self.offset(visible);
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(vec![
            Span::styled("  Select a theme", text),
            Span::styled(format!(" ({})", self.themes.len()), muted),
        ]));
        lines.push(Line::from(""));
        for idx in offset..offset.saturating_add(visible) {
            let Some(theme) = self.themes.get(idx) else {
                break;
            };
            let focused = idx == self.selected;
            let marker = if focused { "> " } else { "  " };
            let marker_style =
                if focused { crate::ui::theme::focus_row() } else { text };
            let name = crate::infra::safe_text::encode_for_display(&theme.name);
            let active_tag = if theme.name == self.active {
                " (active)".to_string()
            } else {
                String::new()
            };
            // Per-theme shape preview: the sample pill wears that
            // theme's own bookends, so squares/rounds/bars read
            // before anything is applied.
            let sample = format!(
                "  {}{}{}{}",
                theme.button_left, " Sample ", theme.button_right, active_tag
            );
            let row_style = if focused {
                crate::ui::theme::focus_row()
            } else {
                text
            };
            lines.push(Line::from(vec![
                Span::styled(marker.to_string(), marker_style),
                Span::styled(format!("{name}{sample}"), row_style),
            ]));
        }
        // Fixed slots: buttons sit right above the hint, the hint pins
        // to the bottom, and the gap between rows and buttons absorbs
        // short lists so the footer never moves.
        let used = 2 + visible as u16;
        let total = inner.height;
        let buttons_at = total.saturating_sub(3);
        while (lines.len() as u16) < buttons_at {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(self.buttons()));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "↑/↓ select • Enter apply • Esc close",
            muted,
        )));
        lines.truncate(inner.height as usize);
        // Content keeps two cells of side padding; the buttons row
        // centers in the content width like every other modal.
        let buttons_idx = buttons_at as usize;
        let padded: Vec<Line<'static>> = lines
            .into_iter()
            .enumerate()
            .map(|(i, line)| {
                if i == buttons_idx {
                    let width: usize =
                        line.spans.iter().map(|s| s.width()).sum();
                    let pad = inner.width.saturating_sub(width as u16) / 2;
                    let mut spans =
                        vec![Span::raw(" ".repeat(pad as usize))];
                    spans.extend(line.spans);
                    Line::from(spans)
                } else {
                    let mut spans = vec![Span::raw("  ")];
                    spans.extend(line.spans);
                    Line::from(spans)
                }
            })
            .collect();
        let _ = used;
        frame.render_widget(Paragraph::new(padded), inner);
    }

    /// Action pills: accent-filled default carrying `*`, Cancel at
    /// rest. Paint and [`Self::click`] share the row.
    fn action_row() -> crate::ui::button::ButtonRow {
        crate::ui::button::ButtonRow::new(vec![
            crate::ui::button::Button::new("Apply*").emphasized(true),
            crate::ui::button::Button::new("Cancel"),
        ])
    }

    fn buttons(&self) -> Vec<ratatui::text::Span<'static>> {
        use ratatui::text::Span;
        use crate::ui::theme::{Role, style};
        let mut spans = Vec::new();
        if self.pills {
            spans.extend(Self::action_row().spans());
        } else {
            spans.push(Span::styled(
                "[Apply*]  [Cancel]".to_string(),
                style(Role::Text),
            ));
        }
        // Center in the content width (set by the caller area).
        spans
    }

    /// Hit-test a click: rows take focus, the Apply/Cancel pills fire.
    /// Coordinates come from cells read back off the paint in tests,
    /// never from hand-computed math.
    pub fn click(&mut self, col: u16, row: u16, area: Rect) -> Option<ThemeOutcome> {
        if col < area.x
            || col >= area.x.saturating_add(area.width)
            || row < area.y
            || row >= area.y.saturating_add(area.height)
        {
            return None;
        }
        // Recompute the inner rect the same way ratatui does: one
        // cell of border on each side.
        if area.width < 3 || area.height < 3 {
            return None;
        }
        let inner = Rect::new(
            area.x.saturating_add(1),
            area.y.saturating_add(1),
            area.width.saturating_sub(2),
            area.height.saturating_sub(2),
        );
        if inner.height < 8 || inner.width < 30 {
            return None;
        }
        let visible = visible_rows(inner.height, self.themes.len());
        let offset = self.offset(visible);
        let rows_start = inner.y.saturating_add(2);
        // Theme rows: click takes focus (no apply yet — Enter or the
        // Apply pill applies, so shapes stay previewable).
        for i in 0..visible {
            let y = rows_start.saturating_add(i as u16);
            if row == y {
                let idx = offset.saturating_add(i);
                if idx < self.themes.len()
                    && col >= inner.x
                    && col < inner.x.saturating_add(inner.width)
                {
                    self.selected = idx;
                    return Some(ThemeOutcome::Pending);
                }
            }
        }
        // Buttons row: centered Apply/Cancel pills (or the legacy
        // labels). Hit-testing mirrors `view` exactly.
        let buttons_y = inner.y.saturating_add(inner.height.saturating_sub(3));
        if row != buttons_y {
            return None;
        }
        if !self.pills {
            return None;
        }
        // One layout for paint and clicks: the row owns both.
        let buttons = Self::action_row();
        let total = buttons.width() as u16;
        let start = buttons.origin_centered(inner.x, inner.width);
        match buttons.click(col, row, start, buttons_y, total) {
            Some(0) => return Some(ThemeOutcome::Applied(self.selected_name().to_string())),
            Some(_) => return Some(ThemeOutcome::Cancelled),
            None => {}
        }
        None
    }
}

/// Visible theme rows for an inner height: header(2) + buttons(1) +
/// gap(1) + hint(1) + pad leave the rest for rows, capped at 10 so
/// huge theme dirs never blow out the modal.
fn visible_rows(inner_h: u16, count: usize) -> usize {
    let room = inner_h.saturating_sub(8) as usize;
    room.min(10).min(count.max(1)).max(1).min(count.max(1))
}

/// Centered picker box, clamped into tiny terminals.
pub fn theme_area(term: Rect) -> Rect {
    let w = 64.min(term.width);
    let h = 20.min(term.height);
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
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn sample_themes() -> Vec<ExternalTheme> {
        vec![
            ExternalTheme::builtin(),
            crate::ui::theme::parse_external_theme(
                r##"{"name": "square", "buttons": {"left": "[", "right": "]"}}"##,
            )
            .unwrap(),
            crate::ui::theme::parse_external_theme(
                r##"{"name": "round", "buttons": {"left": "(", "right": ")"}}"##,
            )
            .unwrap(),
        ]
    }

    #[test]
    fn arrows_move_and_enter_applies() {
        let mut d = ThemeDialog::new(sample_themes(), "default", true);
        assert_eq!(d.selected_name(), "default");
        assert_eq!(d.key(&key(KeyCode::Down)), ThemeOutcome::Pending);
        assert_eq!(d.selected_name(), "square");
        assert_eq!(d.key(&key(KeyCode::Up)), ThemeOutcome::Pending);
        assert_eq!(d.selected_name(), "default");
        assert_eq!(
            d.key(&key(KeyCode::Enter)),
            ThemeOutcome::Applied("default".to_string())
        );
        assert_eq!(d.key(&key(KeyCode::Esc)), ThemeOutcome::Cancelled);
    }

    #[test]
    fn modal_paints_names_and_shape_previews() {
        use ratatui::{backend::TestBackend, Terminal};
        let d = ThemeDialog::new(sample_themes(), "square", true);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| d.view(f, theme_area(f.area()))).unwrap();
        let buf = terminal.backend().buffer();
        let text: String = buf
            .content
            .iter()
            .map(|c| c.symbol().to_string())
            .collect();
        assert!(text.contains("Themes"), "title: {text:?}");
        assert!(text.contains("default"), "builtin listed");
        assert!(text.contains("square"), "theme listed");
        assert!(text.contains("[ Sample ]"), "square preview wears its caps");
        assert!(text.contains("(active)"), "current marked");
        assert!(text.contains("Apply"), "apply pill");
    }

    #[test]
    fn click_row_focuses_and_apply_pill_fires() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut d = ThemeDialog::new(sample_themes(), "default", true);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let area = theme_area(ratatui::layout::Rect::new(0, 0, 80, 24));
        terminal.draw(|f| d.view(f, area)).unwrap();
        let buf = terminal.backend().buffer();
        // Drive click() from cells read back off the paint: find the
        // "round" row and click its first cell.
        let mut target: Option<(u16, u16)> = None;
        for y in 0..24 {
            let row: String = (0..80)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            if row.contains("round") {
                let x = row.find("round").unwrap();
                target = Some((x as u16, y));
                break;
            }
        }
        let (cx, cy) = target.expect("round row painted");
        assert_eq!(
            d.click(cx, cy, area),
            Some(ThemeOutcome::Pending),
            "row click focuses"
        );
        assert_eq!(d.selected_name(), "round");
        // Find the Apply pill on the painted buttons row the same way.
        let mut apply: Option<(u16, u16)> = None;
        for y in 0..24 {
            let row: String = (0..80)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            if row.contains("Apply") {
                let x = row.find("Apply").unwrap();
                apply = Some((x as u16, y));
                break;
            }
        }
        let (ax, ay) = apply.expect("apply pill painted");
        // Repaint after the row focus moved, then click Apply.
        terminal.draw(|f| d.view(f, area)).unwrap();
        assert_eq!(
            d.click(ax, ay, area),
            Some(ThemeOutcome::Applied("round".to_string())),
            "apply pill fires"
        );
    }
}
