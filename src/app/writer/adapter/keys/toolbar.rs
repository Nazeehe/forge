//! Toolbar pill dispatch: every pill funnels through these, so
//! mouse clicks and shortcut keys share one path.

use crate::app::writer::WriterFocus;
use crate::app::AppState;

impl AppState {
    /// Toolbar dispatch (E2b): every toolbar pill funnels through
    /// these, so mouse clicks and shortcut keys share one path.
    /// Toolbar `(*New document)`: the New-kind prompt. Refused under
    /// the process lock (a doc switch mid-run strands the agent).
    pub fn writer_toolbar_new(&mut self, id: crate::session::SessionId) {
        if self.writer_process_locked(id) {
            self.writer_process_deny(id);
            return;
        }
        self.writer_prompt_open(id, crate::app::writer::PromptKind::New);
    }

    /// Toolbar `(Open…)`: the Open-kind prompt, with completion and
    /// the create-instead confirm. Refused under the process lock.
    pub fn writer_toolbar_open(&mut self, id: crate::session::SessionId) {
        if self.writer_process_locked(id) {
            self.writer_process_deny(id);
            return;
        }
        self.writer_prompt_open(id, crate::app::writer::PromptKind::Open);
    }

    /// Toolbar `(Save)`: plain S2 save; no doc is a fixed-slot error.
    /// Refused under the process lock (the agent owns the file now).
    pub fn writer_toolbar_save(&mut self, id: crate::session::SessionId) {
        if self.writer_process_locked(id) {
            self.writer_process_deny(id);
            return;
        }
        self.writer_save(id);
    }

    /// Toolbar `(Save as)`: needs an open doc, otherwise the prompt
    /// would have nothing to write. The entry is created first so the
    /// refusal lands in the fixed error slot instead of vanishing.
    pub fn writer_toolbar_save_as(&mut self, id: crate::session::SessionId) {
        if self.writer_process_locked(id) {
            self.writer_process_deny(id);
            return;
        }
        let has_doc = self.writers.entry(id).or_default().doc.is_some();
        if !has_doc {
            self.writer_fail(id, "no document open");
            return;
        }
        self.writer_prompt_open(id, crate::app::writer::PromptKind::SaveAs);
    }

    /// Toolbar `(Close)`: clean closes at once, dirty confirms.
    /// Refused under the process lock.
    pub fn writer_toolbar_close(&mut self, id: crate::session::SessionId) {
        if self.writer_process_locked(id) {
            self.writer_process_deny(id);
            return;
        }
        self.writer_close_doc(id);
    }

    /// Toolbar `(Assistant)` / `(Assistant*)`: the real E10 toggle.
    pub fn writer_toolbar_assistant(&mut self, id: crate::session::SessionId) {
        self.writer_toggle_assistant(id);
    }

    /// Toolbar `(Preview)`: the read-only rendered view (E8).
    /// Entering resets the scroll to the head; the editor keeps its
    /// cursor and viewport underneath, so leaving resumes exactly.
    pub fn writer_toolbar_preview(&mut self, id: crate::session::SessionId) {
        self.writer_toggle_preview(id);
    }

    /// Toggle the rendered preview. Without a doc it is a silent
    /// no-op, like the Assistant toggle: the pill is dimmed there
    /// and the key has nothing to show.
    /// `pub(crate)`: the pill, the More row and Alt+P share it.
    pub(crate) fn writer_toggle_preview(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        if session.doc.is_none() {
            return;
        }
        session.preview = !session.preview;
        if session.preview {
            session.preview_scroll = 0;
            session.error = None;
        }
        // The wrap gesture cannot survive a view change (its text can).
        self.abandon_wrap(id);
        self.dirty = true;
    }

    /// Toggle absolute line numbers in the editor gutter (E8, off by
    /// default). Without a doc, a silent no-op like Preview.
    /// `pub(crate)`: the More menu row calls this directly.
    pub(crate) fn writer_toggle_line_numbers(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        if session.doc.is_none() {
            return;
        }
        session.line_numbers = !session.line_numbers;
        self.dirty = true;
    }

    /// Scroll the rendered preview by `dy` rows, clamped to the
    /// rendered text. No preview, no doc, or empty text: no-op.
    /// `pub(crate)`: wheel, arrows, PgUp/PgDn, Home/End share it.
    pub(crate) fn writer_preview_scroll(&mut self, id: crate::session::SessionId, dy: isize) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        if !session.preview {
            return;
        }
        let Some(doc) = session.doc.as_ref() else {
            return;
        };
        let height = crate::ui::writer::preview_height(&doc.text, session.editor_cols as usize);
        let next = session.preview_scroll as isize + dy;
        session.preview_scroll = next.clamp(0, height.saturating_sub(1) as isize) as u16;
        self.dirty = true;
    }

    /// Scroll the preview to its head or tail.
    /// `pub(crate)`: Home/End in preview share it.
    pub(crate) fn writer_preview_edge(&mut self, id: crate::session::SessionId, tail: bool) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        if !session.preview {
            return;
        }
        let Some(doc) = session.doc.as_ref() else {
            return;
        };
        let height = crate::ui::writer::preview_height(&doc.text, session.editor_cols as usize);
        session.preview_scroll = if tail {
            height.saturating_sub(1) as u16
        } else {
            0
        };
        self.dirty = true;
    }

    /// Toggle the narrow-mode More menu; firing a menu row closes it.
    /// `pub(crate)`: the TUI input layer calls this directly.
    pub(crate) fn writer_toggle_more(&mut self, id: crate::session::SessionId) {
        if let Some(session) = self.writers.get_mut(&id) {
            session.more_open = !session.more_open;
            self.dirty = true;
        }
    }

    /// Toggle the assistant panel. The panel is hidden by default and
    /// the editor takes the full width then; hiding returns focus to
    /// the editor, since the chat box lives in the panel.
    pub fn writer_toggle_assistant(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        session.panel_visible = !session.panel_visible;
        if !session.panel_visible && session.focus != WriterFocus::Editor {
            session.focus = WriterFocus::Editor;
        }
        self.dirty = true;
    }

    /// Cycle keyboard focus Editor → Chat → Thread → Editor. Chat
    /// and Thread live in the assistant panel, so while it is hidden
    /// focus stays in the editor instead of moving somewhere invisible.
    pub fn writer_cycle_focus(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        if !session.panel_visible {
            session.focus = WriterFocus::Editor;
            self.dirty = true;
            return;
        }
        session.focus = match session.focus {
            WriterFocus::Editor => WriterFocus::Chat,
            WriterFocus::Chat => WriterFocus::Thread,
            WriterFocus::Thread => WriterFocus::Editor,
        };
        // Leaving the editor ends the wrap gesture (its text stays).
        self.abandon_wrap(id);
        self.dirty = true;
    }
}
