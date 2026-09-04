# Editing Claude Code's configuration — phases 2 and 3

Status: designed 2026-08-06, not yet implemented. Follows
`2026-08-06-claude-config-panel-design.md`, whose phase 1 (read-only) shipped in
0.10.38.

Phases 2 and 3 are specified together because phase 3 is phase 2's writer with a
different surface on top: hooks are a subtree of `settings.json`, so the hook
editor adds UI and no new file handling.

## What phase 1 left

The panel shows the layered settings, the `CLAUDE.md` chain, MCP and skills, and
writes nothing. Changing any of it still means finding the right file by hand and
knowing which of three layers governs. Phase 2 makes the panel able to change
what it shows; phase 3 makes hooks — the part of `settings.json` least pleasant
to hand-edit and easiest to get subtly wrong — editable as hooks rather than as
JSON.

## The rule that changes, and the one that does not

Phase 1's constraint was that nothing under `claudecfg` writes. That narrows: a
single new module, `claudecfg/write.rs`, is the only thing permitted to write,
and the five readers stay pure. Stated explicitly because a boundary like this
erodes by accident — a reader that "just needs to fix up" a file is how it goes.

What does not change: aiterm never writes its own settings into the user's
config. Its SessionStart hook stays in its own file, passed with `--settings`.
The hook editor in phase 3 shows aiterm's hook, labelled, and refuses to edit it
there.

## Saving a layer

`save_layer(path, new_text, loaded_text)`, in order:

1. **Refuse on collision.** Read the file's current bytes; if they differ from
   `loaded_text` — the exact bytes the panel read — refuse with a distinct error
   the UI recognises. Claude writes `settings.json` itself, so this is a real
   collision, not a theoretical one.
2. **Validate.** `new_text` must parse as a JSON *object*. A valid JSON array or
   bare string is not a settings file.
3. **Back up.** Copy the old contents to `<path>.bak-aiterm`, matching the
   convention already in use for `settings.json.bak-aiterm`.
4. **Write atomically.** Write to a temp file in the same directory, then
   `rename` over the target. A crash mid-write must not leave a truncated file
   that every session on the machine reads.

The comparison is on exact bytes, not parsed equality. If another writer
reformats without changing a value, that still counts as a change: refusing a
save the user can retry is better than silently discarding somebody's edit.

Creating a layer that does not exist yet is allowed — `loaded_text` is then the
empty string, and a file appearing where the panel expected none is itself a
collision.

## Two ways to edit, deliberately not three

**Inline row editing.** Each setting row becomes editable in place, with the
control chosen from the value's current JSON type: text for a string, a toggle
for a boolean, a number field for a number, an add/remove list for an array of
strings. Anything else — a nested object, a mixed array — is not inline
editable and says so, pointing at the raw editor.

The edit is applied to the **parsed original and re-serialised with
`preserve_order`**, never rebuilt from a schema. So a key the UI does not
understand keeps its place and its value. Same reasoning as the union-of-keys
display: the panel must not be able to quietly drop what it cannot read.

An inline edit writes to **the layer that already sets the key** — the winner.
Editing `model` when only the user file sets it writes the user file.

**A raw per-layer editor.** A text area holding that layer's actual file, with
validation before save and the parse error shown when it fails. This is where a
key gets added, removed, or restructured.

**No add-key form in the inline rows.** Two ways to add a key is one too many,
and the raw editor cannot be wrong about structure.

## Hooks (phase 3)

Hooks read as a settings key today — `hooks.SessionStart` with a JSON blob for a
value. Phase 3 gives them their own section: one row per hook, showing its event,
its matcher, the command it runs, and which layer it came from.

Two properties this section must have, because a hook is a shell command that
fires on its own:

- **It shows what will run, in full.** A truncated command in a hook editor is a
  trap; the row expands to the whole thing.
- **Adding one is not weightless.** The editor asks for event, matcher and
  command as separate fields, and the save path is the same
  compare-validate-backup-rename as everything else.

aiterm's own hook appears in the list, labelled as aiterm's, and is not editable
from here — it lives in aiterm's own file by design, and an editor that offered
to change it there would either fail or start a fight between the two writers.

Hooks are additive across layers (phase 1 already reports them as merged rather
than overridden, which is why aiterm's injection works at all), so a hook row
names its layer and never implies it replaced another.

## Failure handling

- collision → the save is refused, the panel says what happened and offers to
  reload the current contents
- invalid JSON → the parse error, inline, save button inert
- unwritable file (permissions) → the reason, not a generic failure
- backup fails → the save does not proceed. A write without a backup is the one
  case where being helpful is worse than being useless.

## Testing

`save_layer` gets real files, in a temp directory — the constraint is that
aiterm never writes *Claude's* files unbidden, not that tests cannot write at
all. Cases: a clean save replaces the contents; a collision refuses and leaves
the file untouched; invalid JSON refuses; a non-object refuses; the backup holds
the previous contents; an inline edit preserves a key the editor does not
understand; creating a new layer works from an empty `loaded_text`; a file that
appeared unexpectedly is a collision.

Hook parsing gets fixtures: one event with several hooks, a matcher present and
absent, aiterm's own hook recognised by its command, and a malformed hooks blob
reported rather than dropped.

## Non-goals

- editing `CLAUDE.md` — prose, better edited with real editing tools
- editing MCP or skills — different files, different shapes, no demand yet
- enterprise/managed policy layers
- a diff view of what a save will change; the raw editor already shows the text
