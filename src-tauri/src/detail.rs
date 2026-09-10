//! Everything worth remembering about a session, in one read of its
//! transcript — for the flyout that opens when a sidebar row is hovered.
//!
//! The row says a title, a path and an age. That is enough to find a session
//! you had in mind, and nothing like enough to recognise one you have
//! forgotten: what was asked, what came of it, which model, how long it ran,
//! which files it touched, how much of the context window it had used when it
//! stopped. Those are the things that make "oh, *that* one" happen, so this
//! collects them.
//!
//! One pass over the file, no JSON kept: transcripts run to tens of megabytes
//! and the hover has to answer while the pointer is still there. Claude's
//! shape is read in full; Codex's for what it records (session meta, turn
//! context, token counts, function calls); aiterm's own API chats for their
//! meta records. An engine whose format is its own — grok's session
//! directory, OpenCode's database — answers through its provider's
//! `detail()`; one that offers only `messages()` gets the conversation and
//! nothing else, and counts plus the first and last exchange are still
//! worth having.

use crate::sessions::{
    is_system_meta_prompt, line_may_hold_message, line_message, strip_system_tags,
};
use serde::Serialize;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct ToolCount {
    pub name: String,
    pub count: u32,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct SessionDetail {
    pub id: String,
    /// ISO timestamps off the transcript: the first line's and the last's.
    pub started: Option<String>,
    pub last_active: Option<String>,
    pub cwd: Option<String>,
    pub branch: Option<String>,
    pub cli_version: Option<String>,
    /// In order of first use. More than one means the model was switched.
    pub models: Vec<String>,
    pub effort: Option<String>,
    pub permission_mode: Option<String>,
    pub user_messages: u32,
    pub assistant_messages: u32,
    pub tool_calls: u32,
    /// The most-used tools, most first, at most six.
    pub tools: Vec<ToolCount>,
    /// Input side of the last assistant turn — prompt, cache reads and cache
    /// writes together — which is what the context window held at the end.
    pub context_tokens: Option<u64>,
    pub context_window: Option<u64>,
    pub output_tokens: u64,
    /// The engine's own title for the conversation, where it wrote one.
    pub title: Option<String>,
    pub first_prompt: Option<String>,
    pub last_user: Option<String>,
    pub last_assistant: Option<String>,
    /// Files written or edited, most recent first, at most eight.
    pub files: Vec<String>,
    pub pr_links: Vec<String>,
    pub compactions: u32,
}

const FIRST_MAX: usize = 240;
const LAST_MAX: usize = 320;
const TOOLS_KEPT: usize = 6;
const FILES_KEPT: usize = 8;

fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut s: String = text.chars().take(max).collect();
    s.push('…');
    s
}

pub(crate) fn push_unique(v: &mut Vec<String>, s: &str) {
    if !s.is_empty() && !v.iter().any(|x| x == s) {
        v.push(s.to_string());
    }
}

/// Most recent first, no repeats, capped — a file edited ten times is one
/// line, and it is the one at the top.
pub(crate) fn touch_file(files: &mut Vec<String>, path: &str) {
    if path.is_empty() {
        return;
    }
    files.retain(|f| f != path);
    files.insert(0, path.to_string());
    files.truncate(FILES_KEPT);
}

#[tauri::command]
pub async fn session_detail(session_id: String) -> Option<SessionDetail> {
    crate::run_blocking(move || session_detail_sync(session_id)).await
}

pub(crate) fn session_detail_sync(session_id: String) -> Option<SessionDetail> {
    let list = crate::agents::backends();
    let (backend, path) = crate::agents::owner_in(&list, &session_id)?;
    if let Some(d) = backend.sessions().detail(&session_id) {
        return Some(d);
    }
    if let Some(msgs) = backend.sessions().messages(&session_id) {
        return Some(from_messages(session_id, msgs));
    }
    let file = File::open(&path).ok()?;
    let mut d = SessionDetail {
        id: session_id,
        ..Default::default()
    };
    let mut tools: HashMap<String, u32> = HashMap::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        // The cheap gate is for messages only; the bookkeeping lines
        // (permission-mode, ai-title, pr-link, token_count) are short and
        // parse for nothing.
        if !line_may_hold_message(&line) && !is_bookkeeping(&line) {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        read_line(&mut d, &mut tools, &v);
    }
    d.tools = top_tools(tools);
    Some(d)
}

/// Lines the message gate skips that still say something worth reading.
fn is_bookkeeping(line: &str) -> bool {
    [
        "\"permission-mode\"",
        "\"ai-title\"",
        "\"pr-link\"",
        "\"compact_boundary\"",
        "\"session_meta\"",
        "\"turn_context\"",
        "\"token_count\"",
        "\"task_started\"",
        "\"function_call\"",
        "\"custom_tool_call\"",
        "\"compacted\"",
        "\"aiterm-chat",
    ]
    .iter()
    .any(|k| line.contains(k))
}

pub(crate) fn top_tools(tools: HashMap<String, u32>) -> Vec<ToolCount> {
    let mut v: Vec<ToolCount> = tools
        .into_iter()
        .map(|(name, count)| ToolCount { name, count })
        .collect();
    v.sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(&b.name)));
    v.truncate(TOOLS_KEPT);
    v
}

fn str_at<'a>(v: &'a serde_json::Value, ptr: &str) -> Option<&'a str> {
    v.pointer(ptr).and_then(|x| x.as_str())
}

fn u64_at(v: &serde_json::Value, ptr: &str) -> Option<u64> {
    v.pointer(ptr).and_then(|x| x.as_u64())
}

/// Unix millis as the ISO string the transcripts write, so a record that
/// keeps a number (aiterm's chats, OpenCode's rows) reads the same as one
/// that keeps a string. Hand-rolled: nothing else in the crate needs a
/// calendar, and this is thirty lines against a dependency.
pub(crate) fn iso_from_ms(ms: u64) -> String {
    let secs = ms / 1000;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60,
        ms % 1000
    )
}

fn note_stamp(d: &mut SessionDetail, ts: &str) {
    if d.started.is_none() {
        d.started = Some(ts.to_string());
    }
    d.last_active = Some(ts.to_string());
}

/// Files named in an `apply_patch` body: `*** Update File: path` and its
/// Add/Delete siblings, each once.
fn patch_files(patch: &str) -> Vec<String> {
    patch
        .lines()
        .filter_map(|l| {
            ["*** Update File: ", "*** Add File: ", "*** Delete File: "]
                .iter()
                .find_map(|p| l.strip_prefix(p))
        })
        .map(|s| s.trim().to_string())
        .collect()
}

/// One transcript line into the running detail. Claude's and Codex's shapes
/// both land here; a line that is neither is a no-op.
pub(crate) fn read_line(
    d: &mut SessionDetail,
    tools: &mut HashMap<String, u32>,
    v: &serde_json::Value,
) {
    if let Some(ts) = str_at(v, "/timestamp") {
        note_stamp(d, ts);
    } else if let Some(ms) = u64_at(v, "/at") {
        // aiterm's own chats stamp lines with millis, not a string.
        note_stamp(d, &iso_from_ms(ms));
    }
    if let Some(c) = str_at(v, "/cwd") {
        d.cwd = Some(c.to_string());
    }
    if let Some(b) = str_at(v, "/gitBranch") {
        if !b.is_empty() {
            d.branch = Some(b.to_string());
        }
    }
    if let Some(ver) = str_at(v, "/version") {
        d.cli_version = Some(ver.to_string());
    }

    let kind = str_at(v, "/type").unwrap_or("");
    match kind {
        // ---- claude bookkeeping ----
        "permission-mode" => {
            if let Some(m) = str_at(v, "/permissionMode") {
                d.permission_mode = Some(m.to_string());
            }
        }
        "ai-title" => {
            if let Some(t) = str_at(v, "/aiTitle") {
                d.title = Some(t.to_string());
            }
        }
        "pr-link" => {
            if let Some(u) = str_at(v, "/prUrl") {
                push_unique(&mut d.pr_links, u);
            }
        }
        "system" => {
            if str_at(v, "/subtype").is_some_and(|s| s.contains("compact")) {
                d.compactions += 1;
            }
        }
        // ---- aiterm's API chats (chat.rs): meta first, then claude-shaped turns ----
        "aiterm-chat" => {
            if let Some(m) = str_at(v, "/model") {
                push_unique(&mut d.models, m);
            }
            if let Some(p) = str_at(v, "/provider") {
                d.permission_mode = Some(format!("via {p}"));
            }
        }
        "aiterm-chat-model" => {
            if let Some(m) = str_at(v, "/model") {
                push_unique(&mut d.models, m);
            }
        }
        "aiterm-chat-clear" => d.compactions += 1,
        // ---- codex bookkeeping ----
        // Codex compacts too: `{"type":"compacted","payload":{…}}` written
        // when context is compacted (manual or automatic).
        // [observed: codex-cli 0.150.1]
        "compacted" => d.compactions += 1,
        "session_meta" => {
            if let Some(c) = str_at(v, "/payload/cwd") {
                d.cwd = Some(c.to_string());
            }
            if let Some(ver) = str_at(v, "/payload/cli_version") {
                d.cli_version = Some(ver.to_string());
            }
        }
        "turn_context" => {
            if let Some(m) = str_at(v, "/payload/model") {
                push_unique(&mut d.models, m);
            }
            if let Some(p) = str_at(v, "/payload/approval_policy") {
                d.permission_mode = Some(p.to_string());
            }
            if let Some(e) = str_at(v, "/payload/effort") {
                d.effort = Some(e.to_string());
            }
        }
        "event_msg" => match str_at(v, "/payload/type") {
            Some("task_started") => {
                if let Some(w) = u64_at(v, "/payload/model_context_window") {
                    d.context_window = Some(w);
                }
            }
            Some("token_count") => {
                if let Some(o) = u64_at(v, "/payload/info/total_token_usage/output_tokens") {
                    d.output_tokens = o;
                }
                let last = "/payload/info/last_token_usage";
                if let Some(i) = u64_at(v, &format!("{last}/input_tokens")) {
                    d.context_tokens = Some(i);
                }
                if let Some(w) = u64_at(v, "/payload/info/model_context_window") {
                    d.context_window = Some(w);
                }
            }
            _ => {}
        },
        "response_item" => {
            // `function_call` is a declared tool; `custom_tool_call` is the
            // freeform kind — codex's `exec` and `apply_patch` arrive so.
            if matches!(
                str_at(v, "/payload/type"),
                Some("function_call" | "custom_tool_call")
            ) {
                d.tool_calls += 1;
                let name = str_at(v, "/payload/name").unwrap_or("tool");
                *tools.entry(name.to_string()).or_insert(0) += 1;
                if name == "apply_patch" {
                    let body = str_at(v, "/payload/input")
                        .map(String::from)
                        .or_else(|| {
                            str_at(v, "/payload/arguments")
                                .and_then(|a| serde_json::from_str::<serde_json::Value>(a).ok())
                                .and_then(|a| str_at(&a, "/input").map(String::from))
                        })
                        .unwrap_or_default();
                    for f in patch_files(&body) {
                        touch_file(&mut d.files, &f);
                    }
                }
            }
        }
        _ => {}
    }

    // ---- claude turns ----
    if kind == "assistant" && v.get("isSidechain").and_then(|b| b.as_bool()) != Some(true) {
        if let Some(m) = str_at(v, "/message/model") {
            push_unique(&mut d.models, m);
        }
        if let Some(e) = str_at(v, "/effort") {
            d.effort = Some(e.to_string());
        }
        if let Some(u) = v.pointer("/message/usage") {
            let input = u64_at(u, "/input_tokens").unwrap_or(0)
                + u64_at(u, "/cache_read_input_tokens").unwrap_or(0)
                + u64_at(u, "/cache_creation_input_tokens").unwrap_or(0);
            if input > 0 {
                d.context_tokens = Some(input);
            }
            d.output_tokens += u64_at(u, "/output_tokens").unwrap_or(0);
        }
        if let Some(blocks) = v.pointer("/message/content").and_then(|c| c.as_array()) {
            for b in blocks {
                if str_at(b, "/type") != Some("tool_use") {
                    continue;
                }
                d.tool_calls += 1;
                let name = str_at(b, "/name").unwrap_or("tool");
                *tools.entry(name.to_string()).or_insert(0) += 1;
                if matches!(name, "Edit" | "Write" | "MultiEdit" | "NotebookEdit") {
                    if let Some(p) =
                        str_at(b, "/input/file_path").or(str_at(b, "/input/notebook_path"))
                    {
                        touch_file(&mut d.files, p);
                    }
                }
            }
        }
    }

    // ---- the conversation, either shape ----
    if v.get("isSidechain").and_then(|b| b.as_bool()) == Some(true) {
        return;
    }
    if let Some((role, text)) = line_message(v) {
        if role == "user" && is_codex_agents_preamble(&text) {
            return;
        }
        let text = strip_system_tags(&text);
        if text.trim().is_empty() {
            return;
        }
        note_message(d, &role, &text);
    }
}

/// Codex sends the repo's AGENTS.md as its own first "user" message: an
/// untagged `# AGENTS.md instructions for <cwd>` header ahead of an
/// `<INSTRUCTIONS>…</INSTRUCTIONS>` block. The whole-block system filter
/// keeps it — stripping the tags leaves the header line — so it has to be
/// named here: harness preamble, never a phone bubble, a `first_prompt` or
/// a title. Both the header AND the block are required, so a genuine message
/// that merely mentions AGENTS.md is not swallowed. Checked against the RAW
/// text, before tags are stripped. Older rollouts (0.147.0–0.149.1) put a
/// `<recommended_plugins>` block ahead of the header — skip it before the
/// prefix check. [observed: codex-cli 0.150.1]
fn is_codex_agents_preamble(text: &str) -> bool {
    let mut t = text.trim_start();
    if t.starts_with("<recommended_plugins>") {
        if let Some(end) = t.find("</recommended_plugins>") {
            t = t[end + "</recommended_plugins>".len()..].trim_start();
        }
    }
    t.starts_with("# AGENTS.md instructions for ") && t.contains("<INSTRUCTIONS>")
}

pub(crate) fn note_message(d: &mut SessionDetail, role: &str, text: &str) {
    match role {
        "user" => {
            if is_system_meta_prompt(text) {
                return;
            }
            d.user_messages += 1;
            if d.first_prompt.is_none() {
                d.first_prompt = Some(clip(text, FIRST_MAX));
            }
            d.last_user = Some(clip(text, LAST_MAX));
        }
        "assistant" => {
            d.assistant_messages += 1;
            d.last_assistant = Some(clip(text, LAST_MAX));
        }
        _ => {}
    }
}

/// A conversation handed over whole by a provider that keeps no transcript
/// file: what can be said from `(role, text)` alone.
fn from_messages(id: String, msgs: Vec<(String, String)>) -> SessionDetail {
    let mut d = SessionDetail {
        id,
        ..Default::default()
    };
    for (role, text) in msgs {
        if role == "user" && is_codex_agents_preamble(&text) {
            continue;
        }
        let text = strip_system_tags(&text);
        if text.trim().is_empty() {
            continue;
        }
        note_message(&mut d, &role, &text);
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn feed(lines: &[serde_json::Value]) -> SessionDetail {
        let mut d = SessionDetail::default();
        let mut tools = HashMap::new();
        for v in lines {
            read_line(&mut d, &mut tools, v);
        }
        d.tools = top_tools(tools);
        d
    }

    #[test]
    fn claude_transcript_yields_the_lot() {
        let d = feed(&[
            json!({"type":"permission-mode","permissionMode":"auto"}),
            json!({"type":"user","timestamp":"2026-08-28T10:00:00Z","cwd":"/w","gitBranch":"5lime","version":"2.1.0",
                   "message":{"role":"user","content":"make the icons bigger"}}),
            json!({"type":"assistant","timestamp":"2026-08-28T10:00:05Z","effort":"high",
                   "message":{"model":"claude-fable-5","usage":{"input_tokens":10,"cache_read_input_tokens":90,"cache_creation_input_tokens":0,"output_tokens":40},
                   "content":[{"type":"text","text":"On it."},{"type":"tool_use","name":"Edit","input":{"file_path":"/w/a.css"}},{"type":"tool_use","name":"Bash","input":{}}]}}),
            json!({"type":"assistant","timestamp":"2026-08-28T10:01:00Z","isSidechain":true,
                   "message":{"model":"claude-haiku","usage":{"input_tokens":999,"output_tokens":1},"content":[{"type":"text","text":"sub"}]}}),
            json!({"type":"assistant","timestamp":"2026-08-28T10:02:00Z",
                   "message":{"model":"claude-fable-5","usage":{"input_tokens":20,"cache_read_input_tokens":180,"cache_creation_input_tokens":5,"output_tokens":60},
                   "content":[{"type":"text","text":"Done — a.css and b.ts."},{"type":"tool_use","name":"Edit","input":{"file_path":"/w/b.ts"}},{"type":"tool_use","name":"Edit","input":{"file_path":"/w/a.css"}}]}}),
            json!({"type":"ai-title","aiTitle":"Bigger icons"}),
            json!({"type":"pr-link","prUrl":"https://github.com/x/y/pull/13"}),
            json!({"type":"system","subtype":"compact_boundary","timestamp":"2026-08-28T10:03:00Z"}),
        ]);
        assert_eq!(d.started.as_deref(), Some("2026-08-28T10:00:00Z"));
        assert_eq!(d.last_active.as_deref(), Some("2026-08-28T10:03:00Z"));
        assert_eq!(d.cwd.as_deref(), Some("/w"));
        assert_eq!(d.branch.as_deref(), Some("5lime"));
        assert_eq!(d.cli_version.as_deref(), Some("2.1.0"));
        assert_eq!(
            d.models,
            vec!["claude-fable-5"],
            "sidechain model is not the session's"
        );
        assert_eq!(d.effort.as_deref(), Some("high"));
        assert_eq!(d.permission_mode.as_deref(), Some("auto"));
        assert_eq!((d.user_messages, d.assistant_messages), (1, 2));
        assert_eq!(d.tool_calls, 4);
        assert_eq!(
            d.tools[0],
            ToolCount {
                name: "Edit".into(),
                count: 3
            }
        );
        assert_eq!(
            d.context_tokens,
            Some(205),
            "last main-thread turn's input side"
        );
        assert_eq!(d.output_tokens, 100, "sidechain output not counted");
        assert_eq!(d.title.as_deref(), Some("Bigger icons"));
        assert_eq!(d.first_prompt.as_deref(), Some("make the icons bigger"));
        assert_eq!(d.last_assistant.as_deref(), Some("Done — a.css and b.ts."));
        assert_eq!(
            d.files,
            vec!["/w/a.css", "/w/b.ts"],
            "most recent first, no repeats"
        );
        assert_eq!(d.pr_links, vec!["https://github.com/x/y/pull/13"]);
        assert_eq!(d.compactions, 1);
    }

    #[test]
    fn codex_transcript_yields_what_it_records() {
        let d = feed(&[
            json!({"timestamp":"2026-08-28T23:18:39Z","type":"session_meta","payload":{"cwd":"/c","cli_version":"0.150.1"}}),
            json!({"timestamp":"2026-08-28T23:18:39Z","type":"event_msg","payload":{"type":"task_started","model_context_window":258400}}),
            json!({"timestamp":"2026-08-28T23:18:39Z","type":"turn_context","payload":{"model":"gpt-5-codex","approval_policy":"on-request"}}),
            // Codex's first "user" message is the repo's AGENTS.md — harness
            // preamble, not the person. [observed: codex-cli 0.150.1]
            json!({"timestamp":"2026-08-28T23:18:39Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions for /c\n\n<INSTRUCTIONS>\n# Agent start\nrules live here\n</INSTRUCTIONS>"}]}}),
            json!({"timestamp":"2026-08-28T23:18:40Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}),
            json!({"timestamp":"2026-08-28T23:18:41Z","type":"response_item","payload":{"type":"function_call","name":"shell"}}),
            json!({"timestamp":"2026-08-28T23:18:42Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Hey John!"}]}}),
            json!({"timestamp":"2026-08-28T23:18:42Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"output_tokens":14},"last_token_usage":{"input_tokens":15612}}}}),
            json!({"timestamp":"2026-08-28T23:18:43Z","type":"compacted","payload":{"message":"","replacement_history":[]}}),
        ]);
        assert_eq!(d.cwd.as_deref(), Some("/c"));
        assert_eq!(d.cli_version.as_deref(), Some("0.150.1"));
        assert_eq!(d.models, vec!["gpt-5-codex"]);
        assert_eq!(d.permission_mode.as_deref(), Some("on-request"));
        assert_eq!(d.context_window, Some(258400));
        assert_eq!(d.context_tokens, Some(15612));
        assert_eq!(d.output_tokens, 14);
        assert_eq!(d.tool_calls, 1);
        assert_eq!(
            d.tools,
            vec![ToolCount {
                name: "shell".into(),
                count: 1
            }]
        );
        assert_eq!(
            d.user_messages, 1,
            "the AGENTS.md preamble is not the conversation"
        );
        assert_eq!(
            d.first_prompt.as_deref(),
            Some("hi"),
            "the preamble is never the first prompt"
        );
        assert_eq!(d.last_assistant.as_deref(), Some("Hey John!"));
        assert_eq!(d.compactions, 1, "codex's `compacted` record counts too");
    }

    #[test]
    fn a_message_that_merely_mentions_agents_md_is_kept() {
        let d = feed(&[
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions for this repo need a rewrite — draft one"}]}}),
        ]);
        assert_eq!(
            d.user_messages, 1,
            "no <INSTRUCTIONS> block: the person's own words"
        );
        assert_eq!(
            d.first_prompt.as_deref(),
            Some("# AGENTS.md instructions for this repo need a rewrite — draft one")
        );
    }

    /// Both 0.150.1 exec spellings summarize to the shell command inside:
    /// flagship `custom_tool_call` name `exec` with bare-`cmd:` JavaScript,
    /// mini `function_call` name `exec_command` with JSON `arguments`.
    /// [observed: codex-cli 0.150.1]
    #[test]
    fn codex_exec_one_liners_read_the_command() {
        // Flagship: bare `cmd:` key, other keys quoted, plan call in the same input.
        let js = line_events(
            &json!({"type":"response_item","payload":{"type":"custom_tool_call","name":"exec",
            "input":"const p = await tools.update_plan({plan:[\n  {step:\"Read hello.txt\",status:\"in_progress\"}\n]});\nconst r = await tools.exec_command({cmd:\"sed -n '1,200p' hello.txt\",\"workdir\":\"/w\"})"}}),
        );
        assert_eq!(
            js,
            vec![("exec".to_string(), "sed -n '1,200p' hello.txt".to_string())]
        );
        // Older rollouts quote the key.
        let quoted = line_events(
            &json!({"type":"response_item","payload":{"type":"custom_tool_call","name":"exec",
            "input":"const r = await tools.exec_command({\"cmd\":\"ls -la\"})"}}),
        );
        assert_eq!(quoted, vec![("exec".to_string(), "ls -la".to_string())]);
        // Mini: JSON arguments on a declared `exec_command` tool.
        let json_args = line_events(
            &json!({"type":"response_item","payload":{"type":"function_call","name":"exec_command",
            "arguments":"{\"cmd\":\"wc -c hello.txt\",\"workdir\":\"/w\",\"yield_time_ms\":10000,\"max_output_tokens\":4000}"}}),
        );
        assert_eq!(
            json_args,
            vec![("exec".to_string(), "wc -c hello.txt".to_string())]
        );

        let many = line_events(
            &json!({"type":"response_item","payload":{"type":"custom_tool_call","name":"exec",
            "input":"const a = await tools.exec_command({cmd:\"cargo test\"}); const b = await tools.exec_command({cmd:\"cargo check\"});"}}),
        );
        assert_eq!(
            many,
            vec![("exec".to_string(), "cargo test\ncargo check".to_string())]
        );

        let patch = line_events(
            &json!({"type":"response_item","payload":{"type":"custom_tool_call","name":"exec",
            "input":"const patch = \"*** Begin Patch\\n*** Update File: src/lib.rs\\n*** End Patch\"; text(await tools.apply_patch(patch));"}}),
        );
        assert_eq!(patch[0].0, "apply_patch");
        assert!(patch[0].1.contains("src/lib.rs"));

        let waited = line_events(
            &json!({"type":"response_item","payload":{"type":"custom_tool_call","name":"exec",
            "input":"const r = await tools.write_stdin({session_id:42,chars:\"\"})"}}),
        );
        assert_eq!(
            waited,
            vec![("wait".to_string(), "Background command".to_string())]
        );
    }

    #[test]
    fn codex_custom_tool_calls_count_and_patches_name_files() {
        let d = feed(&[
            json!({"timestamp":"2026-08-17T16:13:43Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"const r = await tools.exec_command({cmd:\"ls\"})"}}),
            json!({"timestamp":"2026-08-17T16:13:44Z","type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":"*** Begin Patch\n*** Update File: src/a.rs\n@@\n-x\n+y\n*** Add File: src/b.rs\n+z\n*** End Patch"}}),
            json!({"timestamp":"2026-08-17T16:13:45Z","type":"response_item","payload":{"type":"function_call","name":"apply_patch","arguments":"{\"input\":\"*** Begin Patch\\n*** Update File: src/a.rs\\n*** End Patch\"}"}}),
        ]);
        assert_eq!(d.tool_calls, 3);
        assert_eq!(
            d.tools,
            vec![
                ToolCount {
                    name: "apply_patch".into(),
                    count: 2
                },
                ToolCount {
                    name: "exec".into(),
                    count: 1
                }
            ]
        );
        assert_eq!(
            d.files,
            vec!["src/a.rs", "src/b.rs"],
            "most recent touch first"
        );
    }

    #[test]
    fn api_chat_transcript_yields_model_provider_and_times() {
        let d = feed(&[
            json!({"type":"aiterm-chat","id":"x","provider":"openrouter","model":"a/b","cwd":"/c","at":1_700_000_000_000u64}),
            json!({"type":"user","message":{"role":"user","content":"hi"},"at":1_700_000_001_000u64}),
            json!({"type":"assistant","message":{"role":"assistant","content":"hello"},"at":1_700_000_002_000u64}),
            json!({"type":"aiterm-chat-model","model":"c/d"}),
            json!({"type":"aiterm-chat-clear"}),
            json!({"type":"user","message":{"role":"user","content":"again"},"at":1_700_000_003_000u64}),
        ]);
        assert_eq!(d.cwd.as_deref(), Some("/c"));
        assert_eq!(d.models, vec!["a/b", "c/d"]);
        assert_eq!(d.permission_mode.as_deref(), Some("via openrouter"));
        assert_eq!(d.started.as_deref(), Some("2023-11-14T22:13:20.000Z"));
        assert_eq!(d.last_active.as_deref(), Some("2023-11-14T22:13:23.000Z"));
        assert_eq!((d.user_messages, d.assistant_messages), (2, 1));
        assert_eq!(d.compactions, 1, "a /clear is the chat's compaction");
        assert_eq!(d.first_prompt.as_deref(), Some("hi"));
        assert_eq!(d.last_user.as_deref(), Some("again"));
    }

    #[test]
    fn iso_from_ms_matches_the_transcripts() {
        assert_eq!(iso_from_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_from_ms(1_787_967_144_650), "2026-08-29T01:32:24.650Z");
        assert_eq!(iso_from_ms(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn clip_marks_the_cut() {
        assert_eq!(clip("  short  ", 10), "short");
        assert_eq!(clip("abcdefghij", 5), "abcde…");
    }
}

/// The conversation as a list of turns, oldest first — what a second agent
/// is handed when it is brought into a session. Tool calls and injected
/// system blocks are left out; only what was said. Trimmed from the front
/// to `max_chars`, keeping the opening user message so the ask is never
/// lost, with a marker where the cut was made.
#[tauri::command]
pub async fn session_conversation(session_id: String, max_chars: usize) -> Vec<(String, String)> {
    crate::run_blocking(move || conversation_sync(&session_id, max_chars)).await
}

pub(crate) fn conversation_sync(session_id: &str, max_chars: usize) -> Vec<(String, String)> {
    let list = crate::agents::backends();
    let Some((backend, path)) = crate::agents::owner_in(&list, session_id) else {
        return vec![];
    };
    let mut turns: Vec<(String, String)> = match backend.sessions().messages(session_id) {
        Some(m) => m,
        None => {
            let Ok(file) = File::open(&path) else {
                return vec![];
            };
            BufReader::new(file)
                .lines()
                .map_while(Result::ok)
                .filter(|l| crate::sessions::line_may_hold_message(l))
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(&l).ok())
                .filter_map(|v| crate::sessions::line_message(&v))
                .collect()
        }
    };
    turns.retain(|(_, t)| {
        !crate::sessions::is_only_system_block(t)
            && !is_codex_agents_preamble(t)
            && !t.trim().is_empty()
    });
    // Adjacent same-role turns (an assistant that spoke, used a tool, spoke
    // again) read as one.
    let mut merged: Vec<(String, String)> = Vec::new();
    for (role, text) in turns {
        match merged.last_mut() {
            Some((r, t)) if *r == role => {
                t.push_str("\n\n");
                t.push_str(&text);
            }
            _ => merged.push((role, text)),
        }
    }
    let total: usize = merged.iter().map(|(_, t)| t.len()).sum();
    if total <= max_chars || merged.len() < 3 {
        return merged;
    }
    // Keep the first turn and as much of the tail as fits.
    let first = merged.remove(0);
    let mut budget = max_chars.saturating_sub(first.1.len());
    let mut tail: Vec<(String, String)> = Vec::new();
    for turn in merged.into_iter().rev() {
        if turn.1.len() > budget {
            break;
        }
        budget -= turn.1.len();
        tail.push(turn);
    }
    tail.reverse();
    let mut out = vec![
        first,
        (
            "system".into(),
            "[… earlier turns omitted for length …]".into(),
        ),
    ];
    out.extend(tail);
    out
}

/// Where a session's transcript is on disk, for a second agent that will
/// read it itself rather than be handed a paste of it. A backend that keeps
/// sessions somewhere other than a file (opencode's database) gets a plain
/// text export of the conversation under `~/.local/share/aiterm/relay/`.
#[tauri::command]
pub async fn session_transcript_path(session_id: String) -> Result<String, String> {
    crate::run_blocking(move || {
        let list = crate::agents::backends();
        let (backend, path) =
            crate::agents::owner_in(&list, &session_id).ok_or("no transcript for that session")?;
        if backend.sessions().messages(&session_id).is_none() && path.is_file() {
            return Ok(path.to_string_lossy().into_owned());
        }
        let turns = conversation_sync(&session_id, usize::MAX);
        let dir = dirs::home_dir()
            .ok_or("no home directory")?
            .join(".local/share/aiterm/relay");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let f = dir.join(format!("{session_id}.txt"));
        let text: String = turns
            .iter()
            .map(|(r, t)| format!("[{r}]\n{t}\n\n"))
            .collect();
        std::fs::write(&f, text).map_err(|e| e.to_string())?;
        Ok(f.to_string_lossy().into_owned())
    })
    .await
}

/// The conversation with the work shown: every message, plus each tool call
/// as a turn named for the tool, bounded tool results, sub-agent messages,
/// and (Codex) reasoning summaries as "thinking". This is what a phone
/// renders while an agent works — the desktop's own preview stays on the
/// message-only parser above.
pub async fn conversation_rich(session_id: String, max_chars: usize) -> Vec<(String, String)> {
    crate::run_blocking(move || conversation_rich_sync(&session_id, max_chars)).await
}

/// Synchronous service entry point for transports that already run their
/// request dispatch on a bounded blocking worker.
pub(crate) fn conversation_rich_service(
    session_id: &str,
    max_chars: usize,
) -> Vec<(String, String)> {
    conversation_rich_sync(session_id, max_chars)
}

const RICH_FILE_CACHE_LIMIT: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RichFileIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl RichFileIdentity {
    fn from(metadata: &std::fs::Metadata) -> Self {
        Self {
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        }
    }
}

struct RichFileCacheEntry {
    path: PathBuf,
    identity: RichFileIdentity,
    offset: u64,
    turns: Vec<(String, String)>,
    last_used: u64,
}

#[derive(Default)]
struct RichFileCache {
    entries: HashMap<String, RichFileCacheEntry>,
    clock: u64,
}

const RICH_FILE_CACHE_TEXT_BYTES: usize = 2 * 1024 * 1024;
const RICH_CACHE_OMISSION: &str = "[… older rich activity omitted from cache …]";

fn rich_file_cache() -> &'static Mutex<RichFileCache> {
    static CACHE: OnceLock<Mutex<RichFileCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(RichFileCache::default()))
}

fn bound_cached_rich_turns(turns: &mut Vec<(String, String)>) {
    let total = turns.iter().map(|(_, text)| text.len()).sum::<usize>();
    if total <= RICH_FILE_CACHE_TEXT_BYTES {
        return;
    }
    let first_index = turns.iter().position(|(_, text)| {
        !crate::sessions::is_only_system_block(text)
            && !is_codex_agents_preamble(text)
            && !text.trim().is_empty()
    });
    let first = first_index
        .and_then(|index| turns.get(index).cloned())
        .map(|(role, text)| (role, cap(&text, 64 * 1024)));
    let reserved = first.as_ref().map_or(0, |(_, text)| text.len()) + RICH_CACHE_OMISSION.len();
    let mut budget = RICH_FILE_CACHE_TEXT_BYTES.saturating_sub(reserved);
    let mut tail = Vec::new();
    for (index, turn) in turns.iter().enumerate().rev() {
        if first_index.is_some_and(|first_index| index <= first_index) {
            break;
        }
        if turn.1.len() > budget {
            break;
        }
        budget -= turn.1.len();
        tail.push(turn.clone());
    }
    tail.reverse();
    turns.clear();
    if let Some(first) = first {
        turns.push(first);
    }
    turns.push(("system".into(), RICH_CACHE_OMISSION.into()));
    turns.extend(tail);
}

/// Read a growing transcript once, then parse only complete lines appended
/// since the previous phone refresh. A long-running Codex rollout can exceed
/// 100 MiB; reparsing it every 1.5 seconds made the structured view visibly
/// trail the terminal even though the transcript already contained the turn.
///
/// The held file's identity and length make replacement (`/clear`) and
/// truncation reset the cache. A partial final JSONL line is deliberately left
/// before `offset` so the next append retries it rather than losing a message.
fn cached_rich_file_events(session_id: &str, path: &Path) -> Vec<(String, String)> {
    let Ok(mut file) = File::open(path) else {
        return Vec::new();
    };
    let Ok(metadata) = file.metadata() else {
        return Vec::new();
    };
    let identity = RichFileIdentity::from(&metadata);
    let mut cache = rich_file_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.clock = cache.clock.wrapping_add(1);
    let now = cache.clock;

    let reset = cache.entries.get(session_id).is_none_or(|entry| {
        entry.path != path || entry.identity != identity || metadata.len() < entry.offset
    });
    if reset {
        if !cache.entries.contains_key(session_id) && cache.entries.len() >= RICH_FILE_CACHE_LIMIT {
            if let Some(oldest) = cache
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(id, _)| id.clone())
            {
                cache.entries.remove(&oldest);
            }
        }
        cache.entries.insert(
            session_id.to_string(),
            RichFileCacheEntry {
                path: path.to_path_buf(),
                identity,
                offset: 0,
                turns: Vec::new(),
                last_used: now,
            },
        );
    }

    let entry = cache
        .entries
        .get_mut(session_id)
        .expect("rich cache entry was inserted");
    entry.last_used = now;
    if file.seek(SeekFrom::Start(entry.offset)).is_err() {
        return entry.turns.clone();
    }
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    loop {
        line.clear();
        let Ok(read) = reader.read_line(&mut line) else {
            break;
        };
        if read == 0 || !line.ends_with('\n') {
            break;
        }
        entry.offset = entry.offset.saturating_add(read as u64);
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim_end()) else {
            continue;
        };
        if value.get("isSidechain").and_then(|flag| flag.as_bool()) == Some(true)
            || value.get("isMeta").and_then(|flag| flag.as_bool()) == Some(true)
        {
            continue;
        }
        entry.turns.extend(line_events(&value));
    }
    bound_cached_rich_turns(&mut entry.turns);
    entry.turns.clone()
}

fn conversation_rich_sync(session_id: &str, max_chars: usize) -> Vec<(String, String)> {
    let list = crate::agents::backends();
    let Some((backend, path)) = crate::agents::owner_in(&list, session_id) else {
        return vec![];
    };
    let mut turns: Vec<(String, String)> = match backend.sessions().messages(session_id) {
        Some(m) => m,
        None => cached_rich_file_events(session_id, &path),
    };
    turns.retain(|(_, t)| {
        !crate::sessions::is_only_system_block(t)
            && !is_codex_agents_preamble(t)
            && !t.trim().is_empty()
    });
    let mut merged: Vec<(String, String)> = Vec::new();
    for (role, text) in turns {
        match merged.last_mut() {
            // Same speaker twice in a row reads as one; tool calls stay separate.
            Some((r, t))
                if *r == role && (role == "user" || role == "assistant" || role == "thinking") =>
            {
                t.push_str("\n\n");
                t.push_str(&text);
            }
            _ => merged.push((role, text)),
        }
    }
    let total: usize = merged.iter().map(|(_, t)| t.len()).sum();
    if total <= max_chars || merged.len() < 3 {
        return merged;
    }
    let first = merged.remove(0);
    let mut budget = max_chars.saturating_sub(first.1.len());
    let mut tail: Vec<(String, String)> = Vec::new();
    for turn in merged.into_iter().rev() {
        if turn.1.len() > budget {
            break;
        }
        budget -= turn.1.len();
        tail.push(turn);
    }
    tail.reverse();
    let mut out = vec![
        first,
        (
            "system".into(),
            "[… earlier turns omitted for length …]".into(),
        ),
    ];
    out.extend(tail);
    out
}

const TOOL_INPUT_TEXT_CAP: usize = 4 * 1024;
const TOOL_OUTPUT_TEXT_CAP: usize = 8 * 1024;

fn cap(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    const MARKER: &str = "…";
    let mut end = max.saturating_sub(MARKER.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{MARKER}", &s[..end])
}

fn content_text(content: Option<&serde_json::Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    match content {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| {
                block
                    .get("text")
                    .and_then(|text| text.as_str())
                    .or_else(|| block.as_str())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// One transcript line → zero or more turns. Claude's assistant lines carry
/// text and tool-use blocks side by side; Codex writes each item on its own
/// line. Results stay bounded and collapse into the activity group on the
/// phone, so useful command output remains available without taking over the
/// conversation.
fn line_events(v: &serde_json::Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    match v.get("type").and_then(|t| t.as_str()) {
        Some(r @ ("user" | "assistant")) => {
            let Some(content) = v.pointer("/message/content") else {
                return out;
            };
            match content {
                serde_json::Value::String(s) => out.push((r.to_string(), s.clone())),
                serde_json::Value::Array(blocks) => {
                    let mut text = String::new();
                    for b in blocks {
                        match b.get("type").and_then(|t| t.as_str()) {
                            Some("tool_use") => {
                                if !text.trim().is_empty() {
                                    out.push((r.to_string(), std::mem::take(&mut text)));
                                }
                                let name = b.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                                out.push((
                                    name.to_string(),
                                    cap(&tool_input_summary(b.get("input")), TOOL_INPUT_TEXT_CAP),
                                ));
                            }
                            Some("tool_result") => {
                                if !text.trim().is_empty() {
                                    out.push((r.to_string(), std::mem::take(&mut text)));
                                }
                                let result = content_text(b.get("content"));
                                if !result.trim().is_empty() {
                                    out.push((
                                        "tool_output".into(),
                                        cap(&result, TOOL_OUTPUT_TEXT_CAP),
                                    ));
                                }
                            }
                            _ => {
                                if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                                    if !text.is_empty() {
                                        text.push('\n');
                                    }
                                    text.push_str(t);
                                }
                            }
                        }
                    }
                    if !text.trim().is_empty() {
                        out.push((r.to_string(), text));
                    }
                }
                _ => {}
            }
        }
        Some("response_item") => {
            let Some(p) = v.get("payload") else {
                return out;
            };
            match p.get("type").and_then(|t| t.as_str()) {
                Some("message") => {
                    if let Some(t) = crate::sessions::line_message(v) {
                        out.push(t);
                    }
                }
                Some("custom_tool_call") | Some("function_call") => {
                    let name = p.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                    let input = p
                        .get("input")
                        .or_else(|| p.get("arguments"))
                        .map(|i| match i {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .unwrap_or_default();
                    let (name, text) = if name == "exec" || name == "exec_command" {
                        codex_exec_summary(name, &input)
                    } else {
                        (name.to_string(), input)
                    };
                    out.push((name, cap(&text, TOOL_INPUT_TEXT_CAP)));
                }
                Some("custom_tool_call_output") | Some("function_call_output") => {
                    let result = content_text(p.get("output"));
                    if !result.trim().is_empty() {
                        out.push(("tool_output".into(), cap(&result, TOOL_OUTPUT_TEXT_CAP)));
                    }
                }
                Some("agent_message") => {
                    let message = content_text(p.get("content"));
                    if !message.trim().is_empty() {
                        let author = p.get("author").and_then(|author| author.as_str());
                        let text = match author {
                            Some(author) if !author.is_empty() => format!("{author}\n{message}"),
                            _ => message,
                        };
                        out.push(("agent_message".into(), cap(&text, TOOL_OUTPUT_TEXT_CAP)));
                    }
                }
                Some("reasoning") => {
                    let mut text = String::new();
                    if let Some(items) = p.get("summary").and_then(|s| s.as_array()) {
                        for it in items {
                            if let Some(t) = it.get("text").and_then(|t| t.as_str()) {
                                if !text.is_empty() {
                                    text.push('\n');
                                }
                                text.push_str(t);
                            }
                        }
                    }
                    if !text.trim().is_empty() {
                        out.push(("thinking".into(), text));
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
    out
}

/// Codex's exec input, either spelling, down to what a person cares about —
/// the shell command inside, not how it was invoked. Flagship models write
/// `custom_tool_call` name `exec` whose input is a JavaScript snippet,
/// `tools.exec_command({cmd:"…"})` — the `cmd` key BARE on current rollouts,
/// quoted on older ones, so both are probed. Mini models write
/// `function_call` name `exec_command` with JSON `arguments` carrying the
/// same `"cmd"`. `tools.image_gen__imagegen({prompt:…})` is an image being
/// generated — show it as one, with its prompt. Anything else stays raw.
/// [observed: codex-cli 0.150.1; bare `cmd:` back to 0.147.0, quoted before]
fn codex_exec_summary(name: &str, input: &str) -> (String, String) {
    if input.contains("tools.image_gen__imagegen(") {
        let text = js_string_after(input, "prompt:\"")
            .or_else(|| js_string_after(input, "prompt: \""))
            .unwrap_or_else(|| "Generating an image".into());
        return ("image".into(), text);
    }
    if input.contains("tools.apply_patch(") {
        let text = js_string_after(input, "const patch = \"")
            .unwrap_or_else(|| "Applied a source patch".into());
        return ("apply_patch".into(), text);
    }
    if input.contains("tools.write_stdin(") || input.contains("tools.wait(") {
        return ("wait".into(), "Background command".into());
    }
    if input.contains("tools.exec_command(") || name == "exec_command" {
        let commands = js_strings_after(input, &["\"cmd\":\"", "cmd:\""]);
        if !commands.is_empty() {
            return ("exec".into(), commands.join("\n"));
        }
    }
    ("exec".into(), input.to_string())
}

fn js_strings_after(s: &str, keys: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = 0;
    while cursor < s.len() {
        let next = keys
            .iter()
            .filter_map(|key| s[cursor..].find(key).map(|position| (position, *key)))
            .min_by_key(|(position, _)| *position);
        let Some((position, key)) = next else {
            break;
        };
        let start = cursor + position;
        if let Some(value) = js_string_after(&s[start..], key) {
            out.push(value);
        }
        cursor = start + key.len();
    }
    out
}

/// The double-quoted string starting right after `key`, JSON-style escapes
/// resolved. Good enough for the two shapes above; `None` when the string
/// never closes.
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

/// A tool call's input, as a person would skim it: the command for Bash,
/// the path for file tools, the pattern for searches, else the JSON.
pub(crate) fn tool_input_summary(input: Option<&serde_json::Value>) -> String {
    let Some(i) = input else { return String::new() };
    for key in [
        "command",
        "file_path",
        "path",
        "pattern",
        "query",
        "description",
        "prompt",
        "url",
    ] {
        if let Some(s) = i.get(key).and_then(|s| s.as_str()) {
            let extra = i
                .get("description")
                .and_then(|d| d.as_str())
                .filter(|_| key != "description");
            return match extra {
                Some(d) => format!("{s}\n{d}"),
                None => s.to_string(),
            };
        }
    }
    i.to_string()
}

#[cfg(test)]
mod conversation_tests {
    use std::io::Write;

    fn record(role: &str, text: &str) -> String {
        serde_json::json!({"type": role, "message": {"content": text}}).to_string()
    }

    #[test]
    fn rich_file_cache_tails_complete_lines_and_resets_after_truncation() {
        let root = std::env::temp_dir().join(format!("aiterm-rich-tail-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("session.jsonl");
        let session_id = format!("rich-tail-{}", uuid::Uuid::new_v4());
        std::fs::write(&path, format!("{}\n", record("user", "first"))).unwrap();

        assert_eq!(
            super::cached_rich_file_events(&session_id, &path),
            vec![("user".into(), "first".into())],
        );

        let mut append = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        write!(append, "{}", record("assistant", "second")).unwrap();
        append.flush().unwrap();
        assert_eq!(super::cached_rich_file_events(&session_id, &path).len(), 1);
        writeln!(append).unwrap();
        append.flush().unwrap();
        assert_eq!(
            super::cached_rich_file_events(&session_id, &path),
            vec![
                ("user".into(), "first".into()),
                ("assistant".into(), "second".into())
            ],
        );
        drop(append);

        std::fs::write(&path, format!("{}\n", record("user", "fresh"))).unwrap();
        assert_eq!(
            super::cached_rich_file_events(&session_id, &path),
            vec![("user".into(), "fresh".into())],
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn codex_rich_events_include_tool_outputs_and_agent_messages() {
        let custom_output = serde_json::json!({
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call_output",
                "output": [
                    {"type": "input_text", "text": "Script completed"},
                    {"type": "input_text", "text": "tests: 12 passed"}
                ]
            }
        });
        assert_eq!(
            super::line_events(&custom_output),
            vec![(
                "tool_output".into(),
                "Script completed\ntests: 12 passed".into()
            )],
        );

        let function_output = serde_json::json!({
            "type": "response_item",
            "payload": {"type": "function_call_output", "output": "Wait completed."}
        });
        assert_eq!(
            super::line_events(&function_output),
            vec![("tool_output".into(), "Wait completed.".into())],
        );

        let agent_message = serde_json::json!({
            "type": "response_item",
            "payload": {
                "type": "agent_message",
                "author": "/root/reviewer",
                "content": [{"type": "input_text", "text": "Review passed."}]
            }
        });
        assert_eq!(
            super::line_events(&agent_message),
            vec![(
                "agent_message".into(),
                "/root/reviewer\nReview passed.".into()
            )],
        );
    }

    #[test]
    fn claude_rich_events_include_tool_results_without_making_them_user_turns() {
        let result = serde_json::json!({
            "type": "user",
            "message": {
                "content": [{
                    "type": "tool_result",
                    "content": [{"type": "text", "text": "cargo test: ok"}]
                }]
            }
        });
        assert_eq!(
            super::line_events(&result),
            vec![("tool_output".into(), "cargo test: ok".into())],
        );
    }

    #[test]
    fn rich_cache_keeps_the_first_real_prompt_and_newest_bounded_activity() {
        let mut turns = vec![(
            "user".into(),
            "# AGENTS.md instructions for /work\n<INSTRUCTIONS>rules</INSTRUCTIONS>".into(),
        )];
        turns.push(("user".into(), "the real prompt".into()));
        turns.extend((0..400).map(|index| {
            (
                "tool_output".into(),
                format!("{index}:{}", "x".repeat(8_000)),
            )
        }));
        turns.push(("assistant".into(), "newest answer".into()));

        super::bound_cached_rich_turns(&mut turns);

        assert_eq!(turns.first().unwrap().1, "the real prompt");
        assert_eq!(turns[1].1, super::RICH_CACHE_OMISSION);
        assert_eq!(turns.last().unwrap().1, "newest answer");
        assert!(
            turns.iter().map(|(_, text)| text.len()).sum::<usize>()
                <= super::RICH_FILE_CACHE_TEXT_BYTES
        );
    }

    /// Print a real session's conversation as the relay would hand it over.
    /// `AITERM_SESSION=<id> cargo test --lib conversation_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn conversation_live() {
        let id = std::env::var("AITERM_SESSION").expect("AITERM_SESSION");
        let cold_started = std::time::Instant::now();
        let _ = super::conversation_rich_sync(&id, 24_000);
        eprintln!("cold rich read: {:?}", cold_started.elapsed());
        let warm_started = std::time::Instant::now();
        let messages = super::conversation_rich_sync(&id, 24_000);
        eprintln!("warm rich read: {:?}", warm_started.elapsed());
        for (role, text) in messages {
            println!("[{role}] {}", super::clip(&text, 300));
        }
    }
}
