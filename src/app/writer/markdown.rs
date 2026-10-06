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

/// Cached fence parity: `starts[i]` is the fence open at the
/// start of doc line `i` (char and run length, `None` outside).
/// The full tuple is stored because only a same-char run at least
/// as long closes; a bare bool would mistoggle nested fences.
/// Valid for `rev`; entries at and after the first edited line
/// are dropped on revision change, the prefix is reused, and
/// coverage extends forward on demand.
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct FenceCache {
    pub rev: u64,
    pub starts: Vec<Option<(u8, usize)>>,
}

/// Test-only work counters behind the typing guarantee (see
/// `typing_at_the_end_of_a_big_doc_costs_the_window`): lines
/// fence-stepped and lines highlight-painted. Thread-local so
/// parallel tests never share counts; the measuring test resets
/// first. Compiled out entirely outside test builds.
#[cfg(test)]
thread_local! {
    static FENCE_STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static LINES_HIGHLIGHTED: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn test_reset_work_counts() {
    FENCE_STEPS.with(|c| c.set(0));
    LINES_HIGHLIGHTED.with(|c| c.set(0));
}

#[cfg(test)]
fn test_work_counts() -> (usize, usize) {
    (
        FENCE_STEPS.with(|c| c.get()),
        LINES_HIGHLIGHTED.with(|c| c.get()),
    )
}

/// Cover line starts through `through` (exclusive): on a revision
/// change the cache truncates to `dirty_from` (or zero without a
/// hint) and rescans forward from there. One linear walk, byte
/// scans only on uncovered lines.
pub fn fence_cover(
    text: &str,
    cache: &mut FenceCache,
    rev: u64,
    dirty_from: Option<usize>,
    through: usize,
) {
    if cache.rev != rev {
        cache.starts.truncate(dirty_from.unwrap_or(0));
        cache.rev = rev;
    }
    let mut state = match cache.starts.last() {
        None => None,
        Some(&at_last) => {
            // State after the last covered line: re-step its own
            // delimiter, the only line that could have changed it.
            let last = cache.starts.len() - 1;
            step_fence(at_last, text.split('\n').nth(last).unwrap_or(""))
        }
    };
    for (row, line) in text.split('\n').enumerate().skip(cache.starts.len()) {
        if row >= through {
            break;
        }
        cache.starts.push(state);
        state = step_fence(state, line);
    }
}

/// Step fence state past one line with the same open/close rules
/// the highlighter uses: a same-char run at least as long closes,
/// anything else passes through.
fn step_fence(fence: Option<(u8, usize)>, line: &str) -> Option<(u8, usize)> {
    #[cfg(test)]
    FENCE_STEPS.with(|c| c.set(c.get() + 1));
    match (fence_delim(line), fence) {
        (Some((kind, len)), Some((open_kind, open_len))) if kind == open_kind && len >= open_len => {
            None
        }
        (Some((kind, len)), None) => Some((kind, len)),
        (_, state) => state,
    }
}

/// Highlights for doc rows `[first_row, first_row + row_count)`
/// with a known fence state at the window head (from
/// [`fence_cover`]). Stops at the window end: a paint costs only
/// the visible lines.
#[must_use]
pub fn highlight_window(
    text: &str,
    first_row: usize,
    row_count: usize,
    fence_at_first: Option<(u8, usize)>,
) -> Vec<Highlight> {
    let last_row = first_row.saturating_add(row_count);
    let mut out = Vec::new();
    let mut fence = fence_at_first;
    for (row, line) in text
        .split('\n')
        .enumerate()
        .skip(first_row)
        .take(last_row.saturating_sub(first_row))
    {
        let chars: Vec<char> = line.chars().collect();
        if let Some((kind, len)) = fence_delim(line) {
            match fence {
                // Same fence char and at least as long closes.
                Some((open_kind, open_len)) if kind == open_kind && len >= open_len => {
                    fence = None;
                }
                None => {
                    fence = Some((kind, len));
                }
                // A different fence run inside a fence is content.
                // (The in-fence sentinel never matches: any longer
                // run closes it, exactly like a real fence.)
                Some(_) => {}
            }
            push(&mut out, row, 0, chars.len(), code_bold());
            continue;
        }
        if fence.is_some() {
            push(&mut out, row, 0, chars.len(), code());
            continue;
        }
        highlight_line(row, &chars, &mut out);
    }
    out
}

/// A fence delimiter run: up to 3 leading spaces, then 3+ of one
/// of `` ` `` / `~`. Returns the char and the run length.
fn fence_delim(line: &str) -> Option<(u8, usize)> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }
    if i > 3 || i >= bytes.len() {
        return None;
    }
    let kind = bytes[i];
    if kind != b'`' && kind != b'~' {
        return None;
    }
    let mut len = 0;
    while i + len < bytes.len() && bytes[i + len] == kind {
        len += 1;
    }
    (len >= 3).then_some((kind, len))
}

/// Highlights for doc rows `[first_row, first_row + row_count)`.
/// Uncached entry: fence state scans from the document head (one
/// linear pass), so a window mid-document still knows it is inside
/// a fence; scanning stops at the window end.
#[must_use]
pub fn highlight_markdown(text: &str, first_row: usize, row_count: usize) -> Vec<Highlight> {
    let mut state = None;
    for line in text.split('\n').take(first_row) {
        state = step_fence(state, line);
    }
    highlight_window(text, first_row, row_count, state)
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

/// Block constructs for one unfenced line: the heading owns its
/// whole line, rules and quote/list prefixes are muted spans, and
/// the remainder goes through inline parsing.
fn highlight_line(row: usize, chars: &[char], out: &mut Vec<Highlight>) {
    #[cfg(test)]
    LINES_HIGHLIGHTED.with(|c| c.set(c.get() + 1));
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
    fn fence_cache_prefix_survives_a_late_edit() {
        let text = "```\ncode\nmore\n```\n# Head\n";
        let rows = text.lines().count();
        let mut cache = FenceCache::default();
        fence_cover(text, &mut cache, 0, None, rows);
        let fence = Some((b'`', 3));
        assert_eq!(
            cache.starts,
            vec![None, fence, fence, fence, None]
        );
        // Edit on line 4 (rev 1): the fenced prefix stays cached.
        let mut cache2 = cache.clone();
        fence_cover(text, &mut cache2, 1, Some(4), rows);
        assert_eq!(&cache2.starts[..4], &cache.starts[..4]);
        assert_eq!(cache2.starts, cache.starts);
    }

    #[test]
    fn fence_cover_without_a_hint_rescans_from_zero() {
        let text = "```\ncode\n";
        let mut cache = FenceCache::default();
        fence_cover(text, &mut cache, 0, None, 2);
        fence_cover(text, &mut cache, 1, None, 2);
        assert_eq!(cache.starts, vec![None, Some((b'`', 3))]);
    }

    #[test]
    fn typing_at_the_end_of_a_big_doc_costs_the_window() {
        let para = "# Head **bold** *em* `code` [t](http://x)\n\n- item 1\n\n> quote\n\n```rs\nlet x = 1;\n```\n\n| a | b |\n\n---\n";
        let big: String = para.repeat(1024 * 1024 / para.len() + 1);
        let rows = big.lines().count();
        let mut cache = FenceCache::default();
        fence_cover(&big, &mut cache, 0, None, rows);
        // Type at the end: one more line, new revision. The
        // guarantee is work, not wall clock: fence work and painted
        // lines stay window-bounded no matter the doc size or load.
        let typed = format!("{big}tail\n");
        test_reset_work_counts();
        fence_cover(&typed, &mut cache, 1, Some(rows), rows + 1);
        let state = cache.starts[rows - 30];
        let out = highlight_window(&typed, rows - 30, 30, state);
        let (fence_steps, lines_highlighted) = test_work_counts();
        assert!(!out.is_empty(), "the window highlights");
        assert!(
            fence_steps <= 64,
            "fence rescans only new lines, stepped {fence_steps}"
        );
        assert!(
            lines_highlighted <= 64,
            "paint costs the window, highlighted {lines_highlighted}"
        );
    }

    /// Fastest of three runs: damps one-sided scheduling noise.
    /// Load-robust perf checks compare two sizes in the same run
    /// instead of asserting absolute milliseconds.
    fn min_of_3(work: impl Fn()) -> std::time::Duration {
        let mut best = std::time::Duration::MAX;
        for _ in 0..3 {
            let start = std::time::Instant::now();
            work();
            best = best.min(start.elapsed());
        }
        best
    }

    #[test]
    fn one_mebibyte_doc_stays_within_budget() {
        // Representative prose: headings, emphasis, code, fences,
        // lists, quotes, links, rules, tables.
        let para = "# Head **bold** *em* `code` [t](http://x)\n\n- item 1\n- item 2\n\n> quote\n\n```rs\nlet x = 1;\n```\n\n| a | b |\n\n---\n";
        let small: String = para.repeat(256 * 1024 / para.len() + 1);
        let big: String = para.repeat(1024 * 1024 / para.len() + 1);
        assert!(big.len() >= 1024 * 1024, "fixture is a full MiB");
        let rows = big.lines().count();
        let out = highlight_markdown(&big, 0, rows);
        assert!(!out.is_empty(), "the big doc highlights");
        // Scaling, not wall clock: 1 MiB must cost ~4x a 256 KiB doc
        // measured in the same run, so load cancels out. Catches
        // superlinear blowups; the 2 s ceiling is a catastrophe guard
        // only, not a budget.
        let small_rows = small.lines().count();
        let t_small = min_of_3(|| {
            highlight_markdown(&small, 0, small_rows);
        });
        let t_big = min_of_3(|| {
            highlight_markdown(&big, 0, rows);
        });
        assert!(
            t_big < std::time::Duration::from_secs(2),
            "catastrophe guard: 1 MiB highlights in {t_big:?}"
        );
        let ratio = t_big.as_secs_f64() / t_small.as_secs_f64().max(1e-9);
        assert!(
            ratio <= 6.0,
            "linear scaling: 1 MiB {t_big:?} vs 256 KiB {t_small:?}"
        );
    }
}
