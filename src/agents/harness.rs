//! Agent CLI handles: indexes into the process-wide agent registry
//! (`agents.json`, see [`crate::agents::registry`]). The registry is set once at
//! startup and never replaced, so indexes are stable and the handle
//! stays `Copy` for [`crate::ui::dialogs::create::SessionKind`].
//!
//! Launch facts used to live here as a three-variant enum; they now
//! come from the registry file, so adding an agent is a config edit.
//! Launches carry no forced approval-bypass flags — each CLI keeps its
//! own approval behavior while forge gates through hooks where
//! supported, and the registry parser rejects bypasses in agent argv.

use crate::agents::registry::{AgentDef, Attribution, WithId};

/// Agent CLI behind an agent tab: an index into [`crate::agents::registry`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Harness {
    index: usize,
}

impl Harness {
    /// Every registered agent, in file order (drives the create-dialog
    /// tool radio, so file order is display order).
    pub fn all() -> Vec<Harness> {
        (0..crate::agents::registry::registry().len())
            .map(|index| Harness { index })
            .collect()
    }

    pub fn len() -> usize {
        crate::agents::registry::registry().len()
    }

    /// Saturating index: the create radio always picks a live agent,
    /// and the registry is never empty (startup rejects empty files).
    pub fn from_index(index: usize) -> Harness {
        let max = crate::agents::registry::registry().len().saturating_sub(1);
        Harness { index: index.min(max) }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        crate::agents::registry::registry()
            .iter()
            .position(|def| def.name == name)
            .map(|index| Harness { index })
    }

    fn def(self) -> &'static AgentDef {
        &crate::agents::registry::registry()[self.index]
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
            default_model: &def.default_model,
            extra_args: &def.extra_args,
        }
    }

    pub fn supports_hooks(self) -> bool {
        self.def().supports_hooks
    }

    /// Per-agent behavior adapter for this harness (see
    /// [`crate::agents::adapter_for`]).
    pub fn adapter(self) -> &'static dyn crate::agents::AgentAdapter {
        crate::agents::adapter_for(self.as_str())
    }

    /// Provider-specific runtime adapter: how the canonical Forge runtime
    /// contract reaches this agent at launch (see [`crate::agents::runtime`]).
    pub fn runtime_adapter(self) -> crate::agents::runtime::RuntimeAdapter {
        crate::agents::runtime::RuntimeAdapter::for_agent(self.as_str())
    }

    /// Interactive argv plus the runtime-contract injection for a fresh
    /// launch: binary, model, registry extra args, then the adapter extras.
    /// Resume argv intentionally skips this: resumed sessions keep their
    /// recorded instructions (Claude snapshots the system prompt; Codex
    /// keeps thread developer instructions).
    pub fn launch_argv_with_runtime(
        self,
        binary: &str,
        model: Option<&str>,
        runtime_file: &std::path::Path,
    ) -> Vec<String> {
        let mut argv = self.spec().launch_argv(binary, model);
        argv.extend(self.adapter().runtime_injection(runtime_file));
        argv
    }

    /// Whether hook reports need scrubbed-environment attribution: PTY
    /// process-session ID first, cwd+bootstrap-window for old relay records.
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
    pub default_model: &'static str,
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

    /// Interactive argv: binary, the model (explicit, else the
    /// registry default, else nothing), then the agent's extra args.
    pub fn launch_argv(&self, binary: &str, model: Option<&str>) -> Vec<String> {
        let mut argv = vec![binary.to_string()];
        let model = model
            .filter(|m| !m.is_empty())
            .or_else(|| (!self.default_model.is_empty()).then_some(self.default_model));
        if let Some(m) = model {
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
        assert_eq!(names, ["claude", "codex", "muse", "agy"]);
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
            default_model: "",
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
        let agy = Harness::from_name("agy").expect("packaged agy");
        assert_eq!(
            agy.resume_argv("agy", Some("conv-1")),
            vec!["agy", "--conversation", "conv-1"]
        );
        assert_eq!(
            agy.resume_argv("agy", None),
            vec!["agy", "--continue"]
        );
    }

    #[test]
    fn launch_argv_with_runtime_appends_injection_after_model() {
        let file = std::path::Path::new("/home/tester/.forge/runtime.md");
        let claude = Harness::from_name("claude").expect("packaged claude");
        let argv = claude.launch_argv_with_runtime("claude", None, file);
        assert_eq!(argv[0], "claude");
        let tail = &argv[argv.len() - 2..];
        assert_eq!(
            tail,
            [
                "--append-system-prompt-file".to_string(),
                "/home/tester/.forge/runtime.md".to_string(),
            ]
        );
        // Injection never smuggles approval bypasses.
        for arg in &argv {
            assert!(!arg.contains("yolo"), "no bypass flags: {arg}");
            assert!(!arg.contains("dangerously"), "no bypass flags: {arg}");
        }
        // Codex carries developer instructions; muse-family agents carry a
        // startup prompt; every packaged agent injects something.
        let codex = Harness::from_name("codex").expect("packaged codex");
        let codex_argv = codex.launch_argv_with_runtime("codex", None, file);
        assert!(codex_argv.contains(&"-c".to_string()));
        for name in ["muse", "agy"] {
            let h = Harness::from_name(name).expect("packaged agent");
            let a = h.launch_argv_with_runtime(name, None, file);
            assert!(
                a.len() > 1 && a.last().unwrap().contains("Forge"),
                "{name} must carry the runtime contract: {a:?}"
            );
        }
    }

    #[test]
    fn launch_argv_falls_back_to_registry_default_model() {
        let spec = HarnessSpec {
            binary: "gem",
            env_override: "GEM_BIN",
            model_flag: "--model",
            default_model: "auto",
            extra_args: &[],
        };
        assert_eq!(
            spec.launch_argv("gem", None),
            vec!["gem", "--model", "auto"]
        );
        assert_eq!(
            spec.launch_argv("gem", Some("pro")),
            vec!["gem", "--model", "pro"],
            "explicit model wins"
        );
        assert_eq!(
            spec.launch_argv("gem", Some("")),
            vec!["gem", "--model", "auto"],
            "empty model falls back"
        );
    }

    #[test]
    fn hook_support_and_attribution_match_packaged_default() {
        for h in Harness::all() {
            assert_eq!(h.supports_hooks(), h.as_str() != "agy");
        }
        let muse = Harness::from_name("muse").expect("packaged muse");
        assert!(muse.cwd_window_attribution());
        let claude = Harness::from_name("claude").expect("packaged claude");
        assert!(!claude.cwd_window_attribution());
    }
}
