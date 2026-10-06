//! Find bar state and search/replace ops (E7). The bar paints in
//! the fixed error slot, so opening it never shifts the layout; like
//! the path prompt it is a transient without a focus variant — while
//! it is open and focus is Editor, text and navigation keys edit the
//! focused field instead of the document. Undo/redo still reach the
//! editor (a replace-all reverts with the bar open); anything else is
//! ignored, prompt parity. Enter activates the Tab-focused control,
//! F3/Shift+F3 and Up/Down step through matches with wrap-around,
//! Esc closes through `writer_dismiss_top` and leaves the cursor on
//! the current match. Ctrl+F toggles; Ctrl+H opens straight into
//! the replace field (Alt+H is the reachable twin: legacy terminals
//! deliver Ctrl+H as Backspace, which can never carry Control).

use super::chat::word_edge;
use super::{editor_selection_to_range, index2_to_offset, offset_to_index2};
use crate::app::writer::{
    FindFocus, WriterFind, MAX_FIND_CHARS, MAX_FIND_MATCHES, MAX_REPLACE_CHARS,
};
use crate::app::AppState;
use edtui::{
    actions::{DeleteSelection, InsertChar, SwitchMode},
    EditorMode, Lines,
};

/// Incremental match search over `text`: non-overlapping char-offset
/// ranges for a non-empty query, capped at `MAX_FIND_MATCHES` (the
/// flag reports the cap). Case folds per char so no lowercased copy
/// can skew the offsets.
pub(super) fn find_matches(
    text: &str,
    query: &str,
    case_sensitive: bool,
) -> (Vec<std::ops::Range<usize>>, bool) {
    let q: Vec<char> = query.chars().collect();
    if q.is_empty() {
        return (Vec::new(), false);
    }
    let t: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut overflow = false;
    let mut i = 0;
    while i + q.len() <= t.len() {
        if chars_eq(&t[i..i + q.len()], &q, case_sensitive) {
            if out.len() >= MAX_FIND_MATCHES {
                overflow = true;
            } else {
                out.push(i..i + q.len());
            }
            i += q.len();
        } else {
            i += 1;
        }
    }
    (out, overflow)
}

/// One window comparison, folding case per char when asked.
fn chars_eq(window: &[char], query: &[char], case_sensitive: bool) -> bool {
    window.iter().zip(query.iter()).all(|(a, b)| {
        if case_sensitive {
            a == b
        } else {
            a.to_lowercase().eq(b.to_lowercase())
        }
    })
}

/// Recompute matches when the doc moved under the bar (edits,
/// undo, agent applies). Keeps the note and the cursor: only the
/// match list goes stale, never the user's place. `pub(crate)`:
/// the paint layer refreshes before highlighting.
pub(crate) fn refresh_find_matches(session: &mut crate::app::writer::WriterSession) {
    let Some(find) = session.find.as_mut() else {
        return;
    };
    let Some(doc) = session.doc.as_ref() else {
        return;
    };
    if find.rev == doc.revision {
        return;
    }
    let (matches, overflow) = find_matches(&doc.text, &find.query, find.case_sensitive);
    find.matches = matches;
    find.overflow = overflow;
    find.rev = doc.revision;
    if find.matches.is_empty() {
        find.current = 0;
    } else {
        find.current = find.current.min(find.matches.len() - 1);
    }
}

impl AppState {
    /// Open the find bar, seeding the query from a live selection.
    /// With replace already open this only reveals the replace field.
    /// Preview turns off: search runs on the source and the matches
    /// highlight in the editor. `pub(crate)`: Ctrl+F/Ctrl+H/Alt+H
    /// arrive through the TUI input layer.
    pub(crate) fn writer_find_open(&mut self, id: crate::session::SessionId, with_replace: bool) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        if session.doc.is_none() || session.editor.is_none() {
            return;
        }
        if let Some(find) = session.find.as_mut() {
            if with_replace && !find.replace_open {
                find.replace_open = true;
                find.focus = FindFocus::Replace;
                find.note = None;
            }
            self.dirty = true;
            return;
        }
        session.preview = false;
        let doc = session.doc.as_ref().expect("checked above");
        let text = doc.text.clone();
        let seed = session.selection.clone().and_then(|range| {
            (range.end > range.start).then(|| {
                text.chars()
                    .skip(range.start)
                    .take(range.end - range.start)
                    .collect::<String>()
            })
        });
        let mut find = WriterFind::default();
        find.replace_open = with_replace;
        find.focus = if with_replace {
            FindFocus::Replace
        } else {
            FindFocus::Query
        };
        if let Some(query) = seed {
            find.cursor = query.chars().count();
            find.query = query;
            find.select_all = true;
            // Anchor the search at the selection it came from: the
            // seeded occurrence is the current match, not the one
            // after the cursor.
            if let (Some(range), Some(editor)) =
                (session.selection.clone(), session.editor.as_mut())
            {
                let buffer = editor.lines.to_string();
                editor.cursor = offset_to_index2(&buffer, range.start);
            }
        }
        session.find = Some(find);
        self.find_research(id);
        self.dirty = true;
    }

    /// Close the bar, leaving the cursor on the current match (the
    /// selection collapses; with no matches the cursor stays put).
    /// `pub(crate)`: Esc arrives through `writer_dismiss_top`.
    pub(crate) fn writer_find_close(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(find) = session.find.take() else {
            return;
        };
        let target = find.matches.get(find.current).map(|m| m.start);
        if let (Some(start), Some(editor)) = (target, session.editor.as_mut()) {
            let total = editor.lines.to_string().chars().count();
            editor.cursor = offset_to_index2(&editor.lines.to_string(), start.min(total));
            editor.selection = None;
            editor.mode = EditorMode::Insert;
            session.sel_anchor = None;
            session.nav_goal = None;
        } else if let Some(editor) = session.editor.as_mut() {
            editor.selection = None;
            editor.mode = EditorMode::Insert;
            session.sel_anchor = None;
            session.nav_goal = None;
        }
        self.writer_sync_editor(id);
        self.dirty = true;
    }

    /// Flip case sensitivity and re-search from the editor cursor.
    /// `pub(crate)`: the toggle pill (click or Tab+Enter) calls this.
    pub(crate) fn writer_find_toggle_case(&mut self, id: crate::session::SessionId) {
        let has = self
            .writers
            .get_mut(&id)
            .and_then(|session| session.find.as_mut())
            .map(|find| {
                find.case_sensitive = !find.case_sensitive;
                find.note = None;
            })
            .is_some();
        if has {
            self.find_research(id);
            self.dirty = true;
        }
    }

    /// Type into the focused bar field (same editing as the prompt);
    /// typing while a pill is focused jumps back to the query field.
    /// `pub(crate)`: the TUI input layer calls this directly.
    pub(crate) fn writer_find_char(&mut self, id: crate::session::SessionId, c: char) {
        if c.is_control() {
            return;
        }
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(find) = session.find.as_mut() else {
            return;
        };
        if !matches!(find.focus, FindFocus::Query | FindFocus::Replace) {
            find.focus = FindFocus::Query;
        }
        let replace = find.focus == FindFocus::Replace && find.replace_open;
        let (field, cursor, bound) = if replace {
            (&mut find.replace, &mut find.replace_cursor, MAX_REPLACE_CHARS)
        } else {
            (&mut find.query, &mut find.cursor, MAX_FIND_CHARS)
        };
        if find.select_all {
            field.clear();
            *cursor = 0;
            find.select_all = false;
        }
        if field.chars().count() >= bound {
            return;
        }
        let at = (*cursor).min(field.chars().count());
        let byte = field
            .char_indices()
            .nth(at)
            .map(|(index, _)| index)
            .unwrap_or(field.len());
        field.insert(byte, c);
        *cursor = at + 1;
        let query_edited = !replace;
        find.note = None;
        self.dirty = true;
        if query_edited {
            self.find_research(id);
        }
    }

    /// Backspace in the focused bar field.
    /// `pub(crate)`: the TUI input layer calls this directly.
    pub(crate) fn writer_find_backspace(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(find) = session.find.as_mut() else {
            return;
        };
        if !matches!(find.focus, FindFocus::Query | FindFocus::Replace) {
            find.focus = FindFocus::Query;
        }
        let replace = find.focus == FindFocus::Replace && find.replace_open;
        let query_edited = !replace;
        if find.select_all {
            if replace {
                find.replace.clear();
                find.replace_cursor = 0;
            } else {
                find.query.clear();
                find.cursor = 0;
            }
            find.select_all = false;
        } else if replace {
            let at = find.replace_cursor.min(find.replace.chars().count());
            if at > 0 {
                let byte = find.replace.char_indices().nth(at - 1).map(|(i, _)| i).unwrap_or(0);
                find.replace.remove(byte);
                find.replace_cursor = at - 1;
            }
        } else {
            let at = find.cursor.min(find.query.chars().count());
            if at > 0 {
                let byte = find.query.char_indices().nth(at - 1).map(|(i, _)| i).unwrap_or(0);
                find.query.remove(byte);
                find.cursor = at - 1;
            }
        }
        find.note = None;
        self.dirty = true;
        if query_edited {
            self.find_research(id);
        }
    }

    /// Delete in the focused bar field.
    /// `pub(crate)`: the TUI input layer calls this directly.
    pub(crate) fn writer_find_delete(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(find) = session.find.as_mut() else {
            return;
        };
        if !matches!(find.focus, FindFocus::Query | FindFocus::Replace) {
            find.focus = FindFocus::Query;
        }
        let replace = find.focus == FindFocus::Replace && find.replace_open;
        let query_edited = !replace;
        if find.select_all {
            if replace {
                find.replace.clear();
                find.replace_cursor = 0;
            } else {
                find.query.clear();
                find.cursor = 0;
            }
            find.select_all = false;
        } else if replace {
            let at = find.replace_cursor.min(find.replace.chars().count());
            if at < find.replace.chars().count() {
                let byte = find.replace.char_indices().nth(at).map(|(i, _)| i).unwrap_or(0);
                find.replace.remove(byte);
            }
        } else {
            let at = find.cursor.min(find.query.chars().count());
            if at < find.query.chars().count() {
                let byte = find.query.char_indices().nth(at).map(|(i, _)| i).unwrap_or(0);
                find.query.remove(byte);
            }
        }
        find.note = None;
        self.dirty = true;
        if query_edited {
            self.find_research(id);
        }
    }

    /// Move the focused field's cursor by `dir` chars.
    /// `pub(crate)`: the TUI input layer calls this directly.
    pub(crate) fn writer_find_move(&mut self, id: crate::session::SessionId, dir: i32) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(find) = session.find.as_mut() else {
            return;
        };
        if find.focus == FindFocus::Replace && find.replace_open {
            let len = find.replace.chars().count();
            find.replace_cursor = (find.replace_cursor as i32 + dir).clamp(0, len as i32) as usize;
        } else {
            let len = find.query.chars().count();
            find.cursor = (find.cursor as i32 + dir).clamp(0, len as i32) as usize;
        }
        find.select_all = false;
        self.dirty = true;
    }

    /// Start of the focused field. `pub(crate)`: the TUI input layer.
    pub(crate) fn writer_find_home(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(find) = session.find.as_mut() else {
            return;
        };
        if find.focus == FindFocus::Replace && find.replace_open {
            find.replace_cursor = 0;
        } else {
            find.cursor = 0;
        }
        find.select_all = false;
        self.dirty = true;
    }

    /// End of the focused field. `pub(crate)`: the TUI input layer.
    pub(crate) fn writer_find_end(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(find) = session.find.as_mut() else {
            return;
        };
        if find.focus == FindFocus::Replace && find.replace_open {
            find.replace_cursor = find.replace.chars().count();
        } else {
            find.cursor = find.query.chars().count();
        }
        find.select_all = false;
        self.dirty = true;
    }

    /// Word jump in the focused field. `pub(crate)`: the TUI layer.
    pub(crate) fn writer_find_word(&mut self, id: crate::session::SessionId, dir: i32) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(find) = session.find.as_mut() else {
            return;
        };
        if find.focus == FindFocus::Replace && find.replace_open {
            find.replace_cursor = word_edge(&find.replace, find.replace_cursor, dir);
        } else {
            find.cursor = word_edge(&find.query, find.cursor, dir);
        }
        find.select_all = false;
        self.dirty = true;
    }

    /// Mark the focused field for type-to-replace.
    /// `pub(crate)`: the TUI input layer calls this directly.
    pub(crate) fn writer_find_select_all(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(find) = session.find.as_mut() else {
            return;
        };
        let empty = if find.focus == FindFocus::Replace && find.replace_open {
            find.replace.is_empty()
        } else {
            find.query.is_empty()
        };
        find.select_all = !empty;
        self.dirty = true;
    }

    /// Open or collapse the replace field. Expanding focuses the
    /// replace field; collapsing returns to the query field. The
    /// typed replacement survives a collapse.
    /// `pub(crate)`: the toggle pill (click or Tab+Enter) calls this.
    pub(crate) fn writer_find_toggle_replace(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(find) = session.find.as_mut() else {
            return;
        };
        find.replace_open = !find.replace_open;
        find.focus = if find.replace_open {
            FindFocus::Replace
        } else {
            FindFocus::Query
        };
        find.select_all = false;
        find.note = None;
        self.dirty = true;
    }

    /// Tab: cycle Query → case pill → replace toggle → replace
    /// group → back. The replace group is skipped while closed.
    /// `pub(crate)`: the TUI input layer.
    pub(crate) fn writer_find_tab(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(find) = session.find.as_mut() else {
            return;
        };
        find.focus = if find.replace_open {
            match find.focus {
                FindFocus::Query => FindFocus::CaseBtn,
                FindFocus::CaseBtn => FindFocus::ToggleBtn,
                FindFocus::ToggleBtn => FindFocus::Replace,
                FindFocus::Replace => FindFocus::ReplaceNextBtn,
                FindFocus::ReplaceNextBtn => FindFocus::ReplaceAllBtn,
                FindFocus::ReplaceAllBtn => FindFocus::Query,
            }
        } else {
            match find.focus {
                FindFocus::Query => FindFocus::CaseBtn,
                FindFocus::CaseBtn => FindFocus::ToggleBtn,
                _ => FindFocus::Query,
            }
        };
        find.select_all = false;
        self.dirty = true;
    }

    /// Enter: activate the Tab-focused control (next in the query
    /// field, replace under the replace field and its pill).
    /// `pub(crate)`: the TUI input layer calls this directly.
    pub(crate) fn writer_find_activate(&mut self, id: crate::session::SessionId) {
        let focus = self
            .writers
            .get(&id)
            .and_then(|session| session.find.as_ref())
            .map(|find| find.focus);
        match focus {
            Some(FindFocus::Replace | FindFocus::ReplaceNextBtn) => {
                self.writer_find_replace_current(id);
            }
            Some(FindFocus::ReplaceAllBtn) => self.writer_find_replace_all(id),
            Some(FindFocus::CaseBtn) => self.writer_find_toggle_case(id),
            Some(FindFocus::ToggleBtn) => self.writer_find_toggle_replace(id),
            _ => self.writer_find_next(id, 1),
        }
    }

    /// Step the current match by `dir` with wrap-around.
    /// `pub(crate)`: Enter/Up/Down/F3 arrive through the TUI layer.
    pub(crate) fn writer_find_next(&mut self, id: crate::session::SessionId, dir: i32) {
        let len = {
            let Some(session) = self.writers.get_mut(&id) else {
                return;
            };
            if session.find.is_none() {
                return;
            }
            refresh_find_matches(session);
            session.find.as_ref().expect("checked above").matches.len()
        };
        if len == 0 {
            return;
        }
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let find = session.find.as_mut().expect("checked above");
        find.current = (find.current as i32 + dir).rem_euclid(len as i32) as usize;
        self.find_goto_current(id);
        self.dirty = true;
    }

    /// Replace the current match, then move to the next one: one
    /// undo step (only the delete captures, mirror Accept).
    /// `pub(crate)`: the (Replace) pill and the replace field's
    /// Enter arrive through the TUI input layer.
    pub(crate) fn writer_find_replace_current(&mut self, id: crate::session::SessionId) {
        let (range, replacement) = {
            let Some(session) = self.writers.get_mut(&id) else {
                return;
            };
            if session.find.is_none() {
                return;
            }
            refresh_find_matches(session);
            let find = session.find.as_ref().expect("checked above");
            let Some(range) = find.matches.get(find.current).cloned() else {
                return;
            };
            (range, find.replace.clone())
        };
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(editor) = session.editor.as_mut() else {
            return;
        };
        // One undo step: the delete captures, the inserts never do.
        // The Forge clipboard is saved and restored around the
        // delete so replace never clobbers what the user copied.
        let saved = session.clip.0.borrow().clone();
        let buffer = editor.lines.to_string();
        editor.cursor = offset_to_index2(&buffer, range.start);
        editor.execute(SwitchMode(EditorMode::Visual));
        if let Some(sel) = editor.selection.as_mut() {
            sel.start = offset_to_index2(&buffer, range.start);
            sel.end = offset_to_index2(&buffer, range.end.saturating_sub(1).max(range.start));
        }
        editor.execute(DeleteSelection);
        for c in replacement.chars() {
            editor.execute(InsertChar(c));
        }
        editor.mode = EditorMode::Insert;
        *session.clip.0.borrow_mut() = saved;
        session.sel_anchor = None;
        session.nav_goal = None;
        session.type_group = None;
        self.writer_sync_editor(id);
        self.find_research(id);
        self.dirty = true;
    }

    /// Replace every capped match as ONE undo step and report the
    /// count; refused past the match cap until the query narrows.
    /// `pub(crate)`: the (Replace all) pill calls this.
    pub(crate) fn writer_find_replace_all(&mut self, id: crate::session::SessionId) {
        let (matches, replacement) = {
            let Some(session) = self.writers.get_mut(&id) else {
                return;
            };
            if session.find.is_none() {
                return;
            }
            refresh_find_matches(session);
            let find = session.find.as_ref().expect("checked above");
            if find.matches.is_empty() {
                return;
            }
            if find.overflow {
                let find = session.find.as_mut().expect("checked above");
                find.note = Some("10000+ matches: narrow the query first".to_string());
                self.dirty = true;
                return;
            }
            (
                find.matches.clone(),
                find.replace.clone(),
            )
        };
        let count = matches.len();
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(editor) = session.editor.as_mut() else {
            return;
        };
        // Bottom-up: earlier match offsets stay valid while later
        // ones change. The LAST match captures through its delete
        // (mirror outdent); everything above splices the string
        // directly, which never captures — one undo step total.
        let last = matches.len() - 1;
        let saved = session.clip.0.borrow().clone();
        let buffer = editor.lines.to_string();
        editor.cursor = offset_to_index2(&buffer, matches[last].start);
        editor.execute(SwitchMode(EditorMode::Visual));
        if let Some(sel) = editor.selection.as_mut() {
            sel.start = offset_to_index2(&buffer, matches[last].start);
            sel.end = offset_to_index2(
                &buffer,
                matches[last].end.saturating_sub(1).max(matches[last].start),
            );
        }
        editor.execute(DeleteSelection);
        for c in replacement.chars() {
            editor.execute(InsertChar(c));
        }
        *session.clip.0.borrow_mut() = saved;
        let live = editor.lines.to_string();
        let mut chars: Vec<char> = live.chars().collect();
        for m in matches[..last].iter().rev() {
            chars.splice(m.start..m.end, replacement.chars());
        }
        editor.lines = Lines::from(chars.iter().collect::<String>().as_str());
        let fresh = editor.lines.to_string();
        editor.cursor = offset_to_index2(&fresh, matches[0].start.min(fresh.chars().count()));
        editor.selection = None;
        editor.mode = EditorMode::Insert;
        session.sel_anchor = None;
        session.nav_goal = None;
        session.type_group = None;
        self.writer_sync_editor(id);
        self.find_research(id);
        if let Some(session) = self.writers.get_mut(&id) {
            if let Some(find) = session.find.as_mut() {
                find.note = Some(format!("{count} replaced"));
            }
        }
        self.dirty = true;
    }

    /// Focus the query field (clicking it). `pub(crate)`: mouse.
    pub(crate) fn writer_find_focus_query(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(find) = session.find.as_mut() else {
            return;
        };
        find.focus = FindFocus::Query;
        find.select_all = false;
        self.dirty = true;
    }

    /// Focus the replace field (clicking it). `pub(crate)`: mouse.
    pub(crate) fn writer_find_focus_replace(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(find) = session.find.as_mut() else {
            return;
        };
        if find.replace_open {
            find.focus = FindFocus::Replace;
            find.select_all = false;
            self.dirty = true;
        }
    }

    /// Re-search from the editor cursor and jump to the first match
    /// at or after it (wrapping to the first): incremental search.
    fn find_research(&mut self, id: crate::session::SessionId) {
        let (text, rev, case, query, anchor) = {
            let Some(session) = self.writers.get(&id) else {
                return;
            };
            let Some(find) = session.find.as_ref() else {
                return;
            };
            let Some(doc) = session.doc.as_ref() else {
                return;
            };
            let anchor = session
                .editor
                .as_ref()
                .map(|editor| index2_to_offset(&editor.lines, editor.cursor))
                .unwrap_or(0);
            (
                doc.text.clone(),
                doc.revision,
                find.case_sensitive,
                find.query.clone(),
                anchor,
            )
        };
        let (matches, overflow) = find_matches(&text, &query, case);
        let current = if matches.is_empty() {
            0
        } else {
            matches.iter().position(|m| m.start >= anchor).unwrap_or(0)
        };
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(find) = session.find.as_mut() else {
            return;
        };
        find.matches = matches;
        find.overflow = overflow;
        find.rev = rev;
        find.current = current;
        self.find_goto_current(id);
    }

    /// Move the editor cursor and selection onto the current match.
    fn find_goto_current(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let range = session
            .find
            .as_ref()
            .and_then(|find| find.matches.get(find.current).cloned());
        let Some(range) = range else {
            return;
        };
        let Some(editor) = session.editor.as_mut() else {
            return;
        };
        let buffer = editor.lines.to_string();
        editor.cursor = offset_to_index2(&buffer, range.start);
        editor.execute(SwitchMode(EditorMode::Visual));
        if let Some(sel) = editor.selection.as_mut() {
            sel.start = offset_to_index2(&buffer, range.start);
            sel.end = offset_to_index2(&buffer, range.end.saturating_sub(1).max(range.start));
        }
        session.sel_anchor = None;
        session.nav_goal = None;
        session.selection = editor_selection_to_range(editor);
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::find_matches;

    #[test]
    fn empty_query_matches_nothing() {
        let (matches, overflow) = find_matches("aaa", "", false);
        assert!(matches.is_empty());
        assert!(!overflow);
    }

    #[test]
    fn plain_search_lists_char_ranges() {
        let (matches, overflow) = find_matches("alpha beta alpha", "alpha", false);
        assert_eq!(matches, vec![0..5, 11..16]);
        assert!(!overflow);
    }

    #[test]
    fn case_folding_matches_mixed_case() {
        let (matches, _) = find_matches("Alpha ALPHA alpha", "alpha", false);
        assert_eq!(matches.len(), 3);
    }

    #[test]
    fn case_sensitive_keeps_exact_case_only() {
        let (matches, _) = find_matches("Alpha ALPHA alpha", "alpha", true);
        assert_eq!(matches, vec![12..17]);
    }

    #[test]
    fn match_cap_reports_overflow() {
        let text = "a ".repeat(10_001);
        let (matches, overflow) = find_matches(&text, "a", false);
        assert_eq!(matches.len(), crate::app::writer::MAX_FIND_MATCHES);
        assert!(overflow);
    }
}
