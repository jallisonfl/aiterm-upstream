# Claude Code configuration editing — Implementation Plan (phases 2 & 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Claude Code panel able to change what it shows — inline row edits and a raw per-layer editor for `settings.json`, plus a hooks section that edits hooks as hooks.

**Architecture:** One new module, `claudecfg/write.rs`, is the only thing in `claudecfg` permitted to write. It compares, validates, backs up, then renames. Everything else — inline edits, the raw editor, the hook editor — funnels through it.

**Tech Stack:** Rust (serde_json with `preserve_order`), Tauri 2 commands, React + TypeScript, `node --experimental-strip-types --test`.

**Spec:** `docs/design/2026-08-06-claude-config-editing-design.md`

## Global Constraints

- `claudecfg/write.rs` is the ONLY file under `claudecfg` that may write. The five readers (`settings`, `concern`, `instructions`, `mcp`, `skills`) stay pure — no `fs::write`, no `File::create` in any of them.
- aiterm never writes its own settings into the user's config. Its SessionStart hook stays in `~/.local/share/aiterm/claude-hook-settings.json`. The hook editor shows it, labelled, and refuses to edit it.
- No new crate or npm dependencies.
- A save NEVER proceeds without a successful backup.
- Collision detection compares EXACT BYTES, not parsed equality.
- Edits apply to the parsed original re-serialised with `preserve_order` — never rebuilt from a schema, so unknown keys survive.
- No `agent === "claude"` capability gating in the frontend. `ClaudeConfig` may check `agent.id` to assert its own identity (it already does, with a comment).
- Test names are sentences describing behaviour, house style as in `launch.rs`.
- Comments explain why, not what.
- Every task ends: `cd src-tauri && cargo test` green (currently 249 lib + 14 integration), `npx tsc --noEmit` clean, `npm run test:ui` green (currently 18), clippy at exactly its two pre-existing warnings (`pty.rs`, `watcher.rs`).
- Home PC only at the end. The work PC has been held since 2026-08-04 and stays held unless Matt lifts it.

---

### Task 1: `save_layer` — compare, validate, back up, rename

**Files:**
- Create: `src-tauri/src/claudecfg/write.rs`
- Modify: `src-tauri/src/claudecfg/mod.rs` (add `pub mod write;`)

**Interfaces:**
- Produces:
  - `pub enum SaveError { Collision, NotAnObject, Invalid(String), Io(String) }` (serialisable, `#[serde(tag = "kind", content = "detail", rename_all = "camelCase")]`)
  - `pub fn save_layer(path: &str, new_text: &str, loaded_text: &str) -> Result<(), SaveError>`
  - `pub fn backup_path(path: &str) -> String`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Real files, in a temp directory. The rule is that aiterm never writes
    /// *Claude's* files unbidden — not that tests cannot write at all, and a
    /// writer this consequential is not worth testing through a fake.
    fn scratch(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!("aiterm-write-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("settings.json").to_string_lossy().to_string()
    }

    #[test]
    fn a_clean_save_replaces_the_contents() {
        let p = scratch("clean");
        std::fs::write(&p, r#"{"model":"opus"}"#).unwrap();
        save_layer(&p, r#"{"model":"sonnet"}"#, r#"{"model":"opus"}"#).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), r#"{"model":"sonnet"}"#);
    }

    #[test]
    fn the_previous_contents_are_kept_as_a_backup() {
        let p = scratch("backup");
        std::fs::write(&p, r#"{"model":"opus"}"#).unwrap();
        save_layer(&p, r#"{"model":"sonnet"}"#, r#"{"model":"opus"}"#).unwrap();
        assert_eq!(std::fs::read_to_string(backup_path(&p)).unwrap(), r#"{"model":"opus"}"#);
    }

    #[test]
    fn a_file_that_changed_since_it_was_read_refuses_and_is_left_alone() {
        // Claude writes settings.json itself, so this is the real case.
        let p = scratch("collision");
        std::fs::write(&p, r#"{"model":"haiku"}"#).unwrap();
        let err = save_layer(&p, r#"{"model":"sonnet"}"#, r#"{"model":"opus"}"#).unwrap_err();
        assert!(matches!(err, SaveError::Collision), "{err:?}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), r#"{"model":"haiku"}"#);
    }

    #[test]
    fn a_reformat_by_someone_else_still_counts_as_a_change() {
        // Byte comparison, not parsed equality: refusing a save the user can
        // retry beats silently discarding another writer's edit.
        let p = scratch("reformat");
        std::fs::write(&p, "{\n  \"model\": \"opus\"\n}").unwrap();
        let err = save_layer(&p, r#"{"model":"sonnet"}"#, r#"{"model":"opus"}"#).unwrap_err();
        assert!(matches!(err, SaveError::Collision), "{err:?}");
    }

    #[test]
    fn invalid_json_refuses_and_leaves_the_file_untouched() {
        let p = scratch("invalid");
        std::fs::write(&p, r#"{"model":"opus"}"#).unwrap();
        let err = save_layer(&p, "{ not json", r#"{"model":"opus"}"#).unwrap_err();
        assert!(matches!(err, SaveError::Invalid(_)), "{err:?}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), r#"{"model":"opus"}"#);
    }

    #[test]
    fn valid_json_that_is_not_an_object_refuses() {
        // A settings file is an object. An array parses fine and is still wrong.
        let p = scratch("array");
        std::fs::write(&p, r#"{"a":1}"#).unwrap();
        let err = save_layer(&p, "[1,2,3]", r#"{"a":1}"#).unwrap_err();
        assert!(matches!(err, SaveError::NotAnObject), "{err:?}");
    }

    #[test]
    fn a_layer_that_does_not_exist_yet_can_be_created() {
        let p = scratch("create");
        save_layer(&p, r#"{"model":"opus"}"#, "").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), r#"{"model":"opus"}"#);
    }

    #[test]
    fn a_file_that_appeared_where_none_was_expected_is_a_collision() {
        let p = scratch("appeared");
        std::fs::write(&p, r#"{"someone":"else"}"#).unwrap();
        let err = save_layer(&p, r#"{"model":"opus"}"#, "").unwrap_err();
        assert!(matches!(err, SaveError::Collision), "{err:?}");
    }

    #[test]
    fn the_backup_name_sits_beside_the_file_it_copies() {
        assert_eq!(backup_path("/h/.claude/settings.json"), "/h/.claude/settings.json.bak-aiterm");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd src-tauri && cargo test claudecfg::write`
Expected: FAIL to compile — module does not exist.

- [ ] **Step 3: Implement**

```rust
//! The one place in `claudecfg` that writes.
//!
//! Everything else here reads; that split is deliberate and worth keeping
//! visible, because a reader that "just needs to fix up" a file is how a
//! read-only guarantee stops being one.
//!
//! These are files every Claude session on the machine reads, so a save is
//! four steps in a fixed order: refuse if the file moved under us, refuse if
//! the new text is not a settings file, keep the old contents, and replace by
//! rename so a crash cannot leave a truncated file behind.

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", content = "detail", rename_all = "camelCase")]
pub enum SaveError {
    /// The file changed since the panel read it. Claude writes settings.json
    /// itself, so this is an ordinary event, not a corruption.
    Collision,
    /// Parsed, but not an object — a settings file is a map of keys.
    NotAnObject,
    /// Did not parse; carries the reason so the editor can point at it.
    Invalid(String),
    Io(String),
}

/// Beside the file it copies, matching the `settings.json.bak-aiterm`
/// convention already in use on these machines.
pub fn backup_path(path: &str) -> String {
    format!("{path}.bak-aiterm")
}

/// Replace a settings file's contents.
///
/// `loaded_text` is the exact bytes the caller read. Empty means "there was no
/// file" — creating one is allowed, and a file existing anyway is a collision
/// like any other.
pub fn save_layer(path: &str, new_text: &str, loaded_text: &str) -> Result<(), SaveError> {
    let current = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(SaveError::Io(e.to_string())),
    };
    if current != loaded_text {
        return Err(SaveError::Collision);
    }

    let parsed: Value = serde_json::from_str(new_text)
        .map_err(|e| SaveError::Invalid(e.to_string()))?;
    if !parsed.is_object() {
        return Err(SaveError::NotAnObject);
    }

    // Before anything is replaced. A save that cannot keep the old contents is
    // one where being helpful is worse than being useless.
    if !current.is_empty() {
        std::fs::write(backup_path(path), &current)
            .map_err(|e| SaveError::Io(format!("backup failed: {e}")))?;
    }

    // Same directory, so the rename is on one filesystem and therefore atomic.
    let tmp = format!("{path}.tmp-aiterm");
    std::fs::write(&tmp, new_text).map_err(|e| SaveError::Io(e.to_string()))?;
    std::fs::rename(&tmp, path).map_err(|e| SaveError::Io(e.to_string()))?;
    Ok(())
}
```

Add `pub mod write;` to `claudecfg/mod.rs`.

- [ ] **Step 4: Run the tests**

Run: `cd src-tauri && cargo test claudecfg` — expected all pass, 9 new.
Run: `cargo clippy --all-targets 2>&1 | grep -cE "^warning: this"` — expected `2`.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/claudecfg
git commit -m "One writer, and it refuses more than it accepts"
```

---

### Task 2: Layers carry their raw text, and a save command

**Files:**
- Modify: `src-tauri/src/claudecfg/settings.rs` (`Layer` gains `text`)
- Modify: `src-tauri/src/claudecfg/mod.rs` (populate it; add `claude_save_layer`)
- Modify: `src-tauri/src/lib.rs` (register the command)

**Interfaces:**
- Consumes: `write::{save_layer, SaveError}`, `settings::Layer`.
- Produces: `Layer.text: String` (empty when absent); command `claude_save_layer(path: String, new_text: String, loaded_text: String) -> Result<(), SaveError>`.

- [ ] **Step 1: Write the failing tests**

In `claudecfg/mod.rs` tests:

```rust
    #[test]
    fn a_layer_carries_the_bytes_it_was_read_from() {
        // The raw editor needs the text, and the same bytes are the collision
        // token — one read, one source of truth for both.
        if std::env::var("HOME").is_err() {
            return;
        }
        let v = claude_settings(None);
        let user = v.layers.iter().find(|l| l.id == settings::LayerId::User).unwrap();
        if user.present {
            assert!(!user.text.is_empty(), "a present layer must carry its text");
        } else {
            assert!(user.text.is_empty(), "an absent layer carries no text");
        }
    }
```

In `claudecfg/write.rs` tests, add:

```rust
    #[test]
    fn the_command_surfaces_a_collision_as_a_typed_error_not_a_string() {
        // The UI has to tell a collision apart from a syntax error to offer
        // "reload" rather than "fix your JSON".
        let e = SaveError::Collision;
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("collision"), "{json}");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cd src-tauri && cargo test claudecfg`
Expected: FAIL — no field `text` on `Layer`.

- [ ] **Step 3: Implement**

In `settings.rs`, add to `Layer`:

```rust
    /// The exact bytes read, empty when the file is absent.
    ///
    /// Serves two jobs at once on purpose: the raw editor's initial content,
    /// and the token a save is checked against. Reading the file twice would
    /// invite the two to disagree.
    pub text: String,
```

Update every `Layer { .. }` construction in `mod.rs` to pass `text` — the read
arm passes the string it already has, the missing arm passes `String::new()`.
Note `claude_settings` currently moves the text into `texts`; clone into the
layer rather than re-reading.

Add the command:

```rust
/// Replace one settings layer's contents. `loaded_text` must be the bytes the
/// panel last read, or the save is refused — see `write::save_layer`.
#[tauri::command]
pub fn claude_save_layer(
    path: String,
    new_text: String,
    loaded_text: String,
) -> Result<(), write::SaveError> {
    write::save_layer(&path, &new_text, &loaded_text)
}
```

Register `claudecfg::claude_save_layer` in `lib.rs` beside the other four.

- [ ] **Step 4: Run the tests**

Run: `cd src-tauri && cargo test` — all green.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src
git commit -m "Read a layer's bytes once, for the editor and the collision check"
```

---

### Task 3: Applying one key's edit without rebuilding the file

**Files:**
- Create: `src-tauri/src/claudecfg/edit.rs`
- Modify: `src-tauri/src/claudecfg/mod.rs` (`pub mod edit;`, plus a command)

**Interfaces:**
- Produces:
  - `pub fn set_key(original: &str, dotted_key: &str, value: Value) -> Result<String, String>` — returns the new file text
  - command `claude_set_key(path, dotted_key, value, loaded_text) -> Result<(), SaveError>`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_scalar_is_replaced_in_place() {
        let out = set_key(r#"{"model":"opus"}"#, "model", json!("sonnet")).unwrap();
        assert_eq!(out.replace([' ', '\n'], ""), r#"{"model":"sonnet"}"#);
    }

    #[test]
    fn a_key_the_editor_does_not_understand_survives_the_save() {
        // The whole reason this applies an edit to the parsed original instead
        // of rebuilding from a schema.
        let out = set_key(
            r#"{"model":"opus","worktree":{"bgIsolation":"none"}}"#,
            "model",
            json!("sonnet"),
        )
        .unwrap();
        assert!(out.contains("bgIsolation"), "{out}");
    }

    #[test]
    fn key_order_is_preserved() {
        // serde_json's preserve_order feature; a save that reshuffled a user's
        // file would make every diff unreadable.
        let out = set_key(r#"{"z":1,"a":2}"#, "a", json!(3)).unwrap();
        assert!(out.find("\"z\"").unwrap() < out.find("\"a\"").unwrap(), "{out}");
    }

    #[test]
    fn a_nested_key_is_reached_through_its_path() {
        let out = set_key(r#"{"permissions":{"deny":["a"]}}"#, "permissions.deny", json!(["b"]))
            .unwrap();
        assert!(out.contains("\"b\""), "{out}");
        assert!(!out.contains("\"a\""), "{out}");
    }

    #[test]
    fn a_missing_intermediate_object_is_created() {
        let out = set_key("{}", "permissions.deny", json!(["x"])).unwrap();
        assert!(out.contains("permissions"), "{out}");
        assert!(out.contains("deny"), "{out}");
    }

    #[test]
    fn a_path_running_through_a_scalar_is_refused_rather_than_clobbering_it() {
        // "model.nested" when model is a string: overwriting would silently
        // destroy a value the user did not ask to change.
        let err = set_key(r#"{"model":"opus"}"#, "model.nested", json!(1)).unwrap_err();
        assert!(err.contains("model"), "{err}");
    }

    #[test]
    fn an_original_that_is_not_json_is_refused() {
        assert!(set_key("{ broken", "model", json!("x")).is_err());
    }

    #[test]
    fn an_empty_original_starts_from_an_object() {
        let out = set_key("", "model", json!("opus")).unwrap();
        assert!(out.contains("model"), "{out}");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd src-tauri && cargo test claudecfg::edit` — FAIL, module absent.

- [ ] **Step 3: Implement**

```rust
//! Applying a single key's edit to a settings file.
//!
//! The edit goes onto the *parsed original*, which is then re-serialised with
//! `preserve_order`. It is never rebuilt from a list of known settings — the
//! panel shows the union of keys actually present precisely so it cannot hide
//! one, and a save that dropped what it did not recognise would give that back.

use serde_json::{Map, Value};

/// Set `dotted_key` to `value` in `original`, returning the new file text.
///
/// Missing intermediate objects are created. A path running through a
/// non-object is refused rather than overwriting it: replacing a scalar with a
/// map to make room for a nested key destroys a value nobody asked to change.
pub fn set_key(original: &str, dotted_key: &str, value: Value) -> Result<String, String> {
    let mut root: Value = if original.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str(original).map_err(|e| e.to_string())?
    };
    if !root.is_object() {
        return Err("not a JSON object".into());
    }

    let parts: Vec<&str> = dotted_key.split('.').collect();
    let mut cursor = &mut root;
    for (i, part) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        let map = match cursor {
            Value::Object(m) => m,
            _ => return Err(format!("{} is not an object", parts[..i].join("."))),
        };
        if last {
            map.insert((*part).to_string(), value);
            break;
        }
        cursor = map
            .entry((*part).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !cursor.is_object() {
            return Err(format!("{} is not an object", parts[..=i].join(".")));
        }
    }

    // Pretty, because a human edits this file too and a one-line settings.json
    // is a hostile thing to hand back.
    serde_json::to_string_pretty(&root).map_err(|e| e.to_string())
}
```

Add to `mod.rs`:

```rust
/// Change one key in one layer. The panel's inline row editors use this; the
/// raw editor sends whole files through `claude_save_layer` instead.
#[tauri::command]
pub fn claude_set_key(
    path: String,
    dotted_key: String,
    value: serde_json::Value,
    loaded_text: String,
) -> Result<(), write::SaveError> {
    let next = edit::set_key(&loaded_text, &dotted_key, value)
        .map_err(write::SaveError::Invalid)?;
    write::save_layer(&path, &next, &loaded_text)
}
```

Register `claudecfg::claude_set_key` in `lib.rs`.

- [ ] **Step 4: Run the tests**

Run: `cd src-tauri && cargo test claudecfg` — all green, 8 new.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src
git commit -m "Edit a key onto the file that exists, not onto a schema"
```

---

### Task 4: IPC for saving

**Files:**
- Modify: `src/ipc.ts`

**Interfaces:**
- Produces: `ClaudeSaveError`, `claudeSaveLayer`, `claudeSetKey`; `ClaudeLayer` gains `text: string`.

- [ ] **Step 1: Implement**

Add `text: string;` to `ClaudeLayer` with a comment noting it is both the raw
editor's content and the collision token. Then:

```ts
/** Why a save was refused. `collision` is the one the UI must treat specially —
 *  it means the file moved under us and the answer is to reload, not to fix
 *  anything. */
export type ClaudeSaveError =
  | { kind: "collision" }
  | { kind: "notAnObject" }
  | { kind: "invalid"; detail: string }
  | { kind: "io"; detail: string };

/** Replace one layer's whole file. `loadedText` must be the bytes last read. */
export const claudeSaveLayer = (path: string, newText: string, loadedText: string) =>
  invoke<void>("claude_save_layer", { path, newText, loadedText });

/** Change a single key in one layer, applied onto the file as it stands so keys
 *  the panel does not understand are not dropped. */
export const claudeSetKey = (
  path: string,
  dottedKey: string,
  value: unknown,
  loadedText: string,
) => invoke<void>("claude_set_key", { path, dottedKey, value, loadedText });
```

- [ ] **Step 2: Typecheck**

Run: `npx tsc --noEmit` — clean.

- [ ] **Step 3: Commit**

```bash
git add src/ipc.ts
git commit -m "IPC for the two save paths"
```

---

### Task 5: Inline row editing

**Files:**
- Modify: `src/components/agent-config/SettingsSection.tsx`
- Modify: `src/App.css`

**Interfaces:**
- Consumes: `claudeSetKey`, `ClaudeSaveError`, `ClaudeLayer.text`.

- [ ] **Step 1: Implement**

Each setting row gains an edit affordance. Requirements, in the component:

- A row is inline-editable when `effective` is a string, number, boolean, or an
  array whose every element is a string. Anything else renders as today with a
  short note: `"Edit in the raw editor"` — nested objects and mixed arrays are
  not worth a bespoke control each.
- The control follows the type: `<input type="text">` for a string, `type="number"`
  for a number, a checkbox for a boolean, and for a string array a list of
  removable chips plus an add field.
- Saving calls `claudeSetKey(layer.path, s.key, value, layer.text)` where `layer`
  is the layer named by `s.winner` — the layer that already sets the key. Look it
  up from `view.layers`; if it is somehow absent, disable editing for that row
  rather than guessing.
- After a successful save, re-fetch via `claudeSettings(project)` so `text` and
  the resolved values are consistent again. Do not mutate local state and hope.
- On `{ kind: "collision" }`, show: `"That file changed on disk since this panel
  read it — your edit was not saved."` plus a Reload button that re-fetches.
  Every other error shows its detail.
- While a save is in flight the row's control is disabled; two saves racing
  against the same `loadedText` would make the second a guaranteed collision.

Styles: add `.acfg-edit` (the inline control wrapper), `.acfg-chip` (array
element with its remove affordance), `.acfg-save` / `.acfg-cancel`, and
`.acfg-collision` for the refusal line. Keep to the existing visual language —
small, low-contrast, `var(--accent)` only for the active thing.

- [ ] **Step 2: Verify**

Run: `npx tsc --noEmit` — clean.

Then verify against the real installed app is deferred to Task 8; the browser
harness has been unreliable (Chrome extension injection faults, page idle). If
it happens to work, check: a string row edits and saves; a boolean toggles; an
array adds and removes; a nested object row offers no inline edit.

- [ ] **Step 3: Commit**

```bash
git add src/components/agent-config/SettingsSection.tsx src/App.css
git commit -m "Edit a setting where it is shown"
```

---

### Task 6: The raw per-layer editor

**Files:**
- Create: `src/components/agent-config/RawLayerEditor.tsx`
- Modify: `src/components/agent-config/SettingsSection.tsx` (open it per layer)
- Modify: `src/App.css`

**Interfaces:**
- Consumes: `claudeSaveLayer`, `ClaudeLayer`, `ClaudeSaveError`.
- Produces: `<RawLayerEditor layer={ClaudeLayer} onSaved={() => void} onClose={() => void} />`

- [ ] **Step 1: Implement**

- Each layer row in the Files list gains an **Edit** button beside Open. It opens
  the raw editor for that layer, inline beneath the row, not as a modal — the
  panel is already a drill-down and a third stacked surface is one too many.
- A `<textarea>` initialised from `layer.text`, monospace, using the terminal
  font settings is not required — the panel font is fine.
- Live validation: on every change, `JSON.parse` the text; on failure show the
  message and disable Save. Do not wait for the save to find out.
- Save calls `claudeSaveLayer(layer.path, text, layer.text)`. On success call
  `onSaved()` (which re-fetches) and close. On `collision` show the same wording
  as Task 5 with a Reload button; on other errors show the detail.
- An absent layer (`present: false`) may still be edited — that is how a project
  settings file gets created. Start its textarea at `{}` rather than empty, and
  say the file will be created.
- Cancel discards without confirmation only when the text is unchanged; when it
  differs, ask. Losing typed JSON to a stray click is a bad trade.

Styles: `.acfg-raw` (wrapper), `.acfg-raw-area` (textarea, min-height ~180px,
`font-family: monospace`), `.acfg-raw-actions`, reuse `.acfg-err` for the parse
error.

- [ ] **Step 2: Verify**

Run: `npx tsc --noEmit` — clean.

- [ ] **Step 3: Commit**

```bash
git add src/components/agent-config src/App.css
git commit -m "A raw editor per layer, for what rows cannot express"
```

---

### Task 7: Hooks as hooks

**Files:**
- Create: `src-tauri/src/claudecfg/hooks.rs`
- Modify: `src-tauri/src/claudecfg/mod.rs` (`pub mod hooks;`, plus `claude_hooks`)
- Create: `src/components/agent-config/HooksSection.tsx`
- Modify: `src/components/agent-config/ClaudeConfig.tsx` (a fifth tab)
- Modify: `src/ipc.ts`, `src/App.css`, `src-tauri/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub struct Hook { pub event: String, pub matcher: Option<String>, pub command: String, pub layer: String, pub is_aiterm: bool }`
  - `pub fn parse(layer_label: &str, hooks_value: &Value, aiterm_marker: &str) -> (Vec<Hook>, Vec<String>)`
  - command `claude_hooks(project) -> HooksView { hooks, errors }`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const MARKER: &str = "--hook-report";

    #[test]
    fn one_event_with_two_hooks_yields_two_rows() {
        let v = json!({"SessionStart":[{"hooks":[
            {"type":"command","command":"a"},
            {"type":"command","command":"b"}]}]});
        let (h, _) = parse("user", &v, MARKER);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].event, "SessionStart");
    }

    #[test]
    fn a_matcher_is_carried_when_present_and_absent_when_not() {
        let v = json!({"PreToolUse":[
            {"matcher":"Bash","hooks":[{"type":"command","command":"x"}]},
            {"hooks":[{"type":"command","command":"y"}]}]});
        let (h, _) = parse("user", &v, MARKER);
        assert_eq!(h[0].matcher.as_deref(), Some("Bash"));
        assert_eq!(h[1].matcher, None);
    }

    #[test]
    fn aiterms_own_hook_is_recognised_so_it_is_not_offered_for_editing() {
        // It lives in aiterm's own --settings file by design; an editor that
        // offered to change it here would either fail or fight the writer.
        let v = json!({"SessionStart":[{"hooks":[
            {"type":"command","command":"'/usr/bin/aiterm' --hook-report"}]}]});
        let (h, _) = parse("aiterm", &v, MARKER);
        assert!(h[0].is_aiterm);
    }

    #[test]
    fn someone_elses_hook_is_not_mistaken_for_aiterms() {
        let v = json!({"SessionStart":[{"hooks":[{"type":"command","command":"echo hi"}]}]});
        let (h, _) = parse("user", &v, MARKER);
        assert!(!h[0].is_aiterm);
    }

    #[test]
    fn a_malformed_hooks_blob_is_reported_rather_than_dropped() {
        let (h, errors) = parse("user", &json!({"SessionStart":"not a list"}), MARKER);
        assert!(h.is_empty());
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("SessionStart"), "{errors:?}");
    }

    #[test]
    fn a_hook_carries_the_layer_it_came_from() {
        // Hooks are additive across layers, so a row must never imply it
        // replaced another.
        let v = json!({"Stop":[{"hooks":[{"type":"command","command":"z"}]}]});
        let (h, _) = parse("project local", &v, MARKER);
        assert_eq!(h[0].layer, "project local");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd src-tauri && cargo test claudecfg::hooks` — FAIL, module absent.

- [ ] **Step 3: Implement**

`hooks.rs`: walk `{event: [{matcher?, hooks: [{type, command}]}]}`, emitting one
`Hook` per command. A value of the wrong shape at any level pushes an error
naming the event rather than being skipped. `is_aiterm` is
`command.contains(aiterm_marker)`.

In `mod.rs`, `claude_hooks` resolves every layer (reusing `layer_paths` and the
same reads as `claude_settings`), calls `parse` per layer with
`aiterm_marker = "--hook-report"`, and concatenates. Register in `lib.rs`.

`HooksSection.tsx`: one row per hook — event, matcher (or "any"), the command in
full (wrapping, never truncated), and the layer. aiterm's rows are labelled
`aiterm` and carry no edit affordance, with a line saying why: it lives in
aiterm's own settings file so your config stays untouched.

Editing, built on Task 3's `claudeSetKey`: an **Add hook** form asking for event
(a select of the known events: `SessionStart`, `SessionEnd`, `Stop`,
`SubagentStop`, `PreToolUse`, `PostToolUse`, `UserPromptSubmit`, `PreCompact`,
`Notification`), matcher (optional), and command (a text input). Saving reads the
chosen layer's current `hooks` value, appends, and writes the whole `hooks` key
via `claudeSetKey`. A Remove affordance on non-aiterm rows does the same in
reverse. Show the command that will run in the confirm step — a hook is a shell
command that fires by itself, and adding one should not feel weightless.

Add the fifth tab to `ClaudeConfig.tsx` between Instructions and MCP.

- [ ] **Step 4: Run the tests**

Run: `cd src-tauri && cargo test` and `npx tsc --noEmit` — both green.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src src/components/agent-config src/ipc.ts src/App.css
git commit -m "Hooks, shown and edited as hooks"
```

---

### Task 8: Ship it

- [ ] **Step 1: Full verification**

```bash
npx tsc --noEmit && npm run test:ui
cd src-tauri && cargo test && cargo clippy --all-targets 2>&1 | grep -cE "^warning: this"
```

Expected: clean, 18 UI pass, all Rust green, clippy `2`.

- [ ] **Step 2: Bump both version strings to the next patch, build**

```bash
npm run tauri build -- --bundles rpm
```

- [ ] **Step 3: Install on the home PC only**

```bash
sudo -n dnf install -y src-tauri/target/release/bundle/rpm/aiterm-<v>-1.x86_64.rpm
rpm -q aiterm
cp src-tauri/target/release/bundle/rpm/aiterm-<v>-1.x86_64.rpm ~/Projects/aiterm-releases/
```

Do NOT push to the work PC — the 2026-08-04 hold stands until Matt lifts it.

- [ ] **Step 4: Real-app checks after Matt relaunches** (do not relaunch for him)

- a string setting edits inline and the file on disk changes
- `<file>.bak-aiterm` appears beside it holding the previous contents
- a key the panel shows as "Other" (`worktree.bgIsolation`) survives an unrelated
  inline edit — the preserve-unknown-keys guarantee, on his real file
- the raw editor rejects invalid JSON before allowing Save
- the hooks section lists aiterm's SessionStart hook, labelled aiterm, with no
  edit affordance
- his real `PreToolUse` hooks appear with their layer

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "<version>"
```

---

## Self-review

**Spec coverage.** The four-step save → Task 1. Raw text as collision token →
Task 2. Preserve-unknown-keys and inline application → Task 3. Two edit surfaces
→ Tasks 5 and 6. No add-key form in rows → stated in Task 5, adding happens in
Task 6. Hooks section with aiterm's labelled and unedited → Task 7. Failure
handling → Task 1's typed errors, surfaced in Tasks 5, 6, 7. Backup-before-write
→ Task 1 test. Testing list → covered per task.

**Placeholders.** Tasks 1-4 and 7's Rust carry complete code and tests. Tasks 5,
6 and 7's UI are specified by behaviour rather than transcribed line-for-line —
deliberate, because the components must match the existing visual language, and
the constraints that matter (which layer receives an edit, re-fetch after save,
collision wording, disable while in flight, no truncated commands) are each
stated as a requirement rather than left to taste.

**Type consistency.** `SaveError` serialises `#[serde(tag="kind", content="detail")]`
so `Invalid(String)` arrives as `{kind:"invalid",detail:"…"}`, matching
`ClaudeSaveError` in Task 4. `Layer.text` → `text` (no rename needed). `Hook`
uses `rename_all = "camelCase"` so `is_aiterm` → `isAiterm` — Task 7's frontend
must use `isAiterm`.
