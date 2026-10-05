//! Writer overlay paint: L2 layout, empty state, editor, panels.
//!
//! Geometry lives in [`writer_layout`] so the TUI mouse dispatch can
//! hit-test the exact rects the render paints. Every conditional row
//! (prompt, error) owns a fixed slot, so state changes never shove
//! surrounding content around.

pub mod editor;
pub mod layout;
pub mod thread;
#[cfg(test)]
mod tests;

pub use editor::paint;
pub use layout::{action_pill_rects, chip_detach_rect, empty_pill_rects, panel_collapsed, pill_width, writer_layout, WriterLayout};
pub use thread::{panel_rows, panel_skip, PanelClick};
