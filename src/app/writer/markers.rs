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

/// Margin glyph state (M7, spec §10): the shape carries the
/// state, never color alone — `⏳` queued, `🔄` working, `❌`
/// failed or skipped. Sort order is paint priority: on a shared
/// row the highest glyph wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MarginGlyph {
    Queued,
    Working,
    Failed,
}

impl MarginGlyph {
    /// The painted symbol: single codepoint, no VS16.
    pub fn symbol(&self) -> &'static str {
        match self {
            MarginGlyph::Queued => "⏳",
            MarginGlyph::Working => "🔄",
            MarginGlyph::Failed => "❌",
        }
    }

    fn of(status: super::runs::RunMarkerStatus) -> Option<Self> {
        use super::runs::RunMarkerStatus;
        match status {
            RunMarkerStatus::Pending => Some(MarginGlyph::Queued),
            RunMarkerStatus::Started => Some(MarginGlyph::Working),
            RunMarkerStatus::Failed | RunMarkerStatus::Skipped => Some(MarginGlyph::Failed),
            RunMarkerStatus::Done => None,
        }
    }
}

/// Re-anchor margin marks to the CURRENT parse (M7 tracking): the
/// pre-run spans drift as the agent edits, so marks match on the
/// raw verb/prompt, taking the first candidate whose whole-start
/// line still equals the mark-time line. A user edit on that line
/// (or a vanished marker) drops the mark; shifts elsewhere keep
/// it. Returns buffer rows with glyphs, deduped to the highest
/// glyph per row, and prunes dead marks in place.
pub fn run_margin_marks(
    text: &str,
    parsed: &ParseOutput,
    marks: &mut Vec<super::runs::RunMark>,
) -> Vec<(usize, MarginGlyph)> {
    let chars: Vec<char> = text.chars().collect();
    let slice = |range: std::ops::Range<usize>| -> String {
        chars.get(range).unwrap_or(&[]).iter().collect()
    };
    let line_of = |offset: usize| -> (usize, String) {
        let row = offset_to_index2(text, offset.min(chars.len())).row;
        let mut start = offset.min(chars.len());
        while start > 0 && chars[start - 1] != '\n' {
            start -= 1;
        }
        let line: String = chars[start..]
            .iter()
            .take_while(|&&c| c != '\n')
            .collect();
        (row, line)
    };
    let mut kept = Vec::with_capacity(marks.len());
    let mut rows: Vec<(usize, MarginGlyph)> = Vec::with_capacity(marks.len());
    for mark in marks.drain(..) {
        let Some(glyph) = MarginGlyph::of(mark.status) else {
            continue;
        };
        let anchored = parsed
            .markers
            .iter()
            .filter(|m| {
                m.verb.clone().map(|v| slice(v)) == mark.verb
                    && slice(m.prompt.clone()) == mark.prompt
            })
            .map(|m| line_of(m.whole.start))
            .find(|(_, line)| *line == mark.line_text);
        match anchored {
            Some((row, _)) => {
                // Shared rows keep the highest glyph (❌ over 🔄
                // over ⏳): one margin, one symbol.
                match rows.iter_mut().find(|(r, _)| *r == row) {
                    Some(slot) if slot.1 < glyph => slot.1 = glyph,
                    Some(_) => {}
                    None => rows.push((row, glyph)),
                }
                kept.push(mark);
            }
            None => {}
        }
    }
    *marks = kept;
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

    use crate::app::writer::runs::{RunMark, RunMarkerStatus};

    fn queued_mark(run_id: u64, doc_index: usize, verb: Option<&str>, prompt: &str, line: &str) -> RunMark {
        RunMark {
            run_id,
            doc_index,
            verb: verb.map(str::to_string),
            prompt: prompt.to_string(),
            line_text: line.to_string(),
            status: RunMarkerStatus::Pending,
        }
    }

    #[test]
    fn margin_anchor_follows_markers_as_lines_shift() {
        let text = "alpha @@fix typo@@this is teh@@\n\n@@ask capital@@Paris@@\n";
        let mut marks = vec![
            queued_mark(1, 0, Some("fix"), "typo", "alpha @@fix typo@@this is teh@@"),
            queued_mark(1, 1, Some("ask"), "capital", "@@ask capital@@Paris@@"),
        ];
        let out = parse_markers(text);
        let rows = run_margin_marks(text, &out, &mut marks);
        assert_eq!(rows, vec![(0, MarginGlyph::Queued), (2, MarginGlyph::Queued)]);
        // A reload inserts a line above: both glyphs follow.
        let shifted = "new head\n".to_string() + text;
        let out = parse_markers(&shifted);
        let rows = run_margin_marks(&shifted, &out, &mut marks);
        assert_eq!(rows, vec![(1, MarginGlyph::Queued), (3, MarginGlyph::Queued)]);
        assert_eq!(marks.len(), 2, "shifts never drop marks");
    }

    #[test]
    fn margin_anchor_drops_a_mark_whose_line_changed() {
        let text = "alpha @@fix typo@@this is teh@@\n\n@@ask capital@@Paris@@\n";
        let mut marks = vec![
            queued_mark(1, 0, Some("fix"), "typo", "alpha @@fix typo@@this is teh@@"),
            queued_mark(1, 1, Some("ask"), "capital", "@@ask capital@@Paris@@"),
        ];
        // The user fixes the typo on the first marker's line: the
        // marker still parses, but its line changed.
        let edited = "alpha @@fix typo@@this is the@@\n\n@@ask capital@@Paris@@\n";
        let out = parse_markers(edited);
        let rows = run_margin_marks(edited, &out, &mut marks);
        assert_eq!(rows, vec![(2, MarginGlyph::Queued)], "edited line drops");
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].doc_index, 1);
    }

    #[test]
    fn margin_anchor_drops_vanished_markers() {
        let text = "alpha @@fix typo@@this is teh@@\n\n@@ask capital@@Paris@@\n";
        let mut marks = vec![
            queued_mark(1, 0, Some("fix"), "typo", "alpha @@fix typo@@this is teh@@"),
            queued_mark(1, 1, Some("ask"), "capital", "@@ask capital@@Paris@@"),
        ];
        let gone = "alpha done\n\n@@ask capital@@Paris@@\n";
        let out = parse_markers(gone);
        let rows = run_margin_marks(gone, &out, &mut marks);
        assert_eq!(rows, vec![(2, MarginGlyph::Queued)]);
        assert!(marks.iter().all(|m| m.doc_index == 1));
    }

    #[test]
    fn margin_rows_dedupe_sharing_a_row_to_the_highest_glyph() {
        let text = "@@fix a@@b@@ @@note c@@end\n";
        let mut marks = vec![
            RunMark {
                status: RunMarkerStatus::Failed,
                ..queued_mark(1, 0, Some("fix"), "a", "@@fix a@@b@@ @@note c@@end")
            },
            queued_mark(1, 1, Some("note"), "c", "@@fix a@@b@@ @@note c@@end"),
        ];
        let out = parse_markers(text);
        assert_eq!(out.markers.len(), 2, "fixture shares row 0");
        let rows = run_margin_marks(text, &out, &mut marks);
        assert_eq!(rows, vec![(0, MarginGlyph::Failed)], "❌ wins the row");
    }
}
