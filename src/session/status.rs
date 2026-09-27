//! Caller sticky status (blueprint #14/#15): one replaceable
//! `kind: message` pair per session, shown in the sidebar. Kinds are a
//! closed set; messages are short plain text, never markup or control
//! bytes, so the sidebar can print them verbatim.

/// Longest status message, in chars and bytes (both bind: wide chars
/// fill bytes first, narrow chars hit the char cap).
pub const MAX_STATUS_CHARS: usize = 80;
pub const MAX_STATUS_BYTES: usize = 320;

/// Closed status kinds, blueprint-exact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusKind {
    Info,
    Progress,
    Success,
    Warning,
    Blocked,
    Question,
}

impl StatusKind {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "info" => Some(StatusKind::Info),
            "progress" => Some(StatusKind::Progress),
            "success" => Some(StatusKind::Success),
            "warning" => Some(StatusKind::Warning),
            "blocked" => Some(StatusKind::Blocked),
            "question" => Some(StatusKind::Question),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            StatusKind::Info => "info",
            StatusKind::Progress => "progress",
            StatusKind::Success => "success",
            StatusKind::Warning => "warning",
            StatusKind::Blocked => "blocked",
            StatusKind::Question => "question",
        }
    }
}

/// One validated sticky status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionStatus {
    pub kind: StatusKind,
    pub message: String,
}

impl SessionStatus {
    /// Sidebar text: `kind: message`, printable by construction.
    pub fn display(&self) -> String {
        format!("{}: {}", self.kind.as_str(), self.message)
    }
}

/// Validate a status message: bounded chars and bytes, no controls or
/// newlines (the sidebar is single-line per row).
pub fn validate(message: &str) -> Result<String, String> {
    if message.chars().count() > MAX_STATUS_CHARS {
        return Err(format!(
            "status over {MAX_STATUS_CHARS} characters"
        ));
    }
    if message.len() > MAX_STATUS_BYTES {
        return Err(format!("status over {MAX_STATUS_BYTES} bytes"));
    }
    if message.chars().any(|c| c.is_control()) {
        return Err("status must not hold controls or newlines".to_string());
    }
    Ok(message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip_and_reject_unknown() {
        for name in ["info", "progress", "success", "warning", "blocked", "question"] {
            assert_eq!(StatusKind::from_name(name).unwrap().as_str(), name);
        }
        assert!(StatusKind::from_name("urgent").is_none());
        assert!(StatusKind::from_name("").is_none());
    }

    #[test]
    fn messages_bound_chars_bytes_and_controls() {
        assert!(validate("compiling").is_ok());
        assert!(validate(&"x".repeat(80)).is_ok());
        assert!(validate(&"x".repeat(81)).is_err());
        assert!(validate(&"𝄞".repeat(80)).is_ok(), "80 wide chars is the boundary");
        assert!(validate(&"𝄞".repeat(81)).is_err());
        assert!(validate("line\nbreak").is_err());
        assert!(validate("tab\there").is_err());
    }

    #[test]
    fn display_prefixes_kind() {
        let status = SessionStatus {
            kind: StatusKind::Blocked,
            message: "waiting on review".to_string(),
        };
        assert_eq!(status.display(), "blocked: waiting on review");
    }
}
