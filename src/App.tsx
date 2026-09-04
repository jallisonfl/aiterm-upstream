import { TimeFormatContext, fullTime } from "./timefmt";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow, UserAttentionType } from "@tauri-apps/api/window";
import SessionsPanel, { SessionDisplayOpts } from "./components/SessionsPanel";
import { StartChoice } from "./components/NewSessionMenu";
import StartControls, { describePickers, useStartChoice } from "./components/StartControls";
import TerminalView, { TermHandle, TermProgress, TermTab } from "./components/TerminalView";
import AlertBell, { Alert } from "./components/AlertBell";
import FileExplorer from "./components/FileExplorer";
import GitPanel from "./components/GitPanel";
import Composer from "./components/Composer";
import TuiModelConfirm from "./components/TuiModelConfirm";
import TuiModelPicker from "./components/TuiModelPicker";
import TuiPermission from "./components/TuiPermission";
import TuiRewind from "./components/TuiRewind";
import {
  Detected, PermissionMode, detect, detectAgentsView, detectPermissionMode,
} from "./term/screen";
import { cycleModeTo } from "./term/drive";
import AgentPanel from "./components/AgentPanel";
import FileView from "./components/FileView";
import PdfView, { isPdf } from "./components/PdfView";
import AgentIcon from "./components/AgentIcon";
import Icon from "./components/Icon";
import HomeDashboard from "./components/HomeDashboard";
import LiveLanes from "./components/LiveLanes";
import CommandPalette from "./components/CommandPalette";
import type { PaletteItem } from "./palette";
import { useSpineOverview } from "./useSpineOverview";
import { buildFleet } from "./fleet";
import { useLibrarian } from "./librarian";
import BringIn from "./components/BringIn";
import { engineName, useRelay } from "./relay";
import {
  Command, FolderOpen, GitBranch, Home, Keyboard, ListChecks, PanelLeft, RefreshCw, RotateCcw, Settings as SettingsIcon, Users, X,
} from "lucide-react";
import { agentTint } from "./brand";
import SettingsModal, { SettingsTab } from "./components/SettingsModal";
import SessionPreview from "./components/SessionPreview";
import { UsagePanel, UsageSourceAt, mergeUsage } from "./components/UsagePanel";
import { Clock } from "./components/Clock";
import {
  AppSettings, applySettings, loadSettings, saveSettings, termFontFamily, termTheme,
} from "./settings";
import {
  Caps,
  ProjectInfo, Session,
  TrashedSession,
  agentCaps, homeAbbrev,
  listProjects, listSessions, materializeFork, relayReport,
  reindexSessions, sessionFork, uiLog, usageReport,
  resolveResumableId, liveSessionIds, stopSession, unstoppableSessionIds, sessionMovedTo,
  drainSessionEvents,
  sessionDelete, trashDelete, trashEmpty, trashList, trashRestore,
  watchProject,
  adoptAgentSession, clearSuccessorSession, resolveLaunch,
  taskbarBadge,
  trayAlerts,
  desktopNotify, desktopNotifyClose,
  Refusal, sessionRefusal, opencodeDispatch, opencodeDefaultTarget,
  claudeModelDefault, restoreClaudeModelDefault, sessionPreview,
  TabDescriptor, TabId, TabRegistryEvent, tabClose, tabList, tabOpen,
  tabRegistrySnapshot, tabUpdate,
} from "./ipc";
import { createTabRegistryRecovery, reconcileTabs } from "./tabModel";
import RefusalBanner from "./components/RefusalBanner";
import { nextAdoptionDelay } from "./adoption";
import "./App.css";

const OPTS_KEY = "aiterm.sessionOpts";
const SIZES_KEY = "aiterm.panelSizes";
const FONT_KEY = "aiterm.fontScale";
const PANELS_KEY = "aiterm.panelToggles";
const USAGE_KEY = "aiterm.usageReport";
// The old cache held Anthropic bars only, under a different shape. Drop it
// rather than teach the loader to read a format nothing writes any more.
localStorage.removeItem("aiterm.usageCache");

// (The constant that used to sit here — claude's own flags, spelled out in the
// renderer as a fallback for the backend's answer — is gone. Every command a
// tab runs now comes from `resolveLaunch`, which is the one place that knows
// what any engine's command line looks like.)

/** What an engine that is not in the registry can do: nothing.
 *
 *  The honest answer for a session row whose engine has since been removed, and
 *  for a tab with no engine at all (a plain shell). Both used to fall through
 *  to claude's affordances by default, which offered a ▶ that could only open a
 *  black pane. */
const NO_CAPS: Caps = {
  fork: false, clear: false, resume: false, tui_drive: false, panels: false,
  tasks: false, delete: false, config: false,
  // No roster knows about an engine aiterm does not know about either, so its
  // tab is the only sign of life — the same answer every non-claude engine gets.
  roster_liveness: false,
};

interface PanelToggles {
  sessions: boolean;
  explorer: boolean;
  git: boolean;
  composer: boolean;
  agent: boolean;
}
// Composer starts closed: it is opt-in chrome, not something to force on a
// first run. Everything else matches how the app has always opened.
/** One panel's worth of right-hand column — what home leaves Repository. */
const HOME_RIGHT_WIDTH = 320;

const DEFAULT_PANELS: PanelToggles = {
  sessions: true, explorer: true, git: true, composer: false, agent: true,
};

// Sessions used to be hidden when a heuristic decided a newly-appeared one
// "superseded" them. That's gone — the list shows what is on disk, always —
// but the hidden set was persisted, so drop it once or those rows stay
// invisible forever in an app that no longer has a way to bring them back.
localStorage.removeItem("aiterm.superseded");

interface PanelSizes {
  left: number;
  right: number;
  explorerFrac: number;
  /** Height fraction of the right column taken by the agent (tasks) panel. */
  agentFrac: number;
}

/** Why a terminal's process is gone. Both halves are needed: portable-pty
 *  reports `code: 1` for *every* signal death, so the code alone cannot tell a
 *  SIGKILL from a plain `exit 1`. */
interface EndedWhy {
  code: number | null;
  signal: string | null;
}

/** Say how a process ended without inventing a reason it didn't have.
 *
 *  The signal comes first when there is one, because it is the true cause and
 *  the accompanying code is a placeholder — a SIGKILLed shell reporting
 *  "exited with status 1" reads as a program that failed, and sends you
 *  looking for a bug instead of for whoever killed it. */
function describeEnd(why: EndedWhy | undefined): string {
  if (!why) return "";
  if (why.signal) return `The process was stopped (${why.signal}).`;
  if (why.code === null) return "The process is gone and its exit status could not be read.";
  return `The process exited with status ${why.code}.`;
}

/** Everything about a tab past "what it is and what it runs".
 *
 *  One object rather than a tail of positional arguments: these are all
 *  optional and mostly unrelated, so the call sites that set the last one were
 *  spelling `undefined` three times to get to it. */
/** A file open in the center strip, pinned to the terminal tab it was
 *  opened beside — switching sessions switches to that session's files. */
/** The owner of a file tab: a terminal key, a preview, or home (null). */
type FileScope = TabId | null;

interface FileTab {
  key: number;
  /** What this file belongs to — see `fileScope`: a terminal tab's key, a
   *  `preview:<id>` string for a file opened off a session preview, or null
   *  for one opened from the home view. */
  termKey: FileScope;
  path: string;
}

interface OpenTabOpts {
  sessionId?: string;
  fresh?: boolean;
  adopt?: TermTab["adopt"];
  envProvider?: string;
  envModel?: string;
  /** Which engine this tab runs — `LaunchPlan.agent_id`. Omitted for a plain
   *  shell, which runs none. See `TermTab.agentId`. */
  agentId?: string;
  /** The session this tab was opened to reopen — see `TermTab.resumedId`. */
  resumedId?: string;
  /** See `TermTab.parentKey`. */
  parentKey?: TabId;
}

/** A second agent brought into a session, remembered by the session's id
 *  so it can be reopened from that session's row after its tab is closed —
 *  or after the session itself is resumed another day. */
interface BroughtIn {
  sessionId: string;
  agentId: string;
  title: string;
  at: number;
}
const BROUGHT_KEY = "aiterm.broughtIn";

function reconcileTermTabs(
  current: TermTab[], authoritative: TabDescriptor[],
): TermTab[] {
  const projected = reconcileTabs(
    current.map((tab) => ({ ...tab, id: tab.key })),
    authoritative,
  );
  return projected.map(({ id, ...tab }) => ({ ...tab, key: id }));
}

function loadJSON<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    return raw ? { ...fallback, ...JSON.parse(raw) } : fallback;
  } catch {
    return fallback;
  }
}

/** The usage reading from the last run, so a cold start has something to show.
 *
 *  Not `loadJSON`: this one is an array, and that helper's object spread would
 *  hand back `{0: …, 1: …}`. Everything it loads comes back stale by
 *  definition — the numbers were true when they were written, and the panel
 *  says how long ago that was until a live read replaces them. */
function loadUsageCache(): UsageSourceAt[] {
  try {
    const raw = localStorage.getItem(USAGE_KEY);
    const v = raw ? JSON.parse(raw) : null;
    if (!Array.isArray(v)) return [];
    return v.map((s: UsageSourceAt) => ({ ...s, stale: true, failed: s.failed ?? "" }));
  } catch {
    return [];
  }
}

export default function App() {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [activeProject, setActiveProject] = useState<string | null>(null);
  // Transient bottom toast (e.g. a resume with nothing resumable left).
  const [notice, setNotice] = useState<string | null>(null);
  useEffect(() => {
    if (!notice) return;
    const t = setTimeout(() => setNotice(null), 6000);
    return () => clearTimeout(t);
  }, [notice]);
  const [tabs, setTabs] = useState<TermTab[]>([]);
  const [activeTab, setActiveTab] = useState<TabId | null>(null);
  const [previewSession, setPreviewSession] = useState<Session | null>(null);
  const nextFileKey = useRef(1);
  const [gitRefresh, setGitRefresh] = useState(0);
  const [explorerRefresh, setExplorerRefresh] = useState(0);
  // Center file tabs — the browser-tab strip beside the (locked) terminal.
  const [fileTabs, setFileTabs] = useState<FileTab[]>([]);
  /** Key of the file tab on screen; null = the terminal. Reset on terminal
   *  switch: arriving at a session shows the session. */
  const [activeFileTab, setActiveFileTab] = useState<number | null>(null);
  const [dirtyFiles, setDirtyFiles] = useState<Set<number>>(new Set());
  /** File tab whose × was clicked once while dirty — a second click within
   *  the window discards. An inline arm instead of a blocking dialog. */
  const [fileCloseArm, setFileCloseArm] = useState<number | null>(null);
  const fileCloseTimer = useRef<number | null>(null);

  // Tabs whose terminal rang the bell while not being looked at.
  const [attention, setAttention] = useState<Set<TabId>>(new Set());
  /** The message behind a badge, when the program sent one (OSC 9). */
  const [notices, setNotices] = useState<Map<TabId, string>>(new Map());
  /** Long-running work a tab is reporting (OSC 9;4). */
  const [progress, setProgress] = useState<Map<TabId, TermProgress>>(new Map());
  /** When each waiting tab started waiting — the bell lists oldest first, and
   *  "waiting 20m" is the part that decides which one to open. */
  const [alertAt, setAlertAt] = useState<Map<TabId, number>>(new Map());
  // Tabs whose process died on its own, and the exit code it died with. These
  // keep their row: the tab going away used to be the *only* sign anything had
  // happened, which is no help at all when the thing that killed it was
  // somewhere else entirely — `claude agents` in another terminal, or a phone.
  const [ended, setEnded] = useState<Map<TabId, EndedWhy>>(new Map());
  const activeTabRef = useRef<TabId | null>(null);
  // Latest tabs, read without putting `tabs` in an effect's deps (which would
  // re-run it on every tab change).
  const tabsRef = useRef<TermTab[]>(tabs);
  tabsRef.current = tabs;
  const tabRegistryProjection = useRef<{
    revision: number | null;
    tabs: TabDescriptor[];
  }>({ revision: null, tabs: [] });
  const appAlive = useRef(true);
  const pendingOpens = useRef(new Map<string, { cancelled: boolean }>());
  useEffect(() => {
    // Re-armed in the body, not only initialized by the ref: a dev hot
    // reload re-runs cleanup + effect on the same instance, and a ref left
    // false by that cleanup silently closed every tab opened afterwards —
    // the 11ms open-then-close that made resume look dead.
    appAlive.current = true;
    return () => {
      appAlive.current = false;
      for (const pending of pendingOpens.current.values()) pending.cancelled = true;
    };
  }, []);
  // Codex changes its session id in-place for `/clear` but has no hook that
  // reports that transition. A terminal's submitted command is the missing
  // provenance: only a tab that explicitly sent `/clear` may adopt a newly
  // written session row.
  const clearIntents = useRef(new Map<TabId, {
    previousId: string;
    cwd: string;
    since: number;
    known: string[];
    inFlight: boolean;
  }>());
  const [clearIntentRevision, setClearIntentRevision] = useState(0);

  // The tab on screen and the session it is keyed to. Read this high up
  // because everything claude-shaped below gates on which engine is in it.
  const activeTabObj = tabs.find((t) => t.key === activeTab) ?? null;
  const activeSessionId = activeTabObj?.sessionId ?? null;
  /** The session tab in front, seen from the strip: a brought-in agent's
   *  tab belongs to its master's row, so the master is what the first row
   *  highlights and what files and bring-in controls key to. */
  const rootKey: TabId | null = activeTab === null ? null : (activeTabObj?.parentKey ?? activeTab);
  const rootObj = tabs.find((t) => t.key === rootKey) ?? null;
  const [broughtIn, setBroughtIn] = useState<Record<string, BroughtIn[]>>(() => loadJSON(BROUGHT_KEY, {}));
  useEffect(() => localStorage.setItem(BROUGHT_KEY, JSON.stringify(broughtIn)), [broughtIn]);

  // What each engine supports, fetched once on mount. `agent_caps` asks the
  // registry and nothing else — no PATH probe, no `--version` — because these
  // gate what is *drawn*: which buttons a row offers, and whether the screen
  // poll and the transcript panels run at all. Until it answers everything
  // reads as capable of nothing, which is the safe direction: a feature not
  // running for one frame beats one misfiring against the wrong engine.
  const [caps, setCaps] = useState<Record<string, Caps>>({});
  useEffect(() => {
    agentCaps().then(setCaps).catch(() => { /* keep the all-false default */ });
  }, []);
  /** What `agent` supports — nothing, for an id no backend claims. */
  const capsOf = (agent: string) => caps[agent] ?? NO_CAPS;
  /** The engine in the tab on screen. A shell has none, and answers all-false. */
  const activeCaps = capsOf(activeTabObj?.agentId ?? "");
  /** Whether to watch this tab's screen for a TUI.
   *
   *  A tab aiterm launched declares its engine and is taken at its word. A tab
   *  that declares none is a plain shell — and a shell is exactly where someone
   *  types `claude` by hand, which is how the dialogs were reached before there
   *  was a ＋ menu. The poll is a detector, so the undeclared tab is the one
   *  place it still earns its keep; an engine that told us it drives no TUI is
   *  the case worth switching off. */
  const activeDrivesTui = activeTabObj?.agentId ? activeCaps.tui_drive : true;

  // The sessions list, read without putting it in a callback's deps:
  // `newSession` needs to snapshot what already exists, and it must not be
  // rebuilt every time the list refreshes.
  const sessionsRef = useRef<Session[]>(sessions);
  sessionsRef.current = sessions;

  const handles = useRef<Map<TabId, TermHandle>>(new Map());
  const lastOutput = useRef<Map<TabId, number>>(new Map());

  // Which panels are open. These were plain state, so every restart threw the
  // layout away and you rebuilt it by hand — the sizes and fonts beside them
  // had always persisted, which made the loss look arbitrary. Saved as one
  // object rather than five keys: it is one decision, "how I have it set up".
  const [panels, setPanels] = useState<PanelToggles>(() =>
    loadJSON(PANELS_KEY, DEFAULT_PANELS),
  );
  useEffect(() => localStorage.setItem(PANELS_KEY, JSON.stringify(panels)), [panels]);
  const { sessions: showSessions, explorer: showExplorer, git: showGit,
          composer: showComposer, agent: showAgent } = panels;
  // Same shape the old `useState` setters had, so every call site is unchanged.
  const setPanel = (k: keyof PanelToggles) => (v: boolean) =>
    setPanels((p) => ({ ...p, [k]: v }));
  const setShowSessions = setPanel("sessions");
  const setShowExplorer = setPanel("explorer");
  const setShowGit = setPanel("git");
  const setShowComposer = setPanel("composer");
  const setShowAgent = setPanel("agent");

  // Screens in the terminal that aiterm can present better than the TUI does.
  // Polled rather than pushed: xterm has no "screen changed" event worth
  // hanging this on, and reading ~40 already-parsed lines four times a second
  // is nothing. `dismissed` remembers that you asked for the raw terminal for
  // *this* appearance, and clears itself once the screen goes away.
  const [tui, setTui] = useState<Detected | null>(null);
  const [permMode, setPermMode] = useState<PermissionMode | null>(null);
  // claude's agents view is on the active terminal's screen. Entering it moves
  // the conversation to the daemon under a new id, so this both hurries the
  // re-key below and stands the pills down — a /model sent now would be typed
  // into the view's "describe a task" box rather than run as a command.
  const [agentsView, setAgentsView] = useState(false);
  /** Last effect run's view state — detects the just-closed edge. */
  const wasAgentsView = useRef(false);
  const [tuiDismissed, setTuiDismissed] = useState(false);
  // Only dress up a screen *we* opened. Typing /model or /rewind yourself is a
  // request for the terminal, and answering it with our own dialog would be
  // taking the terminal away from someone who just asked for it. Records what
  // was asked for and when, so a screen that never appears stops arming us.
  const armed = useRef<{ what: "model" | "rewind"; at: number } | null>(null);
  const openViaPill = useCallback((what: "model" | "rewind", command: string) => {
    if (activeTab === null) return;
    armed.current = { what, at: Date.now() };
    handles.current.get(activeTab)?.sendComposed(command);
  }, [activeTab]);
  const openModelPicker = useCallback(
    () => openViaPill("model", "/model"), [openViaPill]);
  const openRewind = useCallback(
    () => openViaPill("rewind", "/rewind"), [openViaPill]);

  // Closing a dialog — by answering it, cancelling, or asking for the raw
  // terminal — always ends with the keyboard back in the terminal. Whatever
  // happens next is typed there, and leaving focus on a button that just
  // disappeared makes the first keystroke go nowhere.
  const dismissTui = useCallback((tab: TabId) => {
    setTuiDismissed(true);
    handles.current.get(tab)?.focus();
  }, []);

  const setPermissionMode = useCallback(async (target: PermissionMode) => {
    if (activeTab === null) return;
    const handle = handles.current.get(activeTab);
    if (!handle) return;
    await cycleModeTo(
      () => detectPermissionMode(handle.screen()),
      (d) => handle.write(d),
      target,
    );
    handle.focus();
  }, [activeTab]);

  useEffect(() => {
    // Everything below parses Claude Code's own screens — its `/model` picker,
    // its permission prompts, its `/rewind` steps, its agents view. Against any
    // other engine it is forty lines of somebody else's terminal being matched
    // against claude's phrasing four times a second, and a false positive would
    // draw a dialog that types into a TUI it cannot drive.
    //
    // So a tab that does not drive a TUI schedules no interval at all, and
    // whatever the last tab left behind is cleared on the way out: an open
    // Tui* dialog would otherwise stay on screen over a terminal it no longer
    // belongs to, still sending keystrokes there when answered. The permission
    // mode and the agents-view flag go with it — both describe the tab that has
    // just gone off screen, and both gate other chrome.
    if (!activeDrivesTui) {
      setTui(null);
      setTuiDismissed(false);
      setPermMode(null);
      setAgentsView(false);
      armed.current = null;
      return;
    }
    const id = window.setInterval(() => {
      const handle = activeTab === null ? undefined : handles.current.get(activeTab);
      const lines = handle ? handle.screen() : null;
      setPermMode(lines ? detectPermissionMode(lines) : null);
      setAgentsView(lines ? detectAgentsView(lines) : false);
      const found = lines ? detect(lines) : null;
      if (!found) {
        // Gone, or not painted yet. Give it a moment before disarming, so
        // arming a beat before claude draws does not cancel itself.
        if (armed.current && Date.now() - armed.current.at > 4000) {
          armed.current = null;
        }
        setTui(null);
        setTuiDismissed(false);
        return;
      }
      // A permission prompt is never something we asked for — it interrupts
      // you, which is exactly when a real dialog earns its place. Everything
      // reached by a command has to be armed, because typing that command
      // yourself is a request for the terminal.
      const needs =
        found.kind === "model-picker" || found.kind === "model-confirm" ? "model"
        : found.kind === "rewind-picker" || found.kind === "rewind-confirm" ? "rewind"
        : null;
      if (needs && armed.current?.what !== needs) return;
      // Showing an armed screen keeps it armed. Both flows have a second step
      // that replaces the first on screen, and the repaint between them can
      // land a tick on nothing — which, more than 4s after the pill click,
      // disarmed us and left step two undressed in the terminal.
      if (needs && armed.current) armed.current.at = Date.now();
      setTui((prev) => {
        // Only replace when something actually changed, so the dialog is not
        // rebuilt four times a second while it sits there.
        if (prev && prev.kind === found.kind && prev.highlighted === found.highlighted) {
          const sameSize =
            "options" in prev && "options" in found
              ? prev.options.length === found.options.length
              : "points" in prev && "points" in found
                ? prev.points.length === found.points.length
                : true;
          if (sameSize) return prev;
        }
        return found;
      });
    }, 250);
    return () => window.clearInterval(id);
  }, [activeTab, activeDrivesTui]);

  const [opts, setOpts] = useState<SessionDisplayOpts>(() =>
    loadJSON(OPTS_KEY, { showPath: true, showBranch: true, showTime: true }),
  );
  const [sizes, setSizes] = useState<PanelSizes>(() =>
    loadJSON(SIZES_KEY, { left: 280, right: 560, explorerFrac: 0.5, agentFrac: 0.3 }),
  );

  const [fontScale, setFontScale] = useState<number>(
    () => loadJSON(FONT_KEY, { scale: 1 }).scale,
  );

  const [settings, setSettings] = useState<AppSettings>(loadSettings);
  // Its names reach the list through the backend (`apply_session_names`),
  // the same way a name set by hand does, so the phone sees them too.
  const librarian = useLibrarian(settings.librarian, sessions);
  const [showSettingsModal, setShowSettingsModal] = useState(false);
  /** Where the settings window should open, when a caller has somewhere in
   *  mind. Cleared on close so the ⚙ button still opens on the first tab. */
  const [settingsTarget, setSettingsTarget] = useState<
    { tab: SettingsTab; provider: string | null } | null
  >(null);
  /** Bumped when the settings window closes: the empty pane's start controls
   *  re-read providers, so a shortlist just starred shows up without a
   *  restart. */
  const [settingsGen, setSettingsGen] = useState(0);
  const closeSettings = () => {
    setShowSettingsModal(false);
    setSettingsTarget(null);
    setSettingsGen((g) => g + 1);
  };
  const openModelAccess = (provider?: string) => {
    setSettingsTarget({ tab: "models", provider: provider ?? null });
    setShowSettingsModal(true);
  };
  useEffect(() => {
    applySettings(settings);
    saveSettings(settings);
  }, [settings]);
  useEffect(() => {
    if (!showSettingsModal) return;
    const h = (e: KeyboardEvent) => e.key === "Escape" && closeSettings();
    window.addEventListener("keydown", h, true);
    return () => window.removeEventListener("keydown", h, true);
  }, [showSettingsModal]);

  useEffect(() => localStorage.setItem(OPTS_KEY, JSON.stringify(opts)), [opts]);
  useEffect(() => localStorage.setItem(SIZES_KEY, JSON.stringify(sizes)), [sizes]);
  useEffect(() => localStorage.setItem(FONT_KEY, JSON.stringify({ scale: fontScale })), [fontScale]);

  const bumpFont = useCallback((dir: 1 | -1 | 0) => {
    setFontScale((s) =>
      dir === 0 ? 1 : Math.max(0.7, Math.min(1.6, +(s + dir * 0.1).toFixed(2))),
    );
  }, []);

  // Ctrl+= / Ctrl+- / Ctrl+0 font zoom, captured before xterm sees the keys.
  // Ctrl+Shift+L: force a clean repaint of the active terminal.
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      if (!e.ctrlKey || e.altKey || e.metaKey) return;
      if (e.shiftKey && (e.key === "L" || e.key === "l")) {
        e.preventDefault();
        const key = activeTabRef.current;
        if (key !== null) handles.current.get(key)?.redraw();
        return;
      }
      if (e.shiftKey) return;
      if (e.key === "=" || e.key === "+") { e.preventDefault(); bumpFont(1); }
      else if (e.key === "-") { e.preventDefault(); bumpFont(-1); }
      else if (e.key === "0") { e.preventDefault(); bumpFont(0); }
    };
    window.addEventListener("keydown", h, true);
    return () => window.removeEventListener("keydown", h, true);
  }, [bumpFont]);

  const termFont = Math.round(settings.termFontSize * fontScale);
  const xtermTheme = useMemo(() => termTheme(settings), [settings]);
  const xtermFont = useMemo(() => termFontFamily(settings), [settings]);
  const zoomFor = (panel: keyof AppSettings["panelScale"]): React.CSSProperties =>
    ({ zoom: fontScale * settings.panelScale[panel] } as React.CSSProperties);

  const [projects, setProjects] = useState<ProjectInfo[]>([]);
  const [trashed, setTrashed] = useState<TrashedSession[]>([]);
  // Which sessions the daemon is actually running. Polled rather than derived
  // from tabs, because those are different questions — see SessionsPanel's
  // hasTab/isRunning split.
  //
  // Every tick spawns a whole `claude agents --json`: ~0.26s wall and a peak
  // around 300 MB RSS, measured 2026-07-27. At the old unconditional 6s that
  // was 600 process spawns an hour for the lifetime of the window, running
  // just as hard while the app sat in the background as while it was being
  // looked at.
  //
  // So it only runs while the window has focus, and it stops dead when focus
  // leaves. Nothing is lost by that: the one thing that must reach you while
  // you are elsewhere — a session wanting input — arrives as a terminal bell
  // and an OS attention request, neither of which comes from here. This only
  // feeds dots on rows nobody is looking at.
  //
  // Regaining focus ticks immediately rather than waiting out the interval,
  // which is what makes the pause invisible: by the time your eyes are back on
  // the sidebar the reading is already in flight.
  const [liveSessions, setLiveSessions] = useState<Set<string>>(new Set());
  // Session ids our own terminals have demonstrably moved OFF of — a /clear or
  // an in-place /fork re-keyed the tab away. The roster goes on reporting an
  // interactive entry under the id it *started* with for as long as the client
  // process lives, so the finished conversation's row would wear a green dot
  // indefinitely (Matt hit this on /fork: the marker heuristic that filters a
  // cleared parent from live_session_ids knows nothing about fork children).
  // This is hook evidence, not inference — the pty that owned the id told us
  // it left. An id is taken back off the list the moment anything reopens it:
  // a tab slotted to it, or a hook report of it starting again.
  const [vacated, setVacated] = useState<Set<string>>(new Set());
  const liveShown = useMemo(() => {
    if (vacated.size === 0) return liveSessions;
    const out = new Set(liveSessions);
    for (const id of vacated) out.delete(id);
    return out;
  }, [liveSessions, vacated]);
  useEffect(() => {
    let stop = false;
    let timer: number | undefined;
    const tick = () =>
      liveSessionIds()
        .then((ids) => { if (!stop) setLiveSessions(new Set(ids)); })
        .catch(() => { /* keep the last known set rather than blanking dots */ });
    const start = () => {
      if (stop || timer !== undefined) return;
      tick();
      timer = window.setInterval(tick, 15000);
    };
    const halt = () => {
      if (timer === undefined) return;
      window.clearInterval(timer);
      timer = undefined;
    };
    if (document.hasFocus()) start();
    window.addEventListener("focus", start);
    window.addEventListener("blur", halt);
    return () => {
      stop = true;
      halt();
      window.removeEventListener("focus", start);
      window.removeEventListener("blur", halt);
    };
  }, []);

  // Usage across every service, fetched once for everything that shows it.
  // `/api/oauth/usage` rate limits, so a second poller is not just waste — a
  // refused request comes back with no numbers, and that view would blank
  // while the other still showed bars. `mergeUsage` keeps the last good
  // reading per source, because "couldn't ask" must never render as "you have
  // used nothing".
  //
  // Seeded from the last reading written to disk, so a cold start shows
  // something immediately instead of an empty strip for up to a minute — the
  // first call often lands on a rate limit, which made the gap longer still.
  // Written on every success rather than on exit: an exit hook does not run
  // when the app is killed, which is exactly when the cache would be wanted.
  // What comes off disk is marked stale on the way in: it was true when it was
  // written, and the panel says how long ago that was until a live read lands.
  const [usageSources, setUsageSources] = useState<UsageSourceAt[]>(loadUsageCache);
  const [usageBusy, setUsageBusy] = useState(false);
  // The merge needs the reading currently on screen, and reading it out of
  // state inside the updater would mean writing localStorage from a render.
  const usageRef = useRef<UsageSourceAt[]>(usageSources);
  const readUsage = useCallback(() => {
    setUsageBusy(true);
    return usageReport()
      .then((next) => {
        const merged = mergeUsage(usageRef.current, next);
        usageRef.current = merged;
        setUsageSources(merged);
        try {
          localStorage.setItem(USAGE_KEY, JSON.stringify(merged));
        } catch { /* quota — the cache is a convenience, not a requirement */ }
      })
      .catch(() => {})
      .finally(() => setUsageBusy(false));
  }, []);
  useEffect(() => {
    readUsage();
    const iv = setInterval(readUsage, 60_000);
    return () => clearInterval(iv);
  }, [readUsage]);
  // The composer's usage pill shows plan limits for the session it is attached
  // to, and those sessions are claude — so it gets Anthropic's bars, not the
  // whole report.
  const claudeUsage = useMemo(
    () => usageSources.find((s) => s.id === "anthropic"),
    [usageSources],
  );

  // List-only refresh: cheap, safe to run on every fs event.
  const refreshSessionList = useCallback(() => {
    listSessions().then(setSessions).catch(console.error);
    listProjects().then(setProjects).catch(console.error);
    trashList().then(setTrashed).catch(() => setTrashed([]));
  }, []);
  const refreshSessions = useCallback(() => {
    refreshSessionList();
    // Keep the full-text index warm in the background (30s poll only).
    reindexSessions().catch(() => {});
  }, [refreshSessionList]);
  useEffect(() => {
    refreshSessions();
    const iv = setInterval(refreshSessions, 30_000);
    return () => clearInterval(iv);
  }, [refreshSessions]);
  // ── Classifier-downgrade one-tap ──────────────────────────────────────
  // When the safeguard downgrades or blocks the active session, the CLI writes
  // a `model_refusal*` marker to the transcript; `session_refusal` surfaces it
  // and this raises the banner with the two one-tap moves. Baseline on session
  // switch so a refusal already sitting in the tail never pops the banner — only
  // a newly-appearing `uuid` does.
  const [refusal, setRefusal] = useState<Refusal | null>(null);
  const [ocTarget, setOcTarget] = useState<string | null>(null);
  const handledRefusal = useRef<string | null>(null);
  const activeSessionIdRef = useRef<string | null>(null);
  activeSessionIdRef.current = activeSessionId;

  useEffect(() => {
    setRefusal(null);
    if (!activeSessionId) { handledRefusal.current = null; return; }
    let alive = true;
    sessionRefusal(activeSessionId)
      .then((r) => { if (alive) handledRefusal.current = r?.uuid ?? null; })
      .catch(() => {});
    return () => { alive = false; };
  }, [activeSessionId]);

  const checkRefusal = useCallback(async () => {
    const sid = activeSessionIdRef.current;
    if (!sid) return;
    try {
      const r = await sessionRefusal(sid);
      if (r && r.uuid !== handledRefusal.current) {
        handledRefusal.current = r.uuid;
        opencodeDefaultTarget().then((t) => setOcTarget(t.model)).catch(() => {});
        setRefusal(r);
      }
    } catch { /* a transient read race is fine — the next event retries */ }
  }, []);

  // Event-driven refresh: Claude's transcripts changed (backend debounces).
  // A refusal lands in the transcript too, so the same event checks for one.
  // The reload is throttled: the watcher fires on every transcript-append
  // burst, so with any session writing (one usually is) this event arrives
  // every ~2s — and each unthrottled reload shipped 500+ rows over IPC and
  // reconciled them all, which is what made every click and keystroke queue
  // behind render work [measured 2026-08-31]. Trailing-edge, so the last
  // burst of a quiet-down still lands, at most LIST_RELOAD_MS late.
  const lastListReload = useRef(0);
  const listReloadTimer = useRef<number | null>(null);
  const throttledSessionReload = useCallback(() => {
    const LIST_RELOAD_MS = 4000;
    const since = Date.now() - lastListReload.current;
    if (since >= LIST_RELOAD_MS) {
      lastListReload.current = Date.now();
      refreshSessionList();
    } else if (listReloadTimer.current === null) {
      listReloadTimer.current = window.setTimeout(() => {
        listReloadTimer.current = null;
        lastListReload.current = Date.now();
        refreshSessionList();
      }, LIST_RELOAD_MS - since);
    }
  }, [refreshSessionList]);
  useEffect(() => {
    const un = listen("sessions://changed", () => {
      throttledSessionReload();
      checkRefusal();
    });
    return () => {
      un.then((f) => f());
    };
  }, [throttledSessionReload, checkRefusal]);

  // NOTE: there used to be a large effect here that watched for newly-appeared
  // sessions and decided some of them "superseded" older rows — hiding those
  // rows behind six heuristic guards, with an undo toast. It is gone. The list
  // shows what is on disk, and nothing removes a row but you.
  //
  // The tab-rebind it also did (follow a conversation into a new transcript
  // after a compaction) is not needed either: the backend resolves a pinned
  // session id to the newest transcript in its family on read, so the Agents
  // and Tasks panels already follow without anything rewriting the tab.

  const applyTabDescriptor = useCallback((descriptor: TabDescriptor) => {
    setTabs((current) => {
      const next = current.map((tab) =>
        tab.key === descriptor.id ? { ...tab, ...descriptor, key: descriptor.id } : tab);
      tabsRef.current = next;
      return next;
    });
  }, []);

  useEffect(() => {
    let stopped = false;
    let unlisten: (() => void) | null = null;
    let recoveryTimer: number | null = null;

    const applyRegistryProjection = (projection: { revision: number | null; tabs: TabDescriptor[] }) => {
      if (stopped) return;
      const before = tabsRef.current;
      tabRegistryProjection.current = projection;
      setTabs((current) => {
        const next = reconcileTermTabs(current, projection.tabs);
        tabsRef.current = next;
        setActiveTab((active) =>
          active !== null && next.some((tab) => tab.key === active)
            ? active
            : next[next.length - 1]?.key ?? null);
        return next;
      });

      const live = new Set(projection.tabs.map((tab) => tab.id));
      const removed = new Set(before.filter((tab) => !live.has(tab.key)).map((tab) => tab.key));
      if (removed.size === 0) return;
      setFileTabs((files) => {
        const dropped = files.filter((file) => file.termKey !== null && removed.has(file.termKey));
        if (dropped.length === 0) return files;
        const droppedKeys = new Set(dropped.map((file) => file.key));
        setDirtyFiles((dirty) => {
          const clean = new Set(dirty);
          droppedKeys.forEach((key) => clean.delete(key));
          return clean;
        });
        setActiveFileTab((active) =>
          active !== null && droppedKeys.has(active) ? null : active);
        return files.filter((file) => file.termKey === null || !removed.has(file.termKey));
      });
      setEnded((endedTabs) => {
        const remaining = new Map(endedTabs);
        removed.forEach((key) => remaining.delete(key));
        return remaining;
      });
    };

    const recovery = createTabRegistryRecovery(
      tabRegistryProjection.current,
      tabRegistrySnapshot,
      applyRegistryProjection,
    );

    const recover = () => {
      if (stopped) return;
      void recovery.recover().catch(() => {
        if (!stopped && recoveryTimer === null) {
          recoveryTimer = window.setTimeout(() => {
            recoveryTimer = null;
            recover();
          }, 250);
        }
      });
    };

    const acceptRegistryChange = (change: TabRegistryEvent) => {
      const pending = recovery.accept(change);
      if (pending !== null) {
        void pending.catch(() => {
          if (!stopped && recoveryTimer === null) {
            recoveryTimer = window.setTimeout(() => {
              recoveryTimer = null;
              recover();
            }, 250);
          }
        });
      }
    };

    void (async () => {
      const stop = await listen<TabRegistryEvent>("tab://registry", (event) => {
        acceptRegistryChange(event.payload);
      });
      if (stopped) {
        stop();
        return;
      }
      unlisten = stop;
      recover();
    })();
    return () => {
      stopped = true;
      if (recoveryTimer !== null) clearTimeout(recoveryTimer);
      unlisten?.();
    };
  }, []);

  const openTab = useCallback(
    (title: string, cwd: string | null, command: string | null, slotId: string,
     opts: OpenTabOpts = {}): Promise<TabId | null> => {
      setPreviewSession(null);
      // Opening a terminal on a slot is the deliberate act of going back to
      // it — if its id was on the vacated list (green dot suppressed), it has
      // earned the dot again the moment the roster reports it.
      setVacated((prev) => {
        if (!prev.has(slotId)) return prev;
        const next = new Set(prev);
        next.delete(slotId);
        return next;
      });
      const existing = tabsRef.current.find((tab) => tab.slotId === slotId);
      if (existing) {
        setActiveTab(existing.key);
        return Promise.resolve(existing.key);
      }
      if (pendingOpens.current.has(slotId)) return Promise.resolve(null);

      const pending = { cancelled: false };
      pendingOpens.current.set(slotId, pending);
      return (async () => {
        let descriptor: TabDescriptor | null = null;
        try {
          const { adopt, parentKey, ...registryOpts } = opts;
          descriptor = await tabOpen({
            title, cwd, command, slotId, ...registryOpts,
            size: { cols: 80, rows: 24 },
          });
          if (pending.cancelled || !appAlive.current) {
            uiLog(`openTab guard1 closing ${descriptor.id}: cancelled=${pending.cancelled} appAlive=${appAlive.current}`);
            await tabClose(descriptor.id).catch(() => {});
            return null;
          }
          const authoritative = await tabList().catch(() => [descriptor!]);
          if (pending.cancelled || !appAlive.current) {
            uiLog(`openTab guard2 closing ${descriptor.id}: cancelled=${pending.cancelled} appAlive=${appAlive.current}`);
            await tabClose(descriptor.id).catch(() => {});
            return null;
          }
          setTabs((current) => reconcileTermTabs(current, authoritative).map((tab) =>
            tab.key === descriptor!.id ? { ...tab, adopt, parentKey } : tab));
          setActiveTab(descriptor.id);
          if (descriptor.state === "exited") {
            const why = descriptor.exit;
            uiLog(`openTab exited-branch ${descriptor.id}: code=${why?.code} requested=${why?.requested}`);
            if (why?.code === 0 || why?.requested) {
              await tabClose(descriptor.id).catch(() => {});
              setTabs((current) => current.filter((tab) => tab.key !== descriptor!.id));
              setActiveTab((active) => active === descriptor!.id ? null : active);
            } else {
              setEnded((current) => new Map(current).set(descriptor!.id, {
                code: why?.code ?? null,
                signal: why?.signal ?? null,
              }));
            }
          }
          return descriptor.id;
        } catch (error) {
          uiLog(`tab open failed for ${slotId}: ${String(error)}`);
          if (descriptor && (pending.cancelled || !appAlive.current)) {
            await tabClose(descriptor.id).catch(() => {});
          }
          return null;
        } finally {
          if (pendingOpens.current.get(slotId) === pending) {
            pendingOpens.current.delete(slotId);
          }
        }
      })();
    },
    [],
  );

  // Switching terminal, or picking a different session to preview, puts that
  // terminal or preview on screen — a file tab left selected would cover it.
  /** What is in front, for the purpose of owning files: the preview when one
   *  is up (it covers the terminal, and the explorer shows *its* project),
   *  else the active terminal, else home. A file opened now belongs to this,
   *  and only files belonging to this are in the row. */
  const fileScope: FileScope = previewSession ? `preview:${previewSession.id}` : rootKey;
  const fileScopeRef = useRef<FileScope>(fileScope);
  fileScopeRef.current = fileScope;
  /** Which file each scope had on screen, so switching away and back finds
   *  it where it was left — the file row is part of the session, not a
   *  shared strip. */
  const fileFor = useRef(new Map<FileScope, number | null>());
  const prevScope = useRef(fileScope);
  useEffect(() => {
    if (prevScope.current !== fileScope) {
      prevScope.current = fileScope;
      const k = fileFor.current.get(fileScope) ?? null;
      setActiveFileTab(k !== null && fileTabs.some((f) => f.key === k) ? k : null);
    } else {
      fileFor.current.set(fileScope, activeFileTab);
    }
  }, [fileScope, activeFileTab]);
  // A preview is transient: the files opened off it go when it does, rather
  // than lingering unreachable until the same session is previewed again.
  useEffect(() => {
    const keep = previewSession ? `preview:${previewSession.id}` : null;
    setFileTabs((list) => list.some((f) => typeof f.termKey === "string" && f.termKey !== keep)
      ? list.filter((f) => typeof f.termKey !== "string" || f.termKey === keep)
      : list);
  }, [previewSession?.id]);

  /** Whether a file tab belongs to what is in front. Files open where they
   *  were opened from and nowhere else; ones opened from the home view belong
   *  to it, and the home tab is how they are reached. */
  const showsFile = useCallback(
    (f: FileTab) => f.termKey === fileScope,
    [fileScope],
  );
  /** A file tab is the thing on screen in the center right now. */
  const fileOnScreen =
    activeFileTab !== null && fileTabs.some((f) => showsFile(f) && f.key === activeFileTab);

  const openFileTab = useCallback((path: string) => {
    const term = fileScopeRef.current;
    setFileTabs((list) => {
      const existing = list.find((f) => f.path === path && f.termKey === term);
      if (existing) {
        setActiveFileTab(existing.key);
        return list;
      }
      const key = nextFileKey.current++;
      setActiveFileTab(key);
      return [...list, { key, termKey: term, path }];
    });
  }, []);

  const noteFileDirty = useCallback((key: number, dirty: boolean) => {
    setDirtyFiles((prev) => {
      if (prev.has(key) === dirty) return prev;
      const next = new Set(prev);
      if (dirty) next.add(key);
      else next.delete(key);
      return next;
    });
  }, []);

  const closeFileTab = useCallback((key: number) => {
    if (dirtyFiles.has(key) && fileCloseArm !== key) {
      setFileCloseArm(key);
      if (fileCloseTimer.current !== null) clearTimeout(fileCloseTimer.current);
      fileCloseTimer.current = window.setTimeout(() => setFileCloseArm(null), 2600);
      return;
    }
    setFileCloseArm(null);
    setDirtyFiles((prev) => {
      if (!prev.has(key)) return prev;
      const next = new Set(prev);
      next.delete(key);
      return next;
    });
    setFileTabs((list) => {
      const closing = list.find((f) => f.key === key);
      const next = list.filter((f) => f.key !== key);
      setActiveFileTab((cur) => {
        if (cur !== key) return cur;
        const sib = next.filter((f) => f.termKey === closing?.termKey);
        return sib[sib.length - 1]?.key ?? null;
      });
      return next;
    });
  }, [dirtyFiles, fileCloseArm]);

  const registerHandle = useCallback((key: TabId, handle: TermHandle | null) => {
    if (handle) handles.current.set(key, handle);
    else handles.current.delete(key);
  }, []);

  /** The last time a tab changed which conversation it holds. The sidebar's
   *  click-selection follows this: without it the selection keeps pointing at
   *  the conversation you left, wearing the loudest highlight of the three
   *  while the live row wears the faintest. Carries a sequence so two
   *  identical moves still read as two events. */
  const [rekey, setRekey] = useState<{ from: string; to: string; seq: number } | null>(null);
  const rekeySeq = useRef(0);
  const noteRekey = useCallback((from: string, to: string) => {
    rekeySeq.current += 1;
    setRekey({ from, to, seq: rekeySeq.current });
  }, []);

  /** Everything waiting on you, oldest first — the bell list and the taskbar
   *  number are two views of this one thing. */
  const alerts: Alert[] = useMemo(
    () =>
      tabs
        .filter((t) => attention.has(t.key))
        .map((t) => ({
          key: t.key,
          title: t.title,
          message: notices.get(t.key),
          at: alertAt.get(t.key) ?? Date.now(),
        }))
        .sort((a, b) => a.at - b.at),
    [tabs, attention, notices, alertAt],
  );

  // The count on the icon, for when aiterm is behind another window — which is
  // the only time it matters, and the only time the in-app bell cannot be seen.
  useEffect(() => {
    taskbarBadge(alerts.length).catch(() => {});
  }, [alerts.length]);

  // The tray menu carries the list itself, so a waiting session can be picked
  // without bringing aiterm forward first to read the bell.
  useEffect(() => {
    trayAlerts(
      alerts.map((a) => ({ key: a.key, title: a.title, message: a.message })),
    ).catch(() => {});
  }, [alerts]);

  /** Daemon ids of popups currently on screen, by tab. A ref, not state: these
   *  are the desktop's business, and re-rendering over them would be noise. */
  const popups = useRef<Map<TabId, number>>(new Map());

  // A popup only when aiterm is not the window you are looking at — if it is,
  // the bell is right there and a notification would be telling you something
  // you can already see.
  useEffect(() => {
    const waiting = new Set(alerts.map((a) => a.key));
    for (const [key, id] of popups.current) {
      if (!waiting.has(key)) {
        popups.current.delete(key);
        desktopNotifyClose(id).catch(() => {});
      }
    }
    if (document.hasFocus()) return;
    for (const a of alerts) {
      if (popups.current.has(a.key)) continue;
      // Claimed before the await so a second pass cannot post a duplicate
      // while the first is still in flight.
      popups.current.set(a.key, 0);
      desktopNotify(a.title, a.message ?? "Waiting for your input", 0)
        .then((id) => {
          if (id && popups.current.get(a.key) === 0) popups.current.set(a.key, id);
        })
        .catch(() => popups.current.delete(a.key));
    }
  }, [alerts]);

  // Picking one there means going to it; the window was already raised by the
  // backend before this fires.
  useEffect(() => {
    const un = listen<TabId>("tray-alert", (e) => setActiveTab(e.payload));
    return () => {
      un.then((f) => f()).catch(() => {});
    };
  }, []);

  const noteActivity = useCallback((key: TabId) => {
    lastOutput.current.set(key, Date.now());
  }, []);

  const noteLineSubmit = useCallback((key: TabId, line: string) => {
    const tab = tabsRef.current.find((t) => t.key === key);
    if (
      line.trim() !== "/clear" || tab?.agentId !== "codex" || !tab.sessionId || !tab.cwd
    ) return;
    const known = new Set(
      sessionsRef.current
        .filter((s) => s.project_path === tab.cwd)
        .map((s) => s.id),
    );
    // A tab created in the same project after this one may not have reached
    // the sidebar snapshot yet. It is still a known owner, never a clear child.
    for (const other of tabsRef.current) {
      if (other.key !== key && other.cwd === tab.cwd && other.sessionId) known.add(other.sessionId);
    }
    known.add(tab.sessionId);
    clearIntents.current.set(key, {
      previousId: tab.sessionId,
      cwd: tab.cwd,
      since: Date.now(),
      known: [...known],
      inFlight: false,
    });
    setClearIntentRevision((revision) => revision + 1);
  }, []);

  const noteAttention = useCallback((key: TabId, on: boolean) => {
    // A bell on the tab you're actively looking at isn't news.
    if (on && key === activeTabRef.current && document.hasFocus()) return;
    setAttention((prev) => {
      if (on === prev.has(key)) return prev;
      const next = new Set(prev);
      if (on) next.add(key);
      else next.delete(key);
      return next;
    });
    setAlertAt((prev) => {
      if (on === prev.has(key)) return prev;
      const next = new Map(prev);
      if (on) next.set(key, Date.now());
      else next.delete(key);
      return next;
    });
    if (on && !document.hasFocus()) {
      getCurrentWindow()
        .requestUserAttention(UserAttentionType.Informational)
        .catch(() => {});
    }
    // The badge and the sentence behind it clear together — a message left
    // behind after the badge went is a stale answer to "what did it want?".
    if (!on) {
      setNotices((prev) => {
        if (!prev.has(key)) return prev;
        const next = new Map(prev);
        next.delete(key);
        return next;
      });
    }
  }, []);

  /** What a tab asked for, in its own words (OSC 9), keyed like `attention`. */
  const noteNotify = useCallback((key: TabId, message: string) => {
    setNotices((prev) => {
      if (prev.get(key) === message) return prev;
      return new Map(prev).set(key, message);
    });
  }, []);

  const noteProgress = useCallback((key: TabId, p: TermProgress | null) => {
    setProgress((prev) => {
      if (p === null) {
        if (!prev.has(key)) return prev;
        const next = new Map(prev);
        next.delete(key);
        return next;
      }
      const had = prev.get(key);
      if (had && had.state === p.state && had.pct === p.pct) return prev;
      return new Map(prev).set(key, p);
    });
  }, []);

  // Viewing a tab (with the window focused) clears its badge.
  useEffect(() => {
    activeTabRef.current = activeTab;
    if (activeTab === null) return;
    const clear = () => {
      if (document.hasFocus() && activeTabRef.current !== null) {
        setAttention((prev) => {
          if (!prev.has(activeTabRef.current!)) return prev;
          const next = new Set(prev);
          next.delete(activeTabRef.current!);
          return next;
        });
      }
    };
    clear();
    window.addEventListener("focus", clear);
    return () => window.removeEventListener("focus", clear);
  }, [activeTab]);

  // (Removed the startup OS-window ±1px "jiggle". It forced a Wayland surface
  // reconfigure to fix bottom-edge clipping / stale content — a symptom of the
  // WebKitGTK DMABUF renderer, now disabled at the Rust entry point. No more
  // window growing/shrinking on launch.)

  // Dropping files onto the window pastes their quoted paths into the
  // active terminal (like any terminal emulator) instead of letting the
  // webview navigate to the file.
  const previewRef = useRef<Session | null>(null);
  useEffect(() => {
    previewRef.current = previewSession;
  }, [previewSession]);
  useEffect(() => {
    const un = getCurrentWebview().onDragDropEvent((e) => {
      if (e.payload.type !== "drop" || e.payload.paths.length === 0) return;
      const key = activeTabRef.current;
      if (key === null || previewRef.current) return;
      const h = handles.current.get(key);
      // One paste per path, like a real terminal drop — pasted (not typed)
      // so claude recognizes image/file paths and shows [Image #N].
      e.payload.paths.forEach((p, i) => {
        if (i > 0) h?.write(" ");
        h?.paste(shellEscape(p));
      });
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  // Watch the active project: git changes refresh the repo panel, tree
  // changes refresh the explorer (git status also follows tree edits).
  useEffect(() => {
    if (!activeProject) return;
    watchProject(activeProject).catch(console.error);
  }, [activeProject]);
  useEffect(() => {
    const un = listen<{ git: boolean; tree: boolean }>("fs://changed", (e) => {
      setGitRefresh((n) => n + 1);
      if (e.payload.tree) setExplorerRefresh((n) => n + 1);
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  // Sessions aiterm has started that are not on disk yet. The sidebar lists
  // transcripts, and claude writes none until the first prompt, so without
  // these a brand-new session is a terminal with no row — unreachable the
  // moment you look at something else, since the sidebar is how you get back
  // to a tab. Each one retires by itself: the id was minted by `newSession`,
  // so when that transcript lands the real row is already the tab's row and
  // this list stops naming it. Nothing guesses, and nothing hides a real row.
  //
  // Keyed by `slotId`, not `sessionId`. For claude the two are the same value —
  // `newSession` mints one id and passes it as both — but an agent that has no
  // `--session-id` gets a slot and no session id at all, and filtering on the
  // session id dropped exactly those tabs: Codex opened a terminal with no row,
  // which is the unreachable-tab bug this list exists to prevent. `slotId` is
  // also what the panel hands back to `onSelectPending`/`onExitPending`, so it
  // is the id this list should have been carrying either way.
  const knownSessionIds = useMemo(() => new Set(sessions.map((s) => s.id)), [sessions]);
  const pendingSessions = useMemo(
    () =>
      tabs
        .filter((t) => t.fresh && !knownSessionIds.has(t.slotId))
        // The engine comes along so the placeholder wears its own mark. Without
        // it every pending row drew claude's starburst, so a fresh Codex or
        // OpenCode tab spent its first minutes claiming to be a claude session.
        .map((t) => ({
          id: t.slotId, title: t.title, cwd: t.cwd ?? "", agent: t.agentId ?? "",
        })),
    [tabs, knownSessionIds],
  );
  // The migration watcher's guard, reduced to a boolean before it reaches the
  // effect's dependency list. `knownSessionIds` is a new Set on every sessions
  // refresh — which the transcript watcher fires on every write — so depending
  // on it directly re-armed the watcher constantly during any active
  // conversation: a journal line and an immediate re-check per write instead of
  // one per 15s. The boolean only changes when the answer does, and its one
  // flip (the fresh tab's transcript landing) is exactly when re-arming is
  // wanted.
  const freshUnwritten = !!(
    activeTabObj?.fresh &&
    activeTabObj.sessionId &&
    !knownSessionIds.has(activeTabObj.sessionId)
  );

  // Tabs still waiting to learn their session id, as a string that only
  // changes when the set does. Depending on `tabs` here would re-arm the
  // watcher below on every keystroke's worth of tab state.
  const awaitingAdoption = tabs
    .filter((t) => t.adopt && !t.sessionId)
    .map((t) => t.key)
    .join(",");

  // Agents with no `--session-id` name themselves, and only once they start.
  // Until aiterm learns that name the tab is keyed to a handle no session will
  // ever have: the placeholder row stays at the top of the sidebar forever,
  // and the moment the agent writes its transcript the same conversation gets
  // a second, real row under its project. Adopting the id collapses the two.
  //
  // Codex writes at launch rather than at first prompt, so this usually
  // resolves on the first tick.
  useEffect(() => {
    if (!awaitingAdoption) return;
    let stop = false;
    const tick = async () => {
      for (const t of tabsRef.current) {
        if (stop) return;
        if (!t.adopt || t.sessionId || !t.cwd) continue;
        try {
          const id = await adoptAgentSession(
            t.adopt.agentId, t.cwd, t.adopt.since, t.adopt.known,
          );
          if (stop || !id) continue;
          const descriptor = await tabUpdate(t.key, {
            sessionId: id, slotId: id, fresh: false,
          });
          if (stop) continue;
          setTabs((list) => list.map((x) => x.key === t.key
            ? { ...x, ...descriptor, key: descriptor.id, adopt: undefined }
            : x));
          uiLog(`adopted ${t.adopt.agentId} session ${id} for tab ${t.key}`);
          refreshSessionList();
        } catch {
          /* keep waiting — the agent may not have written anything yet */
        }
      }
    };
    // Keep looking on a backing-off schedule rather than to a flat deadline.
    // The old one was a minute, on the assumption Codex wrote its transcript at
    // launch; it does not, and a session measured on 2026-08-16 took 98.9s to
    // appear — so adoption had already quit when the file it was waiting for
    // arrived, and the conversation kept a placeholder row and a real row for
    // good. `nextAdoptionDelay` owns the timings.
    const startedAt = Date.now();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const loop = async () => {
      await tick();
      if (stop) return;
      const wait = nextAdoptionDelay(Date.now() - startedAt);
      if (wait !== null) timer = setTimeout(loop, wait);
    };
    loop();
    return () => {
      stop = true;
      if (timer) clearTimeout(timer);
    };
  }, [awaitingAdoption, refreshSessionList]);

  // Codex `/clear` keeps the existing PTY but starts a new conversation id.
  // Without this handoff the terminal remains attached to the old sidebar row,
  // while the live row looks unowned; clicking it launches `codex resume` into
  // a thread that already has this terminal as its active writer. The intent
  // recorded above lets us accept a successor without guessing from the
  // directory's normal stream of Codex sessions.
  useEffect(() => {
    if (clearIntents.current.size === 0) return;
    let stop = false;
    const tick = async () => {
      for (const [key, intent] of clearIntents.current) {
        if (stop) return;
        if (intent.inFlight) continue;
        const tab = tabsRef.current.find((t) => t.key === key);
        if (!tab || tab.sessionId !== intent.previousId || tab.agentId !== "codex") {
          clearIntents.current.delete(key);
          setClearIntentRevision((revision) => revision + 1);
          continue;
        }
        try {
          intent.inFlight = true;
          const successor = await clearSuccessorSession(
            tab.agentId, intent.previousId, intent.cwd, intent.since, intent.known,
          );
          if (stop || clearIntents.current.get(key) !== intent) continue;
          if (!successor) {
            intent.inFlight = false;
            continue;
          }
          const descriptor = await tabUpdate(key, {
            sessionId: successor,
            slotId: successor,
            fresh: false,
            title: basename(tab.cwd ?? "") || tab.title,
          });
          if (stop || clearIntents.current.get(key) !== intent) continue;
          applyTabDescriptor(descriptor);
          noteRekey(intent.previousId, successor);
          setVacated((previous) => new Set(previous).add(intent.previousId));
          clearIntents.current.delete(key);
          setClearIntentRevision((revision) => revision + 1);
          uiLog(`codex clear: re-keyed tab ${key} ${intent.previousId} -> ${successor}`);
          setNotice(
            `"${tab.title}" was cleared — this tab is the new conversation. ` +
              "The old one is in the sidebar, resumable.",
          );
          refreshSessionList();
        } catch {
          // The newly-created rollout can appear after the first poll. Keep
          // the explicit intent and try again; ambiguity remains a safe no-op.
          if (clearIntents.current.get(key) === intent) intent.inFlight = false;
        }
      }
    };
    void tick();
    const interval = window.setInterval(() => { void tick(); }, 500);
    return () => {
      stop = true;
      window.clearInterval(interval);
    };
  }, [applyTabDescriptor, clearIntentRevision, noteRekey, refreshSessionList]);

  // A conversation can change its session id while staying in this same pty.
  // Two things do it: moving to the daemon (all that opening the agents view
  // does) and `/clear`. Either way the tab's pinned id was set once when it
  // opened and nothing ever moved it, so from that moment the terminal renders
  // the live conversation while every panel keyed to the tab reads the old
  // transcript: a file nothing is writing. Live text, dead clock — and the live
  // conversation's own sidebar row belongs to no tab, so clicking it opens a
  // SECOND agent on it. Re-key the tab when it happens, and say so, because the
  // row the sidebar shows for this conversation changes underneath the user.
  //
  // Only the active tab, and only every 15s: answering this means reading
  // transcripts off disk, and the parent can be tens of megabytes.
  //
  // Except while the agents view is on screen — the migration happens the
  // moment that view opens, so seeing it is the cue to chase the new id now
  // (and every 2s until the child's transcript lands on disk). This is what
  // closes the stale window where the terminal showed the live conversation
  // over panels reading a file nothing was writing.
  useEffect(() => {
    const key = activeTabObj?.key;
    const pinned = activeTabObj?.sessionId;
    const title = activeTabObj?.title ?? "This session";
    const resumedId = activeTabObj?.resumedId;
    if (key === undefined || !pinned) return;
    // Compaction, `/clear` and the move to the daemon are all things a
    // TUI-driving engine does to its own transcripts. `sessionMovedTo` resolves
    // through the whole registry, so without this an `aiterm chat` tab would
    // have claude's heuristics run over the chats directory and could be
    // re-keyed — and told it had been cleared — on a false positive.
    if (!activeCaps.tui_drive) return;
    // This runs even for tabs the SessionStart hook reports on (drain effect
    // below): the hook cannot see a move to the daemon — that claude is not
    // ours — so the Background kind is still this watcher's alone. For a
    // clear, both mechanisms re-key to the same id, so whichever fires first
    // settles it and the other finds nothing left to do.
    // A session aiterm started that has not written its transcript yet has
    // nothing to have migrated *from*, so this would poll a file that does not
    // exist until the first prompt lands. Narrow on purpose: only a `fresh`
    // tab whose id has never appeared on disk. A pinned id missing from the
    // list is otherwise the very symptom this watcher exists for — a
    // compaction retires the original transcript — and skipping those would
    // disable it exactly when it is needed.
    if (freshUnwritten) return;
    let stop = false;
    const check = async () => {
      try {
        const moved = await sessionMovedTo(pinned);
        if (stop || !moved || moved.id === pinned) return;
        // A tab opened to reopen `pinned` is deliberately holding that
        // conversation. Its `/clear` child on disk is history, not news — the
        // last version of this stole the tab the instant Resume was clicked,
        // re-keyed it onto the old clear's successor, and left the actual
        // resume minting a third session (observed 2026-07-29 21:36, three
        // kill_trees of cleanup). "It moved once" is forever true on disk;
        // "this tab wants the original" beats it.
        //
        // Compares what the tab was opened *for*, not what its command string
        // looks like. Sniffing the text for `--resume <id>` was always the weak
        // version — the tab knows its own intent, and the substring stopped
        // matching the moment the backend started shell-quoting the id.
        if (moved.kind === "cleared" && resumedId === pinned) return;
        uiLog(`migrate: re-keying tab ${key} ${pinned} -> ${moved.id} (${moved.kind})`);
        // `slotId` moves with `sessionId`, not just the pinned id. The slot is
        // what links a tab to a sidebar row — the live dot, the highlight, and
        // the click that focuses an existing terminal instead of opening
        // another. Leaving it on the old id was the second half of this bug:
        // the tab kept the frozen row and the live one looked free to open.
        //
        // Nothing else changes: for a clear, the conversation left behind
        // becomes an ordinary stopped row in the sidebar — click for its
        // preview, ▶ to resume — exactly like any session that ended. (An
        // earlier version manufactured a special "historical" tab for it,
        // blank, with its own explanatory buttons. Matt: make it look like a
        // normal stop instead.) `fresh` + retitle for the cleared kind mirror
        // the hook path above, and for the same reasons.
        const cleared = moved.kind === "cleared";
        const descriptor = await tabUpdate(key, {
          sessionId: moved.id,
          slotId: moved.id,
          fresh: cleared ? true : activeTabObj?.fresh,
          title: cleared ? basename(activeTabObj?.cwd ?? "") || title : title,
        });
        if (stop) return;
        applyTabDescriptor(descriptor);
        noteRekey(pinned, moved.id);
        setVacated((prev) => new Set(prev).add(pinned));
        setNotice(
          moved.kind === "cleared"
            ? `"${title}" was cleared — this tab is the new conversation. The old one is in the sidebar, resumable.`
            : `"${title}" moved to a background session — its panels now follow the live one.`,
        );
        refreshSessionList();
      } catch (e) {
        // Not just "backend unavailable": a rejected invoke lands here too,
        // and silently keeping the pinned id is how a dead path looks tested.
        uiLog(`migrate: check for ${pinned} failed: ${String(e)}`);
      }
    };
    uiLog(`migrate: watching ${pinned} (agentsView=${agentsView})`);
    check();
    const id = setInterval(check, agentsView ? 2000 : 15000);
    // Right after ← the child is a two-line stub with no history and no links
    // into the parent — the copy lands with the *next* activity, usually just
    // after the user escapes back and keeps talking (measured 2026-07-27). So
    // the moment the view closes, check again a couple of times instead of
    // leaving the last word to the slow poll.
    const grace: number[] = [];
    if (wasAgentsView.current && !agentsView) {
      grace.push(window.setTimeout(check, 3000), window.setTimeout(check, 8000));
    }
    wasAgentsView.current = agentsView;
    return () => {
      stop = true;
      clearInterval(id);
      grace.forEach(clearTimeout);
    };
  }, [activeTabObj?.key, activeTabObj?.sessionId, activeTabObj?.title,
      activeTabObj?.resumedId, freshUnwritten, activeCaps.tui_drive,
      applyTabDescriptor, refreshSessionList, agentsView]);

  // The exact channel: every claude aiterm launches carries a SessionStart
  // hook (see hooklink.rs) that reports its session id, its cause and its pid
  // into a spool. Drained here, each event lands on the tab whose process owns
  // that pid — so a `/clear` re-keys within two seconds, on any tab, active
  // or not, with no inference anywhere in the path. The watcher above stays:
  // it alone sees daemon moves, and it covers claudes with no hook — one
  // typed into a shell tab, or from before this build.
  useEffect(() => {
    let stop = false;
    const drain = async () => {
      if (tabsRef.current.length === 0) return;
      let events;
      try {
        events = await drainSessionEvents();
      } catch {
        return; // backend unavailable; the spool keeps until it is back
      }
      if (stop) return;
      for (const evt of events) {
        const tab = tabsRef.current.find((candidate) => candidate.key === evt.tabId);
        if (!tab) continue;
        // A session starting (again) under an id is the definition of "not
        // vacated" — however it left, it's back.
        setVacated((prev) => {
          if (!prev.has(evt.sessionId)) return prev;
          const next = new Set(prev);
          next.delete(evt.sessionId);
          return next;
        });
        // Only re-key tabs that are keyed. A shell tab someone ran `claude`
        // in, or a project-▶ tab, is slotted by its path — handing it a
        // session slot would break the "one terminal per project door" dedupe
        // without buying anything the panels don't already do.
        if (!tab.sessionId || tab.sessionId === evt.sessionId) continue;
        const old = tab.sessionId;
        // `clear` and `fork` both mean: this terminal now holds a NEW
        // conversation and the old id is finished. (`fork` was discovered
        // live 2026-07-30 — claude's own /fork switches the terminal to the
        // branch, hook source "fork", and nothing on disk links the two.)
        const newConversation = evt.source === "clear" || evt.source === "fork";
        uiLog(`hook: tab ${tab.key} ${old} -> ${evt.sessionId} (${evt.source})`);
        applyTabDescriptor(evt.tab);
        noteRekey(old, evt.sessionId);
        if (newConversation) {
          // The roster keeps listing the old id while the client process
          // lives; only we know the terminal left it behind.
          setVacated((prev) => new Set(prev).add(old));
          setNotice(
            evt.source === "clear"
              ? `"${tab.title}" was cleared — this tab is the new conversation. ` +
                `The old one is in the sidebar, resumable.`
              : `"${tab.title}" forked — this tab is the branch. ` +
                `The original conversation is in the sidebar, resumable.`,
          );
        }
        refreshSessionList();
      }
    };
    drain();
    const iv = setInterval(drain, 2000);
    return () => {
      stop = true;
      clearInterval(iv);
    };
  }, [applyTabDescriptor, refreshSessionList]);
  // The composer's status line is gone, and with it three pollers that existed
  // only to feed it: a 1s "working" pulse, a 5s `session_status` call, and a
  // `git_repo_state` call per project change. Claude's own footer already says
  // all three things. Removed rather than left running for nobody to read.

  const closeTab = useCallback(async (key: TabId) => {
    // A session's brought-in agents go with it.
    const gone = new Set([key, ...tabsRef.current.filter((t) => t.parentKey === key).map((t) => t.key)]);
    const parentOfClosed = tabsRef.current.find((t) => t.key === key)?.parentKey;
    setFileTabs((list) => {
      const dropped = list.filter((f) => typeof f.termKey === "string" && gone.has(f.termKey)).map((f) => f.key);
      if (dropped.length) {
        setDirtyFiles((prev) => {
          const next = new Set(prev);
          dropped.forEach((k) => next.delete(k));
          return next;
        });
        setActiveFileTab((cur) =>
          cur !== null && dropped.includes(cur) ? null : cur);
      }
      return list.filter((f) => !(typeof f.termKey === "string" && gone.has(f.termKey)));
    });
    setEnded((m) => {
      if (![...gone].some((k) => m.has(k))) return m;
      const next = new Map(m);
      gone.forEach((k) => next.delete(k));
      return next;
    });
    setTabs((t) => {
      const next = t.filter((x) => !gone.has(x.key));
      setActiveTab((cur) => {
        if (cur === null || !gone.has(cur)) return cur;
        // Closing a child lands back on its master; closing a master lands
        // on the last first-row tab, never on someone else's child.
        if (parentOfClosed !== undefined && next.some((x) => x.key === parentOfClosed)) return parentOfClosed;
        return next.filter((x) => x.parentKey === undefined).slice(-1)[0]?.key ?? null;
      });
      return next;
    });
    for (const k of gone) {
      try {
        await tabClose(k);
      } catch (error) {
        uiLog(`tab close failed for ${k}: ${String(error)}`);
      }
    }
    const authoritative = await tabList().catch(() => null);
    if (authoritative) {
      setTabs((current) => reconcileTermTabs(current, authoritative));
    }
  }, []);

  /** Show the start view with the open sessions left running behind it —
   *  the home tab, and where a closed last tab lands. */
  const goHome = useCallback(() => {
    setPreviewSession(null);
    setActiveFileTab(null);
    setActiveTab(null);
  }, []);

  /** Reopen a second agent under its master's row — or just focus it if a
   *  tab for that session is already open somewhere. */
  const reopenBroughtIn = useCallback(async (rootKey: TabId, rec: BroughtIn) => {
    const parent = tabsRef.current.find((t) => t.key === rootKey);
    if (!parent?.cwd) return;
    const open = tabsRef.current.find((t) => t.sessionId === rec.sessionId);
    if (open) { setActiveTab(open.key); setActiveFileTab(null); return; }
    try {
      const plan = await resolveLaunch({ kind: "resume", sessionId: rec.sessionId });
      void openTab(rec.title, parent.cwd, plan.command, rec.sessionId, {
        sessionId: rec.sessionId, resumedId: rec.sessionId, agentId: plan.agent_id, parentKey: rootKey,
      });
      setActiveFileTab(null);
    } catch (e) {
      setNotice(`Couldn't reopen ${rec.title}: ${e}`);
    }
  }, [openTab]);

  /** Tab strip reordering: drop `from` where `to` sits, shifting the rest. */
  const dragKey = useRef<TabId | null>(null);
  const [dragOver, setDragOver] = useState<TabId | null>(null);
  const moveTab = useCallback((from: TabId | null, to: TabId) => {
    if (from === null || from === to) return;
    setTabs((t) => {
      const a = t.findIndex((x) => x.key === from);
      const b = t.findIndex((x) => x.key === to);
      if (a < 0 || b < 0) return t;
      const next = [...t];
      const [moved] = next.splice(a, 1);
      next.splice(b, 0, moved);
      return next;
    });
  }, []);

  // Ctrl+PageDown / Ctrl+PageUp walk the session tabs, wrapping. Not Ctrl+W:
  // that is delete-word to every shell and editor in the terminal.
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      if (!e.ctrlKey || e.altKey || e.metaKey || e.shiftKey) return;
      if (e.key !== "PageDown" && e.key !== "PageUp") return;
      const list = tabsRef.current.filter((t) => t.parentKey === undefined);
      if (list.length === 0) return;
      e.preventDefault();
      const active = tabsRef.current.find((t) => t.key === activeTabRef.current);
      const cur = list.findIndex((t) => t.key === (active?.parentKey ?? active?.key));
      const step = e.key === "PageDown" ? 1 : -1;
      const next = list[(cur + step + list.length) % list.length];
      setPreviewSession(null);
      setActiveFileTab(null);
      setActiveTab(next.key);
      handles.current.get(next.key)?.focus();
    };
    window.addEventListener("keydown", h, true);
    return () => window.removeEventListener("keydown", h, true);
  }, []);

  /** A terminal's process ended. Whether that closes the tab depends entirely
   *  on why.
   *
   *  Exit 0 is someone leaving — `exit` at a shell, `/quit` in claude — and the
   *  tab should go, which is what it has always done. Any other status means
   *  the process died without being asked to, and the most likely cause is now
   *  something outside this window: a session that has moved to the daemon is
   *  listed by `claude agents` everywhere, so it can be killed from another
   *  terminal or from the phone. Closing the tab in that case throws away the
   *  notice that it happened and leaves the transcript — still on disk, still
   *  resumable — to be found by hand. So the tab stays and says so. */
  const handleTermExit = useCallback(
    (key: TabId, code: number | null, signal: string | null) => {
      if (code === 0) {
        void closeTab(key);
        return;
      }
      setEnded((m) => new Map(m).set(key, { code, signal }));
    },
    [closeTab],
  );

  /** Put an ended tab back, resuming its conversation where it stopped. The
   *  slot is reused so the sidebar row it belongs to stays the same row. */
  const restartEnded = useCallback(
    async (key: TabId) => {
      const t = tabsRef.current.find((x) => x.key === key);
      if (!t) return;
      await closeTab(key);
      // A tab with no session id has no conversation to continue — a shell, or
      // an engine that never named itself — so it reopens on the command it
      // was launched with. So does one whose engine declines to reopen: the
      // resolver saying no is exactly what that fallback is for.
      let command = t.command;
      let resumedId = t.resumedId;
      // The engine carries over when the plan is declined, because the tab is
      // reopening on the very command it ran before — same engine, so the same
      // capabilities.
      let agentId = t.agentId;
      if (t.sessionId) {
        try {
          const plan = await resolveLaunch({ kind: "restart", sessionId: t.sessionId });
          command = plan.command;
          resumedId = t.sessionId;
          agentId = plan.agent_id;
        } catch {
          /* nothing here can reopen it — keep the tab's own command */
        }
      }
      // The provider environment carries over too: it is what gives the tab
      // its key and its routing, and a restart that dropped it would reopen
      // the same command against no provider at all.
      openTab(t.title, t.cwd, command, t.slotId, {
        sessionId: t.sessionId, resumedId, agentId,
        envProvider: t.envProvider, envModel: t.envModel,
      });
    },
    [closeTab, openTab],
  );

  /** Show the terminal a slot names. Used by the placeholder rows, which have
   *  no session on disk to select in the ordinary way. */
  const focusSlot = useCallback((slotId: string) => {
    const t = tabsRef.current.find((x) => x.slotId === slotId);
    if (!t) return;
    setPreviewSession(null);
    setActiveTab(t.key);
  }, []);
  const closeSlot = useCallback((slotId: string) => {
    const pending = pendingOpens.current.get(slotId);
    if (pending) pending.cancelled = true;
    const t = tabsRef.current.find((x) => x.slotId === slotId);
    if (t) void closeTab(t.key);
  }, [closeTab]);

  const selectSession = (s: Session) => {
    setActiveProject(s.project_path);
    // Warp-style: the sidebar is the tab list — switch to this item's live
    // terminal if it has one (resume first, then a project shell). Without
    // one, show a read-only conversation preview so ▶ can be an informed
    // choice.
    const live =
      tabs.find((t) => t.slotId === s.id) ??
      tabs.find((t) => t.slotId === `shell:${s.project_path}`);
    if (live) {
      setPreviewSession(null);
      setActiveTab(live.key);
    } else {
      setPreviewSession(s);
    }
  };
  const resumeSession = async (s: Session) => {
    setActiveProject(s.project_path);
    // Ask the pinned id first, because the plan is also how we learn which
    // engine this row belongs to — and everything below is claude's.
    let plan;
    try {
      plan = await resolveLaunch({ kind: "resume", sessionId: s.id });
    } catch (e) {
      setNotice(`${e}`);
      return;
    }
    // The preamble — moved-to resolution, fork redemption, roster stops — is
    // the live-process machinery of an engine that drives a TUI and holds its
    // sessions open. An engine without it (`aiterm chat` reloads a transcript
    // and carries on) must not be put through any of it: the id it was given
    // is the id it resumes. Gated on the capability rather than on the agent's
    // name, which is the check this refactor exists to remove.
    if (!plan.caps.tui_drive) {
      // The env rides along for the same reason it does on a fresh API
      // launch: a resumed OpenCode tab without it answers "Authentication
      // Error" and falls over to the engine's default model.
      openTab(s.title, s.project_path, plan.command, s.id, {
        sessionId: s.id, resumedId: s.id, agentId: plan.agent_id,
        envProvider: plan.env_provider ?? undefined,
        envModel: plan.env_model ?? undefined,
      });
      return;
    }
    // The pinned id can go stale: a compaction can retire the original
    // transcript (Claude Code deletes it or renames it to `<id>.orphaned-…`),
    // so `claude --resume <original-id>` dies with "no conversation found" — a
    // black pane, the core "broken feel" of resume. (`/clear` was listed here
    // too and does not belong: measured 2026-07-29 on Claude Code 2.1.220, it
    // leaves the original named and resumable and simply starts a new session
    // beside it. That is why it needed detecting rather than resolving.)
    // Resolve to the surviving continuation first; if nothing resumable is
    // left, say so instead of launching a doomed resume. (A forked parent is
    // NOT stale — forking leaves it intact, and it resolves to itself.)
    let liveId = s.id;
    try {
      let resolved = await resolveResumableId(s.id);
      // Nothing resumable — but a `/fork` row is a special case worth rescuing.
      // It has no conversation of its own, only a promise in job state to hold
      // the parent's history up to the fork. Redeem it, then ask again. Every
      // other kind of empty session fails this and falls through to the toast.
      if (resolved === null) {
        try {
          await materializeFork(s.id);
          resolved = await resolveResumableId(s.id);
          refreshSessionList();
        } catch {
          /* not a redeemable fork; the toast below is the right answer */
        }
      }
      if (resolved === null) {
        setNotice(`"${s.title}" was cleared or superseded — no resumable transcript remains.`);
        return;
      }
      liveId = resolved;
    } catch {
      liveId = s.id; // resolver unavailable → fall back to the pinned id
    }
    // Resume the way you would from a shell: if the conversation is still
    // running, close it, then `claude --resume <id>`. `--resume` refuses a live
    // session ("…add --fork-session to branch off a copy"), and the old answer
    // to that was to offer ⑂ instead — which made branching a copy the only
    // way back into your own conversation, and minted an immortal fork on
    // every attempt. Stopping first is what the user actually means by "open
    // this session".
    //
    // Our own tab goes through closeTab so React state stays in step; anything
    // else is signalled through the roster.
    //
    // But ask the roster *first*, because closing comes before stopping and
    // the two can disagree. A session the daemon holds has no pid to signal —
    // a conversation moves there on its own the moment you open the agents
    // view — so `stopSession` can only poll for five seconds and give up. It
    // used to give up having already closed the tab it was going to reuse,
    // which turned "resume this" into "lose this", and the resume never
    // happened either. Nothing is touched until we know a stop can succeed.
    try {
      const held = await unstoppableSessionIds();
      if (held.includes(liveId) || held.includes(s.id)) {
        setNotice(
          `"${s.title}" is running under the Claude Code daemon, so aiterm can't stop it. ` +
            `Stop it from \`claude agents\`, then resume.`,
        );
        return;
      }
    } catch {
      /* roster unavailable — fall through and let stopSession be the judge */
    }
    // Match on both ids: a tab opened before a compaction is slotted under the
    // row's pinned id, not the continuation `liveId` resolves to.
    await Promise.all(tabsRef.current
      .filter((t) => t.slotId === liveId || t.slotId === s.id)
      .map((t) => closeTab(t.key)));
    try {
      await stopSession(liveId);
      if (liveId !== s.id) await stopSession(s.id);
    } catch (e) {
      setNotice(`Couldn't stop "${s.title}" to resume it: ${e}`);
      return;
    }
    // Resume the same way we start anything: the engine's own command, which
    // carries the same flags a fresh start gets. (This used to pass
    // `--permission-mode <configured>`, which is what silently lifted a manual
    // session into bypass on resume.) Re-resolve when the id moved — the plan
    // above names the pinned session, and the continuation is a different
    // conversation to open.
    if (liveId !== s.id) {
      try {
        plan = await resolveLaunch({ kind: "resume", sessionId: liveId });
      } catch (e) {
        setNotice(`${e}`);
        return;
      }
    }
    openTab(s.title, s.project_path, plan.command, liveId, {
      sessionId: liveId, resumedId: liveId, agentId: plan.agent_id,
    });
  };
  // Branch a session. You stay exactly where you are: the backend copies the
  // transcript under a fresh id and the branch appears in the sidebar as a
  // stopped row holding the conversation up to this point — the same shape as
  // `/clear`, deliberately. One rule for both: the tab you're in never
  // changes out from under you, and the other conversation (old context for
  // a clear, the copy for a fork) is a normal stopped row — preview on
  // click, ▶ to resume.
  //
  // (Briefly, 0.10.9 opened the branch live and switched to it. Matt: no —
  // stay in session A; the branch is the stopped one.) Rejections are
  // surfaced by the caller (see `onFork` below).
  const forkSession = async (s: Session) => {
    const branchId = await sessionFork(s.id);
    refreshSessionList();
    setNotice(`Branched "${s.title}" — the copy is in the sidebar, stopped at this point.`);
    return branchId;
  };
  // aiterm's own clear, kin to ⑂ and deliberately off claude's machinery:
  // end the running claude and start a fresh one in the same tab on an id the
  // plan names. The old conversation needs nothing done to it — its transcript
  // is already on disk, so it simply becomes a stopped row, exactly the shape
  // a typed /clear settles into. Costs a claude restart where /clear is warm;
  // in exchange there is no hook, no detection and no id change to chase — the
  // id in the command and the id this tab is keyed to came from the same
  // place, so aiterm knows everything from the first frame.
  const clearSession = async (s: Session) => {
    const t = tabs.find((x) => x.slotId === s.id);
    if (!t) return;
    let plan;
    try {
      plan = await resolveLaunch({ kind: "clear", sessionId: s.id });
    } catch (e) {
      setNotice(`${e}`);
      return;
    }
    // No id on the plan means nothing would name the new conversation, and a
    // tab keyed to nothing is the unreachable-tab bug — say so instead.
    if (!plan.session_id) {
      setNotice(`Clearing isn't available for "${s.title}".`);
      return;
    }
    await closeTab(t.key);
    // The client process is going down, but the roster reports it for a beat
    // longer — same suppress-until-reopened rule as a hook-observed move.
    setVacated((prev) => new Set(prev).add(s.id));
    openTab(
      basename(s.project_path), s.project_path, plan.command, plan.session_id,
      { sessionId: plan.session_id, fresh: true, agentId: plan.agent_id },
    );
    setNotice(`"${s.title}" is parked in the sidebar — this tab is a fresh conversation.`);
  };
  // Exit an active session: close its live terminal tab (ends the running
  // claude process). The transcript stays on disk, so it's resumable later.
  const exitSession = (s: Session) => {
    const live = tabs.find((t) => t.slotId === s.id);
    if (live) void closeTab(live.key);
  };
  const newShell = (s: Session) => {
    setActiveProject(s.project_path);
    openTab(basename(s.project_path), s.project_path, null, `shell:${s.project_path}`);
  };
  const deleteSession = async (s: Session) => {
    try {
      await sessionDelete(s.id);
    } catch (e) {
      console.error("delete failed:", e);
    }
    setPreviewSession((p) => (p?.id === s.id ? null : p));
    refreshSessions();
  };
  /** Move a set of sessions to the trash — the right-click action on a project
   *  or group header. Sequential rather than parallel: each delete renames a
   *  transcript and its task and job records, and firing thirty of those at the
   *  same directory at once is how a half-moved session happens. One refresh at
   *  the end, so the sidebar redraws once instead of thirty times.
   *
   *  Failures are counted and reported. A per-row delete can afford to fail
   *  quietly — you can see the row is still there — but in a set of thirty,
   *  four that did not move would be invisible. */
  const trashSessions = async (list: Session[]) => {
    if (list.length === 0) return;
    let failed = 0;
    for (const s of list) {
      try {
        await sessionDelete(s.id);
      } catch (e) {
        failed += 1;
        uiLog(`bulk trash failed for ${s.id}: ${e}`);
      }
    }
    const moved = list.length - failed;
    setPreviewSession((p) => (p && list.some((s) => s.id === p.id) ? null : p));
    refreshSessions();
    setNotice(
      failed === 0
        ? `Moved ${moved} session${moved === 1 ? "" : "s"} to the trash.`
        : `Moved ${moved} of ${list.length}; ${failed} could not be trashed.`,
    );
  };

  const restoreTrashed = async (id: string) => {
    try {
      await trashRestore(id);
    } catch (e) {
      console.error("restore failed:", e);
    }
    refreshSessions();
  };
  const deleteTrashed = async (id: string) => {
    try {
      await trashDelete(id);
    } catch (e) {
      console.error("trash delete failed:", e);
    }
    refreshSessions();
  };
  const emptyTrash = async () => {
    try {
      await trashEmpty();
    } catch (e) {
      console.error("empty trash failed:", e);
    }
    refreshSessions();
  };
  const selectProject = (p: ProjectInfo) => {
    setActiveProject(p.path);
    const live =
      tabs.find((t) => t.slotId === `claude:${p.path}`) ??
      tabs.find((t) => t.slotId === `shell:${p.path}`);
    if (live) setActiveTab(live.key);
  };
  const projectShell = (p: ProjectInfo) => {
    setActiveProject(p.path);
    openTab(p.name, p.path, null, `shell:${p.path}`);
  };
  const projectClaude = async (p: ProjectInfo) => {
    setActiveProject(p.path);
    try {
      // Naming claude is honest here: this is the "start claude here" button,
      // not a choice the user made in a picker.
      const plan = await resolveLaunch({
        kind: "agent", agentId: "claude", model: null, effort: null,
      });
      // The plan's minted session id is deliberately dropped. This tab is
      // slotted by its project door — `claude:<path>`, one terminal per
      // project — which is what makes clicking the row focus the terminal that
      // is already open rather than starting a second claude in it. Keying it
      // to the session instead would put a placeholder row in the sidebar for
      // a door that already has one.
      openTab(p.name, p.path, plan.command, `claude:${p.path}`, {
        agentId: plan.agent_id,
      });
    } catch (e) {
      setNotice(`Couldn't start claude in ${p.name}: ${e}`);
    }
  };

  /**
   * Start a fresh claude session in `cwd`.
   *
   * The one thing aiterm could not do: every other door into a terminal needs
   * a row to open it from — resume a session, branch one, open a shell beside
   * one, or ▶ a project that has no sessions yet. A conversation that has
   * never existed has no row, so beginning one meant opening a shell and
   * typing `claude` by hand, and on a machine with no `~/Projects` there was
   * no project row either.
   *
   * **The id comes from the plan, not from here.** Letting the engine choose it
   * silently is the reason the first version of this was unusable: a session
   * has no transcript until its first prompt, the sidebar lists what is on
   * disk, and the sidebar is the tab list — so a new session opened a terminal
   * that no row named. It took the pane over from whatever you were looking at
   * and there was no way back to it. `--session-id` composes with a fresh start
   * (unlike with `--resume`, where it is rejected without `--fork-session`), so
   * for an engine that takes one the tab is slotted by its session id from the
   * first frame, exactly like a resume — which is also what lets the composer
   * pills and the agents panel key to it before it has said anything. The
   * resolver mints it, because only the backend knows whether an id will mean
   * anything.
   *
   * *Verified 2026-07-27: `claude --session-id <uuid> --permission-mode auto
   * --allow-dangerously-skip-permissions -p …` wrote its transcript to
   * `<uuid>.jsonl` under the launch cwd.*
   *
   * `fresh` covers the gap until that file exists — see `pendingSessions`.
   */
  const newSession = useCallback(async (
    cwd: string,
    choice: StartChoice,
    prompt?: string,
    extra: { parentKey?: TabId; title?: string; permissionFlags?: string } = {},
  ): Promise<{ key: TabId; sessionId?: string } | null> => {
    // An API-provider model is a request for a model, not for an engine —
    // which one runs it is the resolver's answer. The branch survives because
    // the two are presented differently: a model tab is titled by its model
    // and gets no placeholder row, an agent tab is titled by its directory and
    // does.
    //
    // A `switch` on the kind rather than a test for a field, because the choice
    // is one thing or the other and used to be a shape that could be both: an
    // agent id sat beside an optional `api` object, and "which of these two do
    // I mean" was a runtime question answered by whichever field was checked
    // first. There is nothing left to check.
    switch (choice.kind) {
      case "api": {
        try {
          const title = choice.modelId.split("/").pop() || choice.modelId;
          const plan = await resolveLaunch({
            kind: "apiModel",
            providerId: choice.providerId,
            modelId: choice.modelId,
            prompt: prompt || null,
          });
          setActiveProject(cwd);
          // A tab handle where the plan names no session: an engine that writes
          // no transcript we can read has nothing to key panels to, and the tab
          // is the whole life of that conversation. Where it does name one, the
          // chat becomes a real sidebar row once its first exchange lands.
          //
          // `env_provider` rides along so the spawn can put the key in the
          // tab's environment — set only for an engine that has said it
          // authenticates no other way. `env_model` names the model whose
          // routing goes in beside it; the routing is compiled in Rust.
          const key = await openTab(extra.title ?? title, cwd, plan.command, plan.session_id ?? crypto.randomUUID(), {
            sessionId: plan.session_id ?? undefined,
            envProvider: plan.env_provider ?? undefined,
            envModel: plan.env_model ?? undefined,
            agentId: plan.agent_id,
            parentKey: extra.parentKey,
          });
          if (key === null) return null;
          return { key, sessionId: plan.session_id ?? undefined };
        } catch (e) {
          setNotice(`Couldn't start ${choice.modelId}: ${e}`);
        }
        return null;
      }

      case "agent": {
        setActiveProject(cwd);
        try {
          const plan = await resolveLaunch({
            kind: "agent",
            agentId: choice.agentId,
            model: choice.model,
            effort: choice.effort,
            prompt: prompt || null,
            permissionFlags: extra.permissionFlags ?? null,
          });
          // No session id on the plan means the engine would not take one —
          // Codex has no `--session-id` — so the slot is a tab handle: the
          // placeholder row keeps the tab reachable, but nothing is keyed to it
          // as a session, because there is no session by that name and never
          // will be.
          const key = await openTab(extra.title ?? basename(cwd), cwd, plan.command, plan.session_id ?? crypto.randomUUID(), {
            sessionId: plan.session_id ?? undefined,
            fresh: true,
            agentId: plan.agent_id,
            parentKey: extra.parentKey,
            // An agent we could not name has to be identified after the fact.
            // Snapshot what already exists so adoption can tell its session
            // from one that was open before this tab did anything.
            adopt: plan.session_id
              ? undefined
              : {
                  agentId: plan.agent_id,
                  since: Date.now(),
                  known: sessionsRef.current
                    .filter((s) => s.project_path === cwd)
                    .map((s) => s.id),
                },
          });
          if (key === null) return null;
          return { key, sessionId: plan.session_id ?? undefined };
        } catch (e) {
          setNotice(`Couldn't start ${choice.agentId}: ${e}`);
        }
        return null;
      }
    }
  }, [openTab]);

  /** A deliberately bounded, read-only conversation between the active
   *  session and a second agent. Both remain ordinary Rust-owned tabs; the
   *  relay only composes messages between their terminal handles. */
  const progressRef = useRef(progress);
  progressRef.current = progress;
  const relayCtl = useRelay({
    prompts: () => settings.bringIn,
    tabs: () => tabsRef.current,
    handle: (key) => handles.current.get(key),
    quietFor: (key) => Date.now() - (lastOutput.current.get(key) ?? 0),
    busy: (key) => progressRef.current.has(key),
    open: async (cwd, choice, prompt, extra) => {
      const opened = await newSession(cwd, choice, prompt, extra);
      // Remember who was brought in, by the master session's id, so the row
      // can offer them back after their tab — or the day — ends.
      const parent = tabsRef.current.find((t) => t.key === extra.parentKey);
      if (opened?.sessionId && parent?.sessionId) {
        const rec: BroughtIn = { sessionId: opened.sessionId, agentId: choice.kind === "agent" ? choice.agentId : "api", title: extra.title, at: Date.now() };
        setBroughtIn((m) => ({ ...m, [parent.sessionId!]: [...(m[parent.sessionId!] ?? []).filter((r) => r.sessionId !== rec.sessionId), rec] }));
      }
      return opened;
    },
  });
  const [showBringIn, setShowBringIn] = useState(false);

  /** The empty pane's own source/model/effort, so its button starts the same
   *  session the ＋ menu would. It used to take the first installed agent on
   *  its defaults — a different session from the one the menu offers, with
   *  nothing on the button to say so. */
  const emptyCtl = useStartChoice(settingsGen);
  /** Where the home screen's prompt box starts its session. The project the
   *  sidebar has selected, or the one worked in most recently — the folder
   *  chip on the box swaps it, and holds the swap until the next selection. */
  const [homeCwdPick, setHomeCwdPick] = useState<string | null>(null);
  useEffect(() => { setHomeCwdPick(null); }, [activeProject]);
  const homeCwd = homeCwdPick ?? activeProject
    ?? [...sessions].sort((a, b) => b.last_active - a.last_active)[0]?.project_path ?? null;
  const pickHomeCwd = useCallback(async () => {
    try {
      const picked = await openDialog({ directory: true, title: "Start the session in…" });
      if (typeof picked === "string") setHomeCwdPick(picked);
    } catch { /* cancelled */ }
  }, []);
  /** The prompt box's submit: a session in `homeCwd` with the prompt as its
   *  first message, or an empty one when nothing was typed. No folder known
   *  yet means ask for one first. */
  const launchFromHome = useCallback(async (prompt: string) => {
    if (!emptyCtl.ready) {
      setNotice("Nothing to start yet — set up the API tab, or install claude, codex, grok or antigravity.");
      return;
    }
    let cwd = homeCwd;
    if (!cwd) {
      try {
        const picked = await openDialog({ directory: true, title: "Start the session in…" });
        if (typeof picked !== "string") return;
        cwd = picked;
      } catch { return; }
    }
    void newSession(cwd, emptyCtl.choice(), prompt.trim() || undefined);
  }, [newSession, emptyCtl, homeCwd]);

  /**
   * What the desktop knows about its own tabs, in the shape the fleet board
   * wants it: session ids, not tab keys.
   *
   * The board asks the spine first and only falls back to this, so what it is
   * really for is the cold start (before the first poll answers) and the
   * sessions the spine has no log for. `otherAlerts` is the leftover the
   * mapping cannot express — a plain shell waiting on you is not a session,
   * and dropping it would make the board quieter than the truth.
   */
  const homeFleetTabs = useMemo(() => {
    const ids = new Set(sessions.map((s) => s.id));
    const liveIds = new Set<string>();
    const attentionIds = new Set<string>();
    const busyIds = new Set<string>();
    const sessionTabs = new Set<TabId>();
    for (const t of tabs) {
      if (!t.slotId || !ids.has(t.slotId)) continue;
      sessionTabs.add(t.key);
      liveIds.add(t.slotId);
      // These read App's own `attention`/`progress` maps (keyed by tab); the
      // sets being built used to shadow them, so the fallback was always empty.
      if (attention.has(t.key)) attentionIds.add(t.slotId);
      if (progress.has(t.key)) busyIds.add(t.slotId);
    }
    return {
      live: liveIds, attention: attentionIds, busy: busyIds,
      otherAlerts: alerts.filter((a) => !sessionTabs.has(a.key)),
    };
  }, [sessions, tabs, attention, progress, alerts]);

  /** The spine's per-session phase, polled for the whole window — the
   *  sidebar lanes, the badge and the palette all read it, not only home. */
  const overview = useSpineOverview(true);

  /** Every session blocked on a person, most recent first — the queue the
   *  Ctrl+Shift+A hotkey walks and the badge counts. */
  const blocked = useMemo(
    () => buildFleet({
      sessions, overview,
      live: homeFleetTabs.live, attention: homeFleetTabs.attention, busy: homeFleetTabs.busy, cap: 0,
    }).needsYou.map((r) => r.session),
    [sessions, overview, homeFleetTabs],
  );
  const blockedRef = useRef(blocked);
  blockedRef.current = blocked;
  // `selectSession` closes over `tabs`; the hotkey must see the current one.
  const selectSessionRef = useRef(selectSession);
  selectSessionRef.current = selectSession;
  const otherAlertsRef = useRef(homeFleetTabs.otherAlerts);
  otherAlertsRef.current = homeFleetTabs.otherAlerts;
  const blockedCount = blocked.length + homeFleetTabs.otherAlerts.length;

  /** Jump to the next session waiting on you, cycling from the one on
   *  screen — so clearing a queue of four is four presses, no reading. */
  const goNextBlocked = useCallback(() => {
    const queue = blockedRef.current;
    const shells = otherAlertsRef.current;
    if (queue.length === 0 && shells.length === 0) return;
    const cur = tabsRef.current.find((t) => t.key === activeTabRef.current);
    const idx = cur ? queue.findIndex((s) => s.id === cur.slotId) : -1;
    if (idx >= 0 && idx + 1 >= queue.length && shells.length > 0) {
      setPreviewSession(null); setActiveFileTab(null); setActiveTab(shells[0].key);
      return;
    }
    const next = queue[(idx + 1) % Math.max(1, queue.length)];
    if (next) { selectSessionRef.current(next); return; }
    setPreviewSession(null); setActiveFileTab(null); setActiveTab(shells[0].key);
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const [paletteOpen, setPaletteOpen] = useState(false);

  /** What the palette can reach. Rebuilt when any input moves; the palette
   *  ranks it per keystroke. Sessions are capped — search in the sidebar
   *  reaches the archive; the palette is for what you were just doing. */
  const paletteItems = useMemo((): PaletteItem[] => {
    const items: PaletteItem[] = [];
    const seen = new Set<string>();
    for (const s of blocked) {
      seen.add(s.id);
      const ov = overview.get(s.id);
      items.push({
        id: `need:${s.id}`, group: "Needs you", rank: 0, agent: s.agent,
        title: s.title, subtitle: ov?.detail || homeAbbrev(s.project_path), keywords: s.agent,
        run: () => selectSession(s),
      });
    }
    for (const a of homeFleetTabs.otherAlerts) {
      items.push({
        id: `alert:${a.key}`, group: "Needs you", rank: 0, title: a.title, subtitle: a.message ?? "Waiting for your input",
        run: () => { setPreviewSession(null); setActiveFileTab(null); setActiveTab(a.key); },
      });
    }
    for (const t of tabs.filter((t) => t.parentKey === undefined)) {
      items.push({
        id: `tab:${t.key}`, group: "Open tabs", rank: 1, title: t.title, subtitle: t.cwd ? homeAbbrev(t.cwd) : undefined,
        agent: t.agentId, keywords: t.agentId ?? "", run: () => { setPreviewSession(null); setActiveFileTab(null); setActiveTab(t.key); },
      });
    }
    const recent = [...sessions].sort((a, b) => b.last_active - a.last_active).slice(0, 200);
    for (const s of recent) {
      if (seen.has(s.id)) continue;
      items.push({
        id: `s:${s.id}`, group: "Sessions", rank: 2, title: s.title, agent: s.agent,
        subtitle: homeAbbrev(s.project_path) + (s.branch ? `  ${s.branch}` : ""), keywords: s.agent,
        run: () => selectSession(s),
      });
    }
    const where = homeCwd ? homeAbbrev(homeCwd) : "";
    for (const a of emptyCtl.agents) {
      items.push({
        id: `new:${a.id}`, group: "Actions", rank: 3, title: `New ${a.display_name} session`, subtitle: where, keywords: "start launch", agent: a.id,
        run: () => { if (homeCwd) void newSession(homeCwd, { kind: "agent", agentId: a.id, model: null, effort: null }); },
      });
    }
    const act = (id: string, title: string, run: () => void, keywords = "") =>
      items.push({ id: `act:${id}`, group: "Actions", rank: 3, title, keywords, run });
    act("next", "Next session that needs you", goNextBlocked, "blocked attention jump ctrl+shift+a");
    act("home", "Home", goHome, "dashboard launcher");
    act("sessions", (showSessions ? "Hide" : "Show") + " sessions sidebar", () => setShowSessions(!showSessions), "panel toggle");
    act("explorer", (showExplorer ? "Hide" : "Show") + " file explorer", () => setShowExplorer(!showExplorer), "panel toggle files");
    act("git", (showGit ? "Hide" : "Show") + " repository panel", () => setShowGit(!showGit), "panel toggle git");
    act("agent", (showAgent ? "Hide" : "Show") + " tasks panel", () => setShowAgent(!showAgent), "panel toggle agent");
    act("composer", (showComposer ? "Hide" : "Show") + " composer", () => setShowComposer(!showComposer), "panel toggle input");
    act("settings", "Settings", () => setShowSettingsModal(true), "preferences theme");
    act("refresh", "Refresh sessions", () => { void refreshSessions(); }, "reload reindex");
    for (const p of projects) {
      items.push({
        id: `proj:${p.path}`, group: "Projects", rank: 4, title: homeAbbrev(p.path), keywords: "project folder",
        run: () => selectProject(p),
      });
    }
    return items;
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [blocked, overview, homeFleetTabs.otherAlerts, tabs, sessions, projects, homeCwd, emptyCtl.agents,
      showSessions, showExplorer, showGit, showAgent, showComposer, goNextBlocked]);

  // Ctrl+Shift+P: the palette. Ctrl+Shift+A: next session that needs you.
  // Ctrl+1…9: the Nth session tab. All captured before xterm, like the rest.
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      if (!e.ctrlKey || e.altKey || e.metaKey) return;
      if (e.shiftKey && (e.key === "P" || e.key === "p")) {
        e.preventDefault(); setPaletteOpen((v) => !v); return;
      }
      if (e.shiftKey && (e.key === "A" || e.key === "a")) {
        e.preventDefault(); goNextBlocked(); return;
      }
      if (e.shiftKey) return;
      if (e.key >= "1" && e.key <= "9") {
        const list = tabsRef.current.filter((t) => t.parentKey === undefined);
        const t = list[Number(e.key) - 1];
        if (!t) return;
        e.preventDefault();
        setPreviewSession(null); setActiveFileTab(null); setActiveTab(t.key);
        handles.current.get(t.key)?.focus();
      }
    };
    window.addEventListener("keydown", h, true);
    return () => window.removeEventListener("keydown", h, true);
  }, [goNextBlocked]);

  /** "Show all" on the board: the whole list is the sidebar's job, so open it
   *  and put the cursor in its search box rather than growing a second one. */
  const showAllSessions = useCallback(() => {
    setShowSessions(true);
    requestAnimationFrame(() => {
      document.querySelector<HTMLInputElement>(".panel.sessions .search-input")?.focus();
    });
  }, []);

  /** The quiet half of the home screen's action row: a shell in the working
   *  folder, on the same slot id the sidebar's "new shell" uses, so the two
   *  never open two terminals on one directory. */
  const openHomeTerminal = useCallback(async () => {
    let cwd = homeCwd;
    if (!cwd) {
      try {
        const picked = await openDialog({ directory: true, title: "Open a terminal in…" });
        if (typeof picked !== "string") return;
        cwd = picked;
      } catch { return; }
    }
    void openTab(basename(cwd), cwd, null, `shell:${cwd}`);
  }, [homeCwd, openTab]);

  // --- splitter dragging ---
  const dragging = useRef<null | "left" | "right" | "rightsplit" | "agentsplit">(null);
  const rightColRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const move = (e: MouseEvent) => {
      if (!dragging.current) return;
      e.preventDefault();
      if (dragging.current === "left") {
        setSizes((s) => ({
          ...s,
          left: Math.max(140, Math.min(window.innerWidth - 260, e.clientX)),
        }));
      } else if (dragging.current === "right") {
        setSizes((s) => ({
          ...s,
          right: Math.max(150, Math.min(window.innerWidth - 260, window.innerWidth - e.clientX)),
        }));
      } else if (dragging.current === "rightsplit" && rightColRef.current) {
        const r = rightColRef.current.getBoundingClientRect();
        const frac = (e.clientX - r.left) / r.width;
        setSizes((s) => ({ ...s, explorerFrac: Math.max(0.15, Math.min(0.85, frac)) }));
      } else if (dragging.current === "agentsplit" && rightColRef.current) {
        const r = rightColRef.current.getBoundingClientRect();
        const frac = (r.bottom - e.clientY) / r.height;
        setSizes((s) => ({ ...s, agentFrac: Math.max(0.12, Math.min(0.7, frac)) }));
      }
    };
    const up = () => {
      dragging.current = null;
      document.body.classList.remove("dragging");
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
    return () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
    };
  }, []);

  const startDrag = (which: "left" | "right" | "rightsplit" | "agentsplit") => {
    dragging.current = which;
    document.body.classList.add("dragging");
  };

  /** The home screen has the centre pane. Nothing else is on screen: no
   *  session, no preview, no file. */
  const onHome = activeTab === null && !previewSession && !fileOnScreen;
  /**
   * Explorer and Agent are about a session, and home has none — so on home
   * they were two empty columns saying "select a project" and "tasks appear
   * here", taking a third of the width to tell you nothing. They are not
   * rendered here. The toggles are untouched: the choice is remembered, it
   * just has nothing to show yet, and both panels come back the moment a
   * session tab is on screen.
   *
   * Repository is not in this list on purpose. It reads `activeProject`,
   * which the sidebar sets whether or not a tab is open, so it has real
   * content on home.
   */
  const explorerOnScreen = showExplorer && !onHome;
  const agentOnScreen = showAgent && !onHome;
  const showRight = explorerOnScreen || showGit;

  const timeFormatCtx = useMemo(() => ({
    format: settings.timeFormat,
    setFormat: (f: AppSettings["timeFormat"]) => setSettings((s) => ({ ...s, timeFormat: f })),
  }), [settings.timeFormat]);

  // The phone watches the relay too: report each phase change against the
  // sessions the two tabs run, so a phone looking at either sees the crew.
  // One relay runs at a time: a new bring-in supersedes one in flight. The
  // superseded one is reported stopped against ITS session, or the phone
  // watching that session would show "waiting" for ever.
  const lastRelayReport = useRef<{ aSid: string; bSid: string | null; bName: string; round: number; rounds: number; inFlight: boolean } | null>(null);
  useEffect(() => {
    const r = relayCtl.relay;
    if (!r) return;
    const aTab = tabs.find((t) => t.key === r.aKey);
    if (!aTab?.sessionId) return;
    const bTab = tabs.find((t) => t.key === r.bKey);
    const prev = lastRelayReport.current;
    if (prev && prev.inFlight && prev.aSid !== aTab.sessionId) {
      relayReport(prev.aSid, prev.bSid, prev.bName, "stopped", prev.round, prev.rounds, "replaced by a newer bring-in").catch(() => {});
    }
    const inFlight = r.phase === "opening" || r.phase === "waitB" || r.phase === "waitA";
    lastRelayReport.current = { aSid: aTab.sessionId, bSid: bTab?.sessionId ?? null, bName: r.bName, round: r.round, rounds: r.rounds, inFlight };
    relayReport(aTab.sessionId, bTab?.sessionId ?? null, r.bName, r.phase, r.round, r.rounds, r.note).catch(() => {});
  }, [relayCtl.relay, tabs]);

  // ---- Remote access: a phone asks, the desktop opens. The tab appears here
  // too, so both screens agree about what is running. Refs, not deps: the
  // handlers below are recreated every render and the listener must not be.
  const remoteRef = useRef({ resumeSession, selectSession, newSession, tabs, relayStart: relayCtl.start });
  remoteRef.current = { resumeSession, selectSession, newSession, tabs, relayStart: relayCtl.start };
  useEffect(() => {
    const unOpen = listen<{ sessionId: string }>("remote://open-session", async (e) => {
      const id = e.payload.sessionId;
      // The phone's list can be newer than ours — read fresh rather than trust state.
      const list = sessionsRef.current.find((x) => x.id === id) ? sessionsRef.current : await listSessions();
      const s = list.find((x) => x.id === id);
      if (!s) { setNotice(`The phone asked for a session that is not listed: ${id.slice(0, 8)}…`); return; }
      // A session already open in a tab is FOCUSED, not resumed. `resumeSession`
      // stops the live process and relaunches `--resume`, and under the daemon
      // Claude Code answers a resume of a live conversation with a FORK — a
      // second transcript with a new id and the same history. The phone asks
      // to open whenever its "open" list is a refresh behind, so this path
      // minted a duplicate "Missing notes on CRM opportunity" [2026-09-03].
      const live = remoteRef.current.tabs.find((t) => t.slotId === s.id || t.sessionId === s.id);
      uiLog(`remote open-session ${id.slice(0, 8)} → ${live ? "focus tab " + live.key : "resume"}`);
      if (live) remoteRef.current.selectSession(s);
      else remoteRef.current.resumeSession(s);
    });
    const unNew = listen<{ agentId: string; cwd: string; prompt: string | null; model: string | null; effort: string | null; title: string | null }>("remote://new-session", (e) => {
      const { agentId, cwd, prompt, model, effort, title } = e.payload;
      // An api:<provider> id is a model off a provider's list, not a CLI —
      // the same routing bring-in does. Left unrouted it reached the
      // resolver as an agent and died as "api:… isn't installed", which is
      // what a phone asking for a local model used to get back.
      if (agentId.startsWith("api:")) {
        if (!model) { setNotice("The phone asked for a provider model but named no model"); return; }
        void remoteRef.current.newSession(
          cwd, { kind: "api", providerId: agentId.slice(4), modelId: model }, prompt ?? undefined,
          title ? { title } : {},
        );
        return;
      }
      void remoteRef.current.newSession(
        cwd, { kind: "agent", agentId, model: model ?? null, effort: effort ?? null }, prompt ?? undefined,
        title ? { title } : {},
      );
    });
    const unBring = listen<{ session_id: string; kind?: string; agent_id: string; provider_id?: string | null; model: string | null; effort: string | null; focus: string; rounds: number }>("remote://bring-in", (e) => {
      const p = e.payload;
      const tab = remoteRef.current.tabs.find((t) => t.sessionId === p.session_id);
      if (!tab) { setNotice("The phone asked to bring in a second agent, but that session has no tab here"); return; }
      const choice: StartChoice = p.kind === "api" && p.provider_id && p.model
        ? { kind: "api", providerId: p.provider_id, modelId: p.model }
        : { kind: "agent", agentId: p.agent_id, model: p.model ?? null, effort: p.effort ?? null };
      void remoteRef.current.relayStart({ aKey: tab.key, choice, focus: p.focus ?? "", rounds: p.rounds ?? 2, auto: (p as { auto?: boolean }).auto ?? false });
    });
    return () => {
      unOpen.then((f) => f());
      unNew.then((f) => f());
      unBring.then((f) => f());
    };
  }, []);

  return (
    <TimeFormatContext.Provider value={timeFormatCtx}>
    <div className="app">
      {notice && (
        <div className="app-toast" role="status" onClick={() => setNotice(null)}>
          {notice}
        </div>
      )}
      <div className="topbar">
        <div className="topbar-left">
          <button
            className={"icon-btn" + (showSessions ? " on" : "")}
            title={blockedCount > 0 ? `Toggle sessions panel — ${blockedCount} waiting on you` : "Toggle sessions panel"}
            onClick={() => setShowSessions(!showSessions)}
          >
            <Icon of={PanelLeft} />
            {blockedCount > 0 && <span className="icon-badge">{blockedCount}</span>}
          </button>
          <button
            className="icon-btn"
            title="Command palette (Ctrl+Shift+P) — jump anywhere, start anything"
            onClick={() => setPaletteOpen(true)}
          ><Icon of={Command} /></button>
          <button
            className={"icon-btn" + (explorerOnScreen ? " on" : "")}
            title={onHome ? "File explorer — opens with a session" : "Toggle file explorer"}
            onClick={() => setShowExplorer(!showExplorer)}
          ><Icon of={FolderOpen} /></button>
          <button
            className={"icon-btn" + (showGit ? " on" : "")}
            title="Toggle repository panel"
            onClick={() => setShowGit(!showGit)}
          ><Icon of={GitBranch} /></button>
          <button
            className={"icon-btn" + (agentOnScreen ? " on" : "")}
            title={onHome ? "Tasks panel — opens with a session" : "Toggle tasks panel"}
            onClick={() => setShowAgent(!showAgent)}
          ><Icon of={ListChecks} /></button>
          <button
            className={"icon-btn" + (showComposer ? " on" : "")}
            title="Toggle input composer"
            onClick={() => setShowComposer(!showComposer)}
          ><Icon of={Keyboard} /></button>
          <UsagePanel sources={usageSources} onRefresh={readUsage} refreshing={usageBusy} />
        </div>
        <div className="topbar-spacer" />
        <div className="topbar-right">
          <Clock />
          <button className="icon-btn" title="Smaller fonts (Ctrl+-)" onClick={() => bumpFont(-1)}>A−</button>
          <button
            className="icon-btn"
            title="Reset font size (Ctrl+0)"
            onClick={() => bumpFont(0)}
          >{Math.round(fontScale * 100)}%</button>
          <button className="icon-btn" title="Larger fonts (Ctrl+=)" onClick={() => bumpFont(1)}>A+</button>
          <AlertBell alerts={alerts} onGo={(key) => setActiveTab(key)} />
          <button
            className={"icon-btn" + (showSettingsModal ? " on" : "")}
            title="Settings"
            onClick={() => setShowSettingsModal(!showSettingsModal)}
          ><Icon of={SettingsIcon} /></button>
        </div>
      </div>
      <div className="main">
        {showSessions && (
          <>
            <div className="panel sessions" style={{ width: sizes.left, ...zoomFor("sessions") }}>
              <LiveLanes
                sessions={sessions}
                overview={overview}
                liveIds={homeFleetTabs.live}
                attentionIds={homeFleetTabs.attention}
                busyIds={homeFleetTabs.busy}
                otherAlerts={homeFleetTabs.otherAlerts}
                activeSlot={activeTabObj?.slotId ?? null}
                onSelect={selectSession}
                onResume={(s) => { void resumeSession(s); }}
                onGoTab={(key) => { setPreviewSession(null); setActiveFileTab(null); setActiveTab(key); }}
              />
              <SessionsPanel
                hoverSummary={settings.sessionHover}
                sessions={sessions}
                projects={projects}
                activeProject={activeProject}
                liveSlots={new Set(tabs.map((t) => t.slotId))}
                // Tabs whose process is still running. `liveSlots` deliberately
                // keeps an ended tab — you still need ⏻ to close it — but a
                // terminal whose process died is not a live session, so the dot
                // asks this instead.
                runningSlots={new Set(
                  tabs.filter((t) => !ended.has(t.key)).map((t) => t.slotId),
                )}
                liveSessions={liveShown}
                attentionSlots={new Set(
                  tabs.filter((t) => attention.has(t.key)).map((t) => t.slotId),
                )}
                attentionText={new Map(
                  tabs.flatMap((t) => {
                    const m = notices.get(t.key);
                    return m ? [[t.slotId, m] as [string, string]] : [];
                  }),
                )}
                progressSlots={new Map(
                  tabs.flatMap((t) => {
                    const p = progress.get(t.key);
                    return p ? [[t.slotId, p] as [string, TermProgress]] : [];
                  }),
                )}
                activeSlot={activeTabObj?.slotId ?? null}
                rekey={rekey}
                capsOf={capsOf}
                opts={opts}
                onOptsChange={setOpts}
                onSelect={selectSession}
                onResume={resumeSession}
                onOpenModelAccess={openModelAccess}
                onFork={(s) =>
                  forkSession(s).catch((e) =>
                    setNotice(`Couldn't fork "${s.title}": ${e}`),
                  )
                }
                onClear={clearSession}
                onExit={exitSession}
                onNewShell={newShell}
                onDelete={deleteSession}
                onSelectProject={selectProject}
                onProjectShell={projectShell}
                onProjectClaude={projectClaude}
                onNewSession={newSession}
                pending={pendingSessions}
                onSelectPending={focusSlot}
                onExitPending={closeSlot}
                onRefresh={refreshSessions}
                trashed={trashed}
                onRestore={restoreTrashed}
                onTrashDelete={deleteTrashed}
                onTrashEmpty={emptyTrash}
                onTrashSessions={trashSessions}
              />
            </div>
            <div className="splitter v" onMouseDown={() => startDrag("left")} />
          </>
        )}

        <div className="panel terminal-panel">
          {(tabs.length > 0 || previewSession || fileTabs.some(showsFile)) && (
            <div className="center-tabs">
              {/* Leftmost, always: the way back to the start view — or the
                  preview that stands in for it — so a file or a session is
                  never a dead end. */}
              {previewSession ? (
                <button
                  className={"center-tab locked on" + agentTint(previewSession.agent).className}
                  style={agentTint(previewSession.agent).style}
                  title={previewSession.project_path}
                  onClick={() => setActiveFileTab(null)}
                >
                  <AgentIcon agent={previewSession.agent} size={13} />
                  <span className="center-tab-name">{previewSession.title}</span>
                </button>
              ) : (
                <button
                  className={"center-tab home-tab" + (activeTab === null ? " on" : "")}
                  title="Home — start a session"
                  onClick={goHome}
                >
                  <Icon of={Home} size="sm" />
                </button>
              )}
              {/* Every open session, in the order they sit — drag one to move
                  it, middle-click or × to close (which ends its process; the
                  conversation stays on disk and in the sidebar). */}
              {tabs.filter((t) => t.parentKey === undefined).map((t) => {
                const on = t.key === rootKey && !previewSession;
                const wants = attention.has(t.key) || tabs.some((c) => c.parentKey === t.key && attention.has(c.key));
                const tint = agentTint(t.agentId);
                return (
                  <button
                    key={t.key}
                    className={"center-tab session" + (on ? " on" : "") + tint.className
                      + (ended.has(t.key) ? " ended" : "") + (dragOver === t.key ? " drop" : "")}
                    style={tint.style}
                    title={t.cwd ?? undefined}
                    draggable
                    onDragStart={(e) => { dragKey.current = t.key; e.dataTransfer.effectAllowed = "move"; }}
                    onDragOver={(e) => { if (dragKey.current !== null) { e.preventDefault(); setDragOver(t.key); } }}
                    onDragLeave={() => setDragOver((k) => (k === t.key ? null : k))}
                    onDrop={(e) => { e.preventDefault(); moveTab(dragKey.current, t.key); dragKey.current = null; setDragOver(null); }}
                    onDragEnd={() => { dragKey.current = null; setDragOver(null); }}
                    onClick={() => {
                      setPreviewSession(null);
                      setActiveFileTab(null);
                      setActiveTab(t.key);
                      handles.current.get(t.key)?.focus();
                    }}
                    onAuxClick={(e) => { if (e.button === 1) { e.preventDefault(); closeTab(t.key); } }}
                  >
                    {t.agentId
                      ? <AgentIcon agent={t.agentId} size={13} />
                      : <span className="center-tab-shell">❯</span>}
                    <span className="center-tab-name">{t.title}</span>
                    {wants && !on && <span className="center-tab-dot" title="Waiting for you" />}
                    <span
                      className="center-tab-close"
                      title="Close (ends the session's process)"
                      onClick={(e) => { e.stopPropagation(); closeTab(t.key); }}
                    ><Icon of={X} size="sm" /></span>
                  </button>
                );
              })}

            </div>
          )}
          {showBringIn && rootObj?.sessionId && !previewSession && (
            <BringIn
              host={engineName(rootObj.agentId)}
              onClose={() => setShowBringIn(false)}
              onOpenModelAccess={openModelAccess}
              onGo={(choice, focus, rounds, auto) => {
                setShowBringIn(false);
                if (rootKey !== null) {
                  void relayCtl.start({ aKey: rootKey, choice, focus, rounds, auto });
                }
              }}
            />
          )}
          {/* The session's own files, in a row of their own under the strip:
              what this session opened travels with it, and the leftmost tab
              is the way back to its terminal. */}
          {(fileTabs.some(showsFile) || (rootObj?.sessionId && !previewSession)) && (
            <div className="center-tabs file-row">
              <button
                className={"center-tab sub back" + (activeFileTab === null && activeTab === rootKey ? " on" : "")}
                title={previewSession ? "Back to the preview" : rootObj ? "Back to the terminal" : "Back to the start view"}
                onClick={() => {
                  setActiveFileTab(null);
                  if (rootKey !== null) { setActiveTab(rootKey); handles.current.get(rootKey)?.focus(); }
                }}
              >
                {previewSession
                  ? <AgentIcon agent={previewSession.agent} size={12} />
                  : rootObj
                  ? (rootObj.agentId
                      ? <AgentIcon agent={rootObj.agentId} size={12} />
                      : <span className="center-tab-shell">❯</span>)
                  : <Icon of={Home} size="sm" />}
                <span className="center-tab-name">{previewSession ? "Preview" : rootObj ? "Terminal" : "Home"}</span>
              </button>
              {/* Agents brought into this session: tabs of their own, under it. */}
              {!previewSession && tabs.filter((c) => c.parentKey === rootKey).map((c) => {
                const on = activeTab === c.key && activeFileTab === null;
                return (
                  <button
                    key={c.key}
                    className={"center-tab sub child" + (on ? " on" : "") + agentTint(c.agentId).className}
                    style={agentTint(c.agentId).style}
                    title={`${c.title} — brought into this session`}
                    onClick={() => { setActiveFileTab(null); setActiveTab(c.key); handles.current.get(c.key)?.focus(); }}
                    onAuxClick={(e) => { if (e.button === 1) { e.preventDefault(); closeTab(c.key); } }}
                  >
                    <AgentIcon agent={c.agentId ?? "api"} size={12} />
                    <span className="center-tab-name">{c.title}</span>
                    {attention.has(c.key) && !on && <span className="center-tab-dot" title="Waiting for you" />}
                    <span
                      className="center-tab-close"
                      title="Close (ends their process; reopen from this row later)"
                      onClick={(e) => { e.stopPropagation(); closeTab(c.key); }}
                    ><Icon of={X} size="sm" /></span>
                  </button>
                );
              })}
              {fileTabs.filter(showsFile).map((f) => (
                <button
                  key={f.key}
                  className={"center-tab sub" + (activeFileTab === f.key ? " on" : "")}
                  title={f.path}
                  onClick={() => setActiveFileTab(f.key)}
                >
                  {dirtyFiles.has(f.key) && <span className="center-tab-dot" />}
                  <span className="center-tab-name">{basename(f.path)}</span>
                  <span
                    className={"center-tab-close" + (fileCloseArm === f.key ? " arm" : "")}
                    title={fileCloseArm === f.key
                      ? "Unsaved changes — click again to discard"
                      : "Close"}
                    onClick={(e) => {
                      e.stopPropagation();
                      closeFileTab(f.key);
                    }}
                  ><Icon of={X} size="sm" /></span>
                </button>
              ))}
              {!previewSession && rootObj?.sessionId && rootKey !== null && (
                <div className="strip-right">
                  {(broughtIn[rootObj.sessionId] ?? [])
                    .filter((r) => !tabs.some((t) => t.sessionId === r.sessionId))
                    .map((r) => (
                      <span key={r.sessionId} className="recall-chip" title={`Reopen ${r.title} — brought in ${fullTime(r.at)}`}>
                        <button className="recall-open" onClick={() => void reopenBroughtIn(rootKey, r)}>
                          <Icon of={RotateCcw} size="sm" /> <AgentIcon agent={r.agentId} size={11} /> {r.title}
                        </button>
                        <button className="recall-x" title="Forget" onClick={() => setBroughtIn((m) => ({ ...m, [rootObj.sessionId!]: (m[rootObj.sessionId!] ?? []).filter((x) => x.sessionId !== r.sessionId) }))}><Icon of={X} size="sm" /></button>
                      </span>
                    ))}
                  {!relayCtl.relay || relayCtl.relay.aKey !== rootKey ? (
                    <button
                      className="strip-btn"
                      title="Bring a read-only second agent into this session"
                      onClick={() => setShowBringIn((shown) => !shown)}
                    >
                      <Icon of={Users} size="sm" /> Bring in…
                    </button>
                  ) : (
                  <span
                    className={"relay-pill " + relayCtl.relay.phase}
                    title={relayCtl.relay.note || undefined}
                  >
                    <Icon of={Users} size="sm" />
                    {relayCtl.relay.phase === "opening" && `bringing in ${relayCtl.relay.bName}…`}
                    {relayCtl.relay.phase === "waitB" && `round ${relayCtl.relay.round}/${relayCtl.relay.rounds} · waiting on ${relayCtl.relay.bName}`}
                    {relayCtl.relay.phase === "waitA" && `round ${relayCtl.relay.round}/${relayCtl.relay.rounds} · waiting on ${relayCtl.relay.aName}`}
                    {relayCtl.relay.phase === "done" && `done — ${relayCtl.relay.note}`}
                    {relayCtl.relay.phase === "stopped" && "stopped"}
                    {relayCtl.relay.phase === "error" && `stopped: ${relayCtl.relay.note}`}
                    {relayCtl.relay.phase === "opening" ||
                    relayCtl.relay.phase === "waitA" ||
                    relayCtl.relay.phase === "waitB" ? (
                      <button className="relay-x" title="Stop relaying" onClick={() => relayCtl.stop()}>
                        <Icon of={X} size="sm" />
                      </button>
                    ) : (
                      <button className="relay-x" title="Dismiss" onClick={relayCtl.clear}>
                        <Icon of={X} size="sm" />
                      </button>
                    )}
                    {relayCtl.relay.bKey && (
                      <button
                        className="relay-jump"
                        onClick={() => setActiveTab(
                          activeTab === relayCtl.relay!.aKey
                            ? relayCtl.relay!.bKey
                            : relayCtl.relay!.aKey,
                        )}
                      >
                        {activeTab === relayCtl.relay.aKey ? "their tab" : "first tab"}
                      </button>
                    )}
                  </span>
                  )}
                </div>
              )}
            </div>
          )}
          <div className="term-stack">
            {tabs.map((t) => (
              <TerminalView
                key={t.key}
                tab={t}
                active={t.key === activeTab}
                onExit={handleTermExit}
                onRegister={registerHandle}
                onActivity={noteActivity}
                onAttention={noteAttention}
                onNotify={noteNotify}
                onProgress={noteProgress}
                onLineSubmit={noteLineSubmit}
                autoFocus
                fontSize={termFont}
                fontFamily={xtermFont}
                lineHeight={settings.termLineHeight}
                fontWeight={settings.termFontWeight}
                renderer={settings.termRenderer}
                theme={xtermTheme}
              />
            ))}
            {fileTabs.map((f) => (
              <div
                key={f.key}
                className="file-layer"
                style={{
                  display: showsFile(f) && f.key === activeFileTab ? "flex" : "none",
                }}
              >
                {isPdf(f.path) ? (
                  <PdfView path={f.path} active={showsFile(f) && f.key === activeFileTab} refreshKey={explorerRefresh} />
                ) : (
                  <FileView
                    path={f.path}
                    active={showsFile(f) && f.key === activeFileTab}
                    refreshKey={explorerRefresh}
                    onDirty={(d) => noteFileDirty(f.key, d)}
                  />
                )}
              </div>
            ))}
            {activeTab !== null && ended.has(activeTab) && (
              <div className="term-ended">
                <div className="term-ended-box">
                  {/* A shell tab has no conversation behind it, so it gets none
                      of the claude wording. Promising that "the transcript is
                      still on disk" to someone who just typed `exit 3` in a
                      shell is a reassurance about something that never
                      existed — and a dialog that says one false thing is not
                      worth trusting about the true ones. */}
                  <div className="term-ended-title">
                    {activeTabObj?.sessionId
                      ? "This session ended on its own"
                      : "This shell ended on its own"}
                  </div>
                  <div className="term-ended-sub">
                    {describeEnd(ended.get(activeTab))}
                    {activeTabObj?.sessionId
                      ? " Nothing was lost — the transcript is still on disk."
                      : ""}
                  </div>
                  {activeTabObj?.sessionId && (
                    <div className="term-ended-sub dim">
                      A session listed by <code>claude agents</code> can be stopped from
                      any terminal, or from your phone. That looks exactly like this.
                    </div>
                  )}
                  <div className="term-ended-acts">
                    <button className="tui-pick" onClick={() => restartEnded(activeTab)}>
                      {activeTabObj?.sessionId ? "Resume it" : "Open a new shell"}
                    </button>
                    <button className="tui-plain" onClick={() => closeTab(activeTab)}>
                      Close tab
                    </button>
                  </div>
                </div>
              </div>
            )}
            {/* The preview pane sits above the file layer, and the start view
                would show through under it — neither is drawn while a file
                tab is the one on screen. */}
            {onHome && (
              <HomeDashboard
                sessions={sessions}
                liveIds={homeFleetTabs.live}
                attentionIds={homeFleetTabs.attention}
                busyIds={homeFleetTabs.busy}
                otherAlerts={homeFleetTabs.otherAlerts}
                onSelect={selectSession}
                onResume={(s) => { void resumeSession(s); }}
                onGoTab={(key) => setActiveTab(key)}
                onShowAll={showAllSessions}
                controls={<StartControls ctl={emptyCtl} onOpenModelAccess={openModelAccess} only="tabs" />}
                pickers={<StartControls ctl={emptyCtl} onOpenModelAccess={openModelAccess} only="selects" />}
                pickerSummary={describePickers(emptyCtl)}
                usage={usageSources}
                ready={emptyCtl.ready}
                cwd={homeCwd}
                onPickCwd={pickHomeCwd}
                onSetCwd={setHomeCwdPick}
                onLaunch={launchFromHome}
                onOpenTerminal={openHomeTerminal}
              />
            )}
            {previewSession && !fileOnScreen && (
              <SessionPreview
                session={previewSession}
                onResume={resumeSession}
                canResume={capsOf(previewSession.agent).resume}
                onClose={() => setPreviewSession(null)}
              />
            )}
            {tui && !tuiDismissed && activeTab !== null && (
              tui.kind === "rewind-picker" || tui.kind === "rewind-confirm" ? (
                <TuiRewind
                  step={tui}
                  write={(d) => handles.current.get(activeTab)?.write(d)}
                  screen={() => handles.current.get(activeTab)?.screen() ?? []}
                  onDismiss={() => dismissTui(activeTab)}
                />
              ) : tui.kind === "model-picker" ? (
                <TuiModelPicker
                  picker={tui}
                  write={(d) => handles.current.get(activeTab)?.write(d)}
                  screen={() => handles.current.get(activeTab)?.screen() ?? []}
                  onDismiss={() => dismissTui(activeTab)}
                />
              ) : tui.kind === "model-confirm" ? (
                <TuiModelConfirm
                  confirm={tui}
                  write={(d) => handles.current.get(activeTab)?.write(d)}
                  screen={() => handles.current.get(activeTab)?.screen() ?? []}
                  onDismiss={() => dismissTui(activeTab)}
                />
              ) : (
                <TuiPermission
                  request={tui}
                  write={(d) => handles.current.get(activeTab)?.write(d)}
                  screen={() => handles.current.get(activeTab)?.screen() ?? []}
                  onDismiss={() => dismissTui(activeTab)}
                />
              )
            )}
          </div>
          {/* onCommand goes to the focused terminal, so the pills only offer
              model/effort when there is a live session to run them in — and
              not while the agents view has the screen, where a slash command
              would be typed into its "describe a task" box instead of run.
              Every pill that speaks Claude Code's vocabulary is now gated on
              the active tab's engine declaring `panels`: `/model`, `/effort`
              and `/rewind` are claude's commands, and Tasks/Artifacts/Agents
              read a claude transcript. Sent into a Codex or `aiterm chat` tab
              they were text typed at a prompt that does not understand them.
              The pills that are about the tab rather than the engine — plan
              usage, the repo — stay, because they are true whatever is running.
              `onSetPermMode` goes with the TUI: it works by reading the status
              line and sending shift+tab. */}
          {refusal && activeCaps.panels && (
            <RefusalBanner
              refusal={refusal}
              targetModel={ocTarget}
              onRestore={async () => {
                const alias = (refusal.original_model ?? "").replace(/^claude-/, "").split("-")[0];
                if (!alias || activeTab === null) { setRefusal(null); return; }
                // Typing /model retargets the session AND rewrites the global
                // default; save it first and put it back, so restoring the
                // session doesn't quietly change what new sessions open on.
                const prior = await claudeModelDefault();
                handles.current.get(activeTab)?.sendComposed(`/model ${alias}`);
                await restoreClaudeModelDefault(prior);
                setRefusal(null);
              }}
              onKick={async () => {
                const t = await opencodeDefaultTarget();
                // Hand OpenCode the thread, not just the flagged line — Claude
                // is blocked and can't curate the hand-off itself, so aiterm
                // assembles the recent conversation as context. The flagged
                // message is the task; the last user message is the fallback if
                // the record couldn't resolve it.
                const preview = activeSessionId ? await sessionPreview(activeSessionId) : [];
                const task = refusal.refused_prompt
                  ?? [...preview].reverse().find((m) => m.role === "user")?.text
                  ?? "";
                const context = preview.map((m) => `${m.role}: ${m.text}`).join("\n\n");
                const prompt = context
                  ? `You're picking up a task from a paused Claude Code session — its model was blocked by a safety classifier mid-conversation, so the work is being handed to you. Recent conversation, for context:\n\n${context}\n\n---\nComplete this task from that conversation:\n\n${task}`
                  : task;
                return opencodeDispatch(prompt, activeProject ?? ".", t.provider, t.model);
              }}
              onDismiss={() => setRefusal(null)}
            />
          )}
          {showComposer && <Composer
            sessionId={activeCaps.panels ? activeSessionId : null}
            projectRoot={activeProject}
            usage={claudeUsage?.bars ?? []}
            usageAsOf={claudeUsage?.stale ? claudeUsage.at : null}
            onCommand={activeTab === null || agentsView || !activeCaps.panels
              ? undefined
              : (text) => handles.current.get(activeTab)?.sendComposed(text)}
            onDismiss={activeTab === null ? undefined : () =>
              handles.current.get(activeTab)?.focus()}
            hasPendingInput={activeTab === null ? undefined : () =>
              handles.current.get(activeTab)?.pendingInput() ?? false}
            onOpenModelPicker={activeTab === null || agentsView || !activeCaps.panels
              ? undefined : openModelPicker}
            onOpenRewind={activeTab === null || agentsView || !activeCaps.panels
              ? undefined : openRewind}
            permMode={permMode}
            onSetPermMode={activeTab === null || agentsView || !activeDrivesTui
              ? undefined : setPermissionMode}
          />}
        </div>

        {showRight && (
          <>
            {/* On home the column holds Repository alone and is fixed at a
                single panel's width, so the launcher keeps its 760 and the
                page still composes at 1400. A width nothing can change is
                not a width to offer a drag handle for, so the splitter is
                there as the divider and inert until a session is up. */}
            <div
              className={"splitter v" + (onHome ? " locked" : "")}
              onMouseDown={onHome ? undefined : () => startDrag("right")}
            />
            <div
              className="right-col"
              ref={rightColRef}
              style={{ width: onHome ? Math.min(sizes.right, HOME_RIGHT_WIDTH) : sizes.right }}
            >
              <div
                className="right-top"
                style={{ height: agentOnScreen ? `${(1 - sizes.agentFrac) * 100}%` : "100%" }}
              >
                {explorerOnScreen && (
                  <div
                    className="panel explorer"
                    style={{ width: showGit ? `${sizes.explorerFrac * 100}%` : "100%", ...zoomFor("explorer") }}
                  >
                    <div className="panel-header">
                      <span>EXPLORER</span>
                      <button className="icon-btn" onClick={() => setShowExplorer(false)}><Icon of={X} /></button>
                    </div>
                    <FileExplorer root={activeProject} refreshKey={explorerRefresh} onOpenFile={openFileTab} />
                  </div>
                )}
                {explorerOnScreen && showGit && (
                  <div className="splitter v" onMouseDown={() => startDrag("rightsplit")} />
                )}
                {showGit && (
                  <div className="panel git" style={{ flex: 1, minWidth: 0, ...zoomFor("git") }}>
                    <div className="panel-header">
                      <span>REPOSITORY</span>
                      <div>
                        <button className="icon-btn" title="Refresh"
                          onClick={() => setGitRefresh((n) => n + 1)}><Icon of={RefreshCw} /></button>
                        <button className="icon-btn" onClick={() => setShowGit(false)}><Icon of={X} /></button>
                      </div>
                    </div>
                    <GitPanel root={activeProject} refreshKey={gitRefresh} />
                  </div>
                )}
              </div>
              {agentOnScreen && (
                <>
                  <div className="splitter h" onMouseDown={() => startDrag("agentsplit")} />
                  <div className="panel agent" style={{ flex: 1, minHeight: 0, ...zoomFor("agent") }}>
                    <div className="panel-header">
                      <span>AGENT{activeTabObj?.title ? ` — ${activeTabObj.title}` : ""}</span>
                      <button className="icon-btn" onClick={() => setShowAgent(false)}><Icon of={X} /></button>
                    </div>
                    {/* Gated on `tasks`, not `panels`: the panel reads task
                        lists and file edits, which grok and codex also record
                        in shapes the backend can read. The pills stay behind
                        `panels`, because they speak claude's slash commands.
                        An engine that declares neither hands it no session and
                        the panel says so — the same empty state as a shell
                        tab, which is the truth. */}
                    <AgentPanel
                      sessionId={activeCaps.tasks ? activeSessionId : null}
                      onOpenFile={openFileTab}
                    />
                  </div>
                </>
              )}
            </div>
          </>
        )}
      </div>
      {paletteOpen && (
        <CommandPalette items={paletteItems} onClose={() => setPaletteOpen(false)} />
      )}
      {showSettingsModal && (
        <SettingsModal
          settings={settings}
          onChange={setSettings}
          onClose={closeSettings}
          capsOf={capsOf}
          activeProject={activeProject}
          initialTab={settingsTarget?.tab}
          focusProvider={settingsTarget?.provider}
          librarian={librarian}
        />
      )}
    </div>
    </TimeFormatContext.Provider>
  );
}

function basename(p: string): string {
  return p.split("/").filter(Boolean).pop() ?? p;
}

// Backslash-escape (the way terminals escape dropped paths) — claude's
// pasted-path detection understands this form, unlike single quotes.
function shellEscape(p: string): string {
  return p.replace(/[^A-Za-z0-9_\-./~+:@%=]/g, (c) => "\\" + c);
}
