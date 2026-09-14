//! Session identity and lifecycle states.
//!
//! Pure types only: PTY ownership and the manager live here in later steps.
//! A session moves `Starting -> Running -> Exited`; exited sessions stay
//! visible until explicitly deleted.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(u64);

impl SessionId {
    pub fn fresh() -> Self {
        SessionId(SESSION_COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "s{}", self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    Starting,
    Running,
    Exited(Option<i32>),
}

impl SessionState {
    /// Exited sessions are retained but no longer live.
    pub fn is_live(&self) -> bool {
        !matches!(self, SessionState::Exited(_))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Activity {
    #[default]
    Idle,
    Thinking,
    ToolUse,
    Waiting,
    Stopped,
}

/// Harness-side session ID from a hook envelope body, when the harness
/// reports one (Claude's `session_id`; other harnesses name it
/// differently, so `thread_id` is accepted too). The envelope nests the
/// harness JSON under `body`; both layers reuse the dependency-free
/// field scanner. Unknown shapes yield None rather than a guess.
pub fn session_id_from_hook_body(body: &str) -> Option<String> {
    let nested = crate::mcp::top_raw(body, "body")?;
    crate::mcp::top_str(nested, "session_id")
        .or_else(|| crate::mcp::top_str(nested, "thread_id"))
}

/// Working directory from a hook envelope body, when the harness reports
/// one (muse's `cwd`). Same nesting as the session ID above.
pub fn cwd_from_hook_body(body: &str) -> Option<String> {
    let nested = crate::mcp::top_raw(body, "body")?;
    crate::mcp::top_str(nested, "cwd")
}

/// How long after spawn an unattributed SessionStart may still claim its
/// session. Hooks fire at harness boot, so a generous minute covers slow
/// machines without inviting stale binds.
pub const BOOTSTRAP_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// Hook event to session activity: tool gates mark ToolUse (which holds
/// broker injections), session edges mark Thinking, user-wait marks
/// Waiting, and the stop edge parks at Stopped. Unknown hooks leave the
/// current activity untouched (`None`).
pub fn activity_for_hook(hook: &str) -> Option<Activity> {
    match hook {
        "PreToolUse" | "PostToolUse" | "PermissionRequest" | "BeforeTool" | "AfterTool" => {
            Some(Activity::ToolUse)
        }
        "SessionStart" | "UserPromptSubmit" => Some(Activity::Thinking),
        "Notification" => Some(Activity::Waiting),
        "Stop" | "SessionEnd" => Some(Activity::Stopped),
        _ => None,
    }
}

/// One tab inside a session: the agent CLI or the human terminal.
/// Tab 0 is primary and decides session liveness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabKind {
    Agent,
    Terminal,
}

/// One tab: its kind plus its pane while alive. The terminal tab starts
/// panelless and spawns lazily on first switch.
pub struct Tab {
    pub kind: TabKind,
    pane: Option<crate::pty::PtyPane>,
}

/// One session: identity, human-facing metadata, lifecycle, live run
/// capability, and its tabs while alive. Exited sessions keep their record
/// (and exit code) until explicitly removed.
pub struct SessionRecord {
    pub id: SessionId,
    pub name: String,
    pub cwd: std::path::PathBuf,
    pub state: SessionState,
    pub activity: Activity,
    pub run_id: crate::ids::RunId,
    /// Harness-side conversation ID from the SessionStart hook body, when
    /// the harness reports one. This is what resume argv needs on restore.
    pub harness_session_id: Option<String>,
    /// Agent CLI behind the agent tab (`shell`, `claude`, `codex`, `muse`).
    pub cli_tool: String,
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    pub exit_code: Option<i32>,
    /// Sidebar stats: spawn instant plus hook/settlement counters. A tool
    /// hook (Pre/PostToolUse, PermissionRequest, Before/AfterTool) counts
    /// one call; every Allow/Deny verdict counts once.
    pub spawned_at: std::time::Instant,
    pub tool_calls: u32,
    pub approvals: u32,
    pub denials: u32,
}

/// Ordering, active selection, run-ID index, and pane-event channels: the
/// primary channel carries tab-0 panes, the aux channel every later tab.
pub struct SessionManager {
    order: Vec<SessionId>,
    sessions: std::collections::HashMap<SessionId, SessionRecord>,
    active: Option<SessionId>,
    run_index: std::collections::HashMap<String, SessionId>,
    pty_tx: std::sync::mpsc::Sender<(SessionId, crate::pty::PtyEvent)>,
    pty_rx: std::sync::mpsc::Receiver<(SessionId, crate::pty::PtyEvent)>,
    aux_tx: std::sync::mpsc::Sender<(SessionId, crate::pty::PtyEvent)>,
    aux_rx: std::sync::mpsc::Receiver<(SessionId, crate::pty::PtyEvent)>,
}

impl SessionManager {
    pub fn new() -> Self {
        let (pty_tx, pty_rx) = std::sync::mpsc::channel();
        let (aux_tx, aux_rx) = std::sync::mpsc::channel();
        SessionManager {
            order: Vec::new(),
            sessions: std::collections::HashMap::new(),
            active: None,
            run_index: std::collections::HashMap::new(),
            pty_tx,
            pty_rx,
            aux_tx,
            aux_rx,
        }
    }

    /// Spawn one pane, wiring the harness environment and the right event
    /// channel: tab 0 reports on the primary channel, later tabs on aux.
    fn spawn_pane(
        &self,
        id: SessionId,
        name: &str,
        cwd: &std::path::Path,
        cmd: &str,
        run_id: &str,
        cli_tool: &str,
        tab: usize,
    ) -> std::io::Result<crate::pty::PtyPane> {
        // Harnesses discover the broker through these: the run ID proves
        // authority, the endpoint inherits the listener socket.
        let tx = if tab == 0 {
            self.pty_tx.clone()
        } else {
            self.aux_tx.clone()
        };
        crate::pty::PtyPane::spawn_with_env(
            id,
            cmd,
            cwd,
            24,
            80,
            tx,
            &[
                ("FORGE_RUN_ID", run_id),
                ("FORGE_SESSION_NAME", name),
                ("FORGE_SESSION_CWD", cwd.to_string_lossy().as_ref()),
                ("FORGE_CLI_TOOL", cli_tool),
            ],
        )
    }

    fn insert_record(
        &mut self,
        id: SessionId,
        name: &str,
        cwd: &std::path::Path,
        run_id: crate::ids::RunId,
        cli_tool: &str,
        tabs: Vec<Tab>,
    ) {
        // Run IDs are minted fresh per launch so collisions should not happen;
        // if one ever does, the previous holder loses the binding (fail-safe:
        // a run ID never resolves to two sessions).
        self.run_index.insert(run_id.as_str().to_string(), id);
        self.order.push(id);
        if self.active.is_none() {
            self.active = Some(id);
        }
        self.sessions.insert(
            id,
            SessionRecord {
                id,
                name: name.to_string(),
                cwd: cwd.to_path_buf(),
                state: SessionState::Running,
                activity: Activity::Idle,
                run_id,
                harness_session_id: None,
                cli_tool: cli_tool.to_string(),
                tabs,
                active_tab: 0,
                exit_code: None,
                spawned_at: std::time::Instant::now(),
                tool_calls: 0,
                approvals: 0,
                denials: 0,
            },
        );
    }

    /// Spawn `shell -c cmd` as a new single-tab session and select it when
    /// it is the first. Duplicate names are allowed here; callers validate.
    pub fn spawn(
        &mut self,
        name: &str,
        cwd: &std::path::Path,
        cmd: &str,
        run_id: crate::ids::RunId,
        cli_tool: &str,
    ) -> std::io::Result<SessionId> {
        let id = SessionId::fresh();
        let run_str = run_id.as_str().to_string();
        let pane = self.spawn_pane(id, name, cwd, cmd, &run_str, cli_tool, 0)?;
        let tabs = vec![Tab {
            kind: TabKind::Terminal,
            pane: Some(pane),
        }];
        self.insert_record(id, name, cwd, run_id, cli_tool, tabs);
        Ok(id)
    }

    /// Spawn an agent session: the CLI on tab 0, a panelless terminal tab
    /// waiting for its first switch. Tab 0 decides session liveness.
    pub fn spawn_agent(
        &mut self,
        name: &str,
        cwd: &std::path::Path,
        cmd: &str,
        run_id: crate::ids::RunId,
        cli_tool: &str,
    ) -> std::io::Result<SessionId> {
        let id = SessionId::fresh();
        let run_str = run_id.as_str().to_string();
        let pane = self.spawn_pane(id, name, cwd, cmd, &run_str, cli_tool, 0)?;
        let tabs = vec![
            Tab {
                kind: TabKind::Agent,
                pane: Some(pane),
            },
            Tab {
                kind: TabKind::Terminal,
                pane: None,
            },
        ];
        self.insert_record(id, name, cwd, run_id, cli_tool, tabs);
        Ok(id)
    }

    /// Show one tab directly (top-bar clicks), lazily spawning the
    /// terminal pane on first view. False for unknown sessions, single-tab
    /// sessions, out-of-range tabs, and the already-visible tab.
    pub fn select_tab(&mut self, id: SessionId, index: usize) -> bool {
        let spawn_lazy = match self.sessions.get(&id) {
            None => return false,
            Some(rec) if rec.tabs.len() < 2 => return false,
            Some(rec) if index >= rec.tabs.len() || index == rec.active_tab => return false,
            Some(rec) => rec.tabs[index].pane.is_none(),
        };
        if spawn_lazy {
            let (name, cwd, run) = match self.sessions.get(&id) {
                Some(rec) => (
                    rec.name.clone(),
                    rec.cwd.clone(),
                    rec.run_id.as_str().to_string(),
                ),
                None => return false,
            };
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
            let pane = match self.spawn_pane(
                id,
                &name,
                &cwd,
                &format!("exec {shell} -i"),
                &run,
                "shell",
                index,
            ) {
                Ok(pane) => pane,
                Err(_) => return false,
            };
            if let Some(rec) = self.sessions.get_mut(&id) {
                rec.tabs[index].pane = Some(pane);
            }
        }
        if let Some(rec) = self.sessions.get_mut(&id) {
            rec.active_tab = index;
        }
        true
    }

    /// Cycle the active tab, lazily spawning the terminal pane on first
    /// switch. No-op for single-tab sessions.
    pub fn switch_tab(&mut self, id: SessionId) -> bool {
        let (next, spawn_lazy) = match self.sessions.get(&id) {
            None => return false,
            Some(rec) if rec.tabs.len() < 2 => return false,
            Some(rec) => {
                let next = (rec.active_tab + 1) % rec.tabs.len();
                (next, rec.tabs[next].pane.is_none())
            }
        };
        if spawn_lazy {
            let (name, cwd, run) = match self.sessions.get(&id) {
                Some(rec) => (
                    rec.name.clone(),
                    rec.cwd.clone(),
                    rec.run_id.as_str().to_string(),
                ),
                None => return false,
            };
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
            let pane = match self.spawn_pane(
                id,
                &name,
                &cwd,
                &format!("exec {shell} -i"),
                &run,
                "shell",
                next,
            ) {
                Ok(pane) => pane,
                Err(_) => return false,
            };
            if let Some(rec) = self.sessions.get_mut(&id) {
                rec.tabs[next].pane = Some(pane);
            }
        }
        if let Some(rec) = self.sessions.get_mut(&id) {
            rec.active_tab = next;
        }
        true
    }

    /// Kind of the visible tab, if any.
    pub fn active_tab_kind(&self, id: SessionId) -> Option<TabKind> {
        let rec = self.sessions.get(&id)?;
        rec.tabs.get(rec.active_tab).map(|t| t.kind)
    }

    /// Tab count (1 for plain shells, 2 for agent sessions).
    pub fn tab_count(&self, id: SessionId) -> usize {
        self.sessions.get(&id).map(|rec| rec.tabs.len()).unwrap_or(0)
    }

    /// The visible pane, if it has one.
    fn active_pane(&self, id: SessionId) -> Option<&crate::pty::PtyPane> {
        let rec = self.sessions.get(&id)?;
        rec.tabs.get(rec.active_tab)?.pane.as_ref()
    }

    fn active_pane_mut(&mut self, id: SessionId) -> Option<&mut crate::pty::PtyPane> {
        let rec = self.sessions.get_mut(&id)?;
        let tab = rec.active_tab;
        rec.tabs.get_mut(tab)?.pane.as_mut()
    }

    /// Replace a session's run ID, revoking the old value.
    /// Record the harness-side session ID (resume key). False when unknown.
    pub fn set_harness_session(&mut self, id: SessionId, harness: String) -> bool {
        match self.sessions.get_mut(&id) {
            None => false,
            Some(rec) => {
                rec.harness_session_id = Some(harness);
                true
            }
        }
    }

    /// Mark a session's hook activity (Idle/Stopped gates injections).
    pub fn set_activity(&mut self, id: SessionId, activity: Activity) -> bool {
        match self.sessions.get_mut(&id) {
            None => false,
            Some(rec) => {
                rec.activity = activity;
                true
            }
        }
    }

    /// Count one tool call for the sidebar stats. False when unknown.
    pub fn note_tool_call(&mut self, id: SessionId) -> bool {
        match self.sessions.get_mut(&id) {
            None => false,
            Some(rec) => {
                rec.tool_calls = rec.tool_calls.saturating_add(1);
                true
            }
        }
    }

    /// Count one policy verdict for the sidebar stats. Ask leaves both
    /// counters alone. False when unknown.
    pub fn note_verdict(&mut self, id: SessionId, decision: crate::policy::Decision) -> bool {
        match self.sessions.get_mut(&id) {
            None => false,
            Some(rec) => {
                match decision {
                    crate::policy::Decision::Allow => {
                        rec.approvals = rec.approvals.saturating_add(1)
                    }
                    crate::policy::Decision::Deny => {
                        rec.denials = rec.denials.saturating_add(1)
                    }
                    crate::policy::Decision::Ask => {}
                }
                true
            }
        }
    }

    pub fn rebind(&mut self, id: SessionId, run_id: crate::ids::RunId) -> bool {
        match self.sessions.get_mut(&id) {
            None => false,
            Some(rec) => {
                self.run_index.remove(rec.run_id.as_str());
                rec.run_id = run_id;
                self.run_index.insert(rec.run_id.as_str().to_string(), id);
                true
            }
        }
    }

    /// Resolve a run ID to its live session, if any. Exited sessions never
    /// resolve: their binding died with them.
    pub fn lookup_run(&self, run: &str) -> Option<SessionId> {
        let id = *self.run_index.get(run)?;
        match self.sessions.get(&id) {
            Some(rec) if rec.state.is_live() => Some(id),
            _ => None,
        }
    }

    /// Resolve a harness-side session ID to its live session, if any. This
    /// is the steady-state route for records whose relay could not send a
    /// run ID (muse scrubs hook-child environments).
    pub fn lookup_harness_session(&self, harness: &str) -> Option<SessionId> {
        if harness.is_empty() {
            return None;
        }
        self.sessions
            .iter()
            .find(|(_, rec)| {
                rec.state.is_live() && rec.harness_session_id.as_deref() == Some(harness)
            })
            .map(|(id, _)| *id)
    }

    /// Bind a harness-side session ID learned from an unattributed
    /// SessionStart to the session it belongs to. Succeeds only when
    /// exactly one live muse session without an ID matches the working
    /// directory and spawned recently: ambiguity (two same-cwd spawns in
    /// the window) binds nothing rather than the wrong session. True when
    /// bound.
    pub fn bind_harness_session(&mut self, harness: &str, cwd: &str) -> bool {
        if harness.is_empty() || cwd.is_empty() {
            return false;
        }
        if self.lookup_harness_session(harness).is_some() {
            return true;
        }
        let mut candidates = self.sessions.iter().filter(|(_, rec)| {
            rec.state.is_live()
                && rec.cli_tool == "muse"
                && rec.harness_session_id.is_none()
                && rec.cwd.to_string_lossy() == cwd
                && rec.spawned_at.elapsed() < BOOTSTRAP_WINDOW
        });
        let first = candidates.next();
        if first.is_some() && candidates.next().is_none() {
            let id = *first.expect("just checked Some").0;
            return self.set_harness_session(id, harness.to_string());
        }
        false
    }

    /// Kill every tab pane; the record is retained and marked exited once
    /// the primary reader reports back through [`SessionManager::drain_pty`].
    pub fn kill(&mut self, id: SessionId) -> bool {
        match self.sessions.get_mut(&id) {
            None => false,
            Some(rec) => {
                for tab in rec.tabs.iter_mut() {
                    if let Some(pane) = tab.pane.as_mut() {
                        pane.close();
                    }
                }
                true
            }
        }
    }

    /// Delete a record entirely, closing its panes first when still alive.
    pub fn remove(&mut self, id: SessionId) -> bool {
        let mut rec = match self.sessions.remove(&id) {
            None => return false,
            Some(rec) => rec,
        };
        for tab in rec.tabs.iter_mut() {
            if let Some(mut pane) = tab.pane.take() {
                pane.close();
            }
        }
        if self.run_index.get(rec.run_id.as_str()) == Some(&id) {
            self.run_index.remove(rec.run_id.as_str());
        }
        self.order.retain(|&kept| kept != id);
        if self.active == Some(id) {
            self.active = self.order.first().copied();
        }
        true
    }

    pub fn switch(&mut self, id: SessionId) -> bool {
        if self.sessions.contains_key(&id) {
            self.active = Some(id);
            true
        } else {
            false
        }
    }

    pub fn move_session(&mut self, id: SessionId, to: usize) -> bool {
        let Some(pos) = self.order.iter().position(|&kept| kept == id) else {
            return false;
        };
        self.order.remove(pos);
        let to = to.min(self.order.len());
        self.order.insert(to, id);
        true
    }

    /// Current PTY dimensions of the visible tab, if it has a live pane.
    pub fn pane_size(&self, id: SessionId) -> Option<(u16, u16)> {
        self.active_pane(id).map(|pane| pane.size())
    }

    /// Visible cursor of the visible tab as 0-based (row, col), or `None`
    /// when hidden or the pane is gone.
    pub fn cursor(&self, id: SessionId) -> Option<(u16, u16)> {
        self.active_pane(id).and_then(|pane| pane.cursor())
    }

    /// The visible tab's requested mouse protocol mode; disabled when gone.
    pub fn mouse_mode(&self, id: SessionId) -> vt100::MouseProtocolMode {
        self.active_pane(id)
            .map(|pane| pane.mouse_mode())
            .unwrap_or(vt100::MouseProtocolMode::None)
    }

    /// Whether the visible tab requested bracketed paste; false when gone.
    pub fn bracketed_paste(&self, id: SessionId) -> bool {
        self.active_pane(id)
            .is_some_and(|pane| pane.bracketed_paste())
    }

    /// The visible tab's requested mouse encoding; default when gone.
    pub fn mouse_encoding(&self, id: SessionId) -> vt100::MouseProtocolEncoding {
        self.active_pane(id)
            .map(|pane| pane.mouse_encoding())
            .unwrap_or(vt100::MouseProtocolEncoding::Default)
    }

    /// Whether the visible tab wants SS3 application-cursor arrows.
    /// False when the pane is gone (normal CSI arrows then).
    pub fn app_cursor(&self, id: SessionId) -> bool {
        self.active_pane(id)
            .is_some_and(|pane| pane.application_cursor())
    }

    /// Styled screen rows of the visible tab; empty when gone.
    pub fn styled_rows(&self, id: SessionId) -> Vec<Vec<crate::pty::FormattedCell>> {
        self.active_pane(id)
            .map(|pane| pane.styled_rows())
            .unwrap_or_default()
    }

    pub fn resize(&mut self, id: SessionId, rows: u16, cols: u16) -> std::io::Result<()> {
        match self.active_pane_mut(id) {
            None => Err(if self.sessions.contains_key(&id) {
                std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "session has no live pane",
                )
            } else {
                std::io::Error::new(std::io::ErrorKind::NotFound, "no such session")
            }),
            Some(pane) => pane.resize(rows, cols),
        }
    }

    /// Non-blocking drain of pane events; applies exit transitions
    /// (state, code, run revocation, pane release) and returns what arrived
    /// as `(session, tab, event)`. Tab 0 decides session liveness; a later
    /// tab exiting only releases its own pane.
    pub fn drain_pty(&mut self) -> Vec<(SessionId, usize, crate::pty::PtyEvent)> {
        self.drain_pty_max(usize::MAX)
    }

    /// Bounded drain: at most `max` events per call so a flooding PTY can
    /// never starve input handling or rendering. Primary tabs drain first;
    /// background terminal tabs share the remainder.
    pub fn drain_pty_max(
        &mut self,
        max: usize,
    ) -> Vec<(SessionId, usize, crate::pty::PtyEvent)> {
        let mut out = Vec::new();
        while out.len() < max {
            let Ok((id, ev)) = self.pty_rx.try_recv() else {
                break;
            };
            self.apply_exit(id, 0, &ev);
            out.push((id, 0, ev));
        }
        while out.len() < max {
            let Ok((id, ev)) = self.aux_rx.try_recv() else {
                break;
            };
            self.apply_exit(id, 1, &ev);
            out.push((id, 1, ev));
        }
        out
    }

    /// Fold one exit into record state. Primary-tab exits end the session
    /// (revoking the run binding and reaping sibling panes); later-tab
    /// exits only release that tab's pane.
    fn apply_exit(&mut self, id: SessionId, tab: usize, ev: &crate::pty::PtyEvent) {
        let crate::pty::PtyEvent::Exited(code) = ev else {
            return;
        };
        let Some(rec) = self.sessions.get_mut(&id) else {
            return;
        };
        if tab == 0 {
            rec.state = SessionState::Exited(*code);
            rec.exit_code = *code;
            let run = rec.run_id.as_str().to_string();
            self.run_index.remove(&run);
        }
        if let Some(slot) = rec.tabs.get_mut(tab) {
            slot.pane = None;
        }
        if tab == 0 {
            // Reap siblings: a dead primary leaves no session behind.
            if let Some(rec) = self.sessions.get_mut(&id) {
                for (i, slot) in rec.tabs.iter_mut().enumerate() {
                    if i != 0 {
                        if let Some(mut pane) = slot.pane.take() {
                            pane.close();
                        }
                    }
                }
            }
        }
    }

    /// Visible screen text of the visible tab, if it still has one.
    pub fn screen_text(&self, id: SessionId) -> Option<String> {
        self.active_pane(id).map(|pane| pane.screen_text())
    }

    /// Write bytes to the visible tab's child.
    pub fn pane_write(&mut self, id: SessionId, bytes: &[u8]) -> std::io::Result<()> {
        match self.active_pane_mut(id) {
            None => Err(if self.sessions.contains_key(&id) {
                std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "session has no live pane",
                )
            } else {
                std::io::Error::new(std::io::ErrorKind::NotFound, "no such session")
            }),
            Some(pane) => pane.write_all(bytes),
        }
    }

    /// Write bytes to the agent tab when one exists, else the visible tab.
    /// Broker injections always reach the agent, never a human shell.
    pub fn inject_write(&mut self, id: SessionId, bytes: &[u8]) -> std::io::Result<()> {
        let missing = std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "session has no live pane",
        );
        let Some(rec) = self.sessions.get_mut(&id) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no such session",
            ));
        };
        let agent = rec
            .tabs
            .iter()
            .position(|t| t.kind == TabKind::Agent && t.pane.is_some());
        let tab = agent.unwrap_or(rec.active_tab);
        match rec.tabs.get_mut(tab).and_then(|t| t.pane.as_mut()) {
            Some(pane) => pane.write_all(bytes),
            None => Err(missing),
        }
    }

    pub fn get(&self, id: SessionId) -> Option<&SessionRecord> {
        self.sessions.get(&id)
    }

    pub fn active(&self) -> Option<SessionId> {
        self.active
    }

    pub fn order(&self) -> &[SessionId] {
        &self.order
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_unique_and_readable() {
        let set: HashSet<u64> = (0..100).map(|_| SessionId::fresh().get()).collect();
        assert_eq!(set.len(), 100);
        assert_eq!(SessionId(7).to_string(), "s7");
    }

    #[test]
    fn liveness_matrix() {
        assert!(SessionState::Starting.is_live());
        assert!(SessionState::Running.is_live());
        assert!(!SessionState::Exited(None).is_live());
        assert!(!SessionState::Exited(Some(0)).is_live());
        assert!(!SessionState::Exited(Some(1)).is_live());
    }

    #[test]
    fn hook_body_yields_harness_id() {
        assert_eq!(
            session_id_from_hook_body(
                r#"{"v":1,"hook":"SessionStart","run_id":"r","body":{"session_id":"abc","cwd":"/tmp"}}"#
            )
            .as_deref(),
            Some("abc"),
            "claude envelope"
        );
        assert_eq!(
            session_id_from_hook_body(r#"{"v":1,"body":{"thread_id":"t-1"}}"#).as_deref(),
            Some("t-1"),
            "thread fallback"
        );
        assert_eq!(session_id_from_hook_body("{}"), None);
        assert_eq!(
            session_id_from_hook_body(r#"{"v":1,"body":{"cwd":"/tmp"}}"#),
            None,
            "no id, no guess"
        );
        assert_eq!(session_id_from_hook_body("not json"), None);
    }

    #[test]
    fn activity_defaults_idle() {
        assert_eq!(Activity::default(), Activity::Idle);
    }

    #[test]
    fn hook_events_map_to_activity() {
        assert_eq!(activity_for_hook("PreToolUse"), Some(Activity::ToolUse));
        assert_eq!(activity_for_hook("PostToolUse"), Some(Activity::ToolUse));
        assert_eq!(
            activity_for_hook("PermissionRequest"),
            Some(Activity::ToolUse)
        );
        assert_eq!(activity_for_hook("BeforeTool"), Some(Activity::ToolUse));
        assert_eq!(activity_for_hook("AfterTool"), Some(Activity::ToolUse));
        assert_eq!(activity_for_hook("SessionStart"), Some(Activity::Thinking));
        assert_eq!(
            activity_for_hook("UserPromptSubmit"),
            Some(Activity::Thinking)
        );
        assert_eq!(activity_for_hook("Notification"), Some(Activity::Waiting));
        assert_eq!(activity_for_hook("Stop"), Some(Activity::Stopped));
        assert_eq!(activity_for_hook("SessionEnd"), Some(Activity::Stopped));
        assert_eq!(activity_for_hook("Bogus"), None);
    }

    use crate::ids::RunId;
    use crate::pty::PtyEvent;
    use std::time::{Duration, Instant};

    fn workdir() -> std::path::PathBuf {
        std::env::temp_dir()
    }

    fn poll_exit(m: &mut SessionManager, id: SessionId) -> Option<i32> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            for (eid, _tab, ev) in m.drain_pty() {
                if eid == id {
                    if let PtyEvent::Exited(code) = ev {
                        return code;
                    }
                }
            }
            if Instant::now() > deadline {
                panic!("timed out waiting for {id} to exit");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn spawn_switch_reorder_remove() {
        let mut m = SessionManager::new();
        assert!(m.is_empty());
        let a = m.spawn("a", &workdir(), "exec sleep 30", RunId::generate(), "shell").unwrap();
        assert_eq!(m.active(), Some(a));
        let b = m.spawn("b", &workdir(), "exec sleep 30", RunId::generate(), "shell").unwrap();
        assert_eq!(m.len(), 2);
        assert!(m.switch(a));
        assert_eq!(m.active(), Some(a));
        assert!(!m.switch(SessionId::fresh()));
        assert!(m.move_session(b, 0));
        assert_eq!(m.order(), &[b, a]);
        assert!(!m.move_session(SessionId::fresh(), 0));
        assert!(m.remove(a));
        assert!(m.remove(b));
        assert!(m.is_empty());
        assert!(!m.remove(a));
    }

    #[test]
    fn rebind_revokes_old_run() {
        let mut m = SessionManager::new();
        let old = RunId::generate();
        let id = m.spawn("r", &workdir(), "exec sleep 30", old.clone(), "shell").unwrap();
        assert_eq!(m.lookup_run(old.as_str()), Some(id));
        let new = RunId::generate();
        assert!(m.rebind(id, new.clone()));
        assert_eq!(m.lookup_run(old.as_str()), None);
        assert_eq!(m.lookup_run(new.as_str()), Some(id));
        assert!(!m.rebind(SessionId::fresh(), RunId::generate()));
        assert!(m.remove(id));
    }

    #[test]
    fn kill_retains_exited_card_and_revokes_run() {
        let mut m = SessionManager::new();
        let run = RunId::generate();
        let id = m.spawn("k", &workdir(), "exec sleep 30", run.clone(), "shell").unwrap();
        assert!(m.kill(id));
        assert!(!m.kill(SessionId::fresh()));
        poll_exit(&mut m, id);
        let rec = m.get(id).expect("exited card retained");
        assert!(!rec.state.is_live());
        assert_eq!(m.lookup_run(run.as_str()), None);
        assert!(m.remove(id));
        assert!(m.get(id).is_none());
    }

    #[test]
    fn bounded_drain_never_starves() {
        let mut m = SessionManager::new();
        let id = m
            .spawn("flood", &workdir(), "exec yes", RunId::generate(), "shell")
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let first = m.drain_pty_max(100);
        assert!(!first.is_empty(), "flooder produced output");
        assert!(first.len() <= 100, "drain capped, took {}", first.len());
        // The flood continues: a second capped drain still finds events,
        // proving the first call left the rest queued instead of dropping.
        // Poll briefly: under load the reader thread may not have refilled
        // the channel between two back-to-back drains.
        let deadline = Instant::now() + Duration::from_secs(10);
        let second = loop {
            let drained = m.drain_pty_max(100);
            if !drained.is_empty() || Instant::now() > deadline {
                break drained;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(!second.is_empty());
        assert!(m.remove(id));
    }

    #[test]
    fn pane_write_reaches_child() {
        let mut m = SessionManager::new();
        let id = m
            .spawn("w", &workdir(), "exec cat", RunId::generate(), "shell")
            .unwrap();
        m.pane_write(id, b"via-manager\n").unwrap();
        assert!(m.pane_write(SessionId::fresh(), b"x").is_err());
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = Vec::new();
        loop {
            for (_, _, ev) in m.drain_pty() {
                if let PtyEvent::Output(b) = ev {
                    seen.extend_from_slice(&b);
                }
            }
            if seen.windows(11).any(|w| w == b"via-manager") {
                break;
            }
            if Instant::now() > deadline {
                panic!("write never echoed: {seen:?}");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(m.remove(id));
    }

    #[test]
    fn written_bytes_reach_screen() {
        let mut m = SessionManager::new();
        let id = m
            .spawn("w", &workdir(), "exec cat", RunId::generate(), "shell")
            .unwrap();
        m.pane_write(id, b"hello-screen-write\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(text) = m.screen_text(id) {
                if text.contains("hello-screen-write") {
                    break;
                }
            }
            if Instant::now() > deadline {
                panic!(
                    "write never reached screen: {:?}",
                    m.screen_text(id)
                );
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(m.remove(id));
    }

    #[test]
    fn quick_exit_records_code() {
        let mut m = SessionManager::new();
        let id = m.spawn("q", &workdir(), "exit 7", RunId::generate(), "shell").unwrap();
        assert_eq!(poll_exit(&mut m, id), Some(7));
        let rec = m.get(id).unwrap();
        assert_eq!(rec.state, SessionState::Exited(Some(7)));
        assert_eq!(rec.exit_code, Some(7));
        assert!(m.remove(id));
    }

    #[test]
    fn select_tab_targets_directly_with_lazy_spawn() {
        let mut m = SessionManager::new();
        let id = m
            .spawn_agent("agent", &workdir(), "exec cat", RunId::generate(), "codex")
            .unwrap();
        assert!(m.select_tab(id, 1));
        assert_eq!(m.active_tab_kind(id), Some(TabKind::Terminal));
        assert!(!m.select_tab(id, 1), "already visible is a no-op");
        assert!(!m.select_tab(id, 7), "out of range");
        assert!(!m.select_tab(SessionId::fresh(), 0));
        assert!(m.select_tab(id, 0));
        assert_eq!(m.active_tab_kind(id), Some(TabKind::Agent));
        let solo = m
            .spawn("s", &workdir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        assert!(!m.select_tab(solo, 0), "single-tab refuses");
        assert!(m.remove(id));
        assert!(m.remove(solo));
    }

    #[test]
    fn agent_session_opens_dual_tabs_with_lazy_terminal() {
        let mut m = SessionManager::new();
        let id = m
            .spawn_agent("agent", &workdir(), "exec cat", RunId::generate(), "codex")
            .unwrap();
        assert_eq!(m.tab_count(id), 2);
        assert_eq!(m.active_tab_kind(id), Some(TabKind::Agent));
        // Single-tab shells refuse to cycle.
        let solo = m
            .spawn("s", &workdir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        assert_eq!(m.tab_count(solo), 1);
        assert!(!m.switch_tab(solo));
        assert!(!m.switch_tab(SessionId::fresh()));
        // First switch lazily spawns the human shell at a live size.
        assert!(m.switch_tab(id));
        assert_eq!(m.active_tab_kind(id), Some(TabKind::Terminal));
        assert!(m.pane_size(id).is_some());
        // Cycling wraps back to the agent tab.
        assert!(m.switch_tab(id));
        assert_eq!(m.active_tab_kind(id), Some(TabKind::Agent));
        assert!(m.remove(id));
        assert!(m.remove(solo));
    }

    #[test]
    fn inject_write_reaches_agent_behind_terminal_tab() {
        let mut m = SessionManager::new();
        let id = m
            .spawn_agent("a", &workdir(), "exec cat", RunId::generate(), "codex")
            .unwrap();
        assert!(m.switch_tab(id)); // now looking at the human shell
        m.inject_write(id, b"to-agent\n").unwrap();
        assert!(m.inject_write(SessionId::fresh(), b"x").is_err());
        // Cycle back to the agent tab and read its screen: cat echoed it.
        assert!(m.switch_tab(id));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(text) = m.screen_text(id) {
                if text.contains("to-agent") {
                    break;
                }
            }
            if Instant::now() > deadline {
                panic!("injection never reached the agent tab");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(m.remove(id));
    }

    #[test]
    fn terminal_tab_exit_keeps_session_alive() {
        let mut m = SessionManager::new();
        let run = RunId::generate();
        let id = m
            .spawn_agent("a", &workdir(), "exec cat", run.clone(), "codex")
            .unwrap();
        assert!(m.switch_tab(id));
        // Close only the human shell; the agent tab keeps the session live.
        m.pane_write(id, b"exit\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let aux_gone = m.drain_pty().iter().any(|(eid, tab, ev)| {
                *eid == id && *tab == 1 && matches!(ev, PtyEvent::Exited(_))
            });
            if aux_gone {
                break;
            }
            if Instant::now() > deadline {
                panic!("terminal tab never exited");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let rec = m.get(id).expect("session survives its terminal tab");
        assert!(rec.state.is_live());
        assert_eq!(m.lookup_run(run.as_str()), Some(id));
        assert!(m.remove(id));
    }

    #[test]
    fn primary_exit_ends_agent_session() {
        let mut m = SessionManager::new();
        let run = RunId::generate();
        let id = m
            .spawn_agent("a", &workdir(), "exit 3", run.clone(), "codex")
            .unwrap();
        assert!(m.switch_tab(id)); // lazy terminal beside a dying agent
        assert_eq!(poll_exit(&mut m, id), Some(3));
        let rec = m.get(id).unwrap();
        assert_eq!(rec.state, SessionState::Exited(Some(3)));
        assert_eq!(m.lookup_run(run.as_str()), None);
        assert!(m.pane_size(id).is_none(), "visible agent pane released");
        assert!(m.remove(id));
    }

    #[test]
    fn cwd_parses_from_hook_envelope() {
        let line = r#"{"v":1,"hook":"SessionStart","run_id":"","body":{"session_id":"s-1","cwd":"/tmp/work"}}"#;
        assert_eq!(cwd_from_hook_body(line).as_deref(), Some("/tmp/work"));
        assert_eq!(session_id_from_hook_body(line).as_deref(), Some("s-1"));
        assert_eq!(cwd_from_hook_body(r#"{"v":1,"body":{}}"#), None);
        assert_eq!(cwd_from_hook_body("not json"), None);
    }

    #[test]
    fn harness_bootstrap_binds_unique_muse_session() {
        let mut m = SessionManager::new();
        let cwd = workdir();
        let id = m
            .spawn_agent("m", &cwd, "exec sleep 30", RunId::generate(), "muse")
            .unwrap();
        let cwd_str = cwd.to_string_lossy().into_owned();
        assert!(m.bind_harness_session("sid-1", &cwd_str));
        assert_eq!(
            m.get(id).unwrap().harness_session_id.as_deref(),
            Some("sid-1")
        );
        assert_eq!(m.lookup_harness_session("sid-1"), Some(id));
        assert_eq!(m.lookup_harness_session(""), None);
        assert_eq!(m.lookup_harness_session("nope"), None);
        assert!(m.remove(id));
    }

    #[test]
    fn harness_bootstrap_refuses_ambiguity_and_strangers() {
        let mut m = SessionManager::new();
        let cwd = workdir();
        let cwd_str = cwd.to_string_lossy().into_owned();
        // Two same-cwd muse sessions: neither may claim the ID.
        let a = m
            .spawn_agent("a", &cwd, "exec sleep 30", RunId::generate(), "muse")
            .unwrap();
        let b = m
            .spawn_agent("b", &cwd, "exec sleep 30", RunId::generate(), "muse")
            .unwrap();
        assert!(!m.bind_harness_session("sid-x", &cwd_str));
        assert_eq!(m.get(a).unwrap().harness_session_id, None);
        assert_eq!(m.get(b).unwrap().harness_session_id, None);
        assert!(m.remove(a));
        assert!(m.remove(b));
        // A claude session never matches a muse bootstrap, nor does a
        // foreign cwd or an empty ID.
        let c = m
            .spawn_agent("c", &cwd, "exec sleep 30", RunId::generate(), "claude")
            .unwrap();
        assert!(!m.bind_harness_session("sid-y", &cwd_str));
        assert!(!m.bind_harness_session("sid-y", "/no/such/dir"));
        assert!(!m.bind_harness_session("", &cwd_str));
        assert!(m.remove(c));
    }
}
