# Claude Code configuration panel — Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A read-only panel inside Settings → Agents → Claude Code that shows the layered `settings.json`, the `CLAUDE.md` chain, MCP registrations and skills, plus the flags aiterm itself injects.

**Architecture:** A new `claudecfg` directory module holds four independent readers, each a pure function over strings that a thin `#[tauri::command]` feeds from disk. The frontend adds a drill-down inside the existing Settings modal — a hub with four buttons, one section each. Nothing writes.

**Tech Stack:** Rust (serde, serde_json with `preserve_order`), Tauri 2 commands, React + TypeScript, `node --experimental-strip-types --test` for pure-TS tests.

**Spec:** `docs/design/2026-08-06-claude-config-panel-design.md`

## Global Constraints

- Read-only. Phase 1 writes no Claude file, ever. No `fs::write`, no `File::create` anywhere in `claudecfg`.
- No new crate dependencies. `serde`, `serde_json` (already `features = ["preserve_order"]`) and `std` only.
- No `agent === "claude"` / `a.id == "claude"` checks in the frontend for gating. Gate on `Caps.config`.
- Launch flags displayed in the panel come from one definition in `agents.rs` that `ClaudeBackend::launch()` itself uses. Never a second copy.
- A missing file is "not present", not an error. A malformed file reports its parse error and drops out of resolution without blanking the panel.
- `~/.claude.json` is 155 KB with 49 projects; read only the keys named in each task, never display it whole.
- Every task ends `cargo test` green (currently 199 lib + 14 integration), `npx tsc --noEmit` clean, `npm run test:ui` green (currently 18), and clippy at its two pre-existing warnings (`pty.rs`, `watcher.rs`).
- Test names are sentences describing the behaviour, matching the house style in `launch.rs` (`a_refusal_names_the_engine_that_refused`).

---

### Task 1: `Caps.config` and a single source for the launch flags

**Files:**
- Modify: `src-tauri/src/agents.rs` (the `Caps` struct at line 54, `ClaudeBackend::caps()`, `ClaudeBackend::launch()` at ~line 337)
- Modify: `src/ipc.ts` (the `Caps` interface at line 388)

**Interfaces:**
- Consumes: nothing.
- Produces: `pub const CLAUDE_LAUNCH_FLAGS: &[&str]` in `agents.rs`; `Caps { config: bool, .. }` in Rust and TS.

- [ ] **Step 1: Write the failing tests**

In `src-tauri/src/agents.rs`, in `mod tests`:

```rust
#[test]
fn the_launcher_uses_the_flag_list_the_panel_will_show() {
    // A panel that re-typed these would drift from what is actually run, and
    // this is the surface where being wrong is worst.
    let cmd = ClaudeBackend.launch(&LaunchSpec::default());
    for flag in CLAUDE_LAUNCH_FLAGS {
        assert!(cmd.contains(flag), "{flag} missing from {cmd}");
    }
}

#[test]
fn only_an_engine_with_a_config_surface_offers_one() {
    assert!(ClaudeBackend.caps().config, "claude has settings.json to show");
    assert!(!CodexBackend.caps().config, "nothing is read for codex yet");
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cd src-tauri && cargo test the_launcher_uses_the_flag_list only_an_engine_with_a_config`
Expected: FAIL to compile — `CLAUDE_LAUNCH_FLAGS` not found, no field `config` on `Caps`.

- [ ] **Step 3: Implement**

In `agents.rs`, above `impl AgentBackend for ClaudeBackend`:

```rust
/// The flags every claude aiterm launches carries.
///
/// Public and used by the launcher below, so the configuration panel can show
/// what a session actually starts with instead of a hand-copied list that will
/// eventually disagree with it.
///
/// `--allow-dangerously-skip-permissions` is the consequential one: it disables
/// the permission prompt outright. Kept from the frontend's old `CLAUDE_CMD`,
/// reasoning unchanged.
pub const CLAUDE_LAUNCH_FLAGS: &[&str] =
    &["--permission-mode auto", "--allow-dangerously-skip-permissions"];
```

Change `launch()`'s first line from the literal string to:

```rust
let mut cmd = format!("claude {}", CLAUDE_LAUNCH_FLAGS.join(" "));
```

Add to `Caps`:

```rust
    /// The engine has configuration aiterm can read and show — the Settings
    /// button on its row in the Agents pane. Off by default: an engine whose
    /// config aiterm cannot read is better with no button than with an empty
    /// panel.
    pub config: bool,
```

Set `config: true` in `ClaudeBackend::caps()`. Every other backend uses
`..Default::default()` already, so they stay false.

Add to `src/ipc.ts`'s `Caps`:

```ts
  /** The engine has configuration aiterm can read — its Settings button. */
  config: boolean;
```

- [ ] **Step 4: Run the tests**

Run: `cd src-tauri && cargo test` — expected: all pass, count 201.
Run: `npx tsc --noEmit` — expected: clean.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agents.rs src/ipc.ts
git commit -m "One list of launch flags, and a cap for having config to show"
```

---

### Task 2: Settings layers and precedence

**Files:**
- Create: `src-tauri/src/claudecfg/mod.rs`
- Create: `src-tauri/src/claudecfg/settings.rs`
- Modify: `src-tauri/src/lib.rs` (add `pub mod claudecfg;` next to `pub mod chat;`)

A directory module rather than one flat file: four independent readers land here, and `agents.rs` at 1600 lines is the warning about letting them share one.

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub enum LayerId { User, Project, ProjectLocal, Injected }`
  - `pub struct Layer { pub id: LayerId, pub path: String, pub present: bool, pub error: Option<String> }`
  - `pub struct SetIn { pub layer: LayerId, pub value: serde_json::Value }`
  - `pub struct Setting { pub key: String, pub concern: String, pub effective: serde_json::Value, pub winner: LayerId, pub set_in: Vec<SetIn> }`
  - `pub fn resolve(layers: &[(LayerId, &str)]) -> (Vec<Setting>, Vec<String>)` — settings plus per-layer parse errors

- [ ] **Step 1: Write the failing tests**

`src-tauri/src/claudecfg/settings.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = r#"{"model": "claude-opus-5", "cleanupPeriodDays": 30}"#;
    const PROJECT: &str = r#"{"model": "sonnet", "worktree": {"bgIsolation": "none"}}"#;
    const LOCAL: &str = r#"{"model": "haiku"}"#;

    fn layered() -> Vec<(LayerId, &'static str)> {
        vec![
            (LayerId::User, USER),
            (LayerId::Project, PROJECT),
            (LayerId::ProjectLocal, LOCAL),
        ]
    }

    fn find<'a>(s: &'a [Setting], key: &str) -> &'a Setting {
        s.iter().find(|x| x.key == key).expect(key)
    }

    #[test]
    fn the_most_local_layer_wins() {
        let (s, _) = resolve(&layered());
        assert_eq!(find(&s, "model").winner, LayerId::ProjectLocal);
        assert_eq!(find(&s, "model").effective, serde_json::json!("haiku"));
    }

    #[test]
    fn every_layer_that_set_a_key_is_reported_not_just_the_winner() {
        // "project overrides user" is the display this exists for.
        let (s, _) = resolve(&layered());
        let layers: Vec<_> = find(&s, "model").set_in.iter().map(|x| x.layer).collect();
        assert_eq!(layers, vec![LayerId::User, LayerId::Project, LayerId::ProjectLocal]);
    }

    #[test]
    fn a_key_only_one_layer_sets_still_appears() {
        let (s, _) = resolve(&layered());
        assert_eq!(find(&s, "cleanupPeriodDays").winner, LayerId::User);
        assert_eq!(find(&s, "worktree.bgIsolation").winner, LayerId::Project);
    }

    #[test]
    fn an_injected_layer_outranks_the_project() {
        // aiterm's own --settings file sits at CLI level, above project files.
        let mut l = layered();
        l.push((LayerId::Injected, r#"{"model": "fable"}"#));
        let (s, _) = resolve(&l);
        assert_eq!(find(&s, "model").winner, LayerId::Injected);
    }

    #[test]
    fn a_malformed_layer_reports_its_error_and_leaves_the_rest_readable() {
        // The panel is most likely opened when a file is broken.
        let (s, errors) = resolve(&[
            (LayerId::User, USER),
            (LayerId::Project, "{ this is not json"),
        ]);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("project"), "{errors:?}");
        assert_eq!(find(&s, "model").winner, LayerId::User);
    }

    #[test]
    fn nested_objects_are_flattened_to_dotted_keys() {
        let (s, _) = resolve(&[(LayerId::User, r#"{"permissions": {"deny": ["Bash"]}}"#)]);
        assert_eq!(find(&s, "permissions.deny").effective, serde_json::json!(["Bash"]));
    }

    #[test]
    fn a_leaf_array_is_a_value_not_a_branch_to_walk_into() {
        // permissions.deny is a list of rules; walking into it would produce
        // permissions.deny.0 and lose the shape the user recognises.
        let (s, _) = resolve(&[(LayerId::User, r#"{"a": {"b": [1, 2]}}"#)]);
        assert!(s.iter().all(|x| x.key != "a.b.0"), "{:?}", s.iter().map(|x| &x.key).collect::<Vec<_>>());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd src-tauri && cargo test claudecfg::settings`
Expected: FAIL to compile — module does not exist.

- [ ] **Step 3: Implement**

`src-tauri/src/claudecfg/mod.rs`:

```rust
//! Everything Claude Code reads that decides how a session behaves, gathered
//! for display. Read-only by design: these are files every session on the
//! machine depends on, and Phase 1 shows them without touching them.

pub mod settings;
```

`src-tauri/src/claudecfg/settings.rs`:

```rust
//! The layered settings, resolved.
//!
//! Claude merges several files, most local winning. The panel needs more than
//! the winner — "project overrides user" is the useful sentence — so every
//! layer that sets a key is carried through.

use serde::Serialize;
use serde_json::{Map, Value};

/// Ordered lowest-precedence first. `resolve` relies on that order rather than
/// on a comparison, so adding a layer means putting it in the right place here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LayerId {
    /// `~/.claude/settings.json`
    User,
    /// `<project>/.claude/settings.json`
    Project,
    /// `<project>/.claude/settings.local.json`
    ProjectLocal,
    /// aiterm's own file, passed with `--settings`, which sits at CLI level.
    Injected,
}

impl LayerId {
    pub fn label(self) -> &'static str {
        match self {
            LayerId::User => "user",
            LayerId::Project => "project",
            LayerId::ProjectLocal => "project local",
            LayerId::Injected => "aiterm",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Layer {
    pub id: LayerId,
    pub path: String,
    pub present: bool,
    /// Why this layer could not be used, when it exists but did not parse.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetIn {
    pub layer: LayerId,
    pub value: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Setting {
    /// Dotted path — `permissions.deny`, not a nested tree, so the UI is a list.
    pub key: String,
    pub concern: String,
    pub effective: Value,
    pub winner: LayerId,
    /// Lowest-precedence first, so the last entry is always the winner.
    pub set_in: Vec<SetIn>,
}

/// Walk an object into dotted leaves. Arrays are leaves: `permissions.deny` is
/// a list of rules the user recognises, and `permissions.deny.0` is not.
fn flatten(prefix: &str, map: &Map<String, Value>, out: &mut Vec<(String, Value)>) {
    for (k, v) in map {
        let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
        match v {
            Value::Object(inner) if !inner.is_empty() => flatten(&key, inner, out),
            _ => out.push((key, v.clone())),
        }
    }
}

/// Resolve layers given lowest-precedence first. Returns the settings and one
/// error string per layer that existed but did not parse.
pub fn resolve(layers: &[(LayerId, &str)]) -> (Vec<Setting>, Vec<String>) {
    let mut errors = Vec::new();
    let mut order: Vec<String> = Vec::new();
    let mut found: std::collections::HashMap<String, Vec<SetIn>> = std::collections::HashMap::new();

    for (id, text) in layers {
        let parsed: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => {
                errors.push(format!("{}: {e}", id.label()));
                continue;
            }
        };
        let Some(map) = parsed.as_object() else {
            errors.push(format!("{}: not a JSON object", id.label()));
            continue;
        };
        let mut leaves = Vec::new();
        flatten("", map, &mut leaves);
        for (key, value) in leaves {
            if !found.contains_key(&key) {
                order.push(key.clone());
            }
            found.entry(key).or_default().push(SetIn { layer: *id, value });
        }
    }

    let settings = order
        .into_iter()
        .map(|key| {
            let set_in = found.remove(&key).unwrap_or_default();
            let last = set_in.last().expect("a key exists because a layer set it");
            Setting {
                concern: super::concern::of(&key).to_string(),
                effective: last.value.clone(),
                winner: last.layer,
                key,
                set_in,
            }
        })
        .collect();

    (settings, errors)
}
```

Add `pub mod concern;` to `mod.rs` — Task 3 creates it. To keep this task
compiling on its own, create `src-tauri/src/claudecfg/concern.rs` now with the
minimal body Task 3 will test against:

```rust
//! Which group of the panel a setting belongs in. Filled in by Task 3.
pub fn of(_key: &str) -> &'static str {
    "Other"
}
```

Add `pub mod claudecfg;` to `src-tauri/src/lib.rs` beside `pub mod chat;`.

- [ ] **Step 4: Run the tests**

Run: `cd src-tauri && cargo test claudecfg::settings`
Expected: 7 pass.
Run: `cargo test` — expected all green, and `cargo clippy --all-targets 2>&1 | grep -cE "^warning: this"` still `2`.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/claudecfg src-tauri/src/lib.rs
git commit -m "Resolve Claude's settings layers, and say which one won"
```

---

### Task 3: Grouping by concern, with no schema to age

**Files:**
- Modify: `src-tauri/src/claudecfg/concern.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `pub fn of(key: &str) -> &'static str` — already called by `settings::resolve`.

- [ ] **Step 1: Write the failing tests**

Append to `concern.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_keys_land_in_their_group() {
        assert_eq!(of("model"), "Model");
        assert_eq!(of("permissions.deny"), "Permissions");
        assert_eq!(of("hooks.SessionStart"), "Hooks");
        assert_eq!(of("env.FOO"), "Environment");
        assert_eq!(of("mcpServers.chrome"), "MCP");
        assert_eq!(of("preferredNotifChannel"), "Notifications & UI");
        assert_eq!(of("cleanupPeriodDays"), "Housekeeping");
    }

    #[test]
    fn a_key_we_have_never_heard_of_is_shown_rather_than_hidden() {
        // The failure mode a hardcoded schema has: silently omitting a setting
        // that is genuinely in effect.
        assert_eq!(of("worktree.bgIsolation"), "Other");
        assert_eq!(of("somethingClaudeAddedLastTuesday"), "Other");
    }

    #[test]
    fn grouping_reads_the_root_of_a_dotted_key() {
        // permissions.deny and permissions.allow are one concern, not two.
        assert_eq!(of("permissions.allow"), of("permissions.deny"));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd src-tauri && cargo test claudecfg::concern`
Expected: FAIL — everything returns "Other".

- [ ] **Step 3: Implement**

Replace `concern.rs`'s body:

```rust
//! Which group of the panel a setting belongs in.
//!
//! A lookup on the key's root rather than a schema of every setting Claude
//! supports. `ClaudeBackend::models()` already carries a comment about
//! hardcoded Claude knowledge ageing; a settings schema would age the same way,
//! and its failure mode is worse — quietly omitting a key that is in effect.
//! Anything unrecognised is shown under "Other", which is plain but never a lie.

const GROUPS: &[(&str, &str)] = &[
    ("model", "Model"),
    ("fallbackModel", "Model"),
    ("permissions", "Permissions"),
    ("defaultMode", "Permissions"),
    ("additionalDirectories", "Permissions"),
    ("hooks", "Hooks"),
    ("env", "Environment"),
    ("apiKeyHelper", "Environment"),
    ("mcpServers", "MCP"),
    ("enableAllProjectMcpServers", "MCP"),
    ("enabledMcpjsonServers", "MCP"),
    ("disabledMcpjsonServers", "MCP"),
    ("preferredNotifChannel", "Notifications & UI"),
    ("statusLine", "Notifications & UI"),
    ("outputStyle", "Notifications & UI"),
    ("theme", "Notifications & UI"),
    ("cleanupPeriodDays", "Housekeeping"),
    ("includeCoAuthoredBy", "Housekeeping"),
];

/// The order groups appear in the panel. "Other" last: it is the overflow, and
/// a reader should meet the settings we can explain first.
pub const ORDER: &[&str] = &[
    "Model",
    "Permissions",
    "Hooks",
    "Environment",
    "MCP",
    "Notifications & UI",
    "Housekeeping",
    "Other",
];

pub fn of(key: &str) -> &'static str {
    let root = key.split('.').next().unwrap_or(key);
    GROUPS
        .iter()
        .find(|(k, _)| *k == root)
        .map(|(_, group)| *group)
        .unwrap_or("Other")
}
```

- [ ] **Step 4: Run the tests**

Run: `cd src-tauri && cargo test claudecfg`
Expected: 10 pass (7 settings + 3 concern).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/claudecfg/concern.rs
git commit -m "Group settings by concern without a schema that will age"
```

---

### Task 4: The CLAUDE.md chain, imports followed

**Files:**
- Create: `src-tauri/src/claudecfg/instructions.rs`
- Modify: `src-tauri/src/claudecfg/mod.rs` (add `pub mod instructions;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub struct Doc { pub path: String, pub present: bool, pub lines: usize, pub imports: Vec<Doc>, pub source: String }`
  - `pub fn chain(roots: &[(String, String)], read: &mut dyn FnMut(&str) -> Option<String>) -> Vec<Doc>` — `roots` is `(source label, path)`; `read` returns file text or `None` when absent, so the walk is testable without a filesystem.

- [ ] **Step 1: Write the failing tests**

`src-tauri/src/claudecfg/instructions.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn reader(files: Vec<(&str, &str)>) -> impl FnMut(&str) -> Option<String> {
        let map: HashMap<String, String> =
            files.into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |p: &str| map.get(p).cloned()
    }

    #[test]
    fn a_document_reports_its_length() {
        let mut r = reader(vec![("/h/CLAUDE.md", "one\ntwo\nthree")]);
        let docs = chain(&[("user".into(), "/h/CLAUDE.md".into())], &mut r);
        assert!(docs[0].present);
        assert_eq!(docs[0].lines, 3);
    }

    #[test]
    fn an_import_is_followed_and_nested_under_the_file_that_pulled_it() {
        // The real case here: the global CLAUDE.md imports RTK.md, so a reader
        // that stopped at depth 1 would show the wrong instructions.
        let mut r = reader(vec![
            ("/h/CLAUDE.md", "@RTK.md\nrules"),
            ("/h/RTK.md", "rtk"),
        ]);
        let docs = chain(&[("user".into(), "/h/CLAUDE.md".into())], &mut r);
        assert_eq!(docs[0].imports.len(), 1);
        assert_eq!(docs[0].imports[0].path, "/h/RTK.md");
        assert!(docs[0].imports[0].present);
    }

    #[test]
    fn an_import_that_is_not_there_is_shown_as_missing_not_skipped() {
        let mut r = reader(vec![("/h/CLAUDE.md", "@gone.md")]);
        let docs = chain(&[("user".into(), "/h/CLAUDE.md".into())], &mut r);
        assert_eq!(docs[0].imports.len(), 1);
        assert!(!docs[0].imports[0].present);
    }

    #[test]
    fn a_cycle_terminates() {
        let mut r = reader(vec![("/h/a.md", "@b.md"), ("/h/b.md", "@a.md")]);
        let docs = chain(&[("user".into(), "/h/a.md".into())], &mut r);
        // b is reached once; a is not re-entered.
        assert_eq!(docs[0].imports[0].path, "/h/b.md");
        assert!(docs[0].imports[0].imports.is_empty());
    }

    #[test]
    fn an_absent_root_is_present_in_the_list_and_marked_absent() {
        // "./CLAUDE.md — not present" is the useful answer for a project.
        let mut r = reader(vec![]);
        let docs = chain(&[("project".into(), "/p/CLAUDE.md".into())], &mut r);
        assert_eq!(docs.len(), 1);
        assert!(!docs[0].present);
        assert_eq!(docs[0].lines, 0);
    }

    #[test]
    fn an_at_sign_mid_sentence_is_not_an_import() {
        let mut r = reader(vec![("/h/CLAUDE.md", "email me @ matt@example.com")]);
        let docs = chain(&[("user".into(), "/h/CLAUDE.md".into())], &mut r);
        assert!(docs[0].imports.is_empty());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd src-tauri && cargo test claudecfg::instructions`
Expected: FAIL to compile — module does not exist.

- [ ] **Step 3: Implement**

```rust
//! The CLAUDE.md chain, in load order, with `@imports` followed.
//!
//! Depth matters in practice: the global file on this machine imports RTK.md,
//! so a reader that stopped at the roots would report the wrong instructions.

use serde::Serialize;

/// How deep imports are followed. Well past anything real, and a stop that
/// cannot be reached by accident.
const MAX_DEPTH: usize = 8;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Doc {
    /// "user", "project", or "import" — what put this file in the chain.
    pub source: String,
    pub path: String,
    pub present: bool,
    pub lines: usize,
    pub imports: Vec<Doc>,
}

/// An `@path` import: the whole line, leading whitespace aside, must be the
/// reference. Anything else is prose that happens to contain an at-sign.
fn import_of(line: &str) -> Option<&str> {
    let t = line.trim();
    let rest = t.strip_prefix('@')?;
    if rest.is_empty() || rest.contains(char::is_whitespace) {
        return None;
    }
    Some(rest)
}

fn dir_of(path: &str) -> String {
    match path.rfind('/') {
        Some(i) => path[..i].to_string(),
        None => ".".to_string(),
    }
}

fn resolve_import(from: &str, target: &str) -> String {
    if target.starts_with('/') {
        return target.to_string();
    }
    format!("{}/{}", dir_of(from), target)
}

fn walk(
    source: &str,
    path: &str,
    read: &mut dyn FnMut(&str) -> Option<String>,
    seen: &mut Vec<String>,
    depth: usize,
) -> Doc {
    let text = read(path);
    let present = text.is_some();
    let body = text.unwrap_or_default();
    let lines = if present { body.lines().count() } else { 0 };

    let mut imports = Vec::new();
    if present && depth < MAX_DEPTH {
        for line in body.lines() {
            let Some(target) = import_of(line) else { continue };
            let full = resolve_import(path, target);
            if seen.contains(&full) {
                continue; // a cycle, or the same file pulled in twice
            }
            seen.push(full.clone());
            imports.push(walk("import", &full, read, seen, depth + 1));
        }
    }

    Doc { source: source.to_string(), path: path.to_string(), present, lines, imports }
}

/// `roots` is (source label, path), in load order.
pub fn chain(
    roots: &[(String, String)],
    read: &mut dyn FnMut(&str) -> Option<String>,
) -> Vec<Doc> {
    let mut seen: Vec<String> = roots.iter().map(|(_, p)| p.clone()).collect();
    roots.iter().map(|(src, p)| walk(src, p, read, &mut seen, 0)).collect()
}
```

Add `pub mod instructions;` to `claudecfg/mod.rs`.

- [ ] **Step 4: Run the tests**

Run: `cd src-tauri && cargo test claudecfg`
Expected: 16 pass.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/claudecfg
git commit -m "Read the CLAUDE.md chain, imports and all"
```

---

### Task 5: MCP registrations, from where they actually live

**Files:**
- Create: `src-tauri/src/claudecfg/mcp.rs`
- Modify: `src-tauri/src/claudecfg/mod.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub struct Server { pub name: String, pub scope: String, pub command: Option<String>, pub enabled: Option<bool> }`
  - `pub fn read(claude_json: Option<&str>, mcp_json: Option<&str>, project: &str) -> (Vec<Server>, bool)` — the bool is `local_config_read`, so "none configured" can be told from "nothing was readable"

- [ ] **Step 1: Write the failing tests**

`src-tauri/src/claudecfg/mcp.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE_JSON: &str = r#"{
        "mcpServers": {"chrome": {"command": "claude-in-chrome"}},
        "projects": {
            "/p": {"enabledMcpjsonServers": ["repo"], "disabledMcpjsonServers": ["old"]}
        }
    }"#;

    const MCP_JSON: &str = r#"{"mcpServers": {"repo": {"command": "node server.js"}, "old": {"command": "x"}}}"#;

    #[test]
    fn a_user_scope_server_is_listed_with_its_command() {
        let (s, read) = read(Some(CLAUDE_JSON), None, "/p");
        assert!(read);
        let chrome = s.iter().find(|x| x.name == "chrome").expect("chrome");
        assert_eq!(chrome.scope, "user");
        assert_eq!(chrome.command.as_deref(), Some("claude-in-chrome"));
    }

    #[test]
    fn a_project_server_carries_whether_this_project_enabled_it() {
        let (s, _) = read(Some(CLAUDE_JSON), Some(MCP_JSON), "/p");
        let repo = s.iter().find(|x| x.name == "repo").expect("repo");
        assert_eq!(repo.scope, "project");
        assert_eq!(repo.enabled, Some(true));
        let old = s.iter().find(|x| x.name == "old").expect("old");
        assert_eq!(old.enabled, Some(false));
    }

    #[test]
    fn no_local_config_is_reported_as_read_and_empty_not_as_unread() {
        // Observed 2026-08-06: mcpServers is empty here and there is no
        // .mcp.json, while sessions plainly have MCP tools — those are
        // claude.ai connectors, in no local file. "Empty" and "could not read"
        // must not look the same, or the panel implies there is no MCP at all.
        let (s, read) = read(Some(r#"{"mcpServers": {}}"#), None, "/p");
        assert!(read);
        assert!(s.is_empty());
    }

    #[test]
    fn an_unreadable_config_is_not_reported_as_empty() {
        let (s, read) = read(None, None, "/p");
        assert!(!read);
        assert!(s.is_empty());
    }

    #[test]
    fn a_malformed_config_does_not_take_the_other_source_with_it() {
        let (s, read) = read(Some("{ broken"), Some(MCP_JSON), "/p");
        assert!(read, "the project file was still readable");
        assert!(s.iter().any(|x| x.name == "repo"));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd src-tauri && cargo test claudecfg::mcp`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

```rust
//! MCP servers Claude has registered locally.
//!
//! Its own reader rather than a slice of the settings layers, because MCP does
//! not live in settings.json: user scope is `~/.claude.json`'s `mcpServers`,
//! project scope is a checked-in `.mcp.json`, and whether this project trusts a
//! project server is a per-project list inside `~/.claude.json`.
//!
//! Servers reached as claude.ai connectors appear in none of these files. The
//! caller must say so, or an empty list reads as "no MCP" when there is plenty.

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Server {
    pub name: String,
    /// "user" or "project".
    pub scope: String,
    pub command: Option<String>,
    /// Only meaningful for project scope, where a project opts in per server.
    pub enabled: Option<bool>,
}

fn command_of(v: &Value) -> Option<String> {
    v.get("command")?.as_str().map(str::to_string)
}

fn names(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// `project` is the absolute path used as the key inside `~/.claude.json`.
/// Returns the servers and whether any local config could be read at all.
pub fn read(
    claude_json: Option<&str>,
    mcp_json: Option<&str>,
    project: &str,
) -> (Vec<Server>, bool) {
    let mut out = Vec::new();
    let mut any_read = false;

    let user: Option<Value> = claude_json.and_then(|t| serde_json::from_str(t).ok());
    if let Some(root) = &user {
        any_read = true;
        if let Some(map) = root.get("mcpServers").and_then(Value::as_object) {
            for (name, v) in map {
                out.push(Server {
                    name: name.clone(),
                    scope: "user".into(),
                    command: command_of(v),
                    enabled: None,
                });
            }
        }
    }

    if let Some(root) = mcp_json.and_then(|t| serde_json::from_str::<Value>(t).ok()) {
        any_read = true;
        let entry = user
            .as_ref()
            .and_then(|u| u.get("projects"))
            .and_then(|p| p.get(project))
            .cloned()
            .unwrap_or(Value::Null);
        let on = names(&entry, "enabledMcpjsonServers");
        let off = names(&entry, "disabledMcpjsonServers");
        if let Some(map) = root.get("mcpServers").and_then(Value::as_object) {
            for (name, v) in map {
                let enabled = if on.contains(name) {
                    Some(true)
                } else if off.contains(name) {
                    Some(false)
                } else {
                    None
                };
                out.push(Server {
                    name: name.clone(),
                    scope: "project".into(),
                    command: command_of(v),
                    enabled,
                });
            }
        }
    }

    (out, any_read)
}
```

Add `pub mod mcp;` to `claudecfg/mod.rs`.

- [ ] **Step 4: Run the tests**

Run: `cd src-tauri && cargo test claudecfg`
Expected: 21 pass.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/claudecfg
git commit -m "Read MCP from the files it actually lives in"
```

---

### Task 6: Skills, resolved rather than globbed

**Files:**
- Create: `src-tauri/src/claudecfg/skills.rs`
- Modify: `src-tauri/src/claudecfg/mod.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub struct Skill { pub name: String, pub description: String, pub source: String, pub path: String }`
  - `pub fn plugin_roots(installed_plugins_json: &str) -> Vec<(String, String)>` — `(plugin label, skills dir)`
  - `pub fn frontmatter(text: &str) -> (String, String)` — `(name, description)`

- [ ] **Step 1: Write the failing tests**

`src-tauri/src/claudecfg/skills.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const INSTALLED: &str = r#"{
      "version": 2,
      "plugins": {
        "superpowers@claude-plugins-official": [
          {"scope": "user",
           "installPath": "/h/.claude/plugins/cache/claude-plugins-official/superpowers/6.2.0",
           "version": "6.2.0"}
        ],
        "document-skills@anthropic-agent-skills": [
          {"scope": "user",
           "installPath": "/h/.claude/plugins/cache/anthropic-agent-skills/document-skills/fa0fa64bdc96",
           "version": "fa0fa64bdc96"}
        ]
      }
    }"#;

    #[test]
    fn a_plugins_skills_directory_comes_from_its_recorded_install_path() {
        // The cache holds three versions of document-skills. Globbing it would
        // list every skill three times; installed_plugins.json names the live
        // one, so nothing is guessed.
        let roots = plugin_roots(INSTALLED);
        assert!(roots.iter().any(|(label, dir)| label == "superpowers"
            && dir == "/h/.claude/plugins/cache/claude-plugins-official/superpowers/6.2.0/skills"));
        assert_eq!(roots.len(), 2, "one root per installed plugin, not per cached version");
    }

    #[test]
    fn a_plugin_with_no_install_path_is_skipped_rather_than_guessed() {
        let roots = plugin_roots(r#"{"plugins": {"broken@x": [{"scope": "user"}]}}"#);
        assert!(roots.is_empty());
    }

    #[test]
    fn a_malformed_record_yields_no_roots_rather_than_failing_the_panel() {
        assert!(plugin_roots("{ not json").is_empty());
    }

    #[test]
    fn a_skill_is_named_and_described_by_its_frontmatter() {
        let text = "---\nname: deploy-rpm\ndescription: Install an RPM on Matt's machines\n---\n\nbody\n";
        assert_eq!(
            frontmatter(text),
            ("deploy-rpm".to_string(), "Install an RPM on Matt's machines".to_string())
        );
    }

    #[test]
    fn a_skill_with_no_frontmatter_still_gets_a_row() {
        // A skill that exists is worth listing even if it is undocumented.
        assert_eq!(frontmatter("just a body"), (String::new(), String::new()));
    }

    #[test]
    fn a_description_running_past_the_frontmatter_is_not_swallowed_whole() {
        let text = "---\nname: a\ndescription: one\n---\ndescription: two\n";
        assert_eq!(frontmatter(text).1, "one");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd src-tauri && cargo test claudecfg::skills`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

```rust
//! Skills available to a Claude session, and which tree each came from.
//!
//! User and project skills are one directory each. Plugin skills are not:
//! the plugin cache keeps several versions of the same plugin side by side
//! (`document-skills` is here at three version hashes), so globbing it reports
//! every skill two or three times. `installed_plugins.json` records an
//! `installPath` per installed plugin, which is the only non-guess available.

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// "user", "project", or the plugin's name.
    pub source: String,
    pub path: String,
}

/// `(label, skills directory)` for every installed plugin, from the record that
/// names the live version.
pub fn plugin_roots(installed_plugins_json: &str) -> Vec<(String, String)> {
    let Ok(root) = serde_json::from_str::<Value>(installed_plugins_json) else {
        return Vec::new();
    };
    let Some(plugins) = root.get("plugins").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (id, entries) in plugins {
        // "superpowers@claude-plugins-official" reads better as "superpowers".
        let label = id.split('@').next().unwrap_or(id).to_string();
        let Some(first) = entries.as_array().and_then(|a| a.first()) else { continue };
        let Some(path) = first.get("installPath").and_then(Value::as_str) else { continue };
        out.push((label, format!("{path}/skills")));
    }
    out.sort();
    out
}

/// `name` and `description` from a SKILL.md's YAML frontmatter. Absent fields
/// come back empty: a skill that exists is worth a row even undocumented.
pub fn frontmatter(text: &str) -> (String, String) {
    let mut name = String::new();
    let mut description = String::new();
    let mut in_front = false;
    for line in text.lines() {
        if line.trim() == "---" {
            if in_front {
                break; // only the first block counts
            }
            in_front = true;
            continue;
        }
        if !in_front {
            break; // no frontmatter at all
        }
        if let Some(v) = line.strip_prefix("name:") {
            name = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("description:") {
            description = v.trim().to_string();
        }
    }
    (name, description)
}
```

Add `pub mod skills;` to `claudecfg/mod.rs`.

- [ ] **Step 4: Run the tests**

Run: `cd src-tauri && cargo test claudecfg`
Expected: 27 pass.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/claudecfg
git commit -m "List skills from the install record, not the cache"
```

---

### Task 7: The commands, wired to disk

**Files:**
- Modify: `src-tauri/src/claudecfg/mod.rs`
- Modify: `src-tauri/src/lib.rs` (register four commands beside `rendercost::renderer_probe`)

**Interfaces:**
- Consumes: `settings::resolve`, `instructions::chain`, `mcp::read`, `skills::{plugin_roots, frontmatter}`, `agents::CLAUDE_LAUNCH_FLAGS`.
- Produces: commands `claude_settings`, `claude_instructions`, `claude_mcp`, `claude_skills`, each taking `project: Option<String>`.

- [ ] **Step 1: Write the failing tests**

In `claudecfg/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_paths_are_built_from_home_and_project() {
        let l = layer_paths("/h", Some("/p"));
        assert_eq!(l[0].1, "/h/.claude/settings.json");
        assert_eq!(l[1].1, "/p/.claude/settings.json");
        assert_eq!(l[2].1, "/p/.claude/settings.local.json");
        assert!(l[3].1.contains("claude-hook-settings.json"), "{:?}", l[3].1);
    }

    #[test]
    fn with_no_project_open_only_the_user_and_injected_layers_are_looked_for() {
        let l = layer_paths("/h", None);
        assert!(l.iter().all(|(id, _)| *id != settings::LayerId::Project));
        assert_eq!(l.len(), 2);
    }

    #[test]
    fn the_flags_reported_are_the_launchers_own() {
        // Not a second copy that can drift from what is run.
        assert_eq!(injected_flags(), crate::agents::CLAUDE_LAUNCH_FLAGS);
    }

    /// Reads the real home directory, so it is skipped where there is none.
    #[test]
    fn a_real_read_answers_without_panicking() {
        if std::env::var("HOME").is_err() {
            return;
        }
        let s = claude_settings(None);
        // Whatever is on this machine, the shape must be answerable.
        assert!(s.layers.iter().any(|l| l.id == settings::LayerId::User));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd src-tauri && cargo test claudecfg::tests`
Expected: FAIL to compile — `layer_paths`, `injected_flags`, `claude_settings` missing.

- [ ] **Step 3: Implement**

Append to `claudecfg/mod.rs`:

```rust
pub mod concern;
pub mod instructions;
pub mod mcp;
pub mod settings;
pub mod skills;

use serde::Serialize;
use settings::{Layer, LayerId, Setting};

fn home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/".into())
}

/// Every settings file, lowest precedence first. Paths only — reading happens
/// in the command, so this is testable without a filesystem.
fn layer_paths(home: &str, project: Option<&str>) -> Vec<(LayerId, String)> {
    let mut out = vec![(LayerId::User, format!("{home}/.claude/settings.json"))];
    if let Some(p) = project {
        out.push((LayerId::Project, format!("{p}/.claude/settings.json")));
        out.push((LayerId::ProjectLocal, format!("{p}/.claude/settings.local.json")));
    }
    out.push((
        LayerId::Injected,
        format!("{home}/.local/share/aiterm/claude-hook-settings.json"),
    ));
    out
}

/// The flags aiterm adds to every claude launch — the launcher's own list.
fn injected_flags() -> &'static [&'static str] {
    crate::agents::CLAUDE_LAUNCH_FLAGS
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    pub layers: Vec<Layer>,
    pub settings: Vec<Setting>,
    /// Parse failures, one per unusable layer.
    pub errors: Vec<String>,
    /// Group order for the panel.
    pub order: Vec<String>,
    pub injected_flags: Vec<String>,
}

#[tauri::command]
pub fn claude_settings(project: Option<String>) -> SettingsView {
    let h = home();
    let paths = layer_paths(&h, project.as_deref());
    let mut layers = Vec::new();
    let mut texts: Vec<(LayerId, String)> = Vec::new();
    for (id, path) in &paths {
        match std::fs::read_to_string(path) {
            Ok(t) => {
                layers.push(Layer { id: *id, path: path.clone(), present: true, error: None });
                texts.push((*id, t));
            }
            Err(_) => {
                layers.push(Layer { id: *id, path: path.clone(), present: false, error: None })
            }
        }
    }
    let borrowed: Vec<(LayerId, &str)> = texts.iter().map(|(i, t)| (*i, t.as_str())).collect();
    let (settings, errors) = settings::resolve(&borrowed);
    // A layer that existed but did not parse carries its reason on the row.
    for e in &errors {
        if let Some((label, msg)) = e.split_once(": ") {
            if let Some(l) = layers.iter_mut().find(|l| l.id.label() == label) {
                l.error = Some(msg.to_string());
            }
        }
    }
    SettingsView {
        layers,
        settings,
        errors,
        order: concern::ORDER.iter().map(|s| s.to_string()).collect(),
        injected_flags: injected_flags().iter().map(|s| s.to_string()).collect(),
    }
}

#[tauri::command]
pub fn claude_instructions(project: Option<String>) -> Vec<instructions::Doc> {
    let h = home();
    let mut roots = vec![("user".to_string(), format!("{h}/.claude/CLAUDE.md"))];
    if let Some(p) = &project {
        roots.push(("project".to_string(), format!("{p}/CLAUDE.md")));
    }
    let mut read = |path: &str| std::fs::read_to_string(path).ok();
    instructions::chain(&roots, &mut read)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpView {
    pub servers: Vec<mcp::Server>,
    /// False when no local config could be read at all, which is a different
    /// answer from "none configured".
    pub local_config_read: bool,
}

#[tauri::command]
pub fn claude_mcp(project: Option<String>) -> McpView {
    let h = home();
    let claude_json = std::fs::read_to_string(format!("{h}/.claude.json")).ok();
    let mcp_json = project
        .as_ref()
        .and_then(|p| std::fs::read_to_string(format!("{p}/.mcp.json")).ok());
    let (servers, local_config_read) = mcp::read(
        claude_json.as_deref(),
        mcp_json.as_deref(),
        project.as_deref().unwrap_or(""),
    );
    McpView { servers, local_config_read }
}

#[tauri::command]
pub fn claude_skills(project: Option<String>) -> Vec<skills::Skill> {
    let h = home();
    let mut roots = vec![("user".to_string(), format!("{h}/.claude/skills"))];
    if let Some(p) = &project {
        roots.push(("project".to_string(), format!("{p}/.claude/skills")));
    }
    let installed = std::fs::read_to_string(format!("{h}/.claude/plugins/installed_plugins.json"))
        .unwrap_or_default();
    roots.extend(skills::plugin_roots(&installed));

    let mut out = Vec::new();
    for (source, dir) in roots {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let path = e.path().join("SKILL.md");
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let (name, description) = skills::frontmatter(&text);
            let dir_name = e.file_name().to_string_lossy().to_string();
            out.push(skills::Skill {
                name: if name.is_empty() { dir_name } else { name },
                description,
                source: source.clone(),
                path: path.to_string_lossy().to_string(),
            });
        }
    }
    out.sort_by(|a, b| (a.source.clone(), a.name.clone()).cmp(&(b.source.clone(), b.name.clone())));
    out
}
```

Remove the now-duplicated `pub mod settings;` line if the earlier task left one.
Register in `lib.rs` beside `rendercost::renderer_probe`:

```rust
            claudecfg::claude_settings,
            claudecfg::claude_instructions,
            claudecfg::claude_mcp,
            claudecfg::claude_skills,
```

- [ ] **Step 4: Run the tests**

Run: `cd src-tauri && cargo test` — expected 31 pass in claudecfg, all green overall.
Run: `cargo clippy --all-targets 2>&1 | grep -cE "^warning: this"` — expected `2`.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src
git commit -m "Four commands that read Claude's configuration"
```

---

### Task 8: IPC surface

**Files:**
- Modify: `src/ipc.ts`

**Interfaces:**
- Consumes: the four commands from Task 7.
- Produces: `claudeSettings`, `claudeInstructions`, `claudeMcp`, `claudeSkills`, and the types `ClaudeSettingsView`, `ClaudeDoc`, `ClaudeMcpView`, `ClaudeSkill`.

- [ ] **Step 1: Add the types and calls**

Append near `agentCaps`:

```ts
/** Which file set a value. `injected` is aiterm's own --settings file. */
export type ClaudeLayerId = "user" | "project" | "projectLocal" | "injected";

export interface ClaudeLayer {
  id: ClaudeLayerId;
  path: string;
  present: boolean;
  /** Why an existing file could not be used. */
  error: string | null;
}

export interface ClaudeSetting {
  key: string;
  concern: string;
  effective: unknown;
  winner: ClaudeLayerId;
  /** Lowest precedence first, so the last entry is the winner. */
  setIn: { layer: ClaudeLayerId; value: unknown }[];
}

export interface ClaudeSettingsView {
  layers: ClaudeLayer[];
  settings: ClaudeSetting[];
  errors: string[];
  order: string[];
  /** Flags aiterm adds to every claude launch, from the launcher's own list. */
  injectedFlags: string[];
}

export interface ClaudeDoc {
  source: string;
  path: string;
  present: boolean;
  lines: number;
  imports: ClaudeDoc[];
}

export interface ClaudeMcpView {
  servers: { name: string; scope: string; command: string | null; enabled: boolean | null }[];
  /** False means nothing local was readable — not the same as none configured. */
  localConfigRead: boolean;
}

export interface ClaudeSkill {
  name: string;
  description: string;
  source: string;
  path: string;
}

export const claudeSettings = (project: string | null) =>
  invoke<ClaudeSettingsView>("claude_settings", { project });
export const claudeInstructions = (project: string | null) =>
  invoke<ClaudeDoc[]>("claude_instructions", { project });
export const claudeMcp = (project: string | null) =>
  invoke<ClaudeMcpView>("claude_mcp", { project });
export const claudeSkills = (project: string | null) =>
  invoke<ClaudeSkill[]>("claude_skills", { project });
```

- [ ] **Step 2: Typecheck**

Run: `npx tsc --noEmit`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add src/ipc.ts
git commit -m "IPC for the Claude configuration reader"
```

---

### Task 9: The drill-down hub

**Files:**
- Create: `src/components/agent-config/ClaudeConfig.tsx`
- Modify: `src/components/SettingsModal.tsx` (the agents pane `.agent-row`, and pane state)
- Modify: `src/App.css`
- Modify: `preview.html` (mocks for the four commands)

**Interfaces:**
- Consumes: `claudeSettings` et al from Task 8; `Caps.config` from Task 1.
- Produces: `<ClaudeConfig agent={AgentDetection} project={string | null} onBack={() => void} />`; hub section state lives inside it.

- [ ] **Step 1: Add the button and the drill-down**

In `SettingsModal.tsx`, add state beside `tab`:

```tsx
  /** Engine whose configuration panel is open, drilled into from Agents. */
  const [configFor, setConfigFor] = useState<AgentDetection | null>(null);
```

In the `.agent-row`, after the path/notes, gated on the cap rather than the id:

```tsx
{capsOf(a.id).config && a.available && (
  <button className="agent-config-btn" onClick={() => setConfigFor(a)}>
    Settings
  </button>
)}
```

`SettingsModal` needs `capsOf` and `activeProject`; add both to its `Props`
(`capsOf: (agent: string) => Caps; activeProject: string | null;`) and pass them
from `App.tsx` where `SettingsModal` is rendered — `capsOf` and `activeProject`
already exist there.

Render the drill-down in place of the agents pane body:

```tsx
{tab === "agents" && configFor && (
  <ClaudeConfig agent={configFor} project={activeProject} onBack={() => setConfigFor(null)} />
)}
{tab === "agents" && !configFor && <>
  … existing agent list …
</>}
```

- [ ] **Step 2: Write the hub**

`src/components/agent-config/ClaudeConfig.tsx`:

```tsx
import { useState } from "react";
import { AgentDetection, homeAbbrev } from "../../ipc";
import SettingsSection from "./SettingsSection";
import InstructionsSection from "./InstructionsSection";
import McpSection from "./McpSection";
import SkillsSection from "./SkillsSection";

type Section = "settings" | "instructions" | "mcp" | "skills";

const TABS: [Section, string][] = [
  ["settings", "Settings"],
  ["instructions", "Instructions"],
  ["mcp", "MCP"],
  ["skills", "Skills"],
];

/** Everything Claude Code reads, inside one panel that says whose it is.
 *
 *  The buttons live in here rather than in the Agents list so the scoping is
 *  never in doubt: nothing in this panel is an aiterm setting. */
export default function ClaudeConfig({ agent, project, onBack }: {
  agent: AgentDetection;
  project: string | null;
  onBack: () => void;
}) {
  const [section, setSection] = useState<Section>("settings");
  return (
    <div className="acfg">
      <div className="acfg-head">
        <button className="acfg-back" onClick={onBack}>← Agents</button>
        <span className="acfg-title">{agent.display_name}</span>
        <span className="acfg-ver">{agent.version ?? "installed"}</span>
        {agent.path && <span className="acfg-path">{homeAbbrev(agent.path)}</span>}
      </div>
      <div className="acfg-tabs">
        {TABS.map(([id, label]) => (
          <button
            key={id}
            className={"acfg-tab" + (section === id ? " on" : "")}
            onClick={() => setSection(id)}
          >{label}</button>
        ))}
      </div>
      {section === "settings" && <SettingsSection project={project} />}
      {section === "instructions" && <InstructionsSection project={project} />}
      {section === "mcp" && <McpSection project={project} />}
      {section === "skills" && <SkillsSection project={project} />}
    </div>
  );
}
```

- [ ] **Step 3: Styles**

Append to `src/App.css`:

```css
/* Claude Code configuration panel — a drill-down from the Agents list. */
.agent-config-btn {
  margin-left: auto;
  background: var(--bg-raised);
  border: 1px solid var(--border);
  border-radius: 6px;
  color: var(--text);
  font-size: 11px;
  padding: 3px 9px;
  cursor: pointer;
}
.agent-config-btn:hover { border-color: color-mix(in srgb, var(--accent) 55%, var(--border)); }
.acfg-head { display: flex; align-items: baseline; gap: 9px; padding-bottom: 10px; }
.acfg-back {
  background: none; border: none; color: var(--text-dim);
  font-size: 12px; cursor: pointer; padding: 0;
}
.acfg-back:hover { color: var(--text); }
.acfg-title { font-size: 13px; font-weight: 600; }
.acfg-ver, .acfg-path { font-size: 11px; color: var(--text-faint); }
.acfg-path { margin-left: auto; }
.acfg-tabs {
  display: flex; gap: 2px; padding: 2px; margin-bottom: 12px;
  background: var(--bg); border: 1px solid var(--border); border-radius: 7px;
}
.acfg-tab {
  flex: 1; background: none; border: none; border-radius: 5px;
  color: var(--text-dim); font-size: 12px; padding: 5px 0; cursor: pointer;
}
.acfg-tab:hover { color: var(--text); }
.acfg-tab.on {
  background: var(--bg-raised); color: var(--text);
  box-shadow: 0 0 0 1px color-mix(in srgb, var(--accent) 45%, transparent);
}
.acfg-empty { font-size: 11px; color: var(--text-faint); padding: 8px 0; }
.acfg-err { font-size: 11px; color: var(--red); padding: 4px 0; }
```

- [ ] **Step 4: Harness mocks**

In `preview.html`'s `mocks`, add:

```js
          claude_settings: {
            layers: [
              { id: "user", path: "/home/matt/.claude/settings.json", present: true, error: null },
              { id: "project", path: "/p/.claude/settings.json", present: true, error: null },
              { id: "projectLocal", path: "/p/.claude/settings.local.json", present: false, error: null },
              { id: "injected", path: "/home/matt/.local/share/aiterm/claude-hook-settings.json", present: true, error: null },
            ],
            settings: [
              { key: "model", concern: "Model", effective: "claude-opus-5", winner: "user",
                setIn: [{ layer: "user", value: "claude-opus-5" }] },
              { key: "permissions.deny", concern: "Permissions", effective: ["Bash(rm)"], winner: "project",
                setIn: [{ layer: "user", value: [] }, { layer: "project", value: ["Bash(rm)"] }] },
              { key: "worktree.bgIsolation", concern: "Other", effective: "none", winner: "project",
                setIn: [{ layer: "project", value: "none" }] },
            ],
            errors: [],
            order: ["Model", "Permissions", "Hooks", "Environment", "MCP", "Notifications & UI", "Housekeeping", "Other"],
            injectedFlags: ["--permission-mode auto", "--allow-dangerously-skip-permissions"],
          },
          claude_instructions: [
            { source: "user", path: "/home/matt/.claude/CLAUDE.md", present: true, lines: 412,
              imports: [{ source: "import", path: "/home/matt/.claude/RTK.md", present: true, lines: 28, imports: [] }] },
            { source: "project", path: "/p/CLAUDE.md", present: false, lines: 0, imports: [] },
          ],
          claude_mcp: { servers: [], localConfigRead: true },
          claude_skills: [
            { name: "deploy-rpm", description: "Install an RPM on Matt's machines", source: "user",
              path: "/home/matt/.claude/skills/deploy-rpm/SKILL.md" },
            { name: "brainstorming", description: "Turn an idea into a design", source: "superpowers",
              path: "/home/matt/.claude/plugins/cache/.../skills/brainstorming/SKILL.md" },
          ],
```

Also add `config: true` to the `claude` entry of the existing `agent_caps` mock,
and `config: false` to the others.

- [ ] **Step 5: Verify**

Run: `npx tsc --noEmit` — expected clean. (Sections 10–13 create the four
imported components; until then, stub each as
`export default function X() { return null; }` in its own file so this task
compiles and can be reviewed on its own.)

- [ ] **Step 6: Commit**

```bash
git add src/components/agent-config src/components/SettingsModal.tsx src/App.css src/ipc.ts preview.html
git commit -m "A Claude Code panel, reached from its row in Agents"
```

---

### Task 10: Settings section

**Files:**
- Create (replacing the stub): `src/components/agent-config/SettingsSection.tsx`

**Interfaces:**
- Consumes: `claudeSettings`, `ClaudeSettingsView` from Task 8.
- Produces: `<SettingsSection project={string | null} />`.

- [ ] **Step 1: Implement**

```tsx
import { useEffect, useState } from "react";
import { ClaudeSettingsView, claudeSettings, homeAbbrev, openPath } from "../../ipc";

const LAYER_LABEL: Record<string, string> = {
  user: "user",
  project: "project",
  projectLocal: "project local",
  injected: "aiterm",
};

function show(v: unknown): string {
  if (typeof v === "string") return v;
  return JSON.stringify(v);
}

/** The layers, then every setting grouped by concern.
 *
 *  A setting shows the value in force and the file that set it; when more than
 *  one file sets it, the losers are listed too — "project overrides user" is
 *  the sentence this section exists to make sayable. */
export default function SettingsSection({ project }: { project: string | null }) {
  const [view, setView] = useState<ClaudeSettingsView | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    claudeSettings(project).then(setView).catch((e) => setError(String(e)));
  }, [project]);

  if (error) return <div className="acfg-err">{error}</div>;
  if (!view) return <div className="acfg-empty">Reading…</div>;

  const groups = view.order.filter((g) => view.settings.some((s) => s.concern === g));

  return (
    <div className="acfg-body">
      <div className="acfg-grp">Files</div>
      {view.layers.map((l) => (
        <div key={l.id} className="acfg-file">
          <span className="acfg-file-tag">{LAYER_LABEL[l.id] ?? l.id}</span>
          <span className={"acfg-file-path" + (l.present ? "" : " gone")}>
            {homeAbbrev(l.path)}
          </span>
          {l.present ? (
            <button className="acfg-open" onClick={() => openPath(l.path).catch(() => {})}>
              Open
            </button>
          ) : (
            <span className="acfg-file-state">not present</span>
          )}
          {l.error && <div className="acfg-err">{l.error}</div>}
        </div>
      ))}

      <div className="acfg-grp">Session start</div>
      <div className="acfg-flags">
        {view.injectedFlags.map((f) => (
          <code key={f} className="acfg-flag">{f}</code>
        ))}
        <div className="acfg-empty">
          Added by aiterm to every claude it launches.
          {view.injectedFlags.some((f) => f.includes("skip-permissions")) &&
            " Permission prompts are off in these sessions."}
        </div>
      </div>

      {groups.map((g) => (
        <div key={g}>
          <div className="acfg-grp">{g}</div>
          {view.settings.filter((s) => s.concern === g).map((s) => (
            <div key={s.key} className="acfg-set">
              <span className="acfg-key">{s.key}</span>
              <span className="acfg-val">{show(s.effective)}</span>
              <span className="acfg-src">{LAYER_LABEL[s.winner] ?? s.winner}</span>
              {s.setIn.length > 1 && (
                <div className="acfg-over">
                  also set in{" "}
                  {s.setIn.slice(0, -1).map((x) => LAYER_LABEL[x.layer] ?? x.layer).join(", ")}
                  {" — overridden"}
                </div>
              )}
            </div>
          ))}
        </div>
      ))}
    </div>
  );
}
```

- [ ] **Step 2: Styles**

Append to `src/App.css`:

```css
.acfg-grp {
  font-size: 10px; letter-spacing: 0.06em; text-transform: uppercase;
  color: var(--text-faint); padding: 12px 0 6px;
}
.acfg-file, .acfg-set { display: flex; align-items: baseline; gap: 8px; padding: 4px 0; flex-wrap: wrap; }
.acfg-file-tag, .acfg-src {
  font-size: 10px; color: var(--text-faint); min-width: 74px;
}
.acfg-file-path { font-size: 11px; color: var(--text-dim); overflow-wrap: anywhere; }
.acfg-file-path.gone { opacity: 0.55; }
.acfg-file-state { font-size: 10px; color: var(--text-faint); }
.acfg-open {
  margin-left: auto; background: none; border: 1px solid var(--border);
  border-radius: 5px; color: var(--text-dim); font-size: 10px;
  padding: 2px 7px; cursor: pointer;
}
.acfg-open:hover { color: var(--text); }
.acfg-key { font-size: 12px; min-width: 190px; overflow-wrap: anywhere; }
.acfg-val { font-size: 12px; color: var(--accent); overflow-wrap: anywhere; }
.acfg-src { margin-left: auto; text-align: right; }
.acfg-over { flex-basis: 100%; font-size: 10px; color: var(--text-faint); }
.acfg-flags { display: flex; flex-direction: column; gap: 3px; }
.acfg-flag { font-size: 11px; color: var(--text-dim); }
```

- [ ] **Step 3: Verify in the harness**

```bash
npx tsc --noEmit
(npx vite --port 5199 &) ; sleep 4
```

Open `http://localhost:5199/preview.html`, Settings → Agents → **Settings** on
the Claude Code row. Confirm: four file rows with the absent one marked, the two
injected flags with the permissions sentence, and `permissions.deny` showing
`project` as winner with "also set in user — overridden".

Then: `pkill -f "vite --por[t] 5199"`

- [ ] **Step 4: Commit**

```bash
git add src/components/agent-config/SettingsSection.tsx src/App.css
git commit -m "Show the settings, and which file won"
```

---

### Task 11: Instructions section

**Files:**
- Create (replacing the stub): `src/components/agent-config/InstructionsSection.tsx`

**Interfaces:**
- Consumes: `claudeInstructions`, `ClaudeDoc`.
- Produces: `<InstructionsSection project={string | null} />`.

- [ ] **Step 1: Implement**

```tsx
import { useEffect, useState } from "react";
import { ClaudeDoc, claudeInstructions, homeAbbrev, openPath } from "../../ipc";

/** One row per document, imports nested under the file that pulled them in. */
function DocRow({ doc, depth }: { doc: ClaudeDoc; depth: number }) {
  return (
    <>
      <div className="acfg-file" style={{ paddingLeft: depth * 14 }}>
        <span className="acfg-file-tag">{doc.source}</span>
        <span className={"acfg-file-path" + (doc.present ? "" : " gone")}>
          {homeAbbrev(doc.path)}
        </span>
        {doc.present ? (
          <>
            <span className="acfg-file-state">{doc.lines} lines</span>
            <button className="acfg-open" onClick={() => openPath(doc.path).catch(() => {})}>
              Open
            </button>
          </>
        ) : (
          <span className="acfg-file-state">not present</span>
        )}
      </div>
      {doc.imports.map((d) => (
        <DocRow key={d.path} doc={d} depth={depth + 1} />
      ))}
    </>
  );
}

/** What a session is told before you type anything.
 *
 *  Imports are nested rather than flattened, because "the global file pulls
 *  this in" is different information from "this file is loaded". */
export default function InstructionsSection({ project }: { project: string | null }) {
  const [docs, setDocs] = useState<ClaudeDoc[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    claudeInstructions(project).then(setDocs).catch((e) => setError(String(e)));
  }, [project]);

  if (error) return <div className="acfg-err">{error}</div>;
  if (!docs) return <div className="acfg-empty">Reading…</div>;

  return (
    <div className="acfg-body">
      <div className="acfg-grp">Instructions loaded, in order</div>
      {docs.map((d) => <DocRow key={d.path} doc={d} depth={0} />)}
      <div className="acfg-empty">
        Editing these is left to your editor — aiterm writes none of them.
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Verify in the harness**

Start vite as in Task 10; open Instructions. Confirm the user file shows 412
lines with `RTK.md` **indented beneath it**, and the project file reads "not
present".

- [ ] **Step 3: Commit**

```bash
git add src/components/agent-config/InstructionsSection.tsx
git commit -m "Show the CLAUDE.md chain, imports nested where they belong"
```

---

### Task 12: MCP section

**Files:**
- Create (replacing the stub): `src/components/agent-config/McpSection.tsx`

**Interfaces:**
- Consumes: `claudeMcp`, `ClaudeMcpView`.
- Produces: `<McpSection project={string | null} />`.

- [ ] **Step 1: Implement**

```tsx
import { useEffect, useState } from "react";
import { ClaudeMcpView, claudeMcp } from "../../ipc";

/** MCP servers registered in local files.
 *
 *  The empty case needs words, not a blank list: servers reached as claude.ai
 *  connectors are in no local file, so "none here" is not "no MCP". */
export default function McpSection({ project }: { project: string | null }) {
  const [view, setView] = useState<ClaudeMcpView | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    claudeMcp(project).then(setView).catch((e) => setError(String(e)));
  }, [project]);

  if (error) return <div className="acfg-err">{error}</div>;
  if (!view) return <div className="acfg-empty">Reading…</div>;

  return (
    <div className="acfg-body">
      <div className="acfg-grp">Registered locally</div>
      {view.servers.length === 0 ? (
        <div className="acfg-empty">
          {view.localConfigRead
            ? "No MCP servers in ~/.claude.json or .mcp.json. Servers connected through claude.ai are not in local files, so a session may still have MCP tools."
            : "No local MCP configuration could be read."}
        </div>
      ) : (
        view.servers.map((s) => (
          <div key={`${s.scope}:${s.name}`} className="acfg-set">
            <span className="acfg-key">{s.name}</span>
            <span className="acfg-val">{s.command ?? "—"}</span>
            <span className="acfg-src">
              {s.scope}
              {s.enabled === false && " · disabled here"}
              {s.enabled === true && " · enabled here"}
            </span>
          </div>
        ))
      )}
    </div>
  );
}
```

- [ ] **Step 2: Verify in the harness**

Start vite; open MCP. With the mock (`servers: []`, `localConfigRead: true`),
confirm the connectors sentence appears rather than an empty panel.

- [ ] **Step 3: Commit**

```bash
git add src/components/agent-config/McpSection.tsx
git commit -m "Show MCP, and say what an empty list does not mean"
```

---

### Task 13: Skills section

**Files:**
- Create (replacing the stub): `src/components/agent-config/SkillsSection.tsx`

**Interfaces:**
- Consumes: `claudeSkills`, `ClaudeSkill`.
- Produces: `<SkillsSection project={string | null} />`.

- [ ] **Step 1: Implement**

```tsx
import { useEffect, useState } from "react";
import { ClaudeSkill, claudeSkills, openPath } from "../../ipc";

/** Skills a session can reach, grouped by the tree they came from. */
export default function SkillsSection({ project }: { project: string | null }) {
  const [skills, setSkills] = useState<ClaudeSkill[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    claudeSkills(project).then(setSkills).catch((e) => setError(String(e)));
  }, [project]);

  if (error) return <div className="acfg-err">{error}</div>;
  if (!skills) return <div className="acfg-empty">Reading…</div>;
  if (skills.length === 0) return <div className="acfg-empty">No skills found.</div>;

  const sources = [...new Set(skills.map((s) => s.source))];
  return (
    <div className="acfg-body">
      {sources.map((src) => (
        <div key={src}>
          <div className="acfg-grp">{src}</div>
          {skills.filter((s) => s.source === src).map((s) => (
            <div key={s.path} className="acfg-set">
              <span className="acfg-key">{s.name}</span>
              <span className="acfg-val acfg-desc">{s.description || "—"}</span>
              <button className="acfg-open" onClick={() => openPath(s.path).catch(() => {})}>
                Open
              </button>
            </div>
          ))}
        </div>
      ))}
    </div>
  );
}
```

Add to `src/App.css`:

```css
.acfg-desc { color: var(--text-dim); }
```

- [ ] **Step 2: Verify in the harness**

Start vite; open Skills. Confirm two groups ("user" and "superpowers"), each
skill with its description and an Open button.

- [ ] **Step 3: Commit**

```bash
git add src/components/agent-config/SkillsSection.tsx src/App.css
git commit -m "List skills by the tree they came from"
```

---

### Task 14: Ship it

**Files:**
- Modify: `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml` (version)

- [ ] **Step 1: Full verification**

```bash
npx tsc --noEmit
npm run test:ui
cd src-tauri && cargo test && cargo clippy --all-targets 2>&1 | grep -cE "^warning: this"
```

Expected: clean, 18 pass, all Rust green, clippy count `2`.

- [ ] **Step 2: Bump both version strings to the next patch and build**

```bash
npm run tauri build -- --bundles rpm
```

- [ ] **Step 3: Install on the home PC only**

```bash
sudo -n dnf install -y src-tauri/target/release/bundle/rpm/aiterm-<v>-1.x86_64.rpm
rpm -q aiterm
cp src-tauri/target/release/bundle/rpm/aiterm-<v>-1.x86_64.rpm ~/Projects/aiterm-releases/
```

Do **not** push to the work PC — 2026-08-04 standing instruction, still in force
until Matt lifts it.

- [ ] **Step 4: Verify against the real machine after relaunch**

The harness runs on mocks, so these are the checks only the installed build can
answer. Matt must relaunch first; do not restart aiterm for him.

- Settings: four layers, all three real ones present in this repo, `model` won
  by user, `worktree.bgIsolation` by project, `permissions` by project local.
- Session start: both injected flags, matching `ps` output for a live tab.
- Instructions: the global file with `RTK.md` nested beneath it.
- MCP: the connectors sentence, since local config is empty here.
- Skills: `deploy-rpm` and `kpxc-gateway` under "user", plus plugin groups —
  and **`document-skills` listed once**, not three times.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "<version>"
```

---

## Self-review

**Spec coverage.** Placement and `Caps.config` → Task 1, 9. Layers and
precedence → Task 2. Concern grouping without a schema → Task 3. Instructions
with imports → Task 4. MCP from real sources, including the connectors caveat →
Task 5, 12. Skills via `installed_plugins.json` → Task 6, 13. Session Start with
launcher-owned flags → Task 1, 7, 10. Failure handling → Tasks 2, 5, 7 tests and
the section components' error/empty states. Testing list → covered task by task.
Non-goals → nothing in any task writes a Claude file; `openPath` hands editing
to the user's editor.

**Placeholders.** None: every code step carries the code, every test step the
assertions. Task 9 names the stub files the later tasks replace, so no task
references a component that does not yet exist.

**Type consistency.** `LayerId` variants serialise camelCase (`projectLocal`)
and the TS `ClaudeLayerId` matches. `Setting.set_in` → `setIn` via
`rename_all`, used as `setIn` in Task 10. `SettingsView.injected_flags` →
`injectedFlags`. `McpView.local_config_read` → `localConfigRead`, used in Task
12. `concern::of` is called by `settings::resolve` in Task 2 and defined in Task
3, with a compiling stub in Task 2 so neither task is broken alone.
