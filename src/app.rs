//! Single-owner application state.
//!
//! The main loop (and, in tests, the harness directly) reduces every
//! `AppEvent` through [`AppState::apply`]. Workers never touch this struct;
//! dirty-flag rendering and shutdown flow out of the same reduction.

use crate::event::AppEvent;
use crate::session::SessionManager;

pub struct AppState {
    pub manager: SessionManager,
    pub dirty: bool,
    pub should_quit: bool,
    pub term_size: (u16, u16),
    /// Hook records awaiting a policy decision. Bounded: beyond the cap
    /// newcomers are dropped and their relays fail open on timeout.
    pub pending_hooks: std::collections::VecDeque<crate::listener::HookRequest>,
    /// Open create-session dialog, if any. Captures all input while present.
    pub create_dialog: Option<crate::create::CreateDialog>,
    /// Open group-management dialog, if any. Captures all input while
    /// present; never both dialogs at once (openers are unreachable
    /// behind the other dialog).
    pub group_dialog: Option<crate::groups::GroupDialog>,
    /// Live permission mode. The TUI loop rebuilds policy and persists the
    /// config whenever this diverges from the loaded one.
    pub permission_mode: crate::config::PermissionMode,
    /// Cross-session message broker (Phase 4): groups, conversations, queues.
    pub broker: crate::comms::Broker,
    /// Last human key/paste forwarded to a pane. Injections wait out a short
    /// debounce after typing so they never interleave with user input.
    pub last_human_input: Option<std::time::Instant>,
    /// Sessions owed a staged Enter: an injection body went out and its CR
    /// follows after [`crate::comms::INJECT_ENTER_DELAY`], staged per
    /// session and re-armed by newer bodies.
    pub pending_enter: std::collections::HashMap<crate::session::SessionId, std::time::Instant>,
    /// One read-only chrome view, scoped to the currently focused session.
    pub overlay_view: Option<(crate::session::SessionId, usize)>,
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            manager: SessionManager::new(),
            dirty: true,
            should_quit: false,
            term_size: (24, 80),
            pending_hooks: std::collections::VecDeque::new(),
            create_dialog: None,
            group_dialog: None,
            permission_mode: crate::config::PermissionMode::Yolo,
            broker: crate::comms::Broker::new(),
            last_human_input: None,
            pending_enter: std::collections::HashMap::new(),
            overlay_view: None,
        }
    }

    /// Set the live permission mode; true when it changed. Non Off/Yolo
    /// modes collapse to Yolo on toggle, never back (toggle only spans
    /// the two sidebar buttons).
    pub fn set_permission_mode(&mut self, mode: crate::config::PermissionMode) -> bool {
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
            crate::config::PermissionMode::Yolo => crate::config::PermissionMode::Off,
            _ => crate::config::PermissionMode::Yolo,
        };
        self.set_permission_mode(next)
    }

    /// Per-session tab strip for the focused session: the agent CLI tab
    /// plus the human terminal tab. Empty when nothing is focused.
    pub fn topbar(&self) -> crate::ui::TopBar {
        let Some(id) = self.manager.active() else {
            return crate::ui::TopBar::default();
        };
        let Some(rec) = self.manager.get(id) else {
            return crate::ui::TopBar::default();
        };
        let mut tabs: Vec<crate::ui::TopTab> = rec
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| crate::ui::TopTab {
                label: match tab.kind {
                    crate::session::TabKind::Agent => {
                        let mut chars = rec.cli_tool.chars();
                        match chars.next() {
                            None => "Agent".to_string(),
                            Some(first) => {
                                first.to_uppercase().collect::<String>() + chars.as_str()
                            }
                        }
                    }
                    crate::session::TabKind::Terminal => "Terminal".to_string(),
                },
                active: i == rec.active_tab,
            })
            .collect();
        if rec.tabs.len() > 1 && self.term_size.1 >= 100 {
            for (index, label) in ["Events", "Tasks", "Visual", "SCM"].iter().enumerate() {
                tabs.push(crate::ui::TopTab {
                    label: (*label).to_string(),
                    active: self.overlay_view == Some((id, index + 2)),
                });
            }
            if self.overlay_view.is_some_and(|(view_id, _)| view_id == id) {
                for tab in tabs.iter_mut().take(2) { tab.active = false; }
            }
        }
        if self.term_size.1 >= 100 {
            for (tab, icon) in tabs.iter_mut().zip(["◉", "▣", "▤", "☑", "▧", "✣"]) {
                tab.label = format!("{icon} {}", tab.label);
            }
        }
        crate::ui::TopBar { tabs }
    }

    /// Select a PTY tab or one of the read-only view slots shown in the
    /// agent's top bar. Extra views never create a PTY or accept typing.
    pub fn select_top_tab(&mut self, index: usize) -> bool {
        let Some(id) = self.manager.active() else { return false; };
        let Some(rec) = self.manager.get(id) else { return false; };
        if index >= self.topbar().tabs.len() { return false; }
        if index < rec.tabs.len() {
            let was_overlay = self.overlay_view.take().is_some();
            let changed = self.manager.select_tab(id, index);
            self.dirty |= was_overlay || changed;
            was_overlay || changed
        } else {
            let next = Some((id, index));
            let changed = self.overlay_view != next;
            self.overlay_view = next;
            self.dirty |= changed;
            changed
        }
    }

    pub fn overlay_active(&self) -> bool {
        self.overlay_view.is_some_and(|(id, _)| self.manager.active() == Some(id))
    }

    /// Snapshot the grid: one view per session in manager order.
    pub fn views(&self) -> Vec<crate::ui::PaneView> {
        let active = self.manager.active();
        self.manager
            .order()
            .iter()
            .map(|&id| {
                let rec = self.manager.get(id).expect("ordered session exists");
                let live = rec.state.is_live();
                if let Some((view_id, index)) = self.overlay_view {
                    if Some(id) == active && view_id == id {
                        let label = ["", "", "Events", "Tasks", "Visual", "SCM"]
                            .get(index).copied().unwrap_or("View");
                        return crate::ui::PaneView {
                            title: format!("{} · {label}", rec.name),
                            lines: vec![vec![crate::ui::SpanView {
                                text: format!("{label} view unavailable in this build"),
                                style: crate::theme::style(crate::theme::Role::Muted),
                            }]],
                            live,
                            focused: true,
                            cursor: None,
                        };
                    }
                }
                let mut lines: Vec<Vec<crate::ui::SpanView>> = self
                    .manager
                    .styled_rows(id)
                    .iter()
                    .map(|row| row.iter().map(crate::ui::span_for).collect())
                    .collect();
                if lines.is_empty() {
                    lines = vec![vec![crate::ui::SpanView {
                        text: "(exited)".to_string(),
                        style: ratatui::style::Style::default(),
                    }]];
                }
                crate::ui::PaneView {
                    title: rec.name.clone(),
                    lines,
                    live,
                    focused: Some(id) == active,
                    cursor: self.manager.cursor(id),
                }
            })
            .collect()
    }

    /// One-row status bar text.
    pub fn status_text(&self) -> String {
        let n = self.manager.len();
        let noun = if n == 1 { "session" } else { "sessions" };
        let active = self
            .manager
            .active()
            .and_then(|id| self.manager.get(id))
            .map(|rec| rec.name.clone())
            .unwrap_or_else(|| "-".to_string());
        let group = self
            .manager
            .active()
            .and_then(|id| self.broker.primary_group(id))
            .map(|g| format!(" | group:{g}"))
            .unwrap_or_default();
        // Dual-tab sessions name the visible tab; single-tab shells omit it.
        let tab = self.overlay_view
            .filter(|(id, _)| self.manager.active() == Some(*id))
            .and_then(|(_, index)| ["", "", "events", "tasks", "visual", "scm"].get(index).copied())
            .filter(|label| !label.is_empty())
            .map(|label| format!(" [{label}]"))
            .or_else(|| self
            .manager
            .active()
            .filter(|&id| self.manager.tab_count(id) > 1)
            .and_then(|id| self.manager.active_tab_kind(id))
            .map(|k| {
                format!(
                    " [{}]",
                    match k {
                        crate::session::TabKind::Agent => "agent",
                        crate::session::TabKind::Terminal => "terminal",
                    }
                )
            }))
            .unwrap_or_default();
        format!("{active}{tab} | {n} {noun} | prefix Ctrl-b (q quit, c new, n/p switch, t tab, g peers, o groups, y yolo){group}")
    }

    /// Record human typing into one session: injections debounce until it
    /// settles, and any staged Enter for that session is dropped — the
    /// human owns the prompt now, and our CR must never submit their
    /// half-typed draft.
    pub fn note_human_input(&mut self, id: crate::session::SessionId) {
        self.last_human_input = Some(std::time::Instant::now());
        self.pending_enter.remove(&id);
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

    /// First free `shell-N` name for the dialog prefill.
    pub fn suggested_session_name(&self) -> String {
        let taken = self.live_names();
        let mut n = self.manager.len() + 1;
        loop {
            let candidate = format!("shell-{n}");
            if !taken.iter().any(|t| t == &candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// Open the create-session dialog, prefilled from current state.
    pub fn open_create_dialog(&mut self) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let name = self.suggested_session_name();
        let groups = self.broker.group_names();
        self.create_dialog = Some(crate::create::CreateDialog::new(&name, &cwd, &groups));
        self.dirty = true;
    }

    /// Open the group management dialog.
    pub fn open_group_dialog(&mut self) {
        self.group_dialog = Some(crate::groups::GroupDialog::new());
        self.dirty = true;
    }

    /// Fresh snapshot for the group dialog: groups with live member
    /// counts, sessions in bar order, and the selected group's current
    /// members for the checkbox pre-check.
    pub fn group_ctx(&self) -> crate::groups::GroupCtx {
        let groups = self
            .broker
            .group_names()
            .into_iter()
            .map(|name| crate::groups::GroupRow {
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
                self.manager.get(id).map(|rec| crate::groups::SessionRow {
                    id,
                    name: rec.name.clone(),
                })
            })
            .collect();
        let selected = self
            .group_dialog
            .as_ref()
            .map(|d| d.selected_name())
            .unwrap_or_default();
        let members = self.broker.group_members(selected);
        crate::groups::GroupCtx {
            groups,
            sessions,
            members,
        }
    }

    /// Apply one group-dialog mutation to the broker. The dialog validates
    /// names against a fresh snapshot, so these are infallible in practice;
    /// failures (exit races) stay silent and keep the dialog open.
    /// Unknown session IDs (exited mid-dialog) are skipped.
    pub fn apply_group(&mut self, outcome: crate::groups::GroupOutcome) {
        use crate::groups::GroupOutcome as Out;
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
            }
            Out::Pending | Out::Closed => {}
        }
        self.dirty = true;
    }

    /// Spawn exactly what a submitted dialog describes: shells run the
    /// login shell, agents run their registry argv. Returns the new id.
    pub fn create_session(
        &mut self,
        spec: &crate::create::SessionSpec,
    ) -> std::io::Result<crate::session::SessionId> {
        use crate::create::SessionKind;
        let (cmd, cli_tool) = match spec.kind {
            SessionKind::Shell => {
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
                (format!("exec {shell} -i"), "shell".to_string())
            }
            SessionKind::Agent(h) => {
                let hs = h.spec();
                let model = if spec.model.is_empty() {
                    None
                } else {
                    Some(spec.model.as_str())
                };
                let argv = hs.launch_argv(&hs.resolve_binary(), model);
                let mut cmd = String::from("exec ");
                cmd.push_str(&crate::create::shell_join(&argv));
                (cmd, h.as_str().to_string())
            }
        };
        // Agent CLIs get the dual-tab layout (agent on tab 0, a lazy
        // human terminal on tab 1); plain shells stay single-tab.
        let run = crate::ids::RunId::generate();
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
        }
        Ok(id)
    }

    /// Deliver due injections into idle, non-recently-typed panes. Targets
    /// whose hook activity is Thinking/ToolUse/Waiting keep waiting, as do
    /// panes the human just typed into.
    pub fn settle_comms(&mut self) {
        use crate::session::Activity;
        let now = std::time::Instant::now();
        self.broker.tick(now);
        let settled = self.last_human_input.is_none_or(|t| {
            now.duration_since(t) >= crate::comms::INJECT_DEBOUNCE
        });
        if !settled {
            return;
        }
        let order = self.manager.order().to_vec();
        for id in order {
            if self.broker.queued(id) == 0 {
                continue;
            }
            let idle = self.manager.get(id).is_some_and(|rec| {
                matches!(rec.activity, Activity::Idle | Activity::Stopped)
            });
            if !idle {
                continue;
            }
            let due = self.broker.take_due(id, usize::MAX);
            let mut wrote = false;
            for inj in due {
                if self.manager.inject_write(id, &inj.render_body()).is_ok() {
                    wrote = true;
                    self.dirty = true;
                }
            }
            if wrote {
                // Arm (or re-arm) the staged Enter: the CR goes out on a
                // later tick, never in the same burst as the text.
                self.pending_enter.insert(id, now);
            }
        }
        self.settle_enters(now);
    }

    /// Send staged Enters whose beat has elapsed. Same gates as bodies —
    /// settled debounce plus an idle target — so a CR never lands mid-turn
    /// or into a human draft (human input clears the entry outright).
    /// Sessions that exited meanwhile are pruned.
    fn settle_enters(&mut self, now: std::time::Instant) {
        use crate::session::Activity;
        let settled = self.last_human_input.is_none_or(|t| {
            now.duration_since(t) >= crate::comms::INJECT_DEBOUNCE
        });
        let due: Vec<crate::session::SessionId> = self
            .pending_enter
            .iter()
            .filter(|(_, &at)| now.duration_since(at) >= crate::comms::INJECT_ENTER_DELAY)
            .map(|(&id, _)| id)
            .collect();
        for id in due {
            let idle = self.manager.get(id).is_some_and(|rec| {
                matches!(rec.activity, Activity::Idle | Activity::Stopped)
            });
            if !settled || !idle {
                continue;
            }
            // A dead pane drops the CR: the body stays visible as a draft
            // for the human rather than vanishing silently.
            let _ = self.manager.inject_write(id, &[crate::comms::INJECT_ENTER_CR]);
            self.pending_enter.remove(&id);
            self.dirty = true;
        }
        self.pending_enter
            .retain(|id, _| self.manager.get(*id).is_some());
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
        match self.manager.order().to_vec().get(index) {
            Some(&id) => {
                self.manager.switch(id);
                self.overlay_view = None;
                self.dirty = true;
                true
            }
            None => false,
        }
    }

    /// Session-bar tabs in order with live/focus flags.
    pub fn tabs(&self) -> Vec<crate::ui::SessionTab> {
        let active = self.manager.active();
        self.manager
            .order()
            .to_vec()
            .into_iter()
            .filter_map(|id| {
                self.manager.get(id).map(|rec| {
                    let group = self.broker.primary_group(id).map(str::to_string);
                    let group_color = group
                        .as_deref()
                        .and_then(|g| self.broker.group_color(g));
                    crate::ui::SessionTab {
                        title: rec.name.clone(),
                        live: rec.state.is_live(),
                        focused: Some(id) == active,
                        group,
                        group_color,
                    }
                })
            })
            .collect()
    }

    /// Sidebar content: the focused session's detail plus pending hooks
    /// and the live permission mode.
    pub fn sidebar_info(&self) -> crate::ui::SidebarInfo {
        let session = self.manager.active().and_then(|id| {
            self.manager.get(id).map(|rec| {
                let state = if rec.state.is_live() {
                    match rec.activity {
                        crate::session::Activity::Idle => "running".to_string(),
                        activity => format!("running · {activity:?}"),
                    }
                } else {
                    match rec.exit_code {
                        Some(code) => format!("exited({code})"),
                        None => "exited".to_string(),
                    }
                };
                crate::ui::SessionDetail {
                    name: rec.name.clone(),
                    cli_tool: rec.cli_tool.clone(),
                    cwd: rec.cwd.to_string_lossy().into_owned(),
                    state,
                    uptime_secs: rec.spawned_at.elapsed().as_secs(),
                    tool_calls: rec.tool_calls,
                    approvals: rec.approvals,
                    denials: rec.denials,
                }
            })
        });
        crate::ui::SidebarInfo {
            session,
            pending: self.pending_hooks.len(),
            mode: self.permission_mode.as_str(),
        }
    }

    /// Run deterministic policy over queued hook requests. Every verdict —
    /// allow, deny, or ask-for-the-harness — replies immediately and is
    /// audited; nothing waits on a human. Yolo auto-approves, Safe-Only
    /// blocks still deny, and every other Ask goes back to the harness so
    /// its native permission flow takes over. Audit failures never block.
    pub fn settle_hooks(
        &mut self,
        policy: &mut crate::policy::Policy,
        audit_path: &std::path::Path,
    ) {
        while let Some(req) = self.pending_hooks.pop_front() {
            let (decision, reason) = policy.decide(&req.hook, &req.body);
            let line = crate::policy::decision_line(decision, reason);
            let _ = req.reply.send(line);
            // Attribute the verdict to the sender's sidebar counters.
            if let Some(id) = self.manager.lookup_run(&req.run_id) {
                self.manager.note_verdict(id, decision);
            }
            self.audit_hook(audit_path, &req.hook, &req.body, decision, reason);
        }
        self.dirty = true;
    }

    fn audit_hook(
        &self,
        audit_path: &std::path::Path,
        hook: &str,
        body: &str,
        decision: crate::policy::Decision,
        reason: &str,
    ) {
        let tool = crate::policy::tool_name(body);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let name = format!("{decision:?}").to_lowercase();
        let _ = crate::audit::append(audit_path, hook, &tool, &name, reason, now);
    }

    /// Reduce one event. Input routing arrives with the input slice; until
    /// then input events are acknowledged but change nothing.
    pub fn apply(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Tick => {}
            AppEvent::Shutdown => self.should_quit = true,
            AppEvent::SessionOutput { .. } => {
                self.dirty = true;
            }
            AppEvent::SessionExited { id, .. } => {
                // An exit fails every conversation touching it; sources of
                // open asks/tells are notified through the broker queue.
                self.broker.target_exited(&self.manager, id);
                self.dirty = true;
            }
            AppEvent::CommsRequest(req) => {
                // The broker answers at once; the verdict goes straight back
                // to `mcp-serve`. Failures stay single-line JSON, escaped.
                let now = std::time::Instant::now();
                let line =
                    match self.broker.call(&self.manager, &req.run_id, &req.tool, &req.args, now)
                    {
                        Ok(result) => format!("{{\"ok\":true,\"result\":{result}}}\n"),
                        Err(e) => format!(
                            "{{\"ok\":false,\"error\":{}}}\n",
                            crate::mcp::escape_json(&e)
                        ),
                    };
                let _ = req.reply.send(line);
                self.dirty = true;
            }
            AppEvent::Resize(rows, cols) => {
                self.term_size = (rows, cols);
                if cols < 100 { self.overlay_view = None; }
                self.dirty = true;
            }
            AppEvent::HookRequest(req) => {
                // Attribute hook activity before queuing: the sender's run
                // ID resolves to its session, unknown runs stay untouched.
                // Tool-gated hooks also count one sidebar tool call.
                if let Some(activity) = crate::session::activity_for_hook(&req.hook) {
                    if let Some(id) = self.manager.lookup_run(&req.run_id) {
                        self.manager.set_activity(id, activity);
                        if activity == crate::session::Activity::ToolUse {
                            self.manager.note_tool_call(id);
                        }
                    }
                }
                if self.pending_hooks.len() < crate::listener::MAX_PENDING_HOOKS {
                    self.pending_hooks.push_back(req);
                }
                self.dirty = true;
            }
            AppEvent::Input(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::AppEvent;
    use crate::ids::RunId;
    use crate::session::SessionId;

    #[test]
    fn fresh_state_needs_paint_and_runs() {
        let s = AppState::new();
        assert!(s.dirty);
        assert!(!s.should_quit);
        assert!(s.manager.is_empty());
    }

    #[test]
    fn shutdown_quits() {
        let mut s = AppState::new();
        s.dirty = false;
        s.apply(AppEvent::Shutdown);
        assert!(s.should_quit);
    }

    #[test]
    fn hook_requests_queue_bounded_and_dirty() {
        let mut s = AppState::new();
        s.dirty = false;
        let (reply_tx, _reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
            hook: "PreToolUse".to_string(),
            body: "{}".to_string(),
            run_id: String::new(),
            sync: true,
            reply: reply_tx,
            timed_out: Default::default(),
        }));
        assert!(s.dirty);
        assert_eq!(s.pending_hooks.len(), 1);
        for _ in 0..crate::listener::MAX_PENDING_HOOKS + 10 {
            let (tx, _rx) = std::sync::mpsc::channel();
            s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
                hook: "Stop".to_string(),
                body: "{}".to_string(),
                run_id: String::new(),
                sync: false,
                reply: tx,
                timed_out: Default::default(),
            }));
        }
        assert_eq!(s.pending_hooks.len(), crate::listener::MAX_PENDING_HOOKS);
    }

    #[test]
    fn settle_hooks_replies_every_verdict_immediately() {
        use crate::config::PermissionMode;
        let audit = std::env::temp_dir().join(format!(
            "forge-settle-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&audit);
        let mut yolo = crate::policy::Policy::new(
            PermissionMode::Yolo,
            &[],
            &[],
        )
        .unwrap();
        let mut s = AppState::new();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
            hook: "PreToolUse".to_string(),
            body: r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#.to_string(),
            run_id: String::new(),
            sync: true,
            reply: reply_tx,
            timed_out: Default::default(),
        }));
        s.settle_hooks(&mut yolo, &audit);
        assert!(s.pending_hooks.is_empty());
        let line = reply_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(line.contains(r#""decision":"allow""#), "line: {line:?}");
        let logged = std::fs::read_to_string(&audit).unwrap();
        assert_eq!(logged.lines().count(), 1);
        assert!(logged.contains(r#""decision":"allow""#), "audit: {logged:?}");

        let mut off = crate::policy::Policy::new(
            PermissionMode::Off,
            &[],
            &[],
        )
        .unwrap();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
            hook: "PreToolUse".to_string(),
            body: "{}".to_string(),
            run_id: String::new(),
            sync: true,
            reply: reply_tx,
            timed_out: Default::default(),
        }));
        s.settle_hooks(&mut off, &audit);
        assert!(s.pending_hooks.is_empty(), "no modal queue anymore");
        // Non-yolo Ask goes straight back so the harness handles it.
        let line = reply_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(line.contains(r#""decision":"ask""#), "line: {line:?}");
        let logged = std::fs::read_to_string(&audit).unwrap();
        assert_eq!(logged.lines().count(), 2, "audit: {logged:?}");
        assert!(logged.contains(r#""decision":"ask""#), "audit: {logged:?}");
        let _ = std::fs::remove_file(&audit);
    }

    #[test]
    fn safe_only_blocks_still_deny_without_a_modal() {
        use crate::config::PermissionMode;
        let audit = std::env::temp_dir().join(format!(
            "forge-block-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&audit);
        let mut policy = crate::policy::Policy::new(
            PermissionMode::SafeOnly,
            &[],
            &["doom".to_string()],
        )
        .unwrap();
        let mut s = AppState::new();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
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
        assert!(line.contains(r#""decision":"deny""#), "line: {line:?}");
        let logged = std::fs::read_to_string(&audit).unwrap();
        assert_eq!(logged.lines().count(), 1, "audit: {logged:?}");
        let _ = std::fs::remove_file(&audit);
    }

    #[test]
    fn comms_ask_flows_through_apply_and_replies() {
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "ask_session".to_string(),
            args: r#"{"target":"b","message":"ready?"}"#.to_string(),
            reply: reply_tx,
        }));
        let line = reply_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(line.contains(r#""ok":true"#), "line: {line:?}");
        assert!(line.contains(r#""conversation":""#), "line: {line:?}");
        assert!(line.ends_with('\n'));
        assert_eq!(s.broker.queued(b), 1);
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn comms_rejections_are_single_line_json() {
        let mut s = AppState::new();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: "f".repeat(32),
            tool: "ask_session".to_string(),
            args: "{}".to_string(),
            reply: reply_tx,
        }));
        let line = reply_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(line.contains(r#""ok":false"#), "line: {line:?}");
        assert!(!line.trim_end().contains('\n'), "one line: {line:?}");
    }

    #[test]
    fn session_exit_fails_open_conversations() {
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "ask_session".to_string(),
            args: r#"{"target":"b","message":"q?"}"#.to_string(),
            reply: reply_tx,
        }));
        assert!(s.manager.kill(b));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                s.apply(AppEvent::from_pty(ev.0, ev.2));
            }
            let exited = s.manager.get(b).is_none_or(|rec| !rec.state.is_live());
            if exited || std::time::Instant::now() > deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let due = s.broker.take_due(a, 10);
        assert_eq!(due.len(), 1, "source learns the failure");
        assert!(matches!(
            due[0].kind,
            crate::comms::InjectKind::Failed
        ));
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn settle_comms_delivers_to_idle_panes_only() {
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        // Busy target: the injection waits.
        assert!(s.manager.set_activity(b, crate::session::Activity::ToolUse));
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "tell_session".to_string(),
            args: "{\"target\":\"b\",\"text\":\"hello-b\"}".to_string(),
            reply: reply_tx,
        }));
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 1, "busy target waits");
        // Idle target: delivered into the pane.
        assert!(s.manager.set_activity(b, crate::session::Activity::Idle));
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                s.apply(AppEvent::from_pty(ev.0, ev.2));
            }
            if s.manager.screen_text(b).is_some_and(|t| t.contains("hello-b")) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "injection never reached the pane"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn reply_round_trip_needs_stop_to_reach_first_session() {
        // Live shape: both agents have fired hooks, so neither is Idle.
        // The ask goes out, the target answers, and the reply waits until
        // the first session's turn ends (Stop parks Stopped). Before the
        // Stop edge was installed, that wait never ended.
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        let hook = |s: &mut AppState, hook: &str, run_id: String| {
            let (reply_tx, _) = std::sync::mpsc::channel();
            s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
                hook: hook.to_string(),
                body: "{}".to_string(),
                run_id,
                sync: false,
                reply: reply_tx,
                timed_out: Default::default(),
            }));
        };
        let comms = |s: &mut AppState, run_id: String, tool: &str, args: &str| -> String {
            let (reply_tx, reply_rx) = std::sync::mpsc::channel();
            s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
                run_id,
                tool: tool.to_string(),
                args: args.to_string(),
                reply: reply_tx,
            }));
            reply_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("broker answers at once")
        };
        // Both sessions mid-turn, like live agents that called tools.
        hook(&mut s, "PreToolUse", run_a.to_string());
        hook(&mut s, "PreToolUse", run_b.to_string());
        // A's ask waits while B is busy, then lands when B's turn ends.
        let ask_line = comms(&mut s, run_a.to_string(), "ask_session", "{\"target\":\"b\",\"message\":\"ready?\"}");
        assert!(ask_line.contains("\"ok\":true"), "ask accepted: {ask_line}");
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 1, "busy target holds the ask");
        hook(&mut s, "Stop", run_b.to_string());
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 0, "turn end delivers the ask");
        // B answers; A is still mid-turn so the reply waits.
        let conv = crate::policy::json_string_field(ask_line.as_bytes(), &["conversation"])
            .expect("ask returns a conversation");
        let resp_line = comms(
            &mut s,
            run_b.to_string(),
            "send_response",
            &format!("{{\"conversation_id\":\"{conv}\",\"message\":\"got-it\"}}"),
        );
        assert!(resp_line.contains("\"ok\":true"), "reply accepted: {resp_line}");
        s.settle_comms();
        assert_eq!(s.broker.queued(a), 1, "reply waits while A works");
        // A's turn ends: the reply lands in A's pane.
        hook(&mut s, "Stop", run_a.to_string());
        s.settle_comms();
        assert_eq!(s.broker.queued(a), 0, "turn end delivers the reply");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                s.apply(AppEvent::from_pty(ev.0, ev.2));
            }
            if s.manager.screen_text(a).is_some_and(|t| t.contains("got-it")) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "reply never reached the first session"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn prompt_submit_holds_injections_until_stop() {
        // A freshly prompted session looks settled but its agent is
        // generating: UserPromptSubmit must hold injections (where Enter
        // would be eaten and the text left as a draft) until Stop.
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        let hook = |s: &mut AppState, hook: &str, run_id: String| {
            let (reply_tx, _) = std::sync::mpsc::channel();
            s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
                hook: hook.to_string(),
                body: "{}".to_string(),
                run_id,
                sync: false,
                reply: reply_tx,
                timed_out: Default::default(),
            }));
        };
        // A was parked (Stopped) but just got prompted: generating.
        hook(&mut s, "Stop", run_a.to_string());
        hook(&mut s, "UserPromptSubmit", run_a.to_string());
        assert_eq!(
            s.manager.get(a).unwrap().activity,
            crate::session::Activity::Thinking
        );
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run_b.to_string(),
            tool: "tell_session".to_string(),
            args: "{\"target\":\"a\",\"text\":\"hold\"}".to_string(),
            reply: reply_tx,
        }));
        s.settle_comms();
        assert_eq!(s.broker.queued(a), 1, "generating target waits");
        hook(&mut s, "Stop", run_a.to_string());
        s.settle_comms();
        assert_eq!(s.broker.queued(a), 0, "turn end delivers");
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn settle_comms_stages_enter_beat_after_body() {
        // Byte-level human parity through a raw-mode slave (no CR/LF
        // translation anywhere, no echo): the body arrives whole with no
        // Enter bundled in, and the CR follows as its own input event
        // after the beat — type text, press Enter, never one burst.
        use crate::pty::PtyEvent;
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn(
                "b",
                &std::env::temp_dir(),
                "stty raw -echo && printf READY || printf STTYFAIL; exec cat",
                run_b.clone(),
                "shell",
            )
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        // Wait out the spawn: anything the starting shell echoes (including
        // canonical-mode translations) predates raw mode and must not
        // pollute the byte assertions below.
        let mut raw = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                if ev.0 == b {
                    if let PtyEvent::Output(bytes) = &ev.2 {
                        raw.extend_from_slice(bytes);
                    }
                }
            }
            if raw.windows(8).any(|w| w == b"STTYFAIL") {
                panic!("stty raw failed in the probe session");
            }
            if raw.windows(5).any(|w| w == b"READY") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "probe session never went raw, got: {raw:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        raw.clear();
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "tell_session".to_string(),
            args: "{\"target\":\"b\",\"message\":\"ping-body\"}".to_string(),
            reply: reply_tx,
        }));
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 0, "raw session is idle: body delivered");
        assert!(s.pending_enter.contains_key(&b), "enter staged, not sent");
        // Drain raw output until cat echoes the full body back.
        let body = b"[forge tell_session from a]: ping-body";
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                if ev.0 == b {
                    if let PtyEvent::Output(bytes) = &ev.2 {
                        raw.extend_from_slice(bytes);
                    }
                }
            }
            if raw.windows(body.len()).any(|w| w == body) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "body never echoed, got: {raw:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!raw.contains(&b'\r'), "no Enter bundled with the body");
        // After the beat the CR goes out as its own event, trailing the body.
        std::thread::sleep(
            crate::comms::INJECT_ENTER_DELAY + std::time::Duration::from_millis(100),
        );
        s.settle_comms();
        assert!(!s.pending_enter.contains_key(&b), "enter sent");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                if ev.0 == b {
                    if let PtyEvent::Output(bytes) = &ev.2 {
                        raw.extend_from_slice(bytes);
                    }
                }
            }
            if raw.contains(&b'\r') {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "staged enter never arrived, got: {raw:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let pos = raw
            .windows(body.len())
            .position(|w| w == body)
            .expect("body present");
        assert!(
            raw[pos + body.len()..].contains(&b'\r'),
            "enter trails the body: {raw:?}"
        );
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn settle_comms_holds_during_human_typing() {
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "tell_session".to_string(),
            args: r#"{"target":"b","message":"wait"}"#.to_string(),
            reply: reply_tx,
        }));
        s.note_human_input(a);
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 1, "fresh typing debounces delivery");
        // Human input also drops a staged Enter for that session: our CR
        // must never submit the human's draft.
        s.pending_enter.insert(b, std::time::Instant::now());
        s.note_human_input(b);
        assert!(!s.pending_enter.contains_key(&b), "human owns the prompt");
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn create_session_spawns_named_shell() {
        let mut s = AppState::new();
        let spec = crate::create::SessionSpec {
            kind: crate::create::SessionKind::Shell,
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
    fn create_session_with_group_joins_at_birth() {
        let mut s = AppState::new();
        s.broker.create_group("team").unwrap();
        let id = s
            .create_session(&crate::create::SessionSpec {
                kind: crate::create::SessionKind::Shell,
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
        let spec = crate::create::SessionSpec {
            kind: crate::create::SessionKind::Agent(crate::harness::Harness::Codex),
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
    fn suggested_name_skips_taken_names() {
        let mut s = AppState::new();
        assert_eq!(s.suggested_session_name(), "shell-1");
        s.open_create_dialog();
        assert!(s.create_dialog.is_some());
        let id = s
            .manager
            .spawn("shell-1", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        assert_eq!(s.suggested_session_name(), "shell-2");
        assert!(s.manager.remove(id));
    }

    #[test]
    fn session_traffic_dirties() {
        let mut s = AppState::new();
        s.dirty = false;
        let id = SessionId::fresh();
        s.apply(AppEvent::SessionOutput { id, data: vec![1] });
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
    fn tick_is_quiet() {
        let mut s = AppState::new();
        s.dirty = false;
        s.apply(AppEvent::Tick);
        assert!(!s.dirty);
        assert!(!s.should_quit);
    }

    #[test]
    fn views_begin_empty() {
        let s = AppState::new();
        assert!(s.views().is_empty());
    }

    #[test]
    fn views_reflect_sessions() {
        let mut s = AppState::new();
        let a = s
            .manager
            .spawn("one", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        let b = s
            .manager
            .spawn("two", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        let views = s.views();
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].title, "one");
        assert_eq!(views[1].title, "two");
        assert!(views[0].focused && !views[1].focused);
        assert!(views.iter().all(|v| v.live));
        let status = s.status_text();
        assert!(status.contains("2 sessions"), "status: {status:?}");
        assert!(status.contains("Ctrl-b"), "status: {status:?}");
        s.step_session(1);
        assert_eq!(s.manager.active(), Some(b));
        s.step_session(1);
        assert_eq!(s.manager.active(), Some(a));
        s.step_session(-1);
        assert_eq!(s.manager.active(), Some(b));
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
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
    fn hook_requests_attribute_activity_to_sender() {
        let mut s = AppState::new();
        let run = RunId::generate();
        let id = s
            .manager
            .spawn("h", &std::env::temp_dir(), "exec sleep 30", run.clone(), "shell")
            .unwrap();
        fn hook(s: &mut AppState, hook: &str, run_id: String) {
            let (reply_tx, _) = std::sync::mpsc::channel();
            s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
                hook: hook.to_string(),
                body: "{}".to_string(),
                run_id,
                sync: false,
                reply: reply_tx,
                timed_out: Default::default(),
            }));
        }
        hook(&mut s, "PreToolUse", run.to_string());
        assert_eq!(
            s.manager.get(id).unwrap().activity,
            crate::session::Activity::ToolUse
        );
        hook(&mut s, "Stop", run.to_string());
        assert_eq!(
            s.manager.get(id).unwrap().activity,
            crate::session::Activity::Stopped
        );
        // Unknown runs never touch live sessions.
        hook(&mut s, "PreToolUse", "f".repeat(32));
        assert_eq!(
            s.manager.get(id).unwrap().activity,
            crate::session::Activity::Stopped
        );
        assert!(s.manager.remove(id));
    }

    #[test]
    fn permission_mode_toggle_spans_off_and_yolo() {
        let mut s = AppState::new();
        assert_eq!(s.permission_mode, crate::config::PermissionMode::Yolo);
        s.dirty = false;
        assert!(s.toggle_permission_mode());
        assert_eq!(s.permission_mode, crate::config::PermissionMode::Off);
        assert!(s.dirty);
        assert!(!s.set_permission_mode(crate::config::PermissionMode::Off));
        assert!(s.toggle_permission_mode());
        assert_eq!(s.permission_mode, crate::config::PermissionMode::Yolo);
        // Foreign modes collapse to Yolo, never back through the toggle.
        assert!(s.set_permission_mode(crate::config::PermissionMode::SafeOnly));
        assert!(s.toggle_permission_mode());
        assert_eq!(s.permission_mode, crate::config::PermissionMode::Yolo);
    }

    #[test]
    fn hook_attribution_feeds_sidebar_counters() {
        let mut s = AppState::new();
        let run = RunId::generate();
        let id = s
            .manager
            .spawn("h", &std::env::temp_dir(), "exec sleep 30", run.clone(), "shell")
            .unwrap();
        let rec = s.manager.get(id).unwrap();
        assert_eq!((rec.tool_calls, rec.approvals, rec.denials), (0, 0, 0));
        // Tool-gated hook: activity + one tool call.
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
            hook: "PreToolUse".to_string(),
            body: "{}".to_string(),
            run_id: run.to_string(),
            sync: false,
            reply: reply_tx,
            timed_out: Default::default(),
        }));
        assert_eq!(s.manager.get(id).unwrap().tool_calls, 1);
        // Yolo settle: one approval, no denial.
        let audit = std::env::temp_dir().join(format!(
            "forge-counters-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&audit);
        let mut yolo =
            crate::policy::Policy::new(crate::config::PermissionMode::Yolo, &[], &[]).unwrap();
        s.settle_hooks(&mut yolo, &audit);
        let rec = s.manager.get(id).unwrap();
        assert_eq!((rec.approvals, rec.denials), (1, 0));
        let _ = std::fs::remove_file(&audit);
        assert!(s.manager.remove(id));
    }

    #[test]
    fn create_session_routes_agent_to_dual_tabs() {
        // Point the codex binary at `cat` so the test never depends on a
        // real agent CLI being installed; no other test reads this var.
        std::env::set_var("CODEX_BIN", "cat");
        let mut s = AppState::new();
        let agent = s
            .create_session(&crate::create::SessionSpec {
                kind: crate::create::SessionKind::Agent(crate::harness::Harness::Codex),
                name: "codex-1".to_string(),
                cwd: std::env::temp_dir(),
                model: String::new(),
                group: None,
            })
            .unwrap();
        std::env::remove_var("CODEX_BIN");
        assert_eq!(s.manager.tab_count(agent), 2);
        let status = s.status_text();
        assert!(status.contains("[agent]"), "status: {status:?}");
        assert!(s.manager.switch_tab(agent));
        let status = s.status_text();
        assert!(status.contains("[terminal]"), "status: {status:?}");
        let shell = s
            .create_session(&crate::create::SessionSpec {
                kind: crate::create::SessionKind::Shell,
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
    fn agent_topbar_exposes_clickable_read_only_views() {
        let mut state = AppState::new();
        state.term_size = (40, 180);
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        let labels: Vec<String> = state.topbar().tabs.iter().map(|tab| tab.label.clone()).collect();
        assert_eq!(labels, ["◉ Codex", "▣ Terminal", "▤ Events", "☑ Tasks", "▧ Visual", "✣ SCM"]);
        assert!(state.select_top_tab(2));
        assert!(state.topbar().tabs[2].active);
        assert!(state.status_text().contains("[events]"));
        let view = state.views().into_iter().find(|v| v.focused).unwrap();
        assert!(view.lines.iter().flatten().any(|span| span.text.contains("Events")));
        assert!(view.lines.iter().flatten().any(|span| span.text.contains("unavailable")));
        assert!(state.select_top_tab(0));
        assert!(state.topbar().tabs[0].active);
        assert!(state.manager.remove(id));
    }

    #[test]
    fn wide_agent_topbar_uses_labeled_icons_from_reference() {
        let mut state = AppState::new();
        state.term_size = (40, 180);
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        let labels: Vec<String> = state.topbar().tabs.iter().map(|tab| tab.label.clone()).collect();
        assert!(labels[0].starts_with("◉ "));
        assert!(labels[1].starts_with("▣ "));
        assert!(labels[2].starts_with("▤ "));
        assert!(labels[3].starts_with("☑ "));
        assert!(labels[4].starts_with("▧ "));
        assert!(labels[5].starts_with("✣ "));
        assert!(state.manager.remove(id));
    }

    #[test]
    fn shrinking_hides_and_closes_extra_view() {
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        assert!(state.select_top_tab(2));
        state.apply(AppEvent::Resize(24, 80));
        assert!(!state.overlay_active());
        assert_eq!(state.topbar().tabs.len(), 2);
        assert!(state.manager.remove(id));
    }

    #[test]
    fn ordinary_wide_terminal_keeps_all_topbar_views_visible() {
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(30, 120));
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        assert_eq!(state.topbar().tabs.len(), 6);
        let bar = crate::ui::chrome_areas(ratatui::layout::Rect::new(0, 0, 120, 30)).topbar;
        assert_eq!(crate::ui::layout_topbar(bar, &state.topbar().tabs).len(), 6);
        assert!(state.manager.remove(id));
    }

    #[test]
    fn events_tab_does_not_mislabel_tool_calls_as_event_count() {
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let run = crate::ids::RunId::generate();
        let id = state.manager.spawn_agent("agent", &std::env::temp_dir(), "exec cat", run.clone(), "codex").unwrap();
        let (reply, _) = std::sync::mpsc::channel();
        state.apply(AppEvent::HookRequest(crate::listener::HookRequest {
            hook: "PreToolUse".into(), body: "{}".into(), run_id: run.as_str().into(),
            sync: false, reply, timed_out: Default::default(),
        }));
        assert_eq!(state.topbar().tabs[2].label, "▤ Events");
        assert!(state.manager.remove(id));
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
}
