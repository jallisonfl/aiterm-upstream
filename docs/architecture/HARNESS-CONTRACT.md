# The harness adapter contract

What aiterm must know about an agent CLI to make it a first-class citizen —
in the sidebar, in a tab, and on the phone. Every engine answers every
question differently; an adapter is the set of answers for one engine.

**How to use this file**: hand it to the agent whose CLI you're integrating
("implement/audit the <engine> adapter against HARNESS-CONTRACT.md"), along
with two pointers: the trait in `src-tauri/src/agents.rs`, and `grok.rs` as
the reference adapter — it documents observed behavior with versions, and
nothing in it is guessed. Require the same standard: every answer verified
against real files from a real session, stamped with the CLI version it was
read off. Training-data memory of a CLI's formats is usually stale.

## 1. Launch & identity
- Command to start a fresh session; flags for model, effort, permission mode.
- Can it be TOLD a session id at launch (claude `--session-id`, grok
  `--session-id`)? If not (codex), adoption: how does the transcript that
  appears after launch identify itself (id + cwd), so the placeholder tab
  can be re-keyed to it?
- Resume by id from any directory.

## 2. Session discovery  (`sessions.rs` providers)
- Where sessions live on disk and their unit (file vs directory).
- How to read: id, title, cwd, branch, last_active, forked/parent.
- What must never be shown as a title (harness boilerplate).

## 3. Conversation parsing  (`detail.rs`)
- Turn encoding: roles, text blocks, tool calls, tool results, reasoning.
- What the phone hides: tool outputs, harness preambles (codex sends
  AGENTS.md as its own first "user" message, untagged, BEFORE its
  <INSTRUCTIONS> block; grok ≥1.0.13 splits the first prompt into four
  "user" lines — user_info/rules, two system_reminder lines, then the
  real user_query), env blocks.
- Tool-input summaries: the person-readable one-liner per tool call
  (codex `exec` JS → the shell command inside; image_gen → the prompt).

## 4. Busy / needs-you  (`remote.rs` transcript_state, `pty.rs` activity)
- Turn-in-flight signal: explicit events (codex task_started/complete;
  grok ≥1.0.13 turn_started/turn_ended in the session's events.jsonl),
  last-role (claude), open tool_calls (grok pre-events fallback).
- Waiting-on-a-person signal: an explicit event where the engine writes
  one (grok ≥1.0.13 permission_requested/permission_resolved), bell/OSC 9
  (claude), or inference — an unanswered tool call plus a transcript
  quiet for ~45s (codex approval prompts write NOTHING while up; no
  approval record exists in any rollout 0.144→0.150.1).
- Terminal: OSC 9;4 progress? bell? If neither, output cadence is the
  only working signal.

## 5. Artifacts  (`changes.rs`)
- Harness-owned output dirs outside the workspace, and how the path names
  the session (codex `~/.codex/generated_images/<sid>/…`, grok
  `~/.grok/sessions/<enc-cwd>/<sid>/images/…`). Add the shape to
  `harness_session_of`, the noise filter, backfill, `harness_output_dirs`,
  and the remote file allowlist.
- Which session-dir contents are bookkeeping (grok: everything but
  `images/`) and must never be recorded as artifacts.
- Transcript-declared writes (claude Write/Edit tool_use), for
  `session_artifacts`.

## 6. Tasks / plans  (`sessions.rs` session_tasks)
- The todo format: claude task records, codex `update_plan`,
  grok `todo_write`.

## 7. Usage / limits  (`usage.rs`)
- Endpoint/CLI for plan bars and balances; auth source; observed rate
  limits (Anthropic throttles the usage poll — cache and retry).

## 8. Models / efforts  (`agents.rs` models())
- Static list, or shelled out per launch? Efforts are per-MODEL, not
  per-engine (codex publishes different sets).

## 9. Lifecycle
- Stop: daemon roster (claude) vs pty-tree kill (everyone else).
- Delete/trash: safe for files; a session that is a DIRECTORY (grok)
  needs directory trash or no button — half-working is worse.
- Fork / clear / compaction: NOT claude-only — proven otherwise 2026-08-31.
  Codex writes `compacted` records (0.150.1); grok has /compact, auto-compact
  at 80%, and on-disk compaction dirs (1.0.13). Fork lineage diverges: grok
  stamps parent_session_id in the child's summary.json; claude --fork-session
  (2.1.251) is a full replay with re-minted uuids and NO parent trace — the
  launcher must record lineage itself if it wants any.

## 10. Version stamp
Every answer above rots. Stamp the adapter's doc comments with the CLI
version the behavior was read off, and re-verify on upgrades.

## The standard of proof
An adapter claim is either (a) read off real files produced by a real
session during the work, with the version noted, or (b) not made. The
reference for tone and rigor is `grok.rs`.

## The state machine (why desktop and mobile must not disagree)

The desktop terminal is a window — it can't be wrong. Every remote surface
is a *reconstruction*, and it must come from ONE resolver with explicit
precedence, not from whichever signal spoke last:

1. **Explicit terminal signals** (OSC 9;4 progress, bell) — always
   believed, immediately.
2. **Transcript facts** — an open turn bracket (codex task_started
   without task_complete), an unanswered tool call, whose message is
   last. These outrank cadence: an engine mid tool call is silent AND
   busy.
3. **Output cadence** — a tiebreaker only. Quiet may propose idle;
   the transcript gets a veto before idle is announced
   (`pty_set_activity` → `transcript_state`).

Rule of thumb: cadence may promote to working, never demote to idle on
its own. "Needs you" comes from the bell where the engine rings one, and
from the unanswered-call + stale-transcript inference where it doesn't.

## Onboarding a new harness (deepseek CLI, qwen CLI, whoever's next)

The acceptance test, run against a REAL session of the new engine:

1. Start it in a tab. Does a sidebar row appear with the right title,
   cwd, time? (discovery)
2. Ask something that uses a tool. Does the phone say **working** the
   whole time — including during a long, silent tool call? (state)
3. Does it ask a question / want approval? Does the phone say
   **needs you** within a minute? (attention)
4. Have it write a file in the workspace and one image/artifact wherever
   it likes. Do both appear in the session's Files, attributed? (ledger)
5. Open the conversation on the phone. Readable turns, no harness
   preamble, tool calls summarized in one line each? (parsing)
6. Send a message from the phone. Does it land? Stop from the phone.
   Does it die? (lifecycle)
7. Stamp every adapter answer with the CLI version observed.

Fail any step → that surface's contract answer is wrong for this engine;
fix the adapter, not the resolver. Many new CLIs are forks of codex or
gemini-cli — check whether an existing adapter's shapes match before
writing new ones.
