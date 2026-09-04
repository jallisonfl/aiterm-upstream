//! Legacy adapter: any engine without a native feed yet. Re-derives the
//! conversation from `detail::conversation_rich` and diffs it into events.
//!
//! Owned by the spine-core task. See `docs/architecture/spine.md`.
//!
//! `conversation_rich` hands back `(role, text)` turns where the role is
//! "user", "assistant", "thinking", or — for a tool call — the tool's own
//! name (see `line_events` in detail.rs). That is the whole vocabulary this
//! has to work with: no ids, no timestamps, no streaming. So ids are the
//! turn's ordinal, `done` is always true, and a growing block is re-emitted
//! under the same id rather than appended.

use super::{clip, now_ms, Adapter, Kind, ToolCategory, ToolStatus};
use std::path::PathBuf;

/// What a phone renders of a session at once. Matches the budget the
/// conversation endpoint uses, so the two never disagree about where a long
/// session is elided.
const MAX_CHARS: usize = 60_000;

/// A tool's input on a card, not in full. The turn text is already capped at
/// 600 chars by `detail::cap`; this is the phone-sized share of that.
const INPUT_CLIP: usize = 400;

pub struct LegacyAdapter {
    session_id: String,
    /// The transcript, when the backend keeps one. Empty for opencode,
    /// which answers from a database — the fallback tick covers it.
    paths: Vec<PathBuf>,
    /// The turns the last read produced, so the next one can be diffed
    /// against them instead of re-emitting the whole conversation.
    seen: Vec<(String, String)>,
}

pub fn open(agent: &str, session_id: &str) -> Option<LegacyAdapter> {
    // The transcript path, when this backend has one on disk. Resolved once
    // here rather than per poll: it walks every backend's session directory.
    let list = crate::agents::backends();
    let paths = crate::agents::owner_in(&list, session_id)
        .map(|(_, p)| p)
        .filter(|p| p.is_file())
        .into_iter()
        .collect::<Vec<_>>();
    if paths.is_empty() {
        crate::diag!("spine", "legacy adapter for {agent} has no file to watch; polling only");
    }
    Some(LegacyAdapter { session_id: session_id.to_string(), paths, seen: Vec::new() })
}

impl Adapter for LegacyAdapter {
    fn bootstrap(&mut self) -> Vec<(u64, Kind)> {
        self.seen = self.read();
        map_turns(&self.seen, 0)
    }

    fn poll(&mut self) -> Vec<(u64, Kind)> {
        let turns = self.read();
        diff(&mut self.seen, turns)
    }

    fn watch_paths(&self) -> Vec<PathBuf> {
        self.paths.clone()
    }
}

impl LegacyAdapter {
    fn read(&self) -> Vec<(String, String)> {
        crate::detail::conversation_rich_service(&self.session_id, MAX_CHARS)
    }
}

/// What changed between the turns we last saw and the ones we just read.
///
/// The last turn we saw is not treated as settled: `conversation_rich`
/// merges consecutive same-role turns, so the assistant block at the end
/// grows with every append. Comparing it as part of the stable prefix would
/// declare a rebuild on every poll of a working session. It is re-emitted
/// under its own id instead, which upserts on the phone.
fn diff(seen: &mut Vec<(String, String)>, turns: Vec<(String, String)>) -> Vec<(u64, Kind)> {
    let stable = seen.len().saturating_sub(1);
    let rebuilt = turns.len() < seen.len() || turns[..stable] != seen[..stable];
    if rebuilt {
        // Shorter, or a settled turn changed underneath: a `/clear`, a fork,
        // or the length elision kicking in. Everything the phone holds for
        // this session is wrong.
        *seen = turns;
        let mut out = vec![(now_ms(), Kind::Reset)];
        out.extend(map_turns(seen, 0));
        return out;
    }
    let mut out = Vec::new();
    for (i, turn) in turns.iter().enumerate().skip(stable) {
        if seen.get(i) == Some(turn) {
            continue; // the tail turn did not move
        }
        out.push(map_turn(i, turn));
    }
    *seen = turns;
    out
}

fn map_turns(turns: &[(String, String)], from: usize) -> Vec<(u64, Kind)> {
    turns.iter().enumerate().skip(from).map(|(i, t)| map_turn(i, t)).collect()
}

/// One turn → one event. There are no timestamps on this path, so `ts` is
/// when it was read; the ordinal is what makes an id stable across re-reads.
fn map_turn(index: usize, (role, text): &(String, String)) -> (u64, Kind) {
    let id = format!("legacy:{index}");
    let kind = match role.as_str() {
        "user" => Kind::UserMessage { id, text: text.clone() },
        "assistant" => Kind::AgentText { id, text: text.clone(), done: true },
        "thinking" => Kind::AgentThought { id, text: text.clone(), done: true },
        // The only "system" turn this path produces is the elision marker
        // `conversation_rich` inserts when a conversation is over budget.
        // It is prose about the conversation, not a tool that ran.
        "system" => Kind::AgentText { id, text: text.clone(), done: true },
        // Anything else IS the tool's name — that is how `line_events`
        // encodes a tool call. The result is not in this stream, so the
        // call is reported already finished.
        tool => Kind::ToolCall {
            id,
            tool: tool.to_string(),
            title: tool.to_string(),
            category: category_of(tool),
            input: clip(text, INPUT_CLIP),
            status: ToolStatus::Completed,
        },
    };
    (now_ms(), kind)
}

/// The card's mark, guessed from the tool's name. Every engine names its
/// tools differently and none of them declare a category, so this is the
/// only signal there is. Search is tested before fetch so `WebSearch` is a
/// search rather than a fetch.
fn category_of(tool: &str) -> ToolCategory {
    let t = tool.to_ascii_lowercase();
    let has = |k: &str| t.contains(k);
    if has("bash") || has("exec") || has("shell") || has("terminal") || has("command") {
        ToolCategory::Execute
    } else if has("read") || t == "cat" || has("notebook") || has("view") {
        ToolCategory::Read
    } else if has("edit") || has("write") || has("patch") || has("apply") {
        ToolCategory::Edit
    } else if has("grep") || has("glob") || has("search") || has("find") {
        ToolCategory::Search
    } else if has("fetch") || has("web") || has("http") || has("url") {
        ToolCategory::Fetch
    } else {
        ToolCategory::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turns(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items.iter().map(|(r, t)| (r.to_string(), t.to_string())).collect()
    }

    fn kinds(evs: Vec<(u64, Kind)>) -> Vec<Kind> {
        evs.into_iter().map(|(_, k)| k).collect()
    }

    #[test]
    fn roles_become_the_right_kinds_with_ordinal_ids() {
        let got = kinds(map_turns(
            &turns(&[
                ("user", "fix the build"),
                ("thinking", "cargo first"),
                ("Bash", "cargo build"),
                ("assistant", "done"),
            ]),
            0,
        ));
        assert_eq!(got[0], Kind::UserMessage { id: "legacy:0".into(), text: "fix the build".into() });
        assert_eq!(
            got[1],
            Kind::AgentThought { id: "legacy:1".into(), text: "cargo first".into(), done: true }
        );
        assert_eq!(
            got[2],
            Kind::ToolCall {
                id: "legacy:2".into(),
                tool: "Bash".into(),
                title: "Bash".into(),
                category: ToolCategory::Execute,
                input: "cargo build".into(),
                status: ToolStatus::Completed,
            }
        );
        assert_eq!(
            got[3],
            Kind::AgentText { id: "legacy:3".into(), text: "done".into(), done: true }
        );
    }

    #[test]
    fn tool_names_land_in_a_category() {
        assert_eq!(category_of("Read"), ToolCategory::Read);
        assert_eq!(category_of("cat"), ToolCategory::Read);
        assert_eq!(category_of("Edit"), ToolCategory::Edit);
        assert_eq!(category_of("Write"), ToolCategory::Edit);
        assert_eq!(category_of("apply_patch"), ToolCategory::Edit);
        assert_eq!(category_of("Grep"), ToolCategory::Search);
        assert_eq!(category_of("Glob"), ToolCategory::Search);
        assert_eq!(category_of("WebSearch"), ToolCategory::Search);
        assert_eq!(category_of("WebFetch"), ToolCategory::Fetch);
        assert_eq!(category_of("exec_command"), ToolCategory::Execute);
        assert_eq!(category_of("TodoWrite"), ToolCategory::Edit);
        assert_eq!(category_of("Task"), ToolCategory::Other);
    }

    #[test]
    fn a_diff_emits_only_what_is_new() {
        let mut seen = turns(&[("user", "hi"), ("assistant", "hel")]);
        let got = kinds(diff(&mut seen, turns(&[("user", "hi"), ("assistant", "hello there")])));
        // The tail block grew: re-emitted under the same id, nothing else.
        assert_eq!(
            got,
            vec![Kind::AgentText {
                id: "legacy:1".into(),
                text: "hello there".into(),
                done: true
            }]
        );

        // Nothing moved at all.
        assert!(diff(&mut seen, turns(&[("user", "hi"), ("assistant", "hello there")])).is_empty());

        // A new turn arrives; the settled ones are not repeated.
        let got = kinds(diff(
            &mut seen,
            turns(&[("user", "hi"), ("assistant", "hello there"), ("Read", "src/lib.rs")]),
        ));
        assert_eq!(got.len(), 1);
        assert!(matches!(&got[0], Kind::ToolCall { id, .. } if id == "legacy:2"));
    }

    #[test]
    fn a_shorter_list_resets_and_replays() {
        let mut seen = turns(&[("user", "hi"), ("assistant", "hello"), ("user", "again")]);
        let got = kinds(diff(&mut seen, turns(&[("user", "fresh start")])));
        assert_eq!(got[0], Kind::Reset);
        assert_eq!(
            got[1],
            Kind::UserMessage { id: "legacy:0".into(), text: "fresh start".into() }
        );
        assert_eq!(got.len(), 2);
        assert_eq!(seen.len(), 1);
    }

    #[test]
    fn a_changed_settled_turn_resets_too() {
        // Elision: the conversation went over budget, so turn 1 became the
        // "earlier turns omitted" marker. Same length, different history.
        let mut seen = turns(&[("user", "hi"), ("assistant", "a"), ("assistant", "b")]);
        let got = kinds(diff(
            &mut seen,
            turns(&[("user", "hi"), ("system", "[… earlier turns omitted …]"), ("assistant", "b")]),
        ));
        assert_eq!(got[0], Kind::Reset);
        assert_eq!(got.len(), 4);
        // The marker reads as prose, not as a tool named "system".
        assert!(matches!(got[2], Kind::AgentText { .. }));
    }

    #[test]
    fn an_empty_history_stays_empty() {
        let mut seen: Vec<(String, String)> = Vec::new();
        assert!(diff(&mut seen, Vec::new()).is_empty());
        let got = kinds(diff(&mut seen, turns(&[("user", "first")])));
        assert_eq!(got.len(), 1);
    }
}
