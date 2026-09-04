# Claude Code configuration panel

Status: designed 2026-08-06, not yet implemented. Phase 1 specified here; phases
2 and 3 sketched at the end.

## The problem

Claude Code's behaviour in an aiterm tab is decided by files aiterm never shows:
`~/.claude/settings.json`, the project's `.claude/settings.json` and
`.claude/settings.local.json`, a `CLAUDE.md` chain with `@imports`, MCP
registrations in `~/.claude.json`, skills in three different trees — and flags
aiterm itself injects.

The cost is concrete. Asked on 2026-08-05 whether aiterm sets the model and the
permission mode, answering took a grep through `agents.rs`, a read of
`~/.claude/settings.json`, a check of the injected `--settings` file and a look
at `ps` output. The answers were interesting — aiterm sets no model, and
hardcodes `--permission-mode auto --allow-dangerously-skip-permissions` for
every claude launch — and neither was visible anywhere in the app.

## Scope

Three subsystems, split in dependency order. Each ships useful alone.

- **Phase 1 (this spec)** — read everything, write nothing.
- **Phase 2** — edit `settings.json` per layer, with backup and atomic replace.
- **Phase 3** — a hook editor, built on Phase 2's writer.

Phase 1 alone would have answered the question above without reading source.

## Placement and navigation

Settings → **Agents** already lists one `.agent-row` per engine with a dot,
version and path. A row gains a **Settings** button, which drills down inside
the same modal (back link, no modal-over-modal) to a panel titled for that
engine.

Everything about Claude Code lives inside that panel, so the scoping is never in
doubt — the panel is a hub of buttons rather than one long scroll:

```
← Agents          CLAUDE CODE  2.1.220  ~/.local/bin/claude

  [ Settings ]  [ Instructions ]  [ MCP ]  [ Skills ]
```

The button is gated on a new `Caps.config` flag, not on `agent === "claude"`.
Removing engine-name checks was the point of the engine abstraction; only Claude
declares `config: true` in Phase 1, and Codex can declare it later without the
Agents pane learning anything new.

## Backend: `claudecfg.rs`

One module, four commands, all real work in pure functions over strings so they
are testable without a home directory:

| Command | Answers |
|---|---|
| `claude_settings(project)` | the layered settings, resolved |
| `claude_instructions(project)` | the `CLAUDE.md` chain |
| `claude_mcp(project)` | registered MCP servers and their scope |
| `claude_skills(project)` | skills, and which tree each came from |

### Settings layers and precedence

Highest wins:

1. managed/enterprise policy — out of scope, absent on these machines
2. CLI `--settings` file (this is aiterm's own injected file)
3. `<project>/.claude/settings.local.json`
4. `<project>/.claude/settings.json`
5. `~/.claude/settings.json`

Parsed with `serde_json` in preserve-order mode, which the crate already enables.

Per key the panel receives: the effective value, the layer that won, and *every*
layer that set it — so "project overrides user" is displayable without the
frontend knowing the precedence rules. All three layers exist in this repo
already (`model` from user, `worktree.bgIsolation` from project, `permissions`
from project-local), so the override display is exercised by real data on day
one.

### Grouping by concern, without a hardcoded schema

The key list is the **union of keys actually present across the layers**, each
mapped to a concern by a lookup table, with unrecognised keys landing in
**Other**.

This is deliberate. `ClaudeBackend::models()` already carries a comment warning
that hardcoded Claude knowledge ages and "will age"; a hardcoded settings schema
would age the same way, and its failure mode is worse — silently omitting a key
that is actually in effect. Union-of-present-keys cannot hide anything, and an
unfamiliar key showing up under Other is a correct, if plain, answer.

Concerns: Model, Permissions, Hooks, Environment, MCP, Notifications & UI,
Housekeeping, Other.

### Instructions

The chain, in load order, each with path, presence, line count and an Open
button:

- `~/.claude/CLAUDE.md`
- `<project>/CLAUDE.md`

`@path` imports are followed recursively, depth-limited, with a cycle guard —
the global file here imports `@RTK.md`, so the common case has depth 2 and any
display that stopped at depth 1 would be wrong.

### Session Start

The section that would have answered the original question:

- the flags aiterm injects, **read from a single definition in `agents.rs` that
  the launcher itself uses.** Not re-typed here. A panel listing launch flags
  from a second copy will eventually lie about them, and this is the surface
  where being wrong is worst.
- hooks in effect, with aiterm's own labelled as aiterm's. aiterm's SessionStart
  hook lives in its own file (`~/.local/share/aiterm/claude-hook-settings.json`,
  passed via `--settings`) specifically so the user's config stays untouched.
  The panel shows it and never offers to edit it there, or the two would fight.

### MCP

Its own button, so it can read MCP's real sources rather than only the settings
layers:

- `~/.claude.json` → top-level `mcpServers` (user scope)
- `~/.claude.json` → `projects[<path>]` → `enabledMcpjsonServers` /
  `disabledMcpjsonServers` (per-project enablement)
- `<project>/.mcp.json` (project scope, checked into a repo)

Observed 2026-08-06: top-level `mcpServers` is empty and no `.mcp.json` exists
here, while sessions clearly have MCP tools available — those arrive as
claude.ai connectors, which are not in any local file. The panel must say so
rather than showing an empty list that reads as "no MCP".

`~/.claude.json` is 155 KB with 49 projects tracked, so it is read for the keys
named above and never held or displayed whole.

### Skills

Three trees, each labelled by source:

- `~/.claude/skills/<name>/SKILL.md` — user
- `<project>/.claude/skills/<name>/SKILL.md` — project
- plugins: **resolved through `~/.claude/plugins/installed_plugins.json`**, which
  records an `installPath` per installed plugin; skills are
  `<installPath>/skills/<name>/SKILL.md`

That last point is not a detail. The plugin cache holds **several versions of
the same plugin** — `document-skills` is present at three version hashes — so
globbing the cache lists every skill two or three times. `installed_plugins.json`
names the live one, so nothing is guessed.

Each skill shows name, description from the SKILL.md frontmatter, source, and an
Open button.

## Failure handling

The panel is most likely to be opened when something is wrong, so degradation
matters more than usual:

- a **missing** file reads as "not present" — not an error
- a **malformed** layer shows its parse error inline and drops out of
  resolution, rather than blanking the panel
- **no project open** shows the project layers as such, rather than as absent
- a file that cannot be read shows why, per file

## Testing

Pure functions over fixture strings, the pattern `launch.rs` and `opencode.rs`
already use:

- precedence: a key set in all three layers resolves to project-local
- override display: every setter is reported, not just the winner
- concern mapping, including an unknown key landing in Other
- import chain: depth 2 resolves, a cycle terminates, depth limit holds
- malformed layer: resolution continues without it and the error is reported
- plugin skills: two cached versions of one plugin yield one skill, from the
  path `installed_plugins.json` names
- MCP: an empty local config is reported as "none configured locally", distinct
  from "not read"

## Non-goals for Phase 1

- writing any Claude file (Phase 2)
- editing hooks (Phase 3)
- editing `CLAUDE.md` — a prose file better edited with real editing tools
- enterprise/managed policy layers
- listing claude.ai connector MCP servers, which are not in local files

## Phase 2 sketch — editing settings.json

Per-layer writes. Requirements: back up the file first, write atomically
(temp + rename, never truncate-in-place), validate JSON before replacing, and
**preserve keys the form does not know about** — the union-of-keys model means
the panel can see such keys, and a save that drops them would be data loss in a
file every session on the machine reads.

## Phase 3 sketch — hook editor

Hooks are a settings subtree, so this is mostly UI: event → matcher → command,
on Phase 2's writer. Needs its own care: hooks are shell commands that fire
automatically, so the editor should show what a hook will run and when, and
never make adding one feel weightless.
