//! Typed-path prompt and recent list: the one prompt component
//! behind New/Open/Save-as, plus completion and the resolve flow.

use super::chat::word_edge;
use crate::app::writer::{MAX_PATH_CHARS, WriterOpenPrompt};
use crate::app::AppState;

impl AppState {
    /// Open the typed-path prompt for New / Open / Save-as (E2b:
    /// one prompt component, three kinds; submit routes on the kind).
    /// Entry API: the empty state has no writer entry yet, and the
    /// prompt is exactly how one comes to exist. Opening a prompt
    /// clears any pending confirm: one actionable thing at a time.
    pub fn writer_prompt_open(
        &mut self,
        id: crate::session::SessionId,
        kind: crate::app::writer::PromptKind,
    ) {
        let session = self.writers.entry(id).or_default();
        session.open_prompt = Some(WriterOpenPrompt {
            buffer: String::new(),
            kind,
            cursor: 0,
            select_all: false,
        });
        session.pending_confirm = None;
        session.error = None;
        self.dirty = true;
    }

    /// Type one char into the prompt at the cursor, bounded at
    /// [`MAX_PATH_CHARS`]. Control chars never enter a path. A
    /// select-all replaces the whole buffer (type-to-replace).
    pub fn writer_prompt_char(&mut self, id: crate::session::SessionId, c: char) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(prompt) = session.open_prompt.as_mut() else {
            return;
        };
        if c.is_control() {
            return;
        }
        if prompt.select_all {
            prompt.buffer.clear();
            prompt.cursor = 0;
            prompt.select_all = false;
        }
        if prompt.buffer.chars().count() >= MAX_PATH_CHARS {
            return;
        }
        let at = prompt.cursor.min(prompt.buffer.chars().count());
        let byte = prompt
            .buffer
            .char_indices()
            .nth(at)
            .map(|(index, _)| index)
            .unwrap_or(prompt.buffer.len());
        prompt.buffer.insert(byte, c);
        prompt.cursor = at + 1;
        self.dirty = true;
    }

    /// Backspace one char before the prompt cursor; an empty prompt
    /// stays open (Esc cancels it). A select-all clears the buffer.
    pub fn writer_prompt_backspace(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(prompt) = session.open_prompt.as_mut() else {
            return;
        };
        if prompt.select_all {
            prompt.buffer.clear();
            prompt.cursor = 0;
            prompt.select_all = false;
            self.dirty = true;
            return;
        }
        if prompt.cursor == 0 {
            return;
        }
        let mut chars: Vec<char> = prompt.buffer.chars().collect();
        if prompt.cursor <= chars.len() {
            chars.remove(prompt.cursor - 1);
            prompt.buffer = chars.into_iter().collect();
            prompt.cursor -= 1;
            self.dirty = true;
        }
    }

    /// Delete one char under the prompt cursor. A select-all clears.
    pub fn writer_prompt_delete(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(prompt) = session.open_prompt.as_mut() else {
            return;
        };
        if prompt.select_all {
            prompt.buffer.clear();
            prompt.cursor = 0;
            prompt.select_all = false;
            self.dirty = true;
            return;
        }
        let mut chars: Vec<char> = prompt.buffer.chars().collect();
        if prompt.cursor < chars.len() {
            chars.remove(prompt.cursor);
            prompt.buffer = chars.into_iter().collect();
            self.dirty = true;
        }
    }

    /// Move the prompt cursor one char, clamped. Drops a select-all.
    pub fn writer_prompt_move(&mut self, id: crate::session::SessionId, dir: i32) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(prompt) = session.open_prompt.as_mut() else {
            return;
        };
        let len = prompt.buffer.chars().count();
        prompt.cursor = (prompt.cursor as i32 + dir).clamp(0, len as i32) as usize;
        prompt.select_all = false;
        self.dirty = true;
    }

    /// Prompt cursor to the head of the buffer. Drops a select-all.
    pub fn writer_prompt_home(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(prompt) = session.open_prompt.as_mut() else {
            return;
        };
        prompt.cursor = 0;
        prompt.select_all = false;
        self.dirty = true;
    }

    /// Prompt cursor to the end of the buffer. Drops a select-all.
    pub fn writer_prompt_end(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(prompt) = session.open_prompt.as_mut() else {
            return;
        };
        prompt.cursor = prompt.buffer.chars().count();
        prompt.select_all = false;
        self.dirty = true;
    }

    /// Prompt cursor one word in `dir` (-1/1). Word chars are
    /// alphanumeric plus underscore; anything else is a separator.
    pub fn writer_prompt_word(&mut self, id: crate::session::SessionId, dir: i32) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(prompt) = session.open_prompt.as_mut() else {
            return;
        };
        prompt.cursor = word_edge(&prompt.buffer, prompt.cursor, dir);
        prompt.select_all = false;
        self.dirty = true;
    }

    /// Mark the whole prompt buffer for type-to-replace.
    pub fn writer_prompt_select_all(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(prompt) = session.open_prompt.as_mut() else {
            return;
        };
        prompt.select_all = !prompt.buffer.is_empty();
        self.dirty = true;
    }

    /// Place the prompt cursor at a char index (mouse click-to-place).
    /// Clamped; drops a select-all.
    pub fn writer_prompt_place(&mut self, id: crate::session::SessionId, at: usize) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(prompt) = session.open_prompt.as_mut() else {
            return;
        };
        prompt.cursor = at.min(prompt.buffer.chars().count());
        prompt.select_all = false;
        self.dirty = true;
    }

    /// Cancel the prompt without opening anything.
    pub fn writer_prompt_cancel(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        session.open_prompt = None;
        self.dirty = true;
    }

    /// Submit the prompt: routes on the prompt kind. Success closes
    /// the prompt and builds the editor; failure (and actionable
    /// confirms) keep the prompt open with the reason in the fixed
    /// error slot.
    pub fn writer_submit_open(&mut self, id: crate::session::SessionId) {
        let (buffer, kind) = match self.writers.get(&id).and_then(|s| s.open_prompt.as_ref()) {
            Some(prompt) => (prompt.buffer.clone(), prompt.kind),
            None => return,
        };
        match kind {
            crate::app::writer::PromptKind::New => self.writer_submit_new(id, buffer.trim()),
            crate::app::writer::PromptKind::Open => {
                self.writer_submit_open_existing(id, buffer.trim());
            }
            crate::app::writer::PromptKind::SaveAs => {
                self.writer_submit_save_as(id, buffer.trim());
            }
        }
    }

    /// New-document submit: an existing path offers to open it instead
    /// of silently reusing the name; a missing path creates empty.
    fn writer_submit_new(&mut self, id: crate::session::SessionId, path: &str) {
        if path.is_empty() {
            self.writer_fail(id, "type a path first");
            return;
        }
        let cwd = super::super::requests::writer_cwd(self, id);
        let abs = match crate::infra::paths::confine(&cwd, std::path::Path::new(path)) {
            Ok(abs) => abs,
            Err(e) => {
                self.writer_fail(id, &e.to_string());
                return;
            }
        };
        if abs.is_file() {
            self.writer_raise_confirm(
                id,
                format!("{path} exists:"),
                vec![
                    crate::app::writer::ConfirmAction::OpenInstead(abs),
                    crate::app::writer::ConfirmAction::Cancel,
                ],
            );
            return;
        }
        // A missing path creates empty through the same confined open
        // the agent tool uses (Document::open maps missing → empty).
        self.writer_open_rel(id, path);
    }

    /// Open-document submit: a missing path offers to create it.
    fn writer_submit_open_existing(&mut self, id: crate::session::SessionId, path: &str) {
        if path.is_empty() {
            self.writer_fail(id, "type a path first");
            return;
        }
        let cwd = super::super::requests::writer_cwd(self, id);
        let abs = match crate::infra::paths::confine(&cwd, std::path::Path::new(path)) {
            Ok(abs) => abs,
            Err(e) => {
                self.writer_fail(id, &e.to_string());
                return;
            }
        };
        if !abs.is_file() {
            self.writer_raise_confirm(
                id,
                format!("no file {path}:"),
                vec![
                    crate::app::writer::ConfirmAction::CreateInstead(abs),
                    crate::app::writer::ConfirmAction::Cancel,
                ],
            );
            return;
        }
        self.writer_open_rel(id, path);
    }

    /// Save-as submit: needs an open doc, a Markdown extension (the
    /// same rule the opener enforces), and a non-identity target. An
    /// existing target offers Overwrite; a missing one saves at once.
    fn writer_submit_save_as(&mut self, id: crate::session::SessionId, path: &str) {
        let current = self
            .writers
            .get(&id)
            .and_then(|s| s.doc.as_ref())
            .map(|d| d.abs_path.clone());
        let Some(current) = current else {
            self.writer_fail(id, "no document open");
            return;
        };
        if path.is_empty() {
            self.writer_fail(id, "type a path first");
            return;
        }
        if !crate::writer::document::is_markdown(path) {
            self.writer_fail(id, &format!("{path}: only .md, .markdown or .txt"));
            return;
        }
        let cwd = super::super::requests::writer_cwd(self, id);
        let abs = match crate::infra::paths::confine(&cwd, std::path::Path::new(path)) {
            Ok(abs) => abs,
            Err(e) => {
                self.writer_fail(id, &e.to_string());
                return;
            }
        };
        if abs == current {
            self.writer_fail(id, &format!("already saved as {path}"));
            return;
        }
        if abs.is_file() {
            self.writer_raise_confirm(
                id,
                format!("{path} exists:"),
                vec![
                    crate::app::writer::ConfirmAction::Overwrite(abs),
                    crate::app::writer::ConfirmAction::Cancel,
                ],
            );
            return;
        }
        self.writer_save_as_to(id, path, abs);
    }

    /// Move the recent-list keyboard selection, clamped to the cache.
    /// `pub(crate)`: the TUI input layer calls this directly.
    pub(crate) fn writer_recent_move(&mut self, id: crate::session::SessionId, delta: i32) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        if session.recent_cache.is_empty() {
            session.recent_sel = 0;
        } else {
            let len = session.recent_cache.len();
            let next = (session.recent_sel as i32 + delta).clamp(0, len as i32 - 1);
            session.recent_sel = next as usize;
        }
        self.dirty = true;
    }

    /// Open the keyboard-selected recent row, if any.
    /// `pub(crate)`: the TUI input layer calls this directly.
    pub(crate) fn writer_recent_open(&mut self, id: crate::session::SessionId) {
        let sel = self.writers.get(&id).map(|s| s.recent_sel).unwrap_or(0);
        self.writer_open_recent_at(id, sel);
    }

    /// Open one cached recent row by index; out-of-range opens nothing.
    /// `pub(crate)`: mouse clicks land on rows, not the selection.
    pub(crate) fn writer_open_recent_at(&mut self, id: crate::session::SessionId, index: usize) {
        let rel = self
            .writers
            .get(&id)
            .and_then(|s| s.recent_cache.get(index))
            .map(|e| e.rel.clone());
        let Some(rel) = rel else {
            return;
        };
        if let Some(session) = self.writers.get_mut(&id) {
            session.recent_sel = index;
        }
        self.writer_open_rel(id, &rel);
    }

    /// Dismiss the topmost transient: the More menu first, then the
    /// pending confirm (a Cancel without firing). True when something
    /// dismissed, so keys fall through to their normal target
    /// otherwise. `pub(crate)`: the TUI input layer calls this directly.
    pub(crate) fn writer_dismiss_top(&mut self, id: crate::session::SessionId) -> bool {
        // The bar close below needs `self`, so the transient checks
        // run in a scope that ends the session borrow first.
        let find_open = {
            let Some(session) = self.writers.get_mut(&id) else {
                return false;
            };
            if session.more_open {
                session.more_open = false;
                self.dirty = true;
                return true;
            }
            if session.pending_confirm.is_some() {
                session.pending_confirm = None;
                self.dirty = true;
                return true;
            }
            session.find.is_some()
        };
        // Esc closes the find bar after the menu and confirms,
        // leaving the cursor on the current match.
        if find_open {
            self.writer_find_close(id);
            return true;
        }
        false
    }

    /// Fill the prompt buffer with one suggestion (clicking a row).
    /// Bounded like typing; unknown sessions stay silent.
    /// `pub(crate)`: the TUI input layer calls this directly.
    pub(crate) fn writer_prompt_fill(&mut self, id: crate::session::SessionId, value: String) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(prompt) = session.open_prompt.as_mut() else {
            return;
        };
        prompt.buffer = value.chars().take(crate::app::writer::MAX_PATH_CHARS).collect();
        prompt.cursor = prompt.buffer.chars().count();
        prompt.select_all = false;
        self.dirty = true;
    }

    /// Open `rel` (confined, Markdown-gated by Document::open): record
    /// it as opened this run, rebuild the editor, close the prompt.
    /// Same-path re-opens and dirty-switch refusals surface as errors.
    /// `pub(crate)`: recent rows and confirms share it.
    pub(crate) fn writer_open_rel(&mut self, id: crate::session::SessionId, rel: &str) {
        let cwd = super::super::requests::writer_cwd(self, id);
        let abs = match crate::infra::paths::confine(&cwd, std::path::Path::new(rel)) {
            Ok(abs) => abs,
            Err(e) => {
                self.writer_fail(id, &e.to_string());
                return;
            }
        };
        let opened = match self.writers.get_mut(&id) {
            Some(session) => session.open_document_path(&cwd, rel),
            None => return,
        };
        match opened {
            Ok(_) => {
                if let Some(session) = self.writers.get_mut(&id) {
                    session.open_prompt = None;
                    session.pending_confirm = None;
                    session.error = None;
                }
                self.writer_note_opened(id, abs);
                self.writer_open_editor(id);
                self.dirty = true;
            }
            Err(message) => self.writer_fail(id, &message),
        }
    }

    /// Raise an actionable confirm into the fixed error slot. The
    /// prompt stays open underneath; Cancel returns to it.
    fn writer_raise_confirm(
        &mut self,
        id: crate::session::SessionId,
        message: String,
        actions: Vec<crate::app::writer::ConfirmAction>,
    ) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        session.pending_confirm = Some(crate::app::writer::PendingConfirm { message, actions });
        self.dirty = true;
    }

    /// Fire one confirm action by index; out-of-range clicks are
    /// ignored. The confirm clears first so every branch below lands
    /// with errors (if any) in the fixed slot, never stacked behind it.
    pub fn writer_fire_confirm(&mut self, id: crate::session::SessionId, index: usize) {
        // Out-of-range picks (and firing with no confirm) are ignored
        // with the row left up for a valid pick.
        let action = match self.writers.get_mut(&id) {
            Some(session) => match session.pending_confirm.take() {
                Some(mut confirm) if index < confirm.actions.len() => {
                    Some(confirm.actions.swap_remove(index))
                }
                kept => {
                    session.pending_confirm = kept;
                    None
                }
            },
            None => None,
        };
        self.dirty = true;
        let Some(action) = action else { return };
        match action {
            crate::app::writer::ConfirmAction::Cancel => {}
            crate::app::writer::ConfirmAction::OpenInstead(abs)
            | crate::app::writer::ConfirmAction::CreateInstead(abs) => {
                let rel = self.writer_rel_for(id, &abs);
                self.writer_open_rel(id, &rel);
            }
            crate::app::writer::ConfirmAction::Overwrite(abs) => {
                let rel = self.writer_rel_for(id, &abs);
                self.writer_save_as_to(id, &rel, abs);
            }
            crate::app::writer::ConfirmAction::SaveAndClose => {
                self.writer_save(id);
                let dirty = self
                    .writers
                    .get(&id)
                    .and_then(|s| s.doc.as_ref())
                    .is_some_and(|d| d.dirty);
                if !dirty {
                    self.writer_do_close(id);
                }
            }
            crate::app::writer::ConfirmAction::DiscardClose => self.writer_do_close(id),
            crate::app::writer::ConfirmAction::ReloadFromDisk => {
                if let Err(message) = self.writer_reload_from_disk(id) {
                    self.writer_fail(id, &message);
                }
            }
            crate::app::writer::ConfirmAction::KeepMine => {
                if let Err(message) = self.writer_keep_mine(id) {
                    self.writer_fail(id, &message);
                }
            }
            crate::app::writer::ConfirmAction::SaveToRecreate => {
                let saved = match self.writers.get_mut(&id) {
                    Some(session) => match session.doc.as_mut() {
                        Some(doc) => doc.save_force().map_err(|e| e.to_string()),
                        None => Err("no document open".to_string()),
                    },
                    None => return,
                };
                match saved {
                    Ok(()) => {
                        if let Some(session) = self.writers.get_mut(&id) {
                            session.error = None;
                            session.save_note = Some("saved".to_string());
                        }
                    }
                    Err(message) => self.writer_fail(id, &message),
                }
                self.dirty = true;
            }
            crate::app::writer::ConfirmAction::CloseDoc => self.writer_do_close(id),
        }
    }

    /// Session-relative display path for an absolute path; falls back
    /// to the full path when it escapes the session folder.
    fn writer_rel_for(&self, id: crate::session::SessionId, abs: &std::path::Path) -> String {
        let cwd = super::super::requests::writer_cwd(self, id);
        abs.strip_prefix(&cwd)
            .map(|rel| rel.to_string_lossy().into_owned())
            .unwrap_or_else(|_| abs.to_string_lossy().into_owned())
    }

    /// Tab completion over the prompt: first opened-this-run or recent
    /// path extending the typed prefix. A second Tab is stable (the
    /// filled value no longer extends), so completion never cycles.
    pub fn writer_prompt_complete(&mut self, id: crate::session::SessionId) {
        let prefix = match self.writers.get(&id).and_then(|s| s.open_prompt.as_ref()) {
            Some(prompt) => prompt.buffer.clone(),
            None => return,
        };
        let cwd = super::super::requests::writer_cwd(self, id);
        let mut seen = std::collections::HashSet::new();
        let mut hit: Option<String> = None;
        let opened: Vec<String> = self
            .writers
            .get(&id)
            .map(|s| {
                s.opened
                    .iter()
                    .filter_map(|abs| {
                        abs.strip_prefix(&cwd)
                            .ok()
                            .map(|rel| rel.to_string_lossy().into_owned())
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let recent: Vec<String> = self
            .writers
            .get(&id)
            .map(|s| s.recent_cache.iter().map(|e| e.rel.clone()).collect())
            .unwrap_or_default();
        // Live filesystem last: files the cache has not seen yet (open
        // races it), through the jail. The suggestion row previews only
        // the cache hits above; Tab additionally consults the disk.
        let live = crate::writer::recent::complete(&cwd, &prefix);
        for candidate in opened.into_iter().chain(recent).chain(live) {
            if !seen.insert(candidate.clone()) {
                continue;
            }
            if candidate.starts_with(prefix.as_str()) && candidate.len() > prefix.len() {
                hit = Some(candidate);
                break;
            }
        }
        if let (Some(hit), Some(session)) = (hit, self.writers.get_mut(&id)) {
            if let Some(prompt) = session.open_prompt.as_mut() {
                prompt.buffer = hit;
                prompt.cursor = prompt.buffer.chars().count();
                prompt.select_all = false;
                self.dirty = true;
            }
        }
    }
}
