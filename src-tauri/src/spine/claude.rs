//! Claude Code adapter: the transcript at `~/.claude/projects/<proj>/<id>.jsonl`,
//! read from a byte offset, one line per content block.
//!
//! Owned by the claude-adapter task. See `docs/architecture/spine.md`.
//!
//! The file is append-only while a session runs, so the tail is a seek to the
//! saved offset and a read to EOF — no re-parse of the history. Two things
//! break that and both are handled: the writer can be caught mid-line (the
//! bytes after the last `\n` are held back until the newline arrives), and a
//! `/clear` retires the file and starts a new one under the same name (a
//! shorter file or a new inode ⇒ `Reset`, then read from zero).
//!
//! [observed: Claude Code 2.1.226 – 2.1.259, 2026-09-02]

use super::{clip, now_ms, Adapter, Kind, ToolCategory, ToolStatus};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

/// A tool call's one-line input summary, on the wire.
const INPUT_CAP: usize = 400;
/// A tool result's output, on the wire. Bigger than the input because the
/// output is what a person reads to know whether the call worked.
const OUTPUT_CAP: usize = 2_000;
/// A card's heading. A Bash command's first line is unbounded in principle.
const TITLE_CAP: usize = 200;

pub struct ClaudeAdapter {
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
    /// The uuid of the `user` line that opened the turn now running, so the
    /// `turn_duration` that closes it names the same turn.
    turn: Option<String>,
}

/// The adapter for a Claude session, or `None` when the id is not a Claude
/// session at all.
///
/// A path with no file behind it still gets an adapter: a session launched a
/// moment ago has an id and a tab before Claude Code writes its first line,
/// and the registry's watch on the parent directory is what notices the file
/// appearing. `poll` reads an absent file as empty.
pub fn open(session_id: &str) -> Option<ClaudeAdapter> {
    let list = crate::agents::backends();
    let (backend, path) = crate::agents::owner_in(&list, session_id)?;
    if backend.id() != "claude" {
        return None;
    }
    Some(ClaudeAdapter { path, offset: 0, pending: Vec::new(), ident: None, turn: None })
}

impl Adapter for ClaudeAdapter {
    /// The whole history, as `poll` would have produced it line by line.
    fn bootstrap(&mut self) -> Vec<(u64, Kind)> {
        self.offset = 0;
        self.pending.clear();
        self.ident = None;
        self.turn = None;
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
        // A `/clear` renames the transcript to `<id>.orphaned-…` and opens a
        // new one, so the same path can become a different, shorter file
        // between two polls. Length alone would miss a replacement that is
        // already longer than what we had read.
        let replaced = meta.len() < self.offset
            || (self.ident.is_some() && ident.is_some() && ident != self.ident);
        if replaced {
            out.push((now_ms(), Kind::Reset));
            self.offset = 0;
            self.pending.clear();
            self.turn = None;
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

        // Only whole lines are parsed. Claude Code writes a record with one
        // `write`, but a reader woken by the same inotify event can still see
        // half of it; the rest arrives with the next newline.
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

    /// The transcript, plus the project directory that holds it — a session
    /// whose file does not exist yet is only noticed by watching the folder.
    fn watch_paths(&self) -> Vec<PathBuf> {
        let mut paths = vec![self.path.clone()];
        if let Some(dir) = self.path.parent() {
            paths.push(dir.to_path_buf());
        }
        paths
    }
}

impl ClaudeAdapter {
    fn read_line(&mut self, raw: &[u8], out: &mut Vec<(u64, Kind)>) {
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(raw) else { return };
        // Subagent chatter (`isSidechain`) is a conversation of its own and
        // harness-injected text (`isMeta`: a loaded skill's body, an image's
        // dimensions) was never said by the person. `conversation_rich` skips
        // both for the same reasons. [observed: Claude Code 2.1.251]
        if v.get("isSidechain").and_then(|b| b.as_bool()) == Some(true)
            || v.get("isMeta").and_then(|b| b.as_bool()) == Some(true)
        {
            return;
        }
        let ts = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .and_then(iso_to_ms)
            .unwrap_or_else(now_ms);
        let uuid = v.get("uuid").and_then(|u| u.as_str()).unwrap_or_default();

        match v.get("type").and_then(|t| t.as_str()) {
            Some("user") => self.user_line(&v, uuid, ts, out),
            Some("assistant") => assistant_line(&v, uuid, ts, out),
            // The one system line that says something: the turn is over.
            // `away_summary`, `informational` and the rest are the harness
            // talking about itself. [observed: Claude Code 2.1.258]
            Some("system") if v.get("subtype").and_then(|s| s.as_str()) == Some("turn_duration") => {
                // A tail that started mid-file has not seen the turn open;
                // the line's own uuid at least names it stably.
                let turn = self.turn.take().unwrap_or_else(|| uuid.to_string());
                out.push((ts, Kind::TurnEnded { turn, reason: "completed".into() }));
            }
            // `attachment`, `summary`, `mode`, `permission-mode`, `ai-title`,
            // `last-prompt`, `bridge-session`, `file-history-*`, `cost-state`,
            // `queue-operation`, … — bookkeeping, not conversation.
            _ => {}
        }
    }

    fn user_line(
        &mut self,
        v: &serde_json::Value,
        uuid: &str,
        ts: u64,
        out: &mut Vec<(u64, Kind)>,
    ) {
        match v.pointer("/message/content") {
            Some(serde_json::Value::String(s)) => {
                if let Some(stdout) = between(s, "<local-command-stdout>", "</local-command-stdout>")
                {
                    // The second half of a slash command: what the command
                    // printed, on its own `user` line. It answers the command
                    // rather than opening a turn.
                    out.push((
                        ts,
                        Kind::UserMessage { id: uuid.to_string(), text: stdout.to_string() },
                    ));
                    return;
                }
                let text = slash_command(s).unwrap_or_else(|| s.clone());
                self.turn = Some(uuid.to_string());
                out.push((ts, Kind::TurnStarted { turn: uuid.to_string() }));
                out.push((ts, Kind::UserMessage { id: uuid.to_string(), text }));
            }
            Some(serde_json::Value::Array(blocks)) => {
                for (i, b) in blocks.iter().enumerate() {
                    match b.get("type").and_then(|t| t.as_str()) {
                        Some("tool_result") => {
                            let Some(id) = b.get("tool_use_id").and_then(|t| t.as_str()) else {
                                continue;
                            };
                            let failed =
                                b.get("is_error").and_then(|e| e.as_bool()) == Some(true);
                            out.push((
                                ts,
                                Kind::ToolCallUpdate {
                                    id: id.to_string(),
                                    status: if failed {
                                        ToolStatus::Failed
                                    } else {
                                        ToolStatus::Completed
                                    },
                                    output: result_text(b.get("content"))
                                        .map(|t| clip(&t, OUTPUT_CAP)),
                                },
                            ));
                        }
                        // A `user` line whose content is text blocks rather
                        // than a string: an interruption, "[Request
                        // interrupted by user]". Said on the person's behalf,
                        // so it shows — but it opens no turn.
                        Some("text") => {
                            let Some(t) = b.get("text").and_then(|t| t.as_str()) else { continue };
                            out.push((
                                ts,
                                Kind::UserMessage {
                                    id: format!("{uuid}:{i}"),
                                    text: t.to_string(),
                                },
                            ));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

/// An assistant line: one content block, complete the moment it is written —
/// Claude Code appends the record after the block closes, so `done` is always
/// true and a consumer never has to stitch deltas.
fn assistant_line(v: &serde_json::Value, uuid: &str, ts: u64, out: &mut Vec<(u64, Kind)>) {
    let Some(blocks) = v.pointer("/message/content").and_then(|c| c.as_array()) else { return };
    // `<message.id>:<apiBlockIndex>` is the API's own name for the block and
    // survives a re-read. Before 2.1.252 the line carried no `apiBlockIndex`
    // and every block of one message still wrote `content` of length 1, so
    // `message.id` alone collides across the blocks of a message — there the
    // line's uuid is the only per-block id the file has.
    // [observed: no apiBlockIndex in 2.1.251, present from 2.1.252]
    let base = match (
        v.pointer("/message/id").and_then(|i| i.as_str()),
        v.get("apiBlockIndex").and_then(|i| i.as_u64()),
    ) {
        (Some(mid), Some(idx)) => format!("{mid}:{idx}"),
        _ => uuid.to_string(),
    };
    for (i, b) in blocks.iter().enumerate() {
        let id = if blocks.len() > 1 { format!("{base}.{i}") } else { base.clone() };
        match b.get("type").and_then(|t| t.as_str()) {
            Some("thinking") => {
                let text = b.get("thinking").and_then(|t| t.as_str()).unwrap_or_default();
                if text.is_empty() {
                    // A redacted or signature-only thinking block: nothing to show.
                    continue;
                }
                out.push((ts, Kind::AgentThought { id, text: text.to_string(), done: true }));
            }
            Some("text") => {
                let text = b.get("text").and_then(|t| t.as_str()).unwrap_or_default();
                if text.is_empty() {
                    continue;
                }
                out.push((ts, Kind::AgentText { id, text: text.to_string(), done: true }));
            }
            Some("tool_use") => {
                // The tool's own id, so the `tool_result` that lands later
                // updates this card instead of adding one.
                let Some(call_id) = b.get("id").and_then(|i| i.as_str()) else { continue };
                let name = b.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                let input = b.get("input");
                out.push((
                    ts,
                    Kind::ToolCall {
                        id: call_id.to_string(),
                        tool: name.to_string(),
                        title: clip(&title(name, input), TITLE_CAP),
                        category: category(name),
                        input: clip(&crate::detail::tool_input_summary(input), INPUT_CAP),
                        // Not `Pending`: the record is written ~15 ms before
                        // the tool runs, and unless a permission prompt
                        // intervenes it runs at once. A prompt is reported on
                        // the phase channel, which is where waiting belongs.
                        status: ToolStatus::Running,
                    },
                ));
            }
            _ => {}
        }
    }
}

/// What a tool call is, so a card wears the right mark without knowing the
/// engine's tool names. MCP tools (`mcp__…`) and the harness's own verbs stay
/// `Other` rather than being guessed at from their names.
fn category(name: &str) -> ToolCategory {
    match name {
        "Read" | "NotebookRead" => ToolCategory::Read,
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => ToolCategory::Edit,
        "Bash" | "BashOutput" | "Monitor" => ToolCategory::Execute,
        "Grep" | "Glob" | "WebSearch" | "ToolSearch" => ToolCategory::Search,
        "WebFetch" => ToolCategory::Fetch,
        "Task" | "Agent" => ToolCategory::Think,
        _ => ToolCategory::Other,
    }
}

/// A human one-liner for the card's heading: the part of the input a person
/// would read to know what the call is. Falls back to the tool's name, which
/// is what an unknown tool has.
fn title(name: &str, input: Option<&serde_json::Value>) -> String {
    let field = |key: &str| input.and_then(|i| i.get(key)).and_then(|s| s.as_str());
    let picked = match name {
        // Multi-line commands are common; the first line is the verb.
        "Bash" | "Monitor" => field("command").and_then(|c| c.lines().next()),
        "Read" | "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
            field("file_path").or_else(|| field("notebook_path")).or_else(|| field("path"))
        }
        "Grep" | "Glob" => field("pattern"),
        "WebSearch" | "ToolSearch" => field("query"),
        "WebFetch" => field("url"),
        "Task" | "Agent" => field("description"),
        "Skill" => field("skill"),
        _ => None,
    };
    picked.filter(|s| !s.is_empty()).unwrap_or(name).to_string()
}

/// A `tool_result`'s content as text. It is a bare string most of the time and
/// an array of blocks when the tool returned an image alongside its words —
/// the image is named rather than dropped, so a screenshot result is not a
/// blank card. `None` when the tool said nothing at all.
fn result_text(content: Option<&serde_json::Value>) -> Option<String> {
    match content? {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(blocks) => {
            let mut parts: Vec<&str> = Vec::new();
            for b in blocks {
                match b.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                            parts.push(t);
                        }
                    }
                    Some("image") => parts.push("[image]"),
                    _ => {}
                }
            }
            Some(parts.join("\n"))
        }
        _ => None,
    }
}

/// A slash command's `user` line rendered as the command the person typed.
/// Claude Code stores it as a small XML envelope, which is not what anyone
/// wants to read on a phone. `None` for an ordinary message.
/// [observed: Claude Code 2.1.226 – 2.1.259]
fn slash_command(s: &str) -> Option<String> {
    let name = between(s, "<command-name>", "</command-name>")?.trim();
    let args = between(s, "<command-args>", "</command-args>").unwrap_or("").trim();
    Some(if args.is_empty() { name.to_string() } else { format!("{name} {args}") })
}

fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = s.find(open)? + open.len();
    let end = s[start..].find(close)? + start;
    Some(&s[start..end])
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

/// "2026-09-02T20:39:29.099Z" → millis. Enough of ISO 8601 for the timestamps
/// the transcripts write; anything else gets `None` and the caller stamps now.
/// (`remote_api` keeps a seconds-only sibling of this; the spine needs the
/// millis, because two blocks of one message land in the same second.)
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
    // Milliseconds, whatever precision the writer used.
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

/// The busiest transcript on this machine, for the live test below.
#[cfg(test)]
fn busiest_transcript() -> Option<PathBuf> {
    let root = dirs::home_dir()?.join(".claude/projects");
    let mut best: Option<(u64, PathBuf)> = None;
    for project in std::fs::read_dir(root).ok()?.flatten() {
        let Ok(files) = std::fs::read_dir(project.path()) else { continue };
        for f in files.flatten() {
            let p = f.path();
            if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(len) = f.metadata().map(|m| m.len()) else { continue };
            if best.as_ref().is_none_or(|(b, _)| len > *b) {
                best = Some((len, p));
            }
        }
    }
    best.map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::Path;

    /// Verbatim lines from `~/.claude/projects` on this machine, long strings
    /// (signatures, `usage`, tool output) cut but nothing reshaped.
    /// [observed: Claude Code 2.1.226 and 2.1.258, 2026-09-02]
    const USER: &str = r#"{"parentUuid":"9cb4168e-294d-40ab-b75b-080caca45357","isSidechain":false,"promptId":"cfaf01d8-845b-4864-a224-673c7e732ae1","type":"user","message":{"role":"user","content":"plugged back in. install it and take the screenshot"},"uuid":"2d2da9bc-e53a-44ce-8c35-abda6dba6de3","timestamp":"2026-09-02T21:25:21.179Z","permissionMode":"bypassPermissions","origin":{"kind":"human"},"promptSource":"suggestion_accepted","userType":"external","entrypoint":"cli","cwd":"/home/admin/AI-OS/projects/aiterm","sessionId":"44ced9fd-a7fa-4761-8225-1bd1f24f6d51","version":"2.1.258","gitBranch":"master"}"#;

    const THINKING: &str = r#"{"parentUuid":"0b0dd3c8-6a97-4e6c-9c00-5f1bba4e0293","isSidechain":false,"message":{"model":"claude-fable-5-1","id":"msg_011CefFz2kvnZJqsUbs7oqX4","type":"message","role":"assistant","content":[{"type":"thinking","thinking":"I'll check out `fivelime-updates-20260902` as a separate tracking branch since it diverges from local `5lime`, then install dependencies and launch it."}],"stop_reason":"tool_use","stop_sequence":null,"stop_details":null},"apiBlockIndex":1,"requestId":"req_011CefFyz3De44kH3jrZHT6L","type":"assistant","uuid":"61e7be9a-e113-4761-aaf3-ea8a2cc779cc","timestamp":"2026-09-02T20:40:18.024Z","effort":"high","session_id":"44ced9fd-a7fa-4761-8225-1bd1f24f6d51","userType":"external","entrypoint":"cli","cwd":"/home/admin/AI-OS/projects/aiterm","sessionId":"44ced9fd-a7fa-4761-8225-1bd1f24f6d51","version":"2.1.258","gitBranch":"master"}"#;

    const TEXT: &str = r#"{"parentUuid":"ddf9ca77-d435-45a6-be54-1b13026152cb","isSidechain":false,"message":{"model":"claude-fable-5-1","id":"msg_011CefFwY5xDrhoxhgvJhGs3","type":"message","role":"assistant","content":[{"type":"text","text":"I'll check my notes on the aiterm repo setup first, then pull and launch."}],"stop_reason":"tool_use","stop_sequence":null,"stop_details":null},"apiBlockIndex":1,"requestId":"req_011CefFwVVw91FMvNiD9pi4R","type":"assistant","uuid":"4146ce56-2bb9-4f8f-a388-e335e67825aa","timestamp":"2026-09-02T20:39:29.894Z","effort":"high","session_id":"44ced9fd-a7fa-4761-8225-1bd1f24f6d51","userType":"external","entrypoint":"cli","cwd":"/home/admin/AI-OS","sessionId":"44ced9fd-a7fa-4761-8225-1bd1f24f6d51","version":"2.1.258","gitBranch":"master"}"#;

    const TOOL_USE: &str = r#"{"parentUuid":"e73738b6-78fb-4c72-8e1c-d38bba600a5a","isSidechain":false,"message":{"model":"claude-fable-5-1","id":"msg_011CefM5XgT6QGv7TNECYWUn","type":"message","role":"assistant","content":[{"type":"tool_use","id":"toolu_01P9WUqCNazc41xjTi8AByQN","name":"Bash","input":{"command":"cd /home/admin/AI-OS/projects/aiterm; sed -n 200,275p src-tauri/src/pty.rs","description":"Read how the pty builds the spawn command"},"caller":{"type":"direct"}}],"stop_reason":"tool_use","stop_sequence":null,"stop_details":null},"apiBlockIndex":2,"requestId":"req_011CefM5U6eoY6eznxXyJ2df","type":"assistant","uuid":"eeca728f-ed01-47d9-b5ba-f9ea2a0063e5","timestamp":"2026-09-02T21:46:53.687Z","effort":"high","session_id":"44ced9fd-a7fa-4761-8225-1bd1f24f6d51","userType":"external","entrypoint":"cli","cwd":"/home/admin/AI-OS/projects/aiterm","sessionId":"44ced9fd-a7fa-4761-8225-1bd1f24f6d51","version":"2.1.258","gitBranch":"master"}"#;

    const TOOL_RESULT_OK: &str = r#"{"parentUuid":"f7b744db-6130-4a68-83b4-90d592752d15","isSidechain":false,"promptId":"3f1b1823-87d9-43db-ab07-a1acb1c8451f","type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_01TjdQcwTLExR4cd2gTqMEpN","type":"tool_result","content":"584712\nvite 200\n15","is_error":false}]},"uuid":"4fa524dc-0424-4e7d-bb68-7af05a63d916","timestamp":"2026-09-02T20:42:25.743Z","sourceToolAssistantUUID":"f7b744db-6130-4a68-83b4-90d592752d15","session_id":"44ced9fd-a7fa-4761-8225-1bd1f24f6d51","userType":"external","entrypoint":"cli","cwd":"/home/admin/AI-OS/projects/aiterm","sessionId":"44ced9fd-a7fa-4761-8225-1bd1f24f6d51","version":"2.1.258","gitBranch":"master"}"#;

    const TOOL_RESULT_ERR: &str = r#"{"parentUuid":"a0204fb5-3400-4b77-bb33-cefd80883ac4","isSidechain":false,"promptId":"be15bfff-b8fe-46aa-8ec9-a3d27949e8dc","type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"Exit code 255\nbuild exit=0\nadb: no devices/emulators found","is_error":true,"tool_use_id":"toolu_01AzFqXFbghMnRuf9A9ieswQ"}]},"uuid":"69fa5851-8e31-43f6-b93d-7d8004ed0388","timestamp":"2026-09-02T21:12:29.683Z","sourceToolAssistantUUID":"a0204fb5-3400-4b77-bb33-cefd80883ac4","session_id":"44ced9fd-a7fa-4761-8225-1bd1f24f6d51","userType":"external","entrypoint":"cli","cwd":"/home/admin/AI-OS/projects/aiterm","sessionId":"44ced9fd-a7fa-4761-8225-1bd1f24f6d51","version":"2.1.258","gitBranch":"master"}"#;

    /// An image's dimensions, injected by the harness so the model can read
    /// the screenshot. `isMeta:true` — nobody said it.
    const META: &str = r#"{"parentUuid":"309a87e6-558f-4b51-835c-e30779b49270","isSidechain":false,"promptId":"7045bff3-3ea3-41a8-b5d8-24c12f77c2d5","type":"user","message":{"role":"user","content":"[Image: original 1080x2404, displayed at 899x2000. Multiply "},"isMeta":true,"turnCompanion":true,"uuid":"69af5ca3-76d5-42ff-b8d9-26f0a0c12377","timestamp":"2026-09-02T20:55:43.647Z","session_id":"44ced9fd-a7fa-4761-8225-1bd1f24f6d51","userType":"external","entrypoint":"cli","cwd":"/home/admin/AI-OS/projects/aiterm/mobile","sessionId":"44ced9fd-a7fa-4761-8225-1bd1f24f6d51","version":"2.1.258","gitBranch":"master"}"#;

    /// The `USER` line with `isSidechain` flipped to true. No transcript on
    /// this machine carries one: 2.1.258 writes a subagent's conversation to
    /// its own file under `~/.claude/projects/-home-admin--claude-jobs-…`
    /// rather than inlining it. The flag is still honoured for the older
    /// transcripts that do inline it.
    const SIDECHAIN: &str = r#"{"parentUuid":"9cb4168e-294d-40ab-b75b-080caca45357","isSidechain":true,"promptId":"cfaf01d8-845b-4864-a224-673c7e732ae1","type":"user","message":{"role":"user","content":"plugged back in. install it and take the screenshot"},"uuid":"2d2da9bc-e53a-44ce-8c35-abda6dba6de3","timestamp":"2026-09-02T21:25:21.179Z","userType":"external","entrypoint":"cli","cwd":"/home/admin/AI-OS/projects/aiterm","sessionId":"44ced9fd-a7fa-4761-8225-1bd1f24f6d51","version":"2.1.258","gitBranch":"master"}"#;

    const SLASH: &str = r#"{"parentUuid":"cd89a7bb-b00c-4ad7-a7e4-d03931e0d6ca","isSidechain":false,"type":"user","message":{"role":"user","content":"<command-name>/login</command-name>\n            <command-message>login</command-message>\n            <command-args></command-args>"},"uuid":"9a796962-55c5-47cc-a818-96e426b9980e","timestamp":"2026-08-08T22:41:48.200Z","userType":"external","entrypoint":"cli","cwd":"/home/admin/AI-OS","sessionId":"0ceb22fb-1045-41da-b4d4-242c80ca5f98","version":"2.1.226","gitBranch":"master"}"#;

    const SLASH_STDOUT: &str = r#"{"parentUuid":"9a796962-55c5-47cc-a818-96e426b9980e","isSidechain":false,"type":"user","message":{"role":"user","content":"<local-command-stdout>Login successful</local-command-stdout>"},"uuid":"d1159183-7f17-4505-b643-b753d31f9f5a","timestamp":"2026-08-08T22:41:48.200Z","userType":"external","entrypoint":"cli","cwd":"/home/admin/AI-OS","sessionId":"0ceb22fb-1045-41da-b4d4-242c80ca5f98","version":"2.1.226","gitBranch":"master"}"#;

    const TURN_DURATION: &str = r#"{"parentUuid":"a072e6c6-3c2f-41f0-8c8c-03d8b2b94aa0","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":190654,"messageCount":70,"timestamp":"2026-09-02T20:42:37.670Z","uuid":"c4d366f9-4a05-473e-8568-ec21081616c7","isMeta":false,"userType":"external","entrypoint":"cli","cwd":"/home/admin/AI-OS/projects/aiterm","sessionId":"44ced9fd-a7fa-4761-8225-1bd1f24f6d51","version":"2.1.258","gitBranch":"master"}"#;

    const BOOKKEEPING: &str = r#"{"type":"bridge-session","sessionId":"44ced9fd-a7fa-4761-8225-1bd1f24f6d51","bridgeSessionId":"cse_01DMqvF6hLfA3KgFfxNjcyA5","lastSequenceNum":0}"#;

    const ATTACHMENT: &str = r#"{"type":"attachment","uuid":"9d70fd5a-62cb-4cf9-a687-fdc1aa75eb5c","timestamp":"2026-09-02T20:39:27.012Z","sessionId":"44ced9fd-a7fa-4761-8225-1bd1f24f6d51"}"#;

    /// An adapter over a scratch file, so a test can write the transcript the
    /// way Claude Code does — a line at a time, and sometimes half of one.
    fn adapter_over(path: &Path) -> ClaudeAdapter {
        ClaudeAdapter {
            path: path.to_path_buf(),
            offset: 0,
            pending: Vec::new(),
            ident: None,
            turn: None,
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aiterm-spine-claude-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("session.jsonl")
    }

    fn write(path: &Path, bytes: &str) {
        let mut f =
            std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
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
    fn a_typed_message_opens_a_turn_and_shows_what_was_said() {
        let out = kinds(&[USER]);
        assert_eq!(
            out,
            vec![
                Kind::TurnStarted { turn: "2d2da9bc-e53a-44ce-8c35-abda6dba6de3".into() },
                Kind::UserMessage {
                    id: "2d2da9bc-e53a-44ce-8c35-abda6dba6de3".into(),
                    text: "plugged back in. install it and take the screenshot".into(),
                },
            ]
        );
    }

    #[test]
    fn a_line_is_stamped_with_its_own_timestamp() {
        let mut a = adapter_over(Path::new("/nonexistent"));
        let mut out = Vec::new();
        a.read_line(USER.as_bytes(), &mut out);
        // 2026-09-02T21:25:21.179Z
        assert_eq!(out[0].0, 1_788_384_321_179);
    }

    #[test]
    fn thinking_and_text_are_whole_blocks_keyed_by_message_and_block() {
        assert_eq!(
            kinds(&[THINKING, TEXT]),
            vec![
                Kind::AgentThought {
                    id: "msg_011CefFz2kvnZJqsUbs7oqX4:1".into(),
                    text: "I'll check out `fivelime-updates-20260902` as a separate tracking branch since it diverges from local `5lime`, then install dependencies and launch it.".into(),
                    done: true,
                },
                Kind::AgentText {
                    id: "msg_011CefFwY5xDrhoxhgvJhGs3:1".into(),
                    text: "I'll check my notes on the aiterm repo setup first, then pull and launch.".into(),
                    done: true,
                },
            ]
        );
    }

    #[test]
    fn a_tool_use_is_running_the_moment_it_is_written() {
        assert_eq!(
            kinds(&[TOOL_USE]),
            vec![Kind::ToolCall {
                id: "toolu_01P9WUqCNazc41xjTi8AByQN".into(),
                tool: "Bash".into(),
                title: "cd /home/admin/AI-OS/projects/aiterm; sed -n 200,275p src-tauri/src/pty.rs"
                    .into(),
                category: ToolCategory::Execute,
                input: "cd /home/admin/AI-OS/projects/aiterm; sed -n 200,275p src-tauri/src/pty.rs\nRead how the pty builds the spawn command".into(),
                status: ToolStatus::Running,
            }]
        );
    }

    #[test]
    fn a_result_closes_the_call_it_names_and_carries_its_error() {
        assert_eq!(
            kinds(&[TOOL_RESULT_OK, TOOL_RESULT_ERR]),
            vec![
                Kind::ToolCallUpdate {
                    id: "toolu_01TjdQcwTLExR4cd2gTqMEpN".into(),
                    status: ToolStatus::Completed,
                    output: Some("584712\nvite 200\n15".into()),
                },
                Kind::ToolCallUpdate {
                    id: "toolu_01AzFqXFbghMnRuf9A9ieswQ".into(),
                    status: ToolStatus::Failed,
                    output: Some(
                        "Exit code 255\nbuild exit=0\nadb: no devices/emulators found".into()
                    ),
                },
            ]
        );
    }

    #[test]
    fn subagent_chatter_and_harness_injections_never_reach_the_phone() {
        assert!(kinds(&[SIDECHAIN, META]).is_empty());
    }

    #[test]
    fn a_slash_command_reads_as_the_command_and_its_output() {
        assert_eq!(
            kinds(&[SLASH, SLASH_STDOUT]),
            vec![
                Kind::TurnStarted { turn: "9a796962-55c5-47cc-a818-96e426b9980e".into() },
                Kind::UserMessage {
                    id: "9a796962-55c5-47cc-a818-96e426b9980e".into(),
                    text: "/login".into(),
                },
                // The output answers the command; it opens no turn of its own.
                Kind::UserMessage {
                    id: "d1159183-7f17-4505-b643-b753d31f9f5a".into(),
                    text: "Login successful".into(),
                },
            ]
        );
    }

    #[test]
    fn the_turn_ends_on_the_turn_it_started() {
        assert_eq!(
            kinds(&[USER, TEXT, TURN_DURATION]),
            vec![
                Kind::TurnStarted { turn: "2d2da9bc-e53a-44ce-8c35-abda6dba6de3".into() },
                Kind::UserMessage {
                    id: "2d2da9bc-e53a-44ce-8c35-abda6dba6de3".into(),
                    text: "plugged back in. install it and take the screenshot".into(),
                },
                Kind::AgentText {
                    id: "msg_011CefFwY5xDrhoxhgvJhGs3:1".into(),
                    text: "I'll check my notes on the aiterm repo setup first, then pull and launch.".into(),
                    done: true,
                },
                Kind::TurnEnded {
                    turn: "2d2da9bc-e53a-44ce-8c35-abda6dba6de3".into(),
                    reason: "completed".into(),
                },
            ]
        );
    }

    #[test]
    fn bookkeeping_lines_say_nothing() {
        assert!(kinds(&[BOOKKEEPING, ATTACHMENT, "", "not json at all"]).is_empty());
    }

    #[test]
    fn half_a_line_waits_for_its_newline() {
        let path = scratch("partial");
        let (head, tail) = USER.split_at(120);
        write(&path, head);
        let mut a = adapter_over(&path);
        assert!(a.poll().is_empty(), "an unterminated line is not a line yet");
        write(&path, tail);
        assert!(a.poll().is_empty(), "still no newline");
        write(&path, "\n");
        assert_eq!(a.poll().len(), 2, "turn_started + user_message once the line closes");
        assert!(a.poll().is_empty(), "and nothing on a poll with nothing new");
    }

    #[test]
    fn a_cleared_transcript_resets_before_it_replays() {
        let path = scratch("reset");
        write(&path, &format!("{USER}\n{TEXT}\n"));
        let mut a = adapter_over(&path);
        assert_eq!(a.bootstrap().len(), 3);

        // What `/clear` leaves behind: the same name, a shorter file.
        std::fs::write(&path, format!("{USER}\n")).unwrap();
        let out = a.poll();
        assert_eq!(out.first().map(|(_, k)| k), Some(&Kind::Reset));
        assert_eq!(out.len(), 3, "reset, then the new file's history");
    }

    #[test]
    fn bootstrap_reads_the_whole_file_and_poll_reads_only_the_rest() {
        let path = scratch("bootstrap");
        write(&path, &format!("{USER}\n{THINKING}\n{TOOL_USE}\n"));
        let mut a = adapter_over(&path);
        assert_eq!(a.bootstrap().len(), 4);
        assert!(a.poll().is_empty());
        write(&path, &format!("{TOOL_RESULT_OK}\n"));
        assert_eq!(a.poll().len(), 1);
    }

    #[test]
    fn a_missing_file_is_an_empty_session_that_fills_in_later() {
        let path = scratch("late");
        let mut a = adapter_over(&path);
        assert!(a.bootstrap().is_empty());
        assert_eq!(a.watch_paths().len(), 2, "the file and the folder it will appear in");
        write(&path, &format!("{USER}\n"));
        assert_eq!(a.poll().len(), 2);
    }

    #[test]
    fn a_title_is_the_part_of_the_input_a_person_reads() {
        let input = serde_json::json!({"file_path": "/home/admin/AI-OS/notes.md"});
        assert_eq!(title("Read", Some(&input)), "/home/admin/AI-OS/notes.md");
        assert_eq!(category("Read"), ToolCategory::Read);
        let input = serde_json::json!({"command": "make test\nmake lint"});
        assert_eq!(title("Bash", Some(&input)), "make test");
        let input = serde_json::json!({"description": "Survey the adapters"});
        assert_eq!(title("Agent", Some(&input)), "Survey the adapters");
        assert_eq!(category("Agent"), ToolCategory::Think);
        // An MCP tool has no field we can guess at: it wears its own name.
        assert_eq!(title("mcp__claude-in-chrome__computer", None), "mcp__claude-in-chrome__computer");
        assert_eq!(category("mcp__claude-in-chrome__computer"), ToolCategory::Other);
    }

    /// The real thing: resolve the busiest transcript on this machine through
    /// the session registry, bootstrap it, and check the tail is quiet
    /// afterwards. Ignored because it depends on files that only exist where
    /// Claude Code has run.
    ///
    /// `cargo test --lib spine::claude::tests::a_real_transcript -- --ignored --nocapture`,
    /// or `AITERM_SESSION=<id>` to name the session instead of taking the
    /// biggest one.
    #[test]
    #[ignore]
    fn a_real_transcript_bootstraps_once_and_then_says_nothing() {
        let id = std::env::var("AITERM_SESSION").unwrap_or_else(|_| {
            let p = busiest_transcript().expect("a transcript under ~/.claude/projects");
            p.file_stem().unwrap().to_string_lossy().into_owned()
        });
        let path = crate::agents::owner_in(&crate::agents::backends(), &id)
            .expect("a session with that id")
            .1;
        let mut a = open(&id).expect("the registry finds a claude session by its id");
        assert_eq!(a.path, path, "and hands back the file the id names");

        let lines = std::fs::read_to_string(&path).unwrap().lines().count();
        let started = std::time::Instant::now();
        let events = a.bootstrap();
        let took = started.elapsed();
        let mut tally: std::collections::BTreeMap<&str, usize> = Default::default();
        for (_, k) in &events {
            *tally.entry(kind_name(k)).or_default() += 1;
        }
        println!(
            "{}: {lines} lines → {} events in {} ms {tally:?}",
            path.display(),
            events.len(),
            took.as_millis()
        );
        assert!(!events.is_empty());
        assert!(a.poll().is_empty(), "a second poll on a file nobody wrote to is empty");
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
