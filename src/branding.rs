//! Central product/binary/config naming constants.
//!
//! A rename must touch this file only — no scattered literals elsewhere.

use std::path::{Path, PathBuf};

pub fn product_name() -> &'static str {
    "forge"
}

pub fn binary_name() -> &'static str {
    "forge"
}

pub fn config_dir_name() -> &'static str {
    ".forge"
}

pub fn legacy_config_dir_name() -> &'static str {
    ".ccpp"
}

/// `~/.forge`
pub fn config_dir(home: &Path) -> PathBuf {
    home.join(config_dir_name())
}

/// `~/.ccpp` (predecessor, migrated on startup)
pub fn legacy_config_dir(home: &Path) -> PathBuf {
    home.join(legacy_config_dir_name())
}

/// `~/.forge/config.toml`
pub fn config_file(home: &Path) -> PathBuf {
    config_dir(home).join("config.toml")
}

/// `~/.forge/audit.log`
pub fn audit_log(home: &Path) -> PathBuf {
    config_dir(home).join("audit.log")
}

/// `~/.forge/forge.log`
pub fn app_log(home: &Path) -> PathBuf {
    config_dir(home).join("forge.log")
}

/// `~/.forge/agents.json`: agent CLI definitions, materialized from the
/// packaged default on first launch when missing.
pub fn agents_file(home: &Path) -> PathBuf {
    config_dir(home).join("agents.json")
}

/// `~/.forge/AGENTS.md`: agent configuration guide, dropped from the
/// packaged copy on first launch when missing. Never overwritten: local
/// edits survive upgrades.
pub fn guide_file(home: &Path) -> PathBuf {
    config_dir(home).join("AGENTS.md")
}

/// `~/.forge/sessions`: saved session snapshots, newest last.
pub fn sessions_file(home: &Path) -> PathBuf {
    config_dir(home).join("sessions")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_and_binary_names() {
        assert_eq!(product_name(), "forge");
        assert_eq!(binary_name(), "forge");
    }

    #[test]
    fn config_dir_names() {
        assert_eq!(config_dir_name(), ".forge");
        assert_eq!(legacy_config_dir_name(), ".ccpp");
    }

    #[test]
    fn packaged_files_join_home() {
        let home = Path::new("/home/tester");
        assert_eq!(
            agents_file(home),
            PathBuf::from("/home/tester/.forge/agents.json")
        );
        assert_eq!(
            guide_file(home),
            PathBuf::from("/home/tester/.forge/AGENTS.md")
        );
    }

    #[test]
    fn paths_join_home() {
        let home = Path::new("/home/tester");
        assert_eq!(config_dir(home), PathBuf::from("/home/tester/.forge"));
        assert_eq!(
            legacy_config_dir(home),
            PathBuf::from("/home/tester/.ccpp")
        );
        assert_eq!(
            config_file(home),
            PathBuf::from("/home/tester/.forge/config.toml")
        );
        assert_eq!(
            audit_log(home),
            PathBuf::from("/home/tester/.forge/audit.log")
        );
        assert_eq!(
            app_log(home),
            PathBuf::from("/home/tester/.forge/forge.log")
        );
    }
}
