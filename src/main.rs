mod app;
mod branding;
mod config;
mod event;
mod fs_atomic;
mod ids;
mod input;
mod logging;
mod paths;
mod pty;
mod safe_text;
mod session;
mod theme;
mod tui;
mod ui;

const VERSION: &str = "1.0.0";

fn print_help() {
    println!("Usage: {} [--version|--help]", branding::binary_name());
    println!(
        "{} terminal control plane for AI coding agents (TUI arrives in Phase 2).",
        branding::product_name()
    );
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
    let loaded = match config::LoadedConfig::load_home(&home) {
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
    tui::run(&mut state)
}

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--version") | Some("-V") => println!("{} {VERSION}", branding::binary_name()),
        Some("--help") | Some("-h") => print_help(),
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
