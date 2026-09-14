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
/// Longest self-injection delay in seconds: a day. Beyond that the TUI
/// the timer belongs to is long gone, and `Instant` math would overflow.
pub const MAX_SCHEDULE_DELAY_SECS: f64 = 86_400.0;
/// Silence after an ack before the one courtesy reminder goes out.
pub const COURTESY_GRACE: Duration = Duration::from_secs(30);
/// Human typing holds injections for this long after the last key/paste.
pub const INJECT_DEBOUNCE: Duration = Duration::from_secs(2);
/// Beat between an injection body and its Enter: one input event at a
/// time, like a human typing then pressing Enter. A burst ending in CR
/// parses as one paste in some CLIs and the submit never fires. 300 ms:
/// shorter beats get eaten by prompt redraws on slower machines.
pub const INJECT_ENTER_DELAY: Duration = Duration::from_millis(300);
/// The staged Enter byte.
pub const INJECT_ENTER_CR: u8 = b'\r';

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
    /// A CLI command (`/compact`, a scheduled prompt): no conversation,
    /// no reply guidance, just the text plus the staged Enter.
    Command,
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
            InjectKind::Command => "command",
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

impl Injection {
    /// Per-kind reply guidance: Ask takes `send_response`, Tell takes
    /// `tell_session` follow-ups from either party, while terminal kinds
    /// (Response, Ack, Reminder, Failed) carry none because replying to
    /// them only errors.
    fn guidance(&self) -> Option<String> {
        match self.kind {
            InjectKind::Ask => Some(format!(
                "[forge: reply with send_response conversation_id {}]",
                self.conv
            )),
            InjectKind::Tell | InjectKind::FollowUp => Some(format!(
                "[forge: reply with tell_session target {} conversation_id {}]",
                self.from, self.conv
            )),
            InjectKind::Response
            | InjectKind::Ack
            | InjectKind::Reminder
            | InjectKind::Failed
            | InjectKind::Command => None,
        }
    }

    /// Pane body without the trailing CR: the staged Enter goes out on a
    /// later settle tick (see [`INJECT_ENTER_DELAY`]), never in the same
    /// burst as the text.
    pub fn render_body(&self) -> Vec<u8> {
        let mut out = format!("[forge {} from {}]: {}", self.kind.label(), self.from, self.text);
        if let Some(guidance) = self.guidance() {
            out.push('\n');
            out.push_str(&guidance);
        }
        out.into_bytes()
    }

    /// Full pane sequence: body plus the staged Enter. Delivery writes
    /// the body first and the CR on a later tick (human parity: type
    /// text, press Enter — never one burst). No leading newline: one
    /// would land as a junk blank line in the receiver's draft.
    pub fn render(&self) -> Vec<u8> {
        let mut body = self.render_body();
        // Terminal Enter for raw-mode CLIs and canonical shells alike.
        body.push(INJECT_ENTER_CR);
        body
    }
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
    /// Palette index assigned at creation; drives the sessions-bar chip.
    /// Stable for the group lifetime (never reused after delete).
    color: usize,
}

/// One pending self-injection: fires into the target queue at `due`,
/// then delivers through the normal idle path. `from` is resolved at
/// schedule time because the tick that fires it sees no sessions.
struct Timer {
    target: SessionId,
    from: String,
    text: String,
    due: Instant,
}

/// Ephemeral ACLs, conversations, and per-session injection queues.
/// Liveness always comes from [`SessionManager`]; this struct never caches
/// it, so an exit is visible on the very next call.
pub struct Broker {
    groups: HashMap<String, Group>,
    membership: HashMap<SessionId, Vec<String>>,
    convs: HashMap<String, Conv>,
    queue: HashMap<SessionId, VecDeque<Injection>>,
    timers: HashMap<String, Timer>,
    /// Monotonic palette cursor: each created group takes the next index,
    /// deleted ones never hand theirs back (stable chips, no reuse).
    next_color: usize,
}

impl Broker {
    pub fn new() -> Self {
        Broker {
            groups: HashMap::new(),
            membership: HashMap::new(),
            convs: HashMap::new(),
            queue: HashMap::new(),
            timers: HashMap::new(),
            next_color: 0,
        }
    }

    /// Create an empty group (for the human group dialog). Fails on
    /// empty/duplicate names; creation assigns the next palette color.
    pub fn create_group(&mut self, group: &str) -> Result<(), String> {
        if group.is_empty() {
            return Err("empty group name".to_string());
        }
        if self.groups.contains_key(group) {
            return Err("group already exists".to_string());
        }
        let color = self.next_color;
        self.next_color += 1;
        self.groups.insert(
            group.to_string(),
            Group {
                members: HashSet::new(),
                color,
            },
        );
        Ok(())
    }

    /// Join a group, creating it when missing. The session must exist.
    /// Creation assigns the next palette color to the group.
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
        if !self.groups.contains_key(group) {
            let color = self.next_color;
            self.next_color += 1;
            self.groups.insert(
                group.to_string(),
                Group {
                    members: HashSet::new(),
                    color,
                },
            );
        }
        self.groups
            .get_mut(group)
            .expect("group just created")
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

    /// Every group one session belongs to, in join order. Snapshots use
    /// this so restores rejoin exactly.
    pub fn groups_of(&self, id: SessionId) -> Vec<String> {
        self.membership.get(&id).cloned().unwrap_or_default()
    }

    /// Palette index of a group, if it exists.
    pub fn group_color(&self, group: &str) -> Option<usize> {
        self.groups.get(group).map(|g| g.color)
    }

    /// All group names, sorted for a stable dialog.
    pub fn group_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.groups.keys().cloned().collect();
        names.sort();
        names
    }

    /// Members of a group as sorted session IDs, empty when unknown.
    pub fn group_members(&self, group: &str) -> Vec<SessionId> {
        let mut members: Vec<SessionId> = self
            .groups
            .get(group)
            .map(|g| g.members.iter().copied().collect())
            .unwrap_or_default();
        members.sort();
        members
    }

    /// Rename a group, keeping its members, palette color, and every
    /// member's primary-group order. Fails on empty/duplicate/unknown
    /// names without touching anything.
    pub fn rename_group(&mut self, from: &str, to: &str) -> Result<(), String> {
        if to.is_empty() {
            return Err("empty group name".to_string());
        }
        if from == to {
            return Ok(());
        }
        if !self.groups.contains_key(from) {
            return Err("unknown group".to_string());
        }
        if self.groups.contains_key(to) {
            return Err("group already exists".to_string());
        }
        let group = self.groups.remove(from).expect("group exists");
        for entry in self.membership.values_mut() {
            for name in entry.iter_mut() {
                if name == from {
                    *name = to.to_string();
                }
            }
        }
        self.groups.insert(to.to_string(), group);
        Ok(())
    }

    /// Delete a group outright, releasing every membership. True when the
    /// group existed; colors of surviving groups never shift.
    pub fn remove_group(&mut self, group: &str) -> bool {
        if self.groups.remove(group).is_none() {
            return false;
        }
        for entry in self.membership.values_mut() {
            entry.retain(|g| g != group);
        }
        true
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

    pub(crate) fn push(&mut self, id: SessionId, inj: Injection) {
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
            "list_sessions" => Ok(self.list_sessions(sessions, caller)),
            "compact_session" => self.compact(sessions, caller, args),
            "schedule_prompt" => self.schedule(sessions, caller, args, now),
            "cancel_scheduled_prompt" => self.cancel_scheduled(caller, args),
            _ => Err("unknown tool".to_string()),
        }
    }

    fn arg(args: &str, name: &str) -> Option<String> {
        crate::policy::json_string_field(args.as_bytes(), &[name])
    }

    /// Canonical blueprint names first (`message`, `conversation_id`), with
    /// the Phase 4a short forms accepted as aliases.
    fn arg2(args: &str, names: &[&str]) -> Option<String> {
        crate::policy::json_string_field(args.as_bytes(), names)
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
        let text = Self::arg2(args, &["message", "text"]).filter(|s| !s.is_empty())
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

    /// Raw numeric arg: JSON numbers arrive bare, quoted ones read
    /// liberally too. Non-finite values never pass.
    fn arg_num(args: &str, name: &str) -> Option<f64> {
        let raw = crate::mcp::top_raw(args, name)?.trim();
        let bare = raw
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(raw);
        bare.parse::<f64>().ok().filter(|v| v.is_finite())
    }

    fn arg_bool(args: &str, name: &str) -> Option<bool> {
        match crate::mcp::top_raw(args, name)?.trim() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }
    }

    /// Queue `/compact` for a session: self is always allowed, another
    /// target needs a shared group like any peer write. Delivery waits
    /// for the idle path, so the command never lands mid-turn.
    fn compact(
        &mut self,
        sessions: &SessionManager,
        caller: SessionId,
        args: &str,
    ) -> Result<String, String> {
        let target = match Self::arg(args, "target").filter(|s| !s.is_empty()) {
            None => caller,
            Some(name) => {
                let target = self.resolve_target(sessions, &name)?;
                if target != caller && !self.shared_group(caller, target) {
                    return Err("no shared group with target".to_string());
                }
                target
            }
        };
        if self.pressure(sessions, target) >= PRESSURE_CAP {
            return Err("pressure cap reached".to_string());
        }
        self.push(
            target,
            Injection {
                conv: crate::ids::ConversationId::generate().to_string(),
                kind: InjectKind::Command,
                from: self.names(sessions, caller),
                text: "/compact".to_string(),
            },
        );
        Ok(r#"{"queued":true}"#.to_string())
    }

    /// Arm a self-injection timer: the prompt fires into the caller's
    /// own queue after the delay and delivers when idle. `clear_context`
    /// prefixes `/clear` so the prompt starts a fresh context.
    fn schedule(
        &mut self,
        sessions: &SessionManager,
        caller: SessionId,
        args: &str,
        now: Instant,
    ) -> Result<String, String> {
        let prompt = Self::arg(args, "prompt").filter(|s| !s.is_empty())
            .ok_or_else(|| "schedule_prompt needs a prompt".to_string())?;
        if crate::mcp::top_raw(args, "delay_seconds").is_some()
            && Self::arg_num(args, "delay_seconds").is_none()
        {
            return Err("delay_seconds must be a number of seconds".to_string());
        }
        let delay = Self::arg_num(args, "delay_seconds").unwrap_or(0.0);
        if !(0.0..=MAX_SCHEDULE_DELAY_SECS).contains(&delay) {
            return Err("delay_seconds must sit between 0 and 86400".to_string());
        }
        if crate::mcp::top_raw(args, "clear_context").is_some()
            && Self::arg_bool(args, "clear_context").is_none()
        {
            return Err("clear_context must be true or false".to_string());
        }
        let clear = Self::arg_bool(args, "clear_context").unwrap_or(false);
        let own = self.timers.values().filter(|t| t.target == caller).count();
        if own + self.queued(caller) >= PRESSURE_CAP {
            return Err("pressure cap reached".to_string());
        }
        let text = if clear {
            format!("/clear\n{prompt}")
        } else {
            prompt
        };
        let timer = crate::ids::ConversationId::generate().to_string();
        self.timers.insert(
            timer.clone(),
            Timer {
                target: caller,
                from: self.names(sessions, caller),
                text,
                due: now + Duration::from_secs_f64(delay),
            },
        );
        Ok(format!(r#"{{"timer_id":"{timer}"}}"#))
    }

    /// Armed timers for one session, soonest first: (timer ID, due).
    /// The sidebar countdowns and cancel buttons read this.
    pub fn timers_for(&self, id: SessionId) -> Vec<(String, Instant)> {
        let mut out: Vec<(String, Instant)> = self
            .timers
            .iter()
            .filter(|(_, timer)| timer.target == id)
            .map(|(timer_id, timer)| (timer_id.clone(), timer.due))
            .collect();
        out.sort_by_key(|(_, due)| *due);
        out
    }

    /// Human cancel from the sidebar: any armed timer drops. The UI
    /// only offers the focused session's timers; the tool path keeps
    /// its owner check in `cancel_scheduled`.
    pub fn cancel_timer(&mut self, timer_id: &str) -> bool {
        self.timers.remove(timer_id).is_some()
    }

    /// Cancel an armed timer. Fired or unknown IDs fail rather than
    /// confirming thin air; only the owning session cancels.
    fn cancel_scheduled(&mut self, caller: SessionId, args: &str) -> Result<String, String> {
        let timer = Self::arg(args, "timer_id").filter(|s| !s.is_empty())
            .ok_or_else(|| "cancel_scheduled_prompt needs a timer_id".to_string())?;
        match self.timers.get(&timer) {
            Some(pending) if pending.target == caller => {
                self.timers.remove(&timer);
                Ok(r#"{"cancelled":true}"#.to_string())
            }
            _ => Err("unknown timer".to_string()),
        }
    }

    fn send_response(
        &mut self,
        sessions: &SessionManager,
        caller: SessionId,
        args: &str,
        now: Instant,
    ) -> Result<String, String> {
        let id = Self::arg2(args, &["conversation_id", "conversation"]).filter(|s| !s.is_empty())
            .ok_or_else(|| "a conversation ID is required".to_string())?;
        let text = Self::arg2(args, &["message", "text"]).filter(|s| !s.is_empty())
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
        let text = Self::arg2(args, &["message", "text"]).filter(|s| !s.is_empty())
            .ok_or_else(|| "tell needs text".to_string())?;
        if let Some(id) = Self::arg2(args, &["conversation_id", "conversation"]).filter(|s| !s.is_empty()) {
            // Informational follow-up on an existing conversation: delivered
            // like a tell, but no new ack is expected. Either party can
            // follow up — this is the receiver's way back — delivering to
            // the other side.
            let peer = {
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
                if caller == conv.source {
                    conv.target
                } else if caller == conv.target {
                    conv.source
                } else {
                    return Err("only conversation parties can follow up".to_string());
                }
            };
            let live = self.resolve_target(sessions, &target_name)?;
            if live != peer {
                return Err("follow-up target mismatch".to_string());
            }
            if !self.shared_group(caller, peer) {
                return Err("no shared group with target".to_string());
            }
            if self.pressure(sessions, peer) >= PRESSURE_CAP {
                return Err(format!("pressure cap reached for {target_name:?}"));
            }
            let from = self.names(sessions, caller);
            self.push(
                peer,
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
        let id = Self::arg2(args, &["conversation_id", "conversation"]).filter(|s| !s.is_empty())
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

    /// The `you` field names the calling session first, so a session
    /// reading the list can spot its own entry without guessing.
    fn list_sessions(&self, sessions: &SessionManager, caller: SessionId) -> String {
        let mut out = format!(
            r#"{{"you":{},"sessions":["#,
            crate::mcp::escape_json(&self.names(sessions, caller)),
        );
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
        self.timers.retain(|_, timer| timer.target != id);
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
        // Fire due self-injection timers into their queues; the idle
        // path delivers them like any other injection.
        let mut fired = Vec::new();
        for (timer_id, timer) in self.timers.iter() {
            if now >= timer.due {
                fired.push(timer_id.clone());
            }
        }
        for timer_id in fired {
            if let Some(timer) = self.timers.remove(&timer_id) {
                self.push(
                    timer.target,
                    Injection {
                        conv: timer_id,
                        kind: InjectKind::Command,
                        from: timer.from,
                        text: timer.text,
                    },
                );
            }
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
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let b = state
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
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
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"ready?"}"#)
            .expect("ask validates");
        assert!(res.contains(r#""conversation":""#), "res: {res}");
        let due = p.state.broker.take_due(p.b, 10);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].text, "ready?");
        assert!(matches!(due[0].kind, InjectKind::Ask));
    }

    #[test]
    fn legacy_short_params_still_accepted() {
        let mut p = live_pair().grouped();
        // Phase 4a short forms are aliases for the canonical blueprint names.
        let res = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"q"}"#)
            .expect("canonical message validates");
        let conv = json_field(&res, "conversation").unwrap();
        p.state.broker.take_due(p.b, 10);
        p.call(
            &p.run_b.clone(),
            "send_response",
            &format!(r#"{{"conversation_id":"{conv}","message":"a"}}"#),
        )
        .expect("canonical conversation_id validates");
        // ...and the old shorts keep working.
        p.call(&p.run_a.clone(), "tell_session", r#"{"target":"b","text":"old"}"#)
            .expect("legacy text alias validates");
        let due = p.state.broker.take_due(p.b, 10);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].text, "old");
    }

    #[test]
    fn forged_run_id_is_rejected() {
        let mut p = live_pair().grouped();
        let err = p
            .call(
                &"f".repeat(32),
                "ask_session",
                r#"{"target":"b","message":"hi"}"#,
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
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"hi"}"#)
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
                &format!(r#"{{"target":"b","message":"q{i}"}}"#),
            )
            .expect("first five fit");
        }
        let err = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"q5"}"#)
            .expect_err("sixth must fail");
        assert!(err.contains("pressure"), "err: {err}");
        // Draining the queue does not help while five asks await response.
        p.state.broker.take_due(p.b, 10);
        let err = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"q6"}"#)
            .expect_err("delivered asks still count");
        assert!(err.contains("pressure"), "err: {err}");
    }

    #[test]
    fn answered_ask_releases_pressure() {
        let mut p = live_pair().grouped();
        let res = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"q0"}"#)
            .unwrap();
        let conv = json_field(&res, "conversation").unwrap();
        // b reads the question, then answers; the response goes back to a
        // and the ask stops counting.
        assert_eq!(p.state.broker.take_due(p.b, 10).len(), 1);
        let r = p
            .call(
                &p.run_b.clone(),
                "send_response",
                &format!(r#"{{"conversation_id":"{conv}","message":"yes"}}"#),
            )
            .expect("target answers");
        assert!(r.contains(&conv), "res: {r}");
        let due = p.state.broker.take_due(p.a, 10);
        assert_eq!(due.len(), 1);
        assert!(matches!(due[0].kind, InjectKind::Response));
        assert_eq!(p.state.broker.pressure(&p.state.manager, p.b), 0);
    }

    #[test]
    fn staged_enter_beat_is_300ms() {
        // The body/Enter split only works when the CR trails by enough
        // for a prompt redraw to settle; pin the tuned value.
        assert_eq!(INJECT_ENTER_DELAY, Duration::from_millis(300));
    }

    #[test]
    fn injection_render_submits_with_reply_guidance() {
        // Ask: CR-terminated, send_response guidance naming the conv.
        let ask = Injection {
            conv: "conv-1".to_string(),
            kind: InjectKind::Ask,
            from: "a".to_string(),
            text: "ready?".to_string(),
        };
        assert_eq!(
            ask.render(),
            b"[forge ask_session from a]: ready?\n[forge: reply with send_response conversation_id conv-1]\r"
        );
        // Tell: CR-terminated, tell_session guidance naming target + conv.
        let tell = Injection {
            conv: "conv-2".to_string(),
            kind: InjectKind::Tell,
            from: "a".to_string(),
            text: "fyi".to_string(),
        };
        assert_eq!(
            tell.render(),
            b"[forge tell_session from a]: fyi\n[forge: reply with tell_session target a conversation_id conv-2]\r"
        );
        // Terminal kinds: CR-terminated, no guidance (replying errors).
        for kind in [
            InjectKind::Response,
            InjectKind::Ack,
            InjectKind::Reminder,
            InjectKind::Failed,
        ] {
            let inj = Injection {
                conv: "conv-3".to_string(),
                kind,
                from: "a".to_string(),
                text: "note".to_string(),
            };
            let bytes = inj.render();
            assert_eq!(bytes.last(), Some(&b'\r'), "enter: {kind:?}");
            assert_ne!(bytes.first(), Some(&b'\n'), "no leading newline: {kind:?}");
            assert!(
                !bytes.windows(8).any(|w| w == b"reply wi"),
                "no guidance: {kind:?}"
            );
        }
    }

    #[test]
    fn tell_target_can_follow_up_back_to_source() {
        let mut p = live_pair().grouped();
        let res = p
            .call(&p.run_a.clone(), "tell_session", r#"{"target":"b","message":"fyi"}"#)
            .unwrap();
        let conv = json_field(&res, "conversation").unwrap();
        assert_eq!(p.state.broker.take_due(p.b, 10).len(), 1);
        // The receiver talks back on the same conversation by naming the
        // source as target; the injection lands at the source, from b.
        let r = p
            .call(
                &p.run_b.clone(),
                "tell_session",
                &format!(r#"{{"target":"a","message":"back","conversation_id":"{conv}"}}"#),
            )
            .expect("target follows up");
        assert!(r.contains("followup"), "res: {r}");
        let due = p.state.broker.take_due(p.a, 10);
        assert_eq!(due.len(), 1);
        assert!(matches!(due[0].kind, InjectKind::FollowUp));
        assert_eq!(due[0].from, "b");
        assert_eq!(due[0].conv, conv);
        // Rendered guidance points back at b with the same conversation.
        let bytes = due[0].render();
        assert!(
            bytes.windows(9).any(|w| w == b"target b "),
            "guidance: {bytes:?}"
        );
        assert!(bytes.ends_with(b"\r"), "enter: {bytes:?}");
        // Outsiders still cannot ride the conversation, even in-group.
        let c = p
            .state
            .manager
            .spawn(
                "c",
                &std::env::temp_dir(),
                "exec sleep 30",
                crate::ids::RunId::generate(),
                "shell",
            )
            .unwrap();
        p.state.broker.join(&p.state.manager, c, "peers").unwrap();
        let run_c = p.state.manager.get(c).unwrap().run_id.to_string();
        let err = p
            .call(
                &run_c,
                "tell_session",
                &format!(r#"{{"target":"a","message":"hijack","conversation_id":"{conv}"}}"#),
            )
            .expect_err("outsider follow-up rejected");
        assert!(err.contains("parties"), "err: {err}");
    }

    #[test]
    fn tell_followed_by_ack_notifies_source() {
        let mut p = live_pair().grouped();
        let res = p
            .call(&p.run_a.clone(), "tell_session", r#"{"target":"b","message":"fyi"}"#)
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
                &format!(r#"{{"conversation_id":"{conv}"}}"#),
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
            .call(&p.run_a.clone(), "tell_session", r#"{"target":"b","message":"fyi"}"#)
            .unwrap();
        let conv = json_field(&res, "conversation").unwrap();
        // b reads the tell, then acks; the ack goes back to a.
        assert_eq!(p.state.broker.take_due(p.b, 10).len(), 1);
        p.call(
            &p.run_b.clone(),
            "ack_message",
            &format!(r#"{{"conversation_id":"{conv}"}}"#),
        )
        .unwrap();
        p.state.broker.take_due(p.a, 10);
        // Follow-up on the same conversation: delivered, no ack expected.
        p.call(
            &p.run_a.clone(),
            "tell_session",
            &format!(r#"{{"target":"b","message":"more","conversation_id":"{conv}"}}"#),
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
            .call(&p.run_a.clone(), "tell_session", r#"{"target":"b","message":"fyi"}"#)
            .unwrap();
        let conv = json_field(&res, "conversation").unwrap();
        p.call(
            &p.run_b.clone(),
            "ack_message",
            &format!(r#"{{"conversation_id":"{conv}"}}"#),
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
        p.call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"q?"}"#)
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
                p.state.apply(AppEvent::from_pty(ev.0, ev.2));
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
    fn group_colors_assign_monotonically_and_survive_delete() {
        let mut p = live_pair();
        assert!(p.state.broker.join(&p.state.manager, p.a, "alpha").is_ok());
        assert!(p.state.broker.join(&p.state.manager, p.b, "alpha").is_ok());
        assert!(p.state.broker.join(&p.state.manager, p.a, "beta").is_ok());
        assert_eq!(p.state.broker.group_color("alpha"), Some(0));
        assert_eq!(p.state.broker.group_color("beta"), Some(1));
        assert_eq!(p.state.broker.group_color("missing"), None);
        assert_eq!(p.state.broker.group_names(), vec!["alpha".to_string(), "beta".to_string()]);
        let mut members = p.state.broker.group_members("alpha");
        members.sort();
        assert_eq!(members, vec![p.a.min(p.b), p.a.max(p.b)]);
        // Delete hands no color back: the next group takes 2, and alpha's
        // ex-members lose the membership (primary falls back to beta).
        assert!(p.state.broker.remove_group("alpha"));
        assert!(!p.state.broker.remove_group("alpha"));
        assert!(p.state.broker.join(&p.state.manager, p.b, "gamma").is_ok());
        assert_eq!(p.state.broker.group_color("gamma"), Some(2));
        assert_eq!(p.state.broker.primary_group(p.a), Some("beta"));
        assert_eq!(p.state.broker.primary_group(p.b), Some("gamma"));
    }

    #[test]
    fn create_and_rename_groups_keep_color_and_members() {
        let mut p = live_pair();
        assert!(p.state.broker.create_group("alpha").is_ok());
        assert!(p.state.broker.create_group("alpha").is_err(), "duplicate");
        assert!(p.state.broker.create_group("").is_err(), "empty");
        // Empty group exists with a color but no members.
        assert_eq!(p.state.broker.group_color("alpha"), Some(0));
        assert!(p.state.broker.group_members("alpha").is_empty());
        assert!(p.state.broker.join(&p.state.manager, p.a, "alpha").is_ok());
        assert!(p.state.broker.join(&p.state.manager, p.a, "beta").is_ok());
        assert!(p.state.broker.rename_group("alpha", "alpha-v2").is_ok());
        assert_eq!(p.state.broker.group_color("alpha-v2"), Some(0), "color kept");
        assert_eq!(p.state.broker.group_color("alpha"), None, "old name gone");
        assert!(p.state.broker.is_member(p.a, "alpha-v2"));
        assert_eq!(p.state.broker.primary_group(p.a), Some("alpha-v2"), "order kept");
        // Failures leave everything untouched.
        assert!(p.state.broker.rename_group("alpha-v2", "beta").is_err(), "duplicate");
        assert!(p.state.broker.rename_group("alpha-v2", "").is_err(), "empty");
        assert!(p.state.broker.rename_group("missing", "new").is_err(), "unknown");
        assert!(p.state.broker.is_member(p.a, "alpha-v2"));
        assert_eq!(p.state.broker.group_color("alpha-v2"), Some(0));
        // Same-name rename is a no-op success.
        assert!(p.state.broker.rename_group("alpha-v2", "alpha-v2").is_ok());
    }

    #[test]
    fn leave_breaks_the_shared_group() {
        let mut p = live_pair().grouped();
        assert!(p.state.broker.leave(p.a, "peers"));
        assert_eq!(p.state.broker.primary_group(p.a), None);
        let err = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"hi"}"#)
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
        // The caller sees its own name up front in `you`.
        assert!(res.starts_with(r#"{"you":"a","#), "res: {res}");
        let res_b = p
            .call(&p.run_b.clone(), "list_sessions", "{}")
            .expect("listing works for b too");
        assert!(res_b.starts_with(r#"{"you":"b","#), "res: {res_b}");
    }

    #[test]
    fn compact_self_needs_no_group_peer_needs_one() {
        let mut p = live_pair();
        let out = p
            .call(&p.run_a.clone(), "compact_session", "{}")
            .expect("self compact queues");
        assert!(out.contains(r#""queued":true"#), "out: {out}");
        let due = p.state.broker.take_due(p.a, 10);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].kind, InjectKind::Command);
        assert_eq!(due[0].text, "/compact");
        let body = String::from_utf8(due[0].render_body()).unwrap();
        assert!(body.contains("/compact"), "body: {body}");
        assert!(!body.contains("send_response"), "commands carry no reply guidance: {body}");
        let err = p
            .call(&p.run_a.clone(), "compact_session", r#"{"target":"b"}"#)
            .expect_err("groupless peer compact fails");
        assert!(err.contains("no shared group"), "err: {err}");
        let mut grouped = p.grouped();
        grouped
            .call(&grouped.run_a.clone(), "compact_session", r#"{"target":"b"}"#)
            .expect("grouped peer compact queues");
        assert_eq!(grouped.state.broker.queued(grouped.b), 1);
    }

    #[test]
    fn schedule_fires_clear_text_then_cancel_drops() {
        let mut p = live_pair();
        let out = p
            .call(&p.run_a.clone(), "schedule_prompt", r#"{"prompt":"nudge","delay_seconds":0}"#)
            .expect("schedules");
        assert!(out.contains("timer_id"), "out: {out}");
        assert_eq!(p.state.broker.queued(p.a), 0, "timers wait for the tick");
        p.state.broker.tick(std::time::Instant::now());
        let due = p.state.broker.take_due(p.a, 10);
        assert_eq!(due.len(), 1);
        assert_eq!((due[0].kind, due[0].text.as_str()), (InjectKind::Command, "nudge"));
        p.call(&p.run_a.clone(), "schedule_prompt",
            r#"{"prompt":"fresh","delay_seconds":0,"clear_context":true}"#)
            .expect("clear-context schedules");
        p.state.broker.tick(std::time::Instant::now());
        let due = p.state.broker.take_due(p.a, 10);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].text, "/clear\nfresh", "clear prefixes the prompt");
        let out = p
            .call(&p.run_a.clone(), "schedule_prompt", r#"{"prompt":"later","delay_seconds":60}"#)
            .expect("future schedules");
        let timer = crate::policy::json_string_field(out.as_bytes(), &["timer_id"]).unwrap();
        let cancelled = p
            .call(&p.run_a.clone(), "cancel_scheduled_prompt", &format!(r#"{{"timer_id":"{timer}"}}"#))
            .expect("cancel works");
        assert!(cancelled.contains("cancelled"), "cancelled: {cancelled}");
        p.state.broker.tick(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        assert_eq!(p.state.broker.queued(p.a), 0, "cancelled timer never fires");
        let again = p
            .call(&p.run_a.clone(), "cancel_scheduled_prompt", &format!(r#"{{"timer_id":"{timer}"}}"#))
            .expect_err("fired timers stay unknown");
        assert!(again.contains("unknown timer"), "again: {again}");
    }

    #[test]
    fn timers_for_lists_soonest_first_per_session() {
        let mut p = live_pair();
        let run_a = p.run_a.clone();
        p.call(&run_a, "schedule_prompt", r#"{"prompt":"slow","delay_seconds":60}"#)
            .expect("slow arms");
        let _ = p
            .call(&run_a, "schedule_prompt", r#"{"prompt":"fast","delay_seconds":0}"#)
            .expect("fast arms");
        let listed = p.state.broker.timers_for(p.a);
        assert_eq!(listed.len(), 2);
        assert!(listed[0].1 <= listed[1].1, "soonest first");
        assert!(p.state.broker.timers_for(p.b).is_empty(), "per-session");
        assert!(p.state.broker.cancel_timer(&listed[0].0), "human cancel drops");
        assert!(!p.state.broker.cancel_timer("nope"), "unknown stays false");
        assert_eq!(p.state.broker.timers_for(p.a).len(), 1);
    }

    #[test]
    fn schedule_rejects_blank_prompt_and_bad_delays() {
        let mut p = live_pair();
        let blank = p
            .call(&p.run_a.clone(), "schedule_prompt", "{}")
            .expect_err("blank prompt fails");
        assert!(blank.contains("needs a prompt"), "blank: {blank}");
        for args in [
            r#"{"prompt":"x","delay_seconds":-1}"#,
            r#"{"prompt":"x","delay_seconds":86401}"#,
            r#"{"prompt":"x","delay_seconds":"soon"}"#,
        ] {
            let err = p
                .call(&p.run_a.clone(), "schedule_prompt", args)
                .expect_err("bad delay fails");
            assert!(err.contains("delay_seconds"), "args {args}: {err}");
        }
    }
}
