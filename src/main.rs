const VERSION: &str = "1.0.0";

fn print_help() {
    println!("Usage: forge [--version|--help]");
    println!("Forge terminal control plane for AI coding agents (TUI arrives in Phase 2).");
}

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--version") | Some("-V") => println!("forge {VERSION}"),
        Some("--help") | Some("-h") => print_help(),
        Some(other) => {
            eprintln!("error: unexpected argument `{other}`");
            eprintln!("Usage: forge [--version|--help]");
            std::process::exit(2);
        }
        None => {
            eprintln!("forge TUI not yet implemented (Phase 2)");
            std::process::exit(1);
        }
    }
}
