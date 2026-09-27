//! Shared broker test fixtures: the live session pair, call helpers,
//! and inbox fill/drain helpers.

use crate::app::AppState;
use crate::infra::ids::RunId;
use crate::session::SessionId;

pub(super) fn json_field(haystack: &str, field: &str) -> Option<String> {
    crate::hooks::policy::json_string_field(haystack.as_bytes(), &[field])
}

pub(super) struct Pair {
    pub(super) state: AppState,
    pub(super) a: SessionId,
    pub(super) b: SessionId,
    pub(super) run_a: String,
    pub(super) run_b: String,
}

pub(super) fn live_pair() -> Pair {
    let mut state = AppState::new();
    let run_a = RunId::generate();
    let run_b = RunId::generate();
    let a = state
        .manager
        .spawn("a", &std::env::temp_dir(), "exec sleep 30", run_a.clone(), "shell")
        .unwrap();
    let b = state
        .manager
        .spawn("b", &std::env::temp_dir(), "exec sleep 30", run_b.clone(), "shell")
        .unwrap();
    Pair {
        state,
        a,
        b,
        run_a: run_a.to_string(),
        run_b: run_b.to_string(),
    }
}

impl Pair {
    pub(super) fn grouped(mut self) -> Self {
        self.state.broker.join(&self.state.manager, self.a, "peers").unwrap();
        self.state.broker.join(&self.state.manager, self.b, "peers").unwrap();
        self
    }

    pub(super) fn call(&mut self, run: &str, tool: &str, args: &str) -> Result<String, String> {
        let now = std::time::Instant::now();
        // Disjoint field borrows: the broker reads the manager.
        self.state.broker.call(&self.state.manager, run, tool, args, now)
    }

    pub(super) fn register_bot(&mut self, name: &str, groups: Vec<&str>) {
        let groups = groups.into_iter().map(str::to_string).collect();
        self.state
            .broker
            .register_client(&self.state.manager, name, groups, BOT_TOKEN, Vec::new())
            .expect("registration validates");
    }

    pub(super) fn bcall(
        &mut self,
        name: &str,
        tool: &str,
        args: &str,
    ) -> Result<String, crate::comms::bot::BotError> {
        self.bcall_as(name, BOT_TOKEN, tool, args)
    }

    pub(super) fn bcall_as(
        &mut self,
        name: &str,
        token: &str,
        tool: &str,
        args: &str,
    ) -> Result<String, crate::comms::bot::BotError> {
        let now = std::time::Instant::now();
        self.state
            .broker
            .bot_call(&self.state.manager, name, token, tool, args, now)
    }
}

pub(super) const BOT_TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// Fill a client's inbox to the cap with padding events. Deposit
/// fails exactly when full, so the loop needs no inbox access.
pub(super) fn fill_inbox(p: &mut Pair, client: &str) {
    let c = p
        .state
        .broker
        .clients
        .get_mut(client)
        .expect("client registered");
    let mut n = 0;
    while c
        .deposit(crate::comms::bot::BotKind::Tell, "pad", "pad", "pad", "pad", 0)
        .is_ok()
    {
        n += 1;
    }
    assert!(n > 0, "inbox filled to the cap");
}

/// Poll-plus-ack the whole backlog: polls advance the received
/// cursor 20 at a time, acks drain what was received. Thirteen
/// steps reach exactly 256 (the cap).
pub(super) fn drain_inbox(p: &mut Pair, client: &str) {
    let first = p.bcall(client, "bot_poll", "{}").expect("poll validates");
    let epoch = crate::ipc::mcp::top_raw(&first, "epoch").expect("epoch echoed");
    for step in 1..=13 {
        let cursor = (step * 20).min(crate::comms::bot::INBOX_CAP as u64);
        p.bcall(client, "bot_poll", "{}").expect("poll validates");
        p.bcall(
            client,
            "bot_ack",
            &format!(r#"{{"cursor":{cursor},"epoch":{epoch}}}"#),
        )
        .expect("ack validates");
    }
}
