//! Opaque per-launch session identities.
//!
//! A run ID proves ownership of an incoming request; rebinding revokes the old
//! value. It is the authority for MCP callers — never the session name.

use std::fmt;
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};

/// Hex-encoded length of a run ID (128 bits of entropy).
pub const RUN_ID_LEN: usize = 32;

static FALLBACK_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RunId(String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunIdError {
    BadLength(usize),
    BadCharset,
}

impl fmt::Display for RunIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunIdError::BadLength(n) => write!(f, "run ID must be {RUN_ID_LEN} chars, got {n}"),
            RunIdError::BadCharset => write!(f, "run ID must be lowercase hex"),
        }
    }
}

impl std::error::Error for RunIdError {}

fn os_entropy() -> Option<[u8; 16]> {
    let mut f = std::fs::File::open("/dev/urandom").ok()?;
    let mut buf = [0u8; 16];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn fallback_entropy() -> [u8; 16] {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    std::process::id().hash(&mut h);
    std::time::SystemTime::now().hash(&mut h);
    FALLBACK_COUNTER.fetch_add(1, Ordering::Relaxed).hash(&mut h);
    let a = h.finish();
    let mut hh = DefaultHasher::new();
    a.hash(&mut hh);
    (!a).hash(&mut hh);
    let b = hh.finish();
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&a.to_le_bytes());
    out[8..].copy_from_slice(&b.to_le_bytes());
    out
}

fn hex(bytes: &[u8; 16]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(RUN_ID_LEN);
    for b in bytes {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 0xf) as usize] as char);
    }
    s
}

impl RunId {
    pub fn generate() -> Self {
        let bytes = os_entropy().unwrap_or_else(fallback_entropy);
        RunId(hex(&bytes))
    }

    pub fn parse(s: &str) -> Result<Self, RunIdError> {
        if s.len() != RUN_ID_LEN {
            return Err(RunIdError::BadLength(s.len()));
        }
        if !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(RunIdError::BadCharset);
        }
        Ok(RunId(s.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Opaque conversation identity for ask/response and tell/ack flows.
/// Same entropy shape as a run ID, distinct type so the two never mix.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ConversationId(String);

impl ConversationId {
    pub fn generate() -> Self {
        let bytes = os_entropy().unwrap_or_else(fallback_entropy);
        ConversationId(hex(&bytes))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ConversationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn generated_shape() {
        let id = RunId::generate();
        assert_eq!(id.as_str().len(), RUN_ID_LEN);
        assert!(id.as_str().bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn generated_unique() {
        let set: HashSet<String> = (0..100).map(|_| RunId::generate().to_string()).collect();
        assert_eq!(set.len(), 100);
    }

    #[test]
    fn parse_roundtrip() {
        let id = RunId::generate();
        assert_eq!(RunId::parse(id.as_str()).unwrap(), id);
    }

    #[test]
    fn parse_rejects_bad() {
        assert_eq!(RunId::parse("").unwrap_err(), RunIdError::BadLength(0));
        assert_eq!(
            RunId::parse(&"a".repeat(RUN_ID_LEN - 1)).unwrap_err(),
            RunIdError::BadLength(RUN_ID_LEN - 1)
        );
        assert_eq!(
            RunId::parse(&"a".repeat(RUN_ID_LEN + 1)).unwrap_err(),
            RunIdError::BadLength(RUN_ID_LEN + 1)
        );
        assert_eq!(
            RunId::parse(&"z".repeat(RUN_ID_LEN)).unwrap_err(),
            RunIdError::BadCharset
        );
        // Uppercase hex is valid hex but must normalize, not fail.
        let upper = "A".repeat(RUN_ID_LEN);
        assert_eq!(RunId::parse(&upper).unwrap().as_str(), "a".repeat(RUN_ID_LEN));
    }
}
