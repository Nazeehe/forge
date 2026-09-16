//! Cross-session message broker (Phase 4b): groups, conversations, pressure.
//!
//! The broker owns addressing and authorization state; [`crate::session`]
//! owns liveness. Every call resolves the caller by current run ID, then an
//! exact live target, then a shared communication group. Asks return a
//! conversation ID at once and never block; answers arrive as injections.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use crate::bot::{
    BotClient, BotConv, BotConvKind, BotConvState, BotError, BotKind, ErrorCode, IdemCache,
    IdemCheck,
};
use crate::session::{SessionId, SessionManager};

/// Per-target message pressure cap: undelivered injections plus delivered
/// asks awaiting response. Delivered tells no longer count.
pub const PRESSURE_CAP: usize = 5;
/// Per-session injection queue cap. Completions (responses, acks,
/// failures, reminders) always queue — they finish already-admitted
/// work — so the bound comes from the other end: a session with this
/// many undelivered entries cannot start new sends until it drains.
/// Sized past one loop drain (100) with headroom; hitting it reads as
/// backpressure ("drain first"), never as lost work.
pub const QUEUE_CAP: usize = 128;
/// Longest self-injection delay in seconds: a day. Beyond that the TUI
/// the timer belongs to is long gone, and `Instant` math would overflow.
pub const MAX_SCHEDULE_DELAY_SECS: f64 = 86_400.0;
/// Silence after an ack before the one courtesy reminder goes out.
pub const COURTESY_GRACE: Duration = Duration::from_secs(30);
/// Terminal (Done/Failed) conversation records live this long past
/// their last update before the sweep evicts them. Past the
/// idempotency replay window (600s) with margin, so a replayed
/// conversation ID still resolves while retries can replay it; open
/// asks never evict, whatever their age (quiet tells quiesce below).
pub const CONV_TTL: Duration = Duration::from_secs(1800);
/// Hard cap on terminal records per conversation map: past this the
/// sweep evicts oldest-first even within their TTL, so a burst half
/// hour cannot grow the maps (or the per-second scans) without limit.
/// Open work never counts toward the cap.
pub const CONV_CAP: usize = 4096;
/// An Open tell quiet this long is a dead thread: no delivery, ack,
/// reminder, or follow-up in a full day means nobody is coming back,
/// so the sweep evicts it (resume with a fresh tell). Open asks never
/// quiesce — an awaited answer can land at any time — and queued work
/// pins its record either way.
pub const QUIESCE_TTL: Duration = Duration::from_secs(86400);
/// Fastest courtesy/timer sweep: `Broker::tick` walks every conversation
/// and timer, but grace is 30s and timers are second-scale, so the 16ms
/// loop skips sweeps inside this window. Queue delivery is unaffected
/// and stays per-tick; worst case a reminder or timer fires this late.
pub const BROKER_TICK_INTERVAL: Duration = Duration::from_secs(1);
/// Human typing holds injections for this long after the last key/paste.
pub const INJECT_DEBOUNCE: Duration = Duration::from_secs(2);
/// Beat between an injection body and its Enter: one input event at a
/// time, like a human typing then pressing Enter. A burst ending in CR
/// parses as one paste in some CLIs and the submit never fires. 300 ms:
/// shorter beats get eaten by prompt redraws on slower machines.
pub const INJECT_ENTER_DELAY: Duration = Duration::from_millis(300);
/// Quiet period after hook activity before an injection goes out: hook
/// verdicts land mid-tool-use, and a body racing them interleaves with
/// the agent's own input. Same gate as the human-typing debounce.
pub const INJECT_HOOK_DEBOUNCE: Duration = Duration::from_millis(500);
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

    /// Delivery bytes for one injection: the whole payload in a single
    /// bracketed-paste transaction (`ESC[200~` … `ESC[201~`) when the pane
    /// opted into paste mode, raw otherwise. Embedded terminators are
    /// stripped from framed payloads so hostile text cannot break out of
    /// the paste early and leave the tail to execute. Either way the
    /// result goes out through exactly one `write_all`.
    pub fn render_framed(&self, bracketed: bool) -> Vec<u8> {
        let body = self.render_body();
        if !bracketed {
            return body;
        }
        let mut framed = Vec::with_capacity(body.len() + 12);
        framed.extend_from_slice(b"\x1b[200~");
        let mut rest = body.as_slice();
        while let Some(pos) = rest
            .windows(6)
            .position(|w| w == b"\x1b[201~")
        {
            framed.extend_from_slice(&rest[..pos]);
            rest = &rest[pos + 6..];
        }
        framed.extend_from_slice(rest);
        framed.extend_from_slice(b"\x1b[201~");
        framed
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
    /// The owing party (always the target for courtesy) sent an update
    /// after the ack. Satisfies the obligation outright: sweeps never
    /// remind a conversation its target already updated.
    target_updated: bool,
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
    /// Broker epoch, reminted on every boot: cursors, session references,
    /// conversations, and idempotency records are valid within one epoch
    /// only. Restart drops the whole broker by construction.
    epoch: u64,
    /// Operator-registered bot peers (no panes, cursor inboxes only).
    clients: HashMap<String, BotClient>,
    /// Conversations with a bot peer as one party. Parallel to `convs`
    /// so bot traffic never touches terminal queues.
    bot_convs: HashMap<String, BotConv>,
    /// Exited sessions with bot failure notices still unsent (their
    /// inbox was full). Sweeps retry them and drop ids with nothing
    /// Open left; without this, a lost deposit would close the conv
    /// while the notice never arrives.
    dead_sessions: HashSet<SessionId>,
    /// Bot failure notices that missed a full inbox at exit time:
    /// (client, conversation, session ID, session name). The tick
    /// retries each until deposited; Open records close on success,
    /// Done ones were already terminal.
    pending_failures: Vec<(String, String, String, String)>,
    /// Session-path idempotency records, one cache per caller so two
    /// sessions may mint the same key without colliding and a noisy
    /// session's flood evicts only its own records, never another
    /// caller's live retry. Retries after an IPC or bridge timeout
    /// replay the stored verdict instead of minting a duplicate send.
    /// Keyless calls bypass it and execute every time, exactly as
    /// before. Entries die with their caller on exit.
    session_idem: HashMap<String, IdemCache>,
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
            epoch: crate::bot::generate_epoch(),
            clients: HashMap::new(),
            bot_convs: HashMap::new(),
            dead_sessions: HashSet::new(),
            pending_failures: Vec::new(),
            session_idem: HashMap::new(),
        }
    }

    /// Current broker epoch for poll responses and staleness checks.
    pub fn epoch(&self) -> u64 {
        self.epoch
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

    /// Drop a session from every group it belongs to (termination path);
    /// true when it belonged to at least one.
    pub fn leave_all(&mut self, id: SessionId) -> bool {
        let groups = self.membership.remove(&id).unwrap_or_default();
        let mut removed = false;
        for group in &groups {
            if let Some(g) = self.groups.get_mut(group) {
                removed |= g.members.remove(&id);
            }
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

    /// Pressure on a target: queued injections, delivered asks still
    /// awaiting a response, and armed timers (future queue entries).
    /// One ask occupies exactly one side at a time. Every admission
    /// gate (sends and schedules) reads this one budget, so timers
    /// plus sends can never stack past the cap.
    ///
    /// Completions bypass the gate by design and are not counted
    /// against it: a response resolves its ask (net zero), a fired
    /// timer converts to one queued entry (net zero), and acks,
    /// reminders, and failure notices each answer already-admitted
    /// work. Gating them would fail the very completion that drains
    /// pressure; sequential delivery paces the pane instead.
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
        let bot_asks = self
            .bot_convs
            .values()
            .filter(|c| {
                c.state == BotConvState::Open
                    && c.kind == BotConvKind::Ask
                    && c.from_client
                    && c.session == id
                    && c.delivered
            })
            .count();
        let timers = self.timers.values().filter(|t| t.target == id).count();
        self.queued(id) + asks + bot_asks + timers
    }

    /// Oldest queued injection without popping: delivery peeks, writes,
    /// and only then pops, so a failed write retries on the next settle
    /// instead of losing the message with its pressure already moved.
    pub fn peek_due(&self, id: SessionId) -> Option<Injection> {
        self.queue.get(&id)?.front().cloned()
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
                } else if let Some(conv) = self.bot_convs.get_mut(&inj.conv) {
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

    fn names(&self, sessions: &SessionManager, id: SessionId) -> String {
        sessions
            .get(id)
            .map(|rec| rec.name.clone())
            .unwrap_or_default()
    }

    /// Human activity label for list output (sessions and bots share it).
    fn activity_label(activity: crate::session::Activity) -> &'static str {
        match activity {
            crate::session::Activity::Idle => "Idle",
            crate::session::Activity::Thinking => "Thinking",
            crate::session::Activity::ToolUse => "ToolUse",
            crate::session::Activity::Waiting => "Waiting",
            crate::session::Activity::Stopped => "Stopped",
        }
    }

    /// Parse an optional u64 tool arg: bare JSON numbers or quoted
    /// digits. Absent reads as missing; present-but-garbled is a
    /// client bug, never silent.
    fn arg_u64(args: &str, name: &str) -> Option<Result<u64, BotError>> {
        let raw = crate::mcp::top_raw(args, name)?.trim();
        let bare = raw
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(raw);
        Some(bare.parse::<u64>().map_err(|_| {
            BotError::new(
                ErrorCode::InvalidArguments,
                format!("{name} must be a non-negative integer"),
            )
        }))
    }

    /// Reject calls presenting a stale epoch: cursors and references
    /// from a previous broker run must never read as current.
    fn check_epoch(&self, args: &str) -> Result<(), BotError> {
        match Self::arg_u64(args, "epoch") {
            None => Ok(()),
            Some(Ok(e)) if e == self.epoch => Ok(()),
            Some(Ok(_)) => Err(BotError::new(
                ErrorCode::Conflict,
                "epoch changed; re-list and resume",
            )),
            Some(Err(e)) => Err(e),
        }
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
        if tool == "list_sessions" {
            return Ok(self.list_sessions(sessions, caller));
        }
        // Keyed sends fingerprint the tool plus the caller's exact
        // argument bytes, so a byte-identical retry replays while any
        // changed send under the same key conflicts loudly instead
        // of silently duplicating or, worse, returning a wrong
        // conversation. Broker rejections are never stored.
        let caller_s = caller.to_string();
        let keyed = match Self::arg(args, "idempotency_key").filter(|s| !s.is_empty()) {
            None => None,
            Some(key) => {
                crate::bot::validate_key(&key).map_err(|e| e.message)?;
                let fp = crate::bot::fingerprint(tool, &[&caller_s, args]);
                let cached = self
                    .session_idem
                    .entry(caller_s.clone())
                    .or_insert_with(IdemCache::new)
                    .check(&key, fp, now);
                match cached {
                    IdemCheck::Hit(result) => return Ok(result),
                    IdemCheck::Miss => {}
                    IdemCheck::Conflict => {
                        return Err(
                            "idempotency key reused with different arguments".to_string()
                        );
                    }
                }
                Some((key, fp))
            }
        };
        // New work gates on the caller's own backlog: completions
        // bypass deliberately, so without this a session that never
        // drains could pile responses behind itself without limit.
        // Retries replay above, so only genuinely new sends wait.
        if (tool == "ask_session" || tool == "tell_session")
            && self.queued(caller) >= QUEUE_CAP
        {
            return Err("caller queue full; drain it before sending".to_string());
        }
        let result = match tool {
            "ask_session" => self.ask(sessions, caller, args, now),
            "send_response" => self.send_response(sessions, caller, args, now),
            "tell_session" => self.tell(sessions, caller, args, now),
            "ack_message" => self.ack(sessions, caller, args, now),
            "compact_session" => self.compact(sessions, caller, args),
            "schedule_prompt" => self.schedule(sessions, caller, args, now),
            "cancel_scheduled_prompt" => self.cancel_scheduled(caller, args),
            _ => Err("unknown tool".to_string()),
        };
        if let (Some((key, fp)), Ok(line)) = (keyed, &result) {
            if let Some(cache) = self.session_idem.get_mut(&caller_s) {
                cache.store(&key, fp, line, now);
            }
        }
        result
    }

    /// Operator registration of one bot peer: validated name, at least
    /// one group grant, operator-minted credential, optional tool
    /// grants (messaging is the default; nothing else is implied).
    /// Rejects duplicates and collisions with live session names so
    /// routing can never be ambiguous. No MCP path calls this.
    pub fn register_client(
        &mut self,
        sessions: &SessionManager,
        name: &str,
        groups: Vec<String>,
        token: &str,
        grants: Vec<String>,
    ) -> Result<(), BotError> {
        crate::bot::validate_name(name)?;
        if groups.is_empty() {
            return Err(BotError::new(
                ErrorCode::InvalidArguments,
                "client needs at least one group",
            ));
        }
        for g in &groups {
            crate::bot::validate_group(g)?;
        }
        crate::bot::validate_token(token)?;
        if self.clients.contains_key(name) {
            return Err(BotError::new(
                ErrorCode::Conflict,
                "client already registered",
            ));
        }
        for &id in sessions.order() {
            let live =
                sessions.get(id).is_some_and(|rec| rec.name == name && rec.state.is_live());
            if live {
                return Err(BotError::new(
                    ErrorCode::Conflict,
                    "name is taken by a live session",
                ));
            }
        }
        self.clients.insert(
            name.to_string(),
            BotClient::new(name, token, groups, grants),
        );
        Ok(())
    }

    /// Operator file-bound registration: validates name and grants
    /// like [`Self::register_client`] but takes no inline secret — the
    /// credential arrives exclusively through the bound token file
    /// (empty never authenticates, so an unread file fails closed).
    pub fn register_client_file(
        &mut self,
        sessions: &SessionManager,
        name: &str,
        groups: Vec<String>,
        grants: Vec<String>,
        token_file: std::path::PathBuf,
    ) -> Result<(), BotError> {
        crate::bot::validate_name(name)?;
        if groups.is_empty() {
            return Err(BotError::new(
                ErrorCode::InvalidArguments,
                "client needs at least one group",
            ));
        }
        for g in &groups {
            crate::bot::validate_group(g)?;
        }
        if self.clients.contains_key(name) {
            return Err(BotError::new(
                ErrorCode::Conflict,
                "client already registered",
            ));
        }
        for &id in sessions.order() {
            let live =
                sessions.get(id).is_some_and(|rec| rec.name == name && rec.state.is_live());
            if live {
                return Err(BotError::new(
                    ErrorCode::Conflict,
                    "name is taken by a live session",
                ));
            }
        }
        let mut client = BotClient::new(name, "", groups, grants);
        client.set_token_file(token_file);
        client.refresh_token();
        self.clients.insert(name.to_string(), client);
        Ok(())
    }

    /// Revoke one client: drops the inbox and fails its open
    /// conversations, telling session peers loudly through their panes.
    /// The credential stops working on the very next call.
    pub fn revoke_client(&mut self, name: &str) -> bool {
        if self.clients.remove(name).is_none() {
            return false;
        }
        let mut notify = Vec::new();
        for (conv_id, conv) in self.bot_convs.iter_mut() {
            if conv.client != name || conv.state != BotConvState::Open {
                continue;
            }
            conv.state = BotConvState::Failed;
            notify.push((conv.session, conv_id.clone()));
        }
        for (session, conv_id) in notify {
            self.push(
                session,
                Injection {
                    conv: conv_id,
                    kind: InjectKind::Failed,
                    from: name.to_string(),
                    text: "client revoked".to_string(),
                },
            );
        }
        true
    }

    /// Authenticate one bot call. Unknown names and wrong credentials
    /// share one `unauthorized` answer (no oracle); the stored secret is
    /// only ever constant-time compared, never logged or returned.
    /// Called on every bot call, so rotation and revocation bite at once.
    fn check_client(&self, name: &str, token: &str) -> Result<(), BotError> {
        const DUMMY: &str = "0123456789abcdef0123456789abcdef";
        match self.clients.get(name) {
            Some(c) if c.check_token(token) => Ok(()),
            Some(_) => Err(BotError::new(
                ErrorCode::Unauthorized,
                "unknown or revoked client",
            )),
            None => {
                let _ = crate::bot::token_eq(DUMMY, token);
                Err(BotError::new(
                    ErrorCode::Unauthorized,
                    "unknown or revoked client",
                ))
            }
        }
    }

    /// Whether a session and a client grant list share a group.
    fn shares_with_client(&self, id: SessionId, grants: &[String]) -> bool {
        self.membership
            .get(&id)
            .is_some_and(|mine| mine.iter().any(|g| grants.iter().any(|h| h == g)))
    }

    /// Open client-originated conversations: asks plus unacked tells.
    /// Mirrors pressure semantics at the client level (delivered tells
    /// no longer count).
    fn client_outstanding(&self, name: &str) -> usize {
        self.bot_convs
            .values()
            .filter(|c| {
                c.client == name
                    && c.from_client
                    && c.state == BotConvState::Open
                    && (c.kind == BotConvKind::Ask || !c.acked)
            })
            .count()
    }

    /// Execute one bot tool under an authenticated client identity.
    pub fn bot_call(
        &mut self,
        sessions: &SessionManager,
        name: &str,
        token: &str,
        tool: &str,
        args: &str,
        now: Instant,
    ) -> Result<String, BotError> {
        // Bound token files re-read on every call: rotation and
        // deletion bite at once, with no grant cache anywhere.
        if let Some(client) = self.clients.get_mut(name) {
            client.refresh_token();
        }
        self.check_client(name, token)?;
        match tool {
            "list_sessions" => Ok(self.bot_list_sessions(sessions, name)),
            "ask_session" => self.bot_ask(sessions, name, args, now),
            "tell_session" => self.bot_tell(sessions, name, args, now),
            "send_response" => self.bot_respond(sessions, name, args, now),
            "ack_message" => self.bot_ack_msg(sessions, name, args, now),
            "bot_poll" => self.bot_poll(name, args),
            "bot_ack" => self.bot_ack_cursor(name, args),
            "start_session" => Err(BotError::new(
                ErrorCode::Unauthorized,
                "start_session is not enabled for external bots in this release",
            )),
            _ => Err(BotError::new(ErrorCode::NotFound, "unknown tool")),
        }
    }

    /// Bot-visible discovery: only sessions sharing a grant group,
    /// with epoch-scoped session IDs. Never the unscoped agent list.
    fn bot_list_sessions(&self, sessions: &SessionManager, client: &str) -> String {
        let grants = self
            .clients
            .get(client)
            .map(|c| c.groups.clone())
            .unwrap_or_default();
        let mut out = format!(
            r#"{{"you":{},"epoch":{},"sessions":["#,
            crate::mcp::escape_json(client),
            self.epoch,
        );
        let mut first = true;
        for &id in sessions.order() {
            let Some(rec) = sessions.get(id) else {
                continue;
            };
            if !rec.state.is_live() {
                continue;
            }
            if !self.shares_with_client(id, &grants) {
                continue;
            }
            if !first {
                out.push(',');
            }
            first = false;
            let activity = Self::activity_label(rec.activity);
            let group = self
                .primary_group(id)
                .map(crate::mcp::escape_json)
                .unwrap_or_else(|| "null".to_string());
            out.push_str(&format!(
                r#"{{"id":"{id}","name":{},"activity":"{activity}","group":{group}}}"#,
                crate::mcp::escape_json(&rec.name),
            ));
        }
        out.push_str("]}");
        out
    }

    fn bot_ask(
        &mut self,
        sessions: &SessionManager,
        client: &str,
        args: &str,
        now: Instant,
    ) -> Result<String, BotError> {
        let target_name = Self::arg(args, "target")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| BotError::new(ErrorCode::InvalidArguments, "ask needs a target"))?;
        let text = Self::arg2(args, &["message", "text"])
            .filter(|s| !s.is_empty())
            .ok_or_else(|| BotError::new(ErrorCode::InvalidArguments, "ask needs text"))?;
        let key = Self::arg(args, "idempotency_key").filter(|s| !s.is_empty());
        let fp = crate::bot::fingerprint("ask_session", &[&target_name, &text, ""]);
        if let Some(replay) = self
            .clients
            .get_mut(client)
            .expect("caller authenticated")
            .idem_check(key.as_deref(), fp, now)?
        {
            return Ok(replay);
        }
        let target = self.resolve_target(sessions, &target_name).map_err(|e| {
            if e.starts_with("ambiguous") {
                BotError::new(ErrorCode::AmbiguousTarget, e)
            } else {
                BotError::new(ErrorCode::NotFound, e)
            }
        })?;
        {
            let grants = self
                .clients
                .get(client)
                .expect("caller authenticated")
                .groups
                .clone();
            if !self.shares_with_client(target, &grants) {
                return Err(BotError::new(
                    ErrorCode::NoSharedGroup,
                    "no shared group with target",
                ));
            }
        }
        if self.pressure(sessions, target) >= PRESSURE_CAP {
            return Err(BotError::new(
                ErrorCode::PressureLimit,
                format!("pressure cap reached for {target_name:?}"),
            ));
        }
        if self.client_outstanding(client) >= crate::bot::CLIENT_MAX_OUTSTANDING {
            return Err(BotError::new(
                ErrorCode::PressureLimit,
                "client send cap reached",
            ));
        }
        let conv = crate::ids::ConversationId::generate().to_string();
        let target_name_live = self.names(sessions, target);
        self.push(
            target,
            Injection {
                conv: conv.clone(),
                kind: InjectKind::Ask,
                from: client.to_string(),
                text,
            },
        );
        self.bot_convs.insert(
            conv.clone(),
            BotConv {
                kind: BotConvKind::Ask,
                session: target,
                session_name: target_name_live,
                client: client.to_string(),
                from_client: true,
                state: BotConvState::Open,
                acked: false,
                delivered: false,
                last_update: now,
                reminded: false,
                target_updated: false,
            },
        );
        let result = format!(r#"{{"conversation":"{conv}","epoch":{}}}"#, self.epoch);
        self.clients
            .get_mut(client)
            .expect("caller authenticated")
            .idem_store(key.as_deref(), fp, &result, now);
        Ok(result)
    }

    fn bot_tell(
        &mut self,
        sessions: &SessionManager,
        client: &str,
        args: &str,
        now: Instant,
    ) -> Result<String, BotError> {
        let target_name = Self::arg(args, "target")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| BotError::new(ErrorCode::InvalidArguments, "tell needs a target"))?;
        let text = Self::arg2(args, &["message", "text"])
            .filter(|s| !s.is_empty())
            .ok_or_else(|| BotError::new(ErrorCode::InvalidArguments, "tell needs text"))?;
        let key = Self::arg(args, "idempotency_key").filter(|s| !s.is_empty());
        if let Some(id) = Self::arg2(args, &["conversation_id", "conversation"])
            .filter(|s| !s.is_empty())
        {
            let fp = crate::bot::fingerprint("tell_session", &[&target_name, &text, &id]);
            if let Some(replay) = self
                .clients
                .get_mut(client)
                .expect("caller authenticated")
                .idem_check(key.as_deref(), fp, now)?
            {
                return Ok(replay);
            }
            let result = self.bot_tell_followup(sessions, client, &id, &target_name, &text, now)?;
            self.clients
                .get_mut(client)
                .expect("caller authenticated")
                .idem_store(key.as_deref(), fp, &result, now);
            return Ok(result);
        }
        let fp = crate::bot::fingerprint("tell_session", &[&target_name, &text, ""]);
        if let Some(replay) = self
            .clients
            .get_mut(client)
            .expect("caller authenticated")
            .idem_check(key.as_deref(), fp, now)?
        {
            return Ok(replay);
        }
        let target = self.resolve_target(sessions, &target_name).map_err(|e| {
            if e.starts_with("ambiguous") {
                BotError::new(ErrorCode::AmbiguousTarget, e)
            } else {
                BotError::new(ErrorCode::NotFound, e)
            }
        })?;
        {
            let grants = self
                .clients
                .get(client)
                .expect("caller authenticated")
                .groups
                .clone();
            if !self.shares_with_client(target, &grants) {
                return Err(BotError::new(
                    ErrorCode::NoSharedGroup,
                    "no shared group with target",
                ));
            }
        }
        if self.pressure(sessions, target) >= PRESSURE_CAP {
            return Err(BotError::new(
                ErrorCode::PressureLimit,
                format!("pressure cap reached for {target_name:?}"),
            ));
        }
        if self.client_outstanding(client) >= crate::bot::CLIENT_MAX_OUTSTANDING {
            return Err(BotError::new(
                ErrorCode::PressureLimit,
                "client send cap reached",
            ));
        }
        let conv = crate::ids::ConversationId::generate().to_string();
        let target_name_live = self.names(sessions, target);
        self.push(
            target,
            Injection {
                conv: conv.clone(),
                kind: InjectKind::Tell,
                from: client.to_string(),
                text,
            },
        );
        self.bot_convs.insert(
            conv.clone(),
            BotConv {
                kind: BotConvKind::Tell,
                session: target,
                session_name: target_name_live,
                client: client.to_string(),
                from_client: true,
                state: BotConvState::Open,
                acked: false,
                delivered: false,
                last_update: now,
                reminded: false,
                target_updated: false,
            },
        );
        let result = format!(r#"{{"conversation":"{conv}","epoch":{}}}"#, self.epoch);
        self.clients
            .get_mut(client)
            .expect("caller authenticated")
            .idem_store(key.as_deref(), fp, &result, now);
        Ok(result)
    }

    /// Client follow-up on its own tell: the update reaches the session
    /// pane. Conversations owned by another client read as unknown.
    fn bot_tell_followup(
        &mut self,
        sessions: &SessionManager,
        client: &str,
        id: &str,
        target_name: &str,
        text: &str,
        now: Instant,
    ) -> Result<String, BotError> {
        let (kind, state, session) = match self.bot_convs.get(id) {
            Some(c) if c.client == client => (c.kind, c.state, c.session),
            _ => {
                return Err(BotError::new(
                    ErrorCode::NotFound,
                    "unknown conversation",
                ));
            }
        };
        if kind != BotConvKind::Tell {
            return Err(BotError::new(
                ErrorCode::ConversationClosed,
                "only a tell takes follow-ups",
            ));
        }
        if state != BotConvState::Open {
            return Err(BotError::new(
                ErrorCode::ConversationClosed,
                "conversation is closed",
            ));
        }
        let live = self.resolve_target(sessions, target_name).map_err(|e| {
            if e.starts_with("ambiguous") {
                BotError::new(ErrorCode::AmbiguousTarget, e)
            } else {
                BotError::new(ErrorCode::NotFound, e)
            }
        })?;
        if live != session {
            return Err(BotError::new(
                ErrorCode::ConversationClosed,
                "follow-up target mismatch",
            ));
        }
        {
            let grants = self
                .clients
                .get(client)
                .expect("caller authenticated")
                .groups
                .clone();
            if !self.shares_with_client(session, &grants) {
                return Err(BotError::new(
                    ErrorCode::NoSharedGroup,
                    "no shared group with target",
                ));
            }
        }
        if self.pressure(sessions, session) >= PRESSURE_CAP {
            return Err(BotError::new(
                ErrorCode::PressureLimit,
                format!("pressure cap reached for {target_name:?}"),
            ));
        }
        self.push(
            session,
            Injection {
                conv: id.to_string(),
                kind: InjectKind::FollowUp,
                from: client.to_string(),
                text: text.to_string(),
            },
        );
        if let Some(conv) = self.bot_convs.get_mut(id) {
            // The client owes the update exactly when the session told
            // it (session-originated tell): then this follow-up
            // satisfies courtesy for good.
            if !conv.from_client {
                conv.target_updated = true;
            }
            conv.last_update = now;
        }
        Ok(format!(
            r#"{{"conversation":"{id}","followup":true,"epoch":{}}}"#,
            self.epoch
        ))
    }

    fn bot_respond(
        &mut self,
        sessions: &SessionManager,
        client: &str,
        args: &str,
        now: Instant,
    ) -> Result<String, BotError> {
        let id = Self::arg2(args, &["conversation_id", "conversation"])
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                BotError::new(ErrorCode::InvalidArguments, "a conversation ID is required")
            })?;
        let text = Self::arg2(args, &["message", "text"])
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                BotError::new(ErrorCode::InvalidArguments, "send_response needs text")
            })?;
        let (kind, state, session, from_client) = match self.bot_convs.get(&id) {
            Some(c) if c.client == client => (c.kind, c.state, c.session, c.from_client),
            _ => {
                return Err(BotError::new(
                    ErrorCode::NotFound,
                    "unknown conversation",
                ));
            }
        };
        if kind != BotConvKind::Ask {
            return Err(BotError::new(
                ErrorCode::ConversationClosed,
                "only an ask takes a response",
            ));
        }
        if state != BotConvState::Open {
            return Err(BotError::new(
                ErrorCode::ConversationClosed,
                "conversation is closed",
            ));
        }
        if from_client {
            return Err(BotError::new(
                ErrorCode::ConversationClosed,
                "only the target answers",
            ));
        }
        // Group grants are live: a session or client removed from the
        // shared group since the ask loses the answer channel too.
        let granted = self
            .clients
            .get(client)
            .is_some_and(|c| self.shares_with_client(session, &c.groups));
        if !granted {
            return Err(BotError::new(
                ErrorCode::NoSharedGroup,
                "no shared group with target",
            ));
        }
        self.push(
            session,
            Injection {
                conv: id.clone(),
                kind: InjectKind::Response,
                from: client.to_string(),
                text,
            },
        );
        if let Some(conv) = self.bot_convs.get_mut(&id) {
            conv.state = BotConvState::Done;
            conv.last_update = now;
        }
        let _ = sessions;
        Ok(format!(
            r#"{{"conversation":"{id}","completed":true,"epoch":{}}}"#,
            self.epoch
        ))
    }

    fn bot_ack_msg(
        &mut self,
        sessions: &SessionManager,
        client: &str,
        args: &str,
        now: Instant,
    ) -> Result<String, BotError> {
        let id = Self::arg2(args, &["conversation_id", "conversation"])
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                BotError::new(ErrorCode::InvalidArguments, "a conversation ID is required")
            })?;
        let (kind, state, session, from_client, acked) = match self.bot_convs.get(&id) {
            Some(c) if c.client == client => {
                (c.kind, c.state, c.session, c.from_client, c.acked)
            }
            _ => {
                return Err(BotError::new(
                    ErrorCode::NotFound,
                    "unknown conversation",
                ));
            }
        };
        if kind != BotConvKind::Tell {
            return Err(BotError::new(
                ErrorCode::ConversationClosed,
                "nothing to acknowledge",
            ));
        }
        if state != BotConvState::Open {
            return Err(BotError::new(
                ErrorCode::ConversationClosed,
                "conversation is closed",
            ));
        }
        if from_client {
            return Err(BotError::new(
                ErrorCode::ConversationClosed,
                "only the target acknowledges",
            ));
        }
        // Group grants are live: leaving the shared group since the
        // tell revokes the ack channel too.
        let granted = self
            .clients
            .get(client)
            .is_some_and(|c| self.shares_with_client(session, &c.groups));
        if !granted {
            return Err(BotError::new(
                ErrorCode::NoSharedGroup,
                "no shared group with target",
            ));
        }
        if acked {
            // Idempotent replay, same shape as the session path: no
            // duplicate Ack, no courtesy-clock nudge.
            return Ok(format!(
                r#"{{"conversation":"{id}","acknowledged":true,"epoch":{}}}"#,
                self.epoch
            ));
        }
        self.push(
            session,
            Injection {
                conv: id.clone(),
                kind: InjectKind::Ack,
                from: client.to_string(),
                text: "ack_message".to_string(),
            },
        );
        if let Some(conv) = self.bot_convs.get_mut(&id) {
            conv.acked = true;
            conv.last_update = now;
        }
        let _ = sessions;
        Ok(format!(
            r#"{{"conversation":"{id}","acknowledged":true,"epoch":{}}}"#,
            self.epoch
        ))
    }

    fn bot_poll(&mut self, client: &str, args: &str) -> Result<String, BotError> {
        self.check_epoch(args)?;
        // Event IDs restart at 1 every epoch, so a resumed nonzero
        // cursor that omits the epoch would echo itself as
        // next_cursor forever while fresh events go unseen.
        let cursor_arg = Self::arg_u64(args, "cursor");
        if let Some(Ok(c)) = cursor_arg {
            if c != 0 && Self::arg_u64(args, "epoch").is_none() {
                return Err(BotError::new(
                    ErrorCode::Conflict,
                    "nonzero cursor requires the epoch",
                ));
            }
        }
        let limit = match Self::arg_u64(args, "limit") {
            None => crate::bot::POLL_MAX_EVENTS,
            Some(Ok(n)) => usize::try_from(n)
                .unwrap_or(crate::bot::POLL_MAX_EVENTS)
                .clamp(1, crate::bot::POLL_MAX_EVENTS),
            Some(Err(e)) => return Err(e),
        };
        let acked = self
            .clients
            .get(client)
            .expect("caller authenticated")
            .acked_cursor();
        let start = match cursor_arg {
            None => acked,
            Some(Ok(c)) => c.max(acked),
            Some(Err(e)) => return Err(e),
        };
        // A cursor past everything produced names no event in this
        // epoch (stale pre-restart cursor, or client bug): echoing it
        // back would skip the whole inbox, so conflict instead.
        let produced = self
            .clients
            .get(client)
            .expect("caller authenticated")
            .produced_upto();
        if start > produced {
            return Err(BotError::new(
                ErrorCode::Conflict,
                "cursor is ahead of produced events; poll without a cursor to resume",
            ));
        }
        let events = self
            .clients
            .get_mut(client)
            .expect("caller authenticated")
            .poll(start, limit);
        let next = events.last().map(|e| e.id).unwrap_or(start);
        let mut out = String::from("{\"events\":[");
        for (i, e) in events.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&e.to_json());
        }
        out.push_str(&format!(r#"],"next_cursor":{next},"epoch":{}}}"#, self.epoch));
        Ok(out)
    }

    fn bot_ack_cursor(&mut self, client: &str, args: &str) -> Result<String, BotError> {
        self.check_epoch(args)?;
        // Same restart rule as polls: event IDs restart at 1 every
        // epoch, so a resumed nonzero ack without the epoch could
        // coincide with the fresh received prefix and delete new
        // events. Zero (which advances nothing) needs no epoch.
        if let Some(Ok(c)) = Self::arg_u64(args, "cursor") {
            if c != 0 && Self::arg_u64(args, "epoch").is_none() {
                return Err(BotError::new(
                    ErrorCode::Conflict,
                    "nonzero cursor requires the epoch",
                ));
            }
        }
        let cursor = match Self::arg_u64(args, "cursor") {
            Some(Ok(c)) => c,
            _ => {
                return Err(BotError::new(
                    ErrorCode::InvalidArguments,
                    "bot_ack needs a cursor",
                ));
            }
        };
        let acked = self
            .clients
            .get_mut(client)
            .expect("caller authenticated")
            .ack(cursor)?;
        Ok(format!(
            r#"{{"acknowledged":{acked},"epoch":{}}}"#,
            self.epoch
        ))
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
        let target = match self.resolve_target(sessions, &target_name) {
            Ok(id) => {
                if !self.shared_group(caller, id) {
                    return Err("no shared group with target".to_string());
                }
                if self.pressure(sessions, id) >= PRESSURE_CAP {
                    return Err(format!("pressure cap reached for {target_name:?}"));
                }
                id
            }
            Err(e) if e.starts_with("ambiguous") => return Err(e),
            Err(e) => {
                if !self.clients.contains_key(&target_name) {
                    return Err(e);
                }
                return self.send_client(sessions, caller, &target_name, &text, now, BotConvKind::Ask);
            }
        };
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
                target_updated: false,
                delivered: false,
            },
        );
        Ok(format!(r#"{{"conversation":"{conv}"}}"#))
    }

    /// Session sends to a bot peer (ask or new tell): same
    /// shared-group and inbox-cap rules as any send, delivered as a
    /// structured inbox event — never a pane write, so polling cannot
    /// race terminal delivery.
    fn send_client(
        &mut self,
        sessions: &SessionManager,
        caller: SessionId,
        target_name: &str,
        text: &str,
        now: Instant,
        kind: BotConvKind,
    ) -> Result<String, String> {
        let groups = self
            .clients
            .get(target_name)
            .map(|c| c.groups.clone())
            .expect("caller checked registration");
        if !self.shares_with_client(caller, &groups) {
            return Err("no shared group with target".to_string());
        }
        if self
            .clients
            .get(target_name)
            .expect("caller checked registration")
            .unacked()
            >= crate::bot::INBOX_CAP
        {
            return Err(format!("pressure cap reached for {target_name:?}"));
        }
        let conv = crate::ids::ConversationId::generate().to_string();
        let from_name = self.names(sessions, caller);
        let from_id = caller.to_string();
        let event = match kind {
            BotConvKind::Ask => BotKind::Ask,
            BotConvKind::Tell => BotKind::Tell,
        };
        self.clients
            .get_mut(target_name)
            .expect("caller checked registration")
            .deposit(
                event,
                &conv,
                &from_id,
                &from_name,
                text,
                crate::bot::now_unix_ms(),
            )
            .map_err(|_| format!("pressure cap reached for {target_name:?}"))?;
        self.bot_convs.insert(
            conv.clone(),
            BotConv {
                kind,
                session: caller,
                session_name: from_name,
                client: target_name.to_string(),
                from_client: false,
                state: BotConvState::Open,
                acked: false,
                delivered: false,
                last_update: now,
                reminded: false,
                target_updated: false,
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
        // Same budget as sends: timers are future queue entries, so a
        // loaded target refuses new ones instead of stacking past the cap.
        if self.pressure(sessions, caller) >= PRESSURE_CAP {
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
        if let Some(r) = self.respond_bot(sessions, caller, &id, &text, now) {
            return r;
        }
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
            if !self.shared_group(caller, conv.source) {
                return Err("no shared group with target".to_string());
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
            if let Some(r) = self.tell_bot_followup(sessions, caller, &id, &target_name, &text, now) {
                return r;
            }
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
                // Only the owing party's update satisfies courtesy: a
                // target follow-up suppresses the reminder for good,
                // while the source's own follow-up merely restarts grace.
                if caller == conv.target {
                    conv.target_updated = true;
                }
                conv.last_update = now;
            }
            return Ok(format!(r#"{{"conversation":"{id}","followup":true}}"#));
        }
        let target = match self.resolve_target(sessions, &target_name) {
            Ok(id) => {
                if !self.shared_group(caller, id) {
                    return Err("no shared group with target".to_string());
                }
                if self.pressure(sessions, id) >= PRESSURE_CAP {
                    return Err(format!("pressure cap reached for {target_name:?}"));
                }
                id
            }
            Err(e) if e.starts_with("ambiguous") => return Err(e),
            Err(e) => {
                if !self.clients.contains_key(&target_name) {
                    return Err(e);
                }
                return self.send_client(sessions, caller, &target_name, &text, now, BotConvKind::Tell);
            }
        };
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
                target_updated: false,
                delivered: false,
            },
        );
        Ok(format!(r#"{{"conversation":"{conv}"}}"#))
    }

    /// Session follow-up on a bot conversation: the peer is the client,
    /// so the update lands in its inbox, never a pane. `None` when the
    /// ID is not a bot conversation (the session path decides).
    fn tell_bot_followup(
        &mut self,
        sessions: &SessionManager,
        caller: SessionId,
        id: &str,
        target_name: &str,
        text: &str,
        now: Instant,
    ) -> Option<Result<String, String>> {
        let (kind, state, session, client) = match self.bot_convs.get(id) {
            Some(c) => (c.kind, c.state, c.session, c.client.clone()),
            None => return None,
        };
        if kind != BotConvKind::Tell {
            return Some(Err("only a tell takes follow-ups".to_string()));
        }
        if state != BotConvState::Open {
            return Some(Err("conversation is closed".to_string()));
        }
        if caller != session {
            return Some(Err("only conversation parties can follow up".to_string()));
        }
        if target_name != client {
            return Some(Err("follow-up target mismatch".to_string()));
        }
        let groups = self
            .clients
            .get(&client)
            .map(|c| c.groups.clone())
            .unwrap_or_default();
        if !self.shares_with_client(caller, &groups) {
            return Some(Err("no shared group with target".to_string()));
        }
        if self
            .clients
            .get(&client)
            .is_some_and(|c| c.unacked() >= crate::bot::INBOX_CAP)
        {
            return Some(Err(format!("pressure cap reached for {target_name:?}")));
        }
        let from_name = self.names(sessions, caller);
        let from_id = caller.to_string();
        let deposit = self
            .clients
            .get_mut(&client)
            .expect("conversation names its client")
            .deposit(
                BotKind::FollowUp,
                id,
                &from_id,
                &from_name,
                text,
                crate::bot::now_unix_ms(),
            );
        if deposit.is_err() {
            return Some(Err(format!("pressure cap reached for {target_name:?}")));
        }
        if let Some(conv) = self.bot_convs.get_mut(id) {
            // The session owes the update exactly when the client told
            // it (client-originated tell): then this follow-up satisfies
            // courtesy for good.
            if conv.from_client {
                conv.target_updated = true;
            }
            conv.last_update = now;
        }
        Some(Ok(format!(r#"{{"conversation":"{id}","followup":true}}"#)))
    }

    /// Session answers a bot's ask: the answer lands in the client
    /// inbox. `None` when the ID is not a bot conversation.
    fn respond_bot(
        &mut self,
        sessions: &SessionManager,
        caller: SessionId,
        id: &str,
        text: &str,
        now: Instant,
    ) -> Option<Result<String, String>> {
        let (kind, state, session, client) = match self.bot_convs.get(id) {
            Some(c) => (c.kind, c.state, c.session, c.client.clone()),
            None => return None,
        };
        if kind != BotConvKind::Ask {
            return Some(Err("only an ask takes a response".to_string()));
        }
        if state != BotConvState::Open {
            return Some(Err("conversation is closed".to_string()));
        }
        if caller != session {
            return Some(Err("only the target answers".to_string()));
        }
        // The client grant behind this ask is live: leaving the shared
        // group since revokes the session's answer channel.
        let granted = self
            .clients
            .get(&client)
            .is_some_and(|c| self.shares_with_client(caller, &c.groups));
        if !granted {
            return Some(Err("no shared group with target".to_string()));
        }
        let from_name = self.names(sessions, caller);
        let from_id = caller.to_string();
        let deposit = self
            .clients
            .get_mut(&client)
            .expect("conversation names its client")
            .deposit(
                BotKind::Response,
                id,
                &from_id,
                &from_name,
                text,
                crate::bot::now_unix_ms(),
            );
        if deposit.is_err() {
            return Some(Err(format!("pressure cap reached for {client:?}")));
        }
        if let Some(conv) = self.bot_convs.get_mut(id) {
            conv.state = BotConvState::Done;
            conv.last_update = now;
        }
        Some(Ok(format!(r#"{{"conversation":"{id}","completed":true}}"#)))
    }

    /// Session acknowledges a bot's tell. `None` when the ID is not a
    /// bot conversation.
    fn ack_bot(
        &mut self,
        sessions: &SessionManager,
        caller: SessionId,
        id: &str,
        now: Instant,
    ) -> Option<Result<String, String>> {
        let (kind, state, session, client) = match self.bot_convs.get(id) {
            Some(c) => (c.kind, c.state, c.session, c.client.clone()),
            None => return None,
        };
        if kind != BotConvKind::Tell {
            return Some(Err("nothing to acknowledge".to_string()));
        }
        if state != BotConvState::Open {
            return Some(Err("conversation is closed".to_string()));
        }
        if caller != session {
            return Some(Err("only the target acknowledges".to_string()));
        }
        // The client grant behind this tell is live: leaving the shared
        // group since revokes the session's ack channel.
        let granted = self
            .clients
            .get(&client)
            .is_some_and(|c| self.shares_with_client(caller, &c.groups));
        if !granted {
            return Some(Err("no shared group with target".to_string()));
        }
        let from_name = self.names(sessions, caller);
        let from_id = caller.to_string();
        let deposit = self
            .clients
            .get_mut(&client)
            .expect("conversation names its client")
            .deposit(
                BotKind::Ack,
                id,
                &from_id,
                &from_name,
                "ack_message",
                crate::bot::now_unix_ms(),
            );
        if deposit.is_err() {
            return Some(Err(format!("pressure cap reached for {client:?}")));
        }
        if let Some(conv) = self.bot_convs.get_mut(id) {
            conv.acked = true;
            conv.last_update = now;
        }
        Some(Ok(format!(r#"{{"conversation":"{id}","acknowledged":true}}"#)))
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
        if let Some(r) = self.ack_bot(sessions, caller, &id, now) {
            return r;
        }
        let (source, acked) = {
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
            if !self.shared_group(caller, conv.source) {
                return Err("no shared group with target".to_string());
            }
            (conv.source, conv.acked)
        };
        if acked {
            // Idempotent replay: the Ack already went out, so a
            // retried acknowledgement succeeds without queueing a
            // duplicate or nudging the courtesy clock.
            return Ok(format!(r#"{{"conversation":"{id}","acknowledged":true}}"#));
        }
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
            let activity = Self::activity_label(rec.activity);
            let group = self
                .primary_group(id)
                .map(crate::mcp::escape_json)
                .unwrap_or_else(|| "null".to_string());
            out.push_str(&format!(
                r#"{{"name":{},"live":true,"activity":"{activity}","group":{group}}}"#,
                crate::mcp::escape_json(&rec.name),
            ));
        }
        // Bot peers sharing a group with the caller ride along, marked,
        // so sessions can address the clients they may ask or tell.
        // Nothing is listed when no clients exist: output is unchanged.
        let mut bots: Vec<String> = self
            .clients
            .values()
            .filter(|c| self.shares_with_client(caller, &c.groups))
            .map(|c| c.name.clone())
            .collect();
        bots.sort();
        for name in bots {
            let group = self
                .membership
                .get(&caller)
                .and_then(|mine| {
                    self.clients.get(&name).and_then(|c| {
                        mine.iter().find(|g| c.groups.contains(g)).map(|g| {
                            crate::mcp::escape_json(g)
                        })
                    })
                })
                .unwrap_or_else(|| "null".to_string());
            if !first {
                out.push(',');
            }
            first = false;
            out.push_str(&format!(
                r#"{{"name":{},"live":true,"activity":"Idle","group":{group},"bot":true}}"#,
                crate::mcp::escape_json(&name),
            ));
        }
        out.push_str("]}");
        out
    }

    /// A staged submit died to human input with its body already in
    /// the pane: the message may have mixed with the draft or been
    /// discarded. Tell the sender loudly (one notice, never chained).
    /// Asks and tells notify their source, responses and acks their
    /// target; every other kind stays best-effort as before (timer
    /// commands name no conversation at all, and follow-ups ride
    /// threads their sources already watch). Bot senders hear it
    /// through their inbox, parking on a full one like any exit
    /// notice. Nobody is told about their own typing.
    pub fn clobber_notice(
        &mut self,
        typer: SessionId,
        typer_name: &str,
        conv_id: &str,
        kind: InjectKind,
    ) {
        const TEXT: &str =
            "typed over your message before it submitted; confirm they saw it";
        if let Some(sender) = self.convs.get(conv_id).and_then(|conv| match kind {
            InjectKind::Ask | InjectKind::Tell => Some(conv.source),
            InjectKind::Response | InjectKind::Ack => Some(conv.target),
            _ => None,
        }) {
            if sender == typer {
                return;
            }
            self.push(
                sender,
                Injection {
                    conv: conv_id.to_string(),
                    kind: InjectKind::Failed,
                    from: typer_name.to_string(),
                    text: TEXT.to_string(),
                },
            );
            return;
        }
        // Same four kinds: follow-up senders are ambiguous on both
        // tables, so they stay best-effort.
        let clobbered = matches!(
            kind,
            InjectKind::Ask | InjectKind::Tell | InjectKind::Response | InjectKind::Ack
        );
        if clobbered {
            if let Some(client) = self
                .bot_convs
                .get(conv_id)
                .map(|conv| conv.client.clone())
            {
                let deposited = match self.clients.get_mut(&client) {
                    Some(c) => c
                        .deposit(
                            BotKind::Failed,
                            conv_id,
                            &typer.to_string(),
                            typer_name,
                            TEXT,
                            crate::bot::now_unix_ms(),
                        )
                        .is_ok(),
                    // No inbox exists: nothing to retry toward.
                    None => true,
                };
                if !deposited {
                    self.pending_failures.push((
                        client,
                        conv_id.to_string(),
                        typer.to_string(),
                        typer_name.to_string(),
                    ));
                }
            }
        }
        // Non-clobbered kinds, timer commands, and unknown IDs have
        // nobody to tell.
    }

    /// Fail every open conversation touching an exited session. Targets fail
    /// loudly (their sources are told); sources fail silently.
    pub fn target_exited(&mut self, _sessions: &SessionManager, id: SessionId) {
        // A dead caller can never retry (its run is unbound and a
        // restart mints a fresh session), so its idempotency records
        // go with it instead of occupying another caller's capacity.
        self.session_idem.remove(&id.to_string());
        // A dying queue can strand answers: a queued response or ack
        // means its sender already got success, so the other party must
        // hear the loss loudly instead of assuming it was read.
        let dropped = self.queue.remove(&id).unwrap_or_default();
        let mut bot_dropped = Vec::new();
        for inj in &dropped {
            let (other, from) = match inj.kind {
                InjectKind::Response | InjectKind::Ack => match self.convs.get(&inj.conv) {
                    Some(conv) => (conv.target, conv.source_name.clone()),
                    None => {
                        // Same stranded-answer shape, bot table: the
                        // other party is a client, told via its inbox
                        // below instead of a session queue.
                        if let Some(conv) = self.bot_convs.get(&inj.conv) {
                            bot_dropped.push((
                                conv.client.clone(),
                                inj.conv.clone(),
                                conv.session.to_string(),
                                conv.session_name.clone(),
                            ));
                        }
                        continue;
                    }
                },
                _ => continue,
            };
            if other == id {
                continue;
            }
            self.push(
                other,
                Injection {
                    conv: inj.conv.clone(),
                    kind: InjectKind::Failed,
                    from,
                    text: "source exited before delivery".to_string(),
                },
            );
        }
        for (client, conv_id, session_id, session_name) in bot_dropped {
            // Deposit first: only a recorded failure closes an Open
            // record. A full inbox parks the notice for the tick
            // retry; Done records were already terminal either way.
            let deposited = match self.clients.get_mut(&client) {
                Some(c) => c
                    .deposit(
                        BotKind::Failed,
                        &conv_id,
                        &session_id,
                        &session_name,
                        "target exited",
                        crate::bot::now_unix_ms(),
                    )
                    .is_ok(),
                // No inbox exists: nothing to retry toward.
                None => true,
            };
            if deposited {
                if let Some(conv) = self.bot_convs.get_mut(&conv_id) {
                    if conv.state == BotConvState::Open {
                        conv.state = BotConvState::Failed;
                    }
                }
            } else {
                self.pending_failures
                    .push((client, conv_id, session_id, session_name));
            }
        }
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
        // Bot conversations touching the exited session fail too. A
        // client that asked or told the session is told loudly through
        // its inbox; a client the session asked keeps its inbox event
        // but the conversation is over (late answers close).
        let mut bot_notify = Vec::new();
        for (conv_id, conv) in self.bot_convs.iter_mut() {
            if conv.state != BotConvState::Open || conv.session != id {
                continue;
            }
            if conv.from_client {
                bot_notify.push((
                    conv.client.clone(),
                    conv_id.clone(),
                    conv.session_name.clone(),
                ));
            } else {
                conv.state = BotConvState::Failed;
            }
        }
        for (client, conv_id, session_name) in bot_notify {
            // Deposit first: only a recorded failure closes the
            // conversation. A full inbox leaves it Open and parks the
            // session for retry on later sweeps (see tick).
            let deposited = match self.clients.get_mut(&client) {
                Some(c) => c
                    .deposit(
                        BotKind::Failed,
                        &conv_id,
                        &id.to_string(),
                        &session_name,
                        "target exited",
                        crate::bot::now_unix_ms(),
                    )
                    .is_ok(),
                // No inbox exists (revocation fails convs itself, so
                // this is belt-and-braces): nothing to retry toward.
                None => true,
            };
            if deposited {
                if let Some(conv) = self.bot_convs.get_mut(&conv_id) {
                    conv.state = BotConvState::Failed;
                }
            } else {
                self.dead_sessions.insert(id);
            }
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
                || conv.target_updated
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
        // Fire due self-injection timers into their queues, earliest
        // due first: hash order is not an ordering, and a stalled
        // loop can owe several deadlines at once.
        let mut fired = Vec::new();
        for (timer_id, timer) in self.timers.iter() {
            if now >= timer.due {
                fired.push((timer.due, timer_id.clone()));
            }
        }
        fired.sort();
        let fired: Vec<String> = fired.into_iter().map(|(_, id)| id).collect();
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
        // Bot courtesy reminders follow the same rule; the target side
        // picks the delivery path (inbox event for clients, pane write
        // for sessions).
        let mut bot_due = Vec::new();
        for (conv_id, conv) in self.bot_convs.iter_mut() {
            if conv.kind != BotConvKind::Tell
                || conv.state != BotConvState::Open
                || !conv.acked
                || conv.reminded
                || conv.target_updated
                // A dead session's queue is gone: reminding it only
                // recreates a queue nobody drains (its failure notice
                // is already parked for retry).
                || self.dead_sessions.contains(&conv.session)
            {
                continue;
            }
            if now.duration_since(conv.last_update) >= COURTESY_GRACE {
                let source = if conv.from_client {
                    conv.client.clone()
                } else {
                    conv.session_name.clone()
                };
                bot_due.push((
                    conv.session,
                    conv.client.clone(),
                    conv_id.clone(),
                    source,
                    conv.from_client,
                ));
            }
        }
        for (session, client, conv_id, source, to_session) in bot_due {
            if to_session {
                // Session queues are unbounded: the push cannot fail.
                self.push(
                    session,
                    Injection {
                        conv: conv_id.clone(),
                        kind: InjectKind::Reminder,
                        from: source,
                        text: "no update since your ack; the source is still waiting".to_string(),
                    },
                );
                if let Some(conv) = self.bot_convs.get_mut(&conv_id) {
                    conv.reminded = true;
                }
            } else {
                // The inbox is capped: only a deposited reminder counts
                // as sent. A full inbox leaves the conversation
                // un-reminded and the next sweep retries.
                let deposited = self
                    .clients
                    .get_mut(&client)
                    .map(|c| {
                        c.deposit(
                            BotKind::Reminder,
                            &conv_id,
                            &session.to_string(),
                            &source,
                            "no update since your ack; the source is still waiting",
                            crate::bot::now_unix_ms(),
                        )
                        .is_ok()
                    })
                    .unwrap_or(false);
                if deposited {
                    if let Some(conv) = self.bot_convs.get_mut(&conv_id) {
                        conv.reminded = true;
                    }
                }
            }
        }
        // Retry exit-failure notices whose inbox was full: each success
        // closes its conversation, and ids with nothing Open drop out.
        let dead: Vec<SessionId> = self.dead_sessions.iter().copied().collect();
        for id in dead {
            let mut pending = Vec::new();
            for (conv_id, conv) in self.bot_convs.iter() {
                if conv.state == BotConvState::Open
                    && conv.session == id
                    && conv.from_client
                {
                    pending.push((
                        conv_id.clone(),
                        conv.client.clone(),
                        conv.session_name.clone(),
                    ));
                }
            }
            for (conv_id, client, session_name) in pending {
                let deposited = self
                    .clients
                    .get_mut(&client)
                    .map(|c| {
                        c.deposit(
                            BotKind::Failed,
                            &conv_id,
                            &id.to_string(),
                            &session_name,
                            "target exited",
                            crate::bot::now_unix_ms(),
                        )
                        .is_ok()
                    })
                    .unwrap_or(false);
                if deposited {
                    if let Some(conv) = self.bot_convs.get_mut(&conv_id) {
                        conv.state = BotConvState::Failed;
                    }
                }
            }
            let live = self
                .bot_convs
                .values()
                .any(|c| c.state == BotConvState::Open && c.session == id);
            if !live {
                self.dead_sessions.remove(&id);
                // Nothing legitimate queues to a dead session (sends
                // resolve live targets only), so anything here is
                // sweep debris for nobody: drop it with the entry.
                self.queue.remove(&id);
            }
        }
        // Retry stranded-answer notices parked at exit time: each
        // success closes an Open record (Done ones stay Done) and
        // drops its entry; a still-full inbox keeps its entry for
        // the next sweep. A vanished client drops its entries.
        let mut still = Vec::new();
        for (client, conv_id, session_id, session_name) in
            std::mem::take(&mut self.pending_failures)
        {
            let deposited = match self.clients.get_mut(&client) {
                Some(c) => c
                    .deposit(
                        BotKind::Failed,
                        &conv_id,
                        &session_id,
                        &session_name,
                        "target exited",
                        crate::bot::now_unix_ms(),
                    )
                    .is_ok(),
                None => true,
            };
            if deposited {
                if let Some(conv) = self.bot_convs.get_mut(&conv_id) {
                    if conv.state == BotConvState::Open {
                        conv.state = BotConvState::Failed;
                    }
                }
            } else {
                still.push((client, conv_id, session_id, session_name));
            }
        }
        self.pending_failures = still;
        // Evict terminal records past their TTL so the maps stay
        // bounded by live work, not history. A record survives while
        // a queued response or ack still names it: evicting under one
        // would strand the exit path's loud-failure lookup (None
        // reads as "no party to tell"). Other kinds never look the
        // record up after terminal state, so they pin nothing.
        // Quiescence pins harder: any queued work keeps an Open tell,
        // since its body may still deliver.
        let mut pinned = std::collections::HashSet::new();
        let mut referenced = std::collections::HashSet::new();
        for q in self.queue.values() {
            for inj in q {
                referenced.insert(inj.conv.clone());
                if matches!(
                    inj.kind,
                    InjectKind::Response | InjectKind::Ack
                ) {
                    pinned.insert(inj.conv.clone());
                }
            }
        }
        self.convs.retain(|id, conv| {
            if conv.state == ConvState::Open {
                // Only tells go quiet: an ask awaits its answer,
                // which can land at any time. A tell silent for a day
                // is a dead thread (resume with a fresh tell).
                if conv.kind == ConvKind::Tell
                    && now.duration_since(conv.last_update) >= QUIESCE_TTL
                    && !referenced.contains(id)
                {
                    return false;
                }
                return true;
            }
            now.duration_since(conv.last_update) < CONV_TTL || pinned.contains(id)
        });
        // Terminal count cap, oldest first, even within TTL. Open
        // work never counts.
        let over = self
            .convs
            .values()
            .filter(|c| c.state != ConvState::Open)
            .count()
            .saturating_sub(CONV_CAP);
        if over > 0 {
            let mut oldest: Vec<(String, Instant)> = self
                .convs
                .iter()
                .filter(|(_, c)| c.state != ConvState::Open)
                .map(|(id, c)| (id.clone(), c.last_update))
                .collect();
            oldest.sort_by_key(|(_, at)| *at);
            for (id, _) in oldest.into_iter().take(over) {
                self.convs.remove(&id);
            }
        }
        self.bot_convs.retain(|id, conv| {
            if conv.state == BotConvState::Open {
                if conv.kind == BotConvKind::Tell
                    && now.duration_since(conv.last_update) >= QUIESCE_TTL
                    && !referenced.contains(id)
                {
                    return false;
                }
                return true;
            }
            now.duration_since(conv.last_update) < CONV_TTL
        });
        let bot_over = self
            .bot_convs
            .values()
            .filter(|c| c.state != BotConvState::Open)
            .count()
            .saturating_sub(CONV_CAP);
        if bot_over > 0 {
            let mut oldest: Vec<(String, Instant)> = self
                .bot_convs
                .iter()
                .filter(|(_, c)| c.state != BotConvState::Open)
                .map(|(id, c)| (id.clone(), c.last_update))
                .collect();
            oldest.sort_by_key(|(_, at)| *at);
            for (id, _) in oldest.into_iter().take(bot_over) {
                self.bot_convs.remove(&id);
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

        fn register_bot(&mut self, name: &str, groups: Vec<&str>) {
            let groups = groups.into_iter().map(str::to_string).collect();
            self.state
                .broker
                .register_client(&self.state.manager, name, groups, BOT_TOKEN, Vec::new())
                .expect("registration validates");
        }

        fn bcall(
            &mut self,
            name: &str,
            tool: &str,
            args: &str,
        ) -> Result<String, crate::bot::BotError> {
            self.bcall_as(name, BOT_TOKEN, tool, args)
        }

        fn bcall_as(
            &mut self,
            name: &str,
            token: &str,
            tool: &str,
            args: &str,
        ) -> Result<String, crate::bot::BotError> {
            let now = std::time::Instant::now();
            self.state
                .broker
                .bot_call(&self.state.manager, name, token, tool, args, now)
        }
    }

    const BOT_TOKEN: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn bot_registration_validates_and_revocation_bites_at_once() {
        use crate::bot::ErrorCode;
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        assert!(p
            .state
            .broker
            .register_client(&p.state.manager, "skippy", vec!["peers".into()], BOT_TOKEN, vec![])
            .is_err(), "duplicate registration conflicts");
        assert!(p
            .state
            .broker
            .register_client(&p.state.manager, "a", vec!["peers".into()], BOT_TOKEN, vec![])
            .is_err(), "live session name collision conflicts");
        assert!(p
            .state
            .broker
            .register_client(&p.state.manager, "tiny", vec!["peers".into()], "short", vec![])
            .is_err(), "short credential refused");
        assert!(p
            .state
            .broker
            .register_client(&p.state.manager, "nogroups", vec![], BOT_TOKEN, vec![])
            .is_err(), "group grant required");
        // Unknown names and wrong credentials share one answer: no oracle.
        assert_eq!(
            p.bcall("ghost", "list_sessions", "{}").unwrap_err().code,
            ErrorCode::Unauthorized
        );
        assert_eq!(
            p.bcall_as("skippy", &"0".repeat(32), "list_sessions", "{}")
                .unwrap_err()
                .code,
            ErrorCode::Unauthorized
        );
        assert!(p.state.broker.revoke_client("skippy"));
        assert!(!p.state.broker.revoke_client("skippy"));
        assert_eq!(
            p.bcall("skippy", "list_sessions", "{}").unwrap_err().code,
            ErrorCode::Unauthorized
        );
    }

    #[test]
    fn bot_list_is_scoped_to_shared_groups() {
        let mut p = live_pair();
        p.state.broker.join(&p.state.manager, p.a, "peers").unwrap();
        p.state.broker.join(&p.state.manager, p.b, "elsewhere").unwrap();
        p.register_bot("skippy", vec!["peers"]);
        let list = p.bcall("skippy", "list_sessions", "{}").expect("scoped list validates");
        assert!(list.contains(r#""you":"skippy""#), "list: {list}");
        assert!(list.contains("\"epoch\":"), "list: {list}");
        assert!(list.contains(r#""name":"a""#), "list: {list}");
        assert!(!list.contains(r#""name":"b""#), "list: {list}");
        assert!(list.contains(r#""id":"s"#), "list: {list}");
    }

    #[test]
    fn response_after_group_leave_is_refused() {
        // Leaving the shared group revokes the answer channel: the
        // response must fail closed, not ride the old conversation ID.
        let mut p = live_pair().grouped();
        let ask = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"ready?"}"#)
            .expect("ask validates");
        let conv = json_field(&ask, "conversation").expect("conversation id");
        assert!(p.state.broker.leave(p.b, "peers"));
        let err = p
            .call(
                &p.run_b.clone(),
                "send_response",
                &format!(r#"{{"conversation_id":"{conv}","message":"yes"}}"#),
            )
            .expect_err("leave revokes answers");
        assert!(err.contains("no shared group"), "err: {err}");
    }

    #[test]
    fn ack_after_group_leave_is_refused() {
        let mut p = live_pair().grouped();
        let tell = p
            .call(&p.run_a.clone(), "tell_session", r#"{"target":"b","text":"hi"}"#)
            .expect("tell validates");
        let conv = json_field(&tell, "conversation").expect("conversation id");
        assert!(p.state.broker.leave(p.b, "peers"));
        let err = p
            .call(
                &p.run_b.clone(),
                "ack_message",
                &format!(r#"{{"conversation_id":"{conv}"}}"#),
            )
            .expect_err("leave revokes acks");
        assert!(err.contains("no shared group"), "err: {err}");
    }

    #[test]
    fn client_response_after_session_leaves_group_is_refused() {
        use crate::bot::ErrorCode;
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .call(
                &p.run_a.clone(),
                "ask_session",
                r#"{"target":"skippy","message":"are you there?"}"#,
            )
            .expect("session asks client");
        let conv = json_field(&res, "conversation").expect("conversation id");
        assert!(p.state.broker.leave(p.a, "peers"));
        let err = p
            .bcall(
                "skippy",
                "send_response",
                &format!(r#"{{"conversation_id":"{conv}","message":"yes"}}"#),
            )
            .expect_err("leave revokes client answers");
        assert_eq!(err.code, ErrorCode::NoSharedGroup);
    }

    #[test]
    fn session_response_to_client_ask_after_leave_is_refused() {
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .bcall("skippy", "ask_session", r#"{"target":"b","message":"ready?"}"#)
            .expect("client asks");
        let conv = json_field(&res, "conversation").expect("conversation id");
        assert!(p.state.broker.leave(p.b, "peers"));
        let err = p
            .call(
                &p.run_b.clone(),
                "send_response",
                &format!(r#"{{"conversation_id":"{conv}","message":"yes"}}"#),
            )
            .expect_err("leave revokes session answers to clients");
        assert!(err.contains("no shared group"), "err: {err}");
    }

    #[test]
    fn session_ack_to_client_tell_after_leave_is_refused() {
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .bcall("skippy", "tell_session", r#"{"target":"b","text":"hi"}"#)
            .expect("client tells");
        let conv = json_field(&res, "conversation").expect("conversation id");
        assert!(p.state.broker.leave(p.b, "peers"));
        let err = p
            .call(
                &p.run_b.clone(),
                "ack_message",
                &format!(r#"{{"conversation_id":"{conv}"}}"#),
            )
            .expect_err("leave revokes session acks to clients");
        assert!(err.contains("no shared group"), "err: {err}");
    }

    #[test]
    fn client_ack_after_session_leaves_group_is_refused() {
        use crate::bot::ErrorCode;
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .call(
                &p.run_a.clone(),
                "tell_session",
                r#"{"target":"skippy","text":"hi"}"#,
            )
            .expect("session tells client");
        let conv = json_field(&res, "conversation").expect("conversation id");
        assert!(p.state.broker.leave(p.a, "peers"));
        let err = p
            .bcall(
                "skippy",
                "ack_message",
                &format!(r#"{{"conversation_id":"{conv}"}}"#),
            )
            .expect_err("leave revokes client acks");
        assert_eq!(err.code, ErrorCode::NoSharedGroup);
    }

    #[test]
    fn bot_ask_response_roundtrip_never_touches_a_pane() {
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .bcall("skippy", "ask_session", r#"{"target":"b","message":"ready?"}"#)
            .expect("ask validates");
        let conv = json_field(&res, "conversation").expect("conversation id");
        assert!(res.contains("\"epoch\""), "res: {res}");
        // The question reaches the session pane, never the client inbox.
        assert_eq!(p.state.broker.take_due(p.b, 10).len(), 1);
        assert!(p
            .bcall("skippy", "bot_poll", "{}")
            .unwrap()
            .contains("\"events\":[]"));
        // The session answers through its own tool; the answer lands in
        // the inbox and nowhere else.
        p.call(
            &p.run_b.clone(),
            "send_response",
            &format!(r#"{{"conversation_id":"{conv}","message":"yes"}}"#),
        )
        .expect("target answers");
        assert!(p.state.broker.take_due(p.a, 10).is_empty());
        assert!(p.state.broker.take_due(p.b, 10).is_empty());
        let poll = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
        assert!(poll.contains(&conv), "poll: {poll}");
        assert!(poll.contains(r#""kind":"response""#), "poll: {poll}");
        let epoch = crate::mcp::top_raw(&poll, "epoch").expect("epoch echoed");
        p.bcall(
            "skippy",
            "bot_ack",
            &format!(r#"{{"cursor":1,"epoch":{epoch}}}"#),
        )
        .expect("ack validates");
        assert!(p
            .bcall("skippy", "bot_poll", "{}")
            .unwrap()
            .contains("\"events\":[]"));
    }

    #[test]
    fn session_ask_client_roundtrip_flows_through_the_inbox() {
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"skippy","message":"are you there?"}"#)
            .expect("session asks client");
        let conv = json_field(&res, "conversation").expect("conversation id");
        // No pane anywhere holds the question.
        assert!(p.state.broker.take_due(p.a, 10).is_empty());
        assert!(p.state.broker.take_due(p.b, 10).is_empty());
        let poll = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
        assert!(poll.contains(&conv), "poll: {poll}");
        assert!(poll.contains(r#""kind":"ask""#), "poll: {poll}");
        let epoch = crate::mcp::top_raw(&poll, "epoch").expect("epoch echoed");
        p.bcall(
            "skippy",
            "bot_ack",
            &format!(r#"{{"cursor":1,"epoch":{epoch}}}"#),
        )
        .unwrap();
        p.bcall(
            "skippy",
            "send_response",
            &format!(r#"{{"conversation_id":"{conv}","message":"yes"}}"#),
        )
        .expect("client answers");
        let due = p.state.broker.take_due(p.a, 10);
        assert_eq!(due.len(), 1);
        assert!(matches!(due[0].kind, InjectKind::Response));
    }

    #[test]
    fn bot_tell_ack_roundtrip_confirms_receipt_not_work() {
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .bcall("skippy", "tell_session", r#"{"target":"b","message":"deploy at dawn"}"#)
            .expect("tell validates");
        let conv = json_field(&res, "conversation").expect("conversation id");
        let due = p.state.broker.take_due(p.b, 10);
        assert_eq!(due.len(), 1);
        assert!(matches!(due[0].kind, InjectKind::Tell));
        p.call(
            &p.run_b.clone(),
            "ack_message",
            &format!(r#"{{"conversation_id":"{conv}"}}"#),
        )
        .expect("target acks");
        let poll = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
        assert!(poll.contains(&conv), "poll: {poll}");
        assert!(poll.contains(r#""kind":"ack""#), "poll: {poll}");
    }

    #[test]
    fn stale_cursor_without_epoch_conflicts() {
        // After a restart event IDs begin at 1 again, so a resumed
        // nonzero cursor without the epoch would echo itself as
        // next_cursor forever while new events 1..N go unseen.
        use crate::bot::ErrorCode;
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let err = p
            .bcall("skippy", "bot_poll", r#"{"cursor":500}"#)
            .expect_err("nonzero cursor needs the epoch");
        assert_eq!(err.code, ErrorCode::Conflict);
    }

    #[test]
    fn cursor_ahead_of_produced_events_conflicts() {
        // A cursor past everything produced is a stale pre-restart
        // cursor with a fresh epoch (or a client bug): echoing it
        // back as next_cursor would skip the whole inbox.
        use crate::bot::ErrorCode;
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let first = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
        let epoch = crate::mcp::top_raw(&first, "epoch").expect("epoch echoed");
        let err = p
            .bcall(
                "skippy",
                "bot_poll",
                &format!(r#"{{"cursor":500,"epoch":{epoch}}}"#),
            )
            .expect_err("ahead cursor conflicts");
        assert_eq!(err.code, ErrorCode::Conflict);
        // An up-to-date cursor still polls fine.
        assert!(p
            .bcall("skippy", "bot_poll", &format!(r#"{{"epoch":{epoch}}}"#))
            .expect("current poll validates")
            .contains("\"events\":[]"));
    }

    #[test]
    fn bot_retry_with_same_key_replays_and_conflicts() {
        use crate::bot::ErrorCode;
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let args = r#"{"target":"b","message":"ready?","idempotency_key":"k-1"}"#;
        let first = p.bcall("skippy", "ask_session", args).expect("ask validates");
        let again = p.bcall("skippy", "ask_session", args).expect("retry replays");
        assert_eq!(first, again);
        assert_eq!(p.state.broker.take_due(p.b, 10).len(), 1, "asked once");
        let err = p
            .bcall("skippy", "ask_session", r#"{"target":"b","message":"other","idempotency_key":"k-1"}"#)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict);
        let bad = p
            .bcall("skippy", "ask_session", r#"{"target":"b","message":"x","idempotency_key":"has space"}"#)
            .unwrap_err();
        assert_eq!(bad.code, ErrorCode::InvalidArguments);
    }

    #[test]
    fn bot_malformed_arguments_are_invalid_not_conflicts() {
        use crate::bot::ErrorCode;
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        for (tool, args) in [
            ("ask_session", r#"{"message":"x"}"#),
            ("ask_session", r#"{"target":"b"}"#),
            ("tell_session", r#"{"target":"b"}"#),
            ("send_response", r#"{"message":"x"}"#),
            ("bot_ack", "{}"),
            ("bot_poll", r#"{"epoch":"yesterday"}"#),
            ("bot_poll", r#"{"cursor":"soon"}"#),
            ("bot_poll", r#"{"limit":"plenty"}"#),
        ] {
            assert_eq!(
                p.bcall("skippy", tool, args).unwrap_err().code,
                ErrorCode::InvalidArguments,
                "{tool} {args}"
            );
        }
    }

    #[test]
    fn bot_answers_to_unknown_and_closed_conversations_fail_typed() {
        use crate::bot::ErrorCode;
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        assert_eq!(
            p.bcall("skippy", "send_response", r#"{"conversation_id":"nope","message":"x"}"#)
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        let res = p
            .bcall("skippy", "ask_session", r#"{"target":"b","message":"q"}"#)
            .unwrap();
        let conv = json_field(&res, "conversation").unwrap();
        p.state.broker.take_due(p.b, 10);
        p.call(
            &p.run_b.clone(),
            "send_response",
            &format!(r#"{{"conversation_id":"{conv}","message":"a"}}"#),
        )
        .unwrap();
        assert_eq!(
            p.bcall(
                "skippy",
                "send_response",
                &format!(r#"{{"conversation_id":"{conv}","message":"late"}}"#),
            )
            .unwrap_err()
            .code,
            ErrorCode::ConversationClosed
        );
    }

    #[test]
    fn bot_sends_fail_typed_on_routing_and_pressure() {
        use crate::bot::ErrorCode;
        // Ambiguous names fail rather than guess.
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let run_c = crate::ids::RunId::generate();
        let c = p
            .state
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_c, "shell")
            .unwrap();
        p.state.broker.join(&p.state.manager, c, "peers").unwrap();
        assert_eq!(
            p.bcall("skippy", "ask_session", r#"{"target":"b","message":"q"}"#)
                .unwrap_err()
                .code,
            ErrorCode::AmbiguousTarget
        );
        assert!(p.state.manager.remove(c));
        // No shared group blocks transfer.
        let mut q = live_pair();
        q.register_bot("skippy", vec!["peers"]);
        assert_eq!(
            q.bcall("skippy", "ask_session", r#"{"target":"b","message":"q"}"#)
                .unwrap_err()
                .code,
            ErrorCode::NoSharedGroup
        );
        // A full target queue rejects with pressure, not silence.
        let mut r = live_pair().grouped();
        r.register_bot("skippy", vec!["peers"]);
        for i in 0..5 {
            r.call(
                &r.run_a.clone(),
                "ask_session",
                &format!(r#"{{"target":"b","message":"q{i}"}}"#),
            )
            .expect("fills pressure");
        }
        assert_eq!(
            r.bcall("skippy", "ask_session", r#"{"target":"b","message":"q"}"#)
                .unwrap_err()
                .code,
            ErrorCode::PressureLimit
        );
    }

    #[test]
    fn dropped_response_on_source_exit_notifies_responder() {
        // B answers, gets success, and A's queue still holds the
        // response when A exits: deleting it silently would leave B
        // believing it was read. B must hear the loss loudly.
        let mut p = live_pair().grouped();
        let ask = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"q"}"#)
            .expect("ask validates");
        let conv = json_field(&ask, "conversation").expect("conversation id");
        assert_eq!(p.state.broker.take_due(p.b, 10).len(), 1, "b reads the ask");
        p.call(
            &p.run_b.clone(),
            "send_response",
            &format!(r#"{{"conversation_id":"{conv}","message":"yes"}}"#),
        )
        .expect("target answers");
        assert_eq!(p.state.broker.queued(p.a), 1);
        p.state.broker.target_exited(&p.state.manager, p.a);
        assert_eq!(p.state.broker.queued(p.a), 0, "exited queue is gone");
        let due = p.state.broker.take_due(p.b, 10);
        assert_eq!(due.len(), 1, "responder hears the loss");
        assert!(matches!(due[0].kind, InjectKind::Failed));
        assert_eq!(due[0].conv, conv);
    }

    #[test]
    fn dropped_ack_on_source_exit_notifies_acker() {
        // Same window for acks: the teller exits before reading the
        // ack, so the acker must hear it instead of assuming receipt.
        let mut p = live_pair().grouped();
        let tell = p
            .call(&p.run_a.clone(), "tell_session", r#"{"target":"b","text":"hi"}"#)
            .expect("tell validates");
        let conv = json_field(&tell, "conversation").expect("conversation id");
        assert_eq!(p.state.broker.take_due(p.b, 10).len(), 1, "b reads the tell");
        p.call(
            &p.run_b.clone(),
            "ack_message",
            &format!(r#"{{"conversation_id":"{conv}"}}"#),
        )
        .expect("target acks");
        assert_eq!(p.state.broker.queued(p.a), 1);
        p.state.broker.target_exited(&p.state.manager, p.a);
        let due = p.state.broker.take_due(p.b, 10);
        assert_eq!(due.len(), 1, "acker hears the loss");
        assert!(matches!(due[0].kind, InjectKind::Failed));
        assert_eq!(due[0].conv, conv);
    }

    #[test]
    fn bot_target_exit_arrives_as_a_failed_event() {
        use crate::bot::ErrorCode;
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .bcall("skippy", "ask_session", r#"{"target":"b","message":"q"}"#)
            .expect("client asks session");
        let conv = json_field(&res, "conversation").expect("conversation id");
        p.state.broker.target_exited(&p.state.manager, p.b);
        let poll = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
        assert!(poll.contains(&conv), "poll: {poll}");
        assert!(poll.contains(r#""kind":"failed""#), "poll: {poll}");
        // Late answers to the failed conversation close deterministically.
        assert_eq!(
            p.bcall(
                "skippy",
                "send_response",
                &format!(r#"{{"conversation_id":"{conv}","message":"late"}}"#),
            )
            .unwrap_err()
            .code,
            ErrorCode::ConversationClosed
        );
    }

    #[test]
    fn bot_poll_with_a_stale_epoch_conflicts() {
        use crate::bot::ErrorCode;
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let epoch = p.state.broker.epoch();
        p.bcall("skippy", "bot_poll", "{}").expect("current epoch polls");
        let stale = epoch.wrapping_add(1);
        let err = p
            .bcall("skippy", "bot_poll", &format!(r#"{{"epoch":{stale}}}"#))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict);
    }

    #[test]
    fn bot_control_tools_are_denied_and_unknown_is_not_found() {
        use crate::bot::ErrorCode;
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        assert_eq!(
            p.bcall("skippy", "start_session", r#"{"harness":"codex"}"#)
                .unwrap_err()
                .code,
            ErrorCode::Unauthorized
        );
        assert_eq!(
            p.bcall("skippy", "frobnicate", "{}").unwrap_err().code,
            ErrorCode::NotFound
        );
    }

    #[test]
    fn bot_file_registration_authenticates_through_the_file() {
        let mut p = live_pair().grouped();
        let dir = std::env::temp_dir().join(format!("forge-bot-reg-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("skippy.token");
        crate::bot::write_test_token(&path, BOT_TOKEN);
        p.state
            .broker
            .register_client_file(
                &p.state.manager,
                "skippy",
                vec!["peers".to_string()],
                Vec::new(),
                path.clone(),
            )
            .expect("file registration validates");
        // The bound file supplies the credential on every call.
        let list = p.bcall("skippy", "list_sessions", "{}").expect("file credential works");
        assert!(list.contains(r#""you":"skippy""#), "list: {list}");
        // Deleting the file revokes immediately.
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            p.bcall("skippy", "list_sessions", "{}").unwrap_err().code,
            crate::bot::ErrorCode::Unauthorized
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_list_shows_shared_bots_marked() {
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        p.register_bot("spy", vec!["elsewhere"]);
        let list = p
            .call(&p.run_a.clone(), "list_sessions", "{}")
            .expect("list validates");
        assert!(list.contains(r#""name":"skippy""#), "list: {list}");
        assert!(list.contains(r#""bot":true"#), "list: {list}");
        assert!(!list.contains("spy"), "list: {list}");
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
    fn armed_timers_count_against_send_pressure() {
        // Five armed timers saturate the target: the sixth unit of work
        // (a new ask) must wait, or timers plus sends stack past the cap.
        let mut p = live_pair().grouped();
        for i in 0..PRESSURE_CAP {
            p.call(
                &p.run_a.clone(),
                "schedule_prompt",
                &format!(r#"{{"prompt":"timer-{i}","delay_seconds":3600}}"#),
            )
            .expect("timers arm");
        }
        let err = p
            .call(&p.run_b.clone(), "ask_session", r#"{"target":"a","message":"q"}"#)
            .expect_err("timers hold the pressure budget");
        assert!(err.contains("pressure cap"), "err: {err}");
    }

    #[test]
    fn delivered_asks_count_against_scheduling() {
        // Scheduling reads the same budget as sending: five delivered
        // asks awaiting answers leave no room for a new timer.
        let mut p = live_pair().grouped();
        for i in 0..PRESSURE_CAP {
            p.call(
                &p.run_b.clone(),
                "ask_session",
                &format!(r#"{{"target":"a","message":"q{i}"}}"#),
            )
            .expect("asks queue");
        }
        assert_eq!(p.state.broker.take_due(p.a, 10).len(), PRESSURE_CAP);
        let err = p
            .call(
                &p.run_a.clone(),
                "schedule_prompt",
                r#"{"prompt":"later","delay_seconds":3600}"#,
            )
            .expect_err("asks hold the pressure budget");
        assert!(err.contains("pressure cap"), "err: {err}");
    }

    #[test]
    fn staged_enter_beat_is_300ms() {
        // The body/Enter split only works when the CR trails by enough
        // for a prompt redraw to settle; pin the tuned value.
        assert_eq!(INJECT_ENTER_DELAY, Duration::from_millis(300));
    }

    #[test]
    fn hook_debounce_is_500ms() {
        // Injections hold this long after hook activity so a body never
        // races a mid-tool-use verdict; pin the tuned value.
        assert_eq!(INJECT_HOOK_DEBOUNCE, Duration::from_millis(500));
    }

    #[test]
    fn framed_render_wraps_strips_and_passes_through() {
        let tell = Injection {
            conv: "conv-9".to_string(),
            kind: InjectKind::Tell,
            from: "a".to_string(),
            text: "fyi".to_string(),
        };
        // Unbracketed panes take the raw body, byte for byte.
        assert_eq!(tell.render_framed(false), tell.render_body());
        // Bracketed panes take one paste transaction around the body.
        let framed = tell.render_framed(true);
        assert!(framed.starts_with(b"\x1b[200~"), "opens paste");
        assert!(framed.ends_with(b"\x1b[201~"), "closes paste");
        assert_eq!(
            &framed[6..framed.len() - 6],
            tell.render_body().as_slice(),
            "payload intact inside"
        );
        // A hostile payload cannot break out early: embedded terminators
        // are stripped, leaving exactly one — the real closer.
        let hostile = Injection {
            conv: "conv-9".to_string(),
            kind: InjectKind::Tell,
            from: "a".to_string(),
            text: "part1\x1b[201~; rm -rf ~".to_string(),
        };
        let bytes = hostile.render_framed(true);
        assert!(bytes.starts_with(b"\x1b[200~"));
        assert!(bytes.ends_with(b"\x1b[201~"));
        let inner = &bytes[6..bytes.len() - 6];
        assert!(
            !inner.windows(6).any(|w| w == b"\x1b[201~"),
            "no inner terminator: {inner:?}"
        );
        assert!(inner.windows(8).any(|w| w == b"; rm -rf"), "tail kept, only the break removed");
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

    /// Fill a client's inbox to the cap with padding events. Deposit
    /// fails exactly when full, so the loop needs no inbox access.
    fn fill_inbox(p: &mut Pair, client: &str) {
        let c = p
            .state
            .broker
            .clients
            .get_mut(client)
            .expect("client registered");
        let mut n = 0;
        while c
            .deposit(crate::bot::BotKind::Tell, "pad", "pad", "pad", "pad", 0)
            .is_ok()
        {
            n += 1;
        }
        assert!(n > 0, "inbox filled to the cap");
    }

    /// Poll-plus-ack the whole backlog: polls advance the received
    /// cursor 20 at a time, acks drain what was received. Thirteen
    /// steps reach exactly 256 (the cap).
    fn drain_inbox(p: &mut Pair, client: &str) {
        let first = p.bcall(client, "bot_poll", "{}").expect("poll validates");
        let epoch = crate::mcp::top_raw(&first, "epoch").expect("epoch echoed");
        for step in 1..=13 {
            let cursor = (step * 20).min(crate::bot::INBOX_CAP as u64);
            p.bcall(client, "bot_poll", "{}").expect("poll validates");
            p.bcall(
                client,
                "bot_ack",
                &format!(r#"{{"cursor":{cursor},"epoch":{epoch}}}"#),
            )
            .expect("ack validates");
        }
    }

    #[test]
    fn bot_courtesy_reminder_retries_a_full_inbox() {
        // A reminder that cannot be deposited must not count as sent:
        // the conversation stays un-reminded and the next sweep retries
        // once the client makes room. Session-originated tell, so the
        // reminder travels client-ward into the (possibly full) inbox.
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .call(
                &p.run_b.clone(),
                "tell_session",
                r#"{"target":"skippy","text":"hi"}"#,
            )
            .expect("session tells client");
        let conv = json_field(&res, "conversation").expect("conversation id");
        p.bcall(
            "skippy",
            "ack_message",
            &format!(r#"{{"conversation_id":"{conv}"}}"#),
        )
        .expect("client acks");
        assert_eq!(p.state.broker.take_due(p.b, 10).len(), 1, "b reads the ack");
        fill_inbox(&mut p, "skippy");
        // Past grace with nowhere to put the reminder: still pending.
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(31));
        assert!(
            !p.state.broker.bot_convs.get(&conv).expect("conv").reminded,
            "undelivered reminder stays un-reminded"
        );
        // Room opens: the next sweep delivers exactly one reminder.
        drain_inbox(&mut p, "skippy");
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        assert!(p.state.broker.bot_convs.get(&conv).expect("conv").reminded);
        let poll = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
        assert_eq!(poll.matches(r#""kind":"reminder""#).count(), 1, "poll: {poll}");
        assert!(poll.contains(&conv), "poll: {poll}");
    }

    #[test]
    fn exit_failure_notice_retries_a_full_inbox() {
        // Same ordering for exit notices: a full inbox must hold the
        // conversation Open (retry on later sweeps), never mark Failed
        // while the notice is lost.
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .bcall("skippy", "tell_session", r#"{"target":"b","text":"hi"}"#)
            .expect("client tells");
        let conv = json_field(&res, "conversation").expect("conversation id");
        fill_inbox(&mut p, "skippy");
        p.state.broker.target_exited(&p.state.manager, p.b);
        assert_eq!(
            p.state.broker.bot_convs.get(&conv).expect("conv").state,
            crate::bot::BotConvState::Open,
            "unsent failure notice holds the conv open"
        );
        // Room opens: the next sweep delivers the failure and closes.
        drain_inbox(&mut p, "skippy");
        p.state.broker.tick(std::time::Instant::now());
        assert_eq!(
            p.state.broker.bot_convs.get(&conv).expect("conv").state,
            crate::bot::BotConvState::Failed
        );
        let poll = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
        assert!(poll.contains(&conv), "poll: {poll}");
        assert_eq!(poll.matches(r#""kind":"failed""#).count(), 1, "poll: {poll}");
    }

    #[test]
    fn target_followup_suppresses_the_courtesy_reminder() {
        // The courtesy obligation ends when the owing party updates:
        // b acked and then sent the update itself, so no reminder may
        // fire — not now, not after more silence. The source's own
        // follow-ups merely restart grace (see the back-to-source test).
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
        p.call(
            &p.run_b.clone(),
            "tell_session",
            &format!(r#"{{"target":"a","message":"on it","conversation_id":"{conv}"}}"#),
        )
        .expect("target follows up");
        p.state.broker.take_due(p.a, 10);
        p.state.broker.take_due(p.b, 10);
        // Past grace and far past it: silence, forever.
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(31));
        assert!(p.state.broker.take_due(p.b, 10).is_empty());
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        assert!(p.state.broker.take_due(p.b, 10).is_empty());
    }

    #[test]
    fn client_followup_suppresses_the_courtesy_reminder() {
        // Same rule client-ward: the session told the client, the
        // client acked and followed up, so the inbox stays quiet.
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .call(
                &p.run_b.clone(),
                "tell_session",
                r#"{"target":"skippy","text":"fyi"}"#,
            )
            .expect("session tells client");
        let conv = json_field(&res, "conversation").expect("conversation id");
        p.bcall(
            "skippy",
            "ack_message",
            &format!(r#"{{"conversation_id":"{conv}"}}"#),
        )
        .expect("client acks");
        p.bcall(
            "skippy",
            "tell_session",
            &format!(r#"{{"target":"b","text":"on it","conversation_id":"{conv}"}}"#),
        )
        .expect("client follows up");
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        let poll = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
        assert!(!poll.contains(r#""kind":"reminder""#), "poll: {poll}");
    }

    #[test]
    fn dropped_bot_response_on_source_exit_notifies_client() {
        // The session asked the client, the client answered (got
        // completed:true), and the session exits with the response
        // still queued: the client must hear the loss loudly through
        // its inbox, not lose it silently.
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .call(
                &p.run_a.clone(),
                "ask_session",
                r#"{"target":"skippy","message":"are you there?"}"#,
            )
            .expect("session asks client");
        let conv = json_field(&res, "conversation").expect("conversation id");
        p.bcall(
            "skippy",
            "send_response",
            &format!(r#"{{"conversation_id":"{conv}","message":"yes"}}"#),
        )
        .expect("client answers");
        p.state.broker.target_exited(&p.state.manager, p.a);
        let poll = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
        assert!(poll.contains(&conv), "poll names the conv: {poll}");
        assert!(poll.contains(r#""kind":"failed""#), "failure lands: {poll}");
    }

    #[test]
    fn dropped_bot_ack_on_source_exit_notifies_client() {
        // The client acked a session tell (got acknowledged:true) and
        // the session exits with the Ack still queued: the client is
        // told, and the record closes instead of lingering Open.
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .call(
                &p.run_a.clone(),
                "tell_session",
                r#"{"target":"skippy","message":"hi"}"#,
            )
            .expect("session tells client");
        let conv = json_field(&res, "conversation").expect("conversation id");
        p.bcall("skippy", "ack_message", &format!(r#"{{"conversation_id":"{conv}"}}"#))
            .expect("client acks");
        p.state.broker.target_exited(&p.state.manager, p.a);
        let poll = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
        assert!(poll.contains(&conv), "poll names the conv: {poll}");
        assert!(poll.contains(r#""kind":"failed""#), "failure lands: {poll}");
    }

    #[test]
    fn typed_over_bot_ack_notifies_the_inbox() {
        // The client's ack reached A's prompt with its Enter staged;
        // A types first, so the submit dies and the client is told
        // through its inbox instead of assuming clean delivery.
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .call(
                &p.run_a.clone(),
                "tell_session",
                r#"{"target":"skippy","message":"hi"}"#,
            )
            .expect("session tells client");
        let conv = json_field(&res, "conversation").expect("conversation id");
        p.bcall(
            "skippy",
            "ack_message",
            &format!(r#"{{"conversation_id":"{conv}"}}"#),
        )
        .expect("client acks");
        p.state.settle_comms();
        assert!(p.state.pending_enter.contains_key(&p.a), "enter staged");
        p.state.note_human_input(p.a);
        let poll = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
        assert!(poll.contains(&conv), "poll names the conv: {poll}");
        assert!(poll.contains(r#""kind":"failed""#), "failure lands: {poll}");
    }

    #[test]
    fn courtesy_skips_dead_sessions() {
        // A bot tell was acked, the session exits with a full inbox
        // (failure retry pending): the courtesy sweep must not push
        // a Reminder to the removed queue — nobody will drain it.
        use crate::bot::{BotKind, INBOX_CAP};
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .bcall("skippy", "tell_session", r#"{"target":"b","message":"hi"}"#)
            .expect("tell validates");
        let conv = json_field(&res, "conversation").expect("conversation id");
        p.call(
            &p.run_b.clone(),
            "ack_message",
            &format!(r#"{{"conversation_id":"{conv}"}}"#),
        )
        .expect("session acks");
        {
            let client = p
                .state
                .broker
                .clients
                .get_mut("skippy")
                .expect("client registered");
            for n in 0..INBOX_CAP {
                let _ = client.deposit(BotKind::Tell, "pad", "s", "a", &n.to_string(), 1);
            }
        }
        p.state.broker.target_exited(&p.state.manager, p.b);
        assert!(p.state.broker.dead_sessions.contains(&p.b), "retry pending");
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        assert_eq!(
            p.state.broker.queued(p.b),
            0,
            "no reminder to a dead session"
        );
        assert!(
            p.state.broker.dead_sessions.contains(&p.b),
            "failure retry still pending"
        );
    }

    #[test]
    fn full_inbox_parks_bot_exit_notice_for_retry() {
        // The inbox is full when the session exits: the failure
        // notice parks instead of dropping, and the next sweep
        // delivers it once space frees.
        use crate::bot::{BotKind, INBOX_CAP};
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .call(
                &p.run_a.clone(),
                "ask_session",
                r#"{"target":"skippy","message":"are you there?"}"#,
            )
            .expect("session asks client");
        let conv = json_field(&res, "conversation").expect("conversation id");
        p.bcall(
            "skippy",
            "send_response",
            &format!(r#"{{"conversation_id":"{conv}","message":"yes"}}"#),
        )
        .expect("client answers");
        // Receive and ack the ask so the pads below start past it;
        // the epoch rides along for the later cursor polls and acks.
        let poll1 = p.bcall("skippy", "bot_poll", "{}").expect("poll receives");
        let epoch = crate::mcp::top_raw(&poll1, "epoch").expect("epoch echoed");
        p.bcall("skippy", "bot_ack", &format!(r#"{{"cursor":1,"epoch":{epoch}}}"#))
            .expect("ack advances");
        // Fill the inbox to the cap: the exit notice must park.
        {
            let client = p
                .state
                .broker
                .clients
                .get_mut("skippy")
                .expect("client registered");
            for n in 0..INBOX_CAP {
                let _ = client.deposit(
                    BotKind::Tell,
                    "pad",
                    "s",
                    "a",
                    &n.to_string(),
                    1,
                );
            }
        }
        p.state.broker.target_exited(&p.state.manager, p.a);
        assert_eq!(
            p.state.broker.pending_failures.len(),
            1,
            "full inbox parks the notice"
        );
        // Free one page: receive 2..21, ack through it.
        p.bcall("skippy", "bot_poll", "{}").expect("poll receives");
        p.bcall(
            "skippy",
            "bot_ack",
            &format!(r#"{{"cursor":21,"epoch":{epoch}}}"#),
        )
        .expect("ack frees space");
        p.state.broker.tick(std::time::Instant::now());
        assert!(
            p.state.broker.pending_failures.is_empty(),
            "retry delivers"
        );
        let poll = p
            .bcall(
                "skippy",
                "bot_poll",
                &format!(r#"{{"cursor":240,"epoch":{epoch}}}"#),
            )
            .expect("poll validates");
        assert!(poll.contains(&conv), "poll names the conv: {poll}");
        assert!(poll.contains(r#""kind":"failed""#), "failure lands: {poll}");
    }

    #[test]
    fn terminal_records_cap_oldest_first() {
        // Past CONV_CAP the sweep evicts the oldest terminal
        // records even within their TTL; Open work never counts.
        let mut p = live_pair().grouped();
        let now = std::time::Instant::now();
        let mut oldest = String::new();
        for i in 0..(crate::comms::CONV_CAP + 1) {
            let id = format!("cap-{i}");
            if i == 0 {
                oldest = id.clone();
            }
            p.state.broker.convs.insert(
                id,
                Conv {
                    kind: ConvKind::Ask,
                    source: p.a,
                    target: p.b,
                    source_name: "a".to_string(),
                    target_name: "b".to_string(),
                    state: ConvState::Done,
                    acked: false,
                    last_update: now - std::time::Duration::from_secs(1000 - (i as u64).min(999)),
                    reminded: false,
                    target_updated: false,
                    delivered: false,
                },
            );
        }
        p.state.broker.convs.insert(
            "live-open".to_string(),
            Conv {
                kind: ConvKind::Ask,
                source: p.a,
                target: p.b,
                source_name: "a".to_string(),
                target_name: "b".to_string(),
                state: ConvState::Open,
                acked: false,
                last_update: now - std::time::Duration::from_secs(5000),
                reminded: false,
                target_updated: false,
                delivered: false,
            },
        );
        p.state.broker.tick(now);
        let terminal = p
            .state
            .broker
            .convs
            .values()
            .filter(|c| c.state != ConvState::Open)
            .count();
        assert_eq!(terminal, crate::comms::CONV_CAP, "cap enforced");
        assert!(!p.state.broker.convs.contains_key(&oldest), "oldest evicted");
        assert!(p.state.broker.convs.contains_key("live-open"), "open spared");
    }

    #[test]
    fn quiet_tells_quiesce_past_ttl() {
        // An Open tell quiet for a day is a dead thread: evict it
        // (resume with a fresh tell). Open asks await answers and
        // never quiesce; queued work pins its record.
        let mut p = live_pair().grouped();
        let now = std::time::Instant::now();
        let old = now - crate::comms::QUIESCE_TTL - std::time::Duration::from_secs(60);
        let (a, b) = (p.a, p.b);
        let mk = move |kind: ConvKind| Conv {
            kind,
            source: a,
            target: b,
            source_name: "a".to_string(),
            target_name: "b".to_string(),
            state: ConvState::Open,
            acked: true,
            last_update: old,
            reminded: true,
            target_updated: false,
            delivered: false,
        };
        p.state.broker.convs.insert("quiet-tell".to_string(), mk(ConvKind::Tell));
        p.state.broker.convs.insert("quiet-ask".to_string(), mk(ConvKind::Ask));
        let mut fresh = mk(ConvKind::Tell);
        fresh.last_update = now;
        p.state.broker.convs.insert("fresh-tell".to_string(), fresh);
        p.state.broker.convs.insert("pinned-tell".to_string(), mk(ConvKind::Tell));
        p.state.broker.push(
            b,
            Injection {
                conv: "pinned-tell".to_string(),
                kind: InjectKind::Tell,
                from: "a".to_string(),
                text: "waiting".to_string(),
            },
        );
        p.state.broker.tick(now);
        assert!(!p.state.broker.convs.contains_key("quiet-tell"), "quiet tell evicted");
        assert!(p.state.broker.convs.contains_key("quiet-ask"), "open ask spared");
        assert!(p.state.broker.convs.contains_key("fresh-tell"), "fresh tell spared");
        assert!(p.state.broker.convs.contains_key("pinned-tell"), "queued tell pinned");
    }

    #[test]
    fn quiet_bot_tells_quiesce_past_ttl() {
        // Same quiescence rule, bot table: a day-quiet Open tell
        // evicts, an Open ask never does.
        use crate::bot::{BotConv, BotConvKind, BotConvState};
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let now = std::time::Instant::now();
        let old = now - crate::comms::QUIESCE_TTL - std::time::Duration::from_secs(60);
        let b = p.b;
        let mk = move |kind: BotConvKind| BotConv {
            kind,
            session: b,
            session_name: "b".to_string(),
            client: "skippy".to_string(),
            from_client: true,
            state: BotConvState::Open,
            acked: true,
            delivered: false,
            last_update: old,
            reminded: true,
            target_updated: false,
        };
        p.state.broker.bot_convs.insert("quiet-tell".to_string(), mk(BotConvKind::Tell));
        p.state.broker.bot_convs.insert("quiet-ask".to_string(), mk(BotConvKind::Ask));
        p.state.broker.tick(now);
        assert!(!p.state.broker.bot_convs.contains_key("quiet-tell"), "quiet tell evicted");
        assert!(p.state.broker.bot_convs.contains_key("quiet-ask"), "open ask spared");
    }

    #[test]
    fn caller_backlog_gates_new_sends() {
        // A answers nothing while asking on: each answer piles a
        // response behind busy A. Past the queue cap A's new sends
        // refuse with backpressure instead of growing the queue
        // without limit; draining unblocks. Completions themselves
        // still bypass (B keeps answering throughout).
        let mut p = live_pair().grouped();
        for _ in 0..crate::comms::QUEUE_CAP {
            let res = p
                .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"q"}"#)
                .expect("ask admitted");
            let conv = json_field(&res, "conversation").expect("conversation id");
            assert_eq!(p.state.broker.take_due(p.b, 10).len(), 1);
            p.call(
                &p.run_b.clone(),
                "send_response",
                &format!(r#"{{"conversation_id":"{conv}","message":"a"}}"#),
            )
            .expect("B answers");
        }
        assert_eq!(p.state.broker.queued(p.a), crate::comms::QUEUE_CAP);
        let err = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"one more"}"#)
            .expect_err("backlogged caller waits");
        assert!(err.contains("caller queue full"), "err: {err}");
        let err = p
            .call(&p.run_a.clone(), "tell_session", r#"{"target":"b","message":"one more"}"#)
            .expect_err("tells gate the same way");
        assert!(err.contains("caller queue full"), "err: {err}");
        // Draining unblocks: backpressure, not deadlock.
        assert_eq!(p.state.broker.take_due(p.a, 200).len(), crate::comms::QUEUE_CAP);
        p.call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"again"}"#)
            .expect("drained caller sends");
    }

    #[test]
    fn terminal_conversations_evict_past_ttl() {
        // Done and Failed records must not pile up forever: past the
        // TTL the sweep evicts them and late arrivals read as unknown
        // (still rejected, like a closed conversation).
        let mut p = live_pair().grouped();
        let res = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"q"}"#)
            .expect("ask validates");
        let conv = json_field(&res, "conversation").expect("conversation id");
        let answer = format!(r#"{{"conversation_id":"{conv}","message":"a"}}"#);
        p.call(&p.run_b.clone(), "send_response", &answer)
            .expect("target answers");
        let err = p
            .call(&p.run_b.clone(), "send_response", &answer)
            .expect_err("closed rejects dups");
        assert_eq!(err, "conversation is closed");
        // The Done record survives while its response sits queued...
        assert_eq!(p.state.broker.take_due(p.a, 10).len(), 1);
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        let err = p
            .call(&p.run_b.clone(), "send_response", &answer)
            .expect_err("evicted reads unknown");
        assert_eq!(err, "unknown conversation");
        // ...and a Failed record evicts the same way.
        let res = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"q2"}"#)
            .expect("ask validates");
        let conv2 = json_field(&res, "conversation").expect("conversation id");
        p.state.broker.target_exited(&p.state.manager, p.b);
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        let err = p
            .call(
                &p.run_b.clone(),
                "send_response",
                &format!(r#"{{"conversation_id":"{conv2}","message":"late"}}"#),
            )
            .expect_err("failed record evicted");
        assert_eq!(err, "unknown conversation");
    }

    #[test]
    fn queued_references_pin_terminal_conversations() {
        // Eviction must not strand a queued response: while the
        // injection still waits, the Done record stays so an exit
        // still notifies the responder loudly (Major 5's guarantee).
        let mut p = live_pair().grouped();
        let res = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"q"}"#)
            .expect("ask validates");
        let conv = json_field(&res, "conversation").expect("conversation id");
        let answer = format!(r#"{{"conversation_id":"{conv}","message":"a"}}"#);
        p.call(&p.run_b.clone(), "send_response", &answer)
            .expect("target answers");
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        let err = p
            .call(&p.run_b.clone(), "send_response", &answer)
            .expect_err("pinned record still names closed");
        assert_eq!(err, "conversation is closed");
        // Once the queue drains the pin releases and it evicts.
        assert_eq!(p.state.broker.take_due(p.a, 10).len(), 1);
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        let err = p
            .call(&p.run_b.clone(), "send_response", &answer)
            .expect_err("unpinned record evicts");
        assert_eq!(err, "unknown conversation");
    }

    #[test]
    fn terminal_bot_conversations_evict_past_ttl() {
        use crate::bot::ErrorCode;
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .bcall("skippy", "ask_session", r#"{"target":"b","message":"ready?"}"#)
            .expect("ask validates");
        let conv = json_field(&res, "conversation").expect("conversation id");
        p.call(
            &p.run_b.clone(),
            "send_response",
            &format!(r#"{{"conversation_id":"{conv}","message":"yes"}}"#),
        )
        .expect("target answers");
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        let err = p
            .bcall(
                "skippy",
                "send_response",
                &format!(r#"{{"conversation_id":"{conv}","message":"again"}}"#),
            )
            .expect_err("evicted bot conv reads unknown");
        assert_eq!(err.code, ErrorCode::NotFound);
    }

    #[test]
    fn repeat_ack_replays_without_dup() {
        // A retried acknowledgement (lost verdict, impatient
        // harness) returns success again but queues no second Ack
        // and stops touching the courtesy clock.
        let mut p = live_pair().grouped();
        let res = p
            .call(&p.run_a.clone(), "tell_session", r#"{"target":"b","message":"hi"}"#)
            .expect("tell validates");
        let conv = json_field(&res, "conversation").expect("conversation id");
        let ack = format!(r#"{{"conversation_id":"{conv}"}}"#);
        p.call(&p.run_b.clone(), "ack_message", &ack)
            .expect("ack validates");
        assert_eq!(p.state.broker.take_due(p.a, 10).len(), 1);
        let again = p
            .call(&p.run_b.clone(), "ack_message", &ack)
            .expect("retry still acknowledges");
        assert!(again.contains(r#""acknowledged":true"#), "again: {again}");
        assert_eq!(
            p.state.broker.take_due(p.a, 10).len(),
            0,
            "no duplicate Ack queued"
        );
    }

    #[test]
    fn bot_repeat_ack_replays_without_dup() {
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .call(
                &p.run_a.clone(),
                "tell_session",
                r#"{"target":"skippy","message":"hi"}"#,
            )
            .expect("session tells client");
        let conv = json_field(&res, "conversation").expect("conversation id");
        let ack = format!(r#"{{"conversation_id":"{conv}"}}"#);
        p.bcall("skippy", "ack_message", &ack)
            .expect("ack validates");
        assert_eq!(p.state.broker.take_due(p.a, 10).len(), 1);
        p.bcall("skippy", "ack_message", &ack)
            .expect("retry still acknowledges");
        assert_eq!(
            p.state.broker.take_due(p.a, 10).len(),
            0,
            "no duplicate Ack queued"
        );
    }

    #[test]
    fn overdue_timers_fire_earliest_first() {
        // Five armed out of order must still inject earliest-due
        // first: hash order is not an ordering.
        let mut p = live_pair().grouped();
        for (prompt, delay) in [("p1", 5), ("p2", 4), ("p3", 3), ("p4", 2), ("p5", 1)] {
            p.call(
                &p.run_a.clone(),
                "schedule_prompt",
                &format!(r#"{{"prompt":"{prompt}","delay_seconds":{delay}}}"#),
            )
            .expect("schedule validates");
        }
        p.state
            .broker
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(10));
        let due = p.state.broker.take_due(p.a, 10);
        let texts: Vec<&str> = due.iter().map(|inj| inj.text.as_str()).collect();
        assert_eq!(texts, vec!["p5", "p4", "p3", "p2", "p1"]);
    }

    #[test]
    fn epochless_nonzero_ack_conflicts() {
        // Like polls, a nonzero ack without the epoch may be a stale
        // pre-restart retry: accepting it would advance (and delete)
        // a fresh epoch's received prefix.
        use crate::bot::ErrorCode;
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        for n in 0..3 {
            p.call(
                &p.run_a.clone(),
                "tell_session",
                &format!(r#"{{"target":"skippy","message":"m{n}"}}"#),
            )
            .expect("tell validates");
        }
        p.bcall("skippy", "bot_poll", "{}").expect("poll receives");
        let err = p
            .bcall("skippy", "bot_ack", r#"{"cursor":2}"#)
            .expect_err("epochless nonzero ack conflicts");
        assert_eq!(err.code, ErrorCode::Conflict);
        // Nothing was deleted: the fresh prefix still polls.
        let poll = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
        assert!(poll.contains("m0"), "fresh events intact: {poll}");
    }

    #[test]
    fn bot_ack_cursor_retry_replays() {
        // The server committed the ack; a lost verdict retried with
        // the exact cursor replays instead of conflicting.
        let mut p = live_pair().grouped();
        p.register_bot("skippy", vec!["peers"]);
        let res = p
            .bcall("skippy", "ask_session", r#"{"target":"b","message":"ready?"}"#)
            .expect("ask validates");
        let conv = json_field(&res, "conversation").expect("conversation id");
        p.call(
            &p.run_b.clone(),
            "send_response",
            &format!(r#"{{"conversation_id":"{conv}","message":"yes"}}"#),
        )
        .expect("target answers");
        let poll = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
        let epoch = crate::mcp::top_raw(&poll, "epoch").expect("epoch echoed");
        let ack = format!(r#"{{"cursor":1,"epoch":{epoch}}}"#);
        let first = p.bcall("skippy", "bot_ack", &ack).expect("ack validates");
        assert!(first.contains(r#""acknowledged":1"#), "first: {first}");
        let retry = p
            .bcall("skippy", "bot_ack", &ack)
            .expect("exact retry replays");
        assert!(retry.contains(r#""acknowledged":1"#), "retry: {retry}");
    }

    #[test]
    fn idempotency_capacity_is_per_caller() {
        // One noisy session flooding past the cache cap must not
        // evict another caller's still-live retry record.
        use crate::bot::IDEM_MAX_KEYS;
        let mut p = live_pair().grouped();
        let args = r#"{"target":"a","message":"q","idempotency_key":"bk"}"#;
        let first = p
            .call(&p.run_b.clone(), "ask_session", args)
            .expect("ask validates");
        let now = std::time::Instant::now();
        for i in 0..(IDEM_MAX_KEYS + 10) {
            p.state
                .broker
                .session_idem
                .entry(p.a.to_string())
                .or_insert_with(crate::bot::IdemCache::new)
                .store(&format!("k-{i}"), i as u64, "x", now);
        }
        let replay = p
            .call(&p.run_b.clone(), "ask_session", args)
            .expect("retry validates");
        assert_eq!(replay, first, "B's record survives A's flood");
    }

    #[test]
    fn idempotency_records_die_with_their_caller() {
        let mut p = live_pair().grouped();
        let args = r#"{"target":"a","message":"q","idempotency_key":"bk"}"#;
        p.call(&p.run_b.clone(), "ask_session", args)
            .expect("ask validates");
        assert!(p.state.broker.session_idem.len() >= 1, "record exists");
        p.state.broker.target_exited(&p.state.manager, p.b);
        assert_eq!(
            p.state.broker.session_idem.len(),
            0,
            "dead callers keep no retry records"
        );
    }

    #[test]
    fn keyed_retry_replays_the_first_verdict() {
        // A harness that times out and retries with the same key must
        // get the original conversation back, not a duplicate. Keyless
        // calls still execute every time.
        let mut p = live_pair().grouped();
        let args = r#"{"target":"b","message":"q","idempotency_key":"k-1"}"#;
        let first = p.call(&p.run_a.clone(), "ask_session", args).expect("ask validates");
        let replay = p.call(&p.run_a.clone(), "ask_session", args).expect("retry validates");
        assert_eq!(replay, first, "retry replays instead of duplicating");
        let third = p
            .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"q"}"#)
            .expect("keyless ask validates");
        assert_ne!(third, first, "keyless calls still execute");
    }

    #[test]
    fn keyed_retry_with_changed_args_conflicts() {
        let mut p = live_pair().grouped();
        p.call(
            &p.run_a.clone(),
            "ask_session",
            r#"{"target":"b","message":"q","idempotency_key":"k-2"}"#,
        )
        .expect("ask validates");
        let err = p
            .call(
                &p.run_a.clone(),
                "ask_session",
                r#"{"target":"b","message":"CHANGED","idempotency_key":"k-2"}"#,
            )
            .expect_err("same key, different send conflicts");
        assert!(err.contains("different arguments"), "err: {err}");
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
    fn leave_all_drops_every_group() {
        let mut p = live_pair();
        p.state.broker.join(&p.state.manager, p.a, "peers").unwrap();
        p.state.broker.join(&p.state.manager, p.a, "other").unwrap();
        assert!(p.state.broker.leave_all(p.a));
        assert!(!p.state.broker.is_member(p.a, "peers"));
        assert!(!p.state.broker.is_member(p.a, "other"));
        assert_eq!(p.state.broker.primary_group(p.a), None);
        assert!(!p.state.broker.leave_all(p.a), "second leave is a no-op");
        assert!(!p.state.broker.leave_all(p.b), "never joined");
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
