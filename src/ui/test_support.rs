//! Shared UI test fixtures: chrome builders and buffer readers.

use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::Terminal;

use super::{Chrome, PaneView, SessionTab, SpanView};
use super::sidebar::{FleetRow, FleetTier, SessionDetail, SidebarInfo, TimerView};
use super::topbar::{TopBar, TopTab};

pub(super) fn area() -> Rect {
    Rect::new(0, 0, 80, 24)
}

pub(super) fn tab(title: &str, focused: bool) -> SessionTab {
    SessionTab {
        title: title.to_string(),
        live: true,
        focused,
        group: None,
        group_color: None,
    }
}

pub(super) fn grouped(title: &str, focused: bool, group: &str, color: usize) -> SessionTab {
    SessionTab {
        title: title.to_string(),
        live: true,
        focused,
        group: Some(group.to_string()),
        group_color: Some(color),
    }
}

pub(super) fn text_of(line: &Line) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

pub(super) fn has(lines: &[Line], needle: &str) -> bool {
    lines.iter().any(|l| text_of(l).contains(needle))
}

#[cfg(test)]
pub(super) fn sidebar_chrome() -> Chrome {
    Chrome {
        tabs: vec![],
        topbar: TopBar { tabs: vec![] },
        detail: Some(SessionDetail {
            name: "shell-1".to_string(),
            cli_tool: "shell".to_string(),
            cwd: "/tmp/proj".to_string(),
            state: "running".to_string(),
            status: Some("blocked: waiting on review".to_string()),
            status_kind: Some(crate::session::status::StatusKind::Blocked),
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
        board_open: false,
        board: None,
        tetris_open: false,
        tetris: None,
        grid: false,
        pills: true,
    }
}

pub(super) fn timed_detail() -> SessionDetail {
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

pub(super) fn chrome() -> Chrome {
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
        board_open: false,
        board: None,
        tetris_open: false,
        tetris: None,
        telegram: "off",
        telegram_badge: None,
        grid: false,
        pills: false,
    }
}

pub(super) fn buffer_rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buf = terminal.backend().buffer();
    let w = buf.area.width as usize;
    buf.content
        .chunks(w)
        .map(|row| row.iter().map(|c| c.symbol().to_string()).collect())
        .collect()
}

pub(super) fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol().to_string())
        .collect()
}

pub(super) fn pane(title: &str, body: &str, live: bool) -> PaneView {
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

pub(super) fn for_h_fleet(info: &mut SidebarInfo) {
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

