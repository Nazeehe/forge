//! UI layout: chrome areas, pane geometry, grid tiling, mouse mapping.

use ratatui::layout::{Position, Rect};

/// Translate outer 0-based mouse coordinates into 1-based pane-grid cells.
/// `None` when the event lands on borders, the session bar, or outside
/// the pane: chrome keeps those events.
pub fn translate_mouse(area: Rect, col: u16, row: u16) -> Option<(u16, u16)> {
    if area.width < 3 || area.height < 3 {
        return None;
    }
    let (ix, iy) = (area.x + 1, area.y + 1);
    let (iw, ih) = (area.width - 2, area.height - 2);
    if col < ix || row < iy || col >= ix + iw || row >= iy + ih {
        return None;
    }
    Some((col - ix + 1, row - iy + 1))
}

/// Translate a terminal-grid cursor into outer-frame coordinates, clamped
/// inside the pane's borders. `None` when the pane hides its cursor or the
/// pane is too small for an inner area.
pub fn cursor_screen_pos(area: Rect, cursor: Option<(u16, u16)>) -> Option<Position> {
    let (row, col) = cursor?;
    if area.width < 3 || area.height < 3 {
        return None;
    }
    let x = area
        .x
        .saturating_add(1)
        .saturating_add(col)
        .min(area.x + area.width - 2);
    let y = area
        .y
        .saturating_add(1)
        .saturating_add(row)
        .min(area.y + area.height - 2);
    Some(Position::new(x, y))
}

/// Chrome geometry: one focused session fills the 80% main pane (with a
/// one-row tab strip pinned to its top), the sidebar keeps 20%, and one
/// bottom row holds the session bar. There is no status bar: dialogs
/// carry their own key hints. Tiny terminals sacrifice chrome for
/// content.
pub struct ChromeAreas {
    pub main: Rect,
    pub topbar: Rect,
    pub sidebar: Rect,
    pub session_bar: Rect,
}

pub fn chrome_areas(area: Rect) -> ChromeAreas {
    let bar_h = if area.height >= 3 { 1 } else { 0 };
    let content_h = area.height.saturating_sub(bar_h);
    let topbar_h = if content_h > 4 { 1 } else { 0 };
    let raw_main = if area.width >= 160 { area.width * 3 / 4 } else { area.width * 4 / 5 };
    // Wide terminals stop donating a full percentage to the sidebar:
    // it caps around 36 columns and the main pane keeps the rest, so
    // 159→160 no longer steals seven columns from the main pane.
    let raw_side = area.width.saturating_sub(raw_main);
    let sidebar_w = raw_side.min(36);
    let main_w = area.width.saturating_sub(sidebar_w);
    let inset_tabs = area.width >= 160 && topbar_h > 0;
    ChromeAreas {
        main: if inset_tabs {
            Rect::new(area.x, area.y, main_w, content_h)
        } else {
            Rect::new(area.x, area.y + topbar_h, main_w, content_h.saturating_sub(topbar_h))
        },
        topbar: Rect::new(area.x, area.y + inset_tabs as u16, main_w, topbar_h),
        sidebar: Rect::new(area.x + main_w, area.y, area.width.saturating_sub(main_w), content_h),
        session_bar: Rect::new(area.x, area.y + content_h, area.width, bar_h),
    }
}

/// For wide layouts the tab strip occupies the first inner pane row;
/// mouse/cursor/PTY sizing begin one row below it.
pub fn pane_grid_area(areas: &ChromeAreas) -> Rect {
    if areas.topbar.height > 0 && areas.topbar.y > areas.main.y {
        Rect::new(areas.main.x, areas.main.y + 1, areas.main.width,
            areas.main.height.saturating_sub(1))
    } else {
        areas.main
    }
}

/// Content cells inside a framed single-pane tab: the pane-grid rect
/// minus the block border, where overlay tab lines actually paint.
/// Matches `cursor_screen_pos`, whose inner origin is also area + 1.
#[cfg(feature = "visual")]
pub fn pane_content_area(areas: &ChromeAreas) -> Rect {
    let grid = pane_grid_area(areas);
    Rect::new(
        grid.x.saturating_add(1),
        grid.y.saturating_add(1),
        grid.width.saturating_sub(2),
        grid.height.saturating_sub(2),
    )
}

/// Grid tiling for grid mode: the blueprint's 1x1, 2x1, 2x2, 3x2, 3x3
/// for up to nine sessions, then keeps widening (4x3, 4x4, ...) past it.
pub fn grid_dims(n: usize) -> (usize, usize) {
    if n == 0 {
        return (0, 0);
    }
    let root = n.isqrt();
    let cols = (if root * root < n { root + 1 } else { root }).max(1);
    (cols, n.div_ceil(cols))
}

/// Full-width grid area: every content row above the session bar, with
/// no tab strip and no sidebar, so tiles get maximal room.
pub fn grid_area(area: Rect) -> Rect {
    let bar_h = if area.height >= 3 { 1 } else { 0 };
    Rect::new(area.x, area.y, area.width, area.height.saturating_sub(bar_h))
}

/// Cell rects in session order: width/height split evenly, the remainder
/// dealt to the leading columns/rows so tiles never overlap or gap.
pub fn grid_cells(area: Rect, n: usize) -> Vec<Rect> {
    let (cols, rows) = grid_dims(n);
    if cols == 0 || rows == 0 || area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    let cols_u16 = cols as u16;
    let base_w = area.width / cols_u16;
    let extra_w = (area.width % cols_u16) as usize;
    let rows_u16 = rows as u16;
    let base_h = area.height / rows_u16;
    let extra_h = (area.height % rows_u16) as usize;
    let mut out = Vec::with_capacity(n);
    let mut y = area.y;
    for r in 0..rows {
        let h = base_h + u16::from(r < extra_h);
        let mut x = area.x;
        for c in 0..cols {
            if r * cols + c >= n {
                break;
            }
            let w = base_w + u16::from(c < extra_w);
            out.push(Rect::new(x, y, w, h));
            x += w;
        }
        y += h;
    }
    out
}

/// Index of the cell holding a point, if any (borders count as hits, so
/// clicking a frame still focuses its session).
pub fn grid_cell_at(cells: &[Rect], col: u16, row: u16) -> Option<usize> {
    cells.iter().position(|cell| {
        col >= cell.x && col < cell.right() && row >= cell.y && row < cell.bottom()
    })
}



#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn chrome_splits_main_sidebar_and_bars() {
        // No status bar: the session bar owns the last row.
        let c = chrome_areas(Rect::new(0, 0, 80, 24));
        assert_eq!(c.topbar, Rect::new(0, 0, 64, 1));
        assert_eq!(c.main, Rect::new(0, 1, 64, 22));
        assert_eq!(c.sidebar, Rect::new(64, 0, 16, 23));
        assert_eq!(c.session_bar, Rect::new(0, 23, 80, 1));
        let wide = chrome_areas(Rect::new(0, 0, 120, 40));
        assert_eq!(wide.topbar, Rect::new(0, 0, 96, 1));
        assert_eq!(wide.main, Rect::new(0, 1, 96, 38));
        assert_eq!(wide.sidebar, Rect::new(96, 0, 24, 39));
        assert_eq!(wide.session_bar, Rect::new(0, 39, 120, 1));
        // Tiny terminals keep content over chrome.
        let tiny = chrome_areas(Rect::new(0, 0, 80, 2));
        assert_eq!(tiny.main.height, 2);
        assert_eq!(tiny.session_bar.height, 0);
    }

    #[cfg(feature = "visual")]
    #[test]
    fn pane_content_area_sits_inside_the_pane_border() {
        let areas = chrome_areas(Rect::new(0, 0, 180, 40));
        let grid = pane_grid_area(&areas);
        let content = pane_content_area(&areas);
        assert_eq!(content.x, grid.x + 1);
        assert_eq!(content.y, grid.y + 1);
        assert_eq!(content.width, grid.width - 2);
        assert_eq!(content.height, grid.height - 2);
    }

    #[test]
    fn grid_dims_follow_blueprint_then_widen() {
        assert_eq!(grid_dims(0), (0, 0));
        assert_eq!(grid_dims(1), (1, 1));
        assert_eq!(grid_dims(2), (2, 1));
        assert_eq!(grid_dims(3), (2, 2));
        assert_eq!(grid_dims(4), (2, 2));
        assert_eq!(grid_dims(5), (3, 2));
        assert_eq!(grid_dims(6), (3, 2));
        assert_eq!(grid_dims(7), (3, 3));
        assert_eq!(grid_dims(9), (3, 3));
        assert_eq!(grid_dims(10), (4, 3));
        assert_eq!(grid_dims(12), (4, 3));
        assert_eq!(grid_dims(13), (4, 4));
    }

    #[test]
    fn grid_cells_tile_without_overlap_or_gap() {
        let area = Rect::new(0, 1, 80, 22);
        let cells = grid_cells(area, 5);
        assert_eq!(cells.len(), 5);
        // 80 across 3 columns deals the remainder to the leaders.
        assert_eq!(cells[0], Rect::new(0, 1, 27, 11));
        assert_eq!(cells[1], Rect::new(27, 1, 27, 11));
        assert_eq!(cells[2], Rect::new(54, 1, 26, 11));
        assert_eq!(cells[3], Rect::new(0, 12, 27, 11));
        assert_eq!(cells[4], Rect::new(27, 12, 27, 11));
        assert!(grid_cells(area, 0).is_empty());
    }

    #[test]
    fn grid_cell_at_hits_frames_only() {
        // No tab strip in grid: tiles own row 0 through the session bar.
        let cells = grid_cells(Rect::new(0, 0, 80, 23), 2);
        assert_eq!(grid_cell_at(&cells, 5, 5), Some(0));
        assert_eq!(grid_cell_at(&cells, 10, 0), Some(0), "row 0 is tiles");
        assert_eq!(grid_cell_at(&cells, 40, 0), Some(1), "borders count");
        assert_eq!(grid_cell_at(&cells, 10, 23), None, "session bar is dead");
    }

    #[test]
    fn mouse_translates_to_pane_cells() {
        let area = Rect::new(0, 0, 80, 24);
        assert_eq!(translate_mouse(area, 1, 1), Some((1, 1)));
        assert_eq!(translate_mouse(area, 10, 5), Some((10, 5)));
        // Borders and outside belong to chrome, not the pane.
        assert_eq!(translate_mouse(area, 0, 0), None);
        assert_eq!(translate_mouse(area, 79, 23), None);
        assert_eq!(translate_mouse(area, 200, 200), None);
        assert_eq!(translate_mouse(Rect::new(40, 0, 40, 24), 41, 1), Some((1, 1)));
    }

    #[test]
    fn cursor_mapping_clamps_and_hides() {
        let full = Rect::new(0, 0, 80, 24);
        assert_eq!(cursor_screen_pos(full, Some((0, 0))), Some(Position::new(1, 1)));
        assert_eq!(cursor_screen_pos(full, None), None);
        assert_eq!(
            cursor_screen_pos(full, Some((500, 500))),
            Some(Position::new(78, 22))
        );
        assert_eq!(cursor_screen_pos(Rect::new(0, 0, 2, 2), Some((0, 0))), None);
    }

}
