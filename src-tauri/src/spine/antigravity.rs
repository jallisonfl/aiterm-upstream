//! Antigravity (`agy`) adapter: the step log at
//! `~/.gemini/antigravity-cli/brain/<id>/.system_generated/logs/transcript.jsonl`,
//! read from a byte offset, one JSON object per step.
//!
//! Owned by the antigravity-adapter task. See `docs/architecture/spine.md`.
//!
//! agy appends one record per *step* as a turn runs, and a step is the unit
//! of everything here:
//!
//! ```text
//! {"step_index":4,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE",
//!  "created_at":"2026-09-03T01:16:24Z",
//!  "thinking":"…", "content":"…",
//!  "tool_calls":[{"name":"run_command","args":{"CommandLine":"\"echo hi\"",…}}],
//!  "truncated_fields":["content"]}
//! ```
//!
//! Four `type`s were ever seen: `USER_INPUT` (the person, inside an envelope
//! [`crate::antigravity::user_request`] strips), `PLANNER_RESPONSE` (the
//! model — `thinking` and/or `content` and/or `tool_calls`), `GENERIC` (one
//! tool's output) and `SYSTEM_MESSAGE` (the "subagents stopped due to server
//! restart" notice agy injects on every resume, which says nothing to a
//! person and is dropped). `status` was `DONE` on all 48 records read.
//!
//! Four things this file exists to get right, all of them observed rather
//! than assumed:
//!
//! * **Steps carry no ids.** `<conversation>:<step_index>` names a step and
//!   `<conversation>:<step_index>:<i>` names the i-th tool call in it. The
//!   file is append-only and an index is never reused, so both survive a
//!   re-read.
//! * **An output is paired to its call by index arithmetic, not by
//!   position.** A `PLANNER_RESPONSE` at index S with N calls is answered by
//!   the `GENERIC` steps at S+1 … S+N, in call order. Position alone is not
//!   enough because —
//! * **File order is not step order.** With two parallel calls the first
//!   output (index 2) was written *before* the step that issued it (index
//!   1): the real file starts `0, 2, 1, 3, 4, …`. An output whose call has
//!   not been read yet is held in `orphans` until the call registers it.
//! * **A tool that fails writes no output step at all.** Its index is
//!   consumed and skipped: a `view_file` of a missing path, a `list_dir` of
//!   a missing directory and a `write_to_file` to an unwritable path each
//!   left a hole in the sequence (`…,27,29,…` / `…,1,3,…`). So a call whose
//!   expected index is passed by a later `USER_INPUT`/`PLANNER_RESPONSE` is
//!   reported `Failed` — that hole IS the failure signal. The one exception
//!   is `run_command`, which writes its output whatever the exit code; there
//!   the code in the text decides.
//!
//! The SQLite `conversations/<id>.db` beside the transcript is not read, and
//! watching it would be worse than useless: over a 20 s turn sampled every
//! 200 ms its main file was touched exactly once, at startup, and never
//! again. Every write goes to the `-wal`, which moves within a few ms of the
//! transcript at each step and never led it by more than ~80 ms. The
//! transcript is written at step completion, so the whole feed is
//! block-level: no token streaming, and `done` is always true.
//!
//! [observed: agy 1.1.24, 2026-09-02, five conversations under
//! ~/.gemini/antigravity-cli/brain]

use super::{clip, now_ms, Adapter, Kind, Phase, ToolCategory, ToolStatus};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{Read as _, Seek, SeekFrom};
use std::path::PathBuf;

/// A tool call's one-line input summary, on the wire.
const INPUT_CAP: usize = 400;
/// A tool result's output. Bigger than the input, as in the other adapters:
/// the output is what a person reads to know whether the call worked.
const OUTPUT_CAP: usize = 2_000;
/// A card's heading.
const TITLE_CAP: usize = 200;
/// One argument inside the input summary, so a `CodeContent` of a whole file
/// cannot push the `TargetFile` beside it off the line.
const ARG_CAP: usize = 120;
/// Outputs held waiting for the call that owns them. One is enough for the
/// reordering ever seen; the cap only stops a pathological file from growing
/// the map without bound.
const ORPHANS_MAX: usize = 64;

pub struct AntigravityAdapter {
    /// The conversation uuid — the first half of every id this adapter mints.
    id: String,
    path: PathBuf,
    /// Bytes of the file already handed to the parser, complete lines or not.
    offset: u64,
    /// The tail after the last `\n`: a line agy has not finished. Bytes, not
    /// a `String`, because the split can land inside a multi-byte character.
    pending: Vec<u8>,
    /// `(dev, ino)` as last read, to notice a replacement whose length
    /// happens not to shrink.
    ident: Option<(u64, u64)>,
    /// `step_index` of an output not yet written → the call waiting for it.
    awaiting: BTreeMap<u64, Awaited>,
    /// `step_index` → an output whose call has not been read yet.
    orphans: BTreeMap<u64, (u64, String)>,
    /// The `step_index` of the `USER_INPUT` that opened the running turn.
    turn: Option<String>,
    /// Whether the last thing we said on the phase channel was `NeedsYou`.
    asking: bool,
    /// The agy store, for the db that holds what the transcript cut.
    root: PathBuf,
}

/// A tool call with its output still to come.
struct Awaited {
    /// The event id of the `ToolCall`, so the update lands on that card.
    call: String,
    /// Whether it is one of agy's `ask_*` tools — a question to the person,
    /// and so the phase this adapter owns.
    ask: bool,
}

/// The adapter for an antigravity conversation, or `None` when there is no
/// agy store or the id could not name a file in it.
///
/// The transcript itself need not exist: agy mints the conversation id
/// server-side and the tab is *adopted* a moment later, so a session can be
/// bound before `brain/<id>/` has been created at all. `poll` reads an
/// absent file as empty and `watch_paths` arms the nearest directory that
/// does exist.
pub fn open(session_id: &str) -> Option<AntigravityAdapter> {
    let root = crate::antigravity::store_root()?;
    if !crate::antigravity::valid_id(session_id) {
        return None;
    }
    Some(AntigravityAdapter {
        id: session_id.to_string(),
        path: crate::antigravity::transcript_path(&root, session_id),
        offset: 0,
        pending: Vec::new(),
        ident: None,
        awaiting: BTreeMap::new(),
        orphans: BTreeMap::new(),
        turn: None,
        asking: false,
        root,
    })
}

impl Adapter for AntigravityAdapter {
    /// The whole history, as `poll` would have produced it step by step.
    fn bootstrap(&mut self) -> Vec<(u64, Kind)> {
        self.rewind();
        self.poll()
    }

    fn poll(&mut self) -> Vec<(u64, Kind)> {
        let mut out = Vec::new();
        let Ok(meta) = std::fs::metadata(&self.path) else {
            // Not written yet, or gone. The watch on the directory brings us
            // back; so does the driver's fallback tick.
            return out;
        };
        let ident = identity(&meta);
        // agy has no `/clear` (a fresh conversation is a fresh directory), so
        // a replacement here means the store was edited or restored under us.
        // Length alone would miss one that is already longer than what we had.
        let replaced = meta.len() < self.offset
            || (self.ident.is_some() && ident.is_some() && ident != self.ident);
        if replaced {
            out.push((now_ms(), Kind::Reset));
            self.rewind();
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

        // Only whole lines are parsed: a reader woken by the same inotify
        // event that carried half a record sees half a record.
        let buffered = std::mem::take(&mut self.pending);
        let mut start = 0;
        for (i, byte) in buffered.iter().enumerate() {
            if *byte != b'\n' {
                continue;
            }
            self.read_step(&buffered[start..i], &mut out);
            start = i + 1;
        }
        self.pending = buffered[start..].to_vec();
        out
    }

    /// The transcript, plus the deepest directory on its path that exists —
    /// the registry watches the PARENT of everything it is given, so naming
    /// `…/logs` arms `.system_generated` and catches the log directory being
    /// created. For a conversation whose `brain/<id>` is not there yet that
    /// walks up to `brain` itself. `watch_paths` is read once, so a watch
    /// armed high up is never narrowed; the driver's 2 s tick is what
    /// actually carries a session through its first steps.
    fn watch_paths(&self) -> Vec<PathBuf> {
        let mut paths = vec![self.path.clone()];
        let mut dir = self.path.parent();
        while let Some(d) = dir {
            // The first directory that exists, or the first whose own parent
            // does: either way something can be armed on it.
            if d.is_dir() || d.parent().is_some_and(std::path::Path::is_dir) {
                paths.push(d.to_path_buf());
                break;
            }
            dir = d.parent();
        }
        paths
    }
}

impl AntigravityAdapter {
    fn rewind(&mut self) {
        self.offset = 0;
        self.pending.clear();
        self.ident = None;
        self.awaiting.clear();
        self.orphans.clear();
        self.turn = None;
        self.asking = false;
    }

    /// `<conversation>:<step_index>` — the name of one step.
    fn step_id(&self, index: u64) -> String {
        format!("{}:{index}", self.id)
    }

    fn read_step(&mut self, raw: &[u8], out: &mut Vec<(u64, Kind)>) {
        let Ok(v) = serde_json::from_slice::<Value>(raw) else { return };
        let Some(index) = v.get("step_index").and_then(Value::as_u64) else { return };
        let ts = v
            .get("created_at")
            .and_then(|c| c.as_str())
            .and_then(iso_to_ms)
            // `created_at` on a step that opens a segment is the segment's
            // start, not its write time — two steps of one turn share it.
            // Good enough to stamp with, never to order by.
            .unwrap_or_else(now_ms);

        match v.get("type").and_then(|t| t.as_str()) {
            Some("USER_INPUT") => self.user_input(&v, index, ts, out),
            Some("PLANNER_RESPONSE") => self.planner(&v, index, ts, out),
            Some("GENERIC") => self.tool_output(&v, index, ts, out),
            // `SYSTEM_MESSAGE` is the resume notice, addressed to the model
            // about its own subagents and wrapped in a `<SYSTEM_MESSAGE>`
            // tag. Both conversations that were resumed carry an identical
            // one; nothing in it is for a person to read.
            _ => {}
        }
    }

    fn user_input(&mut self, v: &Value, index: u64, ts: u64, out: &mut Vec<(u64, Kind)>) {
        let content = v.get("content").and_then(|c| c.as_str()).unwrap_or_default();
        let text = crate::antigravity::user_request(content);
        let text = text.trim();
        // A `USER_INPUT` carrying only `<USER_SETTINGS_CHANGE>` and the local
        // time is agy noting a model switch, not a person speaking; it opens
        // no turn. Same rule `parse_messages` uses for the desktop's view.
        if text.is_empty() || crate::sessions::is_only_system_block(text) {
            return;
        }
        self.settle(index, ts, out);
        // A turn still open when the next prompt arrives never got its final
        // answer — interrupted, or a run killed mid-flight. Close it honestly
        // rather than leaving the phone a turn that never ends.
        if let Some(turn) = self.turn.take() {
            out.push((ts, Kind::TurnEnded { turn, reason: "unknown".into() }));
        }
        let turn = index.to_string();
        self.turn = Some(turn.clone());
        out.push((ts, Kind::TurnStarted { turn }));
        out.push((ts, Kind::UserMessage { id: self.step_id(index), text: text.to_string() }));
    }

    fn planner(&mut self, v: &Value, index: u64, ts: u64, out: &mut Vec<(u64, Kind)>) {
        self.settle(index, ts, out);
        let step = self.step_id(index);

        // agy appends the record once the step is finished, so every block is
        // whole the moment it is read: `done` is never false here.
        let thinking = v.get("thinking").and_then(|t| t.as_str()).unwrap_or_default().trim();
        if !thinking.is_empty() {
            out.push((
                ts,
                Kind::AgentThought {
                    id: format!("{step}:thinking"),
                    text: thinking.to_string(),
                    done: true,
                },
            ));
        }
        // A long answer is logged with its middle cut out; the db has it whole.
        let logged = v.get("content").and_then(|c| c.as_str()).unwrap_or_default().trim();
        let content = crate::antigravity::recover_truncated(&self.root, &self.id, index, logged);
        let content = content.trim();
        if !content.is_empty() {
            out.push((
                ts,
                Kind::AgentText { id: step.clone(), text: content.to_string(), done: true },
            ));
        }

        let calls = v.get("tool_calls").and_then(|c| c.as_array()).map(Vec::as_slice).unwrap_or(&[]);
        for (i, call) in calls.iter().enumerate() {
            let name = call.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
            let id = format!("{step}:{i}");
            out.push((
                ts,
                Kind::ToolCall {
                    id: id.clone(),
                    tool: name.to_string(),
                    title: clip(&title(call, name), TITLE_CAP),
                    category: category(name),
                    input: clip(&input_summary(call), INPUT_CAP),
                    // Running, not Pending: nothing sits between the record
                    // and the tool. On this account `toolPermission` is
                    // `always-proceed`, and even a prompt would show up as an
                    // `ask_*` call of its own rather than as a delay here.
                    status: ToolStatus::Running,
                },
            ));
            let ask = is_ask(name);
            // The i-th call is answered by the step i places after this one.
            self.awaiting.insert(index + 1 + i as u64, Awaited { call: id, ask });
            if ask && !self.asking {
                self.asking = true;
                out.push((ts, Kind::Phase { phase: Phase::NeedsYou, detail: "question".into() }));
            }
        }
        // An output that arrived before the call that owns it now has one.
        self.claim_orphans(out);

        // The end of a turn, as well as agy ever says it: a response that is
        // prose and nothing else. See the module doc for what that misses.
        if calls.is_empty() && !content.is_empty() {
            let turn = self.turn.take().unwrap_or_else(|| index.to_string());
            out.push((ts, Kind::TurnEnded { turn, reason: "completed".into() }));
        }
    }

    fn tool_output(&mut self, v: &Value, index: u64, ts: u64, out: &mut Vec<(u64, Kind)>) {
        let body = v.get("content").and_then(|c| c.as_str()).unwrap_or_default();
        match self.awaiting.remove(&index) {
            Some(waiting) => self.finish(waiting, ts, body, out),
            // The call is further down the file than its own output — the
            // reordering the module doc describes. Hold it.
            None => {
                self.orphans.insert(index, (ts, body.to_string()));
                while self.orphans.len() > ORPHANS_MAX {
                    let Some(oldest) = self.orphans.keys().next().copied() else { break };
                    self.orphans.remove(&oldest);
                }
            }
        }
    }

    /// Close a call with the output that answers it.
    fn finish(&mut self, waiting: Awaited, ts: u64, body: &str, out: &mut Vec<(u64, Kind)>) {
        let (status, output) = result_of(body);
        out.push((ts, Kind::ToolCallUpdate { id: waiting.call, status, output }));
        if waiting.ask {
            self.answered(ts, out);
        }
    }

    /// Hand every held output to the call that has since claimed its index.
    fn claim_orphans(&mut self, out: &mut Vec<(u64, Kind)>) {
        let ready: Vec<u64> =
            self.orphans.keys().copied().filter(|i| self.awaiting.contains_key(i)).collect();
        for index in ready {
            let Some((ts, body)) = self.orphans.remove(&index) else { continue };
            let Some(waiting) = self.awaiting.remove(&index) else { continue };
            self.finish(waiting, ts, &body, out);
        }
    }

    /// A `USER_INPUT` or `PLANNER_RESPONSE` at `index` means the model has
    /// moved past every output index below it, and the tools still waiting on
    /// one never got theirs — they failed, silently, which is the only way
    /// agy records a tool failure. `output` is left absent: the contract
    /// reads that as "unchanged", and there is nothing to show.
    ///
    /// Deliberately not called for a `GENERIC` step: two outputs of one
    /// parallel step could in principle land out of order, and the later of
    /// them must not condemn the earlier.
    fn settle(&mut self, index: u64, ts: u64, out: &mut Vec<(u64, Kind)>) {
        let stale: Vec<u64> = self.awaiting.range(..index).map(|(i, _)| *i).collect();
        for i in stale {
            let Some(waiting) = self.awaiting.remove(&i) else { continue };
            out.push((
                ts,
                Kind::ToolCallUpdate {
                    id: waiting.call,
                    status: ToolStatus::Failed,
                    output: None,
                },
            ));
            if waiting.ask {
                self.answered(ts, out);
            }
        }
    }

    /// Nobody is being waited on any more.
    fn answered(&mut self, ts: u64, out: &mut Vec<(u64, Kind)>) {
        if self.asking {
            self.asking = false;
            out.push((ts, Kind::Phase { phase: Phase::Working, detail: String::new() }));
        }
    }
}

/// agy's tools for putting a question to the person. `remote_api`'s
/// needs-you verdict names three of them; the binary also carries
/// `ask_for_user_feedback`, so the prefix is what is matched.
fn is_ask(name: &str) -> bool {
    name.starts_with("ask_")
}

/// What a tool call is, so a card wears the right mark. Only the names agy's
/// own binary carries are claimed; `browser_*`, MCP tools and anything a
/// plugin adds stay `Other` rather than being guessed at.
/// [observed in real steps: run_command, view_file, list_dir, grep_search,
/// write_to_file, ask_question — the rest are from agy's tool list]
fn category(name: &str) -> ToolCategory {
    match name {
        "run_command" => ToolCategory::Execute,
        "read_file" | "view_file" | "view_code_item" | "view_file_outline" | "list_dir"
        | "read_terminal" | "command_status" => ToolCategory::Read,
        "write_to_file" | "replace_file_content" | "multi_replace_file_content" | "sed_file" => {
            ToolCategory::Edit
        }
        "grep_search" | "find_by_name" | "codebase_search" => ToolCategory::Search,
        "search_web" | "read_url_content" | "open_browser_url" => ToolCategory::Fetch,
        n if is_ask(n) => ToolCategory::Think,
        _ => ToolCategory::Other,
    }
}

/// The card's heading: the argument a person would read to know what the
/// call is — the command, the path, the query. agy writes its own
/// `toolSummary` on every call ("Run echo hello-agy"), which is the fallback
/// for a tool whose arguments we do not know, and `tool_summary` falls back
/// again to the tool's name.
fn title(call: &Value, name: &str) -> String {
    let arg = |key: &str| {
        crate::antigravity::arg_str(call, key).filter(|s| !s.trim().is_empty())
    };
    let picked = match name {
        // Multi-line commands are common; the first line is the verb.
        "run_command" => arg("CommandLine").map(|c| first_line(&c)),
        "read_file" | "view_file" | "view_code_item" | "view_file_outline" => {
            arg("AbsolutePath").or_else(|| arg("TargetFile")).or_else(|| arg("File"))
        }
        "write_to_file" | "replace_file_content" | "multi_replace_file_content" | "sed_file" => {
            arg("TargetFile")
        }
        "list_dir" => arg("DirectoryPath"),
        "grep_search" | "codebase_search" | "search_web" => arg("Query"),
        "find_by_name" => arg("Pattern").or_else(|| arg("Query")),
        "read_url_content" | "open_browser_url" => arg("Url"),
        _ => None,
    };
    picked.unwrap_or_else(|| crate::antigravity::tool_summary(call))
}

/// Every argument that says something, as one line. The two label keys are
/// agy's own prose about the call (already the title's fallback) and
/// `ArtifactMetadata` is a JSON blob for agy's artifact list, so all three
/// are dropped; each value is clipped on its own so a file's whole body
/// cannot crowd out the path it is being written to.
fn input_summary(call: &Value) -> String {
    let Some(args) = call.get("args").and_then(|a| a.as_object()) else { return String::new() };
    let mut parts: Vec<String> = Vec::new();
    for key in args.keys() {
        if matches!(key.as_str(), "toolAction" | "toolSummary" | "ArtifactMetadata") {
            continue;
        }
        let Some(value) = crate::antigravity::arg_str(call, key) else { continue };
        let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if value.is_empty() {
            continue;
        }
        parts.push(format!("{key}={}", clip(&value, ARG_CAP)));
    }
    parts.join(" ")
}

/// A `GENERIC` step read as a result: whether the call worked, and what it
/// printed.
///
/// Every output opens with the two timestamp lines agy stamps on it; they
/// duplicate the step's own `created_at` and are dropped. What is left is the
/// tool's own words, whose shape differs per tool — an exit code and output
/// for `run_command`, a numbered listing for `view_file`, JSON lines for
/// `list_dir` and `grep_search`, "Created file …" for `write_to_file`,
/// "A1: <answer>" for `ask_question`.
///
/// `Failed` only where a failure is actually recorded: a non-zero exit code.
/// No text is sniffed for the word "error" — a `grep_search` for one would
/// then fail its own card, and the tools that really do fail write no step at
/// all (see [`AntigravityAdapter::settle`]).
fn result_of(body: &str) -> (ToolStatus, Option<String>) {
    let body = strip_stamps(body);
    let status = match exit_code(body) {
        Some(0) | None => ToolStatus::Completed,
        Some(_) => ToolStatus::Failed,
    };
    let output = (!body.trim().is_empty()).then(|| clip(body, OUTPUT_CAP));
    (status, output)
}

/// Drop the `Created At:` / `Completed At:` header lines every output opens
/// with, and the blank line `run_command` puts after them.
fn strip_stamps(body: &str) -> &str {
    let mut rest = body;
    for stamp in ["Created At:", "Completed At:"] {
        if !rest.starts_with(stamp) {
            break;
        }
        rest = match rest.find('\n') {
            Some(i) => &rest[i + 1..],
            None => "",
        };
    }
    rest.trim_start_matches('\n')
}

/// The exit status out of `run_command`'s "The command exited with code 2."
/// `None` for every other tool, none of which reports one.
fn exit_code(body: &str) -> Option<i64> {
    const MARK: &str = "The command exited with code ";
    let rest = &body[body.find(MARK)? + MARK.len()..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '-')
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or(s).to_string()
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

/// "2026-09-03T01:16:24Z" → millis. agy writes whole seconds in UTC; the
/// fractional branch is there because nothing stops it from not doing so.
/// (`spine::claude` keeps a sibling of this for Claude's timestamps — one
/// line of `use` would not be worth reaching across two adapters for.)
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
    let days = era * 146_097 + doe - 719_468;
    u64::try_from((days * 86_400 + h * 3_600 + mi * 60 + sec) * 1_000 + millis).ok()
}

/// Every conversation with a transcript on this machine, for the live test.
#[cfg(test)]
fn real_conversations() -> Vec<String> {
    let Some(root) = crate::antigravity::store_root() else { return Vec::new() };
    let Ok(dir) = std::fs::read_dir(root.join("brain")) else { return Vec::new() };
    let mut ids: Vec<String> = dir
        .flatten()
        .filter_map(|e| {
            let id = e.file_name().to_string_lossy().into_owned();
            crate::antigravity::transcript_path(&root, &id).is_file().then_some(id)
        })
        .collect();
    ids.sort();
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::Path;

    // Every record below is verbatim from a real transcript under
    // ~/.gemini/antigravity-cli/brain, agy 1.1.24, 2026-09-02. Long
    // `thinking` and one `content` are cut; nothing is reshaped.

    /// The person, wrapped in the envelope agy adds. [5e765eef, step 0]
    const INPUT_0: &str = r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-09-03T01:16:21Z","content":"<USER_REQUEST>\nDo all of this without asking me anything. First, read a.txt and b.txt at the same time (issue both read tool calls in one step, in parallel). Then run the shell command 'echo hello-agy'. Then run the shell command 'ls /definitely-not-a-real-dir' which will fail. Then write a file summary.txt containing the two words you read. Then tell me you are done.\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nThe current local time is: 2026-09-02T21:16:21-04:00.\n</ADDITIONAL_METADATA>\n<USER_SETTINGS_CHANGE>\nThe user changed setting `Model Selection` from None to Gemini 3.8 Flash (High). No need to comment on this change if the user doesn't ask about it. If reporting what model you are, please use a human readable name instead of the exact string.\n</USER_SETTINGS_CHANGE>"}"#;

    /// Two view_file calls in one step, with the thinking that chose them (cut). [step 1]
    const PARALLEL_CALLS: &str = r#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-09-03T01:16:21Z","thinking":"**Executing User's Instructions**\n\nInitiating parallel reads of `a.txt` and `b.txt` to gat","tool_calls":[{"name":"view_file","args":{"AbsolutePath":"\"/tmp/claude-1000/-home-admin-AI-OS/44ced9fd-a7fa-4761-8225-1bd1f24f6d51/scratchpad/agyprobe/a.txt\"","toolAction":"\"Viewing file\"","toolSummary":"\"Read a.txt\""}},{"name":"view_file","args":{"AbsolutePath":"\"/tmp/claude-1000/-home-admin-AI-OS/44ced9fd-a7fa-4761-8225-1bd1f24f6d51/scratchpad/agyprobe/b.txt\"","toolAction":"\"Viewing file\"","toolSummary":"\"Read b.txt\""}}]}"#;

    /// The first call's output — written BEFORE the step that issued it. [step 2]
    const OUT_A: &str = r#"{"step_index":2,"source":"MODEL","type":"GENERIC","status":"DONE","created_at":"2026-09-03T01:16:24Z","content":"Created At: 2026-09-02T21:16:24-04:00\nCompleted At: 2026-09-02T21:16:24-04:00\nFile Path: `file:///tmp/claude-1000/-home-admin-AI-OS/44ced9fd-a7fa-4761-8225-1bd1f24f6d51/scratchpad/agyprobe/a.txt`\nTotal Lines: 2\nTotal Bytes: 10\nShowing lines 1 to 2\nThe following code has been modified to include a line number before every line, in the format: <line_number>: <original_line>. Please note that any changes targeting the original code should remove the line number, colon, and leading space.\n1: alpha one\n2: \nThe above content shows the entire, complete file contents of the requested file.\n"}"#;

    /// The second call's output. [step 3]
    const OUT_B: &str = r#"{"step_index":3,"source":"MODEL","type":"GENERIC","status":"DONE","created_at":"2026-09-03T01:16:24Z","content":"Created At: 2026-09-02T21:16:24-04:00\nCompleted At: 2026-09-02T21:16:24-04:00\nFile Path: `file:///tmp/claude-1000/-home-admin-AI-OS/44ced9fd-a7fa-4761-8225-1bd1f24f6d51/scratchpad/agyprobe/b.txt`\nTotal Lines: 2\nTotal Bytes: 9\nShowing lines 1 to 2\nThe following code has been modified to include a line number before every line, in the format: <line_number>: <original_line>. Please note that any changes targeting the original code should remove the line number, colon, and leading space.\n1: beta two\n2: \nThe above content shows the entire, complete file contents of the requested file.\n"}"#;

    /// A run_command. [step 4]
    const RUN_OK_CALL: &str = r#"{"step_index":4,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-09-03T01:16:24Z","thinking":"**Interpreting \"two words\" literally**\n\nThe instruction spec","tool_calls":[{"name":"run_command","args":{"CommandLine":"\"echo hello-agy\"","Cwd":"\"/tmp/claude-1000/-home-admin-AI-OS/44ced9fd-a7fa-4761-8225-1bd1f24f6d51/scratchpad/agyprobe\"","WaitMsBeforeAsync":"5000","toolAction":"\"Running command\"","toolSummary":"\"Run echo hello-agy\""}}]}"#;

    /// Its output, exit 0. [step 5]
    const RUN_OK_OUT: &str = r#"{"step_index":5,"source":"MODEL","type":"GENERIC","status":"DONE","created_at":"2026-09-03T01:16:27Z","content":"Created At: 2026-09-02T21:16:27-04:00\nCompleted At: 2026-09-02T21:16:27-04:00\n\nThe command exited with code 0.\nOutput:\nhello-agy\r\n\n"}"#;

    /// A run_command that will fail. [step 6]
    const RUN_FAIL_CALL: &str = r#"{"step_index":6,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-09-03T01:16:27Z","tool_calls":[{"name":"run_command","args":{"CommandLine":"\"ls /definitely-not-a-real-dir\"","Cwd":"\"/tmp/claude-1000/-home-admin-AI-OS/44ced9fd-a7fa-4761-8225-1bd1f24f6d51/scratchpad/agyprobe\"","WaitMsBeforeAsync":"5000","toolAction":"\"Running command\"","toolSummary":"\"Run failing ls command\""}}]}"#;

    /// Its output, exit 2 — a failing command still writes a step. [step 7]
    const RUN_FAIL_OUT: &str = r#"{"step_index":7,"source":"MODEL","type":"GENERIC","status":"DONE","created_at":"2026-09-03T01:16:28Z","content":"Created At: 2026-09-02T21:16:28-04:00\nCompleted At: 2026-09-02T21:16:28-04:00\n\nThe command exited with code 2.\nOutput:\nls: cannot access '/definitely-not-a-real-dir': No such file or directory\r\n\n"}"#;

    /// A write_to_file. [step 22]
    const WRITE_CALL: &str = r#"{"step_index":22,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-09-03T01:16:45Z","thinking":"**Interpreting Agent's Testing Intent**\n\nThe `run.sh` execution appears to be a t","tool_calls":[{"name":"write_to_file","args":{"CodeContent":"\"alpha one\\nbeta two\\n\"","Description":"\"Create summary.txt with the words read from a.txt and b.txt\"","Overwrite":"true","TargetFile":"\"/tmp/claude-1000/-home-admin-AI-OS/44ced9fd-a7fa-4761-8225-1bd1f24f6d51/scratchpad/agyprobe/summary.txt\"","toolAction":"\"Editing file\"","toolSummary":"\"Write summary.txt\""}}]}"#;

    /// Its output. [step 23]
    const WRITE_OUT: &str = r#"{"step_index":23,"source":"MODEL","type":"GENERIC","status":"DONE","created_at":"2026-09-03T01:16:51Z","content":"Created At: 2026-09-02T21:16:51-04:00\nCompleted At: 2026-09-02T21:16:51-04:00\nCreated file file:///tmp/claude-1000/-home-admin-AI-OS/44ced9fd-a7fa-4761-8225-1bd1f24f6d51/scratchpad/agyprobe/summary.txt with requested content.\nIf relevant, proactively run terminal commands to execute this code for the USER. Don't ask for permission."}"#;

    /// The last answer of the turn: content, no tool_calls. [step 24]
    const FINAL: &str = r#"{"step_index":24,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-09-03T01:16:51Z","content":"I am done. All requested steps have been completed:\n\n1. Read [a.txt](file:///tmp/claude-1000/-home-admin-AI-OS/44ced9fd-a7fa-4761-8225-1bd1f24f6d51/scratchpad/agyprobe/a.txt) and [b.txt](file:///tmp/claude-1000/-home-admin-AI-OS/44ced9fd-a7fa-4761-8225-1bd1f24f6d51/scratchpad/agyprobe/b.txt) in parallel.\n2. Executed `echo hello-agy`.\n3. Executed `ls /definitely-not-a-real-dir` (exited with error code 2 as expected).\n4. Created [summary.txt](file:///tmp/claude-1000/-home-admin-AI-OS/44ced9fd-a7fa-4761-8225-1bd1f24f6d51/scratchpad/agyprobe/summary.txt) containing the words read from both files.","thinking":"**Confirming Execution Steps**\n\nThe task requires simultaneous reading of two fi"}"#;

    /// The prompt that made agy ask. [888a8fb2, step 0]
    const ASK_INPUT: &str = r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-09-03T01:20:19Z","content":"<USER_REQUEST>\nUse your ask_question tool right now to ask me which of two colors I prefer, red or blue. Do not guess, do not answer for me, do not use any other tool. Just issue the ask_question tool call and stop.\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nThe current local time is: 2026-09-02T21:20:19-04:00.\n</ADDITIONAL_METADATA>\n<USER_SETTINGS_CHANGE>\nThe user changed setting `Model Selection` from None to Gemini 3.8 Flash (High). No need to comment on this change if the user doesn't ask about it. If reporting what model you are, please use a human readable name instead of the exact string.\n</USER_SETTINGS_CHANGE>"}"#;

    /// ask_question — the tool agy puts a question to the person with. [step 1]
    const ASK_CALL: &str = r#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-09-03T01:20:19Z","tool_calls":[{"name":"ask_question","args":{"questions":"[{\"is_multi_select\":false,\"options\":[\"Red\",\"Blue\"],\"question\":\"Which color do you prefer?\"}]","toolAction":"\"Asking color preference\"","toolSummary":"\"Color preference question\""}}]}"#;

    /// The answer, as the call's own output step (print mode skips it). [step 2]
    const ASK_OUT: &str = r#"{"step_index":2,"source":"MODEL","type":"GENERIC","status":"DONE","created_at":"2026-09-03T01:20:21Z","content":"Created At: 2026-09-02T21:20:21-04:00\nCompleted At: 2026-09-02T21:20:21-04:00\nA1: User Skipped"}"#;

    /// A list_dir on a directory that is not there. Its output step (2) is NEVER written. [338b6eba, step 1]
    const LOST_CALL: &str = r#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-09-03T01:22:26Z","thinking":"**Initiating Ordered Operations**\n\nAcknowledging the directive to execute four operations sequentially, one per turn, without error handling. The first action is to list the contents of a non-existent directory. Anticipating a potential failure, but proceeding as instructed.\n\n\n","tool_calls":[{"name":"list_dir","args":{"DirectoryPath":"\"/no/such/dir/at/all\"","toolAction":"\"Listing nonexistent directory\"","toolSummary":"\"Nonexistent directory listing\""}}]}"#;

    /// The step after it, at index 3 — the model moved on. [step 3]
    const NEXT_CALL: &str = r#"{"step_index":3,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-09-03T01:22:27Z","tool_calls":[{"name":"grep_search","args":{"MatchPerLine":"true","Query":"\"zzzqqqnotfound\"","SearchPath":"\"/tmp/claude-1000\"","toolAction":"\"Searching for string\"","toolSummary":"\"Pattern search\""}}]}"#;

    /// The notice every resume injects. [e6aebcf2, step 3, cut]
    const SYSTEM: &str = r#"{"step_index":3,"source":"SYSTEM","type":"SYSTEM_MESSAGE","status":"DONE","created_at":"2026-09-02T22:24:29Z","content":"The following is a <SYSTEM_MESSAGE> not actually sent by the user. It is provided by the system as important information to pay attention to.\n\n<SYSTEM_MESSAGE>\n[Message] timestamp=2026-09-02T22:24:29Z"}"#;

    /// A second prompt, later in the same conversation. [step 25]
    const INPUT_25: &str = r#"{"step_index":25,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-09-03T01:18:34Z","content":"<USER_REQUEST>\nWithout asking anything: 1) read the file /tmp/no-such-file-here.txt (it does not exist). 2) run the shell command 'sleep 4; echo slept'. 3) run 'sleep 4; echo again'. 4) run 'sleep 4; echo third'. 5) say done.\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nThe current local time is: 2026-09-02T21:18:34-04:00.\n</ADDITIONAL_METADATA>"}"#;

    /// A settings change with no request behind it — agy noting a model
    /// switch. Built from `INPUT_0`'s tail; no conversation here carries one
    /// on its own, but `parse_messages` guards for it and so does this.
    const SETTINGS_ONLY: &str = r#"{"step_index":9,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-09-03T01:16:21Z","content":"<ADDITIONAL_METADATA>\nThe current local time is: 2026-09-02T21:16:21-04:00.\n</ADDITIONAL_METADATA>\n<USER_SETTINGS_CHANGE>\nThe user changed setting `Model Selection` from None to Gemini 3.8 Flash (High).\n</USER_SETTINGS_CHANGE>"}"#;

    const CONV: &str = "5e765eef-3ed9-4c77-8b22-1fd873bab193";

    /// An adapter over a scratch file, so a test can write the transcript the
    /// way agy does — a step at a time, and sometimes half of one.
    fn adapter_over(path: &Path) -> AntigravityAdapter {
        AntigravityAdapter {
            id: CONV.to_string(),
            path: path.to_path_buf(),
            offset: 0,
            pending: Vec::new(),
            ident: None,
            awaiting: BTreeMap::new(),
            orphans: BTreeMap::new(),
            turn: None,
            asking: false,
            root: PathBuf::from("/nonexistent"),
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aiterm-spine-agy-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("transcript.jsonl")
    }

    fn write(path: &Path, bytes: &str) {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
        f.write_all(bytes.as_bytes()).unwrap();
    }

    /// Feed steps straight through the parser, no file involved.
    fn kinds(steps: &[&str]) -> Vec<Kind> {
        let mut a = adapter_over(Path::new("/nonexistent"));
        let mut out = Vec::new();
        for s in steps {
            a.read_step(s.as_bytes(), &mut out);
        }
        out.into_iter().map(|(_, k)| k).collect()
    }

    #[test]
    fn a_prompt_opens_a_turn_and_shows_only_what_the_person_typed() {
        assert_eq!(
            kinds(&[INPUT_0]),
            vec![
                Kind::TurnStarted { turn: "0".into() },
                Kind::UserMessage {
                    id: format!("{CONV}:0"),
                    text: "Do all of this without asking me anything. First, read a.txt and b.txt at the same time (issue both read tool calls in one step, in parallel). Then run the shell command 'echo hello-agy'. Then run the shell command 'ls /definitely-not-a-real-dir' which will fail. Then write a file summary.txt containing the two words you read. Then tell me you are done.".into(),
                },
            ],
            "the metadata and settings tails of the envelope never reach the phone",
        );
    }

    #[test]
    fn a_step_is_stamped_with_its_own_created_at() {
        let mut a = adapter_over(Path::new("/nonexistent"));
        let mut out = Vec::new();
        a.read_step(INPUT_0.as_bytes(), &mut out);
        // 2026-09-03T01:16:21Z
        assert_eq!(out[0].0, 1_788_398_181_000);
    }

    #[test]
    fn a_settings_change_with_no_request_is_nobody_speaking() {
        assert!(kinds(&[SETTINGS_ONLY]).is_empty());
    }

    #[test]
    fn the_resume_notice_says_nothing() {
        assert!(kinds(&[SYSTEM, "", "not json at all", r#"{"type":"USER_INPUT"}"#]).is_empty());
    }

    #[test]
    fn thinking_and_prose_are_whole_blocks_under_two_ids() {
        let out = kinds(&[FINAL]);
        assert_eq!(out.len(), 3, "thought, text, and the turn they end");
        assert!(matches!(
            &out[0],
            Kind::AgentThought { id, done: true, .. } if id == &format!("{CONV}:24:thinking")
        ));
        assert!(matches!(
            &out[1],
            Kind::AgentText { id, text, done: true }
                if id == &format!("{CONV}:24") && text.starts_with("I am done.")
        ));
    }

    #[test]
    fn a_command_runs_the_moment_it_is_written_and_its_output_closes_it() {
        assert_eq!(
            kinds(&[RUN_OK_CALL, RUN_OK_OUT]),
            vec![
                Kind::AgentThought {
                    id: format!("{CONV}:4:thinking"),
                    text: "**Interpreting \"two words\" literally**\n\nThe instruction spec".into(),
                    done: true,
                },
                Kind::ToolCall {
                    id: format!("{CONV}:4:0"),
                    tool: "run_command".into(),
                    title: "echo hello-agy".into(),
                    category: ToolCategory::Execute,
                    input: "CommandLine=echo hello-agy Cwd=/tmp/claude-1000/-home-admin-AI-OS/44ced9fd-a7fa-4761-8225-1bd1f24f6d51/scratchpad/agyprobe WaitMsBeforeAsync=5000".into(),
                    status: ToolStatus::Running,
                },
                Kind::ToolCallUpdate {
                    id: format!("{CONV}:4:0"),
                    status: ToolStatus::Completed,
                    // The two stamp lines are gone; the tool's own words are not.
                    output: Some("The command exited with code 0.\nOutput:\nhello-agy\r\n\n".into()),
                },
            ]
        );
    }

    #[test]
    fn a_non_zero_exit_fails_the_card_it_belongs_to() {
        let out = kinds(&[RUN_FAIL_CALL, RUN_FAIL_OUT]);
        assert_eq!(
            out[1],
            Kind::ToolCallUpdate {
                id: format!("{CONV}:6:0"),
                status: ToolStatus::Failed,
                output: Some("The command exited with code 2.\nOutput:\nls: cannot access '/definitely-not-a-real-dir': No such file or directory\r\n\n".into()),
            }
        );
    }

    #[test]
    fn a_write_wears_its_path_and_is_an_edit() {
        let out = kinds(&[WRITE_CALL, WRITE_OUT]);
        assert_eq!(
            out[1],
            Kind::ToolCall {
                id: format!("{CONV}:22:0"),
                tool: "write_to_file".into(),
                title: "/tmp/claude-1000/-home-admin-AI-OS/44ced9fd-a7fa-4761-8225-1bd1f24f6d51/scratchpad/agyprobe/summary.txt".into(),
                category: ToolCategory::Edit,
                input: "CodeContent=alpha one beta two Description=Create summary.txt with the words read from a.txt and b.txt Overwrite=true TargetFile=/tmp/claude-1000/-home-admin-AI-OS/44ced9fd-a7fa-4761-8225-1bd1f24f6d51/scratchpad/agyprobe/summary.txt".into(),
                status: ToolStatus::Running,
            }
        );
        assert!(matches!(
            &out[2],
            Kind::ToolCallUpdate { status: ToolStatus::Completed, output: Some(o), .. }
                if o.starts_with("Created file file:///tmp/")
        ));
    }

    /// The real file's own order: the output of the first parallel call is
    /// written BEFORE the step that issued both of them.
    #[test]
    fn parallel_calls_pair_to_their_outputs_however_they_are_written() {
        let out = kinds(&[INPUT_0, OUT_A, PARALLEL_CALLS, OUT_B]);
        let ids: Vec<&str> = out
            .iter()
            .filter_map(|k| match k {
                Kind::ToolCall { id, .. } | Kind::ToolCallUpdate { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            ids,
            vec![
                format!("{CONV}:1:0"),
                format!("{CONV}:1:1"),
                // The held output finds its call the moment the call is read.
                format!("{CONV}:1:0"),
                format!("{CONV}:1:1"),
            ],
            "two calls, then the update for each, in call order",
        );
        let a = out.iter().find_map(|k| match k {
            Kind::ToolCall { id, title, .. } if id.ends_with(":1:0") => Some(title.clone()),
            _ => None,
        });
        assert!(a.unwrap().ends_with("/a.txt"), "the first call is the first path");
        assert!(matches!(
            out.last(),
            Some(Kind::ToolCallUpdate { status: ToolStatus::Completed, output: Some(o), .. })
                if o.contains("1: beta two")
        ));
    }

    /// A tool that fails writes no output step: index 2 never appears, and
    /// the step at 3 is the model moving on.
    #[test]
    fn a_call_whose_output_never_lands_is_a_failure() {
        let out = kinds(&[LOST_CALL, NEXT_CALL]);
        let updates: Vec<&Kind> =
            out.iter().filter(|k| matches!(k, Kind::ToolCallUpdate { .. })).collect();
        assert_eq!(
            updates,
            vec![&Kind::ToolCallUpdate {
                id: format!("{CONV}:1:0"),
                status: ToolStatus::Failed,
                // Absent, not empty: there is nothing to show and the
                // consumer keeps whatever it holds.
                output: None,
            }]
        );
        assert!(
            out.iter().any(|k| matches!(k, Kind::ToolCall { id, .. } if id.ends_with(":3:0"))),
            "and the step that condemned it still shows its own call",
        );
    }

    #[test]
    fn a_question_needs_you_until_it_is_answered() {
        let out = kinds(&[ASK_INPUT, ASK_CALL, ASK_OUT]);
        assert_eq!(
            out[2],
            Kind::ToolCall {
                id: format!("{CONV}:1:0"),
                tool: "ask_question".into(),
                // No path or query to show: agy's own summary is the heading.
                title: "Color preference question".into(),
                category: ToolCategory::Think,
                input: r#"questions=[{"is_multi_select":false,"options":["Red","Blue"],"question":"Which color do you prefer?"}]"#.into(),
                status: ToolStatus::Running,
            }
        );
        assert_eq!(out[3], Kind::Phase { phase: Phase::NeedsYou, detail: "question".into() });
        assert_eq!(
            out[4],
            Kind::ToolCallUpdate {
                id: format!("{CONV}:1:0"),
                status: ToolStatus::Completed,
                output: Some("A1: User Skipped".into()),
            }
        );
        assert_eq!(out[5], Kind::Phase { phase: Phase::Working, detail: String::new() });
    }

    #[test]
    fn the_last_answer_of_a_turn_ends_the_turn_that_opened_it() {
        let out = kinds(&[INPUT_0, RUN_OK_CALL, RUN_OK_OUT, FINAL]);
        assert_eq!(
            out.last(),
            Some(&Kind::TurnEnded { turn: "0".into(), reason: "completed".into() })
        );
        assert_eq!(
            out.iter().filter(|k| matches!(k, Kind::TurnEnded { .. })).count(),
            1,
            "a response carrying tool calls is not the end of anything",
        );
    }

    /// A turn that never got its final answer — the tool at step 1 failed,
    /// so nothing was written at step 2, and the next thing in the file is
    /// the person speaking again.
    #[test]
    fn a_prompt_arriving_mid_turn_closes_the_turn_it_interrupted() {
        let out = kinds(&[INPUT_0, LOST_CALL, INPUT_25]);
        assert_eq!(
            out[out.len() - 4],
            Kind::ToolCallUpdate {
                id: format!("{CONV}:1:0"),
                status: ToolStatus::Failed,
                output: None,
            },
            "the call it was waiting on will never report",
        );
        assert_eq!(
            out[out.len() - 3],
            Kind::TurnEnded { turn: "0".into(), reason: "unknown".into() },
            "and the turn it belonged to has no answer to end on",
        );
        assert_eq!(out[out.len() - 2], Kind::TurnStarted { turn: "25".into() });
    }

    #[test]
    fn half_a_step_waits_for_its_newline() {
        let path = scratch("partial");
        let (head, tail) = INPUT_0.split_at(120);
        write(&path, head);
        let mut a = adapter_over(&path);
        assert!(a.poll().is_empty(), "an unterminated line is not a line yet");
        write(&path, tail);
        assert!(a.poll().is_empty(), "still no newline");
        write(&path, "\n");
        assert_eq!(a.poll().len(), 2, "turn_started + user_message once the record closes");
        assert!(a.poll().is_empty(), "and nothing on a poll with nothing new");
    }

    #[test]
    fn a_split_inside_a_multi_byte_character_survives() {
        let path = scratch("utf8");
        // The em dash in FINAL's prose is three bytes; cut the file inside it.
        let step = format!("{FINAL}\n");
        let cut = step
            .char_indices()
            .find(|(_, c)| !c.is_ascii())
            .map(|(i, _)| i + 1)
            .unwrap_or(step.len() / 2);
        write(&path, &step[..cut]);
        let mut a = adapter_over(&path);
        assert!(a.poll().is_empty());
        write(&path, &step[cut..]);
        assert_eq!(a.poll().len(), 3, "thought, text, turn_ended");
    }

    #[test]
    fn a_replaced_transcript_resets_before_it_replays() {
        let path = scratch("reset");
        write(&path, &format!("{INPUT_0}\n{RUN_OK_CALL}\n{RUN_OK_OUT}\n"));
        let mut a = adapter_over(&path);
        assert_eq!(a.bootstrap().len(), 5);

        // Shorter under us: the store was restored or edited.
        std::fs::write(&path, format!("{INPUT_0}\n")).unwrap();
        let out = a.poll();
        assert_eq!(out.first().map(|(_, k)| k), Some(&Kind::Reset));
        assert_eq!(out.len(), 3, "reset, then the new file's history");
        assert!(
            a.awaiting.is_empty() && a.orphans.is_empty(),
            "and nothing from the old file is still pending",
        );
    }

    #[test]
    fn bootstrap_reads_the_whole_file_and_poll_reads_only_the_rest() {
        let path = scratch("bootstrap");
        write(&path, &format!("{INPUT_0}\n{OUT_A}\n{PARALLEL_CALLS}\n"));
        let mut a = adapter_over(&path);
        // turn_started, user_message, thought, two calls, the held output.
        assert_eq!(a.bootstrap().len(), 6);
        assert!(a.poll().is_empty());
        write(&path, &format!("{OUT_B}\n"));
        assert_eq!(a.poll().len(), 1);
    }

    #[test]
    fn a_conversation_with_no_transcript_yet_is_empty_and_fills_in_later() {
        let path = scratch("late");
        let mut a = adapter_over(&path);
        assert!(a.bootstrap().is_empty());
        assert!(a.poll().is_empty());
        assert_eq!(a.watch_paths().len(), 2, "the file and a directory that exists");
        write(&path, &format!("{INPUT_0}\n"));
        assert_eq!(a.poll().len(), 2);
    }

    #[test]
    fn an_empty_transcript_says_nothing() {
        let path = scratch("empty");
        write(&path, "");
        let mut a = adapter_over(&path);
        assert!(a.bootstrap().is_empty());
        write(&path, "\n\n");
        assert!(a.poll().is_empty());
    }

    #[test]
    fn a_conversation_whose_directories_do_not_exist_still_arms_a_watch() {
        let Some(root) = crate::antigravity::store_root() else { return };
        let a = open("00000000-0000-0000-0000-000000000000").unwrap();
        let paths = a.watch_paths();
        assert_eq!(paths[0], crate::antigravity::transcript_path(&root, &a.id));
        assert!(paths[1].exists() || paths[1].parent().is_some_and(|p| p.exists()));
    }

    #[test]
    fn an_id_that_could_name_a_path_is_refused() {
        assert!(open("../../etc").is_none());
        assert!(open("").is_none());
    }

    #[test]
    fn a_result_is_read_for_its_exit_code_and_nothing_else() {
        assert_eq!(exit_code("The command exited with code 0.\n"), Some(0));
        assert_eq!(exit_code("The command exited with code 130.\n"), Some(130));
        assert_eq!(exit_code("no code here"), None);
        // A search whose hit contains the word error is not a failed search.
        let (status, _) = result_of("Created At: x\nCompleted At: y\n{\"LineContent\":\"error\"}");
        assert_eq!(status, ToolStatus::Completed);
    }

    /// The real thing: every conversation in the store on this machine,
    /// bootstrapped through the adapter. Ignored because it depends on files
    /// that only exist where agy has run.
    ///
    /// `cargo test --lib spine::antigravity::tests::real_conversations -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_conversations_bootstrap_once_and_then_say_nothing() {
        let ids = super::real_conversations();
        assert!(!ids.is_empty(), "a transcript under ~/.gemini/antigravity-cli/brain");
        for id in ids {
            let mut a = open(&id).expect("the store opens a conversation by its id");
            let lines = std::fs::read_to_string(&a.path).unwrap().lines().count();
            let started = std::time::Instant::now();
            let events = a.bootstrap();
            let took = started.elapsed();
            let mut tally: std::collections::BTreeMap<&str, usize> = Default::default();
            for (_, k) in &events {
                *tally.entry(kind_name(k)).or_default() += 1;
            }
            println!(
                "{}: {lines} lines → {} events in {} ms {tally:?}",
                &id[..8],
                events.len(),
                took.as_millis()
            );
            assert!(!events.is_empty());
            assert!(a.poll().is_empty(), "a second poll on a file nobody wrote to is empty");
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
