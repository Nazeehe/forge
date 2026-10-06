//! Dependency-free infrastructure everything else builds on.
//!
//! Branding, identities, the cross-thread event protocol, atomic file
//! writes, path confinement, display encoding, logging, and configuration.

pub mod branding;
pub mod config;
pub mod event;
pub mod fs_atomic;
pub mod ids;
pub mod logging;
pub mod paths;
pub mod safe_text;
#[cfg(test)]
pub(crate) mod test_timing;
