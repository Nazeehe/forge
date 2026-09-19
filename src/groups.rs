//! Group management dialog: keyboard-first local group CRUD.
//!
//! Groups are human-only organization: no MCP tool exposes them, agents
//! only feel them through ask/tell gating. `n` news a group (typed name),
//! `a` edits members (checkbox list), `r` renames (typed name), `d`
//! deletes, arrows/`j`/`k` move, Enter applies, Esc steps out or closes.

use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use tui_realm_stdlib::components::Checkbox;
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
/// mutations, unlike an index), a flat row cursor over the overview
/// (group headers AND member rows, so `r` can remove the member under
/// the cursor), the selected member name when the cursor sits on one, a
/// name buffer, and the checkbox set for member editing.
pub struct GroupDialog {
    mode: Mode,
    selected: String,
    member: Option<String>,
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
            member: None,
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

    /// Member name under the cursor, if the cursor sits on a member row.
    pub fn selected_member(&self) -> Option<&str> {
        self.member.as_deref()
    }

    /// Group under the cursor, if the selection names a live group.
    fn current<'a>(&self, ctx: &'a GroupCtx) -> Option<&'a str> {
        ctx.groups
            .iter()
            .find(|g| g.name == self.selected)
            .map(|g| g.name.as_str())
    }

    /// Reconcile selection with a fresh snapshot: a deleted selection
    /// falls to the cursor row, a removed member falls back to its group
    /// header, and the cursor clamps into range. In member-edit mode the
    /// cursor walks the session list instead, so it only clamps there
    /// and never snaps back to the group row.
    fn reconcile(&mut self, ctx: &GroupCtx) {
        if self.mode == Mode::Members {
            if !ctx.sessions.is_empty() {
                self.cursor = self.cursor.min(ctx.sessions.len() - 1);
            } else {
                self.cursor = 0;
            }
            return;
        }
        let rows = overview_rows(ctx);
        if ctx.groups.iter().all(|g| g.name != self.selected) {
            self.selected = ctx
                .groups
                .get(self.cursor.min(ctx.groups.len().saturating_sub(1)))
                .map(|g| g.name.clone())
                .unwrap_or_default();
            self.member = None;
        }
        match ctx.groups.iter().position(|g| g.name == self.selected) {
            None => {
                self.cursor = 0;
                self.member = None;
            }
            Some(pos) => {
                let header = rows
                    .iter()
                    .position(|r| r.group == pos && r.member.is_none());
                let landed = match self.member.clone() {
                    Some(name) => rows
                        .iter()
                        .position(|r| r.group == pos && r.member.as_deref() == Some(&name))
                        .or(header),
                    None => header,
                };
                match landed {
                    Some(index) => {
                        self.cursor = index;
                        self.member = rows[index].member.clone();
                    }
                    None => {
                        self.cursor = 0;
                        self.member = None;
                    }
                }
            }
        }
    }

    /// Point the cursor at the selected group's header row. Used right
    /// after a removal so the next frame (drawn from a stale snapshot
    /// until the caller applies and rebuilds) never highlights a gone
    /// row. Header indices are stable under member removal because
    /// member rows always follow their header.
    fn snap_to_header(&mut self, ctx: &GroupCtx) {
        self.member = None;
        self.cursor = ctx
            .groups
            .iter()
            .position(|g| g.name == self.selected)
            .and_then(|pos| {
                overview_rows(ctx)
                    .iter()
                    .position(|r| r.group == pos && r.member.is_none())
            })
            .unwrap_or(0);
    }

    fn move_cursor(&mut self, ctx: &GroupCtx, dir: i32) {
        let rows = overview_rows(ctx);
        if rows.is_empty() {
            return;
        }
        let len = rows.len() as i32;
        self.cursor = (self.cursor as i32 + dir).rem_euclid(len) as usize;
        let row = &rows[self.cursor];
        self.selected = ctx.groups[row.group].name.clone();
        self.member = row.member.clone();
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
                // On a member row `r` removes that session from the group
                // via an exact-membership set; on a header it renames.
                if let Some(member) = self.member.clone() {
                    let Some(id) = ctx.sessions.iter().find(|s| s.name == member).map(|s| s.id)
                    else {
                        self.error = Some(format!("{member:?} already left"));
                        return GroupOutcome::Pending;
                    };
                    let members: Vec<SessionId> = ctx
                        .members
                        .iter()
                        .copied()
                        .filter(|kept| kept != &id)
                        .collect();
                    self.snap_to_header(ctx);
                    self.error = None;
                    return GroupOutcome::SetMembers {
                        group: current,
                        members,
                    };
                }
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
                self.snap_to_header(ctx);
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
        // Same padded origin the view paints: clicks on the pad row
        // or border die here, so mouse and keyboard never disagree.
        // The window also reserves the pad, error-slot, and footer
        // rows the view holds back, matching its counts exactly.
        let pad_top = pad_top_for(area.height.saturating_sub(2));
        let raw = row - area.y - 1;
        if raw < pad_top {
            return false;
        }
        let index = (raw - pad_top) as usize;
        let visible_rows = area.height.saturating_sub(3 + 2 * pad_top) as usize;
        match self.mode {
            Mode::List => {
                let rows = overview_rows(ctx);
                let start = visible_start(self.cursor, visible_rows);
                if index >= visible_rows {
                    return false;
                }
                if let Some(hit) = rows.get(start + index) {
                    self.cursor = start + index;
                    self.selected = ctx.groups[hit.group].name.clone();
                    self.member = hit.member.clone();
                    self.error = None;
                    return true;
                }
            }
            Mode::Members => {
                // One header row plus the footer row are not sessions.
                let body_rows = visible_rows.saturating_sub(1);
                if let Some(index) = index.checked_sub(1).filter(|i| *i < body_rows) {
                    let selected = visible_start(self.cursor, body_rows) + index;
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

    /// Render the centered dialog per the UI guidance: an opaque modal,
    /// `>` plus reverse video on the cursor row, selection marks in
    /// accent yellow, and key-hint footers. Content keeps border
    /// padding; pickers pin the footer with a fixed error slot above
    /// it so failures never shift the list. Rows are drawn by hand
    /// from the same padded top-down order `click` hit-tests, so
    /// mouse and keyboard never disagree.
    pub fn view(&self, frame: &mut ratatui::Frame, area: Rect, ctx: &GroupCtx) {
        use ratatui::text::{Line, Span};
        use ratatui::widgets::{Block, Borders, Clear, Paragraph};
        use crate::theme::{Role, focus_row, style};
        // Opaque: the live grid must not show through the modal.
        frame.render_widget(Clear, area);
        let hint = match self.mode {
            Mode::List | Mode::Members => " Communication Groups ",
            Mode::Name { rename: false } => " new group (Enter create • Esc back) ",
            Mode::Name { rename: true } => " rename group (Enter apply • Esc back) ",
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(hint)
            .style(crate::theme::modal_fill())
            .border_style(style(Role::BorderModal));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height < 5 || inner.width < 20 {
            return;
        }
        let text = style(Role::Text);
        // Padded content origin; the scroll windows below reserve the
        // same pad, error-slot, and footer rows `click` holds back.
        let pad_top = pad_top_for(inner.height);
        let pad_x = if inner.width >= 30 { 2 } else { 0 };
        let roomy = pad_top > 0;
        let cx = inner.x + pad_x;
        let cw = inner.width.saturating_sub(pad_x * 2);
        let mut row = inner.y + pad_top;
        let end = inner.y + inner.height;
        match self.mode {
            Mode::List => {
                if ctx.groups.is_empty() {
                    frame.render_widget(
                        Paragraph::new("No groups yet. n creates one.").style(text),
                        Rect::new(cx, row, cw, 1),
                    );
                } else {
                    let visible_rows =
                        inner.height.saturating_sub(1 + 2 * pad_top) as usize;
                    let all_rows = overview_rows(ctx);
                    // Clamp: a removal can shrink the rows under a stale
                    // cursor for one frame before the next reconcile.
                    let selected_row = self.cursor.min(all_rows.len().saturating_sub(1));
                    let start = visible_start(selected_row, visible_rows);
                    for (pos, over) in all_rows.iter().enumerate().skip(start).take(visible_rows) {
                        let y = row + (pos - start) as u16;
                        if y >= end.saturating_sub(1) {
                            break;
                        }
                        let focused = pos == selected_row;
                        let row_style = if focused { focus_row() } else { text };
                        let mut spans = vec![if focused {
                            Span::styled("> ", focus_row())
                        } else {
                            Span::raw("  ")
                        }];
                        match &over.member {
                            None => {
                                let label = fit_row(&over.text, cw as usize - 2);
                                spans.push(Span::styled(label, row_style));
                            }
                            Some(member) => {
                                let color = crate::ui::group_palette(ctx.groups[over.group].color);
                                let name = fit_row(member, cw as usize - 8);
                                spans.push(Span::styled("    ", row_style));
                                spans.push(Span::styled("●", style(Role::Text).fg(color)));
                                spans.push(Span::styled(format!(" {name}"), row_style));
                            }
                        }
                        frame.render_widget(
                            Paragraph::new(Line::from(spans)),
                            Rect::new(cx, y, cw, 1),
                        );
                    }
                }
                row = row.saturating_add(overview_rows(ctx).len().min(inner.height as usize) as u16);
            }
            Mode::Name { rename } => {
                let verb = if rename { "Rename to" } else { "Group name" };
                let room = (cw as usize)
                    .saturating_sub(verb.chars().count() + 7);
                let buf = fit_row(&safe_name(&self.name_buf), room);
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled("> ", focus_row()),
                        Span::styled(format!("{verb}: "), text),
                        Span::styled(format!("[{buf}▌]"), focus_row()),
                    ])),
                    Rect::new(cx, row, cw, 1),
                );
                row += 1;
            }
            Mode::Members => {
                // Reference look (screens/group_select.jpeg): dotted
                // "Add sessions" header, `> [√] name` rows with the
                // checked box in accent yellow, and a key-hint footer.
                frame.render_widget(
                    Paragraph::new(dotted_header(cw as usize, "Add sessions"))
                        .style(style(Role::Muted)),
                    Rect::new(cx, row, cw, 1),
                );
                row += 1;
                if ctx.sessions.is_empty() {
                    frame.render_widget(
                        Paragraph::new("  (no sessions)").style(text),
                        Rect::new(cx, row, cw, 1),
                    );
                    row += 1;
                }
                let visible_rows =
                    inner.height.saturating_sub(2 + 2 * pad_top) as usize;
                let start = visible_start(self.cursor, visible_rows);
                for (i, s) in ctx.sessions.iter().enumerate().skip(start).take(visible_rows) {
                    if row >= end.saturating_sub(1) {
                        break;
                    }
                    let checked = self.checked.contains(&s.id);
                    let focused = i == self.cursor;
                    let name = fit_row(&safe_name(&s.name), cw as usize - 8);
                    let (box_glyph, box_style, name_style) = if focused {
                        ("[√] ", focus_row(), focus_row())
                    } else if checked {
                        ("[√] ", style(Role::Brand), text.add_modifier(ratatui::style::Modifier::BOLD))
                    } else {
                        ("[ ] ", style(Role::Muted), text)
                    };
                    // Unchecked boxes still show `[ ]` when focused so the
                    // row keeps its checkbox shape in reverse video.
                    let (box_glyph, box_style) = if focused && !checked {
                        ("[ ] ", focus_row())
                    } else {
                        (box_glyph, box_style)
                    };
                    let line = Line::from(vec![
                        if focused {
                            Span::styled("> ", focus_row())
                        } else {
                            Span::raw("  ")
                        },
                        Span::styled(box_glyph, box_style),
                        Span::styled(name, name_style),
                    ]);
                    frame.render_widget(
                        Paragraph::new(line),
                        Rect::new(cx, row, cw, 1),
                    );
                    row += 1;
                }
            }
        }
        // Both pickers keep a key-hint footer; the name prompt spends
        // its last row on the input instead.
        let footer_hints: Option<Vec<ratatui::text::Span>> = match self.mode {
            Mode::List => {
                use ratatui::text::Span;
                let key = crate::theme::style(crate::theme::Role::KeyHint);
                // `r` follows the cursor: a member row removes that
                // session from the group, a header renames the group.
                let r_hint = if self.member.is_some() {
                    " remove member   "
                } else {
                    " rename   "
                };
                Some(vec![
                    Span::styled("n", key), Span::raw(" new group   "),
                    Span::styled("a", key), Span::raw(" add session   "),
                    Span::styled("r", key), Span::raw(r_hint),
                    Span::styled("d", key), Span::raw(" delete group"),
                ])
            }
            Mode::Members => {
                use ratatui::text::Span;
                let key = crate::theme::style(crate::theme::Role::KeyHint);
                Some(vec![
                    Span::styled("↑↓", key), Span::raw(" navigate   "),
                    Span::styled("Space", key), Span::raw(" toggle   "),
                    Span::styled("Enter", key), Span::raw(" confirm   "),
                    Span::styled("Esc", key), Span::raw(" back"),
                ])
            }
            Mode::Name { .. } => None,
        };
        let has_footer = footer_hints.is_some();
        if let Some(hints) = footer_hints {
            if inner.height > 0 {
                use ratatui::text::Line;
                frame.render_widget(
                    Paragraph::new(Line::from(hints)),
                    Rect::new(cx, end - 1, cw, 1),
                );
            }
        }
        // Fixed error slot above the footer when roomy, so a failure
        // never shifts the list; the footer-less name prompt keeps the
        // error right after its input, where nothing sits below it.
        let error_at = if roomy && has_footer { end.saturating_sub(2) } else { row };
        if let Some(err) = &self.error {
            if error_at < end.saturating_sub(has_footer as u16) {
                frame.render_widget(
                    Paragraph::new(format!("! {err}")).style(style(Role::Danger)),
                    Rect::new(cx, error_at, cw, 1),
                );
            }
        }
    }
}

/// Padded-content top offset, shared by view and click: roomy
/// dialogs rest content one row down from the border; cramped ones
/// keep every row for the list.
fn pad_top_for(inner_height: u16) -> u16 {
    if inner_height >= 8 {
        1
    } else {
        0
    }
}

fn safe_name(raw: &str) -> String {
    crate::safe_text::encode_for_display(raw)
}

/// Truncate a row to a cell width, marking cuts with an ellipsis so
/// long names can never wrap the one-row-per-entry layout.
pub(crate) fn fit_row(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Centered `········ Add sessions ········` header for the member
/// picker, dotted out to the available width.
fn dotted_header(width: usize, label: &str) -> String {
    let label = format!(" {label} ");
    if width <= label.len() + 2 {
        return label;
    }
    let fill = width - label.len();
    let left = fill / 2;
    format!("{}{label}{}", "·".repeat(left), "·".repeat(fill - left))
}

fn visible_start(cursor: usize, rows: usize) -> usize {
    cursor.saturating_sub(rows.saturating_sub(1))
}

/// One flat overview row: the owning group index, the raw member name
/// for member rows (None for headers), and the rendered text.
struct OverRow {
    group: usize,
    member: Option<String>,
    text: String,
}

fn overview_rows(ctx: &GroupCtx) -> Vec<OverRow> {
    let mut rows = Vec::new();
    for (index, group) in ctx.groups.iter().enumerate() {
        rows.push(OverRow {
            group: index,
            member: None,
            text: format!("Group {}: {}", index + 1, safe_name(&group.name)),
        });
        for name in &group.member_names {
            rows.push(OverRow {
                group: index,
                member: Some(name.clone()),
                text: format!("    ● {}", safe_name(name)),
            });
        }
    }
    rows
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
    fn arrows_walk_member_rows_and_r_removes_member() {
        let (a, _b, ctx) = fixture();
        // Flat rows: [codex-proj, a1, other]. j lands on the member.
        let mut d = GroupDialog::new();
        assert!(matches!(d.key(&ch('j'), &ctx), GroupOutcome::Pending));
        assert_eq!(d.selected_name(), "codex-proj");
        assert_eq!(d.selected_member(), Some("a1"));
        // r on a member drops it via exact-membership set, not rename.
        match d.key(&ch('r'), &ctx) {
            GroupOutcome::SetMembers { group, members } => {
                assert_eq!(group, "codex-proj");
                assert!(!members.contains(&a), "a1 removed");
            }
            other => panic!("expected member removal, got {other:?}"),
        }
        // Cursor fell back to the group header, where r renames again.
        assert_eq!(d.selected_member(), None);
        assert!(matches!(d.key(&ch('r'), &ctx), GroupOutcome::Pending));
        typed(&mut d, "-v2", &ctx);
        match d.key(&key(KeyCode::Enter), &ctx) {
            GroupOutcome::Rename { from, to } => {
                assert_eq!((from.as_str(), to.as_str()), ("codex-proj", "codex-proj-v2"));
            }
            other => panic!("expected rename, got {other:?}"),
        }
    }

    #[test]
    fn d_on_member_row_deletes_the_whole_group() {
        // Reference footer: `d` is delete-group unconditionally; `r` is
        // the member remover.
        let (_, _, ctx) = fixture();
        let mut d = GroupDialog::new();
        let _ = d.key(&ch('j'), &ctx);
        assert_eq!(d.selected_member(), Some("a1"));
        match d.key(&ch('d'), &ctx) {
            GroupOutcome::Delete(name) => assert_eq!(name, "codex-proj"),
            other => panic!("expected group delete, got {other:?}"),
        }
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
        // Padded content starts one row lower in both modes.
        assert!(d.click(12, 6, area, &ctx));
        assert_eq!(d.selected_name(), "other");
        ctx.members.clear(); // fresh membership snapshot for the selected group
        assert!(matches!(d.key(&ch('a'), &ctx), GroupOutcome::Pending));
        assert!(d.click(12, 6, area, &ctx));
        match d.key(&key(KeyCode::Enter), &ctx) {
            GroupOutcome::SetMembers { group, members } => {
                assert_eq!(group, "other");
                assert_eq!(members, vec![b]);
            }
            other => panic!("expected members, got {other:?}"),
        }
    }

    #[test]
    fn member_picker_matches_add_sessions_reference() {
        // screens/group_select.jpeg: dotted "Add sessions" header,
        // `> [√] name` rows, and an ↑↓/Space/Enter/Esc footer. The
        // tui-realm Checkbox glyphs (☑/☐) were replaced to match it,
        // and the UI guidance mandates `>` (not `▸`) for focus.
        use ratatui::{backend::TestBackend, Terminal};
        let (_, _, ctx) = fixture();
        let mut d = GroupDialog::new();
        let _ = d.key(&ch('a'), &ctx);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| d.view(f, group_area(f.area()), &ctx)).unwrap();
        let text = terminal.backend().buffer().content.iter()
            .map(|cell| cell.symbol()).collect::<String>();
        assert!(text.contains("Add sessions"), "header: {text:?}");
        assert!(text.contains("[√]"), "checked box: {text:?}");
        assert!(text.contains("[ ]"), "unchecked box: {text:?}");
        assert!(!text.contains("▸"), "old marker is gone: {text:?}");
        assert!(text.contains("> [√]"), "focused row: {text:?}");
        for hint in ["navigate", "toggle", "confirm"] {
            assert!(text.contains(hint), "footer {hint:?}: {text:?}");
        }
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
        // Padded content: header sits one row down, so the first
        // session row is area.y + 3, not + 2.
        assert!(d.click(area.x + 2, area.y + 3, area, &ctx));
        match d.key(&key(KeyCode::Enter), &ctx) {
            // Pad, header, error slot, and footer leave 14 body rows,
            // so the scrolled window starts at session 11; the click
            // still hits the rendered first row.
            GroupOutcome::SetMembers { members, .. } => assert_eq!(members, vec![ctx.sessions[11].id]),
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
        // One Down from the first header lands on its first member row
        // (flat navigation), not the next group header.
        assert_eq!(dialog.selected_member(), Some("jarvis_cdx"));
        // Padded one row down: cursor member renders on buffer row 5.
        let selected_row = terminal.backend().buffer().content.chunks(80).nth(5).unwrap()
            .iter().map(|cell| cell.symbol()).collect::<String>();
        assert!(selected_row.contains(">"), "{selected_row:?}");
        assert!(content.contains("n new group"));
        assert!(content.contains("a add session"));
        let dot = terminal.backend().buffer().content.chunks(80).nth(5).unwrap()
            .iter().find(|cell| cell.symbol() == "●").unwrap();
        assert_eq!(dot.fg, crate::ui::group_palette(2));
        // The forge header moved down one row with the padding.
        assert!(dialog.click(area.x + 4, area.y + 5, area, &ctx));
        assert_eq!(dialog.selected_name(), "forge");
    }

    #[test]
    fn overview_marks_cursor_cyan_and_keeps_dots_colored() {
        use ratatui::{backend::TestBackend, Terminal};
        use ratatui::style::{Color, Modifier};
        let (_, _, ctx) = fixture();
        let mut d = GroupDialog::new();
        // One Down lands on the first member row (flat navigation).
        let _ = d.key(&key(KeyCode::Down), &ctx);
        let area = Rect::new(8, 2, 64, 20);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| d.view(f, area, &ctx)).unwrap();
        let buf = terminal.backend().buffer();
        // Padded content starts at (11, 4): header above, cursor member
        // one row down from the unpadded layout.
        let mark = &buf[(11, 5)];
        assert_eq!(mark.symbol(), ">");
        assert_eq!(mark.fg, Color::Cyan, "focus is cyan, not yellow");
        assert!(mark.modifier.contains(Modifier::REVERSED));
        // Member row: marker, 4-space indent, dot, name.
        let name = &buf[(19, 5)];
        assert_eq!(name.symbol(), "a");
        assert!(name.modifier.contains(Modifier::REVERSED), "cursor row reverses");
        // The group dot keeps its identity color under focus.
        assert_eq!(buf[(17, 5)].symbol(), "●");
        assert_eq!(buf[(17, 5)].fg, crate::ui::group_palette(0));
        // The header row above stays plain white text.
        assert_eq!(buf[(11, 4)].symbol(), " ");
        assert_eq!(buf[(13, 4)].fg, Color::White);
    }

    #[test]
    fn name_prompt_boxes_the_buffer_in_focus_style() {
        use ratatui::{backend::TestBackend, Terminal};
        use ratatui::style::{Color, Modifier};
        let (_, _, ctx) = fixture();
        let mut d = GroupDialog::new();
        let _ = d.key(&ch('n'), &ctx);
        typed(&mut d, "ab", &ctx);
        let area = Rect::new(8, 2, 64, 20);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| d.view(f, area, &ctx)).unwrap();
        let buf = terminal.backend().buffer();
        let text = terminal.backend().buffer().content.iter()
            .map(|cell| cell.symbol()).collect::<String>();
        assert!(text.contains("Group name: [ab▌]"), "boxed input: {text:?}");
        // Padded input row one down and two in.
        let mark = &buf[(11, 4)];
        assert_eq!(mark.symbol(), ">");
        assert_eq!(mark.fg, Color::Cyan);
        let open = &buf[(11 + 2 + 12, 4)];
        assert_eq!(open.symbol(), "[");
        assert!(open.modifier.contains(Modifier::REVERSED), "field reverses");
    }

    #[test]
    fn error_slot_sits_above_the_pinned_footer() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = empty_ctx();
        let mut d = GroupDialog::new();
        assert!(matches!(d.key(&ch('r'), &ctx), GroupOutcome::Pending));
        assert!(d.error().is_some());
        let area = Rect::new(8, 2, 64, 20);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| d.view(f, area, &ctx)).unwrap();
        let row = |y: usize| {
            terminal.backend().buffer().content.chunks(80).nth(y).unwrap()
                .iter().map(|cell| cell.symbol()).collect::<String>()
        };
        // Fixed slot on the second-to-last row; the footer never moves
        // off the last one, error or not.
        assert!(row(19).contains("n creates one"), "{:?}", row(19));
        assert!(row(20).contains("new group"), "{:?}", row(20));
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

