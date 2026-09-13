//! Group management dialog: keyboard-first local group CRUD.
//!
//! Groups are human-only organization: no MCP tool exposes them, agents
//! only feel them through ask/tell gating. `n` news a group (typed name),
//! `a` edits members (checkbox list), `r` renames (typed name), `d`
//! deletes, arrows/`j`/`k` move, Enter applies, Esc steps out or closes.

use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use tui_realm_stdlib::components::{Checkbox, List};
use tuirealm::command::Cmd;
use tuirealm::component::Component;

use crate::session::SessionId;

/// One group row for the dialog: name plus live member count.
#[derive(Clone, Debug)]
pub struct GroupRow {
    pub name: String,
    pub members: usize,
    pub member_names: Vec<String>,
    pub color: usize,
}

/// One session row: identity plus display name, in bar order.
#[derive(Clone, Debug)]
pub struct SessionRow {
    pub id: SessionId,
    pub name: String,
}

/// Live snapshot the dialog reads but never owns: group list, session
/// list, and the current members of the dialog's selected group (for the
/// checkbox pre-check).
#[derive(Clone, Debug)]
pub struct GroupCtx {
    pub groups: Vec<GroupRow>,
    pub sessions: Vec<SessionRow>,
    pub members: Vec<SessionId>,
}

/// Dialog result after one input: still open, closed, or one broker
/// mutation for the caller to apply (the dialog stays open in List).
#[derive(Debug)]
pub enum GroupOutcome {
    Pending,
    Closed,
    Create(String),
    Rename { from: String, to: String },
    Delete(String),
    SetMembers { group: String, members: Vec<SessionId> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    List,
    Name { rename: bool },
    Members,
}

/// Group dialog state: a mode, a selected group name (stable across
/// mutations, unlike an index), a row cursor, a name buffer, and the
/// checkbox set for member editing.
pub struct GroupDialog {
    mode: Mode,
    selected: String,
    cursor: usize,
    name_buf: String,
    checked: HashSet<SessionId>,
    error: Option<String>,
}

impl GroupDialog {
    pub fn new() -> Self {
        GroupDialog {
            mode: Mode::List,
            selected: String::new(),
            cursor: 0,
            name_buf: String::new(),
            checked: HashSet::new(),
            error: None,
        }
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Selected group name ("" when nothing is selected yet).
    pub fn selected_name(&self) -> &str {
        &self.selected
    }

    /// Group under the cursor, if the selection names a live group.
    fn current<'a>(&self, ctx: &'a GroupCtx) -> Option<&'a str> {
        ctx.groups
            .iter()
            .find(|g| g.name == self.selected)
            .map(|g| g.name.as_str())
    }

    /// Reconcile selection with a fresh snapshot: a deleted selection
    /// falls to the cursor row, and the cursor clamps into range. In
    /// member-edit mode the cursor walks the session list instead, so it
    /// only clamps there and never snaps back to the group row.
    fn reconcile(&mut self, ctx: &GroupCtx) {
        if self.mode == Mode::Members {
            if !ctx.sessions.is_empty() {
                self.cursor = self.cursor.min(ctx.sessions.len() - 1);
            } else {
                self.cursor = 0;
            }
            return;
        }
        if ctx.groups.iter().all(|g| g.name != self.selected) {
            self.selected = ctx
                .groups
                .get(self.cursor.min(ctx.groups.len().saturating_sub(1)))
                .map(|g| g.name.clone())
                .unwrap_or_default();
        }
        if let Some(pos) = ctx.groups.iter().position(|g| g.name == self.selected) {
            self.cursor = pos;
        } else {
            self.cursor = 0;
        }
    }

    fn move_cursor(&mut self, ctx: &GroupCtx, dir: i32) {
        if ctx.groups.is_empty() {
            return;
        }
        let len = ctx.groups.len() as i32;
        self.cursor = (self.cursor as i32 + dir).rem_euclid(len) as usize;
        self.selected = ctx.groups[self.cursor].name.clone();
        self.error = None;
    }

    /// One key against a fresh snapshot. Validation (empty/duplicate
    /// names) never closes the dialog: it reports and stays.
    pub fn key(&mut self, key: &KeyEvent, ctx: &GroupCtx) -> GroupOutcome {
        self.reconcile(ctx);
        match self.mode {
            Mode::List => self.key_list(key, ctx),
            Mode::Name { rename } => self.key_name(key, ctx, rename),
            Mode::Members => self.key_members(key, ctx),
        }
    }

    fn key_list(&mut self, key: &KeyEvent, ctx: &GroupCtx) -> GroupOutcome {
        match key.code {
            KeyCode::Esc => return GroupOutcome::Closed,
            KeyCode::Up => {
                self.move_cursor(ctx, -1);
                return GroupOutcome::Pending;
            }
            KeyCode::Down => {
                self.move_cursor(ctx, 1);
                return GroupOutcome::Pending;
            }
            KeyCode::Char('n') if key.modifiers.is_empty() => {
                self.mode = Mode::Name { rename: false };
                self.name_buf.clear();
                self.error = None;
                return GroupOutcome::Pending;
            }
            KeyCode::Char('r') if key.modifiers.is_empty() => {
                let Some(current) = self.current(ctx).map(str::to_string) else {
                    self.error = Some("no group selected — n creates one".to_string());
                    return GroupOutcome::Pending;
                };
                self.mode = Mode::Name { rename: true };
                self.name_buf = current;
                self.error = None;
                return GroupOutcome::Pending;
            }
            KeyCode::Char('d') if key.modifiers.is_empty() => {
                let Some(current) = self.current(ctx).map(str::to_string) else {
                    self.error = Some("no group selected — n creates one".to_string());
                    return GroupOutcome::Pending;
                };
                // Selection falls to the next row (or previous at the end)
                // once the caller applies the delete.
                let rest: Vec<&str> = ctx
                    .groups
                    .iter()
                    .map(|g| g.name.as_str())
                    .filter(|n| *n != current)
                    .collect();
                self.selected = rest
                    .get(self.cursor.min(rest.len().saturating_sub(1)))
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                self.error = None;
                return GroupOutcome::Delete(current);
            }
            KeyCode::Char('a') if key.modifiers.is_empty() => {
                let Some(_) = self.current(ctx) else {
                    self.error = Some("no group selected — n creates one".to_string());
                    return GroupOutcome::Pending;
                };
                self.mode = Mode::Members;
                self.cursor = 0;
                self.checked = ctx.members.iter().copied().collect();
                self.error = None;
                return GroupOutcome::Pending;
            }
            KeyCode::Char('k') if key.modifiers.is_empty() => {
                self.move_cursor(ctx, -1);
                return GroupOutcome::Pending;
            }
            KeyCode::Char('j') if key.modifiers.is_empty() => {
                self.move_cursor(ctx, 1);
                return GroupOutcome::Pending;
            }
            _ => {}
        }
        GroupOutcome::Pending
    }

    fn key_name(&mut self, key: &KeyEvent, ctx: &GroupCtx, rename: bool) -> GroupOutcome {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::List;
                self.name_buf.clear();
                self.error = None;
                return GroupOutcome::Pending;
            }
            KeyCode::Enter => {
                let name = self.name_buf.trim().to_string();
                if name.is_empty() {
                    self.error = Some("name must not be empty".to_string());
                    return GroupOutcome::Pending;
                }
                let taken = ctx.groups.iter().any(|g| g.name == name);
                if rename {
                    let from = self.selected.clone();
                    if taken && name != from {
                        self.error = Some(format!("group {name:?} already exists"));
                        return GroupOutcome::Pending;
                    }
                    self.mode = Mode::List;
                    self.name_buf.clear();
                    self.error = None;
                    if name == from {
                        return GroupOutcome::Pending;
                    }
                    self.selected = name.clone();
                    return GroupOutcome::Rename { from, to: name };
                }
                if taken {
                    self.error = Some(format!("group {name:?} already exists"));
                    return GroupOutcome::Pending;
                }
                self.mode = Mode::List;
                self.name_buf.clear();
                self.error = None;
                self.selected = name.clone();
                return GroupOutcome::Create(name);
            }
            KeyCode::Backspace => {
                self.name_buf.pop();
                self.error = None;
                return GroupOutcome::Pending;
            }
            KeyCode::Char(ch)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.name_buf.push(ch);
                self.error = None;
                return GroupOutcome::Pending;
            }
            _ => {}
        }
        GroupOutcome::Pending
    }

    fn key_members(&mut self, key: &KeyEvent, ctx: &GroupCtx) -> GroupOutcome {
        let len = ctx.sessions.len();
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::List;
                self.checked.clear();
                self.error = None;
                return GroupOutcome::Pending;
            }
            KeyCode::Enter => {
                let group = self.selected.clone();
                let members: Vec<SessionId> = ctx
                    .sessions
                    .iter()
                    .filter(|s| self.checked.contains(&s.id))
                    .map(|s| s.id)
                    .collect();
                self.mode = Mode::List;
                self.checked.clear();
                self.error = None;
                return GroupOutcome::SetMembers { group, members };
            }
            KeyCode::Up => {
                if len > 0 {
                    self.cursor = (self.cursor + len - 1) % len;
                }
                return GroupOutcome::Pending;
            }
            KeyCode::Down => {
                if len > 0 {
                    self.cursor = (self.cursor + 1) % len;
                }
                return GroupOutcome::Pending;
            }
            KeyCode::Char('k') if key.modifiers.is_empty() => {
                if len > 0 {
                    self.cursor = (self.cursor + len - 1) % len;
                }
                return GroupOutcome::Pending;
            }
            KeyCode::Char('j') if key.modifiers.is_empty() => {
                if len > 0 {
                    self.cursor = (self.cursor + 1) % len;
                }
                return GroupOutcome::Pending;
            }
            KeyCode::Char(' ') if key.modifiers.is_empty() => {
                if let Some(row) = ctx.sessions.get(self.cursor) {
                    self.toggle_member(row);
                }
                return GroupOutcome::Pending;
            }
            _ => {}
        }
        GroupOutcome::Pending
    }

    fn toggle_member(&mut self, row: &SessionRow) {
        let selected = if self.checked.contains(&row.id) {
            vec![0]
        } else {
            vec![]
        };
        let mut checkbox = Checkbox::default()
            .choices([safe_name(&row.name)])
            .values(&selected);
        checkbox.perform(Cmd::Toggle);
        if checkbox.states.has(0) {
            self.checked.insert(row.id);
        } else {
            self.checked.remove(&row.id);
        }
    }

    /// Dispatch a left click within the modal. Rows are the same rows
    /// rendered by the List and Checkbox components below.
    pub fn click(&mut self, col: u16, row: u16, area: Rect, ctx: &GroupCtx) -> bool {
        self.reconcile(ctx);
        if col <= area.x || col >= area.right().saturating_sub(1)
            || row <= area.y || row >= area.bottom().saturating_sub(1)
        {
            return false;
        }
        let index = (row - area.y - 1) as usize;
        let visible_rows = area.height.saturating_sub(3) as usize;
        match self.mode {
            Mode::List => {
                let rows = overview_rows(ctx);
                let selected_row = overview_selected_row(ctx, self.cursor);
                let start = visible_start(selected_row, visible_rows);
                if index >= visible_rows {
                    return false;
                }
                if let Some((group_index, _)) = rows.get(start + index) {
                    self.cursor = *group_index;
                    let group = &ctx.groups[*group_index];
                    self.selected = group.name.clone();
                    self.error = None;
                    return true;
                }
            }
            Mode::Members => {
                if let Some(index) = index.checked_sub(1).filter(|i| *i < visible_rows) {
                    let selected = visible_start(self.cursor, visible_rows) + index;
                    if let Some(session) = ctx.sessions.get(selected) {
                        self.cursor = selected;
                        self.toggle_member(session);
                        return true;
                    }
                }
            }
            Mode::Name { .. } => {}
        }
        false
    }

    /// Render the centered dialog from the same snapshot `key` used.
    pub fn view(&self, frame: &mut ratatui::Frame, area: Rect, ctx: &GroupCtx) {
        use ratatui::widgets::{Block, Borders, Paragraph};
        let hint = match self.mode {
            Mode::List => " Communication Groups ",
            Mode::Name { rename: false } => " new group (Enter create • Esc back) ",
            Mode::Name { rename: true } => " rename group (Enter apply • Esc back) ",
            Mode::Members => " members (Space toggle • Enter apply • Esc back) ",
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(hint)
            .style(crate::theme::style(crate::theme::Role::BorderFocused));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height < 5 || inner.width < 20 {
            return;
        }
        let mut row = inner.y;
        let end = inner.y + inner.height;
        match self.mode {
            Mode::List => {
                if ctx.groups.is_empty() {
                    frame.render_widget(
                        Paragraph::new("No groups yet. n creates one."),
                        Rect::new(inner.x, row, inner.width, 1),
                    );
                } else {
                    let visible_rows = inner.height.saturating_sub(1) as usize;
                    let all_rows = overview_rows(ctx);
                    let selected_row = overview_selected_row(ctx, self.cursor);
                    let start = visible_start(selected_row, visible_rows);
                    let rows = all_rows.iter().skip(start).take(visible_rows)
                        .map(|(_, text)| text.clone());
                    let mut list = List::default()
                        .rows(rows)
                        .always_active()
                        .scroll(true)
                        .selected_line(selected_row.saturating_sub(start))
                        .highlight_str("▸");
                    list.view(
                        frame,
                        Rect::new(inner.x, row, inner.width, inner.height.saturating_sub(1)),
                    );
                    for (offset, (group_index, text)) in all_rows.iter().skip(start)
                        .take(visible_rows).enumerate()
                    {
                        if text.starts_with("    ● ") && inner.width > 6 {
                            let color = crate::ui::group_palette(ctx.groups[*group_index].color);
                            let mut dot = tui_realm_stdlib::components::Label::default()
                                .text("●")
                                .style(ratatui::style::Style::default().fg(color));
                            dot.view(frame, Rect::new(inner.x + 5, row + offset as u16, 1, 1));
                        }
                    }
                }
                row = row.saturating_add(overview_rows(ctx).len().min(inner.height as usize) as u16);
            }
            Mode::Name { rename } => {
                let verb = if rename { "Rename to" } else { "New group" };
                frame.render_widget(
                    Paragraph::new(format!("{verb}: {}▌", safe_name(&self.name_buf))),
                    Rect::new(inner.x, row, inner.width, 1),
                );
                row += 1;
            }
            Mode::Members => {
                frame.render_widget(
                    Paragraph::new(format!("{}:", safe_name(&self.selected))),
                    Rect::new(inner.x, row, inner.width, 1),
                );
                row += 1;
                if ctx.sessions.is_empty() {
                    frame.render_widget(
                        Paragraph::new("  (no sessions)"),
                        Rect::new(inner.x, row, inner.width, 1),
                    );
                    row += 1;
                }
                let visible_rows = inner.height.saturating_sub(1) as usize;
                let start = visible_start(self.cursor, visible_rows);
                for (i, s) in ctx.sessions.iter().enumerate().skip(start).take(visible_rows) {
                    if row >= end {
                        break;
                    }
                    let selected = if self.checked.contains(&s.id) {
                        vec![0]
                    } else {
                        vec![]
                    };
                    let mut checkbox = Checkbox::default()
                        .choices([safe_name(&s.name)])
                        .values(&selected)
                        .style(if i == self.cursor {
                            crate::theme::style(crate::theme::Role::Focus)
                        } else {
                            crate::theme::style(crate::theme::Role::Text)
                        });
                    checkbox.view(frame, Rect::new(inner.x, row, inner.width, 1));
                    row += 1;
                }
            }
        }
        if self.mode == Mode::List && inner.height > 0 {
            use ratatui::text::{Line, Span};
            let key = crate::theme::style(crate::theme::Role::KeyHint);
            let footer = Line::from(vec![
                Span::styled("n", key), Span::raw(" new group   "),
                Span::styled("a", key), Span::raw(" add session   "),
                Span::styled("r", key), Span::raw(" rename   "),
                Span::styled("d", key), Span::raw(" delete group"),
            ]);
            frame.render_widget(Paragraph::new(footer), Rect::new(inner.x, end - 1, inner.width, 1));
        }
        if let Some(err) = &self.error {
            if row < end.saturating_sub((self.mode == Mode::List) as u16) {
                frame.render_widget(
                    Paragraph::new(err.clone())
                        .style(crate::theme::style(crate::theme::Role::Danger)),
                    Rect::new(inner.x, row, inner.width, 1),
                );
            }
        }
    }
}

fn safe_name(raw: &str) -> String {
    crate::safe_text::encode_for_display(raw)
}

fn visible_start(cursor: usize, rows: usize) -> usize {
    cursor.saturating_sub(rows.saturating_sub(1))
}

fn overview_rows(ctx: &GroupCtx) -> Vec<(usize, String)> {
    let mut rows = Vec::new();
    for (index, group) in ctx.groups.iter().enumerate() {
        rows.push((index, format!("Group {}: {}", index + 1, safe_name(&group.name))));
        for name in &group.member_names {
            rows.push((index, format!("    ● {}", safe_name(name))));
        }
    }
    rows
}

fn overview_selected_row(ctx: &GroupCtx, group_index: usize) -> usize {
    ctx.groups.iter().take(group_index).map(|g| 1 + g.member_names.len()).sum()
}

impl Default for GroupDialog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ch(c: char) -> KeyEvent {
        key(KeyCode::Char(c))
    }

    fn fixture() -> (SessionId, SessionId, GroupCtx) {
        let a = SessionId::fresh();
        let b = SessionId::fresh();
        let ctx = GroupCtx {
            groups: vec![
                GroupRow { name: "codex-proj".to_string(), members: 1, member_names: vec!["a1".into()], color: 0 },
                GroupRow { name: "other".to_string(), members: 0, member_names: vec![], color: 1 },
            ],
            sessions: vec![
                SessionRow { id: a, name: "a1".to_string() },
                SessionRow { id: b, name: "a2".to_string() },
            ],
            members: vec![a],
        };
        (a, b, ctx)
    }

    fn typed(d: &mut GroupDialog, text: &str, ctx: &GroupCtx) {
        for c in text.chars() {
            assert!(matches!(d.key(&ch(c), ctx), GroupOutcome::Pending), "typing {c:?}");
        }
    }

    fn empty_ctx() -> GroupCtx {
        GroupCtx { groups: vec![], sessions: vec![], members: vec![] }
    }

    #[test]
    fn esc_closes_from_list() {
        let (_, _, ctx) = fixture();
        let mut d = GroupDialog::new();
        assert!(matches!(d.key(&key(KeyCode::Esc), &ctx), GroupOutcome::Closed));
    }

    #[test]
    fn arrows_and_jk_move_and_wrap() {
        let (_, _, ctx) = fixture();
        let mut d = GroupDialog::new();
        // Reconcile selects the first group.
        let _ = d.key(&key(KeyCode::Down), &ctx);
        let _ = d.key(&key(KeyCode::Up), &ctx);
        assert!(matches!(d.key(&ch('j'), &ctx), GroupOutcome::Pending));
        // Wrap past the end back to the first: delete it twice would fail
        // the second time if selection did not move, so delete once, move,
        // and check the neighbor goes next.
        assert!(matches!(d.key(&ch('k'), &ctx), GroupOutcome::Pending));
        match d.key(&ch('d'), &ctx) {
            GroupOutcome::Delete(name) => assert_eq!(name, "codex-proj"),
            other => panic!("expected delete, got {other:?}"),
        }
    }

    #[test]
    fn n_new_validates_then_creates_and_stays_open() {
        let (_, _, ctx) = fixture();
        let mut d = GroupDialog::new();
        assert!(matches!(d.key(&ch('n'), &ctx), GroupOutcome::Pending));
        // Empty name stays open with an error.
        assert!(matches!(d.key(&key(KeyCode::Enter), &ctx), GroupOutcome::Pending));
        assert!(d.error().is_some_and(|e| e.contains("empty")));
        // Duplicate name stays open with an error.
        typed(&mut d, "other", &ctx);
        assert!(matches!(d.key(&key(KeyCode::Enter), &ctx), GroupOutcome::Pending));
        assert!(d.error().is_some_and(|e| e.contains("already exists")));
        // Esc backs out to the list without closing.
        assert!(matches!(d.key(&key(KeyCode::Esc), &ctx), GroupOutcome::Pending));
        assert!(matches!(d.key(&ch('n'), &ctx), GroupOutcome::Pending));
        typed(&mut d, "fresh", &ctx);
        match d.key(&key(KeyCode::Enter), &ctx) {
            GroupOutcome::Create(name) => assert_eq!(name, "fresh"),
            other => panic!("expected create, got {other:?}"),
        }
        // Still open for more management.
        assert!(matches!(d.key(&key(KeyCode::Esc), &ctx), GroupOutcome::Closed));
    }

    #[test]
    fn r_rename_applies_and_same_name_is_noop() {
        let (_, _, ctx) = fixture();
        let mut d = GroupDialog::new();
        assert!(matches!(d.key(&ch('r'), &ctx), GroupOutcome::Pending));
        typed(&mut d, "-v2", &ctx);
        match d.key(&key(KeyCode::Enter), &ctx) {
            GroupOutcome::Rename { from, to } => {
                assert_eq!(from, "codex-proj");
                assert_eq!(to, "codex-proj-v2");
            }
            other => panic!("expected rename, got {other:?}"),
        }
        // Renaming onto an existing group is rejected.
        let mut d2 = GroupDialog::new();
        assert!(matches!(d2.key(&ch('r'), &ctx), GroupOutcome::Pending));
        for _ in 0.."codex-proj".len() {
            let _ = d2.key(&key(KeyCode::Backspace), &ctx);
        }
        typed(&mut d2, "other", &ctx);
        assert!(matches!(d2.key(&key(KeyCode::Enter), &ctx), GroupOutcome::Pending));
        assert!(d2.error().is_some_and(|e| e.contains("already exists")));
    }

    #[test]
    fn d_delete_falls_to_neighbor() {
        let (_, _, ctx) = fixture();
        let mut d = GroupDialog::new();
        match d.key(&ch('d'), &ctx) {
            GroupOutcome::Delete(name) => assert_eq!(name, "codex-proj"),
            other => panic!("expected delete, got {other:?}"),
        }
        // Selection fell through to the neighbor without a fresh snapshot.
        match d.key(&ch('d'), &ctx) {
            GroupOutcome::Delete(name) => assert_eq!(name, "other"),
            other => panic!("expected neighbor delete, got {other:?}"),
        }
    }

    #[test]
    fn a_checkbox_prechecks_toggle_and_applies_exact_membership() {
        let (a, b, ctx) = fixture();
        let mut d = GroupDialog::new();
        assert!(matches!(d.key(&ch('a'), &ctx), GroupOutcome::Pending));
        // `a` was pre-checked (sole member): uncheck it, check `b` instead.
        assert!(matches!(d.key(&key(KeyCode::Char(' ')), &ctx), GroupOutcome::Pending));
        assert!(matches!(d.key(&key(KeyCode::Down), &ctx), GroupOutcome::Pending));
        assert!(matches!(d.key(&key(KeyCode::Char(' ')), &ctx), GroupOutcome::Pending));
        match d.key(&key(KeyCode::Enter), &ctx) {
            GroupOutcome::SetMembers { group, members } => {
                assert_eq!(group, "codex-proj");
                assert_eq!(members, vec![b], "exact membership, a dropped");
            }
            other => panic!("expected members, got {other:?}"),
        }
        let _ = a;
    }

    #[test]
    fn a_esc_cancels_without_mutation() {
        let (_, _, ctx) = fixture();
        let mut d = GroupDialog::new();
        assert!(matches!(d.key(&ch('a'), &ctx), GroupOutcome::Pending));
        assert!(matches!(d.key(&key(KeyCode::Char(' ')), &ctx), GroupOutcome::Pending));
        assert!(matches!(d.key(&key(KeyCode::Esc), &ctx), GroupOutcome::Pending));
        // Back in the list: Esc now closes.
        assert!(matches!(d.key(&key(KeyCode::Esc), &ctx), GroupOutcome::Closed));
    }

    #[test]
    fn mouse_selects_group_and_toggles_member_checkbox() {
        let (_, b, mut ctx) = fixture();
        let mut d = GroupDialog::new();
        let area = Rect::new(8, 2, 64, 20);
        assert!(d.click(12, 5, area, &ctx));
        assert_eq!(d.selected_name(), "other");
        ctx.members.clear(); // fresh membership snapshot for the selected group
        assert!(matches!(d.key(&ch('a'), &ctx), GroupOutcome::Pending));
        assert!(d.click(12, 5, area, &ctx));
        match d.key(&key(KeyCode::Enter), &ctx) {
            GroupOutcome::SetMembers { group, members } => {
                assert_eq!(group, "other");
                assert_eq!(members, vec![b]);
            }
            other => panic!("expected members, got {other:?}"),
        }
    }

    #[test]
    fn member_picker_renders_tuirealm_checkboxes() {
        use ratatui::{backend::TestBackend, Terminal};
        let (_, _, ctx) = fixture();
        let mut d = GroupDialog::new();
        let _ = d.key(&ch('a'), &ctx);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| d.view(f, group_area(f.area()), &ctx)).unwrap();
        let cells = &terminal.backend().buffer().content;
        assert!(cells.iter().any(|c| c.symbol() == "☑"));
        assert!(cells.iter().any(|c| c.symbol() == "☐"));
    }

    #[test]
    fn member_picker_scrolls_and_clicks_visible_row() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut ctx = empty_ctx();
        ctx.groups.push(GroupRow { name: "team".into(), members: 0, member_names: vec![], color: 0 });
        for n in 0..25 {
            ctx.sessions.push(SessionRow { id: SessionId::fresh(), name: format!("member-{n}") });
        }
        let mut d = GroupDialog::new();
        let _ = d.key(&ch('a'), &ctx);
        for _ in 0..24 { let _ = d.key(&key(KeyCode::Down), &ctx); }
        let area = Rect::new(8, 2, 64, 20);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| d.view(f, area, &ctx)).unwrap();
        let content = terminal.backend().buffer().content.iter()
            .map(|cell| cell.symbol()).collect::<String>();
        assert!(content.contains("member-24"), "selected member must be visible");
        assert!(d.click(area.x + 2, area.y + 2, area, &ctx));
        match d.key(&key(KeyCode::Enter), &ctx) {
            GroupOutcome::SetMembers { members, .. } => assert_eq!(members, vec![ctx.sessions[8].id]),
            other => panic!("expected members, got {other:?}"),
        }
    }

    #[test]
    fn group_overview_shows_numbered_groups_nested_members_and_footer() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut ctx = empty_ctx();
        ctx.groups = vec![
            GroupRow {
                name: "aura".into(),
                members: 2,
                member_names: vec!["jarvis_cdx".into(), "jarvis_dev".into()],
                color: 2,
            },
            GroupRow { name: "forge".into(), members: 0, member_names: vec![], color: 3 },
        ];
        let mut dialog = GroupDialog::new();
        let _ = dialog.key(&key(KeyCode::Down), &ctx);
        let area = Rect::new(8, 2, 64, 20);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| dialog.view(f, area, &ctx)).unwrap();
        let content = terminal.backend().buffer().content.iter()
            .map(|cell| cell.symbol()).collect::<String>();
        assert!(content.contains("Communication Groups"));
        assert!(content.contains("Group 1: aura"));
        assert!(content.contains("jarvis_cdx"));
        assert!(content.contains("jarvis_dev"));
        assert!(content.contains("Group 2: forge"));
        let selected_row = terminal.backend().buffer().content.chunks(80).nth(6).unwrap()
            .iter().map(|cell| cell.symbol()).collect::<String>();
        assert!(selected_row.contains("▸"), "{selected_row:?}");
        assert!(content.contains("n new group"));
        assert!(content.contains("a add session"));
        let dot = terminal.backend().buffer().content.chunks(80).nth(4).unwrap()
            .iter().find(|cell| cell.symbol() == "●").unwrap();
        assert_eq!(dot.fg, crate::ui::group_palette(2));
        assert!(dialog.click(area.x + 4, area.y + 4, area, &ctx));
        assert_eq!(dialog.selected_name(), "forge");
    }

    #[test]
    fn mutations_without_groups_point_at_n() {
        let ctx = empty_ctx();
        let mut d = GroupDialog::new();
        for c in ['r', 'd', 'a'] {
            assert!(matches!(d.key(&ch(c), &ctx), GroupOutcome::Pending));
            assert!(
                d.error().is_some_and(|e| e.contains("n creates one")),
                "key {c}: {:?}",
                d.error()
            );
        }
    }
}

/// Centered dialog box, taller than the create form for member lists,
/// clamped into tiny terminals.
pub fn group_area(term: Rect) -> Rect {
    let (w, h) = (64.min(term.width), 20.min(term.height));
    Rect::new(
        term.x + term.width.saturating_sub(w) / 2,
        term.y + term.height.saturating_sub(h) / 2,
        w,
        h,
    )
}
