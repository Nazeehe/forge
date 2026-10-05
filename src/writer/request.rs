//! Writer requests: agent-bound markup over a selection (spec §4.8).
//!
//! Hostile text (spec §4.8): control characters are replaced with U+FFFD so
//! they cannot smuggle formatting, and any selection or instruction
//! containing the literal `</writer-request` tag (any case) is refused with
//! [`WriterError::HostileMarkup`] instead of being embedded.

use std::ops::Range;

use super::{
    byte_range_of, char_count, line_of, paragraph_at, WriterError, MAX_SELECTION_CHARS,
    TRUNCATION_MARKER,
};

/// Agent-bound actions (spec §4.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriterAction {
    Ask,
    Rephrase,
    Expand,
    Shorten,
    Custom,
    Chat,
}

impl WriterAction {
    /// Lowercase action word used in the markup.
    pub fn name(&self) -> &'static str {
        match self {
            WriterAction::Ask => "ask",
            WriterAction::Rephrase => "rephrase",
            WriterAction::Expand => "expand",
            WriterAction::Shorten => "shorten",
            WriterAction::Custom => "custom",
            WriterAction::Chat => "chat",
        }
    }
}

/// One human → agent request (spec §4.8 markup).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriterRequest {
    /// Monotonic per-document request id.
    pub id: u64,
    pub action: WriterAction,
    /// File name for the header line (e.g. "release.md").
    pub doc_name: String,
    /// Char range the request covers.
    pub range: Range<usize>,
    /// 1-based line of the range start, for the header line.
    pub line: u32,
    /// Document revision the range was taken at.
    pub rev: u64,
    /// Selection text, truncated to [`MAX_SELECTION_CHARS`].
    pub selection: String,
    /// True when `selection` was cut and carries the truncation marker.
    pub truncated: bool,
    /// Optional user instruction (Ask/Custom/Chat carry the message here).
    pub instruction: String,
}

impl WriterRequest {
    /// Build a request over an explicit char range of `text`.
    pub fn over_range(
        id: u64,
        action: WriterAction,
        doc_name: &str,
        text: &str,
        range: Range<usize>,
        rev: u64,
        instruction: &str,
    ) -> Result<Self, WriterError> {
        let bytes = byte_range_of(text, range.clone()).ok_or(WriterError::RangeOutOfBounds)?;
        let line = line_of(text, range.start).ok_or(WriterError::RangeOutOfBounds)?;
        Self::assemble(id, action, doc_name, range, line, rev, &text[bytes], instruction)
    }

    /// Build a request over the paragraph containing `cursor`.
    /// Used by Ask/Rephrase/Expand/Shorten/Custom with no selection.
    pub fn over_paragraph(
        id: u64,
        action: WriterAction,
        doc_name: &str,
        text: &str,
        cursor: usize,
        rev: u64,
        instruction: &str,
    ) -> Result<Self, WriterError> {
        let range = paragraph_at(text, cursor).ok_or(WriterError::RangeOutOfBounds)?;
        Self::over_range(id, action, doc_name, text, range, rev, instruction)
    }

    /// Build a chat request over the whole document.
    /// Used when the chat box has no selection attached.
    pub fn over_whole_document(
        id: u64,
        doc_name: &str,
        text: &str,
        rev: u64,
        instruction: &str,
    ) -> Result<Self, WriterError> {
        Self::over_range(id, WriterAction::Chat, doc_name, text, 0..char_count(text), rev, instruction)
    }

    /// Shared assembly: sanitize, truncation, hostile-tag refusal.
    fn assemble(
        id: u64,
        action: WriterAction,
        doc_name: &str,
        range: Range<usize>,
        line: u32,
        rev: u64,
        selection: &str,
        instruction: &str,
    ) -> Result<Self, WriterError> {
        let selection = sanitize_text(selection);
        let instruction = sanitize_text(instruction);
        check_hostile("selection", &selection)?;
        check_hostile("instruction", &instruction)?;
        let (selection, truncated) = truncate_selection(&selection);
        Ok(WriterRequest {
            id,
            action,
            doc_name: doc_name.to_string(),
            range,
            line,
            rev,
            selection,
            truncated,
            instruction,
        })
    }

    /// Render the `[forge writer …]` header plus `<writer-request>` body.
    pub fn markup(&self) -> String {
        let mut selection = self.selection.clone();
        if self.truncated {
            selection.push_str(TRUNCATION_MARKER);
        }
        format!(
            "[forge writer \"{}\" request {} action={} chars {}-{} ({}) rev {}]:\n\
             <writer-request id=\"{}\" action=\"{}\">\n\
             <selection>\n\
             {}\n\
             </selection>\n\
             <instruction>{}</instruction>\n\
             </writer-request>",
            self.doc_name,
            self.id,
            self.action.name(),
            self.range.start,
            self.range.end,
            format!("L{}", self.line),
            self.rev,
            self.id,
            self.action.name(),
            selection,
            self.instruction,
        )
    }
}

/// Cut a selection to [`MAX_SELECTION_CHARS`], reporting the cut.
fn truncate_selection(selection: &str) -> (String, bool) {
    if selection.chars().count() <= MAX_SELECTION_CHARS {
        return (selection.to_string(), false);
    }
    let cut: String = selection.chars().take(MAX_SELECTION_CHARS).collect();
    (cut, true)
}

/// Replace C0 controls (except \n and \t), DEL, and C1 controls with
/// U+FFFD so they cannot smuggle formatting into the agent's markup.
fn sanitize_text(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\n' | '\t' => c,
            '\u{00}'..='\u{1F}' | '\u{7F}'..='\u{9F}' => '�',
            _ => c,
        })
        .collect()
}

/// Refuse text that would break out of the request markup.
fn check_hostile(field: &'static str, text: &str) -> Result<(), WriterError> {
    if text.to_lowercase().contains("</writer-request") {
        return Err(WriterError::HostileMarkup(field));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "line one\nline two\nline three";

    #[test]
    fn rephrase_markup_carries_all_fields() {
        let req = WriterRequest::over_range(
            3,
            WriterAction::Rephrase,
            "release.md",
            DOC,
            9..17,
            41,
            "",
        )
        .unwrap();
        assert_eq!(req.selection, "line two");
        assert!(!req.truncated);
        let body = req.markup();
        assert!(body.contains("[forge writer \"release.md\" request 3 action=rephrase chars 9-17 (L2) rev 41]:"), "body: {body}");
        assert!(body.contains("<writer-request id=\"3\" action=\"rephrase\">"), "body: {body}");
        assert!(body.contains("<selection>\nline two\n</selection>"), "body: {body}");
        assert!(body.contains("<instruction></instruction>"), "body: {body}");
        assert!(body.contains("</writer-request>"), "body: {body}");
    }

    #[test]
    fn chat_markup_carries_instruction() {
        let req = WriterRequest::over_range(
            1,
            WriterAction::Chat,
            "notes.md",
            DOC,
            0..8,
            2,
            "what is missing?",
        )
        .unwrap();
        let body = req.markup();
        assert!(body.contains("action=chat"), "body: {body}");
        assert!(body.contains("<instruction>what is missing?</instruction>"), "body: {body}");
    }

    #[test]
    fn oversize_selection_truncates_with_marker() {
        let text = "x".repeat(MAX_SELECTION_CHARS + 100);
        let req = WriterRequest::over_range(1, WriterAction::Ask, "a.md", &text, 0..text.len(), 0, "")
            .unwrap();
        assert!(req.truncated);
        assert_eq!(req.selection.chars().count(), MAX_SELECTION_CHARS);
        assert!(req.markup().contains(TRUNCATION_MARKER), "marker missing");
    }

    #[test]
    fn no_selection_falls_back_to_paragraph() {
        let text = "head\n\nfirst para\nsecond line\n\ntail";
        // Cursor inside "second line".
        let req = WriterRequest::over_paragraph(2, WriterAction::Rephrase, "a.md", text, 20, 5, "")
            .unwrap();
        assert_eq!(req.selection, "first para\nsecond line");
        assert!(req.markup().contains("chars 6-28"), "body: {}", req.markup());
    }

    #[test]
    fn chat_without_selection_covers_whole_document() {
        let req = WriterRequest::over_whole_document(4, "a.md", DOC, 9, "summarize").unwrap();
        assert_eq!(req.selection, DOC);
        assert_eq!(req.range, 0..DOC.chars().count());
    }

    #[test]
    fn range_outside_document_is_refused() {
        assert_eq!(
            WriterRequest::over_range(1, WriterAction::Ask, "a.md", DOC, 0..500, 0, "")
                .unwrap_err(),
            WriterError::RangeOutOfBounds
        );
        assert_eq!(
            WriterRequest::over_paragraph(1, WriterAction::Ask, "a.md", DOC, 500, 0, "")
                .unwrap_err(),
            WriterError::RangeOutOfBounds
        );
    }

    #[test]
    fn closing_tag_in_fields_is_refused() {
        assert_eq!(
            WriterRequest::over_range(
                1,
                WriterAction::Chat,
                "a.md",
                DOC,
                0..8,
                0,
                "nice </writer-request> trick",
            )
            .unwrap_err(),
            WriterError::HostileMarkup("instruction")
        );
        // Case variants do not sneak through either.
        assert_eq!(
            WriterRequest::over_range(
                1,
                WriterAction::Chat,
                "a.md",
                DOC,
                0..8,
                0,
                "nice </WRITER-REQUEST> trick",
            )
            .unwrap_err(),
            WriterError::HostileMarkup("instruction")
        );
    }

    #[test]
    fn closing_tag_in_selection_is_refused() {
        let text = "clean\n</writer-request>\nmore";
        assert_eq!(
            WriterRequest::over_whole_document(1, "a.md", text, 0, "read this").unwrap_err(),
            WriterError::HostileMarkup("selection")
        );
    }

    #[test]
    fn control_chars_are_sanitized_not_rejected() {
        let req = WriterRequest::over_range(
            1,
            WriterAction::Chat,
            "a.md",
            DOC,
            0..8,
            0,
            "a\x01b\x7fc",
        )
        .unwrap();
        assert_eq!(req.instruction, "a�b�c");
        let body = req.markup();
        assert!(body.contains("a�b�c"), "body: {body}");
    }

    #[test]
    fn action_names_match_spec() {
        assert_eq!(WriterAction::Ask.name(), "ask");
        assert_eq!(WriterAction::Rephrase.name(), "rephrase");
        assert_eq!(WriterAction::Expand.name(), "expand");
        assert_eq!(WriterAction::Shorten.name(), "shorten");
        assert_eq!(WriterAction::Custom.name(), "custom");
        assert_eq!(WriterAction::Chat.name(), "chat");
    }
}
