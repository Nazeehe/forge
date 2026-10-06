//! Writer assistant-panel paint: thread rows with P2 diffs, chat box,
//! status and action rows, error slot, and the panel click targets.
//!
//! Painters shared with the document view are `pub(super)`; geometry
//! stays in `layout`.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::editor::row_of;
use super::layout::pill_spans;
use crate::app::writer::{WriterFocus, WriterSession};
use crate::ui::theme::{style, Role};

/// Fixed one-line narrow notice in place of the panel.

pub(super) fn paint_notice(f: &mut Frame, area: Rect) {
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            "widen to ≥100 columns",
            style(Role::Warning),
        )])),
        area,
    );
}

/// Click target on one panel row: pills fire, any other row of a
/// proposal entry selects it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelClick {
    Select(u64),
    Accept(u64),
    Reject(u64),
}

/// One assistant-panel row with its click target. Built once per
/// paint AND per mouse event from the same function, so hit areas
/// can never desync from what is on screen.
pub struct PanelRow<'a> {
    pub line: Line<'a>,
    pub click: Option<PanelClick>,
}

fn diff_rows(
    rows: &mut Vec<PanelRow<'_>>,
    proposal: &crate::writer::proposal::Proposal,
    width: usize,
    selected: bool,
) {
    let head = if selected { "> " } else { "  " };
    rows.push(PanelRow {
        line: Line::from(vec![Span::styled(
            format!(
                "{head}P{} chars {}-{}",
                proposal.id, proposal.range.start, proposal.range.end
            ),
            if selected {
                style(Role::Focus)
            } else {
                style(Role::Muted)
            },
        )]),
        click: Some(PanelClick::Select(proposal.id)),
    });
    for row in proposal.original.split('\n') {
        rows.push(PanelRow {
            line: Line::from(vec![
                Span::styled("- ".to_string(), style(Role::Danger)),
                Span::styled(truncate(row, width), style(Role::Text)),
            ]),
            click: Some(PanelClick::Select(proposal.id)),
        });
    }
    for row in proposal.text.split('\n') {
        rows.push(PanelRow {
            line: Line::from(vec![
                Span::styled("+ ".to_string(), style(Role::Success)),
                Span::styled(truncate(row, width), style(Role::Text)),
            ]),
            click: Some(PanelClick::Select(proposal.id)),
        });
    }
}

pub fn panel_rows(session: &WriterSession, width: usize) -> Vec<PanelRow<'_>> {
    let mut rows: Vec<PanelRow<'_>> = Vec::new();
    for proposal in session.proposals.pending() {
        let selected = session.selected_proposal == Some(proposal.id);
        diff_rows(&mut rows, proposal, width, selected);
        if let Some(note) = proposal.note.as_ref() {
            rows.push(PanelRow {
                line: Line::from(vec![Span::styled(
                    truncate(&format!("note: {note}"), width),
                    style(Role::Muted),
                )]),
                click: Some(PanelClick::Select(proposal.id)),
            });
        }
        rows.push(PanelRow {
            line: pill_line(&[("Accept", true, true), ("Reject", false, false)]),
            click: None,
        });
    }
    // A selected stale proposal stays visible with its diff, but its
    // pills are gone: Accept is disabled once the text moved.
    if let Some(selected) = session.selected_proposal {
        if let Some(proposal) = session.proposals.get(selected) {
            if proposal.state == crate::writer::proposal::ProposalState::Stale {
                diff_rows(&mut rows, proposal, width, true);
                rows.push(PanelRow {
                    line: Line::from(vec![Span::styled(
                        "stale: text changed",
                        style(Role::Warning),
                    )]),
                    click: Some(PanelClick::Select(proposal.id)),
                });
            }
        }
    }
    // Recent answers first-glance last: at most 8, Markdown-skinned.
    // Run finish notes ride the same thread, headed `Run N:`.
    for entry in session.thread.iter().rev().take(8).rev() {
        let head = match entry.run {
            Some(run) => format!("Run {run}:"),
            None => format!("A{}:", entry.request_id),
        };
        rows.push(PanelRow {
            line: Line::from(vec![Span::styled(head, style(Role::Brand))]),
            click: None,
        });
        for text_line in crate::walkthrough::highlight::md_text(&entry.answer).lines {
            rows.push(PanelRow {
                line: truncate_line(text_line, width),
                click: None,
            });
        }
    }
    if rows.is_empty() {
        rows.push(PanelRow {
            line: Line::from(vec![Span::styled(
                "proposals and answers land here",
                style(Role::Muted),
            )]),
            click: None,
        });
    }
    rows
}

/// Rows dropped off the top when the panel overflows: bottom-anchored.
pub fn panel_skip(rows: usize, height: usize) -> usize {
    rows.saturating_sub(height)
}

/// Assistant panel: pending proposals with diff rows, note, and pills,
/// then recent thread answers as Markdown. Bottom-anchored; oldest
/// rows drop when the panel overflows.
pub(super) fn paint_panel(f: &mut Frame, area: Rect, session: &WriterSession, buffer: &str) {
    let _ = buffer;
    let rows = panel_rows(session, area.width as usize);
    let skip = panel_skip(rows.len(), area.height as usize);
    f.render_widget(
        Paragraph::new(rows.into_iter().skip(skip).map(|row| row.line).collect::<Vec<_>>()),
        area,
    );
}

/// Chat box: selection chip row, `> ` input row, hint row. Returns the
/// input cursor cell.
pub(super) fn paint_chat(
    f: &mut Frame,
    area: Rect,
    session: &WriterSession,
) -> Option<ratatui::layout::Position> {
    let chip = match session.selection.as_ref() {
        Some(range) => format!("[{}–{}] (✕)", range.start, range.end),
        None => "no selection".to_string(),
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(chip, style(Role::Info))])),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let input: String = session
        .chat_input
        .chars()
        .take((area.width as usize).saturating_sub(2))
        .collect();
    // A select-all renders the whole input reversed for
    // type-to-replace.
    let input_style = if session.chat_select_all {
        style(Role::Text).add_modifier(ratatui::style::Modifier::REVERSED)
    } else {
        style(Role::Text)
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("> ".to_string(), style(Role::Brand)),
            Span::styled(input.clone(), input_style),
        ])),
        Rect::new(area.x, area.y.saturating_add(1), area.width, 1),
    );
    let hint = if area.width >= 60 {
        "Enter sends · Rephrase uses the selection or paragraph"
    } else {
        "Enter sends"
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(hint, style(Role::Muted))])),
        Rect::new(area.x, area.y.saturating_add(2), area.width, 1),
    );
    if session.focus != WriterFocus::Chat {
        return None;
    }
    let before: String = session
        .chat_input
        .chars()
        .take(session.chat_cursor)
        .collect();
    use ratatui::text::Line as TextLine;
    let dx = TextLine::from(format!("> {before}")).width() as u16;
    Some(ratatui::layout::Position::new(
        area.x.saturating_add(dx.min(area.width.saturating_sub(1))),
        area.y.saturating_add(1),
    ))
}

/// Status row: revision, 1-based line:col, word/char counts (plus
/// the selection's when one exists), agent activity word, the word
/// `unsaved` while dirty, and the last save result.
pub(super) fn paint_status(
    f: &mut Frame,
    area: Rect,
    session: &WriterSession,
    buffer: &str,
    rev: u64,
    dirty: bool,
    activity: crate::session::Activity,
) {
    let (line, col) = session
        .editor
        .as_ref()
        .map(|editor| {
            let off = crate::app::writer::adapter::editor_cursor_offset(editor);
            (row_of(buffer, off) + 1, off - line_start(buffer, off) + 1)
        })
        .unwrap_or((1, 1));
    let (words, chars) = counts(buffer);
    let mut text = format!(
        "rev {rev} · {line}:{col} · {words}w {chars}c · {}",
        crate::comms::Broker::activity_label(activity)
    );
    if let Some(range) = session.selection.as_ref() {
        let selected: String = buffer
            .chars()
            .skip(range.start)
            .take(range.end.saturating_sub(range.start))
            .collect();
        let (sel_words, sel_chars) = counts(&selected);
        text.push_str(&format!(" · sel {sel_words}w {sel_chars}c"));
    }
    if dirty {
        text.push_str(" · unsaved");
    }
    if let Some(note) = session.save_note.as_ref() {
        text.push_str(&format!(" · {note}"));
    }
    let marker_count = session.markers.markers.len();
    if marker_count > 0 {
        text.push_str(&format!(
            " · {} marker{}",
            marker_count,
            if marker_count == 1 { "" } else { "s" }
        ));
    }
    let error_count = session.markers.errors.len();
    if error_count > 0 {
        text.push_str(&format!(
            " · {} error{}",
            error_count,
            if error_count == 1 { "" } else { "s" }
        ));
    }
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(text, style(Role::Muted))])),
        area,
    );
}

/// Words (whitespace runs) and chars of `text`.
fn counts(text: &str) -> (usize, usize) {
    (
        text.split_whitespace().count(),
        text.chars().count(),
    )
}

/// Start offset of the line holding `offset`.
fn line_start(text: &str, offset: usize) -> usize {
    let mut start = 0;
    for (index, c) in text.chars().enumerate() {
        if index >= offset {
            break;
        }
        if c == '\n' {
            start = index + 1;
        }
    }
    start
}

/// One row of pills separated by two spaces.
fn pill_line(buttons: &[(&str, bool, bool)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, (label, emphasized, default_mark)) in buttons.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ".to_string(), Style::default()));
        }
        spans.extend(pill_spans(label, *emphasized, (*default_mark).then_some('*')));
    }
    Line::from(spans)
}

/// Truncate a row to the measured width (char count approximates the
/// prose cells the panel holds).
fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    text.chars().take(width.saturating_sub(1)).collect::<String>() + "…"
}

fn truncate_line(line: Line<'_>, width: usize) -> Line<'_> {
    let mut kept = 0usize;
    let mut spans = Vec::new();
    for span in line.spans {
        let take = width.saturating_sub(kept);
        let text: String = span.content.chars().take(take).collect();
        kept += text.chars().count();
        spans.push(Span::styled(text, span.style));
        if kept >= width {
            break;
        }
    }
    Line::from(spans)
}

/// Fixed one-row error slot: the message or nothing, never shifting
/// the rows around it.
pub(super) fn paint_error_slot(f: &mut Frame, area: Rect, error: Option<&str>) {
    let line = match error {
        Some(message) => Line::from(vec![Span::styled(message.to_string(), style(Role::Danger))]),
        None => Line::from(vec![Span::styled(String::new(), Style::default())]),
    };
    f.render_widget(Paragraph::new(line), area);
}

/// Watch notice for the fixed slot: informational (Info, never the
/// error Danger), painted only when no confirm, bar, or error owns
/// the row.
pub(super) fn paint_banner_slot(f: &mut Frame, area: Rect, banner: Option<&str>) {
    let line = match banner {
        Some(message) => Line::from(vec![Span::styled(message.to_string(), style(Role::Info))]),
        None => Line::from(vec![Span::styled(String::new(), Style::default())]),
    };
    f.render_widget(Paragraph::new(line), area);
}

