//! Testable view layer: grid geometry plus rendering from plain view models.
//!
//! Content generation stays separate from frame rendering: [`render_grid`]
//! takes snapshots, so the whole grid is assertable through a test backend
//! without a terminal.

use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use tui_realm_stdlib::components::Label;
use tuirealm::command::{Cmd, CmdResult};
use tuirealm::component::Component;
use tuirealm::props::{AttrValue, Attribute, QueryResult};
use tuirealm::state::State;

use crate::{
    pty::{CellColor, CellFormat, FormattedCell},
    safe_text, theme,
};

/// One styled run of pane text.
#[derive(Clone, Debug)]
pub struct SpanView {
    pub text: String,
    pub style: Style,
}

/// Map one parsed cell format to a render style. App colors pass through
/// raw; the semantic theme stays chrome-only by design.
pub fn style_for(format: &CellFormat) -> Style {
    let mut style = Style::default();
    if let Some(fg) = map_color(format.fg) {
        style = style.fg(fg);
    }
    if let Some(bg) = map_color(format.bg) {
        style = style.bg(bg);
    }
    if format.bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if format.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if format.underline {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if format.inverse {
        style = style.add_modifier(Modifier::REVERSED);
    }
    // Faint ghost/prediction text (SGR 2): real terminals render this dim
    // gray instead of full-bright.
    if format.dim {
        style = style.add_modifier(Modifier::DIM);
    }
    style
}

/// Encode one cell for display and attach its style. Cells never contain
/// newlines (rows are structural now), so single-line encoding is exact.
pub fn span_for(cell: &FormattedCell) -> SpanView {
    SpanView {
        text: safe_text::encode_for_display(&cell.text),
        style: style_for(&cell.format),
    }
}

fn map_color(color: CellColor) -> Option<Color> {
    match color {
        CellColor::Default => None,
        CellColor::Indexed(i) => Some(Color::Indexed(i)),
        CellColor::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

/// One pane's renderable snapshot.
pub struct PaneView {
    pub title: String,
    pub lines: Vec<Vec<SpanView>>,
    pub live: bool,
    pub focused: bool,
    /// Visible cursor as 0-based (row, col) in terminal-grid coordinates.
    pub cursor: Option<(u16, u16)>,
}

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
            spans.push(SpanView { text: crate::theme::pill_left().to_string(), style: cap_left });
            spans.push(SpanView { text: " ".to_string(), style });
            spans.push(SpanView { text: text.to_string(), style });
            spans.push(SpanView { text: " ".to_string(), style });
            spans.push(SpanView { text: crate::theme::pill_right().to_string(), style: cap_right });
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

/// Wrap one row of spans to `width` display cells, splitting overlong
/// spans on char boundaries with wide chars counting double. Styles
/// ride along on every piece; empty input yields no rows.
#[cfg(feature = "visual")]
pub fn wrap_spans(spans: Vec<SpanView>, width: u16) -> Vec<Vec<SpanView>> {
    use ratatui::text::Line;
    fn char_width(c: char) -> usize {
        let mut buf = [0u8; 4];
        Line::from(c.encode_utf8(&mut buf) as &str).width()
    }
    let width = width.max(1) as usize;
    let mut rows: Vec<Vec<SpanView>> = Vec::new();
    let mut cur: Vec<SpanView> = Vec::new();
    let mut used = 0usize;
    for span in spans {
        let chars: Vec<char> = span.text.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let room = width.saturating_sub(used);
            if room == 0 {
                rows.push(std::mem::take(&mut cur));
                used = 0;
                continue;
            }
            let mut acc = 0usize;
            let mut j = i;
            while j < chars.len() {
                let cw = char_width(chars[j]);
                if acc + cw > room {
                    break;
                }
                acc += cw;
                j += 1;
            }
            if j == i {
                j = i + 1;
                acc = char_width(chars[i]);
            }
            cur.push(SpanView {
                text: chars[i..j].iter().collect(),
                style: span.style,
            });
            used += acc;
            i = j;
        }
    }
    if !cur.is_empty() {
        rows.push(cur);
    }
    rows
}

/// Display width of one span row in cells, wide chars counting
/// double: the same measure [`wrap_spans`] splits on.
#[cfg(feature = "visual")]
pub fn spans_width(row: &[SpanView]) -> usize {
    use ratatui::text::Line;
    row.iter().map(|s| Line::from(s.text.as_str()).width()).sum()
}

/// Cut one span row to `max` display cells, ending a cut row with an
/// ellipsis that inherits the cut span's style. Fitting rows pass
/// through untouched; wide chars never split. Shortens one-row UI
/// suffixes (like the Visual strip alt text) without touching the
/// buttons and hints ahead of them.
#[cfg(feature = "visual")]
pub fn truncate_spans(spans: Vec<SpanView>, max: u16) -> Vec<SpanView> {
    use ratatui::text::Line;
    fn char_width(c: char) -> usize {
        let mut buf = [0u8; 4];
        Line::from(c.encode_utf8(&mut buf) as &str).width()
    }
    let max = max as usize;
    if max == 0 {
        return Vec::new();
    }
    let total: usize = spans
        .iter()
        .flat_map(|s| s.text.chars())
        .map(char_width)
        .sum();
    if total <= max {
        return spans;
    }
    let mut out: Vec<SpanView> = Vec::new();
    let mut piece = String::new();
    let mut piece_style = Style::default();
    let mut started = false;
    let mut used = 0usize;
    let mut mark_style = Style::default();
    let budget = max.saturating_sub(1);
    'spans: for span in &spans {
        for c in span.text.chars() {
            let cw = char_width(c);
            if used + cw > budget {
                mark_style = span.style;
                break 'spans;
            }
            if !started {
                piece_style = span.style;
                started = true;
            }
            piece.push(c);
            used += cw;
        }
        if started {
            out.push(SpanView { text: std::mem::take(&mut piece), style: piece_style });
            started = false;
        }
    }
    if started {
        out.push(SpanView { text: std::mem::take(&mut piece), style: piece_style });
    }
    out.push(SpanView { text: "…".to_string(), style: mark_style });
    out
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
pub const PILL_LEFT: char = crate::theme::BUILTIN_PILL_LEFT;
pub const PILL_RIGHT: char = crate::theme::BUILTIN_PILL_RIGHT;

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

/// One armed self-injection timer for the sidebar: stable ID plus a
/// preformatted countdown (`9:55`), soonest first.
#[derive(Clone, Debug)]
pub struct TimerView {
    pub id: String,
    pub remaining: String,
}

/// Sidebar content source: the focused session's detail plus stats,
/// pending hooks, and the permission mode buttons.
#[derive(Clone, Debug)]
pub struct SessionDetail {
    pub name: String,
    pub cli_tool: String,
    pub cwd: String,
    pub state: String,
    /// Caller sticky status text (`kind: message`), if one is set.
    pub status: Option<String>,
    /// Kind behind `status`, for the signal emoji. `None` matches `status`.
    pub status_kind: Option<crate::session_status::StatusKind>,
    /// Armed timers of the focused session only; empty hides the section.
    pub timers: Vec<TimerView>,
}

/// Fleet attention tier. Sorts Attention first, then Working, then
/// Idle; the mark carries meaning alone, color only reinforces it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FleetTier {
    Attention,
    Working,
    Idle,
}

impl FleetTier {
    pub fn mark(self) -> &'static str {
        match self {
            FleetTier::Attention => "!",
            FleetTier::Working => "~",
            FleetTier::Idle => " ",
        }
    }
}

/// One router row: every session, attention-first. The reason is the
/// attention/sticky text verbatim (already display-encoded).
#[derive(Clone, Debug)]
pub struct FleetRow {
    pub id: crate::session::SessionId,
    pub name: String,
    pub tier: FleetTier,
    pub state: String,
    pub reason: String,
    /// Signal emoji (`🔔` for pings, the sticky kind glyph otherwise,
    /// empty when there is nothing to signal). Marks still carry the
    /// meaning alone; this rides beside them.
    pub emoji: &'static str,
}

/// Signal emoji per sticky kind. Bare glyphs only: no variation
/// selectors, no ZWJ sequences (same rule as the topbar icons), so
/// `Line::width` measures them exactly.
pub fn status_emoji(kind: crate::session_status::StatusKind) -> &'static str {
    match kind {
        crate::session_status::StatusKind::Info => "ℹ",
        crate::session_status::StatusKind::Progress => "🔄",
        crate::session_status::StatusKind::Success => "✔",
        crate::session_status::StatusKind::Warning => "⚠",
        crate::session_status::StatusKind::Blocked => "🛑",
        crate::session_status::StatusKind::Question => "💬",
    }
}

/// Cut a string to at most `max_cells` display cells without splitting
/// a character. Short strings pass through untouched.
pub fn cut_cells(s: &str, max_cells: usize) -> String {
    if Line::from(s).width() <= max_cells {
        return s.to_string();
    }
    let mut out = String::new();
    for ch in s.chars() {
        let trial = format!("{out}{ch}");
        if Line::from(trial.as_str()).width() > max_cells {
            break;
        }
        out = trial;
    }
    out
}

/// Cell width of a string as the renderer measures it.
fn cells(s: &str) -> usize {
    Line::from(s).width()
}

/// Wrap a string to lines of at most `max_cells` display cells,
/// splitting on spaces and hard-splitting words longer than the
/// budget. Never returns an empty vec.
pub fn wrap_cells(s: &str, max_cells: usize) -> Vec<String> {
    let max_cells = max_cells.max(1);
    if s.split(' ').all(|w| w.is_empty()) {
        return Vec::new();
    }
    let mut rows = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0;
    let push_word = |word: &str, rows: &mut Vec<String>, cur: &mut String, cur_w: &mut usize| {
        let w = cells(word);
        if w > max_cells {
            // Hard-split the long word across rows.
            let mut chunk = String::new();
            let mut chunk_w = 0;
            for ch in word.chars() {
                let cw = cells(&ch.to_string());
                if chunk_w + cw > max_cells {
                    rows.push(std::mem::take(&mut chunk));
                    chunk_w = 0;
                }
                chunk.push(ch);
                chunk_w += cw;
            }
            if *cur_w > 0 {
                rows.push(std::mem::take(cur));
                *cur_w = 0;
            }
            *cur = chunk;
            *cur_w = cells(cur);
            return;
        }
        let sep = if *cur_w > 0 { 1 } else { 0 };
        if *cur_w + sep + w > max_cells {
            rows.push(std::mem::take(cur));
            *cur_w = 0;
        } else if sep > 0 {
            cur.push(' ');
            *cur_w += 1;
        }
        cur.push_str(word);
        *cur_w += w;
    };
    for word in s.split(' ') {
        if word.is_empty() {
            continue;
        }
        push_word(word, &mut rows, &mut cur, &mut cur_w);
    }
    if !cur.is_empty() {
        rows.push(cur);
    }
    rows
}

#[derive(Clone, Debug)]
pub struct SidebarInfo {
    pub session: Option<SessionDetail>,
    /// Every session, attention-first; the fleet slot scrolls past the cap.
    pub sessions: Vec<FleetRow>,
    /// Focused session for the `*` mark.
    pub active: Option<crate::session::SessionId>,
    /// Fleet cursor for `>` plus keyboard activation.
    pub fleet_cursor: Option<crate::session::SessionId>,
    /// Clamped scroll offset into `sessions`.
    pub fleet_scroll: usize,
    /// Armed timers on non-focused sessions, collapsed to a count.
    pub other_timers: usize,
    pub pending: usize,
    /// "off" or "yolo": drives which settings button highlights.
    pub mode: &'static str,
    /// "on" or "off": Telegram mobile transport state.
    pub telegram: &'static str,
    /// Latest `message_user` badge as `(session, text)`, if any. The
    /// badge row renders only when present, last in the panel where no
    /// interactive row can shift.
    pub telegram_badge: Option<(String, String)>,
}

/// Explicit sidebar regions: the list scrolls, the footer never
/// moves. Render, mouse, and keys share this split so a repaint can
/// never desync controls from the paint.
pub struct SidebarLayout {
    pub list: Rect,
    pub footer: Rect,
}

/// Rich layout gate: wide and tall enough for the brand header,
/// status rows, and settings explainers. Calibrated to the 36-column
/// clamp — a plain `>= 40` would make rich unreachable.
pub fn sidebar_is_rich(sidebar: Rect) -> bool {
    sidebar.width >= 30 && sidebar.height >= 30
}

/// Pinned footer height: settings rows, plus the badge row when
/// present. Compact also spends a pending row, hidden when there is
/// nothing pending (rich pending lives in the focused block instead).
pub fn sidebar_footer_height(info: &SidebarInfo, rich: bool) -> u16 {
    let base = if rich { 6 } else { 5 };
    base + u16::from(info.telegram_badge.is_some())
        - u16::from(!rich && info.pending == 0)
}

pub fn sidebar_layout(sidebar: Rect, footer_h: u16) -> SidebarLayout {
    let footer_h = footer_h.min(sidebar.height);
    SidebarLayout {
        footer: Rect::new(
            sidebar.x,
            sidebar.y + sidebar.height - footer_h,
            sidebar.width,
            footer_h,
        ),
        list: Rect::new(sidebar.x, sidebar.y, sidebar.width, sidebar.height - footer_h),
    }
}

/// Whole blocks that fit the cell budget starting at `scroll`:
/// scroll saturates at the tail, and the first block always shows even
/// when it alone overflows the budget.
pub fn fleet_cell_window(scroll: usize, heights: &[u16], budget: u16) -> (usize, usize) {
    if heights.is_empty() {
        return (0, 0);
    }
    let start = scroll.min(heights.len() - 1);
    let mut used = 0u16;
    let mut end = start;
    while end < heights.len() {
        let h = heights[end].max(1);
        if end > start && used + h > budget {
            break;
        }
        used += h;
        end += 1;
    }
    (start, end)
}

/// Painted height per fleet block in row order: name, wrapped reason,
/// breathing room. Render, hit-testing, and cursor-following share
/// this, so variable heights can never desync them.
pub fn fleet_block_heights(info: &SidebarInfo, width: u16) -> Vec<u16> {
    info.sessions
        .iter()
        .map(|row| {
            1 + wrap_cells(
                &safe_text::encode_for_display(&row.reason),
                fleet_wrap_width(width),
            )
            .len() as u16
                + 1
        })
        .collect()
}

/// One rendered fleet item: a zone header or a session block by row
/// index. Headers travel with their blocks through the same budgeted
/// window, so a header can never strand without its rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FleetItem {
    AttnHeader,
    SessionsHeader,
    Block(usize),
}

/// Grouped window over the fleet: attention blocks under their header,
/// the rest under theirs, whole items while they fit the cell budget.
/// A trailing stranded header is dropped; an empty window means the
/// list region is too small for even one header.
pub fn fleet_window_items(
    info: &SidebarInfo,
    width: u16,
    items_budget: u16,
) -> Vec<FleetItem> {
    let n = info.sessions.len();
    if n == 0 {
        return Vec::new();
    }
    let start = info.fleet_scroll.min(n - 1);
    let heights = fleet_block_heights(info, width);
    let mut items = Vec::new();
    let attn: Vec<usize> = (0..n)
        .filter(|&i| i >= start && info.sessions[i].tier == FleetTier::Attention)
        .collect();
    let rest: Vec<usize> = (0..n)
        .filter(|&i| i >= start && info.sessions[i].tier != FleetTier::Attention)
        .collect();
    if !attn.is_empty() {
        items.push(FleetItem::AttnHeader);
        items.extend(attn.into_iter().map(FleetItem::Block));
    }
    if !rest.is_empty() {
        items.push(FleetItem::SessionsHeader);
        items.extend(rest.into_iter().map(FleetItem::Block));
    }
    let height_of = |item: &FleetItem| -> u16 {
        match item {
            FleetItem::AttnHeader | FleetItem::SessionsHeader => 1,
            FleetItem::Block(i) => heights[*i].max(1),
        }
    };
    let mut used = 0u16;
    let mut end = 0;
    for (k, item) in items.iter().enumerate() {
        if k > 0 && used + height_of(item) > items_budget {
            break;
        }
        used += height_of(item);
        end = k + 1;
    }
    while end > 0 && matches!(items[end - 1], FleetItem::AttnHeader | FleetItem::SessionsHeader) {
        end -= 1;
    }
    items.truncate(end);
    items
}

/// Fleet items budget inside the list region: list height minus the
/// brand, focused block, gap row, and title row above the items.
pub fn fleet_items_budget(info: &SidebarInfo, rich: bool, width: u16, list_h: u16) -> u16 {
    let top = (if rich { 3 } else { 0 }) + focused_block_lines(info, rich, width).len() + 1;
    list_h.saturating_sub(top as u16 + 1)
}

/// Reason wrap width: two indent cells plus a margin off the edge.
fn fleet_wrap_width(width: u16) -> usize {
    (width as usize).saturating_sub(4).max(8)
}

/// Name budget on the block headline: selector, mark, emoji, gaps.
fn fleet_name_budget(width: u16, emoji: &str) -> usize {
    (width as usize).saturating_sub(6 + cells(emoji)).max(4)
}

/// Fleet slot lines: header plus the visible window, one airy block
/// per session — name headline, wrapped reason, breathing room.
/// `>` marks the cursor, `*` the focused session, otherwise the tier
/// mark; emoji ride beside the marks, never alone.
/// Fleet slot lines from a grouped window: zoned title, attention
/// zone with a Danger accent, then the sessions zone. `>` marks the
/// cursor, `*` the focused session, otherwise the tier mark; emoji
/// ride beside the marks, never alone. The cursor row takes full-row
/// Focus reverse; idle rows sink to Muted; the active name is Brand.
fn fleet_block_lines(info: &SidebarInfo, width: u16, items: &[FleetItem]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let total = info.sessions.len();
    if total == 0 {
        lines.push(Line::from(Span::styled(
            " sessions · 0",
            theme::style(theme::Role::Brand),
        )));
        lines.push(Line::from("  none yet"));
        lines.push(Line::from("  Ctrl-b c creates one"));
        return lines;
    }
    let shown: Vec<usize> = items
        .iter()
        .filter_map(|it| match it {
            FleetItem::Block(i) => Some(*i),
            FleetItem::AttnHeader | FleetItem::SessionsHeader => None,
        })
        .collect();
    // Window position over the sorted fleet: min–max, since grouping
    // can interleave the shown blocks out of index order.
    let title = match (shown.iter().min(), shown.iter().max()) {
        (Some(first), Some(last)) if shown.len() < total => {
            format!(" Fleet ({total}) · {}–{}", first + 1, last + 1)
        }
        _ => format!(" Fleet ({total})"),
    };
    lines.push(Line::from(Span::styled(title, theme::style(theme::Role::Brand))));
    for item in items {
        match item {
            FleetItem::AttnHeader => {
                let k = items
                    .iter()
                    .filter(|it| matches!(it, FleetItem::Block(i) if info.sessions[*i].tier == FleetTier::Attention))
                    .count();
                lines.push(Line::from(Span::styled(
                    format!(" NEEDS ATTENTION {k}"),
                    theme::style(theme::Role::Brand),
                )));
            }
            FleetItem::SessionsHeader => {
                lines.push(Line::from(Span::styled(
                    " SESSIONS".to_string(),
                    theme::style(theme::Role::Brand),
                )));
            }
            FleetItem::Block(i) => {
                let row = &info.sessions[*i];
                let cursor = info.fleet_cursor == Some(row.id);
                let sel = if cursor {
                    ">"
                } else if info.active == Some(row.id) {
                    "*"
                } else {
                    " "
                };
                let raw_name = safe_text::encode_for_display(&row.name);
                let name = cut_cells(&raw_name, fleet_name_budget(width, row.emoji));
                let headline = format!("{}{}{} {}", sel, row.tier.mark(), row.emoji, name);
                if cursor {
                    let style = theme::style(theme::Role::Focus)
                        .add_modifier(Modifier::REVERSED);
                    lines.push(Line::from(vec![
                        Span::styled(format!(" {headline}"), style),
                    ]));
                    for wrapped in wrap_cells(
                        &safe_text::encode_for_display(&row.reason),
                        fleet_wrap_width(width),
                    ) {
                        lines.push(Line::from(vec![
                            Span::styled(format!("  {wrapped}"), style),
                        ]));
                    }
                } else {
                    let name_style = if info.active == Some(row.id) {
                        theme::style(theme::Role::Brand)
                    } else if row.tier == FleetTier::Idle {
                        theme::style(theme::Role::Muted)
                    } else {
                        theme::style(theme::Role::Text)
                    };
                    if row.tier == FleetTier::Attention {
                        lines.push(Line::from(vec![
                            Span::styled("┃", theme::style(theme::Role::Danger)),
                            Span::styled(headline, name_style),
                        ]));
                        for wrapped in wrap_cells(
                            &safe_text::encode_for_display(&row.reason),
                            fleet_wrap_width(width),
                        ) {
                            lines.push(Line::from(vec![
                                Span::styled("┃", theme::style(theme::Role::Danger)),
                                Span::styled(format!(" {wrapped}"), name_style),
                            ]));
                        }
                    } else {
                        lines.push(Line::from(vec![
                            Span::raw(" "),
                            Span::styled(headline, name_style),
                        ]));
                        for wrapped in wrap_cells(
                            &safe_text::encode_for_display(&row.reason),
                            fleet_wrap_width(width),
                        ) {
                            lines.push(Line::from(vec![
                                Span::styled(format!("  {wrapped}"), name_style),
                            ]));
                        }
                    }
                }
                lines.push(Line::from(""));
            }
        }
    }
    lines
}

/// Click areas for fleet blocks: name plus reason rows over the same
/// window the render paints, so clicks can never desync from what is
/// on screen. Gap rows stay dead.
pub fn sidebar_session_rects(
    sidebar: Rect,
    info: &SidebarInfo,
    rich: bool,
) -> Vec<(crate::session::SessionId, Rect)> {
    let width = sidebar.width;
    let layout = sidebar_layout(sidebar, sidebar_footer_height(info, rich));
    let items = fleet_window_items(
        info,
        width,
        fleet_items_budget(info, rich, width, layout.list.height),
    );
    let heights = fleet_block_heights(info, width);
    // Title row sits after the brand, focused block, and gap row.
    let title_idx =
        (if rich { 3 } else { 0 }) + focused_block_lines(info, rich, width).len() + 1;
    let mut y = layout.list.y + 1 + title_idx as u16 + 1;
    let mut out = Vec::new();
    for item in &items {
        match item {
            FleetItem::AttnHeader | FleetItem::SessionsHeader => {
                y += 1;
            }
            FleetItem::Block(i) => {
                let h = heights[*i];
                // The click area covers name plus reason rows; the gap
                // and headers stay dead.
                let w = width.saturating_sub(2);
                out.push((
                    info.sessions[*i].id,
                    Rect::new(sidebar.x + 1, y, w, h.saturating_sub(1).max(1)),
                ));
                y += h;
            }
        }
    }
    out
}

/// Compact sidebar mode-button row. Taller sidebars pin it to the bottom.
pub const SETTINGS_ROW: u16 = 11;

/// Compact countdown: `0:07`, `9:55`, `1:00:00`. Past-due saturates.
pub fn format_countdown(remaining: std::time::Duration) -> String {
    let secs = remaining.as_secs();
    let (hours, mins, secs) = (secs / 3600, secs % 3600 / 60, secs % 60);
    if hours > 0 {
        format!("{hours}:{mins:02}:{secs:02}")
    } else {
        format!("{mins}:{secs:02}")
    }
}

/// Sidebar lines. Never blank: with no sessions it still guides. The
/// active mode button renders highlighted. Width 24 stands in for the
/// narrowest compact sidebar.
pub fn sidebar_lines(info: &SidebarInfo) -> Vec<Line<'static>> {
    sidebar_lines_at(info, false, 24, 30).0
}

/// Focused-session block for the compact layout: detail first, fleet
/// below. Shared by the render and the fleet-offset math so clicks
/// track the paint.
fn compact_focused_lines(info: &SidebarInfo, _width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(" Session")];
    let Some(detail) = info.session.as_ref() else {
        lines.push(Line::from(" No session selected"));
        return lines;
    };
    lines.push(Line::from(format!(
        "  {}",
        safe_text::encode_for_display(&detail.name)
    )));
    lines.push(Line::from(format!(
        "  {} {}",
        safe_text::encode_for_display(&detail.cli_tool),
        safe_text::encode_for_display(&detail.state)
    )));
    lines.push(Line::from(format!(
        "  {}",
        safe_text::encode_for_display(&detail.cwd)
    )));
    if let Some(status) = detail.status.as_deref() {
        let glyph = detail
            .status_kind
            .map(|k| format!("{} ", status_emoji(k)))
            .unwrap_or_default();
        lines.push(Line::from(format!(
            "  {glyph}{}",
            safe_text::encode_for_display(status)
        )));
    }
    if !detail.timers.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(" Scheduled"));
        for timer in &detail.timers {
            lines.push(Line::from(format!("  ◷ in {}", timer.remaining)));
        }
    }
    if info.other_timers > 0 {
        lines.push(Line::from(format!("  ◷ +{} elsewhere", info.other_timers)));
    }
    lines
}

/// Painted sidebar: exactly `height` rows. The list part (brand,
/// focused block, budgeted fleet window) is clipped to the list
/// region; the footer is always appended whole, so long content clips
/// the list and never the controls. Render, hit-testing, and buttons
/// share this one builder.
pub struct SidebarPaint {
    pub lines: Vec<Line<'static>>,
    /// Content row of the mode-button widgets (footer-relative).
    pub btn_row: usize,
}

pub fn sidebar_paint(
    info: &SidebarInfo,
    rich: bool,
    width: u16,
    height: u16,
    pills: bool,
) -> SidebarPaint {
    let footer_h = sidebar_footer_height(info, rich) as usize;
    let height = height as usize;
    let list_h = height.saturating_sub(footer_h);
    let mut list = Vec::new();
    if rich {
        list.push(Line::from(Span::styled(" Forge", theme::style(theme::Role::Brand))));
        list.push(Line::from(Span::styled(
            " Session Control Plane",
            theme::style(theme::Role::Muted),
        )));
        list.push(Line::from(""));
    }
    list.extend(focused_block_lines(info, rich, width));
    list.push(Line::from(""));
    let items = fleet_window_items(
        info,
        width,
        fleet_items_budget(info, rich, width, list_h as u16),
    );
    list.extend(fleet_block_lines(info, width, &items));
    list.truncate(list_h);
    while list.len() < list_h {
        list.push(Line::from(""));
    }
    let btn_row = list_h + if rich { 3 } else { 1 };
    let mut lines = list;
    lines.extend(footer_lines(info, rich, pills));
    SidebarPaint { lines, btn_row }
}

/// Pinned footer rows, exactly `sidebar_footer_height` long: settings,
/// mode buttons, shortcuts, transport, badge. The button line carries
/// the same text the overlay widgets paint, so content readers agree
/// with the buffer.
fn footer_lines(info: &SidebarInfo, rich: bool, pills: bool) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if rich {
        lines.push(Line::from(Span::styled(
            " Global Settings",
            theme::style(theme::Role::Brand),
        )));
        lines.push(Line::from(" Autopilot"));
        lines.push(Line::from(""));
    } else {
        lines.push(Line::from(" Settings"));
    }
    let (off_style, yolo_style) = if info.mode == "yolo" {
        (Style::default(), Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED))
    } else {
        (Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED), Style::default())
    };
    lines.push(if pills {
        // Pill mode buttons: rounded ends, centered labels, the active
        // mode filled. The overlay widgets below paint the same cells.
        let (off_style, yolo_style) = if info.mode == "yolo" {
            (
                theme::style(theme::Role::TabInactive),
                theme::style(theme::Role::TabActive),
            )
        } else {
            (
                theme::style(theme::Role::TabActive),
                theme::style(theme::Role::TabInactive),
            )
        };
        let (off_cap, yolo_cap) = if info.mode == "yolo" {
            (Color::DarkGray, Color::Yellow)
        } else {
            (Color::Yellow, Color::DarkGray)
        };
        Line::from(vec![
            Span::raw("  "),
            Span::styled(crate::theme::pill_left().to_string(), Style::default().fg(off_cap)),
            Span::styled(" Off ", off_style),
            Span::styled(crate::theme::pill_right().to_string(), Style::default().fg(off_cap)),
            Span::raw(" "),
            Span::styled(crate::theme::pill_left().to_string(), Style::default().fg(yolo_cap)),
            Span::styled(" Yolo ", yolo_style),
            Span::styled(crate::theme::pill_right().to_string(), Style::default().fg(yolo_cap)),
        ])
    } else {
        Line::from(vec![
            Span::raw("  "),
            Span::styled("[Off]", off_style),
            Span::raw(" "),
            Span::styled("[Yolo]", yolo_style),
        ])
    });
    if rich {
        lines.push(Line::from(Span::styled(
            " Ctrl-b shortcuts",
            theme::style(theme::Role::KeyHint),
        )));
    } else {
        lines.push(Line::from(""));
        // Zero pending hides: the footer only spends a row when there
        // is something to show.
        if info.pending > 0 {
            lines.push(Line::from(format!(" Pending: {}", info.pending)));
        }
    }
    lines.push(telegram_line(info.telegram));
    if let Some(badge) = telegram_badge_line(&info.telegram_badge) {
        lines.push(badge);
    }
    lines
}

fn sidebar_lines_at(
    info: &SidebarInfo,
    pills: bool,
    width: u16,
    height: u16,
) -> (Vec<Line<'static>>, usize) {
    let paint = sidebar_paint(info, false, width, height, pills);
    (paint.lines, paint.btn_row)
}

/// Mode-button hit areas on the pinned footer row: the paint puts the
/// buttons at the same footer-relative row, so clicks never desync
/// from what is on screen no matter how long the list above gets.
pub fn footer_mode_buttons(
    sidebar: Rect,
    info: &SidebarInfo,
    rich: bool,
    pills: bool,
) -> ModeButtons {
    let layout = sidebar_layout(sidebar, sidebar_footer_height(info, rich));
    // Content row i paints at screen y+1+i (border consumes the first
    // row), so the footer-relative button row shifts one down.
    mode_button_areas_at(
        sidebar,
        pills,
        layout.footer.y + 1 + if rich { 3 } else { 1 },
    )
}

/// Focused-session block for the rich layout, shared by the render
/// and the fleet-offset math so clicks track the paint.
fn rich_focused_lines(info: &SidebarInfo, width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(" Session", theme::style(theme::Role::Text).add_modifier(Modifier::BOLD))),
    ];
    let Some(detail) = info.session.as_ref() else {
        lines.push(Line::from(" No session selected"));
        return lines;
    };
    lines.push(Line::from(Span::styled(
        format!(" {}", safe_text::encode_for_display(&detail.name)), theme::style(theme::Role::Focus))));
    lines.push(Line::from(format!(" {}", safe_text::encode_for_display(&detail.cli_tool))));
    lines.push(Line::from(format!(" {}", safe_text::encode_for_display(&detail.cwd))));
    if let Some(status) = detail.status.as_deref() {
        let glyph = detail
            .status_kind
            .map(|k| format!("{} ", status_emoji(k)))
            .unwrap_or_default();
        lines.push(Line::from(format!(
            " {glyph}{}",
            safe_text::encode_for_display(status)
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::raw(" Status  "),
        Span::styled(format!("● {}", safe_text::encode_for_display(&detail.state)),
            theme::style(theme::Role::Running)),
    ]));
    if info.pending > 0 {
        lines.push(Line::from(format!(" Pending hooks: {}", info.pending)));
    }
    lines.push(Line::from(""));
    if !detail.timers.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            " Scheduled",
            theme::style(theme::Role::Text).add_modifier(Modifier::BOLD),
        )));
        for timer in &detail.timers {
            lines.push(Line::from(format!("  ◷ in {}", timer.remaining)));
        }
    }
    if info.other_timers > 0 {
        lines.push(Line::from(format!("  ◷ +{} elsewhere", info.other_timers)));
    }
    lines.push(rule_line(width));
    lines
}

/// Focused block dispatcher for the fleet offset: same branch the
/// render takes, so the header index always matches the paint.
fn focused_block_lines(info: &SidebarInfo, rich: bool, width: u16) -> Vec<Line<'static>> {
    if rich {
        rich_focused_lines(info, width)
    } else {
        compact_focused_lines(info, width)
    }
}

fn rich_sidebar_lines(info: &SidebarInfo, width: u16, height: u16) -> Vec<Line<'static>> {
    // Text keeps one indent cell off the border; rules share the
    // narrower measure so the right edge stays aligned.
    sidebar_paint(info, true, width, height, false).lines
}

/// Telegram transport row: state plus the settings key. Appended after
/// the pinned rows so it never shifts mode buttons or shortcuts.
fn telegram_line(state: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" Telegram {state}"), theme::style(theme::Role::Text)),
        Span::styled(" · Ctrl-b m", theme::style(theme::Role::KeyHint)),
    ])
}

/// Latest operator badge row, if any: session plus escaped text capped
/// at 48 chars. Raw input stays the policy input; only the display is
/// encoded (see [`safe_text::encode_for_display`]).
fn telegram_badge_line(badge: &Option<(String, String)>) -> Option<Line<'static>> {
    let (session, text) = badge.as_ref()?;
    let raw = format!("{session}: {text}");
    let cut = cut_cells(&raw, 48);
    Some(Line::from(format!(" {}", safe_text::encode_for_display(&cut))))
}

/// Divider rule: one indent cell, then dashes to the common right edge.
fn rule_line(width: u16) -> Line<'static> {
    Line::from(format!(" {}", "─".repeat(width.saturating_sub(6) as usize)))
}

/// Click areas for the mode buttons, relative to the sidebar rect. Row is
/// fixed by [`SETTINGS_ROW`]; columns match the rendered button spans.
pub struct ModeButtons {
    pub off: Rect,
    pub yolo: Rect,
}

/// Cancel-button rects for the painted Scheduled rows, keyed by timer
/// ID. Recomputed from live info by the render and mouse paths alike,
/// so both always agree on which row cancels which timer. The header
/// matches exactly plus a timer-row lookahead, so a session literally
/// named "Scheduled" can never hijack it; clipped rows get no button.
pub(crate) fn timer_cancel_rects(
    sidebar: Rect,
    info: &SidebarInfo,
    pills: bool,
) -> Vec<(String, Rect)> {
    let Some(detail) = info.session.as_ref() else {
        return Vec::new();
    };
    if detail.timers.is_empty() {
        return Vec::new();
    }
    // Row math only needs the y, which pills never move.
    let rich = sidebar_is_rich(sidebar);
    let lines = sidebar_paint(info, rich, sidebar.width, sidebar.height, false).lines;
    let mut header = None;
    for (idx, line) in lines.iter().enumerate() {
        // Headers carry one indent cell now; the timer-row lookahead
        // below still keeps a session literally named Scheduled from
        // matching.
        let exact = line.spans.len() == 1 && line.spans[0].content.trim() == "Scheduled";
        let next_row = lines.get(idx + 1).is_some_and(|next| {
            let text: String = next.spans.iter().map(|s| s.content.as_ref()).collect();
            text.starts_with("  ◷ in ") || text.starts_with("◷ in ")
        });
        if exact && next_row {
            header = Some(idx);
            break;
        }
    }
    let Some(header) = header else {
        return Vec::new();
    };
    // "[Cancel]" is 8 cells, the pill 10. Rich sidebars right-align it
    // to the divider edge (4 shy of the rect); compact ones have no
    // rules, so the button hugs the border instead.
    let edge = if sidebar_is_rich(sidebar) {
        sidebar.right().saturating_sub(3)
    } else {
        sidebar.right().saturating_sub(1)
    };
    let wide = if pills { 10 } else { 8 };
    let x = edge.saturating_sub(wide).max(sidebar.x + 1);
    if edge.saturating_sub(x) < wide {
        return Vec::new();
    }
    detail
        .timers
        .iter()
        .enumerate()
        .filter_map(|(i, timer)| {
            let row = header + 1 + i;
            if row >= lines.len() {
                return None;
            }
            let y = sidebar.y + 1 + row as u16;
            if y + 1 >= sidebar.bottom() {
                return None;
            }
            Some((timer.id.clone(), Rect::new(x, y, wide, 1)))
        })
        .collect()
}

/// Mode-button hit areas: `[Off]`/`[Yolo]` legacy, pill containers
/// (` Off ` 7 wide, ` Yolo ` 8 wide) when `pills`. Rows never move.
pub fn mode_button_areas(sidebar: Rect, pills: bool) -> ModeButtons {
    let y = if sidebar.height >= 30 {
        sidebar.bottom().saturating_sub(4)
    } else {
        sidebar.y + SETTINGS_ROW
    };
    mode_button_areas_at(sidebar, pills, y)
}

/// Mode-button hit areas on an explicit row: the compact render and
/// mouse paths pass the content-built row so the buttons follow long
/// content instead of doubling against a fixed overlay.
pub fn mode_button_areas_at(sidebar: Rect, pills: bool, y: u16) -> ModeButtons {
    let visible = sidebar.height >= SETTINGS_ROW + 2 && sidebar.width >= 16;
    let (off_w, yolo_x, yolo_w) = if pills {
        (7, sidebar.x + 10, 8)
    } else {
        (5, sidebar.x + 8, 6)
    };
    ModeButtons {
        off: Rect::new(sidebar.x + 2, y, if visible { off_w } else { 0 }, 1),
        yolo: Rect::new(yolo_x, y, if visible { yolo_w } else { 0 }, 1),
    }
}

/// Permission mode under a sidebar click, if any.
pub fn mode_at(buttons: &ModeButtons, col: u16, row: u16) -> Option<&'static str> {
    if row != buttons.off.y {
        return None;
    }
    if col >= buttons.off.x && col < buttons.off.x + buttons.off.width {
        Some("off")
    } else if col >= buttons.yolo.x && col < buttons.yolo.x + buttons.yolo.width {
        Some("yolo")
    } else {
        None
    }
}

/// Chrome snapshots: session-bar tabs plus sidebar state.
pub struct Chrome {
    pub tabs: Vec<SessionTab>,
    pub topbar: TopBar,
    pub detail: Option<SessionDetail>,
    /// Fleet router rows plus cursor/scroll/active for the sidebar slot.
    pub sessions: Vec<FleetRow>,
    pub active: Option<crate::session::SessionId>,
    pub fleet_cursor: Option<crate::session::SessionId>,
    pub fleet_scroll: usize,
    pub other_timers: usize,
    pub pending: usize,
    pub mode: &'static str,
    /// "on" or "off": Telegram mobile transport state for the sidebar row.
    pub telegram: &'static str,
    /// Latest `message_user` badge as `(session, text)`, if any.
    pub telegram_badge: Option<(String, String)>,
    /// Grid mode: the main area shows every session in framed cells and
    /// the sidebar hides for full-width tiles.
    pub grid: bool,
    /// Pill session tabs: rounded Nerd Font ends around each tab button.
    pub pills: bool,
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

/// Render one focused session in the main pane with sidebar and session
/// bar. Titles and bodies are untrusted PTY output, so both pass
/// through display encoding: raw escape sequences must never reach the
/// outer terminal.
/// One tab-strip row: `[label]` buttons, the active tab reversed.
fn render_topbar(frame: &mut Frame, bar: Rect, tabs: &[TopTab], pills: bool) {
    let buttons = layout_topbar(bar, tabs, pills);
    for button in &buttons {
        let tab = &tabs[button.index];
        let area = Rect::new(button.start, bar.y, button.end - button.start, 1);
        if pills {
            let (style, left, right) = topbar_pill(tab);
            render_pill(frame, area, &safe_text::encode_for_display(&tab.label), style, left, right);
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

/// Clip one styled row to a cell width, preserving per-span styles.
fn clip_spans(row: &[SpanView], width: usize) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut room = width;
    for span in row {
        if room == 0 {
            break;
        }
        let take: String = span.text.chars().take(room).collect();
        room -= take.chars().count();
        if !take.is_empty() {
            out.push(Span::styled(take, span.style));
        }
    }
    out
}

/// Grid mode: every session in its own framed cell across the full
/// width, name in the frame, the focused frame highlighted. Cells show
/// the tail of each pane (latest output first); panes are never resized,
/// so entering grid never reflows an agent's terminal.
fn render_grid(frame: &mut Frame, area: Rect, panes: &[PaneView], _chrome: &Chrome) {
    let grid = grid_area(area);
    let cells = grid_cells(grid, panes.len());
    if cells.is_empty() {
        let hint =
            Paragraph::new("No sessions yet — press Ctrl-b c to create one.\nCtrl-b q quits.")
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                            .border_type(crate::theme::border_type())
                        .border_style(theme::style(theme::Role::BorderUnfocused))
                        .title(" forge "),
                );
        frame.render_widget(hint, grid);
        return;
    }
    for (view, cell) in panes.iter().zip(cells.iter()) {
        let (glyph, _role) = if view.live {
            theme::status_glyph_running()
        } else {
            theme::status_glyph_exited()
        };
        let title = format!("{} {} ", glyph, safe_text::encode_for_display(&view.title));
        let (border, name_style) = if view.focused {
            (
                theme::style(theme::Role::BorderFocused),
                theme::style(theme::Role::Focus).add_modifier(Modifier::REVERSED),
            )
        } else {
            (theme::style(theme::Role::BorderUnfocused), theme::style(theme::Role::Text))
        };
        let block = Block::default()
            .borders(Borders::ALL)
                .border_type(crate::theme::border_type())
            .border_style(border)
            .title(Span::styled(title, name_style));
        let inner = block.inner(*cell);
        frame.render_widget(block, *cell);
        if inner.width < 1 || inner.height < 1 {
            continue;
        }
        let h = inner.height as usize;
        let rows: Vec<Line> = view
            .lines
            .iter()
            .rev()
            .take(h)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|row| Line::from(clip_spans(row, inner.width as usize)))
            .collect();
        frame.render_widget(Paragraph::new(Text::from(rows)), inner);
        if view.focused {
            if let Some((row, col)) = view.cursor {
                // Rows are bottom-anchored: shift pane coordinates down
                // by the skipped head rows before placing the cursor.
                let skipped = view.lines.len().saturating_sub(h) as u16;
                if let Some(pos) =
                    cursor_screen_pos(*cell, Some((row.saturating_sub(skipped), col)))
                {
                    frame.set_cursor_position(pos);
                }
            }
        }
    }
}

pub fn render(frame: &mut Frame, area: Rect, panes: &[PaneView], chrome: &Chrome) {
    let areas = chrome_areas(area);
    if chrome.grid {
        render_grid(frame, area, panes, chrome);
    } else {
        render_focused(frame, &areas, panes, chrome.detail.as_ref());
        if areas.topbar.height > 0 {
            render_topbar(frame, areas.topbar, &chrome.topbar.tabs, chrome.pills);
        }
        render_sidebar(frame, &areas, chrome);
    }
    render_session_bar(frame, &areas, chrome);
}

/// Focused-session view: one framed pane plus sidebar. The border
/// title carries the context line — name, live state, sticky status —
/// mirroring the fleet block's reason without spending a pane row.
fn render_focused(
    frame: &mut Frame,
    areas: &ChromeAreas,
    panes: &[PaneView],
    detail: Option<&SessionDetail>,
) {
    let focused = panes.iter().find(|p| p.focused).or(panes.first());
    match focused {
        Some(view) => {
            let (glyph, _role) = if view.live {
                theme::status_glyph_running()
            } else {
                theme::status_glyph_exited()
            };
            let title = match detail {
                Some(d) => {
                    let mut t = format!(
                        "{} {} · {}",
                        glyph,
                        safe_text::encode_for_display(&d.name),
                        safe_text::encode_for_display(&d.state)
                    );
                    if let Some(status) = d.status.as_deref() {
                        t.push_str(&format!(
                            " · {}",
                            safe_text::encode_for_display(status)
                        ));
                    }
                    t.push(' ');
                    t
                }
                None => format!("{} {} ", glyph, safe_text::encode_for_display(&view.title)),
            };
            let block = Block::default()
                .borders(Borders::ALL)
                    .border_type(crate::theme::border_type())
                .border_style(theme::style(theme::Role::BorderFocused))
                .title(title);
            let mut text = pane_text(view);
            if areas.topbar.y > areas.main.y {
                text.lines.insert(0, Line::from(""));
            }
            frame.render_widget(Paragraph::new(text).block(block), areas.main);
            if let Some(pos) = cursor_screen_pos(pane_grid_area(&areas), view.cursor) {
                frame.set_cursor_position(pos);
            }
        }
        None => {
            let hint =
                Paragraph::new("No sessions yet — press Ctrl-b c to create one.\nCtrl-b q quits.")
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                                .border_type(crate::theme::border_type())
                            .border_style(theme::style(theme::Role::BorderUnfocused))
                            .title(" forge "),
                    );
            frame.render_widget(hint, areas.main);
        }
    }
}

/// Sidebar panel with mode buttons and timer cancels.
fn render_sidebar(frame: &mut Frame, areas: &ChromeAreas, chrome: &Chrome) {
    if areas.sidebar.width > 0 && areas.sidebar.height > 0 {
        let info = SidebarInfo {
            session: chrome.detail.clone(),
            sessions: chrome.sessions.clone(),
            active: chrome.active,
            fleet_cursor: chrome.fleet_cursor,
            fleet_scroll: chrome.fleet_scroll,
            other_timers: chrome.other_timers,
            pending: chrome.pending,
            mode: chrome.mode,
            telegram: chrome.telegram,
            telegram_badge: chrome.telegram_badge.clone(),
        };
        let rich = sidebar_is_rich(areas.sidebar);
        // One shared paint: list clipped above, footer pinned below,
        // buttons overlaid on the footer row. The mouse path rebuilds
        // the same paint, so controls track it exactly.
        let paint = sidebar_paint(
            &info,
            rich,
            areas.sidebar.width,
            areas.sidebar.height,
            chrome.pills,
        );
        let mut lines = paint.lines;
        let btn_areas =
            footer_mode_buttons(areas.sidebar, &info, rich, chrome.pills);
        let btn_row = btn_areas.off.y.saturating_sub(areas.sidebar.y + 1) as usize;
        if let Some(line) = lines.get_mut(btn_row) {
            *line = Line::from(""); // the controls below own this row
        }
        let side = Paragraph::new(Text::from(lines)).block(
            Block::default()
                .borders(Borders::ALL)
                    .border_type(crate::theme::border_type())
                .border_style(theme::style(theme::Role::BorderUnfocused))
                .title(" status "),
        );
        frame.render_widget(side, areas.sidebar);
        for (label, area, active) in [
            ("Off", btn_areas.off, info.mode == "off"),
            ("Yolo", btn_areas.yolo, info.mode == "yolo"),
        ] {
            if area.width > 0 && area.right() <= areas.sidebar.right().saturating_sub(1) {
                if chrome.pills {
                    let (style, left, right) = theme::button_chrome(
                        active,
                        theme::style(theme::Role::TabActive),
                        theme::style(theme::Role::TabInactive),
                        Color::DarkGray,
                    );
                    render_pill(frame, area, label, style, left, right);
                } else {
                    let style = if active {
                        theme::style(theme::Role::Focus).add_modifier(Modifier::REVERSED)
                    } else {
                        theme::style(theme::Role::Text)
                    };
                    ChromeButton::new(&format!("[{label}]"), style).view(frame, area);
                }
            }
        }
        // One red Cancel per armed timer, over its own countdown row.
        // The pill is the destructive variant: red container, dark label.
        for (_, area) in timer_cancel_rects(areas.sidebar, &info, chrome.pills) {
            if chrome.pills {
                render_pill(
                    frame,
                    area,
                    "Cancel",
                    Style::default().fg(Color::Black).bg(Color::Red),
                    Color::Red,
                    Color::Red,
                );
            } else {
                ChromeButton::new(
                    "[Cancel]",
                    theme::style(theme::Role::Danger).add_modifier(Modifier::REVERSED),
                )
                .view(frame, area);
            }
        }
    }
}

/// Numbered session bar, kept in grid mode: digits exit grid and focus.
/// One pill button: container-colored bookends around the label, with
/// symmetric inner padding so the text sits centered in the container.
/// The padding inherits the label style, keeping filled pills solid.
/// Left and right bookends take separate colors so `Left`-highlight
/// themes can light only the leading edge.
fn render_pill(
    frame: &mut Frame,
    area: Rect,
    text: &str,
    style: Style,
    left_cap: Color,
    right_cap: Color,
) {
    let line = Line::from(vec![
        Span::styled(
            crate::theme::pill_left().to_string(),
            Style::default().fg(left_cap),
        ),
        Span::styled(" ".to_string(), style),
        Span::styled(text.to_string(), style),
        Span::styled(" ".to_string(), style),
        Span::styled(
            crate::theme::pill_right().to_string(),
            Style::default().fg(right_cap),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn render_session_bar(frame: &mut Frame, areas: &ChromeAreas, chrome: &Chrome) {
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

fn pane_text(view: &PaneView) -> Text<'static> {
    Text::from(
        view.lines
            .iter()
            .map(|line| {
                Line::from(
                    line.iter()
                        .map(|span| Span::styled(span.text.clone(), span.style))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn area() -> Rect {
        Rect::new(0, 0, 80, 24)
    }

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
    fn button_caps_follow_the_active_theme() {
        let text = crate::theme::parse_external_theme(
            r##"{"name": "sq", "buttons": {"left": "[", "right": "]"}}"##,
        )
        .expect("square theme parses");
        let _guard = crate::theme::hold_external_theme(text);
        assert_eq!(crate::theme::pill_left(), '[');
        assert_eq!(crate::theme::pill_right(), ']');
        let strip: String = visual_button_spans(true, false)
            .iter()
            .map(|s| s.text.as_str())
            .collect();
        assert!(strip.contains('[') && strip.contains(']'), "square caps: {strip:?}");
        assert!(!strip.contains(PILL_LEFT), "no builtin caps: {strip:?}");
    }

    #[test]
    fn left_highlight_keeps_group_fill_with_split_caps() {
        let theme = crate::theme::parse_external_theme(
            r##"{"name": "t", "highlight": "left"}"##,
        )
        .expect("left theme parses");
        let _guard = crate::theme::hold_external_theme(theme);
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

    fn tab(title: &str, focused: bool) -> SessionTab {
        SessionTab {
            title: title.to_string(),
            live: true,
            focused,
            group: None,
            group_color: None,
        }
    }

    fn grouped(title: &str, focused: bool, group: &str, color: usize) -> SessionTab {
        SessionTab {
            title: title.to_string(),
            live: true,
            focused,
            group: Some(group.to_string()),
            group_color: Some(color),
        }
    }

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
    fn tall_sidebar_keeps_mode_buttons_near_bottom() {
        let sidebar = chrome_areas(Rect::new(0, 0, 120, 40)).sidebar;
        let buttons = mode_button_areas(sidebar, false);
        assert_eq!(buttons.off.y, sidebar.bottom() - 4);
        assert_eq!(mode_at(&buttons, buttons.yolo.x, buttons.yolo.y), Some("yolo"));
    }

    #[test]
    fn pill_mode_buttons_widen_hit_areas() {
        let sidebar = chrome_areas(Rect::new(0, 0, 80, 24)).sidebar;
        let buttons = mode_button_areas(sidebar, true);
        assert_eq!(buttons.off.width, 7, "off pill");
        assert_eq!(buttons.yolo.x, buttons.off.x + 8, "one gap cell");
        assert_eq!(buttons.yolo.width, 8, "yolo pill");
        assert_eq!(buttons.off.y, buttons.yolo.y, "same row as legacy");
        assert_eq!(mode_at(&buttons, buttons.off.x, buttons.off.y), Some("off"));
        assert_eq!(mode_at(&buttons, buttons.yolo.x + 7, buttons.yolo.y), Some("yolo"), "right cap hits");
        assert_eq!(mode_at(&buttons, buttons.off.x + 7, buttons.off.y), None, "gap misses");
    }

    #[test]
    fn pill_cancel_rects_widen_and_keep_edge() {
        let info = SidebarInfo { session: Some(timed_detail()), sessions: Vec::new(), active: None, fleet_cursor: None, fleet_scroll: 0, other_timers: 0, pending: 0, mode: "off", telegram: "off", telegram_badge: None };
        let areas = chrome_areas(Rect::new(0, 0, 180, 40));
        let rects = timer_cancel_rects(areas.sidebar, &info, true);
        assert_eq!(rects.len(), 2);
        for (_, area) in &rects {
            assert_eq!(area.width, 10, "pill Cancel width");
            assert_eq!(area.right(), areas.sidebar.right() - 3, "keeps divider edge");
        }
    }

    #[test]
    fn render_pill_mode_buttons_and_cancel() {
        let mut c = chrome();
        c.pills = true;
        c.detail = Some(timed_detail());
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render(f, area(), &[], &c)).unwrap();
        let rows = buffer_rows(&terminal);
        // The pinned footer puts the button row at 20; pills still
        // replace the brackets there, and the fixed row stays blank.
        assert!(!rows[11].contains("Off"), "no ghost row: {:?}", rows[11]);
        assert!(rows[21].contains("Off"), "off pill: {:?}", rows[21]);
        assert!(!rows[21].contains("[Off]"), "no legacy brackets");
        let cancel_y = rows.iter().position(|r| r.contains("Cancel")).expect("cancel pill");
        assert!(rows[cancel_y].contains("\u{e0b6}"));
        let buf = terminal.backend().buffer();
        assert_eq!(buf[(66, 21)].fg, Color::Yellow, "active off cap");
        assert_eq!(buf[(67, 21)].bg, Color::Yellow, "active off fill");
        let cap_byte = rows[cancel_y].find("\u{e0b6}").expect("left cap");
        let cap_x = rows[cancel_y][..cap_byte].chars().count() as u16;
        assert_eq!(buf[(cap_x, cancel_y as u16)].fg, Color::Red, "destructive caps");
        assert_eq!(buf[(cap_x + 1, cancel_y as u16)].bg, Color::Red, "destructive fill");
    }

    #[test]
    fn render_pill_mode_buttons_fill_inactive_container() {
        // The 80-col sidebar drops the Yolo overlay pill, so render wide
        // enough for both mode pills and check the resting container.
        let mut c = chrome();
        c.pills = true;
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        terminal.draw(|f| render(f, Rect::new(0, 0, 160, 40), &[], &c)).unwrap();
        let rows = buffer_rows(&terminal);
        let y = rows.iter().position(|r| r.contains("Yolo")).expect("yolo pill paints");
        let byte_x = rows[y].find("Yolo").expect("yolo label");
        let cell_x = rows[y][..byte_x].chars().count() as u16;
        let buf = terminal.backend().buffer();
        assert_eq!(buf[(cell_x - 1, y as u16)].bg, Color::DarkGray, "inactive yolo fill");
        assert_eq!(buf[(cell_x, y as u16)].fg, Color::Black, "inactive yolo label");
    }

    #[test]
    fn chrome_button_is_a_submittable_tuirealm_component() {
        use tuirealm::command::{Cmd, CmdResult};
        use tuirealm::component::Component;
        let mut button = ChromeButton::new("[Terminal]", theme::style(theme::Role::Focus));
        assert_eq!(button.perform(Cmd::Submit), CmdResult::Submit(tuirealm::state::State::None));
        assert_eq!(button.state(), tuirealm::state::State::None);
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
    fn sidebar_drawn_buttons_match_click_rows_without_duplicate_glyphs() {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render(f, area(), &[], &chrome())).unwrap();
        let rows = buffer_rows(&terminal);
        assert_eq!(rows[21].chars().skip(66).take(12).collect::<String>(), "[Off] [Yolo]");
        assert!(!rows[12].contains("[Off]"));

        let mut tall = Terminal::new(TestBackend::new(120, 40)).unwrap();
        tall.draw(|f| render(f, Rect::new(0, 0, 120, 40), &[], &chrome())).unwrap();
        let tall_rows = buffer_rows(&tall);
        assert!(tall_rows[37].contains("[Off] [Yolo]"));
        assert!(!tall_rows[11].contains("[Off]"));
    }

    #[test]
    fn short_sidebar_does_not_draw_controls_on_its_border() {
        let mut terminal = Terminal::new(TestBackend::new(80, 14)).unwrap();
        terminal.draw(|f| render(f, Rect::new(0, 0, 80, 14), &[], &chrome())).unwrap();
        let rows = buffer_rows(&terminal);
        // Taller sidebar earns its button row; the bottom border stays clean.
        assert!(rows[11].contains("[Off]"), "buttons visible: {:?}", rows[11]);
        assert!(!rows[12].contains("[Off]"), "border clean: {:?}", rows[12]);
    }

    #[test]
    fn wide_sidebar_matches_control_plane_sections() {
        let areas = chrome_areas(Rect::new(0, 0, 180, 40));
        assert_eq!(areas.sidebar.width, 36);
        assert_eq!(areas.main.width, 144, "main keeps the clamped remainder");
        let mut c = chrome();
        c.detail = Some(SessionDetail {
            name: "jarvis_senior".into(), cli_tool: "Codex".into(),
            cwd: "/work/jarvis".into(), state: "PROGRESS".into(),
            status: None, status_kind: None, timers: Vec::new(),
        });
        let mut terminal = Terminal::new(TestBackend::new(180, 40)).unwrap();
        terminal.draw(|f| render(f, Rect::new(0, 0, 180, 40), &[], &c)).unwrap();
        let rows = buffer_rows(&terminal);
        let sidebar_text = rows.iter().map(|row| row.chars().skip(areas.sidebar.x as usize)
            .collect::<String>()).collect::<Vec<_>>().join("\n");
        assert!(sidebar_text.contains("Session Control Plane"));
        assert!(sidebar_text.contains("jarvis_senior"));
        assert!(sidebar_text.contains("Status"));
        assert!(!sidebar_text.contains("Tool Approvals"), "stats removed: {sidebar_text:?}");
        assert!(sidebar_text.contains("Global Settings"));
        assert!(sidebar_text.contains("[Off] [Yolo]"));
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
    fn wide_agent_tabs_sit_inside_pane_border_above_pty_content() {
        let bounds = Rect::new(0, 0, 180, 40);
        let areas = chrome_areas(bounds);
        assert_eq!(areas.main.y, 0);
        assert_eq!(areas.topbar.y, 1);
        let mut c = chrome();
        c.topbar.tabs[0].label = "Codex".into();
        let mut terminal = Terminal::new(TestBackend::new(180, 40)).unwrap();
        terminal.draw(|f| render(f, bounds, &[pane("agent", "PTY first line", true)], &c)).unwrap();
        let rows = buffer_rows(&terminal);
        assert!(rows[0].contains("agent"));
        assert!(rows[1].contains("[Codex]"));
        assert!(rows[2].contains("PTY first line"));
        assert_eq!(translate_mouse(pane_grid_area(&areas), 1, 2), Some((1, 1)));
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
    fn wrap_spans_splits_by_display_width() {
        use ratatui::style::Style;
        let spans = vec![SpanView { text: "hello world".to_string(), style: Style::default() }];
        let rows = wrap_spans(spans, 5);
        let text: Vec<String> = rows.iter().map(|r| r.iter().map(|s| s.text.as_str()).collect()).collect();
        assert_eq!(text, vec!["hello", " worl", "d"], "hard splits words: {text:?}");
        // Styles ride along, wide chars count double.
        let spans = vec![
            SpanView { text: "ab".to_string(), style: Style::default() },
            SpanView { text: "日本".to_string(), style: Style::default() },
        ];
        let rows = wrap_spans(spans, 4);
        let text: Vec<String> = rows.iter().map(|r| r.iter().map(|s| s.text.as_str()).collect()).collect();
        assert_eq!(text, vec!["ab日", "本"], "width-aware greedy: {text:?}");
        assert!(wrap_spans(Vec::new(), 10).is_empty(), "empty in, empty out");
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
    fn truncate_spans_cuts_to_width_with_ellipsis() {
        // The strip must stay exactly one row: fitting spans pass
        // through, overflow cuts at a char boundary with an ellipsis,
        // wide chars never split.
        let spans = vec![
            SpanView { text: "ab".to_string(), style: Style::default() },
            SpanView { text: "cdef".to_string(), style: Style::default() },
        ];
        let same = truncate_spans(spans.clone(), 10);
        assert_eq!(same.len(), 2, "fits, untouched");
        assert!(!same.iter().any(|s| s.text.contains('…')), "no ellipsis");
        let cut = truncate_spans(spans.clone(), 4);
        let text: String = cut.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "abc…", "cut plus marker: {text:?}");
        assert_eq!(spans_width(&cut), 4);
        assert!(truncate_spans(spans.clone(), 0).is_empty(), "zero width");
        let wide = vec![SpanView { text: "日本語".to_string(), style: Style::default() }];
        let cut = truncate_spans(wide, 5);
        let text: String = cut.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "日本…", "no split wide char: {text:?}");
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

    fn text_of(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn has(lines: &[Line], needle: &str) -> bool {
        lines.iter().any(|l| text_of(l).contains(needle))
    }

    #[cfg(test)]
    fn sidebar_chrome() -> Chrome {
        Chrome {
            tabs: vec![],
            topbar: TopBar { tabs: vec![] },
            detail: Some(SessionDetail {
                name: "shell-1".to_string(),
                cli_tool: "shell".to_string(),
                cwd: "/tmp/proj".to_string(),
                state: "running".to_string(),
                status: Some("blocked: waiting on review".to_string()),
                status_kind: Some(crate::session_status::StatusKind::Blocked),
                timers: vec![TimerView { id: "t1".to_string(), remaining: "9:55".to_string() }],
            }),
            sessions: Vec::new(),
            active: None,
            fleet_cursor: None,
            fleet_scroll: 0,
            other_timers: 0,
            pending: 3,
            mode: "off",
            telegram: "off",
            telegram_badge: None,
            grid: false,
            pills: true,
        }
    }

    #[test]
    fn compact_status_and_timers_show_a_single_button_row() {
        use ratatui::{backend::TestBackend, Terminal};
        // Narrow terminal: 24-wide compact sidebar with status and a
        // timer armed, the combination that used to push the content
        // buttons past the fixed overlay row and paint both.
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|f| render(f, f.area(), &[], &sidebar_chrome())).unwrap();
        let buf = terminal.backend().buffer();
        let areas = chrome_areas(Rect::new(0, 0, 120, 30));
        let row_text = |y: u16| {
            (areas.sidebar.x..areas.sidebar.right())
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        };
        // The pinned footer owns the buttons now: Settings heads it
        // at row 25 whatever the list above holds.
        assert!(!row_text(11).contains(PILL_LEFT), "no ghost row: {:?}", row_text(11));
        assert!(row_text(25).contains("Settings"), "header visible: {:?}", row_text(25));
        let pill_rows: Vec<u16> = (0..30)
            .filter(|y| row_text(*y).contains(PILL_LEFT))
            .collect();
        // Mode pills plus the timer Cancel: exactly two pill rows.
        assert_eq!(pill_rows.len(), 2, "one button row, one cancel: {pill_rows:?}");
        assert!(mode_at(
            &footer_mode_buttons(areas.sidebar, &sidebar_chrome_info(), false, true),
            areas.sidebar.x + 3,
            pill_rows[1],
        ).is_some(), "mouse follows the visible buttons");
    }

    #[cfg(test)]
    fn sidebar_chrome_info() -> SidebarInfo {
        let c = sidebar_chrome();
        SidebarInfo { session: c.detail.clone(), sessions: c.sessions.clone(), active: c.active, fleet_cursor: c.fleet_cursor, fleet_scroll: c.fleet_scroll, other_timers: c.other_timers, pending: c.pending, mode: c.mode, telegram: "off", telegram_badge: None }
    }

    #[test]
    fn sidebar_content_keeps_border_padding() {
        use ratatui::{backend::TestBackend, Terminal};
        // Compact headers sit one cell inside the border: the focused
        // session owns the first content row, the fleet follows it.
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|f| render(f, f.area(), &[], &sidebar_chrome())).unwrap();
        let buf = terminal.backend().buffer();
        assert_eq!(buf[(97, 1)].symbol(), " ", "border gap");
        assert_eq!(buf[(98, 1)].symbol(), "S", "Session header");
        // ...and so do rich ones (sidebar now opens at x=144).
        let mut wide = Terminal::new(TestBackend::new(180, 40)).unwrap();
        wide.draw(|f| render(f, f.area(), &[], &sidebar_chrome())).unwrap();
        let buf = wide.backend().buffer();
        assert_eq!(buf[(145, 1)].symbol(), " ");
        assert_eq!(buf[(146, 1)].symbol(), "F", "Forge header");
    }

    #[test]
    fn sidebar_shows_focused_session_detail_and_mode() {
        let info = SidebarInfo {
            session: Some(SessionDetail {
                name: "shell-1".to_string(),
                cli_tool: "shell".to_string(),
                cwd: "/tmp/proj".to_string(),
                state: "running · Thinking".to_string(),
                status: Some("blocked: waiting on review".to_string()),
                status_kind: Some(crate::session_status::StatusKind::Blocked),
                timers: Vec::new(),
            }),
            sessions: Vec::new(),
            active: None,
            fleet_cursor: None,
            fleet_scroll: 0,
            other_timers: 0,
            pending: 3,
            mode: "off",
            telegram: "off",
            telegram_badge: None,
        };
        let lines = sidebar_lines(&info);
        assert!(has(&lines, "shell-1"), "name: {lines:?}");
        assert!(has(&lines, "shell"), "tool: {lines:?}");
        assert!(has(&lines, "/tmp/proj"), "cwd: {lines:?}");
        assert!(has(&lines, "blocked: waiting on review"), "status: {lines:?}");
        assert!(has(&lines, "running"), "state: {lines:?}");
        assert!(has(&lines, "Pending: 3"), "pending: {lines:?}");
        assert!(has(&lines, "[Off]"), "off highlighted: {lines:?}");
        assert!(has(&lines, "Yolo"), "yolo offered: {lines:?}");
        assert!(!has(&lines, "safe-only"), "no third mode: {lines:?}");
        // Armed timers add a Scheduled section; empty hides it.
        let mut timed = info.clone();
        timed.session.as_mut().unwrap().timers = vec![
            TimerView { id: "t1".to_string(), remaining: "9:55".to_string() },
            TimerView { id: "t2".to_string(), remaining: "1:00:05".to_string() },
        ];
        let timed_lines = sidebar_lines(&timed);
        assert!(has(&timed_lines, "Scheduled"), "section: {timed_lines:?}");
        assert!(has(&timed_lines, "◷ in 9:55"), "countdown: {timed_lines:?}");
        assert!(!has(&lines, "Scheduled"), "hidden when empty: {lines:?}");
        // Yolo highlights instead when active.
        let yolo = SidebarInfo { session: info.session.clone(), sessions: Vec::new(), active: None, fleet_cursor: None, fleet_scroll: 0, other_timers: 0, pending: 0, mode: "yolo", telegram: "off", telegram_badge: None };
        let yolo_lines = sidebar_lines(&yolo);
        assert!(has(&yolo_lines, "[Yolo]"), "yolo highlighted: {yolo_lines:?}");
        // Never blank: empty state still guides.
        let empty = sidebar_lines(&SidebarInfo { session: None, sessions: Vec::new(), active: None, fleet_cursor: None, fleet_scroll: 0, other_timers: 0, pending: 0, mode: "off", telegram: "off", telegram_badge: None });
        assert!(has(&empty, "Ctrl-b c"), "guides: {empty:?}");
    }

    #[test]
    fn sidebar_has_no_stats_section() {
        // Uptime, tool calls, and approval counts were dropped: the
        // sidebar shows status and schedule, never Stats.
        let info = SidebarInfo {
            session: Some(SessionDetail {
                name: "shell-1".to_string(),
                cli_tool: "shell".to_string(),
                cwd: "/tmp/proj".to_string(),
                state: "running".to_string(),
                status: Some("progress: compiling".to_string()),
                status_kind: Some(crate::session_status::StatusKind::Progress),
                timers: vec![TimerView { id: "t1".to_string(), remaining: "9:55".to_string() }],
            }),
            sessions: Vec::new(),
            active: None,
            fleet_cursor: None,
            fleet_scroll: 0,
            other_timers: 0,
            pending: 0,
            mode: "off",
            telegram: "off",
            telegram_badge: None,
        };
        let lines = sidebar_lines(&info);
        for needle in ["Stats", "Uptime", "Tool calls", "✓", "×"] {
            assert!(!has(&lines, needle), "compact leaks {needle}: {lines:?}");
        }
        let rich = rich_sidebar_lines(&info, 30, 45);
        for needle in ["Stats", "Uptime", "Tool calls", "✓", "×"] {
            assert!(!has(&rich, needle), "rich leaks {needle}: {rich:?}");
        }
        // Status and schedule survive the removal.
        assert!(has(&lines, "progress: compiling"), "status kept: {lines:?}");
        assert!(has(&lines, "Scheduled"), "schedule kept: {lines:?}");
    }

    #[test]
    fn sidebar_badge_escapes_hostile_text() {
        let info = SidebarInfo {
            session: None,
            sessions: Vec::new(),
            active: None,
            fleet_cursor: None,
            fleet_scroll: 0,
            other_timers: 0,
            pending: 0,
            mode: "off",
            telegram: "off",
            telegram_badge: Some(("agent".to_string(), "hi\u{202E}bye".to_string())),
        };
        let lines = sidebar_lines(&info);
        let text: String = lines.iter().flat_map(|l| l.spans.iter().map(|s| s.content.as_ref())).collect();
        assert!(!text.contains("\u{202E}"), "bidi exposed, never raw: {text:?}");
        assert!(text.contains("agent"), "session named: {text:?}");
        let bare = SidebarInfo { session: None, sessions: Vec::new(), active: None, fleet_cursor: None, fleet_scroll: 0, other_timers: 0, pending: 0, mode: "off", telegram: "off", telegram_badge: None };
        // The paint always fills the panel; the badge grows the pinned
        // footer by exactly one row instead.
        assert_eq!(
            sidebar_footer_height(&bare, false) + 1,
            sidebar_footer_height(&info, false),
            "badge grows the footer"
        );
    }

    #[test]
    fn sidebar_shows_telegram_state() {
        let off = sidebar_lines(&SidebarInfo { session: None, sessions: Vec::new(), active: None, fleet_cursor: None, fleet_scroll: 0, other_timers: 0, pending: 0, mode: "off", telegram: "off", telegram_badge: None });
        assert!(has(&off, "Telegram off"), "off state: {off:?}");
        assert!(has(&off, "Ctrl-b m"), "settings key: {off:?}");
        let on = sidebar_lines(&SidebarInfo { session: None, sessions: Vec::new(), active: None, fleet_cursor: None, fleet_scroll: 0, other_timers: 0, pending: 0, mode: "off", telegram: "on", telegram_badge: None });
        assert!(has(&on, "Telegram on"), "on state: {on:?}");
    }

    #[test]
    fn sidebar_width_clamps_around_thirty_six() {
        // Wide terminals stop donating a full percentage to the panel:
        // 159→160 must not steal seven columns from the main pane.
        for (cols, want) in [
            (80u16, 16u16),
            (120, 24),
            (159, 32),
            (160, 36),
            (180, 36),
        ] {
            let areas = chrome_areas(Rect::new(0, 0, cols, 40));
            assert_eq!(areas.sidebar.width, want, "cols {cols}");
            assert_eq!(
                areas.main.width + areas.sidebar.width,
                cols,
                "no gap: cols {cols}"
            );
        }
    }

    /// Six fleet rows with staggered reasons for overflow pressure.
    fn for_h_fleet(info: &mut SidebarInfo) {
        for (i, reason) in [
            "need the production API key from the vault",
            "running",
            "review the migration plan before Friday deploy",
            "running",
            "which region should the new bucket live in",
            "running",
        ]
        .into_iter()
        .enumerate()
        {
            let id = crate::session::SessionId::fresh();
            info.sessions.push(FleetRow {
                id,
                name: format!("agent-{i}"),
                tier: if i == 0 {
                    FleetTier::Attention
                } else {
                    FleetTier::Idle
                },
                state: "running".to_string(),
                reason: reason.to_string(),
                emoji: if i == 0 { "🔔" } else { "" },
            });
        }
    }

    #[test]
    fn sidebar_layout_pins_a_footer() {
        let bar = Rect::new(144, 0, 36, 39);
        let layout = sidebar_layout(bar, 7);
        assert_eq!(layout.footer.height, 7);
        assert_eq!(layout.footer.y + layout.footer.height, bar.y + bar.height);
        assert_eq!(layout.list.y, bar.y);
        assert_eq!(
            layout.list.height + layout.footer.height,
            bar.height,
            "list plus footer fill the panel"
        );
    }

    #[test]
    fn fleet_cell_window_budgets_variable_blocks() {
        // Whole blocks that fit the cell budget; the first always shows.
        assert_eq!(fleet_cell_window(0, &[3, 3, 5, 3], 6), (0, 2));
        assert_eq!(fleet_cell_window(2, &[3, 3, 5, 3], 6), (2, 3));
        assert_eq!(fleet_cell_window(999, &[3, 3], 100), (1, 2));
        assert_eq!(fleet_cell_window(0, &[], 10), (0, 0));
        assert_eq!(fleet_cell_window(0, &[9], 2), (0, 1));
    }

    #[test]
    fn sidebar_content_pins_footer_to_the_bottom() {
        // Tall content must clip the list, never the footer: the last
        // footer rows stay visible at any height.
        let mut info = SidebarInfo {
            session: None,
            sessions: Vec::new(),
            active: None,
            fleet_cursor: None,
            fleet_scroll: 0,
            other_timers: 0,
            pending: 0,
            mode: "off",
            telegram: "on",
            telegram_badge: Some(("agent".to_string(), "hi".to_string())),
        };
        for_h_fleet(&mut info);
        let lines = rich_sidebar_lines(&info, 45, 20);
        assert_eq!(lines.len(), 20, "content fills exactly");
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!(
            text[19].contains("agent: hi"),
            "badge pinned last: {text:?}"
        );
        assert!(text[18].contains("Telegram on"), "footer intact: {text:?}");
    }

    #[test]
    fn status_emoji_has_no_joiners_or_selectors() {
        use crate::session_status::StatusKind;
        // Bare glyphs only: no VS16, no ZWJ, like the topbar icons.
        for kind in [
            StatusKind::Info,
            StatusKind::Progress,
            StatusKind::Success,
            StatusKind::Warning,
            StatusKind::Blocked,
            StatusKind::Question,
        ] {
            let e = status_emoji(kind);
            assert!(!e.contains('\u{FE0F}'), "no VS16: {e:?}");
            assert!(!e.contains('\u{200D}'), "no ZWJ: {e:?}");
            assert!(Line::from(e).width() >= 1, "paints something: {e:?}");
        }
        assert_eq!(status_emoji(StatusKind::Blocked), "🛑");
    }

    #[test]
    fn cut_cells_respects_display_width() {
        assert_eq!(cut_cells("abcdef", 4), "abcd");
        assert_eq!(cut_cells("short", 99), "short");
        assert_eq!(cut_cells("🛑🛑🛑", 4), "🛑🛑");
        assert_eq!(cut_cells("a🛑b", 3), "a🛑");
        assert_eq!(cut_cells("a🛑b", 0), "");
    }

    #[test]
    fn fleet_rows_and_status_carry_emoji_beside_marks() {
        use crate::session_status::StatusKind;
        let id = crate::session::SessionId::fresh();
        let info = SidebarInfo {
            session: Some(SessionDetail {
                name: "muse".to_string(),
                cli_tool: "muse".to_string(),
                cwd: "/tmp/proj".to_string(),
                state: "running · Waiting".to_string(),
                status: Some("blocked: need the API key".to_string()),
                status_kind: Some(StatusKind::Blocked),
                timers: Vec::new(),
            }),
            sessions: vec![FleetRow {
                id,
                name: "muse".to_string(),
                tier: FleetTier::Attention,
                state: "running · Waiting".to_string(),
                reason: "need the API key".to_string(),
                emoji: "🔔",
            }],
            active: Some(id),
            fleet_cursor: None,
            fleet_scroll: 0,
            other_timers: 0,
            pending: 0,
            mode: "off",
            telegram: "off",
            telegram_badge: None,
        };
        // Marks still carry meaning; emoji ride beside them.
        let compact = sidebar_lines(&info);
        assert!(has(&compact, "!"), "tier mark: {compact:?}");
        assert!(has(&compact, "🔔"), "attention emoji: {compact:?}");
        assert!(has(&compact, "🛑"), "status emoji: {compact:?}");
        let rich = rich_sidebar_lines(&info, 30, 45);
        assert!(has(&rich, "*"), "active mark: {rich:?}");
        assert!(has(&rich, "need the API key"), "reason: {rich:?}");
    }

    #[test]
    fn wrap_cells_splits_on_words_and_cells() {
        assert_eq!(wrap_cells("aaa bbb ccc", 5), vec!["aaa", "bbb", "ccc"]);
        assert_eq!(wrap_cells("abcdef", 4), vec!["abcd", "ef"]);
        assert_eq!(wrap_cells("🛑🛑 x", 4), vec!["🛑🛑", "x"]);
        assert_eq!(wrap_cells("", 4), Vec::<String>::new());
    }

    #[test]
    fn session_block_precedes_fleet() {
        let id = crate::session::SessionId::fresh();
        let other = crate::session::SessionId::fresh();
        let info = SidebarInfo {
            session: Some(SessionDetail {
                name: "muse".to_string(),
                cli_tool: "muse".to_string(),
                cwd: "/tmp/proj".to_string(),
                state: "running".to_string(),
                status: None,
                status_kind: None,
                timers: Vec::new(),
            }),
            sessions: vec![
                FleetRow { id, name: "muse".to_string(), tier: FleetTier::Idle, state: "running".to_string(), reason: "running".to_string(), emoji: "" },
                FleetRow { id: other, name: "codex".to_string(), tier: FleetTier::Idle, state: "running".to_string(), reason: "running".to_string(), emoji: "" },
            ],
            active: Some(id),
            fleet_cursor: None,
            fleet_scroll: 0,
            other_timers: 0,
            pending: 0,
            mode: "off",
            telegram: "off",
            telegram_badge: None,
        };
        let text_of = |l: &Line| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>();
        let compact = sidebar_lines(&info);
        let session_at = compact.iter().position(|l| text_of(l).trim() == "Session").expect("session header");
        let fleet_at = compact.iter().position(|l| text_of(l).starts_with(" Fleet (")).expect("fleet header");
        assert!(session_at < fleet_at, "session above fleet");
        let rich = rich_sidebar_lines(&info, 30, 45);
        let session_at = rich.iter().position(|l| text_of(l).trim() == "Session").expect("session header");
        let fleet_at = rich.iter().position(|l| text_of(l).starts_with(" Fleet (")).expect("fleet header");
        assert!(session_at < fleet_at, "session above fleet");
    }

    #[test]
    fn fleet_blocks_breathe_with_gaps_and_wrapped_reasons() {
        let id = crate::session::SessionId::fresh();
        let other = crate::session::SessionId::fresh();
        let long = "need the production API key from the vault under the stairs";
        let info = SidebarInfo {
            session: None,
            sessions: vec![
                FleetRow { id, name: "muse".to_string(), tier: FleetTier::Attention, state: "running · Waiting".to_string(), reason: long.to_string(), emoji: "🔔" },
                FleetRow { id: other, name: "codex".to_string(), tier: FleetTier::Idle, state: "running".to_string(), reason: "running".to_string(), emoji: "" },
            ],
            active: Some(id),
            fleet_cursor: None,
            fleet_scroll: 0,
            other_timers: 0,
            pending: 0,
            mode: "off",
            telegram: "off",
            telegram_badge: None,
        };
        // Width 24: the long reason must wrap, never clip mid-word off-panel.
        let lines = sidebar_lines_at(&info, false, 24, 60).0;
        let text: Vec<String> = lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
        let name_at = text.iter().position(|l| l.contains("muse")).expect("name line");
        assert!(text[name_at].contains("!"), "tier mark on the name line");
        // Reason lines run until the gap; the whole reason survives.
        let mut end = name_at + 1;
        assert!(text[end].contains("need the production"), "reason starts its own line");
        while !text[end].trim().is_empty() {
            end += 1;
        }
        let joined = text[name_at + 1..end].join(" ");
        assert!(joined.contains("API key"), "reason wraps instead of clipping: {joined:?}");
        assert!(joined.contains("stairs"), "nothing lost: {joined:?}");
        // Zoned fleet: the gap yields to the sessions zone header, and
        // the next block follows inside its zone.
        assert!(text[end + 1].contains("SESSIONS"), "zone follows the gap");
        assert!(text[end + 2].contains("codex"), "next block inside its zone");
        for (i, l) in lines.iter().enumerate() {
            assert!(l.width() <= 24, "line {i} fits: {:?}", text[i]);
        }
    }

    #[test]
    fn pending_zero_hides_its_row() {
        let mut info = SidebarInfo {
            session: None,
            sessions: Vec::new(),
            active: None,
            fleet_cursor: None,
            fleet_scroll: 0,
            other_timers: 0,
            pending: 0,
            mode: "off",
            telegram: "off",
            telegram_badge: None,
        };
        for line in sidebar_lines(&info) {
            assert!(!text_of(&line).contains("Pending"), "zero hides: {line:?}");
        }
        for line in rich_sidebar_lines(&info, 45, 30) {
            assert!(!text_of(&line).contains("Pending"), "zero hides: {line:?}");
        }
        info.pending = 2;
        assert!(has(&sidebar_lines(&info), "Pending: 2"));
        info.session = Some(timed_detail());
        assert!(has(&rich_sidebar_lines(&info, 45, 30), "Pending hooks: 2"));
    }

    #[test]
    fn fleet_header_shows_window_position() {
        let mut info = SidebarInfo {
            session: None,
            sessions: Vec::new(),
            active: None,
            fleet_cursor: None,
            fleet_scroll: 0,
            other_timers: 0,
            pending: 0,
            mode: "off",
            telegram: "off",
            telegram_badge: None,
        };
        for_h_fleet(&mut info);
        let clipped = sidebar_paint(&info, false, 24, 20, false).lines;
        let header = clipped
            .iter()
            .map(text_of)
            .find(|l| l.starts_with(" Fleet ("))
            .expect("fleet header");
        assert!(header.contains("· 1–2"), "partial window shows position: {header:?}");
        let full = sidebar_paint(&info, false, 24, 60, false).lines;
        let header = full
            .iter()
            .map(text_of)
            .find(|l| l.starts_with(" Fleet ("))
            .expect("fleet header");
        assert_eq!(header, " Fleet (6)", "full window stays clean: {header:?}");
    }

    #[test]
    fn fleet_window_items_group_attention_first() {
        let a = crate::session::SessionId::fresh();
        let b = crate::session::SessionId::fresh();
        let c = crate::session::SessionId::fresh();
        let info = SidebarInfo {
            session: None,
            sessions: vec![
                FleetRow { id: a, name: "aaa".into(), tier: FleetTier::Idle, state: "running".into(), reason: "running".into(), emoji: "" },
                FleetRow { id: b, name: "bbb".into(), tier: FleetTier::Attention, state: "running · Waiting".into(), reason: "need a key".into(), emoji: "🔔" },
                FleetRow { id: c, name: "ccc".into(), tier: FleetTier::Idle, state: "running".into(), reason: "running".into(), emoji: "" },
            ],
            active: None,
            fleet_cursor: None,
            fleet_scroll: 0,
            other_timers: 0,
            pending: 0,
            mode: "off",
            telegram: "off",
            telegram_badge: None,
        };
        // Room for everything: attention group leads, spawn order inside.
        let full = fleet_window_items(&info, 24, 100);
        assert_eq!(
            full,
            vec![
                FleetItem::AttnHeader,
                FleetItem::Block(1),
                FleetItem::SessionsHeader,
                FleetItem::Block(0),
                FleetItem::Block(2),
            ],
            "grouped: {full:?}"
        );
        // Tight budget: the sessions group drops whole, never stranded.
        let tight = fleet_window_items(&info, 24, 5);
        assert_eq!(
            tight,
            vec![FleetItem::AttnHeader, FleetItem::Block(1)],
            "budgeted: {tight:?}"
        );
        // Starved budget strands no lonely header.
        let starved = fleet_window_items(&info, 24, 1);
        assert!(starved.is_empty(), "nothing rather than a stray header: {starved:?}");
    }

    #[test]
    fn attention_zone_carries_danger_accent() {
        use ratatui::{backend::TestBackend, Terminal};
        let id = crate::session::SessionId::fresh();
        let info = SidebarInfo {
            session: None,
            sessions: vec![
                FleetRow { id, name: "muse".into(), tier: FleetTier::Attention, state: "running · Waiting".into(), reason: "need a key".into(), emoji: "🔔" },
            ],
            active: Some(id),
            fleet_cursor: None,
            fleet_scroll: 0,
            other_timers: 0,
            pending: 0,
            mode: "off",
            telegram: "off",
            telegram_badge: None,
        };
        let lines = sidebar_paint(&info, false, 24, 30, false).lines;
        let text: Vec<String> = lines.iter().map(text_of).collect();
        assert!(text.iter().any(|l| l.contains("┃")), "accent paints: {text:?}");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| {
                let paint = sidebar_paint(&info, false, 16, 23, false);
                let side = Paragraph::new(Text::from(paint.lines)).block(
                    Block::default().borders(Borders::ALL),
                );
                f.render_widget(side, f.area());
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let accent = buf
            .content
            .iter()
            .find(|c| c.symbol() == "┃")
            .expect("accent cell paints");
        assert_eq!(accent.fg, Color::Red, "danger accent");
    }

    #[test]
    fn cursor_row_takes_focus_reverse() {
        use ratatui::{backend::TestBackend, Terminal};
        let id = crate::session::SessionId::fresh();
        let other = crate::session::SessionId::fresh();
        let info = SidebarInfo {
            session: None,
            sessions: vec![
                FleetRow { id, name: "muse".into(), tier: FleetTier::Idle, state: "running".into(), reason: "running".into(), emoji: "" },
                FleetRow { id: other, name: "codex".into(), tier: FleetTier::Idle, state: "running".into(), reason: "running".into(), emoji: "" },
            ],
            active: None,
            fleet_cursor: Some(other),
            fleet_scroll: 0,
            other_timers: 0,
            pending: 0,
            mode: "off",
            telegram: "off",
            telegram_badge: None,
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| {
                let paint = sidebar_paint(&info, false, 16, 23, false);
                let side = Paragraph::new(Text::from(paint.lines)).block(
                    Block::default().borders(Borders::ALL),
                );
                f.render_widget(side, f.area());
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        // Drive off the real paint: scan for the block name rows,
        // never hand-computed geometry.
        let row_text = |y: u16| {
            (0..80)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        };
        let mut cursor_y = None;
        let mut idle_y = None;
        for y in 0..24 {
            let row = row_text(y);
            if row.contains("codex") {
                cursor_y = Some(y);
            }
            if row.contains("muse") {
                idle_y = Some(y);
            }
        }
        let cy = cursor_y.expect("cursor block paints");
        let iy = idle_y.expect("idle block paints");
        assert!(
            buf[(2, cy)].modifier.contains(Modifier::REVERSED),
            "cursor reverses"
        );
        assert!(
            !buf[(2, iy)].modifier.contains(Modifier::REVERSED),
            "neighbors stay flat"
        );
    }

    #[test]
    fn main_pane_title_carries_session_context() {
        use ratatui::{backend::TestBackend, Terminal};
        // The focused PTY border names the session plus its live state
        // and sticky status; without detail it stays the legacy title.
        let title_of = |chrome: &Chrome| -> String {
            let view = PaneView {
                title: "shell-1".to_string(),
                lines: Vec::new(),
                live: true,
                focused: true,
                cursor: None,
            };
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            terminal
                .draw(|f| render(f, f.area(), &[view], chrome))
                .unwrap();
            let buf = terminal.backend().buffer();
            let areas = chrome_areas(Rect::new(0, 0, 80, 24));
            (areas.main.x..areas.main.right())
                .map(|x| buf[(x, areas.main.y)].symbol().to_string())
                .collect()
        };
        let with_detail = title_of(&sidebar_chrome());
        assert!(with_detail.contains("shell-1"), "name: {with_detail:?}");
        assert!(with_detail.contains("running"), "state: {with_detail:?}");
        assert!(
            with_detail.contains("waiting on review"),
            "status: {with_detail:?}"
        );
        let mut bare = sidebar_chrome();
        bare.detail = None;
        let legacy = title_of(&bare);
        assert!(legacy.contains("shell-1"), "legacy name: {legacy:?}");
        assert!(
            !legacy.contains("waiting on review"),
            "no status without detail: {legacy:?}"
        );
    }

    #[test]
    fn rule_line_spans_sidebar_width() {
        // One indent cell plus a width-6 span of dashes.
        for width in [30u16, 45, 60] {
            let rule = rule_line(width);
            let rule_text: String = rule.spans.iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(rule_text.chars().count(), (width - 5) as usize, "rule {width}");
            assert!(rule_text.starts_with(' '), "indent");
        }
    }

    #[test]
    fn format_countdown_matches_reference_shapes() {
        use std::time::Duration;
        assert_eq!(format_countdown(Duration::from_secs(595)), "9:55");
        assert_eq!(format_countdown(Duration::from_secs(3605)), "1:00:05");
        assert_eq!(format_countdown(Duration::from_secs(7)), "0:07");
        assert_eq!(format_countdown(Duration::ZERO), "0:00");
    }

    fn timed_detail() -> SessionDetail {
        SessionDetail {
            name: "agent".into(), cli_tool: "Codex".into(),
            cwd: "/work".into(), state: "running".into(),
            status: None, status_kind: None,
            timers: vec![
                TimerView { id: "t1".into(), remaining: "9:55".into() },
                TimerView { id: "t2".into(), remaining: "1:00:05".into() },
            ],
        }
    }

    #[test]
    fn scheduled_section_keeps_breathing_room() {
        let info = SidebarInfo { session: Some(timed_detail()), sessions: Vec::new(), active: None, fleet_cursor: None, fleet_scroll: 0, other_timers: 0, pending: 0, mode: "off", telegram: "off", telegram_badge: None };
        let lines = rich_sidebar_lines(&info, 30, 45);
        let header = lines
            .iter()
            .position(|l| l.spans.len() == 1 && l.spans[0].content.trim() == "Scheduled")
            .expect("section renders");
        let blank = lines[header - 1].spans.iter().all(|s| s.content.is_empty());
        assert!(blank, "breathing room above Scheduled");
    }

    #[test]
    fn scheduled_timers_render_countdowns_and_cancel_buttons() {
        let mut c = chrome();
        c.detail = Some(timed_detail());
        let mut terminal = Terminal::new(TestBackend::new(180, 40)).unwrap();
        terminal.draw(|f| render(f, Rect::new(0, 0, 180, 40), &[], &c)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Scheduled"), "section: {text:?}");
        assert!(text.contains("9:55"), "countdown: {text:?}");
        assert!(text.contains("[Cancel]"), "button: {text:?}");
    }

    #[test]
    fn timer_cancel_rects_match_painted_rows() {
        let info = SidebarInfo { session: Some(timed_detail()), sessions: Vec::new(), active: None, fleet_cursor: None, fleet_scroll: 0, other_timers: 0, pending: 0, mode: "off", telegram: "off", telegram_badge: None };
        let areas = chrome_areas(Rect::new(0, 0, 180, 40));
        let rects = timer_cancel_rects(areas.sidebar, &info, false);
        assert_eq!(rects.len(), 2);
        assert_eq!((rects[0].0.as_str(), rects[1].0.as_str()), ("t1", "t2"), "soonest first");
        assert_eq!(rects[1].1.y, rects[0].1.y + 1, "stacked rows");
        for (_, area) in &rects {
            assert_eq!(area.width, 8, "Cancel width");
            assert_eq!(
                area.right(),
                areas.sidebar.right() - 3,
                "button ends at the divider edge: {area:?}"
            );
        }
        // The painted button labels sit exactly on the rects.
        let mut terminal = Terminal::new(TestBackend::new(180, 40)).unwrap();
        let mut c = chrome();
        c.detail = Some(timed_detail());
        terminal.draw(|f| render(f, Rect::new(0, 0, 180, 40), &[], &c)).unwrap();
        let buf = terminal.backend().buffer();
        for (_, area) in &rects {
            assert_eq!(buf[(area.x, area.y)].symbol(), "[", "button at {area:?}");
        }
        // A session literally named Scheduled cannot hijack the rows.
        let mut impostor = timed_detail();
        impostor.name = "Scheduled".to_string();
        impostor.timers.clear();
        let bare = SidebarInfo { session: Some(impostor), sessions: Vec::new(), active: None, fleet_cursor: None, fleet_scroll: 0, other_timers: 0, pending: 0, mode: "off", telegram: "off", telegram_badge: None };
        assert!(timer_cancel_rects(areas.sidebar, &bare, false).is_empty());
    }

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().to_string())
            .collect()
    }

    fn pane(title: &str, body: &str, live: bool) -> PaneView {
        PaneView {
            title: title.to_string(),
            lines: body
                .split('\n')
                .map(|line| {
                    vec![SpanView {
                        text: line.to_string(),
                        style: Style::default(),
                    }]
                })
                .collect(),
            live,
            focused: true,
            cursor: None,
        }
    }

    fn chrome() -> Chrome {
        Chrome {
            tabs: vec![tab("sh", true)],
            topbar: TopBar {
                tabs: vec![
                    TopTab {
                        label: "Shell".to_string(),
                        active: true,
                    },
                    TopTab {
                        label: "Terminal".to_string(),
                        active: false,
                    },
                ],
            },
            detail: None,
            sessions: Vec::new(),
            active: None,
            fleet_cursor: None,
            fleet_scroll: 0,
            other_timers: 0,
            pending: 0,
            mode: "off",
            telegram: "off",
            telegram_badge: None,
            grid: false,
            pills: false,
        }
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
    fn render_shows_titles_bodies_and_session_bar() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut c = chrome();
        c.tabs = vec![tab("agent-1", true)];
        terminal
            .draw(|f| render(f, area(), &[pane("agent-1", "hello out", true)], &c))
            .unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("agent-1"), "title visible");
        assert!(text.contains("hello out"), "body visible");
        assert!(text.contains("1 agent-1"), "session button visible");
        // No status bar: the last row is the session bar, and no help
        // line is rendered anywhere.
        let rows = buffer_rows(&terminal);
        assert!(rows[23].contains("1 agent-1"), "bar owns last row: {:?}", rows[23]);
        assert!(!text.contains("prefix Ctrl-b"), "help line is gone");
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
    fn render_grid_frames_every_session_without_sidebar() {
        use ratatui::style::Modifier;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut c = chrome();
        c.grid = true;
        let mut b = pane("b", "bravo output", true);
        b.focused = false;
        terminal
            .draw(|f| render(f, area(), &[pane("a", "alpha output", true), b], &c))
            .unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("alpha output"), "first cell body");
        assert!(text.contains("bravo output"), "second cell body");
        assert!(!text.contains("status"), "sidebar hidden in grid");
        assert!(!text.contains("Terminal"), "tab strip hidden in grid");
        let buf = terminal.backend().buffer();
        // Titles sit inside the top borders on row 0: `● a`, `● b`.
        assert_eq!(buf[(3, 0)].symbol(), "a");
        assert_eq!(buf[(43, 0)].symbol(), "b");
        // The focused name reverses; the other frame stays plain.
        assert!(buf[(1, 0)].modifier.contains(Modifier::REVERSED), "focused name");
        assert!(!buf[(41, 0)].modifier.contains(Modifier::REVERSED), "plain name");
        // Full-width tiles: the second frame opens mid-screen.
        assert_eq!(buf[(40, 0)].symbol(), "┌");
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

    #[test]
    fn render_grid_empty_shows_hint() {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut c = chrome();
        c.grid = true;
        terminal.draw(|f| render(f, area(), &[], &c)).unwrap();
        assert!(buffer_text(&terminal).contains("No sessions yet"));
    }

    fn buffer_rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buf = terminal.backend().buffer();
        let w = buf.area.width as usize;
        buf.content
            .chunks(w)
            .map(|row| row.iter().map(|c| c.symbol().to_string()).collect())
            .collect()
    }

    #[test]
    fn multiline_body_renders_across_rows() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render(f, area(), &[pane("sh", "line1\nline2", true)], &chrome()))
            .unwrap();
        let rows = buffer_rows(&terminal);
        let r1 = rows.iter().position(|r| r.contains("line1")).expect("line1 visible");
        let r2 = rows.iter().position(|r| r.contains("line2")).expect("line2 visible");
        assert_eq!(r2, r1 + 1, "row break preserved: {rows:?}");
        assert!(rows.iter().all(|r| !r.contains('⏎')), "no flattened newlines");
    }

    #[test]
    fn dim_spans_render_faint_in_the_grid() {
        // End of the ghost-text chain: parser marks dim (pty test),
        // style_for maps it (sgr test), and the grid must keep the
        // modifier so the terminal faints it instead of full-bright.
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut p = pane("sh", "ghost", true);
        p.lines = vec![vec![SpanView {
            text: "ghost".to_string(),
            style: Style::default().add_modifier(Modifier::DIM),
        }]];
        terminal
            .draw(|f| render(f, area(), &[p], &chrome()))
            .unwrap();
        let cell = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .find(|c| c.symbol() == "g")
            .expect("ghost visible");
        assert!(
            cell.modifier.contains(Modifier::DIM),
            "faint reaches the grid"
        );
    }

    #[test]
    fn focused_pane_cursor_is_placed_inside_borders() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut p = pane("sh", "hi", true);
        p.cursor = Some((2, 5));
        terminal
            .draw(|f| render(f, area(), &[p], &chrome()))
            .unwrap();
        terminal.backend_mut().assert_cursor_position(Position::new(6, 4));
    }

    #[test]
    fn style_for_maps_sgr_attributes() {
        use crate::pty::{CellColor, CellFormat};
        let f = CellFormat {
            fg: CellColor::Indexed(1),
            bg: CellColor::Rgb(10, 20, 30),
            bold: true,
            italic: false,
            underline: true,
            inverse: true,
            dim: true,
        };
        let s = style_for(&f);
        assert_eq!(s.fg, Some(Color::Indexed(1)));
        assert_eq!(s.bg, Some(Color::Rgb(10, 20, 30)));
        assert!(s.add_modifier.contains(Modifier::BOLD));
        assert!(s.add_modifier.contains(Modifier::UNDERLINED));
        assert!(s.add_modifier.contains(Modifier::REVERSED));
        assert!(s.add_modifier.contains(Modifier::DIM));
        assert!(!s.add_modifier.contains(Modifier::ITALIC));
        let plain = style_for(&CellFormat::plain());
        assert_eq!(plain.fg, None);
        assert_eq!(plain.bg, None);
        assert!(plain.add_modifier.is_empty());
    }

    #[test]
    fn render_shows_sgr_colors() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let red = Style::default().fg(Color::Indexed(1));
        let view = PaneView {
            title: "sh".to_string(),
            lines: vec![vec![SpanView {
                text: "RED".to_string(),
                style: red,
            }]],
            live: true,
            focused: true,
            cursor: None,
        };
        terminal
            .draw(|f| render(f, area(), &[view], &chrome()))
            .unwrap();
        let buf = terminal.backend().buffer();
        let w = buf.area.width as usize;
        let cell = &buf.content[2 * w + 1];
        assert_eq!(cell.symbol(), "R");
        assert_eq!(cell.fg, Color::Indexed(1));
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

    #[test]
    fn empty_grid_explains_itself() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render(f, area(), &[], &chrome()))
            .unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Ctrl-b c"), "empty state guides: {text:?}");
    }

    #[test]
    fn render_shows_only_the_focused_session() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut old = pane("old", "bye", false);
        old.focused = false;
        let new = pane("new", "hi", true);
        let mut c = chrome();
        c.tabs = vec![
            SessionTab { title: "old".to_string(), live: false, focused: false, group: None, group_color: None },
            SessionTab { title: "new".to_string(), live: true, focused: true, group: None, group_color: None },
        ];
        terminal
            .draw(|f| render(f, area(), &[old, new], &c))
            .unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("hi"), "focused body visible");
        assert!(!text.contains("bye"), "background body hidden");
        assert!(text.contains("1 old") && text.contains("2 new"), "both buttons");
        let _ = theme::style(theme::Role::Text);
    }
}
