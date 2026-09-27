//! UI visual tab: zoom/chat buttons, chrome geometry, footer helpers.

#[cfg(feature = "visual")]
use ratatui::layout::Rect;
#[cfg(feature = "visual")]
use ratatui::style::{Color, Style};

#[cfg(feature = "visual")]
use super::SpanView;
#[cfg(feature = "visual")]
use super::theme;

/// Visual tab zoom buttons: fixed labels, so hit rects stay stable.
/// Bracketed legacy text; pills use the bare text below with caps.
#[cfg(feature = "visual")]
pub const VISUAL_ZOOM_IN_LABEL: &str = "[+ zoom in]";
#[cfg(feature = "visual")]
pub const VISUAL_ZOOM_OUT_LABEL: &str = "[- zoom out]";

/// Pill inner text for the zoom buttons (caps and pads wrap it).
#[cfg(feature = "visual")]
pub const VISUAL_ZOOM_IN_TEXT: &str = "+ zoom in";
#[cfg(feature = "visual")]
pub const VISUAL_ZOOM_OUT_TEXT: &str = "- zoom out";
/// Pill inner text for the chat toggle.
#[cfg(feature = "visual")]
pub const VISUAL_CHAT_TEXT: &str = "chat";
/// Legacy bracketed chat toggle label.
#[cfg(feature = "visual")]
pub const VISUAL_CHAT_LABEL: &str = "[chat]";

/// Share of the tab content height reserved for the dismissable
/// chat footer while open: ask row plus history. Scales with the
/// tab instead of pinning a fixed height.
#[cfg(feature = "visual")]
pub const VISUAL_CHAT_FOOTER_PCT: u32 = 30;

/// Footer rows for one content height, rounded to the nearest row.
/// Zero content means zero footer; the chrome still sheds what does
/// not fit, so tiny tabs degrade the same way.
#[cfg(feature = "visual")]
pub fn visual_chat_footer_rows(content_h: u16) -> u16 {
    if content_h == 0 {
        return 0;
    }
    ((content_h as u32 * VISUAL_CHAT_FOOTER_PCT + 50) / 100) as u16
}

/// Button widths in cells: pills add caps plus inner pads, like
/// the sidebar mode pills; legacy is the bare bracketed label.
#[cfg(feature = "visual")]
pub fn visual_button_widths(pills: bool) -> (u16, u16, u16) {
    if pills {
        (
            VISUAL_ZOOM_IN_TEXT.len() as u16 + 4,
            VISUAL_ZOOM_OUT_TEXT.len() as u16 + 4,
            VISUAL_CHAT_TEXT.len() as u16 + 4,
        )
    } else {
        (
            VISUAL_ZOOM_IN_LABEL.len() as u16,
            VISUAL_ZOOM_OUT_LABEL.len() as u16,
            VISUAL_CHAT_LABEL.len() as u16,
        )
    }
}

/// Button strip spans (zoom pair, chat toggle, two-space gaps), pill
/// or legacy to match the active chrome. The chat toggle renders in
/// the focus style while open. Widths equal [`visual_button_widths`],
/// so the hit rects never desync.
#[cfg(feature = "visual")]
pub fn visual_button_spans(pills: bool, chat_open: bool) -> Vec<SpanView> {
    if pills {
        // Caps match their container like the sidebar mode pills, so
        // every toggle reads as one solid pill. The open chat pill
        // uses the active container: a bare focus style has no
        // background and paints terminal-black in the middle.
        let gray = || Style::default().fg(Color::DarkGray);
        let label = theme::style(theme::Role::TabInactive);
        let (chat_fill, chat_left, chat_right) = theme::button_chrome(
            chat_open,
            theme::style(theme::Role::TabActive),
            theme::style(theme::Role::TabInactive),
            Color::DarkGray,
        );
        let mut spans = Vec::new();
        for (text, style, cap_left, cap_right) in [
            (VISUAL_ZOOM_IN_TEXT, label, gray(), gray()),
            (VISUAL_ZOOM_OUT_TEXT, label, gray(), gray()),
            (
                VISUAL_CHAT_TEXT,
                chat_fill,
                Style::default().fg(chat_left),
                Style::default().fg(chat_right),
            ),
        ] {
            if !spans.is_empty() {
                spans.push(SpanView { text: "  ".to_string(), style: Style::default() });
            }
            spans.push(SpanView { text: crate::ui::theme::pill_left().to_string(), style: cap_left });
            spans.push(SpanView { text: " ".to_string(), style });
            spans.push(SpanView { text: text.to_string(), style });
            spans.push(SpanView { text: " ".to_string(), style });
            spans.push(SpanView { text: crate::ui::theme::pill_right().to_string(), style: cap_right });
        }
        spans
    } else {
        let button = theme::style(theme::Role::Focus);
        let chat = if chat_open {
            button
        } else {
            theme::style(theme::Role::TabInactive)
        };
        vec![
            SpanView { text: VISUAL_ZOOM_IN_LABEL.to_string(), style: button },
            SpanView { text: "  ".to_string(), style: Style::default() },
            SpanView { text: VISUAL_ZOOM_OUT_LABEL.to_string(), style: button },
            SpanView { text: "  ".to_string(), style: Style::default() },
            SpanView { text: VISUAL_CHAT_LABEL.to_string(), style: chat },
        ]
    }
}

/// Which Visual tab strip button a click hit, if any.
#[cfg(feature = "visual")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisualButton {
    ZoomIn,
    ZoomOut,
    Chat,
}

/// Visual tab chrome inside its content rect: a blank spacer row on
/// top, the one-row button strip below it, the image region under
/// that, and the dismissable chat footer pinned to the bottom.
/// Render and mouse handling both recompute these from the same
/// content rect, so clicks never desync from what the tab paints.
/// Tiny heights shed the footer first, then the buttons, then the
/// spacer.
#[cfg(feature = "visual")]
pub struct VisualChrome {
    pub image: Rect,
    pub zoom_in: Rect,
    pub zoom_out: Rect,
    pub chat: Rect,
    pub footer: Rect,
}

#[cfg(feature = "visual")]
pub fn visual_chrome(content: Rect, pills: bool, footer_h: u16) -> VisualChrome {
    let spacer_h = if content.height > 0 { 1 } else { 0 };
    let row_h = if content.height > spacer_h { 1 } else { 0 };
    let btn_y = content.y.saturating_add(spacer_h);
    let foot_h = footer_h.min(content.height.saturating_sub(spacer_h + row_h));
    let image = Rect::new(
        content.x,
        btn_y.saturating_add(row_h),
        content.width,
        content.height.saturating_sub(spacer_h + row_h + foot_h),
    );
    let footer = Rect::new(
        content.x,
        btn_y.saturating_add(row_h).saturating_add(image.height),
        content.width,
        foot_h,
    );
    let (zin_full, zout_full, chat_full) = visual_button_widths(pills);
    let zin_w = zin_full.min(content.width);
    let zout_x = content.x.saturating_add(zin_w + 2);
    let zout_w = zout_full.min(content.width.saturating_sub(zin_w + 2));
    let chat_x = zout_x.saturating_add(zout_w + 2);
    let chat_w = chat_full.min(content.width.saturating_sub(chat_x.saturating_sub(content.x)));
    VisualChrome {
        image,
        zoom_in: Rect::new(content.x, btn_y, zin_w, row_h),
        zoom_out: Rect::new(zout_x, btn_y, zout_w, row_h),
        chat: Rect::new(chat_x, btn_y, chat_w, row_h),
        footer,
    }
}

/// Hit-test a click against the Visual tab strip buttons.
#[cfg(feature = "visual")]
pub fn visual_button_at(chrome: &VisualChrome, col: u16, row: u16) -> Option<VisualButton> {
    let hit = |r: Rect| {
        r.height > 0 && r.width > 0 && row == r.y && col >= r.x && col < r.x.saturating_add(r.width)
    };
    if hit(chrome.zoom_in) {
        Some(VisualButton::ZoomIn)
    } else if hit(chrome.zoom_out) {
        Some(VisualButton::ZoomOut)
    } else if hit(chrome.chat) {
        Some(VisualButton::Chat)
    } else {
        None
    }
}

/// History rows inside the bordered Q/A footer: the total minus the
/// top/bottom border, the pad row under the title, the divider, and
/// the docked prompt row.
#[cfg(feature = "visual")]
pub fn visual_chat_history_rows(footer_h: u16) -> u16 {
    footer_h.saturating_sub(5)
}

/// Max cells for the alt-text suffix on the Visual strip row: the
/// strip is chrome, so long descriptions shorten with an ellipsis
/// instead of running on as a caption.
#[cfg(feature = "visual")]
pub const VISUAL_STRIP_ALT_MAX: u16 = 48;


#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::session_bar::PILL_LEFT;

    #[cfg(feature = "visual")]
    #[test]
    fn button_caps_follow_the_active_theme() {
        let text = crate::ui::theme::parse_external_theme(
            r##"{"name": "sq", "buttons": {"left": "[", "right": "]"}}"##,
        )
        .expect("square theme parses");
        let _guard = crate::ui::theme::hold_external_theme(text);
        assert_eq!(crate::ui::theme::pill_left(), '[');
        assert_eq!(crate::ui::theme::pill_right(), ']');
        let strip: String = visual_button_spans(true, false)
            .iter()
            .map(|s| s.text.as_str())
            .collect();
        assert!(strip.contains('[') && strip.contains(']'), "square caps: {strip:?}");
        assert!(!strip.contains(PILL_LEFT), "no builtin caps: {strip:?}");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_chrome_leaves_a_spacer_row_above_the_buttons() {
        // Breathing room from the tab strip: row zero of the content
        // is always blank, the zoom strip rides row one, art below.
        let chrome = visual_chrome(Rect::new(10, 5, 60, 20), false, 0);
        assert_eq!((chrome.zoom_in.x, chrome.zoom_in.y), (10, 6));
        assert_eq!(
            (chrome.image.x, chrome.image.y, chrome.image.width, chrome.image.height),
            (10, 7, 60, 18)
        );
        assert_eq!(visual_button_at(&chrome, 10, 5), None, "spacer row is dead");
        assert_eq!(visual_button_at(&chrome, 10, 6), Some(VisualButton::ZoomIn));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_chrome_reserves_footer_rows_off_the_image() {
        // The dismissable chat footer pins to the bottom: fixed rows
        // the Kitty paint and the click map both exclude.
        let chrome = visual_chrome(Rect::new(10, 5, 60, 20), false, 7);
        assert_eq!((chrome.zoom_in.x, chrome.zoom_in.y), (10, 6));
        assert_eq!(
            (chrome.image.x, chrome.image.y, chrome.image.width, chrome.image.height),
            (10, 7, 60, 11),
            "image sheds strip and footer"
        );
        assert_eq!(
            (chrome.footer.x, chrome.footer.y, chrome.footer.width, chrome.footer.height),
            (10, 18, 60, 7),
            "footer pins to the bottom"
        );
        assert_eq!(visual_button_at(&chrome, 10, 6), Some(VisualButton::ZoomIn));
        assert_eq!(visual_button_at(&chrome, 10, 7), None, "image rows are not buttons");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_chat_button_hits_after_zoom_out() {
        let chrome = visual_chrome(Rect::new(10, 5, 60, 20), false, 0);
        // Legacy widths: [+ zoom in]=11, gap 2, [- zoom out]=12, gap 2.
        assert_eq!((chrome.chat.x, chrome.chat.width), (37, 6), "[chat]");
        assert_eq!(visual_button_at(&chrome, 37, 6), Some(VisualButton::Chat));
        assert_eq!(visual_button_at(&chrome, 42, 6), Some(VisualButton::Chat));
        assert_eq!(visual_button_at(&chrome, 43, 6), None, "past the label");
        assert_eq!(visual_button_at(&chrome, 37, 5), None, "spacer row is dead");
        assert_eq!(visual_button_at(&chrome, 37, 7), None, "image rows are not buttons");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_chrome_splits_buttons_row_and_image_region() {
        let chrome = visual_chrome(Rect::new(10, 5, 60, 20), false, 0);
        assert_eq!(
            (chrome.zoom_in.x, chrome.zoom_in.y),
            (10, 6),
            "buttons paint one row below the tab strip"
        );
        assert_eq!(
            (chrome.image.x, chrome.image.y, chrome.image.width, chrome.image.height),
            (10, 7, 60, 18),
            "image region fills the rest"
        );
        assert_eq!((chrome.zoom_in.x, chrome.zoom_in.width), (10, 11));
        assert_eq!((chrome.zoom_out.x, chrome.zoom_out.width), (23, 12));
        assert_eq!(visual_button_at(&chrome, 10, 5), None, "spacer row is dead");
        assert_eq!(visual_button_at(&chrome, 10, 6), Some(VisualButton::ZoomIn));
        assert_eq!(visual_button_at(&chrome, 20, 6), Some(VisualButton::ZoomIn));
        assert_eq!(visual_button_at(&chrome, 21, 6), None, "gap is dead");
        assert_eq!(visual_button_at(&chrome, 23, 6), Some(VisualButton::ZoomOut));
        assert_eq!(visual_button_at(&chrome, 34, 6), Some(VisualButton::ZoomOut));
        assert_eq!(visual_button_at(&chrome, 35, 6), None, "past the label");
        assert_eq!(visual_button_at(&chrome, 10, 7), None, "image rows are not buttons");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_chrome_pills_widen_the_buttons() {
        // Pill text plus caps and pads: "+ zoom in" fills 13 cells,
        // "- zoom out" 14, "chat" 8, like the sidebar mode pills.
        assert_eq!(visual_button_widths(true), (13, 14, 8));
        assert_eq!(visual_button_widths(false), (11, 12, 6));
        let chrome = visual_chrome(Rect::new(10, 5, 60, 20), true, 0);
        assert_eq!((chrome.chat.x, chrome.chat.width), (41, 8), "chat pill");
        assert_eq!(visual_button_at(&chrome, 41, 6), Some(VisualButton::Chat));
        assert_eq!((chrome.zoom_in.x, chrome.zoom_in.y), (10, 6));
        assert_eq!((chrome.zoom_in.x, chrome.zoom_in.width), (10, 13));
        assert_eq!((chrome.zoom_out.x, chrome.zoom_out.width), (25, 14));
        assert_eq!(visual_button_at(&chrome, 10, 5), None, "spacer row is dead");
        assert_eq!(visual_button_at(&chrome, 22, 6), Some(VisualButton::ZoomIn));
        assert_eq!(visual_button_at(&chrome, 23, 6), None, "gap is dead");
        assert_eq!(visual_button_at(&chrome, 25, 6), Some(VisualButton::ZoomOut));
        assert_eq!(visual_button_at(&chrome, 38, 6), Some(VisualButton::ZoomOut));
        assert_eq!(visual_button_at(&chrome, 39, 6), None, "past the cap");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_button_spans_match_the_active_style() {
        let strip: String = visual_button_spans(true, false)
            .iter()
            .map(|s| s.text.as_str())
            .collect();
        assert!(strip.contains(PILL_LEFT), "pill caps: {strip:?}");
        assert!(strip.contains("+ zoom in") && strip.contains("- zoom out") && strip.contains("chat"));
        assert!(!strip.contains('['), "no legacy brackets: {strip:?}");
        let width: usize = visual_button_spans(true, false).iter().map(|s| s.text.chars().count()).sum();
        assert_eq!(width, 13 + 2 + 14 + 2 + 8, "buttons plus gaps");
        let legacy: String = visual_button_spans(false, false)
            .iter()
            .map(|s| s.text.as_str())
            .collect();
        assert!(legacy.contains("[+ zoom in]") && legacy.contains("[- zoom out]") && legacy.contains("[chat]"));
        assert!(!legacy.contains(PILL_LEFT), "no caps: {legacy:?}");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_chat_pill_uses_active_container_while_open() {
        // The open chat toggle must read as one solid pill like the
        // zoom pair: active container plus matching caps. A bare
        // focus style has no background, which paints the middle
        // terminal-black against the gray pills.
        let open = visual_button_spans(true, true);
        let at = open.iter().position(|s| s.text == VISUAL_CHAT_TEXT).expect("chat pill");
        assert_eq!(open[at].style, theme::style(theme::Role::TabActive), "open fill");
        assert_eq!(open[at - 2].style, Style::default().fg(Color::Yellow), "left cap");
        assert_eq!(open[at + 2].style, Style::default().fg(Color::Yellow), "right cap");
        let closed = visual_button_spans(true, false);
        let at = closed.iter().position(|s| s.text == VISUAL_CHAT_TEXT).expect("chat pill");
        assert_eq!(closed[at].style, theme::style(theme::Role::TabInactive), "closed fill");
        assert_eq!(closed[at - 2].style, Style::default().fg(Color::DarkGray), "left cap");
        assert_eq!(closed[at + 2].style, Style::default().fg(Color::DarkGray), "right cap");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_chat_history_rows_subtract_the_box_chrome() {
        // Inside the bordered footer the history viewport is the
        // total minus top/bottom border, pad row, divider, and the
        // docked prompt row.
        assert_eq!(visual_chat_history_rows(11), 6);
        assert_eq!(visual_chat_history_rows(7), 2);
        assert_eq!(visual_chat_history_rows(5), 0);
        assert_eq!(visual_chat_history_rows(0), 0);
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_chat_footer_rows_take_thirty_percent() {
        // The Q/A panel scales with the tab instead of pinning a
        // fixed height: ask row plus history share 30% of content.
        assert_eq!(visual_chat_footer_rows(100), 30);
        assert_eq!(visual_chat_footer_rows(36), 11);
        assert_eq!(visual_chat_footer_rows(10), 3);
        assert_eq!(visual_chat_footer_rows(0), 0);
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_chrome_degrades_on_tiny_content() {
        for pills in [false, true] {
            let chrome = visual_chrome(Rect::new(0, 0, 0, 0), pills, 0);
            assert_eq!(chrome.image.height, 0);
            assert_eq!(visual_button_at(&chrome, 0, 0), None);
            // One row keeps the spacer only: the buttons shed first so
            // a stray click can never hit an invisible button.
            let chrome = visual_chrome(Rect::new(0, 0, 10, 1), pills, 0);
            assert_eq!(chrome.image.height, 0);
            assert_eq!(visual_button_at(&chrome, 0, 0), None);
        }
    }

}
