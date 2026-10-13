//! Form focus cycling: Tab/Down advance, BackTab/Up retreat, both
//! wrapping. Every form dialog hand-rolled the modulo arithmetic;
//! these own the wrap so BackTab can never be miscounted.

/// Advance one row, wrapping past the end.
pub fn focus_next(focus: usize, count: usize) -> usize {
    (focus + 1) % count.max(1)
}

/// Retreat one row, wrapping past the start.
pub fn focus_prev(focus: usize, count: usize) -> usize {
    (focus + count.max(1) - 1) % count.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_wraps_past_the_end() {
        assert_eq!(focus_next(0, 3), 1);
        assert_eq!(focus_next(2, 3), 0);
    }

    #[test]
    fn prev_wraps_past_the_start() {
        assert_eq!(focus_prev(0, 3), 2);
        assert_eq!(focus_prev(1, 3), 0);
    }

    #[test]
    fn single_row_never_moves() {
        assert_eq!(focus_next(0, 1), 0);
        assert_eq!(focus_prev(0, 1), 0);
    }
}
