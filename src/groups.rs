//! Group management dialog: keyboard-first local group CRUD.
//!
//! Groups are human-only organization: no MCP tool exposes them, agents
//! only feel them through ask/tell gating. `n` news a group (typed name),
//! `a` edits members (checkbox list), `r` renames (typed name), `d`
//! deletes, arrows/`j`/`k` move, Enter applies, Esc steps out or closes.

use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::session::SessionId;

/// One group row for the dialog: name plus live member count.
#[derive(Clone, Debug)]
pub struct GroupRow {
    pub name: String,
    pub members: usize,
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
                    if !self.checked.remove(&row.id) {
                        self.checked.insert(row.id);
                    }
                }
                return GroupOutcome::Pending;
            }
            _ => {}
        }
        GroupOutcome::Pending
    }

    /// Render the centered dialog from the same snapshot `key` used.
    pub fn view(&self, frame: &mut ratatui::Frame, area: Rect, ctx: &GroupCtx) {
        use ratatui::widgets::{Block, Borders, Paragraph};
        let hint = match self.mode {
            Mode::List => " groups (n new • a members • r rename • d delete • Esc close) ",
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
        let mut lines: Vec<String> = Vec::new();
        match self.mode {
            Mode::List => {
                if ctx.groups.is_empty() {
                    lines.push("No groups yet. n creates one.".to_string());
                }
                for g in ctx.groups.iter() {
                    let mark = if g.name == self.selected { "▸" } else { " " };
                    let noun = if g.members == 1 { "member" } else { "members" };
                    lines.push(format!("{mark} {} ({} {noun})", g.name, g.members));
                }
            }
            Mode::Name { rename } => {
                let verb = if rename { "Rename to" } else { "New group" };
                lines.push(format!("{verb}: {}▌", self.name_buf));
            }
            Mode::Members => {
                lines.push(format!("{}:", self.selected));
                if ctx.sessions.is_empty() {
                    lines.push("  (no sessions)".to_string());
                }
                for (i, s) in ctx.sessions.iter().enumerate() {
                    let cursor = if i == self.cursor { "▸" } else { " " };
                    let check = if self.checked.contains(&s.id) { "[x]" } else { "[ ]" };
                    lines.push(format!("{cursor} {check} {}", s.name));
                }
            }
        }
        for line in lines {
            if row >= end {
                break;
            }
            frame.render_widget(
                Paragraph::new(line),
                Rect::new(inner.x, row, inner.width, 1),
            );
            row += 1;
        }
        if let Some(err) = &self.error {
            if row < end {
                frame.render_widget(
                    Paragraph::new(err.clone())
                        .style(crate::theme::style(crate::theme::Role::Danger)),
                    Rect::new(inner.x, row, inner.width, 1),
                );
            }
        }
    }
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
                GroupRow { name: "codex-proj".to_string(), members: 1 },
                GroupRow { name: "other".to_string(), members: 0 },
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
