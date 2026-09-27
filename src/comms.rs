//! Cross-session message broker (Phase 4b): groups, conversations, pressure.
//!
//! The broker owns addressing and authorization state; [`crate::session`]
//! owns liveness. Every call resolves the caller by current run ID, then an
//! exact live target, then a shared communication group. Asks return a
//! conversation ID at once and never block; answers arrive as injections.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use crate::comms::bot::{
    BotClient, BotConv, BotConvKind, BotConvState, BotError, BotKind, ErrorCode, IdemCache,
    IdemCheck,
};
use crate::session::{SessionId, SessionManager};

pub mod bot;
pub mod timers;
pub mod clients;
pub mod conversations;
pub mod call;
pub mod queue;
pub mod groups;

#[cfg(test)]
mod test_support;

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
/// Bound on a logged message-text preview: the trace names the exact
/// length plus this many leading characters. The queue keeps the full
/// text; the log never does.
pub const COMMS_LOG_TEXT_PREVIEW: usize = 200;

/// Quote one log field: backslashes and double quotes escape so the
/// `name="value"` shape survives hostile text. Control characters are
/// stripped later at write time (see [`crate::infra::logging::FileLogger`]).
pub(crate) fn log_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `name="value"` when present, `name=absent` when missing.
fn log_field(name: &str, value: Option<String>) -> String {
    match value {
        Some(v) => format!("{name}={}", log_quote(&v)),
        None => format!("{name}=absent"),
    }
}

/// `text_len=N text="preview"` for a message body, `text=absent` when
/// missing. The preview truncates at [`COMMS_LOG_TEXT_PREVIEW`]
/// characters with a cut marker; the length is always exact.
fn log_text_part(text: Option<String>) -> String {
    match text {
        Some(t) => {
            let len: usize = t.chars().count();
            format!(
                "text_len={len} text={}",
                log_quote(&crate::infra::logging::truncate(&t, COMMS_LOG_TEXT_PREVIEW))
            )
        }
        None => "text=absent".to_string(),
    }
}

/// Log-safe one-line summary of a comms tool call's arguments: target,
/// conversation/timer ids, numeric options, and text length plus a
/// truncated preview. Idempotency keys and anything credential-shaped
/// never ride this summary; unknown tools report only the arg length.
pub fn summarize_call(tool: &str, args: &str) -> String {
    let field = |names: &[&str]| crate::hooks::policy::json_string_field(args.as_bytes(), names);
    let text = || field(&["message", "text", "prompt"]);
    let conv = || field(&["conversation_id", "conversation"]);
    match tool {
        "ask_session" | "tell_session" => {
            let mut out = log_field("target", field(&["target"]));
            if let Some(c) = conv() {
                out.push_str(&format!(" conv={}", log_quote(&c)));
            }
            out.push(' ');
            out.push_str(&log_text_part(text()));
            out
        }
        "send_response" => format!("{} {}", log_field("conv", conv()), log_text_part(text())),
        "ack_message" => log_field("conv", conv()),
        "compact_session" => match field(&["target"]) {
            Some(t) => format!("target={}", log_quote(&t)),
            None => "target=self".to_string(),
        },
        "schedule_prompt" => {
            let mut out = log_text_part(text());
            if let Some(raw) = crate::ipc::mcp::top_raw(args, "delay_seconds") {
                out.push_str(&format!(" delay={}", crate::infra::logging::truncate(raw.trim(), 32)));
            }
            if let Some(raw) = crate::ipc::mcp::top_raw(args, "clear_context") {
                out.push_str(&format!(" clear={}", crate::infra::logging::truncate(raw.trim(), 8)));
            }
            out
        }
        "cancel_scheduled_prompt" => log_field("timer", field(&["timer_id"])),
        _ => format!("args_len={}", args.len()),
    }
}

/// One-line delivery-hold reason for a session with a non-empty queue:
/// pane activity, queued depth, staged-Enter state, and ms since the
/// last human/hook activity (`none` when no record). The caller gates
/// logging (see `settle_comms`) so a stuck message cannot flood the log.
pub fn hold_summary(
    activity: &str,
    queued: usize,
    pending_enter: bool,
    human_ms_ago: Option<u64>,
    hook_ms_ago: Option<u64>,
) -> String {
    let age = |ms: Option<u64>| ms.map_or_else(|| "none".to_string(), |ms| ms.to_string());
    format!(
        "activity={activity} queued={queued} pending_enter={pending_enter} human_ms_ago={} hook_ms_ago={}",
        age(human_ms_ago),
        age(hook_ms_ago),
    )
}

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
            epoch: crate::comms::bot::generate_epoch(),
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
                            crate::comms::bot::now_unix_ms(),
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
                        crate::comms::bot::now_unix_ms(),
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
                        crate::comms::bot::now_unix_ms(),
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
                            crate::comms::bot::now_unix_ms(),
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
                            crate::comms::bot::now_unix_ms(),
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
                        crate::comms::bot::now_unix_ms(),
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
    use super::test_support::*;
    use crate::app::AppState;
    use crate::infra::event::AppEvent;
    use crate::infra::ids::RunId;
    use crate::session::SessionId;

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
    fn summarize_call_names_target_and_truncates_text() {
        let s = summarize_call("tell_session", r#"{"target":"mu_2","text":"hello"}"#);
        assert!(s.contains(r#"target="mu_2""#), "{s}");
        assert!(s.contains("text_len=5"), "{s}");
        assert!(s.contains(r#"text="hello""#), "{s}");
        // Long bodies truncate with a marker; the length stays exact.
        let big = "x".repeat(300);
        let s = summarize_call("ask_session", &format!(r#"{{"target":"b","message":"{big}"}}"#));
        assert!(s.contains("text_len=300"), "{s}");
        assert!(s.contains("..."), "{s}");
        assert!(!s.contains(&big), "full body must not land in the log");
        // Follow-ups name the conversation too.
        let s = summarize_call(
            "tell_session",
            r#"{"target":"b","text":"hi","conversation_id":"conv-9"}"#,
        );
        assert!(s.contains(r#"conv="conv-9""#), "{s}");
        // Missing fields read as absent, never as empty quotes.
        let s = summarize_call("ask_session", r#"{"target":"b"}"#);
        assert!(s.contains("text=absent"), "{s}");
        // Credentials never ride the summary, even if present.
        let s = summarize_call("tell_session", r#"{"target":"b","text":"hi","token":"tok-1"}"#);
        assert!(!s.contains("tok-1"), "{s}");
    }

    #[test]
    fn hold_summary_names_activity_and_debounce_ages() {
        let s = hold_summary("Thinking", 2, false, Some(120), None);
        assert!(s.contains("activity=Thinking"), "{s}");
        assert!(s.contains("queued=2"), "{s}");
        assert!(s.contains("pending_enter=false"), "{s}");
        assert!(s.contains("human_ms_ago=120"), "{s}");
        assert!(s.contains("hook_ms_ago=none"), "{s}");
        let s = hold_summary("Idle", 1, true, None, Some(5));
        assert!(s.contains("pending_enter=true"), "{s}");
        assert!(s.contains("hook_ms_ago=5"), "{s}");
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
            crate::comms::bot::BotConvState::Open,
            "unsent failure notice holds the conv open"
        );
        // Room opens: the next sweep delivers the failure and closes.
        drain_inbox(&mut p, "skippy");
        p.state.broker.tick(std::time::Instant::now());
        assert_eq!(
            p.state.broker.bot_convs.get(&conv).expect("conv").state,
            crate::comms::bot::BotConvState::Failed
        );
        let poll = p.bcall("skippy", "bot_poll", "{}").expect("poll validates");
        assert!(poll.contains(&conv), "poll: {poll}");
        assert_eq!(poll.matches(r#""kind":"failed""#).count(), 1, "poll: {poll}");
    }

    #[test]
    fn full_inbox_parks_bot_exit_notice_for_retry() {
        // The inbox is full when the session exits: the failure
        // notice parks instead of dropping, and the next sweep
        // delivers it once space frees.
        use crate::comms::bot::{BotKind, INBOX_CAP};
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
        let epoch = crate::ipc::mcp::top_raw(&poll1, "epoch").expect("epoch echoed");
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
}
