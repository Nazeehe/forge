//! Cross-session message broker (Phase 4b): groups, conversations, pressure.
//!
//! The broker owns addressing and authorization state; [`crate::session`]
//! owns liveness. Every call resolves the caller by current run ID, then an
//! exact live target, then a shared communication group. Asks return a
//! conversation ID at once and never block; answers arrive as injections.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use crate::session::{SessionId, SessionManager};

/// Per-target message pressure cap: undelivered injections plus delivered
/// asks awaiting response. Delivered tells no longer count.
pub const PRESSURE_CAP: usize = 5;
/// Silence after an ack before the one courtesy reminder goes out.
pub const COURTESY_GRACE: Duration = Duration::from_secs(30);
/// Human typing holds injections for this long after the last key/paste.
pub const INJECT_DEBOUNCE: Duration = Duration::from_secs(2);

/// What a queued injection is. Decided clicks/keys elsewhere consume these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InjectKind {
    Ask,
    Response,
    Tell,
    FollowUp,
    Ack,
    Failed,
    Reminder,
}

impl InjectKind {
    /// Human-facing label for the pane-delivered block.
    pub fn label(self) -> &'static str {
        match self {
            InjectKind::Ask => "ask_session",
            InjectKind::Response => "response",
            InjectKind::Tell => "tell_session",
            InjectKind::FollowUp => "tell (follow-up)",
            InjectKind::Ack => "ack_message",
            InjectKind::Failed => "failed",
            InjectKind::Reminder => "reminder",
        }
    }
}

/// One queued prompt for a session, delivered when it is idle/debounced.
#[derive(Clone, Debug)]
pub struct Injection {
    pub conv: String,
    pub kind: InjectKind,
    pub from: String,
    pub text: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConvKind {
    Ask,
    Tell,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConvState {
    Open,
    Done,
    Failed,
}

struct Conv {
    kind: ConvKind,
    source: SessionId,
    target: SessionId,
    source_name: String,
    target_name: String,
    state: ConvState,
    acked: bool,
    last_update: Instant,
    reminded: bool,
    /// An ask counts toward pressure once while queued, then once while
    /// delivered-awaiting-response — never both at once.
    delivered: bool,
}

struct Group {
    members: HashSet<SessionId>,
}

/// Ephemeral ACLs, conversations, and per-session injection queues.
/// Liveness always comes from [`SessionManager`]; this struct never caches
/// it, so an exit is visible on the very next call.
pub struct Broker {
    groups: HashMap<String, Group>,
    membership: HashMap<SessionId, Vec<String>>,
    convs: HashMap<String, Conv>,
    queue: HashMap<SessionId, VecDeque<Injection>>,
}

impl Broker {
    pub fn new() -> Self {
        Broker {
            groups: HashMap::new(),
            membership: HashMap::new(),
            convs: HashMap::new(),
            queue: HashMap::new(),
        }
    }

    /// Join a group, creating it when missing. The session must exist.
    pub fn join(
        &mut self,
        sessions: &SessionManager,
        id: SessionId,
        group: &str,
    ) -> Result<(), String> {
        if sessions.get(id).is_none() {
            return Err("unknown session".to_string());
        }
        if group.is_empty() {
            return Err("empty group name".to_string());
        }
        self.groups
            .entry(group.to_string())
            .or_insert(Group {
                members: HashSet::new(),
            })
            .members
            .insert(id);
        let entry = self.membership.entry(id).or_default();
        if !entry.iter().any(|g| g == group) {
            entry.push(group.to_string());
        }
        Ok(())
    }

    pub fn is_member(&self, id: SessionId, group: &str) -> bool {
        self.groups
            .get(group)
            .is_some_and(|g| g.members.contains(&id))
    }

    pub fn leave(&mut self, id: SessionId, group: &str) -> bool {
        let mut removed = false;
        if let Some(g) = self.groups.get_mut(group) {
            removed = g.members.remove(&id);
        }
        if let Some(entry) = self.membership.get_mut(&id) {
            entry.retain(|g| g != group);
        }
        removed
    }

    /// First membership is the primary display group.
    pub fn primary_group(&self, id: SessionId) -> Option<&str> {
        self.membership
            .get(&id)
            .and_then(|entry| entry.first().map(String::as_str))
    }

    fn shared_group(&self, a: SessionId, b: SessionId) -> bool {
        let Some(mine) = self.membership.get(&a) else {
            return false;
        };
        mine.iter().any(|g| {
            self.groups
                .get(g)
                .is_some_and(|group| group.members.contains(&b))
        })
    }

    /// Undelivered injections waiting for one session.
    pub fn queued(&self, id: SessionId) -> usize {
        self.queue.get(&id).map(VecDeque::len).unwrap_or(0)
    }

    /// Pressure on a target: queued injections plus delivered asks still
    /// awaiting a response. One ask occupies exactly one side at a time.
    pub fn pressure(&self, _sessions: &SessionManager, id: SessionId) -> usize {
        let asks = self
            .convs
            .values()
            .filter(|c| {
                c.state == ConvState::Open
                    && c.kind == ConvKind::Ask
                    && c.target == id
                    && c.delivered
            })
            .count();
        self.queued(id) + asks
    }

    /// Pop up to `limit` queued injections, oldest first. Popping an ask
    /// marks it delivered so pressure moves with it instead of doubling.
    pub fn take_due(&mut self, id: SessionId, limit: usize) -> Vec<Injection> {
        let mut out = Vec::new();
        if let Some(q) = self.queue.get_mut(&id) {
            while out.len() < limit {
                let Some(inj) = q.pop_front() else {
                    break;
                };
                out.push(inj);
            }
        }
        for inj in &out {
            if inj.kind == InjectKind::Ask {
                if let Some(conv) = self.convs.get_mut(&inj.conv) {
                    conv.delivered = true;
                }
            }
        }
        out
    }

    fn push(&mut self, id: SessionId, inj: Injection) {
        self.queue.entry(id).or_default().push_back(inj);
    }

    /// Resolve the caller by current run ID: bound, live, and matching the
    /// record (a rebound ID never resolves to its previous holder).
    fn resolve_caller(
        &self,
        sessions: &SessionManager,
        caller_run: &str,
    ) -> Result<SessionId, String> {
        let id = sessions
            .lookup_run(caller_run)
            .ok_or_else(|| "unknown or stale run ID".to_string())?;
        let rec = sessions
            .get(id)
            .filter(|rec| rec.run_id.as_str() == caller_run && rec.state.is_live())
            .ok_or_else(|| "unknown or stale run ID".to_string())?;
        Ok(rec.id)
    }

    /// Resolve an exact live target by name. Ambiguous names fail rather
    /// than guess: names route for humans, run IDs confer authority.
    fn resolve_target(
        &self,
        sessions: &SessionManager,
        name: &str,
    ) -> Result<SessionId, String> {
        let mut found = None;
        let mut ambiguous = false;
        for &id in sessions.order() {
            let live = sessions
                .get(id)
                .is_some_and(|rec| rec.name == name && rec.state.is_live());
            if live {
                if found.is_some() {
                    ambiguous = true;
                }
                found = Some(id);
            }
        }
        if ambiguous {
            return Err(format!("ambiguous session name {name:?}"));
        }
        found.ok_or_else(|| format!("no live session named {name:?}"))
    }

    fn check_peer(
        &self,
        sessions: &SessionManager,
        caller: SessionId,
        target_name: &str,
    ) -> Result<SessionId, String> {
        let target = self.resolve_target(sessions, target_name)?;
        if !self.shared_group(caller, target) {
            return Err("no shared group with target".to_string());
        }
        if self.pressure(sessions, target) >= PRESSURE_CAP {
            return Err(format!("pressure cap reached for {target_name:?}"));
        }
        Ok(target)
    }

    fn names(&self, sessions: &SessionManager, id: SessionId) -> String {
        sessions
            .get(id)
            .map(|rec| rec.name.clone())
            .unwrap_or_default()
    }

    /// Execute one comms tool. Returns the JSON `result` fragment; every
    /// failure is a harness-visible string, never a panic.
    pub fn call(
        &mut self,
        sessions: &SessionManager,
        caller_run: &str,
        tool: &str,
        args: &str,
        now: Instant,
    ) -> Result<String, String> {
        let caller = self.resolve_caller(sessions, caller_run)?;
        match tool {
            "ask_session" => self.ask(sessions, caller, args, now),
            "send_response" => self.send_response(sessions, caller, args, now),
            "tell_session" => self.tell(sessions, caller, args, now),
            "ack_message" => self.ack(sessions, caller, args, now),
            "list_sessions" => Ok(self.list_sessions(sessions)),
            _ => Err("unknown tool".to_string()),
        }
    }

    fn arg(args: &str, name: &str) -> Option<String> {
        crate::policy::json_string_field(args.as_bytes(), &[name])
    }

    fn ask(
        &mut self,
        sessions: &SessionManager,
        caller: SessionId,
        args: &str,
        now: Instant,
    ) -> Result<String, String> {
        let target_name = Self::arg(args, "target").filter(|s| !s.is_empty())
            .ok_or_else(|| "ask needs a target".to_string())?;
        let text = Self::arg(args, "text").filter(|s| !s.is_empty())
            .ok_or_else(|| "ask needs text".to_string())?;
        let target = self.check_peer(sessions, caller, &target_name)?;
        let conv = crate::ids::ConversationId::generate().to_string();
        let from = self.names(sessions, caller);
        let target_name_live = self.names(sessions, target);
        self.push(
            target,
            Injection {
                conv: conv.clone(),
                kind: InjectKind::Ask,
                from: from.clone(),
                text,
            },
        );
        self.convs.insert(
            conv.clone(),
            Conv {
                kind: ConvKind::Ask,
                source: caller,
                target,
                source_name: from,
                target_name: target_name_live,
                state: ConvState::Open,
                acked: false,
                last_update: now,
                reminded: false,
                delivered: false,
            },
        );
        Ok(format!(r#"{{"conversation":"{conv}"}}"#))
    }

    fn send_response(
        &mut self,
        sessions: &SessionManager,
        caller: SessionId,
        args: &str,
        now: Instant,
    ) -> Result<String, String> {
        let id = Self::arg(args, "conversation").filter(|s| !s.is_empty())
            .ok_or_else(|| "a conversation ID is required".to_string())?;
        let text = Self::arg(args, "text").filter(|s| !s.is_empty())
            .ok_or_else(|| "send_response needs text".to_string())?;
        let source = {
            let conv = self
                .convs
                .get(&id)
                .ok_or_else(|| "unknown conversation".to_string())?;
            if conv.state != ConvState::Open {
                return Err("conversation is closed".to_string());
            }
            if conv.kind != ConvKind::Ask {
                return Err("only an ask takes a response".to_string());
            }
            if caller != conv.target {
                return Err("only the target answers".to_string());
            }
            conv.source
        };
        let from = self.names(sessions, caller);
        self.push(
            source,
            Injection {
                conv: id.clone(),
                kind: InjectKind::Response,
                from,
                text,
            },
        );
        if let Some(conv) = self.convs.get_mut(&id) {
            conv.state = ConvState::Done;
            conv.last_update = now;
        }
        Ok(format!(r#"{{"conversation":"{id}","completed":true}}"#))
    }

    fn tell(
        &mut self,
        sessions: &SessionManager,
        caller: SessionId,
        args: &str,
        now: Instant,
    ) -> Result<String, String> {
        let target_name = Self::arg(args, "target").filter(|s| !s.is_empty())
            .ok_or_else(|| "tell needs a target".to_string())?;
        let text = Self::arg(args, "text").filter(|s| !s.is_empty())
            .ok_or_else(|| "tell needs text".to_string())?;
        if let Some(id) = Self::arg(args, "conversation").filter(|s| !s.is_empty()) {
            // Informational follow-up on an existing conversation: delivered
            // like a tell, but no new ack is expected.
            let (source, target) = {
                let conv = self
                    .convs
                    .get(&id)
                    .ok_or_else(|| "unknown conversation".to_string())?;
                if conv.kind != ConvKind::Tell {
                    return Err("only a tell takes follow-ups".to_string());
                }
                if conv.state != ConvState::Open {
                    return Err("conversation is closed".to_string());
                }
                if caller != conv.source {
                    return Err("only the source follows up".to_string());
                }
                (conv.source, conv.target)
            };
            let live = self.resolve_target(sessions, &target_name)?;
            if live != target {
                return Err("follow-up target mismatch".to_string());
            }
            if !self.shared_group(caller, target) {
                return Err("no shared group with target".to_string());
            }
            if self.pressure(sessions, target) >= PRESSURE_CAP {
                return Err(format!("pressure cap reached for {target_name:?}"));
            }
            let from = self.names(sessions, source);
            self.push(
                target,
                Injection {
                    conv: id.clone(),
                    kind: InjectKind::FollowUp,
                    from,
                    text,
                },
            );
            if let Some(conv) = self.convs.get_mut(&id) {
                conv.last_update = now;
            }
            return Ok(format!(r#"{{"conversation":"{id}","followup":true}}"#));
        }
        let target = self.check_peer(sessions, caller, &target_name)?;
        let conv = crate::ids::ConversationId::generate().to_string();
        let from = self.names(sessions, caller);
        let target_name_live = self.names(sessions, target);
        self.push(
            target,
            Injection {
                conv: conv.clone(),
                kind: InjectKind::Tell,
                from: from.clone(),
                text,
            },
        );
        self.convs.insert(
            conv.clone(),
            Conv {
                kind: ConvKind::Tell,
                source: caller,
                target,
                source_name: from,
                target_name: target_name_live,
                state: ConvState::Open,
                acked: false,
                last_update: now,
                reminded: false,
                delivered: false,
            },
        );
        Ok(format!(r#"{{"conversation":"{conv}"}}"#))
    }

    fn ack(
        &mut self,
        sessions: &SessionManager,
        caller: SessionId,
        args: &str,
        now: Instant,
    ) -> Result<String, String> {
        let id = Self::arg(args, "conversation").filter(|s| !s.is_empty())
            .ok_or_else(|| "a conversation ID is required".to_string())?;
        let source = {
            let conv = self
                .convs
                .get(&id)
                .ok_or_else(|| "unknown conversation".to_string())?;
            if conv.kind != ConvKind::Tell {
                return Err("nothing to acknowledge".to_string());
            }
            if conv.state != ConvState::Open {
                return Err("conversation is closed".to_string());
            }
            if caller != conv.target {
                return Err("only the target acknowledges".to_string());
            }
            conv.source
        };
        let from = self.names(sessions, caller);
        self.push(
            source,
            Injection {
                conv: id.clone(),
                kind: InjectKind::Ack,
                from,
                text: "ack_message".to_string(),
            },
        );
        if let Some(conv) = self.convs.get_mut(&id) {
            conv.acked = true;
            conv.last_update = now;
        }
        Ok(format!(r#"{{"conversation":"{id}","acknowledged":true}}"#))
    }

    fn list_sessions(&self, sessions: &SessionManager) -> String {
        let mut out = String::from(r#"{"sessions":["#);
        let mut first = true;
        for &id in sessions.order() {
            let Some(rec) = sessions.get(id) else {
                continue;
            };
            if !rec.state.is_live() {
                continue;
            }
            if !first {
                out.push(',');
            }
            first = false;
            let activity = match rec.activity {
                crate::session::Activity::Idle => "Idle",
                crate::session::Activity::Thinking => "Thinking",
                crate::session::Activity::ToolUse => "ToolUse",
                crate::session::Activity::Waiting => "Waiting",
                crate::session::Activity::Stopped => "Stopped",
            };
            let group = self
                .primary_group(id)
                .map(crate::mcp::escape_json)
                .unwrap_or_else(|| "null".to_string());
            out.push_str(&format!(
                r#"{{"name":{},"live":true,"activity":"{activity}","group":{group}}}"#,
                crate::mcp::escape_json(&rec.name),
            ));
        }
        out.push_str("]}");
        out
    }

    /// Fail every open conversation touching an exited session. Targets fail
    /// loudly (their sources are told); sources fail silently.
    pub fn target_exited(&mut self, _sessions: &SessionManager, id: SessionId) {
        self.queue.remove(&id);
        let mut notify = Vec::new();
        for (conv_id, conv) in self.convs.iter_mut() {
            if conv.state != ConvState::Open {
                continue;
            }
            if conv.target == id {
                conv.state = ConvState::Failed;
                notify.push((conv.source, conv_id.clone(), conv.target_name.clone()));
            } else if conv.source == id {
                conv.state = ConvState::Failed;
            }
        }
        for (source, conv_id, target_name) in notify {
            self.push(
                source,
                Injection {
                    conv: conv_id,
                    kind: InjectKind::Failed,
                    from: target_name,
                    text: "target exited".to_string(),
                },
            );
        }
    }

    /// Emit due courtesy reminders: one per acked tell whose target went
    /// quiet past the grace period. Never repeats, never nudges otherwise.
    pub fn tick(&mut self, now: Instant) {
        let mut due = Vec::new();
        for (conv_id, conv) in self.convs.iter_mut() {
            if conv.kind != ConvKind::Tell
                || conv.state != ConvState::Open
                || !conv.acked
                || conv.reminded
            {
                continue;
            }
            if now.duration_since(conv.last_update) >= COURTESY_GRACE {
                conv.reminded = true;
                due.push((conv.target, conv_id.clone(), conv.source_name.clone()));
            }
        }
        for (target, conv_id, source_name) in due {
            self.push(
                target,
                Injection {
                    conv: conv_id,
                    kind: InjectKind::Reminder,
                    from: source_name,
                    text: "no update since your ack; the source is still waiting".to_string(),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppState;
    use crate::event::AppEvent;
    use crate::ids::RunId;
    use crate::session::SessionId;

    fn json_field(haystack: &str, field: &str) -> Option<String> {
        crate::policy::json_string_field(haystack.as_bytes(), &[field])
    }

    struct Pair {
        state: AppState,
        a: SessionId,
        b: SessionId,
        run_a: String,
        run_b: String,
    }

    fn live_pair() -> Pair {
        let mut state = AppState::new();
        let run_a = RunId::generate();
        let run_b = RunId::generate();
        let a = state
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone())
            .unwrap();
        let b = state
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone())
            .unwrap();
        Pair {
            state,
            a,
            b,
            run_a: run_a.to_string(),
            run_b: run_b.to_string(),
        }
    }

    impl Pair {
        fn grouped(mut self) -> Self {
            self.state.broker.join(&self.state.manager, self.a, "peers").unwrap();
            self.state.broker.join(&self.state.manager, self.b, "peers").unwrap();
            self
        }

        fn call(&mut self, run: &str, tool: &str, args: &str) -> Result<String, String> {
            let now = std::time::Instant::now();
            // Disjoint field borrows: the broker reads the manager.
            self.state.broker.call(&self.state.manager, run, tool, args, now)
        }
    }

    #[test]
    fn ask_returns_an_id_and_queues_target_injection() {
        let mut p = live_pair().grouped();
        let res = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","text":"ready?"}"#)
            .expect("ask validates");
        assert!(res.contains(r#""conversation":""#), "res: {res}");
        let due = p.state.broker.take_due(p.b, 10);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].text, "ready?");
        assert!(matches!(due[0].kind, InjectKind::Ask));
    }

    #[test]
    fn forged_run_id_is_rejected() {
        let mut p = live_pair().grouped();
        let err = p
            .call(
                &"f".repeat(32),
                "ask_session",
                r#"{"target":"b","text":"hi"}"#,
            )
            .expect_err("forged run ID must fail");
        assert!(err.contains("run ID"), "err: {err}");
    }

    #[test]
    fn no_shared_group_blocks_transfer() {
        let mut p = live_pair();
        // a joins alone; b is groupless.
        p.state.broker.join(&p.state.manager, p.a, "solo").unwrap();
        let err = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","text":"hi"}"#)
            .expect_err("no shared group must fail");
        assert!(err.contains("shared group"), "err: {err}");
        assert!(p.state.broker.take_due(p.b, 10).is_empty());
    }

    #[test]
    fn sixth_message_hits_the_pressure_cap() {
        let mut p = live_pair().grouped();
        for i in 0..5 {
            p.call(
                &p.run_a.clone(),
                "ask_session",
                &format!(r#"{{"target":"b","text":"q{i}"}}"#),
            )
            .expect("first five fit");
        }
        let err = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","text":"q5"}"#)
            .expect_err("sixth must fail");
        assert!(err.contains("pressure"), "err: {err}");
        // Draining the queue does not help while five asks await response.
        p.state.broker.take_due(p.b, 10);
        let err = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","text":"q6"}"#)
            .expect_err("delivered asks still count");
        assert!(err.contains("pressure"), "err: {err}");
    }

    #[test]
    fn answered_ask_releases_pressure() {
        let mut p = live_pair().grouped();
        let res = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","text":"q0"}"#)
            .unwrap();
        let conv = json_field(&res, "conversation").unwrap();
        // b reads the question, then answers; the response goes back to a
        // and the ask stops counting.
        assert_eq!(p.state.broker.take_due(p.b, 10).len(), 1);
        let r = p
            .call(
                &p.run_b.clone(),
                "send_response",
                &format!(r#"{{"conversation":"{conv}","text":"yes"}}"#),
            )
            .expect("target answers");
        assert!(r.contains(&conv), "res: {r}");
        let due = p.state.broker.take_due(p.a, 10);
        assert_eq!(due.len(), 1);
        assert!(matches!(due[0].kind, InjectKind::Response));
        assert_eq!(p.state.broker.pressure(&p.state.manager, p.b), 0);
    }

    #[test]
    fn tell_followed_by_ack_notifies_source() {
        let mut p = live_pair().grouped();
        let res = p
            .call(&p.run_a.clone(), "tell_session", r#"{"target":"b","text":"fyi"}"#)
            .expect("tell validates");
        let conv = json_field(&res, "conversation").unwrap();
        let due = p.state.broker.take_due(p.b, 10);
        assert_eq!(due.len(), 1);
        assert!(matches!(due[0].kind, InjectKind::Tell));
        // Delivered tells no longer count toward pressure.
        assert_eq!(p.state.broker.pressure(&p.state.manager, p.b), 0);
        let r = p
            .call(
                &p.run_b.clone(),
                "ack_message",
                &format!(r#"{{"conversation":"{conv}"}}"#),
            )
            .expect("target acks");
        assert!(r.contains(&conv), "res: {r}");
        let due = p.state.broker.take_due(p.a, 10);
        assert_eq!(due.len(), 1);
        assert!(matches!(due[0].kind, InjectKind::Ack));
    }

    #[test]
    fn tell_with_existing_conversation_needs_no_new_ack() {
        let mut p = live_pair().grouped();
        let res = p
            .call(&p.run_a.clone(), "tell_session", r#"{"target":"b","text":"fyi"}"#)
            .unwrap();
        let conv = json_field(&res, "conversation").unwrap();
        // b reads the tell, then acks; the ack goes back to a.
        assert_eq!(p.state.broker.take_due(p.b, 10).len(), 1);
        p.call(
            &p.run_b.clone(),
            "ack_message",
            &format!(r#"{{"conversation":"{conv}"}}"#),
        )
        .unwrap();
        p.state.broker.take_due(p.a, 10);
        // Follow-up on the same conversation: delivered, no ack expected.
        p.call(
            &p.run_a.clone(),
            "tell_session",
            &format!(r#"{{"target":"b","text":"more","conversation":"{conv}"}}"#),
        )
        .expect("follow-up validates");
        let due = p.state.broker.take_due(p.b, 10);
        assert_eq!(due.len(), 1);
        assert!(matches!(due[0].kind, InjectKind::FollowUp));
        assert!(p.state.broker.take_due(p.a, 10).is_empty());
    }

    #[test]
    fn courtesy_reminder_fires_once_after_grace() {
        let mut p = live_pair().grouped();
        let res = p
            .call(&p.run_a.clone(), "tell_session", r#"{"target":"b","text":"fyi"}"#)
            .unwrap();
        let conv = json_field(&res, "conversation").unwrap();
        p.call(
            &p.run_b.clone(),
            "ack_message",
            &format!(r#"{{"conversation":"{conv}"}}"#),
        )
        .unwrap();
        p.state.broker.take_due(p.a, 10);
        p.state.broker.take_due(p.b, 10);
        // Inside the grace period: silence.
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(10));
        assert!(p.state.broker.take_due(p.b, 10).is_empty());
        // Past it: exactly one reminder to the target, never repeated.
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(31));
        let due = p.state.broker.take_due(p.b, 10);
        assert_eq!(due.len(), 1);
        assert!(matches!(due[0].kind, InjectKind::Reminder));
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        assert!(p.state.broker.take_due(p.b, 10).is_empty());
    }

    #[test]
    fn target_exit_fails_the_conversation() {
        let mut p = live_pair().grouped();
        p.call(&p.run_a.clone(), "ask_session", r#"{"target":"b","text":"q?"}"#)
            .unwrap();
        p.state.broker.take_due(p.b, 10);
        p.state.broker.target_exited(&p.state.manager, p.b);
        let due = p.state.broker.take_due(p.a, 10);
        assert_eq!(due.len(), 1);
        assert!(matches!(due[0].kind, InjectKind::Failed));
        // The dead target keeps no letters.
        assert!(p.state.broker.take_due(p.b, 10).is_empty());
    }

    #[test]
    fn stale_run_id_after_exit_is_rejected() {
        let mut p = live_pair().grouped();
        assert!(p.state.manager.kill(p.b));
        // The reader thread reports the exit asynchronously; poll for it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in p.state.manager.drain_pty_max(100) {
                p.state.apply(AppEvent::from_pty(ev.0, ev.1));
            }
            let exited = p
                .state
                .manager
                .get(p.b)
                .is_none_or(|rec| !rec.state.is_live());
            if exited || std::time::Instant::now() > deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            p.state.manager.get(p.b).is_none_or(|rec| !rec.state.is_live()),
            "b exited"
        );
        p.state.broker.target_exited(&p.state.manager, p.b);
        let err = p
            .call(&p.run_b.clone(), "list_sessions", "{}")
            .expect_err("exited run ID is stale");
        assert!(err.contains("run ID"), "err: {err}");
    }

    #[test]
    fn leave_breaks_the_shared_group() {
        let mut p = live_pair().grouped();
        assert!(p.state.broker.leave(p.a, "peers"));
        assert_eq!(p.state.broker.primary_group(p.a), None);
        let err = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","text":"hi"}"#)
            .expect_err("left group must fail");
        assert!(err.contains("shared group"), "err: {err}");
    }

    #[test]
    fn list_sessions_reports_live_peers() {
        let mut p = live_pair().grouped();
        let res = p
            .call(&p.run_a.clone(), "list_sessions", "{}")
            .expect("listing works");
        assert!(res.contains(r#""name":"a""#), "res: {res}");
        assert!(res.contains(r#""name":"b""#), "res: {res}");
        assert!(res.contains(r#""live":true"#), "res: {res}");
    }
}
