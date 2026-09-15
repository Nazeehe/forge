//! External bot clients: operator-registered broker peers (first release).
//!
//! Bots are broker peers without panes. Identity is an operator-issued
//! credential checked against the transport envelope on every call (never
//! MCP arguments); groups are bound at registration; delayed replies wait
//! in a per-client cursor inbox drained by `bot_poll`/`bot_ack`. There is
//! no terminal delivery path for bots, so polling cannot race PTY
//! injection. Everything here is per-broker-epoch: restart invalidates
//! cursors, conversations, and idempotency records by construction (a new
//! broker mints a new epoch). Cross-restart durability is a later
//! increment and must not be assumed.

use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::session::SessionId;

/// Max events returned by one `bot_poll`.
pub const POLL_MAX_EVENTS: usize = 20;
/// Max text bytes carried per inbox event (char-boundary truncated).
pub const EVENT_MAX_TEXT: usize = 8 * 1024;
/// Max unacknowledged events held per client inbox.
pub const INBOX_CAP: usize = 256;
/// Max idempotency keys remembered per client.
pub const IDEM_MAX_KEYS: usize = 1024;
/// How long an idempotency record survives.
pub const IDEM_TTL: Duration = Duration::from_secs(600);
/// Idempotency key shape: 1..=128 chars of `[A-Za-z0-9._:-]`.
pub const KEY_MAX_LEN: usize = 128;
/// Max open client-originated conversations per client (abuse guard
/// mirroring per-target pressure at the client level).
pub const CLIENT_MAX_OUTSTANDING: usize = 20;
/// Credential bounds: operator-minted, never empty or trivially short.
/// Mint with `(umask 077 && openssl rand -hex 32 > token)`; Forge only
/// reads token files, it never writes them.
pub const TOKEN_MIN_LEN: usize = 16;
pub const TOKEN_MAX_LEN: usize = 256;
/// Client name bound (routing label, same human scale as sessions).
pub const CLIENT_NAME_MAX: usize = 64;

/// Stable machine-readable bot error codes. Bot errors fail closed;
/// the harness fail-open policy never applies to them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    Unavailable,
    Unauthorized,
    NotFound,
    AmbiguousTarget,
    NoSharedGroup,
    PressureLimit,
    ConversationClosed,
    Conflict,
    Timeout,
    /// Malformed bot arguments (missing required fields, bad key
    /// charset, unparsable cursor). `conflict` is reserved for valid
    /// requests colliding with broker state: epoch mismatch,
    /// idempotency-key reuse with changed arguments, and cursor
    /// advances outside the received prefix.
    InvalidArguments,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Unavailable => "unavailable",
            ErrorCode::Unauthorized => "unauthorized",
            ErrorCode::NotFound => "not_found",
            ErrorCode::AmbiguousTarget => "ambiguous_target",
            ErrorCode::NoSharedGroup => "no_shared_group",
            ErrorCode::PressureLimit => "pressure_limit",
            ErrorCode::ConversationClosed => "conversation_closed",
            ErrorCode::Conflict => "conflict",
            ErrorCode::Timeout => "timeout",
            ErrorCode::InvalidArguments => "invalid_arguments",
        }
    }
}

/// One bot failure: a stable code for the caller's parser plus a human
/// message. Serializes as the `error` object of the verdict envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotError {
    pub code: ErrorCode,
    pub message: String,
}

impl BotError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        BotError {
            code,
            message: message.into(),
        }
    }

    pub fn to_json(&self) -> String {
        format!(
            r#"{{"code":"{}","message":{}}}"#,
            self.code.as_str(),
            crate::mcp::escape_json(&self.message),
        )
    }
}

/// Constant-time credential comparison (lengths may differ; that only
/// reveals the length band, never a prefix). Empty inputs never compare
/// equal: an absent credential must fail closed, even against itself.
pub fn token_eq(a: &str, b: &str) -> bool {
    let (x, y) = (a.as_bytes(), b.as_bytes());
    if x.is_empty() || y.is_empty() || x.len() != y.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..x.len() {
        diff |= x[i] ^ y[i];
    }
    diff == 0
}

/// Validate a client registration name: a non-empty routing label.
pub fn validate_name(name: &str) -> Result<(), BotError> {
    if name.is_empty() || name.len() > CLIENT_NAME_MAX {
        return Err(BotError::new(
            ErrorCode::InvalidArguments,
            format!("client name must be 1..={CLIENT_NAME_MAX} chars"),
        ));
    }
    Ok(())
}

/// Validate an operator-minted credential (length only; entropy is the
/// operator's mint procedure).
pub fn validate_token(token: &str) -> Result<(), BotError> {
    if token.len() < TOKEN_MIN_LEN || token.len() > TOKEN_MAX_LEN {
        return Err(BotError::new(
            ErrorCode::InvalidArguments,
            format!("client credential must be {TOKEN_MIN_LEN}..={TOKEN_MAX_LEN} chars"),
        ));
    }
    Ok(())
}

/// Validate one group grant: non-empty (existence is not required —
/// groups are created on join, like sessions).
pub fn validate_group(group: &str) -> Result<(), BotError> {
    if group.is_empty() {
        return Err(BotError::new(
            ErrorCode::InvalidArguments,
            "client group must not be empty",
        ));
    }
    Ok(())
}

/// Validate a caller-generated idempotency key.
pub fn validate_key(key: &str) -> Result<(), BotError> {
    if key.is_empty() || key.len() > KEY_MAX_LEN {
        return Err(BotError::new(
            ErrorCode::InvalidArguments,
            format!("idempotency key must be 1..={KEY_MAX_LEN} chars"),
        ));
    }
    if !key
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
    {
        return Err(BotError::new(
            ErrorCode::InvalidArguments,
            "idempotency key must match [A-Za-z0-9._:-]",
        ));
    }
    Ok(())
}

/// Fingerprint canonical send arguments (operation plus exact target,
/// message, and conversation ID) for idempotency comparison.
pub fn fingerprint(op: &str, parts: &[&str]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    let mut h = DefaultHasher::new();
    op.hash(&mut h);
    for part in parts {
        0u8.hash(&mut h);
        part.hash(&mut h);
    }
    h.finish()
}

/// One remembered accepted send: same key plus same fingerprint replays
/// the result, same key with a different fingerprint is a conflict.
struct IdemEntry {
    key: String,
    fingerprint: u64,
    result: String,
    at: Instant,
}

/// Per-client, per-epoch idempotency cache. Bounded with TTL; the epoch
/// boundary (a new broker) drops the whole cache by construction.
pub struct IdemCache {
    entries: VecDeque<IdemEntry>,
}

pub enum IdemCheck {
    Hit(String),
    Miss,
    Conflict,
}

impl IdemCache {
    pub fn new() -> Self {
        IdemCache {
            entries: VecDeque::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    fn prune(&mut self, now: Instant) {
        while self
            .entries
            .front()
            .is_some_and(|e| now.duration_since(e.at) > IDEM_TTL)
        {
            self.entries.pop_front();
        }
    }

    /// Look up a key: expired records read as absent. Callers store only
    /// accepted results, never broker rejections.
    pub fn check(&mut self, key: &str, fingerprint: u64, now: Instant) -> IdemCheck {
        self.prune(now);
        match self.entries.iter().find(|e| e.key == key) {
            None => IdemCheck::Miss,
            Some(e) if e.fingerprint == fingerprint => IdemCheck::Hit(e.result.clone()),
            Some(_) => IdemCheck::Conflict,
        }
    }

    pub fn store(&mut self, key: &str, fingerprint: u64, result: &str, now: Instant) {
        self.prune(now);
        while self.entries.len() >= IDEM_MAX_KEYS {
            self.entries.pop_front();
        }
        self.entries.push_back(IdemEntry {
            key: key.to_string(),
            fingerprint,
            result: result.to_string(),
            at: now,
        });
    }
}

/// Transport event kind of one inbox event. Mirrors the terminal
/// injection kinds minus Command (bots never receive CLI commands).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotKind {
    Ask,
    Response,
    Tell,
    FollowUp,
    Ack,
    Failed,
    Reminder,
}

impl BotKind {
    pub fn api_name(self) -> &'static str {
        match self {
            BotKind::Ask => "ask",
            BotKind::Response => "response",
            BotKind::Tell => "tell",
            BotKind::FollowUp => "follow_up",
            BotKind::Ack => "ack",
            BotKind::Failed => "failed",
            BotKind::Reminder => "reminder",
        }
    }
}

/// Truncate event text to the per-event byte budget on a char boundary.
pub fn truncate_text(text: &str) -> (String, bool) {
    if text.len() <= EVENT_MAX_TEXT {
        return (text.to_string(), false);
    }
    let mut end = EVENT_MAX_TEXT;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

/// Milliseconds since the Unix epoch for event timestamps.
pub fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

static EPOCH_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Mint one broker epoch: 64 bits from the OS pool, hashed process
/// entropy when unavailable. Reminted on every broker boot, so epochs
/// never repeat across restarts in practice.
pub fn generate_epoch() -> u64 {
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let mut buf = [0u8; 8];
        if f.read_exact(&mut buf).is_ok() {
            return u64::from_le_bytes(buf);
        }
    }
    use std::collections::hash_map::DefaultHasher;
    let mut h = DefaultHasher::new();
    std::process::id().hash(&mut h);
    std::time::SystemTime::now().hash(&mut h);
    EPOCH_COUNTER.fetch_add(1, Ordering::Relaxed).hash(&mut h);
    h.finish()
}

/// One structured inbox event: authenticated sender identity and
/// correlation live outside `text`, which the client treats as
/// untrusted content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotEvent {
    pub id: u64,
    pub conv: String,
    pub kind: BotKind,
    pub from_id: String,
    pub from_name: String,
    pub text: String,
    pub unix_ms: u64,
    pub truncated: bool,
}

impl BotEvent {
    pub fn to_json(&self) -> String {
        format!(
            r#"{{"event_id":{},"conversation_id":{},"kind":"{}","from":{{"session_id":{},"name":{}}},"text":{},"unix_ms":{},"truncated":{}}}"#,
            self.id,
            crate::mcp::escape_json(&self.conv),
            self.kind.api_name(),
            crate::mcp::escape_json(&self.from_id),
            crate::mcp::escape_json(&self.from_name),
            crate::mcp::escape_json(&self.text),
            self.unix_ms,
            if self.truncated { "true" } else { "false" },
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotConvKind {
    Ask,
    Tell,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotConvState {
    Open,
    Done,
    Failed,
}

/// One conversation with a bot peer as one party and a live session as
/// the other. Parallel to the session `Conv`, deliberately separate so
/// bot traffic never touches terminal queues.
pub struct BotConv {
    pub kind: BotConvKind,
    pub session: SessionId,
    pub session_name: String,
    pub client: String,
    pub from_client: bool,
    pub state: BotConvState,
    pub acked: bool,
    pub delivered: bool,
    pub last_update: Instant,
    pub reminded: bool,
}

/// One operator-registered bot peer: credential, group grants, cursor
/// inbox, and idempotency cache. The credential is only ever compared,
/// never logged or returned.
pub struct BotClient {
    pub name: String,
    token: String,
    pub groups: Vec<String>,
    /// Tool grants beyond messaging. Parsed and stored, but no tool
    /// consults them in this release (reserved for a later extension;
    /// session creation stays operator-only until then).
    pub grants: Vec<String>,
    token_file: Option<PathBuf>,
    inbox: VecDeque<BotEvent>,
    next_id: u64,
    acked: u64,
    received_upto: u64,
    pub idem: IdemCache,
}

impl BotClient {
    pub fn new(name: &str, token: &str, groups: Vec<String>, grants: Vec<String>) -> Self {
        BotClient {
            name: name.to_string(),
            token: token.to_string(),
            groups,
            grants,
            token_file: None,
            inbox: VecDeque::new(),
            next_id: 1,
            acked: 0,
            received_upto: 0,
            idem: IdemCache::new(),
        }
    }

    /// Bind a token file: the credential is re-read from this path on
    /// every authenticated call, so rotation and deletion bite at once.
    /// Forge only reads token files, it never writes them; mint with
    /// `(umask 077 && openssl rand -hex 32 > file)`.
    pub fn set_token_file(&mut self, path: PathBuf) {
        self.token_file = Some(path);
    }

    /// Re-read the bound token file when one is bound. Missing,
    /// unreadable, or malformed files clear the credential (fail
    /// closed). Returns whether the installed secret changed. With no
    /// file bound, reads nothing and changes nothing.
    pub fn refresh_token(&mut self) -> bool {
        let Some(path) = self.token_file.clone() else {
            return false;
        };
        let secret = std::fs::read_to_string(&path)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        // Owner-only on Unix: any group/other permission bit fails
        // closed. metadata follows symlinks, so link games cannot
        // widen access; Forge is the server boundary and enforces this
        // itself rather than trusting callers to check.
        #[cfg(unix)]
        let secret = {
            use std::os::unix::fs::PermissionsExt;
            let locked_down = std::fs::metadata(&path)
                .map(|m| m.permissions().mode() & 0o077 == 0)
                .unwrap_or(false);
            if locked_down {
                secret
            } else {
                String::new()
            }
        };
        let next = if (TOKEN_MIN_LEN..=TOKEN_MAX_LEN).contains(&secret.len()) {
            secret
        } else {
            String::new()
        };
        if self.token == next {
            return false;
        }
        self.token = next;
        true
    }

    /// Constant-time credential check against the stored secret.
    pub fn check_token(&self, presented: &str) -> bool {
        token_eq(&self.token, presented)
    }

    pub fn unacked(&self) -> usize {
        self.inbox.len()
    }

    pub fn acked_cursor(&self) -> u64 {
        self.acked
    }

    /// Replay-or-reserve one idempotent send: a hit returns the stored
    /// result, a reused key with changed arguments conflicts, a miss
    /// reserves nothing (the caller stores after acceptance).
    pub fn idem_check(
        &mut self,
        key: Option<&str>,
        fingerprint: u64,
        now: Instant,
    ) -> Result<Option<String>, BotError> {
        let Some(key) = key else {
            return Ok(None);
        };
        validate_key(key)?;
        match self.idem.check(key, fingerprint, now) {
            IdemCheck::Hit(result) => Ok(Some(result)),
            IdemCheck::Miss => Ok(None),
            IdemCheck::Conflict => Err(BotError::new(
                ErrorCode::Conflict,
                "idempotency key reused with different arguments",
            )),
        }
    }

    /// Remember one accepted send result for later replays.
    pub fn idem_store(
        &mut self,
        key: Option<&str>,
        fingerprint: u64,
        result: &str,
        now: Instant,
    ) {
        if let Some(key) = key {
            self.idem.store(key, fingerprint, result, now);
        }
    }

    /// Deposit one event, truncating text to budget. Fails when the
    /// inbox cap is reached; the sender must surface pressure.
    pub fn deposit(
        &mut self,
        kind: BotKind,
        conv: &str,
        from_id: &str,
        from_name: &str,
        text: &str,
        unix_ms: u64,
    ) -> Result<u64, ()> {
        if self.inbox.len() >= INBOX_CAP {
            return Err(());
        }
        let (text, truncated) = truncate_text(text);
        let id = self.next_id;
        self.next_id += 1;
        self.inbox.push_back(BotEvent {
            id,
            conv: conv.to_string(),
            kind,
            from_id: from_id.to_string(),
            from_name: from_name.to_string(),
            text,
            unix_ms,
            truncated,
        });
        Ok(id)
    }

    /// Events after `start`, oldest first, up to `limit`. Paging ahead
    /// of the received prefix does not extend it: only a contiguous
    /// run from the acknowledged cursor counts as received.
    pub fn poll(&mut self, start: u64, limit: usize) -> Vec<BotEvent> {
        let out: Vec<BotEvent> = self
            .inbox
            .iter()
            .filter(|e| e.id > start)
            .take(limit)
            .cloned()
            .collect();
        for e in &out {
            if e.id == self.received_upto + 1 {
                self.received_upto = e.id;
            }
        }
        out
    }

    /// Advance the acknowledged cursor through a contiguous received
    /// prefix only. Anything else is a conflict, never a silent skip.
    pub fn ack(&mut self, cursor: u64) -> Result<u64, BotError> {
        if cursor <= self.acked || cursor > self.received_upto {
            return Err(BotError::new(
                ErrorCode::Conflict,
                "ack cursor must advance through received events only",
            ));
        }
        self.inbox.retain(|e| e.id > cursor);
        self.acked = cursor;
        Ok(cursor)
    }
}

/// Test-only token writer with owner-only permissions (0600 on
/// Unix; default elsewhere). Production never writes token files;
/// operators mint with `(umask 077 && openssl rand -hex 32 > file)`.
#[cfg(test)]
pub(crate) fn write_test_token(path: &std::path::Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn client() -> BotClient {
        BotClient::new("skippy", TOKEN, vec!["peers".to_string()], Vec::new())
    }

    #[test]
    fn token_compare_is_exact() {
        assert!(token_eq(TOKEN, TOKEN));
        assert!(!token_eq(TOKEN, "0123456789abcdef0123456789abcdeg"));
        assert!(!token_eq(TOKEN, "short"));
        assert!(!token_eq("", ""));
    }

    #[test]
    fn error_json_carries_code_and_escapes_message() {
        let e = BotError::new(ErrorCode::PressureLimit, "cap for \"b\"\nnow");
        assert_eq!(
            e.to_json(),
            r#"{"code":"pressure_limit","message":"cap for \"b\"\nnow"}"#
        );
    }

    #[test]
    fn key_shape_is_enforced() {
        assert!(validate_key("a").is_ok());
        assert!(validate_key("AZaz09._:-").is_ok());
        let long = "k".repeat(KEY_MAX_LEN + 1);
        for bad in ["", long.as_str(), "has space", "semi;colon"] {
            assert_eq!(
                validate_key(bad).unwrap_err().code,
                ErrorCode::InvalidArguments,
                "key: {bad:?}"
            );
        }
    }

    #[test]
    fn name_token_group_shape_is_enforced() {
        assert!(validate_name("skippy").is_ok());
        assert!(validate_group("peers").is_ok());
        assert!(validate_token(TOKEN).is_ok());
        assert_eq!(
            ErrorCode::InvalidArguments.as_str(),
            "invalid_arguments"
        );
        for bad in [
            validate_name(""),
            validate_name(&"n".repeat(CLIENT_NAME_MAX + 1)),
            validate_token("short"),
            validate_group(""),
        ] {
            assert_eq!(bad.unwrap_err().code, ErrorCode::InvalidArguments);
        }
    }

    #[test]
    fn idempotency_replays_conflicts_and_expires() {
        let mut cache = IdemCache::new();
        let now = Instant::now();
        let fp = fingerprint("ask_session", &["b", "hi", ""]);
        assert!(matches!(cache.check("k1", fp, now), IdemCheck::Miss));
        cache.store("k1", fp, r#"{"conversation":"c1"}"#, now);
        match cache.check("k1", fp, now) {
            IdemCheck::Hit(r) => assert_eq!(r, r#"{"conversation":"c1"}"#),
            _ => panic!("replay must hit"),
        }
        let other = fingerprint("ask_session", &["b", "other", ""]);
        assert!(matches!(cache.check("k1", other, now), IdemCheck::Conflict));
        let later = now + IDEM_TTL + Duration::from_secs(1);
        assert!(matches!(cache.check("k1", fp, later), IdemCheck::Miss));
    }

    #[test]
    fn idempotency_evicts_oldest_past_the_cap() {
        let mut cache = IdemCache::new();
        let now = Instant::now();
        for n in 0..IDEM_MAX_KEYS + 5 {
            let key = format!("k{n}");
            cache.store(&key, n as u64, "r", now);
        }
        assert_eq!(cache.len(), IDEM_MAX_KEYS);
        assert!(matches!(
            cache.check("k0", 0, now),
            IdemCheck::Miss
        ));
        assert!(matches!(
            cache.check(&format!("k{}", IDEM_MAX_KEYS + 4), (IDEM_MAX_KEYS + 4) as u64, now),
            IdemCheck::Hit(_)
        ));
    }

    #[test]
    fn long_text_truncates_on_a_char_boundary() {
        let oul: String = "é".repeat(EVENT_MAX_TEXT);
        let (text, truncated) = truncate_text(&oul);
        assert!(text.len() <= EVENT_MAX_TEXT);
        assert!(truncated);
        assert!(text.contains('é'));
        let (short, flag) = truncate_text("hi");
        assert_eq!(short, "hi");
        assert!(!flag);
    }

    #[test]
    fn event_json_names_every_field() {
        let e = BotEvent {
            id: 42,
            conv: "c1".to_string(),
            kind: BotKind::FollowUp,
            from_id: "s7".to_string(),
            from_name: "forge".to_string(),
            text: "hi".to_string(),
            unix_ms: 1_700_000_000_000,
            truncated: false,
        };
        let j = e.to_json();
        assert!(j.contains(r#""event_id":42"#), "j: {j}");
        assert!(j.contains(r#""conversation_id":"c1""#), "j: {j}");
        assert!(j.contains(r#""kind":"follow_up""#), "j: {j}");
        assert!(j.contains(r#""session_id":"s7""#), "j: {j}");
        assert!(j.contains(r#""name":"forge""#), "j: {j}");
        assert!(j.contains(r#""unix_ms":1700000000000"#), "j: {j}");
        assert!(j.contains(r#""truncated":false"#), "j: {j}");
    }

    #[test]
    fn poll_ack_redelivers_until_acknowledged() {
        let mut c = client();
        c.deposit(BotKind::Tell, "c1", "s1", "a", "one", 1).unwrap();
        c.deposit(BotKind::Tell, "c1", "s1", "a", "two", 2).unwrap();
        let first = c.poll(0, 20);
        assert_eq!(first.len(), 2);
        assert_eq!((first[0].id, first[1].id), (1, 2));
        c.ack(1).unwrap();
        assert_eq!(c.unacked(), 1);
        // Duplicate read redelivers the unacked remainder.
        let again = c.poll(0, 20);
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].id, 2);
        c.ack(2).unwrap();
        assert!(c.poll(0, 20).is_empty());
    }

    #[test]
    fn ack_rejects_skips_and_unknown_cursors() {
        let mut c = client();
        c.deposit(BotKind::Tell, "c1", "s1", "a", "one", 1).unwrap();
        c.deposit(BotKind::Tell, "c1", "s1", "a", "two", 2).unwrap();
        assert!(c.ack(0).is_err(), "zero advances nothing");
        // Page ahead without receiving the prefix, then try to skip it.
        let page = c.poll(1, 20);
        assert_eq!(page.len(), 1);
        assert!(c.ack(2).is_err(), "gap 1..=2 never received contiguously");
        assert!(c.ack(99).is_err(), "unknown cursor");
        // Receive in order, then advance.
        assert!(c.poll(0, 20).len() == 2);
        c.ack(2).unwrap();
        assert!(c.ack(2).is_err(), "no double advance");
    }

    #[test]
    fn inbox_cap_fails_the_deposit() {
        let mut c = client();
        for n in 0..INBOX_CAP {
            c.deposit(BotKind::Tell, "c", "s1", "a", &n.to_string(), 1)
                .expect("fits to cap");
        }
        assert!(c
            .deposit(BotKind::Tell, "c", "s1", "a", "overflow", 1)
            .is_err());
        assert_eq!(c.unacked(), INBOX_CAP);
    }

    #[test]
    fn token_file_refresh_loads_rotates_and_revokes() {
        let dir = std::env::temp_dir().join(format!("forge-bot-token-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("skippy.token");
        // Trailing newlines (echo minting) are tolerated.
        write_test_token(&path, &format!("{TOKEN}\n"));
        // A distinct inline secret proves the file install does the work.
        let mut c = BotClient::new(
            "skippy",
            "11111111111111111111111111111111",
            vec!["peers".to_string()],
            Vec::new(),
        );
        assert!(!c.refresh_token(), "no file bound reads nothing");
        c.set_token_file(path.clone());
        assert!(c.refresh_token(), "first load installs the secret");
        assert!(c.check_token(TOKEN));
        assert!(!c.refresh_token(), "unchanged file is quiet");
        // Rotation takes effect on the next refresh.
        let rotated = "fedcba9876543210fedcba9876543210";
        write_test_token(&path, rotated);
        assert!(c.refresh_token());
        assert!(!c.check_token(TOKEN));
        assert!(c.check_token(rotated));
        // Malformed files clear the credential: fail closed.
        write_test_token(&path, "short");
        assert!(c.refresh_token());
        assert!(!c.check_token("short"));
        // Restore a live credential, then deleting the file revokes it.
        std::fs::write(&path, rotated).unwrap();
        assert!(c.refresh_token());
        assert!(c.check_token(rotated));
        std::fs::remove_file(&path).unwrap();
        assert!(c.refresh_token());
        assert!(!c.check_token(rotated));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn group_readable_token_files_fail_closed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("forge-bot-perm-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("skippy.token");
        std::fs::write(&path, TOKEN).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let live = "33333333333333333333333333333333";
        let mut c = BotClient::new("skippy", live, vec!["peers".to_string()], Vec::new());
        c.set_token_file(path.clone());
        // The insecure file installs nothing and revokes the live inline
        // credential: fail closed, with the change reported.
        assert!(c.refresh_token(), "insecure file revokes live credential");
        assert!(!c.check_token(TOKEN));
        assert!(!c.check_token(live));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
