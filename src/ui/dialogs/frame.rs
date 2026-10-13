//! Shared modal frame: opaque clear, bordered titled container,
//! content rect or nothing when too small. Every dialog painted its
//! own; this owns the one layout so frames can never drift apart.

use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders, Clear};
use ratatui::Frame;

/// One modal chrome for every dialog: opaque clear so the live grid
/// never shows through, rounded theme border with the title,
/// container fill, and the content rect when the area fits `min_size`
/// (painted but `None` below it, like the legacy guards).
#[derive(Clone, Debug)]
pub struct ModalFrame {
    title: String,
    min_w: u16,
    min_h: u16,
    fill: bool,
}

impl ModalFrame {
    pub fn new(title: impl Into<String>) -> Self {
        ModalFrame { title: title.into(), min_w: 0, min_h: 0, fill: true }
    }

    /// Minimum content-rect size; smaller areas paint but refuse.
    pub fn min_size(mut self, w: u16, h: u16) -> Self {
        self.min_w = w;
        self.min_h = h;
        self
    }

    /// Skip the container fill (transparent overlays like which-key).
    pub fn transparent(mut self) -> Self {
        self.fill = false;
        self
    }

    /// Paint the frame; the content rect, or `None` when cramped.
    pub fn open(&self, frame: &mut Frame, area: Rect) -> Option<Rect> {
        use crate::ui::theme::{self, Role};
        frame.render_widget(Clear, area);
        let mut block = Block::default()
            .borders(Borders::ALL)
            .border_type(theme::border_type())
            .title(self.title.as_str())
            .border_style(theme::style(Role::BorderModal));
        if self.fill {
            block = block.style(theme::modal_fill());
        }
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.width >= self.min_w && inner.height >= self.min_h {
            Some(inner)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn paint(frame: ModalFrame, w: u16, h: u16) -> (Option<Rect>, Terminal<TestBackend>) {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut inner = None;
        terminal
            .draw(|f| {
                inner = frame.open(f, f.area());
            })
            .unwrap();
        (inner, terminal)
    }

    #[test]
    fn open_paints_border_title_and_returns_inner() {
        let (inner, terminal) =
            paint(ModalFrame::new(" Hi ").min_size(10, 5), 40, 12);
        let inner = inner.expect("roomy area opens");
        assert_eq!(inner, Rect::new(1, 1, 38, 10), "border insets");
        let buf = terminal.backend().buffer();
        assert_ne!(buf[(0, 0)].symbol(), " ", "border paints");
        let title: String = (0..8).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(title.contains("Hi"), "title paints: {title:?}");
    }

    #[test]
    fn too_small_paints_but_returns_none() {
        let (inner, terminal) =
            paint(ModalFrame::new(" Hi ").min_size(44, 17), 40, 12);
        assert!(inner.is_none(), "cramped area refuses content");
        let buf = terminal.backend().buffer();
        assert_ne!(buf[(0, 0)].symbol(), " ", "frame still paints");
    }

    #[test]
    fn transparent_skips_the_modal_fill() {
        let (_, filled) = paint(ModalFrame::new(" A "), 40, 12);
        let (_, plain) = paint(ModalFrame::new(" B ").transparent(), 40, 12);
        let bg = |t: &Terminal<TestBackend>| t.backend().buffer()[(20, 6)].bg;
        assert_eq!(
            bg(&filled),
            crate::ui::theme::modal_fill().bg.unwrap_or(ratatui::style::Color::Reset),
            "default frame carries the fill"
        );
        assert_eq!(bg(&plain), ratatui::style::Color::Reset, "transparent skips it");
    }
}
