//! Grok adapter: `~/.grok/sessions/<cwd>/<id>/updates.jsonl` (an on-disk ACP
//! `session/update` stream) plus `events.jsonl` for turns and permissions.
//!
//! Owned by the grok-adapter task. See `docs/architecture/spine.md`.
//!
//! `updates.jsonl` is one JSON-RPC notification per line, appended as the
//! turn runs. The two methods that appear are `session/update` (plain ACP)
//! and `_x.ai/session/update` (xAI's extensions), both carrying
//! `params.update.sessionUpdate` as the discriminator:
//!
//! ```text
//! {"timestamp":1787956220,                       // unix SECONDS
//!  "method":"session/update",
//!  "params":{"sessionId":"…",
//!            "update":{"sessionUpdate":"agent_message_chunk",
//!                      "content":{"type":"text","text":"…"}},
//!            "_meta":{"eventId":"<session>-<n>","agentTimestampMs":1787956221679,…}}}
//! ```
//!
//! Every `sessionUpdate` seen across 22 sessions, and what becomes of it:
//! `user_message_chunk`, `agent_message_chunk`, `agent_thought_chunk` fold
//! into blocks; `tool_call` / `tool_call_update` are the cards;
//! `turn_completed` ends the turn; `task_backgrounded` / `task_completed`
//! and `subagent_spawned` / `subagent_finished` are long-running work that
//! outlives the line that started it (below); `plan` is the checklist.
//! `session_recap`, `current_mode_update` and `image_compressed` are still
//! ignored — none of them is content, and none means "waiting for you".
//!
//! Work that outlives its own tool call comes in two flavours, and both
//! leave a card grok has already marked `completed` while the thing it
//! names is still running:
//!
//! - a **backgrounded task** — `task_backgrounded` names the card, then the
//!   card's own `tool_call_update` says `completed` with the text
//!   "Background task … started". The card is held open instead, and
//!   `task_completed` (which quotes only a task id, hence the map) closes
//!   it with the real output and exit code.
//! - a **subagent** — the `spawn_subagent` card closes the same way with
//!   "Subagent started in background". The child gets a second card of its
//!   own, keyed by `subagent_id`, open until `subagent_finished`. The child
//!   writes an entire session of its own under
//!   `~/.grok/sessions/<cwd>/<child_session_id>/`; NOTHING it streams
//!   appears in the parent's `updates.jsonl`, so watching it live would
//!   mean a second adapter on that directory.
//!
//! `events.jsonl` beside it is a different shape entirely — flat objects
//! keyed by `type`, with an ISO-8601 `ts`, no `params` wrapper:
//!
//! ```text
//! {"ts":"2026-08-28T22:30:22.790Z","type":"tool_completed",
//!  "tool_name":"read_file","duration_ms":0,"outcome":"success","tool_call_id":"call-…-0"}
//! ```
//!
//! [observed: grok 1.0.13, 2026-09-02, 22 sessions under ~/.grok/sessions]

use super::{clip, now_ms, Adapter, Kind, Phase, ToolCategory, ToolStatus};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{Read as _, Seek, SeekFrom};
use std::path::PathBuf;

/// How much of a tool's input a card shows, and how much of its output.
const INPUT_MAX: usize = 400;
const OUTPUT_MAX: usize = 2000;

pub struct GrokAdapter {
    dir: PathBuf,
    updates: Tail,
    events: Tail,
    /// 1-based line number of the next `updates.jsonl` line — the ordinal
    /// that ids and turn names are built from. Stable across re-reads
    /// because the file is append-only.
    ordinal: u64,
    /// The prose or reasoning block being accumulated, if one is open.
    run: Option<Run>,
    /// The turn a `turn_completed` closes: the ordinal of the user message
    /// that opened it. "0" before any user message has been seen.
    turn: String,
    /// Tool ids `updates.jsonl` called `completed` in this poll and the one
    /// before it, so `events.jsonl` can correct one to `failed`. Two polls
    /// is plenty — the two lines are written in the same millisecond — and
    /// dropping the rest keeps a long session's bookkeeping bounded.
    completed: [HashSet<String>; 2],
    /// Backgrounded task id → the tool card that launched it. The two are
    /// often the same string, but not always: a task grok has to name for
    /// itself gets a fresh uuid (9 of 29 observed), and `task_completed`
    /// only ever quotes the TASK id. Kept for the life of the session; a
    /// task can outlive several turns.
    tasks: HashMap<String, String>,
    /// Cards whose task is still running in the background, so the
    /// `completed` grok writes the moment the launch returns can be held
    /// back — the command it describes has not finished.
    background: HashSet<String>,
    /// The ordinal of the first `plan` line. Every later revision upserts
    /// that same row rather than stacking a second checklist under it.
    plan: Option<u64>,
}

/// A run of consecutive same-kind chunks, folded into one block. Grok emits
/// one chunk per completed block rather than per token, and at 1.0.13 every
/// observed run was length 1 (a tool call always separates two blocks) —
/// but the ACP stream permits a run, so we fold.
struct Run {
    thought: bool,
    id: String,
    text: String,
    ts: u64,
}

/// The adapter for a Grok session, or `None` when no session dir exists.
///
/// `updates.jsonl` need not exist yet: a session that has not finished a
/// turn has only `summary.json`, and the file appears under us.
pub fn open(session_id: &str) -> Option<GrokAdapter> {
    let dir = crate::grok::session_dir(session_id)?;
    Some(GrokAdapter {
        updates: Tail::new(dir.join("updates.jsonl")),
        events: Tail::new(dir.join("events.jsonl")),
        dir,
        ordinal: 1,
        run: None,
        turn: "0".to_string(),
        completed: Default::default(),
        tasks: Default::default(),
        background: Default::default(),
        plan: None,
    })
}

impl Adapter for GrokAdapter {
    fn bootstrap(&mut self) -> Vec<(u64, Kind)> {
        self.poll()
    }

    fn poll(&mut self) -> Vec<(u64, Kind)> {
        let (Some(update_lines), Some(event_lines)) = (self.updates.take(), self.events.take())
        else {
            return self.restart();
        };
        self.merge(update_lines, event_lines)
    }

    fn watch_paths(&self) -> Vec<PathBuf> {
        // The directory too: the first turn CREATES updates.jsonl, and a
        // watch on a path that does not exist yet never fires.
        vec![self.updates.path.clone(), self.events.path.clone(), self.dir.clone()]
    }
}

impl GrokAdapter {
    /// A file was truncated or replaced under us: drop everything we hold
    /// and rebuild from both files, behind a `Reset`.
    fn restart(&mut self) -> Vec<(u64, Kind)> {
        self.updates.rewind();
        self.events.rewind();
        self.ordinal = 1;
        self.run = None;
        self.turn = "0".to_string();
        self.completed = Default::default();
        self.tasks = Default::default();
        self.background = Default::default();
        self.plan = None;
        let mut out = vec![(now_ms(), Kind::Reset)];
        let updates = self.updates.take().unwrap_or_default();
        let events = self.events.take().unwrap_or_default();
        out.extend(self.merge(updates, events));
        out
    }

    /// Both files parsed and interleaved by timestamp. `updates.jsonl` is
    /// parsed first so the tool statuses it reports are known before
    /// `events.jsonl` gets a chance to correct one.
    fn merge(&mut self, updates: Vec<String>, events: Vec<String>) -> Vec<(u64, Kind)> {
        let mut from_updates = self.parse_updates(&updates);
        let mut from_events = self.parse_events(&events);
        monotonic(&mut from_updates);
        monotonic(&mut from_events);
        let mut all: Vec<(u64, u8, Kind)> = from_updates
            .into_iter()
            .map(|(ts, k)| (ts, 0, k))
            .chain(from_events.into_iter().map(|(ts, k)| (ts, 1, k)))
            .collect();
        // Stable, so each file keeps its own order; the tag puts updates
        // first on a tie, which is how a `tool_completed` error (same ms as
        // the `completed` it corrects) lands after the status it fixes.
        all.sort_by_key(|(ts, src, _)| (*ts, *src));
        self.completed.swap(0, 1);
        self.completed[0].clear();
        all.into_iter().map(|(ts, _, k)| (ts, k)).collect()
    }

    fn parse_updates(&mut self, lines: &[String]) -> Vec<(u64, Kind)> {
        let mut out = Vec::new();
        for line in lines {
            let ordinal = self.ordinal;
            self.ordinal += 1;
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            let params = &v["params"];
            let update = &params["update"];
            let Some(sort) = update["sessionUpdate"].as_str() else { continue };
            let ts = ts_ms(&v, params);
            match sort {
                "user_message_chunk" => {
                    // Grok injects background-task notices as user chunks it
                    // hides from its own scrollback; they are the engine
                    // talking to itself, not a person opening a turn.
                    if update["_meta"]["hideFromScrollback"].is_null() {
                        self.user_chunk(ordinal, ts, chunk_text(&update["content"]), &mut out);
                    }
                }
                "agent_message_chunk" => {
                    self.agent_chunk(false, ordinal, ts, chunk_text(&update["content"]), &mut out)
                }
                "agent_thought_chunk" => {
                    self.agent_chunk(true, ordinal, ts, chunk_text(&update["content"]), &mut out)
                }
                "tool_call" => {
                    if let Some(k) = tool_card(update, ToolStatus::Pending) {
                        self.close_run(&mut out);
                        out.push((ts, k));
                    }
                }
                "tool_call_update" => {
                    self.close_run(&mut out);
                    self.tool_update(update, ts, &mut out);
                }
                // xAI's end-of-turn: the usage block is not ours to carry.
                "turn_completed" => {
                    self.close_run(&mut out);
                    let reason = match update["stop_reason"].as_str() {
                        Some("end_turn") => "completed",
                        Some("cancelled") => "interrupted",
                        Some("error") => "error",
                        _ => "unknown",
                    };
                    out.push((ts, Kind::TurnEnded { turn: self.turn.clone(), reason: reason.into() }));
                }
                // The card grok just issued belongs to a process that
                // outlives it; hold the card open and remember the task.
                "task_backgrounded" => {
                    self.close_run(&mut out);
                    self.task_backgrounded(update, ts, &mut out);
                }
                "task_completed" => {
                    self.close_run(&mut out);
                    self.task_completed(update, ts, &mut out);
                }
                "subagent_spawned" => {
                    self.close_run(&mut out);
                    if let Some(k) = subagent_card(update) {
                        out.push((ts, k));
                    }
                }
                "subagent_finished" => {
                    self.close_run(&mut out);
                    if let Some(k) = subagent_result(update) {
                        out.push((ts, k));
                    }
                }
                // The checklist grok keeps.
                "plan" => {
                    self.close_run(&mut out);
                    self.plan(ordinal, update, ts, &mut out);
                }
                // `session_recap`, `current_mode_update`, `image_compressed`:
                // state the phone does not render, and none of them means
                // "waiting for you".
                _ => {}
            }
        }
        out
    }

    /// A person spoke: a new turn, then their words. Emitted on the first
    /// chunk rather than held until the run closes, so the phone echoes what
    /// was just typed instead of waiting for the model's first block; a
    /// second consecutive chunk re-emits the same id with the text grown.
    fn user_chunk(&mut self, ordinal: u64, ts: u64, text: String, out: &mut Vec<(u64, Kind)>) {
        if let Some(run) = self.run.as_mut().filter(|r| r.id.starts_with('u')) {
            run.text.push_str(&text);
            out.push((ts, Kind::UserMessage { id: run.id.clone(), text: run.text.clone() }));
            return;
        }
        self.close_run(out);
        self.turn = ordinal.to_string();
        let id = format!("u{ordinal}");
        out.push((ts, Kind::TurnStarted { turn: self.turn.clone() }));
        out.push((ts, Kind::UserMessage { id: id.clone(), text: text.clone() }));
        self.run = Some(Run { thought: false, id, text, ts });
    }

    fn agent_chunk(
        &mut self,
        thought: bool,
        ordinal: u64,
        ts: u64,
        text: String,
        out: &mut Vec<(u64, Kind)>,
    ) {
        match self.run.as_mut() {
            Some(run) if run.thought == thought && !run.id.starts_with('u') => {
                run.text.push_str(&text);
                run.ts = ts;
            }
            _ => {
                self.close_run(out);
                self.run = Some(Run { thought, id: format!("a{ordinal}"), text, ts });
            }
        }
        let Some(run) = self.run.as_ref() else { return };
        out.push((ts, block(run, false)));
    }

    /// Close the open block, if any: one last snapshot with `done`. Stamped
    /// with the block's own last timestamp, not the line that ended it, so
    /// it sorts before whatever comes next.
    fn close_run(&mut self, out: &mut Vec<(u64, Kind)>) {
        let Some(run) = self.run.take() else { return };
        if run.id.starts_with('u') {
            // A user message is complete when it is emitted; there is no
            // `done` on the kind and re-sending it would only repeat text.
            return;
        }
        out.push((run.ts, block(&run, true)));
    }

    /// A `tool_call_update` is two different lines wearing one name. With no
    /// `status` it is the call being filled in as it starts — the pretty
    /// title, the ACP kind, the real input — which the spine has no kind for,
    /// so it re-issues the card (upsert by id) with everything known. With a
    /// `status` it is the terminal result. [observed: grok 1.0.13]
    fn tool_update(&mut self, update: &Value, ts: u64, out: &mut Vec<(u64, Kind)>) {
        let Some(id) = update["toolCallId"].as_str() else { return };
        let status = update["status"].as_str().map(tool_status);
        if status.is_none() {
            if let Some(k) = tool_card(update, ToolStatus::Running) {
                out.push((ts, k));
            }
        }
        // A backgrounded card is closed by grok the instant the LAUNCH
        // returns — `completed`, output "Background task … started" — while
        // the command itself runs on for minutes. That is the placeholder
        // the phone was left holding. Drop every update on such a card: the
        // note put there by `task_backgrounded` stands (an absent event
        // changes nothing), and `task_completed` is what really ends it.
        // The re-issued ToolCall above still passes, because it is how the
        // card gets its real title, and it carries no output of its own.
        if self.background.contains(id) {
            return;
        }
        let status = status.unwrap_or(ToolStatus::Running);
        let output = tool_output(&update["content"]);
        // The start-of-run line only earns a second event when it brought
        // something to show (a diff, or a command's output so far).
        if status != ToolStatus::Running || output.is_some() {
            out.push((ts, Kind::ToolCallUpdate { id: id.to_string(), status, output }));
        }
        if status == ToolStatus::Completed {
            self.completed[0].insert(id.to_string());
        }
    }

    /// Grok moved a command off the turn: it keeps running while the model
    /// goes on with something else, and its real result arrives later as a
    /// `task_completed` — often several turns later, sometimes never.
    ///
    /// ```text
    /// {"sessionUpdate":"task_backgrounded","tool_call_id":"call-…-10",
    ///  "task_id":"call-…-10","command":"…","cwd":"…","output_file":"…",
    ///  "description":"Run agy print mode to observe auth prompt"}
    /// ```
    ///
    /// `task_id` equals `tool_call_id` for 20 of 29 observed tasks and is a
    /// fresh uuid for the other 9, so the pairing has to be remembered here:
    /// it is the only line that ever carries both. [observed: grok 1.0.13]
    fn task_backgrounded(&mut self, update: &Value, ts: u64, out: &mut Vec<(u64, Kind)>) {
        let Some(call) = update["tool_call_id"].as_str() else { return };
        if let Some(task) = update["task_id"].as_str() {
            self.tasks.insert(task.to_string(), call.to_string());
        }
        self.background.insert(call.to_string());
        let what = one_line(&update["description"]);
        let what = if what.is_empty() { one_line(&update["command"]) } else { what };
        let note = match what.is_empty() {
            true => "running in the background…".to_string(),
            false => format!("running in the background… {what}"),
        };
        out.push((
            ts,
            Kind::ToolCallUpdate {
                id: call.to_string(),
                status: ToolStatus::Running,
                output: Some(clip(&note, OUTPUT_MAX)),
            },
        ));
    }

    /// The background command finished. Everything is under `task_snapshot`,
    /// which names only the TASK id — hence the map — and carries the whole
    /// captured `output`, an `exit_code`, a `signal` and a `truncated` flag.
    ///
    /// A task whose backgrounding we never saw (a session resumed past it)
    /// has no card to update, so one is opened and closed in the same
    /// breath rather than dropping the work on the floor.
    fn task_completed(&mut self, update: &Value, ts: u64, out: &mut Vec<(u64, Kind)>) {
        let snap = &update["task_snapshot"];
        let Some(task) = snap["task_id"].as_str() else { return };
        let id = match self.tasks.remove(task) {
            Some(call) => {
                self.background.remove(&call);
                call
            }
            None => {
                let id = format!("task-{task}");
                let title = one_line(&snap["description"]);
                let command = one_line(&snap["command"]);
                out.push((
                    ts,
                    Kind::ToolCall {
                        id: id.clone(),
                        tool: snap["kind"].as_str().unwrap_or("task").to_string(),
                        title: if title.is_empty() { command.clone() } else { title },
                        category: ToolCategory::Execute,
                        input: clip(&command, INPUT_MAX),
                        status: ToolStatus::Running,
                    },
                ));
                id
            }
        };
        let ok = snap["signal"].is_null() && snap["exit_code"].as_i64().unwrap_or(0) == 0;
        out.push((
            ts,
            Kind::ToolCallUpdate {
                id,
                status: if ok { ToolStatus::Completed } else { ToolStatus::Failed },
                output: Some(task_result(snap)),
            },
        ));
    }

    /// The checklist, as one thought block that is rewritten in place. The
    /// id is the ordinal of the FIRST plan line of the session, so the 2 or
    /// 3 revisions a session writes upsert one row instead of stacking.
    fn plan(&mut self, ordinal: u64, update: &Value, ts: u64, out: &mut Vec<(u64, Kind)>) {
        let Some(entries) = update["entries"].as_array().filter(|e| !e.is_empty()) else { return };
        let id = format!("p{}", *self.plan.get_or_insert(ordinal));
        let text = entries
            .iter()
            .map(|e| {
                // `priority` is on every entry and is `medium` on all 117
                // observed: it says nothing, so it is not shown.
                let mark = match e["status"].as_str() {
                    Some("completed") => "[x]",
                    Some("in_progress") => "[~]",
                    _ => "[ ]",
                };
                format!("{mark} {}", e["content"].as_str().unwrap_or_default())
            })
            .collect::<Vec<_>>()
            .join("\n");
        out.push((ts, Kind::AgentThought { id, text: clip(&text, OUTPUT_MAX), done: true }));
    }

    fn parse_events(&mut self, lines: &[String]) -> Vec<(u64, Kind)> {
        // Permissions come in pairs. Under yolo mode grok still asks and
        // answers itself: 1010 of 1013 observed requests resolved in under
        // 5 ms. A pair that is already closed by the time we read the file
        // never needed anyone, so both halves are dropped; a request still
        // open at the end of the batch is the real thing.
        let mut rows: Vec<Option<(u64, Kind)>> = Vec::new();
        let mut waiting: HashMap<String, usize> = HashMap::new();
        for line in lines {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            let Some(kind) = v["type"].as_str() else { continue };
            let ts = v["ts"].as_str().and_then(iso_ms).unwrap_or_else(now_ms);
            let tool = v["tool_name"].as_str().unwrap_or("tool").to_string();
            match kind {
                "permission_requested" => {
                    waiting.insert(tool.clone(), rows.len());
                    rows.push(Some((
                        ts,
                        Kind::Phase { phase: Phase::NeedsYou, detail: format!("permission: {tool}") },
                    )));
                }
                "permission_resolved" => match waiting.remove(&tool) {
                    Some(at) => {
                        rows[at] = None;
                    }
                    None => rows.push(Some((
                        ts,
                        Kind::Phase { phase: Phase::Working, detail: String::new() },
                    ))),
                },
                // The turn is already announced by updates.jsonl's user
                // chunk; only the phase is worth repeating.
                "turn_started" => {
                    rows.push(Some((ts, Kind::Phase { phase: Phase::Working, detail: String::new() })))
                }
                "turn_ended" => {
                    rows.push(Some((ts, Kind::Phase { phase: Phase::Idle, detail: String::new() })))
                }
                // The one thing events.jsonl knows that updates.jsonl does
                // not: 17 of 36 failing tools were written to updates.jsonl
                // as `completed`. Correct only a status we saw as completed,
                // so a card whose result has not arrived is left alone.
                "tool_completed" if v["outcome"].as_str() != Some("success") => {
                    let Some(id) = v["tool_call_id"].as_str() else { continue };
                    if self.completed[0].remove(id) || self.completed[1].remove(id) {
                        rows.push(Some((
                            ts,
                            Kind::ToolCallUpdate {
                                id: id.to_string(),
                                status: ToolStatus::Failed,
                                output: None,
                            },
                        )));
                    }
                }
                // `first_token`, `loop_started`, `tool_started`, a successful
                // `tool_completed`, `phase_changed` (37 714 of them across 22
                // sessions — a per-token status the spine has no use for),
                // `mcp_*`, `yolo_toggled`.
                _ => {}
            }
        }
        rows.into_iter().flatten().collect()
    }
}

/// Drag each timestamp up to the one before it. A file's own order is the
/// truth — the stamps only exist so the two files can be interleaved — and
/// grok's are not always sorted: a background task's `tool_call_update` is
/// stamped when the task started, up to 43 s before the line above it
/// (55 of 4122 lines observed). Sorting on the raw stamp would deal a
/// tool's result out ahead of its own card. [observed: grok 1.0.13]
fn monotonic(evs: &mut [(u64, Kind)]) {
    let mut floor = 0;
    for (ts, _) in evs.iter_mut() {
        floor = floor.max(*ts);
        *ts = floor;
    }
}

fn block(run: &Run, done: bool) -> Kind {
    let (id, text) = (run.id.clone(), run.text.clone());
    if run.thought {
        Kind::AgentThought { id, text, done }
    } else {
        Kind::AgentText { id, text, done }
    }
}

/// A chunk's words. `content` is a single ACP content block, text in every
/// observed case but one — a person can paste an image into a prompt.
fn chunk_text(content: &Value) -> String {
    match content["type"].as_str() {
        Some("text") => content["text"].as_str().unwrap_or_default().to_string(),
        Some("image") => {
            format!("[image {}]", content["mimeType"].as_str().unwrap_or("attached"))
        }
        _ => String::new(),
    }
}

/// A tool card from either the `tool_call` line or the `tool_call_update`
/// that fills it in.
fn tool_card(update: &Value, status: ToolStatus) -> Option<Kind> {
    let id = update["toolCallId"].as_str()?;
    let xai = &update["_meta"]["x.ai/tool"];
    let title = update["title"].as_str().unwrap_or_default();
    let tool = xai["name"]
        .as_str()
        .or_else(|| (!title.is_empty()).then_some(title))
        .unwrap_or("tool");
    Some(Kind::ToolCall {
        id: id.to_string(),
        tool: tool.to_string(),
        title: if title.is_empty() { tool.to_string() } else { title.to_string() },
        category: category(update["kind"].as_str(), xai["kind"].as_str()),
        input: summarize(&update["rawInput"]),
        status,
    })
}

/// A subagent, as a card of its own.
///
/// ```text
/// {"sessionUpdate":"subagent_spawned","subagent_id":"01a02be1-…",
///  "parent_session_id":"…","child_session_id":"01a02be1-…",
///  "subagent_type":"general-purpose","description":"SWFL agent ICP research",
///  "capability_mode":"all","model":"grok-4.6"}
/// ```
///
/// The `spawn_subagent` tool call that issued it gets its own card, closed
/// a millisecond later with "Subagent started in background" — that card is
/// the launch. This one is the child's life, and it stays open for the
/// minutes the child actually runs. Its id is the subagent's, which is the
/// only handle `subagent_finished` carries.
fn subagent_card(update: &Value) -> Option<Kind> {
    let id = update["subagent_id"].as_str()?;
    let kind = update["subagent_type"].as_str().unwrap_or("subagent");
    let title = one_line(&update["description"]);
    let input = [("type", kind), ("model", update["model"].as_str().unwrap_or_default())]
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ");
    Some(Kind::ToolCall {
        id: format!("sub-{id}"),
        tool: "subagent".to_string(),
        title: if title.is_empty() { kind.to_string() } else { title },
        category: ToolCategory::Think,
        input: clip(&input, INPUT_MAX),
        status: ToolStatus::Running,
    })
}

/// The child is done: a `status`, a little accounting, and the summary it
/// handed back. Nothing the child did on the way is here — it wrote its own
/// session under `~/.grok/sessions/<cwd>/<child_session_id>/`, and not one
/// line of it reaches the parent's `updates.jsonl`. [observed: grok 1.0.13]
fn subagent_result(update: &Value) -> Option<Kind> {
    let id = update["subagent_id"].as_str()?;
    let status = match update["status"].as_str() {
        Some("completed") => ToolStatus::Completed,
        Some("cancelled") => ToolStatus::Cancelled,
        _ => ToolStatus::Failed,
    };
    let mut head = update["status"].as_str().unwrap_or("finished").to_string();
    if let Some(n) = update["tool_calls"].as_u64() {
        head.push_str(&format!(" · {n} tool calls"));
    }
    if let Some(ms) = update["duration_ms"].as_u64() {
        head.push_str(&format!(" · {} s", ms / 1000));
    }
    let body = update["output"].as_str().unwrap_or_default();
    let text = if body.is_empty() { head } else { format!("{head}\n{body}") };
    Some(Kind::ToolCallUpdate { id: format!("sub-{id}"), status, output: Some(clip(&text, OUTPUT_MAX)) })
}

/// What a finished background task showed: the reason it stopped when that
/// is not "cleanly", then its captured output. `exit_code` is null whenever
/// a `signal` is set (`killed`, `timeout`, `signal 15`), and `truncated`
/// says grok itself already cut the capture. [observed: grok 1.0.13]
fn task_result(snap: &Value) -> String {
    let mut marks = Vec::new();
    match snap["signal"].as_str() {
        Some(sig) => marks.push(format!("[{sig}]")),
        None => match snap["exit_code"].as_i64() {
            Some(0) | None => {}
            Some(code) => marks.push(format!("[exit {code}]")),
        },
    }
    if snap["truncated"].as_bool() == Some(true) {
        marks.push("[truncated]".to_string());
    }
    let body = snap["output"].as_str().unwrap_or_default();
    if body.is_empty() {
        marks.push("(no output)".to_string());
        return marks.join(" ");
    }
    let text =
        if marks.is_empty() { body.to_string() } else { format!("{} {body}", marks.join(" ")) };
    clip(&text, OUTPUT_MAX)
}

/// A JSON string field as one line of card text: whitespace collapsed, so a
/// heredoc of a command does not arrive as forty lines.
fn one_line(v: &Value) -> String {
    v.as_str().unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The card's mark. The ACP `kind` is only on the fill-in line; the first
/// line carries grok's own richer vocabulary under `_meta["x.ai/tool"]`, so
/// that is mapped the way grok itself maps it (measured by pairing the two
/// lines of all 1013 calls). [observed: grok 1.0.13]
fn category(acp: Option<&str>, xai: Option<&str>) -> ToolCategory {
    match acp.or(xai) {
        Some("read") => ToolCategory::Read,
        Some("edit" | "write") => ToolCategory::Edit,
        Some("execute") => ToolCategory::Execute,
        Some("search") => ToolCategory::Search,
        Some("fetch" | "web_fetch") => ToolCategory::Fetch,
        Some("think" | "plan") => ToolCategory::Think,
        // `list`, `image_gen`, `task`, `search_tool`,
        // `background_task_action`, `kill_task_action` — grok calls these
        // `other` on the fill-in line too.
        _ => ToolCategory::Other,
    }
}

fn tool_status(s: &str) -> ToolStatus {
    match s {
        "pending" => ToolStatus::Pending,
        "in_progress" => ToolStatus::Running,
        "completed" => ToolStatus::Completed,
        "failed" => ToolStatus::Failed,
        "cancelled" => ToolStatus::Cancelled,
        _ => ToolStatus::Running,
    }
}

/// `rawInput` on one line: `k=v` pairs, minus the `variant` tag grok uses to
/// name the input's own shape.
fn summarize(raw: &Value) -> String {
    let text = match raw.as_object() {
        Some(map) => map
            .iter()
            .filter(|(k, _)| k.as_str() != "variant")
            .map(|(k, v)| match v.as_str() {
                Some(s) => format!("{k}={s}"),
                None => format!("{k}={v}"),
            })
            .collect::<Vec<_>>()
            .join(" "),
        None if raw.is_null() => String::new(),
        None => raw.to_string(),
    };
    clip(&text.split_whitespace().collect::<Vec<_>>().join(" "), INPUT_MAX)
}

/// What a tool showed. `content` is an array of ACP tool-call content:
/// `{"type":"content","content":{"type":"text"|"image",…}}` or a
/// `{"type":"diff","path","oldText","newText"}`. [observed: grok 1.0.13]
fn tool_output(content: &Value) -> Option<String> {
    let parts = content.as_array()?;
    let text = parts
        .iter()
        .filter_map(|p| match p["type"].as_str() {
            Some("diff") => Some(format!("[diff {}]", p["path"].as_str().unwrap_or("?"))),
            Some("content") => match p["content"]["type"].as_str() {
                Some("text") => Some(p["content"]["text"].as_str()?.to_string()),
                // Base64 image payloads run to megabytes; the phone gets the
                // fact, not the bytes.
                Some("image") => Some(format!(
                    "[image {}]",
                    p["content"]["mimeType"].as_str().unwrap_or("attached")
                )),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then(|| clip(&text, OUTPUT_MAX))
}

/// When a line happened: xAI's own millisecond stamp, else the JSON-RPC
/// envelope's unix seconds, else now.
fn ts_ms(v: &Value, params: &Value) -> u64 {
    params["_meta"]["agentTimestampMs"]
        .as_u64()
        .or_else(|| v["timestamp"].as_u64().map(|s| s * 1000))
        .unwrap_or_else(now_ms)
}

/// "2026-08-28T22:30:22.790Z" → ms. events.jsonl stamps ISO where
/// updates.jsonl stamps numbers; this is the same civil-days arithmetic as
/// `remote_api::parse_iso_secs`, kept to the millisecond.
fn iso_ms(s: &str) -> Option<u64> {
    let (date, rest) = s.split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let time = rest.trim_end_matches('Z');
    let time = time.split(['+', '-']).next().unwrap_or(time);
    let mut t = time.split(':');
    let (h, mi) = (t.next()?.parse::<i64>().ok()?, t.next()?.parse::<i64>().ok()?);
    let secs_part = t.next()?;
    let (sec, frac) = match secs_part.split_once('.') {
        Some((s, f)) => (s.parse::<i64>().ok()?, format!("{f:0<3}")[..3].parse::<i64>().ok()?),
        None => (secs_part.parse::<i64>().ok()?, 0),
    };
    let (y2, m2) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * m2 + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    u64::try_from((days * 86400 + h * 3600 + mi * 60 + sec) * 1000 + frac).ok()
}

/// One append-only file read from a byte offset. Grok appends whole lines,
/// but a poll can land between a line's bytes and its newline, so the
/// trailing fragment is held until the newline arrives.
struct Tail {
    path: PathBuf,
    offset: u64,
    partial: String,
    /// Which file we have been reading, so a replacement is noticed even
    /// when the new one is already longer than the old.
    id: Option<u64>,
}

impl Tail {
    fn new(path: PathBuf) -> Self {
        Self { path, offset: 0, partial: String::new(), id: None }
    }

    fn rewind(&mut self) {
        self.offset = 0;
        self.partial.clear();
        self.id = None;
    }

    /// Whole lines since the last call. `None` means the file was truncated
    /// or replaced and the caller must rebuild from zero.
    fn take(&mut self) -> Option<Vec<String>> {
        // A session that has not completed a turn has no updates.jsonl yet;
        // it appears under us, and until it does there is nothing to say.
        let Ok(meta) = std::fs::metadata(&self.path) else { return Some(Vec::new()) };
        let id = file_id(&meta);
        if meta.len() < self.offset || self.id.is_some_and(|was| was != id) {
            return None;
        }
        self.id = Some(id);
        if meta.len() == self.offset {
            return Some(Vec::new());
        }
        let mut buf = Vec::new();
        let read = std::fs::File::open(&self.path)
            .and_then(|mut f| {
                f.seek(SeekFrom::Start(self.offset))?;
                f.read_to_end(&mut buf)
            })
            .unwrap_or(0);
        self.offset += read as u64;
        let mut text = std::mem::take(&mut self.partial);
        text.push_str(&String::from_utf8_lossy(&buf));
        let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        // Whatever follows the last newline is not a line yet.
        self.partial = lines.pop().unwrap_or_default();
        Some(lines.into_iter().filter(|l| !l.trim().is_empty()).collect())
    }
}

#[cfg(unix)]
fn file_id(meta: &std::fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::ino(meta)
}

#[cfg(not(unix))]
fn file_id(meta: &std::fs::Metadata) -> u64 {
    // No inode to ask for; a shorter file is the only rotation we can see.
    let _ = meta;
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare adapter over two paths, so the parsing can be driven from
    /// files a test writes rather than from ~/.grok.
    fn adapter(dir: &std::path::Path) -> GrokAdapter {
        GrokAdapter {
            updates: Tail::new(dir.join("updates.jsonl")),
            events: Tail::new(dir.join("events.jsonl")),
            dir: dir.to_path_buf(),
            ordinal: 1,
            run: None,
            turn: "0".to_string(),
            completed: Default::default(),
            tasks: Default::default(),
            background: Default::default(),
            plan: None,
        }
    }

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("spine-grok-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(dir: &std::path::Path, file: &str, body: &str) {
        std::fs::write(dir.join(file), body).unwrap();
    }

    fn append(dir: &std::path::Path, file: &str, body: &str) {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(file)).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    // Lines below are copied from ~/.grok/sessions/…/5d992ea4-… and
    // …/01a02bd1-…, with long prose and rawOutput shortened. Shapes are
    // verbatim. [observed: grok 1.0.13, 2026-09-02]
    const USER: &str = r#"{"timestamp":1787956220,"method":"session/update","params":{"sessionId":"S","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"build a weather page"},"_meta":{"modelId":"grok-4.6","promptIndex":0}},"_meta":{"eventId":"S-2","agentTimestampMs":1787956219612}}}"#;
    const THOUGHT: &str = r#"{"timestamp":1787956221,"method":"session/update","params":{"sessionId":"S","update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"The user wants a page. "}},"_meta":{"totalTokens":6090,"eventId":"S-43","agentTimestampMs":1787956221037,"updateType":"AgentThoughtChunk","chunkId":41}}}"#;
    const SAY: &str = r#"{"timestamp":1787956222,"method":"session/update","params":{"sessionId":"S","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"I'll start "}},"_meta":{"totalTokens":6090,"eventId":"S-61","agentTimestampMs":1787956221679,"updateType":"AgentMessageChunk","chunkId":59}}}"#;
    const CALL: &str = r#"{"timestamp":1787956222,"method":"session/update","params":{"sessionId":"S","update":{"sessionUpdate":"tool_call","toolCallId":"call-A-0","title":"read_file","rawInput":{"target_file":"/home/admin/AI-OS/CLAUDE.md"},"_meta":{"x.ai/tool":{"version":1,"name":"read_file","kind":"read","namespace":"grok_build","label":"Read","read_only":true}}},"_meta":{"totalTokens":16475,"eventId":"S-63","agentTimestampMs":1787956222786,"updateType":"ToolCall","updateParams":{"toolCallId":"call-A-0","title":"read_file","kind":"Other","status":"Pending"}}}}"#;
    const CALL_FILLED: &str = r#"{"timestamp":1787956222,"method":"session/update","params":{"sessionId":"S","update":{"sessionUpdate":"tool_call_update","toolCallId":"call-A-0","kind":"read","title":"Read `/home/admin/AI-OS/CLAUDE.md`","locations":[{"path":"/home/admin/AI-OS/CLAUDE.md"}],"rawInput":{"variant":"ReadFile","target_file":"/home/admin/AI-OS/CLAUDE.md"},"_meta":{"x.ai/tool":{"version":1,"name":"read_file","kind":"read","namespace":"grok_build","label":"Read","read_only":true}}},"_meta":{"eventId":"S-64","agentTimestampMs":1787956222787,"updateType":"ToolCallUpdate","updateParams":{"toolCallId":"call-A-0","status":null}}}}"#;
    const CALL_DONE: &str = r#"{"timestamp":1787956222,"method":"session/update","params":{"sessionId":"S","update":{"sessionUpdate":"tool_call_update","toolCallId":"call-A-0","status":"completed","content":[{"type":"content","content":{"type":"text","text":"1→# aiterm"}}],"rawOutput":{"ok":true}},"_meta":{"eventId":"S-65","agentTimestampMs":1787956222790}}}"#;
    const TURN_DONE: &str = r#"{"timestamp":1787956230,"method":"_x.ai/session/update","params":{"sessionId":"S","update":{"sessionUpdate":"turn_completed","prompt_id":"P","stop_reason":"end_turn","usage":{"inputTokens":48927,"outputTokens":1504,"totalTokens":50431,"numTurns":3}},"_meta":{"eventId":"S-1345","agentTimestampMs":1787956230527}}}"#;
    const PLAN: &str = r#"{"timestamp":1787956223,"method":"session/update","params":{"sessionId":"S","update":{"sessionUpdate":"plan","entries":[{"content":"Load skills","priority":"medium","status":"in_progress"}]},"_meta":{"eventId":"S-70","agentTimestampMs":1787956223000}}}"#;
    /// The same plan, later: the first entry done and a second one added.
    const PLAN_GROWN: &str = r#"{"timestamp":1787956240,"method":"session/update","params":{"sessionId":"S","update":{"sessionUpdate":"plan","entries":[{"content":"Load skills","priority":"medium","status":"completed"},{"content":"Serve, verify, commit","priority":"medium","status":"pending"}]},"_meta":{"totalTokens":63524,"eventId":"S-806","agentTimestampMs":1787956240196,"updateType":"Plan","updateParams":{"planSteps":2}}}}"#;

    // A backgrounded command, from ~/.grok/sessions/…/5d992ea4-… and
    // …/01a063df-…: the card, the fill-in, the `task_backgrounded` that
    // takes it off the turn, the `completed` grok writes the moment the
    // LAUNCH returns, and the `task_completed` that really ends it.
    const BG_CALL: &str = r#"{"timestamp":1787956807,"method":"session/update","params":{"sessionId":"S","update":{"sessionUpdate":"tool_call","toolCallId":"call-B-1","title":"run_terminal_command","rawInput":{"command":"uv run main.py","description":"Start Zip Atlas server on port 8810"},"_meta":{"x.ai/tool":{"version":1,"name":"run_terminal_command","kind":"execute","namespace":"grok_build","label":"Run Command","read_only":false}}},"_meta":{"eventId":"S-771","agentTimestampMs":1787956807742,"updateType":"ToolCall","updateParams":{"toolCallId":"call-B-1","title":"run_terminal_command","kind":"Other","status":"Pending"}}}}"#;
    const BG_FILL: &str = r#"{"timestamp":1787956807,"method":"session/update","params":{"sessionId":"S","update":{"sessionUpdate":"tool_call_update","toolCallId":"call-B-1","kind":"execute","title":"Execute `uv run main.py`","content":[{"type":"content","content":{"type":"text","text":"Start Zip Atlas server on port 8810"}}],"locations":[],"rawInput":{"variant":"Bash","command":"uv run main.py","description":"Start Zip Atlas server on port 8810","is_background":false},"_meta":{"x.ai/tool":{"version":1,"name":"run_terminal_command","kind":"execute","namespace":"grok_build","label":"Run Command","read_only":false}}},"_meta":{"eventId":"S-772","agentTimestampMs":1787956807743,"updateType":"ToolCallUpdate","updateParams":{"toolCallId":"call-B-1","status":null}}}}"#;
    /// `task_id` == `tool_call_id`: 20 of the 29 observed tasks.
    const BG: &str = r#"{"timestamp":1787956807,"method":"_x.ai/session/update","params":{"sessionId":"S","update":{"sessionUpdate":"task_backgrounded","tool_call_id":"call-B-1","task_id":"call-B-1","command":"uv run main.py","cwd":"/home/admin/AI-OS","output_file":"/home/admin/.grok/sessions/%2Fhome%2Fadmin%2FAI-OS/S/terminal/call-B-1.log","description":"Start Zip Atlas server on port 8810"},"_meta":{"eventId":"S-774","agentTimestampMs":1787956807756}}}"#;
    /// The other 9: grok named the task itself, and only this line ever
    /// carries both names.
    const BG_ALIAS: &str = r#"{"timestamp":1787956807,"method":"_x.ai/session/update","params":{"sessionId":"S","update":{"sessionUpdate":"task_backgrounded","tool_call_id":"call-B-1","task_id":"01a04a87-c84c-7140-9dc2-a14a69970efb","command":"uv run main.py","cwd":"/home/admin/AI-OS","output_file":"/home/admin/.grok/sessions/%2Fhome%2Fadmin%2FAI-OS/S/terminal/call-B-1.log","description":"Start Zip Atlas server on port 8810"},"_meta":{"eventId":"S-774","agentTimestampMs":1787956807756}}}"#;
    /// The placeholder: `completed`, seconds in, with the process still up.
    const BG_PLACEHOLDER: &str = r#"{"timestamp":1787956807,"method":"session/update","params":{"sessionId":"S","update":{"sessionUpdate":"tool_call_update","toolCallId":"call-B-1","status":"completed","title":"[bg] uv run main.py (01a04a87)","content":[{"type":"content","content":{"type":"text","text":"Background task 01a04a87-c84c-7140-9dc2-a14a69970efb started"}}],"rawOutput":{"type":"BackgroundTaskStarted","task_id":"01a04a87-c84c-7140-9dc2-a14a69970efb","task_type":"bash","status":"running","command":"uv run main.py"}},"_meta":{"eventId":"S-775","agentTimestampMs":1787956807757}}}"#;
    const TASK_DONE: &str = r#"{"timestamp":1787956950,"method":"_x.ai/session/update","params":{"sessionId":"S","update":{"sessionUpdate":"task_completed","task_snapshot":{"task_id":"call-B-1","command":"uv run main.py","cwd":"/home/admin/AI-OS","start_time":{"secs_since_epoch":1787956807,"nanos_since_epoch":756265349},"end_time":{"secs_since_epoch":1787956950,"nanos_since_epoch":864763157},"output":"serving on :8810\n","output_file":"/home/admin/.grok/sessions/%2Fhome%2Fadmin%2FAI-OS/S/terminal/call-B-1.log","truncated":false,"output_total_bytes":17,"exit_code":0,"signal":null,"completed":true,"kind":"bash","block_waited":false,"explicitly_killed":false,"kill_result_delivered":false,"owner_session_id":"S","description":"Start Zip Atlas server on port 8810","is_backgrounded":true},"will_wake":true},"_meta":{"eventId":"S-776","agentTimestampMs":1787956950865}}}"#;
    /// The aliased task's end. Exit 2, so the card fails; and with no
    /// `task_backgrounded` in front of it, the only line that could have
    /// named the card, this is also the orphan case.
    const TASK_FAILED: &str = r#"{"timestamp":1787956807,"method":"_x.ai/session/update","params":{"sessionId":"S","update":{"sessionUpdate":"task_completed","task_snapshot":{"task_id":"01a04a87-c84c-7140-9dc2-a14a69970efb","command":"uv run main.py","cwd":"/home/admin/AI-OS","start_time":{"secs_since_epoch":1787956807,"nanos_since_epoch":756265349},"end_time":{"secs_since_epoch":1787956807,"nanos_since_epoch":864763157},"output":"error: Failed to spawn: `main.py`\n  Caused by: No such file or directory (os error 2)\n","output_file":"/home/admin/.grok/sessions/%2Fhome%2Fadmin%2FAI-OS/S/terminal/call-B-1.log","truncated":false,"output_total_bytes":86,"exit_code":2,"signal":null,"completed":true,"kind":"bash","block_waited":false,"explicitly_killed":false,"kill_result_delivered":false,"owner_session_id":"S","description":"Start Zip Atlas server on port 8810","is_backgrounded":true},"will_wake":true},"_meta":{"eventId":"S-776","agentTimestampMs":1787956807865}}}"#;

    // A subagent, from ~/.grok/sessions/…/01a03132-…, its report shortened.
    const SUB_SPAWNED: &str = r#"{"timestamp":1787532086,"method":"_x.ai/session/update","params":{"sessionId":"S","update":{"sessionUpdate":"subagent_spawned","subagent_id":"01a03137-0bb1-74a0-9a4f-cf1b27783f4c","parent_session_id":"S","parent_prompt_id":"bf091f58-993b-45b9-95d2-ed75ba73ac93","child_session_id":"01a03137-0bb1-74a0-9a4f-cf1b27783f4c","subagent_type":"explore","description":"Trace click conversion code","effective_context_source":"new","capability_mode":"read-only","role":"explore","model":"grok-4.6"},"_meta":{"eventId":"S-552","agentTimestampMs":1787532086196}}}"#;
    const SUB_FINISHED: &str = r#"{"timestamp":1787532422,"method":"_x.ai/session/update","params":{"sessionId":"S","update":{"sessionUpdate":"subagent_finished","subagent_id":"01a03137-0bb1-74a0-9a4f-cf1b27783f4c","child_session_id":"01a03137-0bb1-74a0-9a4f-cf1b27783f4c","status":"completed","tool_calls":31,"turns":1,"duration_ms":336120,"tokens_used":91504,"output":"Full audit is in `/tmp/affiliate-audit/grok-codepaths.md`.","will_wake":false},"_meta":{"eventId":"S-4957","agentTimestampMs":1787532422316}}}"#;

    const EV_TURN_STARTED: &str = r#"{"ts":"2026-08-28T22:30:19.612Z","type":"turn_started","session_id":"S","turn_number":0,"model_id":"grok-4.6","yolo_mode":true,"conversation_message_count":3,"session_relationship":"primary","schema_version":"1.0"}"#;
    const EV_PERM_REQ: &str = r#"{"ts":"2026-08-28T22:30:22.787Z","type":"permission_requested","tool_name":"read_file"}"#;
    const EV_PERM_OK: &str = r#"{"ts":"2026-08-28T22:30:22.788Z","type":"permission_resolved","tool_name":"read_file","decision":"allow","wait_ms":0}"#;
    const EV_TOOL_ERR: &str = r#"{"ts":"2026-08-28T22:30:22.790Z","type":"tool_completed","tool_name":"read_file","duration_ms":0,"outcome":"error","tool_call_id":"call-A-0"}"#;
    const EV_PHASE: &str = r#"{"ts":"2026-08-28T22:30:20.714Z","type":"phase_changed","phase":"streaming_text"}"#;

    fn kinds(evs: &[(u64, Kind)]) -> Vec<&Kind> {
        evs.iter().map(|(_, k)| k).collect()
    }

    #[test]
    fn a_user_chunk_opens_a_turn_and_speaks() {
        let d = tmpdir("user");
        write(&d, "updates.jsonl", &format!("{USER}\n"));
        let got = adapter(&d).bootstrap();
        assert_eq!(
            kinds(&got),
            vec![
                &Kind::TurnStarted { turn: "1".into() },
                &Kind::UserMessage { id: "u1".into(), text: "build a weather page".into() },
            ]
        );
        // agentTimestampMs wins over the envelope's unix seconds.
        assert_eq!(got[0].0, 1787956219612);
    }

    #[test]
    fn a_run_of_agent_chunks_is_one_growing_block_closed_by_the_next_line() {
        let d = tmpdir("run");
        write(&d, "updates.jsonl", &format!("{SAY}\n{SAY}\n{SAY}\n{CALL}\n"));
        let got = adapter(&d).bootstrap();
        let texts: Vec<_> = got
            .iter()
            .filter_map(|(_, k)| match k {
                Kind::AgentText { id, text, done } => Some((id.as_str(), text.as_str(), *done)),
                _ => None,
            })
            .collect();
        assert_eq!(
            texts,
            vec![
                ("a1", "I'll start ", false),
                ("a1", "I'll start I'll start ", false),
                ("a1", "I'll start I'll start I'll start ", false),
                ("a1", "I'll start I'll start I'll start ", true),
            ]
        );
        assert!(matches!(got.last(), Some((_, Kind::ToolCall { .. }))));
    }

    #[test]
    fn a_thought_run_is_its_own_block() {
        let d = tmpdir("thought");
        write(&d, "updates.jsonl", &format!("{THOUGHT}\n{THOUGHT}\n{SAY}\n"));
        let got = adapter(&d).bootstrap();
        assert_eq!(
            kinds(&got),
            vec![
                &Kind::AgentThought { id: "a1".into(), text: "The user wants a page. ".into(), done: false },
                &Kind::AgentThought {
                    id: "a1".into(),
                    text: "The user wants a page. The user wants a page. ".into(),
                    done: false
                },
                &Kind::AgentThought {
                    id: "a1".into(),
                    text: "The user wants a page. The user wants a page. ".into(),
                    done: true
                },
                &Kind::AgentText { id: "a3".into(), text: "I'll start ".into(), done: false },
            ]
        );
    }

    #[test]
    fn a_tool_call_is_issued_filled_in_then_finished() {
        let d = tmpdir("tool");
        write(&d, "updates.jsonl", &format!("{CALL}\n{CALL_FILLED}\n{CALL_DONE}\n"));
        let got = adapter(&d).bootstrap();
        assert_eq!(
            kinds(&got),
            vec![
                &Kind::ToolCall {
                    id: "call-A-0".into(),
                    tool: "read_file".into(),
                    title: "read_file".into(),
                    category: ToolCategory::Read,
                    input: "target_file=/home/admin/AI-OS/CLAUDE.md".into(),
                    status: ToolStatus::Pending,
                },
                // The fill-in line brings the human title and the ACP kind,
                // so the card is re-issued under the same id.
                &Kind::ToolCall {
                    id: "call-A-0".into(),
                    tool: "read_file".into(),
                    title: "Read `/home/admin/AI-OS/CLAUDE.md`".into(),
                    category: ToolCategory::Read,
                    input: "target_file=/home/admin/AI-OS/CLAUDE.md".into(),
                    status: ToolStatus::Running,
                },
                &Kind::ToolCallUpdate {
                    id: "call-A-0".into(),
                    status: ToolStatus::Completed,
                    output: Some("1→# aiterm".into()),
                },
            ]
        );
    }

    #[test]
    fn turn_completed_closes_an_open_block_and_ends_the_turn() {
        let d = tmpdir("turnend");
        write(&d, "updates.jsonl", &format!("{USER}\n{SAY}\n{PLAN}\n{TURN_DONE}\n"));
        let got = adapter(&d).bootstrap();
        // `plan` ends the prose block, and speaks for itself.
        assert_eq!(
            kinds(&got)[2..],
            [
                &Kind::AgentText { id: "a2".into(), text: "I'll start ".into(), done: false },
                &Kind::AgentText { id: "a2".into(), text: "I'll start ".into(), done: true },
                &Kind::AgentThought { id: "p3".into(), text: "[~] Load skills".into(), done: true },
                &Kind::TurnEnded { turn: "1".into(), reason: "completed".into() },
            ]
        );
    }

    #[test]
    fn a_plan_is_one_thought_row_that_is_rewritten_in_place() {
        let d = tmpdir("plan");
        write(&d, "updates.jsonl", &format!("{PLAN}\n{SAY}\n{PLAN_GROWN}\n"));
        let got = adapter(&d).bootstrap();
        // Both revisions carry the FIRST plan line's ordinal, so the phone
        // upserts one checklist instead of stacking two.
        assert_eq!(
            kinds(&got),
            vec![
                &Kind::AgentThought { id: "p1".into(), text: "[~] Load skills".into(), done: true },
                &Kind::AgentText { id: "a2".into(), text: "I'll start ".into(), done: false },
                &Kind::AgentText { id: "a2".into(), text: "I'll start ".into(), done: true },
                &Kind::AgentThought {
                    id: "p1".into(),
                    text: "[x] Load skills\n[ ] Serve, verify, commit".into(),
                    done: true,
                },
            ]
        );
    }

    #[test]
    fn a_backgrounded_card_stays_open_until_its_task_completes() {
        let d = tmpdir("bg");
        write(
            &d,
            "updates.jsonl",
            &format!("{BG_CALL}\n{BG}\n{BG_FILL}\n{BG_PLACEHOLDER}\n{SAY}\n{TASK_DONE}\n"),
        );
        let got = adapter(&d).bootstrap();
        assert_eq!(
            kinds(&got),
            vec![
                &Kind::ToolCall {
                    id: "call-B-1".into(),
                    tool: "run_terminal_command".into(),
                    title: "run_terminal_command".into(),
                    category: ToolCategory::Execute,
                    input: "command=uv run main.py description=Start Zip Atlas server on port 8810"
                        .into(),
                    status: ToolStatus::Pending,
                },
                // The note that replaces the placeholder.
                &Kind::ToolCallUpdate {
                    id: "call-B-1".into(),
                    status: ToolStatus::Running,
                    output: Some(
                        "running in the background… Start Zip Atlas server on port 8810".into()
                    ),
                },
                // The fill-in still brings the real title; its own update,
                // and the `completed` placeholder after it, are dropped.
                &Kind::ToolCall {
                    id: "call-B-1".into(),
                    tool: "run_terminal_command".into(),
                    title: "Execute `uv run main.py`".into(),
                    category: ToolCategory::Execute,
                    input: "command=uv run main.py description=Start Zip Atlas server on port 8810 is_background=false".into(),
                    status: ToolStatus::Running,
                },
                &Kind::AgentText { id: "a5".into(), text: "I'll start ".into(), done: false },
                &Kind::AgentText { id: "a5".into(), text: "I'll start ".into(), done: true },
                &Kind::ToolCallUpdate {
                    id: "call-B-1".into(),
                    status: ToolStatus::Completed,
                    output: Some("serving on :8810\n".into()),
                },
            ]
        );
    }

    #[test]
    fn a_task_grok_named_for_itself_still_finds_its_card() {
        let d = tmpdir("bg-alias");
        // task_id is a uuid, tool_call_id is the card: only the
        // `task_backgrounded` line knows they are the same thing.
        write(&d, "updates.jsonl", &format!("{BG_CALL}\n{BG_ALIAS}\n{TASK_FAILED}\n"));
        let got = adapter(&d).bootstrap();
        assert_eq!(got.len(), 3);
        assert_eq!(
            got[2].1,
            Kind::ToolCallUpdate {
                id: "call-B-1".into(),
                status: ToolStatus::Failed,
                output: Some(
                    "[exit 2] error: Failed to spawn: `main.py`\n  Caused by: No such file or directory (os error 2)\n"
                        .into()
                ),
            }
        );
    }

    #[test]
    fn a_task_with_no_card_to_land_on_gets_one_of_its_own() {
        let d = tmpdir("bg-orphan");
        // The same end, with the backgrounding never seen — a session
        // resumed past it. Nothing is dropped: a card is opened and closed.
        write(&d, "updates.jsonl", &format!("{TASK_FAILED}\n"));
        let got = adapter(&d).bootstrap();
        assert_eq!(
            kinds(&got),
            vec![
                &Kind::ToolCall {
                    id: "task-01a04a87-c84c-7140-9dc2-a14a69970efb".into(),
                    tool: "bash".into(),
                    title: "Start Zip Atlas server on port 8810".into(),
                    category: ToolCategory::Execute,
                    input: "uv run main.py".into(),
                    status: ToolStatus::Running,
                },
                &Kind::ToolCallUpdate {
                    id: "task-01a04a87-c84c-7140-9dc2-a14a69970efb".into(),
                    status: ToolStatus::Failed,
                    output: Some(
                        "[exit 2] error: Failed to spawn: `main.py`\n  Caused by: No such file or directory (os error 2)\n"
                            .into()
                    ),
                },
            ]
        );
    }

    #[test]
    fn a_subagent_is_a_card_of_its_own_from_spawn_to_report() {
        let d = tmpdir("subagent");
        write(&d, "updates.jsonl", &format!("{SUB_SPAWNED}\n{SAY}\n{SUB_FINISHED}\n"));
        let got = adapter(&d).bootstrap();
        let id = "sub-01a03137-0bb1-74a0-9a4f-cf1b27783f4c";
        assert_eq!(
            kinds(&got),
            vec![
                &Kind::ToolCall {
                    id: id.into(),
                    tool: "subagent".into(),
                    title: "Trace click conversion code".into(),
                    category: ToolCategory::Think,
                    input: "type=explore model=grok-4.6".into(),
                    status: ToolStatus::Running,
                },
                &Kind::AgentText { id: "a2".into(), text: "I'll start ".into(), done: false },
                &Kind::AgentText { id: "a2".into(), text: "I'll start ".into(), done: true },
                &Kind::ToolCallUpdate {
                    id: id.into(),
                    status: ToolStatus::Completed,
                    output: Some(
                        "completed · 31 tool calls · 336 s\nFull audit is in `/tmp/affiliate-audit/grok-codepaths.md`."
                            .into()
                    ),
                },
            ]
        );
    }

    #[test]
    fn a_permission_answered_before_we_looked_never_needed_anyone() {
        let d = tmpdir("perm-fast");
        write(&d, "events.jsonl", &format!("{EV_TURN_STARTED}\n{EV_PERM_REQ}\n{EV_PERM_OK}\n{EV_PHASE}\n"));
        let got = adapter(&d).bootstrap();
        assert_eq!(
            kinds(&got),
            vec![&Kind::Phase { phase: Phase::Working, detail: String::new() }]
        );
    }

    #[test]
    fn a_permission_still_open_asks_for_you_and_is_released_next_poll() {
        let d = tmpdir("perm-slow");
        write(&d, "events.jsonl", &format!("{EV_PERM_REQ}\n"));
        let mut a = adapter(&d);
        assert_eq!(
            kinds(&a.bootstrap()),
            vec![&Kind::Phase { phase: Phase::NeedsYou, detail: "permission: read_file".into() }]
        );
        append(&d, "events.jsonl", &format!("{EV_PERM_OK}\n"));
        assert_eq!(
            kinds(&a.poll()),
            vec![&Kind::Phase { phase: Phase::Working, detail: String::new() }]
        );
    }

    #[test]
    fn events_correct_a_tool_that_updates_called_completed() {
        let d = tmpdir("toolerr");
        write(&d, "updates.jsonl", &format!("{CALL}\n{CALL_DONE}\n"));
        write(&d, "events.jsonl", &format!("{EV_TOOL_ERR}\n"));
        let got = adapter(&d).bootstrap();
        assert_eq!(
            got.last().map(|(_, k)| k),
            Some(&Kind::ToolCallUpdate {
                id: "call-A-0".into(),
                status: ToolStatus::Failed,
                output: None
            })
        );
        // Same millisecond as the `completed` it corrects — the tie-break
        // is what puts it after.
        assert_eq!(got[got.len() - 2].0, got[got.len() - 1].0);
    }

    #[test]
    fn the_two_files_interleave_by_timestamp() {
        let d = tmpdir("merge");
        // The user line is stamped 1787956219612; the events line is
        // 2026-08-28T22:30:19.612Z, which is the same instant.
        write(&d, "updates.jsonl", &format!("{USER}\n{SAY}\n"));
        write(&d, "events.jsonl", &format!("{EV_TURN_STARTED}\n{EV_PHASE}\n"));
        let got = adapter(&d).bootstrap();
        assert_eq!(iso_ms("2026-08-28T22:30:19.612Z"), Some(1787956219612));
        let stamps: Vec<u64> = got.iter().map(|(ts, _)| *ts).collect();
        assert!(stamps.windows(2).all(|w| w[0] <= w[1]), "{stamps:?}");
        // updates first on a tie, then the events phase, then the prose.
        assert!(matches!(got[0].1, Kind::TurnStarted { .. }));
        assert!(matches!(got[1].1, Kind::UserMessage { .. }));
        assert_eq!(got[2].1, Kind::Phase { phase: Phase::Working, detail: String::new() });
        assert!(matches!(got[3].1, Kind::AgentText { .. }));
    }

    #[test]
    fn half_a_line_waits_for_its_newline() {
        let d = tmpdir("partial");
        let (head, tail) = USER.split_at(120);
        write(&d, "updates.jsonl", head);
        let mut a = adapter(&d);
        assert!(a.bootstrap().is_empty());
        append(&d, "updates.jsonl", &format!("{tail}\n"));
        assert_eq!(
            kinds(&a.poll()),
            vec![
                &Kind::TurnStarted { turn: "1".into() },
                &Kind::UserMessage { id: "u1".into(), text: "build a weather page".into() },
            ]
        );
    }

    #[test]
    fn a_truncated_file_rebuilds_behind_a_reset() {
        let d = tmpdir("reset");
        write(&d, "updates.jsonl", &format!("{USER}\n{SAY}\n"));
        let mut a = adapter(&d);
        assert_eq!(a.bootstrap().len(), 3);
        write(&d, "updates.jsonl", &format!("{USER}\n"));
        let got = a.poll();
        assert_eq!(
            kinds(&got),
            vec![
                &Kind::Reset,
                &Kind::TurnStarted { turn: "1".into() },
                &Kind::UserMessage { id: "u1".into(), text: "build a weather page".into() },
            ]
        );
    }

    #[test]
    fn a_session_with_no_updates_file_yet_is_simply_quiet() {
        let d = tmpdir("empty");
        let mut a = adapter(&d);
        assert!(a.bootstrap().is_empty());
        append(&d, "updates.jsonl", &format!("{USER}\n"));
        assert_eq!(a.poll().len(), 2);
    }

    /// Run against the real thing: `cargo test --lib spine::grok -- --ignored --nocapture`.
    #[test]
    #[ignore = "reads ~/.grok, which only exists on a machine that runs grok"]
    fn bootstrap_a_real_session() {
        let root = dirs::home_dir().unwrap().join(".grok/sessions");
        let mut sessions: Vec<PathBuf> = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .filter(|c| c.path().is_dir())
            .flat_map(|c| std::fs::read_dir(c.path()).unwrap().flatten().map(|s| s.path()))
            .filter(|s| s.join("updates.jsonl").is_file())
            .collect();
        sessions.sort();
        let mut histogram: std::collections::BTreeMap<String, usize> = Default::default();
        for dir in sessions {
            let lines = |f: &str| {
                std::fs::read_to_string(dir.join(f)).map(|s| s.lines().count()).unwrap_or(0)
            };
            let (u, e) = (lines("updates.jsonl"), lines("events.jsonl"));
            let start = std::time::Instant::now();
            let out = adapter(&dir).bootstrap();
            println!(
                "{:<38} {u:>5} updates + {e:>6} events → {:>5} events in {:>6.1} ms",
                dir.file_name().unwrap().to_string_lossy(),
                out.len(),
                start.elapsed().as_secs_f64() * 1000.0
            );
            for (_, k) in &out {
                let tag = serde_json::to_value(k).unwrap()["kind"].as_str().unwrap().to_string();
                *histogram.entry(tag).or_default() += 1;
            }
            // At most one block may still be open — the one the file ends
            // on, which a later chunk may still grow. Any earlier block left
            // at `done:false` would be a fold that never closed.
            let open: std::collections::BTreeSet<&String> = out
                .iter()
                .filter_map(|(_, k)| match k {
                    Kind::AgentText { id, done: false, .. }
                    | Kind::AgentThought { id, done: false, .. } => Some(id),
                    _ => None,
                })
                .filter(|id| {
                    !out.iter().any(|(_, k)| {
                        matches!(k, Kind::AgentText { id: i, done: true, .. }
                            | Kind::AgentThought { id: i, done: true, .. } if i == *id)
                    })
                })
                .collect();
            assert!(open.len() <= 1, "{dir:?} left blocks open: {open:?}");
            // Every update lands on a card that was opened for it: no
            // status or output is dealt out to an id the phone never saw.
            let cards: std::collections::BTreeSet<&String> = out
                .iter()
                .filter_map(|(_, k)| match k {
                    Kind::ToolCall { id, .. } => Some(id),
                    _ => None,
                })
                .collect();
            for (_, k) in &out {
                if let Kind::ToolCallUpdate { id, .. } = k {
                    assert!(cards.contains(id), "{dir:?} update with no card: {id}");
                }
            }
        }
        println!("{histogram:#?}");
    }
}
