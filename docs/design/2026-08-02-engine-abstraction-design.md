# Engine abstraction: backends own their lifecycle, the GUI owns none of it

Date: 2026-08-02
Status: implemented in 0.10.25, all four legs

One thing below was wrong when it met the code: the `Clear` rule said "same
ownership lookup, then `backend.clear(id)`", which hands back the id being
parked. ✦ must start a conversation that is *not* the one it parked, so the
resolver finds the owner by the given id and then mints a fresh one. See
`a_clear_starts_a_conversation_that_is_not_the_one_it_parked` in `launch.rs`.

## The problem

aiterm speaks to four engines — Claude Code, Codex, OpenCode, and its own
`aiterm chat` console — and knows how to work with each of them in a different
place. `agents.rs` has an `AgentBackend` trait that covers detection, model
lists, session stores and launch syntax. Everything *after* launch is scattered,
and most of what is scattered ended up in the renderer.

The module doc in `agents.rs` already lists the gap honestly: liveness,
lifecycle, panels and trash are all still hard-wired to Claude Code. Adding
OpenRouter support in the 0.10.17–0.10.24 run is what made the cost concrete —
one engine took edits in `App.tsx`, `SessionsPanel.tsx`, `ipc.ts`,
`StartControls.tsx`, and three new IPC commands, none of which a fifth engine
would be able to reuse.

Where the knowledge leaked to, specifically:

- **Engine routing lives in `App.tsx`.** `newSession` (line 1228) decides
  OpenCode-versus-chat from `choice.api.openrouter && opencodeAvail.current`.
  `resumeSession` (1005) special-cases `s.agent === "api"`. `restartEnded`
  (958) and `clearSession` (1120) build claude flag strings by concatenation.
  `projectClaude` (1184) bypasses the choice machinery entirely.
- **Command strings are built in TSX.** `CLAUDE_CMD` (App.tsx:71),
  `` `${claudeCmdRef.current} --resume ${liveId}` `` (1093), the
  `` `openrouter/${modelId}` `` slug (1241), and client-side
  `crypto.randomUUID()` session minting (1127, 1247, 1256, 1270).
- **UI affordances gate on engine names.** `SessionsPanel` hides ⑂ for `"api"`
  (751) and shows ✦ only for `"claude"` (762); `AgentIcon` switches on three
  names; `SettingsModal` has a Codex-only paragraph.
- **Claude-shaped subsystems run unconditionally.** The 250 ms screen poll
  (`App.tsx:251-298` into `term/screen.ts`), the composer pills that send
  `/model` `/effort` `/rewind`, and `AgentPanel` all run against every tab
  regardless of engine. They only behave today because claude is the sole
  engine that mints session ids end to end — `AgentPanel`'s own prop doc admits
  the assumption without enforcing it.
- **`"api"` is not an engine at all.** It is a special case stitched into
  session scanning (`agents.rs:579`, `sessions.rs:1384`) with its own two IPC
  commands, sitting beside a trait that exists to make exactly that unnecessary.

None of this is tested, because engine routing lives in TSX branching.

## The design

Two concepts.

**A backend is an engine.** It owns its command syntax, its session store, and
a declaration of what it supports. `AgentBackend` already exists; it grows
lifecycle and capabilities.

**A resolver turns intent into a plan.** The GUI holds user intent ("start this
model here", "resume this session") and receives a plan. It never learns which
engine answered.

```rust
enum LaunchRequest {
    Agent   { agent_id: String, model: Option<String>, effort: Option<String> },
    ApiModel { provider_id: String, model_id: String },
    Resume  { session_id: String },
    Restart { session_id: String },
    Clear   { session_id: String },
}

struct LaunchPlan {
    command: String,
    /// Provider id whose key `pty_spawn` injects into the tab environment.
    env_provider: Option<String>,
    /// `Some` = a real session id panels may key to. `None` = tab handle only.
    session_id: Option<String>,
    agent_id: String,
    caps: Caps,
}
```

Rust mints the session id, because only the backend knows whether an id will
mean anything. That single move retires the "a `sessionId` exists, therefore
this is claude" inference that recurs in `AgentPanel`, the Tasks/Artifacts/
Agents pills, `restartEnded`, and the ended-tab panel.

### Capabilities replace name checks

```rust
struct Caps {
    fork: bool,       // ⑂ in the sidebar
    clear: bool,      // ✦ re-key
    resume: bool,     // ▶
    tui_drive: bool,  // term/screen.ts poll + the Tui* dialogs
    panels: bool,     // tasks/artifacts/agents + /model /effort /rewind pills
}
```

`Caps` rides on `Detection`, which the frontend already fetches once on mount,
so `SessionsPanel` asks `caps[s.agent]?.fork` instead of comparing to `"api"`.

`Detection` covers every registered backend regardless of availability, so rows
belonging to an uninstalled engine still resolve. An id with no backend at all —
an index row from an engine since removed — gets `Caps::default()`, all false:
a row with no buttons is a better failure than a row offering claude's.

`tui_drive` and `panels` stay separate. One gates screen parsing, the other
gates transcript reading; an engine could have either without the other, and
today both run against every tab.

Per-engine values:

| Engine | fork | clear | resume | tui_drive | panels |
|---|---|---|---|---|---|
| claude | yes | yes | yes | yes | yes |
| codex | no | no | no¹ | no | no |
| opencode | no | no | no² | no | no |
| api (`aiterm chat`) | no | no | yes | no | no |

¹ Codex resume is parked with the diagnosis done; flipping this flag is the
whole of the UI work when it is picked back up.
² Until an OpenCode session reader exists — its sessions live in a SQLite
`opencode.db` and `NoSessions` reports nothing rather than guessing.

### `aiterm chat` becomes a real backend

`ChatBackend` keeps the id `"api"`. The id is written onto every session row
already in the index, and the trait doc is explicit that changing it orphans
them. It gains a normal `SessionProvider` wrapping the existing
`chat::scan_chats` and `chat::chat_file_if_exists`, which deletes the two
special-cases at `agents.rs:579` and `sessions.rs:1384`. It reports
`offered() == false` (reached through the model dropdown, not the ＋ menu) and
`mints_session_id() == true`.

`LaunchSpec` grows `provider: Option<String>` so a launch can carry an API
provider — the honest shape of what a chat launch needs.

### Choosing an engine for an API model

This is the routing decision currently sitting in `App.tsx:1239`. It becomes a
question each backend answers about itself:

```rust
fn accepts_api(&self, _provider: &Provider) -> bool { false }
```

The resolver walks `backends()` in order and takes the first that is both
available and willing. `OpenCodeBackend` accepts OpenRouter providers when
installed; `ChatBackend` accepts anything and sits last as the fallback. Roughly
fifteen lines, and it is the pluggable seam: a new engine inserts itself by
answering, with no edits at any call site.

### What deliberately stays engine-private

Not trait methods, on purpose:

- the `SessionStart` hook-link settings flag,
- `~/.claude/sessions/<pid>.json` registry reads and the roster,
- the `/clear` re-key and the resume-steal guard,
- `~/.claude/projects` trash and restore layout.

These describe one engine's internals. Promoting them would make three backends
stub methods that mean nothing to them, which is the failure mode the existing
module doc argues against — a trait that pretends to abstract what it does not.
The capability flags exist so unevenness is declared rather than faked.

## IPC changes

Collapsed into one surface:

- `agent_launch_command`, `api_launch_command`, `chat_resume_command`
  → `resolve_launch(request: LaunchRequest) -> LaunchPlan`

Kept: `agent_choices`, `detect_agents` (now carrying `caps`), and every
provider command.

Left alone: `claude_permission_mode`, `claude_model_default`,
`restore_claude_model_default`. The names bake in an engine, but they are read
by the TUI-drive subsystem only, which `tui_drive` now gates. Renaming is churn
without behaviour change.

## Frontend changes

`StartChoice` becomes a discriminated union, and `mintsSessionId` leaves the
frontend entirely — the plan reports whether the id is real:

```ts
type StartChoice =
  | { kind: "agent"; agentId: string; model: string | null; effort: string | null }
  | { kind: "api"; providerId: string; modelId: string };
```

Every command-building site in `App.tsx` collapses to `plan = await
resolveLaunch(...)` followed by `openTab(plan)`. Deleted outright: `CLAUDE_CMD`,
the `opencodeAvail` ref, the `openrouter/` concatenation, and client-side UUID
minting.

`AgentIcon`'s name→SVG map stays. It is presentation, like a theme; gating it
buys nothing and a fallback glyph already exists.

## Migration

Four legs. Each lands green; nothing is broken between them.

1. **Rust, additive.** `Caps`, `resume`/`clear`/`accepts_api` on the trait,
   `ChatBackend`, the resolver, `resolve_launch`. Old commands stay registered.
2. **`App.tsx` switches over.** `newSession`, `resumeSession`, `restartEnded`,
   `clearSession`, `projectClaude` all route through `resolveLaunch`.
3. **Capability gating.** `SessionsPanel`, composer pills, `AgentPanel`, the
   screen poll, `StartChoice` union.
4. **Cleanup.** Drop the dead IPC commands and the chat special-cases in
   `sessions.rs` and `agents.rs`.

Legs 1 and 2 carry the value. Legs 3 and 4 can slip to a later sitting without
leaving anything half-done.

## Testing

The resolver is a pure function from request to plan, so the routing that is
currently untestable TSX branching becomes assertions:

- an `Agent` request reaches the named backend's `launch`;
- an `ApiModel` request on an OpenRouter provider picks OpenCode when it is
  available and `ChatBackend` when it is not;
- an `ApiModel` request on a non-OpenRouter provider always picks
  `ChatBackend`;
- `Resume` on an `"api"` session yields `aiterm chat --resume <id>`;
- `Resume` on a backend whose `caps.resume` is false yields no plan;
- a minted id appears in the plan only where `mints_session_id()` is true;
- `env_provider` is set for an OpenCode-on-OpenRouter plan and absent for
  claude.

Fake backends already exist for `scan_backends` tests; the resolver tests reuse
that approach rather than depending on what is installed on the machine.

## Risks

- **The private list is the judgment call most likely to age.** If a second
  engine turns out to have a hook-link equivalent, that item graduates to the
  trait. Nothing in this design blocks that; it just refuses to guess now.
- **`"api"` as an id is now load-bearing in two ways** — index rows and the
  backend registry. Documented at the `ChatBackend` impl.
- **Leg 3 touches the claude TUI subsystem**, the least-covered code in the
  app. Gating is additive (`if caps.tui_drive`), so the failure mode is a
  feature not running rather than one misfiring.
