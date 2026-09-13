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
    /// Hook records awaiting a policy decision (3c) and the permission
    /// modal (3d). Bounded: beyond the cap newcomers are dropped and their
    /// relays fail open on timeout.
    pub pending_hooks: std::collections::VecDeque<crate::listener::HookRequest>,
    /// Open permission modal, if any. Captures all input while present.
    pub modal: Option<ActiveModal>,
}

/// One modal session: the queued request plus its tuirealm state.
pub struct ActiveModal {
    pub req: crate::listener::HookRequest,
    pub modal: crate::modal::PermissionModal,
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            manager: SessionManager::new(),
            dirty: true,
            should_quit: false,
            term_size: (24, 80),
            pending_hooks: std::collections::VecDeque::new(),
            modal: None,
        }
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
        format!("{active} | {n} {noun} | prefix Ctrl-b (q quit, c new, n/p switch)")
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
    }

    /// Focus session by order index (`Ctrl-b 1` is index 0). Returns false
    /// when out of range, leaving focus untouched.
    pub fn select_session(&mut self, index: usize) -> bool {
        match self.manager.order().to_vec().get(index) {
            Some(&id) => {
                self.manager.switch(id);
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
                self.manager.get(id).map(|rec| crate::ui::SessionTab {
                    title: rec.name.clone(),
                    live: rec.state.is_live(),
                    focused: Some(id) == active,
                })
            })
            .collect()
    }

    /// Sidebar content: session tabs plus pending approvals and mode.
    pub fn sidebar_info(&self, mode: &'static str) -> crate::ui::SidebarInfo {
        crate::ui::SidebarInfo {
            sessions: self.tabs(),
            pending: self.pending_hooks.len(),
            mode,
        }
    }

    /// Run deterministic policy over queued hook requests. Allow/Deny reply
    /// immediately and are audited; synchronous Ask stays queued for the
    /// permission modal while asynchronous Ask is audited and dropped
    /// (nobody waits on it). Audit failures never block a decision.
    pub fn settle_hooks(
        &mut self,
        policy: &mut crate::policy::Policy,
        audit_path: &std::path::Path,
    ) {
        let mut i = 0;
        while i < self.pending_hooks.len() {
            let (hook, body, sync) = {
                let req = &self.pending_hooks[i];
                (req.hook.clone(), req.body.clone(), req.sync)
            };
            let (decision, reason) = policy.decide(&hook, &body);
            if matches!(decision, crate::policy::Decision::Ask) && sync {
                i += 1;
                continue;
            }
            if let Some(req) = self.pending_hooks.remove(i) {
                if !matches!(decision, crate::policy::Decision::Ask) {
                    let line = crate::policy::decision_line(decision, reason);
                    let _ = req.reply.send(line);
                }
                self.audit_hook(audit_path, &hook, &body, decision, reason);
            }
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

    /// Move the front queued request into the permission modal, if none is
    /// open. Returns true when a modal is now showing.
    pub fn open_modal_if_needed(&mut self) -> bool {
        if self.modal.is_some() {
            return true;
        }
        let Some(req) = self.pending_hooks.pop_front() else {
            return false;
        };
        let prompt = crate::modal::PermissionPrompt {
            hook: req.hook.clone(),
            tool: crate::policy::tool_name(&req.body),
            command: crate::policy::command_of(&req.body),
        };
        self.modal = Some(ActiveModal {
            req,
            modal: crate::modal::PermissionModal::new(prompt),
        });
        self.dirty = true;
        true
    }

    /// Answer the open modal: allow-once replies without caching, deny
    /// replies and caches the denial for identical requests. Both audit.
    pub fn decide_modal(
        &mut self,
        allow: bool,
        policy: &mut crate::policy::Policy,
        audit_path: &std::path::Path,
    ) {
        let Some(active) = self.modal.take() else {
            return;
        };
        let decision = if allow {
            crate::policy::Decision::Allow
        } else {
            crate::policy::Decision::Deny
        };
        let reason = if allow { "allow once" } else { "denied by user" };
        let line = crate::policy::decision_line(decision, reason);
        let _ = active.req.reply.send(line);
        if !allow {
            policy.deny(&active.req.body);
        }
        self.audit_hook(audit_path, &active.req.hook, &active.req.body, decision, reason);
        self.dirty = true;
    }

    /// Reduce one event. Input routing arrives with the input slice; until
    /// then input events are acknowledged but change nothing.
    pub fn apply(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Tick => {}
            AppEvent::Shutdown => self.should_quit = true,
            AppEvent::SessionOutput { .. } | AppEvent::SessionExited { .. } => {
                self.dirty = true;
            }
            AppEvent::Resize(rows, cols) => {
                self.term_size = (rows, cols);
                self.dirty = true;
            }
            AppEvent::HookRequest(req) => {
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
            sync: true,
            reply: reply_tx,
        }));
        assert!(s.dirty);
        assert_eq!(s.pending_hooks.len(), 1);
        for _ in 0..crate::listener::MAX_PENDING_HOOKS + 10 {
            let (tx, _rx) = std::sync::mpsc::channel();
            s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
                hook: "Stop".to_string(),
                body: "{}".to_string(),
                sync: false,
                reply: tx,
            }));
        }
        assert_eq!(s.pending_hooks.len(), crate::listener::MAX_PENDING_HOOKS);
    }

    #[test]
    fn settle_hooks_replies_audits_and_keeps_asks() {
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
            sync: true,
            reply: reply_tx,
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
            sync: true,
            reply: reply_tx,
        }));
        s.settle_hooks(&mut off, &audit);
        assert_eq!(s.pending_hooks.len(), 1, "ask waits for the modal");
        assert!(reply_rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err());
        let _ = std::fs::remove_file(&audit);
    }

    #[test]
    fn modal_opens_decides_and_caches_deny() {
        use crate::config::PermissionMode;
        let audit = std::env::temp_dir().join(format!(
            "forge-modal-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&audit);
        let mut off =
            crate::policy::Policy::new(PermissionMode::Off, &[], &[]).unwrap();
        let mut s = AppState::new();
        let body = r#"{"tool_name":"Bash","tool_input":{"command":"doom"}}"#.to_string();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
            hook: "PreToolUse".to_string(),
            body: body.clone(),
            sync: true,
            reply: reply_tx,
        }));
        s.settle_hooks(&mut off, &audit);
        assert!(s.open_modal_if_needed(), "ask opens the modal");
        assert!(s.pending_hooks.is_empty());
        assert_eq!(s.modal.as_ref().unwrap().modal.focused(), 0);
        s.decide_modal(false, &mut off, &audit);
        assert!(s.modal.is_none());
        let line = reply_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(line.contains(r#""decision":"deny""#), "line: {line:?}");
        // Identical request now auto-denies: no modal needed.
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
            hook: "PreToolUse".to_string(),
            body,
            sync: true,
            reply: reply_tx,
        }));
        s.settle_hooks(&mut off, &audit);
        assert!(!s.open_modal_if_needed());
        let line = reply_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(line.contains(r#""decision":"deny""#), "cached: {line:?}");
        let logged = std::fs::read_to_string(&audit).unwrap();
        assert_eq!(logged.lines().count(), 2);
        let _ = std::fs::remove_file(&audit);
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
            .spawn("one", &std::env::temp_dir(), "exec sleep 30", RunId::generate())
            .unwrap();
        let b = s
            .manager
            .spawn("two", &std::env::temp_dir(), "exec sleep 30", RunId::generate())
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
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", RunId::generate())
            .unwrap();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", RunId::generate())
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
        assert_eq!(s.sidebar_info("off").pending, 0);
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
            .spawn("w", &std::env::temp_dir(), "exit 0", RunId::generate())
            .unwrap();
        assert!(s.manager.get(id).is_some());
        assert!(s.manager.remove(id));
    }
}
