//! Adapter input layer: editor ownership, key/mouse feed, buffer
//! sync, and the prompt/chat/focus/thread/paste session ops the TUI
//! input layer calls into. Split by concern: `prompt` owns the
//! typed-path prompt and recent list, `toolbar` the pill dispatch,
//! `chat` the chat box, thread cursor and bracketed paste.
//!
//! Movement is adapter-owned on char offsets (see `nav`); the editor
//! mode invariant lives here too: plain moves, clicks, clean mouse
//! releases and Esc all land back in Insert, because EdTUI's hidden
//! vim Normal strands the keyboard with nothing ever returning.

mod chat;
mod prompt;
mod toolbar;

use super::{
    changed_range, editor_selection_to_range, index2_to_offset, offset_to_index2,
};
use super::nav::{nav_target, vertical_target, NavMove};
use crate::app::AppState;
use edtui::{
    actions::{DeleteSelection, SwitchMode},
    EditorMode, EditorState, Lines,
};

impl AppState {
    /// (Re)create the editor from the open document: the CUA
    /// register with Insert mode at open, Forge clipboard from the start.
    /// `pub(crate)`: test seam for the TUI input layer, which never
    /// opens editors itself (agents do, through `writer_open`).
    pub(crate) fn writer_open_editor(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(doc) = session.doc.as_ref() else {
            session.editor = None;
            return;
        };
        let mut editor = EditorState::new(Lines::from(doc.text.as_str()));
        editor.mode = EditorMode::Insert;
        editor.set_clipboard(session.clip.clone());
        session.editor = Some(editor);
        session.selection = None;
        session.sel_anchor = None;
        session.nav_goal = None;
        self.dirty = true;
    }

    /// Feed one terminal key to the editor and sync back. Movement is
    /// adapter-owned on top of char offsets, because EdTUI's char
    /// motions stop at line ends and can never cross `\n`, its word
    /// jumps stop at word ends (vim-style, not CUA starts), its page
    /// keys move only the viewport, and the CUA register binds text
    /// entry only. Everything else is forwarded as-is.
    pub fn writer_feed_key(&mut self, id: crate::session::SessionId, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        // Any key ends the multi-click chain: a later press starts
        // over at a single click.
        if let Some(session) = self.writers.get_mut(&id) {
            session.press_count = 0;
            session.last_press = None;
        }
        // Preview is read-only: arrows and pages scroll, Home/End
        // jump, Ctrl+S still saves, Esc leaves. Edits never land,
        // and the editor cursor rests underneath for the way back.
        if self.writers.get(&id).is_some_and(|s| s.preview) {
            let rows = self
                .writers
                .get(&id)
                .map(|s| s.editor_rows.max(1) as isize)
                .unwrap_or(1);
            match (key.code, key.modifiers) {
                (KeyCode::Up, _) => self.writer_preview_scroll(id, -1),
                (KeyCode::Down, _) => self.writer_preview_scroll(id, 1),
                (KeyCode::PageUp, _) => self.writer_preview_scroll(id, -rows),
                (KeyCode::PageDown, _) => self.writer_preview_scroll(id, rows),
                (KeyCode::Home, _) => self.writer_preview_edge(id, false),
                (KeyCode::End, _) => self.writer_preview_edge(id, true),
                (KeyCode::Char('s'), KeyModifiers::CONTROL) => self.writer_save(id),
                (KeyCode::Esc, KeyModifiers::NONE) => self.writer_toggle_preview(id),
                _ => {}
            }
            return;
        }
        // Esc always returns the editor to the known-good state: no
        // selection, Insert mode. EdTUI would park in vim Normal
        // (mouse Down while Visual strands there) with nothing ever
        // returning, so the adapter owns this.
        if key.code == KeyCode::Esc && key.modifiers == KeyModifiers::empty() {
            if let Some(session) = self.writers.get_mut(&id) {
                if let Some(editor) = session.editor.as_mut() {
                    editor.selection = None;
                    editor.mode = EditorMode::Insert;
                }
                session.sel_anchor = None;
                session.nav_goal = None;
            }
            self.writer_sync_editor(id);
            self.dirty = true;
            return;
        }
        // CUA text ops, above navigation: Tab indents, Ctrl+A selects,
        // Ctrl+Backspace/Delete kill words. They need adapter
        // selection/offset state, so they never reach the register.
        match (key.code, key.modifiers) {
            (KeyCode::Tab, KeyModifiers::NONE) => {
                self.writer_indent(id);
                return;
            }
            (KeyCode::Tab, KeyModifiers::SHIFT) => {
                self.writer_outdent(id);
                return;
            }
            (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                self.writer_select_all(id);
                return;
            }
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                self.writer_clip_copy(id);
                return;
            }
            (KeyCode::Char('x'), KeyModifiers::CONTROL) => {
                self.writer_clip_cut(id);
                return;
            }
            (KeyCode::Char('v'), KeyModifiers::CONTROL) => {
                self.writer_clip_paste(id);
                return;
            }
            (KeyCode::Backspace, KeyModifiers::CONTROL) => {
                self.writer_delete_word(id, true);
                return;
            }
            (KeyCode::Delete, KeyModifiers::CONTROL) => {
                self.writer_delete_word(id, false);
                return;
            }
            _ => {}
        }
        let shift = KeyModifiers::SHIFT;
        let shift_ctrl = KeyModifiers::SHIFT | KeyModifiers::CONTROL;
        // Adapter-owned moves: every move key plain (collapse) or
        // with Shift (select), plus word jumps and doc bounds on
        // Ctrl. Nothing navigation-like reaches EdTUI motions.
        let nav = if key.modifiers == KeyModifiers::empty() || key.modifiers == shift {
            match key.code {
                KeyCode::Left => Some(NavMove::CharLeft),
                KeyCode::Right => Some(NavMove::CharRight),
                KeyCode::Up => Some(NavMove::Up),
                KeyCode::Down => Some(NavMove::Down),
                KeyCode::Home => Some(NavMove::LineStart),
                KeyCode::End => Some(NavMove::LineEnd),
                KeyCode::PageUp => Some(NavMove::PageUp),
                KeyCode::PageDown => Some(NavMove::PageDown),
                _ => None,
            }
        } else if key.modifiers == KeyModifiers::CONTROL
            || key.modifiers == shift_ctrl
        {
            match key.code {
                KeyCode::Left => Some(NavMove::WordLeft),
                KeyCode::Right => Some(NavMove::WordRight),
                KeyCode::Home => Some(NavMove::DocStart),
                KeyCode::End => Some(NavMove::DocEnd),
                _ => None,
            }
        } else {
            None
        };
        if nav.is_none() {
            // Typing groups (E6) live or die here: the open group is
            // taken; only a continuing plain char restores it below.
            // Every other key starts over.
            let group;
            let had_selection;
            {
                let Some(session) = self.writers.get_mut(&id) else {
                    return;
                };
                let Some(editor) = session.editor.as_mut() else {
                    return;
                };
                group = session.type_group.take();
                // Any other key ends the keyboard-selection gesture. A
                // live selection with a text-producing key replaces it
                // first (CUA): the register has no Visual-char
                // behavior, so the adapter deletes, then inserts below.
                session.sel_anchor = None;
                session.nav_goal = None;
                had_selection = editor.selection.is_some();
                if had_selection
                    && matches!(
                        (key.code, key.modifiers),
                        (KeyCode::Char(_), KeyModifiers::NONE)
                            | (KeyCode::Char(_), KeyModifiers::SHIFT)
                            | (KeyCode::Backspace, KeyModifiers::NONE)
                            | (KeyCode::Delete, KeyModifiers::NONE)
                            | (KeyCode::Enter, KeyModifiers::NONE)
                    )
                {
                    editor.execute(DeleteSelection);
                    editor.mode = EditorMode::Insert;
                }
            }
            // Enter on a list item continues or exits it; with a
            // selection, or off-list, the register keeps its break.
            if matches!(
                (key.code, key.modifiers),
                (KeyCode::Enter, KeyModifiers::NONE)
            ) && !had_selection
                && self.writer_list_enter(id)
            {
                return;
            }
            // Plain chars group into one undo step per word run;
            // everything else keeps its own capture.
            if let KeyCode::Char(c) = key.code {
                if key.modifiers == KeyModifiers::NONE
                    || key.modifiers == KeyModifiers::SHIFT
                {
                    self.writer_type_char(id, c, group, had_selection);
                    return;
                }
            }
            // Only convertible codes reach the register: EdTUI's
            // crossterm conversion panics on the rest (F-keys, media,
            // …), so anything else dies here after ending the gesture.
            if !matches!(
                key.code,
                KeyCode::Char(_)
                    | KeyCode::Enter
                    | KeyCode::Backspace
                    | KeyCode::Delete
            ) {
                self.writer_sync_editor(id);
                self.dirty = true;
                return;
            }
            let Some(session) = self.writers.get_mut(&id) else {
                return;
            };
            let Some(editor) = session.editor.as_mut() else {
                return;
            };
            let mut handler = super::cua::cua_handler();
            handler.on_event(crossterm::event::Event::Key(key), editor);
        } else {
            let Some(session) = self.writers.get_mut(&id) else {
                return;
            };
            let page = session.editor_rows.max(1) as usize;
            let width = session.editor_cols as usize;
            let goal = session.nav_goal;
            let Some(editor) = session.editor.as_mut() else {
                return;
            };
            // The gesture anchor: stored while one runs, the cursor
            // when one starts, or the end opposite the cursor when
            // extending a mouse-made selection.
            let cursor_off = index2_to_offset(&editor.lines, editor.cursor);
            let selecting = key.modifiers == shift || key.modifiers == shift_ctrl;
            let anchor = match session.sel_anchor {
                Some(anchor) => anchor,
                None => match editor.selection.as_ref() {
                    Some(sel) => {
                        let start = index2_to_offset(&editor.lines, sel.start());
                        let end = index2_to_offset(&editor.lines, sel.end()) + 1;
                        if cursor_off == start {
                            end
                        } else {
                            start
                        }
                    }
                    None => cursor_off,
                },
            };
            if selecting {
                session.sel_anchor = Some(anchor);
                if editor.mode != EditorMode::Visual {
                    editor.execute(SwitchMode(EditorMode::Visual));
                }
            }
            if let Some(motion) = nav {
                let buffer = editor.lines.to_string();
                let total = buffer.chars().count();
                let vertical = matches!(
                    motion,
                    NavMove::Up | NavMove::Down | NavMove::PageUp | NavMove::PageDown
                );
                if vertical {
                    let delta = match motion {
                        NavMove::Up => -1,
                        NavMove::Down => 1,
                        NavMove::PageUp => -(page as isize),
                        _ => page as isize,
                    };
                    let (target, kept) =
                        vertical_target(&buffer, total, cursor_off, width, delta, goal);
                    editor.cursor = offset_to_index2(&buffer, target);
                    session.nav_goal = Some(kept);
                } else {
                    let target = nav_target(&buffer, total, cursor_off, motion);
                    editor.cursor = offset_to_index2(&buffer, target);
                    // Any horizontal move drops the visual-column goal.
                    session.nav_goal = None;
                }
            }
            if !selecting {
                // Plain moves collapse, the way every text field
                // does, and land back in Insert: navigation must
                // never strand the editor in a modal state.
                editor.selection = None;
                editor.mode = EditorMode::Insert;
                session.sel_anchor = None;
            } else {
                // Text-field semantics: N presses select N chars. EdTUI
                // visual counts the anchor char too, so the far end moves
                // back one char; meeting the anchor collapses to a cursor.
                let buffer = editor.lines.to_string();
                let total = buffer.chars().count();
                let anchor = anchor.min(total);
                let cursor = index2_to_offset(&editor.lines, editor.cursor).min(total);
                if cursor == anchor {
                    // Back at the start: no selection, not a zero-width
                    // value (which now reads as one char). Leave Visual
                    // too, so the next Shift gesture re-enters it and
                    // gets a fresh Selection instead of moving a None.
                    editor.selection = None;
                    editor.mode = EditorMode::Insert;
                } else if let Some(sel) = editor.selection.as_mut() {
                    let (low, high) = if cursor > anchor {
                        (anchor, cursor - 1)
                    } else {
                        (cursor, anchor - 1)
                    };
                    sel.start = offset_to_index2(&buffer, low);
                    sel.end = offset_to_index2(&buffer, high);
                }
            }
        }
        self.writer_sync_editor(id);
        self.dirty = true;
    }

    /// Feed one terminal mouse event to the editor. Drags select and
    /// the wheel scrolls through EdTUI; EdTUI ignores events that land
    /// outside its painted area or arrive before the first paint.
    pub fn writer_feed_mouse(&mut self, id: crate::session::SessionId, mouse: crossterm::event::MouseEvent) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(editor) = session.editor.as_mut() else {
            return;
        };
        let handler = super::cua::cua_handler();
        // One wheel notch scrolls three lines: EdTUI moves a single
        // line per event, matching the PTY panes and the tour.
        let scrolls = match mouse.kind {
            crossterm::event::MouseEventKind::ScrollUp
            | crossterm::event::MouseEventKind::ScrollDown => 3,
            _ => 1,
        };
        for _ in 0..scrolls {
            handler.on_mouse_event(mouse, editor);
        }
        // Clicks and clean releases land back in Insert: EdTUI parks
        // Down in vim Normal while a Visual selection is live, and
        // nothing would ever return after that. Drags keep EdTUI's
        // Visual and its growing selection untouched.
        match mouse.kind {
            crossterm::event::MouseEventKind::Down(_) => {
                editor.mode = EditorMode::Insert;
            }
            crossterm::event::MouseEventKind::Up(_) => {
                if editor.selection.is_none() {
                    editor.mode = EditorMode::Insert;
                }
            }
            _ => {}
        }
        session.sel_anchor = None;
        session.nav_goal = None;
        self.writer_sync_editor(id);
        self.dirty = true;
    }

    /// Diff the buffer against the document and apply the change.
    /// The document stays the single source of truth; proposals hear
    /// about every edit exactly once, whether typed or accepted.
    pub(super) fn writer_sync_editor(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(editor) = session.editor.as_ref() else {
            return;
        };
        let editor_text = editor.lines.to_string();
        session.selection = editor_selection_to_range(editor);
        let Some(doc) = session.doc.as_mut() else {
            return;
        };
        if editor_text == doc.text {
            return;
        }
        let Some((range, replacement)) = changed_range(&doc.text, &editor_text) else {
            return;
        };
        let edit_line = crate::app::writer::WriterSession::line_of_offset(&doc.text, range.start);
        if doc.apply_edit(range.clone(), &replacement).is_err() {
            return;
        }
        session.note_fence_edit(edit_line);
        session.proposals.on_edit(&range);
    }

    /// Type one plain char with undo grouping (E6): a char that
    /// continues the open group (same run, cursor at its end, no
    /// selection, inside the pause window) applies through a raw
    /// `InsertChar`, which never captures — one undo step per word
    /// run. Anything else goes through the register, opening its
    /// own step. A replace already captured its delete
    /// (`had_selection`), so its char always applies raw: delete
    /// plus insert stay one step.
    /// `pub(crate)`: the feed path owns the group handoff.
    pub(crate) fn writer_type_char(
        &mut self,
        id: crate::session::SessionId,
        c: char,
        group: Option<super::TypeGroup>,
        had_selection: bool,
    ) {
        let now = std::time::Instant::now();
        let continues = match group {
            Some(open) => {
                let Some(session) = self.writers.get(&id) else {
                    return;
                };
                let Some(editor) = session.editor.as_ref() else {
                    return;
                };
                let buffer = editor.lines.to_string();
                let cursor_off = index2_to_offset(&editor.lines, editor.cursor);
                !had_selection
                    && cursor_off == open.end
                    && now.duration_since(open.at) < GROUP_PAUSE
                    && super::nav::is_word_char(c)
                    && open.end > 0
                    && buffer
                        .chars()
                        .nth(open.end - 1)
                        .is_some_and(super::nav::is_word_char)
            }
            None => false,
        };
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(editor) = session.editor.as_mut() else {
            return;
        };
        if continues || had_selection {
            editor.execute(edtui::actions::InsertChar(c));
        } else {
            let mut handler = super::cua::cua_handler();
            handler.on_event(
                crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Char(c),
                    crossterm::event::KeyModifiers::NONE,
                )),
                editor,
            );
        }
        editor.mode = EditorMode::Insert;
        let end = index2_to_offset(&editor.lines, editor.cursor);
        session.type_group = Some(super::TypeGroup { end, at: now });
        session.selection = editor_selection_to_range(editor);
        self.writer_sync_editor(id);
        self.dirty = true;
    }

    /// Enter on a list item (E6): continue the marker on a new line,
    /// or exit the list when the item is empty. Returns true when it
    /// handled the key. Continuation keeps plain-Enter undo behavior
    /// (the register's break captures nothing); exiting deletes the
    /// marker through `DeleteSelection`, which is one step.
    /// `pub(crate)`: the feed path calls this before the register.
    pub(crate) fn writer_list_enter(&mut self, id: crate::session::SessionId) -> bool {
        let Some(session) = self.writers.get_mut(&id) else {
            return false;
        };
        let Some(editor) = session.editor.as_mut() else {
            return false;
        };
        let buffer = editor.lines.to_string();
        let cursor_off = index2_to_offset(&editor.lines, editor.cursor);
        let row = row_of_offset(&buffer, cursor_off);
        let Some(line) = buffer.split('\n').nth(row) else {
            return false;
        };
        let Some(item) = parse_list_item(line) else {
            return false;
        };

        let line_start = cursor_off - col_of_offset(&buffer, cursor_off);
        if item.empty {
            // Exit: the marker goes through one capturing delete.
            // The drain would clobber the Forge clip, so it is
            // saved and restored around the delete.
            use edtui::clipboard::ClipboardTrait;
            let saved_clip = session.clip.0.borrow().clone();
            let marker_end = line_start + item.marker_len();
            editor.cursor = offset_to_index2(&buffer, line_start);
            editor.execute(SwitchMode(EditorMode::Visual));
            if let Some(sel) = editor.selection.as_mut() {
                sel.start = offset_to_index2(&buffer, line_start);
                sel.end = offset_to_index2(&buffer, marker_end.saturating_sub(1));
            }
            editor.execute(DeleteSelection);
            session.clip.set_text(saved_clip);
            editor.mode = EditorMode::Insert;
        } else {
            // Continue: split the line and stamp the next marker.
            // Following numbered items keep their numbers (E6 skips
            // renumbering); task boxes reset to `[ ]`.
            let col = col_of_offset(&buffer, cursor_off);
            let left: String = line.chars().take(col).collect();
            let right: String = line.chars().skip(col).collect();
            let head: String = buffer
                .split('\n')
                .take(row)
                .collect::<Vec<_>>()
                .join("\n");
            let tail: String = buffer
                .split('\n')
                .skip(row + 1)
                .collect::<Vec<_>>()
                .join("\n");
            let mut merged = String::new();
            if row > 0 {
                merged.push_str(&head);
                merged.push('\n');
            }
            merged.push_str(&left);
            merged.push('\n');
            merged.push_str(&item.next_marker());
            merged.push_str(&right);
            if !tail.is_empty() {
                merged.push('\n');
                merged.push_str(&tail);
            }
            let new_cursor = line_start + left.chars().count() + 1 + item.next_marker().chars().count();
            editor.lines = Lines::from(merged.as_str());
            editor.cursor = offset_to_index2(&merged, new_cursor);
            editor.mode = EditorMode::Insert;
            editor.selection = None;
        }
        session.sel_anchor = None;
        session.nav_goal = None;
        session.selection = editor_selection_to_range(editor);
        self.writer_sync_editor(id);
        self.dirty = true;
        true
    }
}

/// Pause that closes a typing group: ~1 s without a char.
const GROUP_PAUSE: std::time::Duration = std::time::Duration::from_millis(1000);

/// A parsed list item: indent width, full marker text (trailing
/// space included), and whether anything follows the marker.
struct ListItem {
    indent: String,
    marker: String,
    empty: bool,
}

impl ListItem {
    /// Marker for the next item: same indent and bullet, numbers
    /// increment, task boxes reset to unchecked.
    fn next_marker(&self) -> String {
        format!("{}{}", self.indent, self.marker)
    }

    fn marker_len(&self) -> usize {
        self.indent.chars().count() + self.marker.chars().count()
    }
}

/// Parse `- `/`* `/`+ `/`1. `/`1) ` plus an optional task box,
/// up to any indent. Returns `None` off-list.
fn parse_list_item(line: &str) -> Option<ListItem> {
    let indent_len = line.chars().take_while(|c| *c == ' ').count();
    let rest = &line[indent_len..];
    let (bullet, after_bullet) = if rest.starts_with("- ") || rest.starts_with("* ") || rest.starts_with("+ ") {
        (rest[..2].to_string(), &rest[2..])
    } else {
        let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 {
            return None;
        }
        let delim = rest[digits..].chars().next()?;
        if delim != '.' && delim != ')' {
            return None;
        }
        let after_delim = &rest[digits + 1..];
        if !after_delim.starts_with(' ') {
            return None;
        }
        let num: u64 = rest[..digits].parse().ok()?;
        (format!("{}{} ", num + 1, delim), &rest[digits + 2..])
    };
    let (task, content) = if after_bullet.starts_with("[ ] ")
        || after_bullet.starts_with("[x] ")
        || after_bullet.starts_with("[X] ")
    {
        ("[ ] ".to_string(), &after_bullet[4..])
    } else {
        (String::new(), after_bullet)
    };
    Some(ListItem {
        indent: line[..indent_len].to_string(),
        marker: format!("{bullet}{task}"),
        empty: content.trim().is_empty(),
    })
}

/// Doc row of a char offset.
fn row_of_offset(text: &str, offset: usize) -> usize {
    text.chars().take(offset).filter(|c| *c == '\n').count()
}

/// Column of a char offset within its row.
fn col_of_offset(text: &str, offset: usize) -> usize {
    let taken: String = text.chars().take(offset).collect();
    taken.rsplit('\n').next().map_or(0, |s| s.chars().count())
}
