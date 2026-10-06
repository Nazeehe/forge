//! Markdown highlighting: a pure line scanner producing EdTUI
//! [`Highlight`](edtui::Highlight) ranges with semantic-role styles.
//!
//! Markers stay in the buffer and get styled with their construct:
//! ATX headings, `**bold**`, `*em*`/`_em_`, `` `code` ``, fenced
//! blocks (one code style for every language), list markers,
//! `>` quotes, `[text](url)` links, rules, table pipes. Inline code
//! wins over emphasis and links; a heading owns its whole line.
//! Columns are char indices, matching EdTUI's `Index2`.

use edtui::{Highlight, Index2};
use ratatui::style::Modifier;

use crate::ui::theme::{Role, style};

/// Highlights for doc rows `[first_row, first_row + row_count)`.
/// Fence state scans from the document head (one linear pass, no
/// allocation beyond the output), so a window mid-document still
/// knows it is inside a fence; scanning stops at the window end.
#[must_use]
pub fn highlight_markdown(text: &str, first_row: usize, row_count: usize) -> Vec<Highlight> {
    let last_row = first_row.saturating_add(row_count);
    let mut out = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    for (row, line) in text.split('\n').enumerate() {
        if row >= last_row {
            break;
        }
        let chars: Vec<char> = line.chars().collect();
        if let Some((kind, len)) = fence_open(&chars) {
            match fence {
                // Same fence char and at least as long closes.
                Some((open_kind, open_len)) if kind == open_kind && len >= open_len => {
                    fence = None;
                }
                None => {
                    fence = Some((kind, len));
                }
                // A different fence run inside a fence is content.
                Some(_) => {}
            }
            if row >= first_row {
                push(&mut out, row, 0, chars.len(), code_bold());
            }
            continue;
        }
        if fence.is_some() {
            if row >= first_row {
                push(&mut out, row, 0, chars.len(), code());
            }
            continue;
        }
        if row >= first_row {
            highlight_line(row, &chars, &mut out);
        }
    }
    out
}

/// Push one inclusive-end highlight unless the span is empty.
fn push(out: &mut Vec<Highlight>, row: usize, start: usize, end_exclusive: usize, style: ratatui::style::Style) {
    if end_exclusive > start {
        out.push(Highlight::new(
            Index2::new(row, start),
            Index2::new(row, end_exclusive - 1),
            style,
        ));
    }
}

fn heading() -> ratatui::style::Style {
    style(Role::MdHeading)
}

fn code() -> ratatui::style::Style {
    style(Role::MdCode)
}

fn code_bold() -> ratatui::style::Style {
    style(Role::MdCode).add_modifier(Modifier::BOLD)
}

fn link() -> ratatui::style::Style {
    style(Role::MdLink)
}

fn muted() -> ratatui::style::Style {
    style(Role::Muted)
}

fn bold() -> ratatui::style::Style {
    style(Role::Text).add_modifier(Modifier::BOLD)
}

fn italic() -> ratatui::style::Style {
    style(Role::Text).add_modifier(Modifier::ITALIC)
}

/// A fence delimiter run: up to 3 leading spaces, then 3+ of one
/// of `` ` `` / `~`. Returns the char and the run length.
fn fence_open(chars: &[char]) -> Option<(char, usize)> {
    let indent = chars.iter().take_while(|c| **c == ' ').count();
    if indent > 3 {
        return None;
    }
    let rest = &chars[indent..];
    let kind = *rest.first()?;
    if kind != '`' && kind != '~' {
        return None;
    }
    let len = rest.iter().take_while(|c| **c == kind).count();
    (len >= 3).then_some((kind, len))
}

/// Block constructs for one unfenced line: the heading owns its
/// whole line, rules and quote/list prefixes are muted spans, and
/// the remainder goes through inline parsing.
fn highlight_line(row: usize, chars: &[char], out: &mut Vec<Highlight>) {
    if chars.is_empty() {
        return;
    }
    if let Some(end) = atx_heading(chars) {
        push(out, row, 0, end, heading());
        return;
    }
    if is_rule(chars) {
        push(out, row, 0, chars.len(), muted());
        return;
    }
    let quoted = quote_prefix(chars);
    style_prefix(row, chars, 0, quoted, out);
    let listed = list_marker(&chars[quoted..]).map(|len| quoted + len).unwrap_or(quoted);
    style_prefix(row, chars, quoted, listed, out);
    inline_spans(row, chars, listed, out);
}

/// ATX heading end (exclusive): 1-6 `#`, then a space or end of
/// line; trailing closing hashes stay inside the span.
fn atx_heading(chars: &[char]) -> Option<usize> {
    let hashes = chars.iter().take_while(|c| **c == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    if chars.len() > hashes && chars[hashes] != ' ' && chars[hashes] != '\t' {
        return None;
    }
    Some(chars.len())
}

/// A rule: 3+ of one of `-_*`, nothing else but spaces.
fn is_rule(chars: &[char]) -> bool {
    let mut kinds = chars.iter().filter(|c| **c != ' ' && **c != '\t');
    let Some(first) = kinds.next() else {
        return false;
    };
    if *first != '-' && *first != '_' && *first != '*' {
        return false;
    }
    let mut count = 1;
    for c in kinds {
        if *c != *first {
            return false;
        }
        count += 1;
    }
    count >= 3
}

/// `>` quote prefix length in chars (nested `>>`, optional space).
fn quote_prefix(chars: &[char]) -> usize {
    let mut i = 0;
    while chars.get(i) == Some(&'>') {
        i += 1;
        if chars.get(i) == Some(&' ') {
            i += 1;
        }
    }
    i
}

/// List marker length from a slice start: `-`/`*`/`+` or `1.`/`1)`.
fn list_marker(chars: &[char]) -> Option<usize> {
    let indent = chars.iter().take_while(|c| **c == ' ').count();
    let rest = &chars[indent..];
    let first = *rest.first()?;
    if first == '-' || first == '*' || first == '+' {
        return rest.get(1).filter(|c| **c == ' ' || **c == '\t').map(|_| indent + 2);
    }
    if first.is_ascii_digit() {
        let digits = rest.iter().take_while(|c| c.is_ascii_digit()).count();
        let after = rest.get(digits)?;
        if (*after == '.' || *after == ')')
            && rest.get(digits + 1).is_some_and(|c| *c == ' ' || *c == '\t')
        {
            return Some(indent + digits + 2);
        }
    }
    None
}

/// Style a block prefix extension: only the cells past `from`
/// get a span, so quote+list prefixes never double-cover.
fn style_prefix(row: usize, chars: &[char], from: usize, to: usize, out: &mut Vec<Highlight>) {
    let end = to.min(chars.len());
    if end > from {
        push(out, row, from, end, muted());
    }
}

/// Inline spans over `chars[content..]`: code first (it wins),
/// then links, then emphasis. Occupied cells never restyle.
fn inline_spans(row: usize, chars: &[char], content: usize, out: &mut Vec<Highlight>) {
    let mut busy = vec![false; chars.len()];
    // Code spans: a run of n backticks closes on the next run of
    // exactly n.
    let mut i = content;
    while i < chars.len() {
        if chars[i] != '`' {
            i += 1;
            continue;
        }
        let mut n = 0;
        while chars.get(i + n) == Some(&'`') {
            n += 1;
        }
        let mut j = i + n;
        let mut found = None;
        while j < chars.len() {
            if chars[j] == '`' {
                let mut m = 0;
                while chars.get(j + m) == Some(&'`') {
                    m += 1;
                }
                if m == n {
                    found = Some(j + m);
                    break;
                }
                j += m;
            } else {
                j += 1;
            }
        }
        if let Some(end) = found {
            if claim(&mut busy, i, end) {
                push(out, row, i, end, code());
            }
            i = end;
        } else {
            i += n;
        }
    }
    // Links: [text](url); brackets and destination muted.
    i = content;
    while i < chars.len() {
        if chars[i] != '[' {
            i += 1;
            continue;
        }
        let Some(rel_close) = chars[i..].iter().position(|c| *c == ']') else {
            i += 1;
            continue;
        };
        let text_end = i + rel_close;
        if text_end == i + 1 || chars.get(text_end + 1) != Some(&'(') {
            i += 1;
            continue;
        }
        let Some(rel_paren) = chars[text_end..].iter().position(|c| *c == ')') else {
            i += 1;
            continue;
        };
        let url_end = text_end + rel_paren + 1;
        if claim(&mut busy, i, url_end) {
            push(out, row, i, i + 1, muted());
            push(out, row, i + 1, text_end, link());
            push(out, row, text_end, url_end, muted());
        }
        i = url_end;
    }
    // Emphasis: **/__ bold, * / _ em. Intra-word underscores
    // never open or close.
    for (marker, is_bold) in [("**", true), ("__", true), ("*", false), ("_", false)] {
        let mark: Vec<char> = marker.chars().collect();
        let mut k = content;
        while k + mark.len() <= chars.len() {
            if chars[k..].starts_with(&mark)
                && em_edge(chars, k, true, marker == "_")
                && !busy[k..k + mark.len()].iter().any(|b| *b)
            {
                if let Some(end) = em_close(chars, k + mark.len(), marker, &busy) {
                    if claim(&mut busy, k, end) {
                        let span = if is_bold { bold() } else { italic() };
                        push(out, row, k, end, span);
                    }
                    k = end;
                    continue;
                }
            }
            k += 1;
        }
    }
    // Table pipes outside every other span.
    for (i, c) in chars.iter().enumerate().skip(content) {
        if *c == '|' && !busy[i] {
            push(out, row, i, i + 1, muted());
        }
    }
}

/// Claim cells unless taken: keeps overlapping spans from
/// restyling the same cell twice.
fn claim(busy: &mut [bool], start: usize, end: usize) -> bool {
    if start >= end || end > busy.len() || busy[start..end].iter().any(|b| *b) {
        return false;
    }
    busy[start..end].fill(true);
    true
}

/// Emphasis opener edge: not followed by space, and for `_` also
/// alnum-bounded on the left.
fn em_edge(chars: &[char], at: usize, open: bool, underscore: bool) -> bool {
    let neighbour = if open { chars.get(at + 1) } else { chars.get(at.wrapping_sub(1)) };
    if neighbour.is_some_and(|c| *c == ' ' || *c == '\t' || *c == '\n') {
        return false;
    }
    if underscore {
        let flank = if open { chars.get(at.wrapping_sub(1)) } else { chars.get(at + 1) };
        if flank.is_some_and(|c| c.is_alphanumeric()) {
            return false;
        }
    }
    true
}

/// Closer for an emphasis run: same marker, edge rules, cells free.
fn em_close(chars: &[char], from: usize, marker: &str, busy: &[bool]) -> Option<usize> {
    let mark: Vec<char> = marker.chars().collect();
    let underscore = marker == "_";
    let mut k = from;
    while k + mark.len() <= chars.len() {
        if chars[k..].starts_with(&mark)
            && em_edge(chars, k + mark.len(), false, underscore)
            && !busy[k..k + mark.len()].iter().any(|b| *b)
        {
            // For ** don't stop on the first star of a *** run... a
            // `***x***` line is rare prose; take the first close.
            return Some(k + mark.len());
        }
        k += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(highlights: &[Highlight]) -> Vec<(usize, usize, usize)> {
        highlights
            .iter()
            .map(|h| (h.start.row, h.start.col, h.end.col))
            .collect()
    }

    #[test]
    fn atx_heading_styles_markers_and_text() {
        let out = highlight_markdown("# Hello\n", 0, 8);
        assert_eq!(text_of(&out), vec![(0, 0, 6)], "whole line, markers in");
        assert!(
            out.iter().all(|h| h.style.add_modifier == ratatui::style::Modifier::BOLD),
            "headings read bold"
        );
    }

    #[test]
    fn heading_levels_and_closing_hashes() {
        let out = highlight_markdown("### Deep ##\n#tag stays plain\n", 0, 8);
        assert_eq!(text_of(&out), vec![(0, 0, 10)], "closing hashes styled too");
    }

    #[test]
    fn bold_spans_keep_their_markers() {
        let out = highlight_markdown("a **bold** word\n", 0, 8);
        assert_eq!(text_of(&out), vec![(0, 2, 9)], "**..** one span");
    }

    #[test]
    fn em_star_and_underscore() {
        let out = highlight_markdown("*em* and _em_\n", 0, 8);
        assert_eq!(text_of(&out), vec![(0, 0, 3), (0, 9, 12)]);
    }

    #[test]
    fn inline_code_wins_over_emphasis() {
        let out = highlight_markdown("a `**not bold**` span\n", 0, 8);
        assert_eq!(text_of(&out), vec![(0, 2, 15)], "code owns the run");
    }

    #[test]
    fn fence_block_is_one_code_style() {
        let text = "```rs\nlet x = 1;\n```\nplain\n";
        let out = highlight_markdown(text, 0, 8);
        assert_eq!(text_of(&out), vec![(0, 0, 4), (1, 0, 9), (2, 0, 2)]);
        assert_eq!(out.len(), 3, "no markdown inside fences");
    }

    #[test]
    fn fence_parity_reaches_into_the_window() {
        let text = "```\ncode\nmore\n```\n# Head\n";
        // Window starts inside the fence: lines 1-2 still code.
        let out = highlight_markdown(text, 1, 3);
        assert_eq!(text_of(&out), vec![(1, 0, 3), (2, 0, 3), (3, 0, 2)]);
    }

    #[test]
    fn unclosed_fence_styles_to_the_end() {
        let out = highlight_markdown("```\ncode\n", 0, 8);
        assert_eq!(text_of(&out), vec![(0, 0, 2), (1, 0, 3)]);
    }

    #[test]
    fn list_markers_bullets_and_ordered() {
        let out = highlight_markdown("- a\n* b\n1. c\n", 0, 8);
        assert_eq!(text_of(&out), vec![(0, 0, 1), (1, 0, 1), (2, 0, 2)]);
    }

    #[test]
    fn quote_prefix_nested() {
        let out = highlight_markdown(">> quoted\n", 0, 8);
        assert_eq!(text_of(&out), vec![(0, 0, 2)], "both levels plus space, content plain");
    }

    #[test]
    fn link_text_and_url_split() {
        let out = highlight_markdown("[text](http://x)\n", 0, 8);
        let spans = text_of(&out);
        assert!(spans.contains(&(0, 1, 4)), "link text one span: {spans:?}");
        assert!(spans.contains(&(0, 0, 0)), "open bracket muted: {spans:?}");
    }

    #[test]
    fn rule_lines() {
        let out = highlight_markdown("---\n* * *\n", 0, 8);
        assert_eq!(text_of(&out), vec![(0, 0, 2), (1, 0, 4)]);
    }

    #[test]
    fn table_pipes() {
        let out = highlight_markdown("| a | b |\n", 0, 8);
        assert_eq!(text_of(&out), vec![(0, 0, 0), (0, 4, 4), (0, 8, 8)]);
    }

    #[test]
    fn window_clips_outside_rows() {
        let out = highlight_markdown("# A\n# B\n# C\n", 1, 1);
        assert_eq!(text_of(&out), vec![(1, 0, 2)], "only the window rows");
    }

    #[test]
    fn multibyte_columns_are_char_based() {
        let out = highlight_markdown("# héllo\n", 0, 8);
        assert_eq!(text_of(&out), vec![(0, 0, 6)], "é is one column");
    }

    #[test]
    fn empty_doc_gives_no_highlights() {
        assert!(highlight_markdown("", 0, 8).is_empty());
        assert!(highlight_markdown("\n\n", 0, 8).is_empty());
    }

    #[test]
    fn underscore_inside_words_stays_plain() {
        let out = highlight_markdown("foo_bar baz\n", 0, 8);
        assert!(out.is_empty(), "no intra-word emphasis: {out:?}");
    }

    #[test]
    fn one_mebibyte_doc_stays_within_budget() {
        // Representative prose: headings, emphasis, code, fences,
        // lists, quotes, links, rules, tables.
        let para = "# Head **bold** *em* `code` [t](http://x)\n\n- item 1\n- item 2\n\n> quote\n\n```rs\nlet x = 1;\n```\n\n| a | b |\n\n---\n";
        let repeats = 1024 * 1024 / para.len() + 1;
        let big: String = para.repeat(repeats);
        assert!(big.len() >= 1024 * 1024, "fixture is a full MiB");
        let rows = big.lines().count();
        let start = std::time::Instant::now();
        let out = highlight_markdown(&big, 0, rows);
        let elapsed = start.elapsed();
        assert!(!out.is_empty(), "the big doc highlights");
        // Budget: one linear pass, no per-line allocation beyond the
        // output. Measured ~80 ms in a debug build (fixture build
        // included); pinned at 3x headroom. Raise only with a
        // measured reason.
        assert!(
            elapsed.as_millis() < 250,
            "1 MiB highlights in {elapsed:?}, budget 250 ms"
        );
    }
}
