//! Writer domain core (vertical slice S2): document, proposals, requests.
//!
//! Widget-independent logic over plain text and character offsets (spec
//! §4.7 seam: nothing here imports an editor widget). Positions are char
//! offsets; display line:col is derived.
//!
//! Interim slice rule (spec §7.0): staleness does NOT shift ranges. Any edit
//! intersecting a pending proposal — or entirely before it — marks it stale.
//! Full shifting arrives in a later task.

pub mod document;
pub mod proposal;
pub mod request;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;

/// Largest accepted document, in bytes (spec §6).
pub const MAX_DOC_BYTES: usize = 1_048_576;

/// Longest selection/instruction slice sent to the agent, in chars (spec §6).
pub const MAX_SELECTION_CHARS: usize = 8_000;

/// Longest proposal body, in chars (spec §6).
pub const MAX_PROPOSAL_CHARS: usize = 32_000;

/// Most pending proposals per document; the next is refused (spec §4.4).
pub const MAX_PENDING_PROPOSALS: usize = 16;

/// Marker appended when a selection is cut for the agent (spec §4.8).
pub const TRUNCATION_MARKER: &str = "[…truncated, call writer_read for full text]";

/// Errors for document open/save, proposals, and request markup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriterError {
    /// Path rejected by the relative-path jail.
    Confined(crate::infra::paths::PathError),
    /// Extension is not .md / .markdown / .txt.
    NotMarkdown(String),
    /// File is larger than [`MAX_DOC_BYTES`]; carries the byte size.
    TooLarge(u64),
    /// File is not valid UTF-8 (never rewritten lossily).
    InvalidUtf8,
    /// On-disk content changed since load/last save; save refused.
    ConflictOnSave,
    /// Filesystem I/O failure; carries the message.
    Io(String),
    /// Char range outside the document.
    RangeOutOfBounds,
    /// Proposal body exceeds [`MAX_PROPOSAL_CHARS`]; carries char count.
    ProposalTooLarge(usize),
    /// [`MAX_PENDING_PROPOSALS`] already pending.
    TooManyPending,
    /// No proposal with this id.
    UnknownProposal(u64),
    /// Proposal went stale; accept is disabled.
    StaleProposal(u64),
    /// Proposal already accepted or rejected.
    AlreadySettled(u64),
    /// Selection or instruction would break out of the request markup.
    HostileMarkup(&'static str),
}

impl std::fmt::Display for WriterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriterError::Confined(e) => write!(f, "path rejected: {e}"),
            WriterError::NotMarkdown(p) => write!(f, "not a Markdown file: {p}"),
            WriterError::TooLarge(n) => write!(f, "file too large ({n} bytes)"),
            WriterError::InvalidUtf8 => write!(f, "file is not valid UTF-8"),
            WriterError::ConflictOnSave => write!(f, "file changed on disk; save refused"),
            WriterError::Io(m) => write!(f, "i/o error: {m}"),
            WriterError::RangeOutOfBounds => write!(f, "range outside the document"),
            WriterError::ProposalTooLarge(n) => write!(f, "proposal too large ({n} chars)"),
            WriterError::TooManyPending => write!(f, "too many pending proposals"),
            WriterError::UnknownProposal(id) => write!(f, "unknown proposal {id}"),
            WriterError::StaleProposal(id) => write!(f, "proposal {id} is stale; re-ask or reject"),
            WriterError::AlreadySettled(id) => write!(f, "proposal {id} already settled"),
            WriterError::HostileMarkup(field) => write!(
                f,
                "{field} contains markup that would break the request; rephrase it"
            ),
        }
    }
}

impl std::error::Error for WriterError {}

/// Hash for on-disk change detection (in-memory only, never persisted).
pub(crate) fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// Char count of `text`.
pub(crate) fn char_count(text: &str) -> usize {
    text.chars().count()
}

/// Byte range of a char-offset range; `None` when out of bounds.
pub(crate) fn byte_range_of(text: &str, range: Range<usize>) -> Option<Range<usize>> {
    if range.start > range.end {
        return None;
    }
    let total = char_count(text);
    if range.end > total {
        return None;
    }
    let mut start_byte = text.len();
    let mut end_byte = text.len();
    for (char_idx, (byte_idx, _)) in text.char_indices().enumerate() {
        if char_idx == range.start {
            start_byte = byte_idx;
        }
        if char_idx == range.end {
            end_byte = byte_idx;
            break;
        }
    }
    // An end at the char count means end of string; a start at the char
    // count of an empty range is also end of string.
    if range.end == total {
        end_byte = text.len();
    }
    if range.start == total {
        start_byte = text.len();
    }
    Some(start_byte..end_byte)
}

/// Byte index of a char offset; `None` when out of bounds.
/// The end of text (`offset == char count`) maps to `text.len()`.
fn byte_index_of(text: &str, offset: usize) -> Option<usize> {
    let total = char_count(text);
    if offset > total {
        return None;
    }
    if offset == total {
        return Some(text.len());
    }
    text.char_indices()
        .nth(offset)
        .map(|(byte_idx, _)| byte_idx)
}

/// 1-based line number of a char offset; `None` when out of bounds.
pub(crate) fn line_of(text: &str, offset: usize) -> Option<u32> {
    let byte = byte_index_of(text, offset)?;
    if offset == char_count(text) && !text.is_empty() {
        return None;
    }
    Some(text[..byte].chars().filter(|&c| c == '\n').count() as u32 + 1)
}

/// 1-based (line, col) of a char offset; `None` when out of bounds.
pub(crate) fn line_col_of(text: &str, offset: usize) -> Option<(u32, u32)> {
    let byte = byte_index_of(text, offset)?;
    if offset == char_count(text) && !text.is_empty() {
        return None;
    }
    let prefix = &text[..byte];
    let line = prefix.chars().filter(|&c| c == '\n').count() as u32 + 1;
    let col = prefix.rsplit('\n').next().unwrap_or("").chars().count() as u32 + 1;
    Some((line, col))
}

/// Char range of the paragraph (block between blank lines) containing
/// `offset`; `None` when out of bounds. Blank means empty or whitespace-only.
pub(crate) fn paragraph_at(text: &str, offset: usize) -> Option<Range<usize>> {
    if text.is_empty() || offset >= char_count(text) {
        return None;
    }
    // Char ranges of each line (without the newline).
    let mut lines: Vec<Range<usize>> = Vec::new();
    let mut start = 0;
    for (idx, c) in text.chars().enumerate() {
        if c == '\n' {
            lines.push(start..idx);
            start = idx + 1;
        }
    }
    if start <= char_count(text) {
        lines.push(start..char_count(text));
    }
    let is_blank = |r: &Range<usize>| {
        byte_range_of(text, r.clone())
            .map(|b| text[b].trim().is_empty())
            .unwrap_or(true)
    };
    let mut line_idx = lines
        .iter()
        .position(|r| r.start <= offset && offset < r.end || (r.is_empty() && offset == r.start))?;
    // An offset on a blank line has no paragraph.
    if is_blank(&lines[line_idx]) {
        // Unless it is the trailing newline edge; keep it simple: no para.
        return None;
    }
    while line_idx > 0 && !is_blank(&lines[line_idx - 1]) {
        line_idx -= 1;
    }
    let mut end_idx = line_idx;
    while end_idx + 1 < lines.len() && !is_blank(&lines[end_idx + 1]) {
        end_idx += 1;
    }
    Some(lines[line_idx].start..lines[end_idx].end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_range_of_ascii_and_multibyte() {
        assert_eq!(byte_range_of("hello", 1..4), Some(1..4));
        // 'é' is 2 bytes: char range 1..3 covers "éc".
        assert_eq!(byte_range_of("aéc", 1..3), Some(1..4));
        // End at the char count means end of string.
        assert_eq!(byte_range_of("héllo", 0..5), Some(0..6));
        // Empty range is an insertion point.
        assert_eq!(byte_range_of("abc", 2..2), Some(2..2));
    }

    #[test]
    fn byte_range_rejects_out_of_bounds() {
        assert_eq!(byte_range_of("abc", 0..4), None);
        assert_eq!(byte_range_of("abc", 3..2), None);
        assert_eq!(byte_range_of("", 0..1), None);
        assert_eq!(byte_range_of("", 0..0), Some(0..0));
    }

    #[test]
    fn line_numbers_are_one_based() {
        let text = "aa\nbbb\nc";
        assert_eq!(line_of(text, 0), Some(1));
        assert_eq!(line_of(text, 2), Some(1));
        assert_eq!(line_of(text, 3), Some(2));
        assert_eq!(line_of(text, 7), Some(3));
        assert_eq!(line_of(text, 8), None);
    }

    #[test]
    fn line_col_counts_chars_not_bytes() {
        let text = "aéc\nxy";
        assert_eq!(line_col_of(text, 0), Some((1, 1)));
        assert_eq!(line_col_of(text, 2), Some((1, 3)));
        // Offset 3 is the newline itself: end of line 1.
        assert_eq!(line_col_of(text, 3), Some((1, 4)));
        assert_eq!(line_col_of(text, 4), Some((2, 1)));
        assert_eq!(line_col_of(text, 6), None);
    }

    #[test]
    fn paragraph_stops_at_blank_lines() {
        let text = "head\n\nfirst para\nsecond line\n\ntail";
        // Offset inside "first para" (char 8) selects the whole block.
        let para = paragraph_at(text, 8).unwrap();
        assert_eq!(&text[byte_range_of(text, para.clone()).unwrap()], "first para\nsecond line");
        // Offset in the head block selects just it.
        let head = paragraph_at(text, 1).unwrap();
        assert_eq!(&text[byte_range_of(text, head).unwrap()], "head");
    }

    #[test]
    fn paragraph_at_document_edges() {
        assert_eq!(paragraph_at("", 0), None);
        let single = paragraph_at("only", 2).unwrap();
        assert_eq!(single, 0..4);
        assert_eq!(paragraph_at("only", 4), None);
    }
}
