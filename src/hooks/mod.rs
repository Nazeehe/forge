//! Hooks: relay, permission policy, audit, and harness installers.
//!
//! The fail-open stdin relay, the deterministic permission policy, the
//! append-only audit log, and the per-harness install/uninstall fan-out.

pub mod audit;
pub mod install;
pub mod policy;
pub mod relay;
