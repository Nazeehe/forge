//! AppState dialogs: openers, themes, groups, and permission modes.

use super::*;

impl AppState {
    /// Open the create-session dialog, prefilled from current state.
    pub fn open_create_dialog(&mut self) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let name = self.suggested_session_name();
        let groups = self.broker.group_names();
        self.create_dialog = Some(crate::ui::dialogs::create::CreateDialog::new(&name, &cwd, &groups, self.pill_tabs));
        self.dirty = true;
    }

    /// Open the group management dialog.
    pub fn open_group_dialog(&mut self) {
        self.group_dialog = Some(crate::ui::dialogs::groups::GroupDialog::new());
        self.dirty = true;
    }

    /// Open the first-run setup dialog (all CLIs checked).
    pub fn open_oobe_dialog(&mut self) {
        self.oobe_dialog = Some(crate::ui::dialogs::oobe::OobeDialog::new(self.pill_tabs));
        self.dirty = true;
    }

    /// Ask before quitting. Dirty Writer docs anywhere turn the
    /// Yes/No into Save/Discard/Cancel over the named files.
    pub fn open_quit_confirm(&mut self) {
        let dirty = self.writer_dirty_docs(None);
        self.confirm = Some(crate::ui::dialogs::quit::Confirm::with_dirty(
            crate::ui::dialogs::quit::ConfirmKind::QuitForge,
            self.pill_tabs,
            dirty,
        ));
        self.dirty = true;
    }

    /// Ask before killing the active session (`Ctrl-b x`). Nothing
    /// opens when no session is live; Yes terminates the session that
    /// was active here. A dirty Writer doc in that session turns the
    /// Yes/No into Save/Discard/Cancel over its file.
    pub fn open_kill_confirm(&mut self) {
        if let Some(active) = self.manager.active() {
            let dirty = self.writer_dirty_docs(Some(active));
            self.confirm = Some(crate::ui::dialogs::quit::Confirm::with_dirty(
                crate::ui::dialogs::quit::ConfirmKind::KillSession(active),
                self.pill_tabs,
                dirty,
            ));
            self.dirty = true;
        }
    }

    /// Every theme the picker can offer: builtin `default` first,
    /// then each valid file under [`Self::themes_dir`]. Invalid files
    /// never hide the rest.
    pub fn available_themes(&self) -> Vec<crate::ui::theme::ExternalTheme> {
        match &self.themes_dir {
            Some(dir) => crate::ui::theme::list_external_themes(dir),
            None => vec![crate::ui::theme::ExternalTheme::builtin()],
        }
    }

    /// Open the theme picker over `themes` (builtin `default` first),
    /// preselected on the currently applied theme.
    pub fn open_theme_dialog(&mut self, themes: Vec<crate::ui::theme::ExternalTheme>) {
        let current = crate::ui::theme::active_theme_name();
        self.theme_dialog =
            Some(crate::ui::dialogs::theme::ThemeDialog::new(themes, &current, self.pill_tabs));
        self.dirty = true;
    }

    /// Apply the named theme from `themes` at runtime. `default` (or an
    /// unknown name) restores the builtin look. True when the screen
    /// must repaint.
    pub fn apply_theme_name(
        &mut self,
        name: &str,
        themes: &[crate::ui::theme::ExternalTheme],
    ) -> bool {
        if name == "default" {
            crate::ui::theme::clear_external_theme();
        } else if let Some(theme) = themes.iter().find(|t| t.name == name) {
            crate::ui::theme::apply_external_theme(theme.clone());
        } else {
            return false;
        }
        self.theme_dialog = None;
        self.dirty = true;
        true
    }

    /// Fresh snapshot for the group dialog: groups with live member
    /// counts, sessions in bar order (each with its group names for the
    /// `Add sessions` suffix), and the selected group's current members
    /// for the checkbox pre-check.
    pub fn group_ctx(&self) -> crate::ui::dialogs::groups::GroupCtx {
        let groups = self
            .broker
            .group_names()
            .into_iter()
            .map(|name| crate::ui::dialogs::groups::GroupRow {
                members: self.broker.group_members(&name).len(),
                member_names: self.broker.group_members(&name).into_iter()
                    .filter_map(|id| self.manager.get(id).map(|rec| rec.name.clone()))
                    .collect(),
                color: self.broker.group_color(&name).unwrap_or(0),
                name,
            })
            .collect();
        let sessions = self
            .manager
            .order()
            .to_vec()
            .into_iter()
            .filter_map(|id| {
                self.manager.get(id).map(|rec| crate::ui::dialogs::groups::SessionRow {
                    id,
                    name: rec.name.clone(),
                    groups: self.broker.groups_of(id),
                })
            })
            .collect();
        let selected = self
            .group_dialog
            .as_ref()
            .map(|d| d.selected_name())
            .unwrap_or_default();
        let members = self.broker.group_members(selected);
        crate::ui::dialogs::groups::GroupCtx {
            groups,
            sessions,
            members,
        }
    }

    /// Apply one group-dialog mutation to the broker. The dialog validates
    /// names against a fresh snapshot, so these are infallible in practice;
    /// failures (exit races) stay silent and keep the dialog open.
    /// Unknown session IDs (exited mid-dialog) are skipped.
    pub fn apply_group(&mut self, outcome: crate::ui::dialogs::groups::GroupOutcome) {
        use crate::ui::dialogs::groups::GroupOutcome as Out;
        match outcome {
            Out::Create(name) => {
                let _ = self.broker.create_group(&name);
            }
            Out::Rename { from, to } => {
                let _ = self.broker.rename_group(&from, &to);
            }
            Out::Delete(name) => {
                self.broker.remove_group(&name);
            }
            Out::SetMembers { group, members } => {
                for id in self.manager.order().to_vec() {
                    if members.contains(&id) {
                        let _ = self.broker.join(&self.manager, id, &group);
                    } else {
                        self.broker.leave(id, &group);
                    }
                }
                // Added sessions cluster with their group: starting at the
                // first member, each next one sits directly to its right.
                let clustered = self.broker.group_members(&group);
                self.manager.cluster_sessions(&clustered);
            }
            Out::Pending | Out::Closed => {}
        }
        self.dirty = true;
    }

    /// Set the live permission mode; true when it changed. Non Off/Yolo
    /// modes collapse to Yolo on toggle, never back (toggle only spans
    /// the two sidebar buttons).
    pub fn set_permission_mode(&mut self, mode: crate::infra::config::PermissionMode) -> bool {
        if self.permission_mode == mode {
            return false;
        }
        self.permission_mode = mode;
        self.dirty = true;
        true
    }

    /// Toggle Off <-> Yolo for the keyboard path.
    pub fn toggle_permission_mode(&mut self) -> bool {
        let next = match self.permission_mode {
            crate::infra::config::PermissionMode::Yolo => crate::infra::config::PermissionMode::Off,
            _ => crate::infra::config::PermissionMode::Yolo,
        };
        self.set_permission_mode(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::ids::RunId;

    #[test]
    fn permission_mode_toggle_spans_off_and_yolo() {
        let mut s = AppState::new();
        assert_eq!(s.permission_mode, crate::infra::config::PermissionMode::Yolo);
        s.dirty = false;
        assert!(s.toggle_permission_mode());
        assert_eq!(s.permission_mode, crate::infra::config::PermissionMode::Off);
        assert!(s.dirty);
        assert!(!s.set_permission_mode(crate::infra::config::PermissionMode::Off));
        assert!(s.toggle_permission_mode());
        assert_eq!(s.permission_mode, crate::infra::config::PermissionMode::Yolo);
        // Foreign modes collapse to Yolo, never back through the toggle.
        assert!(s.set_permission_mode(crate::infra::config::PermissionMode::SafeOnly));
        assert!(s.toggle_permission_mode());
        assert_eq!(s.permission_mode, crate::infra::config::PermissionMode::Yolo);
    }

    #[test]
    fn codex_permission_request_allow_skips_prompt_ask_defers() {
        use crate::infra::config::PermissionMode;
        let audit = std::env::temp_dir().join(format!(
            "forge-codex-preq-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&audit);
        let mut s = AppState::new();
        let run = RunId::generate();
        let id = s
            .manager
            .spawn_agent(
                "cx",
                &std::env::temp_dir(),
                "exec sleep 30",
                run.clone(),
                "codex",
            )
            .unwrap();
        // YOLO allow must arrive as the behavior envelope Codex accepts.
        let mut yolo =
            crate::hooks::policy::Policy::new(PermissionMode::Yolo, &[], &[]).unwrap();
        let (allow_tx, allow_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::ipc::listener::HookRequest {
            hook: "PermissionRequest".to_string(),
            body: r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#.to_string(),
            run_id: run.to_string(),
            sync: true,
            reply: allow_tx,
            timed_out: Default::default(),
        }));
        s.settle_hooks(&mut yolo, &audit);
        let line = allow_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(line.trim()).expect("reply is JSON");
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PermissionRequest");
        assert_eq!(v["hookSpecificOutput"]["decision"]["behavior"], "allow");
        // Off ask declines to decide so the native prompt continues.
        let mut off = crate::hooks::policy::Policy::new(PermissionMode::Off, &[], &[]).unwrap();
        let (ask_tx, ask_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::ipc::listener::HookRequest {
            hook: "PermissionRequest".to_string(),
            body: r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#.to_string(),
            run_id: run.to_string(),
            sync: true,
            reply: ask_tx,
            timed_out: Default::default(),
        }));
        s.settle_hooks(&mut off, &audit);
        let line = ask_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(line.is_empty(), "ask defers silently: {line:?}");
        assert!(s.manager.remove(id));
        let _ = std::fs::remove_file(&audit);
    }

    #[test]
    fn group_ctx_carries_session_groups_for_picker_suffix() {
        let mut s = AppState::new();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "other").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        let ctx = s.group_ctx();
        let row_a = ctx.sessions.iter().find(|r| r.id == a).expect("a row");
        let row_b = ctx.sessions.iter().find(|r| r.id == b).expect("b row");
        assert_eq!(row_a.groups, vec!["peers".to_string()]);
        assert!(row_b.groups.contains(&"other".to_string()), "b groups: {:?}", row_b.groups);
        assert!(row_b.groups.contains(&"peers".to_string()), "b groups: {:?}", row_b.groups);
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn adding_to_group_clusters_member_buttons() {
        let mut s = AppState::new();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        let c = s
            .manager
            .spawn("c", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        assert_eq!(s.manager.order(), &[a, b, c]);
        s.apply_group(crate::ui::dialogs::groups::GroupOutcome::SetMembers {
            group: "peers".to_string(),
            members: vec![a, c],
        });
        assert_eq!(s.manager.order(), &[a, c, b], "c clusters right of a");
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
        assert!(s.manager.remove(c));
    }

    #[test]
    fn safe_only_blocks_still_deny_without_a_modal() {
        use crate::infra::config::PermissionMode;
        let audit = std::env::temp_dir().join(format!(
            "forge-block-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&audit);
        let mut policy = crate::hooks::policy::Policy::new(
            PermissionMode::SafeOnly,
            &[],
            &["doom".to_string()],
        )
        .unwrap();
        let mut s = AppState::new();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::ipc::listener::HookRequest {
            hook: "PreToolUse".to_string(),
            body: r#"{"tool_name":"Bash","tool_input":{"command":"doom"}}"#.to_string(),
            run_id: String::new(),
            sync: true,
            reply: reply_tx,
            timed_out: Default::default(),
        }));
        s.settle_hooks(&mut policy, &audit);
        assert!(s.pending_hooks.is_empty());
        let line = reply_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(line.contains(r#""permissionDecision":"deny""#), "line: {line:?}");
        let logged = std::fs::read_to_string(&audit).unwrap();
        assert_eq!(logged.lines().count(), 1, "audit: {logged:?}");
        let _ = std::fs::remove_file(&audit);
    }
}
