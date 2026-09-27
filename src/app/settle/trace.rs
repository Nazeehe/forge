//! comms.log trace tests for settle delivery: send verdicts, holds,
//! deliveries, and staged Enters.

use super::*;
use crate::infra::ids::RunId;

#[test]
fn comms_trace_records_send_verdicts() {
    let dir = std::env::temp_dir().join(format!("forge-comms-trace-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let log = dir.join("comms.log");
    let mut s = AppState::new();
    s.comms_trace = Some(log.clone());
    let run_a = RunId::generate();
    let a = s
        .manager
        .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
        .unwrap();
    let run_b = RunId::generate();
    let b = s
        .manager
        .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
        .unwrap();
    let comms = |s: &mut AppState, run_id: String, tool: &str, args: &str| {
        let (reply_tx, _) = std::sync::mpsc::channel();
        s.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
            run_id,
            tool: tool.to_string(),
            args: args.to_string(),
            reply: reply_tx,
            claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::ipc::listener::CLAIM_PENDING)),
        }));
    };
    // No shared group: the rejection lands in the trace with the cause.
    comms(&mut s, run_a.to_string(), "tell_session", "{\"target\":\"b\",\"text\":\"hi\"}");
    // Unknown target: liveness reads false, never a guess.
    comms(&mut s, run_a.to_string(), "tell_session", "{\"target\":\"ghost\",\"text\":\"hi\"}");
    // Grouped: the accept lands with its conversation and queue depth.
    s.broker.join(&s.manager, a, "peers").unwrap();
    s.broker.join(&s.manager, b, "peers").unwrap();
    comms(&mut s, run_a.to_string(), "tell_session", "{\"target\":\"b\",\"text\":\"hello-b\"}");
    let text = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert!(lines[0].contains("tool=tell_session"), "{}", lines[0]);
    assert!(lines[0].contains("caller=\"a\""), "{}", lines[0]);
    assert!(lines[0].contains("-> err"), "{}", lines[0]);
    assert!(lines[0].contains("no shared group"), "{}", lines[0]);
    assert!(lines[0].contains("shared_group=false"), "{}", lines[0]);
    assert!(lines[1].contains("-> err"), "{}", lines[1]);
    assert!(lines[1].contains("target_live=false"), "{}", lines[1]);
    assert!(lines[2].contains("-> ok"), "{}", lines[2]);
    assert!(lines[2].contains("conv="), "{}", lines[2]);
    assert!(lines[2].contains("to=\"b\""), "{}", lines[2]);
    assert!(lines[2].contains("queued=1"), "{}", lines[2]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(s.manager.remove(a));
    assert!(s.manager.remove(b));
}

#[test]
fn comms_trace_records_delivery_and_busy_holds() {
    let dir = std::env::temp_dir().join(format!("forge-comms-hold-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let log = dir.join("comms.log");
    let mut s = AppState::new();
    s.comms_trace = Some(log.clone());
    let run_a = RunId::generate();
    let a = s
        .manager
        .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
        .unwrap();
    let run_b = RunId::generate();
    let b = s
        .manager
        .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
        .unwrap();
    s.broker.join(&s.manager, a, "peers").unwrap();
    s.broker.join(&s.manager, b, "peers").unwrap();
    let (reply_tx, _) = std::sync::mpsc::channel();
    s.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
        run_id: run_a.to_string(),
        tool: "tell_session".to_string(),
        args: "{\"target\":\"b\",\"text\":\"hello-b\"}".to_string(),
        reply: reply_tx,
        claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::ipc::listener::CLAIM_PENDING)),
    }));
    // Busy target: the hold names the pane activity, not just silence.
    assert!(s.manager.set_activity(b, crate::session::Activity::ToolUse));
    s.settle_comms();
    assert_eq!(s.broker.queued(b), 1);
    // Idle target: the delivery names conv, kind, and queue remainder.
    assert!(s.manager.set_activity(b, crate::session::Activity::Idle));
    s.settle_comms();
    assert_eq!(s.broker.queued(b), 0);
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("hold"), "{text}");
    assert!(text.contains("to=\"b\""), "{text}");
    assert!(text.contains("activity=ToolUse"), "{text}");
    assert!(text.contains("deliver"), "{text}");
    assert!(text.contains("queued_left=0"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(s.manager.remove(a));
    assert!(s.manager.remove(b));
}

#[test]
fn comms_trace_logs_a_stuck_hold_once_per_reason() {
    // One line per sweep flooded comms.log (3,300+ holds in one run,
    // rotating the evidence away). A hold logs when its reason
    // changes, with a periodic reminder while it stays stuck.
    let dir = std::env::temp_dir().join(format!("forge-comms-holdonce-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let log = dir.join("comms.log");
    let mut s = AppState::new();
    s.comms_trace = Some(log.clone());
    let run_a = RunId::generate();
    let a = s
        .manager
        .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
        .unwrap();
    let b = s
        .manager
        .spawn("b", &std::env::temp_dir(), "exec sleep 30", RunId::generate(), "shell")
        .unwrap();
    s.broker.join(&s.manager, a, "peers").unwrap();
    s.broker.join(&s.manager, b, "peers").unwrap();
    let (reply_tx, _) = std::sync::mpsc::channel();
    s.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
        run_id: run_a.to_string(),
        tool: "tell_session".to_string(),
        args: "{\"target\":\"b\",\"text\":\"hello-b\"}".to_string(),
        reply: reply_tx,
        claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(
            crate::ipc::listener::CLAIM_PENDING,
        )),
    }));
    let holds = |log: &std::path::Path| {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains(" hold "))
            .count()
    };
    assert!(s.manager.set_activity(b, crate::session::Activity::ToolUse));
    for _ in 0..5 {
        s.last_broker_tick = None; // force a sweep
        s.settle_comms();
    }
    assert_eq!(holds(&log), 1, "same reason, five sweeps: one line");
    assert!(s.manager.set_activity(b, crate::session::Activity::Thinking));
    s.last_broker_tick = None;
    s.settle_comms();
    assert_eq!(holds(&log), 2, "reason changed: one more line");
    // Still stuck past the reminder interval: one reminder line.
    let (reason, _) = s.hold_logged.get(&b).cloned().expect("hold remembered");
    s.hold_logged.insert(
        b,
        (reason, std::time::Instant::now() - crate::comms::HOLD_LOG_REMINDER),
    );
    s.last_broker_tick = None;
    s.settle_comms();
    assert_eq!(holds(&log), 3, "periodic reminder while stuck");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(s.manager.remove(a));
    assert!(s.manager.remove(b));
}

#[test]
fn comms_trace_records_staged_enters() {
    let dir = std::env::temp_dir().join(format!("forge-comms-enter-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let log = dir.join("comms.log");
    let mut s = AppState::new();
    s.comms_trace = Some(log.clone());
    let run_a = RunId::generate();
    let a = s
        .manager
        .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
        .unwrap();
    let run_b = RunId::generate();
    let b = s
        .manager
        .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
        .unwrap();
    s.broker.join(&s.manager, a, "peers").unwrap();
    s.broker.join(&s.manager, b, "peers").unwrap();
    let (reply_tx, _) = std::sync::mpsc::channel();
    s.apply(AppEvent::CommsRequest(crate::ipc::listener::CommsRequest {
        run_id: run_a.to_string(),
        tool: "tell_session".to_string(),
        args: "{\"target\":\"b\",\"text\":\"hello-b\"}".to_string(),
        reply: reply_tx,
        claim: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(crate::ipc::listener::CLAIM_PENDING)),
    }));
    assert!(s.manager.set_activity(b, crate::session::Activity::Idle));
    s.settle_comms();
    assert_eq!(s.broker.queued(b), 0);
    assert!(s.pending_enter.contains_key(&b), "body stages its Enter");
    s.settle_enters(std::time::Instant::now() + std::time::Duration::from_millis(500));
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("enter"), "{text}");
    assert!(text.contains("to=\"b\""), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(s.manager.remove(a));
    assert!(s.manager.remove(b));
}
