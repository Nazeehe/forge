//! Broker group membership: create, join, leave, rename, remove, and queries.

use super::*;

impl Broker {
    /// Create an empty group (for the human group dialog). Fails on
    /// empty/duplicate names; creation assigns the next palette color.
    pub fn create_group(&mut self, group: &str) -> Result<(), String> {
        if group.is_empty() {
            return Err("empty group name".to_string());
        }
        if self.groups.contains_key(group) {
            return Err("group already exists".to_string());
        }
        let color = self.next_color;
        self.next_color += 1;
        self.groups.insert(
            group.to_string(),
            Group {
                members: HashSet::new(),
                color,
            },
        );
        Ok(())
    }

    /// Join a group, creating it when missing. The session must exist.
    /// Creation assigns the next palette color to the group.
    pub fn join(
        &mut self,
        sessions: &SessionManager,
        id: SessionId,
        group: &str,
    ) -> Result<(), String> {
        if sessions.get(id).is_none() {
            return Err("unknown session".to_string());
        }
        if group.is_empty() {
            return Err("empty group name".to_string());
        }
        if !self.groups.contains_key(group) {
            let color = self.next_color;
            self.next_color += 1;
            self.groups.insert(
                group.to_string(),
                Group {
                    members: HashSet::new(),
                    color,
                },
            );
        }
        self.groups
            .get_mut(group)
            .expect("group just created")
            .members
            .insert(id);
        let entry = self.membership.entry(id).or_default();
        if !entry.iter().any(|g| g == group) {
            entry.push(group.to_string());
        }
        Ok(())
    }

    pub fn is_member(&self, id: SessionId, group: &str) -> bool {
        self.groups
            .get(group)
            .is_some_and(|g| g.members.contains(&id))
    }

    pub fn leave(&mut self, id: SessionId, group: &str) -> bool {
        let mut removed = false;
        if let Some(g) = self.groups.get_mut(group) {
            removed = g.members.remove(&id);
        }
        if let Some(entry) = self.membership.get_mut(&id) {
            entry.retain(|g| g != group);
        }
        removed
    }

    /// Drop a session from every group it belongs to (termination path);
    /// true when it belonged to at least one.
    pub fn leave_all(&mut self, id: SessionId) -> bool {
        let groups = self.membership.remove(&id).unwrap_or_default();
        let mut removed = false;
        for group in &groups {
            if let Some(g) = self.groups.get_mut(group) {
                removed |= g.members.remove(&id);
            }
        }
        removed
    }

    /// First membership is the primary display group.
    pub fn primary_group(&self, id: SessionId) -> Option<&str> {
        self.membership
            .get(&id)
            .and_then(|entry| entry.first().map(String::as_str))
    }

    /// Every group one session belongs to, in join order. Snapshots use
    /// this so restores rejoin exactly.
    pub fn groups_of(&self, id: SessionId) -> Vec<String> {
        self.membership.get(&id).cloned().unwrap_or_default()
    }

    /// Palette index of a group, if it exists.
    pub fn group_color(&self, group: &str) -> Option<usize> {
        self.groups.get(group).map(|g| g.color)
    }

    /// All group names, sorted for a stable dialog.
    pub fn group_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.groups.keys().cloned().collect();
        names.sort();
        names
    }

    /// Members of a group as sorted session IDs, empty when unknown.
    pub fn group_members(&self, group: &str) -> Vec<SessionId> {
        let mut members: Vec<SessionId> = self
            .groups
            .get(group)
            .map(|g| g.members.iter().copied().collect())
            .unwrap_or_default();
        members.sort();
        members
    }

    /// Rename a group, keeping its members, palette color, and every
    /// member's primary-group order. Fails on empty/duplicate/unknown
    /// names without touching anything.
    pub fn rename_group(&mut self, from: &str, to: &str) -> Result<(), String> {
        if to.is_empty() {
            return Err("empty group name".to_string());
        }
        if from == to {
            return Ok(());
        }
        if !self.groups.contains_key(from) {
            return Err("unknown group".to_string());
        }
        if self.groups.contains_key(to) {
            return Err("group already exists".to_string());
        }
        let group = self.groups.remove(from).expect("group exists");
        for entry in self.membership.values_mut() {
            for name in entry.iter_mut() {
                if name == from {
                    *name = to.to_string();
                }
            }
        }
        self.groups.insert(to.to_string(), group);
        Ok(())
    }

    /// Delete a group outright, releasing every membership. True when the
    /// group existed; colors of surviving groups never shift.
    pub fn remove_group(&mut self, group: &str) -> bool {
        if self.groups.remove(group).is_none() {
            return false;
        }
        for entry in self.membership.values_mut() {
            entry.retain(|g| g != group);
        }
        true
    }

    /// Read-only shared-group check for the comms trace: a rejection
    /// names the membership fact instead of leaving it to guesswork.
    pub(crate) fn shares_group(&self, a: SessionId, b: SessionId) -> bool {
        self.shared_group(a, b)
    }

    pub(super) fn shared_group(&self, a: SessionId, b: SessionId) -> bool {
        let Some(mine) = self.membership.get(&a) else {
            return false;
        };
        mine.iter().any(|g| {
            self.groups
                .get(g)
                .is_some_and(|group| group.members.contains(&b))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comms::test_support::*;

        #[test]
        fn group_colors_assign_monotonically_and_survive_delete() {
            let mut p = live_pair();
            assert!(p.state.broker.join(&p.state.manager, p.a, "alpha").is_ok());
            assert!(p.state.broker.join(&p.state.manager, p.b, "alpha").is_ok());
            assert!(p.state.broker.join(&p.state.manager, p.a, "beta").is_ok());
            assert_eq!(p.state.broker.group_color("alpha"), Some(0));
            assert_eq!(p.state.broker.group_color("beta"), Some(1));
            assert_eq!(p.state.broker.group_color("missing"), None);
            assert_eq!(p.state.broker.group_names(), vec!["alpha".to_string(), "beta".to_string()]);
            let mut members = p.state.broker.group_members("alpha");
            members.sort();
            assert_eq!(members, vec![p.a.min(p.b), p.a.max(p.b)]);
            // Delete hands no color back: the next group takes 2, and alpha's
            // ex-members lose the membership (primary falls back to beta).
            assert!(p.state.broker.remove_group("alpha"));
            assert!(!p.state.broker.remove_group("alpha"));
            assert!(p.state.broker.join(&p.state.manager, p.b, "gamma").is_ok());
            assert_eq!(p.state.broker.group_color("gamma"), Some(2));
            assert_eq!(p.state.broker.primary_group(p.a), Some("beta"));
            assert_eq!(p.state.broker.primary_group(p.b), Some("gamma"));
        }

        #[test]
        fn create_and_rename_groups_keep_color_and_members() {
            let mut p = live_pair();
            assert!(p.state.broker.create_group("alpha").is_ok());
            assert!(p.state.broker.create_group("alpha").is_err(), "duplicate");
            assert!(p.state.broker.create_group("").is_err(), "empty");
            // Empty group exists with a color but no members.
            assert_eq!(p.state.broker.group_color("alpha"), Some(0));
            assert!(p.state.broker.group_members("alpha").is_empty());
            assert!(p.state.broker.join(&p.state.manager, p.a, "alpha").is_ok());
            assert!(p.state.broker.join(&p.state.manager, p.a, "beta").is_ok());
            assert!(p.state.broker.rename_group("alpha", "alpha-v2").is_ok());
            assert_eq!(p.state.broker.group_color("alpha-v2"), Some(0), "color kept");
            assert_eq!(p.state.broker.group_color("alpha"), None, "old name gone");
            assert!(p.state.broker.is_member(p.a, "alpha-v2"));
            assert_eq!(p.state.broker.primary_group(p.a), Some("alpha-v2"), "order kept");
            // Failures leave everything untouched.
            assert!(p.state.broker.rename_group("alpha-v2", "beta").is_err(), "duplicate");
            assert!(p.state.broker.rename_group("alpha-v2", "").is_err(), "empty");
            assert!(p.state.broker.rename_group("missing", "new").is_err(), "unknown");
            assert!(p.state.broker.is_member(p.a, "alpha-v2"));
            assert_eq!(p.state.broker.group_color("alpha-v2"), Some(0));
            // Same-name rename is a no-op success.
            assert!(p.state.broker.rename_group("alpha-v2", "alpha-v2").is_ok());
        }

        #[test]
        fn leave_breaks_the_shared_group() {
            let mut p = live_pair().grouped();
            assert!(p.state.broker.leave(p.a, "peers"));
            assert_eq!(p.state.broker.primary_group(p.a), None);
            let err = p
                .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"hi"}"#)
                .expect_err("left group must fail");
            assert!(err.contains("shared group"), "err: {err}");
        }

        #[test]
        fn leave_all_drops_every_group() {
            let mut p = live_pair();
            p.state.broker.join(&p.state.manager, p.a, "peers").unwrap();
            p.state.broker.join(&p.state.manager, p.a, "other").unwrap();
            assert!(p.state.broker.leave_all(p.a));
            assert!(!p.state.broker.is_member(p.a, "peers"));
            assert!(!p.state.broker.is_member(p.a, "other"));
            assert_eq!(p.state.broker.primary_group(p.a), None);
            assert!(!p.state.broker.leave_all(p.a), "second leave is a no-op");
            assert!(!p.state.broker.leave_all(p.b), "never joined");
        }

        #[test]
        fn response_after_group_leave_is_refused() {
            // Leaving the shared group revokes the answer channel: the
            // response must fail closed, not ride the old conversation ID.
            let mut p = live_pair().grouped();
            let ask = p
                .call(&p.run_a.clone(), "ask_session", r#"{"target":"b","message":"ready?"}"#)
                .expect("ask validates");
            let conv = json_field(&ask, "conversation").expect("conversation id");
            assert!(p.state.broker.leave(p.b, "peers"));
            let err = p
                .call(
                    &p.run_b.clone(),
                    "send_response",
                    &format!(r#"{{"conversation_id":"{conv}","message":"yes"}}"#),
                )
                .expect_err("leave revokes answers");
            assert!(err.contains("no shared group"), "err: {err}");
        }

        #[test]
        fn ack_after_group_leave_is_refused() {
            let mut p = live_pair().grouped();
            let tell = p
                .call(&p.run_a.clone(), "tell_session", r#"{"target":"b","text":"hi"}"#)
                .expect("tell validates");
            let conv = json_field(&tell, "conversation").expect("conversation id");
            assert!(p.state.broker.leave(p.b, "peers"));
            let err = p
                .call(
                    &p.run_b.clone(),
                    "ack_message",
                    &format!(r#"{{"conversation_id":"{conv}"}}"#),
                )
                .expect_err("leave revokes acks");
            assert!(err.contains("no shared group"), "err: {err}");
        }

        #[test]
        fn client_response_after_session_leaves_group_is_refused() {
            use crate::comms::bot::ErrorCode;
            let mut p = live_pair().grouped();
            p.register_bot("skippy", vec!["peers"]);
            let res = p
                .call(
                    &p.run_a.clone(),
                    "ask_session",
                    r#"{"target":"skippy","message":"are you there?"}"#,
                )
                .expect("session asks client");
            let conv = json_field(&res, "conversation").expect("conversation id");
            assert!(p.state.broker.leave(p.a, "peers"));
            let err = p
                .bcall(
                    "skippy",
                    "send_response",
                    &format!(r#"{{"conversation_id":"{conv}","message":"yes"}}"#),
                )
                .expect_err("leave revokes client answers");
            assert_eq!(err.code, ErrorCode::NoSharedGroup);
        }

        #[test]
        fn session_response_to_client_ask_after_leave_is_refused() {
            let mut p = live_pair().grouped();
            p.register_bot("skippy", vec!["peers"]);
            let res = p
                .bcall("skippy", "ask_session", r#"{"target":"b","message":"ready?"}"#)
                .expect("client asks");
            let conv = json_field(&res, "conversation").expect("conversation id");
            assert!(p.state.broker.leave(p.b, "peers"));
            let err = p
                .call(
                    &p.run_b.clone(),
                    "send_response",
                    &format!(r#"{{"conversation_id":"{conv}","message":"yes"}}"#),
                )
                .expect_err("leave revokes session answers to clients");
            assert!(err.contains("no shared group"), "err: {err}");
        }

        #[test]
        fn session_ack_to_client_tell_after_leave_is_refused() {
            let mut p = live_pair().grouped();
            p.register_bot("skippy", vec!["peers"]);
            let res = p
                .bcall("skippy", "tell_session", r#"{"target":"b","text":"hi"}"#)
                .expect("client tells");
            let conv = json_field(&res, "conversation").expect("conversation id");
            assert!(p.state.broker.leave(p.b, "peers"));
            let err = p
                .call(
                    &p.run_b.clone(),
                    "ack_message",
                    &format!(r#"{{"conversation_id":"{conv}"}}"#),
                )
                .expect_err("leave revokes session acks to clients");
            assert!(err.contains("no shared group"), "err: {err}");
        }

        #[test]
        fn client_ack_after_session_leaves_group_is_refused() {
            use crate::comms::bot::ErrorCode;
            let mut p = live_pair().grouped();
            p.register_bot("skippy", vec!["peers"]);
            let res = p
                .call(
                    &p.run_a.clone(),
                    "tell_session",
                    r#"{"target":"skippy","text":"hi"}"#,
                )
                .expect("session tells client");
            let conv = json_field(&res, "conversation").expect("conversation id");
            assert!(p.state.broker.leave(p.a, "peers"));
            let err = p
                .bcall(
                    "skippy",
                    "ack_message",
                    &format!(r#"{{"conversation_id":"{conv}"}}"#),
                )
                .expect_err("leave revokes client acks");
            assert_eq!(err.code, ErrorCode::NoSharedGroup);
        }
}
