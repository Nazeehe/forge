//! UI text: span wrapping/truncation and cell-width measurement.

use ratatui::text::Line;

#[cfg(feature = "visual")]
use ratatui::style::Style;
#[cfg(feature = "visual")]
use super::SpanView;

/// Wrap one row of spans to `width` display cells, splitting overlong
/// spans on char boundaries with wide chars counting double. Styles
/// ride along on every piece; empty input yields no rows.
#[cfg(feature = "visual")]
pub fn wrap_spans(spans: Vec<SpanView>, width: u16) -> Vec<Vec<SpanView>> {
    use ratatui::text::Line;
    fn char_width(c: char) -> usize {
        let mut buf = [0u8; 4];
        Line::from(c.encode_utf8(&mut buf) as &str).width()
    }
    let width = width.max(1) as usize;
    let mut rows: Vec<Vec<SpanView>> = Vec::new();
    let mut cur: Vec<SpanView> = Vec::new();
    let mut used = 0usize;
    for span in spans {
        let chars: Vec<char> = span.text.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let room = width.saturating_sub(used);
            if room == 0 {
                rows.push(std::mem::take(&mut cur));
                used = 0;
                continue;
            }
            let mut acc = 0usize;
            let mut j = i;
            while j < chars.len() {
                let cw = char_width(chars[j]);
                if acc + cw > room {
                    break;
                }
                acc += cw;
                j += 1;
            }
            if j == i {
                j = i + 1;
                acc = char_width(chars[i]);
            }
            cur.push(SpanView {
                text: chars[i..j].iter().collect(),
                style: span.style,
            });
            used += acc;
            i = j;
        }
    }
    if !cur.is_empty() {
        rows.push(cur);
    }
    rows
}

/// Display width of one span row in cells, wide chars counting
/// double: the same measure [`wrap_spans`] splits on.
#[cfg(feature = "visual")]
pub fn spans_width(row: &[SpanView]) -> usize {
    use ratatui::text::Line;
    row.iter().map(|s| Line::from(s.text.as_str()).width()).sum()
}

/// Cut one span row to `max` display cells, ending a cut row with an
/// ellipsis that inherits the cut span's style. Fitting rows pass
/// through untouched; wide chars never split. Shortens one-row UI
/// suffixes (like the Visual strip alt text) without touching the
/// buttons and hints ahead of them.
#[cfg(feature = "visual")]
pub fn truncate_spans(spans: Vec<SpanView>, max: u16) -> Vec<SpanView> {
    use ratatui::text::Line;
    fn char_width(c: char) -> usize {
        let mut buf = [0u8; 4];
        Line::from(c.encode_utf8(&mut buf) as &str).width()
    }
    let max = max as usize;
    if max == 0 {
        return Vec::new();
    }
    let total: usize = spans
        .iter()
        .flat_map(|s| s.text.chars())
        .map(char_width)
        .sum();
    if total <= max {
        return spans;
    }
    let mut out: Vec<SpanView> = Vec::new();
    let mut piece = String::new();
    let mut piece_style = Style::default();
    let mut started = false;
    let mut used = 0usize;
    let mut mark_style = Style::default();
    let budget = max.saturating_sub(1);
    'spans: for span in &spans {
        for c in span.text.chars() {
            let cw = char_width(c);
            if used + cw > budget {
                mark_style = span.style;
                break 'spans;
            }
            if !started {
                piece_style = span.style;
                started = true;
            }
            piece.push(c);
            used += cw;
        }
        if started {
            out.push(SpanView { text: std::mem::take(&mut piece), style: piece_style });
            started = false;
        }
    }
    if started {
        out.push(SpanView { text: std::mem::take(&mut piece), style: piece_style });
    }
    out.push(SpanView { text: "…".to_string(), style: mark_style });
    out
}

/// Cut a string to at most `max_cells` display cells without splitting
/// a character. Short strings pass through untouched.
pub fn cut_cells(s: &str, max_cells: usize) -> String {
    if Line::from(s).width() <= max_cells {
        return s.to_string();
    }
    let mut out = String::new();
    for ch in s.chars() {
        let trial = format!("{out}{ch}");
        if Line::from(trial.as_str()).width() > max_cells {
            break;
        }
        out = trial;
    }
    out
}

/// Cell width of a string as the renderer measures it.
pub(super) fn cells(s: &str) -> usize {
    Line::from(s).width()
}

/// Wrap a string to lines of at most `max_cells` display cells,
/// splitting on spaces and hard-splitting words longer than the
/// budget. Never returns an empty vec.
pub fn wrap_cells(s: &str, max_cells: usize) -> Vec<String> {
    let max_cells = max_cells.max(1);
    if s.split(' ').all(|w| w.is_empty()) {
        return Vec::new();
    }
    let mut rows = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0;
    let push_word = |word: &str, rows: &mut Vec<String>, cur: &mut String, cur_w: &mut usize| {
        let w = cells(word);
        if w > max_cells {
            // Hard-split the long word across rows.
            let mut chunk = String::new();
            let mut chunk_w = 0;
            for ch in word.chars() {
                let cw = cells(&ch.to_string());
                if chunk_w + cw > max_cells {
                    rows.push(std::mem::take(&mut chunk));
                    chunk_w = 0;
                }
                chunk.push(ch);
                chunk_w += cw;
            }
            if *cur_w > 0 {
                rows.push(std::mem::take(cur));
                *cur_w = 0;
            }
            *cur = chunk;
            *cur_w = cells(cur);
            return;
        }
        let sep = if *cur_w > 0 { 1 } else { 0 };
        if *cur_w + sep + w > max_cells {
            rows.push(std::mem::take(cur));
            *cur_w = 0;
        } else if sep > 0 {
            cur.push(' ');
            *cur_w += 1;
        }
        cur.push_str(word);
        *cur_w += w;
    };
    for word in s.split(' ') {
        if word.is_empty() {
            continue;
        }
        push_word(word, &mut rows, &mut cur, &mut cur_w);
    }
    if !cur.is_empty() {
        rows.push(cur);
    }
    rows
}

pub(super) fn truncate_cells(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else if max == 0 {
        String::new()
    } else {
        s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    }
}



#[cfg(test)]
mod tests {
    use super::*;


    #[cfg(feature = "visual")]
    #[test]
    fn wrap_spans_splits_by_display_width() {
        use ratatui::style::Style;
        let spans = vec![SpanView { text: "hello world".to_string(), style: Style::default() }];
        let rows = wrap_spans(spans, 5);
        let text: Vec<String> = rows.iter().map(|r| r.iter().map(|s| s.text.as_str()).collect()).collect();
        assert_eq!(text, vec!["hello", " worl", "d"], "hard splits words: {text:?}");
        // Styles ride along, wide chars count double.
        let spans = vec![
            SpanView { text: "ab".to_string(), style: Style::default() },
            SpanView { text: "日本".to_string(), style: Style::default() },
        ];
        let rows = wrap_spans(spans, 4);
        let text: Vec<String> = rows.iter().map(|r| r.iter().map(|s| s.text.as_str()).collect()).collect();
        assert_eq!(text, vec!["ab日", "本"], "width-aware greedy: {text:?}");
        assert!(wrap_spans(Vec::new(), 10).is_empty(), "empty in, empty out");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn truncate_spans_cuts_to_width_with_ellipsis() {
        // The strip must stay exactly one row: fitting spans pass
        // through, overflow cuts at a char boundary with an ellipsis,
        // wide chars never split.
        let spans = vec![
            SpanView { text: "ab".to_string(), style: Style::default() },
            SpanView { text: "cdef".to_string(), style: Style::default() },
        ];
        let same = truncate_spans(spans.clone(), 10);
        assert_eq!(same.len(), 2, "fits, untouched");
        assert!(!same.iter().any(|s| s.text.contains('…')), "no ellipsis");
        let cut = truncate_spans(spans.clone(), 4);
        let text: String = cut.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "abc…", "cut plus marker: {text:?}");
        assert_eq!(spans_width(&cut), 4);
        assert!(truncate_spans(spans.clone(), 0).is_empty(), "zero width");
        let wide = vec![SpanView { text: "日本語".to_string(), style: Style::default() }];
        let cut = truncate_spans(wide, 5);
        let text: String = cut.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "日本…", "no split wide char: {text:?}");
    }

    #[test]
    fn cut_cells_respects_display_width() {
        assert_eq!(cut_cells("abcdef", 4), "abcd");
        assert_eq!(cut_cells("short", 99), "short");
        assert_eq!(cut_cells("🛑🛑🛑", 4), "🛑🛑");
        assert_eq!(cut_cells("a🛑b", 3), "a🛑");
        assert_eq!(cut_cells("a🛑b", 0), "");
    }

    #[test]
    fn wrap_cells_splits_on_words_and_cells() {
        assert_eq!(wrap_cells("aaa bbb ccc", 5), vec!["aaa", "bbb", "ccc"]);
        assert_eq!(wrap_cells("abcdef", 4), vec!["abcd", "ef"]);
        assert_eq!(wrap_cells("🛑🛑 x", 4), vec!["🛑🛑", "x"]);
        assert_eq!(wrap_cells("", 4), Vec::<String>::new());
    }

}
