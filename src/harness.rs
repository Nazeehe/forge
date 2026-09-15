//! Agent CLI handles: indexes into the process-wide agent registry
//! (`agents.json`, see [`crate::agents`]). The registry is set once at
//! startup and never replaced, so indexes are stable and the handle
//! stays `Copy` for [`crate::create::SessionKind`].
//!
//! Launch facts used to live here as a three-variant enum; they now
//! come from the registry file, so adding an agent is a config edit.
//! Launches carry no forced approval-bypass flags — each CLI keeps its
//! own approval behavior while forge gates through hooks where
//! supported, and the registry parser rejects bypasses in agent argv.

use crate::agents::{AgentDef, Attribution, WithId};

/// Agent CLI behind an agent tab: an index into [`crate::agents::registry`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Harness {
    index: usize,
}

impl Harness {
    /// Every registered agent, in file order (drives the create-dialog
    /// tool radio, so file order is display order).
    pub fn all() -> Vec<Harness> {
        (0..crate::agents::registry().len())
            .map(|index| Harness { index })
            .collect()
    }

    pub fn len() -> usize {
        crate::agents::registry().len()
    }

    /// Saturating index: the create radio always picks a live agent,
    /// and the registry is never empty (startup rejects empty files).
    pub fn from_index(index: usize) -> Harness {
        let max = crate::agents::registry().len().saturating_sub(1);
        Harness { index: index.min(max) }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        crate::agents::registry()
            .iter()
            .position(|def| def.name == name)
            .map(|index| Harness { index })
    }

    fn def(self) -> &'static AgentDef {
        &crate::agents::registry()[self.index]
    }

    pub fn as_str(self) -> &'static str {
        &self.def().name
    }

    pub fn spec(self) -> HarnessSpec {
        let def = self.def();
        HarnessSpec {
            binary: &def.binary,
            env_override: &def.env_override,
            model_flag: &def.model_flag,
            extra_args: &def.extra_args,
        }
    }

    pub fn supports_hooks(self) -> bool {
        self.def().supports_hooks
    }

    /// Whether hook SessionStart reports need the cwd+bootstrap-window
    /// fallback (the CLI scrubs the hook environment).
    pub fn cwd_window_attribution(self) -> bool {
        self.def().attribution == Attribution::CwdWindow
    }

    /// Resume argv for a saved harness conversation. No model override
    /// rides along: a resumed session keeps the model it already had.
    /// The no-ID fallback is whatever the agent declares (cwd-scoped
    /// `--continue` for some, global `--last` for others). Extra args
    /// always ride at the tail.
    pub fn resume_argv(self, binary: &str, harness_session_id: Option<&str>) -> Vec<String> {
        let def = self.def();
        let mut argv = vec![binary.to_string()];
        if let Some(sub) = def.resume.subcommand.as_deref() {
            argv.push(sub.to_string());
        }
        match harness_session_id.filter(|s| !s.is_empty()) {
            Some(id) => match &def.resume.with_id {
                WithId::Flag(flag) => {
                    argv.push(flag.clone());
                    argv.push(id.to_string());
                }
                WithId::Positional => argv.push(id.to_string()),
            },
            None => argv.extend(def.resume.without_id.iter().cloned()),
        }
        argv.extend(def.extra_args.iter().cloned());
        argv
    }
}

/// Launch facts for one harness. No approval-bypass flags are ever added:
/// forge gates through hooks where the CLI supports them.
pub struct HarnessSpec {
    pub binary: &'static str,
    pub env_override: &'static str,
    pub model_flag: &'static str,
    pub extra_args: &'static [String],
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

    /// Interactive argv: binary, an optional model override, then the
    /// agent's extra args.
    pub fn launch_argv(&self, binary: &str, model: Option<&str>) -> Vec<String> {
        let mut argv = vec![binary.to_string()];
        if let Some(m) = model.filter(|m| !m.is_empty()) {
            argv.push(self.model_flag.to_string());
            argv.push(m.to_string());
        }
        argv.extend(self.extra_args.iter().cloned());
        argv
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_names_match_packaged_default_order() {
        let names: Vec<_> = Harness::all().iter().map(|h| h.as_str()).collect();
        assert_eq!(names, ["claude", "codex", "muse"]);
        assert!(Harness::from_name("gemini").is_none());
    }

    #[test]
    fn all_harnesses_have_specs() {
        for h in Harness::all() {
            let spec = h.spec();
            assert!(!spec.binary.is_empty());
            assert!(!spec.model_flag.is_empty());
        }
    }

    #[test]
    fn env_override_wins_over_default_binary() {
        std::env::set_var("FORGE_HARNESS_TEST_BIN", "/custom/claude");
        let spec = HarnessSpec {
            binary: "claude",
            env_override: "FORGE_HARNESS_TEST_BIN",
            model_flag: "--model",
            extra_args: &[],
        };
        assert_eq!(spec.resolve_binary(), "/custom/claude");
        std::env::remove_var("FORGE_HARNESS_TEST_BIN");
        assert_eq!(spec.resolve_binary(), "claude");
    }

    #[test]
    fn launch_argv_carries_optional_model_extras_and_no_bypass() {
        let h = Harness::from_name("codex").expect("packaged codex");
        let argv = h.spec().launch_argv("/usr/bin/codex", Some("gpt-5"));
        assert_eq!(argv, vec!["/usr/bin/codex", "--model", "gpt-5"]);
        let bare = h.spec().launch_argv("codex", None);
        assert_eq!(bare, vec!["codex"]);
        for arg in argv.iter().chain(bare.iter()) {
            assert!(!arg.contains("yolo"), "no bypass flags: {arg}");
            assert!(!arg.contains("dangerously"), "no bypass flags: {arg}");
        }
    }

    #[test]
    fn resume_argv_pins_registry_shapes() {
        let claude = Harness::from_name("claude").expect("packaged claude");
        assert_eq!(
            claude.resume_argv("claude", Some("abc-123")),
            vec!["claude", "--resume", "abc-123"]
        );
        assert_eq!(
            claude.resume_argv("claude", None),
            vec!["claude", "--continue"]
        );
        assert_eq!(
            claude.resume_argv("claude", Some("")),
            vec!["claude", "--continue"],
            "empty ID falls back"
        );
        let codex = Harness::from_name("codex").expect("packaged codex");
        assert_eq!(
            codex.resume_argv("/bin/codex", Some("uuid-1")),
            vec!["/bin/codex", "resume", "uuid-1"]
        );
        assert_eq!(
            codex.resume_argv("codex", None),
            vec!["codex", "resume", "--last"]
        );
        let muse = Harness::from_name("muse").expect("packaged muse");
        assert_eq!(
            muse.resume_argv("muse", Some("uuid-9")),
            vec!["muse", "resume", "uuid-9"]
        );
        assert_eq!(
            muse.resume_argv("muse", None),
            vec!["muse", "resume", "--last"]
        );
    }

    #[test]
    fn hook_support_and_attribution_match_packaged_default() {
        for h in Harness::all() {
            assert!(h.supports_hooks());
        }
        let muse = Harness::from_name("muse").expect("packaged muse");
        assert!(muse.cwd_window_attribution());
        let claude = Harness::from_name("claude").expect("packaged claude");
        assert!(!claude.cwd_window_attribution());
    }
}
