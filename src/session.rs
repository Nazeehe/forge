//! Session identity and lifecycle states.
//!
//! Pure types only: PTY ownership and the manager live here in later steps.
//! A session moves `Starting -> Running -> Exited`; exited sessions stay
//! visible until explicitly deleted.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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

/// One tab inside a session: the agent CLI, the human terminal, or the
/// lazygit SCM view. Tab 0 is primary and decides session liveness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabKind {
    Agent,
    Terminal,
    Scm,
}

/// One tab: its kind plus its pane while alive. The terminal and SCM
/// tabs start panelless and spawn lazily on first switch.
pub struct Tab {
    pub kind: TabKind,
    pane: Option<crate::pty::PtyPane>,
}

/// Lazy-tab command plus harness tag. The agent tab always spawns with
/// the session, so it has no lazy command.
fn lazy_tab_cmd(kind: TabKind) -> Option<(String, &'static str)> {
    match kind {
        TabKind::Terminal => {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
            Some((format!("exec {shell} -i"), "shell"))
        }
        // Missing binary is fine: the shell reports it and the pane
        // exits, which the tab renders like any dead child.
        TabKind::Scm => Some(("exec lazygit".to_string(), "lazygit")),
        TabKind::Agent => None,
    }
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
    /// Harness-side conversation ID from the SessionStart/UserPromptSubmit
    /// hook body, when the harness reports one. This is what resume argv
    /// needs on restore.
    pub harness_session_id: Option<String>,
    /// Agent CLI behind the agent tab (`shell`, `claude`, `codex`, `muse`).
    pub cli_tool: String,
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    pub exit_code: Option<i32>,
    /// Caller sticky status (blueprint #14/#15): one replaceable
    /// `kind: message` pair, shown in the sidebar.
    pub status: Option<crate::session_status::SessionStatus>,
    /// Spawn instant for the bootstrap attribution window.
    pub spawned_at: std::time::Instant,
}

/// Ordering, active selection, run-ID index, and pane-event channels: the
/// primary channel carries tab-0 panes, the aux channel every later tab.
pub struct SessionManager {
    order: Vec<SessionId>,
    sessions: std::collections::HashMap<SessionId, SessionRecord>,
    active: Option<SessionId>,
    run_index: std::collections::HashMap<String, SessionId>,
    pty_tx: std::sync::mpsc::SyncSender<(SessionId, crate::pty::PtyEvent)>,
    pty_rx: std::sync::mpsc::Receiver<(SessionId, crate::pty::PtyEvent)>,
    aux_tx: std::sync::mpsc::SyncSender<(SessionId, crate::pty::PtyEvent)>,
    aux_rx: std::sync::mpsc::Receiver<(SessionId, crate::pty::PtyEvent)>,
}

/// Bound on each pane-event channel (AGENTS.md: "all... queues... bounded").
/// `drain_pty_max` only ever pulls `MAX_DRAIN` (100) events per tick, so
/// this gives several ticks of burst headroom before a flooding session's
/// reader thread has to block on `send` (backpressure), rather than the
/// channel — and its memory — growing without limit.
pub const PTY_CHANNEL_CAPACITY: usize = 1024;

impl SessionManager {
    pub fn new() -> Self {
        let (pty_tx, pty_rx) = std::sync::mpsc::sync_channel(PTY_CHANNEL_CAPACITY);
        let (aux_tx, aux_rx) = std::sync::mpsc::sync_channel(PTY_CHANNEL_CAPACITY);
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
                status: None,
                spawned_at: std::time::Instant::now(),
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

    /// Spawn an agent session: the CLI on tab 0, a panelless terminal
    /// tab and a panelless lazygit tab waiting for their first switch.
    /// Tab 0 decides session liveness.
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
            Tab {
                kind: TabKind::Scm,
                pane: None,
            },
        ];
        self.insert_record(id, name, cwd, run_id, cli_tool, tabs);
        Ok(id)
    }

    /// Spawn a panelless lazy tab (terminal shell or lazygit). False
    /// when the tab is gone, already live, not lazily spawnable, or the
    /// spawn fails.
    fn spawn_lazy_tab(&mut self, id: SessionId, index: usize) -> bool {
        let (name, cwd, run, kind) = match self.sessions.get(&id) {
            Some(rec) => (
                rec.name.clone(),
                rec.cwd.clone(),
                rec.run_id.as_str().to_string(),
                rec.tabs.get(index).map(|tab| tab.kind),
            ),
            None => return false,
        };
        let Some(kind) = kind else { return false };
        let Some((cmd, tool)) = lazy_tab_cmd(kind) else {
            return false;
        };
        let pane = match self.spawn_pane(id, &name, &cwd, &cmd, &run, tool, index) {
            Ok(pane) => pane,
            Err(_) => return false,
        };
        if let Some(rec) = self.sessions.get_mut(&id) {
            if let Some(tab) = rec.tabs.get_mut(index) {
                tab.pane = Some(pane);
            }
        }
        true
    }

    /// Show one tab directly (top-bar clicks), lazily spawning panelless
    /// tabs on first view. False for unknown sessions, single-tab
    /// sessions, out-of-range tabs, and the already-visible tab.
    pub fn select_tab(&mut self, id: SessionId, index: usize) -> bool {
        let spawn_lazy = match self.sessions.get(&id) {
            None => return false,
            Some(rec) if rec.tabs.len() < 2 => return false,
            Some(rec) if index >= rec.tabs.len() || index == rec.active_tab => return false,
            Some(rec) => rec.tabs[index].pane.is_none(),
        };
        if spawn_lazy && !self.spawn_lazy_tab(id, index) {
            return false;
        }
        if let Some(rec) = self.sessions.get_mut(&id) {
            rec.active_tab = index;
        }
        true
    }

    /// Cycle the active tab, lazily spawning panelless tabs on first
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
        if spawn_lazy && !self.spawn_lazy_tab(id, next) {
            return false;
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

    /// Tab count (1 for plain shells, 3 for agent sessions).
    pub fn tab_count(&self, id: SessionId) -> usize {
        self.sessions.get(&id).map(|rec| rec.tabs.len()).unwrap_or(0)
    }

    /// The visible pane, if it has one.
    fn active_pane(&self, id: SessionId) -> Option<&crate::pty::PtyPane> {
        let rec = self.sessions.get(&id)?;
        rec.tabs.get(rec.active_tab)?.pane.as_ref()
    }

    /// Test seam (plus future pane surgery): kill or replace the live
    /// pane while the record stays put, e.g. to prove a failed write
    /// retries instead of dropping the message.
    pub(crate) fn active_pane_mut(&mut self, id: SessionId) -> Option<&mut crate::pty::PtyPane> {
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

    /// Unix process-session ID of an agent's primary PTY. Portable-pty makes
    /// that child a session leader before exec, and hook descendants inherit
    /// the same value even when a harness clears their environment.
    pub fn process_session_id(&self, id: SessionId) -> Option<u32> {
        let rec = self.sessions.get(&id)?;
        rec.tabs.first()?.pane.as_ref()?.process_session_id()
    }

    /// Resolve a hook relay's Unix process-session ID to its live Forge
    /// session. Unlike cwd matching, this remains unambiguous when several
    /// Muse panes run in the same project.
    pub fn lookup_process_session(&self, source_sid: u32) -> Option<SessionId> {
        if source_sid == 0 {
            return None;
        }
        self.sessions.iter().find_map(|(id, rec)| {
            (rec.state.is_live()
                && rec
                    .tabs
                    .first()
                    .and_then(|tab| tab.pane.as_ref())
                    .and_then(crate::pty::PtyPane::process_session_id)
                    == Some(source_sid))
            .then_some(*id)
        })
    }

    /// Bind an ID from a scrubbed-environment harness hook to the exact PTY
    /// process session that emitted it. Existing bindings are never replaced,
    /// and an ID already owned by another live session is rejected.
    pub fn bind_harness_session_to_process(&mut self, harness: &str, source_sid: u32) -> bool {
        if harness.is_empty() {
            return false;
        }
        let Some(id) = self.lookup_process_session(source_sid) else {
            return false;
        };
        if let Some(owner) = self.lookup_harness_session(harness) {
            return owner == id;
        }
        let Some(rec) = self.sessions.get(&id) else {
            return false;
        };
        if !crate::harness::Harness::from_name(&rec.cli_tool)
            .is_some_and(|h| h.cwd_window_attribution())
        {
            return false;
        }
        match rec.harness_session_id.as_deref() {
            Some(existing) => existing == harness,
            None => self.set_harness_session(id, harness.to_string()),
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
        // Muse scrubs the hook env and reports its cwd without a trailing
        // slash, while the create dialog keeps the slash the user typed
        // (`PathBuf` preserves it). Compare paths (components), not strings.
        let hook_cwd = std::path::Path::new(cwd);
        let mut candidates = self.sessions.iter().filter(|(_, rec)| {
            rec.state.is_live()
                && crate::harness::Harness::from_name(&rec.cli_tool)
                    .is_some_and(|h| h.cwd_window_attribution())
                && rec.harness_session_id.is_none()
                && rec.cwd.as_path() == hook_cwd
                && rec.spawned_at.elapsed() < BOOTSTRAP_WINDOW
        });
        let first = candidates.next();
        if first.is_some() && candidates.next().is_none() {
            let id = *first.expect("just checked Some").0;
            return self.set_harness_session(id, harness.to_string());
        }
        false
    }

    /// SIGTERM every live pane's child, then wait up to `grace` total for
    /// all of them to exit on their own (agents flush state on TERM).
    /// Already-exited records are skipped. Returns the number of panes
    /// still alive afterwards: the caller should `close()`/drop those
    /// (SIGKILL). Never blocks longer than `grace`, and never reaps —
    /// reader threads report the real exit codes as usual.
    pub fn shutdown_gracefully(&mut self, grace: Duration) -> usize {
        for rec in self.sessions.values() {
            if !rec.state.is_live() {
                continue;
            }
            for tab in rec.tabs.iter() {
                if let Some(pane) = tab.pane.as_ref() {
                    pane.terminate();
                }
            }
        }
        let deadline = Instant::now() + grace;
        loop {
            let alive = self
                .sessions
                .values()
                .filter(|rec| rec.state.is_live())
                .flat_map(|rec| rec.tabs.iter())
                .filter_map(|tab| tab.pane.as_ref())
                .filter(|pane| pane.child_alive())
                .count();
            if alive == 0 {
                return 0;
            }
            if Instant::now() >= deadline {
                return alive;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
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

    /// Cluster group buttons: starting at the first listed session, place
    /// each next one directly to its right. Order follows the given list
    /// (callers pass group order); unknown IDs vanish, duplicates collapse
    /// to first use, and empty/singleton sets are a no-op. Other sessions
    /// keep their relative order around the block.
    pub fn cluster_sessions(&mut self, ids: &[SessionId]) {
        use std::collections::HashSet;
        let mut seen = HashSet::new();
        let mut block: Vec<SessionId> = Vec::with_capacity(ids.len());
        for &id in ids {
            if seen.insert(id) && self.order.contains(&id) {
                block.push(id);
            }
        }
        if block.len() < 2 {
            return;
        }
        let anchor = self
            .order
            .iter()
            .position(|id| *id == block[0])
            .unwrap_or(0);
        let wanted: HashSet<SessionId> = block.iter().copied().collect();
        self.order.retain(|id| !wanted.contains(id));
        let anchor = anchor.min(self.order.len());
        for (i, id) in block.into_iter().enumerate() {
            self.order.insert(anchor + i, id);
        }
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

    /// Whether the visible tab is on the alternate screen; false when gone.
    pub fn alternate_screen(&self, id: SessionId) -> bool {
        self.active_pane(id)
            .is_some_and(|pane| pane.alternate_screen())
    }

    /// Scroll the visible tab: positive climbs, negative returns toward
    /// live. On the normal screen this moves the scrollback viewport
    /// (see [`crate::pty::PtyPane::scroll_viewport`]); on the alternate
    /// screen there is no scrollback, so the wheel becomes Up/Down arrows
    /// instead — fullscreen apps that never take the mouse (codex,
    /// claude) scroll with those, and a dead wheel would strand the user.
    pub fn scroll_view(&mut self, id: SessionId, lines: i32) {
        let (alt, app_cursor) = match self.active_pane(id) {
            None => return,
            Some(pane) => (pane.alternate_screen(), pane.application_cursor()),
        };
        if !alt {
            if let Some(pane) = self.active_pane(id) {
                pane.scroll_viewport(lines);
            }
            return;
        }
        let code = if lines >= 0 {
            KeyCode::Up
        } else {
            KeyCode::Down
        };
        let key = KeyEvent::new(code, KeyModifiers::NONE);
        if let Some(seq) = crate::input::encode_key(&key, app_cursor) {
            let _ = self.pane_write(id, &seq.repeat(lines.unsigned_abs() as usize));
        }
    }

    /// Write bytes to the visible tab's child. Typing returns a scrolled
    /// view to the live tail first: input means the human is back.
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
            Some(pane) => {
                pane.reset_viewport();
                pane.write_all(bytes)
            }
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
            Some(pane) => {
                // An injection is news: show it live rather than under a
                // scrolled-back view.
                pane.reset_viewport();
                pane.write_all(bytes)
            }
            None => Err(missing),
        }
    }

    pub fn get(&self, id: SessionId) -> Option<&SessionRecord> {
        self.sessions.get(&id)
    }

    pub fn get_mut(&mut self, id: SessionId) -> Option<&mut SessionRecord> {
        self.sessions.get_mut(&id)
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
    fn cluster_sessions_groups_members_next_to_first() {
        // Feature: adding a session to a group clusters all group buttons
        // together, starting at the first member with the rest to its right.
        let mut m = SessionManager::new();
        let a = m.spawn("a", &workdir(), "exec sleep 30", RunId::generate(), "shell").unwrap();
        let b = m.spawn("b", &workdir(), "exec sleep 30", RunId::generate(), "shell").unwrap();
        let c = m.spawn("c", &workdir(), "exec sleep 30", RunId::generate(), "shell").unwrap();
        let d = m.spawn("d", &workdir(), "exec sleep 30", RunId::generate(), "shell").unwrap();
        assert_eq!(m.order(), &[a, b, c, d]);
        m.cluster_sessions(&[b, d]);
        assert_eq!(m.order(), &[a, b, d, c], "d joins b's cluster");
        m.cluster_sessions(&[d, b]);
        assert_eq!(m.order(), &[a, c, d, b], "listed order decides placement");
        m.cluster_sessions(&[a, c, d]);
        assert_eq!(m.order(), &[a, c, d, b], "anchor at first member a");
        m.cluster_sessions(&[b]);
        assert_eq!(m.order(), &[a, c, d, b], "single member is a no-op");
        m.cluster_sessions(&[]);
        assert_eq!(m.order(), &[a, c, d, b], "empty is a no-op");
        m.cluster_sessions(&[SessionId::fresh()]);
        assert_eq!(m.order(), &[a, c, d, b], "unknown ids ignored");
        assert!(m.remove(a));
        assert!(m.remove(b));
        assert!(m.remove(c));
        assert!(m.remove(d));
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
    fn shutdown_gracefully_terms_all_live_panes() {
        let mut m = SessionManager::new();
        // Exit code 3 proves each trap ran: signal-death reports code 1.
        // READY is printed after the trap installs, so shutdown's SIGTERM
        // deterministically runs it instead of racing trap installation.
        let a = m
            .spawn(
                "a",
                &workdir(),
                "trap 'exit 3' TERM; echo READY-A; while :; do :; done",
                RunId::generate(),
                "shell",
            )
            .unwrap();
        let b = m
            .spawn(
                "b",
                &workdir(),
                "trap 'exit 3' TERM; echo READY-B; while :; do :; done",
                RunId::generate(),
                "shell",
            )
            .unwrap();
        let (mut ba, mut bb) = (Vec::new(), Vec::new());
        let deadline = Instant::now() + Duration::from_secs(10);
        while !(ba.windows(7).any(|w| w == b"READY-A") && bb.windows(7).any(|w| w == b"READY-B"))
        {
            for (eid, _tab, ev) in m.drain_pty() {
                match ev {
                    PtyEvent::Output(bytes) => {
                        if eid == a {
                            ba.extend_from_slice(&bytes);
                        } else if eid == b {
                            bb.extend_from_slice(&bytes);
                        }
                    }
                    PtyEvent::Exited(code) => {
                        panic!("session {eid} died before READY: {code:?}")
                    }
                }
            }
            if Instant::now() > deadline {
                panic!("READY never arrived: a={ba:?} b={bb:?}");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(m.shutdown_gracefully(Duration::from_secs(10)), 0);
        // One collecting drain: poll_exit-style per-id drains would eat
        // the other session's Exited (drain_pty yields every session's
        // events, and unconsumed entries are dropped with the Vec).
        let mut codes = std::collections::HashMap::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while codes.len() < 2 {
            for (eid, _tab, ev) in m.drain_pty() {
                if let PtyEvent::Exited(code) = ev {
                    codes.insert(eid, code);
                }
            }
            if Instant::now() > deadline {
                panic!("missing exits: {codes:?}");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(codes.get(&a), Some(&Some(3)));
        assert_eq!(codes.get(&b), Some(&Some(3)));
        assert!(m.remove(a));
        assert!(m.remove(b));
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
        // The SCM tab lazily spawns its lazygit pane on first view.
        assert!(m.select_tab(id, 2));
        assert_eq!(m.active_tab_kind(id), Some(TabKind::Scm));
        let solo = m
            .spawn("s", &workdir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        assert!(!m.select_tab(solo, 0), "single-tab refuses");
        assert!(m.remove(id));
        assert!(m.remove(solo));
    }

    #[test]
    fn agent_session_opens_three_tabs_with_lazy_extras() {
        let mut m = SessionManager::new();
        let id = m
            .spawn_agent("agent", &workdir(), "exec cat", RunId::generate(), "codex")
            .unwrap();
        assert_eq!(m.tab_count(id), 3);
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
        // Next comes the lazygit tab, then cycling wraps to the agent.
        assert!(m.switch_tab(id));
        assert_eq!(m.active_tab_kind(id), Some(TabKind::Scm));
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
        // Back to the agent tab and read its screen: cat echoed it.
        assert!(m.select_tab(id, 0));
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
    fn harness_bootstrap_ignores_trailing_slash() {
        // Grounded in live muse payloads: the hook reports
        // `{"cwd":"/work","hook_event_name":"SessionStart","session_id":"..."}`,
        // no trailing slash, while the dialog keeps the trailing slash the
        // user typed (`PathBuf` preserves it). The bind must compare paths,
        // not strings, or every slashed muse session stays null.
        let mut m = SessionManager::new();
        let cwd = workdir();
        let slashed = format!("{}/", cwd.to_string_lossy().trim_end_matches('/'));
        let slashed_path = std::path::PathBuf::from(&slashed);
        let id = m
            .spawn_agent("m", &slashed_path, "exec sleep 30", RunId::generate(), "muse")
            .unwrap();
        let hook_cwd = cwd.to_string_lossy().trim_end_matches('/').to_owned();
        assert!(m.bind_harness_session("sid-slash", &hook_cwd));
        assert_eq!(
            m.get(id).unwrap().harness_session_id.as_deref(),
            Some("sid-slash")
        );
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

    #[test]
    fn pty_process_session_identifies_one_of_two_same_cwd_muse_sessions() {
        let mut m = SessionManager::new();
        let cwd = workdir();
        let a = m
            .spawn_agent("a", &cwd, "exec sleep 30", RunId::generate(), "muse")
            .unwrap();
        let b = m
            .spawn_agent("b", &cwd, "exec sleep 30", RunId::generate(), "muse")
            .unwrap();

        let a_sid = m.process_session_id(a).expect("first PTY process session");
        let b_sid = m.process_session_id(b).expect("second PTY process session");
        assert_ne!(a_sid, b_sid, "each PTY owns a distinct Unix session");
        assert_eq!(m.lookup_process_session(a_sid), Some(a));
        assert_eq!(m.lookup_process_session(b_sid), Some(b));
        assert_eq!(m.lookup_process_session(0), None);

        assert!(m.remove(a));
        assert!(m.remove(b));
    }
}
