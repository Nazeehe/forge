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
/// height; everything clamps to the document.
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
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Start of the next word run at or after `cursor`, else the doc end.
fn word_right(chars: &[char], cursor: usize) -> usize {
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
fn word_left(chars: &[char], cursor: usize) -> usize {
    let mut i = cursor.min(chars.len());
    while i > 0 && !is_word_char(chars[i - 1]) {
        i -= 1;
    }
    while i > 0 && is_word_char(chars[i - 1]) {
        i -= 1;
    }
    i
}

/// Target char offset for `motion` from `cursor` in `buffer`
/// (`total` chars), moving `page` rows on page keys.
pub(super) fn nav_target(buffer: &str, total: usize, cursor: usize, page: usize, motion: NavMove) -> usize {
    let mut starts = vec![0usize];
    for (i, c) in buffer.chars().enumerate() {
        if c == '\n' {
            starts.push(i + 1);
        }
    }
    let cursor = cursor.min(total);
    let row = starts.partition_point(|&s| s <= cursor).saturating_sub(1);
    let col = cursor - starts[row];
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
        NavMove::Up => {
            let r = row.saturating_sub(1);
            starts[r] + col.min(line_len(r))
        }
        NavMove::Down => {
            let r = (row + 1).min(starts.len() - 1);
            starts[r] + col.min(line_len(r))
        }
        NavMove::LineStart => starts[row],
        NavMove::LineEnd => starts[row] + line_len(row),
        NavMove::DocStart => 0,
        NavMove::DocEnd => total,
        NavMove::PageUp => {
            let r = row.saturating_sub(page);
            starts[r] + col.min(line_len(r))
        }
        NavMove::PageDown => {
            let r = (row + page).min(starts.len() - 1);
            starts[r] + col.min(line_len(r))
        }
    }
}
