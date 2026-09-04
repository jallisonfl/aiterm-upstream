//! Codex adapter: the rollout at
//! `~/.codex/sessions/<yyyy>/<mm>/<dd>/rollout-<ts>-<id>.jsonl`, read from a
//! byte offset, one JSON record per line.
//!
//! Owned by the codex-adapter task. See `docs/architecture/spine.md`.
//!
//! The rollout is append-only while a session runs — every record carries a
//! monotonic `ordinal` and nothing above it is ever rewritten — so the tail is
//! a seek to the saved offset and a read to EOF. The same two things that
//! break that for claude are handled the same way: a writer caught mid-line
//! (bytes after the last `\n` are held back), and the file being replaced
//! (a shorter file or a new inode ⇒ `Reset`, then read from zero).
//!
//! ## Which record is the source of truth
//!
//! A rollout says most things twice. `response_item` lines are the model
//! conversation as it went to the API; `event_msg` lines are the TUI's own
//! narration of the same items — newer builds as `item_completed` with an
//! `item.type`, older ones as bare `agent_message` / `user_message`. Content
//! is taken from the `response_item`, because that is the record with the
//! engine's stable id on it (`payload.id`, `call_id`) and it is written for
//! every item; the `event_msg` mirrors are dropped.
//!
//! The one thing the mirrors know that the `response_item` does not is
//! whether a tool call WORKED. A shell command that exits 1 still gets a
//! `custom_tool_call_output` reading "Script completed"; only
//! `item_completed`'s `CommandExecution` carries `exit_code` and
//! `status:"failed"`. So outcomes come from the `event_msg` side and are
//! attributed to the call that is open at the time — safe because a rollout
//! closes every tool call before opening the next: across the 15 rollouts on
//! this machine (986 records) there is not one nested or unclosed pair.
//!
//! ## Turns
//!
//! Codex names its own turns: `task_started`, `task_complete` and
//! `turn_aborted` all carry the same `turn_id`, and every `response_item`
//! repeats it under `internal_chat_message_metadata_passthrough`. That id is
//! the turn, so a `TurnEnded` always names the `TurnStarted` it closes even
//! when a tail begins mid-file.
//!
//! [observed: codex-cli 0.151.0, rollouts written 2026-07-27 – 2026-09-02]

use super::{clip, now_ms, Adapter, Kind, ToolCategory, ToolStatus};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

/// A tool call's one-line input summary, on the wire.
const INPUT_CAP: usize = 400;
/// A tool result's output. Bigger than the input because the output is what a
/// person reads to know whether the call worked.
const OUTPUT_CAP: usize = 2_000;
/// A card's heading.
const TITLE_CAP: usize = 200;

/// The envelopes codex wraps its own preamble in, injected into the first
/// `user` message of every session. The phone strips the first three already
/// (`SessionScreen.kt`, `HARNESS_BLOCKS`) — they are listed here only to
/// decide whether a message is ENTIRELY scaffolding, never to edit the text
/// that goes on the wire. `recommended_plugins` is codex's alone and the
/// phone does not know it yet.
const HARNESS_TAGS: [&str; 4] =
    ["INSTRUCTIONS", "environment_context", "user_instructions", "recommended_plugins"];

/// The heading codex puts above an AGENTS.md it has loaded.
const HARNESS_HEADING: &str = "# AGENTS.md instructions for ";

pub struct CodexAdapter {
    path: PathBuf,
    /// Bytes of the file already handed to the parser, complete lines or not.
    offset: u64,
    /// The tail of the file after the last `\n`: a line the writer has not
    /// finished. Bytes, not a `String`, because the split can land inside a
    /// multi-byte character.
    pending: Vec<u8>,
    /// `(dev, ino)` of the file as last read, to notice a replacement whose
    /// length happens not to shrink.
    ident: Option<(u64, u64)>,
    /// Records parsed so far. Rollouts carry their own `ordinal`, but only
    /// since 0.14x — 242 of the 986 records on this machine have none — so the
    /// count is kept here. Bootstrap always reads from zero, which is what
    /// makes an id built from it stable across re-reads.
    line: u64,
    /// The `turn_id` of the turn now running, so the record that closes it
    /// names the same turn.
    turn: Option<String>,
    /// The tool call issued and not yet answered, and whatever the TUI's
    /// narration has since said about how it went.
    open: Option<OpenCall>,
}

/// A tool call between its `custom_tool_call` and its `…_call_output`.
struct OpenCall {
    /// The engine's `call_id` — the id the card is keyed by.
    id: String,
    /// The outcome, once an `event_msg` result record has said what it was.
    /// `None` until then, and a plain `Completed` if nothing ever says.
    status: Option<ToolStatus>,
    /// A better output than the call's own, for the calls whose own output is
    /// useless: `tools.apply_patch` answers `{}` and the file list lives in
    /// the `patch_apply_end` beside it.
    output: Option<String>,
}

/// The adapter for a Codex session, or `None` when the id is not one.
///
/// A path with no file behind it still gets an adapter — the registry's watch
/// on the parent directory is what notices the file appearing, and `poll`
/// reads an absent file as empty. Codex is the engine least likely to need
/// that (the header line is written when the session starts, not at the first
/// prompt) but the day directory itself may not exist yet at midnight.
pub fn open(session_id: &str) -> Option<CodexAdapter> {
    let list = crate::agents::backends();
    let (backend, path) = crate::agents::owner_in(&list, session_id)?;
    if backend.id() != "codex" {
        return None;
    }
    Some(CodexAdapter {
        path,
        offset: 0,
        pending: Vec::new(),
        ident: None,
        line: 0,
        turn: None,
        open: None,
    })
}

impl Adapter for CodexAdapter {
    /// The whole history, as `poll` would have produced it line by line.
    fn bootstrap(&mut self) -> Vec<(u64, Kind)> {
        self.offset = 0;
        self.pending.clear();
        self.ident = None;
        self.line = 0;
        self.turn = None;
        self.open = None;
        self.poll()
    }

    fn poll(&mut self) -> Vec<(u64, Kind)> {
        let mut out = Vec::new();
        let Ok(meta) = std::fs::metadata(&self.path) else {
            // Not written yet, or gone. Either way there is nothing to read;
            // the watch on the parent directory brings us back.
            return out;
        };
        let ident = identity(&meta);
        // A rollout is only ever appended to, so a shorter one is a different
        // file under the same name. Length alone would miss a replacement
        // that is already longer than what we had read.
        let replaced = meta.len() < self.offset
            || (self.ident.is_some() && ident.is_some() && ident != self.ident);
        if replaced {
            out.push((now_ms(), Kind::Reset));
            self.offset = 0;
            self.pending.clear();
            self.line = 0;
            self.turn = None;
            self.open = None;
        }
        if ident.is_some() {
            self.ident = ident;
        }
        if meta.len() <= self.offset {
            return out;
        }

        let Ok(mut file) = std::fs::File::open(&self.path) else { return out };
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return out;
        }
        let mut fresh = Vec::new();
        if file.read_to_end(&mut fresh).is_err() {
            return out;
        }
        self.offset += fresh.len() as u64;
        self.pending.extend_from_slice(&fresh);

        // Only whole lines are parsed. A rollout record is one `write`, but a
        // reader woken by the same inotify event can still see half of it —
        // and codex's records run to megabytes, so the half is common.
        let buffered = std::mem::take(&mut self.pending);
        let mut start = 0;
        for (i, byte) in buffered.iter().enumerate() {
            if *byte != b'\n' {
                continue;
            }
            self.read_line(&buffered[start..i], &mut out);
            start = i + 1;
        }
        self.pending = buffered[start..].to_vec();
        out
    }

    /// The rollout, plus the day directory that holds it — a session started
    /// before midnight writes tomorrow's file into a folder that does not
    /// exist yet, and only watching the folder notices it appear.
    fn watch_paths(&self) -> Vec<PathBuf> {
        let mut paths = vec![self.path.clone()];
        if let Some(dir) = self.path.parent() {
            paths.push(dir.to_path_buf());
        }
        paths
    }
}

impl CodexAdapter {
    fn read_line(&mut self, raw: &[u8], out: &mut Vec<(u64, Kind)>) {
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(raw) else { return };
        self.line += 1;
        let ts = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .and_then(iso_to_ms)
            .unwrap_or_else(now_ms);
        let Some(p) = v.get("payload") else { return };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("response_item") => self.response_item(p, ts, out),
            Some("event_msg") => self.event_msg(p, ts, out),
            // `session_meta` is the header, read by `agents::scan_codex_dir`
            // for the session row before we ever get here; a forked thread
            // replays its parent's header as a second one. `turn_context`,
            // `world_state`, `inter_agent_communication_metadata` and
            // `compacted` are the harness's own bookkeeping. A compaction
            // does not rewrite the rollout — the file only grows — so it
            // needs no `Reset`; if codex ever did rewrite it, the shrink
            // check in `poll` is what would catch it.
            _ => {}
        }
    }

    /// A `response_item`: the conversation as the model saw it.
    fn response_item(&mut self, p: &serde_json::Value, ts: u64, out: &mut Vec<(u64, Kind)>) {
        match p.get("type").and_then(|t| t.as_str()) {
            Some("message") => self.message(p, ts, out),
            // A message between agents when codex is running a team: the
            // readable half of it is the `input_text` parts, the payload
            // itself is encrypted. Worth showing — it is the only record of
            // what a subagent was asked and what it answered.
            Some("agent_message") => {
                let text = content_text(p.get("content"));
                if text.trim().is_empty() {
                    return;
                }
                let from = p.get("author").and_then(|a| a.as_str()).unwrap_or("agent");
                let to = p.get("recipient").and_then(|a| a.as_str()).unwrap_or("agent");
                out.push((
                    ts,
                    Kind::AgentText {
                        id: self.id_of(p),
                        text: format!("{from} → {to}\n{text}"),
                        done: true,
                    },
                ));
            }
            // Reasoning is a summary plus an encrypted blob. On every rollout
            // on this machine the summary is empty — gpt-5.6-sol returns its
            // reasoning encrypted and the TUI has nothing to show either — so
            // this is written for the models that do summarise, and stays
            // quiet for the ones that do not.
            Some("reasoning") => {
                let text = content_text(p.get("summary"));
                if text.trim().is_empty() {
                    return;
                }
                out.push((ts, Kind::AgentThought { id: self.id_of(p), text, done: true }));
            }
            Some("function_call" | "custom_tool_call" | "local_shell_call") => {
                self.tool_call(p, ts, out)
            }
            // `function_call_output`, `custom_tool_call_output`,
            // `local_shell_call_output`.
            Some(t) if t.ends_with("_call_output") => self.tool_output(p, ts, out),
            _ => {}
        }
    }

    fn message(&mut self, p: &serde_json::Value, ts: u64, out: &mut Vec<(u64, Kind)>) {
        let text = content_text(p.get("content"));
        match p.get("role").and_then(|r| r.as_str()) {
            Some("assistant") => {
                if text.trim().is_empty() {
                    return;
                }
                out.push((ts, Kind::AgentText { id: self.id_of(p), text, done: true }));
            }
            Some("user") => {
                // The first `user` record of every session is codex's own
                // preamble — the AGENTS.md it loaded, the environment block,
                // the plugin list — on its own line, never mixed with what a
                // person typed. The text goes over the wire untouched (the
                // phone strips the blocks it knows); what is decided here is
                // only whether there is anything else in it at all.
                if harness_only(&text) {
                    return;
                }
                let id = self.id_of(p);
                // `task_started` opens the turn a beat earlier. This is the
                // fallback for a tail that began after it.
                if self.turn.is_none() {
                    self.turn = Some(id.clone());
                    out.push((ts, Kind::TurnStarted { turn: id.clone() }));
                }
                out.push((ts, Kind::UserMessage { id, text }));
            }
            // `developer`: the harness's own system prompt, three or four
            // records of it at the head of every turn. Nobody said it.
            _ => {}
        }
    }

    /// A tool being invoked. Codex 0.151 routes nearly everything through one
    /// custom tool named `exec` whose input is a JavaScript snippet, so what
    /// the call actually IS has to be read out of that snippet — see
    /// [`shape`].
    fn tool_call(&mut self, p: &serde_json::Value, ts: u64, out: &mut Vec<(u64, Kind)>) {
        let id = p
            .get("call_id")
            .and_then(|c| c.as_str())
            .map(String::from)
            .unwrap_or_else(|| self.fallback_id());
        let name = p.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
        let input = match p.get("input").or_else(|| p.get("arguments")) {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => String::new(),
        };
        let (tool, detail, category) = shape(name, &input);
        self.open = Some(OpenCall { id: id.clone(), status: None, output: None });
        out.push((
            ts,
            Kind::ToolCall {
                id,
                tool,
                // The heading is the first line: a shell command is often a
                // whole script, and the verb is at the top of it.
                title: clip(detail.lines().next().unwrap_or(&detail), TITLE_CAP),
                category,
                input: clip(&detail, INPUT_CAP),
                // Not `Pending`: codex writes the record as it dispatches.
                // Waiting on a person is reported on the phase channel, which
                // is where `transcript_verdict` already reads codex's open
                // turn for an approval.
                status: ToolStatus::Running,
            },
        ));
    }

    fn tool_output(&mut self, p: &serde_json::Value, ts: u64, out: &mut Vec<(u64, Kind)>) {
        // The card is keyed by whatever the CALL was issued under, which is
        // the `call_id` on both records — but a call with no id of its own
        // was carded under its line, and the output must land on that card
        // rather than opening a second one. A tail that started between the
        // two has no open call and takes the output's `call_id`, so a phone
        // that joined mid-command still sees the result.
        let open = self.open.take();
        let Some(id) = open
            .as_ref()
            .map(|o| o.id.clone())
            .or_else(|| p.get("call_id").and_then(|c| c.as_str()).map(String::from))
        else {
            return;
        };
        // The exec runtime prefixes every result with "Script completed / Wall
        // time … / Output:" — kept, because it is how long the call took and
        // there is nowhere else to say so.
        let said = content_text(p.get("output"));
        let output = open.as_ref().and_then(|o| o.output.clone()).unwrap_or(said);
        let status = open.and_then(|o| o.status).unwrap_or(ToolStatus::Completed);
        out.push((
            ts,
            Kind::ToolCallUpdate {
                id,
                status,
                output: (!output.trim().is_empty()).then(|| clip(&output, OUTPUT_CAP)),
            },
        ));
    }

    /// An `event_msg`: the TUI narrating. Content here is a duplicate of the
    /// `response_item` beside it and is dropped; what is kept is the turn
    /// boundaries, which exist nowhere else, and the outcome of a tool call,
    /// which the call's own output does not carry.
    fn event_msg(&mut self, p: &serde_json::Value, ts: u64, out: &mut Vec<(u64, Kind)>) {
        match p.get("type").and_then(|t| t.as_str()) {
            Some("task_started") => {
                let turn = p
                    .get("turn_id")
                    .and_then(|t| t.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| self.fallback_id());
                self.turn = Some(turn.clone());
                out.push((ts, Kind::TurnStarted { turn }));
            }
            Some("task_complete") => self.end_turn(p, ts, "completed", out),
            // Codex, unlike claude, does record an interruption as an event.
            Some("turn_aborted") => {
                let reason = match p.get("reason").and_then(|r| r.as_str()) {
                    Some("interrupted") => "interrupted",
                    Some("error") => "error",
                    _ => "unknown",
                };
                self.end_turn(p, ts, reason, out);
            }
            // The narration of one finished item. Only the ones that report a
            // RESULT are of interest; `AgentMessage`, `Reasoning` and
            // `UserMessage` are the duplicates.
            Some("item_completed") => self.outcome(p.get("item")),
            // Older builds' spelling of `item_completed`/`FileChange`: the
            // patch's own output is `{}`, so this is where the file list is.
            Some("patch_apply_end") => {
                let Some(open) = self.open.as_mut() else { return };
                let ok = p.get("success").and_then(|s| s.as_bool()).unwrap_or(true);
                open.status = Some(if ok { ToolStatus::Completed } else { ToolStatus::Failed });
                open.output = first_nonempty(&[p.get("stdout"), p.get("stderr")]);
            }
            // Not written by 0.151 — it narrates a command with
            // `item_completed`/`CommandExecution` — but it is the older
            // spelling and it is one line to honour.
            Some("exec_command_end") => {
                let Some(open) = self.open.as_mut() else { return };
                let bad = p.get("exit_code").and_then(|c| c.as_i64()).is_some_and(|c| c != 0);
                open.status = Some(if bad { ToolStatus::Failed } else { ToolStatus::Completed });
            }
            // `token_count`, `thread_settings_applied`, `web_search_end`,
            // `agent_message`, `user_message`, `agent_reasoning`,
            // `exec_command_begin`, `turn_diff` — usage, settings, and
            // duplicates of records already emitted.
            _ => {}
        }
    }

    fn end_turn(
        &mut self,
        p: &serde_json::Value,
        ts: u64,
        reason: &str,
        out: &mut Vec<(u64, Kind)>,
    ) {
        // An interruption can land between a call and its output, and a card
        // left `Running` spins on the phone for the rest of the session.
        if let Some(open) = self.open.take() {
            out.push((
                ts,
                Kind::ToolCallUpdate {
                    id: open.id,
                    status: ToolStatus::Cancelled,
                    output: open.output,
                },
            ));
        }
        let turn = p
            .get("turn_id")
            .and_then(|t| t.as_str())
            .map(String::from)
            .or_else(|| self.turn.take())
            .unwrap_or_else(|| self.fallback_id());
        self.turn = None;
        out.push((ts, Kind::TurnEnded { turn, reason: reason.into() }));
    }

    /// What an `item_completed` says about the call now open. The item ids do
    /// not match the `call_id` — a command is `exec-<uuid>`, its call is
    /// `call_<…>` — so the pairing is positional, which the rollouts' strict
    /// one-call-at-a-time ordering makes safe.
    fn outcome(&mut self, item: Option<&serde_json::Value>) {
        let (Some(open), Some(item)) = (self.open.as_mut(), item) else { return };
        match item.get("type").and_then(|t| t.as_str()) {
            Some("CommandExecution") => open.status = Some(item_status(item)),
            // A patch: the call answers `{}`, the item lists the files.
            Some("FileChange") => {
                open.status = Some(item_status(item));
                open.output = first_nonempty(&[item.get("stdout"), item.get("stderr")]);
            }
            // `web.search`, `image_gen.generation` — the exec runtime's
            // built-in extensions. A generated image's only useful result is
            // where it was written.
            Some("Extension") => {
                open.status = Some(item_status(item));
                open.output = first_nonempty(&[item.get("savedPath")]);
            }
            Some("ImageView") => {
                open.status = Some(item_status(item));
                open.output = first_nonempty(&[item.get("path")]);
            }
            _ => {}
        }
    }

    /// A record's own id, or the line it arrived on. 17 of the 161 message
    /// records on this machine carry no `id` — the ones a forked thread
    /// replays from its parent.
    fn id_of(&self, p: &serde_json::Value) -> String {
        p.get("id")
            .and_then(|i| i.as_str())
            .filter(|i| !i.is_empty())
            .map(String::from)
            .unwrap_or_else(|| self.fallback_id())
    }

    fn fallback_id(&self) -> String {
        format!("codex:{}", self.line)
    }
}

/// What kind of call this is, and the part of it a person would read.
/// Returns `(tool, detail, category)`; the heading is the detail's first line.
///
/// Codex's flagship models are given ONE tool — `exec`, a JavaScript runtime —
/// and reach the shell, the patcher, web search and image generation through
/// it, so the tool name on the record is `exec` for all of them and the real
/// verb is in the snippet. Mini models write `function_call` with a plain
/// name and JSON `arguments` instead, which needs none of this.
/// [observed: codex-cli 0.151.0; `tools.exec_command`, `tools.apply_patch`,
/// `tools.web__run`, `tools.image_gen__imagegen`, `tools.view_image`]
fn shape(name: &str, input: &str) -> (String, String, ToolCategory) {
    if input.contains("tools.apply_patch(") || input.contains("*** Begin Patch") {
        return ("apply_patch".into(), patch_files(input), ToolCategory::Edit);
    }
    if input.contains("tools.web__run(") {
        let query = js_string_after(input, "q:\"")
            .or_else(|| js_string_after(input, "q: \""))
            .or_else(|| js_string_after(input, "url:\""))
            .unwrap_or_else(|| "web search".into());
        return ("web_search".into(), query, ToolCategory::Search);
    }
    if input.contains("tools.image_gen__imagegen(") {
        let prompt = js_string_after(input, "prompt:\"")
            .or_else(|| js_string_after(input, "prompt: \""))
            .unwrap_or_else(|| "generating an image".into());
        return ("image_gen".into(), prompt, ToolCategory::Other);
    }
    if input.contains("tools.view_image(") {
        let path = js_string_after(input, "path:\"")
            .or_else(|| js_string_after(input, "\"path\":\""))
            .unwrap_or_else(|| "view image".into());
        return ("view_image".into(), path, ToolCategory::Read);
    }
    // The `cmd` key is bare on current rollouts and quoted on older ones, so
    // both are probed — the same pair `detail.rs` probes for the sidebar.
    if input.contains("tools.exec_command(")
        || matches!(name, "exec" | "exec_command" | "shell" | "local_shell")
    {
        let cmd = js_string_after(input, "\"cmd\":\"")
            .or_else(|| js_string_after(input, "cmd:\""))
            .or_else(|| js_string_after(input, "cmd: \""))
            .or_else(|| json_summary(input))
            .unwrap_or_else(|| input.to_string());
        return ("exec_command".into(), cmd, ToolCategory::Execute);
    }
    let detail = json_summary(input).unwrap_or_else(|| input.to_string());
    (name.to_string(), detail, category(name))
}

/// A named tool's category. Codex's own verbs are the collaboration ones (it
/// spawns and waits on subagents) and `wait`, which resumes a shell cell that
/// had not finished. MCP tools wear their own name and stay `Other` rather
/// than being guessed at.
fn category(name: &str) -> ToolCategory {
    match name {
        "spawn_agent" | "wait_agent" | "list_agents" | "interrupt_agent" | "followup_task" => {
            ToolCategory::Think
        }
        "wait" => ToolCategory::Execute,
        "read_file" | "view_image" => ToolCategory::Read,
        "apply_patch" | "write_file" => ToolCategory::Edit,
        "web_search" | "search" => ToolCategory::Search,
        "fetch" | "open_page" => ToolCategory::Fetch,
        _ => ToolCategory::Other,
    }
}

/// JSON `arguments` down to the field a person cares about, through the same
/// probe the sidebar uses. `None` when the input is not JSON at all — which
/// is every `exec` call, whose input is JavaScript.
fn json_summary(input: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(input).ok()?;
    // A `local_shell_call`'s command is argv, not a string.
    if let Some(argv) = v.get("command").and_then(|c| c.as_array()) {
        let joined: Vec<&str> = argv.iter().filter_map(|a| a.as_str()).collect();
        if !joined.is_empty() {
            return Some(joined.join(" "));
        }
    }
    Some(crate::detail::tool_input_summary(Some(&v)))
}

/// The files a patch touches, as codex's own `Success. Updated…` lines name
/// them. The patch lives inside a JavaScript string literal, so its line
/// breaks are the two characters `\` `n`, not newlines.
fn patch_files(input: &str) -> String {
    const VERBS: [(&str, &str); 4] = [
        ("Add File: ", "A"),
        ("Update File: ", "M"),
        ("Delete File: ", "D"),
        ("Move to: ", "→"),
    ];
    let mut files: Vec<String> = Vec::new();
    let mut rest = input;
    while let Some(at) = rest.find("*** ") {
        let after = &rest[at + "*** ".len()..];
        let end = after.find("\\n").or_else(|| after.find('\n')).unwrap_or(after.len());
        let line = &after[..end];
        for (verb, tag) in VERBS {
            if let Some(path) = line.strip_prefix(verb) {
                let path = path.trim().trim_end_matches('"');
                if !path.is_empty() {
                    files.push(format!("{tag} {path}"));
                }
            }
        }
        rest = &after[end..];
    }
    if files.is_empty() {
        return "apply_patch".into();
    }
    files.join("\n")
}

/// How an `item_completed` says a call went. `failure` is the image
/// generator's word, `status` everyone else's, and a command that reports
/// neither is judged by its exit code.
fn item_status(item: &serde_json::Value) -> ToolStatus {
    if item.get("failure").is_some_and(|f| !f.is_null()) {
        return ToolStatus::Failed;
    }
    match item.get("status").and_then(|s| s.as_str()) {
        Some("failed" | "error") => ToolStatus::Failed,
        Some("cancelled" | "aborted") => ToolStatus::Cancelled,
        _ => {
            let bad = item.get("exit_code").and_then(|c| c.as_i64()).is_some_and(|c| c != 0);
            if bad {
                ToolStatus::Failed
            } else {
                ToolStatus::Completed
            }
        }
    }
}

/// The first of these fields that is a non-empty string.
fn first_nonempty(fields: &[Option<&serde_json::Value>]) -> Option<String> {
    fields
        .iter()
        .filter_map(|f| f.and_then(|v| v.as_str()))
        .find(|s| !s.trim().is_empty())
        .map(String::from)
}

/// A record's text, whatever shape it came in: a bare string, or the parts
/// array codex uses everywhere else. The parts are matched on CARRYING text
/// rather than on their tag — `input_text` on the way in, `output_text` on
/// the way out, `Text` in the TUI's mirrors — so a new tag for the same thing
/// does not silently empty a message. Parts with no text (an
/// `encrypted_content` payload between two agents) are dropped.
fn content_text(v: Option<&serde_json::Value>) -> String {
    match v {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => {
            let mut out = String::new();
            for part in parts {
                let Some(t) = part.get("text").and_then(|t| t.as_str()) else { continue };
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(t);
            }
            out
        }
        Some(other) if !other.is_null() => other.to_string(),
        _ => String::new(),
    }
}

/// Whether a `user` record is nothing but codex's preamble. True for exactly
/// one record per session on this machine (16 of 46 `user` records across 16
/// session headers) and for no record anybody typed.
fn harness_only(text: &str) -> bool {
    let mut rest = text.to_string();
    for tag in HARNESS_TAGS {
        let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
        // Non-greedy, like the phone's `(?s)<TAG>.*?</TAG>`: two blocks of one
        // tag with words between them must not swallow the words.
        while let Some(a) = rest.find(&open) {
            let after = a + open.len();
            let Some(rel) = rest[after..].find(&close) else { break };
            rest.replace_range(a..after + rel + close.len(), "");
        }
    }
    rest.lines().all(|l| {
        let l = l.trim();
        l.is_empty() || l.starts_with(HARNESS_HEADING)
    })
}

/// The double-quoted string starting right after `key`, JSON-style escapes
/// resolved. `None` when the string never closes. (`detail.rs` keeps a
/// private twin of this for the sidebar's one-line summaries; it is fifteen
/// lines and not worth a cross-module dependency in either direction.)
fn js_string_after(s: &str, key: &str) -> Option<String> {
    let start = s.find(key)? + key.len();
    let mut out = String::new();
    let mut esc = false;
    for c in s[start..].chars() {
        if esc {
            out.push(match c {
                'n' => '\n',
                't' => '\t',
                other => other,
            });
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == '"' {
            return Some(out);
        } else {
            out.push(c);
        }
    }
    None
}

/// `(dev, ino)`, the pair that says whether this is still the same file. No
/// answer off unix, where the length check carries the whole load.
#[cfg(unix)]
fn identity(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn identity(_meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    None
}

/// "2026-09-02T22:24:08.034Z" → millis. Enough of ISO 8601 for the timestamps
/// a rollout writes; anything else gets `None` and the caller stamps now.
/// (`remote_api` keeps a seconds-only sibling for the sessions list; the
/// spine needs millis, because a call and its result land in the same second.)
fn iso_to_ms(s: &str) -> Option<u64> {
    let (date, rest) = s.split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let time = rest.trim_end_matches('Z');
    let time = time.split(['+', '-']).next().unwrap_or(time);
    let mut t = time.split(':');
    let (h, mi) = (t.next()?.parse::<i64>().ok()?, t.next()?.parse::<i64>().ok()?);
    let secs_part = t.next()?;
    let (sec, frac) = match secs_part.split_once('.') {
        Some((s, f)) => (s.parse::<i64>().ok()?, f),
        None => (secs_part.parse::<i64>().ok()?, ""),
    };
    let millis: i64 = format!("{frac:0<3}")[..3].parse().unwrap_or(0);
    // Days from civil (Howard Hinnant), no calendar crate needed.
    let (y2, m2) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * m2 + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    u64::try_from((days * 86400 + h * 3600 + mi * 60 + sec) * 1000 + millis).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::Path;

    /// Verbatim lines from `~/.codex/sessions` on this machine. Long strings
    /// (patch bodies, command output, the base instructions in the header)
    /// are cut, but nothing is reshaped.
    /// [observed: codex-cli 0.151.0, 2026-09-02]
    const HEADER: &str = r##"{"timestamp":"2026-09-02T22:24:08.034Z","ordinal":0,"type":"session_meta","payload":{"session_id":"01a06438-eeb2-7202-857e-5eacd8dbf644","id":"01a06438-eeb2-7202-857e-5eacd8dbf644","timestamp":"2026-09-02T22:24:07.862Z","cwd":"/home/admin/AI-OS","originator":"codex-tui","cli_version":"0.151.0","source":"cli","thread_source":"user","model_provider":"openai","base_instructions":{"text":"You are Codex"}}}"##;

    const TASK_STARTED: &str = r##"{"timestamp":"2026-09-02T22:24:08.034Z","ordinal":1,"type":"event_msg","payload":{"type":"task_started","turn_id":"01a06438-ef47-7d91-a661-d26a0b396c26","started_at":1788387848,"model_context_window":258400,"collaboration_mode_kind":"default"}}"##;

    const TASK_COMPLETE: &str = r##"{"timestamp":"2026-09-02T22:24:26.009Z","ordinal":29,"type":"event_msg","payload":{"type":"task_complete","turn_id":"01a06438-ef47-7d91-a661-d26a0b396c26","last_agent_message":"Antigravity, the article is informative","started_at":1788387848,"completed_at":1788387866,"duration_ms":17975,"time_to_first_token_ms":1588}}"##;

    /// The harness's own system prompt, three or four of these per turn.
    const DEVELOPER: &str = r##"{"timestamp":"2026-09-02T22:24:09.662Z","ordinal":2,"type":"response_item","payload":{"type":"message","id":"msg_01a06438-f5be-7e50-b2ea-3136adb26c8e","role":"developer","content":[{"type":"input_text","text":"You are `/root`, the primary agent in a team of agents collaborating to fulfill the user's goals."}],"internal_chat_message_metadata_passthrough":{}}}"##;

    /// The first `user` record of every session: the AGENTS.md codex loaded
    /// and the environment block, and nothing a person typed.
    const USER_SCAFFOLD: &str = r##"{"timestamp":"2026-09-02T22:24:09.662Z","ordinal":5,"type":"response_item","payload":{"type":"message","id":"msg_01a06438-f5be-7e50-b2ea-3157bfc24ea1","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions for /home/admin/AI-OS\n\n<INSTRUCTIONS>\n# AI-OS — How Agents Work Here\n\nUser: john.\n</INSTRUCTIONS>"},{"type":"input_text","text":"<environment_context>\n  <cwd>/home/admin/AI-OS</cwd>\n  <shell>bash</shell>\n</environment_context>"}],"internal_chat_message_metadata_passthrough":{}}}"##;

    /// The other shape of pure scaffolding: a plugin catalogue the phone's
    /// filter does not know about.
    const USER_PLUGINS: &str = r##"{"timestamp":"2026-08-30T20:44:00.000Z","ordinal":6,"type":"response_item","payload":{"type":"message","id":"msg_plugins","role":"user","content":[{"type":"input_text","text":"<recommended_plugins>\nHere is a list of plugins that are available but not installed.\n\n- Box (box@openai-curated-remote)\n</recommended_plugins>"}],"internal_chat_message_metadata_passthrough":{}}}"##;

    const USER: &str = r##"{"timestamp":"2026-09-02T22:24:55.354Z","ordinal":33,"type":"response_item","payload":{"type":"message","id":"msg_01a06439-a83a-7461-8dff-56245740268c","role":"user","content":[{"type":"input_text","text":"viewthe conversation and tell meif they improved"}],"internal_chat_message_metadata_passthrough":{"turn_id":"01a06439-a80f-7e91-ae55-bbe1881bcc9b","create_time":1788387895.3541527,"content_item_kinds":["user.text"]}}}"##;

    /// The TUI's mirror of `USER`. Same words, a different id, no new fact.
    const USER_MIRROR: &str = r##"{"timestamp":"2026-09-02T22:24:55.354Z","ordinal":34,"type":"event_msg","payload":{"type":"item_completed","thread_id":"01a06438-eeb2-7202-857e-5eacd8dbf644","turn_id":"01a06439-a80f-7e91-ae55-bbe1881bcc9b","item":{"type":"UserMessage","id":"01a06439-a83a-7461-8dff-563e0f4a7e95","content":[{"type":"text","text":"viewthe conversation and tell meif they improved","text_elements":[]}]},"started_at_ms":1788387895354,"completed_at_ms":1788387895354}}"##;

    const ASSISTANT: &str = r##"{"timestamp":"2026-09-02T22:24:11.622Z","ordinal":11,"type":"response_item","payload":{"type":"message","id":"msg_0c83d0280e8ec08f016a98a20ad16c87d18901890fb302c28c","role":"assistant","content":[{"type":"output_text","text":"I’ll read the live transcript, identify the passage they want improved."}],"phase":"final_answer","internal_chat_message_metadata_passthrough":{"turn_id":"01a06438-ef47-7d91-a661-d26a0b396c26"}}}"##;

    /// The TUI's mirror of `ASSISTANT`, written a millisecond earlier and
    /// sharing its id.
    const ASSISTANT_MIRROR: &str = r##"{"timestamp":"2026-09-02T22:24:11.622Z","ordinal":10,"type":"event_msg","payload":{"type":"item_completed","thread_id":"01a06438-eeb2-7202-857e-5eacd8dbf644","turn_id":"01a06438-ef47-7d91-a661-d26a0b396c26","item":{"type":"AgentMessage","id":"msg_0c83d0280e8ec08f016a98a20ad16c87d18901890fb302c28c","phase":"final_answer","content":[{"type":"Text","text":"I’ll read the live transcript, identify the passage they want improved."}]}}}"##;

    /// Reasoning as every rollout on this machine writes it: encrypted, with
    /// an empty summary and nothing to show.
    const REASONING_EMPTY: &str = r##"{"timestamp":"2026-09-02T22:24:15.466Z","ordinal":16,"type":"response_item","payload":{"type":"reasoning","id":"rs_0c83d0280e8ec08f016a98a20e1e4487d1acc8cab5d491c068","summary":[],"encrypted_content":"gAAAAABqg4GqKs2ezTXyvt5Mjoab","internal_chat_message_metadata_passthrough":{"turn_id":"01a06438-ef47-7d91-a661-d26a0b396c26"}}}"##;

    /// The same record with a summary in it, as a model that returns one
    /// unencrypted writes it. No rollout here carries one; the shape is the
    /// Responses API's, which is what `detail.rs` already reads.
    const REASONING: &str = r##"{"timestamp":"2026-09-02T22:24:15.466Z","ordinal":16,"type":"response_item","payload":{"type":"reasoning","id":"rs_summarised","summary":[{"type":"summary_text","text":"Reading the relay transcript first."}],"encrypted_content":null,"internal_chat_message_metadata_passthrough":{}}}"##;

    const EXEC_CALL: &str = r##"{"timestamp":"2026-09-02T22:24:13.256Z","ordinal":12,"type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_0c83d0280e8ec08f016a98a20b7a5887d1b7c4a79ed9a79b49","status":"completed","call_id":"call_YH30YPzrngNxj7cJGNWJTyYo","name":"exec","input":"const r = await tools.exec_command({cmd:\"sed -n '1,240p' /home/admin/relay.txt\",\"workdir\":\"/home/admin/AI-OS\",\"yield_time_ms\":10000,\"max_output_tokens\":20000}); text(r.output);\n","internal_chat_message_metadata_passthrough":{"turn_id":"01a06438-ef47-7d91-a661-d26a0b396c26"}}}"##;

    const EXEC_ITEM_OK: &str = r##"{"timestamp":"2026-09-02T22:24:13.347Z","ordinal":13,"type":"event_msg","payload":{"type":"item_completed","thread_id":"01a06438-eeb2-7202-857e-5eacd8dbf644","turn_id":"01a06438-ef47-7d91-a661-d26a0b396c26","item":{"type":"CommandExecution","id":"exec-deb6955e-4d62-4fa8-b6b9-a1853414d198","process_id":"44742","command":["/bin/bash","-lc","sed -n '1,240p' /home/admin/relay.txt"],"cwd":"file:///home/admin/AI-OS","parsed_cmd":[{"type":"read","cmd":"sed -n '1,240p' /home/admin/relay.txt","name":"relay.txt"}],"source":"unified_exec_startup","status":"completed","exit_code":0,"duration":{"secs":0,"nanos":3441},"stdout":"[user]\nwrite an article","aggregated_output":"[user]\nwrite an article","formatted_output":"[user]\nwrite an article"}}}"##;

    const EXEC_OUTPUT: &str = r##"{"timestamp":"2026-09-02T22:24:13.351Z","ordinal":14,"type":"response_item","payload":{"type":"custom_tool_call_output","id":"ctco_01a06439-0427-77b3-9954-bcbabf122ab7","call_id":"call_YH30YPzrngNxj7cJGNWJTyYo","output":[{"type":"input_text","text":"Script completed\nWall time 0.1 seconds\nOutput:\n"},{"type":"input_text","text":"[user]\nwrite an article"}],"internal_chat_message_metadata_passthrough":{"turn_id":"01a06438-ef47-7d91-a661-d26a0b396c26"}}}"##;

    /// A command that exited 1. Note the output record still says "Script
    /// completed": the failure is only in the item.
    const FAIL_CALL: &str = r##"{"timestamp":"2026-08-31T00:44:03.210Z","ordinal":20,"type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_0b521e3bd0b4fc80016a94ce51ea0c87d186168ccf9eb3fbf7","status":"completed","call_id":"call_5Kr0K8hQ1nSDOo4x6dnDn6Gk","name":"exec","input":"const r = await tools.exec_command({\"cmd\":\"sed -n '1,200p' .gitignore && git config --global user.name\",\"workdir\":\"/home/admin/AI-OS\",\"yield_time_ms\":10000,\"max_output_tokens\":4000});\ntext(r.output);\n","internal_chat_message_metadata_passthrough":{"turn_id":"01a05545-cae2-76e0-8eb0-d4a92bc43aa7"}}}"##;

    const FAIL_ITEM: &str = r##"{"timestamp":"2026-08-31T00:44:03.276Z","ordinal":21,"type":"event_msg","payload":{"type":"item_completed","thread_id":"01a05545-ca47-7023-8d54-2c9444f9578d","turn_id":"01a05545-cae2-76e0-8eb0-d4a92bc43aa7","item":{"type":"CommandExecution","id":"exec-950a0e7d-6ea2-448e-ba49-896d68cf36b4","process_id":"24823","command":["/bin/bash","-lc","sed -n '1,200p' .gitignore && git config --global user.name"],"cwd":"file:///home/admin/AI-OS","parsed_cmd":[{"type":"unknown","cmd":"sed -n '1,200p' .gitignore"}],"source":"unified_exec_startup","status":"failed","exit_code":1,"stdout":"# secrets — never commit\n.env\n","stderr":"","aggregated_output":"# secrets — never commit\n.env\n"}}}"##;

    const FAIL_OUTPUT: &str = r##"{"timestamp":"2026-08-31T00:44:03.280Z","ordinal":22,"type":"response_item","payload":{"type":"custom_tool_call_output","id":"ctco_01a05545-f550-7413-892e-b914a68c8877","call_id":"call_5Kr0K8hQ1nSDOo4x6dnDn6Gk","output":[{"type":"input_text","text":"Script completed\nWall time 0.1 seconds\nOutput:\n"},{"type":"input_text","text":"# secrets — never commit\n.env\n"}],"internal_chat_message_metadata_passthrough":{"turn_id":"01a05545-cae2-76e0-8eb0-d4a92bc43aa7"}}}"##;

    const PATCH_CALL: &str = r##"{"timestamp":"2026-08-17T21:48:55.704Z","ordinal":110,"type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_0fd0581e3245c7cf016a8381b2662c87d19df699d92927e92e","status":"completed","call_id":"call_9Bask253ycPm6DYD1XYBLZk8","name":"exec","input":"const patch = \"*** Begin Patch\\n*** Add File: scripts/recover-nzbfinder.sh\\n+#!/bin/sh\\n+set -u\\n*** End Patch\\n\";\ntext(await tools.apply_patch(patch));\n","internal_chat_message_metadata_passthrough":{"turn_id":"01a011b1-a3e8-7e32-b6b0-d41515331dd1"}}}"##;

    const PATCH_END: &str = r##"{"timestamp":"2026-08-17T21:48:55.727Z","ordinal":111,"type":"event_msg","payload":{"type":"patch_apply_end","call_id":"exec-5a00bd06-41c3-4d7a-b8f4-1a8e25e81763","turn_id":"01a011b1-a3e8-7e32-b6b0-d41515331dd1","stdout":"Success. Updated the following files:\nA scripts/recover-nzbfinder.sh\n","stderr":"","success":true,"changes":{"/home/admin/AI-OS/scripts/recover-nzbfinder.sh":{"type":"add","content":"#!/bin/sh\n"}},"status":"completed"}}"##;

    /// `tools.apply_patch` answers `{}`. Without the record above there would
    /// be nothing to show for a patch at all.
    const PATCH_OUTPUT: &str = r##"{"timestamp":"2026-08-17T21:48:56.292Z","ordinal":112,"type":"response_item","payload":{"type":"custom_tool_call_output","id":"ctco_01a011b2-f664-7ae0-9de0-ba3b335d2f00","call_id":"call_9Bask253ycPm6DYD1XYBLZk8","output":[{"type":"input_text","text":"Script completed\nWall time 0.0 seconds\nOutput:\n"},{"type":"input_text","text":"{}"}],"internal_chat_message_metadata_passthrough":{"turn_id":"01a011b1-a3e8-7e32-b6b0-d41515331dd1"}}}"##;

    /// The plain `function_call` shape, which is also how a tool from an MCP
    /// server arrives — a namespaced name and JSON arguments. Taken from a
    /// `collaboration` call and renamed: no MCP server was configured on this
    /// machine while these rollouts were written.
    const MCP_CALL: &str = r##"{"timestamp":"2026-08-29T23:31:34.000Z","ordinal":120,"type":"response_item","payload":{"type":"function_call","id":"fc_02846e32f0fdc48f016a936b44ef9c87d18a0821787d86fcb4","call_id":"call_6xiNmVZNm88Vs4VhYxACyl8q","name":"mcp__runpod__list_endpoints","namespace":"mcp","arguments":"{\"query\":\"gpu pods\"}","internal_chat_message_metadata_passthrough":{"turn_id":"01a04fda-e718-7f41-9dab-8f18daabda1a"}}}"##;

    const MCP_OUTPUT: &str = r##"{"timestamp":"2026-08-29T23:31:35.000Z","ordinal":121,"type":"response_item","payload":{"type":"function_call_output","id":"fco_1","call_id":"call_6xiNmVZNm88Vs4VhYxACyl8q","output":"{\"endpoints\":[]}","internal_chat_message_metadata_passthrough":{}}}"##;

    const TURN_ABORTED: &str = r##"{"timestamp":"2026-08-29T23:33:57.767Z","ordinal":139,"type":"event_msg","payload":{"type":"turn_aborted","turn_id":"01a04fdd-97ac-73e3-addf-f4b851ade927","reason":"interrupted","started_at":1788046317,"completed_at":1788046437,"duration_ms":120279}}"##;

    const TOKEN_COUNT: &str = r##"{"timestamp":"2026-09-02T22:24:13.352Z","ordinal":15,"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":564942,"total_tokens":571113},"model_context_window":258400}}}"##;

    const TURN_CONTEXT: &str = r##"{"timestamp":"2026-09-02T22:24:09.664Z","ordinal":7,"type":"turn_context","payload":{"cwd":"/home/admin/AI-OS","approval_policy":"on-request","model":"gpt-5.6-sol"}}"##;

    fn adapter_over(path: &Path) -> CodexAdapter {
        CodexAdapter {
            path: path.to_path_buf(),
            offset: 0,
            pending: Vec::new(),
            ident: None,
            line: 0,
            turn: None,
            open: None,
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aiterm-spine-codex-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("rollout.jsonl")
    }

    fn write(path: &Path, bytes: &str) {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
        f.write_all(bytes.as_bytes()).unwrap();
    }

    /// Feed lines straight through the parser, no file involved.
    fn kinds(lines: &[&str]) -> Vec<Kind> {
        let mut a = adapter_over(Path::new("/nonexistent"));
        let mut out = Vec::new();
        for l in lines {
            a.read_line(l.as_bytes(), &mut out);
        }
        out.into_iter().map(|(_, k)| k).collect()
    }

    #[test]
    fn the_header_and_the_bookkeeping_say_nothing() {
        assert!(kinds(&[HEADER, DEVELOPER, TOKEN_COUNT, TURN_CONTEXT]).is_empty());
    }

    #[test]
    fn a_turn_opens_on_the_engines_own_turn_id() {
        assert_eq!(
            kinds(&[TASK_STARTED, USER]),
            vec![
                Kind::TurnStarted { turn: "01a06438-ef47-7d91-a661-d26a0b396c26".into() },
                Kind::UserMessage {
                    id: "msg_01a06439-a83a-7461-8dff-56245740268c".into(),
                    text: "viewthe conversation and tell meif they improved".into(),
                },
            ]
        );
    }

    #[test]
    fn a_turn_closes_naming_the_turn_it_opened() {
        assert_eq!(
            kinds(&[TASK_STARTED, USER, ASSISTANT, TASK_COMPLETE]).last().unwrap(),
            &Kind::TurnEnded {
                turn: "01a06438-ef47-7d91-a661-d26a0b396c26".into(),
                reason: "completed".into(),
            }
        );
    }

    #[test]
    fn a_tail_that_missed_the_turn_opening_still_opens_one() {
        // No `task_started` in front of it: the message names its own turn.
        assert_eq!(
            kinds(&[USER])[0],
            Kind::TurnStarted { turn: "msg_01a06439-a83a-7461-8dff-56245740268c".into() }
        );
    }

    #[test]
    fn codexs_own_preamble_is_not_something_a_person_said() {
        assert!(kinds(&[USER_SCAFFOLD]).is_empty(), "AGENTS.md and the environment block");
        assert!(kinds(&[USER_PLUGINS]).is_empty(), "the plugin catalogue");
        // Words BETWEEN two blocks of one tag are words, not scaffolding.
        assert!(!harness_only("<INSTRUCTIONS>a</INSTRUCTIONS> ship it <INSTRUCTIONS>b</INSTRUCTIONS>"));
        assert!(harness_only("<INSTRUCTIONS>a</INSTRUCTIONS>\n<INSTRUCTIONS>b</INSTRUCTIONS>"));
        // An envelope that never closes is text, not a block to swallow.
        assert!(!harness_only("<INSTRUCTIONS>truncated"));
    }

    #[test]
    fn a_message_that_carries_a_harness_block_and_words_keeps_both() {
        // The text goes over the wire whole; the phone is what strips it.
        let both = USER_SCAFFOLD.replace(
            "</INSTRUCTIONS>",
            "</INSTRUCTIONS>\\n\\nnow fix the build",
        );
        assert_eq!(
            kinds(&[&both]).last().unwrap(),
            &Kind::UserMessage {
                id: "msg_01a06438-f5be-7e50-b2ea-3157bfc24ea1".into(),
                text: "# AGENTS.md instructions for /home/admin/AI-OS\n\n<INSTRUCTIONS>\n# AI-OS — How Agents Work Here\n\nUser: john.\n</INSTRUCTIONS>\n\nnow fix the build\n<environment_context>\n  <cwd>/home/admin/AI-OS</cwd>\n  <shell>bash</shell>\n</environment_context>".into(),
            }
        );
    }

    #[test]
    fn an_assistant_message_is_a_whole_block_keyed_by_the_apis_id() {
        assert_eq!(
            kinds(&[ASSISTANT]),
            vec![Kind::AgentText {
                id: "msg_0c83d0280e8ec08f016a98a20ad16c87d18901890fb302c28c".into(),
                text: "I’ll read the live transcript, identify the passage they want improved."
                    .into(),
                done: true,
            }]
        );
    }

    /// The rollout says everything twice. Once each, please.
    #[test]
    fn the_tuis_mirror_of_a_message_is_not_a_second_message() {
        assert_eq!(
            kinds(&[ASSISTANT_MIRROR, ASSISTANT, USER_MIRROR, USER]).len(),
            3,
            "the assistant block, the user's turn, and the user's words"
        );
        assert_eq!(
            kinds(&[ASSISTANT_MIRROR, ASSISTANT]),
            kinds(&[ASSISTANT]),
            "the mirror adds nothing at all"
        );
    }

    #[test]
    fn encrypted_reasoning_shows_nothing_and_a_summary_shows_itself() {
        assert!(kinds(&[REASONING_EMPTY]).is_empty());
        assert_eq!(
            kinds(&[REASONING]),
            vec![Kind::AgentThought {
                id: "rs_summarised".into(),
                text: "Reading the relay transcript first.".into(),
                done: true,
            }]
        );
    }

    #[test]
    fn a_shell_call_wears_its_command_and_closes_on_its_output() {
        assert_eq!(
            kinds(&[EXEC_CALL, EXEC_ITEM_OK, EXEC_OUTPUT]),
            vec![
                Kind::ToolCall {
                    id: "call_YH30YPzrngNxj7cJGNWJTyYo".into(),
                    tool: "exec_command".into(),
                    title: "sed -n '1,240p' /home/admin/relay.txt".into(),
                    category: ToolCategory::Execute,
                    input: "sed -n '1,240p' /home/admin/relay.txt".into(),
                    status: ToolStatus::Running,
                },
                Kind::ToolCallUpdate {
                    id: "call_YH30YPzrngNxj7cJGNWJTyYo".into(),
                    status: ToolStatus::Completed,
                    output: Some(
                        "Script completed\nWall time 0.1 seconds\nOutput:\n\n[user]\nwrite an article"
                            .into()
                    ),
                },
            ]
        );
    }

    /// The only record that knows the command failed is the TUI's.
    #[test]
    fn a_nonzero_exit_is_a_failed_card_even_though_the_output_says_completed() {
        assert!(
            FAIL_OUTPUT.contains("Script completed"),
            "the call's own output is cheerful about it"
        );
        let out = kinds(&[FAIL_CALL, FAIL_ITEM, FAIL_OUTPUT]);
        assert!(matches!(
            &out[1],
            Kind::ToolCallUpdate { status: ToolStatus::Failed, .. }
        ));
        // Without the item there is nothing to go on, so it reads as fine.
        let blind = kinds(&[FAIL_CALL, FAIL_OUTPUT]);
        assert!(matches!(
            &blind[1],
            Kind::ToolCallUpdate { status: ToolStatus::Completed, .. }
        ));
    }

    #[test]
    fn a_patch_is_an_edit_named_by_its_files_and_answered_by_the_harness() {
        assert_eq!(
            kinds(&[PATCH_CALL, PATCH_END, PATCH_OUTPUT]),
            vec![
                Kind::ToolCall {
                    id: "call_9Bask253ycPm6DYD1XYBLZk8".into(),
                    tool: "apply_patch".into(),
                    title: "A scripts/recover-nzbfinder.sh".into(),
                    category: ToolCategory::Edit,
                    input: "A scripts/recover-nzbfinder.sh".into(),
                    status: ToolStatus::Running,
                },
                Kind::ToolCallUpdate {
                    id: "call_9Bask253ycPm6DYD1XYBLZk8".into(),
                    status: ToolStatus::Completed,
                    output: Some(
                        "Success. Updated the following files:\nA scripts/recover-nzbfinder.sh\n"
                            .into()
                    ),
                },
            ],
            "the patch's own answer is an empty object — the files come from patch_apply_end"
        );
    }

    #[test]
    fn a_namespaced_tool_wears_its_own_name() {
        let out = kinds(&[MCP_CALL, MCP_OUTPUT]);
        assert_eq!(
            out[0],
            Kind::ToolCall {
                id: "call_6xiNmVZNm88Vs4VhYxACyl8q".into(),
                tool: "mcp__runpod__list_endpoints".into(),
                title: "gpu pods".into(),
                category: ToolCategory::Other,
                input: "gpu pods".into(),
                status: ToolStatus::Running,
            },
            "a plain JSON tool: no `exec` snippet to read, the arguments say it"
        );
        assert_eq!(
            out[1],
            Kind::ToolCallUpdate {
                id: "call_6xiNmVZNm88Vs4VhYxACyl8q".into(),
                status: ToolStatus::Completed,
                output: Some("{\"endpoints\":[]}".into()),
            }
        );
        // A tail that started between the call and its answer still lands the
        // answer on the card the bootstrap will have built.
        assert_eq!(kinds(&[MCP_OUTPUT]), vec![out[1].clone()]);
    }

    #[test]
    fn an_interrupted_turn_says_so_and_stops_the_card_spinning() {
        assert_eq!(
            kinds(&[TASK_STARTED, EXEC_CALL, TURN_ABORTED]),
            vec![
                Kind::TurnStarted { turn: "01a06438-ef47-7d91-a661-d26a0b396c26".into() },
                Kind::ToolCall {
                    id: "call_YH30YPzrngNxj7cJGNWJTyYo".into(),
                    tool: "exec_command".into(),
                    title: "sed -n '1,240p' /home/admin/relay.txt".into(),
                    category: ToolCategory::Execute,
                    input: "sed -n '1,240p' /home/admin/relay.txt".into(),
                    status: ToolStatus::Running,
                },
                Kind::ToolCallUpdate {
                    id: "call_YH30YPzrngNxj7cJGNWJTyYo".into(),
                    status: ToolStatus::Cancelled,
                    output: None,
                },
                Kind::TurnEnded {
                    turn: "01a04fdd-97ac-73e3-addf-f4b851ade927".into(),
                    reason: "interrupted".into(),
                },
            ]
        );
    }

    #[test]
    fn a_line_is_stamped_with_its_own_timestamp() {
        let mut a = adapter_over(Path::new("/nonexistent"));
        let mut out = Vec::new();
        a.read_line(USER.as_bytes(), &mut out);
        // 2026-09-02T22:24:55.354Z
        assert_eq!(out[0].0, 1_788_387_895_354);
    }

    #[test]
    fn what_a_call_is_comes_out_of_the_javascript_it_was_written_in() {
        let (tool, detail, cat) = shape("exec", "const r = await tools.web__run({search_query:[\n {q:\"codex rollout format\"}]});");
        assert_eq!((tool.as_str(), detail.as_str(), cat), ("web_search", "codex rollout format", ToolCategory::Search));
        let (tool, detail, cat) =
            shape("exec", "await tools.image_gen__imagegen({prompt:\"a dog on a bicycle\"});");
        assert_eq!((tool.as_str(), detail.as_str(), cat), ("image_gen", "a dog on a bicycle", ToolCategory::Other));
        let (tool, detail, cat) = shape("exec", "tools.view_image({path:\"/tmp/shot.png\"})");
        assert_eq!((tool.as_str(), detail.as_str(), cat), ("view_image", "/tmp/shot.png", ToolCategory::Read));
        // A subagent being spawned: a plain function call with JSON arguments.
        let (tool, _, cat) = shape("spawn_agent", "{\"task_name\":\"weather_review\"}");
        assert_eq!((tool.as_str(), cat), ("spawn_agent", ToolCategory::Think));
        // A patch that touches three files reads as three lines.
        assert_eq!(
            patch_files("\"*** Begin Patch\\n*** Update File: a.rs\\n*** Delete File: b.rs\\n*** Add File: c.rs\\n*** End Patch\""),
            "M a.rs\nD b.rs\nA c.rs"
        );
    }

    #[test]
    fn a_record_with_no_id_of_its_own_is_named_by_its_line() {
        // The shape a forked thread replays from its parent: no `id`.
        let anonymous = r##"{"timestamp":"2026-08-29T23:29:03.197Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"replayed"}]}}"##;
        assert_eq!(
            kinds(&[HEADER, anonymous]),
            vec![Kind::AgentText { id: "codex:2".into(), text: "replayed".into(), done: true }],
            "stable because bootstrap always counts from the first line"
        );
    }

    #[test]
    fn a_line_split_across_two_polls_is_held_until_it_closes() {
        let path = scratch("partial");
        let mut a = adapter_over(&path);
        assert!(a.bootstrap().is_empty());
        let (head, tail) = USER.split_at(USER.len() / 2);
        write(&path, head);
        assert!(a.poll().is_empty(), "half a record is not a record");
        write(&path, &format!("{tail}\n"));
        assert_eq!(a.poll().len(), 2, "the turn and the message");
        write(&path, &format!("{ASSISTANT}\n"));
        assert_eq!(a.poll().len(), 1);
        assert!(a.poll().is_empty(), "a file nobody wrote to says nothing");
    }

    #[test]
    fn a_replaced_rollout_is_a_reset_and_a_replay() {
        let path = scratch("replaced");
        let mut a = adapter_over(&path);
        write(&path, &format!("{HEADER}\n{TASK_STARTED}\n{USER}\n{ASSISTANT}\n"));
        assert_eq!(a.bootstrap().len(), 3);
        // A shorter file under the same name.
        std::fs::write(&path, format!("{HEADER}\n{TASK_STARTED}\n")).unwrap();
        let out = a.poll();
        assert_eq!(out[0].1, Kind::Reset);
        assert_eq!(out.len(), 2, "the reset, then the rebuilt history");
        assert!(matches!(out[1].1, Kind::TurnStarted { .. }));
    }

    #[test]
    fn a_missing_rollout_is_an_empty_session_that_fills_in_later() {
        let path = scratch("late");
        let mut a = adapter_over(&path);
        assert!(a.bootstrap().is_empty());
        assert_eq!(a.watch_paths().len(), 2, "the file and the day folder it lands in");
        write(&path, &format!("{HEADER}\n{TASK_STARTED}\n{USER}\n"));
        assert_eq!(a.poll().len(), 2);
    }

    /// The real thing: bootstrap every rollout on this machine and report what
    /// came out. Ignored because it depends on files that only exist where
    /// codex has run.
    ///
    /// `cargo test --lib spine::codex::tests::real_rollouts -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_rollouts_turn_into_events() {
        let root = dirs::home_dir().unwrap().join(".codex/sessions");
        let mut files = Vec::new();
        collect(&root, &mut files);
        files.sort();
        assert!(!files.is_empty(), "no rollouts under {}", root.display());

        let (mut all_lines, mut all_events, mut all_ms) = (0usize, 0usize, 0u128);
        let mut totals: std::collections::BTreeMap<&str, usize> = Default::default();
        for path in &files {
            let mut a = adapter_over(path);
            let lines = std::fs::read_to_string(path).map(|s| s.lines().count()).unwrap_or(0);
            let started = std::time::Instant::now();
            let events = a.bootstrap();
            let took = started.elapsed();
            let mut tally: std::collections::BTreeMap<&str, usize> = Default::default();
            for (_, k) in &events {
                *tally.entry(kind_name(k)).or_default() += 1;
                *totals.entry(kind_name(k)).or_default() += 1;
            }
            println!(
                "{}: {lines} lines → {} events in {} ms {tally:?}",
                path.file_name().unwrap().to_string_lossy(),
                events.len(),
                took.as_millis()
            );
            assert!(a.poll().is_empty(), "a second poll on a finished rollout is empty");
            all_lines += lines;
            all_events += events.len();
            all_ms += took.as_millis();
        }
        println!(
            "\n{} rollouts: {all_lines} lines → {all_events} events in {all_ms} ms {totals:?}",
            files.len()
        );

        // And the same file reached the way the registry reaches it: by id.
        let newest = files.last().unwrap();
        let id = std::fs::read_to_string(newest)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s.lines().next()?).ok())
            .and_then(|v| v.pointer("/payload/session_id")?.as_str().map(String::from))
            .expect("a header with a session id");
        let a = open(&id).expect("the registry finds a codex session by its id");
        println!("open({id}) → {}", a.path.display());
        assert!(super::super::open_adapter("codex", &id).is_some());
    }

    #[cfg(test)]
    fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect(&p, out);
            } else if p.extension().is_some_and(|x| x == "jsonl") {
                out.push(p);
            }
        }
    }

    fn kind_name(k: &Kind) -> &'static str {
        match k {
            Kind::UserMessage { .. } => "user_message",
            Kind::AgentText { .. } => "agent_text",
            Kind::AgentThought { .. } => "agent_thought",
            Kind::ToolCall { .. } => "tool_call",
            Kind::ToolCallUpdate { .. } => "tool_call_update",
            Kind::TurnStarted { .. } => "turn_started",
            Kind::TurnEnded { .. } => "turn_ended",
            Kind::Phase { .. } => "phase",
            Kind::Reset => "reset",
        }
    }
}
