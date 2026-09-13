//! Agent CLI registry (Phase 5a): the three harnesses forge can launch.
//!
//! Facts are grounded in the installed CLIs, not the blueprint alone:
//! the muse binary is `muse` (blueprint's `metacode` name is stale), and
//! launches carry no forced approval-bypass flags — each CLI keeps its own
//! approval behavior while forge gates through hooks where supported.

/// Agent CLIs forge can launch into session tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Harness {
    Claude,
    Codex,
    Muse,
}

impl Harness {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "claude" => Some(Harness::Claude),
            "codex" => Some(Harness::Codex),
            "muse" => Some(Harness::Muse),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::Muse => "muse",
        }
    }

    pub fn spec(self) -> HarnessSpec {
        match self {
            Harness::Claude => HarnessSpec {
                binary: "claude",
                env_override: "CLAUDE_BIN",
                model_flag: "--model",
            },
            Harness::Codex => HarnessSpec {
                binary: "codex",
                env_override: "CODEX_BIN",
                model_flag: "--model",
            },
            Harness::Muse => HarnessSpec {
                binary: "muse",
                env_override: "METAMATE_BIN",
                model_flag: "--model",
            },
        }
    }

    /// Whether `install-hooks` can do anything. Muse exposes no shell hook
    /// configuration (blueprint §5); its allow policy lives in opencode.json.
    pub fn supports_hooks(self) -> bool {
        !matches!(self, Harness::Muse)
    }
}

/// Launch facts for one harness. No approval-bypass flags are ever added:
/// forge gates through hooks where the CLI supports them.
pub struct HarnessSpec {
    pub binary: &'static str,
    pub env_override: &'static str,
    pub model_flag: &'static str,
}

impl HarnessSpec {
    /// Explicit binary override wins, otherwise the default name resolves
    /// through PATH at spawn. A missing binary surfaces as a spawn error.
    pub fn resolve_binary(&self) -> String {
        std::env::var(self.env_override)
            .ok()
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| self.binary.to_string())
    }

    /// Interactive argv: binary plus an optional model override.
    pub fn launch_argv(&self, binary: &str, model: Option<&str>) -> Vec<String> {
        let mut argv = vec![binary.to_string()];
        if let Some(m) = model.filter(|m| !m.is_empty()) {
            argv.push(self.model_flag.to_string());
            argv.push(m.to_string());
        }
        argv
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_harnesses_have_specs() {
        for name in ["claude", "codex", "muse"] {
            let h = Harness::from_name(name).expect("known harness");
            assert_eq!(h.as_str(), name);
            let spec = h.spec();
            assert!(!spec.binary.is_empty());
            assert!(!spec.env_override.is_empty());
            assert!(!spec.model_flag.is_empty());
        }
        assert!(Harness::from_name("gemini").is_none());
    }

    #[test]
    fn env_override_wins_over_default_binary() {
        std::env::set_var("FORGE_HARNESS_TEST_BIN", "/custom/claude");
        let spec = HarnessSpec {
            binary: "claude",
            env_override: "FORGE_HARNESS_TEST_BIN",
            model_flag: "--model",
        };
        assert_eq!(spec.resolve_binary(), "/custom/claude");
        std::env::remove_var("FORGE_HARNESS_TEST_BIN");
        assert_eq!(spec.resolve_binary(), "claude");
    }

    #[test]
    fn launch_argv_carries_optional_model_and_no_bypass() {
        let spec = Harness::Codex.spec();
        let argv = spec.launch_argv("/usr/bin/codex", Some("gpt-5"));
        assert_eq!(argv, vec!["/usr/bin/codex", "--model", "gpt-5"]);
        let bare = spec.launch_argv("codex", None);
        assert_eq!(bare, vec!["codex"]);
        for arg in argv.iter().chain(bare.iter()) {
            assert!(!arg.contains("yolo"), "no bypass flags: {arg}");
            assert!(!arg.contains("dangerously"), "no bypass flags: {arg}");
        }
    }

    #[test]
    fn hook_support_matrix_matches_blueprint() {
        assert!(Harness::Claude.supports_hooks());
        assert!(Harness::Codex.supports_hooks());
        assert!(!Harness::Muse.supports_hooks());
    }
}
