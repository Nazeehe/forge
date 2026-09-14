mod app;
mod audit;
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
mod relay;
mod safe_text;
mod session;
mod theme;
mod tui;
mod ui;
mod walkthrough;

const VERSION: &str = "1.0.0";

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
    let code = tui::run(&mut state, &mut loaded, &home, &audit);
    // Quitting serializes the live agent sessions for the next startup's
    // restore picker. An empty topology deletes the file so no stale
    // offer survives; failures warn instead of failing the exit.
    let path = branding::sessions_file(&home);
    let sessions = state.snapshot_sessions();
    if sessions.is_empty() {
        let _ = std::fs::remove_file(&path);
    } else {
        let mut file = checkpoint::SessionsFile::load(&path);
        file.push(checkpoint::make_entry(sessions, checkpoint::now_unix()));
        if let Err(e) = file.save(&path) {
            eprintln!("warning: cannot save sessions: {e}");
        }
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
