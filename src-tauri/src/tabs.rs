use crate::pty::{PtyManager, PtySink, PtySpawnSpec};
use crate::remote::model::TerminalSize;
use crate::terminal::model::{Revision, ScreenDiff, ScreenRow, ScreenSnapshot};
use crate::terminal::screen::ScreenModel;
use portable_pty::PtySize;
use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{
    self, Receiver, RecvError, RecvTimeoutError, SyncSender, TryRecvError,
};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Notify;
use uuid::Uuid;

const DEFAULT_QUEUE_CAPACITY: usize = 64;
pub const MAX_TAB_ROSTER_ENTRIES: usize = 128;
pub const MAX_TAB_DESCRIPTOR_TEXT_BYTES: usize = 32 * 1024;
/// Tauri raw frames must stay comfortably below the remote protocol's 1 MiB
/// ceiling too. PTY reads are normally 8 KiB; this also bounds adversarial or
/// test sinks that deliver a much larger slice in one callback.
const MAX_DESKTOP_RAW_CHUNK: usize = 1024 * 1024 - 1;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct TabId(String);

impl TabId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A TabId from a caller-held string — the phone quotes ids back from
    /// answers it was given, so this mints no identity, only addresses one.
    pub fn from_raw(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }
}

impl Default for TabId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for TabId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for TabId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize_wire_uuid(deserializer).map(Self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct AttachmentId(String);

impl AttachmentId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for AttachmentId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for AttachmentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for AttachmentId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize_wire_uuid(deserializer).map(Self)
    }
}

fn deserialize_wire_uuid<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if value.len() != 36 {
        return Err(D::Error::custom("identifier must be a canonical UUID"));
    }
    let parsed = Uuid::parse_str(&value)
        .map_err(|_| D::Error::custom("identifier must be a canonical UUID"))?;
    if parsed.hyphenated().to_string() != value {
        return Err(D::Error::custom("identifier must be a canonical UUID"));
    }
    Ok(value)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AttachmentKind {
    Desktop,
    Remote,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TabLaunch {
    title: String,
    cwd: Option<String>,
    command: Option<String>,
    session_id: Option<String>,
    resumed_id: Option<String>,
    agent_id: Option<String>,
    slot_id: String,
    #[serde(default)]
    fresh: bool,
    env_provider: Option<String>,
    env_model: Option<String>,
    size: TerminalSize,
    #[serde(skip)]
    desktop_pending: bool,
}

impl TabLaunch {
    pub fn new(title: impl Into<String>, slot_id: impl Into<String>, size: TerminalSize) -> Self {
        Self {
            title: title.into(),
            cwd: None,
            command: None,
            session_id: None,
            resumed_id: None,
            agent_id: None,
            slot_id: slot_id.into(),
            fresh: false,
            env_provider: None,
            env_model: None,
            size,
            desktop_pending: false,
        }
    }

    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn with_command(mut self, command: impl Into<String>) -> Self {
        self.command = Some(command.into());
        self
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn with_resumed_id(mut self, resumed_id: impl Into<String>) -> Self {
        self.resumed_id = Some(resumed_id.into());
        self
    }

    pub fn with_agent_id(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = Some(agent_id.into());
        self
    }

    pub fn with_fresh(mut self, fresh: bool) -> Self {
        self.fresh = fresh;
        self
    }

    pub fn with_environment(
        mut self,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        self.env_provider = Some(provider.into());
        self.env_model = Some(model.into());
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TabUpdate {
    title: Option<String>,
    session_id: Option<String>,
    resumed_id: Option<String>,
    agent_id: Option<String>,
    slot_id: Option<String>,
    fresh: Option<bool>,
}

impl TabUpdate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn resumed_id(mut self, resumed_id: impl Into<String>) -> Self {
        self.resumed_id = Some(resumed_id.into());
        self
    }

    pub fn agent_id(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = Some(agent_id.into());
        self
    }

    pub fn slot_id(mut self, slot_id: impl Into<String>) -> Self {
        self.slot_id = Some(slot_id.into());
        self
    }

    pub fn fresh(mut self, fresh: bool) -> Self {
        self.fresh = Some(fresh);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TabState {
    Running,
    Exited,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TabFocus {
    Desktop,
    Remote,
    Unowned,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TabExit {
    code: Option<u32>,
    signal: Option<String>,
    requested: bool,
}

impl TabExit {
    pub fn code(&self) -> Option<u32> {
        self.code
    }

    pub fn signal(&self) -> Option<&str> {
        self.signal.as_deref()
    }

    pub fn requested(&self) -> bool {
        self.requested
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TabDescriptor {
    id: TabId,
    title: String,
    cwd: Option<String>,
    command: Option<String>,
    session_id: Option<String>,
    resumed_id: Option<String>,
    agent_id: Option<String>,
    slot_id: String,
    fresh: bool,
    env_provider: Option<String>,
    env_model: Option<String>,
    size: TerminalSize,
    #[serde(skip)]
    input_owner: Option<AttachmentId>,
    focus: TabFocus,
    state: TabState,
    exit: Option<TabExit>,
}

impl TabDescriptor {
    pub fn id(&self) -> &TabId {
        &self.id
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    pub fn command(&self) -> Option<&str> {
        self.command.as_deref()
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn resumed_id(&self) -> Option<&str> {
        self.resumed_id.as_deref()
    }

    pub fn agent_id(&self) -> Option<&str> {
        self.agent_id.as_deref()
    }

    pub fn slot_id(&self) -> &str {
        &self.slot_id
    }

    pub fn fresh(&self) -> bool {
        self.fresh
    }

    pub fn env_provider(&self) -> Option<&str> {
        self.env_provider.as_deref()
    }

    pub fn env_model(&self) -> Option<&str> {
        self.env_model.as_deref()
    }

    pub fn size(&self) -> TerminalSize {
        self.size
    }

    pub fn input_owner(&self) -> Option<&AttachmentId> {
        self.input_owner.as_ref()
    }

    pub fn focus(&self) -> TabFocus {
        self.focus
    }

    pub fn state(&self) -> &TabState {
        &self.state
    }

    pub fn exit(&self) -> Option<&TabExit> {
        self.exit.as_ref()
    }

    fn text_bytes(&self) -> usize {
        descriptor_text_bytes([
            Some(self.title.as_str()),
            self.cwd.as_deref(),
            self.command.as_deref(),
            self.session_id.as_deref(),
            self.resumed_id.as_deref(),
            self.agent_id.as_deref(),
            Some(self.slot_id.as_str()),
            self.env_provider.as_deref(),
            self.env_model.as_deref(),
            self.exit.as_ref().and_then(|exit| exit.signal.as_deref()),
        ])
    }
}

fn descriptor_text_bytes<const N: usize>(values: [Option<&str>; N]) -> usize {
    values
        .into_iter()
        .flatten()
        .try_fold(0usize, |total, value| total.checked_add(value.len()))
        .unwrap_or(usize::MAX)
}

fn launch_text_bytes(launch: &TabLaunch) -> usize {
    descriptor_text_bytes([
        Some(launch.title.as_str()),
        launch.cwd.as_deref(),
        launch.command.as_deref(),
        launch.session_id.as_deref(),
        launch.resumed_id.as_deref(),
        launch.agent_id.as_deref(),
        Some(launch.slot_id.as_str()),
        launch.env_provider.as_deref(),
        launch.env_model.as_deref(),
    ])
}

fn truncate_utf8(value: &str, bytes: usize) -> String {
    if value.len() <= bytes {
        return value.to_owned();
    }
    let mut end = bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabEvent {
    Raw(Vec<u8>),
    Snapshot(ScreenSnapshot),
    SharedSnapshot(Arc<ScreenSnapshot>),
    Diff(ScreenDiff),
    FocusChanged {
        owner: Option<AttachmentId>,
        size: TerminalSize,
    },
    Metadata(TabDescriptor),
    Title(String),
    Bell,
    Exited(TabExit),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabRegistryEvent {
    Snapshot {
        revision: u64,
        tabs: Vec<TabDescriptor>,
    },
    Opened {
        revision: u64,
        tab: TabDescriptor,
    },
    Changed {
        revision: u64,
        tab: TabDescriptor,
    },
    Removed {
        revision: u64,
        tab_id: TabId,
        requested: bool,
    },
}

impl TabRegistryEvent {
    pub fn revision(&self) -> u64 {
        match self {
            Self::Snapshot { revision, .. }
            | Self::Opened { revision, .. }
            | Self::Changed { revision, .. }
            | Self::Removed { revision, .. } => *revision,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TabRegistrySnapshot {
    revision: u64,
    tabs: Vec<TabDescriptor>,
}

impl TabRegistrySnapshot {
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn tabs(&self) -> &[TabDescriptor] {
        &self.tabs
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabError {
    code: &'static str,
    message: String,
}

impl TabError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for TabError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for TabError {}

pub trait PtyBackend: Send + Sync + 'static {
    fn spawn(&self, spec: PtySpawnSpec, sink: Arc<dyn PtySink>) -> Result<u32, String>;
    fn write(&self, id: u32, bytes: &[u8]) -> Result<(), String>;
    fn resize(&self, id: u32, cols: u16, rows: u16) -> Result<(), String>;
    fn kill(&self, id: u32);
    fn pty_for_descendant(&self, pid: u32) -> Option<u32>;
    /// Root pid of the PTY's child, when the backend knows it.
    fn child_pid(&self, _id: u32) -> Option<u32> {
        None
    }
}

impl PtyBackend for PtyManager {
    fn spawn(&self, spec: PtySpawnSpec, sink: Arc<dyn PtySink>) -> Result<u32, String> {
        PtyManager::spawn(self, spec, sink)
    }

    fn write(&self, id: u32, bytes: &[u8]) -> Result<(), String> {
        PtyManager::write(self, id, bytes)
    }

    fn resize(&self, id: u32, cols: u16, rows: u16) -> Result<(), String> {
        PtyManager::resize(self, id, cols, rows)
    }

    fn kill(&self, id: u32) {
        PtyManager::kill(self, id)
    }

    fn pty_for_descendant(&self, pid: u32) -> Option<u32> {
        PtyManager::pty_for_descendant(self, pid)
    }

    fn child_pid(&self, id: u32) -> Option<u32> {
        PtyManager::child_pid(self, id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ControlKind {
    Focus,
    Metadata,
    Title,
    Bell,
    Exited,
}

struct QueuedEvent {
    sequence: u64,
    event: TabEvent,
}

struct MailboxState {
    next_sequence: u64,
    screen: VecDeque<QueuedEvent>,
    raw: VecDeque<QueuedEvent>,
    controls: HashMap<ControlKind, QueuedEvent>,
    raw_cancelled: bool,
    receiver_closed: bool,
    producer_closed: bool,
}

impl Default for MailboxState {
    fn default() -> Self {
        Self {
            next_sequence: 0,
            screen: VecDeque::new(),
            raw: VecDeque::new(),
            controls: HashMap::new(),
            raw_cancelled: false,
            receiver_closed: false,
            producer_closed: false,
        }
    }
}

struct EventMailbox {
    kind: AttachmentKind,
    capacity: usize,
    state: Mutex<MailboxState>,
    changed: Condvar,
    async_changed: Notify,
    finalized_changed: Notify,
}

impl EventMailbox {
    fn new(kind: AttachmentKind, capacity: usize) -> Self {
        Self {
            kind,
            capacity: capacity.max(1),
            state: Mutex::new(MailboxState::default()),
            changed: Condvar::new(),
            async_changed: Notify::new(),
            finalized_changed: Notify::new(),
        }
    }

    fn push_initial_snapshot(&self, snapshot: ScreenSnapshot) {
        let mut state = self.state.lock().unwrap();
        if state.receiver_closed || state.producer_closed {
            return;
        }
        let sequence = take_sequence(&mut state);
        state.screen.push_back(QueuedEvent {
            sequence,
            event: TabEvent::Snapshot(snapshot),
        });
        self.changed.notify_one();
        self.async_changed.notify_one();
    }

    fn push_snapshot(&self, snapshot: ScreenSnapshot) {
        self.push_screen(TabEvent::Snapshot(snapshot.clone()), || snapshot);
    }

    fn finish_with_shared_snapshot(&self, snapshot: Arc<ScreenSnapshot>, exit: TabExit) {
        debug_assert_eq!(self.kind, AttachmentKind::Remote);
        let mut state = self.state.lock().unwrap();
        if state.receiver_closed || state.producer_closed {
            return;
        }
        state.screen.clear();
        let snapshot_sequence = take_sequence(&mut state);
        state.screen.push_back(QueuedEvent {
            sequence: snapshot_sequence,
            event: TabEvent::SharedSnapshot(snapshot),
        });
        let exit_sequence = take_sequence(&mut state);
        state.controls.insert(
            ControlKind::Exited,
            QueuedEvent {
                sequence: exit_sequence,
                event: TabEvent::Exited(exit),
            },
        );
        state.producer_closed = true;
        self.changed.notify_all();
        self.async_changed.notify_one();
        self.finalized_changed.notify_waiters();
    }

    fn push_diff(&self, diff: ScreenDiff, recovery: impl FnOnce() -> ScreenSnapshot) {
        self.push_screen(TabEvent::Diff(diff), recovery);
    }

    fn push_screen(&self, event: TabEvent, recovery: impl FnOnce() -> ScreenSnapshot) {
        debug_assert_eq!(self.kind, AttachmentKind::Remote);
        let mut state = self.state.lock().unwrap();
        if state.receiver_closed || state.producer_closed {
            return;
        }
        let event_sequence = take_sequence(&mut state);
        if state.screen.len() >= self.capacity {
            // Keep the earliest replaced event's position relative to control
            // events. The snapshot semantically supersedes every removed
            // screen event, while later diffs receive later sequence numbers.
            let sequence = state
                .screen
                .front()
                .map(|queued| queued.sequence)
                .unwrap_or(event_sequence);
            state.screen.clear();
            state.screen.push_back(QueuedEvent {
                sequence,
                event: TabEvent::Snapshot(recovery()),
            });
        } else {
            state.screen.push_back(QueuedEvent {
                sequence: event_sequence,
                event,
            });
        }
        self.changed.notify_one();
        self.async_changed.notify_one();
    }

    fn push_raw(&self, bytes: Vec<u8>) -> bool {
        debug_assert_eq!(self.kind, AttachmentKind::Desktop);
        let mut state = self.state.lock().unwrap();
        while state.raw.len() >= self.capacity
            && !state.raw_cancelled
            && !state.receiver_closed
            && !state.producer_closed
        {
            state = self.changed.wait(state).unwrap();
        }
        if state.raw_cancelled || state.receiver_closed || state.producer_closed {
            return false;
        }
        let sequence = take_sequence(&mut state);
        state.raw.push_back(QueuedEvent {
            sequence,
            event: TabEvent::Raw(bytes),
        });
        self.changed.notify_one();
        self.async_changed.notify_one();
        true
    }

    fn cancel_raw(&self) {
        let mut state = self.state.lock().unwrap();
        state.raw_cancelled = true;
        self.changed.notify_all();
        self.async_changed.notify_one();
    }

    fn push_control(&self, event: TabEvent) {
        let Some(kind) = control_kind(&event) else {
            return;
        };
        let mut state = self.state.lock().unwrap();
        if state.receiver_closed || state.producer_closed {
            return;
        }
        if kind == ControlKind::Exited && state.controls.contains_key(&kind) {
            return;
        }
        let sequence = take_sequence(&mut state);
        state.controls.insert(kind, QueuedEvent { sequence, event });
        self.changed.notify_one();
        self.async_changed.notify_one();
    }

    fn finish(&self, exit: TabExit) {
        let mut state = self.state.lock().unwrap();
        if state.receiver_closed || state.producer_closed {
            return;
        }
        if !state.controls.contains_key(&ControlKind::Exited) {
            let sequence = take_sequence(&mut state);
            state.controls.insert(
                ControlKind::Exited,
                QueuedEvent {
                    sequence,
                    event: TabEvent::Exited(exit),
                },
            );
        }
        state.producer_closed = true;
        self.changed.notify_all();
        self.async_changed.notify_one();
    }

    fn close_receiver(&self) {
        let mut state = self.state.lock().unwrap();
        state.receiver_closed = true;
        state.screen.clear();
        state.raw.clear();
        state.controls.clear();
        self.changed.notify_all();
        self.async_changed.notify_one();
        self.finalized_changed.notify_waiters();
    }

    async fn wait_finalized(&self) -> bool {
        loop {
            let notified = self.finalized_changed.notified();
            {
                let state = self.state.lock().unwrap();
                if state.producer_closed {
                    return true;
                }
                if state.receiver_closed {
                    return false;
                }
            }
            notified.await;
        }
    }

    fn recv(&self) -> Result<TabEvent, RecvError> {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(event) = pop_next(&mut state) {
                self.changed.notify_all();
                return Ok(event);
            }
            if state.receiver_closed || state.producer_closed {
                return Err(RecvError);
            }
            state = self.changed.wait(state).unwrap();
        }
    }

    fn recv_timeout(&self, timeout: Duration) -> Result<TabEvent, RecvTimeoutError> {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(event) = pop_next(&mut state) {
                self.changed.notify_all();
                return Ok(event);
            }
            if state.receiver_closed || state.producer_closed {
                return Err(RecvTimeoutError::Disconnected);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(RecvTimeoutError::Timeout);
            }
            let (next, timeout_result) = self.changed.wait_timeout(state, deadline - now).unwrap();
            state = next;
            if timeout_result.timed_out()
                && state.screen.is_empty()
                && state.raw.is_empty()
                && state.controls.is_empty()
            {
                return Err(RecvTimeoutError::Timeout);
            }
        }
    }

    fn try_recv(&self) -> Result<TabEvent, TryRecvError> {
        let mut state = self.state.lock().unwrap();
        if let Some(event) = pop_next(&mut state) {
            self.changed.notify_all();
            return Ok(event);
        }
        if state.receiver_closed || state.producer_closed {
            Err(TryRecvError::Disconnected)
        } else {
            Err(TryRecvError::Empty)
        }
    }

    async fn recv_async(&self) -> Result<TabEvent, TabReceiveError> {
        loop {
            // `notify_one` stores a permit when the future is not yet polled;
            // creating it before inspecting state closes the check/await gap.
            let notified = self.async_changed.notified();
            {
                let mut state = self.state.lock().unwrap();
                if let Some(event) = pop_next(&mut state) {
                    self.changed.notify_all();
                    return Ok(event);
                }
                if state.receiver_closed {
                    return Err(TabReceiveError::Cancelled);
                }
                if state.producer_closed {
                    return Err(TabReceiveError::Disconnected);
                }
            }
            notified.await;
        }
    }

    fn recovery_boundary(&self) -> u64 {
        self.state.lock().unwrap().next_sequence
    }

    fn discard_before(&self, boundary: u64) {
        let mut state = self.state.lock().unwrap();
        state.screen.retain(|queued| queued.sequence >= boundary);
        state.raw.retain(|queued| queued.sequence >= boundary);
        state
            .controls
            .retain(|_, queued| queued.sequence >= boundary);
        self.changed.notify_all();
        self.async_changed.notify_one();
    }
}

fn take_sequence(state: &mut MailboxState) -> u64 {
    let sequence = state.next_sequence;
    state.next_sequence = state.next_sequence.saturating_add(1);
    sequence
}

fn control_kind(event: &TabEvent) -> Option<ControlKind> {
    match event {
        TabEvent::FocusChanged { .. } => Some(ControlKind::Focus),
        TabEvent::Metadata(_) => Some(ControlKind::Metadata),
        TabEvent::Title(_) => Some(ControlKind::Title),
        TabEvent::Bell => Some(ControlKind::Bell),
        TabEvent::Exited(_) => Some(ControlKind::Exited),
        TabEvent::Raw(_)
        | TabEvent::Snapshot(_)
        | TabEvent::SharedSnapshot(_)
        | TabEvent::Diff(_) => None,
    }
}

fn pop_next(state: &mut MailboxState) -> Option<TabEvent> {
    enum Lane {
        Screen,
        Raw,
        Control(ControlKind),
    }

    let mut next: Option<(u64, Lane)> = state
        .screen
        .front()
        .map(|queued| (queued.sequence, Lane::Screen));
    if let Some(raw) = state.raw.front() {
        if next
            .as_ref()
            .is_none_or(|(sequence, _)| raw.sequence < *sequence)
        {
            next = Some((raw.sequence, Lane::Raw));
        }
    }
    for (kind, queued) in &state.controls {
        if next
            .as_ref()
            .is_none_or(|(sequence, _)| queued.sequence < *sequence)
        {
            next = Some((queued.sequence, Lane::Control(*kind)));
        }
    }

    match next?.1 {
        Lane::Screen => state.screen.pop_front().map(|queued| queued.event),
        Lane::Raw => state.raw.pop_front().map(|queued| queued.event),
        Lane::Control(kind) => state.controls.remove(&kind).map(|queued| queued.event),
    }
}

#[derive(Default)]
struct RegistryMailboxState {
    queue: VecDeque<TabRegistryEvent>,
    receiver_closed: bool,
    producer_closed: bool,
}

struct RegistryMailbox {
    capacity: usize,
    state: Mutex<RegistryMailboxState>,
    changed: Condvar,
    async_changed: Notify,
}

impl RegistryMailbox {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            state: Mutex::new(RegistryMailboxState::default()),
            changed: Condvar::new(),
            async_changed: Notify::new(),
        }
    }

    fn push_initial(&self, snapshot: TabRegistryEvent) {
        let mut state = self.state.lock().unwrap();
        if state.receiver_closed || state.producer_closed {
            return;
        }
        state.queue.push_back(snapshot);
        self.changed.notify_one();
        self.async_changed.notify_one();
    }

    fn push(&self, event: TabRegistryEvent, recovery: &TabRegistryEvent) {
        let mut state = self.state.lock().unwrap();
        if state.receiver_closed || state.producer_closed {
            return;
        }
        if state.queue.len() >= self.capacity {
            state.queue.clear();
            state.queue.push_back(recovery.clone());
        } else {
            state.queue.push_back(event);
        }
        self.changed.notify_one();
        self.async_changed.notify_one();
    }

    fn recv(&self) -> Result<TabRegistryEvent, RecvError> {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(event) = state.queue.pop_front() {
                return Ok(event);
            }
            if state.receiver_closed || state.producer_closed {
                return Err(RecvError);
            }
            state = self.changed.wait(state).unwrap();
        }
    }

    fn recv_timeout(&self, timeout: Duration) -> Result<TabRegistryEvent, RecvTimeoutError> {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(event) = state.queue.pop_front() {
                return Ok(event);
            }
            if state.receiver_closed || state.producer_closed {
                return Err(RecvTimeoutError::Disconnected);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(RecvTimeoutError::Timeout);
            }
            let (next, timeout_result) = self.changed.wait_timeout(state, deadline - now).unwrap();
            state = next;
            if timeout_result.timed_out() && state.queue.is_empty() {
                return Err(RecvTimeoutError::Timeout);
            }
        }
    }

    async fn recv_async(&self) -> Option<TabRegistryEvent> {
        loop {
            let notified = self.async_changed.notified();
            {
                let mut state = self.state.lock().unwrap();
                if let Some(event) = state.queue.pop_front() {
                    return Some(event);
                }
                if state.receiver_closed || state.producer_closed {
                    return None;
                }
            }
            notified.await;
        }
    }

    fn close_receiver(&self) {
        let mut state = self.state.lock().unwrap();
        state.receiver_closed = true;
        state.queue.clear();
        self.changed.notify_all();
        self.async_changed.notify_one();
    }

    fn close_producer(&self) {
        let mut state = self.state.lock().unwrap();
        state.producer_closed = true;
        self.changed.notify_all();
        self.async_changed.notify_one();
    }
}

pub struct TabRegistryEventReceiver {
    mailbox: Arc<RegistryMailbox>,
    registry: Weak<RegistryInner>,
}

impl TabRegistryEventReceiver {
    pub fn recv(&self) -> Result<TabRegistryEvent, RecvError> {
        self.mailbox.recv()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<TabRegistryEvent, RecvTimeoutError> {
        self.mailbox.recv_timeout(timeout)
    }

    pub async fn recv_async(&self) -> Option<TabRegistryEvent> {
        self.mailbox.recv_async().await
    }
}

impl Drop for TabRegistryEventReceiver {
    fn drop(&mut self) {
        self.mailbox.close_receiver();
        let target = Arc::as_ptr(&self.mailbox);
        if let Some(registry) = self.registry.upgrade() {
            registry
                .maps
                .lock()
                .unwrap()
                .subscribers
                .retain(|subscriber| {
                    subscriber.strong_count() > 0 && subscriber.as_ptr() != target
                });
        }
    }
}

pub struct TabAttachment {
    pub id: AttachmentId,
    pub events: TabEventReceiver,
    pub cancellation: TabAttachmentCancellation,
    descriptor: TabDescriptor,
}

impl fmt::Debug for TabAttachment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TabAttachment")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl TabAttachment {
    /// Metadata captured under the same output/live ordering boundary as the
    /// initial remote snapshot and subscriber insertion.
    pub fn descriptor(&self) -> &TabDescriptor {
        &self.descriptor
    }
}

#[derive(Clone)]
pub struct TabAttachmentCancellation {
    mailbox: Arc<EventMailbox>,
    registry: Weak<RegistryInner>,
    tab_id: TabId,
    attachment_id: AttachmentId,
    cancelled: Arc<AtomicBool>,
}

impl fmt::Debug for TabAttachmentCancellation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TabAttachmentCancellation")
            .field("tab_id", &self.tab_id)
            .field("attachment_id", &self.attachment_id)
            .finish_non_exhaustive()
    }
}

impl TabAttachmentCancellation {
    /// Wake the exact receiver mailbox before removing registry ownership.
    /// The registry mutation is performed at most once across explicit
    /// detach, task completion, connection teardown, and tab-exit races.
    pub fn cancel(&self) {
        self.cancel_deferred();
    }

    /// Cancellation phase one: synchronously close and wake the exact
    /// attachment mailbox without waiting for output-order/backend work.
    pub fn close_mailbox(&self) {
        self.mailbox.close_receiver();
    }

    /// Cancellation phase two: remove registry ownership exactly once. This
    /// may wait for an in-flight ordered backend operation and therefore must
    /// be isolated by async callers.
    pub fn detach_registry(&self) {
        if self.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(registry) = self.registry.upgrade() {
            registry.detach(&self.tab_id, &self.attachment_id);
        }
    }

    pub fn cancel_deferred(&self) {
        self.close_mailbox();
        let cancellation = self.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn_blocking(move || cancellation.detach_registry());
        } else {
            cancellation.detach_registry();
        }
    }
}

pub struct TabEventReceiver {
    mailbox: Arc<EventMailbox>,
    tab_id: TabId,
    attachment_id: AttachmentId,
    cancellation: TabAttachmentCancellation,
}

#[derive(Clone)]
pub struct TabFinalizationSignal {
    mailbox: Arc<EventMailbox>,
}

impl TabFinalizationSignal {
    pub async fn wait(&self) -> bool {
        self.mailbox.wait_finalized().await
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabReceiveError {
    Cancelled,
    Disconnected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecoveryBoundary(u64);

impl fmt::Debug for TabEventReceiver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TabEventReceiver")
            .field("tab_id", &self.tab_id)
            .field("attachment_id", &self.attachment_id)
            .finish_non_exhaustive()
    }
}

impl TabEventReceiver {
    pub fn finalization_signal(&self) -> TabFinalizationSignal {
        TabFinalizationSignal {
            mailbox: self.mailbox.clone(),
        }
    }
    pub fn recv(&self) -> Result<TabEvent, RecvError> {
        self.mailbox.recv()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<TabEvent, RecvTimeoutError> {
        self.mailbox.recv_timeout(timeout)
    }

    pub fn try_recv(&self) -> Result<TabEvent, TryRecvError> {
        self.mailbox.try_recv()
    }

    pub async fn recv_async(&self) -> Result<TabEvent, TabReceiveError> {
        self.mailbox.recv_async().await
    }

    pub fn discard_before(&self, boundary: RecoveryBoundary) {
        self.mailbox.discard_before(boundary.0);
    }
}

impl Drop for TabEventReceiver {
    fn drop(&mut self) {
        self.cancellation.cancel_deferred();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScrollbackPage {
    revision: Revision,
    rows: Vec<ScreenRow>,
}

impl ScrollbackPage {
    pub fn revision(&self) -> Revision {
        self.revision
    }

    pub fn rows(&self) -> &[ScreenRow] {
        &self.rows
    }

    pub fn into_rows(self) -> Vec<ScreenRow> {
        self.rows
    }
}

#[derive(Clone)]
pub struct TabRegistry {
    inner: Arc<RegistryInner>,
}

impl Default for TabRegistry {
    fn default() -> Self {
        Self::new(PtyManager::default())
    }
}

impl TabRegistry {
    pub fn new(manager: PtyManager) -> Self {
        Self::with_backend(Arc::new(manager))
    }

    pub fn with_backend(backend: Arc<dyn PtyBackend>) -> Self {
        Self::with_backend_and_queue_capacity(backend, DEFAULT_QUEUE_CAPACITY)
    }

    pub fn with_backend_and_queue_capacity(
        backend: Arc<dyn PtyBackend>,
        queue_capacity: usize,
    ) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                backend,
                maps: Mutex::new(RegistryMaps::default()),
                queue_capacity: queue_capacity.max(1),
                exit_subscribers: Mutex::new(Vec::new()),
                activity_subscribers: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Subscribe to the one final exit each tab publishes. The desktop bridge
    /// installs this before the webview can invoke `tab_open`; remote callers
    /// continue to receive the same exit through their attachment mailbox.
    pub fn subscribe_exits(&self) -> Receiver<(TabId, TabExit)> {
        let (sender, receiver) = mpsc::channel();
        self.inner.exit_subscribers.lock().unwrap().push(sender);
        receiver
    }

    /// Subscribe to throttled session activity without turning every PTY read
    /// into a roster revision. Consumers use this for attribution and other
    /// presence signals that do not belong in serialized tab state.
    pub fn subscribe_activity(&self) -> Receiver<String> {
        let (sender, receiver) = mpsc::sync_channel(256);
        self.inner.activity_subscribers.lock().unwrap().push(sender);
        receiver
    }

    pub fn subscribe_changes(&self) -> TabRegistryEventReceiver {
        let mailbox = Arc::new(RegistryMailbox::new(self.inner.queue_capacity));
        let mut maps = self.inner.maps.lock().unwrap();
        mailbox.push_initial(maps.snapshot_event());
        maps.subscribers.push(Arc::downgrade(&mailbox));
        TabRegistryEventReceiver {
            mailbox,
            registry: Arc::downgrade(&self.inner),
        }
    }

    pub fn roster_snapshot(&self) -> TabRegistrySnapshot {
        self.inner.maps.lock().unwrap().snapshot()
    }

    pub fn open_desktop(&self, mut launch: TabLaunch) -> Result<TabId, TabError> {
        launch.desktop_pending = true;
        self.open(launch)
    }

    pub fn open(&self, launch: TabLaunch) -> Result<TabId, TabError> {
        if launch_text_bytes(&launch) > MAX_TAB_DESCRIPTOR_TEXT_BYTES {
            return Err(TabError::new(
                "tab.metadata_too_large",
                "tab metadata exceeds the recoverable roster bound",
            ));
        }
        let id = TabId::new();
        let slot_id = launch.slot_id.clone();
        let desktop_open = launch.desktop_pending;
        {
            let mut maps = self.inner.maps.lock().unwrap();
            if maps.by_id.len().saturating_add(maps.pending_slots.len())
                >= MAX_TAB_ROSTER_ENTRIES
            {
                return Err(TabError::new(
                    "tab.too_many_tabs",
                    "the recoverable tab roster limit was reached",
                ));
            }
            if maps.by_slot.contains_key(&slot_id) || maps.pending_slots.contains_key(&slot_id) {
                return Err(TabError::new(
                    "tab.slot_in_use",
                    "another tab already owns this slot",
                ));
            }
            maps.pending_slots.insert(slot_id.clone(), id.clone());
        }
        let descriptor = TabDescriptor {
            id: id.clone(),
            title: launch.title,
            cwd: launch.cwd,
            command: launch.command,
            session_id: launch.session_id,
            resumed_id: launch.resumed_id,
            agent_id: launch.agent_id,
            slot_id: launch.slot_id,
            fresh: launch.fresh,
            env_provider: launch.env_provider,
            env_model: launch.env_model,
            size: launch.size,
            input_owner: None,
            focus: TabFocus::Unowned,
            state: TabState::Running,
            exit: None,
        };
        let spec = PtySpawnSpec {
            cwd: descriptor.cwd.clone(),
            command: descriptor.command.clone(),
            size: PtySize {
                rows: descriptor.size.rows(),
                cols: descriptor.size.cols(),
                pixel_width: 0,
                pixel_height: 0,
            },
            env_provider: descriptor.env_provider.clone(),
            env_model: descriptor.env_model.clone(),
        };
        let tab = Arc::new(TabCell {
            live: Mutex::new(LiveTab {
                descriptor,
                screen: ScreenModel::new(launch.size),
                pty: PtyBinding::Pending,
                attachments: HashMap::new(),
                pending_replies: VecDeque::new(),
                exit_notified: false,
            }),
            raw: RawDispatch::new(desktop_open, self.inner.queue_capacity),
            last_activity: Mutex::new(Instant::now() - Duration::from_secs(1)),
        });

        let sink = Arc::new(TabSink {
            registry: Arc::downgrade(&self.inner),
            tab: Arc::downgrade(&tab),
        });
        let pty_id = match self.inner.backend.spawn(spec, sink) {
            Ok(pty_id) => pty_id,
            Err(error) => {
                self.inner.release_pending_slot(&slot_id, &id);
                return Err(TabError::new("tab.spawn_failed", error));
            }
        };

        let exited_publication = {
            let _output_order = tab.raw.send_order.lock().unwrap();
            let mut live = tab.live.lock().unwrap();
            if live.descriptor.state == TabState::Exited {
                Some(self.inner.publish_locked(&id, &tab, &live, None))
            } else {
                live.pty = PtyBinding::Flushing(pty_id);
                None
            }
        };
        if let Some(publication) = exited_publication {
            if let Err(error) = publication {
                self.inner.release_pending_slot(&slot_id, &id);
                self.inner.backend.kill(pty_id);
                return Err(error);
            }
            return Ok(id);
        }

        loop {
            let (reply, publication) = {
                let _output_order = tab.raw.send_order.lock().unwrap();
                let mut live = tab.live.lock().unwrap();
                if live.descriptor.state == TabState::Exited {
                    (
                        None,
                        Some(self.inner.publish_locked(&id, &tab, &live, None)),
                    )
                } else {
                    match live.pending_replies.pop_front() {
                        Some(reply) => (Some(reply), None),
                        None => {
                            live.pty = PtyBinding::Ready(pty_id);
                            (
                                None,
                                Some(self.inner.publish_locked(&id, &tab, &live, Some(pty_id))),
                            )
                        }
                    }
                }
            };
            if let Some(publication) = publication {
                if let Err(error) = publication {
                    self.inner.release_pending_slot(&slot_id, &id);
                    self.inner.backend.kill(pty_id);
                    return Err(error);
                }
                return Ok(id);
            }
            let Some(reply) = reply else {
                unreachable!("binding either writes a reply or publishes the tab");
            };
            if let Err(error) = self.inner.backend.write(pty_id, &reply) {
                let exited = tab.live.lock().unwrap().descriptor.state == TabState::Exited;
                if exited {
                    continue;
                }
                self.inner.release_pending_slot(&slot_id, &id);
                self.inner.backend.kill(pty_id);
                return Err(TabError::new("tab.reply_failed", error));
            }
        }
    }

    pub fn list(&self) -> Vec<TabDescriptor> {
        let tabs = {
            let maps = self.inner.maps.lock().unwrap();
            maps.order
                .iter()
                .filter_map(|id| maps.by_id.get(id).cloned())
                .collect::<Vec<_>>()
        };
        tabs.into_iter()
            .map(|tab| tab.live.lock().unwrap().descriptor.clone())
            .collect()
    }

    pub fn get(&self, id: &TabId) -> Result<TabDescriptor, TabError> {
        let tab = self.inner.tab(id)?;
        let descriptor = tab.live.lock().unwrap().descriptor.clone();
        Ok(descriptor)
    }

    pub fn update(&self, id: &TabId, update: TabUpdate) -> Result<TabDescriptor, TabError> {
        self.update_guarded(id, update, None)
    }

    fn update_guarded(
        &self,
        id: &TabId,
        update: TabUpdate,
        expected: Option<&TabDescriptor>,
    ) -> Result<TabDescriptor, TabError> {
        let tab = self.inner.tab(id)?;
        let _output_order = tab.raw.send_order.lock().unwrap();
        tab.raw.require_open()?;
        let descriptor = {
            let mut live = tab.live.lock().unwrap();
            if expected.is_some_and(|previous| {
                previous.session_id != live.descriptor.session_id
                    || previous.slot_id != live.descriptor.slot_id
                    || previous.agent_id != live.descriptor.agent_id
            }) {
                return Err(TabError::new(
                    "tab.identity_changed",
                    "tab identity changed during lookup",
                ));
            }
            if live.descriptor.state != TabState::Running {
                return Err(TabError::new("tab.closed", "the tab has exited"));
            }
            let mut candidate = live.descriptor.clone();
            if let Some(title) = update.title {
                candidate.title = title;
            }
            if let Some(session_id) = update.session_id {
                candidate.session_id = Some(session_id);
            }
            if let Some(resumed_id) = update.resumed_id {
                candidate.resumed_id = Some(resumed_id);
            }
            if let Some(agent_id) = update.agent_id {
                candidate.agent_id = Some(agent_id);
            }
            if let Some(slot) = update.slot_id {
                candidate.slot_id = slot;
            }
            if let Some(fresh) = update.fresh {
                candidate.fresh = fresh;
            }
            if candidate.text_bytes() > MAX_TAB_DESCRIPTOR_TEXT_BYTES {
                return Err(TabError::new(
                    "tab.metadata_too_large",
                    "tab metadata exceeds the recoverable roster bound",
                ));
            }
            if candidate.slot_id != live.descriptor.slot_id {
                let slot = &candidate.slot_id;
                let mut maps = self.inner.maps.lock().unwrap();
                if maps.by_slot.get(slot).is_some_and(|owner| owner != id)
                    || maps
                        .pending_slots
                        .get(slot)
                        .is_some_and(|owner| owner != id)
                {
                    return Err(TabError::new(
                        "tab.slot_in_use",
                        "another tab already owns this slot",
                    ));
                }
                maps.by_slot.remove(&live.descriptor.slot_id);
                maps.by_slot.insert(slot.clone(), id.clone());
            }
            live.descriptor = candidate;
            let descriptor = live.descriptor.clone();
            live.enqueue_control_all(TabEvent::Metadata(descriptor.clone()));
            self.inner
                .maps
                .lock()
                .unwrap()
                .publish_changed(descriptor.clone());
            descriptor
        };
        Ok(descriptor)
    }

    /// Reconcile against the conversation actually held open by the PTY process.
    /// No terminal keystroke inference: this also repairs missed clears, command
    /// completion and input from remote attachments. Filesystem work is outside
    /// registry locks, and a changed binding invalidates its result.
    fn refresh_codex_sessions(&self, resolve: impl Fn(u32) -> Option<String>) {
        for previous in self.list() {
            if previous.state != TabState::Running || previous.agent_id.as_deref() != Some("codex")
            {
                continue;
            }
            let Ok(tab) = self.inner.tab(&previous.id) else {
                continue;
            };
            let pty = tab.live.lock().unwrap().live_pty().ok();
            let Some(pid) = pty.and_then(|id| self.inner.backend.child_pid(id)) else {
                continue;
            };
            let Some(session_id) = resolve(pid) else {
                continue;
            };
            if previous.session_id.as_deref() == Some(session_id.as_str())
                && previous.slot_id == session_id
            {
                continue;
            }
            let _ = self.update_guarded(
                &previous.id,
                TabUpdate::new()
                    .session_id(session_id.clone())
                    .slot_id(session_id)
                    .fresh(false),
                Some(&previous),
            );
        }
    }

    pub fn rekey_session(
        &self,
        id: &TabId,
        session_id: impl Into<String>,
    ) -> Result<TabDescriptor, TabError> {
        let session_id = session_id.into();
        self.update(
            id,
            TabUpdate::new()
                .session_id(session_id.clone())
                .slot_id(session_id),
        )
    }

    pub fn attach(&self, id: &TabId, kind: AttachmentKind) -> Result<TabAttachment, TabError> {
        let tab = self.inner.tab(id)?;
        let attachment_id = AttachmentId::new();
        let mailbox = Arc::new(EventMailbox::new(kind, self.inner.queue_capacity));
        let _output_order = tab.raw.send_order.lock().unwrap();
        tab.raw.require_open()?;
        let (descriptor, focus_changed) = {
            let mut live = tab.live.lock().unwrap();
            if live.descriptor.state != TabState::Running {
                return Err(TabError::new("tab.closed", "the tab has exited"));
            }
            // The snapshot is in the mailbox before the attachment enters the
            // tab's subscriber map. Output cannot discover this attachment and
            // enqueue a diff ahead of its initial state.
            if kind == AttachmentKind::Remote {
                mailbox.push_initial_snapshot(live.screen.snapshot(id.as_str()));
            }
            if kind == AttachmentKind::Desktop && !tab.raw.register(attachment_id.clone(), &mailbox)
            {
                return Err(TabError::new("tab.closed", "the tab is closing"));
            }
            live.attachments.insert(
                attachment_id.clone(),
                AttachmentState {
                    kind,
                    mailbox: mailbox.clone(),
                },
            );
            let focus_changed =
                kind == AttachmentKind::Desktop && live.descriptor.input_owner.is_none();
            if focus_changed {
                live.descriptor.input_owner = Some(attachment_id.clone());
                live.descriptor.focus = TabFocus::Desktop;
                live.enqueue_control_all(TabEvent::FocusChanged {
                    owner: Some(attachment_id.clone()),
                    size: live.descriptor.size,
                });
            }
            (live.descriptor.clone(), focus_changed)
        };
        if focus_changed {
            self.inner.publish_changed(descriptor.clone());
        }
        if kind == AttachmentKind::Desktop {
            // Replay is asynchronous: a backlog larger than the downstream
            // mailbox cannot deadlock this attach before JS receives its
            // channel. The opening queue remains in Replaying until every
            // reserved/queued chunk is handed over, so live bytes stay behind
            // the backlog even after this method returns.
            tab.raw.start_opening_replay(mailbox.clone());
        }
        let cancellation = TabAttachmentCancellation {
            mailbox: mailbox.clone(),
            registry: Arc::downgrade(&self.inner),
            tab_id: id.clone(),
            attachment_id: attachment_id.clone(),
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        Ok(TabAttachment {
            id: attachment_id.clone(),
            events: TabEventReceiver {
                mailbox,
                tab_id: id.clone(),
                attachment_id,
                cancellation: cancellation.clone(),
            },
            cancellation,
            descriptor,
        })
    }

    pub fn snapshot(&self, id: &TabId) -> Result<ScreenSnapshot, TabError> {
        let tab = self.inner.tab(id)?;
        let _output_order = tab.raw.send_order.lock().unwrap();
        let snapshot = tab.live.lock().unwrap().screen.snapshot(id.as_str());
        Ok(snapshot)
    }

    /// Capture a recovery viewport and the exact subscriber sequence boundary
    /// under the same output-order lock. A remote actor can discard only the
    /// events made obsolete by this snapshot without losing later damage.
    pub fn recovery_snapshot(
        &self,
        id: &TabId,
        attachment: &AttachmentId,
    ) -> Result<(ScreenSnapshot, RecoveryBoundary, TabDescriptor), TabError> {
        let tab = self.inner.tab(id)?;
        let _output_order = tab.raw.send_order.lock().unwrap();
        tab.raw.require_open()?;
        let live = tab.live.lock().unwrap();
        let state = live.attachments.get(attachment).ok_or_else(|| {
            TabError::new(
                "terminal.attachment_not_found",
                "unknown terminal attachment",
            )
        })?;
        Ok((
            live.screen.snapshot(id.as_str()),
            RecoveryBoundary(state.mailbox.recovery_boundary()),
            live.descriptor.clone(),
        ))
    }

    pub fn scrollback(
        &self,
        id: &TabId,
        offset: usize,
        count: usize,
    ) -> Result<Vec<ScreenRow>, TabError> {
        let tab = self.inner.tab(id)?;
        let _output_order = tab.raw.send_order.lock().unwrap();
        let page = tab
            .live
            .lock()
            .unwrap()
            .screen
            .scrollback_page(offset, count);
        Ok(page)
    }

    /// Read the scrollback revision and page under one output-order boundary.
    pub fn scrollback_page(
        &self,
        id: &TabId,
        offset: usize,
        count: usize,
    ) -> Result<ScrollbackPage, TabError> {
        let tab = self.inner.tab(id)?;
        let _output_order = tab.raw.send_order.lock().unwrap();
        let live = tab.live.lock().unwrap();
        Ok(ScrollbackPage {
            revision: live.screen.revision(),
            rows: live.screen.scrollback_page(offset, count),
        })
    }

    pub fn attachment_count(&self, id: &TabId) -> Result<usize, TabError> {
        let tab = self.inner.tab(id)?;
        let _output_order = tab.raw.send_order.lock().unwrap();
        let count = tab.live.lock().unwrap().attachments.len();
        Ok(count)
    }

    pub fn input(
        &self,
        id: &TabId,
        attachment: &AttachmentId,
        bytes: &[u8],
    ) -> Result<(), TabError> {
        let tab = self.inner.tab(id)?;
        let _output_order = tab.raw.send_order.lock().unwrap();
        tab.raw.require_open()?;
        let live = tab.live.lock().unwrap();
        live.authorize_owner(attachment)?;
        let pty_id = live.live_pty()?;
        self.inner
            .backend
            .write(pty_id, bytes)
            .map_err(|error| TabError::new("terminal.write_failed", error))
    }

    pub fn resize(
        &self,
        id: &TabId,
        attachment: &AttachmentId,
        size: TerminalSize,
    ) -> Result<(), TabError> {
        let tab = self.inner.tab(id)?;
        let _output_order = tab.raw.send_order.lock().unwrap();
        tab.raw.require_open()?;
        let descriptor = {
            let mut live = tab.live.lock().unwrap();
            live.authorize_owner(attachment)?;
            let pty_id = live.live_pty()?;
            self.inner
                .backend
                .resize(pty_id, size.cols(), size.rows())
                .map_err(|error| TabError::new("terminal.resize_failed", error))?;
            live.resize(id, size);
            live.descriptor.clone()
        };
        self.inner.publish_changed(descriptor);
        Ok(())
    }

    pub fn take_focus(
        &self,
        id: &TabId,
        attachment: &AttachmentId,
        size: TerminalSize,
    ) -> Result<(), TabError> {
        let tab = self.inner.tab(id)?;
        let _output_order = tab.raw.send_order.lock().unwrap();
        tab.raw.require_open()?;
        let descriptor = {
            let mut live = tab.live.lock().unwrap();
            if !live.attachments.contains_key(attachment) {
                return Err(TabError::new(
                    "terminal.attachment_not_found",
                    "the attachment does not belong to this tab",
                ));
            }
            let pty_id = live.live_pty()?;
            self.inner
                .backend
                .resize(pty_id, size.cols(), size.rows())
                .map_err(|error| TabError::new("terminal.resize_failed", error))?;
            live.descriptor.input_owner = Some(attachment.clone());
            live.descriptor.focus = match live.attachments[attachment].kind {
                AttachmentKind::Desktop => TabFocus::Desktop,
                AttachmentKind::Remote => TabFocus::Remote,
            };
            live.resize(id, size);
            live.enqueue_control_all(TabEvent::FocusChanged {
                owner: Some(attachment.clone()),
                size,
            });
            live.descriptor.clone()
        };
        self.inner.publish_changed(descriptor);
        Ok(())
    }

    pub fn close(&self, id: &TabId) -> Result<(), TabError> {
        let tab = self.inner.tab(id)?;
        // Cancellation is independent of both the output-order gate and live
        // state. Wake a bounded raw producer first, then join its transaction
        // before publishing Exited.
        tab.raw.close();
        let (pty_id, slot_id, exit) = {
            let _output_order = tab.raw.send_order.lock().unwrap();
            let mut live = tab.live.lock().unwrap();
            let pty_id = live.pty.id();
            live.pty = PtyBinding::Exited;
            let slot_id = live.descriptor.slot_id.clone();
            let exit = live.mark_exited(None, None, true);
            (pty_id, slot_id, exit)
        };
        if let Some(exit) = exit {
            self.inner.publish_exit(id, &exit);
        }
        self.inner.remove_tab(id, &slot_id, pty_id, true);
        if let Some(pty_id) = pty_id {
            self.inner.backend.kill(pty_id);
        }
        Ok(())
    }

    pub fn detach(&self, id: &TabId, attachment: &AttachmentId) -> Result<(), TabError> {
        self.inner.tab(id)?;
        if self.inner.detach(id, attachment) {
            Ok(())
        } else {
            Err(TabError::new(
                "terminal.attachment_not_found",
                "the attachment does not belong to this tab",
            ))
        }
    }

    pub fn tab_for_descendant(&self, pid: u32) -> Option<TabId> {
        let pty_id = self.inner.backend.pty_for_descendant(pid)?;
        self.inner.maps.lock().ok()?.by_pty.get(&pty_id).cloned()
    }

    // Session-keyed views for the phone listener (`remote_api`). That
    // protocol authorizes the phone at the listener (token + pinned TLS),
    // then addresses tabs by the session they run — not by tab id, and not
    // through an input-owner attachment. These helpers answer by session id
    // and write through the backend directly, leaving desktop input
    // ownership untouched.

    fn cells(&self) -> Vec<Arc<TabCell>> {
        let maps = self.inner.maps.lock().unwrap();
        maps.order
            .iter()
            .filter_map(|id| maps.by_id.get(id).cloned())
            .collect()
    }

    fn cell_for_session(&self, session_id: &str) -> Option<Arc<TabCell>> {
        self.cells().into_iter().find(|tab| {
            let live = tab.live.lock().unwrap();
            live.descriptor.state == TabState::Running
                && (live.descriptor.session_id.as_deref() == Some(session_id)
                    || (live.descriptor.session_id.is_none()
                        && live.descriptor.resumed_id.as_deref() == Some(session_id))
                    || live.descriptor.slot_id == session_id)
        })
    }

    /// Session ids of tabs whose process is still running.
    pub fn bound_sessions(&self) -> Vec<String> {
        self.cells()
            .iter()
            .filter_map(|tab| {
                let live = tab.live.lock().unwrap();
                if live.descriptor.state == TabState::Running {
                    live.descriptor.session_id.clone()
                } else {
                    None
                }
            })
            .collect()
    }

    /// Whether some running tab is bound to this session.
    pub fn has_session(&self, session_id: &str) -> bool {
        self.cell_for_session(session_id).is_some()
    }

    /// What each bound session's terminal is doing, as cadence evidence:
    /// "output" while bytes flowed recently, "idle" otherwise. Weak on its
    /// own — the transcript outranks it, and `remote_api` upgrades verdicts
    /// with `transcript_state` before a phone sees them.
    pub fn session_activities(&self) -> Vec<(String, String)> {
        self.cells()
            .iter()
            .filter_map(|tab| {
                let live = tab.live.lock().unwrap();
                if live.descriptor.state != TabState::Running {
                    return None;
                }
                let session_id = live.descriptor.session_id.clone()?;
                drop(live);
                let recent =
                    tab.last_activity.lock().unwrap().elapsed() < Duration::from_secs(10);
                Some((session_id, if recent { "output" } else { "idle" }.to_string()))
            })
            .collect()
    }

    /// Write into the session's tab as the phone: straight to the PTY,
    /// ordered against desktop output but not gated on input ownership.
    pub fn write_session_str(&self, session_id: &str, data: &str) -> Result<(), String> {
        let tab = self
            .cell_for_session(session_id)
            .ok_or_else(|| "session is not open in a tab".to_string())?;
        let _send_order = tab.raw.send_order.lock().unwrap();
        tab.raw.require_open().map_err(|e| e.to_string())?;
        let pty_id = tab
            .live
            .lock()
            .unwrap()
            .live_pty()
            .map_err(|e| e.to_string())?;
        self.inner.backend.write(pty_id, data.as_bytes())
    }

    /// Raw write into a tab as the phone, keyed by tab id — for tabs that
    /// run no session at all (a plain shell). Same doctrine as
    /// `write_session_str`: ordered against output, not gated on input
    /// ownership, authorized at the listener.
    pub fn write_tab_str(&self, id: &TabId, data: &str) -> Result<(), String> {
        let tab = self.inner.tab(id).map_err(|e| e.to_string())?;
        let _send_order = tab.raw.send_order.lock().unwrap();
        tab.raw.require_open().map_err(|e| e.to_string())?;
        let pty_id = tab
            .live
            .lock()
            .unwrap()
            .live_pty()
            .map_err(|e| e.to_string())?;
        self.inner.backend.write(pty_id, data.as_bytes())
    }

    /// End the process of the session's tab. The tab stays, showing its
    /// exit — a phone-side stop reads as "something killed it", not as the
    /// person closing the tab.
    pub fn kill_session_tab(&self, session_id: &str) -> bool {
        let Some(tab) = self.cell_for_session(session_id) else {
            return false;
        };
        let Ok(pty_id) = tab.live.lock().unwrap().live_pty() else {
            return false;
        };
        self.inner.backend.kill(pty_id);
        true
    }

    /// Root pid of the session's tab process, for port scanning.
    pub fn child_pid_for_session(&self, session_id: &str) -> Option<u32> {
        let tab = self.cell_for_session(session_id)?;
        let pty_id = tab.live.lock().unwrap().live_pty().ok()?;
        self.inner.backend.child_pid(pty_id)
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopTabExit {
    tab_id: TabId,
    code: Option<u32>,
    signal: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(
    tag = "change",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
enum DesktopTabRegistryEvent {
    Snapshot {
        revision: u64,
        tabs: Vec<TabDescriptor>,
    },
    Opened {
        revision: u64,
        tab_id: TabId,
        tab: TabDescriptor,
    },
    Changed {
        revision: u64,
        tab_id: TabId,
        tab: TabDescriptor,
    },
    Removed {
        revision: u64,
        tab_id: TabId,
        requested: bool,
    },
}

impl From<TabRegistryEvent> for DesktopTabRegistryEvent {
    fn from(event: TabRegistryEvent) -> Self {
        match event {
            TabRegistryEvent::Snapshot { revision, tabs } => Self::Snapshot { revision, tabs },
            TabRegistryEvent::Opened { revision, tab } => Self::Opened {
                revision,
                tab_id: tab.id().clone(),
                tab,
            },
            TabRegistryEvent::Changed { revision, tab } => Self::Changed {
                revision,
                tab_id: tab.id().clone(),
                tab,
            },
            TabRegistryEvent::Removed {
                revision,
                tab_id,
                requested,
            } => Self::Removed {
                revision,
                tab_id,
                requested,
            },
        }
    }
}

fn command_error(error: TabError) -> String {
    error.to_string()
}

fn emit_desktop_exit(app: &AppHandle, tab_id: &TabId, exit: &TabExit) {
    let _ = app.emit(
        "tab://exit",
        DesktopTabExit {
            tab_id: tab_id.clone(),
            code: exit.code(),
            signal: exit.signal().map(str::to_owned),
        },
    );
}

/// Project one registry event into the phone listener's session-keyed
/// stream (`remote_api`): a tab binding or losing a session moves the
/// phone's list, and a Running→Exited transition is that session's exit.
/// `known` remembers each tab's last-seen (session, state) so both are
/// announced exactly once.
fn note_tab_for_phone(
    app: &AppHandle,
    known: &mut HashMap<TabId, (Option<String>, TabState)>,
    tab: &TabDescriptor,
    list_moved: &mut bool,
) {
    let session = tab.session_id().map(str::to_owned);
    let state = tab.state().clone();
    match known.get(tab.id()) {
        Some((prev_session, prev_state)) => {
            if *prev_session != session {
                *list_moved = true;
            }
            if *prev_state == TabState::Running && state == TabState::Exited {
                crate::remote_api::notify(
                    app,
                    crate::remote_api::Event::SessionExit {
                        session_id: session.clone(),
                        code: tab.exit().and_then(|exit| exit.code()),
                    },
                );
                *list_moved = true;
            }
        }
        None => {
            if session.is_some() {
                *list_moved = true;
            }
        }
    }
    known.insert(tab.id().clone(), (session, state));
}

fn unexpected_desktop_exits(change: &TabRegistryEvent) -> Vec<(TabId, TabExit)> {
    fn unexpected(tab: &TabDescriptor) -> Option<(TabId, TabExit)> {
        tab.exit()
            .filter(|exit| !exit.requested())
            .map(|exit| (tab.id().clone(), exit.clone()))
    }

    match change {
        TabRegistryEvent::Snapshot { tabs, .. } => tabs.iter().filter_map(unexpected).collect(),
        TabRegistryEvent::Opened { tab, .. } | TabRegistryEvent::Changed { tab, .. } => {
            unexpected(tab).into_iter().collect()
        }
        TabRegistryEvent::Removed { .. } => Vec::new(),
    }
}

/// Project the bounded, recoverable process-wide registry stream into the
/// desktop renderer. Attachment workers remain raw-byte-only, so tabs opened
/// or closed by another transport are visible even when no desktop terminal
/// is attached.
pub fn start_desktop_registry_bridge(
    app: AppHandle,
    registry: Arc<TabRegistry>,
) -> Result<(), String> {
    // Keep identity discovery off the UI, PTY input and registry-event threads.
    // Weak ownership lets this worker stop when the application registry closes.
    let identity_registry = Arc::downgrade(&registry);
    std::thread::Builder::new()
        .name("codex-session-identity".to_string())
        .spawn(move || loop {
            let Some(registry) = identity_registry.upgrade() else {
                break;
            };
            registry.refresh_codex_sessions(crate::codex_identity::resolve);
            drop(registry);
            std::thread::sleep(Duration::from_secs(2));
        })
        .map_err(|error| error.to_string())?;
    let changes = registry.subscribe_changes();
    let activity = registry.subscribe_activity();
    std::thread::Builder::new()
        .name("desktop-tab-registry".to_string())
        .spawn(move || {
            let mut known: HashMap<TabId, (Option<String>, TabState)> = HashMap::new();
            loop {
                match changes.recv_timeout(Duration::from_millis(250)) {
                    Ok(change) => {
                        let mut list_moved = false;
                        match &change {
                            TabRegistryEvent::Snapshot { tabs, .. } => {
                                for tab in tabs {
                                    note_tab_for_phone(&app, &mut known, tab, &mut list_moved);
                                }
                            }
                            TabRegistryEvent::Opened { tab, .. }
                            | TabRegistryEvent::Changed { tab, .. } => {
                                note_tab_for_phone(&app, &mut known, tab, &mut list_moved);
                            }
                            TabRegistryEvent::Removed { tab_id, .. } => {
                                if known.remove(tab_id).is_some() {
                                    list_moved = true;
                                }
                            }
                        }
                        if list_moved {
                            crate::remote_api::notify(
                                &app,
                                crate::remote_api::Event::SessionsChanged,
                            );
                        }
                        let sessions: Vec<String> = match &change {
                            TabRegistryEvent::Snapshot { tabs, .. } => tabs
                                .iter()
                                .filter_map(|tab| tab.session_id().map(str::to_owned))
                                .collect(),
                            TabRegistryEvent::Opened { tab, .. }
                            | TabRegistryEvent::Changed { tab, .. } => {
                                tab.session_id().map(str::to_owned).into_iter().collect()
                            }
                            TabRegistryEvent::Removed { .. } => Vec::new(),
                        };
                        for session_id in sessions {
                            // A bound tab is standing interest in a session:
                            // the spine tails it whether or not a phone has
                            // asked, so the history is already there when one
                            // does. Snapshot events cover tabs that were
                            // already bound when this bridge started.
                            crate::spine::ensure_tail_for(&app, &session_id);
                            crate::changes::track(&app, session_id);
                        }
                        let unexpected_exits = unexpected_desktop_exits(&change);
                        let _ = app.emit("tab://registry", DesktopTabRegistryEvent::from(change));
                        for (tab_id, exit) in unexpected_exits {
                            emit_desktop_exit(&app, &tab_id, &exit);
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                while let Ok(session_id) = activity.try_recv() {
                    crate::changes::touch(&app, &session_id);
                    // The same verdict onto the spine, so a consumer has one
                    // stream instead of two. Only for sessions already being
                    // tailed, and only when the phase actually changed — this
                    // fires four times a second while output flows.
                    crate::spine::push_phase(&app, &session_id, "output");
                    // A phone shows "working" off terminal cadence the moment
                    // bytes flow; the transcript upgrades or overrides this on
                    // the next sessions poll.
                    crate::remote_api::notify(
                        &app,
                        crate::remote_api::Event::Activity {
                            session_id,
                            activity: "output".into(),
                        },
                    );
                }
            }
        })
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn tab_open(
    state: State<'_, Arc<TabRegistry>>,
    launch: TabLaunch,
) -> Result<TabDescriptor, String> {
    let registry = (*state).clone();
    crate::run_blocking(move || {
        let id = registry.open_desktop(launch).map_err(command_error)?;
        registry.get(&id).map_err(command_error)
    })
    .await
}

#[tauri::command]
pub fn tab_list(state: State<'_, Arc<TabRegistry>>) -> Vec<TabDescriptor> {
    state.list()
}

#[tauri::command]
pub fn tab_registry_snapshot(state: State<'_, Arc<TabRegistry>>) -> TabRegistrySnapshot {
    state.roster_snapshot()
}

#[tauri::command]
pub fn tab_update(
    state: State<'_, Arc<TabRegistry>>,
    tab_id: TabId,
    update: TabUpdate,
) -> Result<TabDescriptor, String> {
    state.update(&tab_id, update).map_err(command_error)
}

fn forward_desktop_events(events: TabEventReceiver, mut send_raw: impl FnMut(Vec<u8>) -> bool) {
    while let Ok(event) = events.recv() {
        match event {
            TabEvent::Raw(bytes) => {
                if !send_raw(bytes) {
                    break;
                }
            }
            TabEvent::Metadata(_) | TabEvent::Title(_) | TabEvent::Exited(_) => {}
            TabEvent::Snapshot(_)
            | TabEvent::SharedSnapshot(_)
            | TabEvent::Diff(_)
            | TabEvent::FocusChanged { .. }
            | TabEvent::Bell => {}
        }
    }
}

#[tauri::command]
pub fn tab_attach_desktop(
    state: State<'_, Arc<TabRegistry>>,
    tab_id: TabId,
    on_output: Channel<InvokeResponseBody>,
) -> Result<AttachmentId, String> {
    let registry = (*state).clone();
    let attachment = registry
        .attach(&tab_id, AttachmentKind::Desktop)
        .map_err(command_error)?;
    let attachment_id = attachment.id.clone();
    let events = attachment.events;
    std::thread::Builder::new()
        .name(format!("desktop-tab-{}", tab_id.as_str()))
        .spawn(move || {
            forward_desktop_events(events, |bytes| {
                on_output.send(InvokeResponseBody::Raw(bytes)).is_ok()
            });
        })
        .map_err(|error| error.to_string())?;
    Ok(attachment_id)
}

#[tauri::command]
pub async fn tab_detach(
    state: State<'_, Arc<TabRegistry>>,
    tab_id: TabId,
    attachment_id: AttachmentId,
) -> Result<(), String> {
    let registry = (*state).clone();
    crate::run_blocking(move || {
        registry
            .detach(&tab_id, &attachment_id)
            .map_err(command_error)
    })
    .await
}

#[tauri::command]
pub async fn tab_write(
    state: State<'_, Arc<TabRegistry>>,
    tab_id: TabId,
    attachment_id: AttachmentId,
    data: String,
) -> Result<(), String> {
    let registry = (*state).clone();
    crate::run_blocking(move || {
        registry
            .input(&tab_id, &attachment_id, data.as_bytes())
            .map_err(command_error)
    })
    .await
}

fn terminal_size(cols: u16, rows: u16) -> Result<TerminalSize, String> {
    TerminalSize::try_new(cols, rows).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn tab_resize(
    state: State<'_, Arc<TabRegistry>>,
    tab_id: TabId,
    attachment_id: AttachmentId,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let size = terminal_size(cols, rows)?;
    let registry = (*state).clone();
    crate::run_blocking(move || {
        registry
            .resize(&tab_id, &attachment_id, size)
            .map_err(command_error)
    })
    .await
}

#[tauri::command]
pub async fn tab_take_focus(
    state: State<'_, Arc<TabRegistry>>,
    tab_id: TabId,
    attachment_id: AttachmentId,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let size = terminal_size(cols, rows)?;
    let registry = (*state).clone();
    crate::run_blocking(move || {
        registry
            .take_focus(&tab_id, &attachment_id, size)
            .map_err(command_error)
    })
    .await
}

#[tauri::command]
pub async fn tab_close(state: State<'_, Arc<TabRegistry>>, tab_id: TabId) -> Result<(), String> {
    let registry = (*state).clone();
    crate::run_blocking(move || registry.close(&tab_id).map_err(command_error)).await
}

struct RegistryInner {
    backend: Arc<dyn PtyBackend>,
    maps: Mutex<RegistryMaps>,
    queue_capacity: usize,
    exit_subscribers: Mutex<Vec<mpsc::Sender<(TabId, TabExit)>>>,
    activity_subscribers: Mutex<Vec<SyncSender<String>>>,
}

impl Drop for RegistryInner {
    fn drop(&mut self) {
        if let Ok(maps) = self.maps.get_mut() {
            for subscriber in maps.subscribers.iter().filter_map(Weak::upgrade) {
                subscriber.close_producer();
            }
        }
    }
}

#[derive(Default)]
struct RegistryMaps {
    by_id: HashMap<TabId, Arc<TabCell>>,
    by_slot: HashMap<String, TabId>,
    by_pty: HashMap<u32, TabId>,
    pending_slots: HashMap<String, TabId>,
    order: Vec<TabId>,
    roster: HashMap<TabId, TabDescriptor>,
    revision: u64,
    subscribers: Vec<Weak<RegistryMailbox>>,
}

impl RegistryMaps {
    fn snapshot(&self) -> TabRegistrySnapshot {
        TabRegistrySnapshot {
            revision: self.revision,
            tabs: self
                .order
                .iter()
                .filter_map(|id| self.roster.get(id).cloned())
                .collect(),
        }
    }

    fn snapshot_event(&self) -> TabRegistryEvent {
        let snapshot = self.snapshot();
        TabRegistryEvent::Snapshot {
            revision: snapshot.revision,
            tabs: snapshot.tabs,
        }
    }

    fn publish_opened(&mut self, descriptor: TabDescriptor) {
        self.roster
            .insert(descriptor.id().clone(), descriptor.clone());
        self.revision = self.revision.saturating_add(1);
        self.publish(TabRegistryEvent::Opened {
            revision: self.revision,
            tab: descriptor,
        });
    }

    fn publish_changed(&mut self, descriptor: TabDescriptor) {
        self.roster
            .insert(descriptor.id().clone(), descriptor.clone());
        self.revision = self.revision.saturating_add(1);
        self.publish(TabRegistryEvent::Changed {
            revision: self.revision,
            tab: descriptor,
        });
    }

    fn publish_removed(&mut self, tab_id: TabId, requested: bool) {
        self.roster.remove(&tab_id);
        self.revision = self.revision.saturating_add(1);
        self.publish(TabRegistryEvent::Removed {
            revision: self.revision,
            tab_id,
            requested,
        });
    }

    fn publish(&mut self, event: TabRegistryEvent) {
        let recovery = self.snapshot_event();
        self.subscribers.retain(|subscriber| {
            let Some(subscriber) = subscriber.upgrade() else {
                return false;
            };
            subscriber.push(event.clone(), &recovery);
            true
        });
    }
}

struct TabCell {
    live: Mutex<LiveTab>,
    raw: RawDispatch,
    last_activity: Mutex<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpeningRawPhase {
    Pending,
    Replaying,
    Attached,
    Closed,
}

struct OpeningRawState {
    phase: OpeningRawPhase,
    queue: VecDeque<Vec<u8>>,
    reserved: usize,
}

struct OpeningRaw {
    capacity: usize,
    state: Mutex<OpeningRawState>,
    changed: Condvar,
}

enum OpeningRoute {
    Reserved(OpeningReservation),
    Attached,
    Closed,
}

struct OpeningReservation {
    opening: Arc<OpeningRaw>,
    resolved: bool,
}

impl OpeningRaw {
    fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            capacity: capacity.max(1),
            state: Mutex::new(OpeningRawState {
                phase: OpeningRawPhase::Pending,
                queue: VecDeque::new(),
                reserved: 0,
            }),
            changed: Condvar::new(),
        })
    }

    /// Reserve one bounded chunk before taking the Task 4 output-order gate.
    /// Attach and close never need this reservation, so either can make
    /// progress while a producer waits for replay capacity.
    fn reserve(self: &Arc<Self>) -> OpeningRoute {
        let mut state = self.state.lock().unwrap();
        loop {
            match state.phase {
                OpeningRawPhase::Attached => return OpeningRoute::Attached,
                OpeningRawPhase::Closed => return OpeningRoute::Closed,
                OpeningRawPhase::Pending | OpeningRawPhase::Replaying => {
                    if state.queue.len() + state.reserved < self.capacity {
                        state.reserved += 1;
                        return OpeningRoute::Reserved(OpeningReservation {
                            opening: self.clone(),
                            resolved: false,
                        });
                    }
                    state = self.changed.wait(state).unwrap();
                }
            }
        }
    }

    fn start_replay(self: &Arc<Self>, mailbox: Arc<EventMailbox>) {
        {
            let mut state = self.state.lock().unwrap();
            if state.phase != OpeningRawPhase::Pending {
                return;
            }
            state.phase = OpeningRawPhase::Replaying;
            self.changed.notify_all();
        }
        let opening = self.clone();
        std::thread::Builder::new()
            .name("desktop-opening-replay".to_string())
            .spawn(move || opening.replay(mailbox))
            .expect("desktop opening replay thread");
    }

    fn replay(&self, mailbox: Arc<EventMailbox>) {
        loop {
            let chunk = {
                let mut state = self.state.lock().unwrap();
                loop {
                    match state.phase {
                        OpeningRawPhase::Closed => return,
                        OpeningRawPhase::Replaying => {
                            if let Some(chunk) = state.queue.pop_front() {
                                self.changed.notify_all();
                                break chunk;
                            }
                            if state.reserved == 0 {
                                state.phase = OpeningRawPhase::Attached;
                                self.changed.notify_all();
                                return;
                            }
                            state = self.changed.wait(state).unwrap();
                        }
                        OpeningRawPhase::Pending | OpeningRawPhase::Attached => return,
                    }
                }
            };
            if !mailbox.push_raw(chunk) {
                self.close();
                return;
            }
        }
    }

    fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.phase = OpeningRawPhase::Closed;
        state.queue.clear();
        self.changed.notify_all();
    }
}

impl OpeningReservation {
    /// Commit under the output-order gate. The replay worker cannot switch to
    /// live delivery while any reservation exists, so later raw bytes cannot
    /// overtake this chunk.
    fn enqueue(mut self, bytes: Vec<u8>) -> bool {
        let mut state = self.opening.state.lock().unwrap();
        debug_assert!(state.reserved > 0);
        state.reserved -= 1;
        let accepted = state.phase != OpeningRawPhase::Closed;
        if accepted {
            state.queue.push_back(bytes);
        }
        self.resolved = true;
        self.opening.changed.notify_all();
        accepted
    }
}

impl Drop for OpeningReservation {
    fn drop(&mut self) {
        if self.resolved {
            return;
        }
        let mut state = self.opening.state.lock().unwrap();
        debug_assert!(state.reserved > 0);
        state.reserved -= 1;
        self.opening.changed.notify_all();
    }
}

struct RawDispatch {
    phase: AtomicU8,
    mailboxes: Mutex<HashMap<AttachmentId, Weak<EventMailbox>>>,
    opening: Option<Arc<OpeningRaw>>,
    producer_order: Mutex<()>,
    send_order: Mutex<()>,
}

impl RawDispatch {
    fn new(desktop_open: bool, capacity: usize) -> Self {
        Self {
            phase: AtomicU8::new(RAW_OPEN),
            mailboxes: Mutex::new(HashMap::new()),
            opening: desktop_open.then(|| OpeningRaw::new(capacity)),
            producer_order: Mutex::new(()),
            send_order: Mutex::new(()),
        }
    }

    fn reserve_opening(&self) -> OpeningRoute {
        self.opening
            .as_ref()
            .map(OpeningRaw::reserve)
            .unwrap_or(OpeningRoute::Attached)
    }

    fn start_opening_replay(&self, mailbox: Arc<EventMailbox>) {
        if let Some(opening) = &self.opening {
            opening.start_replay(mailbox);
        }
    }

    fn register(&self, id: AttachmentId, mailbox: &Arc<EventMailbox>) -> bool {
        if self.phase.load(Ordering::Acquire) != RAW_OPEN {
            return false;
        }
        let mut mailboxes = self.mailboxes.lock().unwrap();
        if self.phase.load(Ordering::Acquire) != RAW_OPEN {
            return false;
        }
        mailboxes.insert(id, Arc::downgrade(mailbox));
        true
    }

    fn unregister(&self, id: &AttachmentId) {
        self.mailboxes.lock().unwrap().remove(id);
    }

    fn cancel_waits(&self) {
        if let Some(opening) = &self.opening {
            opening.close();
        }
        let mailboxes = self
            .mailboxes
            .lock()
            .unwrap()
            .values()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>();
        for mailbox in mailboxes {
            mailbox.cancel_raw();
        }
    }

    fn prepare_exit(&self) {
        let _ = self.phase.compare_exchange(
            RAW_OPEN,
            RAW_PREPARING_EXIT,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        self.cancel_waits();
    }

    fn close(&self) {
        self.phase.store(RAW_CLOSING, Ordering::Release);
        self.cancel_waits();
    }

    fn is_closing(&self) -> bool {
        self.phase.load(Ordering::Acquire) != RAW_OPEN
    }

    fn require_open(&self) -> Result<(), TabError> {
        if self.phase.load(Ordering::Acquire) == RAW_OPEN {
            Ok(())
        } else {
            Err(TabError::new("tab.closed", "the tab is closing"))
        }
    }
}

const RAW_OPEN: u8 = 0;
const RAW_PREPARING_EXIT: u8 = 1;
const RAW_CLOSING: u8 = 2;

impl RegistryInner {
    fn publish_exit(&self, id: &TabId, exit: &TabExit) {
        self.exit_subscribers
            .lock()
            .unwrap()
            .retain(|subscriber| subscriber.send((id.clone(), exit.clone())).is_ok());
    }

    fn publish_activity(&self, session_id: String) {
        self.activity_subscribers
            .lock()
            .unwrap()
            .retain(|subscriber| match subscriber.try_send(session_id.clone()) {
                Ok(()) | Err(mpsc::TrySendError::Full(_)) => true,
                Err(mpsc::TrySendError::Disconnected(_)) => false,
            });
    }

    fn tab(&self, id: &TabId) -> Result<Arc<TabCell>, TabError> {
        self.maps
            .lock()
            .unwrap()
            .by_id
            .get(id)
            .cloned()
            .ok_or_else(|| TabError::new("tab.not_found", "unknown tab id"))
    }

    fn publish_changed(&self, descriptor: TabDescriptor) {
        let mut maps = self.maps.lock().unwrap();
        if maps.by_id.contains_key(descriptor.id()) {
            maps.publish_changed(descriptor);
        }
    }

    fn remove_tab(&self, id: &TabId, slot_id: &str, pty_id: Option<u32>, requested: bool) {
        let mut maps = self.maps.lock().unwrap();
        maps.by_id.remove(id);
        if maps.by_slot.get(slot_id) == Some(id) {
            maps.by_slot.remove(slot_id);
        }
        if let Some(pty_id) = pty_id {
            maps.by_pty.remove(&pty_id);
        }
        maps.order.retain(|candidate| candidate != id);
        maps.by_pty.retain(|_, tab_id| tab_id != id);
        maps.publish_removed(id.clone(), requested);
    }

    fn release_pending_slot(&self, slot_id: &str, id: &TabId) {
        let mut maps = self.maps.lock().unwrap();
        if maps.pending_slots.get(slot_id) == Some(id) {
            maps.pending_slots.remove(slot_id);
        }
    }

    /// Publish only while the caller holds this tab's lock. This makes the
    /// Ready/Exited state and every public index appear as one transition.
    fn publish_locked(
        &self,
        id: &TabId,
        tab: &Arc<TabCell>,
        live: &LiveTab,
        pty_id: Option<u32>,
    ) -> Result<(), TabError> {
        let mut maps = self.maps.lock().unwrap();
        if maps.pending_slots.get(&live.descriptor.slot_id) != Some(id) {
            return Err(TabError::new(
                "tab.slot_reservation_lost",
                "the opening tab no longer owns its slot reservation",
            ));
        }
        if maps
            .by_slot
            .get(&live.descriptor.slot_id)
            .is_some_and(|owner| owner != id)
        {
            return Err(TabError::new(
                "tab.slot_in_use",
                "another tab already owns this slot",
            ));
        }
        maps.pending_slots.remove(&live.descriptor.slot_id);
        maps.by_slot
            .insert(live.descriptor.slot_id.clone(), id.clone());
        maps.order.push(id.clone());
        maps.by_id.insert(id.clone(), tab.clone());
        if let Some(pty_id) = pty_id {
            maps.by_pty.insert(pty_id, id.clone());
        }
        maps.publish_opened(live.descriptor.clone());
        Ok(())
    }

    fn detach(&self, tab_id: &TabId, attachment_id: &AttachmentId) -> bool {
        let Ok(tab) = self.tab(tab_id) else {
            return false;
        };
        let _output_order = tab.raw.send_order.lock().unwrap();
        let (removed, changed) = {
            let mut live = tab.live.lock().unwrap();
            let Some(attachment) = live.attachments.remove(attachment_id) else {
                return false;
            };
            let mut changed = None;
            if live.descriptor.input_owner.as_ref() == Some(attachment_id) {
                live.descriptor.input_owner = None;
                live.descriptor.focus = TabFocus::Unowned;
                live.enqueue_control_all(TabEvent::FocusChanged {
                    owner: None,
                    size: live.descriptor.size,
                });
                changed = Some(live.descriptor.clone());
            }
            (attachment.kind, changed)
        };
        if let Some(descriptor) = changed {
            self.publish_changed(descriptor);
        }
        if removed == AttachmentKind::Desktop {
            tab.raw.unregister(attachment_id);
        }
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PtyBinding {
    Pending,
    Flushing(u32),
    Ready(u32),
    Exited,
}

impl PtyBinding {
    fn id(self) -> Option<u32> {
        match self {
            Self::Flushing(id) | Self::Ready(id) => Some(id),
            Self::Pending | Self::Exited => None,
        }
    }
}

struct LiveTab {
    descriptor: TabDescriptor,
    screen: ScreenModel,
    pty: PtyBinding,
    attachments: HashMap<AttachmentId, AttachmentState>,
    pending_replies: VecDeque<Vec<u8>>,
    exit_notified: bool,
}

impl LiveTab {
    fn live_pty(&self) -> Result<u32, TabError> {
        if self.descriptor.state != TabState::Running {
            return Err(TabError::new("tab.closed", "the tab has exited"));
        }
        self.pty
            .id()
            .ok_or_else(|| TabError::new("tab.not_ready", "the PTY is still starting"))
    }

    fn authorize_owner(&self, attachment: &AttachmentId) -> Result<(), TabError> {
        if !self.attachments.contains_key(attachment) {
            return Err(TabError::new(
                "terminal.attachment_not_found",
                "the attachment does not belong to this tab",
            ));
        }
        if self.descriptor.input_owner.as_ref() != Some(attachment) {
            return Err(TabError::new(
                "terminal.input_not_owned",
                "another attachment owns terminal input and resize",
            ));
        }
        Ok(())
    }

    fn enqueue_control_all(&self, event: TabEvent) {
        for attachment in self.attachments.values() {
            attachment.mailbox.push_control(event.clone());
        }
    }

    fn desktop_mailboxes(&self) -> Vec<Arc<EventMailbox>> {
        self.attachments
            .values()
            .filter(|attachment| attachment.kind == AttachmentKind::Desktop)
            .map(|attachment| attachment.mailbox.clone())
            .collect()
    }

    fn enqueue_remote_diff(&self, id: &TabId, diff: ScreenDiff) {
        for attachment in self
            .attachments
            .values()
            .filter(|attachment| attachment.kind == AttachmentKind::Remote)
        {
            attachment
                .mailbox
                .push_diff(diff.clone(), || self.screen.snapshot(id.as_str()));
        }
    }

    fn resize(&mut self, id: &TabId, size: TerminalSize) {
        self.descriptor.size = size;
        self.screen.resize(size);
        let snapshot = self.screen.snapshot(id.as_str());
        for attachment in self
            .attachments
            .values()
            .filter(|attachment| attachment.kind == AttachmentKind::Remote)
        {
            attachment.mailbox.push_snapshot(snapshot.clone());
        }
    }

    fn mark_exited(
        &mut self,
        code: Option<u32>,
        signal: Option<String>,
        requested: bool,
    ) -> Option<TabExit> {
        if self.exit_notified {
            return None;
        }
        self.exit_notified = true;
        self.descriptor.state = TabState::Exited;
        self.descriptor.input_owner = None;
        self.descriptor.focus = TabFocus::Unowned;
        let exit = TabExit {
            code,
            signal,
            requested,
        };
        self.descriptor.exit = Some(exit.clone());
        let final_snapshot = Arc::new(self.screen.snapshot(self.descriptor.id.as_str()));
        for attachment in self.attachments.values() {
            if attachment.kind == AttachmentKind::Remote {
                attachment
                    .mailbox
                    .finish_with_shared_snapshot(final_snapshot.clone(), exit.clone());
            } else {
                attachment.mailbox.finish(exit.clone());
            }
        }
        Some(exit)
    }

    fn queue_replies(&mut self, replies: Vec<Vec<u8>>, desktop_opening_owns: bool) -> Option<u32> {
        let desktop_owns = desktop_opening_owns
            || self
                .descriptor
                .input_owner
                .as_ref()
                .and_then(|owner| self.attachments.get(owner))
                .is_some_and(|attachment| attachment.kind == AttachmentKind::Desktop);
        if desktop_owns {
            return None;
        }
        self.pending_replies.extend(replies);
        if let PtyBinding::Ready(pty_id) = self.pty {
            if !self.pending_replies.is_empty() {
                self.pty = PtyBinding::Flushing(pty_id);
                return Some(pty_id);
            }
        }
        None
    }
}

struct AttachmentState {
    kind: AttachmentKind,
    mailbox: Arc<EventMailbox>,
}

struct TabSink {
    registry: Weak<RegistryInner>,
    tab: Weak<TabCell>,
}

impl PtySink for TabSink {
    fn output(&self, _pty_id: u32, bytes: &[u8]) {
        let (Some(registry), Some(tab)) = (self.registry.upgrade(), self.tab.upgrade()) else {
            return;
        };
        // Controlled/fake sinks may call output concurrently. Keep whole
        // callbacks ordered while allowing a capacity wait to happen before
        // the Task 4 transaction gate that attach/focus/close need.
        let _producer_order = tab.raw.producer_order.lock().unwrap();
        let activity = {
            let mut last = tab.last_activity.lock().unwrap();
            if last.elapsed() >= Duration::from_millis(250) {
                *last = Instant::now();
                tab.live.lock().unwrap().descriptor.session_id.clone()
            } else {
                None
            }
        };
        if let Some(session_id) = activity {
            registry.publish_activity(session_id);
        }
        for bytes in bytes.chunks(MAX_DESKTOP_RAW_CHUNK) {
            let route = tab.raw.reserve_opening();
            let _send_order = tab.raw.send_order.lock().unwrap();
            if tab.raw.is_closing() {
                return;
            }

            if tab.live.lock().unwrap().descriptor.state != TabState::Running {
                return; // dropping a reservation refunds it
            }

            let desktop_opening_owns = match route {
                OpeningRoute::Reserved(reservation) => {
                    if !reservation.enqueue(bytes.to_vec()) {
                        return;
                    }
                    true
                }
                OpeningRoute::Attached => {
                    let desktop_mailboxes = tab.live.lock().unwrap().desktop_mailboxes();
                    for mailbox in desktop_mailboxes {
                        if !mailbox.push_raw(bytes.to_vec()) || tab.raw.is_closing() {
                            return;
                        }
                    }
                    false
                }
                OpeningRoute::Closed => return,
            };

            if tab.raw.is_closing() {
                return;
            }
            let (flush, changed) = {
                let mut live = tab.live.lock().unwrap();
                if live.descriptor.state != TabState::Running {
                    return;
                }
                let damage = live.screen.process(bytes);
                if let Some(diff) = damage.diff {
                    live.enqueue_remote_diff(&live.descriptor.id.clone(), diff);
                }
                let changed = if let Some(title) = damage.title {
                    let other = live.descriptor.text_bytes().saturating_sub(live.descriptor.title.len());
                    let title = truncate_utf8(
                        &title,
                        MAX_TAB_DESCRIPTOR_TEXT_BYTES.saturating_sub(other),
                    );
                    live.descriptor.title = title.clone();
                    live.enqueue_control_all(TabEvent::Title(title));
                    Some(live.descriptor.clone())
                } else {
                    None
                };
                if damage.bell {
                    live.enqueue_control_all(TabEvent::Bell);
                }
                (
                    live.queue_replies(damage.replies, desktop_opening_owns),
                    changed,
                )
            };
            if let Some(descriptor) = changed {
                registry.publish_changed(descriptor);
            }
            if let Some(pty_id) = flush {
                flush_replies(&registry, &tab, pty_id);
            }
        }
    }

    fn preparing_exit(&self, _pty_id: u32) {
        if let Some(tab) = self.tab.upgrade() {
            tab.raw.prepare_exit();
        }
    }

    fn exited(&self, pty_id: u32, code: Option<u32>, signal: Option<&str>) {
        let (Some(registry), Some(tab)) = (self.registry.upgrade(), self.tab.upgrade()) else {
            return;
        };
        tab.raw.close();
        let _output_order = tab.raw.send_order.lock().unwrap();
        let exit = {
            let mut live = tab.live.lock().unwrap();
            live.pty = PtyBinding::Exited;
            live.pending_replies.clear();
            let id = live.descriptor.id.clone();
            let exit = live.mark_exited(code, signal.map(str::to_owned), false);
            exit.map(|exit| (id, exit, live.descriptor.clone()))
        };
        if let Some((id, exit, descriptor)) = exit {
            registry.publish_changed(descriptor);
            registry.publish_exit(&id, &exit);
        }
        registry.maps.lock().unwrap().by_pty.remove(&pty_id);
    }
}

fn flush_replies(registry: &RegistryInner, tab: &Arc<TabCell>, pty_id: u32) {
    loop {
        let reply = {
            let mut live = tab.live.lock().unwrap();
            if live.descriptor.state != TabState::Running
                || live.pty != PtyBinding::Flushing(pty_id)
            {
                return;
            }
            match live.pending_replies.pop_front() {
                Some(reply) => reply,
                None => {
                    live.pty = PtyBinding::Ready(pty_id);
                    return;
                }
            }
        };
        if registry.backend.write(pty_id, &reply).is_err() {
            let mut live = tab.live.lock().unwrap();
            if live.pty == PtyBinding::Flushing(pty_id) {
                live.pty = PtyBinding::Ready(pty_id);
            }
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct IdentityBackend;
    impl PtyBackend for IdentityBackend {
        fn spawn(&self, _: PtySpawnSpec, _: Arc<dyn PtySink>) -> Result<u32, String> {
            Ok(1)
        }
        fn write(&self, _: u32, _: &[u8]) -> Result<(), String> {
            Ok(())
        }
        fn resize(&self, _: u32, _: u16, _: u16) -> Result<(), String> {
            Ok(())
        }
        fn kill(&self, _: u32) {}
        fn pty_for_descendant(&self, _: u32) -> Option<u32> {
            None
        }
        fn child_pid(&self, _: u32) -> Option<u32> {
            Some(42)
        }
    }

    fn identity_registry() -> (TabRegistry, TabId) {
        let registry = TabRegistry::with_backend(Arc::new(IdentityBackend));
        let id = registry
            .open(
                TabLaunch::new("Codex", "old", TerminalSize::try_new(80, 24).unwrap())
                    .with_agent_id("codex")
                    .with_session_id("old")
                    .with_resumed_id("old"),
            )
            .unwrap();
        (registry, id)
    }

    #[test]
    fn process_identity_repairs_a_stale_binding_without_any_clear_input() {
        let (registry, id) = identity_registry();
        registry.refresh_codex_sessions(|pid| {
            assert_eq!(pid, 42);
            Some("actual".into())
        });
        assert_eq!(registry.get(&id).unwrap().session_id(), Some("actual"));
        assert_eq!(registry.get(&id).unwrap().slot_id(), "actual");
        assert!(!registry.has_session("old"));
        assert!(registry.has_session("actual"));
        // Unavailable/ambiguous ownership never undoes a proven binding.
        registry.refresh_codex_sessions(|_| None);
        assert_eq!(registry.get(&id).unwrap().session_id(), Some("actual"));
    }

    #[test]
    fn process_identity_does_not_overwrite_a_binding_changed_during_lookup() {
        let (registry, id) = identity_registry();
        registry.refresh_codex_sessions(|_| {
            registry.rekey_session(&id, "newer").unwrap();
            Some("stale-result".into())
        });
        assert_eq!(registry.get(&id).unwrap().session_id(), Some("newer"));
    }

    #[test]
    fn failed_desktop_channel_send_closes_the_attachment_receiver() {
        let mailbox = Arc::new(EventMailbox::new(AttachmentKind::Desktop, 1));
        let tab_id = TabId::new();
        let attachment_id = AttachmentId::new();
        let cancellation = TabAttachmentCancellation {
            mailbox: mailbox.clone(),
            registry: Weak::new(),
            tab_id: tab_id.clone(),
            attachment_id: attachment_id.clone(),
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        let events = TabEventReceiver {
            mailbox: mailbox.clone(),
            tab_id,
            attachment_id,
            cancellation,
        };
        assert!(mailbox.push_raw(vec![1, 2, 3]));

        let mut sends = 0;
        forward_desktop_events(events, |_| {
            sends += 1;
            false
        });

        assert_eq!(sends, 1);
        assert!(!mailbox.push_raw(vec![4]));
    }

    #[test]
    fn desktop_snapshot_recovery_restores_an_unexpected_exit_notice() {
        let tab_id = TabId::new();
        let exit = TabExit {
            code: Some(19),
            signal: None,
            requested: false,
        };
        let descriptor = TabDescriptor {
            id: tab_id.clone(),
            title: "ended".to_string(),
            cwd: None,
            command: None,
            session_id: None,
            resumed_id: None,
            agent_id: None,
            slot_id: "ended".to_string(),
            fresh: false,
            env_provider: None,
            env_model: None,
            size: TerminalSize::try_new(80, 24).unwrap(),
            input_owner: None,
            focus: TabFocus::Unowned,
            state: TabState::Exited,
            exit: Some(exit.clone()),
        };

        assert_eq!(
            unexpected_desktop_exits(&TabRegistryEvent::Snapshot {
                revision: 4,
                tabs: vec![descriptor],
            }),
            vec![(tab_id, exit)]
        );
    }

    #[test]
    fn dropping_a_registry_change_receiver_removes_its_idle_subscriber_entry() {
        struct NeverSpawn;

        impl PtyBackend for NeverSpawn {
            fn spawn(&self, _spec: PtySpawnSpec, _sink: Arc<dyn PtySink>) -> Result<u32, String> {
                Err("unused".to_string())
            }

            fn write(&self, _id: u32, _bytes: &[u8]) -> Result<(), String> {
                Err("unused".to_string())
            }

            fn resize(&self, _id: u32, _cols: u16, _rows: u16) -> Result<(), String> {
                Err("unused".to_string())
            }

            fn kill(&self, _id: u32) {}

            fn pty_for_descendant(&self, _pid: u32) -> Option<u32> {
                None
            }
        }

        let registry = TabRegistry::with_backend(Arc::new(NeverSpawn));
        let changes = registry.subscribe_changes();
        assert_eq!(registry.inner.maps.lock().unwrap().subscribers.len(), 1);

        drop(changes);

        assert!(registry.inner.maps.lock().unwrap().subscribers.is_empty());
    }
}
