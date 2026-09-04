# Claude Code's session model, as far as we have proven it

Working notes for aiterm. Everything here is split into **what was verified and
how**, and **what is still open**. The split is the point: on 2026-07-25 a full
day was lost building fixes on top of plausible-but-unchecked assumptions about
this model. If a claim moves from "open" to "verified", record the evidence.

Last updated 2026-07-25.

---

## Where sessions live

- A session is a JSONL transcript at
  `~/.claude/projects/<flattened-cwd>/<session-uuid>.jsonl`.
  The directory name is the session's launch cwd with `/` and `.` flattened to
  `-`. *Verified: listing the store.*
- Job state for daemon-run sessions is `~/.claude/jobs/<short-id>/state.json`,
  where `<short-id>` is the first UUID segment. *Verified: read directly.*
- Retired transcripts are renamed `<id>.orphaned-<epoch>-<hash>.jsonl` rather
  than deleted — but not always; a `/clear`ed original can be removed outright.
  *Verified: both shapes present in the store.*

## Resuming

- `claude --resume <id>` refuses a session that is **currently running**, with
  "…add --fork-session to branch off a copy". Nearly every awkward behaviour in
  a GUI wrapper descends from this one constraint.
  *Verified 2026-07-25: `--resume` on live bg agent `f0768f17` exited with
  "Session … is currently running as a background agent (bg). Use `claude
  agents` to find and attach to it, or add --fork-session to branch off a
  copy."*
- **`--resume <id>` with no `<id>.jsonl` on disk prints "No conversation found
  with session ID: <id>" — and interactively does not stop there.** It falls
  through into a fresh, empty session under whatever `--session-id` was passed.
  That session has no transcript, so it never gets a row in the list, while its
  process sits in the roster forever. This is what "I click ⑂ and nothing
  happens" actually is: the fork ran, and produced an invisible empty session.
  *Verified: five live processes resuming the long-deleted `8e6ad72e`, none
  with a transcript; reproduced directly in `-p` mode.*
- Forking a session that is **live** works fine, as long as its transcript
  exists. *Verified: forked live bg agent `f0768f17` (still 51,103 B and still
  running afterwards) into `92bd2404`, which got 54,321 B of copied history and
  answered a prompt.*
- `--fork-session` copies the full history into a **new** session id and leaves
  the parent byte-identical and independently resumable.
  *Verified: parent `32cb631d` unchanged at 64358 bytes with its original
  mtime; child carried 36 records back to the parent's first message.*
- `--session-id <uuid>` **composes** with `--fork-session`, so the caller can
  mint the child's id instead of discovering it later.
  *Verified: ran it; the child transcript appeared at the chosen uuid.*
- `--session-id` with `--resume` but **without** `--fork-session` is rejected
  outright: "Error: --session-id can only be used with --continue or --resume
  if --fork-session is also specified." *Verified 2026-07-25 in `-p` mode.*
- **`--resume <id>` resolves a conversation by the `sessionId` recorded *inside*
  the transcript, not by the file's name.** A `cp <parent>.jsonl <new>.jsonl`
  produces a file `--resume` cannot open — it reports "No conversation found
  with session ID: <new>" even though `<new>.jsonl` is sitting right there. The
  same copy with the id fields rewritten resumes perfectly, with full context.
  *Verified 2026-07-25: plain copy of a 452-line transcript failed; a copy with
  `sessionId`/`session_id` rewritten answered a question about the conversation
  it inherited. This is the fact aiterm's ⑂ is built on, and the reason
  opcode's `fs::copy` fork produces unresumable files.*
- A transcript legitimately carries lines stamped with **other** session ids.
  Resuming copies the prior session's records forward verbatim, keeping their
  original `sessionId`, so a resumed transcript is genuinely mixed-id. Rewriting
  only the lines bearing the file's own id is enough — the historical ones can
  be left alone and resume still works.
  *Verified 2026-07-25: a branch of session `8a576195` contained lines stamped
  `437fecea` (the session it had been resumed from); it resumed normally.*
- Together these mean **a session can be branched with no process at all** —
  copy the transcript, rewrite its id fields, and a new resumable session
  exists. `--fork-session` is not the only way to fork, and unlike it, the file
  copy produces the branch *immediately* rather than on first prompt.
  *Verified: this is what `sessions::session_fork` does as of aiterm 0.4.4.*
- Prompting an already-running session does **not** mint a new session.
  **Resuming does.** *Verified: sent a bare test message, no new transcript and
  no new roster entry; only the live transcript grew.*

## Background agents

- Background agents are held by the Claude Code daemon and **survive the client
  exiting**, so they are still running the next time the GUI starts.
- `claude agents --json` lists live sessions — **both** `background` and
  `interactive` — with `sessionId`, `kind`, `pid`, `cwd`, and `state`
  (`working` / `blocked` / `done`). *Verified: used throughout.*
  - It includes `state: "done"` entries, so "appears in the roster" is **not**
    the same as "is running". Filter `done`.
- **There is no attach-by-id.** No flag on `claude agents`, and no other
  subcommand (`auth`, `auto-mode`, `doctor`, `gateway`, `install`, `mcp`,
  `plugin`, `project`, `setup-token`, `ultrareview`, `update`), opens a session
  directly. `claude agents` always lands on a list.
  *Verified: read the full help for every subcommand.*
- `claude agents --cwd <path>` **filters** to agents started under `<path>` — it
  does not merely sort. A fork started in a different tree is therefore absent
  from a view filtered by the row's project path. *Verified: help text plus the
  observed empty view.*
- For an **interactive** session the roster's `pid` is the real `claude`
  process. For a **background** agent it is not: it points at a `claude
  bg-spare` helper parented to `claude bg-pty-host`, itself parented to the
  daemon. The conversation actually runs as
  `…/versions/<v> --session-id <other-id> --agent…`, and *that* id is not in
  the roster at all. Killing the roster pid for a bg agent therefore kills a
  spare, not the session. *Verified: full /proc tree, 2026-07-25.*
  (An earlier note claimed SIGTERM to the roster pid stops the agent; that
  held for the interactive agents it was tested on, not for background ones.)
- So **"did the stop work?" must be answered by the roster**, never by watching
  the pid die.
- The roster lists sessions that have **no transcript on disk** — an empty
  session from a failed `--resume` keeps an entry indefinitely. "In the roster"
  therefore does not imply "has a row in the session list".

## Deleting

- **Deleting a running session's transcript does not stick.** The live process
  recreates the file at the same path within seconds, rebuilt from the deletion
  point — so the row returns *and* the history before the delete is lost.
  *Verified: `b79ba823` trashed at 11,660 bytes, reappeared at 660 bytes one
  minute later.*
- Therefore: stop the process first, then delete. Never offer delete on a
  session known to be running.

## Telling forks apart

Two different things are both called "fork", and they leave completely
different traces.

| | `/fork` command | `--fork-session` (what a GUI runs) |
|---|---|---|
| transcript | ~192-byte stub: `ai-title` + `agent-name` only, no cwd, no message chain | full copy of the parent's history |
| `sessionKind` | absent | absent |
| jobs state | `forkSessionId` + `forkParentSessionId` **present** | fork fields all `null` |
| `parentUuid` | no message records at all | resolves **in-file** (history was copied) |
| `bridgeSessionId` | — | child gets its **own**, not the parent's |
| background | **always** `template: "bg"` | inherits the caller |

- **`/fork` always produces a background agent, regardless of
  `remoteControlAtStartup`.** That setting governs startup, not this command.
  *Verified 2026-07-25: with `remoteControlAtStartup: false` in **both**
  `~/.claude.json` and `~/.claude/settings.json`, `/fork` still wrote a job
  state with `template: "bg"`. The forked row is therefore live, daemon-held,
  and owned by no tab — green dot, no exit button — from the moment it exists.*
- `/fork` branches **beside** the conversation; it does not relocate it. The
  parent keeps its session id and its transcript keeps growing in the same tab.
  *Verified 2026-07-25: forked `8a576195` → `7f8edb5a`; the parent kept its id
  and grew afterwards in place.*

- So `/fork` lineage is discoverable from job state, and **`--fork-session`
  lineage is discoverable from nothing on disk.** *Verified three independent
  ways for the `--fork-session` case.*
- Consequence: a GUI that forks must **mint the child id itself**
  (`--session-id`) and record the pair, or it can never link them afterward.
- There is a **third** mechanism, which aiterm uses as of 0.4.4: copy the
  transcript and rewrite its id fields (see *Resuming*). It leaves no job
  state, starts no process, and produces no background agent — so its lineage
  must be recorded by the app (`~/.local/share/aiterm/forks.json`). Unlike
  `/fork`, the branch is complete and idle the instant it appears rather than
  being a stub that fills in on resume.
- Useful side effect for a GUI: `/fork` titles its stub `<project> ⑂`, so a
  console-made fork is visually distinguishable from an app-made one without
  any extra bookkeeping. Worth *not* stripping.
- `sessionKind: "bg"` in a transcript is a **permanent scar** — it stays true
  long after the agent exits, so it answers "was this ever a background
  session", never "is it running now". *Verified: a dead session still
  reporting it.*

## Conversation identity across session ids

- `bridgeSessionId` is the stable id for a *conversation* while its session ids
  churn.
- It appears in transcripts as a `bridge-session` record, but **not always** —
  one live session had none.
- It **also** appears in job state as `bridgeSessionId`, and that copy linked
  two session ids whose transcripts could not be linked.
  *Verified: `8e6ad72e`'s job state carries the same bridge as `2a7f02c6`'s
  transcript; they are the same conversation.*
- This is the most promising key for "group these rows as one conversation".

## Configuration that changes all of the above

- `remoteControlAtStartup` exists in **two** files:
  - `~/.claude/settings.json`
  - `~/.claude.json` ← **authoritative**; setting only the first has no effect.
  *Verified the hard way.*
- Job state for a conversation that has moved to the background records
  `template: "bg"`, `backend: "daemon"`, `interactiveLineage: true`.
- It governs **startup**, not every path into the background: `/fork` ignores
  it entirely (see *Telling forks apart*). Do not treat `false` as "nothing
  will become a background agent".

---

## Open questions

1. **Does `remoteControlAtStartup: false` actually stop sessions becoming
   background agents?** Both files now read `false`. **Partly answered
   2026-07-25: it does not govern `/fork`, which still produces `template:
   "bg"` with the setting off** (see *Telling forks apart*). Whether it governs
   an ordinary fresh session is still open — one fresh (not resumed) session
   settles that half.
2. Does a conversation whose job state carries `template: "bg"` **always**
   re-spawn as a background job when resumed?
3. What exactly triggers "Your conversation moved to the background"?
4. ~~Reproduce the `--resume`-refuses-a-running-session error directly.~~
   *Answered 2026-07-25 — see Resuming.*
5. How do you stop a **background** agent, given the roster pid is a spare?
   `stop_session` signals what it can and then reports honestly if the roster
   still lists the session; it does not yet know the real move.

---

## What this implies for aiterm

- **A tab owns its session's lifetime** — but it did not, for a long time.
  `pty_kill` called `killer.kill()`, which reaches only the pty's direct child:
  the `zsh -i -c claude …` wrapper. **zsh forks the command rather than
  exec'ing it** (verified: each `claude`'s `PPid` is its `zsh`), so closing a
  tab killed the shell and orphaned `claude`. Every session aiterm ever
  launched stayed in the roster, which made its row permanently "running",
  which left fork-a-copy as the only action offered — and each fork orphaned
  another one. *Verified 2026-07-25: seven live `claude --fork-session` from
  aiterm, all parented to one aiterm pid.* Fixed by killing the whole /proc
  descendant tree, deepest first, SIGTERM then SIGKILL.
- **Resume means: stop what's running, then `--resume`** — the shell workflow.
  Since `--resume` refuses a live session, the alternative was to offer ⑂
  instead, which made branching a copy the only way back into a conversation.
  The stop must be *confirmed complete* (roster no longer lists it) before the
  resume is spawned, or the resume just hits the refusal.
- A **zombie keeps its `/proc/<pid>` entry** until reaped, so a liveness check
  built on directory existence calls every killed process alive. Check
  `State:` for `Z`.
- **"Has a tab" and "is running" are different questions** and must not share a
  flag. They name the same row right up until a conversation moves to the
  background, and then every consequence — the live dot, which actions are
  offered, whether delete appears — lands on the wrong row.
- **Never hide a row on a heuristic.** The list should show what is on disk;
  only an explicit delete removes something.
- Duplicate near-identical rows are **real files**, not a display bug: one
  full-history snapshot per resume, each frozen at that moment. The fix is to
  group and label them honestly, not to hide them.
