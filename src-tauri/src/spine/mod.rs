//! The spine: one live event stream for every harness.
//!
//! Every engine feeds it through an [`Adapter`]; every consumer (the phone
//! first) reads one vocabulary. See `docs/architecture/spine.md` for the contract. The
//! types and the trait here ARE the contract — change them only by
//! agreement, everything else is free.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod grok;
pub mod ipc;
pub mod legacy;
pub mod registry;

pub use registry::{ensure_tail_for, push_phase, read_after, resolve_agent, Spine};

/// What kind of thing a tool call is, so a card can wear the right mark
/// without knowing the engine's tool names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCategory {
    Read,
    Edit,
    Execute,
    Search,
    Fetch,
    Think,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Working,
    NeedsYou,
    Idle,
}

/// One thing that happened in a session. `text` on the text kinds is the
/// FULL text of that block so far — a snapshot, never a delta — so a
/// consumer that misses one event is healed by the next.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Kind {
    UserMessage { id: String, text: String },
    AgentText { id: String, text: String, done: bool },
    AgentThought { id: String, text: String, done: bool },
    ToolCall {
        id: String,
        tool: String,
        title: String,
        category: ToolCategory,
        input: String,
        status: ToolStatus,
    },
    ToolCallUpdate {
        id: String,
        status: ToolStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
    TurnStarted { turn: String },
    TurnEnded { turn: String, reason: String },
    Phase { phase: Phase, detail: String },
    Reset,
}

/// An event as it goes over the wire: the registry stamps `seq`, `epoch`,
/// `session_id` and `agent`; the adapter supplies `ts` and `kind`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpineEvent {
    pub seq: u64,
    pub epoch: u64,
    pub session_id: String,
    pub agent: String,
    pub ts: u64,
    #[serde(flatten)]
    pub kind: Kind,
}

/// An engine's feed. `bootstrap` once, then `poll` on every change of a
/// watched file and on a slow fallback tick. Both return `(ts_ms, kind)`
/// in order. A source replaced underneath (a `/clear`) yields a `Reset`
/// followed by the rebuilt history.
pub trait Adapter: Send {
    fn bootstrap(&mut self) -> Vec<(u64, Kind)>;
    fn poll(&mut self) -> Vec<(u64, Kind)>;
    fn watch_paths(&self) -> Vec<PathBuf>;
}

/// The adapter for a session: the engine's own where one exists, the
/// legacy one (conversation_rich on a slow poll) for everything else.
pub fn open_adapter(agent: &str, session_id: &str) -> Option<Box<dyn Adapter>> {
    match agent {
        "claude" => claude::open(session_id).map(|a| Box::new(a) as Box<dyn Adapter>),
        "grok" => grok::open(session_id).map(|a| Box::new(a) as Box<dyn Adapter>),
        "codex" => codex::open(session_id).map(|a| Box::new(a) as Box<dyn Adapter>),
        "antigravity" => antigravity::open(session_id).map(|a| Box::new(a) as Box<dyn Adapter>),
        _ => legacy::open(agent, session_id).map(|a| Box::new(a) as Box<dyn Adapter>),
    }
}

/// Whether `agent` has an adapter that reads the engine's own live source
/// (as opposed to the legacy re-derivation). Reported as `live` to phones.
pub fn is_native(agent: &str) -> bool {
    matches!(agent, "claude" | "grok" | "codex" | "antigravity")
}

/// Milliseconds since the epoch, for stamping.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Clip text for the wire: tool inputs and outputs can be megabytes and
/// none of it needs to cross to a phone whole.
pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_shape_is_flat_with_a_kind_tag() {
        let ev = SpineEvent {
            seq: 7,
            epoch: 1,
            session_id: "s".into(),
            agent: "claude".into(),
            ts: 2,
            kind: Kind::ToolCallUpdate { id: "t1".into(), status: ToolStatus::Completed, output: None },
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["kind"], "tool_call_update");
        assert_eq!(json["status"], "completed");
        assert_eq!(json["seq"], 7);
        assert!(json.get("output").is_none());
        let back: SpineEvent = serde_json::from_value(json).unwrap();
        assert_eq!(back, ev);
    }
}
