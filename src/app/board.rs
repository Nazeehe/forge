//! AppState board state: columns, cards, drafts, and the board view.

use super::*;

impl AppState {
    /// Flip the global kanban view. Opening it keeps the sidebar
    /// visible so the Kanban button stays clickable; the notice
    /// clears once the human has seen the board footer.
    pub fn toggle_board(&mut self) {
        self.board_open = !self.board_open;
        if self.board_open {
            self.ensure_board_focus();
            self.board_notice = None;
        }
        self.dirty = true;
    }

    /// Point the focus at a live board, defaulting to the first one,
    /// and clamp both cursors so deletion can never leave them dangling.
    pub fn ensure_board_focus(&mut self) {
        let live = self
            .board_focus
            .board
            .as_deref()
            .and_then(|id| self.boards.board(id))
            .is_some();
        if !live {
            self.board_focus.board = self.boards.boards.first().map(|b| b.id.clone());
            self.board_focus.column = 0;
            self.board_focus.card = 0;
        }
        if let Some(id) = self.board_focus.board.clone() {
            if let Some(b) = self.boards.board(&id) {
                self.board_focus.column =
                    crate::kanban::board::clamp_index(b.columns.len(), self.board_focus.column);
                let column = b
                    .columns
                    .get(self.board_focus.column)
                    .map(|c| c.name.clone())
                    .unwrap_or_default();
                self.board_focus.card =
                    crate::kanban::board::clamp_index(b.cards_in(&column).len(), self.board_focus.card);
            }
        }
    }

    /// Load workspace boards; a missing file starts empty, anything
    /// unreadable or corrupt quarantines aside with a footer notice.
    pub fn load_boards(&mut self, home: &std::path::Path) {
        let path = crate::infra::branding::kanban_file(home);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                self.board_notice =
                    Some(format!("kanban save unreadable ({e}); starting empty"));
                return;
            }
        };
        match crate::kanban::board::BoardStore::from_json_str(&text) {
            Ok(store) => {
                self.boards = store;
            }
            Err(e) => {
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let stale = path.with_extension(format!("corrupt-{stamp}.json"));
                let _ = std::fs::rename(&path, &stale);
                self.boards = crate::kanban::board::BoardStore::new();
                self.board_notice =
                    Some(format!("kanban save corrupt ({e}); quarantined, starting empty"));
            }
        }
        self.ensure_board_focus();
    }

    /// Persist workspace boards atomically; failures are the caller's
    /// to surface (a toast/notice), never silent.
    pub fn save_boards(&self, home: &std::path::Path) -> std::io::Result<()> {
        crate::infra::fs_atomic::write_atomic(
            &crate::infra::branding::kanban_file(home),
            self.boards.to_json_string().as_bytes(),
        )
    }

    /// Flush pending board mutations; failures land in the footer
    /// notice instead of blocking the loop.
    pub fn flush_boards(&mut self, home: &std::path::Path) {
        if !self.boards_dirty {
            return;
        }
        match self.save_boards(home) {
            Ok(()) => self.boards_dirty = false,
            Err(e) => self.board_notice = Some(format!("kanban save failed ({e})")),
        }
    }

    fn focused_board(&self) -> Option<&crate::kanban::board::Board> {
        self.board_focus.board.as_deref().and_then(|id| self.boards.board(id))
    }

    fn focused_board_mut(&mut self) -> Option<&mut crate::kanban::board::Board> {
        let id = self.board_focus.board.clone()?;
        self.boards.board_mut(&id)
    }

    fn focused_card_id(&self) -> Option<String> {
        let board = self.focused_board()?;
        let column = board.columns.get(self.board_focus.column)?.name.clone();
        board.cards_in(&column).get(self.board_focus.card).map(|c| c.id.clone())
    }

    fn note_boards_changed(&mut self) {
        self.boards_dirty = true;
        self.dirty = true;
    }

    /// Move the column cursor; the card cursor clamps into the column.
    pub fn board_step_column(&mut self, dir: i32) {
        let len = self.focused_board().map(|b| b.columns.len()).unwrap_or(0);
        if len == 0 {
            return;
        }
        let next = (self.board_focus.column as i32 + dir).clamp(0, len as i32 - 1) as usize;
        self.board_focus.column = next;
        self.ensure_board_focus();
        self.dirty = true;
    }

    /// Move the card cursor inside the focused column.
    pub fn board_step_card(&mut self, dir: i32) {
        let len = self
            .focused_board()
            .and_then(|b| b.columns.get(self.board_focus.column))
            .map(|c| {
                self.focused_board()
                    .map(|b| b.cards_in(&c.name).len())
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        if len == 0 {
            return;
        }
        self.board_focus.card =
            (self.board_focus.card as i32 + dir).clamp(0, len as i32 - 1) as usize;
        self.dirty = true;
    }

    /// Move the focused card `dir` columns over; blocked moves report
    /// the WIP limit in the footer and stay put.
    pub fn board_shift_focused_card(&mut self, dir: i32) {
        let dest = match self.focused_board() {
            Some(b) if !b.columns.is_empty() => (self.board_focus.column as i32 + dir)
                .clamp(0, b.columns.len() as i32 - 1) as usize,
            _ => return,
        };
        let target = match self.focused_board().and_then(|b| b.columns.get(dest)) {
            Some(col) => col.name.clone(),
            None => return,
        };
        let Some(id) = self.focused_card_id() else { return };
        match self.focused_board_mut().map(|b| b.card_move(&id, &target)) {
            Some(Ok(())) => {
                self.board_focus.column = dest;
                self.refocus_card(&id);
                self.note_boards_changed();
            }
            Some(Err(e)) => {
                self.board_notice = Some(e.to_string());
                self.dirty = true;
            }
            None => {}
        }
    }

    /// Send the focused card to the first or last column.
    pub fn board_send_focused_card(&mut self, to_last: bool) {
        let target = match self.focused_board() {
            Some(b) if !b.columns.is_empty() => {
                if to_last {
                    b.columns.last().map(|c| c.name.clone())
                } else {
                    b.columns.first().map(|c| c.name.clone())
                }
            }
            _ => None,
        };
        let (Some(target), Some(id)) = (target, self.focused_card_id()) else { return };
        match self.focused_board_mut().map(|b| b.card_move(&id, &target)) {
            Some(Ok(())) => {
                let col = self
                    .focused_board()
                    .and_then(|b| b.columns.iter().position(|c| c.name == target))
                    .unwrap_or(0);
                self.board_focus.column = col;
                self.refocus_card(&id);
                self.note_boards_changed();
            }
            Some(Err(e)) => {
                self.board_notice = Some(e.to_string());
                self.dirty = true;
            }
            None => {}
        }
    }

    /// Complete the focused card: last column, progress 100.
    pub fn board_complete_focused(&mut self) {
        self.board_send_focused_card(true);
    }

    /// Delete the focused card; the footer confirms what went away.
    pub fn board_delete_focused(&mut self) {
        let Some(id) = self.focused_card_id() else { return };
        let title = self
            .focused_board()
            .and_then(|b| b.card(&id))
            .map(|c| c.title.clone())
            .unwrap_or_default();
        if self.focused_board_mut().is_some_and(|b| b.card_delete(&id).is_ok()) {
            self.board_notice = Some(format!("deleted '{title}'"));
            self.ensure_board_focus();
            self.note_boards_changed();
        }
    }

    /// Cycle the focused card's priority (`p`).
    pub fn board_cycle_priority_focused(&mut self) {
        let Some(id) = self.focused_card_id() else { return };
        let next = self
            .focused_board()
            .and_then(|b| b.card(&id))
            .map(|c| c.priority.cycle());
        if let (Some(next), Some(board)) = (next, self.focused_board_mut()) {
            let patch = crate::kanban::board::CardPatch { priority: Some(next), ..Default::default() };
            if board.card_update(&id, patch).is_ok() {
                self.note_boards_changed();
            }
        }
    }

    /// Nudge the focused card's progress; clamps 0–100.
    pub fn board_bump_progress_focused(&mut self, delta: i16) {
        let Some(id) = self.focused_card_id() else { return };
        let next = self
            .focused_board()
            .and_then(|b| b.card(&id))
            .map(|c| (c.progress as i16 + delta).clamp(0, 100) as u16);
        if let (Some(next), Some(board)) = (next, self.focused_board_mut()) {
            let patch = crate::kanban::board::CardPatch { progress: Some(next), ..Default::default() };
            if board.card_update(&id, patch).is_ok() {
                self.note_boards_changed();
            }
        }
    }

    /// Reorder the focused card inside its column; the cursor follows.
    pub fn board_reorder_focused(&mut self, dir: crate::kanban::board::Shift) {
        let Some(id) = self.focused_card_id() else { return };
        if self.focused_board_mut().is_some_and(|b| b.shift_card(&id, dir).is_ok()) {
            self.refocus_card(&id);
            self.note_boards_changed();
        }
    }

    /// Jump the column cursor to `index`, clamped (`1`–`4`).
    pub fn board_focus_column_index(&mut self, index: usize) {
        let len = self.focused_board().map(|b| b.columns.len()).unwrap_or(0);
        if len == 0 {
            return;
        }
        self.board_focus.column = index.min(len - 1);
        self.ensure_board_focus();
        self.dirty = true;
    }

    /// Jump the card cursor to the first or last card (`g`/`G`,
    /// `Home`/`End`).
    pub fn board_focus_card_edge(&mut self, last: bool) {
        let len = self
            .focused_board()
            .and_then(|b| b.columns.get(self.board_focus.column))
            .map(|c| {
                self.focused_board()
                    .map(|b| b.cards_in(&c.name).len())
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        if len == 0 {
            return;
        }
        self.board_focus.card = if last { len - 1 } else { 0 };
        self.dirty = true;
    }

    /// Cycle to the next board, wrapping (`W`).
    pub fn board_cycle_board(&mut self) {
        if self.boards.boards.is_empty() {
            return;
        }
        let next = self
            .board_focus
            .board
            .as_deref()
            .and_then(|id| self.boards.boards.iter().position(|b| b.id == id))
            .map(|i| (i + 1) % self.boards.boards.len())
            .unwrap_or(0);
        self.board_focus.board = self.boards.boards.get(next).map(|b| b.id.clone());
        self.board_focus.column = 0;
        self.board_focus.card = 0;
        self.ensure_board_focus();
        self.dirty = true;
    }

    /// Show the focused card's details in the footer (`Enter`).
    pub fn board_enter_details(&mut self) {
        let text = self.focused_card_id().and_then(|id| {
            self.focused_board().and_then(|b| {
                b.card(&id).map(|c| {
                    let mut parts = vec![c.title.clone()];
                    if !c.description.is_empty() {
                        parts.push(c.description.clone());
                    }
                    let mut meta = format!("priority {}", c.priority.as_str());
                    if !c.assignee.is_empty() {
                        meta.push_str(&format!(" · @{}", c.assignee));
                    }
                    meta.push_str(&format!(" · {}%", c.progress));
                    if let Some(est) = c.estimate {
                        meta.push_str(&format!(" · {est}pt"));
                    }
                    match (&c.start_date, &c.due_date) {
                        (Some(s), Some(d)) => {
                            meta.push_str(&format!(" · {s} → {d}"));
                        }
                        (Some(s), None) => {
                            meta.push_str(&format!(" · since {s}"));
                        }
                        (None, Some(d)) => {
                            meta.push_str(&format!(" · due {d}"));
                        }
                        (None, None) => {}
                    }
                    if !c.tags.is_empty() {
                        meta.push_str(&format!(" · #{}", c.tags.join(" #")));
                    }
                    parts.push(meta);
                    parts.join(" — ")
                })
            })
        });
        if let Some(text) = text {
            let flat: String = text.chars().take(240).collect();
            self.board_notice = Some(flat);
            self.dirty = true;
        }
    }

    /// Point the card cursor at a card by id (used after mutations move
    /// it inside its new column).
    fn refocus_card(&mut self, id: &str) {
        let column = self
            .focused_board()
            .and_then(|b| b.columns.get(self.board_focus.column))
            .map(|c| c.name.clone())
            .unwrap_or_default();
        if let Some(board) = self.focused_board() {
            let cards = board.cards_in(&column);
            if let Some(pos) = cards.iter().position(|c| c.id == id) {
                self.board_focus.card = pos;
            } else {
                self.board_focus.card =
                    crate::kanban::board::clamp_index(cards.len(), self.board_focus.card);
            }
        }
    }

    /// Testable content for the board view: selection, escaped text,
    /// width-picked hints. Frame rendering lives in `ui`.
    pub fn board_view(&self) -> crate::ui::board::BoardView {
        let wide = self.term_size.1 >= 100;
        let hints = if wide {
            vec![
                "h/l columns · j/k cards · 1-4 jump · Space/. move · x complete · d delete".to_string(),
                "a add · e edit · Enter details · p priority · +/- progress · W boards · q back"
                    .to_string(),
            ]
        } else {
            vec!["h/l/j/k move · Tab column · a add · e edit · x done".to_string()]
        };
        let notice = self.board_notice.clone();
        let draft = self
            .board_draft
            .as_ref()
            .map(|d| format!("{}: {}▌", d.prompt, d.buffer));
        let (Some(id), Some(board)) = (
            self.board_focus.board.clone(),
            self.board_focus.board.as_deref().and_then(|id| self.boards.board(id)),
        ) else {
            return crate::ui::board::BoardView {
                title: "Kanban".to_string(),
                columns: Vec::new(),
                focus_col: 0,
                hints,
                notice,
                draft,
                empty: Some("No boards yet — press a to create one.".to_string()),
            };
        };
        let _ = id;
        let columns = board
            .columns
            .iter()
            .enumerate()
            .map(|(ci, col)| {
                let cards = board.cards_in(&col.name);
                crate::ui::board::BoardColumnView {
                    name: crate::infra::safe_text::encode_for_display(&col.name),
                    wip_limit: col.wip_limit,
                    count: cards.len(),
                    selected: ci == self.board_focus.column,
                    cards: cards
                        .iter()
                        .enumerate()
                        .map(|(ki, card)| {
                            let mut meta = card.priority.as_str().to_string();
                            if !card.assignee.is_empty() {
                                meta.push_str(&format!(
                                    " @{}",
                                    crate::infra::safe_text::encode_for_display(&card.assignee)
                                ));
                            }
                            meta.push_str(&format!(" {}%", card.progress));
                            if let Some(est) = card.estimate {
                                meta.push_str(&format!(" · {est}pt"));
                            }
                            if let Some(start) = card.start_date.as_deref() {
                                meta.push_str(&format!(
                                    " · {}→",
                                    crate::infra::safe_text::encode_for_display(start)
                                ));
                            }
                            if let Some(due) = card.due_date.as_deref() {
                                meta.push_str(&format!(
                                    " · {}",
                                    crate::infra::safe_text::encode_for_display(due)
                                ));
                            }
                            crate::ui::board::BoardCardView {
                                title: crate::infra::safe_text::encode_for_display(&card.title),
                                meta,
                                selected: ci == self.board_focus.column
                                    && ki == self.board_focus.card,
                            }
                        })
                        .collect(),
                }
            })
            .collect();
        crate::ui::board::BoardView {
            title: format!("Kanban · {}", crate::infra::safe_text::encode_for_display(&board.name)),
            columns,
            focus_col: self.board_focus.column,
            hints,
            notice,
            draft,
            empty: None,
        }
    }

    /// Open the `a` draft: a board when there is nothing yet, else a
    /// card in the focused column.
    pub fn board_start_add_draft(&mut self) {
        if self.boards.boards.is_empty() {
            self.board_draft = Some(BoardDraft {
                prompt: "New board name",
                buffer: String::new(),
                action: BoardDraftAction::NewBoard,
            });
        } else {
            self.ensure_board_focus();
            if self.focused_board().is_none() {
                return;
            }
            self.board_draft = Some(BoardDraft {
                prompt: "New card",
                buffer: String::new(),
                action: BoardDraftAction::NewCard,
            });
        }
        self.dirty = true;
    }

    /// Open the `e` editor: the full-field card modal prefilled from
    /// the focused card. Kept under the old `board_start_title_draft`
    /// name so the `e` binding does not move.
    pub fn board_start_title_draft(&mut self) {
        let (Some(id), Some(board)) = (
            self.focused_card_id(),
            self.focused_board().cloned(),
        ) else {
            return;
        };
        let Some(card) = board.card(&id) else {
            return;
        };
        let columns = board.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
        self.card_edit =
            Some(crate::ui::dialogs::card_edit::CardEditDialog::new(card, &columns, self.pill_tabs));
        self.board_draft = None;
        self.dirty = true;
    }

    /// One key inside the open card editor: typing edits the focused
    /// row, Enter saves, Esc cancels. Anything else stays inside the
    /// modal so board navigation never fires mid-edit.
    pub fn board_card_edit_key(
        &mut self,
        key: &crossterm::event::KeyEvent,
    ) -> Option<crate::ui::dialogs::card_edit::CardEditOutcome> {
        let outcome = self.card_edit.as_mut().map(|d| d.key(key))?;
        match outcome {
            crate::ui::dialogs::card_edit::CardEditOutcome::Pending => {
                self.dirty = true;
                Some(crate::ui::dialogs::card_edit::CardEditOutcome::Pending)
            }
            crate::ui::dialogs::card_edit::CardEditOutcome::Cancelled => {
                self.card_edit = None;
                self.dirty = true;
                None
            }
            crate::ui::dialogs::card_edit::CardEditOutcome::Submitted(result) => {
                self.apply_card_edit(result);
                None
            }
        }
    }

    /// One left-click inside the open card editor: rows take focus,
    /// Save/Cancel fire at once. Dead space and outside clicks are
    /// `None`; the caller still swallows everything while open.
    pub fn board_card_edit_click(&mut self, col: u16, row: u16, area: ratatui::layout::Rect) {
        let outcome = self.card_edit.as_mut().and_then(|d| d.click(col, row, area));
        match outcome {
            Some(crate::ui::dialogs::card_edit::CardEditOutcome::Submitted(result)) => {
                self.apply_card_edit(result);
            }
            Some(crate::ui::dialogs::card_edit::CardEditOutcome::Cancelled) => {
                self.card_edit = None;
                self.dirty = true;
            }
            _ => {
                self.dirty = true;
            }
        }
    }

    /// Paste bracketed-paste text into the open card editor's focused
    /// text row. No-op without an open editor so pastes never leak.
    pub fn board_card_edit_paste(&mut self, text: &str) {
        if let Some(dialog) = self.card_edit.as_mut() {
            dialog.paste(text);
            self.dirty = true;
        }
    }

    /// Apply a submitted card edit: optional WIP-checked status move
    /// first, then the scalar patch. A blocked move reports in the
    /// footer and keeps the modal open so nothing is lost; scalar
    /// failures do the same. Success closes and confirms.
    fn apply_card_edit(&mut self, result: crate::ui::dialogs::card_edit::CardEditResult) {
        let Some(id) = self.focused_card_id() else {
            self.board_notice = Some("no card focused".to_string());
            self.dirty = true;
            return;
        };
        let target = result.column.clone();
        let patch = result.patch;
        if let Some(target) = target.as_deref() {
            let moved = self
                .focused_board_mut()
                .map(|board| board.card_move(&id, target))
                .unwrap_or(Err(crate::kanban::board::BoardError::CardNotFound(id.clone())));
            if let Err(e) = moved {
                self.board_notice = Some(e.to_string());
                self.dirty = true;
                return;
            }
            if let Some(next) = self
                .focused_board()
                .and_then(|b| b.columns.iter().position(|c| c.name == target))
            {
                self.board_focus.column = next;
            }
        }
        match self
            .focused_board_mut()
            .map(|board| board.card_update(&id, patch))
            .unwrap_or(Err(crate::kanban::board::BoardError::CardNotFound(id.clone())))
        {
            Ok(()) => {
                self.card_edit = None;
                self.refocus_card(&id);
                self.board_notice = Some("card updated".to_string());
                self.ensure_board_focus();
                self.note_boards_changed();
                self.dirty = true;
            }
            Err(e) => {
                self.board_notice = Some(e.to_string());
                self.dirty = true;
            }
        }
    }

    /// Sanitize pasted text into the open draft: breaks and tabs
    /// collapse to one space, other controls drop, capped like typing.
    /// No-op outside draft mode so pastes never leak anywhere else.
    pub fn board_push_paste(&mut self, text: &str) {
        let Some(draft) = self.board_draft.as_mut() else {
            return;
        };
        for c in text.chars() {
            if draft.buffer.chars().count() >= BOARD_DRAFT_MAX {
                break;
            }
            if c == '\n' || c == '\r' || c == '\t' {
                if draft.buffer.chars().last().is_some_and(|l| l != ' ') {
                    draft.buffer.push(' ');
                }
            } else if !c.is_control() {
                draft.buffer.push(c);
            }
        }
        self.dirty = true;
    }

    /// One key inside an open board draft: text appends (capped),
    /// Backspace deletes, Enter submits, Esc cancels. Anything else
    /// is swallowed so navigation chords never fire mid-typing.
    pub fn board_draft_key(&mut self, key: &crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return;
        }
        if self.board_draft.is_none() {
            return;
        }
        match key.code {
            KeyCode::Esc if key.modifiers.is_empty() => {
                self.board_draft = None;
                self.dirty = true;
            }
            KeyCode::Enter if key.modifiers.is_empty() => self.board_submit_draft(),
            KeyCode::Backspace if key.modifiers.is_empty() => {
                if let Some(draft) = self.board_draft.as_mut() {
                    draft.buffer.pop();
                }
                self.dirty = true;
            }
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                if let Some(draft) = self.board_draft.as_mut() {
                    if draft.buffer.chars().count() < BOARD_DRAFT_MAX {
                        draft.buffer.push(c);
                    }
                }
                self.dirty = true;
            }
            _ => {}
        }
    }

    /// Submit the open draft. Blank drafts stay open with guidance;
    /// failures keep the draft so the text is never lost; success
    /// closes it, focuses the new thing, and confirms in the footer.
    fn board_submit_draft(&mut self) {
        let (action, text) = match self.board_draft.as_ref() {
            Some(draft) => (draft.action, draft.buffer.trim().to_string()),
            None => return,
        };
        if text.is_empty() {
            self.board_notice = Some("type a name first".to_string());
            self.dirty = true;
            return;
        }
        let result = match action {
            BoardDraftAction::NewBoard => self.submit_new_board(&text),
            BoardDraftAction::NewCard => self.submit_new_card(&text),
            BoardDraftAction::EditTitle => self.submit_edit_title(&text),
        };
        match result {
            Ok(notice) => {
                self.board_draft = None;
                self.board_notice = Some(notice);
                self.ensure_board_focus();
                self.note_boards_changed();
            }
            Err(e) => {
                self.board_notice = Some(e);
                self.dirty = true;
            }
        }
    }

    fn submit_new_board(&mut self, name: &str) -> Result<String, String> {
        self.boards.board_create(name, None).map_err(|e| e.to_string())?;
        let id = self.boards.board(name).map(|b| b.id.clone());
        self.board_focus.board = id;
        self.board_focus.column = 0;
        self.board_focus.card = 0;
        Ok(format!("created board '{name}'"))
    }

    fn submit_new_card(&mut self, title: &str) -> Result<String, String> {
        let column = self
            .focused_board()
            .and_then(|b| b.columns.get(self.board_focus.column))
            .map(|c| c.name.clone())
            .ok_or_else(|| "no column focused".to_string())?;
        let mut draft = crate::kanban::board::CardDraft::new(title);
        draft.column = Some(column);
        let id = self
            .focused_board_mut()
            .ok_or_else(|| "no board focused".to_string())?
            .card_create(draft)
            .map_err(|e| e.to_string())?;
        self.refocus_card(&id);
        Ok(format!("added '{title}'"))
    }

    fn submit_edit_title(&mut self, title: &str) -> Result<String, String> {
        let Some(id) = self.focused_card_id() else {
            return Err("no card focused".to_string());
        };
        let patch = crate::kanban::board::CardPatch {
            title: Some(title.to_string()),
            ..Default::default()
        };
        self.focused_board_mut()
            .ok_or_else(|| "no board focused".to_string())?
            .card_update(&id, patch)
            .map_err(|e| e.to_string())?;
        Ok(format!("renamed to '{title}'"))
    }
}

pub(super) fn scratch_home() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static BOARD_HOME_COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "forge-board-test-{}-{}",
        std::process::id(),
        BOARD_HOME_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::*;

        #[test]
        fn board_toggle_flips_global_view() {
            let mut s = AppState::new();
            assert!(!s.board_open);
            s.dirty = false;
            s.toggle_board();
            assert!(s.board_open);
            assert!(s.dirty);
            s.toggle_board();
            assert!(!s.board_open);
        }

        #[test]
        fn board_load_missing_starts_empty_without_notice() {
            let mut s = AppState::new();
            s.load_boards(&scratch_home());
            assert!(s.boards.boards.is_empty());
            assert!(s.board_notice.is_none());
        }

        #[test]
        fn board_load_corrupt_quarantines_and_notices() {
            let mut s = AppState::new();
            let home = scratch_home();
            let path = crate::infra::branding::kanban_file(&home);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"{torn").unwrap();
            s.load_boards(&home);
            assert!(s.boards.boards.is_empty());
            assert!(s.board_notice.is_some());
            assert!(!path.exists(), "corrupt save is quarantined away");
        }

        #[test]
        fn board_save_and_reload_round_trips() {
            let mut s = AppState::new();
            let home = scratch_home();
            s.boards.board_create("team", None).unwrap();
            s.save_boards(&home).unwrap();
            let mut t = AppState::new();
            t.load_boards(&home);
            assert_eq!(t.boards.board_names(), vec!["team".to_string()]);
            assert!(t.board_notice.is_none());
        }

        #[test]
        fn board_view_marks_selection_and_hints() {
            let mut s = AppState::new();
            s.boards.board_create("team", None).unwrap();
            let draft_a = crate::kanban::board::CardDraft::new("alpha");
            s.boards.board_mut("team").unwrap().card_create(draft_a).unwrap();
            let mut draft_b = crate::kanban::board::CardDraft::new("beta");
            draft_b.column = Some("Todo".to_string());
            s.boards.board_mut("team").unwrap().card_create(draft_b).unwrap();
            s.ensure_board_focus();
            let view = s.board_view();
            assert_eq!(view.title, "Kanban · team");
            assert_eq!(view.columns.len(), 4);
            assert!(view.empty.is_none());
            assert!(view.columns[0].selected, "cursor starts on first column");
            assert!(view.columns[0].cards.iter().any(|c| c.title == "alpha" && c.selected));
            s.board_step_column(1);
            let view = s.board_view();
            assert!(view.columns[1].selected, "step moves the column cursor");
            assert!(view.columns[1].cards.iter().any(|c| c.title == "beta" && c.selected));
            assert!(!view.hints.is_empty(), "footer hints always paint");
        }

        #[test]
        fn board_view_empty_without_boards() {
            let s = AppState::new();
            let view = s.board_view();
            assert!(view.empty.is_some(), "never a blank panel");
            assert!(view.columns.is_empty());
        }

        #[test]
        fn board_move_blocked_by_wip_sets_notice() {
            let mut s = AppState::new();
            s.boards.board_create("team", None).unwrap();
            for t in ["one", "two", "three"] {
                let id = s.boards.board_mut("team").unwrap().card_create(crate::kanban::board::CardDraft::new(t)).unwrap();
                s.boards.board_mut("team").unwrap().card_move(&id, "Doing").unwrap();
            }
            let mut fourth = crate::kanban::board::CardDraft::new("fourth");
            fourth.column = Some("Todo".to_string());
            let extra = s.boards.board_mut("team").unwrap().card_create(fourth).unwrap();
            s.ensure_board_focus();
            // Cursor to Todo's "fourth"; shifting right targets full Doing.
            s.board_step_column(1);
            s.board_shift_focused_card(1);
            let card = s.boards.board("team").unwrap().card(&extra).unwrap();
            assert_eq!(card.column, "Todo", "blocked move stays put");
            assert!(s.board_notice.is_some_and(|n| n.contains("WIP")), "actionable notice");
            assert!(!s.boards_dirty, "a blocked move persists nothing");
        }

        #[test]
        fn board_complete_and_delete_focused_card() {
            let mut s = AppState::new();
            s.boards.board_create("team", None).unwrap();
            let id = s.boards.board_mut("team").unwrap().card_create(crate::kanban::board::CardDraft::new("ship")).unwrap();
            s.ensure_board_focus();
            s.board_complete_focused();
            assert_eq!(s.boards.board("team").unwrap().card(&id).unwrap().progress, 100);
            s.board_delete_focused();
            assert!(s.boards.board("team").unwrap().card(&id).is_none());
            assert!(s.board_notice.is_some(), "delete confirms in the footer");
        }

        #[test]
        fn board_cycle_board_wraps() {
            let mut s = AppState::new();
            s.boards.board_create("one", None).unwrap();
            s.boards.board_create("two", None).unwrap();
            s.ensure_board_focus();
            let first = s.board_focus.board.clone().unwrap();
            s.board_cycle_board();
            let second = s.board_focus.board.clone().unwrap();
            assert_ne!(first, second);
            s.board_cycle_board();
            assert_eq!(s.board_focus.board.clone().unwrap(), first);
        }

        #[test]
        fn board_push_paste_sanitizes_and_needs_draft() {
            let mut s = AppState::new();
            // No draft: the paste goes nowhere, nothing to leak into.
            s.board_push_paste("nope");
            assert!(s.board_notice.is_none());
            s.boards.board_create("team", None).unwrap();
            s.ensure_board_focus();
            s.board_start_add_draft();
            s.board_push_paste("fix\nleak\tx\u{7}!");
            assert_eq!(
                s.board_draft.as_ref().map(|d| d.buffer.as_str()),
                Some("fix leak x!"),
                "breaks collapse, controls drop"
            );
        }

        #[test]
        fn board_view_escapes_hostile_text() {
            let mut s = AppState::new();
            s.boards.board_create("team", None).unwrap();
            let mut draft = crate::kanban::board::CardDraft::new("<script>alert(1)</script>");
            draft.assignee = Some("a\u{202E}b".to_string());
            s.boards.board_mut("team").unwrap().card_create(draft).unwrap();
            s.ensure_board_focus();
            let view = s.board_view();
            let card = &view.columns[0].cards[0];
            // Terminal threat model is control/bidi/invisible spoofing
            // (angle brackets paint literally and execute nothing here).
            for shown in [&card.title, &card.meta] {
                assert!(
                    !crate::infra::safe_text::contains_bidi_controls(shown),
                    "bidi encoded: {shown:?}"
                );
                assert!(
                    !crate::infra::safe_text::contains_invisibles(shown),
                    "invisibles encoded: {shown:?}"
                );
                assert!(
                    !shown.chars().any(|c| c.is_control()),
                    "controls encoded: {shown:?}"
                );
            }
        }

        #[test]
        fn board_focus_defaults_to_first_board_and_clamps() {
            let mut s = AppState::new();
            s.boards.board_create("team", None).unwrap();
            s.ensure_board_focus();
            let first = s.boards.boards.first().map(|b| b.id.clone());
            assert_eq!(s.board_focus.board, first);
            s.board_focus.card = 99;
            s.ensure_board_focus();
            assert_eq!(s.board_focus.card, 0);
        }
}
