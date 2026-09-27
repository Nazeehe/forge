//! AppState board tool JSON: card lifecycle over the tool boundary.

use super::*;

impl AppState {
    /// Kanban-family tools: workspace-global boards, authorized by the
    /// caller's run ID like every other tool. `None` when the name is
    /// not a board tool and the next handler should answer instead.
    pub(super) fn board_tool(
        &mut self,
        run_id: &str,
        tool: &str,
        args: &str,
    ) -> Option<Result<String, String>> {
        match tool {
            "board_list" | "board_get" | "board_create" | "card_create" | "card_move"
            | "card_update" | "card_delete" | "card_assign" => {}
            _ => return None,
        }
        let caller = match self.resolve_tool_caller(run_id) {
            Ok(id) => id,
            Err(e) => return Some(Err(e)),
        };
        let args_v: serde_json::Value =
            serde_json::from_str(args).unwrap_or(serde_json::Value::Null);
        // serde_json already decoded the escapes (unlike the raw
        // `tool_arg` path), so these strings are final.
        let arg = |key: &str| {
            args_v
                .get(key)
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .filter(|s| !s.trim().is_empty())
        };
        let outcome = match tool {
            "board_list" => Ok(self.board_list_json()),
            "board_get" => match arg("board_name").or_else(|| arg("board_id")) {
                Some(name) => match self.boards.board(&name) {
                    Some(_) => Ok(self.board_get_json(&name)),
                    None => Err(format!("board not found: {name}")),
                },
                None => match self.default_board_id() {
                    Some(id) => Ok(self.board_get_json(&id)),
                    None => Err("no boards yet".to_string()),
                },
            },
            "board_create" => {
                let Some(name) = arg("name") else {
                    return Some(Err("board_create needs a name".to_string()));
                };
                let columns = match Self::parse_column_specs(&args_v) {
                    Ok(columns) => columns,
                    Err(e) => return Some(Err(e)),
                };
                match self.boards.board_create(&name, columns) {
                    Ok(()) => {
                        self.ensure_board_focus();
                        Ok(self.board_get_json(&name))
                    }
                    Err(e) => Err(e.to_string()),
                }
            }
            "card_create" => {
                let (Some(board_name), Some(title)) = (arg("board_name"), arg("title")) else {
                    return Some(Err("card_create needs board_name and title".to_string()));
                };
                let estimate = match Self::parse_estimate_arg(&args_v) {
                    Ok(estimate) => estimate,
                    Err(e) => return Some(Err(e)),
                };
                let draft = crate::kanban::board::CardDraft {
                    title,
                    column: arg("column"),
                    description: arg("description").unwrap_or_default(),
                    assignee: arg("assignee"),
                    priority: arg("priority")
                        .map(|p| crate::kanban::board::Priority::parse(&p))
                        .unwrap_or(crate::kanban::board::Priority::Normal),
                    tags: args_v
                        .get("tags")
                        .and_then(|t| t.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|t| t.as_str())
                                .map(|t| t.to_string())
                                .collect()
                        })
                        .unwrap_or_default(),
                    start_date: arg("start_date"),
                    due_date: arg("due_date"),
                    estimate,
                };
                match self.boards.board_mut(&board_name) {
                    None => Err(format!("board not found: {board_name}")),
                    Some(board) => match board.card_create(draft) {
                        Ok(id) => Ok(Self::card_json(board.card(&id).expect("just created"))),
                        Err(e) => Err(e.to_string()),
                    },
                }
            }
            "card_move" => {
                let (Some(card_id), Some(column)) = (arg("card_id"), arg("column")) else {
                    return Some(Err("card_move needs card_id and column".to_string()));
                };
                match self.locate_card(&card_id) {
                    None => Err(format!("card not found: {card_id}")),
                    Some((bi, _)) => {
                        let board = &mut self.boards.boards[bi];
                        match board.card_move(&card_id, &column) {
                            Ok(()) => Ok(Self::card_json(board.card(&card_id).expect("just moved"))),
                            Err(e) => Err(e.to_string()),
                        }
                    }
                }
            }
            "card_update" => {
                let Some(card_id) = arg("card_id") else {
                    return Some(Err("card_update needs card_id".to_string()));
                };
                let progress = match Self::parse_progress_arg(&args_v) {
                    Ok(progress) => progress,
                    Err(e) => return Some(Err(e)),
                };
                let (estimate, clear_estimate) = match Self::parse_estimate_patch(&args_v) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                let raw_empty = |key: &str| {
                    args_v.get(key).and_then(|v| v.as_str()).is_some_and(|s| s.trim().is_empty())
                };
                let patch = crate::kanban::board::CardPatch {
                    title: arg("title"),
                    description: arg("description"),
                    assignee: arg("assignee"),
                    clear_assignee: raw_empty("assignee"),
                    priority: arg("priority").map(|p| crate::kanban::board::Priority::parse(&p)),
                    progress,
                    tags: args_v.get("tags").and_then(|t| t.as_array()).map(|arr| {
                        arr.iter().filter_map(|t| t.as_str()).map(|t| t.to_string()).collect()
                    }),
                    start_date: arg("start_date"),
                    clear_start_date: raw_empty("start_date"),
                    due_date: arg("due_date"),
                    clear_due_date: raw_empty("due_date"),
                    estimate,
                    clear_estimate,
                };
                let column = arg("column");
                match self.locate_card(&card_id) {
                    None => Err(format!("card not found: {card_id}")),
                    Some((bi, _)) => {
                        let board = &mut self.boards.boards[bi];
                        if let Some(target) = column {
                            if let Err(e) = board.card_move(&card_id, &target) {
                                return Some(Err(e.to_string()));
                            }
                        }
                        match board.card_update(&card_id, patch) {
                            Ok(()) => Ok(Self::card_json(board.card(&card_id).expect("just updated"))),
                            Err(e) => Err(e.to_string()),
                        }
                    }
                }
            }
            "card_delete" => {
                let Some(card_id) = arg("card_id") else {
                    return Some(Err("card_delete needs card_id".to_string()));
                };
                match self.locate_card(&card_id) {
                    None => Err(format!("card not found: {card_id}")),
                    Some((bi, _)) => {
                        let board = &mut self.boards.boards[bi];
                        match board.card_delete(&card_id) {
                            Ok(()) => {
                                self.ensure_board_focus();
                                Ok(format!(
                                    "{{\"deleted\":true,\"card_id\":{}}}",
                                    crate::ipc::mcp::escape_json(&card_id)
                                ))
                            }
                            Err(e) => Err(e.to_string()),
                        }
                    }
                }
            }
            "card_assign" => {
                let Some(card_id) = arg("card_id") else {
                    return Some(Err("card_assign needs card_id".to_string()));
                };
                let who = arg("assignee").or_else(|| {
                    self.manager
                        .get(caller)
                        .map(|rec| rec.name.clone())
                        .filter(|n| !n.trim().is_empty())
                });
                match self.locate_card(&card_id) {
                    None => Err(format!("card not found: {card_id}")),
                    Some((bi, _)) => {
                        let board = &mut self.boards.boards[bi];
                        match board.card_assign(&card_id, who.as_deref()) {
                            Ok(()) => Ok(Self::card_json(board.card(&card_id).expect("just assigned"))),
                            Err(e) => Err(e.to_string()),
                        }
                    }
                }
            }
            _ => unreachable!("board_tool gate"),
        };
        if outcome.is_ok() {
            self.boards_dirty = true;
            self.ensure_board_focus();
            self.dirty = true;
        }
        Some(outcome)
    }

    fn parse_column_specs(
        args: &serde_json::Value,
    ) -> Result<Option<Vec<crate::kanban::board::ColumnSpec>>, String> {
        let Some(raw) = args.get("columns") else {
            return Ok(None);
        };
        if raw.is_null() {
            return Ok(None);
        }
        let arr = raw.as_array().ok_or("columns must be an array")?;
        let mut out = Vec::with_capacity(arr.len());
        for item in arr {
            if let Some(name) = item.as_str() {
                out.push(crate::kanban::board::ColumnSpec::new(name));
            } else if item.is_object() {
                let name = item
                    .get("name")
                    .and_then(|n| n.as_str())
                    .ok_or("column entries need a name")?;
                let wip = item.get("wip_limit").and_then(|w| w.as_u64()).unwrap_or(0) as u32;
                out.push(crate::kanban::board::ColumnSpec::with_wip(name, wip));
            } else {
                return Err("column entries must be names or {name, wip_limit}".to_string());
            }
        }
        Ok(Some(out))
    }

    fn card_json(card: &crate::kanban::board::Card) -> String {
        serde_json::json!({
            "id": card.id,
            "title": card.title,
            "description": card.description,
            "column": card.column,
            "assignee": card.assignee,
            "tags": card.tags,
            "priority": card.priority.as_str(),
            "created_at": card.created_at,
            "updated_at": card.updated_at,
            "start_date": card.start_date,
            "due_date": card.due_date,
            "estimate": card.estimate,
            "progress": card.progress,
        })
        .to_string()
    }

    /// `card_create` estimate: a number or numeric string; absent/null
    /// means no estimate. Anything else is a loud error, never silent.
    fn parse_estimate_arg(args: &serde_json::Value) -> Result<Option<u32>, String> {
        match args.get("estimate") {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::Number(n)) => n
                .as_u64()
                .map(|e| Some(e.min(crate::kanban::board::MAX_ESTIMATE as u64) as u32))
                .ok_or_else(|| "estimate must be a number".to_string()),
            Some(serde_json::Value::String(s)) if s.trim().is_empty() => Ok(None),
            Some(serde_json::Value::String(s)) => s
                .trim()
                .parse::<u64>()
                .map(|e| Some(e.min(crate::kanban::board::MAX_ESTIMATE as u64) as u32))
                .map_err(|_| "estimate must be a number".to_string()),
            Some(_) => Err("estimate must be a number".to_string()),
        }
    }

    /// `card_update` estimate: absent/null keeps, empty string or null
    /// with intent clears, numbers/strings set. Returns (set, clear).
    fn parse_estimate_patch(args: &serde_json::Value) -> Result<(Option<u32>, bool), String> {
        match args.get("estimate") {
            None | Some(serde_json::Value::Null) => Ok((None, false)),
            Some(serde_json::Value::Number(n)) => n
                .as_u64()
                .map(|e| (Some(e.min(crate::kanban::board::MAX_ESTIMATE as u64) as u32), false))
                .ok_or_else(|| "estimate must be a number".to_string()),
            Some(serde_json::Value::String(s)) if s.trim().is_empty() => Ok((None, true)),
            Some(serde_json::Value::String(s)) => s
                .trim()
                .parse::<u64>()
                .map(|e| (Some(e.min(crate::kanban::board::MAX_ESTIMATE as u64) as u32), false))
                .map_err(|_| "estimate must be a number".to_string()),
            Some(_) => Err("estimate must be a number".to_string()),
        }
    }

    /// `card_update` progress: a number (or numeric string) 0+, clamped
    /// to 100 by the domain. Anything else is a loud error.
    fn parse_progress_arg(args: &serde_json::Value) -> Result<Option<u16>, String> {
        match args.get("progress") {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::Number(n)) => n
                .as_u64()
                .map(|p| Some(p.min(10_000) as u16))
                .ok_or_else(|| "progress must be a number".to_string()),
            Some(serde_json::Value::String(s)) if s.trim().is_empty() => Ok(None),
            Some(serde_json::Value::String(s)) => s
                .trim()
                .parse::<u64>()
                .map(|p| Some(p.min(10_000) as u16))
                .map_err(|_| "progress must be a number".to_string()),
            Some(_) => Err("progress must be a number".to_string()),
        }
    }

    fn board_list_json(&self) -> String {
        let boards: Vec<serde_json::Value> = self
            .boards
            .boards
            .iter()
            .map(|b| {
                serde_json::json!({
                    "id": b.id,
                    "name": b.name,
                    "columns": b.columns.iter().map(|c| {
                        serde_json::json!({
                            "name": c.name,
                            "cards": b.cards_in(&c.name).len(),
                            "wip_limit": c.wip_limit,
                        })
                    }).collect::<Vec<_>>(),
                })
            })
            .collect();
        serde_json::json!({ "boards": boards }).to_string()
    }

    fn board_get_json(&self, name_or_id: &str) -> String {
        let Some(board) = self.boards.board(name_or_id) else {
            return format!(
                "{{\"error\":{}}}",
                crate::ipc::mcp::escape_json(&format!("board not found: {name_or_id}"))
            );
        };
        serde_json::json!({
            "id": board.id,
            "name": board.name,
            "columns": board.columns.iter().map(|c| {
                serde_json::json!({ "name": c.name, "position": c.position, "wip_limit": c.wip_limit })
            }).collect::<Vec<_>>(),
            "cards": board.cards.iter().map(|c| {
                serde_json::json!({
                    "id": c.id, "title": c.title, "description": c.description,
                    "column": c.column, "assignee": c.assignee, "tags": c.tags,
                    "priority": c.priority.as_str(), "created_at": c.created_at,
                    "updated_at": c.updated_at, "start_date": c.start_date,
                    "due_date": c.due_date, "estimate": c.estimate, "progress": c.progress,
                })
            }).collect::<Vec<_>>(),
        })
        .to_string()
    }

    /// `(board index, card index)` for a card ID, boards in order.
    fn locate_card(&self, card_id: &str) -> Option<(usize, usize)> {
        self.boards.boards.iter().enumerate().find_map(|(bi, b)| {
            b.cards.iter().position(|c| c.id == card_id).map(|ci| (bi, ci))
        })
    }

    /// Focused board first, else the first board: `board_get` default.
    fn default_board_id(&self) -> Option<String> {
        self.board_focus
            .board
            .clone()
            .filter(|id| self.boards.board(id).is_some())
            .or_else(|| self.boards.boards.first().map(|b| b.id.clone()))
    }
}

pub(super) fn board_live_state() -> (AppState, String) {
    let mut state = AppState::new();
    let id = state
        .manager
        .spawn_agent(
            "agent",
            &std::env::temp_dir(),
            "exec cat",
            crate::infra::ids::RunId::generate(),
            "codex",
        )
        .unwrap();
    let live_run = state.manager.get(id).unwrap().run_id.as_str().to_string();
    (state, live_run)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::*;

    #[test]
    fn board_tools_drive_card_lifecycle() {
        let (mut state, live_run) = board_live_state();
        let created = comms_reply(&mut state, &live_run, "board_create", r#"{"name":"team"}"#);
        assert!(created.contains(r#""ok":true"#), "created: {created}");
        assert!(created.contains("team"), "created: {created}");
        let listed = comms_reply(&mut state, &live_run, "board_list", r#"{}"#);
        assert!(listed.contains("team"), "listed: {listed}");
        assert!(listed.contains("Doing"), "per-column counts: {listed}");
        let card = comms_reply(
            &mut state,
            &live_run,
            "card_create",
            r#"{"board_name":"team","title":"ship it"}"#,
        );
        assert!(card.contains(r#""ok":true"#), "card: {card}");
        let id: String = {
            let body = &card[card.find(r#""result":"#).unwrap_or(0)..];
            let start = body.find(r#""id":""#).map(|i| i + 6).unwrap_or(0);
            let rest = &body[start..];
            rest[..rest.find('"').unwrap_or(0)].to_string()
        };
        assert!(id.starts_with("c-"), "card id: {card}");
        let moved = comms_reply(
            &mut state,
            &live_run,
            "card_move",
            &format!(r#"{{"card_id":{},"column":"Done"}}"#, crate::ipc::mcp::escape_json(&id)),
        );
        assert!(moved.contains(r#""progress":100"#), "done sets 100: {moved}");
        let got = comms_reply(
            &mut state,
            &live_run,
            "board_get",
            r#"{"board_name":"team"}"#,
        );
        assert!(got.contains("ship it") && got.contains("Done"), "get: {got}");
        // Assign with no name claims the caller's session and tries Doing.
        let assigned = comms_reply(
            &mut state,
            &live_run,
            "card_assign",
            &format!(r#"{{"card_id":{}}}"#, crate::ipc::mcp::escape_json(&id)),
        );
        assert!(assigned.contains("agent"), "caller session named: {assigned}");
        let deleted = comms_reply(
            &mut state,
            &live_run,
            "card_delete",
            &format!(r#"{{"card_id":{}}}"#, crate::ipc::mcp::escape_json(&id)),
        );
        assert!(deleted.contains(r#""deleted":true"#), "deleted: {deleted}");
        assert!(state.boards_dirty, "tool mutations persist");
    }

    #[test]
    fn board_tools_reject_bad_calls() {
        let (mut state, live_run) = board_live_state();
        // Forged run ID never reaches the boards.
        let forged = comms_reply(&mut state, &"0".repeat(32), "board_list", r#"{}"#);
        assert!(forged.contains("unknown or stale run ID"), "forged: {forged}");
        // Unknown board and card stay errors, never panics.
        let no_board = comms_reply(&mut state, &live_run, "board_get", r#"{"board_name":"nope"}"#);
        assert!(no_board.contains("board not found"), "no board: {no_board}");
        let no_card = comms_reply(&mut state, &live_run, "card_move", r#"{"card_id":"c-dead","column":"Done"}"#);
        assert!(no_card.contains("card not found"), "no card: {no_card}");
        // Nameless creates fail loudly.
        let nameless = comms_reply(&mut state, &live_run, "board_create", r#"{}"#);
        assert!(nameless.contains("needs a name"), "nameless: {nameless}");
        // WIP limits bind agents too.
        comms_reply(&mut state, &live_run, "board_create", r#"{"name":"wip"}"#);
        for t in ["a", "b", "c"] {
            let card = comms_reply(
                &mut state,
                &live_run,
                "card_create",
                &format!(r#"{{"board_name":"wip","title":"{t}","column":"Doing"}}"#),
            );
            assert!(card.contains(r#""ok":true"#), "fill: {card}");
        }
        let full = comms_reply(
            &mut state,
            &live_run,
            "card_create",
            r#"{"board_name":"wip","title":"overflow","column":"Doing"}"#,
        );
        assert!(full.contains("WIP"), "wip binds: {full}");
    }

    #[test]
    fn board_create_honors_columns() {
        let (mut state, live_run) = board_live_state();
        let created = comms_reply(
            &mut state,
            &live_run,
            "board_create",
            r#"{"name":"flow","columns":["Inbox",{"name":"Active","wip_limit":2},"Shipped"]}"#,
        );
        assert!(created.contains(r#""ok":true"#), "created: {created}");
        let board = state.boards.board("flow").unwrap();
        let names: Vec<&str> = board.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["Inbox", "Active", "Shipped"]);
        assert_eq!(board.columns[1].wip_limit, 2);
    }

    #[test]
    fn board_update_and_get_defaults() {
        let (mut state, live_run) = board_live_state();
        comms_reply(&mut state, &live_run, "board_create", r#"{"name":"team"}"#);
        let card = comms_reply(
            &mut state,
            &live_run,
            "card_create",
            r#"{"board_name":"team","title":"t","priority":"high","tags":["x","y"],"progress":5}"#,
        );
        assert!(card.contains(r#""ok":true"#), "card: {card}");
        let body = &card[card.find(r#""result":"#).unwrap_or(0)..];
        let start = body.find(r#""id":""#).map(|i| i + 6).unwrap_or(0);
        let rest = &body[start..];
        let id = rest[..rest.find('"').unwrap_or(0)].to_string();
        assert!(id.starts_with("c-"), "card id: {card}");
        // card_create ignores progress; card_update clamps it instead.
        let updated = comms_reply(
            &mut state,
            &live_run,
            "card_update",
            &format!(
                r#"{{"card_id":{},"title":"t2","progress":250}}"#,
                crate::ipc::mcp::escape_json(&id)
            ),
        );
        assert!(updated.contains(r#""title":"t2""#), "renamed: {updated}");
        assert!(updated.contains(r#""progress":100"#), "clamped: {updated}");
        // Duplicate board names fail loudly instead of forking state.
        let dup = comms_reply(&mut state, &live_run, "board_create", r#"{"name":"team"}"#);
        assert!(dup.contains("already exists"), "dup: {dup}");
        // board_id lookups work, and bare board_get falls back instead
        // of erroring when the operator just wants "the board".
        let id = state.boards.board("team").unwrap().id.clone();
        let by_id = comms_reply(
            &mut state,
            &live_run,
            "board_get",
            &format!(r#"{{"board_id":{}}}"#, crate::ipc::mcp::escape_json(&id)),
        );
        assert!(by_id.contains("Backlog"), "by id: {by_id}");
        let bare = comms_reply(&mut state, &live_run, "board_get", r#"{}"#);
        assert!(bare.contains("Backlog"), "default: {bare}");
    }

    #[test]
    fn board_tools_read_and_edit_every_card_field() {
        let (mut state, live_run) = board_live_state();
        comms_reply(&mut state, &live_run, "board_create", r#"{"name":"team"}"#);
        let card = comms_reply(
            &mut state,
            &live_run,
            "card_create",
            r#"{"board_name":"team","title":"full","description":"desc","assignee":"kins","priority":"high","tags":["a","b"],"start_date":"2026-09-01","due_date":"2026-10-01","estimate":5}"#,
        );
        assert!(card.contains(r#""ok":true"#), "card: {card}");
        for needle in [
            r#""description":"desc""#,
            r#""assignee":"kins""#,
            r#""priority":"high""#,
            r#""start_date":"2026-09-01""#,
            r#""due_date":"2026-10-01""#,
            r#""estimate":5"#,
        ] {
            assert!(card.contains(needle), "card carries {needle}: {card}");
        }
        let body = &card[card.find(r#""result":"#).unwrap_or(0)..];
        let start = body.find(r#""id":""#).map(|i| i + 6).unwrap_or(0);
        let rest = &body[start..];
        let id = rest[..rest.find('"').unwrap_or(0)].to_string();
        // card_update edits every field, including a WIP-checked column move.
        let updated = comms_reply(
            &mut state,
            &live_run,
            "card_update",
            &format!(
                r#"{{"card_id":{},"title":"full2","description":"d2","column":"Todo","assignee":"sam","priority":"low","tags":["c"],"start_date":"2026-09-02","due_date":"2026-11-01","estimate":8,"progress":40}}"#,
                crate::ipc::mcp::escape_json(&id)
            ),
        );
        assert!(updated.contains(r#""ok":true"#), "updated: {updated}");
        for needle in [
            r#""title":"full2""#,
            r#""column":"Todo""#,
            r#""assignee":"sam""#,
            r#""start_date":"2026-09-02""#,
            r#""estimate":8"#,
            r#""progress":40"#,
        ] {
            assert!(updated.contains(needle), "updated carries {needle}: {updated}");
        }
        // Empty strings clear the clearable fields.
        let cleared = comms_reply(
            &mut state,
            &live_run,
            "card_update",
            &format!(
                r#"{{"card_id":{},"assignee":"","start_date":"","due_date":"","estimate":""}}"#,
                crate::ipc::mcp::escape_json(&id)
            ),
        );
        assert!(cleared.contains(r#""ok":true"#), "cleared: {cleared}");
        assert!(cleared.contains(r#""assignee":"""#), "assignee cleared: {cleared}");
        assert!(cleared.contains(r#""start_date":null"#), "start cleared: {cleared}");
        assert!(cleared.contains(r#""estimate":null"#), "estimate cleared: {cleared}");
        // board_get reads every field back.
        let got = comms_reply(
            &mut state,
            &live_run,
            "board_get",
            r#"{"board_name":"team"}"#,
        );
        assert!(got.contains("full2") && got.contains("Todo"), "get: {got}");
        assert!(got.contains("start_date") && got.contains("estimate"), "get: {got}");
        // Bad estimate values fail loudly, never silently.
        let bad = comms_reply(
            &mut state,
            &live_run,
            "card_update",
            &format!(
                r#"{{"card_id":{},"estimate":"lots"}}"#,
                crate::ipc::mcp::escape_json(&id)
            ),
        );
        assert!(bad.contains("estimate must be a number"), "bad estimate: {bad}");
    }
}
