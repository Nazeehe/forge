//! AppState walkthrough tours: overlay accessors, view, and step edits.

use super::*;

impl AppState {
    /// Absolute topbar index of the Walkthrough overlay slot for one
    /// session, or `None` for an unknown session.
    fn walkthrough_slot(&self, id: crate::session::SessionId) -> Option<usize> {
        let rec = self.manager.get(id)?;
        OVERLAY_TABS
            .iter()
            .position(|tab| *tab == "Walkthrough")
            .map(|slot| rec.tabs.len() + slot)
    }

    /// The focused walkthrough, if the active session sits on the
    /// Walkthrough overlay slot and the agent opened a tour. The TUI
    /// renders it immediate-mode over the pane grid.
    pub fn walkthrough_overlay(&self) -> Option<&crate::walkthrough::Walkthrough> {
        let active = self.manager.active()?;
        let (view_id, index) = self.overlay_view?;
        if view_id != active || Some(index) != self.walkthrough_slot(active) {
            return None;
        }
        self.walkthroughs.get(&active)
    }

    /// Mutable twin of [`Self::walkthrough_overlay`], for key routing.
    pub fn walkthrough_overlay_mut(
        &mut self,
    ) -> Option<&mut crate::walkthrough::Walkthrough> {
        let active = self.manager.active()?;
        let (view_id, index) = self.overlay_view?;
        if view_id != active || Some(index) != self.walkthrough_slot(active) {
            return None;
        }
        self.walkthroughs.get_mut(&active)
    }

    /// True while walkthrough keys own input: the overlay slot is
    /// focused and a tour is open for the active session.
    pub fn walkthrough_overlay_active(&self) -> bool {
        self.walkthrough_overlay().is_some()
    }

    /// Placeholder pane behind the immediate-mode tour render: the TUI
    /// draws the walkthrough over the grid, so this only carries the
    /// title (and a hint when no tour is open yet).
    pub fn walkthrough_view(&self, id: crate::session::SessionId) -> crate::ui::PaneView {
        let rec = self.manager.get(id).expect("ordered session exists");
        let live = rec.state.is_live();
        let muted = crate::ui::theme::style(crate::ui::theme::Role::Muted);
        let text = crate::ui::theme::style(crate::ui::theme::Role::Text);
        let line = |content: &str, style: ratatui::style::Style| {
            vec![crate::ui::SpanView {
                text: content.to_string(),
                style,
            }]
        };
        let lines = if self.walkthroughs.contains_key(&id) {
            vec![line("Walkthrough tour", muted)]
        } else {
            vec![
                line("Walkthrough plays a step-by-step tour of the code.", text),
                line("Ask this session, e.g. \"walk me through <...>\"", muted),
            ]
        };
        crate::ui::PaneView {
            title: format!("{} · Walkthrough", rec.name),
            lines,
            live,
            focused: true,
            cursor: None,
        }
    }

    /// Submit the overlay draft as a question: the markup goes straight
    /// into the agent pane and the Enter stages for a later tick — the
    /// same split write comms injections use. The question is logged
    /// only after the pane write lands, so the overlay never claims an
    /// undelivered ask. No human-input note: that would drop the staged
    /// CR we just armed.
    pub fn submit_walkthrough_question(&mut self, id: crate::session::SessionId) -> bool {
        let Some(wt) = self.walkthroughs.get(&id) else {
            return false;
        };
        let draft = match wt.input.as_ref() {
            Some(buf) if !buf.trim().is_empty() => buf.trim().to_string(),
            _ => return false,
        };
        let markup = crate::walkthrough::Walkthrough::question_markup(
            &wt.title,
            wt.index,
            wt.steps.len(),
            wt.current_step(),
            &draft,
        );
        if self.manager.inject_write(id, markup.as_bytes()).is_err() {
            return false;
        }
        self.pending_enter.insert(id, (std::time::Instant::now(), None));
        let Some(wt) = self.walkthroughs.get_mut(&id) else {
            return false;
        };
        wt.take_draft();
        wt.push_question(draft);
        self.dirty = true;
        true
    }

    /// Open a tour over a file: relative paths resolve against the
    /// session cwd, oversize files are refused, and the overlay opens
    /// on the tour at once so the human sees it.
    pub(super) fn walkthrough_start(&mut self, id: crate::session::SessionId, args: &str) -> Result<String, String> {
        let file = Self::tool_arg(args, "file")
            .ok_or_else(|| "walkthrough_start needs a file".to_string())?;
        let steps_text = Self::tool_arg(args, "steps")
            .ok_or_else(|| "walkthrough_start needs steps".to_string())?;
        let title = Self::tool_arg(args, "title").unwrap_or_else(|| file.clone());
        let cwd = self
            .manager
            .get(id)
            .map(|rec| rec.cwd.clone())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let path = std::path::PathBuf::from(&file);
        let path = if path.is_relative() { cwd.join(path) } else { path };
        let bytes = std::fs::read(&path)
            .map_err(|_| format!("cannot read walkthrough file {file:?}"))?;
        if bytes.len() > crate::walkthrough::MAX_FILE_BYTES {
            return Err(format!(
                "walkthrough file {file:?} exceeds {} bytes",
                crate::walkthrough::MAX_FILE_BYTES
            ));
        }
        let content = String::from_utf8_lossy(&bytes);
        let steps = crate::walkthrough::Walkthrough::parse_steps(&steps_text, content.lines().count())?;
        let tour = crate::walkthrough::Walkthrough::start(title, file, &content, steps)?;
        let count = tour.step_count();
        self.walkthroughs.insert(id, tour);
        if let Some(slot) = self.walkthrough_slot(id) {
            self.overlay_view = Some((id, slot));
        }
        self.dirty = true;
        Ok(format!(r#"{{"started":true,"steps":{count}}}"#))
    }

    /// Insert one step into the open tour. A `file_path` that is not
    /// the tour file fails: tours cover one file. `position` is
    /// 1-based like the displayed counter and appends when absent.
    pub(super) fn walkthrough_add_step(
        &mut self,
        id: crate::session::SessionId,
        args: &str,
    ) -> Result<String, String> {
        let start = Self::tool_u32(args, "start_line")
            .ok_or_else(|| "walkthrough_add_step needs start_line".to_string())?;
        let end = Self::tool_u32(args, "end_line")
            .ok_or_else(|| "walkthrough_add_step needs end_line".to_string())?;
        let explanation = Self::tool_arg(args, "explanation").unwrap_or_default();
        let position = Self::tool_u32(args, "position")
            .map(|p| (p.max(1) as usize).saturating_sub(1));
        let Some(tour) = self.walkthroughs.get_mut(&id) else {
            return Err("no walkthrough for this session".to_string());
        };
        if let Some(file) = Self::tool_arg(args, "file_path") {
            if file != tour.file_path {
                return Err("walkthrough_add_step targets the open tour file only".to_string());
            }
        }
        tour.add_step(
            crate::walkthrough::Step { start, end, explanation },
            position,
        )?;
        let count = tour.step_count();
        self.dirty = true;
        Ok(format!(r#"{{"added":true,"steps":{count}}}"#))
    }

    /// Update one step of the open tour. `step_index` is 1-based like
    /// the displayed counter; absent fields keep their values.
    pub(super) fn walkthrough_update(
        &mut self,
        id: crate::session::SessionId,
        args: &str,
    ) -> Result<String, String> {
        let one_based = Self::tool_u32(args, "step_index")
            .ok_or_else(|| "walkthrough_update needs step_index".to_string())?;
        let Some(index) = one_based.checked_sub(1) else {
            return Err("step_index starts at 1".to_string());
        };
        let start = Self::tool_u32(args, "start_line");
        let end = Self::tool_u32(args, "end_line");
        let explanation = Self::tool_arg(args, "explanation");
        let Some(tour) = self.walkthroughs.get_mut(&id) else {
            return Err("no walkthrough for this session".to_string());
        };
        tour.update_step(index as usize, start, end, explanation.as_deref())?;
        self.dirty = true;
        Ok(r#"{"updated":true}"#.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::*;

    #[test]
    fn walkthrough_empty_state_describes_tab_and_invocation() {
        let mut state = AppState::new();
        state.term_size = (40, 180);
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::infra::ids::RunId::generate(), "codex",
        ).unwrap();
        let view = state.walkthrough_view(id);
        let text: String = view.lines.iter().flatten().map(|span| span.text.as_str()).collect::<Vec<_>>().join("\n");
        assert!(text.contains("step-by-step"), "describes the tab: {text:?}");
        assert!(text.contains("walk me through"), "shows how to invoke it: {text:?}");
        assert!(state.manager.remove(id));
    }

    #[test]
    fn walkthrough_tools_drive_tour_lifecycle() {
        let mut state = AppState::new();
        state.term_size = (40, 180);
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::infra::ids::RunId::generate(), "codex",
        ).unwrap();
        // The fake harness calls with the record's own run ID.
        let live_run = state.manager.get(id).unwrap().run_id.as_str().to_string();
        let path = std::env::temp_dir().join(format!("forge-walk-test-{}", std::process::id()));
        std::fs::write(
            &path,
            (1..=10).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n"),
        ).unwrap();
        let args = format!(
            r#"{{"file":{},"steps":"1:3:first\n5:5:second"}}"#,
            crate::ipc::mcp::escape_json(&path.to_string_lossy()),
        );
        let started = comms_reply(&mut state, &live_run, "walkthrough_start", &args);
        assert!(started.contains(r#""ok":true"#), "started: {started}");
        assert!(started.contains(r#""steps":2"#), "started: {started}");
        assert!(state.walkthrough_overlay_active());
        assert_eq!(state.walkthrough_overlay().unwrap().title, path.to_string_lossy());
        // Answering with nothing waiting fails instead of inventing Q&A.
        let early = comms_reply(&mut state, &live_run, "walkthrough_answer", r#"{"answer":"x"}"#);
        assert!(early.contains("no walkthrough question waiting"), "early: {early}");
        // Ask through the overlay: the draft submits, the markup stages
        // an Enter, and the question waits for the agent.
        state.walkthrough_overlay_mut().unwrap().input = Some("why three?".to_string());
        assert!(state.submit_walkthrough_question(id));
        assert!(state.pending_enter.contains_key(&id));
        let tour = state.walkthrough_overlay().unwrap();
        assert_eq!(tour.questions.len(), 1);
        assert_eq!(tour.questions[0].question, "why three?");
        assert!(tour.questions[0].answer.is_none());
        assert!(tour.input.is_none());
        let answered = comms_reply(&mut state, &live_run, "walkthrough_answer", r#"{"answer":"because"}"#);
        assert!(answered.contains(r#""answered":true"#), "answered: {answered}");
        assert_eq!(
            state.walkthrough_overlay().unwrap().questions[0].answer.as_deref(),
            Some("because")
        );
        let ended = comms_reply(&mut state, &live_run, "walkthrough_end", r#"{"summary":"done"}"#);
        assert!(ended.contains(r#""ended":true"#), "ended: {ended}");
        assert!(state.walkthrough_overlay().unwrap().completed);
        std::fs::remove_file(&path).ok();
        assert!(state.manager.remove(id));
    }

    #[test]
    fn walkthrough_start_rejects_bad_calls() {
        let mut state = AppState::new();
        state.term_size = (40, 180);
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::infra::ids::RunId::generate(), "codex",
        ).unwrap();
        let live_run = state.manager.get(id).unwrap().run_id.as_str().to_string();
        let missing = comms_reply(
            &mut state, &live_run, "walkthrough_start",
            r#"{"file":"/no/such/forge-walk-missing.rs","steps":"1:1:x"}"#,
        );
        assert!(missing.contains(r#""ok":false"#), "missing: {missing}");
        assert!(missing.contains("cannot read"), "missing: {missing}");
        assert!(!state.walkthrough_overlay_active());
        let stale = comms_reply(&mut state, "bogus-run", "walkthrough_answer", r#"{"answer":"x"}"#);
        assert!(stale.contains("unknown or stale run ID"), "stale: {stale}");
        let no_tour = comms_reply(&mut state, &live_run, "walkthrough_end", "{}");
        assert!(no_tour.contains("no walkthrough for this session"), "no_tour: {no_tour}");
        assert!(state.manager.remove(id));
    }

    #[test]
    fn walkthrough_add_and_update_steps() {
        let mut state = AppState::new();
        state.term_size = (40, 180);
        std::env::set_var("CODEX_BIN", "cat");
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::infra::ids::RunId::generate(), "codex",
        ).unwrap();
        std::env::remove_var("CODEX_BIN");
        let live_run = state.manager.get(id).unwrap().run_id.as_str().to_string();
        let path = std::env::temp_dir().join(format!("forge-walk-steps-{}", std::process::id()));
        std::fs::write(
            &path,
            (1..=10).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n"),
        ).unwrap();
        let start = format!(
            r#"{{"file":{},"steps":"1:3:first\n5:5:second"}}"#,
            crate::ipc::mcp::escape_json(&path.to_string_lossy()),
        );
        comms_reply(&mut state, &live_run, "walkthrough_start", &start);
        let added = comms_reply(
            &mut state, &live_run, "walkthrough_add_step",
            r#"{"start_line":7,"end_line":8,"explanation":"new"}"#,
        );
        assert!(added.contains(r#""added":true"#), "added: {added}");
        assert!(added.contains(r#""steps":3"#), "added: {added}");
        let first = comms_reply(
            &mut state, &live_run, "walkthrough_add_step",
            r#"{"start_line":9,"end_line":9,"explanation":"top","position":1}"#,
        );
        assert!(first.contains(r#""steps":4"#), "first: {first}");
        let tour = state.walkthrough_overlay().unwrap();
        assert_eq!(tour.steps[0].explanation, "top");
        assert_eq!(tour.index, 1, "insert before current shifts it");
        let wrong_file = comms_reply(
            &mut state, &live_run, "walkthrough_add_step",
            r#"{"start_line":1,"end_line":1,"explanation":"x","file_path":"other.rs"}"#,
        );
        assert!(wrong_file.contains("open tour file only"), "wrong_file: {wrong_file}");
        let updated = comms_reply(
            &mut state, &live_run, "walkthrough_update",
            r#"{"step_index":1,"explanation":"revised"}"#,
        );
        assert!(updated.contains(r#""updated":true"#), "updated: {updated}");
        assert_eq!(state.walkthrough_overlay().unwrap().steps[0].explanation, "revised");
        for (args, needle) in [
            (r#"{"step_index":0}"#, "starts at 1"),
            (r#"{"step_index":99}"#, "no walkthrough step"),
            (r#"{"step_index":1,"start_line":9,"end_line":2}"#, "bad walkthrough range"),
        ] {
            let err = comms_reply(&mut state, &live_run, "walkthrough_update", args);
            assert!(err.contains(needle), "args {args}: {err}");
        }
        std::fs::remove_file(&path).ok();
        assert!(state.manager.remove(id));
    }
}
