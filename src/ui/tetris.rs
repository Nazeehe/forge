//! UI tetris paint: sidebar game, preview, ghost piece.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::theme;

/// Tetris button underlay: the same label the overlay widget paints,
/// so content readers agree with the buffer. The open highlight lives
/// on the overlay widget (like the mode buttons), which owns the row
/// in the real render.
pub(super) fn tetris_line(pills: bool) -> Line<'static> {
    if pills {
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                crate::ui::theme::pill_left().to_string(),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(" Tetris ", theme::style(theme::Role::TabInactive)),
            Span::styled(
                crate::ui::theme::pill_right().to_string(),
                Style::default().fg(Color::DarkGray),
            ),
        ])
    } else {
        Line::from(vec![Span::raw("  "), Span::styled("[Tetris]", Style::default())])
    }
}

/// One tetromino's chrome color. Semantic theme roles only, never raw
/// RGB; shape and position carry the game, so monochrome stays fully
/// playable.
fn tetris_kind_style(kind: crate::tetris::PieceKind) -> Style {
    use crate::tetris::PieceKind;
    theme::style(match kind {
        PieceKind::I => theme::Role::Info,
        PieceKind::O => theme::Role::Warning,
        PieceKind::T => theme::Role::Brand,
        PieceKind::S => theme::Role::Success,
        PieceKind::Z => theme::Role::Danger,
        PieceKind::J => theme::Role::Command,
        PieceKind::L => theme::Role::Running,
    })
}

/// Game paint for the sidebar list region: header, bordered well with
/// ghost landing preview, next-piece panel, hints, exactly `height`
/// rows. Wide sidebars get double-width cells; narrow ones fall back
/// to single-width so the default 16-column sidebar still plays. The
/// preview sits right of the well when it fits and stacks below it
/// when cramped. Short regions keep the header and the bottom of the
/// well (where the stack lands) and shed the stack and hints first.
pub fn tetris_lines(
    game: &crate::tetris::TetrisGame,
    width: u16,
    height: u16,
) -> Vec<Line<'static>> {
    use crate::tetris::{HEIGHT, WIDTH};
    let height = height as usize;
    if height == 0 {
        return Vec::new();
    }
    let wide = width.saturating_sub(2) >= 24;
    let (kind, active) = game.active();
    let ghost = game.ghost_cells();
    let upcoming = game.next_piece();
    let shape = upcoming.cells();
    let min_x = shape.iter().map(|(x, _)| *x).min().unwrap_or(0);
    let min_y = shape.iter().map(|(_, y)| *y).min().unwrap_or(0);
    let max_y = shape.iter().map(|(_, y)| *y).max().unwrap_or(0);
    let norm: Vec<(i8, i8)> = shape.iter().map(|(x, y)| (x - min_x, y - min_y)).collect();
    let preview_h = (max_y - min_y + 1).max(0) as usize;
    #[derive(Clone, Copy)]
    enum WellCell {
        Active,
        Settled(crate::tetris::PieceKind),
        Ghost,
        Empty,
    }
    let well: Vec<Vec<WellCell>> = (0..HEIGHT)
        .map(|y| {
            (0..WIDTH)
                .map(|x| {
                    let pos = (x as i8, y as i8);
                    if active.contains(&pos) {
                        WellCell::Active
                    } else if let Some(k) = game.settled()[y][x] {
                        WellCell::Settled(k)
                    } else if ghost.contains(&pos) {
                        WellCell::Ghost
                    } else {
                        WellCell::Empty
                    }
                })
                .collect()
        })
        .collect();
    let mut status = format!(" Tetris · {} pts · lv {}", game.score(), game.level());
    if game.is_over() {
        status.push_str(" · GAME OVER");
    } else if game.is_paused() {
        status.push_str(" · PAUSED");
    }
    let status: String = status.chars().take(width as usize).collect();
    let hints: [String; 2] = if width >= 26 {
        [
            "  \u{2190} \u{2192} move \u{00b7} \u{2191} rotate \u{00b7} space drop".to_string(),
            "  p pause \u{00b7} r restart".to_string(),
        ]
    } else {
        ["  \u{2190}\u{2192}move \u{2191}turn".to_string(), "  SPC=drop p=pause r=new".to_string()]
    };
    // Narrow second hint needs 20 cells; shed it when it cannot fit.
    let hints: Vec<String> = if width >= 26 || width >= 22 {
        hints.to_vec()
    } else {
        hints[..1].to_vec()
    };
    let hint_rows = if height >= 16 {
        hints.len()
    } else if height >= 8 {
        hints.len().min(1)
    } else {
        0
    };
    let well_w = if wide { WIDTH * 2 } else { WIDTH };
    let show_side = (width as usize) >= 2 + 1 + well_w + 1 + 2 + 4;
    // Cramped sidebars stack the preview below the well instead of
    // clipping it; short regions shed the stack first like the hints.
    let stacked = if !show_side && height >= 8 { 1 + preview_h } else { 0 };
    let well_take = height
        .saturating_sub(1 + hint_rows + stacked)
        .min(HEIGHT);
    let mut lines = vec![Line::from(Span::styled(
        status,
        theme::style(theme::Role::Brand),
    ))];
    let cell = if wide { "\u{2588}\u{2588}" } else { "\u{2588}" };
    let blank = if wide { "  " } else { " " };
    let ghost_cell = if wide { "░░" } else { "░" };
    let border = theme::style(theme::Role::BorderUnfocused);
    let ghost_style = tetris_kind_style(kind).add_modifier(Modifier::DIM);
    let preview_style = tetris_kind_style(upcoming);
    for (i, row) in well.iter().skip(HEIGHT - well_take).enumerate() {
        let mut spans = vec![
            Span::raw("  "),
            Span::styled("│".to_string(), border),
        ];
        for filled in row {
            match filled {
                WellCell::Active => {
                    spans.push(Span::styled(cell.to_string(), tetris_kind_style(kind)))
                }
                WellCell::Settled(k) => {
                    spans.push(Span::styled(cell.to_string(), tetris_kind_style(*k)))
                }
                WellCell::Ghost => spans.push(Span::styled(ghost_cell.to_string(), ghost_style)),
                WellCell::Empty => spans.push(Span::raw(blank)),
            }
        }
        spans.push(Span::styled("│".to_string(), border));
        if show_side {
            spans.push(Span::raw("  "));
            if i == 0 {
                spans.push(Span::styled("Next".to_string(), theme::style(theme::Role::KeyHint)));
            } else if i - 1 < preview_h {
                let py = (i - 1) as i8;
                for px in 0..4i8 {
                    if norm.contains(&(px, py)) {
                        spans.push(Span::styled("\u{2588}".to_string(), preview_style));
                    } else {
                        spans.push(Span::raw(" "));
                    }
                }
            }
        }
        lines.push(Line::from(spans));
    }
    if stacked > 0 {
        lines.push(Line::from(Span::styled(
            "  Next".to_string(),
            theme::style(theme::Role::KeyHint),
        )));
        for py in 0..preview_h {
            let mut spans = vec![Span::raw("  ")];
            for px in 0..4i8 {
                if norm.contains(&(px, py as i8)) {
                    spans.push(Span::styled("\u{2588}".to_string(), preview_style));
                } else {
                    spans.push(Span::raw(" "));
                }
            }
            lines.push(Line::from(spans));
        }
    }
    for hint in hints.iter().take(hint_rows) {
        lines.push(Line::from(Span::styled(
            hint.clone(),
            theme::style(theme::Role::KeyHint),
        )));
    }
    while lines.len() < height {
        lines.push(Line::from(""));
    }
    lines.truncate(height);
    lines
}


#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn tetris_lines_honor_exact_height_and_degrade_narrow() {
        let mut game = crate::tetris::TetrisGame::new();
        let full = tetris_lines(&game, 32, 23);
        assert_eq!(full.len(), 23, "paint fills the list region exactly");
        let text: String = full.iter().flat_map(|l| l.spans.iter().map(|s| s.content.as_ref())).collect::<Vec<&str>>().join("");
        assert!(text.contains("pts") && text.contains("lv 1"), "score header: {text:?}");
        // Lock two pieces first: the compact slice shows the bottom
        // of the well, where settled cells land (two tetrominoes can
        // never clear a 10-wide row, so all 8 cells stay).
        game.hard_drop();
        game.hard_drop();
        let narrow = tetris_lines(&game, 16, 17);
        assert_eq!(narrow.len(), 17, "compact paint fills exactly");
        let cells = narrow.iter().flat_map(|l| l.spans.iter()).map(|s| {
            s.content.chars().filter(|c| *c == '\u{2588}').count()
        }).sum::<usize>();
        assert!(cells >= 6, "compact well shows the stack: {cells}");
        let tiny = tetris_lines(&game, 16, 0);
        assert!(tiny.is_empty(), "zero height paints nothing");
    }

    #[test]
    fn tetris_well_paints_side_borders() {
        let game = crate::tetris::TetrisGame::new();
        let lines = tetris_lines(&game, 32, 23);
        let wells: Vec<String> = lines
            .iter()
            .skip(1)
            .filter(|l| {
                l.spans
                    .iter()
                    .any(|s| s.content.contains('\u{2588}'))
                    || l.spans.iter().any(|s| s.content.contains('│'))
            })
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!(!wells.is_empty(), "well rows paint");
        for row in &wells {
            assert!(
                row.contains('│'),
                "well row carries a side border: {row:?}"
            );
            assert_eq!(
                row.chars().filter(|c| *c == '│').count(),
                2,
                "left and right borders: {row:?}"
            );
        }
    }

    #[test]
    fn tetris_shows_next_piece_beside_the_well() {
        let game = crate::tetris::TetrisGame::new();
        let lines = tetris_lines(&game, 32, 23);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect::<Vec<&str>>()
            .join("\n");
        assert!(
            text.contains("Next"),
            "next-piece label paints beside the well"
        );
        // The preview shares rows with the well (right side), so a row
        // holds both a border and the label/preview.
        let side = lines.iter().any(|l| {
            let row: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            row.contains('│') && row.contains("Next")
        });
        assert!(side, "preview sits on the right of the game area");
    }

    #[test]
    fn tetris_ghost_paints_light_landing_cells() {
        let game = crate::tetris::TetrisGame::new();
        let lines = tetris_lines(&game, 32, 23);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect::<Vec<&str>>()
            .join("\n");
        assert!(
            text.contains('░'),
            "ghost landing preview paints in a light glyph: {text:?}"
        );
    }

}
