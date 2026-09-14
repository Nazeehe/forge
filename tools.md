# Forge tools vs blueprint

Source: `error_logs.md` §11. Tick the box (`[ ]` → `[x]`) next to every
tool to build next. DONE tools are already in `src/mcp.rs`.

## Communication and control (5/15 done)

DONE: `ask_session`, `send_response`, `tell_session`, `ack_message`,
`list_sessions`

- [ ] #6 `send_file` — offer one bounded relative file
- [ ] #7 `accept_file` — atomic private non-overwriting receive
- [ ] #8 `compact_session` — queue `/compact` through idle injection
- [ ] #9 `schedule_prompt` — future self-injection timer
- [ ] #10 `cancel_scheduled_prompt` — cancel the timer
- [ ] #11 `message_user` — badge the TUI, optional remote forward
- [ ] #12 `screenshot` — macOS window match to PNG image block
- [ ] #13 `start_session` — create validated session, return ID/name
- [ ] #14 `set_session_status` — replace caller sticky status
- [ ] #15 `clear_session_status` — clear it

## Memory (0/7 done)

- [ ] #16 `memory_save` — kind/title/content plus scope and tags
- [ ] #17 `memory_search` — ranked matches at requested detail
- [ ] #18 `memory_get` — full record by ID
- [ ] #19 `memory_get_context` — curated bounded scope context
- [ ] #20 `memory_record_event` — scoped event, returns event ID
- [ ] #21 `memory_timeline` — reverse chronological events
- [ ] #22 `memory_archive` — archive confirmation

## Checklists and tasks (0/10 done)

- [ ] #23 `checklist_get_active` — active list, progress, tasks
- [ ] #24 `checklist_get_archived` — archived lists and summaries
- [ ] #25 `checklist_create` — UUID, archives prior workspace list
- [ ] #26 `checklist_complete` — archive with summary
- [ ] #27 `task_create` — task UUID at position
- [ ] #28 `task_update` — title and blocked reason
- [ ] #29 `task_set_status` — validated state update
- [ ] #30 `task_set_focus` — make task sole focus
- [ ] #31 `task_reorder` — transactional reorder
- [ ] #32 `task_add_note` — attributed note

## Walkthrough (3/5 done)

DONE: `walkthrough_start`, `walkthrough_answer`, `walkthrough_end`

- [ ] #34 `walkthrough_add_step` — insert step (model method exists)
- [ ] #35 `walkthrough_update` — update step (model method exists)

## Whiteboard (0/9 done)

- [ ] #38 `whiteboard_start` — board ID, URL, revision; opens browser
- [ ] #39 `whiteboard_list` — session-filtered summaries
- [ ] #40 `whiteboard_open` — URL/revision/count; opens browser
- [ ] #41 `whiteboard_add` — atomic element create
- [ ] #42 `whiteboard_update` — atomic full replacement
- [ ] #43 `whiteboard_delete` — deleted/missing IDs
- [ ] #44 `whiteboard_highlight` — transient overlay, no revision
- [ ] #45 `whiteboard_answer` — append chat answer, clear pending
- [ ] #46 `whiteboard_end` — complete, read-only, persist

## Visual, projects, terminal (0/11 done)

- [ ] #47 `visual_show` — publish path or mermaid/html content
- [ ] #48 `project_create` — new registry record
- [ ] #49 `project_get` — full project
- [ ] #50 `project_list` — all projects
- [ ] #51 `project_update` — display fields and root path
- [ ] #52 `project_delete` — cascading deletion
- [ ] #53 `project_path_add` — add path to project
- [ ] #54 `project_path_remove` — remove path from project
- [ ] #55 `terminal_exec` — supervised PTY command (Unix only)
- [ ] #56 `terminal_read` — cursor-based output/status (Unix only)
- [ ] #57 `terminal_send` — Ctrl-C only (Unix only)

## Clikan, independent server (0/8 done)

Only a `clikan_enabled` config flag exists; no server yet.

- [ ] #58 `board_list` — boards and per-column counts
- [ ] #59 `board_get` — columns/cards with WIP and progress
- [ ] #60 `card_create` — new card ID/details
- [ ] #61 `card_move` — WIP-limited move
- [ ] #62 `card_update` — partial update, progress clamped
- [ ] #63 `card_delete` — confirmation
- [ ] #64 `card_assign` — explicit or session-name assignee
- [ ] #65 `board_create` — handler uses default columns today
