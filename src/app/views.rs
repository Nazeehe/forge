//! AppState view selectors: topbar, tabs, sidebar, and layout toggles.

use super::*;

impl AppState {
    /// Per-session tab strip for the focused session: agent CLI, human
    /// terminal, and lazygit SCM tabs, plus read-only overlay views.
    /// Empty when nothing is focused.
    pub fn topbar(&self) -> crate::ui::topbar::TopBar {
        let Some(id) = self.manager.active() else {
            return crate::ui::topbar::TopBar::default();
        };
        let Some(rec) = self.manager.get(id) else {
            return crate::ui::topbar::TopBar::default();
        };
        let mut tabs: Vec<crate::ui::topbar::TopTab> = rec
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| crate::ui::topbar::TopTab {
                label: match tab.kind {
                    crate::session::TabKind::Agent => {
                        let mut chars = rec.cli_tool.chars();
                        match chars.next() {
                            None => "Agent".to_string(),
                            Some(first) => {
                                first.to_uppercase().collect::<String>() + chars.as_str()
                            }
                        }
                    }
                    crate::session::TabKind::Terminal => "Terminal".to_string(),
                    crate::session::TabKind::Scm => "SCM".to_string(),
                },
                active: i == rec.active_tab,
            })
            .collect();
        // Overlay tabs stay reachable at any width (narrow only drops
        // their icons below): the Writer tab in particular must remain
        // selectable when the terminal shrinks under 100 columns.
        // Below 100 columns Visual and Walkthrough drop first, since
        // only Writer has a narrow layout; Writer itself always stays.
        if rec.tabs.len() > 1 {
            for (slot, label) in self.overlay_slots(id) {
                tabs.push(crate::ui::topbar::TopTab {
                    label: label.to_string(),
                    active: self.overlay_view == Some((id, slot)),
                });
            }
            if self.overlay_view.is_some_and(|(view_id, _)| view_id == id) {
                for tab in tabs.iter_mut().take(rec.tabs.len()) { tab.active = false; }
            }
        }
        // Emoji icons: each is one codepoint with default emoji
        // presentation, so every icon is unambiguously two cells wide
        // (no VS16, no ambiguous-width glyphs) and `Line::width`
        // measures the buttons exactly.
        if self.term_size.1 >= 100 {
            for (tab, icon) in tabs.iter_mut().zip(["🤖", "💻", "🔀", "📷", "📖", "📝"]) {
                tab.label = format!("{icon} {}", tab.label);
            }
        }
        crate::ui::topbar::TopBar { tabs }
    }

    /// Overlay tabs with their session slots in strip order. Below
    /// 100 columns Visual and Walkthrough drop (only Writer has a
    /// narrow layout); both the strip and the selection path share
    /// this, so a shown position always maps back to its real slot.
    fn overlay_slots(&self, id: crate::session::SessionId) -> Vec<(usize, &'static str)> {
        let Some(rec) = self.manager.get(id) else {
            return Vec::new();
        };
        if rec.tabs.len() <= 1 {
            return Vec::new();
        }
        let narrow = self.term_size.1 < 100;
        OVERLAY_TABS
            .iter()
            .enumerate()
            .filter(|(_, label)| !narrow || ***label == *"Writer")
            .map(|(index, label)| (rec.tabs.len() + index, *label))
            .collect()
    }

    /// Select a PTY tab or one of the read-only view slots shown in the
    /// agent's top bar. Extra views never create a PTY or accept typing.
    pub fn select_top_tab(&mut self, index: usize) -> bool {
        let Some(id) = self.manager.active() else { return false; };
        let Some(rec) = self.manager.get(id) else { return false; };
        if index >= self.topbar().tabs.len() { return false; }
        if index < rec.tabs.len() {
            let was_overlay = self.overlay_view.take().is_some();
            let changed = self.manager.select_tab(id, index);
            self.dirty |= was_overlay || changed;
            was_overlay || changed
        } else {
            // Shown positions past the PTY tabs map back through the
            // same filtered list the strip paints.
            let slots = self.overlay_slots(id);
            let shown = index.saturating_sub(rec.tabs.len());
            let Some((slot, _)) = slots.get(shown) else {
                return false;
            };
            let slot = *slot;
            let next = Some((id, slot));
            let changed = self.overlay_view != next;
            self.overlay_view = next;
            // The Writer entry exists however the tab gets selected:
            // without it the draw closure paints nothing (blank tab)
            // and keys/mouse find no session.
            if Some(slot) == self.writer_slot(id) {
                self.writers.entry(id).or_default();
                // The scan runs when the tab opens (cached, never per
                // frame), so the Recent list is fresh on entry.
                self.writer_refresh_recent(id);
            }
            self.dirty |= changed;
            changed
        }
    }

    pub fn overlay_active(&self) -> bool {
        self.overlay_view.is_some_and(|(id, _)| self.manager.active() == Some(id))
    }

    /// Snapshot the grid: one view per session in manager order.
    pub fn views(&self) -> Vec<crate::ui::PaneView> {
        let active = self.manager.active();
        self.manager
            .order()
            .iter()
            .map(|&id| {
                let rec = self.manager.get(id).expect("ordered session exists");
                let live = rec.state.is_live();
                let focused = Some(id) == active;
                // Only the focused pane (or every pane in grid mode) ever
                // reaches the screen: `render_focused` draws just the one
                // `focused` view, `render_grid` draws them all. Materializing
                // styled rows for a session nobody is looking at means every
                // background session's paint cost rides along on each frame,
                // so input latency degrades as more sessions are opened even
                // though only one is ever on screen. Skip the conversion
                // entirely for anything neither focused nor grid-visible.
                if !self.grid_mode && !focused {
                    return crate::ui::PaneView {
                        title: rec.name.clone(),
                        lines: Vec::new(),
                        live,
                        focused,
                        cursor: None,
                    };
                }
                if let Some((view_id, index)) = self.overlay_view {
                    if focused && view_id == id {
                        let label = ["", "", "", "Visual", "Walkthrough", "Writer"]
                            .get(index).copied().unwrap_or("View");
                        if label == "Walkthrough" {
                            return self.walkthrough_view(id);
                        }
                        #[cfg(feature = "visual")]
                        if label == "Visual" {
                            return self.visual_view(id, crate::visual::kitty_supported_env());
                        }
                        if label == "Writer" {
                            // The Writer overlay paints itself directly
                            // in the draw closure (live EdTUI widget);
                            // the main area stays empty behind it.
                            return crate::ui::PaneView {
                                title: format!("{} · Writer", rec.name),
                                lines: Vec::new(),
                                live,
                                focused: true,
                                cursor: None,
                            };
                        }
                        return crate::ui::PaneView {
                            title: format!("{} · {label}", rec.name),
                            lines: vec![vec![crate::ui::SpanView {
                                text: format!("{label} view unavailable in this build"),
                                style: crate::ui::theme::style(crate::ui::theme::Role::Muted),
                            }]],
                            live,
                            focused: true,
                            cursor: None,
                        };
                    }
                }
                let mut lines: Vec<Vec<crate::ui::SpanView>> = self
                    .manager
                    .styled_rows(id)
                    .iter()
                    .map(|row| row.iter().map(crate::ui::span_for).collect())
                    .collect();
                if lines.is_empty() {
                    // Blank scrollback means opposite things by state: a
                    // live pane simply hasn't printed yet, an exited one
                    // is gone.
                    let text = if live {
                        "(running — no output yet)"
                    } else {
                        "(exited)"
                    };
                    lines = vec![vec![crate::ui::SpanView {
                        text: text.to_string(),
                        style: ratatui::style::Style::default(),
                    }]];
                }
                crate::ui::PaneView {
                    title: rec.name.clone(),
                    lines,
                    live,
                    focused,
                    cursor: self.manager.cursor(id),
                }
            })
            .collect()
    }

    /// Session-bar tabs in order with live/focus flags.
    pub fn tabs(&self) -> Vec<crate::ui::session_bar::SessionTab> {
        let active = self.manager.active();
        self.bar_order()
            .into_iter()
            .filter_map(|id| {
                self.manager.get(id).map(|rec| {
                    let group = self.broker.primary_group(id).map(str::to_string);
                    let group_color = group
                        .as_deref()
                        .and_then(|g| self.broker.group_color(g));
                    crate::ui::session_bar::SessionTab {
                        title: rec.name.clone(),
                        live: rec.state.is_live(),
                        focused: Some(id) == active,
                        group,
                        group_color,
                    }
                })
            })
            .collect()
    }

    /// Sidebar content: the focused session's detail plus pending hooks
    /// and the live permission mode.
    /// One-line lifecycle label shared by the focused detail and the
    /// fleet rows, so both always agree on what a session is doing.
    fn session_state_label(rec: &crate::session::SessionRecord) -> String {
        if rec.state.is_live() {
            match rec.activity {
                crate::session::Activity::Idle => "running".to_string(),
                activity => format!("running · {activity:?}"),
            }
        } else {
            match rec.exit_code {
                Some(code) => format!("exited({code})"),
                None => "exited".to_string(),
            }
        }
    }

    pub fn sidebar_info(&self) -> crate::ui::sidebar::SidebarInfo {
        let now = std::time::Instant::now();
        let active = self.manager.active();
        let session = active.and_then(|id| {
            self.manager.get(id).map(|rec| {
                crate::ui::sidebar::SessionDetail {
                    name: rec.name.clone(),
                    cli_tool: rec.cli_tool.clone(),
                    cwd: rec.cwd.to_string_lossy().into_owned(),
                    state: Self::session_state_label(rec),
                    status: rec.status.as_ref().map(|s| s.display()),
                    status_kind: rec.status.as_ref().map(|s| s.kind),
                    // Armed timers show for the focused session only;
                    // other sessions collapse to a count below.
                    timers: self
                        .broker
                        .timers_for(id)
                        .into_iter()
                        .map(|(timer_id, due)| crate::ui::sidebar::TimerView {
                            id: timer_id,
                            remaining: crate::ui::sidebar::format_countdown(
                                due.saturating_duration_since(now),
                            ),
                        })
                        .collect(),
                }
            })
        });
        // Fleet rows in router order with reasons; non-focused armed
        // timers collapse to one count for the focused block.
        let mut sessions = Vec::new();
        let mut other_timers = 0;
        for id in self.sorted_fleet_ids() {
            let Some(rec) = self.manager.get(id) else {
                continue;
            };
            let tier_n = self.fleet_tier(id, rec, active);
            let tier = match tier_n {
                0 => crate::ui::sidebar::FleetTier::Attention,
                1 => crate::ui::sidebar::FleetTier::Working,
                _ => crate::ui::sidebar::FleetTier::Idle,
            };
            // Reason prefers the explicit ping, then the sticky
            // status; the emoji mirrors the same source.
            let (reason, emoji) = if let Some((r, _)) = self.attention_flags.get(&id) {
                (r.clone(), "🔔")
            } else if active != Some(id) {
                if let Some(badge) = self.message_user_badges.get(&id) {
                    (badge.clone(), "🔔")
                } else if let Some(st) = rec.status.as_ref() {
                    (st.display(), crate::ui::sidebar::status_emoji(st.kind))
                } else {
                    (Self::session_state_label(rec), "")
                }
            } else if let Some(st) = rec.status.as_ref() {
                (st.display(), crate::ui::sidebar::status_emoji(st.kind))
            } else {
                (Self::session_state_label(rec), "")
            };
            if Some(id) != active {
                other_timers += self.broker.timers_for(id).len();
            }
            sessions.push(crate::ui::sidebar::FleetRow {
                id,
                name: rec.name.clone(),
                tier,
                state: Self::session_state_label(rec),
                reason,
                emoji,
            });
        }
        let fleet_scroll = self
            .fleet_scroll
            .min(sessions.len().saturating_sub(1));
        let telegram_on = self
            .telegram_config
            .lock()
            .map(|cfg| cfg.enabled)
            .unwrap_or(false);
        // A failing last poll shows as retrying: without it a stalled
        // poller (e.g. long backoff after early failures) looks exactly
        // like a quiet healthy one.
        let telegram_state = if telegram_on && self.telegram_last_send_failed {
            "on · delivery failed"
        } else if telegram_on && self.telegram_last_poll_failed {
            "on · retrying"
        } else if telegram_on {
            "on"
        } else {
            "off"
        };
        let telegram_badge = self.last_telegram_badged.and_then(|id| {
            let name = self.manager.get(id)?.name.clone();
            let text = self.message_user_badges.get(&id)?.clone();
            Some((name, text))
        });
        crate::ui::sidebar::SidebarInfo {
            session,
            sessions,
            active,
            fleet_cursor: self.fleet_cursor,
            fleet_scroll,
            other_timers,
            pending: self.pending_hooks.len(),
            mode: self.permission_mode.as_str(),
            board_open: self.board_open,
            telegram: telegram_state,
            telegram_badge,
        }
    }

    /// Flip grid mode; selecting a session by number leaves it.
    pub fn toggle_grid(&mut self) {
        self.grid_mode = !self.grid_mode;
        self.dirty = true;
    }

    /// Flip the sidebar Tetris view. Opening arms the gravity timer
    /// without stepping, so the piece never jumps on entry.
    pub fn toggle_tetris(&mut self) {
        self.tetris_open = !self.tetris_open;
        if self.tetris_open {
            self.tetris_last_drop = None;
        }
        self.dirty = true;
    }

    /// Advance gravity when due. Pure in `now` so tests never sleep:
    /// first call arms the timer, later calls step once per interval.
    pub fn tetris_tick(&mut self, now: std::time::Instant) {
        if !self.tetris_open || self.tetris.is_over() || self.tetris.is_paused() {
            return;
        }
        match self.tetris_last_drop {
            None => self.tetris_last_drop = Some(now),
            Some(last) => {
                if now.duration_since(last).as_millis() >= self.tetris.drop_interval_ms() as u128
                {
                    self.tetris.step();
                    self.tetris_last_drop = Some(now);
                    self.dirty = true;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::*;
    use crate::infra::ids::RunId;

    #[test]
    fn toggle_grid_flips_and_select_exits() {
        let mut s = AppState::new();
        assert!(!s.grid_mode);
        s.toggle_grid();
        assert!(s.grid_mode);
        s.toggle_grid();
        assert!(!s.grid_mode);
        // Picking a number always returns to the focused view.
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        s.toggle_grid();
        assert!(s.select_session(1));
        assert!(!s.grid_mode, "select exits grid");
        assert_eq!(s.manager.active(), Some(b));
        s.toggle_grid();
        assert!(!s.select_session(9), "out of range");
        assert!(s.grid_mode, "failed select stays in grid");
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn views_begin_empty() {
        let s = AppState::new();
        assert!(s.views().is_empty());
    }

    #[test]
    fn views_reflect_sessions() {
        let mut s = AppState::new();
        let a = s
            .manager
            .spawn("one", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        let b = s
            .manager
            .spawn("two", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        let views = s.views();
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].title, "one");
        assert_eq!(views[1].title, "two");
        assert!(views[0].focused && !views[1].focused);
        assert!(views.iter().all(|v| v.live));
        s.step_session(1);
        assert_eq!(s.manager.active(), Some(b));
        s.step_session(1);
        assert_eq!(s.manager.active(), Some(a));
        s.step_session(-1);
        assert_eq!(s.manager.active(), Some(b));
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn views_skip_line_materialization_for_background_sessions() {
        let mut s = AppState::new();
        let a = s
            .manager
            .spawn("one", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        let b = s
            .manager
            .spawn(
                "two",
                &std::env::temp_dir(),
                "echo BACKGROUND-MARKER; exec sleep 30",
                RunId::generate(),
                "shell",
            )
            .unwrap();
        assert_eq!(s.manager.active(), Some(a), "first spawn stays focused");
        // Wait for the background session to actually paint its marker
        // into the vt100 screen, so there is a real non-empty screen to
        // skip below (an empty screen would pass trivially).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for _ in s.manager.drain_pty_max(100) {}
            if s
                .manager
                .screen_text(b)
                .is_some_and(|text| text.contains("BACKGROUND-MARKER"))
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "background session never painted its marker"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        // Focused (non-grid) mode: only the focused pane's lines get
        // materialized. The background session behind it must not pay
        // for a screen it never draws (app.rs `views()`).
        let focused_mode = s.views();
        let bg_view = focused_mode.iter().find(|v| v.title == "two").unwrap();
        assert!(
            bg_view.lines.is_empty(),
            "background pane must skip line materialization outside grid mode"
        );
        let fg_view = focused_mode.iter().find(|v| v.title == "one").unwrap();
        assert!(!fg_view.lines.is_empty(), "the focused pane still renders");

        // Grid mode draws every pane, so every session still needs real
        // lines there.
        s.grid_mode = true;
        let grid_mode = s.views();
        let bg_view = grid_mode.iter().find(|v| v.title == "two").unwrap();
        let rendered: String = bg_view
            .lines
            .iter()
            .flatten()
            .map(|span| span.text.as_str())
            .collect();
        assert!(
            rendered.contains("BACKGROUND-MARKER"),
            "grid mode must still materialize every pane, got: {rendered:?}"
        );

        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn tetris_toggle_flips_sidebar_game() {
        let mut s = AppState::new();
        assert!(!s.tetris_open);
        s.dirty = false;
        s.toggle_tetris();
        assert!(s.tetris_open);
        assert!(s.dirty);
        s.toggle_tetris();
        assert!(!s.tetris_open);
    }

    #[test]
    fn tetris_tick_steps_gravity_when_due() {
        let mut s = AppState::new();
        let t0 = std::time::Instant::now();
        s.toggle_tetris();
        s.tetris_tick(t0);
        assert!(s.tetris_last_drop.is_some(), "first tick arms the timer");
        let before = s.tetris.active_cells();
        s.dirty = false;
        s.tetris_tick(t0 + std::time::Duration::from_millis(900));
        assert_ne!(s.tetris.active_cells(), before, "gravity moves the piece");
        assert!(s.dirty);
    }

    #[test]
    fn tetris_tick_sleeps_while_closed() {
        let mut s = AppState::new();
        let t0 = std::time::Instant::now();
        s.tetris_tick(t0 + std::time::Duration::from_secs(60));
        assert!(s.tetris_last_drop.is_none(), "closed game never arms");
    }

    #[test]
    fn scm_tab_selects_a_live_lazygit_pane() {
        let mut state = AppState::new();
        state.term_size = (40, 180);
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::infra::ids::RunId::generate(), "codex",
        ).unwrap();
        // Index 2 is a real PTY tab now, not an overlay: selecting it
        // lazily spawns the pane (the shell reports a missing binary as
        // an exited child, so this holds with or without lazygit).
        assert!(state.select_top_tab(2));
        assert!(state.topbar().tabs[2].active);
        assert_eq!(
            state.manager.active_tab_kind(id),
            Some(crate::session::TabKind::Scm)
        );
        assert!(state.manager.remove(id));
    }

    #[test]
    fn agent_topbar_exposes_clickable_read_only_views() {
        let mut state = AppState::new();
        state.term_size = (40, 180);
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::infra::ids::RunId::generate(), "codex",
        ).unwrap();
        let labels: Vec<String> = state.topbar().tabs.iter().map(|tab| tab.label.clone()).collect();
        assert_eq!(labels, ["🤖 Codex", "💻 Terminal", "🔀 SCM", "📷 Visual", "📖 Walkthrough", "📝 Writer"]);
        assert!(state.select_top_tab(3));
        assert!(state.topbar().tabs[3].active);
        let view = state.views().into_iter().find(|v| v.focused).unwrap();
        assert!(view.lines.iter().flatten().any(|span| span.text.contains("renders diagrams")));
        assert!(view.lines.iter().flatten().any(|span| span.text.contains("visualize the flow for")));
        assert!(state.select_top_tab(0));
        assert!(state.topbar().tabs[0].active);
        assert!(state.manager.remove(id));
    }

    #[test]
    fn topbar_omits_events_and_tasks() {
        let mut state = AppState::new();
        state.term_size = (40, 180);
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::infra::ids::RunId::generate(), "codex",
        ).unwrap();
        let labels: Vec<String> = state.topbar().tabs.iter().map(|tab| tab.label.clone()).collect();
        assert_eq!(labels, ["🤖 Codex", "💻 Terminal", "🔀 SCM", "📷 Visual", "📖 Walkthrough", "📝 Writer"]);
        assert!(state.manager.remove(id));
    }

    #[test]
    fn wide_agent_topbar_uses_labeled_icons_from_reference() {
        let mut state = AppState::new();
        state.term_size = (40, 180);
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::infra::ids::RunId::generate(), "codex",
        ).unwrap();
        let labels: Vec<String> = state.topbar().tabs.iter().map(|tab| tab.label.clone()).collect();
        assert!(labels[0].starts_with("🤖 "));
        assert!(labels[1].starts_with("💻 "));
        assert!(labels[2].starts_with("🔀 "));
        assert!(labels[3].starts_with("📷 "));
        assert!(labels[4].starts_with("📖 "));
        assert!(state.manager.remove(id));
    }

    #[test]
    fn shrinking_hides_and_closes_extra_view() {
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::infra::ids::RunId::generate(), "codex",
        ).unwrap();
        assert!(state.select_top_tab(3));
        state.apply(AppEvent::Resize(24, 80));
        assert!(!state.overlay_active());
        // Only Writer survives in the narrow strip (R1): the evicted
        // view's tab is gone, Writer's stays reachable.
        assert_eq!(state.topbar().tabs.len(), 4);
        assert!(state.manager.remove(id));
    }

    #[test]
    fn narrow_topbar_keeps_writer_and_drops_visual_walkthrough() {
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(30, 90));
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::infra::ids::RunId::generate(), "codex",
        ).unwrap();
        let labels: Vec<String> = state.topbar().tabs.iter().map(|tab| tab.label.clone()).collect();
        assert!(labels.iter().any(|l| l.contains("Writer")), "writer survives: {labels:?}");
        assert!(!labels.iter().any(|l| l.contains("Visual")), "visual drops first: {labels:?}");
        assert!(!labels.iter().any(|l| l.contains("Walkthrough")), "walkthrough drops first: {labels:?}");
        assert!(state.manager.remove(id));
    }

    #[test]
    fn ordinary_wide_terminal_keeps_all_topbar_views_visible() {
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(30, 120));
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::infra::ids::RunId::generate(), "codex",
        ).unwrap();
        assert_eq!(state.topbar().tabs.len(), 6);
        let bar = crate::ui::layout::chrome_areas(ratatui::layout::Rect::new(0, 0, 120, 30)).topbar;
        assert_eq!(crate::ui::topbar::layout_topbar(bar, &state.topbar().tabs, false).len(), 6);
        assert!(state.manager.remove(id));
    }

    #[test]
    fn sidebar_collapses_background_timers() {
        std::env::set_var("CODEX_BIN", "cat");
        let mut state = AppState::new();
        let a = state
            .manager
            .spawn_agent(
                "a",
                &std::env::temp_dir(),
                "exec cat",
                RunId::generate(),
                "codex",
            )
            .unwrap();
        let b = state
            .manager
            .spawn_agent(
                "b",
                &std::env::temp_dir(),
                "exec cat",
                RunId::generate(),
                "codex",
            )
            .unwrap();
        std::env::remove_var("CODEX_BIN");
        let run_a = state.manager.get(a).unwrap().run_id.as_str().to_string();
        let out = comms_reply(
            &mut state,
            &run_a,
            "schedule_prompt",
            r#"{"prompt":"later","delay_seconds":600}"#,
        );
        assert!(out.contains("timer_id"), "armed: {out}");
        state.manager.switch(b);
        let info = state.sidebar_info();
        assert!(
            info.session.expect("detail renders").timers.is_empty(),
            "focused shows only its own"
        );
        assert_eq!(info.other_timers, 1, "background collapses to a count");
        assert!(state.manager.remove(a));
        assert!(state.manager.remove(b));
    }

    #[test]
    fn sidebar_shows_timers_only_for_focused_session() {
        std::env::set_var("CODEX_BIN", "cat");
        let mut state = AppState::new();
        let a = state.manager.spawn_agent(
            "a", &std::env::temp_dir(), "exec cat",
            crate::infra::ids::RunId::generate(), "codex",
        ).unwrap();
        let b = state.manager.spawn_agent(
            "b", &std::env::temp_dir(), "exec cat",
            crate::infra::ids::RunId::generate(), "codex",
        ).unwrap();
        std::env::remove_var("CODEX_BIN");
        let run_a = state.manager.get(a).unwrap().run_id.as_str().to_string();
        let out = comms_reply(
            &mut state, &run_a, "schedule_prompt",
            r#"{"prompt":"later","delay_seconds":600}"#,
        );
        let timer = crate::hooks::policy::json_string_field(out.as_bytes(), &["timer_id"]).unwrap();
        assert_eq!(state.manager.active(), Some(a));
        let focused = state.sidebar_info().session.expect("detail renders");
        assert_eq!(focused.timers.len(), 1, "focused session shows its timer");
        assert_eq!(focused.timers[0].id, timer);
        state.manager.switch(b);
        let other = state.sidebar_info().session.expect("detail renders");
        assert!(other.timers.is_empty(), "unfocused timers stay hidden");
        assert!(state.cancel_timer(&timer), "sidebar cancel drops it");
        assert!(state.broker.timers_for(a).is_empty());
        assert!(!state.cancel_timer(&timer), "second cancel stays false");
        assert!(state.manager.remove(a));
        assert!(state.manager.remove(b));
    }

    #[test]
    fn sidebar_info_marks_failing_telegram_poll() {
        let mut state = AppState::new();
        state.telegram_config.lock().expect("lock").enabled = true;
        state.telegram_last_poll_failed = true;
        assert_eq!(state.sidebar_info().telegram, "on · retrying");
        state.telegram_last_poll_failed = false;
        assert_eq!(state.sidebar_info().telegram, "on");
        state.telegram_config.lock().expect("lock").enabled = false;
        assert_eq!(state.sidebar_info().telegram, "off");
    }
}
