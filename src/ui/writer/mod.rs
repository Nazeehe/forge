//! Writer overlay paint: L2 layout, empty state, editor, panels.
//!
//! Geometry lives in [`writer_layout`] so the TUI mouse dispatch can
//! hit-test the exact rects the render paints. Every conditional row
//! (prompt, error) owns a fixed slot, so state changes never shove
//! surrounding content around.

pub mod editor;
pub mod empty;
pub mod find;
pub mod layout;
pub mod preview;
pub mod thread;
pub mod toolbar;
#[cfg(test)]
mod tests;

pub use editor::paint;
pub use find::paint_find_bar;
pub use preview::{preview_height, preview_text};
pub use layout::{
    action_pill_rect, chip_detach_rect, confirm_pill_rects, find_bar_rects, more_menu_item_rects,
    more_menu_rect, panel_collapsed, pill_width, prompt_button_rects, prompt_sugg_rect,
    prompt_suggestions, recent_window, start_new_rect, start_open_rect, toolbar_enabled,
    toolbar_narrow, toolbar_pill_rects, writer_layout, ToolbarButton, WriterLayout, EMPTY_INDENT,
    PROMPT_ORIGIN_OFF, RECENT_FIRST_ROW_OFF, DOC_PROMPT_OFF,
};
pub use thread::{panel_rows, panel_skip, PanelClick};
