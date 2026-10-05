//! TUI Writer input: key routing for the Writer overlay tab.
//!
//! While the Writer overlay is focused, keys belong to the document,
//! not the agent pane — except the `Ctrl-b` prefix chord, which always
//! escapes to the router (the routing guard guarantees that before
//! this runs). The empty state, chat box, and thread keys arrive in
//! later slices; this slice routes editor keys to the adapter.


pub mod keys;
pub mod mouse;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

pub(crate) use keys::handle_writer_key;
pub(crate) use mouse::handle_writer_mouse;
