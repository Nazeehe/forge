//! Single-owner application state.
//!
//! The main loop (and, in tests, the harness directly) reduces every
//! `AppEvent` through [`AppState::apply`]. Workers never touch this struct;
//! dirty-flag rendering and shutdown flow out of the same reduction.

use crate::event::AppEvent;
use crate::session::SessionManager;

/// Restore outcome counts for the status line.
pub struct RestoreReport {
    pub spawned: usize,
    pub skipped: Vec<String>,
}

pub struct AppState {
    pub manager: SessionManager,
    pub dirty: bool,
    pub should_quit: bool,
    pub term_size: (u16, u16),
    /// Hook records awaiting a policy decision. Bounded: beyond the cap
    /// newcomers are dropped and their relays fail open on timeout.
    pub pending_hooks: std::collections::VecDeque<crate::listener::HookRequest>,
    /// Open create-session dialog, if any. Captures all input while present.
    pub create_dialog: Option<crate::create::CreateDialog>,
    /// Open group-management dialog, if any. Captures all input while
    /// present; never both dialogs at once (openers are unreachable
    /// behind the other dialog).
    pub group_dialog: Option<crate::groups::GroupDialog>,
    /// Startup restore picker, if a sessions file offered entries. First
    /// input goes here until it resolves to a pick or a fresh start.
    pub restore_picker: Option<crate::checkpoint::RestorePicker>,
    /// Quit-confirmation modal, if the quit chord is pending an answer.
    /// Captures all input while present; No is the default.
    pub quit_confirm: Option<crate::quit::QuitConfirm>,
    /// Live permission mode. The TUI loop rebuilds policy and persists the
    /// config whenever this diverges from the loaded one.
    pub permission_mode: crate::config::PermissionMode,
    /// Cross-session message broker (Phase 4): groups, conversations, queues.
    pub broker: crate::comms::Broker,
    /// Last human key/paste forwarded to a pane. Injections wait out a short
    /// debounce after typing so they never interleave with user input.
    pub last_human_input: Option<std::time::Instant>,
    /// Last hook verdict or queued hook request. Injections wait out
    /// [`crate::comms::INJECT_HOOK_DEBOUNCE`] after hook activity so a
    /// body never races a mid-tool-use verdict into the same pane.
    pub last_hook_activity: Option<std::time::Instant>,
    /// Sessions owed a staged Enter: an injection body went out and its CR
    /// follows after [`crate::comms::INJECT_ENTER_DELAY`], staged per
    /// session and re-armed by newer bodies.
    pub pending_enter: std::collections::HashMap<crate::session::SessionId, std::time::Instant>,
    /// One read-only chrome view, scoped to the currently focused session.
    pub overlay_view: Option<(crate::session::SessionId, usize)>,
    /// Live walkthroughs by session. Entered agent-side through the
    /// walkthrough_* tools; the overlay opens on start and the human
    /// steps through with j/k, asking with Enter.
    pub walkthroughs: std::collections::HashMap<crate::session::SessionId, crate::walkthrough::Walkthrough>,
    /// Grid mode (`Ctrl-b w`): the main area tiles every session in
    /// framed cells instead of showing only the focused one.
    pub grid_mode: bool,
    /// Monotonic visual generation: every accepted `visual_show` takes
    /// the next number so stale renders never win. U3 scopes this per
    /// session with the raster state; the global counter stays as the
    /// tiebreak source.
    pub visual_seq: u64,
    /// Rounded pill buttons everywhere; mirrors the config flag at startup.
    pub pill_tabs: bool,
    /// Sticky per-session visuals: the newest completed generation per
    /// session. Raster failures drop the frame; the accepted verdict
    /// stands and the previous frame (if any) stays put.
    #[cfg(feature = "visual")]
    pub visual_slots: std::collections::HashMap<crate::session::SessionId, VisualSlot>,
    /// Oldest-first recency for eviction; touched on every store.
    #[cfg(feature = "visual")]
    pub visual_lru: std::collections::VecDeque<crate::session::SessionId>,
    /// Image budget, tunable in tests; production defaults below.
    #[cfg(feature = "visual")]
    pub visual_budget_bytes: usize,
    /// Slot count cap, tunable in tests; production defaults below.
    #[cfg(feature = "visual")]
    pub visual_budget_count: usize,
    /// Background raster completions; drained once per main-loop pass.
    #[cfg(feature = "visual")]
    visual_tx: std::sync::mpsc::Sender<VisualDone>,
    /// Drain end of the raster channel; see `drain_visual`.
    #[cfg(feature = "visual")]
    visual_rx: std::sync::mpsc::Receiver<VisualDone>,
    /// Terminal image on screen now; see `visual_take_show`.
    #[cfg(feature = "visual")]
    pub visual_shown: Option<VisualShown>,
    /// Placement id source; monotonic so retransmits never collide.
    #[cfg(feature = "visual")]
    visual_image_seq: u32,
}

/// Terminal image currently on screen, if any. The TUI writes the
/// transmit/delete escapes; this only tracks what the screen holds so
/// repeats and orphans never happen.
#[cfg(feature = "visual")]
pub struct VisualShown {
    pub session: crate::session::SessionId,
    pub generation: u64,
    pub image_id: u32,
    pub paint: crate::visual::VisualPaint,
}

/// Claim for one transmit: the TUI positions the cursor and writes it.
#[cfg(feature = "visual")]
pub struct VisualShowSpec {
    pub image_id: u32,
    pub session: crate::session::SessionId,
    pub generation: u64,
    pub paint: crate::visual::VisualPaint,
    /// Same placement id, new pixels: the TUI deletes before
    /// re-transmitting instead of claiming a fresh id.
    pub replace: bool,
}

/// One finished background raster, headed for a session slot.
#[cfg(feature = "visual")]
pub struct VisualDone {
    pub session: crate::session::SessionId,
    pub generation: u64,
    pub title: String,
    pub alt: String,
    pub result: Result<crate::visual::RasterFrame, String>,
}

/// The newest completed visual for one session.
#[cfg(feature = "visual")]
pub struct VisualSlot {
    pub generation: u64,
    pub png: Vec<u8>,
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub title: String,
    pub alt: String,
    /// Zoom factor for the Visual tab viewport: 1.0 shows the whole
    /// frame contained in the tab, higher zooms scrollable overflow.
    pub zoom: f32,
    /// Viewport origin in displayed cells into the zoomed frame.
    pub scroll_x: u16,
    pub scroll_y: u16,
}

/// Production image budget: decoded bytes across all slots.
#[cfg(feature = "visual")]
pub const DEFAULT_VISUAL_BUDGET_BYTES: usize = 48 * 1024 * 1024;

/// Production slot cap: sticky visuals per session count.
#[cfg(feature = "visual")]
pub const DEFAULT_VISUAL_BUDGET_COUNT: usize = 8;

/// Overlay slot past the PTY tabs: Events, Tasks, Visual, Walkthrough.
/// Agent sessions always carry exactly three PTY tabs, so absolute
/// indices stay stable (3/4/5/6); other overlays are unreachable on
/// single-tab shells by the same gate as the topbar.
pub const OVERLAY_TABS: [&str; 4] = ["Events", "Tasks", "Visual", "Walkthrough"];

impl AppState {
    pub fn new() -> Self {
        #[cfg(feature = "visual")]
        let (visual_tx, visual_rx) = std::sync::mpsc::channel();
        AppState {
            manager: SessionManager::new(),
            dirty: true,
            should_quit: false,
            term_size: (24, 80),
            pending_hooks: std::collections::VecDeque::new(),
            create_dialog: None,
            group_dialog: None,
            restore_picker: None,
            quit_confirm: None,
            permission_mode: crate::config::PermissionMode::Yolo,
            broker: crate::comms::Broker::new(),
            last_human_input: None,
            last_hook_activity: None,
            pending_enter: std::collections::HashMap::new(),
            overlay_view: None,
            walkthroughs: std::collections::HashMap::new(),
            grid_mode: false,
            visual_seq: 0,
            pill_tabs: true,
            #[cfg(feature = "visual")]
            visual_slots: std::collections::HashMap::new(),
            #[cfg(feature = "visual")]
            visual_lru: std::collections::VecDeque::new(),
            #[cfg(feature = "visual")]
            visual_budget_bytes: DEFAULT_VISUAL_BUDGET_BYTES,
            #[cfg(feature = "visual")]
            visual_budget_count: DEFAULT_VISUAL_BUDGET_COUNT,
            #[cfg(feature = "visual")]
            visual_tx,
            #[cfg(feature = "visual")]
            visual_rx,
            #[cfg(feature = "visual")]
            visual_shown: None,
            #[cfg(feature = "visual")]
            visual_image_seq: 0,
        }
    }

    /// Flip grid mode; selecting a session by number leaves it.
    pub fn toggle_grid(&mut self) {
        self.grid_mode = !self.grid_mode;
        self.dirty = true;
    }

    /// Set the live permission mode; true when it changed. Non Off/Yolo
    /// modes collapse to Yolo on toggle, never back (toggle only spans
    /// the two sidebar buttons).
    pub fn set_permission_mode(&mut self, mode: crate::config::PermissionMode) -> bool {
        if self.permission_mode == mode {
            return false;
        }
        self.permission_mode = mode;
        self.dirty = true;
        true
    }

    /// Toggle Off <-> Yolo for the keyboard path.
    pub fn toggle_permission_mode(&mut self) -> bool {
        let next = match self.permission_mode {
            crate::config::PermissionMode::Yolo => crate::config::PermissionMode::Off,
            _ => crate::config::PermissionMode::Yolo,
        };
        self.set_permission_mode(next)
    }

    /// Per-session tab strip for the focused session: agent CLI, human
    /// terminal, and lazygit SCM tabs, plus read-only overlay views.
    /// Empty when nothing is focused.
    pub fn topbar(&self) -> crate::ui::TopBar {
        let Some(id) = self.manager.active() else {
            return crate::ui::TopBar::default();
        };
        let Some(rec) = self.manager.get(id) else {
            return crate::ui::TopBar::default();
        };
        let mut tabs: Vec<crate::ui::TopTab> = rec
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| crate::ui::TopTab {
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
        if rec.tabs.len() > 1 && self.term_size.1 >= 100 {
            for (index, label) in OVERLAY_TABS.iter().enumerate() {
                tabs.push(crate::ui::TopTab {
                    label: (*label).to_string(),
                    active: self.overlay_view == Some((id, index + rec.tabs.len())),
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
            for (tab, icon) in tabs.iter_mut().zip(["🤖", "💻", "🔀", "🔔", "📝", "📷", "📖"]) {
                tab.label = format!("{icon} {}", tab.label);
            }
        }
        crate::ui::TopBar { tabs }
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
            let next = Some((id, index));
            let changed = self.overlay_view != next;
            self.overlay_view = next;
            self.dirty |= changed;
            changed
        }
    }

    pub fn overlay_active(&self) -> bool {
        self.overlay_view.is_some_and(|(id, _)| self.manager.active() == Some(id))
    }

    /// Absolute topbar index of the Walkthrough overlay slot for one
    /// session, or `None` for an unknown session.
    fn walkthrough_slot(&self, id: crate::session::SessionId) -> Option<usize> {
        let rec = self.manager.get(id)?;
        OVERLAY_TABS
            .iter()
            .position(|tab| *tab == "Walkthrough")
            .map(|slot| rec.tabs.len() + slot)
    }

    /// The focused walkthrough, if the active session sits on the
    /// Walkthrough overlay slot and the agent opened a tour. The TUI
    /// renders it immediate-mode over the pane grid.
    pub fn walkthrough_overlay(&self) -> Option<&crate::walkthrough::Walkthrough> {
        let active = self.manager.active()?;
        let (view_id, index) = self.overlay_view?;
        if view_id != active || Some(index) != self.walkthrough_slot(active) {
            return None;
        }
        self.walkthroughs.get(&active)
    }

    /// Mutable twin of [`Self::walkthrough_overlay`], for key routing.
    pub fn walkthrough_overlay_mut(
        &mut self,
    ) -> Option<&mut crate::walkthrough::Walkthrough> {
        let active = self.manager.active()?;
        let (view_id, index) = self.overlay_view?;
        if view_id != active || Some(index) != self.walkthrough_slot(active) {
            return None;
        }
        self.walkthroughs.get_mut(&active)
    }

    /// True while walkthrough keys own input: the overlay slot is
    /// focused and a tour is open for the active session.
    pub fn walkthrough_overlay_active(&self) -> bool {
        self.walkthrough_overlay().is_some()
    }

    /// Overlay slot index of the Visual tab for one session.
    #[cfg(feature = "visual")]
    fn visual_slot(&self, id: crate::session::SessionId) -> Option<usize> {
        let rec = self.manager.get(id)?;
        OVERLAY_TABS
            .iter()
            .position(|tab| *tab == "Visual")
            .map(|slot| rec.tabs.len() + slot)
    }

    /// The active session's stored visual, if it sits on its Visual
    /// slot: the (session, generation) the terminal should show.
    #[cfg(feature = "visual")]
    fn visual_overlay_active(&self) -> Option<(crate::session::SessionId, u64)> {
        let active = self.manager.active()?;
        let (view_id, index) = self.overlay_view?;
        if view_id != active || Some(index) != self.visual_slot(active) {
            return None;
        }
        let slot = self.visual_slots.get(&active)?;
        Some((active, slot.generation))
    }

    /// Claim the next terminal transmit, if the overlay wants an image
    /// the screen does not already show. The paint fingerprint covers
    /// zoom, scroll, crop, and cursor, so scrolling or zooming
    /// re-transmits under the same placement id (`replace`) while an
    /// untouched frame claims nothing. The TUI positions the cursor
    /// and writes the escape; `None` means nothing to do.
    #[cfg(feature = "visual")]
    pub fn visual_take_show(
        &mut self,
        kitty: bool,
        paint: crate::visual::VisualPaint,
    ) -> Option<VisualShowSpec> {
        if !kitty {
            return None;
        }
        let (session, generation) = self.visual_overlay_active()?;
        let same_slot = matches!(&self.visual_shown, Some(s) if s.session == session && s.generation == generation);
        if same_slot && matches!(&self.visual_shown, Some(s) if s.paint == paint) {
            return None;
        }
        let (image_id, replace) = match &self.visual_shown {
            Some(shown) if shown.session == session && shown.generation == generation => {
                (shown.image_id, true)
            }
            _ => {
                self.visual_image_seq += 1;
                (self.visual_image_seq, false)
            }
        };
        self.visual_shown = Some(VisualShown { session, generation, image_id, paint });
        Some(VisualShowSpec { image_id, session, generation, paint, replace })
    }

    /// Whether the focused tab is a session Visual tab: arrows,
    /// `+`/`-`, wheel, and zoom clicks route to its viewport.
    #[cfg(feature = "visual")]
    pub fn visual_tab_focused(&self) -> bool {
        self.visual_overlay_active().is_some()
    }

    /// Layout for one session's visual inside its tab image region:
    /// contained display size from zoom, clamped scroll, source crop,
    /// and the centered cursor. `cell_w`/`cell_h` come from the
    /// terminal's pixel report (Kitty path) or the exact 8x16 the
    /// half-block fallback draws with.
    #[cfg(feature = "visual")]
    pub fn visual_paint(
        &self,
        id: crate::session::SessionId,
        image: ratatui::layout::Rect,
        cell_w: f64,
        cell_h: f64,
    ) -> Option<crate::visual::VisualPaint> {
        use crate::visual::{clamp_scroll, crop_for_view, fit_display};
        let slot = self.visual_slots.get(&id)?;
        let (disp_cols, disp_rows) = fit_display(
            slot.width,
            slot.height,
            slot.zoom,
            image.width,
            image.height,
            cell_w,
            cell_h,
        );
        let (ox, oy) = clamp_scroll(
            slot.scroll_x,
            slot.scroll_y,
            disp_cols,
            disp_rows,
            image.width,
            image.height,
        );
        let crop =
            crop_for_view(slot.width, slot.height, disp_cols, disp_rows, image.width, image.height, ox, oy);
        Some(crate::visual::VisualPaint {
            zoom_bits: slot.zoom.to_bits(),
            ox,
            oy,
            out_cols: crop.out_cols,
            out_rows: crop.out_rows,
            cursor_x: image.x.saturating_add(image.width.saturating_sub(crop.out_cols) / 2),
            cursor_y: image.y.saturating_add(image.height.saturating_sub(crop.out_rows) / 2),
            sx: crop.sx,
            sy: crop.sy,
            sw: crop.sw,
            sh: crop.sh,
        })
    }

    /// Step one session's visual zoom, re-clamping the scroll offset
    /// to the new overflow. Geometry is the tab image region plus the
    /// cell size the paint uses. Returns whether anything changed.
    #[cfg(feature = "visual")]
    pub fn visual_zoom(
        &mut self,
        id: crate::session::SessionId,
        dir: crate::ui::VisualButton,
        area_cols: u16,
        area_rows: u16,
        cell_w: f64,
        cell_h: f64,
    ) -> bool {
        use crate::visual::{ZoomDir, clamp_scroll, fit_display, zoom_step};
        let Some(slot) = self.visual_slots.get_mut(&id) else {
            return false;
        };
        let dir = match dir {
            crate::ui::VisualButton::ZoomIn => ZoomDir::In,
            crate::ui::VisualButton::ZoomOut => ZoomDir::Out,
        };
        let next = zoom_step(slot.zoom, dir);
        if next == slot.zoom {
            return false;
        }
        slot.zoom = next;
        let (disp_cols, disp_rows) =
            fit_display(slot.width, slot.height, slot.zoom, area_cols, area_rows, cell_w, cell_h);
        (slot.scroll_x, slot.scroll_y) = clamp_scroll(
            slot.scroll_x,
            slot.scroll_y,
            disp_cols,
            disp_rows,
            area_cols,
            area_rows,
        );
        self.dirty = true;
        true
    }

    /// Pan one session's visual viewport by (`dx`, `dy`) displayed
    /// cells, positive right and down, clamped to the zoom overflow.
    /// Returns whether anything changed.
    #[cfg(feature = "visual")]
    pub fn visual_scroll(
        &mut self,
        id: crate::session::SessionId,
        dx: i16,
        dy: i16,
        area_cols: u16,
        area_rows: u16,
        cell_w: f64,
        cell_h: f64,
    ) -> bool {
        use crate::visual::{clamp_scroll, fit_display};
        let Some(slot) = self.visual_slots.get_mut(&id) else {
            return false;
        };
        let (disp_cols, disp_rows) =
            fit_display(slot.width, slot.height, slot.zoom, area_cols, area_rows, cell_w, cell_h);
        let max_x = disp_cols.saturating_sub(area_cols);
        let max_y = disp_rows.saturating_sub(area_rows);
        let (nx, ny) = (
            slot.scroll_x.saturating_add_signed(dx).min(max_x),
            slot.scroll_y.saturating_add_signed(dy).min(max_y),
        );
        let (nx, ny) = clamp_scroll(nx, ny, disp_cols, disp_rows, area_cols, area_rows);
        if (nx, ny) == (slot.scroll_x, slot.scroll_y) {
            return false;
        }
        slot.scroll_x = nx;
        slot.scroll_y = ny;
        self.dirty = true;
        true
    }

    /// The focused Visual tab's (session, generation), if the overlay
    /// sits on a visual slot with a stored frame.
    #[cfg(feature = "visual")]
    pub fn visual_focused_frame(&self) -> Option<(crate::session::SessionId, u64)> {
        self.visual_overlay_active()
    }

    /// PNG bytes for one paint of a stored frame: the frame itself
    /// when the viewport shows it whole (no re-encode), else the
    /// cropped region re-encoded for transmit. Independent of the
    /// show gate, so the TUI only claims a transmit it can render.
    #[cfg(feature = "visual")]
    pub fn visual_frame_png(
        &self,
        id: crate::session::SessionId,
        generation: u64,
        paint: crate::visual::VisualPaint,
    ) -> Option<Vec<u8>> {
        let slot = self.visual_slots.get(&id)?;
        if slot.generation != generation {
            return None;
        }
        if paint.sx == 0 && paint.sy == 0 && paint.sw == slot.width && paint.sh == slot.height {
            return Some(slot.png.clone());
        }
        let crop = crate::visual::ViewCrop {
            sx: paint.sx,
            sy: paint.sy,
            sw: paint.sw,
            sh: paint.sh,
            out_cols: paint.out_cols,
            out_rows: paint.out_rows,
        };
        let cut = crate::visual::crop_rgba(&slot.rgba, slot.width, slot.height, crop);
        crate::visual::encode_png(&cut, paint.sw, paint.sh).ok()
    }

    /// Release the terminal image when the overlay no longer wants it:
    /// tab moved away, newer generation stored, or session gone.
    /// Returns the placement id for the TUI to delete.
    #[cfg(feature = "visual")]
    pub fn visual_take_hide(&mut self) -> Option<u32> {
        let shown = self.visual_shown.as_ref()?;
        if self.visual_overlay_active() == Some((shown.session, shown.generation)) {
            return None;
        }
        let image_id = shown.image_id;
        self.visual_shown = None;
        Some(image_id)
    }

    /// PNG bytes of the image the terminal currently shows, if any.
    #[cfg(feature = "visual")]
    /// Unconditional take of the shown image id, for shutdown cleanup
    /// while the overlay still wants it.
    #[cfg(feature = "visual")]
    pub fn visual_take_shown(&mut self) -> Option<u32> {
        self.visual_shown.take().map(|s| s.image_id)
    }

    /// Visual pane content: zoom strip first, then title and alt
    /// text; half-block art when the terminal cannot take a Kitty
    /// image (the image paints over the image region in Kitty mode,
    /// so no art is emitted there).
    #[cfg(feature = "visual")]
    pub fn visual_view(&self, id: crate::session::SessionId, kitty: bool) -> crate::ui::PaneView {
        use ratatui::style::{Color, Style};
        let rec = self.manager.get(id).expect("ordered session exists");
        let live = rec.state.is_live();
        let title = format!("{} · Visual", rec.name);
        let text = crate::theme::style(crate::theme::Role::Text);
        let muted = crate::theme::style(crate::theme::Role::Muted);
        let line = |content: &str, style: Style| {
            vec![crate::ui::SpanView {
                text: content.to_string(),
                style,
            }]
        };
        let Some(slot) = self.visual_slots.get(&id) else {
            return crate::ui::PaneView {
                title,
                lines: vec![line(
                    "No visualization yet — ask this session to show one",
                    muted,
                )],
                live,
                focused: true,
                cursor: None,
            };
        };
        let mut lines = Vec::new();
        // Row zero is always the zoom strip, in both backends: the
        // Kitty image paints only the region below it, and the mouse
        // hit test assumes these exact columns.
        lines.push(vec![
            crate::ui::SpanView {
                text: crate::ui::VISUAL_ZOOM_IN_LABEL.to_string(),
                style: crate::theme::style(crate::theme::Role::Focus),
            },
            crate::ui::SpanView {
                text: "  ".to_string(),
                style: Style::default(),
            },
            crate::ui::SpanView {
                text: crate::ui::VISUAL_ZOOM_OUT_LABEL.to_string(),
                style: crate::theme::style(crate::theme::Role::Focus),
            },
            crate::ui::SpanView {
                text: "  ←→↑↓ scroll · wheel scrolls".to_string(),
                style: muted,
            },
        ]);
        if !slot.title.is_empty() {
            lines.push(line(&slot.title, text));
        }
        if !kitty {
            // Half-block cells are exactly 1:2 by construction, so the
            // fallback layout uses the fixed 8x16 cell, never the
            // terminal pixel report. Art covers the visible crop only,
            // capped at the tab image region like the Kitty paint.
            let (rows, cols) = self.term_size;
            let areas = crate::ui::chrome_areas(ratatui::layout::Rect::new(0, 0, cols, rows));
            let image = crate::ui::visual_chrome(crate::ui::pane_content_area(&areas)).image;
            if let Some(paint) =
                self.visual_paint(id, image, crate::visual::FALLBACK_CELL_PX.0, crate::visual::FALLBACK_CELL_PX.1)
            {
                let crop = crate::visual::ViewCrop {
                    sx: paint.sx,
                    sy: paint.sy,
                    sw: paint.sw,
                    sh: paint.sh,
                    out_cols: paint.out_cols,
                    out_rows: paint.out_rows,
                };
                let cut = crate::visual::crop_rgba(&slot.rgba, slot.width, slot.height, crop);
                for row in crate::visual::halfblock_rows(
                    &cut,
                    paint.sw,
                    paint.sh,
                    paint.out_cols.max(1) as usize,
                ) {
                    lines.push(
                        row.into_iter()
                            .map(|cell| crate::ui::SpanView {
                                text: cell.ch.to_string(),
                                style: Style::default()
                                    .fg(Color::Rgb(cell.fg.0, cell.fg.1, cell.fg.2))
                                    .bg(Color::Rgb(cell.bg.0, cell.bg.1, cell.bg.2)),
                            })
                            .collect(),
                    );
                }
            }
        }
        if !slot.alt.is_empty() {
            lines.push(line(&slot.alt, muted));
        }
        crate::ui::PaneView {
            title,
            lines,
            live,
            focused: true,
            cursor: None,
        }
    }

    /// Placeholder pane behind the immediate-mode tour render: the TUI
    /// draws the walkthrough over the grid, so this only carries the
    /// title (and a hint when no tour is open yet).
    pub fn walkthrough_view(&self, id: crate::session::SessionId) -> crate::ui::PaneView {
        let rec = self.manager.get(id).expect("ordered session exists");
        let live = rec.state.is_live();
        let hint = if self.walkthroughs.contains_key(&id) {
            "Walkthrough tour"
        } else {
            "No walkthrough started for this session"
        };
        crate::ui::PaneView {
            title: format!("{} · Walkthrough", rec.name),
            lines: vec![vec![crate::ui::SpanView {
                text: hint.to_string(),
                style: crate::theme::style(crate::theme::Role::Muted),
            }]],
            live,
            focused: true,
            cursor: None,
        }
    }

    /// Submit the overlay draft as a question: the markup goes straight
    /// into the agent pane and the Enter stages for a later tick — the
    /// same split write comms injections use. The question is logged
    /// only after the pane write lands, so the overlay never claims an
    /// undelivered ask. No human-input note: that would drop the staged
    /// CR we just armed.
    pub fn submit_walkthrough_question(&mut self, id: crate::session::SessionId) -> bool {
        let Some(wt) = self.walkthroughs.get(&id) else {
            return false;
        };
        let draft = match wt.input.as_ref() {
            Some(buf) if !buf.trim().is_empty() => buf.trim().to_string(),
            _ => return false,
        };
        let markup = crate::walkthrough::Walkthrough::question_markup(
            &wt.title,
            wt.index,
            wt.steps.len(),
            wt.current_step(),
            &draft,
        );
        if self.manager.inject_write(id, markup.as_bytes()).is_err() {
            return false;
        }
        self.pending_enter.insert(id, std::time::Instant::now());
        let Some(wt) = self.walkthroughs.get_mut(&id) else {
            return false;
        };
        wt.take_draft();
        wt.push_question(draft);
        self.dirty = true;
        true
    }

    /// One string tool arg: JSON escapes decoded (agents must escape
    /// newlines, so multi-line steps and Markdown answers arrive
    /// intact), blanked to missing.
    fn tool_arg(args: &str, name: &str) -> Option<String> {
        crate::policy::json_string_field(args.as_bytes(), &[name])
            .map(|s| crate::mcp::decode_json_string(&s))
            .filter(|s| !s.is_empty())
    }

    /// One boolean tool arg from a bare JSON literal.
    fn tool_bool(args: &str, name: &str) -> Option<bool> {
        match crate::mcp::top_raw(args, name)?.trim() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }
    }

    /// One integer tool arg: bare JSON numbers, quoted ones read
    /// liberally too. Fractions, negatives, and overflow never pass.
    fn tool_u32(args: &str, name: &str) -> Option<u32> {
        let raw = crate::mcp::top_raw(args, name)?.trim();
        let bare = raw
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(raw);
        let value = bare.parse::<f64>().ok()?;
        if !value.is_finite() || value < 0.0 || value.fract() != 0.0 || value > u32::MAX as f64 {
            return None;
        }
        Some(value as u32)
    }

    /// Cancel an armed timer from the sidebar button. True when one
    /// was armed; repaint follows only then.
    pub fn cancel_timer(&mut self, timer_id: &str) -> bool {
        if self.broker.cancel_timer(timer_id) {
            self.dirty = true;
            true
        } else {
            false
        }
    }

    /// Resolve a tool caller to its live session, rebound IDs included:
    /// the run must still belong to the record that holds it.
    fn resolve_tool_caller(&self, run_id: &str) -> Result<crate::session::SessionId, String> {
        let id = self
            .manager
            .lookup_run(run_id)
            .ok_or_else(|| "unknown or stale run ID".to_string())?;
        self.manager
            .get(id)
            .filter(|rec| rec.run_id.as_str() == run_id)
            .map(|rec| rec.id)
            .ok_or_else(|| "unknown or stale run ID".to_string())
    }

    /// Execute one walkthrough MCP tool. `None` when the name is not a
    /// walkthrough tool and the broker should answer instead.
    fn walkthrough_tool(
        &mut self,
        run_id: &str,
        tool: &str,
        args: &str,
    ) -> Option<Result<String, String>> {
        match tool {
            "walkthrough_start" | "walkthrough_answer" | "walkthrough_end" | "walkthrough_add_step" | "walkthrough_update" => {}
            _ => return None,
        }
        let id = match self.resolve_tool_caller(run_id) {
            Ok(id) => id,
            Err(e) => return Some(Err(e)),
        };
        match tool {
            "walkthrough_start" => Some(self.walkthrough_start(id, args)),
            "walkthrough_add_step" => Some(self.walkthrough_add_step(id, args)),
            "walkthrough_update" => Some(self.walkthrough_update(id, args)),
            "walkthrough_answer" => {
                let answer = match Self::tool_arg(args, "answer") {
                    Some(answer) => answer,
                    None => return Some(Err("walkthrough_answer needs an answer".to_string())),
                };
                let Some(wt) = self.walkthroughs.get_mut(&id) else {
                    return Some(Err("no walkthrough for this session".to_string()));
                };
                if wt.answer_latest(&answer) {
                    self.dirty = true;
                    Some(Ok(r#"{"answered":true}"#.to_string()))
                } else {
                    Some(Err("no walkthrough question waiting".to_string()))
                }
            }
            _ => {
                let summary = Self::tool_arg(args, "summary");
                let Some(wt) = self.walkthroughs.get_mut(&id) else {
                    return Some(Err("no walkthrough for this session".to_string()));
                };
                wt.end(summary);
                self.dirty = true;
                Some(Ok(r#"{"ended":true}"#.to_string()))
            }
        }
    }

    /// Open a tour over a file: relative paths resolve against the
    /// session cwd, oversize files are refused, and the overlay opens
    /// on the tour at once so the human sees it.
    fn walkthrough_start(&mut self, id: crate::session::SessionId, args: &str) -> Result<String, String> {
        let file = Self::tool_arg(args, "file")
            .ok_or_else(|| "walkthrough_start needs a file".to_string())?;
        let steps_text = Self::tool_arg(args, "steps")
            .ok_or_else(|| "walkthrough_start needs steps".to_string())?;
        let title = Self::tool_arg(args, "title").unwrap_or_else(|| file.clone());
        let cwd = self
            .manager
            .get(id)
            .map(|rec| rec.cwd.clone())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let path = std::path::PathBuf::from(&file);
        let path = if path.is_relative() { cwd.join(path) } else { path };
        let bytes = std::fs::read(&path)
            .map_err(|_| format!("cannot read walkthrough file {file:?}"))?;
        if bytes.len() > crate::walkthrough::MAX_FILE_BYTES {
            return Err(format!(
                "walkthrough file {file:?} exceeds {} bytes",
                crate::walkthrough::MAX_FILE_BYTES
            ));
        }
        let content = String::from_utf8_lossy(&bytes);
        let steps = crate::walkthrough::Walkthrough::parse_steps(&steps_text, content.lines().count())?;
        let tour = crate::walkthrough::Walkthrough::start(title, file, &content, steps)?;
        let count = tour.step_count();
        self.walkthroughs.insert(id, tour);
        if let Some(slot) = self.walkthrough_slot(id) {
            self.overlay_view = Some((id, slot));
        }
        self.dirty = true;
        Ok(format!(r#"{{"started":true,"steps":{count}}}"#))
    }

    /// Insert one step into the open tour. A `file_path` that is not
    /// the tour file fails: tours cover one file. `position` is
    /// 1-based like the displayed counter and appends when absent.
    fn walkthrough_add_step(
        &mut self,
        id: crate::session::SessionId,
        args: &str,
    ) -> Result<String, String> {
        let start = Self::tool_u32(args, "start_line")
            .ok_or_else(|| "walkthrough_add_step needs start_line".to_string())?;
        let end = Self::tool_u32(args, "end_line")
            .ok_or_else(|| "walkthrough_add_step needs end_line".to_string())?;
        let explanation = Self::tool_arg(args, "explanation").unwrap_or_default();
        let position = Self::tool_u32(args, "position")
            .map(|p| (p.max(1) as usize).saturating_sub(1));
        let Some(tour) = self.walkthroughs.get_mut(&id) else {
            return Err("no walkthrough for this session".to_string());
        };
        if let Some(file) = Self::tool_arg(args, "file_path") {
            if file != tour.file_path {
                return Err("walkthrough_add_step targets the open tour file only".to_string());
            }
        }
        tour.add_step(
            crate::walkthrough::Step { start, end, explanation },
            position,
        )?;
        let count = tour.step_count();
        self.dirty = true;
        Ok(format!(r#"{{"added":true,"steps":{count}}}"#))
    }

    /// Update one step of the open tour. `step_index` is 1-based like
    /// the displayed counter; absent fields keep their values.
    fn walkthrough_update(
        &mut self,
        id: crate::session::SessionId,
        args: &str,
    ) -> Result<String, String> {
        let one_based = Self::tool_u32(args, "step_index")
            .ok_or_else(|| "walkthrough_update needs step_index".to_string())?;
        let Some(index) = one_based.checked_sub(1) else {
            return Err("step_index starts at 1".to_string());
        };
        let start = Self::tool_u32(args, "start_line");
        let end = Self::tool_u32(args, "end_line");
        let explanation = Self::tool_arg(args, "explanation");
        let Some(tour) = self.walkthroughs.get_mut(&id) else {
            return Err("no walkthrough for this session".to_string());
        };
        tour.update_step(index as usize, start, end, explanation.as_deref())?;
        self.dirty = true;
        Ok(r#"{"updated":true}"#.to_string())
    }

    /// Execute one session-lifecycle MCP tool. `None` when the name is
    /// not a session tool and the broker should answer instead.
    fn session_tool(
        &mut self,
        run_id: &str,
        tool: &str,
        args: &str,
    ) -> Option<Result<String, String>> {
        match tool {
            "start_session" | "set_session_status" | "clear_session_status" => {}
            #[cfg(feature = "visual")]
            "visual_show" => {}
            _ => return None,
        }
        let id = match self.resolve_tool_caller(run_id) {
            Ok(id) => id,
            Err(e) => return Some(Err(e)),
        };
        match tool {
            "start_session" => Some(self.start_session_tool(id, args)),
            "set_session_status" => Some(self.set_session_status(id, args)),
            #[cfg(feature = "visual")]
            "visual_show" => Some(self.visual_show_tool(id, args)),
            _ => Some(self.clear_session_status(id)),
        }
    }

    /// Accept the caller's diagram for background raster: caps run
    /// before anything spawns, the worker renders off the main loop,
    /// and the verdict carries the generation the completion will
    /// bear. Dimensions arrive with the frame (U4 paints it).
    #[cfg(feature = "visual")]
    fn visual_show_tool(
        &mut self,
        caller: crate::session::SessionId,
        args: &str,
    ) -> Result<String, String> {
        let content = Self::tool_arg(args, "content")
            .ok_or_else(|| "visual_show needs content".to_string())?;
        let format = Self::tool_arg(args, "format").unwrap_or_else(|| "mermaid".to_string());
        crate::visual::check_request(&content, &format)?;
        self.visual_seq += 1;
        let generation = self.visual_seq;
        let title = Self::tool_arg(args, "title").unwrap_or_default();
        let alt = Self::tool_arg(args, "alt").unwrap_or_default();
        let tx = self.visual_tx.clone();
        std::thread::Builder::new()
            .name("visual-raster".to_string())
            .spawn(move || {
                let result = crate::visual::render_frame(&content);
                let _ = tx.send(VisualDone {
                    session: caller,
                    generation,
                    title,
                    alt,
                    result,
                });
            })
            .map_err(|e| format!("cannot spawn raster worker: {e}"))?;
        self.dirty = true;
        Ok(format!(
            r#"{{"accepted":true,"format":"mermaid","generation":{generation}}}"#
        ))
    }

    /// Collect finished background rasters into sticky per-session
    /// slots. Called once per main-loop pass; never blocks.
    #[cfg(feature = "visual")]
    pub fn drain_visual(&mut self) {
        let done: Vec<VisualDone> = self.visual_rx.try_iter().collect();
        for d in done {
            self.visual_complete(d);
        }
    }

    /// No-op drain when the feature is off, so the main loop needs no
    /// feature gate at the call site.
    #[cfg(not(feature = "visual"))]
    pub fn drain_visual(&mut self) {}

    /// Store a finished raster unless it is stale (an older generation
    /// than the slot holds) or its session is gone. Raster failures
    /// drop the frame and keep any previous one.
    #[cfg(feature = "visual")]
    fn visual_complete(&mut self, done: VisualDone) {
        if self.manager.get(done.session).is_none() {
            return;
        }
        if let Some(slot) = self.visual_slots.get(&done.session) {
            if done.generation <= slot.generation {
                return;
            }
        }
        let Ok(frame) = done.result else {
            return;
        };
        self.visual_lru.retain(|id| *id != done.session);
        self.visual_lru.push_back(done.session);
        self.visual_slots.insert(
            done.session,
            VisualSlot {
                generation: done.generation,
                png: frame.png,
                rgba: frame.rgba,
                width: frame.width,
                height: frame.height,
                title: done.title,
                alt: done.alt,
                // A new frame resets the viewport: whole diagram
                // contained in the tab, no scroll.
                zoom: 1.0,
                scroll_x: 0,
                scroll_y: 0,
            },
        );
        self.visual_evict();
        self.dirty = true;
    }

    /// Image bytes held across all slots: transmit PNG plus fallback RGBA.
    #[cfg(feature = "visual")]
    fn visual_bytes(&self) -> usize {
        self.visual_slots
            .values()
            .map(|slot| slot.png.len() + slot.rgba.len())
            .sum()
    }

    /// Enforce the budget: oldest sessions first, but never the
    /// focused one until nothing else can go.
    #[cfg(feature = "visual")]
    fn visual_evict(&mut self) {
        let active = self.manager.active();
        while self.visual_slots.len() > self.visual_budget_count
            || self.visual_bytes() > self.visual_budget_bytes
        {
            let victim = self
                .visual_lru
                .iter()
                .position(|id| Some(*id) != active)
                .or_else(|| (!self.visual_lru.is_empty()).then_some(0));
            let Some(index) = victim else {
                break;
            };
            let id = self.visual_lru.remove(index).expect("eviction index live");
            self.visual_slots.remove(&id);
        }
    }

    /// Create a local agent session: validated name/cwd/harness, an
    /// optional comm-group join (else the caller's primary group), and
    /// an optional opening prompt queued as the first idle injection.
    /// Remote flavors, internet sessions, and focus theft are refused:
    /// tool births never steal the human view.
    fn start_session_tool(
        &mut self,
        caller: crate::session::SessionId,
        args: &str,
    ) -> Result<String, String> {
        for (param, what) in [
            ("connection", "remote connections"),
            ("host", "remote hosts"),
            ("od_preset", "OD presets"),
            ("od_type", "OD session types"),
        ] {
            if Self::tool_arg(args, param).is_some() {
                return Err(format!("{what} are unsupported (local sessions only)"));
            }
        }
        if Self::tool_bool(args, "internet").unwrap_or(false) {
            return Err("internet sessions are unsupported (local sessions only)".to_string());
        }
        let harness_name = Self::tool_arg(args, "harness").unwrap_or_else(|| "claude".to_string());
        let harness = crate::harness::Harness::from_name(&harness_name)
            .ok_or_else(|| format!("unknown harness {harness_name:?} (claude/codex/muse)"))?;
        let cwd = match Self::tool_arg(args, "path") {
            Some(path) => {
                let cwd = std::path::PathBuf::from(&path);
                if !cwd.is_dir() {
                    return Err(format!("session path {path:?} is not a directory"));
                }
                cwd
            }
            None => self
                .manager
                .get(caller)
                .map(|rec| rec.cwd.clone())
                .unwrap_or_else(|| std::path::PathBuf::from(".")),
        };
        let name = match Self::tool_arg(args, "name") {
            Some(name) => {
                if self.live_names().iter().any(|taken| taken == &name) {
                    return Err(format!("session name {name:?} is taken"));
                }
                name
            }
            None => {
                let stem = harness.as_str();
                let mut n = self.manager.len() + 1;
                loop {
                    let candidate = format!("{stem}-{n}");
                    if !self.live_names().iter().any(|taken| taken == &candidate) {
                        break candidate;
                    }
                    n += 1;
                }
            }
        };
        let group = Self::tool_arg(args, "communication_group")
            .or_else(|| self.broker.primary_group(caller).map(str::to_string));
        let previous = self.manager.active();
        let id = self
            .create_session(&crate::create::SessionSpec {
                kind: crate::create::SessionKind::Agent(harness),
                name: name.clone(),
                cwd,
                model: String::new(),
                group,
            })
            .map_err(|e| format!("cannot start session: {e}"))?;
        if let Some(prompt) = Self::tool_arg(args, "prompt") {
            let from = self
                .manager
                .get(caller)
                .map(|rec| rec.name.clone())
                .unwrap_or_default();
            self.broker.push(
                id,
                crate::comms::Injection {
                    conv: crate::ids::ConversationId::generate().to_string(),
                    kind: crate::comms::InjectKind::Tell,
                    from,
                    text: prompt,
                },
            );
        }
        if let Some(active) = previous {
            if active != id {
                self.manager.switch(active);
            }
        }
        self.dirty = true;
        Ok(format!(
            r#"{{"session_id":"{id}","name":{}}}"#,
            crate::mcp::escape_json(&name),
        ))
    }

    /// Replace the caller's sticky status: closed kind set, message
    /// bounded with no controls or newlines.
    fn set_session_status(
        &mut self,
        caller: crate::session::SessionId,
        args: &str,
    ) -> Result<String, String> {
        let kind = Self::tool_arg(args, "kind")
            .ok_or_else(|| "set_session_status needs a kind".to_string())?;
        let kind = crate::session_status::StatusKind::from_name(&kind).ok_or_else(|| {
            "unknown status kind (info/progress/success/warning/blocked/question)".to_string()
        })?;
        let message = Self::tool_arg(args, "message")
            .ok_or_else(|| "set_session_status needs a message".to_string())?;
        let message = crate::session_status::validate(&message)?;
        let Some(rec) = self.manager.get_mut(caller) else {
            return Err("unknown or stale run ID".to_string());
        };
        rec.status = Some(crate::session_status::SessionStatus { kind, message: message.clone() });
        self.dirty = true;
        Ok(format!(
            r#"{{"kind":{},"message":{}}}"#,
            crate::mcp::escape_json(kind.as_str()),
            crate::mcp::escape_json(&message),
        ))
    }

    /// Clear the caller's sticky status; already-clear stays success.
    fn clear_session_status(&mut self, caller: crate::session::SessionId) -> Result<String, String> {
        let Some(rec) = self.manager.get_mut(caller) else {
            return Err("unknown or stale run ID".to_string());
        };
        rec.status = None;
        self.dirty = true;
        Ok(r#"{"status_cleared":true}"#.to_string())
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
                if let Some((view_id, index)) = self.overlay_view {
                    if Some(id) == active && view_id == id {
                        let label = ["", "", "", "Events", "Tasks", "Visual", "Walkthrough"]
                            .get(index).copied().unwrap_or("View");
                        if label == "Walkthrough" {
                            return self.walkthrough_view(id);
                        }
                        #[cfg(feature = "visual")]
                        if label == "Visual" {
                            return self.visual_view(id, crate::visual::kitty_supported_env());
                        }
                        return crate::ui::PaneView {
                            title: format!("{} · {label}", rec.name),
                            lines: vec![vec![crate::ui::SpanView {
                                text: format!("{label} view unavailable in this build"),
                                style: crate::theme::style(crate::theme::Role::Muted),
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
                    focused: Some(id) == active,
                    cursor: self.manager.cursor(id),
                }
            })
            .collect()
    }

    /// Record human typing into one session: injections debounce until it
    /// settles, and any staged Enter for that session is dropped — the
    /// human owns the prompt now, and our CR must never submit their
    /// half-typed draft.
    pub fn note_human_input(&mut self, id: crate::session::SessionId) {
        self.last_human_input = Some(std::time::Instant::now());
        self.pending_enter.remove(&id);
    }

    /// Live session names for dialog validation.
    pub fn live_names(&self) -> Vec<String> {
        self.manager
            .order()
            .iter()
            .filter_map(|&id| self.manager.get(id))
            .filter(|rec| rec.state.is_live())
            .map(|rec| rec.name.clone())
            .collect()
    }

    /// First free `shell-N` name for the dialog prefill.
    /// Prefill name for the create dialog. The dialog defaults to the
    /// claude tool, so the prefix matches; the user can rename freely.
    pub fn suggested_session_name(&self) -> String {
        let taken = self.live_names();
        let mut n = self.manager.len() + 1;
        loop {
            let candidate = format!("claude-{n}");
            if !taken.iter().any(|t| t == &candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// Open the create-session dialog, prefilled from current state.
    pub fn open_create_dialog(&mut self) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let name = self.suggested_session_name();
        let groups = self.broker.group_names();
        self.create_dialog = Some(crate::create::CreateDialog::new(&name, &cwd, &groups, self.pill_tabs));
        self.dirty = true;
    }

    /// Open the group management dialog.
    pub fn open_group_dialog(&mut self) {
        self.group_dialog = Some(crate::groups::GroupDialog::new());
        self.dirty = true;
    }

    pub fn open_quit_confirm(&mut self) {
        self.quit_confirm = Some(crate::quit::QuitConfirm::new(self.pill_tabs));
        self.dirty = true;
    }

    /// Fresh snapshot for the group dialog: groups with live member
    /// counts, sessions in bar order, and the selected group's current
    /// members for the checkbox pre-check.
    pub fn group_ctx(&self) -> crate::groups::GroupCtx {
        let groups = self
            .broker
            .group_names()
            .into_iter()
            .map(|name| crate::groups::GroupRow {
                members: self.broker.group_members(&name).len(),
                member_names: self.broker.group_members(&name).into_iter()
                    .filter_map(|id| self.manager.get(id).map(|rec| rec.name.clone()))
                    .collect(),
                color: self.broker.group_color(&name).unwrap_or(0),
                name,
            })
            .collect();
        let sessions = self
            .manager
            .order()
            .to_vec()
            .into_iter()
            .filter_map(|id| {
                self.manager.get(id).map(|rec| crate::groups::SessionRow {
                    id,
                    name: rec.name.clone(),
                })
            })
            .collect();
        let selected = self
            .group_dialog
            .as_ref()
            .map(|d| d.selected_name())
            .unwrap_or_default();
        let members = self.broker.group_members(selected);
        crate::groups::GroupCtx {
            groups,
            sessions,
            members,
        }
    }

    /// Apply one group-dialog mutation to the broker. The dialog validates
    /// names against a fresh snapshot, so these are infallible in practice;
    /// failures (exit races) stay silent and keep the dialog open.
    /// Unknown session IDs (exited mid-dialog) are skipped.
    pub fn apply_group(&mut self, outcome: crate::groups::GroupOutcome) {
        use crate::groups::GroupOutcome as Out;
        match outcome {
            Out::Create(name) => {
                let _ = self.broker.create_group(&name);
            }
            Out::Rename { from, to } => {
                let _ = self.broker.rename_group(&from, &to);
            }
            Out::Delete(name) => {
                self.broker.remove_group(&name);
            }
            Out::SetMembers { group, members } => {
                for id in self.manager.order().to_vec() {
                    if members.contains(&id) {
                        let _ = self.broker.join(&self.manager, id, &group);
                    } else {
                        self.broker.leave(id, &group);
                    }
                }
            }
            Out::Pending | Out::Closed => {}
        }
        self.dirty = true;
    }

    /// Live agent sessions as restorable records, in bar order. Shells
    /// have no resume form and exited sessions are gone, so both are
    /// left out; groups ride along for exact rejoins.
    pub fn snapshot_sessions(&self) -> Vec<crate::checkpoint::SavedSession> {
        self.manager
            .order()
            .to_vec()
            .into_iter()
            .filter_map(|id| {
                let rec = self.manager.get(id)?;
                if !rec.state.is_live() {
                    return None;
                }
                if crate::harness::Harness::from_name(&rec.cli_tool).is_none() {
                    return None;
                }
                Some(crate::checkpoint::SavedSession {
                    name: rec.name.clone(),
                    cli_tool: rec.cli_tool.clone(),
                    cwd: rec.cwd.to_string_lossy().into_owned(),
                    groups: self.broker.groups_of(id),
                    harness_session_id: rec.harness_session_id.clone(),
                })
            })
            .collect()
    }

    /// Recreate every restorable session in an entry: agents relaunch
    /// with resume argv (or the cwd-scoped fallback), rejoin their
    /// groups, and fit the main pane. Unknown tools and vanished working
    /// directories skip with a reason instead of failing the batch.
    pub fn restore_entry(&mut self, entry: &crate::checkpoint::SavedEntry) -> RestoreReport {
        let mut report = RestoreReport {
            spawned: 0,
            skipped: Vec::new(),
        };
        for saved in &entry.sessions {
            let Some(harness) = crate::harness::Harness::from_name(&saved.cli_tool) else {
                report.skipped.push(format!("{}: unknown tool {}", saved.name, saved.cli_tool));
                continue;
            };
            let cwd = std::path::PathBuf::from(&saved.cwd);
            if !cwd.is_dir() {
                report.skipped.push(format!("{}: missing directory {}", saved.name, saved.cwd));
                continue;
            }
            let spec = harness.spec();
            let argv = harness.resume_argv(&spec.resolve_binary(), saved.harness_session_id.as_deref());
            let mut cmd = String::from("exec ");
            cmd.push_str(&crate::create::shell_join(&argv));
            let run = crate::ids::RunId::generate();
            match self.manager.spawn_agent(&saved.name, &cwd, &cmd, run, &saved.cli_tool) {
                Ok(id) => {
                    for group in &saved.groups {
                        let _ = self.broker.join(&self.manager, id, group);
                    }
                    report.spawned += 1;
                }
                Err(e) => report.skipped.push(format!("{}: spawn failed ({e})", saved.name)),
            }
        }
        self.dirty = true;
        report
    }

    /// Spawn exactly what a submitted dialog describes: shells run the
    /// login shell, agents run their registry argv. Returns the new id.
    pub fn create_session(
        &mut self,
        spec: &crate::create::SessionSpec,
    ) -> std::io::Result<crate::session::SessionId> {
        use crate::create::SessionKind;
        let (cmd, cli_tool) = match spec.kind {
            SessionKind::Shell => {
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
                (format!("exec {shell} -i"), "shell".to_string())
            }
            SessionKind::Agent(h) => {
                let hs = h.spec();
                let model = if spec.model.is_empty() {
                    None
                } else {
                    Some(spec.model.as_str())
                };
                let argv = hs.launch_argv(&hs.resolve_binary(), model);
                let mut cmd = String::from("exec ");
                cmd.push_str(&crate::create::shell_join(&argv));
                (cmd, h.as_str().to_string())
            }
        };
        // Agent CLIs get the dual-tab layout (agent on tab 0, a lazy
        // human terminal on tab 1); plain shells stay single-tab.
        let run = crate::ids::RunId::generate();
        let id = match spec.kind {
            SessionKind::Agent(_) => {
                self.manager
                    .spawn_agent(&spec.name, &spec.cwd, &cmd, run, &cli_tool)
            }
            SessionKind::Shell => {
                self.manager
                    .spawn(&spec.name, &spec.cwd, &cmd, run, &cli_tool)
            }
        }?;
        // The dialog's comm-group choice joins at birth (a group deleted
        // mid-dialog is recreated by the join — the user explicitly picked
        // it a moment ago).
        if let Some(group) = spec.group.as_deref() {
            let _ = self.broker.join(&self.manager, id, group);
        }
        // Creating focuses the new session: the user just asked for it,
        // and the caller fits the active pane to the dialog's area.
        self.manager.switch(id);
        Ok(id)
    }

    /// Deliver due injections into idle, non-recently-typed panes. Targets
    /// whose hook activity is Thinking/ToolUse/Waiting keep waiting, as do
    /// panes the human just typed into.
    /// Delivery gate for bodies and staged Enters alike: quiet human
    /// hands plus quiet hooks. Either recent activity holds everything.
    fn injection_settled(&self, now: std::time::Instant) -> bool {
        let hands_off = self.last_human_input.is_none_or(|t| {
            now.duration_since(t) >= crate::comms::INJECT_DEBOUNCE
        });
        let hooks_quiet = self.last_hook_activity.is_none_or(|t| {
            now.duration_since(t) >= crate::comms::INJECT_HOOK_DEBOUNCE
        });
        hands_off && hooks_quiet
    }

    pub fn settle_comms(&mut self) {
        use crate::session::Activity;
        let now = std::time::Instant::now();
        self.broker.tick(now);
        if !self.injection_settled(now) {
            return;
        }
        let order = self.manager.order().to_vec();
        for id in order {
            if self.broker.queued(id) == 0 {
                continue;
            }
            let idle = self.manager.get(id).is_some_and(|rec| {
                matches!(rec.activity, Activity::Idle | Activity::Stopped)
            });
            if !idle {
                continue;
            }
            let due = self.broker.take_due(id, usize::MAX);
            let mut wrote = false;
            // Panes that opted into paste mode (DECSET 2004) take the
            // whole payload as one bracketed-paste transaction; the rest
            // take it raw, exactly as before.
            let bracketed = self.manager.bracketed_paste(id);
            for inj in due {
                if self.manager.inject_write(id, &inj.render_framed(bracketed)).is_ok() {
                    wrote = true;
                    self.dirty = true;
                }
            }
            if wrote {
                // Arm (or re-arm) the staged Enter: the CR goes out on a
                // later tick, never in the same burst as the text.
                self.pending_enter.insert(id, now);
            }
        }
        self.settle_enters(now);
    }

    /// Send staged Enters whose beat has elapsed. Same gates as bodies —
    /// settled debounce plus an idle target — so a CR never lands mid-turn
    /// or into a human draft (human input clears the entry outright).
    /// Sessions that exited meanwhile are pruned.
    fn settle_enters(&mut self, now: std::time::Instant) {
        use crate::session::Activity;
        let settled = self.injection_settled(now);
        let due: Vec<crate::session::SessionId> = self
            .pending_enter
            .iter()
            .filter(|(_, &at)| now.duration_since(at) >= crate::comms::INJECT_ENTER_DELAY)
            .map(|(&id, _)| id)
            .collect();
        for id in due {
            let idle = self.manager.get(id).is_some_and(|rec| {
                matches!(rec.activity, Activity::Idle | Activity::Stopped)
            });
            if !settled || !idle {
                continue;
            }
            // A dead pane drops the CR: the body stays visible as a draft
            // for the human rather than vanishing silently.
            let _ = self.manager.inject_write(id, &[crate::comms::INJECT_ENTER_CR]);
            self.pending_enter.remove(&id);
            self.dirty = true;
        }
        self.pending_enter
            .retain(|id, _| self.manager.get(*id).is_some());
    }

    /// Cycle the active session; wraps around. No-op when empty.
    pub fn step_session(&mut self, dir: i32) {
        let order = self.manager.order().to_vec();
        if order.is_empty() {
            return;
        }
        let cur = self
            .manager
            .active()
            .and_then(|a| order.iter().position(|&id| id == a))
            .unwrap_or(0) as i32;
        let next = (cur + dir).rem_euclid(order.len() as i32) as usize;
        self.manager.switch(order[next]);
        self.overlay_view = None;
    }

    /// Focus session by order index (`Ctrl-b 1` is index 0). Returns false
    /// when out of range, leaving focus untouched.
    pub fn select_session(&mut self, index: usize) -> bool {
        match self.manager.order().to_vec().get(index) {
            Some(&id) => {
                self.manager.switch(id);
                self.overlay_view = None;
                // Picking a number always returns to the focused view.
                self.grid_mode = false;
                self.dirty = true;
                true
            }
            None => false,
        }
    }

    /// Terminate a session (`Ctrl-b x`): leave every comm group, fail its
    /// open conversations and timers, then drop the record so it leaves
    /// the UI. Focus falls to the first remaining session. False when the
    /// id is unknown.
    pub fn terminate_session(&mut self, id: crate::session::SessionId) -> bool {
        self.broker.leave_all(id);
        self.broker.target_exited(&self.manager, id);
        if !self.manager.remove(id) {
            return false;
        }
        self.overlay_view = None;
        self.dirty = true;
        true
    }

    /// Session-bar tabs in order with live/focus flags.
    pub fn tabs(&self) -> Vec<crate::ui::SessionTab> {
        let active = self.manager.active();
        self.manager
            .order()
            .to_vec()
            .into_iter()
            .filter_map(|id| {
                self.manager.get(id).map(|rec| {
                    let group = self.broker.primary_group(id).map(str::to_string);
                    let group_color = group
                        .as_deref()
                        .and_then(|g| self.broker.group_color(g));
                    crate::ui::SessionTab {
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
    pub fn sidebar_info(&self) -> crate::ui::SidebarInfo {
        let now = std::time::Instant::now();
        let session = self.manager.active().and_then(|id| {
            self.manager.get(id).map(|rec| {
                let state = if rec.state.is_live() {
                    match rec.activity {
                        crate::session::Activity::Idle => "running".to_string(),
                        activity => format!("running · {activity:?}"),
                    }
                } else {
                    match rec.exit_code {
                        Some(code) => format!("exited({code})"),
                        None => "exited".to_string(),
                    }
                };
                crate::ui::SessionDetail {
                    name: rec.name.clone(),
                    cli_tool: rec.cli_tool.clone(),
                    cwd: rec.cwd.to_string_lossy().into_owned(),
                    state,
                    status: rec.status.as_ref().map(|s| s.display()),
                    // Armed timers show for the focused session only;
                    // other sessions keep their own countdowns hidden.
                    timers: self
                        .broker
                        .timers_for(id)
                        .into_iter()
                        .map(|(timer_id, due)| crate::ui::TimerView {
                            id: timer_id,
                            remaining: crate::ui::format_countdown(
                                due.saturating_duration_since(now),
                            ),
                        })
                        .collect(),
                    uptime_secs: rec.spawned_at.elapsed().as_secs(),
                    tool_calls: rec.tool_calls,
                    approvals: rec.approvals,
                    denials: rec.denials,
                }
            })
        });
        crate::ui::SidebarInfo {
            session,
            pending: self.pending_hooks.len(),
            mode: self.permission_mode.as_str(),
        }
    }

    /// Run deterministic policy over queued hook requests. Every verdict —
    /// allow, deny, or ask-for-the-harness — replies immediately and is
    /// audited; nothing waits on a human. Yolo auto-approves, Safe-Only
    /// blocks still deny, and every other Ask goes back to the harness so
    /// its native permission flow takes over. Audit failures never block.
    pub fn settle_hooks(
        &mut self,
        policy: &mut crate::policy::Policy,
        audit_path: &std::path::Path,
    ) {
        while let Some(req) = self.pending_hooks.pop_front() {
            let (decision, reason) = policy.decide(&req.hook, &req.body);
            let line = crate::policy::decision_line(&req.hook, decision, reason);
            let _ = req.reply.send(line);
            self.last_hook_activity = Some(std::time::Instant::now());
            // Attribute the verdict to the sender's sidebar counters.
            if let Some(id) = self.manager.lookup_run(&req.run_id) {
                self.manager.note_verdict(id, decision);
            }
            self.audit_hook(audit_path, &req.hook, &req.body, decision, reason);
        }
        self.dirty = true;
    }

    fn audit_hook(
        &self,
        audit_path: &std::path::Path,
        hook: &str,
        body: &str,
        decision: crate::policy::Decision,
        reason: &str,
    ) {
        let tool = crate::policy::tool_name(body);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let name = format!("{decision:?}").to_lowercase();
        let _ = crate::audit::append(audit_path, hook, &tool, &name, reason, now);
    }

    /// Reduce one event. Input routing arrives with the input slice; until
    /// then input events are acknowledged but change nothing.
    pub fn apply(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Tick => {}
            AppEvent::Shutdown => self.should_quit = true,
            AppEvent::SessionOutput { .. } => {
                self.dirty = true;
            }
            AppEvent::SessionExited { id, .. } => {
                // An exit fails every conversation touching it; sources of
                // open asks/tells are notified through the broker queue.
                self.broker.target_exited(&self.manager, id);
                self.dirty = true;
            }
            AppEvent::CommsRequest(req) => {
                // Walkthrough and session tools answer here (they own
                // overlay and manager state the broker cannot see);
                // everything else goes to the broker at once. The
                // verdict goes straight back to `mcp-serve`. Failures
                // stay single-line JSON, escaped.
                let now = std::time::Instant::now();
                let verdict = match self.walkthrough_tool(&req.run_id, &req.tool, &req.args) {
                    Some(verdict) => verdict,
                    None => match self.session_tool(&req.run_id, &req.tool, &req.args) {
                        Some(verdict) => verdict,
                        None => self.broker.call(&self.manager, &req.run_id, &req.tool, &req.args, now),
                    },
                };
                let line = match verdict {
                    Ok(result) => format!("{{\"ok\":true,\"result\":{result}}}\n"),
                    Err(e) => format!(
                        "{{\"ok\":false,\"error\":{}}}\n",
                        crate::mcp::escape_json(&e)
                    ),
                };
                let _ = req.reply.send(line);
                self.dirty = true;
            }
            AppEvent::BotRequest(req) => {
                let now = std::time::Instant::now();
                let line = match self.broker.bot_call(
                    &self.manager,
                    &req.name,
                    &req.token,
                    &req.tool,
                    &req.args,
                    now,
                ) {
                    Ok(result) => format!("{{\"ok\":true,\"result\":{result}}}\n"),
                    Err(e) => {
                        let mut line = String::from("{\"ok\":false,\"error\":");
                        line.push_str(&e.to_json());
                        line.push_str("}\n");
                        line
                    }
                };
                let _ = req.reply.send(line);
                self.dirty = true;
            }
            AppEvent::Resize(rows, cols) => {
                self.term_size = (rows, cols);
                if cols < 100 { self.overlay_view = None; }
                self.dirty = true;
            }
            AppEvent::HookRequest(req) => {
                // Unattributed SessionStarts bind by working directory when
                // unambiguous (muse scrubs hook-child environments, so its
                // relays send no run ID). The bind runs first so the record
                // below attributes like any other SessionStart.
                if req.hook == "SessionStart" && self.manager.lookup_run(&req.run_id).is_none()
                {
                    if let Some(harness) = crate::session::session_id_from_hook_body(&req.body) {
                        if let Some(cwd) = crate::session::cwd_from_hook_body(&req.body) {
                            self.manager.bind_harness_session(&harness, &cwd);
                        }
                    }
                }
                // Attribute hook activity before queuing: the sender's run
                // ID resolves to its session; records without one resolve
                // by the harness session ID instead. Unknown runs stay
                // untouched. Tool-gated hooks also count one sidebar call.
                let fallback_id = if self.manager.lookup_run(&req.run_id).is_none() {
                    crate::session::session_id_from_hook_body(&req.body)
                        .and_then(|h| self.manager.lookup_harness_session(&h))
                } else {
                    None
                };
                let attributed = self.manager.lookup_run(&req.run_id).or(fallback_id);
                if let Some(activity) = crate::session::activity_for_hook(&req.hook) {
                    if let Some(id) = attributed {
                        self.manager.set_activity(id, activity);
                        if activity == crate::session::Activity::ToolUse {
                            self.manager.note_tool_call(id);
                        }
                    }
                }
                // SessionStart carries the harness-side conversation ID the
                // restore path resumes with. Bodies without one (or from
                // unknown runs) leave any earlier value alone.
                if req.hook == "SessionStart" {
                    if let Some(harness) = crate::session::session_id_from_hook_body(&req.body) {
                        if let Some(id) = attributed {
                            self.manager.set_harness_session(id, harness);
                        }
                    }
                }
                if self.pending_hooks.len() < crate::listener::MAX_PENDING_HOOKS {
                    self.pending_hooks.push_back(req);
                    self.last_hook_activity = Some(std::time::Instant::now());
                }
                self.dirty = true;
            }
            AppEvent::Input(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::AppEvent;
    use crate::ids::RunId;
    use crate::session::SessionId;

    #[test]
    fn fresh_state_needs_paint_and_runs() {
        let s = AppState::new();
        assert!(s.dirty);
        assert!(!s.should_quit);
        assert!(s.manager.is_empty());
    }

    #[test]
    fn shutdown_quits() {
        let mut s = AppState::new();
        s.dirty = false;
        s.apply(AppEvent::Shutdown);
        assert!(s.should_quit);
    }

    fn hook_request(hook: &str, run_id: &str, body: &str) -> AppEvent {
        let (reply_tx, _) = std::sync::mpsc::channel();
        AppEvent::HookRequest(crate::listener::HookRequest {
            hook: hook.to_string(),
            body: body.to_string(),
            run_id: run_id.to_string(),
            sync: false,
            reply: reply_tx,
            timed_out: Default::default(),
        })
    }

    #[test]
    fn session_start_captures_harness_id() {
        let mut s = AppState::new();
        let run = RunId::generate();
        let id = s
            .manager
            .spawn_agent("a", &std::env::temp_dir(), "exec sleep 30", run.clone(), "claude")
            .unwrap();
        assert_eq!(s.manager.get(id).unwrap().harness_session_id, None);
        // Claude envelope nests the harness JSON under body.
        s.apply(hook_request(
            "SessionStart",
            run.as_str(),
            r#"{"v":1,"hook":"SessionStart","run_id":"r","body":{"session_id":"harness-9","cwd":"/tmp"}}"#,
        ));
        assert_eq!(
            s.manager.get(id).unwrap().harness_session_id.as_deref(),
            Some("harness-9")
        );
        // Bodies without an ID (or other hooks) leave the value alone.
        s.apply(hook_request("SessionStart", run.as_str(), "{}"));
        s.apply(hook_request("PreToolUse", run.as_str(), "{}"));
        assert_eq!(
            s.manager.get(id).unwrap().harness_session_id.as_deref(),
            Some("harness-9")
        );
        // Unknown runs touch nothing.
        s.apply(hook_request(
            "SessionStart",
            "nope",
            r#"{"v":1,"body":{"session_id":"other"}}"#,
        ));
        assert!(s.manager.remove(id));
    }

    #[test]
    fn unattributed_muse_hooks_bootstrap_and_attribute() {
        use crate::session::Activity;
        let mut s = AppState::new();
        let cwd = std::env::temp_dir();
        let cwd_json = crate::mcp::escape_json(&cwd.to_string_lossy());
        let run = RunId::generate();
        let id = s
            .manager
            .spawn_agent("m", &cwd, "exec sleep 30", run.clone(), "muse")
            .unwrap();
        // Scrubbed relay: empty run_id, muse SessionStart body shape.
        let start = format!(
            "{{\"v\":1,\"hook\":\"SessionStart\",\"run_id\":\"\",\"forge_pid\":0,\"body\":{{\"session_id\":\"muse-9\",\"cwd\":{cwd_json}}}}}"
        );
        s.apply(hook_request("SessionStart", "", &start));
        assert_eq!(
            s.manager.get(id).unwrap().harness_session_id.as_deref(),
            Some("muse-9"),
            "bootstrap captured the resume ID"
        );
        assert_eq!(s.manager.get(id).unwrap().activity, Activity::Thinking);
        // Later edges carry no run either; the bound ID attributes them.
        let stop = format!(
            "{{\"v\":1,\"hook\":\"Stop\",\"run_id\":\"\",\"forge_pid\":0,\"body\":{{\"session_id\":\"muse-9\",\"cwd\":{cwd_json}}}}}"
        );
        s.apply(hook_request("Stop", "", &stop));
        assert_eq!(s.manager.get(id).unwrap().activity, Activity::Stopped);
        // An unknown harness ID still touches nothing.
        let strange = format!(
            "{{\"v\":1,\"hook\":\"Stop\",\"run_id\":\"\",\"forge_pid\":0,\"body\":{{\"session_id\":\"ghost\",\"cwd\":{cwd_json}}}}}"
        );
        s.apply(hook_request("Stop", "", &strange));
        assert_eq!(s.manager.get(id).unwrap().activity, Activity::Stopped);
        assert!(s.manager.remove(id));
    }

    #[test]
    fn snapshot_keeps_live_agents_with_groups() {
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn_agent("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "claude")
            .unwrap();
        s.manager.set_harness_session(a, "h-1".to_string());
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, a, "team").unwrap();
        // Shells and exited sessions never snapshot.
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("sh", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        let snap = s.snapshot_sessions();
        assert_eq!(snap.len(), 1, "agent only: {snap:?}");
        assert_eq!(snap[0].name, "a");
        assert_eq!(snap[0].cli_tool, "claude");
        assert_eq!(snap[0].groups, vec!["peers".to_string(), "team".to_string()]);
        assert_eq!(snap[0].harness_session_id.as_deref(), Some("h-1"));
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn restore_respawns_with_resume_and_rejoins() {
        // Hermetic stand-in binary; the record outlives its instant exit.
        let saved = std::env::var("CODEX_BIN").ok();
        std::env::set_var("CODEX_BIN", "/bin/true");
        let mut s = AppState::new();
        let entry = crate::checkpoint::SavedEntry {
            label: "a, gone, weird".to_string(),
            saved_at_unix: 1_700_000_000,
            sessions: vec![
                crate::checkpoint::SavedSession {
                    name: "a".to_string(),
                    cli_tool: "codex".to_string(),
                    cwd: std::env::temp_dir().to_string_lossy().into_owned(),
                    groups: vec!["peers".to_string()],
                    harness_session_id: Some("uuid-a".to_string()),
                },
                crate::checkpoint::SavedSession {
                    name: "gone".to_string(),
                    cli_tool: "codex".to_string(),
                    cwd: "/no/such/dir-anywhere".to_string(),
                    groups: vec![],
                    harness_session_id: None,
                },
                crate::checkpoint::SavedSession {
                    name: "weird".to_string(),
                    cli_tool: "shell".to_string(),
                    cwd: std::env::temp_dir().to_string_lossy().into_owned(),
                    groups: vec![],
                    harness_session_id: None,
                },
            ],
        };
        let report = s.restore_entry(&entry);
        assert_eq!(report.spawned, 1, "report: {:?}", report.skipped);
        assert_eq!(report.skipped.len(), 2, "missing dir + shell: {:?}", report.skipped);
        let id = s.manager.order().to_vec().pop().unwrap();
        assert_eq!(s.manager.get(id).unwrap().name, "a");
        assert!(s.broker.is_member(id, "peers"), "rejoined");
        match saved {
            Some(v) => std::env::set_var("CODEX_BIN", v),
            None => std::env::remove_var("CODEX_BIN"),
        }
        assert!(s.manager.remove(id));
    }

    #[test]
    fn hook_requests_queue_bounded_and_dirty() {
        let mut s = AppState::new();
        s.dirty = false;
        let (reply_tx, _reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
            hook: "PreToolUse".to_string(),
            body: "{}".to_string(),
            run_id: String::new(),
            sync: true,
            reply: reply_tx,
            timed_out: Default::default(),
        }));
        assert!(s.dirty);
        assert_eq!(s.pending_hooks.len(), 1);
        for _ in 0..crate::listener::MAX_PENDING_HOOKS + 10 {
            let (tx, _rx) = std::sync::mpsc::channel();
            s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
                hook: "Stop".to_string(),
                body: "{}".to_string(),
                run_id: String::new(),
                sync: false,
                reply: tx,
                timed_out: Default::default(),
            }));
        }
        assert_eq!(s.pending_hooks.len(), crate::listener::MAX_PENDING_HOOKS);
    }

    #[test]
    fn pre_tool_use_replies_use_hook_specific_output() {
        // Newer Claude (and muse) reject the legacy top-level `decision`
        // field on PreToolUse replies: "unsupported legacy PreToolUse
        // output; use hookSpecificOutput.permissionDecision".
        use crate::config::PermissionMode;
        let audit = std::env::temp_dir().join(format!(
            "forge-hookshape-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&audit);
        let mut yolo = crate::policy::Policy::new(PermissionMode::Yolo, &[], &[]).unwrap();
        let mut s = AppState::new();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
            hook: "PreToolUse".to_string(),
            body: r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#.to_string(),
            run_id: String::new(),
            sync: true,
            reply: reply_tx,
            timed_out: Default::default(),
        }));
        s.settle_hooks(&mut yolo, &audit);
        let line = reply_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(line.trim()).expect("reply is JSON");
        assert_eq!(
            v["hookSpecificOutput"]["hookEventName"],
            serde_json::Value::String("PreToolUse".to_string()),
            "line: {line:?}"
        );
        assert_eq!(
            v["hookSpecificOutput"]["permissionDecision"],
            serde_json::Value::String("allow".to_string()),
            "line: {line:?}"
        );
        assert!(
            v["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .is_some_and(|r| !r.is_empty()),
            "deny requires a non-empty reason; allow carries one too: {line:?}"
        );
        assert!(v.get("decision").is_none(), "no legacy field: {line:?}");
        let _ = std::fs::remove_file(&audit);
    }

    #[test]
    fn settle_hooks_replies_every_verdict_immediately() {
        use crate::config::PermissionMode;
        let audit = std::env::temp_dir().join(format!(
            "forge-settle-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&audit);
        let mut yolo = crate::policy::Policy::new(
            PermissionMode::Yolo,
            &[],
            &[],
        )
        .unwrap();
        let mut s = AppState::new();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
            hook: "PreToolUse".to_string(),
            body: r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#.to_string(),
            run_id: String::new(),
            sync: true,
            reply: reply_tx,
            timed_out: Default::default(),
        }));
        s.settle_hooks(&mut yolo, &audit);
        assert!(s.pending_hooks.is_empty());
        let line = reply_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(
            line.contains(r#""permissionDecision":"allow""#),
            "line: {line:?}"
        );
        let logged = std::fs::read_to_string(&audit).unwrap();
        assert_eq!(logged.lines().count(), 1);
        assert!(logged.contains(r#""decision":"allow""#), "audit: {logged:?}");

        let mut off = crate::policy::Policy::new(
            PermissionMode::Off,
            &[],
            &[],
        )
        .unwrap();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
            hook: "PreToolUse".to_string(),
            body: "{}".to_string(),
            run_id: String::new(),
            sync: true,
            reply: reply_tx,
            timed_out: Default::default(),
        }));
        s.settle_hooks(&mut off, &audit);
        assert!(s.pending_hooks.is_empty(), "no modal queue anymore");
        // Non-yolo Ask goes straight back so the harness handles it.
        let line = reply_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(line.contains(r#""permissionDecision":"ask""#), "line: {line:?}");
        let logged = std::fs::read_to_string(&audit).unwrap();
        assert_eq!(logged.lines().count(), 2, "audit: {logged:?}");
        assert!(logged.contains(r#""decision":"ask""#), "audit: {logged:?}");
        let _ = std::fs::remove_file(&audit);
    }

    #[test]
    fn safe_only_blocks_still_deny_without_a_modal() {
        use crate::config::PermissionMode;
        let audit = std::env::temp_dir().join(format!(
            "forge-block-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&audit);
        let mut policy = crate::policy::Policy::new(
            PermissionMode::SafeOnly,
            &[],
            &["doom".to_string()],
        )
        .unwrap();
        let mut s = AppState::new();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
            hook: "PreToolUse".to_string(),
            body: r#"{"tool_name":"Bash","tool_input":{"command":"doom"}}"#.to_string(),
            run_id: String::new(),
            sync: true,
            reply: reply_tx,
            timed_out: Default::default(),
        }));
        s.settle_hooks(&mut policy, &audit);
        assert!(s.pending_hooks.is_empty());
        let line = reply_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(line.contains(r#""permissionDecision":"deny""#), "line: {line:?}");
        let logged = std::fs::read_to_string(&audit).unwrap();
        assert_eq!(logged.lines().count(), 1, "audit: {logged:?}");
        let _ = std::fs::remove_file(&audit);
    }

    #[test]
    fn comms_ask_flows_through_apply_and_replies() {
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "ask_session".to_string(),
            args: r#"{"target":"b","message":"ready?"}"#.to_string(),
            reply: reply_tx,
        }));
        let line = reply_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(line.contains(r#""ok":true"#), "line: {line:?}");
        assert!(line.contains(r#""conversation":""#), "line: {line:?}");
        assert!(line.ends_with('\n'));
        assert_eq!(s.broker.queued(b), 1);
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn comms_rejections_are_single_line_json() {
        let mut s = AppState::new();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: "f".repeat(32),
            tool: "ask_session".to_string(),
            args: "{}".to_string(),
            reply: reply_tx,
        }));
        let line = reply_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(line.contains(r#""ok":false"#), "line: {line:?}");
        assert!(!line.trim_end().contains('\n'), "one line: {line:?}");
    }

    #[test]
    fn session_exit_fails_open_conversations() {
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "ask_session".to_string(),
            args: r#"{"target":"b","message":"q?"}"#.to_string(),
            reply: reply_tx,
        }));
        assert!(s.manager.kill(b));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                s.apply(AppEvent::from_pty(ev.0, ev.2));
            }
            let exited = s.manager.get(b).is_none_or(|rec| !rec.state.is_live());
            if exited || std::time::Instant::now() > deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let due = s.broker.take_due(a, 10);
        assert_eq!(due.len(), 1, "source learns the failure");
        assert!(matches!(
            due[0].kind,
            crate::comms::InjectKind::Failed
        ));
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn settle_comms_delivers_to_idle_panes_only() {
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        // Busy target: the injection waits.
        assert!(s.manager.set_activity(b, crate::session::Activity::ToolUse));
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "tell_session".to_string(),
            args: "{\"target\":\"b\",\"text\":\"hello-b\"}".to_string(),
            reply: reply_tx,
        }));
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 1, "busy target waits");
        // Idle target: delivered into the pane.
        assert!(s.manager.set_activity(b, crate::session::Activity::Idle));
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                s.apply(AppEvent::from_pty(ev.0, ev.2));
            }
            if s.manager.screen_text(b).is_some_and(|t| t.contains("hello-b")) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "injection never reached the pane"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn reply_round_trip_needs_stop_to_reach_first_session() {
        // Live shape: both agents have fired hooks, so neither is Idle.
        // The ask goes out, the target answers, and the reply waits until
        // the first session's turn ends (Stop parks Stopped). Before the
        // Stop edge was installed, that wait never ended.
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        let hook = |s: &mut AppState, hook: &str, run_id: String| {
            let (reply_tx, _) = std::sync::mpsc::channel();
            s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
                hook: hook.to_string(),
                body: "{}".to_string(),
                run_id,
                sync: false,
                reply: reply_tx,
                timed_out: Default::default(),
            }));
        };
        let comms = |s: &mut AppState, run_id: String, tool: &str, args: &str| -> String {
            let (reply_tx, reply_rx) = std::sync::mpsc::channel();
            s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
                run_id,
                tool: tool.to_string(),
                args: args.to_string(),
                reply: reply_tx,
            }));
            reply_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("broker answers at once")
        };
        // Both sessions mid-turn, like live agents that called tools.
        hook(&mut s, "PreToolUse", run_a.to_string());
        hook(&mut s, "PreToolUse", run_b.to_string());
        // A's ask waits while B is busy, then lands when B's turn ends.
        let ask_line = comms(&mut s, run_a.to_string(), "ask_session", "{\"target\":\"b\",\"message\":\"ready?\"}");
        assert!(ask_line.contains("\"ok\":true"), "ask accepted: {ask_line}");
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 1, "busy target holds the ask");
        hook(&mut s, "Stop", run_b.to_string());
        // The Stop hook stamped hook activity; age it past the debounce
        // beat so this settle tests activity gating, not hook timing.
        s.last_hook_activity = Some(
            std::time::Instant::now()
                - crate::comms::INJECT_HOOK_DEBOUNCE
                - std::time::Duration::from_millis(100),
        );
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 0, "turn end delivers the ask");
        // B answers; A is still mid-turn so the reply waits.
        let conv = crate::policy::json_string_field(ask_line.as_bytes(), &["conversation"])
            .expect("ask returns a conversation");
        let resp_line = comms(
            &mut s,
            run_b.to_string(),
            "send_response",
            &format!("{{\"conversation_id\":\"{conv}\",\"message\":\"got-it\"}}"),
        );
        assert!(resp_line.contains("\"ok\":true"), "reply accepted: {resp_line}");
        s.settle_comms();
        assert_eq!(s.broker.queued(a), 1, "reply waits while A works");
        // A's turn ends: the reply lands in A's pane.
        hook(&mut s, "Stop", run_a.to_string());
        s.last_hook_activity = Some(
            std::time::Instant::now()
                - crate::comms::INJECT_HOOK_DEBOUNCE
                - std::time::Duration::from_millis(100),
        );
        s.settle_comms();
        assert_eq!(s.broker.queued(a), 0, "turn end delivers the reply");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                s.apply(AppEvent::from_pty(ev.0, ev.2));
            }
            if s.manager.screen_text(a).is_some_and(|t| t.contains("got-it")) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "reply never reached the first session"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn prompt_submit_holds_injections_until_stop() {
        // A freshly prompted session looks settled but its agent is
        // generating: UserPromptSubmit must hold injections (where Enter
        // would be eaten and the text left as a draft) until Stop.
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        let hook = |s: &mut AppState, hook: &str, run_id: String| {
            let (reply_tx, _) = std::sync::mpsc::channel();
            s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
                hook: hook.to_string(),
                body: "{}".to_string(),
                run_id,
                sync: false,
                reply: reply_tx,
                timed_out: Default::default(),
            }));
        };
        // A was parked (Stopped) but just got prompted: generating.
        hook(&mut s, "Stop", run_a.to_string());
        hook(&mut s, "UserPromptSubmit", run_a.to_string());
        assert_eq!(
            s.manager.get(a).unwrap().activity,
            crate::session::Activity::Thinking
        );
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run_b.to_string(),
            tool: "tell_session".to_string(),
            args: "{\"target\":\"a\",\"text\":\"hold\"}".to_string(),
            reply: reply_tx,
        }));
        s.settle_comms();
        assert_eq!(s.broker.queued(a), 1, "generating target waits");
        hook(&mut s, "Stop", run_a.to_string());
        // The Stop hook stamped hook activity; age it past the debounce
        // beat so this settle tests activity gating, not hook timing.
        s.last_hook_activity = Some(
            std::time::Instant::now()
                - crate::comms::INJECT_HOOK_DEBOUNCE
                - std::time::Duration::from_millis(100),
        );
        s.settle_comms();
        assert_eq!(s.broker.queued(a), 0, "turn end delivers");
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn settle_comms_stages_enter_beat_after_body() {
        // Byte-level human parity through a raw-mode slave (no CR/LF
        // translation anywhere, no echo): the body arrives whole with no
        // Enter bundled in, and the CR follows as its own input event
        // after the beat — type text, press Enter, never one burst.
        use crate::pty::PtyEvent;
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn(
                "b",
                &std::env::temp_dir(),
                "stty raw -echo && printf READY || printf STTYFAIL; exec cat",
                run_b.clone(),
                "shell",
            )
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        // Wait out the spawn: anything the starting shell echoes (including
        // canonical-mode translations) predates raw mode and must not
        // pollute the byte assertions below.
        let mut raw = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                if ev.0 == b {
                    if let PtyEvent::Output(bytes) = &ev.2 {
                        raw.extend_from_slice(bytes);
                    }
                }
            }
            if raw.windows(8).any(|w| w == b"STTYFAIL") {
                panic!("stty raw failed in the probe session");
            }
            if raw.windows(5).any(|w| w == b"READY") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "probe session never went raw, got: {raw:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        raw.clear();
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "tell_session".to_string(),
            args: "{\"target\":\"b\",\"message\":\"ping-body\"}".to_string(),
            reply: reply_tx,
        }));
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 0, "raw session is idle: body delivered");
        assert!(s.pending_enter.contains_key(&b), "enter staged, not sent");
        // Drain raw output until cat echoes the full body back.
        let body = b"[forge tell_session from a]: ping-body";
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                if ev.0 == b {
                    if let PtyEvent::Output(bytes) = &ev.2 {
                        raw.extend_from_slice(bytes);
                    }
                }
            }
            if raw.windows(body.len()).any(|w| w == body) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "body never echoed, got: {raw:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!raw.contains(&b'\r'), "no Enter bundled with the body");
        // After the beat the CR goes out as its own event, trailing the body.
        std::thread::sleep(
            crate::comms::INJECT_ENTER_DELAY + std::time::Duration::from_millis(100),
        );
        s.settle_comms();
        assert!(!s.pending_enter.contains_key(&b), "enter sent");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            for ev in s.manager.drain_pty_max(100) {
                if ev.0 == b {
                    if let PtyEvent::Output(bytes) = &ev.2 {
                        raw.extend_from_slice(bytes);
                    }
                }
            }
            if raw.contains(&b'\r') {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "staged enter never arrived, got: {raw:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let pos = raw
            .windows(body.len())
            .position(|w| w == body)
            .expect("body present");
        assert!(
            raw[pos + body.len()..].contains(&b'\r'),
            "enter trails the body: {raw:?}"
        );
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn settle_comms_holds_during_human_typing() {
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "tell_session".to_string(),
            args: r#"{"target":"b","message":"wait"}"#.to_string(),
            reply: reply_tx,
        }));
        s.note_human_input(a);
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 1, "fresh typing debounces delivery");
        // Human input also drops a staged Enter for that session: our CR
        // must never submit the human's draft.
        s.pending_enter.insert(b, std::time::Instant::now());
        s.note_human_input(b);
        assert!(!s.pending_enter.contains_key(&b), "human owns the prompt");
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn settle_comms_holds_after_hook_activity() {
        let mut s = AppState::new();
        let run_a = RunId::generate();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
            .unwrap();
        let run_b = RunId::generate();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, b, "peers").unwrap();
        assert!(s.manager.set_activity(b, crate::session::Activity::Idle));
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run_a.to_string(),
            tool: "tell_session".to_string(),
            args: r#"{"target":"b","message":"wait"}"#.to_string(),
            reply: reply_tx,
        }));
        // Fresh hook activity holds delivery even to an idle target.
        s.last_hook_activity = Some(std::time::Instant::now());
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 1, "hook debounce holds delivery");
        // Past the beat the same settle delivers.
        s.last_hook_activity = Some(
            std::time::Instant::now()
                - crate::comms::INJECT_HOOK_DEBOUNCE
                - std::time::Duration::from_millis(100),
        );
        s.settle_comms();
        assert_eq!(s.broker.queued(b), 0);
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn hook_requests_and_verdicts_stamp_hook_activity() {
        use crate::config::PermissionMode;
        let mut s = AppState::new();
        assert_eq!(s.last_hook_activity, None);
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
            hook: "PreToolUse".to_string(),
            body: "{}".to_string(),
            run_id: String::new(),
            sync: true,
            reply: reply_tx,
            timed_out: Default::default(),
        }));
        assert!(s.last_hook_activity.is_some(), "enqueue stamps");
        // A verdict re-stamps: backdate first so only the settle can renew.
        s.last_hook_activity =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(3600));
        let audit = std::env::temp_dir().join(format!(
            "forge-hook-stamp-test-{}",
            std::process::id()
        ));
        let mut yolo =
            crate::policy::Policy::new(PermissionMode::Yolo, &[], &[]).unwrap();
        s.settle_hooks(&mut yolo, &audit);
        let age = std::time::Instant::now()
            .duration_since(s.last_hook_activity.unwrap());
        assert!(age < std::time::Duration::from_secs(5), "verdict stamps: {age:?}");
        let _ = std::fs::remove_file(&audit);
    }

    #[test]
    fn create_session_spawns_named_shell() {
        let mut s = AppState::new();
        let spec = crate::create::SessionSpec {
            kind: crate::create::SessionKind::Shell,
            name: "work".to_string(),
            cwd: std::env::temp_dir(),
            model: String::new(),
            group: None,
        };
        let id = s.create_session(&spec).unwrap();
        let rec = s.manager.get(id).unwrap();
        assert_eq!(rec.name, "work");
        assert_eq!(rec.cli_tool, "shell");
        assert_eq!(rec.cwd, std::env::temp_dir());
        assert!(s.manager.remove(id));
    }

    #[test]
    fn create_session_focuses_the_new_session() {
        let mut s = AppState::new();
        let spec = |name: &str| crate::create::SessionSpec {
            kind: crate::create::SessionKind::Shell,
            name: name.to_string(),
            cwd: std::env::temp_dir(),
            model: String::new(),
            group: None,
        };
        let first = s.create_session(&spec("a")).unwrap();
        assert_eq!(s.manager.active(), Some(first));
        let second = s.create_session(&spec("b")).unwrap();
        assert_eq!(s.manager.active(), Some(second), "creating focuses the new one");
        assert!(s.manager.remove(first));
        assert!(s.manager.remove(second));
    }

    #[test]
    fn create_session_with_group_joins_at_birth() {
        let mut s = AppState::new();
        s.broker.create_group("team").unwrap();
        let id = s
            .create_session(&crate::create::SessionSpec {
                kind: crate::create::SessionKind::Shell,
                name: "work".to_string(),
                cwd: std::env::temp_dir(),
                model: String::new(),
                group: Some("team".to_string()),
            })
            .unwrap();
        assert!(s.broker.is_member(id, "team"));
        assert_eq!(s.tabs().iter().find(|t| t.title == "work").and_then(|t| t.group.clone()), Some("team".to_string()), "bar reflects it");
        assert!(s.manager.remove(id));
    }

    #[test]
    fn create_session_agent_records_cli_tool() {
        // Hermetic: stand in for the codex binary; argv shape is locked in
        // the harness registry tests.
        let saved = std::env::var("CODEX_BIN").ok();
        std::env::set_var("CODEX_BIN", "/bin/true");
        let mut s = AppState::new();
        let spec = crate::create::SessionSpec {
            kind: crate::create::SessionKind::Agent(
                crate::harness::Harness::from_name("codex").unwrap(),
            ),
            name: "coder".to_string(),
            cwd: std::env::temp_dir(),
            model: "gpt-5".to_string(),
            group: None,
        };
        let id = s.create_session(&spec).unwrap();
        let rec = s.manager.get(id).unwrap();
        assert_eq!(rec.cli_tool, "codex");
        assert!(s.manager.remove(id));
        match saved {
            Some(v) => std::env::set_var("CODEX_BIN", v),
            None => std::env::remove_var("CODEX_BIN"),
        }
    }

    #[test]
    fn suggested_name_skips_taken_names() {
        let mut s = AppState::new();
        assert_eq!(s.suggested_session_name(), "claude-1");
        s.open_create_dialog();
        assert!(s.create_dialog.is_some());
        let id = s
            .manager
            .spawn("claude-1", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        assert_eq!(s.suggested_session_name(), "claude-2");
        assert!(s.manager.remove(id));
    }

    #[test]
    fn session_traffic_dirties() {
        let mut s = AppState::new();
        s.dirty = false;
        let id = SessionId::fresh();
        s.apply(AppEvent::SessionOutput { id, data: vec![1] });
        assert!(s.dirty);
        s.dirty = false;
        s.apply(AppEvent::SessionExited { id, code: Some(0) });
        assert!(s.dirty);
    }

    #[test]
    fn resize_records_and_dirties() {
        let mut s = AppState::new();
        s.dirty = false;
        s.apply(AppEvent::Resize(40, 120));
        assert_eq!(s.term_size, (40, 120));
        assert!(s.dirty);
    }

    #[test]
    fn tick_is_quiet() {
        let mut s = AppState::new();
        s.dirty = false;
        s.apply(AppEvent::Tick);
        assert!(!s.dirty);
        assert!(!s.should_quit);
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
    fn select_session_focuses_by_index() {
        let mut s = AppState::new();
        assert!(!s.select_session(0), "empty: no-op");
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        assert!(s.select_session(1));
        assert_eq!(s.manager.active(), Some(b));
        assert!(s.select_session(0));
        assert_eq!(s.manager.active(), Some(a));
        assert!(!s.select_session(9), "out of range keeps focus");
        assert_eq!(s.manager.active(), Some(a));
        let tabs = s.tabs();
        assert_eq!(tabs.len(), 2);
        assert!(tabs[0].focused && !tabs[1].focused);
        assert_eq!(s.sidebar_info().pending, 0);
        assert!(s.manager.remove(a));
        assert!(s.manager.remove(b));
    }

    #[test]
    fn terminate_session_drops_ui_and_groups() {
        let mut s = AppState::new();
        let a = s
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        let b = s
            .manager
            .spawn("b", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
            .unwrap();
        s.broker.join(&s.manager, a, "peers").unwrap();
        s.broker.join(&s.manager, a, "other").unwrap();
        assert!(s.terminate_session(a));
        assert!(s.manager.get(a).is_none(), "record gone");
        assert_eq!(s.manager.order().len(), 1);
        assert!(!s.broker.is_member(a, "peers"), "left peers");
        assert!(!s.broker.is_member(a, "other"), "left other");
        assert_eq!(s.manager.active(), Some(b), "focus falls through");
        assert!(!s.terminate_session(a), "unknown id is a no-op");
        assert!(s.manager.remove(b));
    }

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
    fn hook_requests_attribute_activity_to_sender() {
        let mut s = AppState::new();
        let run = RunId::generate();
        let id = s
            .manager
            .spawn("h", &std::env::temp_dir(), "exec sleep 30", run.clone(), "shell")
            .unwrap();
        fn hook(s: &mut AppState, hook: &str, run_id: String) {
            let (reply_tx, _) = std::sync::mpsc::channel();
            s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
                hook: hook.to_string(),
                body: "{}".to_string(),
                run_id,
                sync: false,
                reply: reply_tx,
                timed_out: Default::default(),
            }));
        }
        hook(&mut s, "PreToolUse", run.to_string());
        assert_eq!(
            s.manager.get(id).unwrap().activity,
            crate::session::Activity::ToolUse
        );
        hook(&mut s, "Stop", run.to_string());
        assert_eq!(
            s.manager.get(id).unwrap().activity,
            crate::session::Activity::Stopped
        );
        // Unknown runs never touch live sessions.
        hook(&mut s, "PreToolUse", "f".repeat(32));
        assert_eq!(
            s.manager.get(id).unwrap().activity,
            crate::session::Activity::Stopped
        );
        assert!(s.manager.remove(id));
    }

    #[test]
    fn permission_mode_toggle_spans_off_and_yolo() {
        let mut s = AppState::new();
        assert_eq!(s.permission_mode, crate::config::PermissionMode::Yolo);
        s.dirty = false;
        assert!(s.toggle_permission_mode());
        assert_eq!(s.permission_mode, crate::config::PermissionMode::Off);
        assert!(s.dirty);
        assert!(!s.set_permission_mode(crate::config::PermissionMode::Off));
        assert!(s.toggle_permission_mode());
        assert_eq!(s.permission_mode, crate::config::PermissionMode::Yolo);
        // Foreign modes collapse to Yolo, never back through the toggle.
        assert!(s.set_permission_mode(crate::config::PermissionMode::SafeOnly));
        assert!(s.toggle_permission_mode());
        assert_eq!(s.permission_mode, crate::config::PermissionMode::Yolo);
    }

    #[test]
    fn hook_attribution_feeds_sidebar_counters() {
        let mut s = AppState::new();
        let run = RunId::generate();
        let id = s
            .manager
            .spawn("h", &std::env::temp_dir(), "exec sleep 30", run.clone(), "shell")
            .unwrap();
        let rec = s.manager.get(id).unwrap();
        assert_eq!((rec.tool_calls, rec.approvals, rec.denials), (0, 0, 0));
        // Tool-gated hook: activity + one tool call.
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::HookRequest(crate::listener::HookRequest {
            hook: "PreToolUse".to_string(),
            body: "{}".to_string(),
            run_id: run.to_string(),
            sync: false,
            reply: reply_tx,
            timed_out: Default::default(),
        }));
        assert_eq!(s.manager.get(id).unwrap().tool_calls, 1);
        // Yolo settle: one approval, no denial.
        let audit = std::env::temp_dir().join(format!(
            "forge-counters-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&audit);
        let mut yolo =
            crate::policy::Policy::new(crate::config::PermissionMode::Yolo, &[], &[]).unwrap();
        s.settle_hooks(&mut yolo, &audit);
        let rec = s.manager.get(id).unwrap();
        assert_eq!((rec.approvals, rec.denials), (1, 0));
        let _ = std::fs::remove_file(&audit);
        assert!(s.manager.remove(id));
    }

    #[test]
    fn create_session_routes_agent_to_dual_tabs() {
        // Point the codex binary at `cat` so the test never depends on a
        // real agent CLI being installed; no other test reads this var.
        std::env::set_var("CODEX_BIN", "cat");
        let mut s = AppState::new();
        let agent = s
            .create_session(&crate::create::SessionSpec {
                kind: crate::create::SessionKind::Agent(
                    crate::harness::Harness::from_name("codex").unwrap(),
                ),
                name: "codex-1".to_string(),
                cwd: std::env::temp_dir(),
                model: String::new(),
                group: None,
            })
            .unwrap();
        std::env::remove_var("CODEX_BIN");
        assert_eq!(s.manager.tab_count(agent), 3);
        assert!(s.manager.switch_tab(agent));
        let shell = s
            .create_session(&crate::create::SessionSpec {
                kind: crate::create::SessionKind::Shell,
                name: "shell-1".to_string(),
                cwd: std::env::temp_dir(),
                model: String::new(),
                group: None,
            })
            .unwrap();
        assert_eq!(s.manager.tab_count(shell), 1);
        assert!(s.manager.remove(agent));
        assert!(s.manager.remove(shell));
    }

    #[test]
    fn agent_topbar_exposes_clickable_read_only_views() {
        let mut state = AppState::new();
        state.term_size = (40, 180);
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        let labels: Vec<String> = state.topbar().tabs.iter().map(|tab| tab.label.clone()).collect();
        assert_eq!(labels, ["🤖 Codex", "💻 Terminal", "🔀 SCM", "🔔 Events", "📝 Tasks", "📷 Visual", "📖 Walkthrough"]);
        assert!(state.select_top_tab(3));
        assert!(state.topbar().tabs[3].active);
        let view = state.views().into_iter().find(|v| v.focused).unwrap();
        assert!(view.lines.iter().flatten().any(|span| span.text.contains("Events")));
        assert!(view.lines.iter().flatten().any(|span| span.text.contains("unavailable")));
        assert!(state.select_top_tab(0));
        assert!(state.topbar().tabs[0].active);
        assert!(state.manager.remove(id));
    }

    fn comms_reply(state: &mut AppState, run: &str, tool: &str, args: &str) -> String {
        let (tx, rx) = std::sync::mpsc::channel();
        state.apply(AppEvent::CommsRequest(crate::listener::CommsRequest {
            run_id: run.to_string(),
            tool: tool.to_string(),
            args: args.to_string(),
            reply: tx,
        }));
        rx.recv().expect("comms verdict arrives")
    }

    fn bot_reply(
        state: &mut AppState,
        name: &str,
        token: &str,
        tool: &str,
        args: &str,
    ) -> String {
        let (tx, rx) = std::sync::mpsc::channel();
        state.apply(AppEvent::BotRequest(crate::listener::BotRequest {
            name: name.to_string(),
            token: token.to_string(),
            tool: tool.to_string(),
            args: args.to_string(),
            reply: tx,
        }));
        rx.recv().expect("bot verdict arrives")
    }

    const BOT_TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn bot_state() -> AppState {
        let mut state = AppState::new();
        let run = RunId::generate();
        let id = state
            .manager
            .spawn("a", &std::env::temp_dir(), "exec sleep 30", run, "shell")
            .unwrap();
        state.broker.join(&state.manager, id, "peers").unwrap();
        state
            .broker
            .register_client(
                &state.manager,
                "skippy",
                vec!["peers".to_string()],
                BOT_TOKEN,
                Vec::new(),
            )
            .expect("registration validates");
        state
    }

    #[test]
    fn bot_dispatch_answers_and_object_errors() {
        let mut state = bot_state();
        let list = bot_reply(&mut state, "skippy", BOT_TOKEN, "list_sessions", "{}");
        assert!(list.contains(r#""ok":true"#), "list: {list}");
        assert!(list.contains(r#""you":"skippy""#), "list: {list}");
        assert!(list.contains("\"epoch\":"), "list: {list}");
        let bad = bot_reply(&mut state, "skippy", "wrong-credential-0000000000000000", "list_sessions", "{}");
        assert!(bad.contains(r#""ok":false"#), "bad: {bad}");
        assert!(bad.contains(r#""code":"unauthorized""#), "bad: {bad}");
        assert!(state.manager.remove(state.manager.order()[0]));
    }

    #[cfg(feature = "visual")]
    fn spawn_visual_agent(state: &mut AppState, name: &str) -> (crate::session::SessionId, String) {
        let id = state.manager.spawn_agent(
            name, &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        let run = state.manager.get(id).unwrap().run_id.as_str().to_string();
        (id, run)
    }

    #[cfg(feature = "visual")]
    fn fake_png(len: usize, w: u32, h: u32) -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A,
            0, 0, 0, 13, b'I', b'H', b'D', b'R'];
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.resize(len.max(24), 0);
        v
    }

    #[cfg(feature = "visual")]
    fn complete(
        state: &mut AppState,
        session: crate::session::SessionId,
        generation: u64,
        png: Vec<u8>,
    ) {
        let (width, height) = crate::visual::png_dimensions(&png).unwrap();
        state.visual_complete(crate::app::VisualDone {
            session,
            generation,
            title: String::new(),
            alt: String::new(),
            result: Ok(crate::visual::RasterFrame {
                png,
                rgba: Vec::new(),
                width,
                height,
            }),
        });
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_show_accepts_mermaid_for_background_render() {
        let mut state = AppState::new();
        let (_id, live_run) = spawn_visual_agent(&mut state, "agent");
        let out = comms_reply(
            &mut state,
            &live_run,
            "visual_show",
            r#"{"content":"flowchart LR\n    A-->B","format":"mermaid","title":"flow"}"#,
        );
        assert!(out.contains(r#""ok":true"#), "accepted: {out}");
        assert!(out.contains("\"accepted\":true"), "queued: {out}");
        assert!(out.contains("\"generation\":1"), "stamped: {out}");
        assert!(!out.contains("\"width\""), "no sync dimensions: {out}");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_complete_stores_newest_and_drops_stale() {
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        complete(&mut state, id, 2, fake_png(64, 10, 10));
        assert_eq!(state.visual_slots.get(&id).unwrap().generation, 2);
        complete(&mut state, id, 1, fake_png(64, 20, 20));
        let slot = state.visual_slots.get(&id).unwrap();
        assert_eq!(slot.generation, 2, "stale render dropped");
        assert_eq!((slot.width, slot.height), (10, 10));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_complete_drops_missing_session() {
        let mut state = AppState::new();
        let ghost = crate::session::SessionId::fresh();
        complete(&mut state, ghost, 1, fake_png(64, 10, 10));
        assert!(state.visual_slots.is_empty(), "no slot for dead session");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_lru_evicts_oldest_inactive_first() {
        let mut state = AppState::new();
        state.visual_budget_count = 2;
        let (a, _) = spawn_visual_agent(&mut state, "a");
        let (b, _) = spawn_visual_agent(&mut state, "b");
        let (c, _) = spawn_visual_agent(&mut state, "c");
        assert!(state.select_session(0), "focus oldest");
        assert_eq!(state.manager.active(), Some(a));
        complete(&mut state, a, 1, fake_png(64, 10, 10));
        complete(&mut state, b, 2, fake_png(64, 10, 10));
        complete(&mut state, c, 3, fake_png(64, 10, 10));
        assert_eq!(state.visual_slots.len(), 2);
        assert!(state.visual_slots.contains_key(&a), "active survives");
        assert!(state.visual_slots.contains_key(&c), "newest survives");
        assert!(!state.visual_slots.contains_key(&b), "oldest inactive evicted");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_budget_counts_decoded_bytes() {
        let mut state = AppState::new();
        state.visual_budget_bytes = 100;
        let (a, _) = spawn_visual_agent(&mut state, "a");
        let (b, _) = spawn_visual_agent(&mut state, "b");
        assert!(state.select_session(1), "focus newest");
        complete(&mut state, a, 1, fake_png(60, 10, 10));
        complete(&mut state, b, 2, fake_png(60, 10, 10));
        assert_eq!(state.visual_slots.len(), 1);
        assert!(state.visual_slots.contains_key(&b), "newest survives bytes");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_drain_collects_worker_results() {
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        state.visual_tx.clone().send(crate::app::VisualDone {
            session: id,
            generation: 7,
            title: "t".to_string(),
            alt: String::new(),
            result: Ok(crate::visual::RasterFrame {
                png: fake_png(64, 10, 10),
                rgba: Vec::new(),
                width: 10,
                height: 10,
            }),
        }).unwrap();
        state.drain_visual();
        assert_eq!(state.visual_slots.get(&id).unwrap().generation, 7);
        assert_eq!(state.visual_slots.get(&id).unwrap().title, "t");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_show_rejects_bad_format_empty_and_stale_caller() {
        let mut state = AppState::new();
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        let live_run = state.manager.get(id).unwrap().run_id.as_str().to_string();
        // Unknown formats name the supported set.
        let out = comms_reply(
            &mut state,
            &live_run,
            "visual_show",
            r#"{"content":"<b>x</b>","format":"html"}"#,
        );
        assert!(out.contains(r#""ok":false"#), "rejected: {out}");
        assert!(out.contains("mermaid"), "names supported: {out}");
        // Missing format defaults to mermaid.
        let out = comms_reply(
            &mut state,
            &live_run,
            "visual_show",
            r#"{"content":"flowchart LR\n    A-->B"}"#,
        );
        assert!(out.contains(r#""ok":true"#), "default format: {out}");
        // Empty content and stale callers fail closed.
        let out = comms_reply(&mut state, &live_run, "visual_show", r#"{"content":""}"#);
        assert!(out.contains(r#""ok":false"#), "empty: {out}");
        let out = comms_reply(
            &mut state,
            "run-dead",
            "visual_show",
            r#"{"content":"flowchart LR\n    A-->B"}"#,
        );
        assert!(out.contains("stale run ID"), "stale: {out}");
    }

    #[cfg(feature = "visual")]
    fn show_visual_overlay(state: &mut AppState, id: crate::session::SessionId) {
        // Selection clears the overlay, so focus first, then open it.
        state.select_session(0);
        let slot = state.visual_slot(id).expect("visual slot exists");
        state.overlay_view = Some((id, slot));
    }

    #[cfg(feature = "visual")]
    fn paint_for(state: &AppState, id: crate::session::SessionId) -> crate::visual::VisualPaint {
        state
            .visual_paint(id, ratatui::layout::Rect::new(0, 0, 200, 50), 8.0, 16.0)
            .expect("paint")
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_take_show_hide_tracks_terminal_image() {
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        complete(&mut state, id, 1, fake_png(64, 10, 10));
        show_visual_overlay(&mut state, id);
        let paint = paint_for(&state, id);
        let spec = state.visual_take_show(true, paint).expect("show once");
        assert_eq!(spec.generation, 1);
        assert!(!spec.replace, "first claim takes a fresh id");
        assert!(state.visual_take_show(true, paint).is_none(), "already shown");
        assert!(state.visual_take_show(false, paint).is_none(), "no kitty no show");
        assert_eq!(state.visual_frame_png(id, 1, paint).unwrap().len(), 64);
        // New generation hides the old image, then shows the new one.
        complete(&mut state, id, 2, fake_png(64, 10, 10));
        assert_eq!(state.visual_take_hide(), Some(spec.image_id));
        let spec2 = state.visual_take_show(true, paint_for(&state, id)).expect("reshow");
        assert_ne!(spec2.image_id, spec.image_id, "fresh placement id");
        // Overlay away hides; double hide is silent.
        state.overlay_view = None;
        assert_eq!(state.visual_take_hide(), Some(spec2.image_id));
        assert_eq!(state.visual_take_hide(), None);
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_take_show_repaints_viewport_changes_under_the_same_id() {
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        complete(&mut state, id, 1, fake_png(64, 100, 100));
        show_visual_overlay(&mut state, id);
        let paint = paint_for(&state, id);
        let spec = state.visual_take_show(true, paint).expect("show");
        // Zooming changes the fingerprint: same placement id, replace.
        assert!(state.visual_zoom(id, crate::ui::VisualButton::ZoomIn, 200, 50, 8.0, 16.0));
        let zoomed = paint_for(&state, id);
        assert_ne!(zoomed, paint, "zoom moves the paint");
        let reshow = state.visual_take_show(true, zoomed).expect("repaint");
        assert_eq!(reshow.image_id, spec.image_id, "no fresh id for a viewport change");
        assert!(reshow.replace, "TUI deletes before re-transmitting");
        assert!(state.visual_take_show(true, zoomed).is_none(), "paint now current");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_zoom_and_scroll_clamp_to_the_overflow() {
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        complete(&mut state, id, 1, fake_png(64, 1000, 1000));
        // Zoomed out at minimum: further out changes nothing.
        for _ in 0..20 {
            state.visual_zoom(id, crate::ui::VisualButton::ZoomOut, 200, 50, 8.0, 16.0);
        }
        let slot = state.visual_slots.get(&id).unwrap();
        assert_eq!(slot.zoom, crate::visual::MIN_ZOOM);
        state.dirty = false;
        assert!(!state.visual_zoom(id, crate::ui::VisualButton::ZoomOut, 200, 50, 8.0, 16.0));
        assert!(!state.dirty, "no-op zoom stays clean");
        // Zoom to maximum: 1000x1000 at 8x overflows a 200x50 tab by
        // (600, 350) cells.
        for _ in 0..30 {
            state.visual_zoom(id, crate::ui::VisualButton::ZoomIn, 200, 50, 8.0, 16.0);
        }
        assert_eq!(state.visual_slots.get(&id).unwrap().zoom, crate::visual::MAX_ZOOM);
        assert!(!state.visual_zoom(id, crate::ui::VisualButton::ZoomIn, 200, 50, 8.0, 16.0));
        state.dirty = false;
        assert!(state.visual_scroll(id, 5, 7, 200, 50, 8.0, 16.0));
        assert!(state.dirty, "scroll marks dirty");
        let slot = state.visual_slots.get(&id).unwrap();
        assert_eq!((slot.scroll_x, slot.scroll_y), (5, 7));
        assert!(!state.visual_scroll(id, 0, 0, 200, 50, 8.0, 16.0), "no-op scroll");
        assert!(state.visual_scroll(id, 10_000, 10_000, 200, 50, 8.0, 16.0));
        let slot = state.visual_slots.get(&id).unwrap();
        assert_eq!((slot.scroll_x, slot.scroll_y), (600, 350), "pinned to overflow");
        assert!(state.visual_scroll(id, -1, -1, 200, 50, 8.0, 16.0));
        // Unknown sessions never dirty.
        state.dirty = false;
        let ghost = crate::session::SessionId::fresh();
        assert!(!state.visual_scroll(ghost, 1, 1, 200, 50, 8.0, 16.0));
        assert!(!state.visual_zoom(ghost, crate::ui::VisualButton::ZoomIn, 200, 50, 8.0, 16.0));
        assert!(!state.dirty);
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_paint_contains_and_centers_a_small_diagram() {
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        complete(&mut state, id, 1, fake_png(64, 400, 200));
        // 400x200 at 8x16 cells is natively 50x13: contained, not
        // stretched to the 200-column region.
        let paint = state
            .visual_paint(id, ratatui::layout::Rect::new(1, 2, 200, 50), 8.0, 16.0)
            .expect("paint");
        assert_eq!((paint.out_cols, paint.out_rows), (50, 13));
        assert_eq!((paint.cursor_x, paint.cursor_y), (76, 20), "centered");
        assert_eq!((paint.sx, paint.sy, paint.sw, paint.sh), (0, 0, 400, 200));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_paint_png_skips_reencode_for_full_frames() {
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        let mut rgba = Vec::new();
        for p in [[255u8, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [255, 255, 255, 255]] {
            rgba.extend_from_slice(&p);
        }
        state.visual_slots.insert(id, crate::app::VisualSlot {
            generation: 1,
            png: fake_png(30, 2, 2),
            rgba,
            width: 2,
            height: 2,
            title: String::new(),
            alt: String::new(),
            zoom: 1.0,
            scroll_x: 0,
            scroll_y: 0,
        });
        let full = crate::visual::VisualPaint {
            zoom_bits: 1f32.to_bits(),
            ox: 0, oy: 0, out_cols: 1, out_rows: 1,
            cursor_x: 0, cursor_y: 0,
            sx: 0, sy: 0, sw: 2, sh: 2,
        };
        assert_eq!(state.visual_frame_png(id, 1, full).unwrap(), fake_png(30, 2, 2));
        // A cropped viewport re-encodes just its region: top-left red.
        let cut = crate::visual::VisualPaint { sw: 1, sh: 1, ..full };
        let png = state.visual_frame_png(id, 1, cut).expect("cropped bytes");
        let (back, w, h) = crate::visual::decode_rgba(&png).expect("decodes");
        assert_eq!((w, h), (1, 1));
        assert_eq!(&back[..4], &[255, 0, 0, 255]);
        assert!(state.visual_frame_png(id, 2, full).is_none(), "stale generation");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_kitty_view_carries_the_zoom_strip_without_art() {
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        complete(&mut state, id, 1, fake_png(64, 10, 10));
        let view = state.visual_view(id, true);
        let strip: String = view.lines[0].iter().map(|s| s.text.as_str()).collect();
        assert!(strip.contains("zoom in") && strip.contains("zoom out"), "buttons: {strip:?}");
        let text: String = view.lines.iter().flat_map(|r| r.iter().map(|s| s.text.as_str())).collect();
        assert!(!text.contains('▀'), "image paints over the pane");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_view_falls_back_to_halfblock_with_alt() {
        let mut state = AppState::new();
        let (id, _) = spawn_visual_agent(&mut state, "agent");
        // 2x2: red/green over blue/white, titled with alt text.
        let mut rgba = Vec::new();
        for p in [[255u8, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [255, 255, 255, 255]] {
            rgba.extend_from_slice(&p);
        }
        state.visual_slots.insert(id, crate::app::VisualSlot {
            generation: 1,
            png: Vec::new(),
            rgba,
            width: 2,
            height: 2,
            title: "flow".to_string(),
            alt: "a diagram".to_string(),
            zoom: 1.0,
            scroll_x: 0,
            scroll_y: 0,
        });
        let view = state.visual_view(id, false);
        assert!(view.title.contains("Visual"), "pane title: {}", view.title);
        let text: String = view.lines.iter().flat_map(|r| r.iter().map(|s| s.text.as_str())).collect();
        assert!(text.contains("flow"), "title line: {text:?}");
        assert!(text.contains("a diagram"), "alt line: {text:?}");
        assert!(text.contains('▀'), "half-block art: {text:?}");
        // Row zero is the zoom strip; title, then art.
        let strip: String = view.lines[0].iter().map(|s| s.text.as_str()).collect();
        assert!(strip.contains("zoom in") && strip.contains("zoom out"), "buttons: {strip:?}");
        assert!(view.lines[1].iter().any(|s| s.text == "flow"), "title row: {text:?}");
        let cell = &view.lines[2][0];
        assert_eq!(cell.style.fg, Some(ratatui::style::Color::Rgb(255, 0, 0)));
        assert_eq!(cell.style.bg, Some(ratatui::style::Color::Rgb(0, 0, 255)));
        // Kitty mode carries title and alt for the record, no art.
        let view = state.visual_view(id, true);
        let text: String = view.lines.iter().flat_map(|r| r.iter().map(|s| s.text.as_str())).collect();
        assert!(text.contains("flow") && text.contains("a diagram"));
        assert!(!text.contains('▀'), "image paints over the pane");
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_tool_round_trip_renders_stores_and_shows() {
        // Acceptance: tool call → background worker → sticky slot →
        // fallback view → one terminal show claim. Bounded wait, same
        // drain the main loop performs.
        let mut state = AppState::new();
        let (id, live_run) = spawn_visual_agent(&mut state, "agent");
        let out = comms_reply(
            &mut state,
            &live_run,
            "visual_show",
            r#"{"content":"flowchart LR\n    A-->B","format":"mermaid","title":"flow","alt":"a to b"}"#,
        );
        assert!(out.contains(r#""accepted":true"#), "queued: {out}");
        let mut stored = None;
        for _ in 0..100 {
            state.drain_visual();
            if let Some(slot) = state.visual_slots.get(&id) {
                stored = Some((slot.generation, slot.width, slot.height));
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let (generation, width, height) =
            stored.expect("worker stored the frame");
        assert_eq!(generation, 1);
        // Content-scaled, font-measured: bounds stay loose on purpose.
        assert!(width > 50 && height > 20, "real raster: {width}x{height}");
        let view = state.visual_view(id, false);
        let text: String = view
            .lines
            .iter()
            .flat_map(|row| row.iter().map(|s| s.text.as_str()))
            .collect();
        assert!(text.contains("flow"), "title: {text:?}");
        assert!(text.contains("a to b"), "alt: {text:?}");
        assert!(text.contains('▀'), "art: {text:?}");
        show_visual_overlay(&mut state, id);
        let paint = paint_for(&state, id);
        let spec = state.visual_take_show(true, paint).expect("show");
        assert_eq!(spec.generation, 1);
        assert!(state.visual_frame_png(id, 1, paint).is_some(), "bytes behind the claim");
    }

    #[test]
    fn walkthrough_tools_drive_tour_lifecycle() {
        let mut state = AppState::new();
        state.term_size = (40, 180);
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        // The fake harness calls with the record's own run ID.
        let live_run = state.manager.get(id).unwrap().run_id.as_str().to_string();
        let path = std::env::temp_dir().join(format!("forge-walk-test-{}", std::process::id()));
        std::fs::write(
            &path,
            (1..=10).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n"),
        ).unwrap();
        let args = format!(
            r#"{{"file":{},"steps":"1:3:first\n5:5:second"}}"#,
            crate::mcp::escape_json(&path.to_string_lossy()),
        );
        let started = comms_reply(&mut state, &live_run, "walkthrough_start", &args);
        assert!(started.contains(r#""ok":true"#), "started: {started}");
        assert!(started.contains(r#""steps":2"#), "started: {started}");
        assert!(state.walkthrough_overlay_active());
        assert_eq!(state.walkthrough_overlay().unwrap().title, path.to_string_lossy());
        // Answering with nothing waiting fails instead of inventing Q&A.
        let early = comms_reply(&mut state, &live_run, "walkthrough_answer", r#"{"answer":"x"}"#);
        assert!(early.contains("no walkthrough question waiting"), "early: {early}");
        // Ask through the overlay: the draft submits, the markup stages
        // an Enter, and the question waits for the agent.
        state.walkthrough_overlay_mut().unwrap().input = Some("why three?".to_string());
        assert!(state.submit_walkthrough_question(id));
        assert!(state.pending_enter.contains_key(&id));
        let tour = state.walkthrough_overlay().unwrap();
        assert_eq!(tour.questions.len(), 1);
        assert_eq!(tour.questions[0].question, "why three?");
        assert!(tour.questions[0].answer.is_none());
        assert!(tour.input.is_none());
        let answered = comms_reply(&mut state, &live_run, "walkthrough_answer", r#"{"answer":"because"}"#);
        assert!(answered.contains(r#""answered":true"#), "answered: {answered}");
        assert_eq!(
            state.walkthrough_overlay().unwrap().questions[0].answer.as_deref(),
            Some("because")
        );
        let ended = comms_reply(&mut state, &live_run, "walkthrough_end", r#"{"summary":"done"}"#);
        assert!(ended.contains(r#""ended":true"#), "ended: {ended}");
        assert!(state.walkthrough_overlay().unwrap().completed);
        std::fs::remove_file(&path).ok();
        assert!(state.manager.remove(id));
    }

    #[test]
    fn walkthrough_start_rejects_bad_calls() {
        let mut state = AppState::new();
        state.term_size = (40, 180);
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        let live_run = state.manager.get(id).unwrap().run_id.as_str().to_string();
        let missing = comms_reply(
            &mut state, &live_run, "walkthrough_start",
            r#"{"file":"/no/such/forge-walk-missing.rs","steps":"1:1:x"}"#,
        );
        assert!(missing.contains(r#""ok":false"#), "missing: {missing}");
        assert!(missing.contains("cannot read"), "missing: {missing}");
        assert!(!state.walkthrough_overlay_active());
        let stale = comms_reply(&mut state, "bogus-run", "walkthrough_answer", r#"{"answer":"x"}"#);
        assert!(stale.contains("unknown or stale run ID"), "stale: {stale}");
        let no_tour = comms_reply(&mut state, &live_run, "walkthrough_end", "{}");
        assert!(no_tour.contains("no walkthrough for this session"), "no_tour: {no_tour}");
        assert!(state.manager.remove(id));
    }

    #[test]
    fn session_status_round_trips_to_sidebar() {
        std::env::set_var("CODEX_BIN", "cat");
        let mut state = AppState::new();
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        std::env::remove_var("CODEX_BIN");
        let live_run = state.manager.get(id).unwrap().run_id.as_str().to_string();
        let set = comms_reply(
            &mut state, &live_run, "set_session_status",
            r#"{"kind":"progress","message":"compiling"}"#,
        );
        assert!(set.contains(r#""ok":true"#), "set: {set}");
        let detail = state.sidebar_info().session.expect("detail renders");
        assert!(detail.status.as_deref() == Some("progress: compiling"), "detail: {detail:?}");
        let lines = crate::ui::sidebar_lines(&state.sidebar_info());
        assert!(lines.iter().any(|l| {
            l.spans.iter().any(|s| s.content.contains("progress: compiling"))
        }), "sidebar shows status");
        let bad_kind = comms_reply(
            &mut state, &live_run, "set_session_status",
            r#"{"kind":"urgent","message":"x"}"#,
        );
        assert!(bad_kind.contains("unknown status kind"), "bad_kind: {bad_kind}");
        let long = comms_reply(
            &mut state, &live_run, "set_session_status",
            &format!(r#"{{"kind":"info","message":"{}"}}"#, "x".repeat(81)),
        );
        assert!(long.contains("over 80"), "long: {long}");
        let cleared = comms_reply(&mut state, &live_run, "clear_session_status", "{}");
        assert!(cleared.contains(r#""status_cleared":true"#), "cleared: {cleared}");
        assert!(state.manager.get(id).unwrap().status.is_none());
        assert!(state.manager.remove(id));
    }

    #[test]
    fn sidebar_shows_timers_only_for_focused_session() {
        std::env::set_var("CODEX_BIN", "cat");
        let mut state = AppState::new();
        let a = state.manager.spawn_agent(
            "a", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        let b = state.manager.spawn_agent(
            "b", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        std::env::remove_var("CODEX_BIN");
        let run_a = state.manager.get(a).unwrap().run_id.as_str().to_string();
        let out = comms_reply(
            &mut state, &run_a, "schedule_prompt",
            r#"{"prompt":"later","delay_seconds":600}"#,
        );
        let timer = crate::policy::json_string_field(out.as_bytes(), &["timer_id"]).unwrap();
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
    fn start_session_validates_creates_and_keeps_focus() {
        std::env::set_var("CODEX_BIN", "cat");
        let mut state = AppState::new();
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        std::env::remove_var("CODEX_BIN");
        let live_run = state.manager.get(id).unwrap().run_id.as_str().to_string();
        for (args, needle) in [
            (r#"{"harness":"nope"}"#, "unknown harness"),
            (r#"{"path":"/no/such/dir"}"#, "not a directory"),
            (r#"{"name":"agent"}"#, "is taken"),
            (r#"{"host":"remote"}"#, "unsupported"),
            (r#"{"internet":true}"#, "unsupported"),
        ] {
            let err = comms_reply(&mut state, &live_run, "start_session", args);
            assert!(err.contains(r#""ok":false"#), "args {args}: {err}");
            assert!(err.contains(needle), "args {args}: {err}");
        }
        std::env::set_var("CODEX_BIN", "cat");
        let started = comms_reply(
            &mut state, &live_run, "start_session",
            r#"{"name":"helper-1","harness":"codex","prompt":"hello"}"#,
        );
        std::env::remove_var("CODEX_BIN");
        assert!(started.contains(r#""ok":true"#), "started: {started}");
        assert!(started.contains(r#""name":"helper-1""#), "started: {started}");
        assert_eq!(state.manager.active(), Some(id), "tool birth keeps focus");
        let new = state.manager.order().iter()
            .find(|&&cand| cand != id).copied().expect("second session exists");
        assert_eq!(state.broker.queued(new), 1, "prompt queued for the birth");
        let due = state.broker.take_due(new, 10);
        assert_eq!(due[0].text, "hello");
        // Default naming plus the caller's group carry over.
        state.broker.join(&state.manager, id, "team").unwrap();
        std::env::set_var("CODEX_BIN", "cat");
        let auto = comms_reply(&mut state, &live_run, "start_session", r#"{"harness":"codex"}"#);
        std::env::remove_var("CODEX_BIN");
        assert!(auto.contains("codex-"), "auto name: {auto}");
        let third = state.manager.order().iter()
            .find(|&&cand| cand != id && cand != new).copied().expect("third exists");
        assert_eq!(state.broker.primary_group(third), Some("team"));
        assert!(state.manager.remove(id));
        assert!(state.manager.remove(new));
        assert!(state.manager.remove(third));
    }

    #[test]
    fn walkthrough_add_and_update_steps() {
        let mut state = AppState::new();
        state.term_size = (40, 180);
        std::env::set_var("CODEX_BIN", "cat");
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        std::env::remove_var("CODEX_BIN");
        let live_run = state.manager.get(id).unwrap().run_id.as_str().to_string();
        let path = std::env::temp_dir().join(format!("forge-walk-steps-{}", std::process::id()));
        std::fs::write(
            &path,
            (1..=10).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n"),
        ).unwrap();
        let start = format!(
            r#"{{"file":{},"steps":"1:3:first\n5:5:second"}}"#,
            crate::mcp::escape_json(&path.to_string_lossy()),
        );
        comms_reply(&mut state, &live_run, "walkthrough_start", &start);
        let added = comms_reply(
            &mut state, &live_run, "walkthrough_add_step",
            r#"{"start_line":7,"end_line":8,"explanation":"new"}"#,
        );
        assert!(added.contains(r#""added":true"#), "added: {added}");
        assert!(added.contains(r#""steps":3"#), "added: {added}");
        let first = comms_reply(
            &mut state, &live_run, "walkthrough_add_step",
            r#"{"start_line":9,"end_line":9,"explanation":"top","position":1}"#,
        );
        assert!(first.contains(r#""steps":4"#), "first: {first}");
        let tour = state.walkthrough_overlay().unwrap();
        assert_eq!(tour.steps[0].explanation, "top");
        assert_eq!(tour.index, 1, "insert before current shifts it");
        let wrong_file = comms_reply(
            &mut state, &live_run, "walkthrough_add_step",
            r#"{"start_line":1,"end_line":1,"explanation":"x","file_path":"other.rs"}"#,
        );
        assert!(wrong_file.contains("open tour file only"), "wrong_file: {wrong_file}");
        let updated = comms_reply(
            &mut state, &live_run, "walkthrough_update",
            r#"{"step_index":1,"explanation":"revised"}"#,
        );
        assert!(updated.contains(r#""updated":true"#), "updated: {updated}");
        assert_eq!(state.walkthrough_overlay().unwrap().steps[0].explanation, "revised");
        for (args, needle) in [
            (r#"{"step_index":0}"#, "starts at 1"),
            (r#"{"step_index":99}"#, "no walkthrough step"),
            (r#"{"step_index":1,"start_line":9,"end_line":2}"#, "bad walkthrough range"),
        ] {
            let err = comms_reply(&mut state, &live_run, "walkthrough_update", args);
            assert!(err.contains(needle), "args {args}: {err}");
        }
        std::fs::remove_file(&path).ok();
        assert!(state.manager.remove(id));
    }

    #[test]
    fn scm_tab_selects_a_live_lazygit_pane() {
        let mut state = AppState::new();
        state.term_size = (40, 180);
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
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
    fn wide_agent_topbar_uses_labeled_icons_from_reference() {
        let mut state = AppState::new();
        state.term_size = (40, 180);
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        let labels: Vec<String> = state.topbar().tabs.iter().map(|tab| tab.label.clone()).collect();
        assert!(labels[0].starts_with("🤖 "));
        assert!(labels[1].starts_with("💻 "));
        assert!(labels[2].starts_with("🔀 "));
        assert!(labels[3].starts_with("🔔 "));
        assert!(labels[4].starts_with("📝 "));
        assert!(labels[5].starts_with("📷 "));
        assert!(state.manager.remove(id));
    }

    #[test]
    fn shrinking_hides_and_closes_extra_view() {
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        assert!(state.select_top_tab(3));
        state.apply(AppEvent::Resize(24, 80));
        assert!(!state.overlay_active());
        assert_eq!(state.topbar().tabs.len(), 3);
        assert!(state.manager.remove(id));
    }

    #[test]
    fn ordinary_wide_terminal_keeps_all_topbar_views_visible() {
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(30, 120));
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            crate::ids::RunId::generate(), "codex",
        ).unwrap();
        assert_eq!(state.topbar().tabs.len(), 7);
        let bar = crate::ui::chrome_areas(ratatui::layout::Rect::new(0, 0, 120, 30)).topbar;
        assert_eq!(crate::ui::layout_topbar(bar, &state.topbar().tabs, false).len(), 7);
        assert!(state.manager.remove(id));
    }

    #[test]
    fn events_tab_does_not_mislabel_tool_calls_as_event_count() {
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let run = crate::ids::RunId::generate();
        let id = state.manager.spawn_agent("agent", &std::env::temp_dir(), "exec cat", run.clone(), "codex").unwrap();
        let (reply, _) = std::sync::mpsc::channel();
        state.apply(AppEvent::HookRequest(crate::listener::HookRequest {
            hook: "PreToolUse".into(), body: "{}".into(), run_id: run.as_str().into(),
            sync: false, reply, timed_out: Default::default(),
        }));
        assert_eq!(state.topbar().tabs[3].label, "🔔 Events");
        assert!(state.manager.remove(id));
    }

    #[test]
    fn step_is_safe_when_empty() {
        let mut s = AppState::new();
        s.step_session(1);
        assert!(s.manager.active().is_none());
    }

    #[test]
    fn manager_spawn_flows_through_state() {
        let mut s = AppState::new();
        let id = s
            .manager
            .spawn("w", &std::env::temp_dir(), "exit 0", RunId::generate(), "shell")
            .unwrap();
        assert!(s.manager.get(id).is_some());
        assert!(s.manager.remove(id));
    }
}
