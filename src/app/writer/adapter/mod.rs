//! Writer EdTUI adapter: editor ownership, buffer sync, selection,
//! accept/reject, requests, and the delivery queue flush.
//!
//! The domain `Document` is the single source of truth. After every
//! editor event the adapter diffs the buffer against it (common
//! prefix/suffix), applies the change through `doc.apply_edit`, and
//! notifies `proposals.on_edit`. Accept/Reject run through exactly one
//! method each so no caller can skip a step.

use edtui::{clipboard::ClipboardTrait, EditorState, Index2, Lines};

pub mod accept;
pub mod clipboard;
pub mod cua;
pub mod keys;
pub mod nav;
pub mod requests;

/// Most outbound requests awaiting the settle flush; never drops.
pub const MAX_WRITER_QUEUE: usize = 8;

/// Most paths remembered as opened this run (E2b: recent list seed).
pub(super) const MAX_OPENED_THIS_RUN: usize = 32;

/// Forge-owned clipboard backing the editor: Accept saves and restores
/// the user's copied text around the DeleteSelection+InsertChar replace,
/// which EdTUI would otherwise clobber with the drained range.
#[derive(Clone, Default)]
pub struct SharedClipboard(pub std::rc::Rc<std::cell::RefCell<String>>);

/// Open typing undo group (E6): chars typed at `end` within the
/// pause window extend one undo step instead of capturing per char.
/// Any other key, a cursor jump, a class break, or a selection
/// closes it; the next plain char opens a fresh one.
#[derive(Clone, Copy, Debug)]
pub struct TypeGroup {
    /// Char offset just past the group's last char.
    pub end: usize,
    /// When the last group char landed.
    pub at: std::time::Instant,
}

impl ClipboardTrait for SharedClipboard {
    fn set_text(&mut self, text: String) {
        *self.0.borrow_mut() = text;
    }

    fn get_text(&mut self) -> String {
        self.0.borrow().clone()
    }
}
/// The inclusive→exclusive conversion, in one place: EdTUI selections
/// address both ends inclusively, while the domain counts the end
/// exclusive — so start == end still covers exactly one char. A true
/// cursor is `selection = None`, never a zero-width value.
pub fn editor_selection_to_range(editor: &EditorState) -> Option<std::ops::Range<usize>> {
    let selection = editor.selection.as_ref()?;
    let lines = &editor.lines;
    let start = index2_to_offset(lines, selection.start());
    let end = index2_to_offset(lines, selection.end());
    let total = lines.to_string().chars().count();
    if start > end || start >= total {
        return None;
    }
    Some(start..(end + 1).min(total))
}

/// Char offset of the live editor cursor: the paint layer's anchor
/// for gutter marks and the 1-based line:col status.
pub fn editor_cursor_offset(editor: &EditorState) -> usize {
    index2_to_offset(&editor.lines, editor.cursor)
}

/// Char offset of an EdTUI cursor over the buffer lines. Columns past
/// the line end clamp to it; rows past the buffer clamp to the end.
pub(super) fn index2_to_offset(lines: &Lines, index: Index2) -> usize {
    use edtui::RowIndex;
    let mut offset = 0;
    for row in 0..index.row {
        let len = lines.get(RowIndex::new(row)).map(|line| line.len()).unwrap_or(0);
        offset += len + 1;
    }
    let current = lines
        .get(RowIndex::new(index.row))
        .map(|line| line.len())
        .unwrap_or(0);
    offset + index.col.min(current)
}

/// EdTUI cursor for a char offset over plain text. Offsets past the end
/// clamp to the last position.
pub fn offset_to_index2(text: &str, offset: usize) -> Index2 {
    let mut row = 0;
    let mut col = 0;
    for (index, c) in text.chars().enumerate() {
        if index == offset {
            break;
        }
        if c == '\n' {
            row += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    Index2::new(row, col)
}

/// Minimal changed span: common prefix/suffix in chars. The range
/// addresses the old text; the string is the replacement.
pub(super) fn changed_range(old: &str, new: &str) -> Option<(std::ops::Range<usize>, String)> {
    if old == new {
        return None;
    }
    let old_chars: Vec<char> = old.chars().collect();
    let new_chars: Vec<char> = new.chars().collect();
    let mut prefix = 0;
    while prefix < old_chars.len()
        && prefix < new_chars.len()
        && old_chars[prefix] == new_chars[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old_chars.len() - prefix
        && suffix < new_chars.len() - prefix
        && old_chars[old_chars.len() - 1 - suffix] == new_chars[new_chars.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let range = prefix..(old_chars.len() - suffix);
    let replacement: String = new_chars[prefix..(new_chars.len() - suffix)].iter().collect();
    Some((range, replacement))
}
