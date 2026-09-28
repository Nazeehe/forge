//! AppState sessions: spawn, fleet, focus, terminate, and restore.

use super::*;

impl AppState {
    /// Spawn exactly what a submitted dialog describes: shells run the
    /// login shell, agents run their registry argv. Returns the new id.
    pub fn create_session(
        &mut self,
        spec: &crate::ui::dialogs::create::SessionSpec,
    ) -> std::io::Result<crate::session::SessionId> {
        let home = crate::infra::branding::home_dir();
        let forge_bin = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "forge".to_string());
        self.create_session_with_home(&home, &forge_bin, spec)
    }

    /// [`AppState::create_session`] against an explicit home and forge
    /// binary, so tests heal scratch homes instead of the developer's
    /// real config. Production callers use [`AppState::create_session`].
    fn create_session_with_home(
        &mut self,
        home: &std::path::Path,
        forge_bin: &str,
        spec: &crate::ui::dialogs::create::SessionSpec,
    ) -> std::io::Result<crate::session::SessionId> {
        use crate::ui::dialogs::create::SessionKind;
        let (cmd, cli_tool) = match spec.kind {
            SessionKind::Shell => {
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
                (format!("exec {shell} -i"), "shell".to_string())
            }
            SessionKind::Agent(h) => {
                // A CLI installed after forge (or with wiped config) is
                // set up here, before its first launch, so the user never
                // has to run install-hooks by hand. Fail-open like the
                // runtime materialization below: setup problems never
                // block the session.
                let _ = crate::hooks::install::ensure_installed(home, h.as_str(), forge_bin);
                let hs = h.spec();
                let model = if spec.model.is_empty() {
                    None
                } else {
                    Some(spec.model.as_str())
                };
                let binary = hs.resolve_binary();
                // Fresh launches carry the Forge runtime contract through
                // the agent's strongest injection mechanism. Materialization
                // fails open: a session without injection still launches.
                let argv = match crate::agents::runtime::ensure_materialized(home) {
                    Ok(file) => h.launch_argv_with_runtime(&binary, model, &file),
                    Err(_) => hs.launch_argv(&binary, model),
                };
                let mut cmd = String::from("exec ");
                cmd.push_str(&crate::ui::dialogs::create::shell_join(&argv));
                (cmd, h.as_str().to_string())
            }
        };
        // Agent CLIs get the dual-tab layout (agent on tab 0, a lazy
        // human terminal on tab 1); plain shells stay single-tab.
        let run = crate::infra::ids::RunId::generate();
        let id = match spec.kind {
            SessionKind::Agent(_) => {
                self.manager
                    .spawn_agent(&spec.name, &spec.cwd, &cmd, run, &cli_tool)
            }
            SessionKind::Shell => {
                self.manager
                    .spawn(&spec.name, &spec.cwd, &cmd, run, &cli_tool)
            }
        }?;
        // The dialog's comm-group choice joins at birth (a group deleted
        // mid-dialog is recreated by the join — the user explicitly picked
        // it a moment ago).
        if let Some(group) = spec.group.as_deref() {
            let _ = self.broker.join(&self.manager, id, group);
            let clustered = self.broker.group_members(group);
            self.manager.cluster_sessions(&clustered);
        }
        // Creating focuses the new session: the user just asked for it,
        // and the caller fits the active pane to the dialog's area.
        self.manager.switch(id);
        Ok(id)
    }

    /// Live agent sessions as restorable records, in bar order. Shells
    /// have no resume form and exited sessions are gone, so both are
    /// left out; groups ride along for exact rejoins.
    pub fn snapshot_sessions(&self) -> Vec<crate::session::checkpoint::SavedSession> {
        self.manager
            .order()
            .to_vec()
            .into_iter()
            .filter_map(|id| {
                let rec = self.manager.get(id)?;
                if !rec.state.is_live() {
                    return None;
                }
                if crate::agents::harness::Harness::from_name(&rec.cli_tool).is_none() {
                    return None;
                }
                Some(crate::session::checkpoint::SavedSession {
                    name: rec.name.clone(),
                    cli_tool: rec.cli_tool.clone(),
                    cwd: rec.cwd.to_string_lossy().into_owned(),
                    groups: self.broker.groups_of(id),
                    harness_session_id: rec.harness_session_id.clone(),
                })
            })
            .collect()
    }

    /// Recreate every restorable session in an entry: agents relaunch
    /// with resume argv (or the cwd-scoped fallback), rejoin their
    /// groups, and fit the main pane. Unknown tools and vanished working
    /// directories skip with a reason instead of failing the batch.
    pub fn restore_entry(&mut self, entry: &crate::session::checkpoint::SavedEntry) -> RestoreReport {
        let home = crate::infra::branding::home_dir();
        let forge_bin = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "forge".to_string());
        self.restore_entry_with_home(&home, &forge_bin, entry)
    }

    /// [`AppState::restore_entry`] against an explicit home and forge
    /// binary. Same seam as [`AppState::create_session_with_home`].
    fn restore_entry_with_home(
        &mut self,
        home: &std::path::Path,
        forge_bin: &str,
        entry: &crate::session::checkpoint::SavedEntry,
    ) -> RestoreReport {
        let mut report = RestoreReport {
            spawned: 0,
            skipped: Vec::new(),
        };
        for saved in &entry.sessions {
            let Some(harness) = crate::agents::harness::Harness::from_name(&saved.cli_tool) else {
                report.skipped.push(format!("{}: unknown tool {}", saved.name, saved.cli_tool));
                continue;
            };
            let cwd = std::path::PathBuf::from(&saved.cwd);
            if !cwd.is_dir() {
                report.skipped.push(format!("{}: missing directory {}", saved.name, saved.cwd));
                continue;
            }
            // Same create-time repair as fresh sessions: a restored agent
            // whose CLI arrived after forge still needs hooks/MCP before
            // its resume argv runs. Fail-open: never blocks the restore.
            let _ = crate::hooks::install::ensure_installed(home, &saved.cli_tool, forge_bin);
            let spec = harness.spec();
            let argv = harness.resume_argv(&spec.resolve_binary(), saved.harness_session_id.as_deref());
            let mut cmd = String::from("exec ");
            cmd.push_str(&crate::ui::dialogs::create::shell_join(&argv));
            let run = crate::infra::ids::RunId::generate();
            match self.manager.spawn_agent(&saved.name, &cwd, &cmd, run, &saved.cli_tool) {
                Ok(id) => {
                    // The save file knows the resume ID the argv above
                    // just reused: stamp it onto the fresh record. A
                    // resumed muse session fires no SessionStart, so
                    // without this it would re-save as null forever.
                    if let Some(harness) =
                        saved.harness_session_id.as_deref().filter(|s| !s.is_empty())
                    {
                        self.manager.set_harness_session(id, harness.to_string());
                    }
                    for group in &saved.groups {
                        let _ = self.broker.join(&self.manager, id, group);
                        let clustered = self.broker.group_members(group);
                        self.manager.cluster_sessions(&clustered);
                    }
                    report.spawned += 1;
                }
                Err(e) => report.skipped.push(format!("{}: spawn failed ({e})", saved.name)),
            }
        }
        self.dirty = true;
        report
    }

    /// Trace every session's attribution state (hooks.log): what a save
    /// would write, plus the PTY process-session ID hooks must match.
    pub fn trace_snapshot(&self, reason: &str) {
        let Some(path) = self.hook_trace.as_deref() else {
            return;
        };
        let sessions: Vec<String> = self
            .manager
            .order()
            .iter()
            .filter_map(|id| {
                let rec = self.manager.get(*id)?;
                Some(format!(
                    "{id}[name={} tool={} live={} sid={} harness={}]",
                    rec.name,
                    rec.cli_tool,
                    rec.state.is_live(),
                    self.manager
                        .process_session_id(*id)
                        .map_or_else(|| "-".to_string(), |sid| sid.to_string()),
                    rec.harness_session_id.as_deref().unwrap_or("-"),
                ))
            })
            .collect();
        crate::infra::logging::hook_trace(path, &format!("tui snapshot {reason}: {}", sessions.join(" ")));
    }

    /// Append one line to the comms trace, when on. Best-effort:
    /// tracing never fails or blocks the send or delivery it describes.
    pub(super) fn trace_comms(&self, line: &str) {
        if let Some(path) = self.comms_trace.as_deref() {
            crate::infra::logging::comms_trace(path, line);
        }
    }

    /// Caller display for the comms trace: `caller="name"` when the run
    /// ID resolves to a live session, `caller=unknown-run` otherwise
    /// (stale or forged run IDs fail exactly here).
    pub(super) fn comms_caller(&self, run_id: &str) -> String {
        match self
            .manager
            .lookup_run(run_id)
            .and_then(|id| self.manager.get(id))
        {
            Some(rec) => format!("caller={}", crate::comms::log_quote(&rec.name)),
            None => "caller=unknown-run".to_string(),
        }
    }

    /// Routing facts for the args' named target: liveness, shared group
    /// with the caller, pressure, and queue depth. Empty when the call
    /// names no session target (responses, acks, self-schedules); a bot
    /// peer reads `target_live=false` since it owns no pane.
    fn comms_target_state(
        &self,
        caller: Option<crate::session::SessionId>,
        args: &str,
    ) -> String {
        let Some(target_name) =
            crate::hooks::policy::json_string_field(args.as_bytes(), &["target"])
                .filter(|s| !s.is_empty())
        else {
            return String::new();
        };
        let quoted = crate::comms::log_quote(&target_name);
        let mut live_id = None;
        let mut ambiguous = false;
        for &id in self.manager.order() {
            let live = self.manager.get(id).is_some_and(|rec| {
                rec.name == target_name && rec.state.is_live()
            });
            if live {
                if live_id.is_some() {
                    ambiguous = true;
                }
                live_id = Some(id);
            }
        }
        if ambiguous {
            return format!(" to={quoted} target_live=ambiguous");
        }
        let Some(tid) = live_id else {
            return format!(" to={quoted} target_live=false");
        };
        let shared = caller.is_some_and(|c| self.broker.shares_group(c, tid));
        let pressure = self.broker.pressure(&self.manager, tid);
        let queued = self.broker.queued(tid);
        format!(" to={quoted} target_live=true shared_group={shared} pressure={pressure} queued={queued}")
    }

    /// One trace line per comms tool verdict: the call summary plus the
    /// outcome and the routing facts that explain it. Read-only:
    /// tracing never changes the verdict it describes.
    pub(super) fn trace_comms_verdict(
        &self,
        tool: &str,
        run_id: &str,
        args: &str,
        verdict: &Result<String, String>,
    ) {
        if self.comms_trace.is_none() {
            return;
        }
        let caller_id = self.manager.lookup_run(run_id);
        let caller = self.comms_caller(run_id);
        let summary = crate::comms::summarize_call(tool, args);
        let target_state = self.comms_target_state(caller_id, args);
        let line = match verdict {
            Ok(result) => {
                let conv =
                    crate::hooks::policy::json_string_field(result.as_bytes(), &["conversation", "timer_id"])
                        .map(|c| format!(" conv={}", crate::comms::log_quote(&c)))
                        .unwrap_or_default();
                format!("comms tool={tool} {caller} {summary} -> ok{conv}{target_state}")
            }
            Err(e) => {
                let reason = crate::infra::logging::truncate(e, 200);
                let caller_queued = match caller_id {
                    Some(id) => format!(" caller_queued={}", self.broker.queued(id)),
                    None => String::new(),
                };
                format!(
                    "comms tool={tool} {caller} {summary} -> err reason={}{caller_queued}{target_state}",
                    crate::comms::log_quote(&reason)
                )
            }
        };
        self.trace_comms(&line);
    }

    /// ms since `at`, saturating clock skew to zero (a future stamp
    /// reads as just-active). `None` when no record exists.
    pub(super) fn ms_ago(now: std::time::Instant, at: Option<&std::time::Instant>) -> Option<u64> {
        at.map(|t| {
            now.checked_duration_since(*t)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0)
        })
    }

    /// Cycle the active session; wraps around. No-op when empty.
    pub fn step_session(&mut self, dir: i32) {
        let order = self.manager.order().to_vec();
        if order.is_empty() {
            return;
        }
        let cur = self
            .manager
            .active()
            .and_then(|a| order.iter().position(|&id| id == a))
            .unwrap_or(0) as i32;
        let next = (cur + dir).rem_euclid(order.len() as i32) as usize;
        self.manager.switch(order[next]);
        self.overlay_view = None;
    }

    /// Focus session by order index (`Ctrl-b 1` is index 0). Returns false
    /// when out of range, leaving focus untouched.
    pub fn select_session(&mut self, index: usize) -> bool {
        match self.bar_order().get(index) {
            Some(&id) => self.focus_session(id),
            None => false,
        }
    }

    /// Session-bar order: ungrouped sessions left, grouped sessions
    /// after, each side keeping manager order. The bar, its digits,
    /// and bar clicks all resolve through here, so the numbers never
    /// lie about what they select.
    pub(super) fn bar_order(&self) -> Vec<crate::session::SessionId> {
        let mut ungrouped = Vec::new();
        let mut grouped = Vec::new();
        for id in self.manager.order().to_vec() {
            if self.broker.primary_group(id).is_none() {
                ungrouped.push(id);
            } else {
                grouped.push(id);
            }
        }
        ungrouped.extend(grouped);
        ungrouped
    }

    /// Focus a session by id: switch, clear its unread pings (attention
    /// flag and badge entry; the badged id stays for reply routing),
    /// and return to the focused view. False when unknown.
    pub fn focus_session(&mut self, id: crate::session::SessionId) -> bool {
        if !self.manager.order().contains(&id) {
            return false;
        }
        self.manager.switch(id);
        self.overlay_view = None;
        self.attention_flags.remove(&id);
        self.message_user_badges.remove(&id);
        self.grid_mode = false;
        self.dirty = true;
        true
    }

    /// Fleet rows in router order: attention first, then spawn order
    /// within each tier. Hook activity never reshuffles peers — marks
    /// update in place so spatial memory survives busy sessions.
    /// Shared by the sidebar, cursor movement, and activation so all
    /// three agree.
    pub(super) fn sorted_fleet_ids(&self) -> Vec<crate::session::SessionId> {
        let active = self.manager.active();
        let mut rows: Vec<(u8, usize, crate::session::SessionId)> = Vec::new();
        for (idx, id) in self.manager.order().to_vec().iter().enumerate() {
            let Some(rec) = self.manager.get(*id) else {
                continue;
            };
            let tier = self.fleet_tier(*id, rec, active);
            rows.push((tier, idx, *id));
        }
        rows.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        rows.into_iter().map(|(_, _, id)| id).collect()
    }

    /// Attention tier for one session: explicit pings and waiting beat
    /// working, which beats idle. Lower sorts first.
    pub(super) fn fleet_tier(
        &self,
        id: crate::session::SessionId,
        rec: &crate::session::SessionRecord,
        active: Option<crate::session::SessionId>,
    ) -> u8 {
        if self.attention_flags.contains_key(&id) {
            return 0;
        }
        if active != Some(id) && self.message_user_badges.contains_key(&id) {
            return 0;
        }
        match rec.activity {
            crate::session::Activity::Waiting => 0,
            crate::session::Activity::ToolUse | crate::session::Activity::Thinking => 1,
            _ => match &rec.status {
                Some(s)
                    if matches!(
                        s.kind,
                        crate::session::status::StatusKind::Blocked
                            | crate::session::status::StatusKind::Question
                    ) =>
                {
                    0
                }
                _ => 2,
            },
        }
    }

    /// Move the fleet cursor, pulling the scroll offset so the cursor
    /// stays visible under the live sidebar breakpoint.
    pub fn fleet_step(&mut self, dir: i32) {
        let ids = self.sorted_fleet_ids();
        if ids.is_empty() {
            return;
        }
        let cur = self
            .fleet_cursor
            .and_then(|c| ids.iter().position(|id| *id == c));
        let next = match cur {
            Some(i) => (i as i32 + dir).clamp(0, ids.len() as i32 - 1) as usize,
            None => {
                if dir >= 0 {
                    0
                } else {
                    ids.len() - 1
                }
            }
        };
        self.fleet_cursor = Some(ids[next]);
        // Pull the scroll offset until the cursor sits inside the live
        // grouped window, sharing the paint's exact math.
        let (rows, cols) = self.term_size;
        let sidebar =
            crate::ui::layout::chrome_areas(ratatui::layout::Rect::new(0, 0, cols, rows)).sidebar;
        let rich = crate::ui::sidebar::sidebar_is_rich(sidebar);
        for _ in 0..=ids.len() {
            let info = self.sidebar_info();
            let layout = crate::ui::sidebar::sidebar_layout(
                sidebar,
                crate::ui::sidebar::sidebar_footer_height(&info, rich),
            );
            let budget = crate::ui::sidebar::fleet::fleet_items_budget(
                &info,
                rich,
                sidebar.width,
                layout.list.height,
            );
            let window = crate::ui::sidebar::fleet::fleet_window_items(&info, sidebar.width, budget);
            let cursor_block = self.fleet_cursor.and_then(|c| {
                info.sessions
                    .iter()
                    .position(|row| row.id == c)
                    .map(crate::ui::sidebar::fleet::FleetItem::Block)
            });
            match cursor_block {
                Some(block) if window.contains(&block) => break,
                Some(crate::ui::sidebar::fleet::FleetItem::Block(p)) if p < self.fleet_scroll => {
                    self.fleet_scroll = p;
                }
                Some(_) => {
                    self.fleet_scroll =
                        (self.fleet_scroll + 1).min(ids.len().saturating_sub(1));
                }
                None => break,
            }
        }
        self.dirty = true;
    }

    /// Activate the fleet cursor (or the focused session when unset).
    pub fn fleet_activate(&mut self) -> bool {
        let target = self.fleet_cursor.or_else(|| self.manager.active());
        match target {
            Some(id) => self.focus_session(id),
            None => false,
        }
    }

    /// Terminate a session (`Ctrl-b x`): leave every comm group, fail its
    /// open conversations and timers, then drop the record so it leaves
    /// the UI. Focus falls to the first remaining session. False when the
    /// id is unknown.
    pub fn terminate_session(&mut self, id: crate::session::SessionId) -> bool {
        self.broker.leave_all(id);
        self.broker.target_exited(&self.manager, id);
        if !self.manager.remove(id) {
            return false;
        }
        // Same cleanup as a natural exit: debounce entries die with
        // the session, or the maps grow with every termination.
        self.last_human_input.remove(&id);
        self.last_hook_activity.remove(&id);
        self.attention_flags.remove(&id);
        self.overlay_view = None;
        self.dirty = true;
        true
    }

    /// Record human typing into one session: injections debounce until it
    /// settles, and any staged Enter for that session is dropped — the
    /// human owns the prompt now, and our CR must never submit their
    /// half-typed draft.
    pub fn note_human_input(&mut self, id: crate::session::SessionId) {
        self.last_human_input
            .insert(id, std::time::Instant::now());
        // Dropping the staged submit protects the human's draft, but
        // the body is already in the prompt: it may mix or be
        // discarded, so its sender is told loudly (walkthrough
        // prompts have no sender and stay silent).
        if let Some((_, staged)) = self.pending_enter.remove(&id) {
            if let Some((conv, kind)) = staged {
                let typer = self
                    .manager
                    .get(id)
                    .map(|rec| rec.name.clone())
                    .unwrap_or_else(|| id.to_string());
                self.broker.clobber_notice(id, &typer, &conv, kind);
            }
        }
    }

    /// Live session names for dialog validation.
    pub fn live_names(&self) -> Vec<String> {
        self.manager
            .order()
            .iter()
            .filter_map(|&id| self.manager.get(id))
            .filter(|rec| rec.state.is_live())
            .map(|rec| rec.name.clone())
            .collect()
    }

    /// First free `<default-agent>-N` name for the dialog prefill.
    /// Prefill name for the create dialog. The dialog defaults to the
    /// first registry tool, so the prefix matches; the user can rename freely.
    pub fn suggested_session_name(&self) -> String {
        let prefix = crate::agents::registry::registry()
            .first()
            .map(|def| def.name.as_str())
            .unwrap_or("agent");
        let taken = self.live_names();
        let mut n = self.manager.len() + 1;
        loop {
            let candidate = format!("{prefix}-{n}");
            if !taken.iter().any(|t| t == &candidate) {
                return candidate;
            }
            n += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::*;
    use crate::infra::ids::RunId;
    use crate::session::SessionId;

    #[test]
    fn session_start_captures_harness_id() {
        let mut s = AppState::new();
        let run = RunId::generate();
        let id = s
            .manager
            .spawn_agent("a", &std::env::temp_dir(), "exec sleep 30", run.clone(), "claude")
            .unwrap();
        assert_eq!(s.manager.get(id).unwrap().harness_session_id, None);
        // Claude envelope nests the harness JSON under body.
        s.apply(hook_request(
            "SessionStart",
            run.as_str(),
            r#"{"v":1,"hook":"SessionStart","run_id":"r","body":{"session_id":"harness-9","cwd":"/tmp"}}"#,
        ));
        assert_eq!(
            s.manager.get(id).unwrap().harness_session_id.as_deref(),
            Some("harness-9")
        );
        // Bodies without an ID (or other hooks) leave the value alone.
        s.apply(hook_request("SessionStart", run.as_str(), "{}"));
        s.apply(hook_request("PreToolUse", run.as_str(), "{}"));
        assert_eq!(
            s.manager.get(id).unwrap().harness_session_id.as_deref(),
            Some("harness-9")
        );
        // Unknown runs touch nothing.
        s.apply(hook_request(
            "SessionStart",
            "nope",
            r#"{"v":1,"body":{"session_id":"other"}}"#,
        ));
        assert!(s.manager.remove(id));
    }

    #[test]
    fn snapshot_keeps_live_agents_with_groups() {
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn_agent("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "claude")
            .unwrap();
        s.manager.set_harness_session(a, "h-1".to_string());
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, a, "team").unwrap();
        // Shells and exited sessions never snapshot.
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("sh", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        let snap = s.snapshot_sessions();
        assert_eq!(snap.len(), 1, "agent only: {snap:?}");
        assert_eq!(snap[0].name, "a");
        assert_eq!(snap[0].cli_tool, "claude");
        assert_eq!(snap[0].groups, vec!["peers".to_string(), "team".to_string()]);
        assert_eq!(snap[0].harness_session_id.as_deref(), Some("h-1"));
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn restore_respawns_with_resume_and_rejoins() {
        // Hermetic stand-in binary; the record outlives its instant exit.
        let saved = std::env::var("CODEX_BIN").ok();
        std::env::set_var("CODEX_BIN", "/bin/true");
        let mut s = AppState::new();
        let entry = crate::session::checkpoint::SavedEntry {
            label: "a, gone, weird".to_string(),
            saved_at_unix: 1_700_000_000,
            sessions: vec![
                crate::session::checkpoint::SavedSession {
                    name: "a".to_string(),
                    cli_tool: "codex".to_string(),
                    cwd: std::env::temp_dir().to_string_lossy().into_owned(),
                    groups: vec!["peers".to_string()],
                    harness_session_id: Some("uuid-a".to_string()),
                },
                crate::session::checkpoint::SavedSession {
                    name: "gone".to_string(),
                    cli_tool: "codex".to_string(),
                    cwd: "/no/such/dir-anywhere".to_string(),
                    groups: vec![],
                    harness_session_id: None,
                },
                crate::session::checkpoint::SavedSession {
                    name: "weird".to_string(),
                    cli_tool: "shell".to_string(),
                    cwd: std::env::temp_dir().to_string_lossy().into_owned(),
                    groups: vec![],
                    harness_session_id: None,
                },
            ],
        };
        let report = s.restore_entry(&entry);
        assert_eq!(report.spawned, 1, "report: {:?}", report.skipped);
        assert_eq!(report.skipped.len(), 2, "missing dir + shell: {:?}", report.skipped);
        let id = s.manager.order().to_vec().pop().unwrap();
        assert_eq!(s.manager.get(id).unwrap().name, "a");
        assert!(s.broker.is_member(id, "peers"), "rejoined");
        match saved {
            Some(v) => std::env::set_var("CODEX_BIN", v),
            None => std::env::remove_var("CODEX_BIN"),
        }
        assert!(s.manager.remove(id));
    }

    #[test]
    fn restore_stamps_the_saved_harness_id() {
        // The save file already knows the resume ID and the resume argv
        // uses it — but the fresh record dropped it, so a resumed muse
        // session (which fires no SessionStart) re-saved as null.
        let saved = std::env::var("METAMATE_BIN").ok();
        std::env::set_var("METAMATE_BIN", "/bin/true");
        let mut s = AppState::new();
        let entry = crate::session::checkpoint::SavedEntry {
            label: "m".to_string(),
            saved_at_unix: 1_700_000_000,
            sessions: vec![crate::session::checkpoint::SavedSession {
                name: "m".to_string(),
                cli_tool: "muse".to_string(),
                cwd: std::env::temp_dir().to_string_lossy().into_owned(),
                groups: vec![],
                harness_session_id: Some("muse-saved".to_string()),
            }],
        };
        let report = s.restore_entry(&entry);
        assert_eq!(report.spawned, 1, "report: {:?}", report.skipped);
        let id = s.manager.order().to_vec().pop().unwrap();
        assert_eq!(
            s.manager.get(id).unwrap().harness_session_id.as_deref(),
            Some("muse-saved"),
            "restored record keeps its resume ID"
        );
        match saved {
            Some(v) => std::env::set_var("METAMATE_BIN", v),
            None => std::env::remove_var("METAMATE_BIN"),
        }
        assert!(s.manager.remove(id));
    }

    #[test]
    fn create_session_spawns_named_shell() {
        let mut s = AppState::new();
        let spec = crate::ui::dialogs::create::SessionSpec {
            kind: crate::ui::dialogs::create::SessionKind::Shell,
            name: "work".to_string(),
            cwd: std::env::temp_dir(),
            model: String::new(),
            group: None,
        };
        let id = s.create_session(&spec).unwrap();
        let rec = s.manager.get(id).unwrap();
        assert_eq!(rec.name, "work");
        assert_eq!(rec.cli_tool, "shell");
        assert_eq!(rec.cwd, std::env::temp_dir());
        assert!(s.manager.remove(id));
    }

    #[test]
    fn create_session_focuses_the_new_session() {
        let mut s = AppState::new();
        let spec = |name: &str| crate::ui::dialogs::create::SessionSpec {
            kind: crate::ui::dialogs::create::SessionKind::Shell,
            name: name.to_string(),
            cwd: std::env::temp_dir(),
            model: String::new(),
            group: None,
        };
        let first = s.create_session(&spec("a")).unwrap();
        assert_eq!(s.manager.active(), Some(first));
        let second = s.create_session(&spec("b")).unwrap();
        assert_eq!(s.manager.active(), Some(second), "creating focuses the new one");
        assert!(s.manager.remove(first));
        assert!(s.manager.remove(second));
    }

    #[test]
    fn create_session_with_group_joins_at_birth() {
        let mut s = AppState::new();
        s.broker.create_group("team").unwrap();
        let id = s
            .create_session(&crate::ui::dialogs::create::SessionSpec {
                kind: crate::ui::dialogs::create::SessionKind::Shell,
                name: "work".to_string(),
                cwd: std::env::temp_dir(),
                model: String::new(),
                group: Some("team".to_string()),
            })
            .unwrap();
        assert!(s.broker.is_member(id, "team"));
        assert_eq!(s.tabs().iter().find(|t| t.title == "work").and_then(|t| t.group.clone()), Some("team".to_string()), "bar reflects it");
        assert!(s.manager.remove(id));
    }

    #[test]
    fn create_session_agent_records_cli_tool() {
        // Hermetic: stand in for the codex binary; argv shape is locked in
        // the harness registry tests.
        let saved = std::env::var("CODEX_BIN").ok();
        std::env::set_var("CODEX_BIN", "/bin/true");
        let mut s = AppState::new();
        let spec = crate::ui::dialogs::create::SessionSpec {
            kind: crate::ui::dialogs::create::SessionKind::Agent(
                crate::agents::harness::Harness::from_name("codex").unwrap(),
            ),
            name: "coder".to_string(),
            cwd: std::env::temp_dir(),
            model: "gpt-5".to_string(),
            group: None,
        };
        let id = s.create_session(&spec).unwrap();
        let rec = s.manager.get(id).unwrap();
        assert_eq!(rec.cli_tool, "codex");
        assert!(s.manager.remove(id));
        match saved {
            Some(v) => std::env::set_var("CODEX_BIN", v),
            None => std::env::remove_var("CODEX_BIN"),
        }
    }

    #[test]
    fn create_session_routes_agent_to_dual_tabs() {
        // Point the codex binary at `cat` so the test never depends on a
        // real agent CLI being installed; no other test reads this var.
        std::env::set_var("CODEX_BIN", "cat");
        let mut s = AppState::new();
        let agent = s
            .create_session(&crate::ui::dialogs::create::SessionSpec {
                kind: crate::ui::dialogs::create::SessionKind::Agent(
                    crate::agents::harness::Harness::from_name("codex").unwrap(),
                ),
                name: "codex-1".to_string(),
                cwd: std::env::temp_dir(),
                model: String::new(),
                group: None,
            })
            .unwrap();
        std::env::remove_var("CODEX_BIN");
        assert_eq!(s.manager.tab_count(agent), 3);
        assert!(s.manager.switch_tab(agent));
        let shell = s
            .create_session(&crate::ui::dialogs::create::SessionSpec {
                kind: crate::ui::dialogs::create::SessionKind::Shell,
                name: "shell-1".to_string(),
                cwd: std::env::temp_dir(),
                model: String::new(),
                group: None,
            })
            .unwrap();
        assert_eq!(s.manager.tab_count(shell), 1);
        assert!(s.manager.remove(agent));
        assert!(s.manager.remove(shell));
    }

    #[test]
    fn suggested_name_skips_taken_names() {
        let mut s = AppState::new();
        assert_eq!(s.suggested_session_name(), "claude-1");
        s.open_create_dialog();
        assert!(s.create_dialog.is_some());
        let id = s
            .manager
            .spawn("claude-1", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        assert_eq!(s.suggested_session_name(), "claude-2");
        assert!(s.manager.remove(id));
    }

    #[test]
    fn session_traffic_dirties() {
        let mut s = AppState::new();
        s.dirty = false;
        let id = SessionId::fresh();
        s.apply(AppEvent::SessionOutput { id });
        assert!(s.dirty);
        s.dirty = false;
        s.apply(AppEvent::SessionExited { id, code: Some(0) });
        assert!(s.dirty);
    }

    #[test]
    fn resize_records_and_dirties() {
        let mut s = AppState::new();
        s.dirty = false;
        s.apply(AppEvent::Resize(40, 120));
        assert_eq!(s.term_size, (40, 120));
        assert!(s.dirty);
    }

    #[test]
    fn select_session_focuses_by_index() {
        let mut s = AppState::new();
        assert!(!s.select_session(0), "empty: no-op");
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        assert!(s.select_session(1));
        assert_eq!(s.manager.active(), Some(b));
        assert!(s.select_session(0));
        assert_eq!(s.manager.active(), Some(a));
        assert!(!s.select_session(9), "out of range keeps focus");
        assert_eq!(s.manager.active(), Some(a));
        let tabs = s.tabs();
        assert_eq!(tabs.len(), 2);
        assert!(tabs[0].focused && !tabs[1].focused);
        assert_eq!(s.sidebar_info().pending, 0);
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn bar_keeps_ungrouped_sessions_left_of_grouped() {
        let mut s = AppState::new();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "team").unwrap();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        // Manager order is [a, b], but the bar shows ungrouped first.
        let titles: Vec<_> = s.tabs().iter().map(|t| t.title.clone()).collect();
        assert_eq!(titles, vec!["b".to_string(), "a".to_string()]);
        // Digits follow the bar, not the manager order.
        assert!(s.select_session(0));
        assert_eq!(s.manager.active(), Some(b));
        assert!(s.select_session(1));
        assert_eq!(s.manager.active(), Some(a));
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn terminate_session_drops_ui_and_groups() {
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
        s.broker.join(&s.manager, a, "other").unwrap();
        assert!(s.terminate_session(a));
        assert!(s.manager.get(a).is_none(), "record gone");
        assert_eq!(s.manager.order().len(), 1);
        assert!(!s.broker.is_member(a, "peers"), "left peers");
        assert!(!s.broker.is_member(a, "other"), "left other");
        assert_eq!(s.manager.active(), Some(b), "focus falls through");
        assert!(!s.terminate_session(a), "unknown id is a no-op");
        assert!(s.manager.remove(b));
    }

    #[test]
    fn terminate_session_prunes_debounce_entries() {
        // Manual termination must clean the per-target debounce maps
        // like a natural exit does, or they grow with every session.
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        s.note_human_input(a);
        s.last_hook_activity.insert(a, std::time::Instant::now());
        assert!(s.terminate_session(a));
        assert!(!s.last_human_input.contains_key(&a), "typing entry pruned");
        assert!(!s.last_hook_activity.contains_key(&a), "hook entry pruned");
    }

    #[test]
    fn fleet_lists_attention_first() {
        let mut s = AppState::new();
        let a = s
            .manager
            .spawn(
                "aaa",
                &std::env::temp_dir(),
                "exec sleep 30",
                RunId::generate(),
                "shell",
            )
            .unwrap();
        let b = s
            .manager
            .spawn(
                "bbb",
                &std::env::temp_dir(),
                "exec sleep 30",
                RunId::generate(),
                "shell",
            )
            .unwrap();
        // Idle order follows spawn order.
        let ids: Vec<_> = s.sidebar_info().sessions.iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![a, b], "spawn order: {ids:?}");
        // Background attention jumps to the top.
        let run_b = s.manager.get(b).unwrap().run_id.as_str().to_string();
        let raised = comms_reply(
            &mut s,
            &run_b,
            "request_attention",
            r#"{"reason":"need a decision"}"#,
        );
        assert!(raised.contains(r#""ok":true"#), "raise: {raised}");
        let ids: Vec<_> = s.sidebar_info().sessions.iter().map(|r| r.id).collect();
        assert_eq!(ids[0], b, "attention first: {ids:?}");
        assert!(
            s.sidebar_info().sessions[0].reason.contains("need a decision"),
            "reason rides along"
        );
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn focusing_clears_badge_entry_keeps_reply_target() {
        let (mut state, id, live_run) = message_user_agent();
        let ok = comms_reply(
            &mut state,
            &live_run,
            "message_user",
            r#"{"message":"hello operator"}"#,
        );
        assert!(ok.contains(r#""ok":true"#), "ok: {ok}");
        assert!(state.message_user_badges.contains_key(&id));
        let order = state.manager.order().to_vec();
        let idx = order.iter().position(|s| *s == id).unwrap();
        assert!(state.select_session(idx), "focus it");
        assert!(
            !state.message_user_badges.contains_key(&id),
            "seen badge clears"
        );
        assert_eq!(
            state.last_telegram_badged,
            Some(id),
            "bare-text replies still route"
        );
        assert!(state.manager.remove(id));
    }

    #[test]
    fn fleet_order_ignores_hook_recency() {
        let mut s = AppState::new();
        let mut ids = Vec::new();
        for name in ["aaa", "bbb", "ccc"] {
            ids.push(
                s.manager
                    .spawn(
                        name,
                        &std::env::temp_dir(),
                        "exec sleep 30",
                        RunId::generate(),
                        "shell",
                    )
                    .unwrap(),
            );
        }
        // Late hook activity on the last session must not reshuffle
        // idle peers: spatial memory beats recency.
        s.last_hook_activity
            .insert(ids[2], std::time::Instant::now());
        let order: Vec<_> = s.sidebar_info().sessions.iter().map(|r| r.id).collect();
        assert_eq!(order, ids, "spawn order holds");
        for id in ids {
            assert!(s.manager.remove(id));
        }
    }

    #[test]
    fn fleet_cursor_survives_resort_and_activates() {
        let mut s = AppState::new();
        let a = s
            .manager
            .spawn(
                "aaa",
                &std::env::temp_dir(),
                "exec sleep 30",
                RunId::generate(),
                "shell",
            )
            .unwrap();
        let b = s
            .manager
            .spawn(
                "bbb",
                &std::env::temp_dir(),
                "exec sleep 30",
                RunId::generate(),
                "shell",
            )
            .unwrap();
        s.fleet_step(1);
        s.fleet_step(1);
        assert_eq!(s.fleet_cursor, Some(b), "two steps reach b");
        // Resorting under the cursor keeps it by id, not position.
        let run_a = s.manager.get(a).unwrap().run_id.as_str().to_string();
        let raised = comms_reply(
            &mut s,
            &run_a,
            "request_attention",
            r#"{"reason":"need a decision"}"#,
        );
        assert!(raised.contains(r#""ok":true"#), "raise: {raised}");
        assert_eq!(s.fleet_cursor, Some(b), "cursor stable across resort");
        assert!(s.fleet_activate(), "activate focuses cursor");
        assert_eq!(s.manager.active(), Some(b));
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn step_is_safe_when_empty() {
        let mut s = AppState::new();
        s.step_session(1);
        assert!(s.manager.active().is_none());
    }

    #[test]
    fn manager_spawn_flows_through_state() {
        let mut s = AppState::new();
        let id = s
            .manager
            .spawn("w", &std::env::temp_dir(), "exit 0", RunId::generate(), "shell")
            .unwrap();
        assert!(s.manager.get(id).is_some());
        assert!(s.manager.remove(id));
    }

    fn scratch_home(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "forge-create-heal-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn muse_spec(name: &str) -> crate::ui::dialogs::create::SessionSpec {
        crate::ui::dialogs::create::SessionSpec {
            kind: crate::ui::dialogs::create::SessionKind::Agent(
                crate::agents::harness::Harness::from_name("muse").unwrap(),
            ),
            name: name.to_string(),
            cwd: std::env::temp_dir(),
            model: String::new(),
            group: None,
        }
    }

    #[test]
    fn create_session_heals_missing_agent_setup_before_launch() {
        // A CLI installed after forge (no hooks/MCP anywhere) is repaired
        // at create time: the session launches AND the config is fixed,
        // with no manual install-hooks run.
        let saved = std::env::var("METAMATE_BIN").ok();
        std::env::set_var("METAMATE_BIN", "/bin/true");
        let home = scratch_home("muse");
        let mut s = AppState::new();
        // The heal runs before the spawn, so it holds whether or not the
        // runner permits PTYs (sandboxes deny openpty; the assertions
        // below are spawn-agnostic).
        match s.create_session_with_home(&home, "/tmp/forge-under-test", &muse_spec("m")) {
            Ok(id) => {
                assert_eq!(s.manager.get(id).unwrap().cli_tool, "muse");
                assert!(s.manager.remove(id));
            }
            Err(_) => {}
        }
        let text =
            std::fs::read_to_string(home.join(".config/muse/settings.json")).unwrap();
        assert!(text.contains("hook-relay"), "hooks healed: {text}");
        assert!(text.contains("mcpServers"), "mcp healed: {text}");
        match saved {
            Some(v) => std::env::set_var("METAMATE_BIN", v),
            None => std::env::remove_var("METAMATE_BIN"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn create_session_leaves_present_setup_untouched() {
        let saved = std::env::var("METAMATE_BIN").ok();
        std::env::set_var("METAMATE_BIN", "/bin/true");
        let home = scratch_home("muse-set");
        crate::hooks::install::install_one(&home, "muse", "/tmp/forge-under-test");
        let before =
            std::fs::read_to_string(home.join(".config/muse/settings.json")).unwrap();
        let mut s = AppState::new();
        // Spawn-agnostic like the heal test above: the setup check runs
        // before the spawn either way.
        if let Ok(id) = s.create_session_with_home(&home, "/tmp/forge-under-test", &muse_spec("m")) {
            assert!(s.manager.remove(id));
        }
        let after =
            std::fs::read_to_string(home.join(".config/muse/settings.json")).unwrap();
        assert_eq!(before, after, "setup present means byte-identical config");
        match saved {
            Some(v) => std::env::set_var("METAMATE_BIN", v),
            None => std::env::remove_var("METAMATE_BIN"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn create_session_shells_never_touch_agent_setup() {
        let home = scratch_home("shell");
        let mut s = AppState::new();
        if let Ok(id) = s.create_session_with_home(
            &home,
            "/tmp/forge-under-test",
            &crate::ui::dialogs::create::SessionSpec {
                kind: crate::ui::dialogs::create::SessionKind::Shell,
                name: "sh".to_string(),
                cwd: std::env::temp_dir(),
                model: String::new(),
                group: None,
            },
        ) {
            assert_eq!(s.manager.get(id).unwrap().cli_tool, "shell");
            assert!(s.manager.remove(id));
        }
        assert!(
            !home.join(".config/muse/settings.json").exists(),
            "shells heal nothing"
        );
        assert!(!home.join(".claude/settings.json").exists());
        let _ = std::fs::remove_dir_all(&home);
    }
}
