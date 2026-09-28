//! UI top bar: tabs, buttons, layout, hit-testing, paint.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::Frame;
use tui_realm_stdlib::components::Label;
use tuirealm::command::{Cmd, CmdResult};
use tuirealm::component::Component;
use tuirealm::props::{AttrValue, Attribute, QueryResult};
use tuirealm::state::State;

use crate::infra::safe_text;

use super::session_bar::render_pill;
use super::theme;

/// One per-session tab: the agent CLI tab plus the human terminal tab.
#[derive(Clone, Debug, Default)]
pub struct TopTab {
    pub label: String,
    pub active: bool,
}

/// Per-session tab strip: empty when no session is focused.
#[derive(Clone, Debug, Default)]
pub struct TopBar {
    pub tabs: Vec<TopTab>,
}

/// A laid-out top-bar button: area-relative column span.
pub struct TopButton {
    pub index: usize,
    pub start: u16,
    pub end: u16,
}

/// A small chrome button backed by a tui-realm Label. The standard
/// library has labels and selectors but no button component, so this
/// wrapper gives the label a Submit command for mouse activation.
pub struct ChromeButton {
    label: Label,
}

impl ChromeButton {
    pub fn new(text: &str, style: Style) -> Self {
        Self {
            label: Label::default()
                .text(safe_text::encode_for_display(text))
                .style(style),
        }
    }

    pub fn click(&mut self, col: u16, row: u16, area: Rect) -> bool {
        col >= area.x && col < area.right() && row >= area.y && row < area.bottom()
            && matches!(self.perform(Cmd::Submit), CmdResult::Submit(_))
    }
}

impl Component for ChromeButton {
    fn view(&mut self, frame: &mut Frame, area: Rect) {
        self.label.view(frame, area);
    }
    fn query<'a>(&'a self, attr: Attribute) -> Option<QueryResult<'a>> {
        self.label.query(attr)
    }
    fn attr(&mut self, attr: Attribute, value: AttrValue) {
        self.label.attr(attr, value);
    }
    fn state(&self) -> State {
        State::None
    }
    fn perform(&mut self, cmd: Cmd) -> CmdResult {
        if matches!(cmd, Cmd::Submit) {
            CmdResult::Submit(self.state())
        } else {
            CmdResult::Invalid(cmd)
        }
    }
}

/// Lay tab buttons left to right with two-space gaps, clipping at the edge.
/// Topbar tab layout: `[label]` buttons legacy, pill buttons (caps plus
/// centering pads) when `pills`. Tab 0 always starts one cell in.
pub fn layout_topbar(bar: Rect, tabs: &[TopTab], pills: bool) -> Vec<TopButton> {
    let mut buttons = Vec::new();
    let mut col = bar.x.saturating_add(1);
    let edge = bar.x + bar.width;
    for (index, tab) in tabs.iter().enumerate() {
        let shown = safe_text::encode_for_display(&tab.label);
        let label = if pills { shown } else { format!("[{shown}]") };
        let width = (Line::from(label.as_str()).width() + if pills { 4 } else { 0 })
            .min(u16::MAX as usize) as u16;
        let end = col.saturating_add(width);
        if col >= edge || end > edge {
            break;
        }
        buttons.push(TopButton { index, start: col, end });
        col = end.saturating_add(2);
    }
    buttons
}

/// Pill container for a topbar tab: selected yellow, the rest dim gray.
fn topbar_pill(tab: &TopTab) -> (Style, Color, Color) {
    let (fill, left, right) = theme::button_chrome(
        tab.active,
        theme::style(theme::Role::TabActive),
        theme::style(theme::Role::TabInactive),
        Color::DarkGray,
    );
    (fill, left, right)
}

/// Button index under an area-relative column, if any.
pub fn topbar_at(buttons: &[TopButton], col: u16) -> Option<usize> {
    buttons
        .iter()
        .find(|b| col >= b.start && col < b.end)
        .map(|b| b.index)
}

/// Render one focused session in the main pane with sidebar and session
/// bar. Titles and bodies are untrusted PTY output, so both pass
/// through display encoding: raw escape sequences must never reach the
/// outer terminal.
/// One tab-strip row: `[label]` buttons, the active tab reversed.
pub(super) fn render_topbar(frame: &mut Frame, bar: Rect, tabs: &[TopTab], pills: bool) {
    let buttons = layout_topbar(bar, tabs, pills);
    for button in &buttons {
        let tab = &tabs[button.index];
        let area = Rect::new(button.start, bar.y, button.end - button.start, 1);
        if pills {
            let (style, left, right) = topbar_pill(tab);
            render_pill(frame, area, &safe_text::encode_for_display(&tab.label), style, left, right, false);
        } else {
            let style = if tab.active {
                theme::style(theme::Role::Focus).add_modifier(Modifier::REVERSED)
            } else {
                theme::style(theme::Role::Text)
            };
            let mut control = ChromeButton::new(&format!("[{}]", tab.label), style);
            control.view(frame, area);
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use crate::ui::render;
    use crate::ui::test_support::{area, chrome, pane};

    #[test]
    fn chrome_button_is_a_submittable_tuirealm_component() {
        use tuirealm::command::{Cmd, CmdResult};
        use tuirealm::component::Component;
        let mut button = ChromeButton::new("[Terminal]", theme::style(theme::Role::Focus));
        assert_eq!(button.perform(Cmd::Submit), CmdResult::Submit(tuirealm::state::State::None));
        assert_eq!(button.state(), tuirealm::state::State::None);
    }

    #[test]
    fn topbar_layout_clips_and_hit_tests() {
        let bar = Rect::new(0, 0, 20, 1);
        let tabs = vec![
            TopTab { label: "Codex".to_string(), active: true },
            TopTab { label: "Terminal".to_string(), active: false },
        ];
        let buttons = layout_topbar(bar, &tabs, false);
        assert_eq!(buttons.len(), 2);
        // "[Codex]" spans 1..8, gap, "[Terminal]" spans 10..20.
        assert_eq!(topbar_at(&buttons, 1), Some(0));
        assert_eq!(topbar_at(&buttons, 7), Some(0));
        assert_eq!(topbar_at(&buttons, 8), None, "gap is dead");
        assert_eq!(topbar_at(&buttons, 10), Some(1));
        assert_eq!(topbar_at(&buttons, 0), None, "margin is dead");
        // Narrow bar clips the second tab instead of wrapping.
        let narrow = layout_topbar(Rect::new(0, 0, 12, 1), &tabs, false);
        assert_eq!(narrow.len(), 1);
    }

    #[test]
    fn topbar_layout_measures_wide_emoji_icons() {
        // Emoji icons are two cells: hit areas must use display width,
        // not char count, or clicks land one cell off per icon.
        let tabs = vec![
            TopTab { label: "🤖 Codex".to_string(), active: true },
            TopTab { label: "💻 Terminal".to_string(), active: false },
        ];
        let buttons = layout_topbar(Rect::new(0, 0, 40, 1), &tabs, false);
        assert_eq!(buttons.len(), 2);
        // "[🤖 Codex]" spans 10 cells: brackets + icon + space + name.
        assert_eq!((buttons[0].start, buttons[0].end), (1, 11));
        // "[💻 Terminal]" spans 13 cells starting after the 2-cell gap.
        assert_eq!((buttons[1].start, buttons[1].end), (13, 26));
        assert_eq!(topbar_at(&buttons, 11), None, "gap is dead");
        assert_eq!(topbar_at(&buttons, 13), Some(1));
    }

    #[test]
    fn topbar_pills_widen_and_highlight_selected() {
        let tabs = vec![
            TopTab { label: "Codex".into(), active: true },
            TopTab { label: "Terminal".into(), active: false },
        ];
        let bar = Rect::new(0, 0, 40, 1);
        let buttons = layout_topbar(bar, &tabs, true);
        // "Codex" (5) + caps/pads spans 1..10; "Terminal" (8) + 4 spans 12..24.
        assert_eq!((buttons[0].start, buttons[0].end), (1, 10));
        assert_eq!((buttons[1].start, buttons[1].end), (12, 24));
        assert_eq!(topbar_at(&buttons, 1), Some(0), "left cap hits");
        assert_eq!(topbar_at(&buttons, 9), Some(0), "right cap hits");
        assert_eq!(topbar_at(&buttons, 10), None, "gap is dead");
    }

    #[test]
    fn render_topbar_pills_fill_selected() {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut c = chrome();
        c.pills = true;
        terminal.draw(|f| render(f, area(), &[pane("sh", "body", true)], &c)).unwrap();
        let buf = terminal.backend().buffer();
        // Helper topbar leads with active "Shell" at x1.
        let row: String = (1..10).map(|x| buf[(x, 0)].symbol()).collect();
        assert_eq!(row, "\u{e0b6} Shell \u{e0b4}");
        assert_eq!(buf[(1, 0)].fg, Color::Yellow, "selected cap");
        assert_eq!(buf[(2, 0)].bg, Color::Yellow, "selected fill");
        // Next pill starts at x12 (2-cell gap): cap, pad, text.
        assert_eq!(buf[(13, 0)].bg, Color::DarkGray, "inactive fill");
        assert_eq!(buf[(14, 0)].fg, Color::Black, "inactive dark label");
    }

}
