//! Telegram mobile transport: bounded `getUpdates` polling plus an
//! independently wakeable `sendMessage` worker. The token itself is never
//! stored here or logged; [`read_token`] loads it from a private file on each
//! use so rotation takes effect without restarting Forge.

/// Longest `sendMessage` text in chars (Telegram limit).
pub const MAX_TEXT: usize = 4096;
/// Most updates accepted from one poll response (flood bound).
pub const MAX_UPDATES: usize = 100;
/// Shortest acceptable bot token: operator-minted, never trivial.
/// Mirrors the `bot.rs` credential bound.
pub const TOKEN_MIN_LEN: usize = 16;
/// Telegram response bodies are small JSON envelopes. Bound reads well
/// below ureq's general-purpose default so a broken peer cannot grow us.
pub const MAX_RESPONSE_BYTES: u64 = 512 * 1024;
/// DNS/TCP/TLS establishment must never strand the only transport worker.
pub const HTTP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// Non-polling requests have a short end-to-end budget.
pub const HTTP_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Disabled workers wake periodically to observe live configuration without
/// consuming a core in a tight loop.
pub const DISABLED_WAIT: std::time::Duration = std::time::Duration::from_millis(250);
/// Long failure backoffs are checked in short slices so a settings change is
/// observed promptly instead of waiting as long as fifteen minutes.
pub const BACKOFF_SLICE: std::time::Duration = std::time::Duration::from_millis(250);
/// A reply gets a small bounded retry budget before it becomes a visible drop.
pub const MAX_SEND_ATTEMPTS: u8 = 3;

/// Give Telegram's server-side long poll a bounded transport margin.
pub fn poll_http_timeout(poll_secs: u64) -> std::time::Duration {
    std::time::Duration::from_secs(poll_secs.saturating_add(10))
}

/// One fresh inbound text message from `getUpdates`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Update {
    pub update_id: i64,
    pub user_id: i64,
    pub chat_id: i64,
    pub text: String,
    /// `message_id` of the message this one replies to, when the operator
    /// used Telegram's native reply action. Lets a reply address the
    /// session that sent the original message without typing `[name]`.
    pub reply_to_message_id: Option<i64>,
}

/// Parse a `getUpdates` response body. Only entries carrying fresh
/// `message.text` become [`Update`]s; edits and non-text traffic are
/// skipped so they can never double-deliver.
pub fn parse_updates(body: &str) -> Result<Vec<Update>, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("invalid JSON: {e}"))?;
    if value.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let desc = value
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        return Err(format!("telegram error: {desc}"));
    }
    let none: Vec<serde_json::Value> = Vec::new();
    let results = value
        .get("result")
        .and_then(|v| v.as_array())
        .unwrap_or(&none);
    let mut out = Vec::new();
    for entry in results.iter().take(MAX_UPDATES) {
        let Some(update_id) = entry.get("update_id").and_then(|v| v.as_i64()) else {
            continue;
        };
        let Some(msg) = entry.get("message") else {
            continue;
        };
        let Some(text) = msg.get("text").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(user_id) = msg
            .get("from")
            .and_then(|f| f.get("id"))
            .and_then(|v| v.as_i64())
        else {
            continue;
        };
        let Some(chat_id) = msg
            .get("chat")
            .and_then(|c| c.get("id"))
            .and_then(|v| v.as_i64())
        else {
            continue;
        };
        let reply_to_message_id = msg
            .get("reply_to_message")
            .and_then(|r| r.get("message_id"))
            .and_then(|v| v.as_i64());
        out.push(Update {
            update_id,
            user_id,
            chat_id,
            text: text.to_string(),
            reply_to_message_id,
        });
    }
    Ok(out)
}

/// Offset confirming everything in `updates`: one past the largest
/// `update_id`. `None` keeps the current offset so empty polls replay
/// nothing and advance nothing.
pub fn next_offset(updates: &[Update]) -> Option<i64> {
    updates
        .iter()
        .map(|u| u.update_id)
        .max()
        .map(|m| m.saturating_add(1))
}

/// Confirmation offset from every raw update, including edits, callback
/// queries, media, and malformed message records that are not actionable.
/// Leaving one of those unconfirmed makes Telegram return it forever.
pub fn response_next_offset(body: &str) -> Result<Option<i64>, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("invalid JSON: {e}"))?;
    if value.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let desc = value
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        return Err(format!("telegram error: {desc}"));
    }
    Ok(value
        .get("result")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .take(MAX_UPDATES)
        .filter_map(|entry| entry.get("update_id").and_then(|v| v.as_i64()))
        .max()
        .map(|id| id.saturating_add(1)))
}

/// Sender allowlist. An empty list denies everyone: Telegram delivery
/// fails closed until the operator configures users.
pub fn is_allowed(user_id: i64, allowed: &[i64]) -> bool {
    allowed.contains(&user_id)
}

/// Poll-failure backoff in seconds: `min` doubled per attempt, capped at
/// `max`. Attempt counts saturate so a long outage cannot overflow.
pub fn backoff_delay(attempt: u32, min_secs: u64, max_secs: u64) -> u64 {
    let shift = attempt.min(32);
    min_secs
        .saturating_mul(2u64.saturating_pow(shift))
        .min(max_secs)
}

/// Outbound operator-visible text: the sending session names itself
/// first (`[name] text`) so the operator always knows who answered.
/// Badges stay raw; only the Telegram wire carries the prefix.
pub fn forward_text(session_name: &str, text: &str) -> String {
    format!("[{session_name}] {text}")
}

/// `sendMessage` request body for `chat_id`. Overlong text truncates at a
/// char boundary, preserving the message prefix.
pub fn send_payload(chat_id: i64, text: &str) -> String {
    let cut = truncate_text(text, MAX_TEXT);
    format!(
        r#"{{"chat_id":{chat_id},"text":{}}}"#,
        crate::mcp::escape_json(cut)
    )
}

/// The only token location: the settings form writes here and the
/// poller/tester read here. No UI edits it; `~/` expands via
/// [`expand_user`] at every use.
pub const TELEGRAM_TOKEN_FILE: &str = "~/.forge/telegram.token";

/// Expand a leading `~/` against `$HOME`. Anything else passes
/// through unchanged; an unset `HOME` leaves even tildes alone (the
/// read then fails closed as before).
pub fn expand_user(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            if !home.is_empty() {
                return format!("{home}/{rest}");
            }
        }
    }
    path.to_string()
}

/// Load the bot token from `path`, trimmed. A leading `~/` expands
/// against `$HOME` first (see [`expand_user`]): the settings form
/// stores the path as typed, so every reader must resolve it the same
/// way. Missing, unreadable, empty, and trivially short files all fail
/// closed; callers must never log the value on success or failure.
pub fn read_token(path: &str) -> Result<String, String> {
    let expanded = expand_user(path);
    let raw = std::fs::read_to_string(&expanded)
        .map_err(|e| format!("telegram token unreadable: {e}"))?;
    let token = raw.trim().to_string();
    if token.len() < TOKEN_MIN_LEN {
        return Err("telegram token missing or too short".to_string());
    }
    Ok(token)
}

/// Most inbound messages held for the main loop. Past this the owner
/// drops newcomers and counts them, so a Telegram flood cannot grow the
/// owner without limit.
pub const INBOX_CAP: usize = 64;

/// One allowlisted inbound text, queued for the main loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InboundMessage {
    pub user_id: i64,
    pub chat_id: i64,
    pub text: String,
    /// `message_id` this one replies to, when the operator used
    /// Telegram's native reply action. See [`Update::reply_to_message_id`].
    pub reply_to_message_id: Option<i64>,
}

/// One poller delivery: fresh inbound text, a poll failure, or both.
/// Quiet polls never become reports (see [`Poller::observe`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PollReport {
    pub messages: Vec<InboundMessage>,
    pub failed: bool,
}

/// Long-poll state: the confirmation offset plus the consecutive-failure
/// count driving backoff. Restart resets both by construction: offsets
/// are delivery confirmations, and replaying history after a restart is
/// safer than silently dropping operator text. Offset and failures stay
/// in this struct so `observe` needs no I/O and no clock.
#[derive(Clone, Debug)]
pub struct Poller {
    offset: Option<i64>,
    pending_offset: Option<i64>,
    failures: u32,
}

impl Poller {
    pub fn new() -> Self {
        Poller {
            offset: None,
            pending_offset: None,
            failures: 0,
        }
    }

    /// Confirmed offset for the next `getUpdates` call.
    pub fn offset(&self) -> Option<i64> {
        self.offset
    }

    /// Consecutive failures (transport or parse) since the last success.
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// Commit the raw response offset after an actionable report has been
    /// handed to the bounded owner queue.
    pub fn confirm_observed(&mut self) {
        if let Some(next) = self.pending_offset.take() {
            self.offset = Some(next);
        }
    }

    /// Forget an unhanded observation so the next poll requests it again.
    pub fn abandon_observed(&mut self) {
        self.pending_offset = None;
    }

    /// Fold one fetch result into poller state. `Ok(body)` parses and
    /// partitions by allowlist, confirming seen updates even when denied;
    /// `Err` counts a transport failure. Returns `Some` only when the
    /// owner must act (fresh allowed text or a failure); quiet polls
    /// return `None` so the loop never wakes for nothing.
    pub fn observe(
        &mut self,
        fetch: Result<String, String>,
        allowed: &[i64],
    ) -> Option<PollReport> {
        match fetch {
            Err(_) => {
                self.failures = self.failures.saturating_add(1);
                Some(PollReport {
                    messages: Vec::new(),
                    failed: true,
                })
            }
            Ok(body) => match parse_updates(&body) {
                Err(_) => {
                    self.failures = self.failures.saturating_add(1);
                    Some(PollReport {
                        messages: Vec::new(),
                        failed: true,
                    })
                }
                Ok(updates) => {
                    let recovered = self.failures > 0;
                    self.failures = 0;
                    let raw_next = response_next_offset(&body).ok().flatten();
                    let messages = updates
                        .into_iter()
                        .filter(|u| is_allowed(u.user_id, allowed))
                        .map(|u| InboundMessage {
                            user_id: u.user_id,
                            chat_id: u.chat_id,
                            text: u.text,
                            reply_to_message_id: u.reply_to_message_id,
                        })
                        .collect::<Vec<_>>();
                    if messages.is_empty() {
                        if let Some(next) = raw_next {
                            self.offset = Some(next);
                        }
                        if !recovered {
                            return None;
                        }
                    } else {
                        self.pending_offset = raw_next;
                    }
                    {
                        Some(PollReport {
                            messages,
                            failed: false,
                        })
                    }
                }
            },
        }
    }
}

impl Default for Poller {
    fn default() -> Self {
        Self::new()
    }
}

/// `getUpdates` URL for `token`. `offset` confirms history (absent on a
/// fresh poller); `timeout_secs` is the server-side long-poll hold.
pub fn updates_url(token: &str, offset: Option<i64>, timeout_secs: u64) -> String {
    let mut url = format!("https://api.telegram.org/bot{token}/getUpdates?timeout={timeout_secs}&limit=100");
    if let Some(offset) = offset {
        url.push_str(&format!("&offset={offset}"));
    }
    url
}

/// `sendMessage` URL for `token`.
pub fn send_url(token: &str) -> String {
    format!("https://api.telegram.org/bot{token}/sendMessage")
}

/// One blocking `getUpdates` fetch, returning the raw body. All errors
/// map to one opaque string: ureq errors quote the request URL, which
/// carries the token, so they must never propagate or be logged.
pub fn fetch_updates(
    token: &str,
    offset: Option<i64>,
    timeout_secs: u64,
) -> Result<String, String> {
    let url = updates_url(token, offset, timeout_secs);
    let mut response = ureq::get(&url)
        .config()
        .timeout_connect(Some(HTTP_CONNECT_TIMEOUT))
        .timeout_global(Some(poll_http_timeout(timeout_secs)))
        .build()
        .call()
        .map_err(|_| "telegram poll failed".to_string())?;
    response
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE_BYTES)
        .read_to_string()
        .map_err(|_| "telegram poll failed".to_string())
}

/// Most `message_user` forwards per session per window (blueprint §6).
pub const MESSAGE_USER_CAP: usize = 3;
/// Sliding window for the per-session `message_user` cap.
pub const MESSAGE_USER_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// Sliding-window send gate: prune sends older than the window, then
/// admit while fewer than [`MESSAGE_USER_CAP`] remain. Pruning on every
/// call keeps the window bounded by the cap plus one probe.
pub fn check_message_rate(
    window: &mut std::collections::VecDeque<std::time::Instant>,
    now: std::time::Instant,
) -> bool {
    while window
        .front()
        .is_some_and(|t| now.duration_since(*t) > MESSAGE_USER_WINDOW)
    {
        window.pop_front();
    }
    if window.len() >= MESSAGE_USER_CAP {
        return false;
    }
    window.push_back(now);
    true
}

/// Longest prefix of `s` holding at most `max_chars` chars.
pub fn truncate_text(s: &str, max_chars: usize) -> &str {
    if s.chars().count() > max_chars {
        let end = s
            .char_indices()
            .map(|(i, _)| i)
            .nth(max_chars)
            .unwrap_or(s.len());
        &s[..end]
    } else {
        s
    }
}

/// One connection-test result for the settings dialog. `detail` names
/// the bot (`@user`) or carries an opaque failure; it never holds the
/// token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TelegramTested {
    pub ok: bool,
    pub detail: String,
}

/// Read the bot identity out of a `getMe` response body: `@username`
/// when present, `connected` for an anonymous success. Anything else
/// (error envelope, garbage) fails.
pub fn me_detail(body: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("invalid JSON: {e}"))?;
    if value.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let desc = value
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        return Err(format!("telegram error: {desc}"));
    }
    Ok(value
        .get("result")
        .and_then(|r| r.get("username"))
        .and_then(|u| u.as_str())
        .map(|u| format!("@{u}"))
        .unwrap_or_else(|| "connected".to_string()))
}

/// `getMe` URL for `token`.
pub fn me_url(token: &str) -> String {
    format!("https://api.telegram.org/bot{token}/getMe")
}

/// One blocking `getMe` call, returning the bot identity detail (see
/// [`me_detail`]). Errors stay opaque: they must never carry the token.
pub fn fetch_me(token: &str) -> Result<String, String> {
    let mut response = ureq::get(&me_url(token))
        .config()
        .timeout_connect(Some(HTTP_CONNECT_TIMEOUT))
        .timeout_global(Some(HTTP_SEND_TIMEOUT))
        .build()
        .call()
        .map_err(|_| "telegram test failed".to_string())?;
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE_BYTES)
        .read_to_string()
        .map_err(|_| "telegram test failed".to_string())?;
    me_detail(&body).map_err(|_| "telegram test failed".to_string())
}

/// `message_id` Telegram assigned the message a `sendMessage` call just
/// created, so the caller can later match an inbound reply back to it.
/// Distinct from [`response_next_offset`]'s shape: this reads a single
/// `result` object, not an array of updates.
pub fn sent_message_id(body: &str) -> Result<i64, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("invalid JSON: {e}"))?;
    if value.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let desc = value
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        return Err(format!("telegram error: {desc}"));
    }
    value
        .get("result")
        .and_then(|r| r.get("message_id"))
        .and_then(|v| v.as_i64())
        .ok_or_else(|| "telegram send response carried no message_id".to_string())
}

/// One blocking `sendMessage` POST. Overlong text truncates first (see
/// [`send_payload`]); errors stay opaque for the same reason as
/// [`fetch_updates`]. Returns the new message's `message_id` (see
/// [`sent_message_id`]).
pub fn send_message(token: &str, chat_id: i64, text: &str) -> Result<i64, String> {
    let body = send_payload(chat_id, text);
    let payload: serde_json::Value =
        serde_json::from_str(&body).map_err(|_| "telegram send failed".to_string())?;
    let mut response = ureq::post(&send_url(token))
        .config()
        .timeout_connect(Some(HTTP_CONNECT_TIMEOUT))
        .timeout_global(Some(HTTP_SEND_TIMEOUT))
        .build()
        .send_json(payload)
        .map_err(|_| "telegram send failed".to_string())?;
    let reply_body = response
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE_BYTES)
        .read_to_string()
        .map_err(|_| "telegram send failed".to_string())?;
    sent_message_id(&reply_body).map_err(|_| "telegram send failed".to_string())
}

/// One Telegram reply queued for the dedicated sender worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboundMessage {
    pub chat_id: i64,
    pub text: String,
    attempts: u8,
    /// The session this message speaks for, when it is one (a
    /// `message_user` forward), never a control-plane reply. Once sent,
    /// the owner can map its Telegram `message_id` back to this session
    /// so a native Telegram "reply" addresses it directly.
    pub session: Option<crate::session::SessionId>,
}

impl OutboundMessage {
    pub fn new(chat_id: i64, text: impl Into<String>) -> Self {
        Self { chat_id, text: text.into(), attempts: 0, session: None }
    }

    pub fn for_session(
        chat_id: i64,
        text: impl Into<String>,
        session: crate::session::SessionId,
    ) -> Self {
        Self { chat_id, text: text.into(), attempts: 0, session: Some(session) }
    }
}

/// Most replies held for the sender worker. Past this newcomers drop
/// and count, so a command loop cannot grow the owner without limit.
pub const OUTBOX_CAP: usize = 64;
/// Most replies the poller thread sends per loop turn: paced, never a burst.
pub const OUTBOX_DRAIN: usize = 8;
/// Most inbox messages routed per settle: a flood drains over ticks,
/// at most one injection each; commands and errors also send one reply.
pub const ROUTE_BATCH: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    Empty,
    Sent,
    Retrying(u8),
    Dropped,
}

/// Owner-visible health transition for one attempted delivery.
pub fn delivery_status(outcome: SendOutcome) -> Option<(bool, bool)> {
    match outcome {
        SendOutcome::Empty => None,
        SendOutcome::Sent => Some((false, false)),
        SendOutcome::Retrying(_) => Some((true, false)),
        SendOutcome::Dropped => Some((true, true)),
    }
}

/// Attempt the queue head once. Success removes it; a transient failure keeps
/// it at the head with a bounded attempt count; the final failure drops loudly.
pub fn send_outbox_head(
    queue: &mut std::collections::VecDeque<OutboundMessage>,
    mut send: impl FnMut(&OutboundMessage) -> Result<(), String>,
) -> SendOutcome {
    let Some(mut msg) = queue.pop_front() else {
        return SendOutcome::Empty;
    };
    if send(&msg).is_ok() {
        return SendOutcome::Sent;
    }
    msg.attempts = msg.attempts.saturating_add(1);
    if msg.attempts >= MAX_SEND_ATTEMPTS {
        SendOutcome::Dropped
    } else {
        let attempt = msg.attempts;
        queue.push_front(msg);
        SendOutcome::Retrying(attempt)
    }
}

/// Wait for one queued reply. `notify_one` makes the common path immediate;
/// the timeout lets workers re-read live configuration even without traffic.
pub fn wait_outbound(
    outbox: &std::sync::Arc<
        std::sync::Mutex<std::collections::VecDeque<OutboundMessage>>,
    >,
    wake: &std::sync::Arc<std::sync::Condvar>,
    timeout: std::time::Duration,
) -> Option<OutboundMessage> {
    let queue = outbox.lock().ok()?;
    let (mut queue, _) = wake
        .wait_timeout_while(queue, timeout, |queue| queue.is_empty())
        .ok()?;
    queue.pop_front()
}

/// Pop up to `limit` replies and send each. Failures still pop: Telegram
/// replies are best-effort operator feedback, and requeueing a failing
/// send would wedge the queue behind one bad chat. Returns sends that
/// succeeded.
pub fn drain_outbox(
    queue: &mut std::collections::VecDeque<OutboundMessage>,
    mut send: impl FnMut(&OutboundMessage) -> Result<(), String>,
    limit: usize,
) -> usize {
    let mut sent = 0;
    for _ in 0..limit {
        match send_outbox_head(queue, &mut send) {
            SendOutcome::Sent => sent += 1,
            SendOutcome::Empty | SendOutcome::Retrying(_) => break,
            SendOutcome::Dropped => {}
        }
    }
    sent
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerKind {
    Poller,
    Sender,
}

/// One poller turn: idle when disabled, a fetch when live, bounded backoff
/// after failure, or stop when the owner is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollTurn {
    IdleDisabled,
    Live,
    Backoff(std::time::Duration),
    Stopped,
}

const HANDOFF_RETRIES: usize = 20;
const HANDOFF_WAIT: std::time::Duration = std::time::Duration::from_millis(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandoffOutcome {
    Delivered,
    Busy,
    Stopped,
}

/// Put one report on the bounded owner queue without either blocking forever
/// or confirming a command that Forge did not accept.
pub fn handoff_report(
    poller: &mut Poller,
    tx: &std::sync::mpsc::SyncSender<crate::event::AppEvent>,
    report: PollReport,
) -> HandoffOutcome {
    let mut event = crate::event::AppEvent::TelegramPoll(report);
    for attempt in 0..HANDOFF_RETRIES {
        match tx.try_send(event) {
            Ok(()) => {
                poller.confirm_observed();
                return HandoffOutcome::Delivered;
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                poller.abandon_observed();
                return HandoffOutcome::Stopped;
            }
            Err(std::sync::mpsc::TrySendError::Full(returned)) => {
                event = returned;
                if attempt + 1 < HANDOFF_RETRIES {
                    std::thread::sleep(HANDOFF_WAIT);
                }
            }
        }
    }
    poller.abandon_observed();
    HandoffOutcome::Busy
}

/// Block forever polling Telegram into `tx`. The
/// section is re-read every turn so modal saves land without a restart;
/// the token file itself is re-read too, so rotation bites at once.
/// Disabling stops all traffic until re-enabled. Without a readable
/// token polling waits out backoff. Outbound delivery remains independent.
/// Fail-soft by design: every
/// failure waits and retries; only a dead owner ends the thread. Quiet
/// polls send nothing. The offset lives only in memory: a restart
/// repolls from scratch, which can redeliver recent operator text but
/// never drops it.
pub fn poll_forever(
    shared: std::sync::Arc<std::sync::Mutex<crate::config::TelegramConfig>>,
    tx: std::sync::mpsc::SyncSender<crate::event::AppEvent>,
) {
    let mut poller = Poller::new();
    loop {
        let cfg = shared.lock().map(|cfg| cfg.clone()).unwrap_or_default();
        match poll_turn(&mut poller, &cfg, &tx) {
            PollTurn::Stopped => return,
            PollTurn::IdleDisabled => std::thread::sleep(DISABLED_WAIT),
            PollTurn::Backoff(wait) => sleep_backoff_while_unchanged(&shared, &cfg, wait),
            PollTurn::Live => {}
        }
    }
}

/// One turn of [`poll_forever`], factored for tests: a disabled section
/// touches nothing (no fetch, no events, no confirmations).
pub fn poll_turn(
    poller: &mut Poller,
    cfg: &crate::config::TelegramConfig,
    tx: &std::sync::mpsc::SyncSender<crate::event::AppEvent>,
) -> PollTurn {
    if !cfg.enabled {
        return PollTurn::IdleDisabled;
    }
    let token = read_token(&cfg.token_file).ok();
    let fetch = match token {
        Some(ref token) => fetch_updates(token, poller.offset(), cfg.poll_seconds),
        None => Err("telegram token unreadable".to_string()),
    };
    match poller.observe(fetch, &cfg.allowed_user_ids) {
        None => {}
        Some(report) => {
            let failed = report.failed;
            match handoff_report(poller, tx, report) {
                HandoffOutcome::Delivered => {}
                HandoffOutcome::Busy => return PollTurn::Backoff(DISABLED_WAIT),
                HandoffOutcome::Stopped => return PollTurn::Stopped,
            }
            if failed {
                return PollTurn::Backoff(std::time::Duration::from_secs(backoff_delay(
                    poller.failures().saturating_sub(1),
                    cfg.backoff_min_seconds,
                    cfg.backoff_max_seconds,
                )));
            }
        }
    }
    PollTurn::Live
}

fn sleep_backoff_while_unchanged(
    shared: &std::sync::Arc<std::sync::Mutex<crate::config::TelegramConfig>>,
    original: &crate::config::TelegramConfig,
    total: std::time::Duration,
) {
    let start = std::time::Instant::now();
    loop {
        let elapsed = start.elapsed();
        if elapsed >= total {
            return;
        }
        std::thread::sleep(BACKOFF_SLICE.min(total - elapsed));
        if shared.lock().map(|cfg| &*cfg != original).unwrap_or(true) {
            return;
        }
    }
}

/// Dedicated outbound worker. A condition variable wakes it as soon as the
/// owner queues a reply, independently of any in-flight long poll.
pub fn sender_forever(
    shared: std::sync::Arc<std::sync::Mutex<crate::config::TelegramConfig>>,
    outbox: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<OutboundMessage>>>,
    wake: std::sync::Arc<std::sync::Condvar>,
    tx: std::sync::mpsc::SyncSender<crate::event::AppEvent>,
) {
    let mut failed = false;
    loop {
        let cfg = shared.lock().map(|cfg| cfg.clone()).unwrap_or_default();
        if !cfg.enabled {
            let Ok(queue) = outbox.lock() else { return };
            let _ = wake.wait_timeout(queue, DISABLED_WAIT);
            continue;
        }
        let token = match read_token(&cfg.token_file) {
            Ok(token) => token,
            Err(_) => {
                if !failed {
                    if tx.send(crate::event::AppEvent::TelegramSendStatus { failed: true, dropped: false }).is_err() {
                        return;
                    }
                    failed = true;
                }
                let Ok(queue) = outbox.lock() else { return };
                let _ = wake.wait_timeout(queue, DISABLED_WAIT);
                continue;
            }
        };
        let message = wait_outbound(&outbox, &wake, DISABLED_WAIT);
        let Some(mut message) = message else { continue };
        match send_message(&token, message.chat_id, &message.text) {
            Ok(message_id) => {
                if let Some(session) = message.session {
                    if tx
                        .send(crate::event::AppEvent::TelegramMessageSent { message_id, session })
                        .is_err()
                    {
                        return;
                    }
                }
                if tx.send(crate::event::AppEvent::TelegramSendStatus { failed: false, dropped: false }).is_err() {
                    return;
                }
                failed = false;
            }
            Err(_) => {
                failed = true;
                message.attempts = message.attempts.saturating_add(1);
                let dropped = message.attempts >= MAX_SEND_ATTEMPTS;
                let attempt = message.attempts;
                if !dropped {
                    if let Ok(mut queue) = outbox.lock() {
                        queue.push_front(message);
                    } else {
                        return;
                    }
                }
                if tx.send(crate::event::AppEvent::TelegramSendStatus { failed: true, dropped }).is_err() {
                    return;
                }
                let delay = std::time::Duration::from_secs(1u64 << attempt.min(5));
                let Ok(queue) = outbox.lock() else { return };
                let _ = wake.wait_timeout(queue, delay);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{"ok":true,"result":[
        {"update_id":10,"message":{"message_id":1,"from":{"id":11},"chat":{"id":11},"date":1,"text":"/sessions"}},
        {"update_id":11,"message":{"message_id":2,"from":{"id":99},"chat":{"id":99},"date":2,"text":"hi"}},
        {"update_id":12,"edited_message":{"message_id":3,"from":{"id":11},"chat":{"id":11},"text":"late edit"}}
    ]}"#;

    #[test]
    fn parses_message_updates_and_skips_non_message_entries() {
        let updates = parse_updates(FIXTURE).expect("fixture parses");
        assert_eq!(updates.len(), 2, "edited_message carries no fresh text: {updates:?}");
        assert_eq!(updates[0].update_id, 10);
        assert_eq!(updates[0].user_id, 11);
        assert_eq!(updates[0].chat_id, 11);
        assert_eq!(updates[0].text, "/sessions");
        assert_eq!(updates[1].user_id, 99);
    }

    #[test]
    fn parses_reply_to_message_id_when_present() {
        let body = r#"{"ok":true,"result":[
            {"update_id":20,"message":{"message_id":5,"from":{"id":11},"chat":{"id":11},"date":1,"text":"do more","reply_to_message":{"message_id":2}}},
            {"update_id":21,"message":{"message_id":6,"from":{"id":11},"chat":{"id":11},"date":2,"text":"no reply here"}}
        ]}"#;
        let updates = parse_updates(body).expect("fixture parses");
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].reply_to_message_id, Some(2));
        assert_eq!(updates[1].reply_to_message_id, None);
    }

    #[test]
    fn rejects_error_envelopes_and_garbage() {
        assert!(parse_updates(r#"{"ok":false,"description":"unauthorized"}"#).is_err());
        assert!(parse_updates("not json").is_err());
        assert!(parse_updates(r#"{"ok":true,"result":[]}"#).unwrap().is_empty());
    }

    #[test]
    fn offset_advances_past_max_and_stalls_when_empty() {
        let updates = parse_updates(FIXTURE).expect("fixture parses");
        assert_eq!(next_offset(&updates), Some(12));
        assert_eq!(next_offset(&[]), None, "empty poll keeps its offset");
    }

    #[test]
    fn response_offset_advances_past_non_text_updates() {
        assert_eq!(
            response_next_offset(FIXTURE).expect("fixture envelope parses"),
            Some(13),
            "edited/callback/non-text updates must still be confirmed"
        );
    }

    #[test]
    fn empty_allowlist_denies_everyone() {
        assert!(!is_allowed(11, &[]), "fail closed with no configured users");
        assert!(is_allowed(11, &[11, 22]));
        assert!(!is_allowed(99, &[11, 22]));
    }

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff_delay(0, 60, 900), 60);
        assert_eq!(backoff_delay(1, 60, 900), 120);
        assert_eq!(backoff_delay(2, 60, 900), 240);
        assert_eq!(backoff_delay(10, 60, 900), 900);
        assert_eq!(backoff_delay(100, 60, 900), 900, "attempts never overflow");
    }

    #[test]
    fn send_payload_truncates_long_text_at_char_boundary() {
        let body = send_payload(11, "hi");
        assert!(body.contains(r#""chat_id":11"#), "payload: {body}");
        assert!(body.contains(r#""text":"hi""#), "payload: {body}");
        let long = "é".repeat(5000);
        let cut = send_payload(11, &long);
        let value: serde_json::Value = serde_json::from_str(&cut).expect("payload is JSON");
        let text = value["text"].as_str().expect("text field");
        assert_eq!(text.chars().count(), MAX_TEXT, "truncated to the Telegram limit");
        assert!(long.starts_with(text), "prefix-preserving truncation");
    }

    #[test]
    fn sent_message_id_reads_the_new_message_id() {
        let body = r#"{"ok":true,"result":{"message_id":77,"chat":{"id":11},"text":"hi"}}"#;
        assert_eq!(sent_message_id(body), Ok(77));
        assert!(sent_message_id(r#"{"ok":false,"description":"blocked"}"#).is_err());
        assert!(sent_message_id("not json").is_err());
        assert!(
            sent_message_id(r#"{"ok":true,"result":{"chat":{"id":11}}}"#).is_err(),
            "a response with no message_id must not be mistaken for id 0"
        );
    }

    #[test]
    fn poller_reports_allowed_text_and_tracks_offset() {
        let mut p = Poller::new();
        assert_eq!(p.offset(), None, "fresh poller confirms from scratch");
        let report = p
            .observe(Ok(FIXTURE.to_string()), &[11])
            .expect("allowed text reports");
        assert!(!report.failed);
        assert_eq!(report.messages.len(), 1, "user 99 is not allowlisted");
        assert_eq!(report.messages[0].user_id, 11);
        assert_eq!(report.messages[0].chat_id, 11);
        assert_eq!(report.messages[0].text, "/sessions");
        assert_eq!(p.offset(), None, "actionable updates wait for owner handoff");
        p.confirm_observed();
        assert_eq!(p.offset(), Some(13), "handoff confirms all raw updates, including edits");
        assert_eq!(p.failures(), 0);
    }

    #[test]
    fn failed_handoff_leaves_actionable_updates_unconfirmed() {
        let mut p = Poller::new();
        let report = p.observe(Ok(FIXTURE.to_string()), &[11]).expect("actionable");
        assert_eq!(report.messages.len(), 1);
        assert_eq!(p.offset(), None);
        p.abandon_observed();
        assert_eq!(p.offset(), None, "a full/disconnected owner queue must not lose commands");
    }

    #[test]
    fn bounded_full_owner_handoff_preserves_the_offset() {
        let mut p = Poller::new();
        let report = p.observe(Ok(FIXTURE.to_string()), &[11]).expect("actionable");
        let (tx, _rx) = std::sync::mpsc::sync_channel(0);
        assert_eq!(handoff_report(&mut p, &tx, report), HandoffOutcome::Busy);
        assert_eq!(p.offset(), None, "a full owner queue leaves Telegram unconfirmed");
    }

    #[test]
    fn successful_owner_handoff_confirms_the_offset() {
        let mut p = Poller::new();
        let report = p.observe(Ok(FIXTURE.to_string()), &[11]).expect("actionable");
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        assert_eq!(handoff_report(&mut p, &tx, report), HandoffOutcome::Delivered);
        assert!(matches!(rx.try_recv(), Ok(crate::event::AppEvent::TelegramPoll(_))));
        assert_eq!(p.offset(), Some(13));
    }

    #[test]
    fn poller_stays_quiet_without_actionable_text() {
        let mut p = Poller::new();
        assert!(p.observe(Ok(FIXTURE.to_string()), &[7]).is_none(), "all-denied is quiet");
        assert_eq!(p.offset(), Some(13), "denied and non-text updates are still confirmed");
        assert!(p
            .observe(Ok(r#"{"ok":true,"result":[]}"#.to_string()), &[7])
            .is_none(), "empty poll is quiet");
        assert_eq!(p.offset(), Some(13), "empty poll advances nothing");
        assert_eq!(p.failures(), 0);
    }

    #[test]
    fn poller_counts_failures_and_recovers() {
        let mut p = Poller::new();
        let failed = p.observe(Err("down".to_string()), &[11]).expect("failure reports");
        assert!(failed.failed);
        assert!(failed.messages.is_empty());
        let failed = p.observe(Ok("garbage".to_string()), &[11]).expect("garbage reports");
        assert!(failed.failed);
        assert_eq!(p.failures(), 2);
        assert_eq!(p.offset(), None, "failures confirm nothing");
        let ok = p
            .observe(Ok(FIXTURE.to_string()), &[11, 99])
            .expect("recovery reports");
        assert!(!ok.failed);
        assert_eq!(ok.messages.len(), 2);
        assert_eq!(p.failures(), 0, "success resets the count");
    }

    #[test]
    fn updates_url_carries_offset_only_when_set() {
        let plain = updates_url("tok", None, 20);
        assert!(plain.contains("timeout=20"), "long-poll timeout: {plain}");
        assert!(!plain.contains("offset"), "fresh poller fetches from scratch: {plain}");
        let resumed = updates_url("tok", Some(12), 20);
        assert!(resumed.contains("offset=12"), "resumed poller confirms: {resumed}");
    }

    #[test]
    fn message_rate_allows_three_per_minute() {
        use std::collections::VecDeque;
        use std::time::{Duration, Instant};
        let mut window = VecDeque::new();
        let t0 = Instant::now();
        assert!(check_message_rate(&mut window, t0));
        assert!(check_message_rate(&mut window, t0 + Duration::from_secs(10)));
        assert!(check_message_rate(&mut window, t0 + Duration::from_secs(20)));
        assert!(
            !check_message_rate(&mut window, t0 + Duration::from_secs(30)),
            "fourth send inside the window"
        );
        assert!(
            check_message_rate(&mut window, t0 + Duration::from_secs(61)),
            "window slides past the oldest send"
        );
        assert_eq!(window.len(), 3, "pruned, never grown without limit");
    }

    #[test]
    fn outbox_drain_sends_bounded_batches() {
        use std::collections::VecDeque;
        let mut queue: VecDeque<OutboundMessage> = (0..10)
            .map(|i| OutboundMessage::new(i, format!("m{i}")))
            .collect();
        let mut sent = Vec::new();
        let n = drain_outbox(
            &mut queue,
            |m| {
                sent.push(m.chat_id);
                Ok(())
            },
            4,
        );
        assert_eq!(n, 4);
        assert_eq!(sent, vec![0, 1, 2, 3], "FIFO order");
        assert_eq!(queue.len(), 6);
        let failed = drain_outbox(&mut queue, |_| Err::<(), String>("down".to_string()), 8);
        assert_eq!(failed, 0, "failures send nothing");
        assert_eq!(queue.len(), 6, "failed sends remain queued for retry");
        assert_eq!(queue.front().map(|m| m.chat_id), Some(4), "FIFO head is preserved");
    }

    #[test]
    fn quiet_success_reports_recovery_once() {
        let mut p = Poller::new();
        assert!(p.observe(Err("down".to_string()), &[11]).unwrap().failed);
        let recovered = p
            .observe(Ok(r#"{"ok":true,"result":[]}"#.to_string()), &[11])
            .expect("first success clears visible failure");
        assert!(!recovered.failed);
        assert!(recovered.messages.is_empty());
        assert!(p
            .observe(Ok(r#"{"ok":true,"result":[]}"#.to_string()), &[11])
            .is_none(), "later quiet success stays quiet");
    }

    #[test]
    fn transport_limits_are_finite_and_cover_long_poll() {
        assert!(HTTP_CONNECT_TIMEOUT > std::time::Duration::ZERO);
        assert!(HTTP_SEND_TIMEOUT > std::time::Duration::ZERO);
        assert!(poll_http_timeout(20) > std::time::Duration::from_secs(20));
        assert!(MAX_RESPONSE_BYTES < 1024 * 1024, "Telegram envelopes stay tightly bounded");
    }

    #[test]
    fn sender_attempt_retries_then_drops_at_the_bound() {
        let mut queue = std::collections::VecDeque::from([OutboundMessage::new(7, "reply")]);
        for attempt in 1..MAX_SEND_ATTEMPTS {
            let outcome = send_outbox_head(&mut queue, |_| Err("offline".to_string()));
            assert_eq!(outcome, SendOutcome::Retrying(attempt));
            assert_eq!(queue.len(), 1);
        }
        assert_eq!(
            send_outbox_head(&mut queue, |_| Err("offline".to_string())),
            SendOutcome::Dropped
        );
        assert!(queue.is_empty());
    }

    #[test]
    fn every_success_clears_visible_delivery_failure() {
        assert_eq!(delivery_status(SendOutcome::Sent), Some((false, false)));
        assert_eq!(delivery_status(SendOutcome::Retrying(1)), Some((true, false)));
        assert_eq!(delivery_status(SendOutcome::Dropped), Some((true, true)));
        assert_eq!(delivery_status(SendOutcome::Empty), None);
    }

    #[test]
    fn disabled_sender_and_poller_wait_instead_of_spinning() {
        assert!(DISABLED_WAIT >= std::time::Duration::from_millis(50));
        assert!(DISABLED_WAIT <= std::time::Duration::from_secs(1));
        assert!(BACKOFF_SLICE <= std::time::Duration::from_millis(250));
    }

    #[test]
    fn outbound_enqueue_wakes_sender_without_waiting_for_a_poll() {
        let queue = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
        let wake = std::sync::Arc::new(std::sync::Condvar::new());
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
        let (got_tx, got_rx) = std::sync::mpsc::sync_channel(0);
        let worker_queue = queue.clone();
        let worker_wake = wake.clone();
        let worker = std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            let msg = wait_outbound(&worker_queue, &worker_wake, std::time::Duration::from_secs(2));
            got_tx.send(msg.map(|m| m.text)).unwrap();
        });
        ready_rx.recv().unwrap();
        queue.lock().unwrap().push_back(OutboundMessage::new(7, "now"));
        wake.notify_one();
        assert_eq!(
            got_rx.recv_timeout(std::time::Duration::from_millis(250)).unwrap(),
            Some("now".to_string())
        );
        worker.join().unwrap();
    }

    #[test]
    fn disabled_poller_turn_touches_nothing() {
        let mut poller = Poller::new();
        let cfg = crate::config::TelegramConfig {
            enabled: false,
            ..crate::config::TelegramConfig::default()
        };
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        assert!(matches!(poll_turn(&mut poller, &cfg, &tx), PollTurn::IdleDisabled));
        assert!(rx.try_recv().is_err(), "disabled sends no events");
        assert_eq!(poller.offset(), None, "disabled confirms nothing");
        assert_eq!(poller.failures(), 0, "disabled counts no failures");
    }

    #[test]
    fn tilde_token_paths_expand_against_home() {
        assert_eq!(expand_user("/abs/t.token"), "/abs/t.token");
        assert_eq!(expand_user("rel/t.token"), "rel/t.token");
        let prior = std::env::var("HOME").ok();
        let scratch = std::env::temp_dir().join(format!("forge-tg-home-{}", std::process::id()));
        std::fs::create_dir_all(scratch.join(".forge")).unwrap();
        std::fs::write(scratch.join(".forge/tg.token"), "0123456789abcdef0123456789abcdef").unwrap();
        std::env::set_var("HOME", &scratch);
        assert_eq!(
            expand_user("~/.forge/tg.token"),
            scratch.join(".forge/tg.token").to_string_lossy(),
            "tilde resolves for every reader"
        );
        assert_eq!(
            read_token("~/.forge/tg.token").unwrap(),
            "0123456789abcdef0123456789abcdef",
            "reads follow the expansion"
        );
        match prior {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn forward_prefix_names_session() {
        assert_eq!(forward_text("agent", "hi"), "[agent] hi");
        assert_eq!(forward_text("forge_dev", "i am working on it"), "[forge_dev] i am working on it");
    }

    #[test]
    fn me_detail_names_bot_or_confirms() {
        let ok = r#"{"ok":true,"result":{"id":7,"is_bot":true,"username":"forkbot"}}"#;
        assert_eq!(me_detail(ok).unwrap(), "@forkbot");
        let anon = r#"{"ok":true,"result":{"id":7,"is_bot":true}}"#;
        assert_eq!(me_detail(anon).unwrap(), "connected");
        assert!(me_detail(r#"{"ok":false,"description":"unauthorized"}"#).is_err());
        assert!(me_detail("garbage").is_err());
    }

    #[test]
    fn token_reader_fails_closed() {
        assert!(read_token("/nonexistent-forge-test-token").is_err(), "missing file");
        let dir = std::env::temp_dir().join(format!("forge-tg-token-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let empty = dir.join("empty.token");
        std::fs::write(&empty, "").unwrap();
        assert!(read_token(empty.to_str().unwrap()).is_err(), "empty token");
        let short = dir.join("short.token");
        std::fs::write(&short, "tiny").unwrap();
        assert!(read_token(short.to_str().unwrap()).is_err(), "trivially short token");
        let good = dir.join("good.token");
        std::fs::write(&good, "0123456789abcdef0123456789abcdef\n").unwrap();
        assert_eq!(
            read_token(good.to_str().unwrap()).unwrap(),
            "0123456789abcdef0123456789abcdef",
            "trailing newline trimmed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
