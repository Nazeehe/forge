//! Writer preview: read-only rendered Markdown (E8) with a Forge
//! stylesheet. Every tui-markdown style maps to a semantic role,
//! so preview follows the terminal palette in light and dark
//! setups; heading markers are omitted (headings read as headings,
//! not source).

use ratatui::style::Modifier;
use ratatui::text::Text;

use crate::ui::theme::{Role, style};

/// tui-markdown stylesheet in Forge roles. Unit struct: the trait
/// needs `Clone + Send + Sync + 'static`.
#[derive(Clone, Copy, Debug, Default)]
pub struct ForgeStyleSheet;

impl tui_markdown::StyleSheet for ForgeStyleSheet {
    fn heading(&self, level: u8) -> ratatui::style::Style {
        if level <= 1 {
            style(Role::MdHeading).add_modifier(Modifier::UNDERLINED)
        } else {
            style(Role::MdHeading)
        }
    }

    fn heading_marker(&self, _level: u8) -> &str {
        ""
    }

    fn code(&self) -> ratatui::style::Style {
        style(Role::MdCode)
    }

    fn link(&self) -> ratatui::style::Style {
        style(Role::MdLink)
    }

    fn blockquote(&self) -> ratatui::style::Style {
        style(Role::Muted)
    }

    fn heading_meta(&self) -> ratatui::style::Style {
        style(Role::Muted)
    }

    fn metadata_block(&self) -> ratatui::style::Style {
        style(Role::Muted)
    }

    fn html(&self) -> ratatui::style::Style {
        style(Role::Muted)
    }

    fn math_inline(&self) -> ratatui::style::Style {
        style(Role::MdCode).add_modifier(Modifier::ITALIC)
    }

    fn math_display(&self) -> ratatui::style::Style {
        style(Role::MdCode)
    }

    fn footnote_ref(&self) -> ratatui::style::Style {
        style(Role::Muted).add_modifier(Modifier::ITALIC)
    }

    fn footnote_def(&self) -> ratatui::style::Style {
        style(Role::Muted)
    }

    fn definition_term(&self) -> ratatui::style::Style {
        style(Role::Text).add_modifier(Modifier::BOLD)
    }

    fn alert(&self, kind: tui_markdown::AlertKind) -> ratatui::style::Style {
        use tui_markdown::AlertKind as K;
        match kind {
            K::Note => style(Role::Info),
            K::Tip => style(Role::Success),
            K::Important => style(Role::Brand),
            K::Warning => style(Role::Warning),
            K::Caution => style(Role::Danger),
        }
    }

    fn table_header(&self) -> ratatui::style::Style {
        style(Role::Text).add_modifier(Modifier::BOLD)
    }

    fn table_border(&self) -> ratatui::style::Style {
        style(Role::Muted)
    }

    fn image_alt(&self) -> ratatui::style::Style {
        style(Role::Muted).add_modifier(Modifier::ITALIC)
    }
}

/// Render `text` to wrapped-ready lines in Forge roles. Owned:
/// the marker-hiding pre-pass allocates, so the render cannot
/// borrow the input. Standalone labels mute through the post-pass.
pub fn preview_text(text: &str) -> Text<'static> {
    use ratatui::text::{Line, Span};
    use std::borrow::Cow;
    let options = tui_markdown::Options::new(ForgeStyleSheet);
    let source = preview_source(text);
    let rendered = tui_markdown::from_str_with_options(&source, &options);
    let mut owned = Text {
        lines: rendered
            .lines
            .into_iter()
            .map(|line| Line {
                spans: line
                    .spans
                    .into_iter()
                    .map(|span| Span {
                        content: Cow::Owned(span.content.into_owned()),
                        style: span.style,
                    })
                    .collect(),
                style: line.style,
                alignment: line.alignment,
            })
            .collect(),
        style: rendered.style,
        alignment: rendered.alignment,
    };
    for label in preview_labels(text) {
        mute_label(&mut owned, &label);
    }
    owned
}

/// Source transform behind §5.2: hide the delimiters and headers
/// before the Markdown render. Wrap markers (ask included) show
/// their target text; standalone markers become a plain `[verb:
/// prompt]` label (missing verb: `[prompt]`), muted by a styling
/// post-pass in [`preview_text`]; parse errors stay raw. Plain
/// brackets never lose text; when `(` follows the marker the join
/// is backslash-escaped so no link forms. Pure over char ranges:
/// immune spans never contribute markers, so fenced text passes
/// through untouched.
pub fn preview_source(text: &str) -> String {
    use crate::writer::markers::parse_markers;
    let out = parse_markers(text);
    if out.markers.is_empty() {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let slice = |range: std::ops::Range<usize>| -> String {
        chars.get(range).unwrap_or(&[]).iter().collect()
    };
    let mut source = String::with_capacity(text.len());
    let mut pos = 0;
    for marker in &out.markers {
        source.extend(chars[pos..marker.whole.start].iter());
        match &marker.target {
            Some(target) => source.push_str(&slice(target.clone())),
            None => {
                source.push_str(&format!("[{}]", standalone_label(&chars, marker)));
                // No link may form across the join.
                if chars.get(marker.whole.end) == Some(&'(') {
                    source.push('\\');
                }
            }
        }
        pos = marker.whole.end;
    }
    source.extend(chars[pos..].iter());
    source
}

/// The `[verb: prompt]` label (`[prompt]` without a verb) for a
/// standalone marker's slices.
fn standalone_label(chars: &[char], marker: &crate::writer::markers::Marker) -> String {
    let slice = |range: std::ops::Range<usize>| -> String {
        chars.get(range).unwrap_or(&[]).iter().collect()
    };
    match marker.verb.clone().map(|v| slice(v)) {
        Some(verb) => format!("{verb}: {}", slice(marker.prompt.clone())),
        None => slice(marker.prompt.clone()),
    }
}

/// Every standalone label in `text`, brackets included, in parse
/// order: the styling post-pass mutes exactly these (never the
/// user's own brackets).
fn preview_labels(text: &str) -> Vec<String> {
    use crate::writer::markers::parse_markers;
    let chars: Vec<char> = text.chars().collect();
    parse_markers(text)
        .markers
        .iter()
        .filter(|m| m.target.is_none())
        .map(|m| format!("[{}]", standalone_label(&chars, m)))
        .collect()
}

/// Mute every occurrence of `label` in the rendered text: flatten
/// spans, split the boundary spans, and paint the label range with
/// the Muted role. A label split across spans (its prompt holds
/// Markdown syntax) mutes as one range.
fn mute_label(rendered: &mut Text<'static>, label: &str) {
    use ratatui::text::Span;
    use std::borrow::Cow;
    if label.is_empty() {
        return;
    }
    let muted = style(Role::Muted);
    // Flat char offsets per span, lines joined by `\n` (labels from
    // the fallback path may hold newlines).
    let mut flat = String::new();
    let mut bounds: Vec<(usize, usize, usize)> = Vec::new();
    for (line_idx, line) in rendered.lines.iter().enumerate() {
        if line_idx > 0 {
            flat.push('\n');
        }
        for (span_idx, span) in line.spans.iter().enumerate() {
            let start = flat.chars().count();
            flat.push_str(&span.content);
            bounds.push((line_idx, span_idx, start));
        }
    }
    let total = flat.chars().count();
    let mut hits: Vec<(usize, usize)> = Vec::new();
    let mut from = 0;
    while from + label.chars().count() <= total {
        let rest: String = flat.chars().skip(from).collect();
        let Some(at) = rest.find(label) else { break };
        let start = from + rest[..at].chars().count();
        hits.push((start, start + label.chars().count()));
        from = start + label.chars().count().max(1);
    }
    if hits.is_empty() {
        return;
    }
    // Span char ranges for splitting, computed up front (the
    // rebuild below mutates the lines). `bounds` and the nested
    // loops both run line-major, so the orders agree.
    let ranges: Vec<(usize, usize)> = bounds
        .iter()
        .map(|(line_idx, span_idx, start)| {
            let len = rendered.lines[*line_idx].spans[*span_idx]
                .content
                .chars()
                .count();
            (*start, *start + len)
        })
        .collect();
    let mut at = ranges.iter();
    for line_idx in 0..rendered.lines.len() {
        let mut rebuilt = Vec::new();
        for span_idx in 0..rendered.lines[line_idx].spans.len() {
            let span = rendered.lines[line_idx].spans[span_idx].clone();
            let (start, end) = *at.next().unwrap_or(&(0, 0));
            // Cut points inside this span from hit boundaries.
            let mut cuts = vec![start, end];
            for (hit_start, hit_end) in &hits {
                for edge in [*hit_start, *hit_end] {
                    if edge > start && edge < end {
                        cuts.push(edge);
                    }
                }
            }
            cuts.sort();
            cuts.dedup();
            let text: String = span.content.chars().collect();
            for window in cuts.windows(2) {
                let (a, b) = (window[0] - start, window[1] - start);
                let piece: String = text.chars().skip(a).take(b - a).collect();
                let in_hit = hits.iter().any(|(hs, he)| a + start >= *hs && b + start <= *he);
                rebuilt.push(Span {
                    content: Cow::Owned(piece),
                    style: if in_hit { muted } else { span.style },
                });
            }
        }
        // Merge same-style runs so a label the render split
        // across spans reads as one muted span.
        let mut merged: Vec<Span<'static>> = Vec::with_capacity(rebuilt.len());
        for span in rebuilt {
            let same = merged.last().is_some_and(|last| last.style == span.style);
            if same {
                if let Some(last) = merged.last_mut() {
                    let mut text = std::mem::take(&mut last.content).into_owned();
                    text.push_str(&span.content);
                    last.content = Cow::Owned(text);
                }
            } else {
                merged.push(span);
            }
        }
        rendered.lines[line_idx].spans = merged;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_markers_show_the_target_text() {
        assert_eq!(
            preview_source("alpha @@fix typo@@this is teh@@ gamma\n"),
            "alpha this is teh gamma\n"
        );
        assert_eq!(
            preview_source("@@ask capital@@Paris@@\n"),
            "Paris\n",
            "ask wraps show the target too"
        );
    }

    #[test]
    fn standalone_markers_become_muted_labels() {
        assert_eq!(
            preview_source("before @@fix typo@@end after\n"),
            "before [fix: typo] after\n"
        );
        assert_eq!(
            preview_source("@@lunch plans@@end\n"),
            "[lunch plans]\n",
            "missing verb: prompt only"
        );
        assert_eq!(
            preview_source("@@ask capital@@end(see below)\n"),
            "[ask: capital]\\(see below)\n",
            "no link forms across the join"
        );
    }

    #[test]
    fn errors_and_code_show_raw() {
        assert_eq!(
            preview_source("@@fix typo\n"),
            "@@fix typo\n",
            "unterminated stays raw"
        );
        assert_eq!(
            preview_source("```\n@@fix x@@y@@\n```\n"),
            "```\n@@fix x@@y@@\n```\n",
            "fenced markers stay raw"
        );
    }

    #[test]
    fn hostile_labels_stay_readable() {
        assert_eq!(
            preview_source("@@a [b]@@end\n"),
            "[a [b]]\n",
            "brackets pass through: text preserved"
        );
        let long = "p".repeat(1500);
        let text = format!("@@{long}@@end\n");
        assert_eq!(
            preview_source(&text),
            format!("[{long}]\n"),
            "very long labels stay readable"
        );
    }

    #[test]
    fn rendered_label_paints_muted() {
        let rendered = preview_text("before @@fix typo@@end after\n");
        let flat: String = rendered
            .lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(flat.contains("[fix: typo]"), "label renders: {flat:?}");
        let muted = rendered.lines.iter().flat_map(|l| l.spans.iter()).any(|s| {
            s.content.contains("[fix: typo]") && s.style == style(Role::Muted)
        });
        assert!(muted, "label paints muted");
        // The user's own brackets never mute.
        let rendered = preview_text("see [fix: typo] here\n");
        let muted = rendered.lines.iter().flat_map(|l| l.spans.iter()).any(|s| {
            s.content.contains("[fix: typo]") && s.style == style(Role::Muted)
        });
        assert!(!muted, "user brackets keep their style");
    }
}

/// Visual row count of the rendered preview at `width` cells:
/// each line wraps greedily, empty lines take one row. Scroll
/// clamps to this, so the tail stays reachable.
pub fn preview_height(text: &str, width: usize) -> usize {
    use unicode_width::UnicodeWidthStr;
    let width = width.max(1);
    let rendered = preview_text(text);
    rendered
        .lines
        .iter()
        .map(|line| {
            let cells: usize = line.spans.iter().map(|span| span.content.width()).sum();
            cells.div_ceil(width).max(1)
        })
        .sum::<usize>()
        .max(1)
}
