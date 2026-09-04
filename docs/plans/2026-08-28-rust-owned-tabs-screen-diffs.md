# Rust-Owned Tabs and Screen Diffs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Rust the authoritative owner of live terminal tabs and produce revisioned terminal screen snapshots/diffs for remote native renderers without changing the desktop's xterm.js rendering.

**Architecture:** A transport-independent `TabRegistry` owns PTYs, stable tab ids, metadata, attachments, focus, dimensions, and a canonical `alacritty_terminal` screen from the first PTY byte. Desktop Tauri commands and the remote gateway attach to the same registry; desktop attachments continue receiving raw bytes for xterm.js, while remote attachments receive typed screen snapshots and changed-row diffs.

**Tech Stack:** Rust 2021, Tauri 2, `portable-pty` 0.9, exact `alacritty_terminal` 0.26.0, serde/CBOR, Tokio channels, React/TypeScript, xterm.js, Node test runner.

**Spec:** `docs/design/2026-08-28-android-remote-client.md`

## Global Constraints

- PTYs and canonical screen state remain in the desktop process; Android never receives numeric PTY ids or raw PTY output.
- `TabId` is a random UUID that is stable only for the life of its PTY; conversation session ids remain separate and persistent.
- Rust owns tab lifecycle, metadata, focus, dimensions, and screen state. React owns only presentation state.
- The existing xterm.js desktop renderer, selection, paste, scrollback, OSC handling, and TUI overlays must keep working with Remote Access disabled.
- Exactly one attachment owns input and resize. Desktop and phone use the same authorization path.
- Remote recovery uses a complete typed viewport snapshot. Scrollback is fetched as a separate bounded page. Live updates use revisioned typed changed-row diffs and never historical byte replay.
- Screen work is bounded to 512 columns, 512 rows, 5,000 scrollback rows, 1 MiB serialized frames, and at most one diff emission per 16 ms.
- A cell carries at most its base scalar plus 32 combining scalars. Oversized multi-row snapshots/diffs are split on row boundaries into ordered sub-1 MiB transfer chunks and applied atomically; semantic rows are never truncated.
- Terminal output, input, QR payloads, credentials, and enrollment secrets never enter logs.
- The unfinished untracked Android Task 8 files are out of scope for this plan and must be preserved unchanged.

---

## File structure

- `src-tauri/src/terminal/mod.rs` — terminal-core exports and shared constants.
- `src-tauri/src/terminal/model.rs` — serializable cells, rows, cursor, modes, snapshots, diffs, and revisions.
- `src-tauri/src/terminal/screen.rs` — exact-version Alacritty adapter, damage conversion, reply/title events, and scrollback reads.
- `src-tauri/src/tabs.rs` — tab ids, tab metadata, PTY ownership, attachments, focus, and subscriptions.
- `src-tauri/src/pty.rs` — `portable-pty` process lifecycle and output-sink boundary; no tab policy.
- `src-tauri/src/remote/terminal.rs` — remote attachment adapter over `TabRegistry`; no PTY map or byte replay buffer.
- `src-tauri/src/remote/model.rs` — validated tab/snapshot/diff protocol kinds.
- `src-tauri/src/lib.rs`, `src-tauri/src/hooklink.rs` — managed registry, Tauri commands, and session-hook metadata updates.
- `src/ipc.ts` — typed tab IPC bindings.
- `src/writeQueue.ts` — generic ordered/coalesced writes keyed by tab attachment.
- `src/components/TerminalView.tsx` — desktop registry attachment and xterm rendering.
- `src/App.tsx` — React projection of Rust tab descriptors and string `TabId` keys.
- `src-tauri/tests/terminal_screen.rs` — screen fixtures and snapshot/diff correctness.
- `src-tauri/tests/tab_registry.rs` — lifecycle, metadata, attachment, focus, and output tests.
- `src-tauri/tests/remote_terminal.rs` — remote opaque-id, focus, snapshot, and recovery tests.
- `src/tabModel.test.ts` — frontend reconciliation and `TabId` tests.
- `src/writeQueue.test.ts` — ordered writes for string/composite tab keys.

### Task 1: Define the typed terminal screen protocol

**Files:**
- Create: `src-tauri/src/terminal/mod.rs`
- Create: `src-tauri/src/terminal/model.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/remote/model.rs`
- Test: `src-tauri/tests/terminal_screen.rs`
- Test: `src-tauri/tests/remote_protocol.rs`

**Interfaces:**
- Consumes: validated `TerminalSize` from `remote::model`.
- Produces: `Revision`, `TerminalColor`, `CellAttributes`, `ScreenCell`, `ScreenRow`, `CursorState`, `TerminalModes`, `ScreenSnapshot`, `RowPatch`, and `ScreenDiff`.

- [ ] **Step 1: Write failing model and CBOR-bound tests**

```rust
#[test]
fn a_diff_names_the_snapshot_revision_it_applies_to() {
    let diff = ScreenDiff::new(Revision(7), Revision(8), vec![row_patch(3, "ready")]);
    assert_eq!(diff.base_revision(), Revision(7));
    assert_eq!(diff.revision(), Revision(8));
}

#[test]
fn a_wide_glyph_has_one_lead_cell_and_one_explicit_continuation() {
    let row = screen_row("你");
    assert_eq!(row.cells()[0].width(), 2);
    assert!(row.cells()[1].is_continuation());
}

#[test]
fn terminal_frames_larger_than_one_mebibyte_are_rejected() {
    assert_eq!(validate_terminal_frame(&vec![0; 1024 * 1024 + 1]).unwrap_err().code(),
               "protocol.frame_too_large");
}
```

- [ ] **Step 2: Run the tests to verify the types are absent**

Run: `CARGO_TARGET_DIR=/tmp/aiterm-android-rust-target cargo test --test terminal_screen --test remote_protocol`

Expected: FAIL because `terminal::model` and the screen frame validators do not exist.

- [ ] **Step 3: Implement the wire-facing types with private fields and constructors**

```rust
pub const MAX_SCREEN_FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_SCROLLBACK_ROWS: usize = 5_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revision(pub u64);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalColor { Default, Indexed(u8), Rgb { r: u8, g: u8, b: u8 } }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenSnapshot {
    tab_id: String,
    revision: Revision,
    cols: u16,
    rows: u16,
    visible: Vec<ScreenRow>,
    scrollback: Vec<ScreenRow>,
    cursor: CursorState,
    modes: TerminalModes,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenDiff {
    tab_id: String,
    base_revision: Revision,
    revision: Revision,
    rows: Vec<RowPatch>,
    cursor: Option<CursorState>,
    modes: Option<TerminalModes>,
}
```

Expose protocol request kinds `tab.list`, `tab.open`, `tab.close`, `terminal.scrollback`, and existing terminal attachment/focus/input/resize kinds. Enforce terminal dimensions and serialized-frame bounds before allocation or send.

- [ ] **Step 4: Run focused tests and formatting**

Run: `rustfmt --edition 2021 src/terminal/mod.rs src/terminal/model.rs tests/terminal_screen.rs tests/remote_protocol.rs && CARGO_TARGET_DIR=/tmp/aiterm-android-rust-target cargo test --test terminal_screen --test remote_protocol`

Expected: PASS.

- [ ] **Step 5: Commit the typed protocol**

```bash
git add src-tauri/src/terminal src-tauri/src/lib.rs src-tauri/src/remote/model.rs src-tauri/tests/terminal_screen.rs src-tauri/tests/remote_protocol.rs
git commit -m "feat(terminal): define screen snapshot and diff protocol"
```

### Task 2: Build the canonical Alacritty screen adapter

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/Cargo.lock`
- Create: `src-tauri/src/terminal/screen.rs`
- Modify: `src-tauri/src/terminal/mod.rs`
- Test: `src-tauri/tests/terminal_screen.rs`

**Interfaces:**
- Consumes: `TerminalSize` and the model types from Task 1.
- Produces: `ScreenModel::{new, process, resize, snapshot, scrollback_page}` and `ScreenDamage { diff, replies, title, bell }`.

- [ ] **Step 1: Add failing terminal-behavior fixtures**

```rust
#[test]
fn split_utf8_and_combining_marks_survive_a_snapshot() {
    let mut screen = ScreenModel::new(size(8, 2));
    screen.process(&[0xe4, 0xbd]);
    screen.process(&[0xa0, b'e', 0xcc, 0x81]);
    assert_eq!(screen.snapshot(tab()).visible_text(), vec!["你e\u{301}", ""]);
}

#[test]
fn alternate_screen_is_current_even_when_it_started_before_attach() {
    let mut screen = ScreenModel::new(size(12, 3));
    screen.process(b"shell\r\n\x1b[?1049hfull screen");
    assert!(screen.snapshot(tab()).modes().alternate_screen());
    assert!(screen.snapshot(tab()).visible_text()[0].contains("full screen"));
}

#[test]
fn applying_damage_to_the_previous_snapshot_equals_a_fresh_snapshot() {
    let mut screen = ScreenModel::new(size(20, 4));
    screen.process(b"before");
    let mut client = screen.snapshot(tab());
    let damage = screen.process(b"\r\x1b[32mafter\x1b[0m");
    client.apply(damage.diff.unwrap()).unwrap();
    assert_eq!(client, screen.snapshot(tab()));
}
```

Also add deterministic tests for RGB/indexed colors, cursor shape/visibility, wide-cell continuation, wrapped rows, resize/reflow, title, bell, bracketed paste, application cursor mode, scrollback paging, terminal-query replies, and 10,000 pseudo-random byte/resize operations without panic.

- [ ] **Step 2: Run the fixtures to verify the adapter is absent**

Run: `CARGO_TARGET_DIR=/tmp/aiterm-android-rust-target cargo test --test terminal_screen screen_model`

Expected: FAIL because `ScreenModel` does not exist.

- [ ] **Step 3: Pin and wrap Alacritty's terminal core**

Add exactly:

```toml
alacritty_terminal = "=0.26.0"
```

Implement only this AITerm-owned public surface:

```rust
pub struct ScreenDamage {
    pub diff: Option<ScreenDiff>,
    pub replies: Vec<Vec<u8>>,
    pub title: Option<String>,
    pub bell: bool,
}

pub struct ScreenModel { /* Processor, Term<ScreenEvents>, revision, last metadata */ }

impl ScreenModel {
    pub fn new(size: TerminalSize) -> Self;
    pub fn process(&mut self, bytes: &[u8]) -> ScreenDamage;
    pub fn resize(&mut self, size: TerminalSize) -> ScreenDamage;
    pub fn snapshot(&self, tab: &str) -> ScreenSnapshot;
    pub fn scrollback_page(&self, offset: usize, count: usize) -> Vec<ScreenRow>;
}
```

Map `Term::damage()` to complete changed rows, then call `reset_damage()`. Convert `RenderableContent` and cell flags into AITerm model types. Collect `Event::PtyWrite`, `Event::Title`, `Event::ResetTitle`, and `Event::Bell`; reject clipboard events from the remote model. Clamp scrollback requests to 5,000 rows and output frames to 1 MiB.

- [ ] **Step 4: Run adapter tests and the library suite**

Run: `rustfmt --edition 2021 src/terminal/screen.rs tests/terminal_screen.rs && CARGO_TARGET_DIR=/tmp/aiterm-android-rust-target cargo test --test terminal_screen && CARGO_TARGET_DIR=/tmp/aiterm-android-rust-target cargo test --lib`

Expected: PASS with no panic in the deterministic random-input test.

- [ ] **Step 5: Commit the screen engine**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/terminal src-tauri/tests/terminal_screen.rs
git commit -m "feat(terminal): maintain canonical Rust screen state"
```

### Task 3: Give the PTY core an output-sink boundary

**Files:**
- Modify: `src-tauri/src/pty.rs`
- Test: `src-tauri/tests/backend.rs`
- Test: `src-tauri/tests/tab_registry.rs`

**Interfaces:**
- Consumes: existing `PtyManager`, `PtySize`, process-tree kill logic, environment setup, and provider injection.
- Produces: `PtySpawnSpec`, `PtySink`, `PtyManager::{spawn, write, resize, kill, pty_for_descendant}`.

- [ ] **Step 1: Write a failing sink-from-first-byte test**

```rust
#[test]
fn spawn_delivers_output_and_exit_to_its_sink() {
    let sink = Arc::new(RecordingSink::default());
    let id = manager.spawn(PtySpawnSpec::command("printf first"), sink.clone()).unwrap();
    sink.wait_for_exit();
    assert_eq!(sink.output(), b"first");
    assert_eq!(sink.exits()[0].pty_id, id);
}
```

- [ ] **Step 2: Run the test to verify `PtySink` is absent**

Run: `CARGO_TARGET_DIR=/tmp/aiterm-android-rust-target cargo test --test tab_registry spawn_delivers_output`

Expected: FAIL at compile time.

- [ ] **Step 3: Extract process lifecycle without changing behavior**

```rust
pub trait PtySink: Send + Sync + 'static {
    fn output(&self, pty_id: u32, bytes: &[u8]);
    fn exited(&self, pty_id: u32, code: Option<u32>, signal: Option<&str>);
}

pub struct PtySpawnSpec {
    pub cwd: Option<String>,
    pub command: Option<String>,
    pub size: TerminalSize,
    pub env_provider: Option<String>,
    pub env_model: Option<String>,
}
```

Move the body of `pty_spawn` into `PtyManager::spawn`. The reader thread calls exactly the sink passed to that spawn; remove the process-global `OBSERVER`. Keep existing environment scrubbing, binary 8 KiB reads, child reaping, descendant lookup, and graceful process-tree kill unchanged. Keep temporary command wrappers until Task 5 migrates the desktop.

- [ ] **Step 4: Run PTY and backend regression tests**

Run: `rustfmt --edition 2021 src/pty.rs tests/tab_registry.rs && CARGO_TARGET_DIR=/tmp/aiterm-android-rust-target cargo test --test tab_registry spawn_delivers_output && CARGO_TARGET_DIR=/tmp/aiterm-android-rust-target cargo test --test backend`

Expected: PASS.

- [ ] **Step 5: Commit the PTY boundary**

```bash
git add src-tauri/src/pty.rs src-tauri/tests/tab_registry.rs
git commit -m "refactor(pty): deliver each process through an output sink"
```

### Task 4: Implement the Rust tab registry

**Files:**
- Create: `src-tauri/src/tabs.rs`
- Modify: `src-tauri/src/lib.rs`
- Test: `src-tauri/tests/tab_registry.rs`

**Interfaces:**
- Consumes: `PtyManager`, `PtySpawnSpec`, `PtySink`, `ScreenModel`, and screen model types.
- Produces: `TabId`, `AttachmentId`, `AttachmentKind`, `TabLaunch`, `TabDescriptor`, `TabRegistry`, and `TabEventReceiver`.

- [ ] **Step 1: Write failing registry ownership tests**

```rust
#[test]
fn registry_lists_a_tab_before_any_remote_client_attaches() {
    let id = registry.open(shell_launch()).unwrap();
    assert_eq!(registry.list()[0].id(), &id);
    assert!(registry.snapshot(&id).unwrap().visible_text()[0].contains("ready"));
}

#[test]
fn phone_cannot_input_or_resize_until_it_takes_focus() {
    let tab = registry.open(shell_launch()).unwrap();
    let desktop = registry.attach(&tab, AttachmentKind::Desktop).unwrap();
    let phone = registry.attach(&tab, AttachmentKind::Remote).unwrap();
    assert_eq!(registry.input(&tab, &phone.id, b"x").unwrap_err().code(),
               "terminal.input_not_owned");
    registry.take_focus(&tab, &phone.id, size(42, 18)).unwrap();
    assert_eq!(registry.input(&tab, &phone.id, b"x"), Ok(()));
    assert_eq!(registry.resize(&tab, &desktop.id, size(80, 24)).unwrap_err().code(),
               "terminal.input_not_owned");
}
```

Add tests for random/non-numeric ids, duplicate slot rejection, metadata update, session-hook rekey, desktop raw-byte delivery, remote typed diff delivery, xterm reply suppression while remote owns focus, Rust reply delivery while remote owns focus, close/exit state, dropped-subscriber cleanup, and unknown ids.

- [ ] **Step 2: Run registry tests to verify the registry is absent**

Run: `CARGO_TARGET_DIR=/tmp/aiterm-android-rust-target cargo test --test tab_registry registry_`

Expected: FAIL because `tabs` does not exist.

- [ ] **Step 3: Implement the registry with one lock boundary per tab**

```rust
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TabId(String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentKind { Desktop, Remote }

pub struct TabRegistry { /* PtyManager + HashMap<TabId, Arc<Mutex<LiveTab>>> */ }

impl TabRegistry {
    pub fn open(&self, launch: TabLaunch) -> Result<TabId, TabError>;
    pub fn list(&self) -> Vec<TabDescriptor>;
    pub fn update(&self, id: &TabId, update: TabUpdate) -> Result<TabDescriptor, TabError>;
    pub fn attach(&self, id: &TabId, kind: AttachmentKind) -> Result<TabAttachment, TabError>;
    pub fn snapshot(&self, id: &TabId) -> Result<ScreenSnapshot, TabError>;
    pub fn input(&self, id: &TabId, attachment: &AttachmentId, bytes: &[u8]) -> Result<(), TabError>;
    pub fn resize(&self, id: &TabId, attachment: &AttachmentId, size: TerminalSize) -> Result<(), TabError>;
    pub fn take_focus(&self, id: &TabId, attachment: &AttachmentId, size: TerminalSize) -> Result<(), TabError>;
    pub fn close(&self, id: &TabId) -> Result<(), TabError>;
    pub fn tab_for_descendant(&self, pid: u32) -> Option<TabId>;
}
```

Create the `LiveTab` and screen before spawning the PTY, so the `PtySink` can parse the first byte. Use per-tab mutexes so one blocked PTY write cannot stop other tabs. Broadcast raw output only to desktop attachments and `ScreenDiff` only to remote attachments. Emit a complete snapshot on remote attach or queue loss. Process terminal replies only when no desktop attachment owns focus.

- [ ] **Step 4: Run registry, screen, and backend tests**

Run: `rustfmt --edition 2021 src/tabs.rs tests/tab_registry.rs && CARGO_TARGET_DIR=/tmp/aiterm-android-rust-target cargo test --test tab_registry --test terminal_screen --test backend`

Expected: PASS.

- [ ] **Step 5: Commit the authoritative tab model**

```bash
git add src-tauri/src/tabs.rs src-tauri/src/lib.rs src-tauri/tests/tab_registry.rs
git commit -m "feat(tabs): own terminal lifecycle and focus in Rust"
```

### Task 5: Migrate Tauri and React to Rust tab ids

**Files:**
- Modify: `src-tauri/src/tabs.rs`
- Modify: `src-tauri/src/pty.rs`
- Modify: `src-tauri/src/hooklink.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src/ipc.ts`
- Modify: `src/writeQueue.ts`
- Modify: `src/writeQueue.test.ts`
- Modify: `src/components/TerminalView.tsx`
- Modify: `src/App.tsx`
- Create: `src/tabModel.ts`
- Test: `src/tabModel.test.ts`

**Interfaces:**
- Consumes: `TabRegistry` and `TabDescriptor` from Task 4.
- Produces: Tauri commands `tab_open`, `tab_list`, `tab_update`, `tab_attach_desktop`, `tab_write`, `tab_resize`, `tab_take_focus`, and `tab_close`; TypeScript `TabId = string` and matching bindings.

- [ ] **Step 1: Write failing frontend reconciliation tests**

```ts
test("Rust descriptors replace metadata without changing active tab identity", () => {
  const before = [{ id: "tab-a", title: "repo", slotId: "old" }];
  const after = reconcileTabs(before, [{ id: "tab-a", title: "repo", slotId: "new" }]);
  assert.equal(after[0].id, "tab-a");
  assert.equal(after[0].slotId, "new");
});

test("a closed Rust tab is removed from the renderer projection", () => {
  assert.deepEqual(reconcileTabs([{ id: "tab-a" }, { id: "tab-b" }], [{ id: "tab-b" }]),
                   [{ id: "tab-b" }]);
});
```

- [ ] **Step 2: Run tests to verify the frontend model is absent**

Run: `npm run test:ui -- tabModel`

Expected: FAIL because `tabModel.ts` does not exist.

- [ ] **Step 3: Add Tauri adapters and migrate the renderer**

Use this TypeScript boundary:

```ts
export type TabId = string;
export interface TabDescriptor {
  id: TabId; title: string; cwd: string | null; command: string | null;
  sessionId?: string; resumedId?: string; agentId?: string; slotId: string;
  fresh?: boolean; envProvider?: string; envModel?: string;
}

type TabWriteTarget = { tabId: TabId; attachmentId: string };
const queuedTabWrite = makeWriteQueue<TabWriteTarget>(
  (target, data) => invoke("tab_write", { ...target, data }),
  (target) => `${target.tabId}\0${target.attachmentId}`,
);
export const tabWrite = (tabId: TabId, attachmentId: string, data: string) =>
  queuedTabWrite({ tabId, attachmentId }, data);
```

Generalize `makeWriteQueue<K>` with an injected `(key: K) => string | number`
identity function and retain the latest full key alongside each outbox entry.
Add a test proving two separately allocated `{tabId, attachmentId}` objects with
the same identity serialize into one ordered stream while different attachment
ids drain independently.

Change `TermTab.key`, `activeTab`, terminal-handle maps, attention/ended maps, and `FileTab.termKey` from `number` to `TabId`. `TerminalView` calls `tab_open` once, attaches its binary `Channel`, renders raw bytes exactly as before, and sends all xterm `onData`/resize through its attachment id. `closeTab` calls `tab_close`; component cleanup detaches but does not independently kill a different PTY. Hook draining resolves a pid to `TabId` and updates the registry before returning the event, so React receives authoritative metadata rather than rekeying only its own object.

Remove `nextKey`, direct numeric `ptyId` exposure in `TermHandle`, and direct renderer calls to `pty_spawn`, `pty_write`, `pty_resize`, and `pty_kill`. Keep the legacy Rust commands only until the same commit's frontend build succeeds, then remove them from `generate_handler!`.

- [ ] **Step 4: Run desktop tests and builds**

Run: `npm run test:ui && npm run build && CARGO_TARGET_DIR=/tmp/aiterm-android-rust-target cargo test --test tab_registry --test backend && CARGO_TARGET_DIR=/tmp/aiterm-android-rust-target cargo test --lib`

Expected: PASS. Manually launch two shell tabs with Remote Access disabled, type/paste, resize, switch tabs, run a TUI, close one, and verify the other remains interactive.

- [ ] **Step 5: Commit the desktop migration**

```bash
git add src-tauri/src/tabs.rs src-tauri/src/pty.rs src-tauri/src/hooklink.rs src-tauri/src/lib.rs src/ipc.ts src/writeQueue.ts src/writeQueue.test.ts src/components/TerminalView.tsx src/App.tsx src/tabModel.ts src/tabModel.test.ts
git commit -m "refactor(tabs): use the Rust tab registry on desktop"
```

### Task 6: Replace remote byte replay with registry screen subscriptions

**Files:**
- Modify: `src-tauri/src/remote/terminal.rs`
- Modify: `src-tauri/src/remote/server.rs`
- Modify: `src-tauri/src/remote/mod.rs`
- Modify: `src-tauri/src/remote/model.rs`
- Modify: `src-tauri/tests/remote_terminal.rs`
- Modify: `src-tauri/tests/remote_server.rs`

**Interfaces:**
- Consumes: authenticated connection requests and `TabRegistry` attachments.
- Produces: tab list/open/close handlers plus `terminal.snapshot`, `terminal.diff`, `terminal.focus_changed`, `terminal.title`, and `terminal.exited` events.

- [ ] **Step 1: Replace replay assertions with failing snapshot recovery tests**

```rust
#[test]
fn attaching_after_early_escape_sequences_gets_the_current_screen() {
    let tab = registry.open(command("printf '\033[?1049hphone view' && sleep 1")).unwrap();
    wait_for_text(&registry, &tab, "phone view");
    let attached = remote.attach(&tab).unwrap();
    assert!(attached.snapshot.visible_text()[0].contains("phone view"));
    assert!(attached.snapshot.modes().alternate_screen());
}

#[test]
fn a_revision_mismatch_is_recovered_by_snapshot_not_byte_replay() {
    let attached = remote.attach(&tab).unwrap();
    let recovered = remote.resume(&tab, Revision(attached.snapshot.revision().0 - 1)).unwrap();
    assert!(matches!(recovered, TerminalEvent::Snapshot(_)));
}
```

- [ ] **Step 2: Run remote tests to verify they still expose byte replay**

Run: `CARGO_TARGET_DIR=/tmp/aiterm-android-rust-target cargo test --test remote_terminal`

Expected: FAIL because `TerminalBroker` returns `Replay::{Snapshot,Delta}` containing bytes.

- [ ] **Step 3: Make the remote terminal module a registry adapter**

Delete `ReplayBuffer`, `Chunk`, `Stream { pty_id, ... }`, `by_pty`, `open_stream`, and the process-global observer hookup. Keep opaque remote `AttachmentId` values, but make their target a `TabId`. On attach, subscribe as `AttachmentKind::Remote` and immediately send the current `ScreenSnapshot`. Forward coalesced `ScreenDiff` events; if the bounded event queue drops or a client supplies an unknown revision, send a new snapshot. Route input, resize, focus, detach, tab list/open/close, and scrollback requests into `TabRegistry`.

Encode semantic snapshots and diffs as ordered row-boundary transfer chunks.
Each chunk must serialize below 1 MiB and identify the transfer, tab, kind,
base/final revision, row range, index, and total. The receiver applies a
transfer only after every chunk validates; missing, duplicate, out-of-order, or
invalid chunks discard the transfer and request a fresh snapshot. A partial
transfer never advances the receiver revision. Send scrollback through the same
bounded row-chunk mechanism as a separate resource, not inside the live
snapshot.

Coalesce remote damage with a per-attachment 16 ms Tokio interval. Merge row
patches by row index so the newest row wins, carry the oldest base revision and
newest revision, and flush cursor/mode/title values from the newest update. Do
not delay snapshots, focus changes, exits, request replies, or errors.

Change the authenticated server loop from accepting requests without replies to dispatching validated requests through a `RemoteServices` value containing `Arc<TabRegistry>`. Serialize typed responses only after authorization and preserve the existing request-id and token-bucket guard.

- [ ] **Step 4: Run remote and full Rust regression tests**

Run: `CARGO_TARGET_DIR=/tmp/aiterm-android-rust-target cargo test --test remote_protocol --test remote_auth --test remote_server --test remote_terminal --test remote_desktop --test tab_registry --test terminal_screen && CARGO_TARGET_DIR=/tmp/aiterm-android-rust-target cargo test --lib`

Expected: PASS. Gateway socket tests require loopback permission.

- [ ] **Step 5: Commit screen-diff transport**

```bash
git add src-tauri/src/remote src-tauri/tests/remote_terminal.rs src-tauri/tests/remote_server.rs
git commit -m "feat(remote): stream canonical terminal screen diffs"
```

### Task 7: Verify bounds, desktop independence, and documentation

**Files:**
- Modify: `docs/remote/android-remote-testing.md`
- Modify: `docs/plans/2026-08-28-android-remote-client.md`
- Test: all touched Rust and desktop suites.

**Interfaces:**
- Consumes: completed registry, screen adapter, desktop migration, and remote screen protocol.
- Produces: documented evidence and an updated parent plan whose Task 9 consumes screen snapshots/diffs instead of raw bytes.

- [ ] **Step 1: Update the parent plan's execution status and Task 9 contract**

Mark committed Tasks 1–4, 6–7, and the completed Rust-owned-tab subplan by commit id. Preserve Task 8 as in progress. Replace Task 9's `TerminalSession` byte-emulator interface with:

```kotlin
interface TerminalScreenStore {
    val screen: StateFlow<ScreenSnapshot?>
    fun replace(snapshot: ScreenSnapshot)
    fun apply(diff: ScreenDiff): ApplyResult
}

sealed interface ApplyResult {
    data object Applied : ApplyResult
    data object NeedsSnapshot : ApplyResult
}
```

- [ ] **Step 2: Run format, diff, test, and build verification from fresh commands**

Run: `git diff --check && npm run test:ui && npm run build`

Run: `CARGO_TARGET_DIR=/tmp/aiterm-android-rust-target cargo test --lib --test backend --test remote_protocol --test remote_auth --test remote_server --test remote_terminal --test remote_desktop --test tab_registry --test terminal_screen`

Expected: every command exits 0. Run the gateway suites with loopback permission rather than treating sandbox bind denial as a product failure.

- [ ] **Step 3: Perform the desktop manual smoke test**

With Remote Access disabled: open a shell and Codex tab, type and paste Unicode, resize, run `top`, clear a Codex conversation, switch tabs, and close both. With Remote Access enabled: attach a test client after `top` is already drawing, verify an alternate-screen snapshot, transfer focus to the test client, reject desktop input/resize, transfer focus back, and verify desktop input resumes.

- [ ] **Step 4: Inspect the final diff and commit documentation**

Run: `git status --short && git diff --stat origin/main...HEAD && git diff --check origin/main...HEAD`

```bash
git add docs/remote/android-remote-testing.md docs/plans/2026-08-28-android-remote-client.md
git commit -m "docs: record Rust terminal screen verification"
```

## Plan self-review

- Spec coverage: Rust ownership, stable opaque tab ids, first-byte parsing, xterm.js preservation, one input/resize owner, canonical dimensions, snapshots, revisioned changed-row diffs, scrollback bounds, terminal replies, recovery, and verification each map to an explicit task.
- Scope: Android pairing/biometric work and the Compose renderer remain in their parent-plan tasks; this subplan produces the independent desktop and protocol foundation they consume.
- Placeholder scan: the plan contains no deferred implementation markers; every task has concrete files, interfaces, red test examples, commands, expected results, implementation boundaries, and a commit.
- Type consistency: `TabId`, `AttachmentId`, `TerminalSize`, `Revision`, `ScreenSnapshot`, and `ScreenDiff` are introduced once and used under those names throughout later tasks.
