//! AppState visual rendering: paint, zoom, chat geometry, and selection.

use super::super::*;

impl AppState {
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
            selected: slot.selected,
        })
    }

    /// Step one session's visual zoom, re-clamping the scroll offset
    /// to the new overflow. Geometry is the tab image region plus the
    /// cell size the paint uses. Returns whether anything changed.
    #[cfg(feature = "visual")]
    pub fn visual_zoom(
        &mut self,
        id: crate::session::SessionId,
        dir: crate::ui::visual::VisualButton,
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
            crate::ui::visual::VisualButton::ZoomIn => ZoomDir::In,
            crate::ui::visual::VisualButton::ZoomOut => ZoomDir::Out,
            // The chat toggle never reaches zoom: the mouse path
            // routes it to the toggle first. Unreachable by design.
            crate::ui::visual::VisualButton::Chat => return false,
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

    /// Flip one slot's chat footer, dirtying on change. Dismissing
    /// reclaims the footer rows for the diagram; the selection and
    /// history survive underneath until the next selection reopens.
    #[cfg(feature = "visual")]
    pub fn visual_toggle_chat(&mut self, id: crate::session::SessionId) -> bool {
        let Some(slot) = self.visual_slots.get_mut(&id) else {
            return false;
        };
        slot.chat_open = !slot.chat_open;
        // Reopening onto a selection arms the prompt, same as a
        // fresh selection: typing starts immediately.
        if slot.chat_open && slot.selected.is_some() {
            slot.input_active = true;
            slot.draft.get_or_insert_with(String::new);
        }
        self.dirty = true;
        true
    }

    /// Reserved chat footer rows for one slot: 30% of the content
    /// height while open, zero while dismissed. Render and mouse
    /// handling both derive from this, so the footer never desyncs
    /// from the image region.
    #[cfg(feature = "visual")]
    pub fn visual_footer_rows(&self, id: crate::session::SessionId, content_h: u16) -> u16 {
        match self.visual_slots.get(&id) {
            Some(slot) if slot.chat_open => crate::ui::visual::visual_chat_footer_rows(content_h),
            _ => 0,
        }
    }

    /// Content width the chat footer wraps to: the same pane content
    /// rect the chrome derives from, so wrapped rows match the tab.
    #[cfg(feature = "visual")]
    fn visual_footer_width(&self) -> u16 {
        // History wraps to the box interior: content minus borders
        // and side pads, so sided rows stay exactly content-wide.
        let (rows, cols) = self.term_size;
        let areas = crate::ui::layout::chrome_areas(ratatui::layout::Rect::new(0, 0, cols, rows));
        crate::ui::layout::pane_content_area(&areas).width.saturating_sub(6)
    }

    /// All chat history rows for one slot, oldest first: one wrapped
    /// question row plus markdown answer rows (or the waiting marker)
    /// per pair, with one blank row between pairs like the
    /// walkthrough Q&A log. The viewport shows the tail of these.
    #[cfg(feature = "visual")]
    fn visual_chat_history_lines(&self, slot: &VisualSlot) -> Vec<Vec<crate::ui::SpanView>> {
        use crate::ui::theme::{style, Role};
        let width = self.visual_footer_width();
        let text = style(Role::Text);
        let muted = style(Role::Muted);
        let mut rows = Vec::new();
        for (n, q) in slot.questions.iter().enumerate() {
            if n > 0 {
                rows.push(Vec::new());
            }
            rows.extend(crate::ui::text::wrap_spans(
                vec![crate::ui::SpanView {
                    text: format!(
                        "Q ({}): {}",
                        crate::infra::safe_text::encode_for_display(&q.shape_label),
                        crate::infra::safe_text::encode_for_display(&q.question)
                    ),
                    style: text,
                }],
                width,
            ));
            match q.answer.as_deref() {
                Some(a) => {
                    for line in crate::walkthrough::highlight::md_text(a).lines {
                        let spans: Vec<crate::ui::SpanView> = line
                            .spans
                            .iter()
                            .map(|s| crate::ui::SpanView {
                                text: s.content.to_string(),
                                style: s.style,
                            })
                            .collect();
                        if spans.is_empty() {
                            rows.push(Vec::new());
                        } else {
                            rows.extend(crate::ui::text::wrap_spans(spans, width));
                        }
                    }
                }
                None => rows.push(vec![crate::ui::SpanView {
                    text: crate::visual::VISUAL_WAITING_TEXT.to_string(),
                    style: muted,
                }]),
            }
        }
        rows
    }

    /// Scroll one slot's chat history by rows up from the tail
    /// (positive reads back, negative comes forward), clamped to the
    /// rendered history. `visible` is the history viewport rows the
    /// tab currently shows (footer minus the ask row). Returns whether
    /// the offset changed.
    #[cfg(feature = "visual")]
    pub fn visual_chat_scroll(&mut self, id: crate::session::SessionId, delta: i16, visible: u16) -> bool {
        let max = match self.visual_slots.get(&id) {
            Some(slot) => self
                .visual_chat_history_lines(slot)
                .len()
                .saturating_sub(visible as usize) as i16,
            None => return false,
        };
        let Some(slot) = self.visual_slots.get_mut(&id) else {
            return false;
        };
        let next = (slot.chat_scroll as i16 + delta).clamp(0, max.max(0)) as u16;
        if next == slot.chat_scroll {
            return false;
        }
        slot.chat_scroll = next;
        self.dirty = true;
        true
    }

    /// Clear one slot's shape selection, dirtying on change. Margin
    /// clicks share it: empty space always means no selection.
    #[cfg(feature = "visual")]
    fn visual_clear_selection(state: &mut AppState, id: crate::session::SessionId) -> bool {
        let Some(slot) = state.visual_slots.get_mut(&id) else {
            return false;
        };
        if slot.selected.take().is_some() {
            state.dirty = true;
            true
        } else {
            false
        }
    }

    /// Select the shape under an image-region click (`vx`, `vy` in
    /// region cells), zoom- and scroll-aware. `centered` must match
    /// the backend that painted: the Kitty image centers a smaller
    /// diagram in the region (same offset the paint cursor adds)
    /// while the half-block fallback draws from the top-left.
    /// Clicking the selected shape toggles it off; clicking empty
    /// space clears. Returns whether the selection changed.
    #[cfg(feature = "visual")]
    pub fn visual_select_at(
        &mut self,
        id: crate::session::SessionId,
        vx: u16,
        vy: u16,
        area_cols: u16,
        area_rows: u16,
        cell_w: f64,
        cell_h: f64,
        centered: bool,
    ) -> bool {
        use crate::visual::{fit_display, hit_shape, source_to_svg, view_to_source};
        let hit = {
            let Some(slot) = self.visual_slots.get(&id) else {
                return false;
            };
            if slot.shapes.is_empty() {
                return false;
            }
            let (disp_cols, disp_rows) = fit_display(
                slot.width,
                slot.height,
                slot.zoom,
                area_cols,
                area_rows,
                cell_w,
                cell_h,
            );
            // Mirror crop_for_view's bounds, then drop the centering
            // offset the paint cursor adds on the Kitty path. Clicks
            // landing in the margin resolve to no shape (clear).
            let out_cols = disp_cols.saturating_sub(slot.scroll_x).min(area_cols).max(1);
            let out_rows = disp_rows.saturating_sub(slot.scroll_y).min(area_rows).max(1);
            let (off_x, off_y) = if centered {
                (
                    area_cols.saturating_sub(out_cols) / 2,
                    area_rows.saturating_sub(out_rows) / 2,
                )
            } else {
                (0, 0)
            };
            let inside = match (vx.checked_sub(off_x), vy.checked_sub(off_y)) {
                (Some(rx), Some(ry)) if rx < out_cols && ry < out_rows => Some((rx, ry)),
                _ => None,
            };
            let Some((rx, ry)) = inside else {
                return Self::visual_clear_selection(self, id);
            };
            let (px, py) = view_to_source(
                rx,
                ry,
                slot.scroll_x,
                slot.scroll_y,
                disp_cols,
                disp_rows,
                slot.width,
                slot.height,
            );
            let (sx, sy) =
                source_to_svg(px, py, slot.width, slot.height, &slot.vb);
            hit_shape(&slot.shapes, sx, sy)
        };
        let Some(slot) = self.visual_slots.get_mut(&id) else {
            return false;
        };
        let next = match (slot.selected, hit) {
            (Some(a), Some(b)) if a == b => None,
            _ => hit,
        };
        if next == slot.selected {
            return false;
        }
        slot.selected = next;
        // A fresh selection opens the dismissed chat and arms the
        // prompt at once (the draft survives): typing starts
        // immediately, no Enter needed. Deselecting disarms.
        // Toggling off keeps both.
        slot.input_active = next.is_some();
        if slot.input_active {
            slot.draft.get_or_insert_with(String::new);
        }
        if next.is_some() {
            slot.chat_open = true;
        }
        self.dirty = true;
        true
    }

    /// The focused Visual tab's (session, generation), if the overlay
    /// sits on a visual slot with a stored frame.
    #[cfg(feature = "visual")]
    pub fn visual_focused_frame(&self) -> Option<(crate::session::SessionId, u64)> {
        self.visual_overlay_active()
    }

    /// Paint the terminal already shows, if the overlay still wants
    /// exactly it. Lets the TUI skip all byte work for an unchanged
    /// frame: byte building (clone or crop+encode) happens only when
    /// this differs from the fresh paint.
    #[cfg(feature = "visual")]
    pub fn visual_current_paint(&self) -> Option<crate::visual::VisualPaint> {
        let shown = self.visual_shown.as_ref()?;
        let (session, generation) = self.visual_overlay_active()?;
        (shown.session == session && shown.generation == generation).then_some(shown.paint)
    }

    /// PNG bytes for one paint of a stored frame: the frame itself
    /// when the viewport shows it whole (no re-encode), else the
    /// cropped region re-encoded for transmit. A selection always
    /// re-encodes so the highlight border bakes in. Independent of
    /// the show gate, so the TUI only claims a transmit it can render.
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
        let selected = slot.selected.and_then(|i| slot.shapes.get(i));
        if selected.is_none()
            && paint.sx == 0
            && paint.sy == 0
            && paint.sw == slot.width
            && paint.sh == slot.height
        {
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
        let mut cut = crate::visual::crop_rgba(&slot.rgba, slot.width, slot.height, crop);
        if let Some(shape) = selected {
            // Shape box (SVG units) to source pixels, clipped to the
            // crop so off-view shapes draw nothing.
            let (ex, ey) = (
                paint.sx.saturating_add(paint.sw),
                paint.sy.saturating_add(paint.sh),
            );
            let (bx0, by0) = crate::visual::svg_to_source(
                shape.x,
                shape.y,
                slot.width,
                slot.height,
                &slot.vb,
            );
            let (bx1, by1) = crate::visual::svg_to_source(
                shape.x + shape.width,
                shape.y + shape.height,
                slot.width,
                slot.height,
                &slot.vb,
            );
            let x0 = bx0.clamp(paint.sx, ex);
            let x1 = bx1.clamp(paint.sx, ex);
            let y0 = by0.clamp(paint.sy, ey);
            let y1 = by1.clamp(paint.sy, ey);
            if x1 > x0 && y1 > y0 {
                crate::visual::stroke_rect(
                    &mut cut,
                    paint.sw,
                    paint.sh,
                    x0 - paint.sx,
                    y0 - paint.sy,
                    x1 - paint.sx,
                    y1 - paint.sy,
                    crate::visual::SELECT_RGB,
                    crate::visual::SELECT_BORDER_PX,
                );
            }
        }
        crate::visual::encode_png(&cut, paint.sw, paint.sh).ok()
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
        let text = crate::ui::theme::style(crate::ui::theme::Role::Text);
        let muted = crate::ui::theme::style(crate::ui::theme::Role::Muted);
        let line = |content: &str, style: Style| {
            vec![crate::ui::SpanView {
                text: content.to_string(),
                style,
            }]
        };
        let Some(slot) = self.visual_slots.get(&id) else {
            return crate::ui::PaneView {
                title,
                lines: vec![
                    line("Visual renders diagrams to visualize code flow.", text),
                    line(
                        "Ask this session, e.g. \"visualize the flow for <...>\"",
                        muted,
                    ),
                ],
                live,
                focused: true,
                cursor: None,
            };
        };
        let mut lines = Vec::new();
        // Row zero is always blank: breathing room between the tab
        // strip and the zoom strip, which rides row one with the
        // title. Both backends share this: the Kitty image paints only
        // the region below the strip, and the mouse hit test assumes
        // these exact rows and columns.
        lines.push(Vec::new());
        // One chrome for both backends: the strip, image, and footer
        // rows below must match the rects the mouse path hit-tests,
        // or clicks and wheel routing desync from the paint.
        let (rows, cols) = self.term_size;
        let areas = crate::ui::layout::chrome_areas(ratatui::layout::Rect::new(0, 0, cols, rows));
        let content = crate::ui::layout::pane_content_area(&areas);
        let foot_rows = self.visual_footer_rows(id, content.height);
        let chrome = crate::ui::visual::visual_chrome(content, self.pill_tabs, foot_rows);
        let mut strip = crate::ui::visual::visual_button_spans(self.pill_tabs, slot.chat_open);
        strip.push(crate::ui::SpanView {
            text: "  ←→↑↓ scroll · wheel scrolls".to_string(),
            style: muted,
        });
        // The title rides the strip row: the fitted image fills every
        // row below it, so a title line there would be painted over.
        // A selection appends its shape label the same way. In Kitty
        // mode the alt text rides here too, shortened to fit: its own
        // line would hide under the transmitted image.
        if !slot.title.is_empty() {
            strip.push(crate::ui::SpanView {
                text: format!("  ·  {}", slot.title),
                style: text,
            });
        }
        if let Some(shape) = slot.selected.and_then(|i| slot.shapes.get(i)) {
            strip.push(crate::ui::SpanView {
                text: format!(
                    "  ▸  {}",
                    crate::infra::safe_text::encode_for_display(&shape.label)
                ),
                style: text,
            });
        }
        if kitty && !slot.alt.is_empty() {
            // Shortened: the strip is chrome, so the description
            // arrives capped with an ellipsis, never a sentence.
            let alt = vec![crate::ui::SpanView {
                text: format!(
                    "  ·  {}",
                    crate::infra::safe_text::encode_for_display(&slot.alt)
                ),
                style: muted,
            }];
            strip.extend(crate::ui::text::truncate_spans(
                alt,
                crate::ui::visual::VISUAL_STRIP_ALT_MAX,
            ));
        }
        lines.push(strip);
        if !kitty {
            // Half-block cells are exactly 1:2 by construction, so the
            // fallback layout uses the fixed 8x16 cell, never the
            // terminal pixel report. Art covers the visible crop only,
            // capped at the tab image region like the Kitty paint.
            let image = chrome.image;
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
                let selected = slot.selected.and_then(|i| slot.shapes.get(i));
                let cols = (paint.sw as usize).min(paint.out_cols.max(1) as usize).max(1);
                for (r, row) in crate::visual::halfblock_rows(
                    &cut,
                    paint.sw,
                    paint.sh,
                    paint.out_cols.max(1) as usize,
                )
                .into_iter()
                .enumerate()
                {
                    lines.push(
                        row.into_iter()
                            .enumerate()
                            .map(|(dx, cell)| {
                                let mut style = Style::default()
                                    .fg(Color::Rgb(cell.fg.0, cell.fg.1, cell.fg.2))
                                    .bg(Color::Rgb(cell.bg.0, cell.bg.1, cell.bg.2));
                                // Reverse cells inside the selected shape:
                                // the Kitty backend cannot style cells,
                                // so it bakes a border into the bytes
                                // instead (see visual_frame_png).
                                if let Some(shape) = selected {
                                    let px = paint.sx.saturating_add(
                                        (dx as u32).saturating_mul(paint.sw) / cols.max(1) as u32,
                                    );
                                    let py = paint.sy.saturating_add((r as u32).saturating_mul(2));
                                    let (sx, sy) = crate::visual::source_to_svg(
                                        px,
                                        py,
                                        slot.width,
                                        slot.height,
                                        &slot.vb,
                                    );
                                    if crate::visual::shape_contains(shape, sx, sy) {
                                        style = style.add_modifier(
                                            ratatui::style::Modifier::REVERSED,
                                        );
                                    }
                                }
                                crate::ui::SpanView {
                                    text: cell.ch.to_string(),
                                    style,
                                }
                            })
                            .collect(),
                    );
                }
            }
        }
        // Fallback only: in Kitty mode the alt text rides the strip
        // row, since this line would hide under the image.
        if !kitty && !slot.alt.is_empty() {
            lines.push(line(&slot.alt, muted));
        }
        // Dismissable Q/A footer: a bordered box while open, none
        // while dismissed. Blank filler first: the Kitty backend
        // emits no art and small diagrams leave empty image rows, so
        // without it the box would paint under the strip (beneath
        // the image in Kitty mode) while the click map and wheel
        // routing use the chrome footer pinned to the bottom.
        if slot.chat_open {
            let used = lines.len().saturating_sub(2) as u16;
            for _ in used..chrome.image.height {
                lines.push(Vec::new());
            }
            // Box metrics: full content width, two-cell side pads, one
            // pad row under the title; the history viewport is what
            // remains (see visual_chat_history_rows). Claude-style
            // order: history on top, a divider, then the prompt row
            // docked above the bottom border. Every emitted row is
            // exactly content-wide so the sides align.
            let width = content.width as usize;
            let inner = width.saturating_sub(6);
            let side = |pad: &str| crate::ui::SpanView {
                text: pad.to_string(),
                style: muted,
            };
            let fill_content = |mut row: Vec<crate::ui::SpanView>| {
                let w = crate::ui::text::spans_width(&row);
                let style = row.last().map(|s| s.style).unwrap_or(muted);
                row.push(crate::ui::SpanView {
                    text: " ".repeat(inner.saturating_sub(w)),
                    style,
                });
                row
            };
            let foot_start = lines.len();
            let mut top = String::from("╭─ Q/A ");
            top.push_str(&"─".repeat(width.saturating_sub(8)));
            top.push('╮');
            lines.push(line(&top, muted));
            lines.push(vec![
                side("│"),
                side(&" ".repeat(width.saturating_sub(2))),
                side("│"),
            ]);
            let history = self.visual_chat_history_lines(slot);
            let tail = history.len().saturating_sub(slot.chat_scroll as usize);
            let start = tail.saturating_sub(
                crate::ui::visual::visual_chat_history_rows(foot_rows) as usize,
            );
            for row in history[start..tail.min(history.len())].iter().cloned() {
                let mut history_line = vec![side("│  ")];
                history_line.extend(fill_content(row));
                history_line.push(side("  │"));
                lines.push(history_line);
            }
            while lines.len() - foot_start < foot_rows.saturating_sub(3) as usize {
                let mut blank = vec![side("│  ")];
                blank.extend(fill_content(Vec::new()));
                blank.push(side("  │"));
                lines.push(blank);
            }
            let mut divider = String::from("├");
            divider.push_str(&"─".repeat(width.saturating_sub(2)));
            divider.push('┤');
            lines.push(line(&divider, muted));
            // Docked prompt row: `>` plus a gray hint until the first
            // keystroke swaps it for the draft. Typing is armed by
            // selection itself, so Enter is only ever submit.
            let input_spans: Vec<crate::ui::SpanView> = match (slot.selected.is_some(), slot.draft.as_deref()) {
                (false, _) => vec![
                    crate::ui::SpanView { text: "> ".to_string(), style: text },
                    crate::ui::SpanView {
                        text: "Click a shape to ask · c toggles chat".to_string(),
                        style: muted,
                    },
                ],
                (true, Some(d)) if !d.is_empty() => {
                    let cursor = if slot.input_active { "▌" } else { "" };
                    vec![
                        crate::ui::SpanView { text: "> ".to_string(), style: text },
                        crate::ui::SpanView {
                            text: format!("{}{}", crate::infra::safe_text::encode_for_display(d), cursor),
                            style: text,
                        },
                    ]
                }
                _ => vec![
                    crate::ui::SpanView { text: "> ".to_string(), style: text },
                    crate::ui::SpanView { text: "type question here".to_string(), style: muted },
                ],
            };
            let mut input_line = vec![side("│  ")];
            input_line.extend(fill_content(crate::ui::text::truncate_spans(
                input_spans,
                inner as u16,
            )));
            input_line.push(side("  │"));
            lines.push(input_line);
            let mut bottom = String::from("╰");
            bottom.push_str(&"─".repeat(width.saturating_sub(2)));
            bottom.push('╯');
            lines.push(line(&bottom, muted));
        }
        crate::ui::PaneView {
            title,
            lines,
            live,
            focused: true,
            cursor: None,
        }
    }
}

#[cfg(test)]
mod tests;
