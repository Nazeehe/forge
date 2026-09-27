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

/// Process home for Forge-owned state. `$HOME` when set, the current
/// directory otherwise (mirrors the headless startup fallback).
pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
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

/// `~/.forge/sessions.json`: saved session snapshots, newest last.
pub fn sessions_file(home: &Path) -> PathBuf {
    config_dir(home).join("sessions.json")
}

/// `~/.forge/hooks.log`: bounded hook delivery/attribution trace.
pub fn hooks_log(home: &Path) -> PathBuf {
    config_dir(home).join("hooks.log")
}

/// Move the pre-`.json` `~/.forge/sessions` into place. Never clobbers an
/// existing `sessions.json`; failures leave both files as they were.
pub fn migrate_sessions_file(home: &Path) {
    let legacy = config_dir(home).join("sessions");
    let current = sessions_file(home);
    if legacy.is_file() && !current.exists() {
        let _ = std::fs::rename(&legacy, &current);
    }
}

/// `~/.forge/themes`: external `theme.json` files (one `*.json` per
/// theme, or `*/theme.json` per theme directory).
pub fn themes_dir(home: &Path) -> PathBuf {
    config_dir(home).join("themes")
}

/// `~/.forge/kanban.json`: workspace kanban boards, atomically written.
pub fn kanban_file(home: &Path) -> PathBuf {
    config_dir(home).join("kanban.json")
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
    fn themes_dir_joins_home() {
        let home = Path::new("/home/tester");
        assert_eq!(
            themes_dir(home),
            PathBuf::from("/home/tester/.forge/themes")
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
        assert_eq!(
            kanban_file(home),
            PathBuf::from("/home/tester/.forge/kanban.json")
        );
        assert_eq!(
            sessions_file(home),
            PathBuf::from("/home/tester/.forge/sessions.json")
        );
        assert_eq!(
            hooks_log(home),
            PathBuf::from("/home/tester/.forge/hooks.log")
        );
    }

    #[test]
    fn extensionless_sessions_file_moves_to_json() {
        let home = std::env::temp_dir().join(format!(
            "forge-branding-sessions-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(config_dir(&home)).unwrap();
        let legacy = config_dir(&home).join("sessions");
        std::fs::write(&legacy, b"old").unwrap();
        migrate_sessions_file(&home);
        assert!(!legacy.exists(), "old name moved away");
        assert_eq!(std::fs::read(sessions_file(&home)).unwrap(), b"old");
        // An existing new file is never clobbered by a stale old one.
        std::fs::write(&legacy, b"stale").unwrap();
        migrate_sessions_file(&home);
        assert_eq!(std::fs::read(sessions_file(&home)).unwrap(), b"old");
        assert!(legacy.exists(), "old file left intact on collision");
        let _ = std::fs::remove_dir_all(&home);
    }
}
