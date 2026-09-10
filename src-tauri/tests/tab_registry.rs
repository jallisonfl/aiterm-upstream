//! PTY output-boundary checks. Tab ownership tests join this file in Task 4.

use aiterm_lib::pty::{PtyManager, PtySink, PtySpawnSpec};
use aiterm_lib::remote::model::TerminalSize;
use aiterm_lib::remote::terminal::RemoteTerminal;
use aiterm_lib::tabs::{
    AttachmentId, AttachmentKind, PtyBackend, TabEvent, TabId, TabLaunch, TabRegistry,
    TabRegistryEvent, TabState, TabUpdate,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Barrier, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

// The compatibility observer is process-global until Task 6. Serialize PTY
// tests so one test cannot intentionally observe another test's output.
static PTY_TEST_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, PartialEq, Eq)]
struct Exit {
    pty_id: u32,
    code: Option<u32>,
    signal: Option<String>,
}

#[derive(Default)]
struct RecordingSink {
    output: Mutex<Vec<u8>>,
    exits: Mutex<Vec<Exit>>,
    exited: Condvar,
}

impl RecordingSink {
    fn output(&self) -> Vec<u8> {
        self.output.lock().unwrap().clone()
    }

    fn exits(&self) -> Vec<Exit> {
        self.exits.lock().unwrap().clone()
    }

    fn wait_for_exit(&self) {
        let exits = self.exits.lock().unwrap();
        let (exits, timeout) = self
            .exited
            .wait_timeout_while(exits, Duration::from_secs(10), |exits| exits.is_empty())
            .unwrap();
        assert!(
            !timeout.timed_out(),
            "the PTY never delivered its exit event: {exits:?}"
        );
    }
}

impl PtySink for RecordingSink {
    fn output(&self, _pty_id: u32, bytes: &[u8]) {
        self.output.lock().unwrap().extend_from_slice(bytes);
    }

    fn exited(&self, pty_id: u32, code: Option<u32>, signal: Option<&str>) {
        self.exits.lock().unwrap().push(Exit {
            pty_id,
            code,
            signal: signal.map(str::to_owned),
        });
        self.exited.notify_all();
    }
}

#[derive(Default)]
struct ExitOrderingState {
    preparing: bool,
    events: Vec<&'static str>,
}

#[derive(Default)]
struct BackpressuredExitSink {
    state: Mutex<ExitOrderingState>,
    changed: Condvar,
}

impl BackpressuredExitSink {
    fn wait_for_exit(&self) -> Vec<&'static str> {
        let state = self.state.lock().unwrap();
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(5), |state| {
                !state.events.contains(&"exited")
            })
            .unwrap();
        assert!(!timeout.timed_out(), "final PTY exit was never delivered");
        state.events.clone()
    }
}

impl PtySink for BackpressuredExitSink {
    fn output(&self, _pty_id: u32, _bytes: &[u8]) {
        let mut state = self.state.lock().unwrap();
        state.events.push("output-started");
        self.changed.notify_all();
        while !state.preparing {
            state = self.changed.wait(state).unwrap();
        }
        state.events.push("output-finished");
    }

    fn preparing_exit(&self, _pty_id: u32) {
        let mut state = self.state.lock().unwrap();
        state.preparing = true;
        state.events.push("preparing-exit");
        self.changed.notify_all();
    }

    fn exited(&self, _pty_id: u32, _code: Option<u32>, _signal: Option<&str>) {
        let mut state = self.state.lock().unwrap();
        state.events.push("exited");
        self.changed.notify_all();
    }
}

#[test]
fn spawn_delivers_output_and_exactly_one_exit_to_its_sink() {
    let _pty_test_lock = PTY_TEST_LOCK.lock().unwrap();
    let manager = PtyManager::default();
    let sink = Arc::new(RecordingSink::default());

    let id = manager
        .spawn(PtySpawnSpec::command("printf first"), sink.clone())
        .expect("spawn PTY");
    sink.wait_for_exit();

    assert_eq!(sink.output(), b"first");
    assert_eq!(
        sink.exits(),
        vec![Exit {
            pty_id: id,
            code: Some(0),
            signal: None,
        }],
        "a PTY must report exactly one terminal exit to its own sink"
    );
}

#[test]
fn child_exit_cancels_output_backpressure_before_ordered_final_exit() {
    let _pty_test_lock = PTY_TEST_LOCK.lock().unwrap();
    let manager = PtyManager::default();
    let sink = Arc::new(BackpressuredExitSink::default());

    manager
        .spawn(
            PtySpawnSpec::command("printf output-before-exit; sleep 0.1"),
            sink.clone(),
        )
        .expect("spawn PTY");
    let events = sink.wait_for_exit();

    let output_finished = events
        .iter()
        .position(|event| *event == "output-finished")
        .expect("output callback never finished");
    let exited = events
        .iter()
        .position(|event| *event == "exited")
        .expect("final exit was not delivered");
    assert!(
        output_finished < exited,
        "final exit overtook output delivery: {events:?}"
    );
}

#[test]
fn each_spawn_routes_bytes_only_to_its_own_sink() {
    let _pty_test_lock = PTY_TEST_LOCK.lock().unwrap();
    let manager = PtyManager::default();
    let first = Arc::new(RecordingSink::default());
    let second = Arc::new(RecordingSink::default());

    let first_id = manager
        .spawn(PtySpawnSpec::command("printf alpha"), first.clone())
        .expect("spawn first PTY");
    let second_id = manager
        .spawn(PtySpawnSpec::command("printf beta"), second.clone())
        .expect("spawn second PTY");
    first.wait_for_exit();
    second.wait_for_exit();

    assert_eq!(first.output(), b"alpha");
    assert_eq!(second.output(), b"beta");
    assert_eq!(first.exits()[0].pty_id, first_id);
    assert_eq!(second.exits()[0].pty_id, second_id);
}

#[test]
fn a_naturally_exited_pty_is_removed_before_its_sink_is_notified() {
    let _pty_test_lock = PTY_TEST_LOCK.lock().unwrap();
    let manager = PtyManager::default();
    let sink = Arc::new(RecordingSink::default());

    let id = manager
        .spawn(PtySpawnSpec::command("printf finished"), sink.clone())
        .expect("spawn PTY");
    sink.wait_for_exit();

    assert_eq!(
        manager.write(id, b"late input").unwrap_err(),
        "no such pty",
        "a reaped PTY must no longer retain a writer"
    );
    assert_eq!(
        manager.resize(id, 100, 30).unwrap_err(),
        "no such pty",
        "a reaped PTY must no longer retain its master"
    );
}

#[derive(Default)]
struct FakePty {
    next_id: AtomicU32,
    sinks: Mutex<HashMap<u32, Arc<dyn PtySink>>>,
    writes: Mutex<Vec<(u32, Vec<u8>)>>,
    resizes: Mutex<Vec<(u32, u16, u16)>>,
    kills: Mutex<Vec<u32>>,
    descendants: Mutex<HashMap<u32, u32>>,
}

impl FakePty {
    fn last_id(&self) -> u32 {
        self.next_id.load(Ordering::SeqCst)
    }

    fn emit_output(&self, id: u32, bytes: &[u8]) {
        let sink = self
            .sinks
            .lock()
            .unwrap()
            .get(&id)
            .expect("fake PTY has a registered sink")
            .clone();
        sink.output(id, bytes);
    }

    fn exit(&self, id: u32, code: Option<u32>, signal: Option<&str>) {
        let sink = self
            .sinks
            .lock()
            .unwrap()
            .remove(&id)
            .expect("fake PTY has a registered sink");
        sink.exited(id, code, signal);
    }

    fn prepare_exit(&self, id: u32) {
        let sink = self
            .sinks
            .lock()
            .unwrap()
            .get(&id)
            .expect("fake PTY has a registered sink")
            .clone();
        sink.preparing_exit(id);
    }

    fn writes(&self) -> Vec<Vec<u8>> {
        self.writes
            .lock()
            .unwrap()
            .iter()
            .map(|(_, bytes)| bytes.clone())
            .collect()
    }

    fn clear_writes(&self) {
        self.writes.lock().unwrap().clear();
    }

    fn resizes(&self) -> Vec<(u16, u16)> {
        self.resizes
            .lock()
            .unwrap()
            .iter()
            .map(|(_, cols, rows)| (*cols, *rows))
            .collect()
    }

    fn map_descendant(&self, pid: u32, pty_id: u32) {
        self.descendants.lock().unwrap().insert(pid, pty_id);
    }
}

impl PtyBackend for FakePty {
    fn spawn(&self, spec: PtySpawnSpec, sink: Arc<dyn PtySink>) -> Result<u32, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        self.sinks.lock().unwrap().insert(id, sink.clone());
        if spec.command.as_deref() == Some("emit-first") {
            sink.output(id, b"ready");
        } else if spec.command.as_deref() == Some("query-first") {
            sink.output(id, b"\x1b[5n");
        } else if spec.command.as_deref() == Some("exit-first") {
            self.sinks.lock().unwrap().remove(&id);
            sink.exited(id, Some(0), None);
        }
        Ok(id)
    }

    fn write(&self, id: u32, bytes: &[u8]) -> Result<(), String> {
        self.writes.lock().unwrap().push((id, bytes.to_vec()));
        Ok(())
    }

    fn resize(&self, id: u32, cols: u16, rows: u16) -> Result<(), String> {
        self.resizes.lock().unwrap().push((id, cols, rows));
        Ok(())
    }

    fn kill(&self, id: u32) {
        self.kills.lock().unwrap().push(id);
        if let Some(sink) = self.sinks.lock().unwrap().remove(&id) {
            sink.exited(id, None, None);
        }
    }

    fn pty_for_descendant(&self, pid: u32) -> Option<u32> {
        self.descendants.lock().unwrap().get(&pid).copied()
    }
}

fn size(cols: u16, rows: u16) -> TerminalSize {
    TerminalSize::try_new(cols, rows).expect("test terminal dimensions are valid")
}

fn shell_launch(slot: &str) -> TabLaunch {
    TabLaunch::new("Shell", slot, size(80, 24))
}

fn registry() -> (TabRegistry, Arc<FakePty>) {
    let pty = Arc::new(FakePty::default());
    (TabRegistry::with_backend(pty.clone()), pty)
}

#[test]
fn registry_refuses_a_tab_that_would_make_roster_recovery_unbounded() {
    let (registry, _) = registry();
    for index in 0..128 {
        registry
            .open(shell_launch(&format!("bounded-roster-{index}")))
            .unwrap();
    }

    let error = registry.open(shell_launch("one-tab-too-many")).unwrap_err();
    assert_eq!(error.code(), "tab.too_many_tabs");
}

#[test]
fn registry_rejects_metadata_that_cannot_fit_the_bounded_roster_transfer() {
    let (registry, _) = registry();
    let error = registry
        .open(TabLaunch::new(
            "x".repeat(33 * 1024),
            "oversized-roster-entry",
            size(80, 24),
        ))
        .unwrap_err();

    assert_eq!(error.code(), "tab.metadata_too_large");
    assert!(registry.list().is_empty());
}

#[test]
fn registry_change_stream_recovers_overflow_with_a_current_snapshot() {
    let pty = Arc::new(FakePty::default());
    let registry = TabRegistry::with_backend_and_queue_capacity(pty, 1);
    let changes = registry.subscribe_changes();
    assert!(matches!(
        changes.recv_timeout(Duration::from_secs(1)).unwrap(),
        TabRegistryEvent::Snapshot { revision: 0, tabs } if tabs.is_empty()
    ));

    let tab = registry.open(shell_launch("stream-overflow")).unwrap();
    registry
        .update(&tab, TabUpdate::new().title("latest title"))
        .unwrap();

    assert!(matches!(
        changes.recv_timeout(Duration::from_secs(1)).unwrap(),
        TabRegistryEvent::Snapshot { revision: 2, tabs }
            if tabs.len() == 1
                && tabs[0].id() == &tab
                && tabs[0].title() == "latest title"
    ));
}

#[test]
fn remote_open_and_close_are_process_wide_requested_roster_changes() {
    let (registry, _) = registry();
    let registry = Arc::new(registry);
    let changes = registry.subscribe_changes();
    let _initial = changes.recv_timeout(Duration::from_secs(1)).unwrap();
    let remote = RemoteTerminal::new(registry);

    let tab = remote.open(shell_launch("remote-roster")).unwrap();
    assert!(matches!(
        changes.recv_timeout(Duration::from_secs(1)).unwrap(),
        TabRegistryEvent::Opened { tab: descriptor, .. } if descriptor.id() == &tab
    ));

    remote.close(&tab).unwrap();
    assert!(matches!(
        changes.recv_timeout(Duration::from_secs(1)).unwrap(),
        TabRegistryEvent::Removed { tab_id, requested: true, .. } if tab_id == tab
    ));
}

#[test]
fn desktop_descriptor_projects_safe_focus_owner_and_canonical_dimensions() {
    let (registry, _) = registry();
    let tab = registry
        .open(shell_launch("safe-focus-projection"))
        .unwrap();
    let _desktop = registry.attach(&tab, AttachmentKind::Desktop).unwrap();
    let phone = registry.attach(&tab, AttachmentKind::Remote).unwrap();
    let canonical = size(47, 11);
    registry.take_focus(&tab, &phone.id, canonical).unwrap();

    let encoded = serde_json::to_value(registry.get(&tab).unwrap()).unwrap();
    assert_eq!(encoded["focus"], "remote");
    assert_eq!(encoded["size"]["cols"], 47);
    assert_eq!(encoded["size"]["rows"], 11);
    assert!(encoded.get("inputOwner").is_none());
    assert!(!encoded.to_string().contains(phone.id.as_str()));
}

#[test]
fn registry_stream_projects_focus_size_title_and_unexpected_exit_as_changes() {
    let (registry, pty) = registry();
    let changes = registry.subscribe_changes();
    let _initial = changes.recv_timeout(Duration::from_secs(1)).unwrap();
    let tab = registry.open(shell_launch("registry-controls")).unwrap();
    let _opened = changes.recv_timeout(Duration::from_secs(1)).unwrap();

    let desktop = registry.attach(&tab, AttachmentKind::Desktop).unwrap();
    assert!(matches!(
        changes.recv_timeout(Duration::from_secs(1)).unwrap(),
        TabRegistryEvent::Changed { tab: descriptor, .. }
            if descriptor.focus() == aiterm_lib::tabs::TabFocus::Desktop
    ));
    let phone = registry.attach(&tab, AttachmentKind::Remote).unwrap();
    registry.take_focus(&tab, &phone.id, size(53, 13)).unwrap();
    assert!(matches!(
        changes.recv_timeout(Duration::from_secs(1)).unwrap(),
        TabRegistryEvent::Changed { tab: descriptor, .. }
            if descriptor.focus() == aiterm_lib::tabs::TabFocus::Remote
                && descriptor.size() == size(53, 13)
    ));

    pty.emit_output(pty.last_id(), b"\x1b]2;registry title\x07");
    assert!(matches!(
        changes.recv_timeout(Duration::from_secs(1)).unwrap(),
        TabRegistryEvent::Changed { tab: descriptor, .. }
            if descriptor.title() == "registry title"
    ));

    pty.exit(pty.last_id(), Some(17), None);
    assert!(matches!(
        changes.recv_timeout(Duration::from_secs(1)).unwrap(),
        TabRegistryEvent::Changed { tab: descriptor, .. }
            if descriptor.state() == &TabState::Exited
                && descriptor.exit().is_some_and(|exit| !exit.requested())
    ));
    assert_eq!(
        registry.list().len(),
        1,
        "unexpected exits stay recoverable in the roster"
    );
    drop(desktop);
}

fn text(snapshot: &aiterm_lib::terminal::model::ScreenSnapshot) -> Vec<String> {
    snapshot
        .visible()
        .iter()
        .map(|row| row.cells().iter().map(|cell| cell.text()).collect())
        .collect()
}

fn recv_matching(
    receiver: &aiterm_lib::tabs::TabEventReceiver,
    predicate: impl Fn(&TabEvent) -> bool,
) -> TabEvent {
    for _ in 0..10 {
        let event = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("expected a tab event");
        if predicate(&event) {
            return event;
        }
    }
    panic!("matching tab event was not delivered");
}

#[test]
fn registry_lists_a_tab_and_owns_its_first_output_before_any_client_attaches() {
    let (registry, _) = registry();
    let id = registry
        .open(shell_launch("shell:ready").with_command("emit-first"))
        .unwrap();

    assert_eq!(registry.list()[0].id(), &id);
    assert!(text(&registry.snapshot(&id).unwrap())[0].contains("ready"));
}

#[test]
fn first_desktop_attachment_replays_raw_output_emitted_while_opening() {
    let (registry, _) = registry();
    let id = registry
        .open_desktop(shell_launch("shell:desktop-ready").with_command("emit-first"))
        .unwrap();

    let desktop = registry.attach(&id, AttachmentKind::Desktop).unwrap();

    assert_eq!(
        recv_matching(&desktop.events, |event| matches!(event, TabEvent::Raw(_))),
        TabEvent::Raw(b"ready".to_vec()),
    );
}

#[test]
fn opening_output_waits_for_xterm_to_answer_terminal_queries_once() {
    let (registry, pty) = registry();

    let tab = registry
        .open_desktop(shell_launch("shell:query").with_command("query-first"))
        .unwrap();
    let desktop = registry.attach(&tab, AttachmentKind::Desktop).unwrap();

    assert!(pty.writes().is_empty());
    assert_eq!(
        recv_matching(&desktop.events, |event| matches!(event, TabEvent::Raw(_))),
        TabEvent::Raw(b"\x1b[5n".to_vec()),
    );
}

#[test]
fn registry_mints_random_non_numeric_tab_and_attachment_ids() {
    let (registry, _) = registry();
    let first = registry.open(shell_launch("slot:first")).unwrap();
    let second = registry.open(shell_launch("slot:second")).unwrap();
    let attachment = registry.attach(&first, AttachmentKind::Remote).unwrap();

    assert_ne!(first, second);
    assert!(uuid::Uuid::parse_str(first.as_str()).is_ok());
    assert!(uuid::Uuid::parse_str(second.as_str()).is_ok());
    assert!(uuid::Uuid::parse_str(attachment.id.as_str()).is_ok());
    assert!(first.as_str().parse::<u32>().is_err());
    assert!(attachment.id.as_str().parse::<u32>().is_err());

    let encoded = serde_json::to_value(registry.list()).unwrap();
    assert_eq!(encoded[0]["id"], first.as_str());
    assert!(encoded[0].get("ptyId").is_none());
}

#[test]
fn registry_rejects_duplicate_slots_without_spawning_a_second_pty() {
    let (registry, pty) = registry();
    registry.open(shell_launch("claude:repo")).unwrap();

    let error = registry.open(shell_launch("claude:repo")).unwrap_err();

    assert_eq!(error.code(), "tab.slot_in_use");
    assert_eq!(pty.last_id(), 1);
}

#[test]
fn registry_updates_authoritative_metadata_without_changing_tab_identity() {
    let (registry, _) = registry();
    let id = registry.open(shell_launch("slot:old")).unwrap();

    let updated = registry
        .update(
            &id,
            TabUpdate::new()
                .title("Renamed")
                .session_id("session-1")
                .agent_id("claude"),
        )
        .unwrap();

    assert_eq!(updated.id(), &id);
    assert_eq!(updated.title(), "Renamed");
    assert_eq!(updated.session_id(), Some("session-1"));
    assert_eq!(updated.agent_id(), Some("claude"));
    assert_eq!(registry.list(), vec![updated]);
}

#[test]
fn session_hook_rekeys_the_slot_atomically_and_releases_the_old_slot() {
    let (registry, _) = registry();
    let id = registry.open(shell_launch("claude:repo")).unwrap();

    let descriptor = registry.rekey_session(&id, "session-new").unwrap();

    assert_eq!(descriptor.id(), &id);
    assert_eq!(descriptor.slot_id(), "session-new");
    assert_eq!(descriptor.session_id(), Some("session-new"));
    registry.open(shell_launch("claude:repo")).unwrap();
    assert_eq!(
        registry
            .open(shell_launch("session-new"))
            .unwrap_err()
            .code(),
        "tab.slot_in_use"
    );
}

#[test]
fn rekeying_a_resumed_codex_tab_retires_the_old_session_identity_everywhere() {
    let (registry, pty) = registry();
    let id = registry
        .open(
            TabLaunch::new("Codex", "session-old", size(80, 24))
                .with_agent_id("codex")
                .with_session_id("session-old")
                .with_resumed_id("session-old"),
        )
        .unwrap();
    let original_pty = pty.last_id();
    let remote = registry.attach(&id, AttachmentKind::Remote).unwrap();
    let changes = registry.subscribe_changes();
    let _initial_roster = changes.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(registry.has_session("session-old"));
    assert!(!registry.has_session("session-new"));

    let descriptor = registry.rekey_session(&id, "session-new").unwrap();

    assert!(!registry.has_session("session-old"));
    assert!(registry.has_session("session-new"));
    assert_eq!(descriptor.id(), &id);
    assert_eq!(descriptor.session_id(), Some("session-new"));
    assert_eq!(descriptor.slot_id(), "session-new");
    assert_eq!(descriptor.resumed_id(), Some("session-old"));
    assert_eq!(registry.list(), vec![descriptor.clone()]);
    assert_eq!(
        registry
            .write_session_str("session-old", "must not reach the PTY")
            .unwrap_err(),
        "session is not open in a tab"
    );
    assert!(pty.writes().is_empty());

    registry
        .write_session_str("session-new", "input for the rekeyed session")
        .unwrap();
    assert_eq!(
        *pty.writes.lock().unwrap(),
        vec![(original_pty, b"input for the rekeyed session".to_vec())]
    );
    assert_eq!(pty.last_id(), original_pty, "rekey must not spawn a new PTY");
    assert!(matches!(
        changes.recv_timeout(Duration::from_secs(1)).unwrap(),
        TabRegistryEvent::Changed { tab, .. } if tab == descriptor
    ));
    assert!(matches!(
        recv_matching(&remote.events, |event| matches!(event, TabEvent::Metadata(_))),
        TabEvent::Metadata(tab) if tab == descriptor
    ));
}

#[test]
fn descendant_lookup_returns_an_opaque_tab_id_not_the_numeric_pty_id() {
    let (registry, pty) = registry();
    let id = registry.open(shell_launch("slot:descendant")).unwrap();
    pty.map_descendant(44_001, pty.last_id());

    assert_eq!(registry.tab_for_descendant(44_001), Some(id));
    assert_eq!(registry.tab_for_descendant(44_002), None);
}

#[test]
fn phone_cannot_input_or_resize_until_it_takes_focus() {
    let (registry, pty) = registry();
    let tab = registry.open(shell_launch("slot:focus")).unwrap();
    let desktop = registry.attach(&tab, AttachmentKind::Desktop).unwrap();
    let phone = registry.attach(&tab, AttachmentKind::Remote).unwrap();

    assert_eq!(
        registry.input(&tab, &phone.id, b"x").unwrap_err().code(),
        "terminal.input_not_owned"
    );
    registry.take_focus(&tab, &phone.id, size(42, 18)).unwrap();
    assert_eq!(registry.input(&tab, &phone.id, b"x"), Ok(()));
    assert_eq!(
        registry
            .resize(&tab, &desktop.id, size(80, 24))
            .unwrap_err()
            .code(),
        "terminal.input_not_owned"
    );
    assert_eq!(pty.writes(), vec![b"x".to_vec()]);
    assert_eq!(pty.resizes(), vec![(42, 18)]);

    let focus = recv_matching(
        &desktop.events,
        |event| matches!(event, TabEvent::FocusChanged { owner: Some(owner), .. } if owner == &phone.id),
    );
    assert!(matches!(
        focus,
        TabEvent::FocusChanged { size: current, .. } if current == size(42, 18)
    ));
}

#[test]
fn desktop_receives_raw_bytes_and_never_typed_screen_frames() {
    let (registry, pty) = registry();
    let tab = registry.open(shell_launch("slot:desktop-raw")).unwrap();
    let desktop = registry.attach(&tab, AttachmentKind::Desktop).unwrap();
    let remote = registry.attach(&tab, AttachmentKind::Remote).unwrap();
    let _ = remote
        .events
        .recv_timeout(Duration::from_secs(1))
        .expect("remote initial snapshot");

    pty.emit_output(pty.last_id(), b"raw\0bytes");

    assert_eq!(
        recv_matching(&desktop.events, |event| matches!(event, TabEvent::Raw(_))),
        TabEvent::Raw(b"raw\0bytes".to_vec())
    );
    while let Ok(event) = desktop.events.try_recv() {
        assert!(!matches!(event, TabEvent::Snapshot(_) | TabEvent::Diff(_)));
    }
}

#[test]
fn remote_attach_gets_a_viewport_snapshot_then_typed_diffs_and_no_raw_bytes() {
    let (registry, pty) = registry();
    let tab = registry.open(shell_launch("slot:remote-diff")).unwrap();
    pty.emit_output(pty.last_id(), b"before\r\nline\r\nold");
    let remote = registry.attach(&tab, AttachmentKind::Remote).unwrap();

    let snapshot = recv_matching(&remote.events, |event| {
        matches!(event, TabEvent::Snapshot(_))
    });
    let TabEvent::Snapshot(snapshot) = snapshot else {
        unreachable!()
    };
    assert!(snapshot.scrollback().is_empty());
    assert!(text(&snapshot).iter().any(|line| line.contains("before")));

    pty.emit_output(pty.last_id(), b"\rnew");
    let event = remote
        .events
        .recv_timeout(Duration::from_secs(1))
        .expect("remote typed damage");
    assert!(matches!(event, TabEvent::Diff(diff) if diff.tab_id() == tab.as_str()));
    while let Ok(event) = remote.events.try_recv() {
        assert!(!matches!(event, TabEvent::Raw(_)));
    }
}

#[test]
fn scrollback_is_paged_separately_from_remote_viewport_snapshots() {
    let (registry, pty) = registry();
    let tab = registry
        .open(TabLaunch::new("Small", "slot:scrollback", size(8, 2)))
        .unwrap();
    pty.emit_output(pty.last_id(), b"one\r\ntwo\r\nthree");
    let remote = registry.attach(&tab, AttachmentKind::Remote).unwrap();
    let TabEvent::Snapshot(snapshot) = recv_matching(&remote.events, |event| {
        matches!(event, TabEvent::Snapshot(_))
    }) else {
        unreachable!()
    };

    assert!(snapshot.scrollback().is_empty());
    assert_eq!(registry.scrollback(&tab, 0, 1).unwrap().len(), 1);
}

#[test]
fn desktop_owned_queries_suppress_rust_replies_because_xterm_answers_them() {
    let (registry, pty) = registry();
    let tab = registry.open(shell_launch("slot:xterm-reply")).unwrap();
    let _desktop = registry.attach(&tab, AttachmentKind::Desktop).unwrap();

    pty.emit_output(pty.last_id(), b"\x1b[5n");

    assert!(pty.writes().is_empty());
}

#[test]
fn remote_focus_suppresses_xterm_input_and_delivers_the_rust_reply() {
    let (registry, pty) = registry();
    let tab = registry.open(shell_launch("slot:rust-reply")).unwrap();
    let desktop = registry.attach(&tab, AttachmentKind::Desktop).unwrap();
    let remote = registry.attach(&tab, AttachmentKind::Remote).unwrap();
    registry.take_focus(&tab, &remote.id, size(80, 24)).unwrap();

    assert_eq!(
        registry
            .input(&tab, &desktop.id, b"\x1b[0n")
            .unwrap_err()
            .code(),
        "terminal.input_not_owned"
    );
    pty.emit_output(pty.last_id(), b"\x1b[5n");

    assert_eq!(pty.writes(), vec![b"\x1b[0n".to_vec()]);
}

#[test]
fn desktop_open_hands_terminal_query_replies_to_the_focused_remote() {
    let (registry, pty) = registry();
    let tab = registry
        .open_desktop(shell_launch("slot:desktop-remote-reply"))
        .unwrap();
    let desktop = registry.attach(&tab, AttachmentKind::Desktop).unwrap();
    let remote = registry.attach(&tab, AttachmentKind::Remote).unwrap();
    registry.take_focus(&tab, &remote.id, size(80, 24)).unwrap();

    // Opening replay completes on its own worker. Poll the observable reply
    // boundary rather than sleeping: before handoff xterm owns the query;
    // afterwards the focused remote does and Rust answers it.
    let handoff_deadline = Instant::now() + Duration::from_secs(1);
    while pty.writes().is_empty() {
        pty.emit_output(pty.last_id(), b"\x1b[5n");
        while desktop.events.try_recv().is_ok() {}
        assert!(
            Instant::now() < handoff_deadline,
            "desktop opening replay never completed"
        );
        thread::yield_now();
    }
    assert_eq!(
        pty.writes(),
        vec![b"\x1b[0n".to_vec()],
        "a desktop-opened tab never handed query replies to its focused remote"
    );

    pty.clear_writes();
    pty.emit_output(pty.last_id(), b"\x1b[5n");
    assert_eq!(pty.writes(), vec![b"\x1b[0n".to_vec()]);

    registry
        .take_focus(&tab, &desktop.id, size(80, 24))
        .unwrap();
    pty.clear_writes();
    pty.emit_output(pty.last_id(), b"\x1b[5n");
    assert!(
        pty.writes().is_empty(),
        "Rust answered a terminal query while desktop xterm owned input"
    );
}

#[test]
fn natural_exit_and_explicit_close_each_notify_once_and_cleanup_pty_mapping() {
    let (registry, pty) = registry();
    let natural = registry.open(shell_launch("slot:natural")).unwrap();
    let attachment = registry.attach(&natural, AttachmentKind::Remote).unwrap();
    let natural_pty = pty.last_id();
    pty.map_descendant(91_001, natural_pty);

    pty.exit(natural_pty, Some(7), None);

    assert!(matches!(
        recv_matching(&attachment.events, |event| matches!(event, TabEvent::Exited(_))),
        TabEvent::Exited(exit) if exit.code() == Some(7) && !exit.requested()
    ));
    assert_eq!(registry.get(&natural).unwrap().state(), &TabState::Exited);
    assert_eq!(registry.tab_for_descendant(91_001), None);
    registry.close(&natural).unwrap();
    assert!(attachment.events.try_recv().is_err());

    let explicit = registry.open(shell_launch("slot:explicit")).unwrap();
    let attachment = registry.attach(&explicit, AttachmentKind::Remote).unwrap();
    registry.close(&explicit).unwrap();
    assert!(matches!(
        recv_matching(&attachment.events, |event| matches!(event, TabEvent::Exited(_))),
        TabEvent::Exited(exit) if exit.requested()
    ));
    assert_eq!(registry.get(&explicit).unwrap_err().code(), "tab.not_found");
    assert!(attachment.events.try_recv().is_err());
}

#[test]
fn dropping_an_attachment_cleans_it_up_and_releases_focus_ownership() {
    let (registry, pty) = registry();
    let tab = registry.open(shell_launch("slot:drop")).unwrap();
    let desktop = registry.attach(&tab, AttachmentKind::Desktop).unwrap();
    let old_id = desktop.id.clone();
    drop(desktop);
    let replacement = registry.attach(&tab, AttachmentKind::Desktop).unwrap();

    assert_eq!(
        registry.input(&tab, &old_id, b"stale").unwrap_err().code(),
        "terminal.attachment_not_found"
    );
    registry.input(&tab, &replacement.id, b"live").unwrap();
    assert_eq!(pty.writes(), vec![b"live".to_vec()]);
}

#[test]
fn explicit_attachment_cancellation_wakes_receivers_and_releases_focus_once() {
    let (registry, pty) = registry();
    let tab = registry.open(shell_launch("slot:cancel")).unwrap();
    let remote = registry.attach(&tab, AttachmentKind::Remote).unwrap();
    let _ = remote.events.recv_timeout(Duration::from_secs(1)).unwrap();
    registry.take_focus(&tab, &remote.id, size(80, 24)).unwrap();
    assert_eq!(registry.attachment_count(&tab).unwrap(), 1);

    remote.cancellation.cancel();
    remote.cancellation.cancel();

    assert_eq!(registry.attachment_count(&tab).unwrap(), 0);
    assert!(registry.get(&tab).unwrap().input_owner().is_none());
    assert!(remote
        .events
        .recv_timeout(Duration::from_millis(20))
        .is_err());
    assert!(registry.input(&tab, &remote.id, b"stale").is_err());
    assert!(pty.writes().is_empty());
}

#[test]
fn scrollback_page_captures_revision_and_rows_under_one_output_boundary() {
    let (registry, pty) = registry();
    let tab = registry
        .open(TabLaunch::new(
            "Small",
            "slot:atomic-scrollback",
            size(8, 2),
        ))
        .unwrap();
    pty.emit_output(pty.last_id(), b"one\r\ntwo\r\nthree");

    let page = registry.scrollback_page(&tab, 0, 1).unwrap();

    assert_eq!(page.rows().len(), 1);
    assert_eq!(page.revision(), registry.snapshot(&tab).unwrap().revision());
}

#[test]
fn concurrent_output_keeps_atomic_scrollback_page_revisions_monotonic() {
    let (registry, pty) = registry();
    let tab = registry
        .open(TabLaunch::new(
            "Small",
            "slot:concurrent-atomic-scrollback",
            size(8, 2),
        ))
        .unwrap();
    let pty_id = pty.last_id();
    let completed = Arc::new(AtomicUsize::new(0));
    let producer_pty = pty.clone();
    let producer_completed = completed.clone();
    let producer = thread::spawn(move || {
        for index in 0..200 {
            producer_pty.emit_output(pty_id, format!("{index:04}\r\n").as_bytes());
            producer_completed.store(index + 1, Ordering::Release);
        }
    });

    let mut previous = aiterm_lib::terminal::model::Revision(0);
    while completed.load(Ordering::Acquire) < 200 {
        let page = registry.scrollback_page(&tab, 0, 16).unwrap();
        assert!(page.revision().0 >= previous.0);
        assert!(page.rows().len() <= 16);
        previous = page.revision();
        thread::yield_now();
    }
    producer.join().unwrap();
    let page = registry.scrollback_page(&tab, 0, 16).unwrap();
    assert_eq!(page.revision(), registry.snapshot(&tab).unwrap().revision());
}

#[test]
fn remote_queue_loss_discards_stale_diffs_and_recovers_with_a_snapshot() {
    let pty = Arc::new(FakePty::default());
    let registry = TabRegistry::with_backend_and_queue_capacity(pty.clone(), 1);
    let tab = registry.open(shell_launch("slot:overflow")).unwrap();
    let remote = registry.attach(&tab, AttachmentKind::Remote).unwrap();
    let _initial = remote.events.recv_timeout(Duration::from_secs(1)).unwrap();

    pty.emit_output(pty.last_id(), b"one");
    pty.emit_output(pty.last_id(), b"\rtwo");

    let recovered = remote.events.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(matches!(recovered, TabEvent::Snapshot(_)));
    assert!(text(match &recovered {
        TabEvent::Snapshot(snapshot) => snapshot,
        _ => unreachable!(),
    })[0]
        .contains("two"));
    assert!(remote.events.try_recv().is_err());
}

#[test]
fn registry_rejects_unknown_tab_and_attachment_ids_consistently() {
    let (registry, _) = registry();
    let tab = registry.open(shell_launch("slot:unknown")).unwrap();
    let unknown_tab = TabId::new();
    let unknown_attachment = AttachmentId::new();

    assert_eq!(
        registry.snapshot(&unknown_tab).unwrap_err().code(),
        "tab.not_found"
    );
    assert_eq!(
        registry
            .attach(&unknown_tab, AttachmentKind::Remote)
            .unwrap_err()
            .code(),
        "tab.not_found"
    );
    assert_eq!(
        registry
            .input(&tab, &unknown_attachment, b"x")
            .unwrap_err()
            .code(),
        "terminal.attachment_not_found"
    );
    assert_eq!(
        registry
            .resize(&tab, &unknown_attachment, size(80, 24))
            .unwrap_err()
            .code(),
        "terminal.attachment_not_found"
    );
    assert_eq!(
        registry.close(&unknown_tab).unwrap_err().code(),
        "tab.not_found"
    );
}

#[derive(Default)]
struct BlockingSpawnPty {
    next_id: AtomicU32,
    state: Mutex<BlockingSpawnState>,
    changed: Condvar,
}

#[derive(Default)]
struct BlockingSpawnState {
    entered: bool,
    released: bool,
    sink: Option<Arc<dyn PtySink>>,
}

impl BlockingSpawnPty {
    fn wait_until_entered(&self) {
        let state = self.state.lock().unwrap();
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(5), |state| !state.entered)
            .unwrap();
        assert!(!timeout.timed_out(), "spawn did not reach its barrier");
        drop(state);
    }

    fn release(&self) {
        let mut state = self.state.lock().unwrap();
        state.released = true;
        self.changed.notify_all();
    }
}

impl PtyBackend for BlockingSpawnPty {
    fn spawn(&self, spec: PtySpawnSpec, sink: Arc<dyn PtySink>) -> Result<u32, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        if spec.command.as_deref() != Some("block-spawn") {
            return Ok(id);
        }
        let mut state = self.state.lock().unwrap();
        state.entered = true;
        state.sink = Some(sink);
        self.changed.notify_all();
        while !state.released {
            state = self.changed.wait(state).unwrap();
        }
        Ok(id)
    }

    fn write(&self, _id: u32, _bytes: &[u8]) -> Result<(), String> {
        Ok(())
    }

    fn resize(&self, _id: u32, _cols: u16, _rows: u16) -> Result<(), String> {
        Ok(())
    }

    fn kill(&self, _id: u32) {}

    fn pty_for_descendant(&self, _pid: u32) -> Option<u32> {
        None
    }
}

#[test]
fn a_blocked_spawn_is_unpublished_so_close_cannot_find_a_phantom_tab() {
    let pty = Arc::new(BlockingSpawnPty::default());
    let registry = TabRegistry::with_backend(pty.clone());
    let opener = registry.clone();
    let open = thread::spawn(move || {
        opener.open(shell_launch("slot:blocked-spawn").with_command("block-spawn"))
    });
    pty.wait_until_entered();

    assert!(registry.list().is_empty());
    assert_eq!(
        registry.close(&TabId::new()).unwrap_err().code(),
        "tab.not_found"
    );

    pty.release();
    let id = open.join().unwrap().unwrap();
    assert_eq!(registry.list()[0].id(), &id);
}

#[test]
fn a_pending_open_reservation_blocks_live_rekey_and_keeps_one_slot_owner() {
    let pty = Arc::new(BlockingSpawnPty::default());
    let registry = TabRegistry::with_backend(pty.clone());
    let live = registry.open(shell_launch("slot:live")).unwrap();
    let opener = registry.clone();
    let open = thread::spawn(move || {
        opener.open(shell_launch("slot:reserved").with_command("block-spawn"))
    });
    pty.wait_until_entered();

    assert_eq!(
        registry
            .update(&live, TabUpdate::new().slot_id("slot:reserved"))
            .unwrap_err()
            .code(),
        "tab.slot_in_use"
    );

    pty.release();
    let opened = open.join().unwrap().unwrap();
    let descriptors = registry.list();
    assert_eq!(
        descriptors
            .iter()
            .filter(|descriptor| descriptor.slot_id() == "slot:reserved")
            .count(),
        1
    );
    assert_eq!(registry.get(&opened).unwrap().slot_id(), "slot:reserved");
    assert_eq!(registry.get(&live).unwrap().slot_id(), "slot:live");
}

#[test]
fn a_tab_that_exited_during_spawn_is_coherent_and_rejects_late_attach() {
    let (registry, _) = registry();
    let id = registry
        .open(shell_launch("slot:exit-first").with_command("exit-first"))
        .unwrap();

    assert_eq!(registry.get(&id).unwrap().state(), &TabState::Exited);
    assert_eq!(
        registry
            .attach(&id, AttachmentKind::Desktop)
            .unwrap_err()
            .code(),
        "tab.closed"
    );
    assert_eq!(
        registry
            .attach(&id, AttachmentKind::Remote)
            .unwrap_err()
            .code(),
        "tab.closed"
    );
}

#[test]
fn remote_screen_overflow_atomically_replaces_stale_diffs_then_keeps_later_diffs() {
    let pty = Arc::new(FakePty::default());
    let registry = TabRegistry::with_backend_and_queue_capacity(pty.clone(), 2);
    let tab = registry
        .open(shell_launch("slot:ordered-recovery"))
        .unwrap();
    let remote = registry.attach(&tab, AttachmentKind::Remote).unwrap();
    let _initial = remote.events.recv_timeout(Duration::from_secs(1)).unwrap();

    pty.emit_output(pty.last_id(), b"one");
    pty.emit_output(pty.last_id(), b"\rtwo");
    pty.emit_output(pty.last_id(), b"\rthree");
    pty.emit_output(pty.last_id(), b"\rfour");

    let TabEvent::Snapshot(mut recovered) =
        remote.events.recv_timeout(Duration::from_secs(1)).unwrap()
    else {
        panic!("overflow must atomically replace stale diffs with a snapshot");
    };
    let TabEvent::Diff(after) = remote.events.recv_timeout(Duration::from_secs(1)).unwrap() else {
        panic!("a diff produced after recovery snapshot replacement must be retained");
    };
    recovered.apply(after).unwrap();
    assert_eq!(recovered, registry.snapshot(&tab).unwrap());
}

#[test]
fn full_remote_screen_queue_retains_control_events_and_exit_exactly_once() {
    let pty = Arc::new(FakePty::default());
    let registry = TabRegistry::with_backend_and_queue_capacity(pty.clone(), 1);
    let tab = registry
        .open(shell_launch("slot:control-overflow"))
        .unwrap();
    let remote = registry.attach(&tab, AttachmentKind::Remote).unwrap();

    registry
        .update(&tab, TabUpdate::new().title("metadata-title"))
        .unwrap();
    registry.take_focus(&tab, &remote.id, size(70, 20)).unwrap();
    pty.emit_output(pty.last_id(), b"\x1b]2;terminal-title\x07\x07");
    pty.exit(pty.last_id(), Some(9), Some("Killed"));

    let mut events = Vec::new();
    loop {
        match remote.events.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => events.push(event),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                panic!("exited mailbox did not close after retained events")
            }
        }
    }

    let final_snapshot = events
        .iter()
        .position(|event| matches!(event, TabEvent::SharedSnapshot(_)))
        .expect("final shared snapshot must be retained");
    assert!(matches!(
        events.get(final_snapshot + 1),
        Some(TabEvent::Exited(_))
    ));
    assert!(events.iter().any(|event| matches!(
        event,
        TabEvent::Metadata(descriptor) if descriptor.title() == "metadata-title"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        TabEvent::FocusChanged { owner: Some(owner), size: current }
            if owner == &remote.id && *current == size(70, 20)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        TabEvent::Title(title) if title == "terminal-title"
    )));
    assert!(events.iter().any(|event| matches!(event, TabEvent::Bell)));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, TabEvent::Exited(_)))
            .count(),
        1
    );
}

#[test]
fn desktop_raw_queue_applies_lossless_backpressure_instead_of_dropping_bytes() {
    let pty = Arc::new(FakePty::default());
    let registry = TabRegistry::with_backend_and_queue_capacity(pty.clone(), 1);
    let tab = registry
        .open(shell_launch("slot:desktop-backpressure"))
        .unwrap();
    let desktop = registry.attach(&tab, AttachmentKind::Desktop).unwrap();
    let _focus = desktop.events.recv_timeout(Duration::from_secs(1)).unwrap();
    pty.emit_output(pty.last_id(), b"first");

    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let second = pty.clone();
    let pty_id = pty.last_id();
    let writer = thread::spawn(move || {
        started_tx.send(()).unwrap();
        second.emit_output(pty_id, b"second");
        done_tx.send(()).unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());

    assert_eq!(
        desktop.events.recv_timeout(Duration::from_secs(1)).unwrap(),
        TabEvent::Raw(b"first".to_vec())
    );
    done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(
        desktop.events.recv_timeout(Duration::from_secs(1)).unwrap(),
        TabEvent::Raw(b"second".to_vec())
    );
    writer.join().unwrap();
}

#[test]
fn dropping_a_full_desktop_receiver_unblocks_the_output_producer() {
    let pty = Arc::new(FakePty::default());
    let registry = TabRegistry::with_backend_and_queue_capacity(pty.clone(), 1);
    let tab = registry
        .open(shell_launch("slot:desktop-drop-unblock"))
        .unwrap();
    let desktop = registry.attach(&tab, AttachmentKind::Desktop).unwrap();
    let _focus = desktop.events.recv_timeout(Duration::from_secs(1)).unwrap();
    pty.emit_output(pty.last_id(), b"first");

    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let second = pty.clone();
    let pty_id = pty.last_id();
    let writer = thread::spawn(move || {
        started_tx.send(()).unwrap();
        second.emit_output(pty_id, b"second");
        done_tx.send(()).unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());

    drop(desktop);
    done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    writer.join().unwrap();
}

#[test]
fn explicit_close_cancels_blocked_raw_output_and_retains_one_exit() {
    let pty = Arc::new(FakePty::default());
    let registry = TabRegistry::with_backend_and_queue_capacity(pty.clone(), 1);
    let tab = registry.open(shell_launch("slot:close-raw-block")).unwrap();
    let desktop = registry.attach(&tab, AttachmentKind::Desktop).unwrap();
    let _focus = desktop.events.recv_timeout(Duration::from_secs(1)).unwrap();
    pty.emit_output(pty.last_id(), b"first");

    let (writer_done_tx, writer_done_rx) = mpsc::channel();
    let output = pty.clone();
    let pty_id = pty.last_id();
    let writer = thread::spawn(move || {
        output.emit_output(pty_id, b"cancelled-second");
        writer_done_tx.send(()).unwrap();
    });
    assert!(writer_done_rx
        .recv_timeout(Duration::from_millis(100))
        .is_err());

    let (close_done_tx, close_done_rx) = mpsc::channel();
    let closer = registry.clone();
    let closing_tab = tab.clone();
    let close = thread::spawn(move || {
        let result = closer.close(&closing_tab);
        close_done_tx.send(result).unwrap();
    });
    close_done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("close stayed blocked behind a full desktop raw queue")
        .unwrap();
    writer_done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("close did not wake the blocked raw producer");

    assert_eq!(
        desktop.events.recv_timeout(Duration::from_secs(1)).unwrap(),
        TabEvent::Raw(b"first".to_vec())
    );
    assert!(matches!(
        desktop.events.recv_timeout(Duration::from_secs(1)).unwrap(),
        TabEvent::Exited(_)
    ));
    assert_eq!(
        desktop.events.recv_timeout(Duration::from_secs(1)),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
    );
    writer.join().unwrap();
    close.join().unwrap();
}

#[test]
fn focus_transfer_cannot_interleave_between_raw_delivery_and_reply_ownership() {
    let pty = Arc::new(FakePty::default());
    let registry = TabRegistry::with_backend_and_queue_capacity(pty.clone(), 1);
    let tab = registry.open(shell_launch("slot:ordered-focus")).unwrap();
    let desktop = registry.attach(&tab, AttachmentKind::Desktop).unwrap();
    let _desktop_focus = desktop.events.recv_timeout(Duration::from_secs(1)).unwrap();
    let remote = registry.attach(&tab, AttachmentKind::Remote).unwrap();
    let _remote_snapshot = remote.events.recv_timeout(Duration::from_secs(1)).unwrap();
    let pty_id = pty.last_id();
    pty.emit_output(pty_id, b"first");

    let (output_done_tx, output_done_rx) = mpsc::channel();
    let output = pty.clone();
    let writer = thread::spawn(move || {
        output.emit_output(pty_id, b"\x1b[5n");
        output_done_tx.send(()).unwrap();
    });
    assert!(output_done_rx
        .recv_timeout(Duration::from_millis(100))
        .is_err());

    let (focus_done_tx, focus_done_rx) = mpsc::channel();
    let focusing = registry.clone();
    let focus_tab = tab.clone();
    let remote_id = remote.id.clone();
    let focus = thread::spawn(move || {
        focus_done_tx
            .send(focusing.take_focus(&focus_tab, &remote_id, size(80, 24)))
            .unwrap();
    });
    assert!(
        focus_done_rx
            .recv_timeout(Duration::from_millis(100))
            .is_err(),
        "focus interleaved into an active raw/parse/reply transaction"
    );

    assert_eq!(
        desktop.events.recv_timeout(Duration::from_secs(1)).unwrap(),
        TabEvent::Raw(b"first".to_vec())
    );
    output_done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    focus_done_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap();
    assert!(
        pty.writes().is_empty(),
        "focus transfer changed reply ownership midway through output"
    );
    writer.join().unwrap();
    focus.join().unwrap();
}

#[test]
fn preparing_exit_persists_and_rejects_a_late_desktop_attachment() {
    let (registry, pty) = registry();
    let tab = registry.open(shell_launch("slot:preparing-exit")).unwrap();
    let pty_id = pty.last_id();

    pty.prepare_exit(pty_id);
    assert_eq!(registry.get(&tab).unwrap().state(), &TabState::Running);
    assert_eq!(
        registry
            .attach(&tab, AttachmentKind::Desktop)
            .unwrap_err()
            .code(),
        "tab.closed"
    );

    pty.exit(pty_id, Some(0), None);
}

#[test]
fn rejected_attach_during_prepare_still_publishes_one_final_tab_exit() {
    let (registry, pty) = registry();
    let exits = registry.subscribe_exits();
    let tab = registry
        .open(shell_launch("slot:prepare-exit-observed"))
        .unwrap();
    let remote = registry.attach(&tab, AttachmentKind::Remote).unwrap();
    let _snapshot = remote.events.recv_timeout(Duration::from_secs(1)).unwrap();
    let pty_id = pty.last_id();

    pty.prepare_exit(pty_id);
    assert_eq!(
        registry
            .attach(&tab, AttachmentKind::Desktop)
            .unwrap_err()
            .code(),
        "tab.closed"
    );
    pty.exit(pty_id, Some(17), Some("Terminated"));

    let (observed_tab, observed_exit) = exits.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(observed_tab, tab);
    assert_eq!(observed_exit.code(), Some(17));
    assert_eq!(observed_exit.signal(), Some("Terminated"));
    assert!(!observed_exit.requested());
    assert!(
        exits.try_recv().is_err(),
        "the final exit was published twice"
    );
    assert!(matches!(
        recv_matching(&remote.events, |event| matches!(event, TabEvent::Exited(_))),
        TabEvent::Exited(exit) if exit.code() == Some(17)
    ));
}

#[test]
fn explicit_close_publishes_one_final_tab_exit() {
    let (registry, _) = registry();
    let exits = registry.subscribe_exits();
    let tab = registry.open(shell_launch("slot:close-observed")).unwrap();
    let remote = registry.attach(&tab, AttachmentKind::Remote).unwrap();
    let _snapshot = remote.events.recv_timeout(Duration::from_secs(1)).unwrap();

    registry.close(&tab).unwrap();

    let (observed_tab, observed_exit) = exits.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(observed_tab, tab);
    assert!(observed_exit.requested());
    assert!(
        exits.try_recv().is_err(),
        "explicit close was published twice"
    );
    assert!(matches!(
        recv_matching(&remote.events, |event| matches!(event, TabEvent::Exited(_))),
        TabEvent::Exited(exit) if exit.requested()
    ));
}

#[test]
fn desktop_opening_backlog_backpressures_then_replays_ordered_bounded_chunks() {
    let pty = Arc::new(FakePty::default());
    let registry = TabRegistry::with_backend_and_queue_capacity(pty.clone(), 1);
    let tab = registry
        .open_desktop(shell_launch("slot:bounded-opening"))
        .unwrap();
    let pty_id = pty.last_id();
    let first = vec![b'a'; 800 * 1024];
    let second = vec![b'b'; 800 * 1024];
    let third = vec![b'c'; 800 * 1024];
    let (first_done_tx, first_done_rx) = mpsc::channel();
    let (writer_done_tx, writer_done_rx) = mpsc::channel();
    let output = pty.clone();
    let first_for_writer = first.clone();
    let second_for_writer = second.clone();
    let writer = thread::spawn(move || {
        output.emit_output(pty_id, &first_for_writer);
        first_done_tx.send(()).unwrap();
        output.emit_output(pty_id, &second_for_writer);
        writer_done_tx.send(()).unwrap();
    });

    first_done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(
        writer_done_rx
            .recv_timeout(Duration::from_millis(100))
            .is_err(),
        "a full pre-attach backlog did not apply bounded backpressure"
    );

    let (attach_tx, attach_rx) = mpsc::channel();
    let attaching = registry.clone();
    let attaching_tab = tab.clone();
    let attach = thread::spawn(move || {
        attach_tx
            .send(attaching.attach(&attaching_tab, AttachmentKind::Desktop))
            .unwrap();
    });
    let desktop = attach_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("first attach deadlocked behind a full backlog")
        .unwrap();

    let (third_done_tx, third_done_rx) = mpsc::channel();
    let output = pty.clone();
    let third_for_writer = third.clone();
    let live_writer = thread::spawn(move || {
        output.emit_output(pty_id, &third_for_writer);
        third_done_tx.send(()).unwrap();
    });

    let mut replayed = Vec::new();
    while replayed.len() < first.len() + second.len() + third.len() {
        match desktop.events.recv_timeout(Duration::from_secs(1)).unwrap() {
            TabEvent::Raw(bytes) => {
                assert!(
                    bytes.len() < 1024 * 1024,
                    "desktop raw chunk was {} bytes",
                    bytes.len()
                );
                replayed.extend(bytes);
            }
            _ => {}
        }
    }
    let expected = [first, second, third].concat();
    assert_eq!(
        replayed, expected,
        "opening and live output reordered or dropped bytes"
    );
    writer_done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    third_done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    writer.join().unwrap();
    live_writer.join().unwrap();
    attach.join().unwrap();
}

#[test]
fn close_cancels_a_producer_blocked_on_the_desktop_opening_backlog() {
    let pty = Arc::new(FakePty::default());
    let registry = TabRegistry::with_backend_and_queue_capacity(pty.clone(), 1);
    let tab = registry
        .open_desktop(shell_launch("slot:close-opening"))
        .unwrap();
    let pty_id = pty.last_id();
    let chunk = vec![b'x'; 800 * 1024];
    let (first_done_tx, first_done_rx) = mpsc::channel();
    let (writer_done_tx, writer_done_rx) = mpsc::channel();
    let output = pty.clone();
    let writer = thread::spawn(move || {
        output.emit_output(pty_id, &chunk);
        first_done_tx.send(()).unwrap();
        output.emit_output(pty_id, &chunk);
        writer_done_tx.send(()).unwrap();
    });

    first_done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(writer_done_rx
        .recv_timeout(Duration::from_millis(100))
        .is_err());
    registry.close(&tab).unwrap();
    writer_done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("close did not wake the blocked pre-attach producer");
    writer.join().unwrap();
}

#[test]
fn prepare_exit_cancels_a_producer_blocked_on_the_desktop_opening_backlog() {
    let pty = Arc::new(FakePty::default());
    let registry = TabRegistry::with_backend_and_queue_capacity(pty.clone(), 1);
    let tab = registry
        .open_desktop(shell_launch("slot:prepare-opening"))
        .unwrap();
    let pty_id = pty.last_id();
    let chunk = vec![b'x'; 800 * 1024];
    let (first_done_tx, first_done_rx) = mpsc::channel();
    let (writer_done_tx, writer_done_rx) = mpsc::channel();
    let output = pty.clone();
    let writer = thread::spawn(move || {
        output.emit_output(pty_id, &chunk);
        first_done_tx.send(()).unwrap();
        output.emit_output(pty_id, &chunk);
        writer_done_tx.send(()).unwrap();
    });

    first_done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(writer_done_rx
        .recv_timeout(Duration::from_millis(100))
        .is_err());
    pty.prepare_exit(pty_id);
    writer_done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("prepare-exit did not wake the blocked pre-attach producer");
    pty.exit(pty_id, Some(0), None);
    assert_eq!(registry.get(&tab).unwrap().state(), &TabState::Exited);
    writer.join().unwrap();
}

#[test]
fn natural_exit_cancels_blocked_raw_output_after_queued_output() {
    let pty = Arc::new(FakePty::default());
    let registry = TabRegistry::with_backend_and_queue_capacity(pty.clone(), 1);
    let tab = registry
        .open(shell_launch("slot:natural-exit-raw-block"))
        .unwrap();
    let desktop = registry.attach(&tab, AttachmentKind::Desktop).unwrap();
    let _focus = desktop.events.recv_timeout(Duration::from_secs(1)).unwrap();
    let pty_id = pty.last_id();
    pty.emit_output(pty_id, b"before-exit");

    let (writer_done_tx, writer_done_rx) = mpsc::channel();
    let output = pty.clone();
    let writer = thread::spawn(move || {
        output.emit_output(pty_id, b"blocked-at-exit");
        writer_done_tx.send(()).unwrap();
    });
    assert!(writer_done_rx
        .recv_timeout(Duration::from_millis(100))
        .is_err());

    let (exit_done_tx, exit_done_rx) = mpsc::channel();
    let exiting = pty.clone();
    let exit = thread::spawn(move || {
        exiting.exit(pty_id, Some(0), None);
        exit_done_tx.send(()).unwrap();
    });
    exit_done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("natural exit stayed blocked behind raw output");
    writer_done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("natural exit did not wake the raw producer");

    assert_eq!(
        desktop.events.recv_timeout(Duration::from_secs(1)).unwrap(),
        TabEvent::Raw(b"before-exit".to_vec())
    );
    assert!(matches!(
        desktop.events.recv_timeout(Duration::from_secs(1)).unwrap(),
        TabEvent::Exited(_)
    ));
    assert_eq!(
        desktop.events.recv_timeout(Duration::from_secs(1)),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
    );
    writer.join().unwrap();
    exit.join().unwrap();
}

#[derive(Default)]
struct OrderedReplyPty {
    sink: Mutex<Option<Arc<dyn PtySink>>>,
    calls: AtomicUsize,
    writes: Mutex<Vec<Vec<u8>>>,
    first_state: Mutex<(bool, bool)>,
    first_changed: Condvar,
}

impl OrderedReplyPty {
    fn wait_for_first_write(&self) {
        let state = self.first_state.lock().unwrap();
        let (state, timeout) = self
            .first_changed
            .wait_timeout_while(state, Duration::from_secs(5), |state| !state.0)
            .unwrap();
        assert!(!timeout.timed_out(), "pending reply A was never written");
        drop(state);
    }

    fn emit_second_query(&self) {
        self.sink
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .output(1, b"\x1b[6n");
    }

    fn release_first(&self) {
        let mut state = self.first_state.lock().unwrap();
        state.1 = true;
        self.first_changed.notify_all();
    }

    fn writes(&self) -> Vec<Vec<u8>> {
        self.writes.lock().unwrap().clone()
    }
}

impl PtyBackend for OrderedReplyPty {
    fn spawn(&self, _spec: PtySpawnSpec, sink: Arc<dyn PtySink>) -> Result<u32, String> {
        *self.sink.lock().unwrap() = Some(sink.clone());
        sink.output(1, b"\x1b[5n");
        Ok(1)
    }

    fn write(&self, _id: u32, bytes: &[u8]) -> Result<(), String> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            let mut state = self.first_state.lock().unwrap();
            state.0 = true;
            self.first_changed.notify_all();
            while !state.1 {
                state = self.first_changed.wait(state).unwrap();
            }
        }
        self.writes.lock().unwrap().push(bytes.to_vec());
        Ok(())
    }

    fn resize(&self, _id: u32, _cols: u16, _rows: u16) -> Result<(), String> {
        Ok(())
    }

    fn kill(&self, _id: u32) {}

    fn pty_for_descendant(&self, _pid: u32) -> Option<u32> {
        None
    }
}

#[test]
fn a_new_reply_cannot_overtake_the_pending_reply_being_flushed_during_bind() {
    let pty = Arc::new(OrderedReplyPty::default());
    let registry = TabRegistry::with_backend(pty.clone());
    let opener = registry.clone();
    let open = thread::spawn(move || opener.open(shell_launch("slot:reply-order")));
    pty.wait_for_first_write();

    pty.emit_second_query();
    assert!(pty.writes().is_empty(), "reply B overtook blocked reply A");
    assert!(
        registry.list().is_empty(),
        "tab published before bind flush completed"
    );

    pty.release_first();
    open.join().unwrap().unwrap();
    assert_eq!(
        pty.writes(),
        vec![b"\x1b[0n".to_vec(), b"\x1b[1;1R".to_vec()]
    );
}

#[test]
fn a_waiting_remote_receiver_has_no_event_notification_lost_wakeup() {
    let (registry, pty) = registry();
    let tab = registry.open(shell_launch("slot:mailbox-wakeup")).unwrap();
    let remote = registry.attach(&tab, AttachmentKind::Remote).unwrap();
    let _initial = remote.events.recv_timeout(Duration::from_secs(1)).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let waiting = barrier.clone();
    let receiver = thread::spawn(move || {
        waiting.wait();
        remote.events.recv_timeout(Duration::from_secs(1))
    });

    barrier.wait();
    pty.emit_output(pty.last_id(), b"wake");

    assert!(matches!(
        receiver.join().unwrap().unwrap(),
        TabEvent::Diff(_)
    ));
}

#[tokio::test]
async fn async_remote_receiver_wakes_without_using_the_blocking_pool() {
    let (registry, pty) = registry();
    let tab = registry.open(shell_launch("slot:async-mailbox")).unwrap();
    let remote = registry.attach(&tab, AttachmentKind::Remote).unwrap();
    let _initial = remote.events.recv_async().await.unwrap();

    pty.emit_output(pty.last_id(), b"wake");
    let event = tokio::time::timeout(Duration::from_secs(1), remote.events.recv_async())
        .await
        .expect("async receive must wake")
        .unwrap();
    assert!(matches!(event, TabEvent::Diff(_)));
}

#[test]
fn concurrent_remote_attach_and_output_always_expose_snapshot_before_diff() {
    let (registry, pty) = registry();
    let tab = registry.open(shell_launch("slot:attach-order")).unwrap();

    for _ in 0..32 {
        let barrier = Arc::new(Barrier::new(2));
        let attaching = barrier.clone();
        let attaching_registry = registry.clone();
        let attaching_tab = tab.clone();
        let attach = thread::spawn(move || {
            attaching.wait();
            attaching_registry
                .attach(&attaching_tab, AttachmentKind::Remote)
                .unwrap()
        });
        barrier.wait();
        pty.emit_output(pty.last_id(), b"x\r");
        let remote = attach.join().unwrap();

        assert!(matches!(
            remote.events.recv_timeout(Duration::from_secs(1)).unwrap(),
            TabEvent::Snapshot(_)
        ));
    }
}
