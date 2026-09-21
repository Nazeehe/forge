// Temporary bring-up cover: most modules carry scaffold APIs that future
// phases wire up (PTY spawn, path jail, hooks, walkthrough, ...). Silence
// their dead-code noise so NEW warnings stay visible; remove this line as
// phases land and re-address whatever it unhides.
#![allow(dead_code)]

mod agents;
mod app;
mod audit;
mod bot;
mod branding;
mod comms;
mod config;
mod create;
mod checkpoint;
mod event;
mod fs_atomic;
mod groups;
mod harness;
mod ids;
mod input;
mod install;
mod listener;
mod logging;
mod mcp;
mod paths;
mod policy;
mod pty;
mod quit;
mod relay;
mod safe_text;
mod telegram;
mod telegram_dialog;
mod theme_dialog;
#[cfg(all(target_os = "linux", feature = "visual"))]
mod screenshot;
mod session;
mod session_status;
mod theme;
mod tui;
mod ui;
#[cfg(feature = "visual")]
mod visual;
mod walkthrough;
mod whichkey;

const VERSION: &str = "1.0.0";

/// Packaged `AGENTS.md`, dropped into `~/.forge` when missing.
const DEFAULT_AGENTS_MD: &str = include_str!("../assets/AGENTS.md");

/// Packaged example themes, seeded into `~/.forge/themes/` on first
/// launch when that directory holds no themes yet.
const EXAMPLE_THEMES: &[(&str, &str)] = &[
    ("square.json", include_str!("../assets/themes/square.json")),
    ("round.json", include_str!("../assets/themes/round.json")),
    (
        "tokyo-night.json",
        include_str!("../assets/themes/tokyo-night.json"),
    ),
    ("triangle.json", include_str!("../assets/themes/triangle.json")),
    ("wedge.json", include_str!("../assets/themes/wedge.json")),
];

/// Create `~/.forge/themes/` and top up any missing packaged
/// examples, so new examples arrive on upgrade. Present files are
/// never overwritten, so user edits survive. Failures warn instead of
/// blocking startup — themes are cosmetic, unlike `agents.json`.
fn seed_example_themes(home: &std::path::Path) {
    let dir = branding::themes_dir(home);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    for (name, contents) in EXAMPLE_THEMES {
        let path = dir.join(name);
        if !path.exists() {
            if let Err(e) = std::fs::write(&path, contents) {
                eprintln!("warning: cannot seed example theme {name}: {e}");
            }
        }
    }
}

fn print_help() {
    println!("Usage: {} [--version|--help|hook-relay [endpoint]|mcp-serve [--endpoint PATH]|install-*|uninstall-*]", branding::binary_name());
    println!(
        "{} terminal control plane for AI coding agents.",
        branding::product_name()
    );
    println!("  hook-relay [endpoint]  forward one hook event from stdin; always exits 0");
    println!("  mcp-serve [--endpoint PATH]  JSON-RPC comms server on stdio for harnesses");
    println!("  install-hooks | install-mcp | install-skills  register forge with all harnesses");
    println!("  uninstall-hooks | uninstall-mcp | uninstall-skills  remove forge registration");
    println!("  install-codex|gemini|metamate  full per-harness install (uninstall-* reverses)");
}

/// Print installer outcomes, one line each. Skips are not failures; only
/// real errors fail the subcommand.
fn report_install(outs: Vec<install::Outcome>) -> i32 {
    let mut code = 0;
    for o in outs {
        if let Some(e) = &o.error {
            println!("{}: error: {e}", o.harness);
            code = 1;
        } else if o.installed {
            println!("{}: installed ({})", o.harness, o.detail);
        } else if o.removed {
            println!("{}: removed ({})", o.harness, o.detail);
        } else if o.skipped {
            println!("{}: skipped ({})", o.harness, o.detail);
        } else {
            println!("{}: unchanged ({})", o.harness, o.detail);
        }
    }
    code
}

fn forge_binary() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "forge".to_string())
}

fn home_dir() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// Headless startup sequence (Phase 1): migrate legacy state, load
/// configuration, mint the launch run ID. The TUI takes over in Phase 2.
fn startup() -> i32 {
    let home = home_dir();
    match config::migrate_legacy(&home) {
        Ok(config::Migration::Migrated) => eprintln!(
            "migrated legacy config to {}",
            branding::config_dir(&home).display()
        ),
        Ok(config::Migration::Collision) => eprintln!(
            "warning: both {} and {} exist; leaving both intact",
            branding::config_dir(&home).display(),
            branding::legacy_config_dir(&home).display()
        ),
        Ok(_) => {}
        Err(e) => {
            eprintln!("error: config migration failed: {e}");
            return 1;
        }
    }
    let mut loaded = match config::LoadedConfig::load_home(&home) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    // First launch normalizes state: a missing config file is materialized
    // from defaults (preserving nothing, since there is nothing yet) and an
    // absent audit log is created private before any decision is recorded.
    if !branding::config_file(&home).exists() {
        if let Err(e) = loaded.save_home(&home) {
            eprintln!("error: cannot write config: {e}");
            return 1;
        }
    }
    let audit = branding::audit_log(&home);
    if !audit.exists() {
        if let Err(e) = fs_atomic::write_private(&audit, b"") {
            eprintln!("error: cannot init audit log: {e}");
            return 1;
        }
    }
    // Agent CLI definitions: materialize the packaged default on first
    // launch, then load. A bad file is a hard error — launching an agent
    // with the wrong argv is worse than not launching.
    let agents_path = branding::agents_file(&home);
    if !agents_path.exists() {
        if let Err(e) = std::fs::write(&agents_path, crate::agents::DEFAULT_AGENTS_JSON) {
            eprintln!("error: cannot write agents file: {e}");
            return 1;
        }
    }
    if let Err(e) = crate::agents::load_registry(&agents_path) {
        eprintln!("error: {e}");
        return 1;
    }
    // Example themes: seeded into `~/.forge/themes/` when the directory
    // is missing or holds no themes, so the picker (`Ctrl-b e`) shows
    // something on first launch. Existing files are never overwritten.
    seed_example_themes(&home);
    // Configuration guide for AI agents: docs only, so a failed drop
    // warns instead of blocking startup — and an existing file is never
    // overwritten, since local edits must survive upgrades.
    let guide_path = branding::guide_file(&home);
    if !guide_path.exists() {
        if let Err(e) = std::fs::write(&guide_path, DEFAULT_AGENTS_MD) {
            eprintln!("warning: cannot write configuration guide: {e}");
        }
    }
    let run = ids::RunId::generate();
    if let Ok(mut log) =
        logging::FileLogger::open(&branding::app_log(&home), logging::DEFAULT_MAX_BYTES)
    {
        let _ = log.append(&format!(
            "forge {VERSION} start run {run} permission={:?}",
            loaded.config.permission.mode
        ));
    }
    let mut state = app::AppState::new();
    // Operator bot clients come from config only (never self-registered):
    // each entry binds a token file, and bad entries warn instead of
    // failing the boot. Calls still fail closed per-call.
    for reg in &loaded.config.bots {
        let path = create::expand_folder(&reg.token_file, &home);
        match state.broker.register_client_file(
            &state.manager,
            &reg.name,
            reg.groups.clone(),
            reg.grants.clone(),
            path,
        ) {
            Ok(()) => {}
            Err(e) => eprintln!("warning: skipping bot {:?}: {}", reg.name, e.message),
        }
    }
    let code = tui::run(&mut state, &mut loaded, &home, &audit);
    // Quitting serializes the live agent sessions for the next startup's
    // restore picker; failures warn instead of failing the exit. An
    // empty quit leaves the file alone: it may hold older entries from
    // before (Esc + quit-empty is a normal flow), and the picker shows
    // each entry's age.
    if let Err(e) = checkpoint::save_quit_snapshot(
        &branding::sessions_file(&home),
        state.snapshot_sessions(),
    ) {
        eprintln!("warning: cannot save sessions: {e}");
    }
    code
}

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--version") | Some("-V") => println!("{} {VERSION}", branding::binary_name()),
        Some("--help") | Some("-h") => print_help(),
        Some("hook-relay") => {
            let endpoint = args.next();
            std::process::exit(relay::run_stdin(endpoint.as_deref()));
        }
        Some("install-hooks") => {
            std::process::exit(report_install(install::install_hooks(&home_dir(), &forge_binary())));
        }
        Some("uninstall-hooks") => {
            std::process::exit(report_install(install::uninstall_hooks(&home_dir())));
        }
        Some("install-mcp") => {
            std::process::exit(report_install(install::install_mcp(&home_dir(), &forge_binary())));
        }
        Some("uninstall-mcp") => {
            std::process::exit(report_install(install::uninstall_mcp(&home_dir())));
        }
        Some("install-skills") => {
            std::process::exit(report_install(install::install_skills(&home_dir())));
        }
        Some("uninstall-skills") => {
            std::process::exit(report_install(install::uninstall_skills(&home_dir())));
        }
        Some("install-codex") => {
            std::process::exit(report_install(install::install_one(&home_dir(), "codex", &forge_binary())));
        }
        Some("uninstall-codex") => {
            std::process::exit(report_install(install::uninstall_one(&home_dir(), "codex")));
        }
        Some("install-gemini") => {
            std::process::exit(report_install(install::install_one(&home_dir(), "gemini", &forge_binary())));
        }
        Some("uninstall-gemini") => {
            std::process::exit(report_install(install::uninstall_one(&home_dir(), "gemini")));
        }
        Some("install-metamate") => {
            std::process::exit(report_install(install::install_one(&home_dir(), "metamate", &forge_binary())));
        }
        Some("uninstall-metamate") => {
            std::process::exit(report_install(install::uninstall_one(&home_dir(), "metamate")));
        }
        Some("mcp-serve") => {
            let mut explicit: Option<String> = None;
            while let Some(flag) = args.next() {
                if flag == "--endpoint" {
                    explicit = args.next();
                } else {
                    eprintln!("error: unexpected mcp-serve argument `{flag}`");
                    std::process::exit(2);
                }
            }
            let srv = mcp::ServerCtx {
                instructions_extra: String::new(),
            };
            let cc = mcp::CallCtx {
                endpoint: mcp::resolve_endpoint(explicit.as_deref()),
                run_id: std::env::var("FORGE_RUN_ID").unwrap_or_default(),
                timeout: std::time::Duration::from_secs(3),
            };
            std::process::exit(mcp::serve_stdio(&srv, &cc));
        }
        Some(other) => {
            eprintln!("error: unexpected argument `{other}`");
            eprintln!("Usage: forge [--version|--help]");
            std::process::exit(2);
        }
        None => {
            std::process::exit(startup());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_adds_missing_examples_without_touching_user_files() {
        let home = std::env::temp_dir().join(format!("forge-seed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let dir = branding::themes_dir(&home);
        std::fs::create_dir_all(&dir).unwrap();
        // A user file plus a customized packaged file: both must survive.
        std::fs::write(dir.join("mine.json"), r##"{"name": "mine"}"##).unwrap();
        std::fs::write(dir.join("square.json"), r##"{"name": "custom-square"}"##).unwrap();
        seed_example_themes(&home);
        assert_eq!(
            std::fs::read_to_string(dir.join("mine.json")).unwrap(),
            r##"{"name": "mine"}"##
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("square.json")).unwrap(),
            r##"{"name": "custom-square"}"##,
            "never overwrite"
        );
        for name in ["round.json", "tokyo-night.json", "triangle.json", "wedge.json"] {
            assert!(dir.join(name).is_file(), "missing example seeded: {name}");
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn packaged_guide_tracks_the_live_schema() {
        // Doc rot guard: if a registry key is renamed, the guide must
        // follow. File names, schema keys, enum values, and rules.
        for needle in [
            "config.toml",
            "agents.json",
            "sessions",
            "extra_args",
            "env_override",
            "model_flag",
            "default_model",
            "subcommand",
            "with_id",
            "without_id",
            "supports_hooks",
            "session_attribution",
            "hook_env",
            "cwd_window",
            "positional",
            "safe-only",
            "ai-assisted",
            "hook-relay",
            "install-hooks",
            "uninstall-hooks",
            "mcp-serve",
            "FORGE_IPC_ENDPOINT",
            "FORGE_RUN_ID",
            "SKILL.md",
        ] {
            assert!(
                DEFAULT_AGENTS_MD.contains(needle),
                "guide lost {needle:?}"
            );
        }
    }
}
