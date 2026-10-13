//! Pill button widget: label, fill, caps, paint, and click in one
//! type. Every dialog, bar, and strip painted its own pill spans and
//! walked them separately for clicks; this owns both from one layout
//! so paint and hit-testing can never desync.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::theme::{self, Role};

/// One pill button: `[cap][pad][label][pad][cap]` with an optional
/// `>` chosen-marker ahead of the caps. The fill always carries the
/// runtime contrast text for its background. Labels arrive
/// display-ready (callers bake in `*` default marks and encode hostile
/// text); widths count chars like the legacy layout math did.
#[derive(Clone, Debug)]
pub struct Button {
    label: String,
    source: ButtonSource,
    marker: bool,
    padding: bool,
}

/// Where the fill and caps come from: the theme pair resolved by
/// emphasis, or an explicit triple (destructive variants, resolved
/// bar segments).
#[derive(Clone, Debug)]
enum ButtonSource {
    Themed {
        emphasized: bool,
        accent: Style,
        rest: Style,
        rest_cap: Color,
    },
    Explicit {
        fill: Style,
        left: Color,
        right: Color,
    },
}

impl Button {
    pub fn new(label: impl Into<String>) -> Self {
        Button {
            label: label.into(),
            source: ButtonSource::Themed {
                emphasized: false,
                accent: theme::style(Role::TabActive),
                rest: theme::style(Role::TabInactive),
                rest_cap: Color::DarkGray,
            },
            marker: false,
            padding: true,
        }
    }

    /// Explicit fill and caps (resolved bar segments, destructive
    /// variants). The fill still gains the contrast text.
    pub fn filled(label: impl Into<String>, fill: Style, left: Color, right: Color) -> Self {
        Button {
            label: label.into(),
            source: ButtonSource::Explicit { fill, left, right },
            marker: false,
            padding: true,
        }
    }

    /// Accent fill when true, rest container when false.
    pub fn emphasized(mut self, emphasized: bool) -> Self {
        if let ButtonSource::Themed { emphasized: e, .. } = &mut self.source {
            *e = emphasized;
        }
        self
    }

    /// Override the theme fills and resting cap color (grouped tabs,
    /// always-accent defaults).
    pub fn styles(mut self, accent: Style, rest: Style, rest_cap: Color) -> Self {
        if let ButtonSource::Themed {
            accent: a,
            rest: r,
            rest_cap: c,
            ..
        } = &mut self.source
        {
            *a = accent;
            *r = rest;
            *c = rest_cap;
        }
        self
    }

    /// Paint the `>` chosen-marker ahead of the left cap.
    pub fn marker(mut self, marker: bool) -> Self {
        self.marker = marker;
        self
    }

    /// Drop the inner padding (`[cap][label][cap]`, the writer shape).
    pub fn unpadded(mut self) -> Self {
        self.padding = false;
        self
    }

    fn chrome(&self) -> (Style, Color, Color) {
        match &self.source {
            ButtonSource::Themed { emphasized, accent, rest, rest_cap } => {
                theme::button_chrome(*emphasized, *accent, *rest, *rest_cap)
            }
            ButtonSource::Explicit { fill, left, right } => {
                (theme::with_contrast_text(*fill), *left, *right)
            }
        }
    }

    /// The exact spans the paint lays out, marker included.
    pub fn spans(&self) -> Vec<Span<'static>> {
        let (fill, left_cap, right_cap) = self.chrome();
        let mut spans = Vec::new();
        if self.marker {
            spans.push(Span::styled(">", theme::focus_row()));
        }
        spans.push(Span::styled(
            theme::pill_left().to_string(),
            Style::default().fg(left_cap),
        ));
        if self.padding {
            spans.push(Span::styled(" ".to_string(), fill));
        }
        spans.push(Span::styled(self.label.clone(), fill));
        if self.padding {
            spans.push(Span::styled(" ".to_string(), fill));
        }
        spans.push(Span::styled(
            theme::pill_right().to_string(),
            Style::default().fg(right_cap),
        ));
        spans
    }

    /// Painted cell width: marker, caps, pads, and label.
    pub fn width(&self) -> usize {
        self.label.chars().count() + 2 + if self.padding { 2 } else { 0 } + usize::from(self.marker)
    }

    /// Paint into `area` (clips like every other widget row).
    pub fn paint(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Paragraph::new(Line::from(self.spans())), area);
    }

    /// Hit-test a click against the painted rect.
    pub fn click(&self, col: u16, row: u16, area: Rect) -> bool {
        col >= area.x && col < area.x.saturating_add(area.width) && row == area.y && area.height > 0
    }
}

/// One row of pill buttons with two-space gaps: centered dialog
/// action rows. Markers ride with their button; gaps and the clipped
/// tail belong to nothing.
#[derive(Clone, Debug, Default)]
pub struct ButtonRow {
    buttons: Vec<Button>,
    gap: Span<'static>,
}

impl ButtonRow {
    pub fn new(buttons: Vec<Button>) -> Self {
        ButtonRow { buttons, gap: Span::raw("  ") }
    }

    /// Separator between buttons (cards/telegrams style theirs).
    pub fn gap(mut self, gap: Span<'static>) -> Self {
        self.gap = gap;
        self
    }

    pub fn len(&self) -> usize {
        self.buttons.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buttons.is_empty()
    }

    /// The exact spans the paint lays out.
    pub fn spans(&self) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        for (index, button) in self.buttons.iter().enumerate() {
            if index > 0 {
                spans.push(self.gap.clone());
            }
            spans.extend(button.spans());
        }
        spans
    }

    /// Painted cell width: buttons plus gaps.
    pub fn width(&self) -> usize {
        self.buttons.iter().map(Button::width).sum::<usize>()
            + self.gap_width() * self.buttons.len().saturating_sub(1)
    }

    fn gap_width(&self) -> usize {
        self.gap.content.chars().count()
    }

    /// Left origin that centers the row in `[x, x + room)`.
    pub fn origin_centered(&self, x: u16, room: u16) -> u16 {
        x.saturating_add(room.saturating_sub(self.width().min(room as usize) as u16) / 2)
    }

    /// Paint left-aligned into `area`.
    pub fn paint(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Paragraph::new(Line::from(self.spans())), area);
    }

    /// Button index under a click on row `y` starting at `ox`,
    /// clipped to `w` cells like the paint. Markers fire their own
    /// button; gaps and the tail belong to nothing.
    pub fn click(&self, col: u16, row: u16, ox: u16, y: u16, w: u16) -> Option<usize> {
        if row != y {
            return None;
        }
        let mut x = ox;
        let end = ox.saturating_add(w);
        for (index, button) in self.buttons.iter().enumerate() {
            let bw = button.width() as u16;
            if col >= x && col < x.saturating_add(bw) && x < end {
                return Some(index);
            }
            x = x.saturating_add(bw.saturating_add(self.gap_width() as u16));
            if x >= end {
                break;
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Style};

    #[test]
    fn pill_spans_match_legacy_layout() {
        // Caps, pads, label: the exact spans every site hand-rolled.
        let spans = Button::new("No").spans();
        let text: Vec<&str> = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, vec!["\u{e0b6}", " ", "No", " ", "\u{e0b4}"]);
        // Pads and label share the rest fill; caps carry their color.
        assert_eq!(spans[1].style.bg, Some(Color::DarkGray));
        assert_eq!(spans[2].style.bg, Some(Color::DarkGray));
        assert_eq!(spans[2].style.fg, Some(Color::White), "contrast text");
        assert_eq!(spans[0].style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn emphasized_fill_reads_dark_on_accent() {
        let spans = Button::new("Yes").emphasized(true).spans();
        assert_eq!(spans[2].style.bg, Some(Color::Yellow));
        assert_eq!(spans[2].style.fg, Some(Color::Black));
    }

    #[test]
    fn marker_prefixes_the_chosen_button() {
        let plain = Button::new("Save").width();
        let marked = Button::new("Save").marker(true);
        assert_eq!(marked.width(), plain + 1, "marker costs one cell");
        let spans = marked.spans();
        assert_eq!(spans[0].content.as_ref(), ">");
        assert_eq!(spans[0].style, crate::ui::theme::focus_row());
        assert_eq!(spans[1].content.as_ref(), "\u{e0b6}", "caps follow the marker");
    }

    #[test]
    fn unpadded_writer_style_skips_inner_spaces() {
        let spans = Button::new("*Rephrase").unpadded().emphasized(true).spans();
        let text: Vec<&str> = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, vec!["\u{e0b6}", "*Rephrase", "\u{e0b4}"]);
    }

    #[test]
    fn custom_styles_cover_grouped_rest() {
        let group = Style::default().bg(Color::Indexed(2));
        let spans = Button::new("g")
            .styles(Style::default(), group, Color::Indexed(2))
            .spans();
        assert_eq!(spans[2].style.bg, Some(Color::Indexed(2)));
        assert_eq!(spans[0].style.fg, Some(Color::Indexed(2)), "caps match container");
    }

    #[test]
    fn row_joins_with_gaps_and_clicks_from_painted_cells() {
        use ratatui::{backend::TestBackend, Terminal};
        let row = ButtonRow::new(vec![Button::new("Yes").emphasized(true), Button::new("No")]);
        assert_eq!(row.width(), 7 + 2 + 6, "two pills plus gap");
        let mut terminal = Terminal::new(TestBackend::new(40, 3)).unwrap();
        terminal
            .draw(|f| {
                let ox = row.origin_centered(0, 40);
                row.paint(f, ratatui::layout::Rect::new(ox, 1, row.width() as u16, 1));
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        // Drive clicks from cells read back off the paint, never from
        // hand-computed coordinates.
        let row_text: String = (0..40).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        let cell_x = |needle: &str| {
            let byte = row_text.find(needle).unwrap_or_else(|| panic!("{needle} paints"));
            row_text[..byte].chars().count() as u16
        };
        let (yes_x, no_x) = (cell_x("Yes"), cell_x("No"));
        let ox = row.origin_centered(0, 40);
        assert_eq!(row.click(yes_x, 1, ox, 1, 40), Some(0));
        assert_eq!(row.click(no_x, 1, ox, 1, 40), Some(1));
        assert_eq!(row.click(ox + 7, 1, ox, 1, 40), None, "gap belongs to nothing");
        assert_eq!(row.click(ox, 0, ox, 1, 40), None, "off-row dies");
        assert_eq!(row.click(ox + 40, 1, ox, 1, 40), None, "past-end dies");
    }

    #[test]
    fn row_click_marker_rides_with_its_button() {
        let row = ButtonRow::new(vec![
            Button::new("Save*").emphasized(true).marker(true),
            Button::new("Cancel"),
        ]);
        let ox = 10;
        assert_eq!(row.click(ox, 0, ox, 0, 60), Some(0), "marker cell fires its button");
        assert_eq!(row.click(ox + 12, 0, ox, 0, 60), Some(1), "second button");
    }

    #[test]
    fn filled_explicit_triple_contrasts_too() {
        // Destructive red Cancel: explicit fill still reads light.
        let fill = Style::default().fg(Color::Black).bg(Color::Red);
        let spans = Button::filled("Cancel", fill, Color::Red, Color::Red).spans();
        let text: Vec<&str> = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, vec!["\u{e0b6}", " ", "Cancel", " ", "\u{e0b4}"]);
        assert_eq!(spans[2].style.bg, Some(Color::Red));
        assert_eq!(spans[2].style.fg, Some(Color::White));
        assert_eq!(spans[0].style.fg, Some(Color::Red), "caps stay explicit");
    }

    #[test]
    fn single_button_click_is_rect_containment() {
        let b = Button::new("Kanban");
        let area = ratatui::layout::Rect::new(5, 5, 10, 1);
        assert!(b.click(5, 5, area));
        assert!(b.click(14, 5, area));
        assert!(!b.click(15, 5, area));
        assert!(!b.click(5, 6, area));
    }
}
