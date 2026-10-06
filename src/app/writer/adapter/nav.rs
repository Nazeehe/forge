//! Adapter motion math: CUA moves on char offsets plus word jumps.
//!
//! EdTUI's own motions cannot serve here (char motions stop at line
//! ends, word jumps stop at word ends, page keys move only the
//! viewport), so the key layer moves the cursor through these and
//! syncs the selection off a stored anchor.

/// One adapter-owned cursor move: the CUA set from the E-phase
/// keymap table. Char moves cross `\n` by offset (EdTUI motions
/// stop at line ends); word jumps go to word starts (EdTUI stops
/// at word ends, vim-style); pages move by the last painted editor
/// height; everything clamps to the document. Home/End stay
/// logical-line by design (screen-row Home/End is an E3 keymap
/// question, not a navigation bug).
#[derive(Clone, Copy)]
pub(super) enum NavMove {
    CharLeft,
    CharRight,
    WordLeft,
    WordRight,
    Up,
    Down,
    LineStart,
    LineEnd,
    DocStart,
    DocEnd,
    PageUp,
    PageDown,
}

/// Word chars for word jumps: letters, digits, underscore.
/// Everything else (spaces, newlines, punctuation) separates.
pub(super) fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Start of the next word run at or after `cursor`, else the doc end.
pub(super) fn word_right(chars: &[char], cursor: usize) -> usize {
    let total = chars.len();
    let mut i = cursor.min(total);
    while i < total && is_word_char(chars[i]) {
        i += 1;
    }
    while i < total && !is_word_char(chars[i]) {
        i += 1;
    }
    i
}

/// Start of the word run before `cursor` (or the run holding it),
/// else 0.
pub(super) fn word_left(chars: &[char], cursor: usize) -> usize {
    let mut i = cursor.min(chars.len());
    while i > 0 && !is_word_char(chars[i - 1]) {
        i -= 1;
    }
    while i > 0 && is_word_char(chars[i - 1]) {
        i -= 1;
    }
    i
}

/// EdTUI's tab stop: its view state default, which we never change.
const WRAP_TAB_WIDTH: usize = 2;

/// Cell width of one char under EdTUI's wrap, mirroring its
/// `LineWrapper` greedy fill (`helper::char_width`).
fn wrap_cell_width(ch: char) -> usize {
    use unicode_width::UnicodeWidthChar;
    if ch == '\t' {
        WRAP_TAB_WIDTH
    } else {
        ch.width().unwrap_or(0)
    }
}

/// Char index where each wrapped chunk of `line` starts, plus no
/// sentinel: the split points of EdTUI's `wrap_line` greedy fill.
/// An empty line is one (empty) chunk.
fn chunk_starts(line: &str, width: usize) -> Vec<usize> {
    let width = width.max(1);
    let mut starts = vec![0usize];
    let mut used = 0usize;
    for (i, ch) in line.chars().enumerate() {
        let cw = wrap_cell_width(ch);
        if used + cw > width {
            starts.push(i);
            used = 0;
        }
        used += cw;
    }
    starts
}

/// Screen rows one doc line occupies at `width` cells: the chunk
/// count of EdTUI's `wrap_line` (empty lines still take one row).
/// Shared with the paint layer's gutter mapping.
pub(crate) fn wrapped_height(line: &str, width: usize) -> usize {
    chunk_starts(line, width).len()
}

/// Which wrapped chunk holds the char at column `col`: the split
/// count EdTUI's `wrap_line` would have emitted before it.
/// Shared with the paint layer's gutter mapping.
pub(crate) fn chunk_of(line: &str, col: usize, width: usize) -> usize {
    chunk_starts(line, width)
        .partition_point(|&s| s <= col)
        .saturating_sub(1)
}

/// Visual column of the char at `col` within its wrapped chunk:
/// the cell width of the chunk's chars before it.
fn visual_col(line: &str, col: usize, width: usize) -> (usize, usize) {
    let starts = chunk_starts(line, width);
    let chunk = starts.partition_point(|&s| s <= col).saturating_sub(1);
    let from = starts[chunk];
    let chars: Vec<char> = line.chars().collect();
    let v = chars[from..col.min(chars.len())]
        .iter()
        .map(|&c| wrap_cell_width(c))
        .sum();
    (chunk, v)
}

/// Target char offset for `motion` from `cursor` in `buffer`
/// (`total` chars), moving `page` rows on page keys.
pub(super) fn nav_target(buffer: &str, total: usize, cursor: usize, motion: NavMove) -> usize {
    let mut starts = vec![0usize];
    for (i, c) in buffer.chars().enumerate() {
        if c == '\n' {
            starts.push(i + 1);
        }
    }
    let cursor = cursor.min(total);
    let row = starts.partition_point(|&s| s <= cursor).saturating_sub(1);
    let line_len = |r: usize| {
        let end = if r + 1 < starts.len() {
            starts[r + 1] - 1
        } else {
            total
        };
        end - starts[r]
    };
    match motion {
        NavMove::CharLeft => cursor.saturating_sub(1),
        NavMove::CharRight => (cursor + 1).min(total),
        NavMove::WordLeft => word_left(&buffer.chars().collect::<Vec<_>>(), cursor),
        NavMove::WordRight => word_right(&buffer.chars().collect::<Vec<_>>(), cursor),
        NavMove::LineStart => starts[row],
        NavMove::LineEnd => starts[row] + line_len(row),
        NavMove::DocStart => 0,
        NavMove::DocEnd => total,
        // Vertical moves go through `vertical_target` (screen rows
        // under wrapping, with a visual-column goal), not here.
        NavMove::Up | NavMove::Down | NavMove::PageUp | NavMove::PageDown => cursor,
    }
}

/// Vertical move by `delta` screen rows (`-1`/`+1`, or `∓page`),
/// following wrapped rows at `width` cells. A width of 0 (never
/// painted) wraps nothing: every line is one screen row. Returns
/// the target offset plus the visual-column goal to keep.
///
/// The goal persists across consecutive vertical moves (a short
/// line clamps but the next long line restores it) and the caller
/// resets it on any horizontal move, edit or click.
pub(super) fn vertical_target(
    buffer: &str,
    total: usize,
    cursor: usize,
    width: usize,
    delta: isize,
    goal: Option<usize>,
) -> (usize, usize) {
    let width = if width == 0 { usize::MAX } else { width };
    let lines: Vec<&str> = buffer.split('\n').collect();
    let mut starts = vec![0usize];
    for line in &lines {
        starts.push(starts.last().expect("nonempty") + line.chars().count() + 1);
    }
    let cursor = cursor.min(total);
    let row = starts.partition_point(|&s| s <= cursor).saturating_sub(1).min(lines.len() - 1);
    let col = cursor - starts[row];
    // Screen-row prefix per doc row.
    let mut prefix = vec![0usize];
    for line in &lines {
        prefix.push(prefix.last().expect("nonempty") + chunk_starts(line, width).len());
    }
    let screen_total = prefix.last().copied().unwrap_or(1).max(1);
    let (chunk, vcol) = visual_col(lines[row], col, width);
    let here = prefix[row] + chunk;
    let goal = goal.unwrap_or(vcol);
    let there = (here as isize + delta).clamp(0, screen_total as isize - 1) as usize;
    // Back to (row, chunk): the doc row holding screen row `there`.
    let row2 = prefix.partition_point(|&p| p <= there).saturating_sub(1).min(lines.len() - 1);
    let k = there - prefix[row2];
    let line = lines[row2];
    let bounds = chunk_starts(line, width);
    let from = bounds[k.min(bounds.len() - 1)];
    let to = bounds.get(k + 1).copied().unwrap_or_else(|| line.chars().count());
    // First char in the chunk whose span passes the goal, else the
    // chunk end (a goal past the text rests at the line end).
    let mut run = 0usize;
    let mut col2 = to;
    for (i, ch) in line.chars().enumerate().skip(from).take(to - from) {
        if run + wrap_cell_width(ch) > goal {
            col2 = i;
            break;
        }
        run += wrap_cell_width(ch);
    }
    (starts[row2] + col2, goal)
}
