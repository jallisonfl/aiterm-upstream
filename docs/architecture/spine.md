# The spine — one live event stream for every harness

The spine is how a conversation reaches the phone (and, later, anything
else that wants to watch a session) as it happens. Every engine feeds it
through an adapter; every consumer reads one vocabulary. Adding a harness
means writing one adapter. Nothing above it changes.

This replaces the phone's poll of `GET /v1/sessions/{id}/conversation`
every 3 s, which re-parsed the whole transcript and swapped the whole list.

## Vocabulary

ACP-shaped (agentclientprotocol.com `session/update`), trimmed to what the
phone renders. Rust types are the source of truth: `src-tauri/src/spine/mod.rs`.

Every event on the wire:

```json
{
  "seq": 42,                 // per session, from 1, assigned by the registry
  "epoch": 1788390000123,    // registry start (ms). Changes when the desktop restarts.
  "session_id": "…",
  "agent": "claude",         // the engine id
  "ts": 1788390012345,       // ms; the source's own timestamp when it has one
  "kind": "agent_text",      // one of the kinds below
  … kind fields …
}
```

Kinds:

| kind | fields | semantics |
|---|---|---|
| `user_message` | `id`, `text` | The person (or a relay) said something. |
| `agent_text` | `id`, `text`, `done` | Assistant prose. `text` is the FULL text of this block so far, never a delta. Upsert by `id`. `done:false` means more may come for this id. |
| `agent_thought` | `id`, `text`, `done` | Reasoning, same rules as `agent_text`. |
| `tool_call` | `id`, `tool`, `title`, `category`, `input`, `status` | A tool was invoked. Appears the moment the call is issued. `input` is a one-line summary, clipped. |
| `tool_call_update` | `id`, `status`, `output` | Status moved; `output` (clipped, optional) is the result when there is one. Upsert by `id`. An absent `output` means "no change" — never "clear it"; the consumer keeps whatever it already holds for that id. |
| `turn_started` | `turn` | A turn opened (a person spoke; the engine is going to answer). |
| `turn_ended` | `turn`, `reason` | `completed` \| `interrupted` \| `error` \| `unknown`. |
| `phase` | `phase`, `detail` | `working` \| `needs_you` \| `idle`. Status, not content. `detail` is human text ("running Bash", "permission: Edit foo.rs"). |
| `reset` | — | History was rebuilt (a `/clear`, a file replaced). The phone drops everything it holds for this session and fetches from `after=0`. It carries `ts = now`: there is no source record behind it to take a time from. |

`category` ∈ `read` \| `edit` \| `execute` \| `search` \| `fetch` \| `think` \| `other`.
`status` ∈ `pending` \| `running` \| `completed` \| `failed` \| `cancelled`.

Ids are stable across re-reads of the same source. A consumer that sees an
id twice replaces, never appends. What each engine builds one from, as the
adapters found them in real files:

- **Claude** — the API `message.id` plus `apiBlockIndex`, which together
  name one content block of one assistant message. Transcripts written
  before Claude Code 2.1.252 have no `apiBlockIndex`; those fall back to
  the line's own `uuid`.
- **Grok** — the line ordinal of the FIRST chunk of a run, so every chunk
  that folds into one block shares the block's id. Tool calls use the
  engine's own `toolCallId` instead.
- **The legacy adapter** — the turn's ordinal, which is all a re-derived
  conversation has.

A `tool_call` re-issued under an id that already exists is not a mistake:
it is how a card gets filled in. Grok writes the call first and the human
`title` and kind on a second line, so the second `tool_call` carries the
better text. Merge it like any other upsert, and keep any `output` already
held for that id — a re-issued call never carries one.

## Endpoints

- `GET /v1/sessions/{id}/spine?after=N` →
  `{ "epoch": u64, "live": bool, "events": [ SpineEvent… ] }`
  Everything after `seq` N (N=0 for all). Calling it registers interest:
  the registry starts (or keeps) the adapter tail for that session.
  `live` is false when the session is served by the legacy adapter
  (an engine with no native adapter yet: the conversation is re-derived
  from `conversation_rich` on a slow poll and diffed into events).
  When the call is what STARTS the tail it waits up to 2 s for the
  adapter's `bootstrap()` to land before answering, so a phone opening a
  session gets its history in that one request rather than a blank screen
  and a wait for the next event. A tail that is already running answers
  immediately; an agent with no adapter at all answers at once with
  `live:false` and no events.
- The existing WebSocket `/v1/events` now also carries
  `{"type":"spine", …SpineEvent fields flattened…}` for every session with
  a running tail. The phone ignores sessions it is not looking at.

## Client rule (the phone)

1. On opening a session: `GET …/spine?after=0`, apply all, remember `epoch`
   and `lastSeq`.
2. On a `spine` WS event for that session: if `epoch` differs → go to 1.
   If `seq == lastSeq + 1` → apply. Otherwise → `GET …/spine?after=lastSeq`
   and apply what comes back (dedupe by `seq`).
3. Apply = upsert by `id` for every kind that carries one, `user_message`
   included — Grok folds a pasted image and its caption into one message
   across two chunks, so the same id arrives twice with more text the
   second time. Append only when the id is new. `reset` → clear and go to 1.
4. Never rebuild the list from scratch on a normal event. Rows are keyed by
   id so a growing block re-renders in place.
5. On WS reconnect: `GET …/spine?after=lastSeq`.

## Registry lifecycle (desktop)

- `Spine` lives in Tauri managed state. One `SessionLog` per session:
  bounded ring (last 5 000 events or 4 MB), `next_seq`, adapter handle,
  `last_interest`.
- A tail starts when: a phone asks (`GET …/spine`), or a tab binds the
  session (`TabRegistry`), or a relay starts on it.
- A tail stops when: no tab is bound AND no interest for 15 minutes.
- Adapter driver: `bootstrap()` once (history → events, seq assigned in
  order), then `poll()` on every change of any `watch_paths()` file
  (notify, 250 ms coalesce) and on a 1 s fallback tick. The notify watch
  is on the parent DIRECTORY of each watched path, not the file: a
  `/clear` replaces the transcript, and a watch on the old inode goes
  quiet.
- `open_adapter` is retried until it succeeds. A session launched a moment
  ago has an id and a bound tab before its engine has written anything,
  and every adapter's `open` resolves through files on disk — so the first
  try fails and there is no path to watch yet either. The driver retries on
  its 1 s tick for the first minute, then every 10 s (`open_adapter` asks
  each backend in turn and codex's answer rescans its whole session tree).
  Nothing is pushed and `live` stays false until it opens; the tail is
  still reaped on the usual rule if the source never appears.
- A tail that was reaped and later starts again pushes a `Reset` before it
  bootstraps. A phone may still hold the history from the first run, and
  replaying it would append every `user_message` a second time. `seq`
  keeps counting across the restart; only `epoch` ever resets it.
- The ring drops everything before a `Reset` as it stores one. The
  consumer is about to refetch from `after=0` anyway, and the dropped
  events are usually the bulk of the ring. The `Reset` itself stays, so a
  phone catching up on `after=lastSeq` still sees it.

## Phase — where the verdict comes from

`phase` is not the adapter's. Adapters read content; what a session is
DOING is decided by one function, `remote_api::activity_verdict`, which the
sessions list (`GET /v1/sessions`) and the spine's driver both call. There
is one rule, so the phone's list and the session it opens can never
disagree about a session.

Four inputs:

- **Terminal cadence** — bytes on the tab's pty in the last 10 s
  (`TabRegistry::session_activities`, which spells it `output`). Immediate,
  and blind: it cannot tell working from waiting-on-a-person, and it cannot
  tell an agent thinking from a TUI repainting.
- **The transcript** — `remote_api::transcript_verdict`, a tail read of the
  session's own files: grok's `events.jsonl` (`permission_requested` →
  attention, with the reason `permission`), antigravity's step types,
  codex's open turn that has written nothing for 45 s (`approval`),
  opencode's store. Returns the verdict and a short reason, or nothing.
  For agy it also reads `~/.gemini/antigravity-cli/cli.log` — see below.
- **The spine's own turn bracket** — whether the adapter's last
  `turn_started` / `turn_ended` left a turn open. `Spine::push` records it
  wherever an event enters the log, so no path can route around it, and
  `Spine::turn_open` hands it to both callers. `None` means no adapter has
  ever reported a turn boundary for this session (the legacy adapter emits
  neither), and the rule below leaves such a session exactly as it was. A
  Claude Code `Stop` or `UserPromptSubmit` hook moves this bracket too — see
  Hooks below.
- **A Claude Code hook** — the harness saying, as it happens, that a
  permission dialog is up. `Spine::hook_attention`. See Hooks below.

```rust
activity_verdict(
    terminal: Option<&str>,                            // cadence
    transcript: Option<(&'static str, &'static str)>,  // verdict + reason
    turn_open: Option<bool>,                           // the spine's bracket
    hook_attention: bool,                              // a hook says a person is waited on
) -> (&'static str, &'static str)
```

The rule, in order:

1. Cadence normalises to working/attention/idle (`session_activities`
   spells working as `output`).
2. **A closed turn stops cadence claiming work.** When `turn_open` is
   `Some(false)`, a cadence of `working` becomes `idle`. A TUI goes on
   repainting after the answer is finished — a spinner clearing, a footer
   redrawn, the prompt coming back — and held on its own that kept the
   phone's header on "working" for the full ten seconds cadence counts as
   recent, well after the turn had visibly ended
   [observed: Claude Code, 2026-09-02]. The gate is a gate and not a latch:
   the next `turn_started` re-opens it, within one poll of the user's line
   reaching disk.
3. The transcript outranks cadence when cadence is idle, and when the
   transcript says `attention` against a cadence `working` — codex's TUI
   animates through its own approval dialog, so cadence alone holds
   `working` forever. Cadence is never demoted to idle from the transcript
   side.
4. **A hook that says attention outranks all three.** It is the harness's
   own announcement, not an inference from a file or a pty; nothing below
   it may demote it, and it stands until another hook or the transcript
   retires it.

Attention still outranks everything, from either source: a closed turn with
a permission prompt still up is a person being waited on, not an idle
session. The gate is on cadence only — a transcript that says `working`
against a closed bracket is still believed.

Two paths push it, both through the same rule and the same dedupe:

- The tabs registry bridge, on every cadence change, for immediacy — it
  fires the moment bytes flow, where the tick can be up to a second behind.
  No transcript half (a 256 KB tail read has no business on the pty's
  output path) and no detail: cadence knows no reason.
- The driver's 1 s tick, which is the authority — it is what carries
  `attention` and its reason. A poll whose batch contained a `turn_started`
  or `turn_ended` recomputes immediately and skips the cache below, so idle
  lands with the turn that ended rather than up to a second after it.

Both are dropped when the (phase, detail) pair is the one already standing.
Cadence fires four times a second while output flows; without that the ring
would be nothing but identical `working` events. The tick's transcript read
is skipped whenever the length and mtime of every file it reads are
unchanged AND the newest of them is more than 60 s old — past that a
verdict can no longer flip on its own, and before it, codex's 45 s
staleness rule still can. Cadence and the turn bracket are re-read every
tick regardless: a terminal falling quiet is the one change a cached
transcript cannot see.

### Hooks — Claude Code's own account of itself

Claude Code is the one engine that will *tell* us what it is doing, as it
happens. `hooklink` already installed a `SessionStart` hook (the exact
pid → session link); the same injected settings file now carries the rest,
all running the same `aiterm --hook-report` binary, matcher-less, with a
5 s timeout:

```json
{"hooks":{"SessionStart":[{"hooks":[{"type":"command",
  "command":"'/usr/bin/aiterm' --hook-report","timeout":5}]}],
  "UserPromptSubmit":[…],"PreToolUse":[…],"PostToolUse":[…],
  "Notification":[…],"PermissionRequest":[…],"Stop":[…]}}
```

`MessageDisplay` is deliberately absent: partial assistant text would
duplicate on the phone what the transcript adapter already streams as
blocks.

The hook writes one small file into `~/.local/share/aiterm/hook-events/`,
named for the nanosecond it was written (`{nanos:020}-{pid}.json`, so name
order is time order), carrying the payload's own field names — trimmed, not
reshaped, with `tool_input` reduced to the one key that names what is being
acted on. `SessionStart` still goes to its own spool (`session-events/`,
one file per process, polled by the frontend); the two never mix. Both are
write-then-rename, both swallow every error and exit 0: a hook runs inside
claude's own path, before and after every tool call.

The app drains the phase spool on an inotify watch of the directory, with a
2 s tick behind it in case the watch could not be armed. Measured latency
from the hook process writing the file to the `phase` event's `ts`: 53–55 ms
[observed 2026-09-02]. Each file is deleted as it is read — a status nobody
could place is worth nothing a second later — and a session with no tail
drops on the floor, like every other phase.

| hook | phase | detail |
|---|---|---|
| `Notification` `permission_prompt` | `needs_you` | `permission: <tool>`, dug out of the harness's own sentence ("Claude needs your permission to use Bash") |
| `Notification` `idle_prompt` | `idle` | — |
| `Notification` anything else | ignored | (`auth_success`, `elicitation_dialog`, `agent_needs_input`, …) |
| `PermissionRequest` | `needs_you` | `permission: <tool_name>` |
| `PreToolUse` | `working` | `running <tool>: <command \| file_path \| path \| pattern \| url>`, one line, 60 chars |
| `PostToolUse` | `working` | "" — the tool is done, and the empty detail is what clears its name |
| `UserPromptSubmit` | `working` | "", and the turn bracket opens |
| `Stop` | recomputed | idle unless the transcript says attention, and the turn bracket closes |

Two rules make hook-derived phase outrank the rest, both in
`activity_verdict`'s one place:

- **A hook that says needs-you is not demoted.** It is the only input here
  that is not an inference — not a file read after the fact, not bytes on a
  pty — so `activity_verdict` gained a fourth argument, `hook_attention`,
  which turns any non-attention verdict into `("attention", "permission")`.
  Cadence is what would otherwise demote it: claude's TUI redraws its own
  dialog, so the pty is busy for as long as the person takes to answer. It
  stands until a later hook retires it (the tool running, the turn ending)
  or the transcript says attention with a better reason. The sessions list
  passes the same flag, so the phone's list and the session it opens cannot
  disagree.
- **A hook's own words survive the tick.** The phase tick a second later
  computes `("working", "")` and would blank "running Bash: npm test"; the
  registry keeps the last hook phase per session and re-applies its detail
  while that phase is still the standing one.

Hooks emit no `turn_started` / `turn_ended`: the transcript's
`turn_duration` stays the single source of turn events, and a second one
from this side would unbalance the bracket the rule above reads. They do
move the bracket itself — `Stop` closes it, `UserPromptSubmit` opens it —
because that is the gate cadence is measured against and the hook is a beat
ahead of the transcript. Without it, `Stop`'s idle was undone 28 ms later
by the TUI's next repaint and re-corrected 270 ms after that [observed
live, 2026-09-02]; with it, idle lands 10 ms *before* the transcript's own
`turn_ended`.

A session aiterm did not launch has no `--settings` flag, so none of its
hooks fire and nothing changes for it: the transcript path still works. And
`bypassPermissions` (john's own claude setting) means the permission arm
never fires in his tabs at all — nothing asks, so nothing is announced.

The payloads, all probed live against Claude Code 2.1.259 on 2026-09-02
rather than taken from the docs, which are wrong in two places: `PostToolUse`
carries `tool_response` (not `tool_result`), and `Stop` carries
`stop_hook_active` + `last_assistant_message` and no `stop_reason` at all —
which is why `Stop` recomputes a verdict instead of claiming one.
`PermissionRequest` exists and, unlike grok's `permission_requested`, fires
only when a dialog is actually displayed: it did not fire for an
auto-allowed Bash.

### Antigravity's invisible permission prompt

agy is the one engine whose "waiting on a person" state reaches none of its
own records. It writes the `PLANNER_RESPONSE` carrying the tool call and
then nothing at all while its TUI holds a confirmation dialog — no `ask_*`
tool, no further step, no change to the transcript. Observed live: a
`run_command` sat on its dialog for minutes while the spine read `working`
[observed: agy 1.1.24, 2026-09-02].

The only record is one line in agy's own log:

```
I0902 21:38:28.616360  492 tool_confirmation_manager.go:197] Surfacing tool confirmation: "RunCommand" at step 2
```

So the agy arm answers `("attention", "permission")` when both hold:

1. the transcript ends on an open call — a `PLANNER_RESPONSE` with
   `tool_calls` and no `GENERIC` result step after it, and
2. `cli.log`'s last 64 KB carries a `Surfacing tool confirmation` line
   stamped later than the transcript's mtime.

Answering the prompt writes the result step, which moves the transcript's
mtime past the log line, and the verdict falls back to `working` on its
own. `cli.log` is a symlink into `log/cli-<stamp>.log` re-pointed on every
run, and both the stat and the read follow it, so a confirmation from a
previous run is never seen.

Two things to know about this signal. glog writes no year and no zone, so
the timestamp is parsed as local civil time through `mktime` (which is what
knows this machine's zone and its DST rule) with the current year, minus
one if that would put the line in the future. And **the log carries no
conversation id**: with two agy TUIs open at once, one prompt would read as
attention on both sessions that have an open call. One at a time is the
normal case; a spurious "come and look" costs a glance, where the
alternative is missing every real one.

`cli.log` is registered in `phase_sources`, so the tick's mtime gate
re-reads when the log moves. agy writes to it constantly, which in practice
means an agy session's verdict is never skipped while one is running — its
transcript is a few KB, so that read stays cheap.

The tick is a second rather than two because idle has to land within about
one. A tick that finds nothing changed is two `stat` calls and a compare —
1.8 µs measured, debug build, on a session watching a 28 MB transcript and
a 64 KB `events.jsonl`. The read it skips is 3.6 ms on that same session,
so the gate is what makes the faster tick free. `phase_sources` (the
`owner_in` walk that finds those files) is only re-run every 5 s while it
is still finding nothing: a miss costs 2.7 ms, because `owner_in` asks
every backend and codex's answer rescans its whole session tree.

A phase is only ever pushed for a session that already has a tail. Neither
path starts one — a phase with no content behind it is not worth opening a
transcript for.

A duplicate `phase` is harmless: it is status, not content, and applying
the same one twice changes nothing. The dedupe above is there to keep the
ring from filling with them, not because a consumer would mind.

## Adapter contract

```rust
pub trait Adapter: Send {
    /// Full history, in order. Called once when the tail starts.
    fn bootstrap(&mut self) -> Vec<(u64, Kind)>;   // (ts ms, kind)
    /// Everything new since the last call. Cheap: read from an offset.
    fn poll(&mut self) -> Vec<(u64, Kind)>;
    /// Files whose change should trigger `poll()`.
    fn watch_paths(&self) -> Vec<PathBuf>;
}
pub fn open_adapter(agent: &str, session_id: &str) -> Option<Box<dyn Adapter>>;
```

`claude::open(session_id)` and `grok::open(session_id)` return their
adapters; anything else gets `legacy::open(agent, session_id)`.

If a source is truncated or replaced under the adapter (a `/clear`, a new
file), `poll()` returns a `Reset` followed by the rebuilt history.

## Per-engine sources

| engine | content | phase / turns |
|---|---|---|
| claude | `~/.claude/projects/<proj>/<id>.jsonl`, one line per content block (thinking, text, tool_use) + `user` lines carrying `tool_result`. Skip `isSidechain` and `isMeta`. | `user` line → `turn_started`; `system` `turn_duration` → `turn_ended`; terminal activity → `phase`; Claude Code's own hooks → `phase` (see Hooks above). |
| grok | `~/.grok/sessions/<cwd>/<id>/updates.jsonl` — ACP `session/update` lines: `user_message_chunk`, `agent_message_chunk`, `agent_thought_chunk`, `tool_call`, `tool_call_update`, and `_x.ai` `turn_completed` (in updates.jsonl, not events.jsonl). Consecutive chunks of one kind fold into one block (id = ordinal of the first line). | `events.jsonl`: `turn_started`, `turn_ended` (with `outcome`), `permission_requested` / `permission_resolved`, `tool_started` / `tool_completed` (with `outcome`), `first_token`, and `phase_changed` (ignored — it describes the TUI, not the session). Also `_x.ai` `task_backgrounded` / `task_completed` (a backgrounded command: its card stays running and closes from the task result — Grok marks the card completed with a placeholder the moment the launch returns, and the result quotes a task id that differs from the tool call id for 9 of 29 observed tasks; the backgrounded line is the only record of the pairing), `subagent_spawned` / `subagent_finished` (a second card, `sub-<id>`, category think; the child writes its own session directory, invisible from the parent), and ACP `plan` (an `agent_thought` checklist under `p<first line ordinal>`, rewritten in place). |
| codex | `~/.codex/sessions/<y>/<m>/<d>/rollout-*.jsonl`. `response_item` lines carry content under the engine's own ids (`payload.id`, `call_id`); the TUI's mirror (`event_msg` `item_completed`) is the ONLY record that says a tool failed — a command that exits 1 still gets an output reading "Script completed". Almost every tool is one JavaScript `exec`; tool and category are parsed out of the snippet. Reasoning arrives encrypted with an empty summary. | `event_msg` `task_started` / `task_complete` / `turn_aborted` (a real `interrupted`), all under codex's `turn_id`. |
| antigravity | `~/.gemini/antigravity-cli/brain/<id>/.system_generated/logs/transcript.jsonl`, one step per line, no ids (id = `<conversation>:<step_index>`, `:<i>` per call, `:thinking`). File order is NOT step order: a parallel call's output can land before the step that issued it, so outputs pair to calls by index arithmetic. A failed tool writes NOTHING — its index is a hole — so a call whose output index is passed by a later step is `failed`; `run_command` alone writes its output whatever the exit code. | `USER_INPUT` opens a turn; a prose-only `PLANNER_RESPONSE` ends it; an unanswered `ask_*` is `needs_you`. Written at step completion, never earlier (the SQLite db is touched once, at startup). |
| others | legacy adapter over `conversation_rich`. | the phase verdict above. |

Two things the adapters learned that the vocabulary above does not say:

- **Claude's `turn_ended.reason` is only ever `completed`.** The transcript
  has no record for an interruption — it arrives as a `user` text block
  reading "[Request interrupted by user]", which is prose, not an event.
  `interrupted` will have to come from a hook or from `phase`.
- **Grok's `permission_requested` fires for every tool, yolo mode
  included** — it is the harness announcing a check, not necessarily a
  prompt on screen. Only a request still unresolved at the END of a poll
  batch is a person being waited on, so that is the only one the adapter
  reports as `needs_you`.

The legacy adapter has no ids, no timestamps and no streaming to work
with — `conversation_rich` returns `(role, text)` turns — so: ids are the
turn's ordinal (`legacy:<n>`), `ts` is when it was read, `done` is always
true, and the role IS the tool name for anything that is not `user`,
`assistant` or `thinking`. The one exception is `system`, which on that
path is only ever the "earlier turns omitted for length" marker
`conversation_rich` inserts when a conversation is over budget: it is prose
about the conversation, not a tool that ran, so it maps to `agent_text`.
The last turn is never treated as settled (a growing assistant block is
re-emitted under its own id, which upserts); a shorter list, or a settled
turn that changed, is a `Reset` and a full replay.

## Ownership while it is being built

- spine core, registry, endpoints, WS, legacy adapter, phase bridge:
  `src-tauri/src/spine/mod.rs`, `spine/legacy.rs`, `remote_api.rs`, `lib.rs`, `tabs.rs`.
- claude adapter: `src-tauri/src/spine/claude.rs` (+ visibility tweaks in `detail.rs`).
- Claude Code hooks → phase: `src-tauri/src/hooklink.rs`, the `--hook-report`
  argv mode in `main.rs`, `push_hook_phase` in `spine/registry.rs`.
- grok adapter: `src-tauri/src/spine/grok.rs` (+ visibility tweaks in `grok.rs`).
- phone: `mobile/**`.

The types and trait in `spine/mod.rs` are the contract. Change them only by
agreement; everything else is yours.
