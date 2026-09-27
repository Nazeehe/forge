//! UI session bar: pill segments, layout, hit-testing, paint.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tui_realm_stdlib::components::Label;
use tuirealm::component::Component;

use crate::core::safe_text;

use super::layout::ChromeAreas;
use super::topbar::ChromeButton;
use super::theme;
use super::Chrome;

/// One session-bar entry: 1-based number plus title, with the primary
/// communication group (if any) and its stable palette index.
#[derive(Clone, Debug)]
pub struct SessionTab {
    pub title: String,
    pub live: bool,
    pub focused: bool,
    pub group: Option<String>,
    pub group_color: Option<usize>,
}

/// Group-chip palette: named ANSI colors only (theme roles stay semantic),
/// cycling forever so late groups still get a chip.
pub fn group_palette(index: usize) -> Color {
    const PALETTE: [Color; 8] = [
        Color::Cyan,
        Color::Magenta,
        Color::Blue,
        Color::LightCyan,
        Color::LightMagenta,
        Color::LightGreen,
        Color::White,
        Color::LightBlue,
    ];
    PALETTE[index % PALETTE.len()]
}

/// Nerd Font half circles framing a pill tab: `label`. Single-cell
/// each; a Nerd Font is required, otherwise they show as gaps (see the
/// `pills` config flag).
pub const PILL_LEFT: char = crate::ui::theme::BUILTIN_PILL_LEFT;
pub const PILL_RIGHT: char = crate::ui::theme::BUILTIN_PILL_RIGHT;

/// One rendered chunk of the sessions bar: literal text, its style, and the
/// tab it selects when clicked (`None` for group headers, which never
/// switch sessions). `cap` paints rounded pill ends in the container
/// color around tab buttons (`None` keeps the flat legacy look).
pub struct BarSegment {
    pub text: String,
    pub style: Style,
    pub index: Option<usize>,
    pub accent: Option<Color>,
    pub cap: Option<Color>,
}

/// Rest cap color for a tab: the group color when grouped (matching
/// the label background), dim gray otherwise. Selection never changes
/// it — the highlight behavior decides which caps light up.
fn pill_rest_cap(tab: &SessionTab) -> Color {
    tab.group_color.map(group_palette).unwrap_or(Color::DarkGray)
}

/// Rest fill for a tab: grouped tabs fill with their group color and
/// dark text; ungrouped rest sits in a dim container.
fn pill_rest_style(tab: &SessionTab) -> Style {
    if let Some(color) = tab.group_color {
        Style::default()
            .fg(Color::Black)
            .bg(group_palette(color))
    } else {
        theme::style(theme::Role::TabInactive)
    }
}

/// Fill plus cap colors for a session tab under the active highlight
/// behavior. Focused tabs emphasize; grouped rest keeps its group
/// container so the group still reads at a glance.
fn pill_chrome(tab: &SessionTab) -> (Style, Color, Color) {
    let rest = pill_rest_style(tab);
    let accent = if tab.focused {
        theme::style(theme::Role::TabActive)
    } else {
        rest
    };
    theme::button_chrome(tab.focused, accent, rest, pill_rest_cap(tab))
}

/// Left bookend color for a tab: accent yellow when focused under the
/// Full behavior, the rest cap otherwise. `BarSegment.cap` carries
/// this; the render loop derives the right cap from the same tab.
fn pill_cap(tab: &SessionTab) -> Color {
    pill_chrome(tab).1
}

/// Label style for a pill tab: the selected tab takes the accent
/// container under the Full behavior (group ignored); under `Left`
/// highlight a focused grouped tab keeps its group container and only
/// the left bookend lights up.
fn pill_style(tab: &SessionTab) -> Style {
    pill_chrome(tab).0
}

/// Build bar segments in manager order so `N` numbering (and `Ctrl-b N`)
/// never shifts: consecutive tabs sharing a group get one colored
/// `group:` header; a group split by outsiders repeats its header rather
/// than reordering anyone. With `pills`, tab buttons gain rounded ends
/// and grouped/focused styling moves onto the container.
pub fn session_bar_segments(tabs: &[SessionTab], pills: bool) -> Vec<BarSegment> {
    let mut segments = Vec::new();
    let mut prev_group: Option<&str> = None;
    for (n, tab) in tabs.iter().enumerate() {
        // Pills carry the group color themselves, so the header is gone.
        if !pills {
            if let Some(group) = tab.group.as_deref() {
                if prev_group != Some(group) {
                    segments.push(BarSegment {
                        text: format!("{}:", safe_text::encode_for_display(group)),
                        style: Style::default()
                            .fg(group_palette(tab.group_color.unwrap_or(0)))
                            .add_modifier(Modifier::BOLD),
                        index: None,
                        accent: tab.group_color.map(group_palette),
                        cap: None,
                    });
                }
            }
        }
        segments.push(BarSegment {
            text: format!("{} {}", n + 1, safe_text::encode_for_display(&tab.title)),
            style: if pills {
                pill_style(tab)
            } else if tab.focused {
                let mut style = theme::style(theme::Role::Focus);
                if let Some(color) = tab.group_color {
                    style = style.bg(group_palette(color));
                }
                style
            } else if let Some(color) = tab.group_color {
                Style::default().fg(group_palette(color))
            } else {
                Style::default()
            },
            index: Some(n),
            accent: tab.group_color.map(group_palette),
            cap: pills.then(|| pill_cap(tab)),
        });
        prev_group = tab.group.as_deref();
    }
    segments
}

/// The reference strip gives every session its own status dot, group
/// swatch, and numbered click target. Keep the compact strip on small
/// terminals so labels remain usable there. Pills drop the status dot,
/// swatch, brackets, and dividers — the container carries the group and
/// the number stays in the centered label.
pub fn session_bar_segments_for_area(tabs: &[SessionTab], bar: Rect, pills: bool) -> Vec<BarSegment> {
    if bar.width < 160 {
        return session_bar_segments(tabs, pills);
    }
    tabs.iter().enumerate().map(|(index, tab)| {
        let status = if tab.live { "●" } else { "○" };
        let title = safe_text::encode_for_display(&tab.title);
        let text = if pills {
            format!("{} {title}", index + 1)
        } else {
            format!("{status} ■ [{}] {}  │", index + 1, title)
        };
        BarSegment {
            text,
            style: if pills {
                pill_style(tab)
            } else if tab.focused {
                theme::style(theme::Role::Focus).add_modifier(Modifier::REVERSED)
            } else {
                theme::style(theme::Role::Text)
            },
            index: Some(index),
            accent: tab.group_color.map(group_palette),
            cap: pills.then(|| pill_cap(tab)),
        }
    }).collect()
}

/// A laid-out session button: label plus area-relative column span.
/// Group headers lay out like buttons but carry no index, so clicks on
/// them never switch sessions.
pub struct SessionButton {
    pub index: Option<usize>,
    pub label: String,
    pub start: u16,
    pub end: u16,
}

/// Lay session segments left to right, clipping at the bar edge instead
/// of wrapping. Buttons keep two-space gaps; a group header takes one
/// trailing space so the run reads `group: 1 a 2 b`. Pill buttons reserve
/// two cells per side (cap plus centering pad), so clicks anywhere on the
/// container still land.
pub fn layout_session_bar(bar: Rect, segments: &[BarSegment]) -> Vec<SessionButton> {
    let mut buttons = Vec::new();
    let mut col = bar.x;
    let edge = bar.x + bar.width;
    for segment in segments {
        let width = (Line::from(segment.text.as_str()).width()
            + if segment.cap.is_some() { 4 } else { 0 })
        .min(u16::MAX as usize) as u16;
        let end = col.saturating_add(width);
        if col >= edge || end > edge {
            break;
        }
        let gap = if segment.index.is_none() { 1 } else { 2 };
        buttons.push(SessionButton {
            index: segment.index,
            label: segment.text.clone(),
            start: col,
            end,
        });
        col = end.saturating_add(gap);
    }
    buttons
}

/// Button index under an area-relative column, if any. Group headers are
/// skipped: clicking one selects nothing.
pub fn session_at(buttons: &[SessionButton], col: u16) -> Option<usize> {
    buttons
        .iter()
        .find(|b| col >= b.start && col < b.end)
        .and_then(|b| b.index)
}

/// Numbered session bar, kept in grid mode: digits exit grid and focus.
/// One pill button: container-colored bookends around the label, with
/// symmetric inner padding so the text sits centered in the container.
/// The padding inherits the label style, keeping filled pills solid.
/// Left and right bookends take separate colors so `Left`-highlight
/// themes can light only the leading edge.
pub(super) fn render_pill(
    frame: &mut Frame,
    area: Rect,
    text: &str,
    style: Style,
    left_cap: Color,
    right_cap: Color,
) {
    let line = Line::from(vec![
        Span::styled(
            crate::ui::theme::pill_left().to_string(),
            Style::default().fg(left_cap),
        ),
        Span::styled(" ".to_string(), style),
        Span::styled(text.to_string(), style),
        Span::styled(" ".to_string(), style),
        Span::styled(
            crate::ui::theme::pill_right().to_string(),
            Style::default().fg(right_cap),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

pub(super) fn render_session_bar(frame: &mut Frame, areas: &ChromeAreas, chrome: &Chrome) {
    if areas.session_bar.height > 0 {
        let segments = session_bar_segments_for_area(&chrome.tabs, areas.session_bar, chrome.pills);
        let buttons = layout_session_bar(areas.session_bar, &segments);
        for (button, segment) in buttons.iter().zip(segments.iter()) {
            let area = Rect::new(button.start, areas.session_bar.y, button.end - button.start, 1);
            if button.index.is_some() {
                if let Some(left) = segment.cap {
                    // The right bookend follows the same tab: Full keeps
                    // both caps lit, Left drops the trailing edge back
                    // to the rest color.
                    let right = segment
                        .index
                        .and_then(|n| chrome.tabs.get(n))
                        .map(|tab| pill_chrome(tab).2)
                        .unwrap_or(left);
                    render_pill(frame, area, &segment.text, segment.style, left, right);
                } else {
                    ChromeButton::new(&button.label, segment.style).view(frame, area);
                }
                // Pills carry their color in the container: no swatch.
                if segment.cap.is_none()
                    && areas.session_bar.width >= 160
                    && area.width >= 3
                {
                    // The swatch overwrites the text ■ with the accent color.
                    let at = area.x + 2;
                    let accent = segment.accent.unwrap_or(theme::style(theme::Role::Muted).fg.unwrap_or(Color::Reset));
                    Label::default().text("■").style(Style::default().fg(accent))
                        .view(frame, Rect::new(at, area.y, 1, 1));
                }
            } else {
                Label::default()
                    .text(safe_text::encode_for_display(&button.label))
                    .style(segment.style)
                    .view(frame, area);
            }
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use crate::ui::render;
    use crate::ui::test_support::{area, chrome, grouped, pane, tab};
    use crate::ui::topbar::{TopTab, layout_topbar};

    #[test]
    fn session_bar_buttons_number_left_to_right() {
        let bar = Rect::new(0, 22, 80, 1);
        let segments = session_bar_segments(&[tab("shell-1", true), tab("shell-2", false)], false);
        let buttons = layout_session_bar(bar, &segments);
        assert_eq!(buttons.len(), 2);
        assert_eq!(buttons[0].label, "1 shell-1");
        assert_eq!((buttons[0].start, buttons[0].end), (0, 9));
        assert_eq!(buttons[1].label, "2 shell-2");
        assert_eq!((buttons[1].start, buttons[1].end), (11, 20));
        // Hit-test lands on labels, not gaps or borders.
        assert_eq!(session_at(&buttons, 0), Some(0));
        assert_eq!(session_at(&buttons, 8), Some(0));
        assert_eq!(session_at(&buttons, 9), None);
        assert_eq!(session_at(&buttons, 11), Some(1));
        assert_eq!(session_at(&buttons, 79), None);
        // Overflow clips instead of wrapping.
        let narrow = layout_session_bar(
            Rect::new(0, 0, 10, 1),
            &session_bar_segments(&[tab("shell-1", true), tab("shell-2", false)], false),
        );
        assert_eq!(narrow.len(), 1);
    }

    #[test]
    fn session_bar_groups_share_one_colored_header() {
        let tabs = vec![
            grouped("a1", true, "codex-proj", 0),
            grouped("a2", false, "codex-proj", 0),
            tab("solo", false),
            grouped("b1", false, "other", 1),
        ];
        let segments = session_bar_segments(&tabs, false);
        let texts: Vec<&str> = segments.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["codex-proj:", "1 a1", "2 a2", "3 solo", "other:", "4 b1"],
            "one header per run, numbering never shifts"
        );
        // Headers are colored with the group palette, never clickable.
        assert_eq!(segments[0].style.fg, Some(group_palette(0)));
        assert_eq!(segments[0].index, None);
        assert_eq!(segments[4].style.fg, Some(group_palette(1)));
        assert_ne!(group_palette(0), group_palette(1));
        // Focused session keeps the focus style.
        assert_eq!(segments[1].style.fg, Some(Color::Yellow));
        let bar = Rect::new(0, 22, 80, 1);
        let buttons = layout_session_bar(bar, &segments);
        assert_eq!(buttons.len(), segments.len());
        // One trailing space after a header: `codex-proj: 1 a1`.
        assert_eq!((buttons[0].start, buttons[0].end), (0, 11));
        assert_eq!((buttons[1].start, buttons[1].end), (12, 16));
        // Clicking the header selects nothing; clicking a member selects it.
        assert_eq!(session_at(&buttons, buttons[0].start), None);
        assert_eq!(session_at(&buttons, buttons[1].start), Some(0));
        assert_eq!(session_at(&buttons, buttons[5].start), Some(3));
        // Palette cycles instead of running out.
        assert_eq!(group_palette(8), group_palette(0));
    }

    #[test]
    fn group_members_share_palette_color_and_focus_stays_visible() {
        let segments = session_bar_segments(&[
            grouped("a", true, "team", 2),
            grouped("b", false, "team", 2),
        ], false);
        assert_eq!(segments[1].style.fg, Some(Color::Yellow));
        assert_eq!(segments[1].style.bg, Some(group_palette(2)));
        assert_eq!(segments[2].style.fg, Some(group_palette(2)));
        assert!(segments[1].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn left_highlight_keeps_group_fill_with_split_caps() {
        let theme = crate::ui::theme::parse_external_theme(
            r##"{"name": "t", "highlight": "left"}"##,
        )
        .expect("left theme parses");
        let _guard = crate::ui::theme::hold_external_theme(theme);
        let g = group_palette(2);
        // Focused grouped tab: group fill stays, left edge lights.
        let (fill, left, right) = pill_chrome(&grouped("a", true, "team", 2));
        assert_eq!(fill.bg, Some(g), "button keeps its group color");
        assert_eq!(left, Color::Yellow, "left edge is the highlight");
        assert_eq!(right, g, "trailing edge rests");
        // Segments carry the same fill through the bar plumbing.
        let segs = session_bar_segments(&[grouped("a", true, "team", 2)], true);
        assert_eq!(segs[0].cap, Some(Color::Yellow));
        assert_eq!(segs[0].style.bg, Some(g));
    }

    #[test]
    fn tab_hit_areas_follow_escaped_display_width() {
        let top = layout_topbar(Rect::new(0, 0, 40, 1), &[
            TopTab { label: "A\n界".into(), active: true },
            TopTab { label: "Terminal".into(), active: false },
        ], false);
        assert_eq!((top[0].start, top[0].end), (1, 7));
        assert_eq!(top[1].start, 9);
        let segments = session_bar_segments(&[tab("a\nb", true), tab("界", false)], false);
        assert_eq!(segments[0].text, "1 a⏎b");
        let buttons = layout_session_bar(Rect::new(0, 0, 40, 1), &segments);
        assert_eq!((buttons[0].start, buttons[0].end), (0, 5));
        assert_eq!(buttons[1].start, 7);
    }

    #[test]
    fn wide_session_strip_uses_status_group_chips_and_clickable_numbers() {
        let tabs = [grouped("jarvis_dev", false, "aura", 0), grouped("web_client", true, "aura", 0),
            grouped("gl_rev", false, "gl", 1)];
        let segments = session_bar_segments_for_area(&tabs, Rect::new(0, 38, 180, 1), false);
        assert_eq!(segments.len(), 3);
        assert!(segments[0].text.contains("● ■ [1] jarvis_dev"));
        assert!(segments[1].text.contains("[2] web_client"));
        assert_eq!(segments[0].accent, Some(group_palette(0)));
        assert_eq!(segments[2].accent, Some(group_palette(1)));
        let buttons = layout_session_bar(Rect::new(0, 38, 180, 1), &segments);
        assert_eq!(session_at(&buttons, buttons[1].start + 5), Some(1));
    }

    #[test]
    fn wide_session_pills_drop_status_markers_and_keep_number() {
        let tabs = [tab("a", true), grouped("b", false, "team", 2)];
        let segments = session_bar_segments_for_area(&tabs, Rect::new(0, 0, 180, 1), true);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].text, "1 a");
        assert_eq!(segments[1].text, "2 b");
        assert!(segments.iter().all(|s| !s.text.contains('●') && !s.text.contains('■')));
    }

    #[test]
    fn pill_segments_fill_group_and_select_overrides() {
        let segs = session_bar_segments(
            &[
                grouped("a", true, "team", 2),
                grouped("b", false, "team", 2),
                tab("c", false),
            ],
            true,
        );
        assert_eq!(segs.len(), 3, "no headers in pills mode");
        assert!(segs.iter().all(|s| s.index.is_some()));
        let order: Vec<_> = segs.iter().map(|s| s.index).collect();
        assert_eq!(order, vec![Some(0), Some(1), Some(2)]);
        // Selected ignores group: default yellow container, dark text.
        assert_eq!(segs[0].cap, Some(Color::Yellow));
        assert_eq!(segs[0].style.bg, Some(Color::Yellow));
        assert_eq!(segs[0].style.fg, Some(Color::Black));
        // Grouped rest fills the group color with dark text.
        let g = group_palette(2);
        assert_eq!(segs[1].cap, Some(g));
        assert_eq!(segs[1].style.bg, Some(g));
        assert_eq!(segs[1].style.fg, Some(Color::Black));
        // Ungrouped rest sits in the dim container with dark text.
        assert_eq!(segs[2].cap, Some(Color::DarkGray));
        assert_eq!(segs[2].style.bg, Some(Color::DarkGray));
        assert_eq!(segs[2].style.fg, Some(Color::Black));
    }

    #[test]
    fn pill_layout_reserves_caps_and_centering_pads() {
        let bar = Rect::new(0, 0, 80, 1);
        let segs = session_bar_segments(&[tab("a", true)], true);
        let buttons = layout_session_bar(bar, &segs);
        // "1 a" (3) + 2 caps + 2 pads.
        assert_eq!(buttons[0].end - buttons[0].start, 7);
        assert_eq!(session_at(&buttons, 0), Some(0), "left cap hits");
        assert_eq!(session_at(&buttons, 1), Some(0), "left pad hits");
        assert_eq!(session_at(&buttons, 6), Some(0), "right cap hits");
        assert_eq!(session_at(&buttons, 7), None, "gap misses");
    }

    #[test]
    fn render_pill_centers_label_in_container() {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut c = chrome();
        c.pills = true;
        terminal.draw(|f| render(f, area(), &[pane("sh", "body", true)], &c)).unwrap();
        let buf = terminal.backend().buffer();
        // chrome() tab "sh" focused: `cap sp 1 sp sh sp cap` on row 23.
        assert_eq!(buf[(0, 23)].symbol(), "");
        assert_eq!(buf[(0, 23)].fg, Color::Yellow);
        assert_eq!(buf[(1, 23)].symbol(), " ");
        assert_eq!(buf[(1, 23)].bg, Color::Yellow, "pad fills container");
        assert_eq!(buf[(7, 23)].symbol(), "");
        let row: String = (0..8).map(|x| buf[(x, 23)].symbol()).collect();
        assert_eq!(row, " 1 sh ");
    }

}
