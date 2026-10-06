//! Marker paint model (M2): the rev-gated marker cache on the
//! session, highlight spans per marker part, error rows for the
//! gutter, and user-facing error reasons. Painting lives in
//! `ui/writer`; this file owns the data and the ranges.

use edtui::Highlight;

use super::adapter::offset_to_index2;
use super::WriterSession;
use crate::ui::theme::{style, Role};
use crate::writer::markers::{parse_markers, ErrorKind, MarkerError, ParseOutput};

/// Re-parse `text` into the session cache when `rev` moved.
/// Returns true when it parsed (false = cache hit). Paint calls
/// this once per frame next to the find-match refresh; the O(n)
/// parse runs on edits only, never per frame on a static doc.
pub fn refresh_markers(session: &mut WriterSession, text: &str, rev: u64) -> bool {
    if session.marker_rev == Some(rev) {
        return false;
    }
    session.markers = parse_markers(text);
    session.marker_rev = Some(rev);
    true
}

/// User-facing reason for an error kind, shown in the fixed error
/// slot when the cursor sits on the span.
pub fn error_reason(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Unterminated => "marker not closed: add @@ after the text (or @@end)",
        ErrorKind::EmptyHeader => "marker has no instruction: type a prompt after @@",
        ErrorKind::Nesting => "@@ inside a marker: escape it as \\@@ or close the first marker",
        ErrorKind::StrayCloser => "stray @@: escape as \\@@ if it's literal text",
        ErrorKind::EmptyTarget => {
            "wrap has no text: select text first or use @@end for a standalone marker"
        }
        ErrorKind::PromptTooLong => "prompt too long (max 2000 chars): shorten it",
    }
}

/// Highlights for every marker part and error span, in doc order.
/// Parts are disjoint by construction; callers insert these between
/// the find marks and the markdown marks, so proposal and find win
/// ties and marker styles sit on top of the E4 scanner (EdTUI lets
/// the first-added highlight win). Delimiters muted; the verb in
/// the accent role (`ask` and `?` verbs in the question role); the
/// prompt italic; the target on the marker background; errors in
/// the error role.
pub fn marker_highlights(text: &str, out: &ParseOutput) -> Vec<Highlight> {
    let mut marks: Vec<Highlight> = Vec::new();
    let mut span = |start: usize, end: usize, paint: ratatui::style::Style| {
        if end > start {
            marks.push(Highlight::new(
                offset_to_index2(text, start),
                offset_to_index2(text, end.saturating_sub(1)),
                paint,
            ));
        }
    };
    for m in &out.markers {
        span(m.whole.start, m.whole.start + 2, style(Role::Muted));
        span(m.header.end, m.header.end + 2, style(Role::Muted));
        if let Some(verb) = m.verb.as_ref() {
            let word: String = text
                .chars()
                .skip(verb.start)
                .take(verb.end.saturating_sub(verb.start))
                .collect();
            let role = if word == "ask" || word.ends_with('?') {
                Role::Info
            } else {
                Role::Brand
            };
            span(verb.start, verb.end, style(role));
        }
        span(
            m.prompt.start,
            m.prompt.end,
            style(Role::Text).add_modifier(ratatui::style::Modifier::ITALIC),
        );
        if let Some(target) = m.target.as_ref() {
            span(target.start, target.end, style(Role::MarkerTarget));
            span(target.end, target.end + 2, style(Role::Muted));
            if m.whole.end.saturating_sub(target.end) == 5 {
                span(target.end + 2, target.end + 5, style(Role::Muted));
            }
        } else {
            span(m.whole.end - 3, m.whole.end, style(Role::Muted));
        }
    }
    for e in &out.errors {
        span(e.range.start, e.range.end, style(Role::Danger));
    }
    marks
}

/// Sorted doc rows touched by error spans, for the `✕` gutter.
pub fn error_rows(text: &str, out: &ParseOutput) -> Vec<usize> {
    let mut rows: Vec<usize> = Vec::new();
    for e in &out.errors {
        let first = offset_to_index2(text, e.range.start).row;
        let last = offset_to_index2(text, e.range.end.saturating_sub(1)).row;
        for row in first..=last {
            if !rows.contains(&row) {
                rows.push(row);
            }
        }
    }
    rows.sort();
    rows
}

/// First error whose span holds `offset`, if any.
pub fn error_at(out: &ParseOutput, offset: usize) -> Option<&MarkerError> {
    out.errors.iter().find(|e| e.range.contains(&offset))
}

/// Buffer rows of the locked run's started markers: the live `⟳`
/// gutter follows the reported index. Spans are pre-run positions
/// and drift as the agent edits (the M2 wrapped-row drift, logged
/// for W6); the mark is brightest right after `started`, before
/// the write lands.
pub fn process_spin_rows(text: &str, session: &WriterSession) -> Vec<usize> {
    use crate::app::writer::runs::{RunMarkerStatus, WriterRunState};
    let Some(lock) = session.process.as_ref() else {
        return Vec::new();
    };
    let Some(run) = session
        .runs
        .iter()
        .find(|r| r.id == lock.run_id && matches!(r.state, WriterRunState::Active))
    else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for marker in &run.markers {
        if marker.status != RunMarkerStatus::Started {
            continue;
        }
        let first = offset_to_index2(text, marker.marker.whole.start).row;
        let last = offset_to_index2(text, marker.marker.whole.end.saturating_sub(1)).row;
        for row in first..=last {
            if !rows.contains(&row) {
                rows.push(row);
            }
        }
    }
    rows.sort();
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::markers::{parse_markers, ErrorKind};

    fn session_with(text: &str, rev: u64) -> crate::app::writer::WriterSession {
        let mut session = crate::app::writer::WriterSession::default();
        assert!(refresh_markers(&mut session, text, rev), "first parse");
        session
    }

    #[test]
    fn refresh_reparses_only_on_new_revision() {
        let mut session = crate::app::writer::WriterSession::default();
        assert!(
            refresh_markers(&mut session, "a @@fix x@@y@@", 0),
            "first parse"
        );
        assert_eq!(session.markers.markers.len(), 1);
        assert!(
            !refresh_markers(&mut session, "a @@fix x@@y@@", 0),
            "same rev keeps the cache"
        );
        assert!(
            refresh_markers(&mut session, "changed @@a@@b@@", 1),
            "new rev re-parses"
        );
        assert_eq!(session.markers.markers.len(), 1);
        assert_eq!(session.markers.markers[0].prompt, 10..11);
    }

    #[test]
    fn reasons_name_every_error_kind() {
        assert_eq!(
            error_reason(ErrorKind::Unterminated),
            "marker not closed: add @@ after the text (or @@end)"
        );
        assert_eq!(
            error_reason(ErrorKind::EmptyHeader),
            "marker has no instruction: type a prompt after @@"
        );
        assert_eq!(
            error_reason(ErrorKind::Nesting),
            "@@ inside a marker: escape it as \\@@ or close the first marker"
        );
        assert_eq!(
            error_reason(ErrorKind::StrayCloser),
            "stray @@: escape as \\@@ if it's literal text"
        );
        assert_eq!(
            error_reason(ErrorKind::EmptyTarget),
            "wrap has no text: select text first or use @@end for a standalone marker"
        );
        assert_eq!(
            error_reason(ErrorKind::PromptTooLong),
            "prompt too long (max 2000 chars): shorten it"
        );
    }

    #[test]
    fn highlights_cover_every_wrap_part() {
        let text = "@@fix typo@@this is teh@@";
        let out = parse_markers(text);
        let marks = marker_highlights(text, &out);
        // Opener, header close, verb, prompt, target, target close.
        assert_eq!(marks.len(), 6, "one span per part: {marks:?}");
        let style_at = |col: usize| {
            marks
                .iter()
                .find(|m| m.start == edtui::Index2::new(0, col))
                .expect("part highlight")
                .style
        };
        use crate::ui::theme::{style, Role};
        use ratatui::style::Modifier;
        assert_eq!(style_at(0), style(Role::Muted), "opener muted");
        assert_eq!(style_at(10), style(Role::Muted), "header close muted");
        assert_eq!(style_at(2), style(Role::Brand), "verb accent");
        assert_eq!(
            style_at(6),
            style(Role::Text).add_modifier(Modifier::ITALIC),
            "prompt italic"
        );
        assert_eq!(
            style_at(12),
            style(Role::MarkerTarget),
            "target subtle background"
        );
        assert_eq!(style_at(23), style(Role::Muted), "target close muted");
    }

    #[test]
    fn question_verbs_use_the_distinct_role() {
        use crate::ui::theme::{style, Role};
        for (text, verb_col) in [
            ("@@ask capital@@Paris@@", 2),
            ("@@rewrite?@@x@@", 2),
            ("@@Fix?@@x@@", 2),
        ] {
            let out = parse_markers(text);
            let marks = marker_highlights(text, &out);
            let verb = marks
                .iter()
                .find(|m| m.start == edtui::Index2::new(0, verb_col))
                .expect("verb highlight");
            assert_eq!(verb.style, style(Role::Info), "question verb: {text}");
        }
        let text = "@@fix typo@@this is teh@@";
        let out = parse_markers(text);
        let marks = marker_highlights(text, &out);
        assert_ne!(
            marks
                .iter()
                .find(|m| m.start == edtui::Index2::new(0, 2))
                .expect("verb highlight")
                .style,
            style(Role::Info),
            "plain verbs stay out of the question role"
        );
    }

    #[test]
    fn standalone_paints_without_a_target() {
        let text = "@@note hi @@end";
        let out = parse_markers(text);
        let marks = marker_highlights(text, &out);
        // Opener, header close, verb, prompt, end word.
        assert_eq!(marks.len(), 5, "no target span: {marks:?}");
        use crate::ui::theme::{style, Role};
        let end = marks
            .iter()
            .find(|m| m.start == edtui::Index2::new(0, 12))
            .expect("end highlight");
        assert_eq!(end.style, style(Role::Muted), "closer muted");
        assert_eq!(end.end, edtui::Index2::new(0, 14));
    }

    #[test]
    fn errors_paint_in_the_error_role() {
        let text = "@@fix@@@@target@@";
        let out = parse_markers(text);
        assert!(!out.errors.is_empty(), "fixture has an error");
        let marks = marker_highlights(text, &out);
        use crate::ui::theme::{style, Role};
        assert!(
            marks.iter().all(|m| m.style == style(Role::Danger)),
            "error spans read as errors: {marks:?}"
        );
        assert_eq!(error_rows(text, &out), vec![0]);
    }

    #[test]
    fn error_at_finds_the_span_under_the_cursor() {
        let text = "ok @@p @@end bad @@ here";
        let out = parse_markers(text);
        assert!(!out.errors.is_empty(), "fixture has an error");
        let err = out.errors[0].clone();
        assert!(
            error_at(&out, err.range.start).is_some(),
            "cursor on the error finds it"
        );
        assert!(
            error_at(&out, err.range.end.saturating_sub(1)).is_some(),
            "last error cell finds it"
        );
        assert_eq!(error_at(&out, 0), None, "plain text finds nothing");
    }

    #[test]
    fn unterminated_error_covers_rows_to_end_of_text() {
        let text = "ok\n@@fix@@target with no close\ntail\n";
        let out = parse_markers(text);
        assert_eq!(out.errors.len(), 1);
        assert_eq!(error_rows(text, &out), vec![1, 2]);
    }
}
