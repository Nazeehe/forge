//! Full-field card editor modal: the `e` key on the kanban board.
//!
//! The old single-line title draft only edited one of the card's many
//! fields; this modal edits them all in one place — title,
//! description, status (column), assignee, priority, tags (labels),
//! start date, due date, estimate (story points), and progress.
//! Tab/Up/Down cycles rows, typing edits the focused text,
//! Left/Right drives the status/priority cyclers and the action row,
//! Enter submits (or fires the focused action), Esc cancels. Like
//! every other modal it swallows all mouse input while open: rows
//! take focus on left-click and Save/Cancel fire at once.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use tui_realm_stdlib::components::{Input, Radio, Select};

/// Focus rows in Tab order.
pub const FOCUS_TITLE: usize = 0;
pub const FOCUS_DESCRIPTION: usize = 1;
pub const FOCUS_COLUMN: usize = 2;
pub const FOCUS_ASSIGNEE: usize = 3;
pub const FOCUS_PRIORITY: usize = 4;
pub const FOCUS_TAGS: usize = 5;
pub const FOCUS_START: usize = 6;
pub const FOCUS_DUE: usize = 7;
pub const FOCUS_ESTIMATE: usize = 8;
pub const FOCUS_PROGRESS: usize = 9;
pub const FOCUS_ACTIONS: usize = 10;
const FIELD_COUNT: usize = 11;

/// Validated dialog output: the scalar patch plus an optional
/// WIP-checked status move (`None` keeps the card where it is).
#[derive(Clone, Debug)]
pub struct CardEditResult {
    pub patch: crate::kanban::board::CardPatch,
    pub column: Option<String>,
}

/// Dialog result after one input: still open, cancelled, or submitted.
#[derive(Debug)]
pub enum CardEditOutcome {
    Pending,
    Cancelled,
    Submitted(CardEditResult),
}

/// Multi-field card form: eight text inputs, two selects, and a
/// two-way action row. Text rows start prefilled from the card;
/// empty date/assignee/estimate rows clear those fields on save.
pub struct CardEditDialog {
    card_id: String,
    original_column: String,
    original_estimate: Option<u32>,
    title: Input,
    description: Input,
    column: Select,
    columns: Vec<String>,
    assignee: Input,
    priority: Select,
    tags: Input,
    start: Input,
    due: Input,
    estimate: Input,
    progress: Input,
    actions: Radio,
    pills: bool,
    focus: usize,
    error: Option<String>,
}

impl CardEditDialog {
    /// Prefill every row from the card. `columns` are the live board
    /// column names in order; the status cycler starts on the card's
    /// own column (or the first when it somehow went missing).
    pub fn new(card: &crate::kanban::board::Card, columns: &[String], pills: bool) -> Self {
        let column_pos = columns.iter().position(|c| c == &card.column).unwrap_or(0);
        let column = Select::default()
            .choices(columns.to_vec())
            .value(column_pos)
            .rewind(true);
        let priorities = vec![
            "low".to_string(),
            "normal".to_string(),
            "high".to_string(),
            "urgent".to_string(),
        ];
        let priority_pos = match card.priority {
            crate::kanban::board::Priority::Low => 0,
            crate::kanban::board::Priority::Normal => 1,
            crate::kanban::board::Priority::High => 2,
            crate::kanban::board::Priority::Urgent => 3,
        };
        let priority = Select::default()
            .choices(priorities)
            .value(priority_pos)
            .rewind(true);
        let actions = Radio::default()
            .choices(["Save", "Cancel"])
            .value(0)
            .rewind(true);
        CardEditDialog {
            card_id: card.id.clone(),
            original_column: card.column.clone(),
            original_estimate: card.estimate,
            title: Input::default().title("Title").value(card.title.clone()),
            description: Input::default()
                .title("Description")
                .value(card.description.clone()),
            column,
            columns: columns.to_vec(),
            assignee: Input::default()
                .title("Assignee")
                .value(card.assignee.clone()),
            priority,
            tags: Input::default()
                .title("Tags")
                .value(card.tags.join(", ")),
            start: Input::default()
                .title("Start date")
                .value(card.start_date.clone().unwrap_or_default()),
            due: Input::default()
                .title("Due date")
                .value(card.due_date.clone().unwrap_or_default()),
            estimate: Input::default()
                .title("Estimate")
                .value(card.estimate.map(|e| e.to_string()).unwrap_or_default()),
            progress: Input::default()
                .title("Progress")
                .value(card.progress.to_string()),
            actions,
            pills,
            focus: 0,
            error: None,
        }
    }

    /// Card under edit, for the caller to locate the board.
    pub fn card_id(&self) -> &str {
        &self.card_id
    }

    #[cfg(test)]
    pub fn focus(&self) -> usize {
        self.focus
    }

    #[cfg(test)]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    fn selected(select: &Select) -> String {
        select
            .states
            .choices
            .get(select.states.selected)
            .cloned()
            .unwrap_or_default()
    }

    /// Parse the estimate row: blank clears a previously set value
    /// and keeps an unset one; numbers clamp to
    /// [`crate::kanban::board::MAX_ESTIMATE`]; anything else errors.
    fn estimate_patch(&self) -> Result<(Option<u32>, bool), String> {
        let raw = super::fields::input_text(&self.estimate);
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok((None, self.original_estimate.is_some()));
        }
        match trimmed.parse::<u64>() {
            Ok(e) => Ok((
                Some(e.min(crate::kanban::board::MAX_ESTIMATE as u64) as u32),
                false,
            )),
            Err(_) => Err("estimate must be a number".to_string()),
        }
    }

    /// Parse the progress row: blank keeps the current value;
    /// numbers clamp to 100 in the domain; anything else errors.
    fn progress_patch(&self) -> Result<Option<u16>, String> {
        let raw = super::fields::input_text(&self.progress);
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        match trimmed.parse::<u64>() {
            Ok(p) => Ok(Some(p.min(10_000) as u16)),
            Err(_) => Err("progress must be 0-100".to_string()),
        }
    }

    fn submit(&mut self) -> CardEditOutcome {
        let title = super::fields::input_text(&self.title);
        if title.trim().is_empty() {
            self.error = Some("title must not be empty".to_string());
            return CardEditOutcome::Pending;
        }
        if title.trim().chars().count() > crate::kanban::board::MAX_TITLE_LEN {
            self.error = Some(format!(
                "title too long (max {} chars)",
                crate::kanban::board::MAX_TITLE_LEN
            ));
            return CardEditOutcome::Pending;
        }
        let (estimate, clear_estimate) = match self.estimate_patch() {
            Ok(v) => v,
            Err(e) => {
                self.error = Some(e);
                return CardEditOutcome::Pending;
            }
        };
        let progress = match self.progress_patch() {
            Ok(v) => v,
            Err(e) => {
                self.error = Some(e);
                return CardEditOutcome::Pending;
            }
        };
        let assignee_raw = super::fields::input_text(&self.assignee);
        let start_raw = super::fields::input_text(&self.start);
        let due_raw = super::fields::input_text(&self.due);
        let tags_raw = super::fields::input_text(&self.tags);
        let tags: Vec<String> = tags_raw
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        let column_now = Self::selected(&self.column);
        let column = if column_now.is_empty() || column_now == self.original_column {
            None
        } else {
            Some(column_now)
        };
        let priority = crate::kanban::board::Priority::parse(&Self::selected(&self.priority));
        self.error = None;
        CardEditOutcome::Submitted(CardEditResult {
            patch: crate::kanban::board::CardPatch {
                title: Some(title.trim().to_string()),
                description: Some(super::fields::input_text(&self.description)),
                assignee: (!assignee_raw.trim().is_empty()).then(|| assignee_raw.trim().to_string()),
                clear_assignee: assignee_raw.trim().is_empty(),
                priority: Some(priority),
                progress,
                tags: Some(tags),
                start_date: (!start_raw.trim().is_empty())
                    .then(|| start_raw.trim().to_string()),
                clear_start_date: start_raw.trim().is_empty(),
                due_date: (!due_raw.trim().is_empty()).then(|| due_raw.trim().to_string()),
                clear_due_date: due_raw.trim().is_empty(),
                estimate,
                clear_estimate,
            },
            column,
        })
    }

    /// Paste bracketed-paste text into the focused text row, one char
    /// at a time like typing. Select rows ignore it.
    pub fn paste(&mut self, text: &str) {
        let field = match self.focus {
            FOCUS_TITLE => &mut self.title,
            FOCUS_DESCRIPTION => &mut self.description,
            FOCUS_ASSIGNEE => &mut self.assignee,
            FOCUS_TAGS => &mut self.tags,
            FOCUS_START => &mut self.start,
            FOCUS_DUE => &mut self.due,
            FOCUS_ESTIMATE => &mut self.estimate,
            FOCUS_PROGRESS => &mut self.progress,
            _ => return,
        };
        super::fields::type_text(field, text);
        self.error = None;
    }

    /// One key: Tab/BackTab/Up/Down cycle rows, Left/Right drives the
    /// status/priority cyclers and the action radio (or the text
    /// cursor), typing edits the focused input, Enter submits (or
    /// fires the focused action), Esc cancels.
    pub fn key(&mut self, key: &KeyEvent) -> CardEditOutcome {
        match key.code {
            KeyCode::Esc => return CardEditOutcome::Cancelled,
            KeyCode::Tab => {
                self.focus = super::focus::focus_next(self.focus, FIELD_COUNT);
                return CardEditOutcome::Pending;
            }
            KeyCode::BackTab => {
                self.focus = super::focus::focus_prev(self.focus, FIELD_COUNT);
                return CardEditOutcome::Pending;
            }
            KeyCode::Up => {
                self.focus = super::focus::focus_prev(self.focus, FIELD_COUNT);
                return CardEditOutcome::Pending;
            }
            KeyCode::Down => {
                self.focus = super::focus::focus_next(self.focus, FIELD_COUNT);
                return CardEditOutcome::Pending;
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
            FOCUS_COLUMN => match key.code {
                KeyCode::Left => super::fields::cycle_select(&mut self.column, -1),
                KeyCode::Right => super::fields::cycle_select(&mut self.column, 1),
                _ => {}
            },
            FOCUS_PRIORITY => match key.code {
                KeyCode::Left => super::fields::cycle_select(&mut self.priority, -1),
                KeyCode::Right => super::fields::cycle_select(&mut self.priority, 1),
                _ => {}
            },
            FOCUS_ACTIONS => {
                super::fields::option_key(&mut self.actions, key);
            }
            _ => {
                let field = match self.focus {
                    FOCUS_TITLE => &mut self.title,
                    FOCUS_DESCRIPTION => &mut self.description,
                    FOCUS_ASSIGNEE => &mut self.assignee,
                    FOCUS_TAGS => &mut self.tags,
                    FOCUS_START => &mut self.start,
                    FOCUS_DUE => &mut self.due,
                    FOCUS_ESTIMATE => &mut self.estimate,
                    _ => &mut self.progress,
                };
                if super::fields::field_key(field, key) {
                    self.error = None;
                }
            }
        }
        CardEditOutcome::Pending
    }

    /// Fire one action button: Save submits, anything else cancels.
    /// Shared by Enter and mouse so both paths agree.
    fn fire_action(&mut self, index: usize) -> CardEditOutcome {
        match index {
            0 => self.submit(),
            _ => CardEditOutcome::Cancelled,
        }
    }

    /// Action row the view paints: accent-hot default carrying `*`,
    /// `>` marker on the chosen button, pills or legacy brackets per
    /// mode. [`Self::click`] hit-tests this same row, so the mouse can
    /// never desync from the paint.
    fn dialog_row(&self) -> crate::ui::button::ButtonRow {
        crate::ui::button::ButtonRow::dialog(
            &["Save", "Cancel"],
            0,
            self.actions.states.choice,
            self.focus == FOCUS_ACTIONS,
            self.pills,
        )
    }

    /// Left-click dispatch inside the modal rect the view paints:
    /// form rows take focus, action buttons fire at once. `None` is
    /// dead space or outside; the caller still swallows everything
    /// while the modal is open.
    pub fn click(&mut self, col: u16, row: u16, area: Rect) -> Option<CardEditOutcome> {
        use super::frame::{click_inner, BorderHit};
        let Some(inner) = click_inner(col, row, area, BorderHit::Inclusive) else {
            return None;
        };
        if inner.height < 17 || inner.width < 44 {
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
                return Some(CardEditOutcome::Pending);
            }
        }
        let action_y = inner.y + 1 + FOCUS_ACTIONS as u16;
        if row != action_y || action_y >= status_row {
            return None;
        }
        // One layout for paint and clicks: the row owns both (the `>`
        // marker rides with its button); separators and the clipped
        // tail belong to nothing.
        let buttons = self.dialog_row();
        let cx = inner.x + 2;
        let cw = inner.width.saturating_sub(4);
        let width = (buttons.width() as u16).min(cw);
        let ox = buttons.origin_centered(cx, cw);
        if let Some(index) = buttons.click(col, row, ox, action_y, width) {
            self.actions.states.choice = index;
            self.focus = FOCUS_ACTIONS;
            self.error = None;
            return Some(self.fire_action(index));
        }
        None
    }

    /// Centered modal: the ten-row form, one action row, one fixed
    /// status slot (error or blank), and a pinned key hint. Content
    /// keeps two cells of border padding.
    pub fn view(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        use ratatui::text::{Line, Span};
        use ratatui::widgets::Paragraph;
        use crate::ui::theme::{Role, style};
        let Some(inner) =
            super::frame::ModalFrame::new(" Edit card ").min_size(44, 17).open(frame, area)
        else {
            return;
        };
        let end = inner.y + inner.height;
        let cx = inner.x + 2;
        let cw = inner.width.saturating_sub(4);
        let hint_row = end.saturating_sub(1);
        let status_row = end.saturating_sub(2);
        let mut row = inner.y + 1;
        let label_w = 11u16;
        let content_w = cw.saturating_sub(2 + label_w) as usize;
        // Form rows paint through the shared fields helpers; the
        // `>` marker rides with the focused row.
        let field = |value: &str, focused: bool, placeholder: &str| {
            super::fields::form_field(value, focused, placeholder, content_w)
        };
        let select = |value: &str, focused: bool| {
            super::fields::form_select(value, focused, content_w)
        };
        let rows: Vec<(&str, Vec<Span>)> = vec![
            ("Title", field(&super::fields::input_text(&self.title), self.focus == FOCUS_TITLE, "card title")),
            (
                "Description",
                field(
                    &super::fields::input_text(&self.description),
                    self.focus == FOCUS_DESCRIPTION,
                    "what needs doing",
                ),
            ),
            ("Status", select(&Self::selected(&self.column), self.focus == FOCUS_COLUMN)),
            (
                "Assignee",
                field(&super::fields::input_text(&self.assignee), self.focus == FOCUS_ASSIGNEE, "unassigned"),
            ),
            ("Priority", select(&Self::selected(&self.priority), self.focus == FOCUS_PRIORITY)),
            ("Tags", field(&super::fields::input_text(&self.tags), self.focus == FOCUS_TAGS, "a, b")),
            (
                "Start date",
                field(&super::fields::input_text(&self.start), self.focus == FOCUS_START, "2026-09-01"),
            ),
            (
                "Due date",
                field(&super::fields::input_text(&self.due), self.focus == FOCUS_DUE, "2026-10-01"),
            ),
            ("Estimate", field(&super::fields::input_text(&self.estimate), self.focus == FOCUS_ESTIMATE, "3")),
            (
                "Progress",
                field(&super::fields::input_text(&self.progress), self.focus == FOCUS_PROGRESS, "0-100"),
            ),
        ];
        for (index, (label, control)) in rows.into_iter().enumerate() {
            if row >= status_row {
                return;
            }
            let focused = index == self.focus;
            frame.render_widget(
                Paragraph::new(super::fields::form_line(label, focused, control)),
                Rect::new(cx, row, cw, 1),
            );
            row += 1;
        }
        // Centered action row: paint and click share the row's own
        // centering, so the two can never desync.
        {
            self.dialog_row().paint_centered(frame, cx, row, cw);
            row += 1;
            let _ = row;
        }
        // One fixed status slot: the error or blank. State changes
        // never move surrounding rows.
        let status = if let Some(err) = &self.error {
            Line::from(Span::styled(crate::ui::text::cut_chars(err, cw as usize), style(Role::Danger)))
        } else {
            Line::from("")
        };
        frame.render_widget(Paragraph::new(status), Rect::new(cx, status_row, cw, 1));
        let hint = Line::from(Span::styled(
            crate::ui::text::cut_chars("Tab move · ←/→ select · Enter save · Esc cancel", cw as usize),
            style(Role::Muted),
        ));
        frame.render_widget(Paragraph::new(hint), Rect::new(cx, hint_row, cw, 1));
    }
}

/// Centered dialog box: 80 wide, 21 tall for the ten-row form plus
/// action, status, and hint rows; clamped into tiny terminals.
pub fn card_edit_area(term: Rect) -> Rect {
    let (w, h) = (80.min(term.width), 21.min(term.height));
    Rect::new(
        term.x + term.width.saturating_sub(w) / 2,
        term.y + term.height.saturating_sub(h) / 2,
        w,
        h,
    )
}

/// Truncate to `room` chars at the end.


#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ch(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn sample_card() -> crate::kanban::board::Card {
        crate::kanban::board::Card {
            id: "c-1".to_string(),
            title: "fix leak".to_string(),
            description: "torn write".to_string(),
            column: "Backlog".to_string(),
            assignee: "kins".to_string(),
            tags: vec!["pty".to_string()],
            priority: crate::kanban::board::Priority::High,
            created_at: 1,
            updated_at: 1,
            start_date: Some("2026-09-01".to_string()),
            due_date: Some("2026-10-01".to_string()),
            estimate: Some(3),
            progress: 40,
        }
    }

    fn columns() -> Vec<String> {
        vec![
            "Backlog".to_string(),
            "Todo".to_string(),
            "Doing".to_string(),
            "Done".to_string(),
        ]
    }

    fn buffer_rows(
        terminal: &ratatui::Terminal<ratatui::backend::TestBackend>,
    ) -> Vec<String> {
        let buf = terminal.backend().buffer();
        let (w, h) = (buf.area.width as usize, buf.area.height as usize);
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| {
                        buf[(x as u16, y as u16)]
                            .symbol()
                            .chars()
                            .next()
                            .unwrap_or(' ')
                    })
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn prefills_every_row_from_the_card() {
        let d = CardEditDialog::new(&sample_card(), &columns(), true);
        assert_eq!(super::super::fields::input_text(&d.title), "fix leak");
        assert_eq!(super::super::fields::input_text(&d.description), "torn write");
        assert_eq!(super::super::fields::input_text(&d.assignee), "kins");
        assert_eq!(super::super::fields::input_text(&d.tags), "pty");
        assert_eq!(super::super::fields::input_text(&d.start), "2026-09-01");
        assert_eq!(super::super::fields::input_text(&d.due), "2026-10-01");
        assert_eq!(super::super::fields::input_text(&d.estimate), "3");
        assert_eq!(super::super::fields::input_text(&d.progress), "40");
    }

    #[test]
    fn tab_cycles_all_rows_and_wraps() {
        let mut d = CardEditDialog::new(&sample_card(), &columns(), true);
        assert_eq!(d.focus(), FOCUS_TITLE);
        for _ in 0..FOCUS_ACTIONS {
            d.key(&key(KeyCode::Tab));
        }
        assert_eq!(d.focus(), FOCUS_ACTIONS);
        d.key(&key(KeyCode::Tab));
        assert_eq!(d.focus(), FOCUS_TITLE);
        d.key(&key(KeyCode::BackTab));
        assert_eq!(d.focus(), FOCUS_ACTIONS);
    }

    #[test]
    fn typing_edits_title_and_enter_saves() {
        let mut d = CardEditDialog::new(&sample_card(), &columns(), true);
        for c in " v2".chars() {
            d.key(&ch(c));
        }
        match d.key(&key(KeyCode::Enter)) {
            CardEditOutcome::Submitted(out) => {
                assert_eq!(out.patch.title.as_deref(), Some("fix leak v2"));
                assert_eq!(out.column, None, "status unchanged moves nothing");
            }
            other => panic!("expected submit, got {other:?}"),
        }
    }

    #[test]
    fn empty_title_stays_open_with_error() {
        let mut d = CardEditDialog::new(&sample_card(), &columns(), true);
        // Clear the title row, then try to save.
        for _ in 0..32 {
            d.key(&key(KeyCode::Backspace));
        }
        match d.key(&key(KeyCode::Enter)) {
            CardEditOutcome::Pending => assert!(d.error().is_some(), "guidance in the modal"),
            other => panic!("expected pending, got {other:?}"),
        }
    }

    #[test]
    fn status_cycler_reports_a_column_move() {
        let mut d = CardEditDialog::new(&sample_card(), &columns(), true);
        d.key(&key(KeyCode::Down));
        d.key(&key(KeyCode::Down));
        assert_eq!(d.focus(), FOCUS_COLUMN);
        d.key(&key(KeyCode::Right));
        match d.key(&key(KeyCode::Enter)) {
            CardEditOutcome::Submitted(out) => {
                assert_eq!(out.column.as_deref(), Some("Todo"), "move reported");
            }
            other => panic!("expected submit, got {other:?}"),
        }
    }

    #[test]
    fn bad_estimate_and_progress_stay_open_with_error() {
        let mut d = CardEditDialog::new(&sample_card(), &columns(), true);
        for _ in 0..8 {
            d.key(&key(KeyCode::Tab));
        }
        assert_eq!(d.focus(), FOCUS_ESTIMATE);
        for _ in 0..8 {
            d.key(&key(KeyCode::Backspace));
        }
        for c in "lots".chars() {
            d.key(&ch(c));
        }
        match d.key(&key(KeyCode::Enter)) {
            CardEditOutcome::Pending => assert!(
                d.error().is_some_and(|e| e.contains("estimate")),
                "estimate error: {:?}",
                d.error()
            ),
            other => panic!("expected pending, got {other:?}"),
        }
    }

    #[test]
    fn clearing_a_date_row_sets_the_clear_flag() {
        let mut d = CardEditDialog::new(&sample_card(), &columns(), true);
        for _ in 0..7 {
            d.key(&key(KeyCode::Tab));
        }
        assert_eq!(d.focus(), FOCUS_DUE);
        for _ in 0..16 {
            d.key(&key(KeyCode::Backspace));
        }
        match d.key(&key(KeyCode::Enter)) {
            CardEditOutcome::Submitted(out) => {
                assert!(out.patch.clear_due_date, "blank due clears");
                assert_eq!(out.patch.due_date, None);
            }
            other => panic!("expected submit, got {other:?}"),
        }
    }

    #[test]
    fn esc_cancels_from_any_row() {
        let mut d = CardEditDialog::new(&sample_card(), &columns(), true);
        d.key(&key(KeyCode::Down));
        match d.key(&key(KeyCode::Esc)) {
            CardEditOutcome::Cancelled => {}
            other => panic!("expected cancel, got {other:?}"),
        }
    }

    #[test]
    fn render_centers_modal_and_pins_hint() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut d = CardEditDialog::new(&sample_card(), &columns(), true);
        let area = card_edit_area(ratatui::layout::Rect::new(0, 0, 100, 30));
        assert_eq!((area.width, area.height), (80, 21), "modal size pinned");
        terminal.draw(|f| d.view(f, area)).unwrap();
        let text = buffer_rows(&terminal).join("\n");
        for label in ["Title", "Description", "Status", "Assignee", "Priority", "Tags"] {
            assert!(text.contains(label), "row paints: {label}");
        }
        for label in ["Start date", "Due date", "Estimate", "Progress"] {
            assert!(text.contains(label), "row paints: {label}");
        }
        assert!(text.contains("Edit card"), "titled modal, never blank");
        assert!(text.contains("Esc cancel"), "hint pinned");
    }

    #[test]
    fn click_rows_take_focus_and_save_fires() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut d = CardEditDialog::new(&sample_card(), &columns(), true);
        let area = card_edit_area(ratatui::layout::Rect::new(0, 0, 100, 30));
        // Drive clicks from cells read back off a real paint, never
        // from hand-computed coordinates.
        terminal.draw(|f| d.view(f, area)).unwrap();
        let rows = buffer_rows(&terminal);
        let assignee_y = rows.iter().position(|r| r.contains("Assignee")).expect("row paints");
        let byte_x = rows[assignee_y].find("Assignee").expect("label cell");
        let cell_x = rows[assignee_y][..byte_x].chars().count() as u16 + 3;
        match d.click(cell_x, assignee_y as u16, area) {
            Some(CardEditOutcome::Pending) => assert_eq!(d.focus(), FOCUS_ASSIGNEE),
            other => panic!("expected focus, got {other:?}"),
        }
        terminal.draw(|f| d.view(f, area)).unwrap();
        let rows = buffer_rows(&terminal);
        let save_y = rows.iter().position(|r| r.contains("Save")).expect("actions paint");
        let byte_x = rows[save_y].find("Save").expect("button cell");
        let cell_x = rows[save_y][..byte_x].chars().count() as u16 + 1;
        match d.click(cell_x, save_y as u16, area) {
            Some(CardEditOutcome::Submitted(_)) => {}
            other => panic!("expected submit, got {other:?}"),
        }
    }

    #[test]
    fn click_outside_is_dead_but_swallowed_by_caller() {
        let mut d = CardEditDialog::new(&sample_card(), &columns(), true);
        let area = card_edit_area(ratatui::layout::Rect::new(0, 0, 100, 30));
        assert!(d.click(0, 0, area).is_none(), "outside dies here");
    }
}
