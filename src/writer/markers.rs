//! Writer marker protocol parser (M1): a pure function over the
//! document text. Markers carry char ranges (whole / header / verb /
//! prompt / target); errors carry ranges and reasons. Grammar §3:
//! `@@[verb ]prompt@@target@@` (wrap, `@@end` close accepted),
//! `@@[verb ]prompt @@end` (standalone). `\@@` escapes; markers
//! inside fenced blocks and inline code are immune. Nesting is an
//! opener `@@` where the target should be and more marker text
//! follows (a doubled delimiter); a blank target at the end of the
//! text is EmptyTarget instead. `@@p@@end` is standalone by the
//! grammar (the end check runs at the header close).

use std::ops::Range;

/// One parsed marker. All ranges are char offsets. `header` spans
/// the raw text between the delimiters; `prompt` is the trimmed
/// header minus the verb; `target` is the raw span between the
/// closers (`None` for standalone) — effects trim its edges, keeping
/// internal formatting. `whole` covers the opener through the final
/// closer, including a consumed `end`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Marker {
    pub whole: Range<usize>,
    pub header: Range<usize>,
    pub verb: Option<Range<usize>>,
    pub prompt: Range<usize>,
    pub target: Option<Range<usize>>,
}

/// One skipped span: the parser could not make a marker of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkerError {
    pub range: Range<usize>,
    pub kind: ErrorKind,
}

/// Why a span was skipped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// Opener and header close seen, but the target never closes.
    Unterminated,
    /// Nothing (after trimming) between opener and header close.
    EmptyHeader,
    /// An opener where the target should be: the target so far is
    /// blank and more marker text follows (a doubled delimiter).
    Nesting,
    /// A wrap whose target is blank: the closer sits where the
    /// target should be and nothing but whitespace follows it
    /// ("wrap has no text to apply to").
    EmptyTarget,
    /// A lone `@@` with no partner ahead.
    StrayCloser,
}

/// Parser output: valid markers plus every skipped span.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParseOutput {
    pub markers: Vec<Marker>,
    pub errors: Vec<MarkerError>,
}

/// Known verbs (§3). Matched case-sensitively per `verb =
/// [a-z][a-z0-9-]*[?]?`; anything else ending in `?` is also a
/// verb (usually a question). Unknown first words leave verb empty
/// and the whole header is the prompt.
const KNOWN_VERBS: &[&str] = &[
    "rephrase", "shorten", "expand", "rewrite", "fix", "note", "write", "ask",
];

/// Parse every marker in `text` (§3). Pure: char offsets only, no
/// I/O. Runs of `\@@` are literal; immune spans (fences, inline
/// code) never open, close, or split a marker.
pub fn parse_markers(text: &str) -> ParseOutput {
    let chars: Vec<char> = text.chars().collect();
    let immune = immune_spans(&chars);
    let mut out = ParseOutput::default();
    let mut pos = 0;
    while let Some(o) = next_delim(&chars, &immune, pos) {
        // Header close: the next live `@@` after the opener.
        let Some(h) = next_delim(&chars, &immune, o + 2) else {
            // A lone `@@` with no partner ahead is a stray closer.
            out.errors.push(MarkerError {
                range: o..o + 2,
                kind: ErrorKind::StrayCloser,
            });
            break;
        };
        if is_blank_header(&chars[o + 2..h]) {
            out.errors.push(MarkerError {
                range: o..h + 2,
                kind: ErrorKind::EmptyHeader,
            });
            pos = h + 2;
            continue;
        }
        let (verb, prompt) = split_header(&chars, o + 2, h);
        if is_end_word(&chars, h + 2) {
            // Standalone: the header close is followed by `end`.
            out.markers.push(Marker {
                whole: o..h + 5,
                header: o + 2..h,
                verb,
                prompt,
                target: None,
            });
            pos = h + 5;
            continue;
        }
        let after = next_delim(&chars, &immune, h + 2);
        if after == Some(h + 2) {
            // A `@@` where the target should be: the target is blank.
            // A `@@end` close still accepts it (an empty-target wrap);
            // trailing off into whitespace ends the text with nothing
            // to apply to; anything else is a doubled delimiter.
            if is_end_word(&chars, h + 4) {
                out.markers.push(Marker {
                    whole: o..h + 7,
                    header: o + 2..h,
                    verb,
                    prompt,
                    target: Some(h + 2..h + 2),
                });
                pos = h + 7;
            } else if chars[h + 4..].iter().all(|c| c.is_whitespace()) {
                out.errors.push(MarkerError {
                    range: o..h + 4,
                    kind: ErrorKind::EmptyTarget,
                });
                pos = h + 4;
            } else {
                out.errors.push(MarkerError {
                    range: o..h + 4,
                    kind: ErrorKind::Nesting,
                });
                pos = h + 4;
            }
            continue;
        }
        let Some(t) = after else {
            // Opener and header close seen, but the target never closes.
            out.errors.push(MarkerError {
                range: o..chars.len(),
                kind: ErrorKind::Unterminated,
            });
            break;
        };
        let whole_end = if is_end_word(&chars, t + 2) {
            t + 5
        } else {
            t + 2
        };
        out.markers.push(Marker {
            whole: o..whole_end,
            header: o + 2..h,
            verb,
            prompt,
            target: Some(h + 2..t),
        });
        pos = whole_end;
    }
    out
}

/// Find the next live `@@` opener/closer at or after `from`: both
/// `@`, neither immune, and not `\`-escaped (an odd run of
/// immediately preceding backslashes escapes it).
fn next_delim(chars: &[char], immune: &[bool], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 1 < chars.len() {
        if chars[i] == '@' && chars[i + 1] == '@' && !immune[i] && !is_escaped(chars, i) {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// True when `chars[at]` (the first `@` of a pair) is escaped.
fn is_escaped(chars: &[char], at: usize) -> bool {
    let mut backslashes = 0;
    let mut i = at;
    while i > 0 && chars[i - 1] == '\\' {
        backslashes += 1;
        i -= 1;
    }
    backslashes % 2 == 1
}

/// `end` at `at` with a word boundary after it (or end of text).
fn is_end_word(chars: &[char], at: usize) -> bool {
    if at + 3 > chars.len() {
        return false;
    }
    if chars[at] != 'e' || chars[at + 1] != 'n' || chars[at + 2] != 'd' {
        return false;
    }
    at + 3 == chars.len() || !(chars[at + 3].is_alphanumeric() || chars[at + 3] == '_')
}

/// A header with no content: only whitespace and `:` separators.
fn is_blank_header(header: &[char]) -> bool {
    !header.iter().any(|c| !c.is_whitespace() && *c != ':')
}

/// Split a header span into the verb range (if the first word is a
/// known verb or ends in `?`) and the trimmed prompt range.
fn split_header(chars: &[char], start: usize, end: usize) -> (Option<Range<usize>>, Range<usize>) {
    let mut wstart = start;
    while wstart < end && chars[wstart].is_whitespace() {
        wstart += 1;
    }
    let mut wend = wstart;
    while wend < end && !chars[wend].is_whitespace() && chars[wend] != ':' {
        wend += 1;
    }
    let word: String = chars[wstart..wend].iter().collect();
    let is_verb = !word.is_empty() && (KNOWN_VERBS.contains(&word.as_str()) || word.ends_with('?'));
    let mut prompt_end = end;
    while prompt_end > start && chars[prompt_end - 1].is_whitespace() {
        prompt_end -= 1;
    }
    if !is_verb {
        let mut prompt_start = start;
        while prompt_start < prompt_end && chars[prompt_start].is_whitespace() {
            prompt_start += 1;
        }
        return (None, prompt_start..prompt_end);
    }
    let mut prompt_start = wend;
    while prompt_start < prompt_end
        && (chars[prompt_start].is_whitespace() || chars[prompt_start] == ':')
    {
        prompt_start += 1;
    }
    (Some(wstart..wend), prompt_start..prompt_end)
}

/// Char positions that can never be a delimiter: fenced code block
/// lines (an unclosed fence runs to end of text) and inline code
/// spans (equal-length backtick pairs per line; an unclosed backtick
/// is literal text).
fn immune_spans(chars: &[char]) -> Vec<bool> {
    let mut immune = vec![false; chars.len()];
    // Line starts (char index of the first char of each line).
    let mut starts = vec![0];
    for (i, c) in chars.iter().enumerate() {
        if *c == '\n' && i + 1 < chars.len() {
            starts.push(i + 1);
        }
    }
    starts.push(chars.len());
    let mut fence: Option<(char, usize)> = None;
    for w in starts.windows(2) {
        let (ls, le) = (w[0], w[1]);
        if fence.is_none() {
            mark_inline_spans(chars, &mut immune, ls, le);
        }
        let trimmed: String = chars[ls..le].iter().collect();
        let stripped = trimmed.trim_start();
        let (mark, run) = if let Some(rest) = stripped.strip_prefix("```") {
            ('`', 3 + rest.chars().take_while(|c| *c == '`').count())
        } else if let Some(rest) = stripped.strip_prefix("~~~") {
            ('~', 3 + rest.chars().take_while(|c| *c == '~').count())
        } else {
            continue;
        };
        match fence {
            Some((c, open_len)) if c == mark && run >= open_len => fence = None,
            None => fence = Some((mark, run)),
            _ => {}
        }
        // The fence marker lines themselves are immune too; content
        // lines are marked in the second pass below.
        for i in ls..le {
            immune[i] = true;
        }
    }
    // Second pass: mark fenced content lines immune (kept separate so
    // fence open/close detection above sees clean state).
    let mut fence: Option<(char, usize)> = None;
    for w in starts.windows(2) {
        let (ls, le) = (w[0], w[1]);
        let trimmed: String = chars[ls..le].iter().collect();
        let stripped = trimmed.trim_start();
        let fence_mark = if stripped.starts_with("```") {
            Some(('`', stripped.chars().take_while(|c| *c == '`').count()))
        } else if stripped.starts_with("~~~") {
            Some(('~', stripped.chars().take_while(|c| *c == '~').count()))
        } else {
            None
        };
        if fence.is_some() {
            for i in ls..le {
                immune[i] = true;
            }
        }
        if let Some((mark, run)) = fence_mark {
            match fence {
                Some((c, open_len)) if c == mark && run >= open_len => fence = None,
                None => fence = Some((mark, run)),
                _ => {}
            }
        }
    }
    immune
}

/// Mark equal-length backtick pairs on one line (`ls..le`) immune.
fn mark_inline_spans(chars: &[char], immune: &mut [bool], ls: usize, le: usize) {
    // Collect backtick runs on this line.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = ls;
    while i < le {
        if chars[i] == '`' {
            let mut j = i;
            while j < le && chars[j] == '`' {
                j += 1;
            }
            runs.push((i, j - i));
            i = j;
        } else {
            i += 1;
        }
    }
    let mut used = vec![false; runs.len()];
    for a in 0..runs.len() {
        if used[a] {
            continue;
        }
        let mut closer = None;
        for b in a + 1..runs.len() {
            if !used[b] && runs[b].1 == runs[a].1 {
                closer = Some(b);
                break;
            }
        }
        if let Some(b) = closer {
            used[a] = true;
            used[b] = true;
            for k in runs[a].0..runs[b].0 + runs[b].1 {
                immune[k] = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Table row: (whole, header, verb, prompt, target).
    type M = (
        Range<usize>,
        Range<usize>,
        Option<Range<usize>>,
        Range<usize>,
        Option<Range<usize>>,
    );
    struct Case {
        name: &'static str,
        input: &'static str,
        markers: Vec<M>,
        errors: Vec<(Range<usize>, ErrorKind)>,
    }

    fn case(
        name: &'static str,
        input: &'static str,
        markers: Vec<M>,
        errors: Vec<(Range<usize>, ErrorKind)>,
    ) -> Case {
        Case {
            name,
            input,
            markers,
            errors,
        }
    }

    fn cases() -> Vec<Case> {
        use ErrorKind as E;
        vec![
            case(
                "wrap with verb",
                "@@fix typo@@this is teh@@",
                vec![(0..25, 2..10, Some(2..5), 6..10, Some(12..23))],
                vec![],
            ),
            case(
                "standalone with verb",
                "@@note remember milk @@end",
                vec![(0..26, 2..21, Some(2..6), 7..20, None)],
                vec![],
            ),
            case(
                "wrap @@end close",
                "@@shorten this@@too long text@@end",
                vec![(0..34, 2..14, Some(2..9), 10..14, Some(16..29))],
                vec![],
            ),
            case(
                "inline single line",
                "a @@fix x@@y@@ b",
                vec![(2..14, 4..9, Some(4..7), 8..9, Some(11..12))],
                vec![],
            ),
            case(
                "multi-paragraph target",
                "@@expand@@line1\n\nline2@@",
                vec![(0..24, 2..8, Some(2..8), 8..8, Some(10..22))],
                vec![],
            ),
            case(
                "no verb: whole header is the prompt",
                "@@make it better@@text@@",
                vec![(0..24, 2..16, None, 2..16, Some(18..22))],
                vec![],
            ),
            case(
                "ask verb with prompt",
                "@@ask about this@@long text here@@",
                vec![(0..34, 2..16, Some(2..5), 6..16, Some(18..32))],
                vec![],
            ),
            case(
                "question-mark verb",
                "@@rewrite?@@t@@",
                vec![(0..15, 2..10, Some(2..10), 10..10, Some(12..13))],
                vec![],
            ),
            case(
                "question-mark verb with prompt",
                "@@shorter? make it brief@@text here@@",
                vec![(0..37, 2..24, Some(2..10), 11..24, Some(26..35))],
                vec![],
            ),
            case(
                "verb with colon",
                "@@fix: teh typo@@x@@",
                vec![(0..20, 2..15, Some(2..5), 7..15, Some(17..18))],
                vec![],
            ),
            case(
                "capitalized word is not a verb",
                "@@Fix@@x@@",
                vec![(0..10, 2..5, None, 2..5, Some(7..8))],
                vec![],
            ),
            case(
                "escaped delimiter in header",
                "@@fix a \\@@ b@@target@@",
                vec![(0..23, 2..13, Some(2..5), 6..13, Some(15..21))],
                vec![],
            ),
            case(
                "escaped opener is literal",
                "\\@@fix@@x@@",
                vec![],
                vec![(6..11, E::Unterminated)],
            ),
            case(
                "fence immunity",
                "```\n@@fix@@x@@\n```\n@@go@@y@@",
                vec![(19..28, 21..23, None, 21..23, Some(25..26))],
                vec![],
            ),
            case(
                "tilde fence immunity",
                "~~~\n@@a@@b@@\n~~~\n",
                vec![],
                vec![],
            ),
            case(
                "unclosed fence stays immune",
                "```\n@@a@@b@@\n",
                vec![],
                vec![],
            ),
            case(
                "inline code immunity",
                "`@@a@@` @@b@@c@@",
                vec![(8..16, 10..11, None, 10..11, Some(13..14))],
                vec![],
            ),
            case(
                "unclosed backtick is literal text",
                "`code @@a@@b@@",
                vec![(6..14, 8..9, None, 8..9, Some(11..12))],
                vec![],
            ),
            case(
                "unterminated target",
                "@@fix@@target with no close",
                vec![],
                vec![(0..27, E::Unterminated)],
            ),
            case(
                "lone opener is a stray closer",
                "@@prompt",
                vec![],
                vec![(0..2, E::StrayCloser)],
            ),
            case(
                "empty header plus leftover stray",
                "@@@@x@@",
                vec![],
                vec![(0..4, E::EmptyHeader), (5..7, E::StrayCloser)],
            ),
            case(
                "nesting: opener where the target goes",
                "@@fix@@@@target@@",
                vec![],
                vec![(0..9, E::Nesting), (15..17, E::StrayCloser)],
            ),
            case(
                "stray closer in prose",
                "text @@ more",
                vec![],
                vec![(5..7, E::StrayCloser)],
            ),
            case(
                "header then end is standalone, not wrap",
                "@@fix it@@end",
                vec![(0..13, 2..8, Some(2..5), 6..8, None)],
                vec![],
            ),
            case(
                "end needs a word boundary",
                "@@p@@t@@endless",
                vec![(0..8, 2..3, None, 2..3, Some(5..6))],
                vec![],
            ),
            case(
                "edge whitespace trimmed for prompt",
                "@@  fix it  @@  target  @@",
                vec![(0..26, 2..12, Some(4..7), 8..10, Some(14..24))],
                vec![],
            ),
            case(
                "ask question standalone",
                "@@ask capital of France @@end",
                vec![(0..29, 2..24, Some(2..5), 6..23, None)],
                vec![],
            ),
            case(
                "char ranges over multibyte text",
                "@@fix@@héllo wörld@@",
                vec![(0..20, 2..5, Some(2..5), 5..5, Some(7..18))],
                vec![],
            ),
            case(
                "two markers in one line",
                "@@a@@1@@ and @@b@@2@@",
                vec![
                    (0..8, 2..3, None, 2..3, Some(5..6)),
                    (13..21, 15..16, None, 15..16, Some(18..19)),
                ],
                vec![],
            ),
            case(
                "K1 empty-target wrap close",
                "@@fix@@end",
                // Grammar first: header-close followed by `end` is
                // standalone. A wrap @@end close needs a target.
                vec![(0..10, 2..5, Some(2..5), 5..5, None)],
                vec![],
            ),
            case(
                "blank wrap target is its own error",
                "@@p@@@@",
                vec![],
                vec![(0..7, E::EmptyTarget)],
            ),
            case(
                "blank wrap target with trailing whitespace",
                "@@p@@@@ \n",
                vec![],
                vec![(0..7, E::EmptyTarget)],
            ),
        ]
    }

    #[test]
    fn table_covers_the_grammar() {
        for c in cases() {
            let out = parse_markers(c.input);
            let markers: Vec<M> = out
                .markers
                .iter()
                .map(|m| {
                    (
                        m.whole.clone(),
                        m.header.clone(),
                        m.verb.clone(),
                        m.prompt.clone(),
                        m.target.clone(),
                    )
                })
                .collect();
            let errors: Vec<(Range<usize>, ErrorKind)> = out
                .errors
                .iter()
                .map(|e| (e.range.clone(), e.kind))
                .collect();
            assert_eq!(markers, c.markers, "markers: {}", c.name);
            assert_eq!(errors, c.errors, "errors: {}", c.name);
        }
    }

    #[test]
    fn prompt_slices_match() {
        // The ranges are usable directly: slicing the input at the
        // prompt range yields the prompt text.
        for c in cases() {
            let chars: Vec<char> = c.input.chars().collect();
            let out = parse_markers(c.input);
            for m in &out.markers {
                let prompt: String = chars[m.prompt.clone()].iter().collect();
                assert!(!prompt.starts_with([' ', '\n']), "{}: {}", c.name, prompt);
                assert!(
                    !prompt.ends_with([' ', '\n']) || prompt.is_empty(),
                    "{}",
                    c.name
                );
            }
        }
    }

    #[test]
    fn hostile_inputs_do_not_panic() {
        // The parser runs on live buffer text; degenerate inputs must
        // complete (markers/errors unchecked here, covered above).
        for input in [
            "",
            "@",
            "@@",
            "@@@",
            "@@@@",
            "\\",
            "\\@",
            "\\\\@@",
            "`",
            "``",
            "```",
            "~~",
            "@@ @@",
            "@@ : @@",
            "@@a@@",
            "@@a@@end",
            "@@a@@@@end",
            "@@?@@x@@",
            "@@Fix?@@x@@",
            "  ```\n@@a@@b@@",
            "@@a@@`@@`b@@",
            "\n\n\n",
            "@@\n@@\n@@",
            "end",
            "@@end",
            "@@end@@",
            "\u{00e9}@@\u{00e9}@@\u{00e9}",
        ] {
            let _ = parse_markers(input);
        }
    }

    #[test]
    fn two_mebibyte_doc_parses_scales_linearly() {
        // Representative prose with fences, inline code, and a marker
        // every few paragraphs (plus hostile almost-markers).
        let para = "# Head @@fix typo@@this is teh\n\nsome `code @@x@@` and\n\n```\n@@f@@t@@\n```\n\ntail @@note hi @@end after\n\nstray @@ here\n\n";
        let small: String = para.repeat(512 * 1024 / para.len() + 1);
        let big: String = para.repeat(2 * 1024 * 1024 / para.len() + 1);
        assert!(big.len() >= 2 * 1024 * 1024, "fixture is 2 MiB");
        let out = parse_markers(&big);
        assert!(!out.markers.is_empty(), "markers found");
        assert!(!out.errors.is_empty(), "strays found");
        // Scaling, not wall clock: shared interleaved helper, 4x size
        // ratio, both samples far above scheduler jitter.
        crate::infra::test_timing::assert_scales_linearly(
            "markers 2 MiB",
            || {
                parse_markers(&small);
            },
            || {
                parse_markers(&big);
            },
        );
    }
}
