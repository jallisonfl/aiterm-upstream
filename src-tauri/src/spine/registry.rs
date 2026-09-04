//! The spine's registry: one bounded event log per session, the adapter
//! driver that fills it, and the broadcast every consumer reads.
//!
//! Lives in Tauri managed state as `Arc<Spine>`. See `docs/architecture/spine.md` for the
//! lifecycle this implements.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tauri::{AppHandle, Manager};
use tokio::sync::broadcast;
use tokio::sync::mpsc::{self, UnboundedSender};

use super::{now_ms, Adapter, Kind, Phase, SpineEvent, ToolStatus};

/// Ring bounds, whichever comes first. 5 000 events is a long day's session;
/// 4 MB is where keeping history stops being free.
const MAX_EVENTS: usize = 5_000;
const MAX_BYTES: usize = 4 * 1024 * 1024;

/// A tail with no tab bound and nobody asking dies after this.
const INTEREST_TTL: Duration = Duration::from_secs(15 * 60);
/// How often a driver asks whether it is still wanted.
const REAP_EVERY: Duration = Duration::from_secs(60);
/// Fallback poll when no watched file moved — and the only poll the legacy
/// adapter usually gets, since opencode keeps sessions in a database with no
/// path to watch. Also the cadence of the phase verdict, which is why it is
/// a second and not two: idle has to land within about a second of a turn
/// ending, and a tick that finds nothing changed costs two `stat` calls.
const TICK: Duration = Duration::from_secs(1);
/// A transcript append is several lines and lands as several inotify events;
/// one poll per burst, not one per line.
const COALESCE: Duration = Duration::from_millis(250);
/// Deep enough that bootstrapping a long session cannot lag a phone that is
/// keeping up. Its own channel on purpose: a burst here must not push the
/// coarse `remote_api::Event`s out of theirs.
const BROADCAST_CAPACITY: usize = 1024;
/// Retry cadence for a source that is not there yet, once the fast phase
/// below has given up on it. `open_adapter` asks every backend in turn
/// until one claims the id, and codex's claim rescans its whole session
/// tree — not something to spend every 2 s on forever for a session whose
/// engine died at launch and will never write anything.
const SLOW_OPEN_RETRY: Duration = Duration::from_secs(10);
/// How long to retry at the 2 s tick before dropping to the cadence above.
/// A CLI writes its first transcript line within a second or two of
/// starting; a minute of nothing means something else is wrong.
const FAST_OPEN_RETRY_FOR: Duration = Duration::from_secs(60);

/// How long after the last write to a session's files its phase verdict can
/// still move on its own. `transcript_verdict` flips an open codex turn from
/// working to attention once the file has been quiet for 45 s, and nothing
/// writes to mark that — so the stamp gate below must not start skipping
/// until past it.
const VERDICT_SETTLES_AFTER: Duration = Duration::from_secs(60);

/// How often to look again for the files a session's phase verdict reads,
/// when the last look found none. `phase_sources` goes through `owner_in`,
/// which asks every backend in turn — not a per-tick cost now that the tick
/// is a second.
const RESOLVE_SOURCES_EVERY: Duration = Duration::from_secs(5);

/// How far back `Spine::overview` will walk one session's ring looking for
/// its last line of prose and its last tool card. Both are normally within a
/// few events of the end; this only bounds the session that has emitted
/// nothing else for a very long time.
const OVERVIEW_SCAN: usize = 400;

/// How long a `GET …/spine` waits for a just-started tail to finish reading
/// history. Answering the first call empty leaves the phone on a blank
/// screen until something else happens to move.
const BOOTSTRAP_GRACE: Duration = Duration::from_secs(2);

/// One session's log. Created by the first `push` or `ensure_tail` for that
/// id and kept after the tail stops, so a phone that comes back late still
/// gets what it missed.
struct SessionLog {
    agent: String,
    events: VecDeque<SpineEvent>,
    /// Running sum of `weight`, so bounding does not re-walk the ring.
    bytes: usize,
    next_seq: u64,
    last_interest: Instant,
    /// The driver task, while one runs. Kept so `ensure_tail` is idempotent.
    tail: Option<tauri::async_runtime::JoinHandle<()>>,
    /// The adapter opened AND reads the engine's own live source. Reported
    /// to a phone as `live`.
    live: bool,
    /// `bootstrap()` has returned; a waiting GET can stop waiting.
    ready: bool,
    /// The last phase pushed, with its detail. The terminal publishes
    /// cadence four times a second while output flows and the phase tick
    /// runs every second; without this the ring would be nothing but
    /// identical `working` events.
    last_phase: Option<(Phase, String)>,
    /// Whether a turn is open, and the `ts` of the event that said so.
    /// `None` means no adapter has ever reported a turn boundary for this
    /// session — the legacy adapter emits neither — and the cadence rule
    /// stands unchanged for it.
    turn: Option<(bool, u64)>,
    /// What a Claude Code hook last said about this session, with its own
    /// words for it. Held rather than merely pushed because the tick and
    /// the cadence bridge both recompute a verdict from scratch every time
    /// they run: without this, the second after a hook said "permission:
    /// Edit" cadence would say "working" and be believed.
    hook: Option<(Phase, String)>,
}

impl SessionLog {
    fn new(agent: &str) -> Self {
        Self {
            agent: agent.to_string(),
            events: VecDeque::new(),
            bytes: 0,
            next_seq: 1,
            last_interest: Instant::now(),
            tail: None,
            live: false,
            ready: false,
            last_phase: None,
            turn: None,
            hook: None,
        }
    }

    fn tailing(&self) -> bool {
        self.tail.as_ref().is_some_and(|h| !h.inner().is_finished())
    }
}

pub struct Spine {
    epoch: u64,
    sessions: Mutex<HashMap<String, SessionLog>>,
    /// session id → agent id. Resolving one walks every backend's session
    /// directory; a phone polling one session must not pay that twice.
    agents: Mutex<HashMap<String, String>>,
    tx: broadcast::Sender<SpineEvent>,
}

impl Default for Spine {
    fn default() -> Self {
        Self::new()
    }
}

impl Spine {
    pub fn new() -> Self {
        Self {
            epoch: now_ms(),
            sessions: Mutex::new(HashMap::new()),
            agents: Mutex::new(HashMap::new()),
            tx: broadcast::channel(BROADCAST_CAPACITY).0,
        }
    }

    /// When this registry started, in ms. A phone that sees a new epoch
    /// throws away everything it holds — the seq numbering started over.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SpineEvent> {
        self.tx.subscribe()
    }

    /// Stamp, store and broadcast one event. The only way anything enters
    /// the spine.
    pub fn push(&self, session_id: &str, agent: &str, ts: u64, kind: Kind) -> SpineEvent {
        let ev = {
            let mut sessions = self.sessions.lock().unwrap();
            let log = sessions
                .entry(session_id.to_string())
                .or_insert_with(|| SessionLog::new(agent));
            let ev = SpineEvent {
                seq: log.next_seq,
                epoch: self.epoch,
                session_id: session_id.to_string(),
                agent: agent.to_string(),
                ts,
                kind,
            };
            log.next_seq += 1;
            // The turn bracket, recorded where nothing can route around it.
            // It is what lets the phase rule below distinguish a TUI still
            // redrawing from an agent still working.
            match &ev.kind {
                Kind::TurnStarted { .. } => log.turn = Some((true, ev.ts)),
                Kind::TurnEnded { .. } => log.turn = Some((false, ev.ts)),
                _ => {}
            }
            // A reset says everything before it is gone. Keeping the old
            // events would only hand a reconnecting phone history it is
            // about to throw away — and they are the events most likely to
            // be the bulk of the ring.
            if matches!(ev.kind, Kind::Reset) {
                log.events.clear();
                log.bytes = 0;
            }
            log.bytes += weight(&ev);
            log.events.push_back(ev.clone());
            while log.events.len() > MAX_EVENTS
                || (log.bytes > MAX_BYTES && log.events.len() > 1)
            {
                if let Some(old) = log.events.pop_front() {
                    log.bytes = log.bytes.saturating_sub(weight(&old));
                }
            }
            ev
        };
        // Outside the lock: a subscriber's wake must never be able to
        // re-enter the registry while it is held.
        let _ = self.tx.send(ev.clone());
        ev
    }

    /// Everything after `after_seq` that is still in the ring. `after_seq`
    /// of 0 means all of it.
    pub fn after(&self, session_id: &str, after_seq: u64) -> Vec<SpineEvent> {
        self.sessions
            .lock()
            .unwrap()
            .get(session_id)
            .map(|log| log.events.iter().filter(|e| e.seq > after_seq).cloned().collect())
            .unwrap_or_default()
    }

    /// Every session with a log, folded to one row each — see
    /// [`super::ipc::spine_overview`], which is the only caller.
    ///
    /// The phase comes from `last_phase`, which the registry already keeps so
    /// it can dedupe pushes; only the text and the tool card need looking for,
    /// and the walk that finds them runs backwards and stops on the first of
    /// each. `OVERVIEW_SCAN` bounds the pathological case — a session that
    /// has said nothing but tool output for thousands of events.
    pub fn overview(&self) -> Vec<super::ipc::SessionOverview> {
        let sessions = self.sessions.lock().unwrap();
        sessions
            .iter()
            .map(|(id, log)| {
                let (phase, detail) = match &log.last_phase {
                    Some((p, d)) => (phase_word(*p).to_string(), d.clone()),
                    None => ("idle".to_string(), String::new()),
                };
                let (turn_open, turn_started_ts) = match log.turn {
                    Some((true, ts)) => (true, Some(ts)),
                    _ => (false, None),
                };
                let mut last_text: Option<String> = None;
                let mut last_tool: Option<super::ipc::OverviewTool> = None;
                // Status updates land AFTER the call that they are about, so a
                // backwards walk meets them first; hold them until the call
                // itself turns up and carries the title.
                let mut updates: HashMap<&str, ToolStatus> = HashMap::new();
                for ev in log.events.iter().rev().take(OVERVIEW_SCAN) {
                    match &ev.kind {
                        Kind::AgentText { text, .. } if last_text.is_none() => {
                            last_text = last_line(text);
                        }
                        Kind::ToolCallUpdate { id, status, .. } => {
                            updates.entry(id.as_str()).or_insert(*status);
                        }
                        Kind::ToolCall { id, title, status, .. } if last_tool.is_none() => {
                            let status = updates.get(id.as_str()).copied().unwrap_or(*status);
                            last_tool = Some(super::ipc::OverviewTool {
                                title: super::clip(title, 80),
                                status: status_word(status).to_string(),
                            });
                        }
                        _ => {}
                    }
                    if last_text.is_some() && last_tool.is_some() {
                        break;
                    }
                }
                super::ipc::SessionOverview {
                    session_id: id.clone(),
                    agent: log.agent.clone(),
                    phase,
                    detail,
                    turn_open,
                    turn_started_ts,
                    last_text,
                    last_tool,
                }
            })
            .collect()
    }

    /// Whether this session's adapter reads the engine's own source, as
    /// opposed to the legacy re-derivation (or nothing at all).
    pub fn is_live(&self, session_id: &str) -> bool {
        self.sessions.lock().unwrap().get(session_id).is_some_and(|l| l.live)
    }

    /// Whether the tail has finished reading history.
    pub fn is_ready(&self, session_id: &str) -> bool {
        self.sessions.lock().unwrap().get(session_id).is_some_and(|l| l.ready)
    }

    /// Whether a turn is open for this session, or `None` when no adapter
    /// has ever said. The phase rule treats `None` as "no opinion" and
    /// falls back to cadence alone.
    pub fn turn_open(&self, session_id: &str) -> Option<bool> {
        self.sessions.lock().unwrap().get(session_id).and_then(|l| l.turn).map(|(open, _)| open)
    }

    /// Remember what a hook last said about a session. Only for a session
    /// the registry already knows: a phase with no log behind it has nobody
    /// to tell, and inventing a log here would leak one per foreign claude.
    fn set_hook_phase(&self, session_id: &str, hook: Option<(Phase, String)>) {
        if let Some(log) = self.sessions.lock().unwrap().get_mut(session_id) {
            log.hook = hook;
        }
    }

    /// A hook said the turn opened or closed. The bracket, not the events:
    /// the transcript stays the only source of `turn_started` /
    /// `turn_ended`, and this only moves the gate cadence is measured
    /// against — a beat earlier than the transcript can, which is the whole
    /// difference between "idle" landing at the end of an answer and the
    /// TUI's last few repaints re-raising "working" for another second
    /// [observed live: Stop's idle undone 28 ms later by a cadence push,
    /// 2026-09-02].
    fn note_hook_turn(&self, session_id: &str, open: bool) {
        if let Some(log) = self.sessions.lock().unwrap().get_mut(session_id) {
            log.turn = Some((open, now_ms()));
        }
    }

    fn hook_phase(&self, session_id: &str) -> Option<(Phase, String)> {
        self.sessions.lock().unwrap().get(session_id).and_then(|l| l.hook.clone())
    }

    /// Whether a hook says this session is waiting on a person. Public
    /// because the sessions list feeds it to the same verdict function the
    /// spine's tick does — the phone's list and the session it opens must
    /// not disagree about a session that is holding a permission dialog.
    pub fn hook_attention(&self, session_id: &str) -> bool {
        matches!(self.hook_phase(session_id), Some((Phase::NeedsYou, _)))
    }

    /// Start the adapter driver for a session if one is not already
    /// running, and mark the session as wanted either way.
    pub fn ensure_tail(self: &Arc<Self>, app: &AppHandle, session_id: &str, agent: &str) {
        let mut sessions = self.sessions.lock().unwrap();
        let log = sessions
            .entry(session_id.to_string())
            .or_insert_with(|| SessionLog::new(agent));
        log.last_interest = Instant::now();
        if log.tailing() {
            return;
        }
        log.ready = false;
        log.tail = Some(tauri::async_runtime::spawn(drive(
            self.clone(),
            app.clone(),
            session_id.to_string(),
            agent.to_string(),
        )));
    }

    /// Record a phase for a session that already has a tail, unless it is
    /// the one already standing.
    pub fn push_phase_if_tailed(&self, session_id: &str, phase: Phase, detail: &str) {
        let agent = {
            let mut sessions = self.sessions.lock().unwrap();
            let Some(log) = sessions.get_mut(session_id) else { return };
            // Deliberately not started here: a phase with no content behind
            // it is not worth opening a transcript for.
            if !log.tailing() {
                return;
            }
            if log.last_phase.as_ref().is_some_and(|(p, d)| *p == phase && d == detail) {
                return;
            }
            log.last_phase = Some((phase, detail.to_string()));
            log.agent.clone()
        };
        self.push(session_id, &agent, now_ms(), Kind::Phase { phase, detail: detail.to_string() });
    }

    /// A running tail is what gates a phase push, and a unit test has no
    /// driver to run. This stands one in for it.
    #[cfg(test)]
    fn pretend_tailing(&self, session_id: &str, agent: &str) {
        let mut sessions = self.sessions.lock().unwrap();
        let log = sessions
            .entry(session_id.to_string())
            .or_insert_with(|| SessionLog::new(agent));
        log.tail = Some(tauri::async_runtime::spawn(std::future::pending::<()>()));
    }

    fn agent_of(&self, session_id: &str) -> Option<String> {
        self.agents.lock().unwrap().get(session_id).cloned()
    }

    fn remember_agent(&self, session_id: &str, agent: &str) {
        self.agents.lock().unwrap().insert(session_id.to_string(), agent.to_string());
    }

    fn set_flags(&self, session_id: &str, live: Option<bool>, ready: Option<bool>) {
        let mut sessions = self.sessions.lock().unwrap();
        let Some(log) = sessions.get_mut(session_id) else { return };
        if let Some(v) = live {
            log.live = v;
        }
        if let Some(v) = ready {
            log.ready = v;
        }
    }

    fn has_events(&self, session_id: &str) -> bool {
        self.sessions.lock().unwrap().get(session_id).is_some_and(|l| !l.events.is_empty())
    }

    /// A tail is wanted while a tab is bound to the session, or while
    /// somebody asked about it recently.
    fn still_wanted(&self, app: &AppHandle, session_id: &str) -> bool {
        if let Some(tabs) = app.try_state::<Arc<crate::tabs::TabRegistry>>() {
            if tabs.bound_sessions().iter().any(|s| s == session_id) {
                return true;
            }
        }
        self.sessions
            .lock()
            .unwrap()
            .get(session_id)
            .is_some_and(|l| l.last_interest.elapsed() < INTEREST_TTL)
    }
}

/// The verdict words `activity_verdict` speaks, as a `Phase`.
fn phase_of(activity: &str) -> Phase {
    match activity {
        "working" => Phase::Working,
        "attention" => Phase::NeedsYou,
        _ => Phase::Idle,
    }
}

/// A `Phase` as the wire spells it. The enum's own serde renaming would do
/// this too, but only through a serializer — and `overview` needs the word
/// itself, in a `String` field beside a detail that is already one.
fn phase_word(p: Phase) -> &'static str {
    match p {
        Phase::Working => "working",
        Phase::NeedsYou => "needs_you",
        Phase::Idle => "idle",
    }
}

/// A `ToolStatus` as the wire spells it, for the same reason.
fn status_word(s: ToolStatus) -> &'static str {
    match s {
        ToolStatus::Pending => "pending",
        ToolStatus::Running => "running",
        ToolStatus::Completed => "completed",
        ToolStatus::Failed => "failed",
        ToolStatus::Cancelled => "cancelled",
    }
}

/// The last non-blank line of a block, whitespace collapsed and clipped to a
/// row's worth. `None` for a block that is all whitespace — a row would
/// rather show nothing than an empty quotation.
fn last_line(text: &str) -> Option<String> {
    let line = text.lines().rev().find(|l| !l.trim().is_empty())?;
    let flat: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return None;
    }
    Some(super::clip(&flat, 120))
}

/// Roughly what one event costs to hold, for the byte bound. Exact JSON
/// length would mean serializing every event twice; the texts are all of
/// the size and the constant covers the envelope.
fn weight(ev: &SpineEvent) -> usize {
    let body = match &ev.kind {
        Kind::UserMessage { id, text }
        | Kind::AgentText { id, text, .. }
        | Kind::AgentThought { id, text, .. } => id.len() + text.len(),
        Kind::ToolCall { id, tool, title, input, .. } => {
            id.len() + tool.len() + title.len() + input.len()
        }
        Kind::ToolCallUpdate { id, output, .. } => {
            id.len() + output.as_ref().map_or(0, |o| o.len())
        }
        Kind::TurnStarted { turn } => turn.len(),
        Kind::TurnEnded { turn, reason } => turn.len() + reason.len(),
        Kind::Phase { detail, .. } => detail.len(),
        Kind::Reset => 0,
    };
    body + ev.session_id.len() + ev.agent.len() + 64
}

/// The agent id that owns a session, cached. `None` when no backend claims
/// it — a session id the phone made up, or a transcript that has gone.
pub async fn resolve_agent(spine: &Arc<Spine>, session_id: &str) -> Option<String> {
    if let Some(agent) = spine.agent_of(session_id) {
        return Some(agent);
    }
    let sid = session_id.to_string();
    let found = crate::run_blocking(move || {
        let list = crate::agents::backends();
        crate::agents::owner_in(&list, &sid).map(|(b, _)| b.id().to_string())
    })
    .await?;
    spine.remember_agent(session_id, &found);
    Some(found)
}

/// Start (or refresh) a tail for a session, resolving its agent first.
/// Callable from a plain thread — the tabs registry bridge runs on one and
/// has no async context of its own.
pub fn ensure_tail_for(app: &AppHandle, session_id: &str) {
    let Some(spine) = app.try_state::<Arc<Spine>>().map(|s| s.inner().clone()) else { return };
    let app = app.clone();
    let sid = session_id.to_string();
    tauri::async_runtime::spawn(async move {
        if let Some(agent) = resolve_agent(&spine, &sid).await {
            spine.ensure_tail(&app, &sid, &agent);
        }
    });
}

/// An adapter's event onto the log. A phase goes through the same dedupe
/// the verdict tick uses: grok's events.jsonl says idle at turn end and
/// the tick says it again two seconds later, and the phone does not need
/// to hear it twice. [observed: spine test grok, 2026-09-02]
fn push_from_adapter(spine: &Spine, session_id: &str, agent: &str, ts: u64, kind: Kind) {
    match kind {
        Kind::Phase { phase, detail } => spine.push_phase_if_tailed(session_id, phase, &detail),
        kind => {
            spine.push(session_id, agent, ts, kind);
        }
    }
}

/// Bridge the terminal's cadence onto the spine, for immediacy: this fires
/// the moment bytes flow, where the tick is up to a second behind.
///
/// Through the same rule as the tick, turn gate and all — otherwise a TUI
/// still repainting after `turn_ended` would re-raise Working half a second
/// after the tick correctly said Idle, which is the flap this rule exists to
/// stop. No transcript half: cadence knows no reason, and reading one here
/// would put a 256 KB tail read on the pty's output path.
pub fn push_phase(app: &AppHandle, session_id: &str, activity: &str) {
    let Some(spine) = app.try_state::<Arc<Spine>>() else { return };
    push_cadence(&spine, session_id, activity);
}

/// [`push_phase`] without the state lookup, so the rule can be exercised
/// against a bare registry.
fn push_cadence(spine: &Spine, session_id: &str, activity: &str) {
    let hook = spine.hook_phase(session_id);
    let (verdict, detail) = crate::remote_api::activity_verdict(
        Some(activity),
        None,
        spine.turn_open(session_id),
        matches!(hook, Some((Phase::NeedsYou, _))),
    );
    let phase = phase_of(verdict);
    spine.push_phase_if_tailed(session_id, phase, &with_hook_detail(phase, detail, &hook));
}

/// What a Claude Code hook said about a session, in the spine's own terms.
///
/// A hook is the harness talking about itself — it is not read off a file
/// after the fact and it is not inferred from bytes on a pty — so this
/// outranks cadence wherever the two disagree. See `hooklink::hook_verdict`
/// for which payload becomes which.
#[derive(Debug, Clone, PartialEq)]
pub enum HookPhase {
    /// A person is being waited on; the detail names what for.
    NeedsYou(String),
    /// The session is doing something; the detail names it, or is empty to
    /// say the last named thing is finished.
    Working(String),
    /// A person just submitted a prompt: working, and the turn bracket is
    /// open again a beat before the transcript's own user line says so.
    TurnOpened,
    /// Claude is waiting to be spoken to.
    Idle,
    /// The turn ended. Deliberately not a verdict of its own: `Stop` carries
    /// no reason, and a permission dialog can be up when it fires (the tool
    /// that asked was in the answer that just ended), so the verdict is
    /// recomputed the way the driver's own turn-ended tick would.
    Stopped,
}

/// A hook's word about a session, onto the spine.
///
/// No `turn_ended` is emitted here on purpose: the transcript's
/// `turn_duration` is the single source of turn events, and a second one
/// from this side would unbalance the bracket `activity_verdict` reads.
pub async fn push_hook_phase(app: &AppHandle, session_id: &str, hook: HookPhase) {
    let Some(spine) = app.try_state::<Arc<Spine>>().map(|s| s.inner().clone()) else { return };
    let (phase, detail) = match hook {
        // A permission dialog does not close a turn: the tool that asked
        // for it is part of an answer still being given.
        HookPhase::NeedsYou(detail) => (Phase::NeedsYou, detail),
        HookPhase::Working(detail) => (Phase::Working, detail),
        HookPhase::TurnOpened => {
            spine.note_hook_turn(session_id, true);
            (Phase::Working, String::new())
        }
        HookPhase::Idle => {
            spine.note_hook_turn(session_id, false);
            (Phase::Idle, String::new())
        }
        HookPhase::Stopped => {
            // The turn is over whatever the transcript's own bracket says
            // yet — its `turn_duration` line lands a moment after this — so
            // the gate is closed by hand and cadence cannot hold "working".
            // A transcript that says a person is being waited on still wins.
            spine.set_hook_phase(session_id, None);
            spine.note_hook_turn(session_id, false);
            let sid = session_id.to_string();
            let transcript =
                crate::run_blocking(move || crate::remote_api::transcript_verdict(&sid)).await;
            let (activity, detail) = crate::remote_api::activity_verdict(
                cadence_of(app, session_id).as_deref(),
                transcript,
                Some(false),
                false,
            );
            spine.push_phase_if_tailed(session_id, phase_of(activity), detail);
            return;
        }
    };
    spine.set_hook_phase(session_id, Some((phase, detail.clone())));
    spine.push_phase_if_tailed(session_id, phase, &detail);
}

/// The tab registry's cadence for one session, or `None` when no tab of
/// ours is running it.
fn cadence_of(app: &AppHandle, session_id: &str) -> Option<String> {
    app.try_state::<Arc<crate::tabs::TabRegistry>>().and_then(|tabs| {
        tabs.session_activities().into_iter().find(|(id, _)| id == session_id).map(|(_, a)| a)
    })
}

/// The detail to push, given the verdict and what a hook last said.
///
/// A hook's own words for the state — "running Bash: npm test",
/// "permission: Edit" — replace whatever the tick a second later would say
/// for the same phase, which is "" from cadence and one flat word from the
/// transcript. Without this the phone would see the tool's name for one
/// second and an unexplained "working" for the rest of the call, and the
/// permission prompt would lose the name of what it is asking about.
///
/// Only while the hook's phase is still the standing one: a hook that said
/// "running Edit" has nothing to say about a session that has since gone
/// idle.
fn with_hook_detail(phase: Phase, detail: &str, hook: &Option<(Phase, String)>) -> String {
    match hook {
        Some((hp, hd)) if *hp == phase && !hd.is_empty() => hd.clone(),
        _ => detail.to_string(),
    }
}

/// Answer `GET /v1/sessions/{id}/spine`: register interest, wait out a
/// first bootstrap, and hand back everything after `after_seq`.
pub async fn read_after(
    app: &AppHandle,
    session_id: &str,
    after_seq: u64,
) -> Option<(u64, bool, Vec<SpineEvent>)> {
    let spine = app.try_state::<Arc<Spine>>().map(|s| s.inner().clone())?;
    let agent = resolve_agent(&spine, session_id).await?;
    spine.ensure_tail(app, session_id, &agent);
    let deadline = Instant::now() + BOOTSTRAP_GRACE;
    while !spine.is_ready(session_id) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Some((spine.epoch(), spine.is_live(session_id), spine.after(session_id, after_seq)))
}

// -------------------------------------------------------------- the phase

/// The driver's phase half: works out what the session is doing every tick
/// and pushes it when it moved.
///
/// The verdict itself comes from `remote_api::activity_verdict`, the same
/// function the sessions list uses — the two cannot disagree about a
/// session. Only the transcript half is cached here: reading it is a tail
/// read of up to 256 KB per tick, and it cannot have changed while none of
/// the files it reads have.
struct PhaseGate {
    /// The files `transcript_verdict` will read for this session.
    paths: Vec<PathBuf>,
    /// When those paths were last looked for, while none have been found.
    looked: Instant,
    /// (len, mtime) of each of them at the last read.
    stamp: Option<Vec<(u64, u64)>>,
    /// What that read said, held until one of the files moves.
    transcript: Option<(&'static str, &'static str)>,
}

impl PhaseGate {
    async fn new(session_id: &str) -> Self {
        let sid = session_id.to_string();
        let paths = crate::run_blocking(move || phase_sources(&sid)).await;
        Self { paths, looked: Instant::now(), stamp: None, transcript: None }
    }

    /// Work out what the session is doing and push it if it moved. `force`
    /// skips the unchanged-files shortcut: a turn boundary just landed and
    /// the answer has to be right now, not next tick.
    async fn tick(&mut self, spine: &Spine, app: &AppHandle, session_id: &str, force: bool) {
        // A session whose transcript has not appeared yet (a tab that just
        // opened) resolves to nothing; keep asking until it does, but not
        // on every tick.
        if self.paths.is_empty() && self.looked.elapsed() > RESOLVE_SOURCES_EVERY {
            let sid = session_id.to_string();
            self.paths = crate::run_blocking(move || phase_sources(&sid)).await;
            self.looked = Instant::now();
        }
        let sid = session_id.to_string();
        let paths = self.paths.clone();
        let last = self.stamp.clone();
        let (stamp, fresh) = crate::run_blocking(move || {
            let (stamp, quiet) = stamp_of(&paths);
            if !force && stamp.is_some() && stamp == last && quiet > VERDICT_SETTLES_AFTER {
                return (stamp, None); // nothing it reads moved, and nothing will
            }
            let verdict = crate::remote_api::transcript_verdict(&sid);
            (stamp, Some(verdict))
        })
        .await;
        self.stamp = stamp;
        if let Some(verdict) = fresh {
            self.transcript = verdict;
        }
        // Cadence is read every tick either way: it is an in-memory flag,
        // and a terminal falling quiet is exactly the change the cached
        // transcript half cannot see.
        let cadence = cadence_of(app, session_id);
        let hook = spine.hook_phase(session_id);
        let (activity, detail) = crate::remote_api::activity_verdict(
            cadence.as_deref(),
            self.transcript,
            spine.turn_open(session_id),
            matches!(hook, Some((Phase::NeedsYou, _))),
        );
        let phase = phase_of(activity);
        spine.push_phase_if_tailed(session_id, phase, &with_hook_detail(phase, detail, &hook));
    }
}

/// The files whose changes can move a session's phase verdict: its
/// transcript, plus grok's `events.jsonl` beside it — `transcript_verdict`
/// prefers that file's explicit turn and permission records over the
/// inference it falls back to.
fn phase_sources(session_id: &str) -> Vec<PathBuf> {
    let list = crate::agents::backends();
    let Some((_, path)) = crate::agents::owner_in(&list, session_id) else { return Vec::new() };
    let mut out = Vec::new();
    if let Some(dir) = path.parent().filter(|d| d.file_name().is_some_and(|n| n == session_id)) {
        out.push(dir.join("events.jsonl"));
    }
    // agy's permission dialogs never reach its transcript — the only record
    // is a line in its own log, so the tick has to notice that moving too.
    // agy writes to it constantly, which means an agy session's verdict is
    // effectively never skipped while one is running. Its transcript is a
    // few KB, so that read is cheap; see `antigravity_confirmation_after`.
    if path.to_string_lossy().contains("/antigravity-cli/brain/") {
        out.extend(crate::remote_api::antigravity_log_path());
    }
    out.push(path);
    out
}

/// (len, mtime seconds) for each path that exists, and how long ago the most
/// recent of them was written. `None` when none of them do.
fn stamp_of(paths: &[PathBuf]) -> (Option<Vec<(u64, u64)>>, Duration) {
    let mut out = Vec::new();
    let mut newest = Duration::MAX;
    for path in paths {
        let Ok(meta) = std::fs::metadata(path) else { continue };
        let modified = meta.modified().ok();
        let secs = modified
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Some(age) = modified.and_then(|t| t.elapsed().ok()) {
            newest = newest.min(age);
        }
        out.push((meta.len(), secs));
    }
    if out.is_empty() {
        (None, Duration::ZERO)
    } else {
        (Some(out), newest)
    }
}

// ------------------------------------------------------------- the driver

/// Open the session's adapter, waiting for its source to exist.
///
/// A session launched a moment ago has an id and a bound tab before its
/// engine has written anything, and every `open` resolves through the files
/// on disk — claude's through `owner_in`, grok's through its session
/// directory — so it can only succeed once the first one is there. Retrying
/// is the whole mechanism: there is no path to watch before the adapter
/// exists to name one.
///
/// Nothing is pushed and `live` stays false until it opens. `ready` is set
/// after the first failure so a `GET …/spine` on a session with nothing to
/// show yet answers at once instead of sitting out its bootstrap grace.
///
/// `None` when the session was reaped while waiting.
async fn open_when_it_exists(
    spine: &Arc<Spine>,
    app: &AppHandle,
    session_id: &str,
    agent: &str,
) -> Option<Box<dyn Adapter>> {
    let short = short(session_id);
    let mut tick = tokio::time::interval(TICK);
    let mut slow = tokio::time::interval(SLOW_OPEN_RETRY);
    // The slow interval goes unpolled for the whole fast minute; on Burst
    // (the default) it would then fire its missed ticks back to back and
    // retry six times in a row before settling.
    slow.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut reap = tokio::time::interval(REAP_EVERY);
    reap.tick().await;
    let waiting_since = Instant::now();
    let mut tries: u32 = 0;
    loop {
        let (a, s) = (agent.to_string(), session_id.to_string());
        if let Some(adapter) = crate::run_blocking(move || super::open_adapter(&a, &s)).await {
            if tries > 0 {
                crate::diag!(
                    "spine",
                    "{agent} source for {short} appeared after {}s",
                    waiting_since.elapsed().as_secs()
                );
            }
            return Some(adapter);
        }
        if tries == 0 {
            crate::diag!("spine", "{agent} has no source for {short} yet; waiting for one");
            // Not live, and nothing to hand back — but do not make a phone
            // wait out the grace period to be told so.
            spine.set_flags(session_id, Some(false), Some(true));
        }
        tries = tries.saturating_add(1);
        let fast = waiting_since.elapsed() < FAST_OPEN_RETRY_FOR;
        tokio::select! {
            _ = tick.tick(), if fast => {}
            _ = slow.tick(), if !fast => {}
            _ = reap.tick() => {
                if !spine.still_wanted(app, session_id) {
                    crate::diag!("spine", "gave up waiting for {agent} source for {short}");
                    return None;
                }
            }
        }
    }
}

/// One tokio task per tailed session: open the adapter, replay history,
/// then poll on every change of a watched file and on the fallback tick
/// until nobody wants this session any more.
async fn drive(spine: Arc<Spine>, app: AppHandle, session_id: String, agent: String) {
    let short = short(&session_id);
    let Some(adapter) = open_when_it_exists(&spine, &app, &session_id, &agent).await else {
        return; // reaped while waiting for a source that never appeared
    };
    spine.set_flags(&session_id, Some(super::is_native(&agent)), None);

    // A tail that ran before and was reaped left history a phone may still
    // hold; bootstrapping again would append every user message twice. Say
    // the history was rebuilt and let it start over.
    if spine.has_events(&session_id) {
        spine.push(&session_id, &agent, now_ms(), Kind::Reset);
    }

    let (mut adapter, history) = step(adapter, |a| a.bootstrap()).await;
    let count = history.len();
    for (ts, kind) in history {
        push_from_adapter(&spine, &session_id, &agent, ts, kind);
    }
    spine.set_flags(&session_id, None, Some(true));
    crate::diag!("spine", "tail up for {agent} {short}: {count} events from history");

    // Kept alive alongside the watcher so `fs.recv()` pends forever rather
    // than resolving immediately when there is nothing to watch.
    let (tx, mut fs) = mpsc::unbounded_channel::<()>();
    let _keepalive = tx.clone();
    let _watcher = spawn_watch(&adapter.watch_paths(), tx);

    let mut tick = tokio::time::interval(TICK);
    tick.tick().await; // the first tick is immediate; skip it
    // A slow poll (a big transcript, a busy blocking pool) must not be paid
    // back with a burst of catch-up polls the moment it returns.
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut reap = tokio::time::interval(REAP_EVERY);
    reap.tick().await;
    let mut phase = PhaseGate::new(&session_id).await;
    phase.tick(&spine, &app, &session_id, true).await;

    loop {
        tokio::select! {
            _ = fs.recv() => {
                // Fold the rest of the burst into this one poll.
                while tokio::time::timeout(COALESCE, fs.recv()).await.is_ok() {}
            }
            _ = tick.tick() => {}
            _ = reap.tick() => {
                if !spine.still_wanted(&app, &session_id) {
                    break;
                }
                continue;
            }
        }
        let (a, events) = step(adapter, |a| a.poll()).await;
        adapter = a;
        // A turn opening or closing settles the phase on its own — the
        // verdict is recomputed now rather than on the next tick, so idle
        // lands within the poll that saw `turn_ended` instead of up to a
        // second later.
        let boundary = events
            .iter()
            .any(|(_, k)| matches!(k, Kind::TurnStarted { .. } | Kind::TurnEnded { .. }));
        for (ts, kind) in events {
            // A Reset from the adapter is pushed like anything else; the
            // ring drops what came before it and the phone re-fetches.
            push_from_adapter(&spine, &session_id, &agent, ts, kind);
        }
        // After the content, so a phase never claims a turn ended before
        // the text of it is in the log.
        phase.tick(&spine, &app, &session_id, boundary).await;
    }
    crate::diag!("spine", "tail stopped for {short}: no tab bound and no interest");
}

/// Run one adapter call on the blocking pool. Adapters are synchronous and
/// read files, so none of it belongs on an async worker; the adapter is
/// moved in and handed back because it is `Send` but not `Sync`.
async fn step<F>(mut adapter: Box<dyn Adapter>, f: F) -> (Box<dyn Adapter>, Vec<(u64, Kind)>)
where
    F: FnOnce(&mut Box<dyn Adapter>) -> Vec<(u64, Kind)> + Send + 'static,
{
    crate::run_blocking(move || {
        let out = f(&mut adapter);
        (adapter, out)
    })
    .await
}

/// Watch the directories holding `paths` and ping `tx` when one of those
/// files moves. Directories rather than the files themselves because a
/// transcript is often replaced rather than appended (a `/clear` writes a
/// new file), and an inotify watch on the old inode would go quiet.
fn spawn_watch(paths: &[PathBuf], tx: UnboundedSender<()>) -> Option<RecommendedWatcher> {
    if paths.is_empty() {
        return None;
    }
    let wanted: HashSet<PathBuf> = paths.iter().cloned().collect();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            if ev.paths.iter().any(|p| wanted.contains(p)) {
                let _ = tx.send(());
            }
        }
    })
    .ok()?;
    let dirs: HashSet<PathBuf> =
        paths.iter().filter_map(|p| p.parent().map(PathBuf::from)).collect();
    let mut armed = false;
    for dir in dirs {
        if watcher.watch(&dir, RecursiveMode::NonRecursive).is_ok() {
            armed = true;
        }
    }
    armed.then_some(watcher)
}

/// Session ids are uuids; the first eight characters identify one in a log
/// line without making the line unreadable.
fn short(session_id: &str) -> &str {
    &session_id[..8.min(session_id.len())]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spine::{ToolCategory, ToolStatus};

    fn text(id: &str, body: &str) -> Kind {
        Kind::AgentText { id: id.into(), text: body.into(), done: true }
    }

    #[test]
    fn seq_starts_at_one_and_after_returns_only_what_follows() {
        let spine = Spine::new();
        for i in 0..5 {
            spine.push("s1", "claude", 100 + i, text(&format!("b{i}"), "hi"));
        }
        let all = spine.after("s1", 0);
        assert_eq!(all.len(), 5);
        assert_eq!(all.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2, 3, 4, 5]);
        assert!(all.iter().all(|e| e.epoch == spine.epoch()));
        assert_eq!(all[0].ts, 100);

        let tail = spine.after("s1", 3);
        assert_eq!(tail.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![4, 5]);
        assert!(spine.after("s1", 5).is_empty());
        // Sessions do not share a sequence.
        assert_eq!(spine.push("s2", "codex", 1, text("x", "y")).seq, 1);
        assert!(spine.after("nobody", 0).is_empty());
    }

    #[test]
    fn the_ring_drops_the_oldest_past_the_event_bound() {
        let spine = Spine::new();
        for i in 0..(MAX_EVENTS + 20) {
            spine.push("s", "claude", 0, text(&format!("b{i}"), "x"));
        }
        let held = spine.after("s", 0);
        assert_eq!(held.len(), MAX_EVENTS);
        // Seq keeps counting; only the storage is bounded.
        assert_eq!(held.first().unwrap().seq, 21);
        assert_eq!(held.last().unwrap().seq, (MAX_EVENTS + 20) as u64);
    }

    #[test]
    fn the_ring_drops_the_oldest_past_the_byte_bound() {
        let spine = Spine::new();
        let big = "x".repeat(512 * 1024);
        for i in 0..12 {
            spine.push("s", "claude", 0, text(&format!("b{i}"), &big));
        }
        let held = spine.after("s", 0);
        assert!(held.len() < 12, "byte bound never fired: {} events held", held.len());
        assert!(held.iter().map(weight).sum::<usize>() <= MAX_BYTES + big.len());
        assert_eq!(held.last().unwrap().seq, 12);
    }

    #[test]
    fn a_reset_clears_the_history_before_it() {
        let spine = Spine::new();
        spine.push("s", "codex", 1, text("a", "one"));
        spine.push("s", "codex", 2, text("b", "two"));
        spine.push("s", "codex", 3, Kind::Reset);
        spine.push("s", "codex", 4, text("a", "one again"));
        let held = spine.after("s", 0);
        // The reset survives — a phone catching up on `after=1` has to see
        // it to know to drop what it holds.
        assert_eq!(held.len(), 2);
        assert_eq!(held[0].kind, Kind::Reset);
        assert_eq!(held[0].seq, 3);
    }

    /// The turn bracket is recorded by `push` itself, so no route into the
    /// log can skip it. A session no adapter has spoken for has no opinion.
    #[test]
    fn a_turn_bracket_is_recorded_wherever_it_enters() {
        let spine = Spine::new();
        assert_eq!(spine.turn_open("s"), None);
        spine.push("s", "claude", 1, text("a", "hi"));
        assert_eq!(spine.turn_open("s"), None, "content says nothing about turns");
        spine.push("s", "claude", 2, Kind::TurnStarted { turn: "t1".into() });
        assert_eq!(spine.turn_open("s"), Some(true));
        spine.push("s", "claude", 3, Kind::TurnEnded { turn: "t1".into(), reason: "completed".into() });
        assert_eq!(spine.turn_open("s"), Some(false));
        spine.push("s", "claude", 4, Kind::TurnStarted { turn: "t2".into() });
        assert_eq!(spine.turn_open("s"), Some(true));
    }

    fn phases(spine: &Spine, session_id: &str) -> Vec<(Phase, String)> {
        spine
            .after(session_id, 0)
            .into_iter()
            .filter_map(|e| match e.kind {
                Kind::Phase { phase, detail } => Some((phase, detail)),
                _ => None,
            })
            .collect()
    }

    /// The bug this rule exists for: Claude's TUI goes on repainting after
    /// the answer is finished, so cadence held "working" for the ten
    /// seconds `session_activities` counts as recent and the phone's header
    /// stayed busy long after the turn visibly ended.
    #[test]
    fn idle_lands_with_the_turn_ended_and_later_bytes_do_not_undo_it() {
        let spine = Spine::new();
        spine.pretend_tailing("s", "claude");
        spine.push("s", "claude", 1, Kind::TurnStarted { turn: "t1".into() });
        push_cadence(&spine, "s", "output");
        assert_eq!(phases(&spine, "s"), vec![(Phase::Working, String::new())]);

        // The adapter sees the turn close. The very next cadence push — a
        // spinner clearing, half a second later — must not raise Working.
        spine.push("s", "claude", 2, Kind::TurnEnded { turn: "t1".into(), reason: "completed".into() });
        push_cadence(&spine, "s", "output");
        assert_eq!(
            phases(&spine, "s"),
            vec![(Phase::Working, String::new()), (Phase::Idle, String::new())]
        );

        // And it stays put however many more repaints arrive.
        for _ in 0..5 {
            push_cadence(&spine, "s", "output");
        }
        assert_eq!(phases(&spine, "s").len(), 2, "a repainting TUI must not flap the header");
    }

    /// The gate is a gate, not a latch: the next turn opens it again, and
    /// the adapter sees the user's line within a poll of it being written.
    #[test]
    fn a_new_turn_re_opens_working() {
        let spine = Spine::new();
        spine.pretend_tailing("s", "claude");
        spine.push("s", "claude", 1, Kind::TurnEnded { turn: "t1".into(), reason: "completed".into() });
        push_cadence(&spine, "s", "output");
        spine.push("s", "claude", 2, Kind::TurnStarted { turn: "t2".into() });
        push_cadence(&spine, "s", "output");
        assert_eq!(
            phases(&spine, "s"),
            vec![(Phase::Idle, String::new()), (Phase::Working, String::new())]
        );
    }

    /// A legacy-adapter session reports no turns at all, and must keep the
    /// cadence rule it had before any of this.
    #[test]
    fn a_session_with_no_turn_events_still_works_off_cadence() {
        let spine = Spine::new();
        spine.pretend_tailing("s", "codex");
        push_cadence(&spine, "s", "output");
        assert_eq!(phases(&spine, "s"), vec![(Phase::Working, String::new())]);
    }

    /// A hook's needs-you survives the cadence pushes that follow it. The
    /// case: claude's TUI redraws its own permission dialog, so bytes keep
    /// flowing at four pushes a second for as long as the person takes to
    /// answer, and every one of them would otherwise say "working".
    #[test]
    fn a_hook_permission_prompt_is_not_flapped_away_by_a_repainting_dialog() {
        let spine = Spine::new();
        spine.pretend_tailing("s", "claude");
        spine.push("s", "claude", 1, Kind::TurnStarted { turn: "t1".into() });
        push_cadence(&spine, "s", "output");

        // The hook fires: the dialog is up.
        spine.set_hook_phase("s", Some((Phase::NeedsYou, "permission: Edit".into())));
        spine.push_phase_if_tailed("s", Phase::NeedsYou, "permission: Edit");
        for _ in 0..8 {
            push_cadence(&spine, "s", "output");
        }
        assert_eq!(
            phases(&spine, "s"),
            vec![
                (Phase::Working, String::new()),
                (Phase::NeedsYou, "permission: Edit".to_string()),
            ],
            "cadence must not demote a hook's needs-you, and must not repeat it"
        );

        // Answered: the tool runs, the hook says so, and cadence is
        // believed again.
        spine.set_hook_phase("s", Some((Phase::Working, "running Edit: src/main.rs".into())));
        spine.push_phase_if_tailed("s", Phase::Working, "running Edit: src/main.rs");
        push_cadence(&spine, "s", "output");
        assert_eq!(
            phases(&spine, "s").last().unwrap(),
            &(Phase::Working, "running Edit: src/main.rs".to_string()),
            "the tick behind it keeps the hook's own words rather than blanking them"
        );
    }

    /// What a hook said about a tool call outlives the tick that follows it,
    /// but only while its phase is still the standing one.
    #[test]
    fn a_hooks_detail_survives_a_tick_with_nothing_to_say() {
        let working = Some((Phase::Working, "running Bash: npm test".to_string()));
        assert_eq!(with_hook_detail(Phase::Working, "", &working), "running Bash: npm test");
        // And it outranks the one flat word the other sources have for the
        // same phase: "permission: Edit" says more than "permission".
        assert_eq!(with_hook_detail(Phase::Working, "busy", &working), "running Bash: npm test");
        // A different phase is a different state; the hook's words do not
        // follow it there.
        assert_eq!(with_hook_detail(Phase::Idle, "", &working), "");
        assert_eq!(with_hook_detail(Phase::NeedsYou, "", &working), "");
        assert_eq!(with_hook_detail(Phase::Working, "", &None), "");
        // PostToolUse clears the detail by saying nothing.
        let cleared = Some((Phase::Working, String::new()));
        assert_eq!(with_hook_detail(Phase::Working, "", &cleared), "");
    }

    /// The other half of the same rule: a `Stop` hook lands before the
    /// transcript's `turn_duration` line does, and the cadence pushes in
    /// between must not re-raise Working. Without the bracket moving, the
    /// idle `Stop` pushed was undone 28 ms later by the TUI's next repaint
    /// [observed live, 2026-09-02].
    #[test]
    fn a_stop_hook_closes_the_gate_the_transcript_has_not_reached_yet() {
        let spine = Spine::new();
        spine.pretend_tailing("s", "claude");
        spine.push("s", "claude", 1, Kind::TurnStarted { turn: "t1".into() });
        push_cadence(&spine, "s", "output");
        assert_eq!(phases(&spine, "s"), vec![(Phase::Working, String::new())]);

        // Stop fires. The transcript's own turn_ended is still a poll away.
        spine.note_hook_turn("s", false);
        spine.push_phase_if_tailed("s", Phase::Idle, "");
        for _ in 0..4 {
            push_cadence(&spine, "s", "output");
        }
        assert_eq!(
            phases(&spine, "s"),
            vec![(Phase::Working, String::new()), (Phase::Idle, String::new())]
        );

        // And the next prompt re-opens it before the transcript's user line
        // has been written, so the phone does not wait a second to go busy.
        spine.note_hook_turn("s", true);
        push_cadence(&spine, "s", "output");
        assert_eq!(phases(&spine, "s").last().unwrap(), &(Phase::Working, String::new()));
    }

    /// A hook only ever speaks about a session the registry already knows:
    /// a foreign claude launched with our settings file must not leave a
    /// log behind for every tool it runs.
    #[test]
    fn a_hook_for_an_unknown_session_leaves_nothing_behind() {
        let spine = Spine::new();
        spine.set_hook_phase("nobody", Some((Phase::NeedsYou, "permission: Bash".into())));
        spine.note_hook_turn("nobody", false);
        assert!(!spine.hook_attention("nobody"));
        assert_eq!(spine.hook_phase("nobody"), None);
        assert_eq!(spine.turn_open("nobody"), None);
        assert!(spine.sessions.lock().unwrap().is_empty());
    }

    #[test]
    fn subscribers_see_every_push_in_order() {
        let spine = Spine::new();
        let mut rx = spine.subscribe();
        spine.push("s", "grok", 7, text("a", "one"));
        spine.push(
            "s",
            "grok",
            8,
            Kind::ToolCall {
                id: "t1".into(),
                tool: "Bash".into(),
                title: "Bash".into(),
                category: ToolCategory::Execute,
                input: "ls".into(),
                status: ToolStatus::Completed,
            },
        );
        assert_eq!(rx.try_recv().unwrap().seq, 1);
        let second = rx.try_recv().unwrap();
        assert_eq!(second.seq, 2);
        assert_eq!(second.agent, "grok");
    }
}


