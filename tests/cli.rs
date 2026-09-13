use std::process::Command;

fn forge() -> Command {
    Command::new(env!("CARGO_BIN_EXE_forge"))
}

#[test]
fn version_flag_prints_version() {
    let out = forge().arg("--version").output().expect("run forge");
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "forge 1.0.0\n");
}

#[test]
fn help_flag_prints_usage() {
    let out = forge().arg("--help").output().expect("run forge");
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Usage: forge"), "help was: {text:?}");
}

#[test]
fn no_command_reports_missing_tui() {
    let out = forge().output().expect("run forge");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("TUI"), "stderr was: {err:?}");
}
