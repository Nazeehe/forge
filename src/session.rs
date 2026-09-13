//! Session identity and lifecycle states.
//!
//! Pure types only: PTY ownership and the manager live here in later steps.
//! A session moves `Starting -> Running -> Exited`; exited sessions stay
//! visible until explicitly deleted.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

/// One session: identity, human-facing metadata, lifecycle, live run
/// capability, and its pane while alive. Exited sessions keep their record
/// (and exit code) until explicitly removed.
pub struct SessionRecord {
    pub id: SessionId,
    pub name: String,
    pub cwd: std::path::PathBuf,
    pub state: SessionState,
    pub activity: Activity,
    pub run_id: crate::ids::RunId,
    pub exit_code: Option<i32>,
    pane: Option<crate::pty::PtyPane>,
}

/// Ordering, active selection, run-ID index, and pane-event channel.
pub struct SessionManager {
    order: Vec<SessionId>,
    sessions: std::collections::HashMap<SessionId, SessionRecord>,
    active: Option<SessionId>,
    run_index: std::collections::HashMap<String, SessionId>,
    pty_tx: std::sync::mpsc::Sender<(SessionId, crate::pty::PtyEvent)>,
    pty_rx: std::sync::mpsc::Receiver<(SessionId, crate::pty::PtyEvent)>,
}

impl SessionManager {
    pub fn new() -> Self {
        let (pty_tx, pty_rx) = std::sync::mpsc::channel();
        SessionManager {
            order: Vec::new(),
            sessions: std::collections::HashMap::new(),
            active: None,
            run_index: std::collections::HashMap::new(),
            pty_tx,
            pty_rx,
        }
    }

    /// Raw pane events for the main loop (translated to `AppEvent` there).
    pub fn pty_events(&self) -> &std::sync::mpsc::Receiver<(SessionId, crate::pty::PtyEvent)> {
        &self.pty_rx
    }

    /// Spawn `shell -c cmd` as a new running session and select it when it
    /// is the first. Duplicate names are allowed here; callers validate.
    pub fn spawn(
        &mut self,
        name: &str,
        cwd: &std::path::Path,
        cmd: &str,
        run_id: crate::ids::RunId,
    ) -> std::io::Result<SessionId> {
        let id = SessionId::fresh();
        let pane = crate::pty::PtyPane::spawn(id, cmd, cwd, 24, 80, self.pty_tx.clone())?;
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
                exit_code: None,
                pane: Some(pane),
            },
        );
        Ok(id)
    }

    /// Replace a session's run ID, revoking the old value.
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

    /// Kill the pane; the record is retained and marked exited once the
    /// reader reports back through [`SessionManager::drain_pty`].
    pub fn kill(&mut self, id: SessionId) -> bool {
        match self.sessions.get_mut(&id) {
            None => false,
            Some(rec) => {
                if let Some(pane) = rec.pane.as_mut() {
                    pane.close();
                }
                true
            }
        }
    }

    /// Delete a record entirely, closing its pane first when still alive.
    pub fn remove(&mut self, id: SessionId) -> bool {
        let mut rec = match self.sessions.remove(&id) {
            None => return false,
            Some(rec) => rec,
        };
        if let Some(mut pane) = rec.pane.take() {
            pane.close();
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

    /// Current PTY dimensions of a session's live pane, if it has one.
    pub fn pane_size(&self, id: SessionId) -> Option<(u16, u16)> {
        self.sessions.get(&id)?.pane.as_ref().map(|pane| pane.size())
    }

    /// Visible cursor of a session's live pane as 0-based (row, col), or
    /// `None` when hidden or the pane is gone.
    pub fn cursor(&self, id: SessionId) -> Option<(u16, u16)> {
        self.sessions.get(&id)?.pane.as_ref().and_then(|pane| pane.cursor())
    }

    /// Whether the pane's application wants SS3 application-cursor arrows.
    /// False when the pane is gone (normal CSI arrows then).
    pub fn app_cursor(&self, id: SessionId) -> bool {
        self.sessions
            .get(&id)
            .and_then(|rec| rec.pane.as_ref())
            .is_some_and(|pane| pane.application_cursor())
    }

    /// Styled screen rows of a session's live pane; empty when gone.
    pub fn styled_rows(&self, id: SessionId) -> Vec<Vec<crate::pty::FormattedCell>> {
        self.sessions
            .get(&id)
            .and_then(|rec| rec.pane.as_ref())
            .map(|pane| pane.styled_rows())
            .unwrap_or_default()
    }

    pub fn resize(&mut self, id: SessionId, rows: u16, cols: u16) -> std::io::Result<()> {
        match self.sessions.get_mut(&id) {
            None => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no such session",
            )),
            Some(rec) => match rec.pane.as_mut() {
                Some(pane) => pane.resize(rows, cols),
                None => Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "session has no live pane",
                )),
            },
        }
    }

    /// Non-blocking drain of pane events; applies exit transitions
    /// (state, code, run revocation, pane release) and returns what arrived.
    pub fn drain_pty(&mut self) -> Vec<(SessionId, crate::pty::PtyEvent)> {
        self.drain_pty_max(usize::MAX)
    }

    /// Bounded drain: at most `max` events per call so a flooding PTY can
    /// never starve input handling or rendering.
    pub fn drain_pty_max(&mut self, max: usize) -> Vec<(SessionId, crate::pty::PtyEvent)> {
        let mut out = Vec::new();
        while out.len() < max {
            let Ok((id, ev)) = self.pty_rx.try_recv() else {
                break;
            };
            if let crate::pty::PtyEvent::Exited(code) = &ev {
                if let Some(rec) = self.sessions.get_mut(&id) {
                    rec.state = SessionState::Exited(*code);
                    rec.exit_code = *code;
                    rec.pane = None;
                    let run = rec.run_id.as_str().to_string();
                    self.run_index.remove(&run);
                }
            }
            out.push((id, ev));
        }
        out
    }

    /// Visible screen text of a live pane, if it still has one.
    pub fn screen_text(&self, id: SessionId) -> Option<String> {
        self.sessions
            .get(&id)?
            .pane
            .as_ref()
            .map(|pane| pane.screen_text())
    }

    /// Write bytes to a live pane's child.
    pub fn pane_write(&mut self, id: SessionId, bytes: &[u8]) -> std::io::Result<()> {
        match self.sessions.get_mut(&id) {
            None => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no such session",
            )),
            Some(rec) => match rec.pane.as_mut() {
                Some(pane) => pane.write_all(bytes),
                None => Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "session has no live pane",
                )),
            },
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
    fn activity_defaults_idle() {
        assert_eq!(Activity::default(), Activity::Idle);
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
            for (eid, ev) in m.drain_pty() {
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
        let a = m.spawn("a", &workdir(), "exec sleep 30", RunId::generate()).unwrap();
        assert_eq!(m.active(), Some(a));
        let b = m.spawn("b", &workdir(), "exec sleep 30", RunId::generate()).unwrap();
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
        let id = m.spawn("r", &workdir(), "exec sleep 30", old.clone()).unwrap();
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
        let id = m.spawn("k", &workdir(), "exec sleep 30", run.clone()).unwrap();
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
            .spawn("flood", &workdir(), "exec yes", RunId::generate())
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let first = m.drain_pty_max(100);
        assert!(!first.is_empty(), "flooder produced output");
        assert!(first.len() <= 100, "drain capped, took {}", first.len());
        // The flood continues: a second capped drain still finds events,
        // proving the first call left the rest queued instead of dropping.
        let second = m.drain_pty_max(100);
        assert!(!second.is_empty());
        assert!(m.remove(id));
    }

    #[test]
    fn pane_write_reaches_child() {
        let mut m = SessionManager::new();
        let id = m
            .spawn("w", &workdir(), "exec cat", RunId::generate())
            .unwrap();
        m.pane_write(id, b"via-manager\n").unwrap();
        assert!(m.pane_write(SessionId::fresh(), b"x").is_err());
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = Vec::new();
        loop {
            for (_, ev) in m.drain_pty() {
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
            .spawn("w", &workdir(), "exec cat", RunId::generate())
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
        let id = m.spawn("q", &workdir(), "exit 7", RunId::generate()).unwrap();
        assert_eq!(poll_exit(&mut m, id), Some(7));
        let rec = m.get(id).unwrap();
        assert_eq!(rec.state, SessionState::Exited(Some(7)));
        assert_eq!(rec.exit_code, Some(7));
        assert!(m.remove(id));
    }
}
