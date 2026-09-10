import { useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  Caps, ProjectInfo, Session, SessionDetail, TrashedSession, homeAbbrev, searchSessions, sessionBroughtIn, sessionDetail,
  sessionStar, sessionStars,
} from "../ipc";
import SessionFlyout from "./SessionFlyout";
import NewSessionMenu, { StartChoice, StartPoint } from "./NewSessionMenu";
import AgentIcon from "./AgentIcon";
import Icon from "./Icon";
import {
  ChevronsDownUp, ChevronsUpDown, CornerDownRight, Folder, GitBranch, GitFork, Pin, Play, RefreshCw, Search,
  Settings as SettingsIcon, Trash2, Users, X,
} from "lucide-react";
import { agentTint } from "../brand";
import { TermProgress } from "./TerminalView";
import { stableOrder } from "../order";
import { followRekey } from "../selection";

import { fmtTimeShort, fullTime, useTimeFormat } from "../timefmt";

type ViewMode = "recent" | "project" | "date";

function dateBucket(ms: number): string {
  const now = new Date();
  const startOfDay = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  if (ms >= startOfDay) return "Today";
  if (ms >= startOfDay - 86400_000) return "Yesterday";
  if (ms >= startOfDay - 6 * 86400_000) return "This week";
  if (ms >= startOfDay - 29 * 86400_000) return "This month";
  return "Older";
}

/** A started-but-not-yet-written session. `id` is both its slot id and the
 *  session id it will have, because aiterm minted it. */
export interface PendingSession {
  id: string;
  title: string;
  cwd: string;
  /** The engine that was started, so the row wears its own mark. Every pending
   *  row used to draw claude's starburst, which meant a fresh Codex or
   *  OpenCode tab impersonated a claude session for as long as it took its
   *  transcript to land. Empty for a tab whose engine is unknown — `AgentIcon`
   *  has a generic glyph for exactly that. */
  agent: string;
}

export interface SessionDisplayOpts {
  showPath: boolean;
  showBranch: boolean;
  showTime: boolean;
}

interface Group {
  id: string;
  name: string;
  color: string;
  collapsed: boolean;
  /** Project paths — a group holds projects, and every session of a member
   *  project shows under it. */
  members: string[];
}

const GROUPS_KEY = "aiterm.projectGroups";
const ORDER_KEY = "aiterm.sessionOrder";
const PALETTE = ["#61afef", "#98c379", "#e5c07b", "#e06c75", "#c678dd", "#56b6c2", "#da7756"];

function loadGroups(): Group[] {
  try {
    return JSON.parse(localStorage.getItem(GROUPS_KEY) ?? "[]");
  } catch {
    return [];
  }
}

/** Manual drag-rearranged order per container ("recent" or "group:<id>"). */
function loadOrders(): Record<string, string[]> {
  try {
    return JSON.parse(localStorage.getItem(ORDER_KEY) ?? "{}");
  } catch {
    return {};
  }
}

/** Items the user hand-ordered keep that order; ones not ordered yet
 *  (new arrivals) stay on top in their natural order. */
function orderBy<T>(list: T[], id: (t: T) => string, order?: string[]): T[] {
  if (!order || order.length === 0) return list;
  const idx = new Map(order.map((k, i) => [k, i]));
  const fresh = list.filter((t) => !idx.has(id(t)));
  const ordered = list
    .filter((t) => idx.has(id(t)))
    .sort((a, b) => idx.get(id(a))! - idx.get(id(b))!);
  return [...fresh, ...ordered];
}
const applyOrder = (list: Session[], order?: string[]) =>
  orderBy(list, (s) => s.id, order);

interface DragArm {
  /** session = a row; group = a custom group header (recent view);
   *  section = an auto-section header (project view). */
  kind: "session" | "group" | "section";
  /** Session id, group id, or section key. */
  id: string;
  /** Session's project path (session drags — group membership drops). */
  path: string;
  /** Container the session was dragged from ("recent", "group:<id>",
   *  or "sec:<view>:<key>"). */
  container: string;
  label: string;
  x: number;
  y: number;
}

interface Props {
  sessions: Session[];
  projects: ProjectInfo[];
  activeProject: string | null;
  /** Slot ids aiterm has a terminal tab open for. Tab ownership only — NOT
   *  the same as the session being alive, and after a background-mode resume
   *  the two name different rows. */
  liveSlots: Set<string>;
  /** Session ids the Claude Code daemon is actually running right now. */
  liveSessions: Set<string>;
  /** Slot ids whose terminal process is still alive — `liveSlots` minus the
   *  tabs whose process ended. For an engine with no external roster this is
   *  the only evidence a session is running. */
  runningSlots: Set<string>;
  /** Slot ids whose terminal rang the bell (claude waiting on input). */
  attentionSlots: Set<string>;
  /** What the waiting session actually said, when it sent words rather than a
   *  bell (OSC 9). Keyed by slot, like `attentionSlots`. */
  attentionText: Map<string, string>;
  /** Long-running work a live session is reporting (OSC 9;4). */
  progressSlots: Map<string, TermProgress>;
  /** Slot id of the terminal currently displayed. */
  activeSlot: string | null;
  /** Set when a tab swapped the conversation it holds (/clear, /fork, or
   *  picking another agent from claude's own agents screen). The click
   *  selection follows it, so the highlight does not stay on the conversation
   *  that was left behind. */
  rekey: { from: string; to: string; seq: number } | null;
  opts: SessionDisplayOpts;
  onOptsChange: (o: SessionDisplayOpts) => void;
  /** What an engine supports, by agent id. Every lifecycle button in a row is
   *  drawn from this rather than from the row's agent *name*: an id with no
   *  backend — a row from an engine since removed — answers all-false, and a
   *  row with no buttons is a better failure than a row offering claude's. */
  capsOf: (agent: string) => Caps;
  onSelect: (s: Session) => void;
  onResume: (s: Session) => void;
  onFork: (s: Session) => void;
  /** Open Settings → Model access — the ＋ menu's setup note lands there. */
  onOpenModelAccess?: (providerId?: string) => void;
  /** Park this conversation and start a fresh one in its tab — aiterm's own
   *  clear, kin to ⑂: pure process control and disk, no claude machinery.
   *  Only offered on rows whose engine declares `clear` and that have a live
   *  terminal of their own. */
  onClear: (s: Session) => void;
  onExit: (s: Session) => void;
  onNewShell: (s: Session) => void;
  onDelete: (s: Session) => void;
  onSelectProject: (p: ProjectInfo) => void;
  onProjectShell: (p: ProjectInfo) => void;
  onProjectClaude: (p: ProjectInfo) => void;
  /** Start a fresh claude session in a directory — always a new one, unlike
   *  `onProjectClaude`, which reuses the tab already open for that project. */
  onNewSession: (path: string, choice: StartChoice) => void;
  /** Sessions aiterm has started that have not written a transcript yet, so
   *  they have no row of their own. Listed on the strength of the tab alone —
   *  they are the one thing in this panel that is not read off disk, and they
   *  stop being listed the moment the real row exists. */
  pending: PendingSession[];
  onSelectPending: (slotId: string) => void;
  onExitPending: (slotId: string) => void;
  onRefresh: () => void;
  trashed: TrashedSession[];
  onRestore: (id: string) => void;
  onTrashDelete: (id: string) => void;
  onTrashEmpty: () => void;
  /** Move a whole set of sessions to the trash — the right-click action on a
   *  project or group header. The panel has already excluded anything a per-row
   *  🗑 would refuse. */
  onTrashSessions: (sessions: Session[]) => void;
  /** Open the summary card when the pointer rests on a row. */
  hoverSummary?: boolean;
}

/** How many recent rows render before "Show all" — a screen and a half of
 *  history; everything stays reachable through search. */
const RECENT_WINDOW = 80;

export default function SessionsPanel({
  sessions, projects, activeProject, liveSlots, liveSessions, runningSlots, attentionSlots,
  attentionText, progressSlots, activeSlot, rekey, opts,
  capsOf, onOptsChange, onSelect, onResume, onFork, onClear, onExit, onNewShell, onDelete,
  onOpenModelAccess, onSelectProject, onProjectShell, onProjectClaude, onNewSession,
  pending, onSelectPending, onExitPending, onRefresh,
  trashed, onRestore, onTrashDelete, onTrashEmpty, onTrashSessions,
  hoverSummary = true,
}: Props) {
  const [query, setQuery] = useState("");
  const [showNewSession, setShowNewSession] = useState(false);
  /** Brought-in session → the master it joined, from the same lineage store
   *  the phone reads; and which masters have their crew folded away. */
  const [broughtIn, setBroughtIn] = useState<Record<string, string>>({});
  const [foldedCrews, setFoldedCrews] = useState<Set<string>>(new Set());
  useEffect(() => {
    const load = () => { sessionBroughtIn().then(setBroughtIn).catch(() => {}); };
    load();
    const un = listen("sessions://changed", load);
    return () => { un.then((f) => f()); };
  }, []);
  /** Pinned sessions, from the same store the backend has kept all along
   *  (`session_star`) — the sidebar just never drew it. A pin is a row that
   *  stays at the top whatever the recency order does to everything else. */
  const [stars, setStars] = useState<Set<string>>(new Set());
  useEffect(() => {
    const load = () => { sessionStars().then((ids) => setStars(new Set(ids))).catch(() => {}); };
    load();
    const un = listen("sessions://changed", load);
    return () => { un.then((f) => f()); };
  }, []);
  const togglePin = (s: Session) => {
    const on = !stars.has(s.id);
    // Optimistic: the row moves the moment it is clicked; the reload on
    // `sessions://changed` settles it.
    setStars((prev) => { const next = new Set(prev); if (on) next.add(s.id); else next.delete(s.id); return next; });
    sessionStar(s.id, on).catch(() => {});
  };
  /** The recent list renders a window, not the archive: 500+ rows of DOM
   *  reconciled on every transcript-append reload is what made typing lag
   *  [measured 2026-08-31]. Search always looks at everything; "Show all"
   *  opens the rest for a browse. */
  const [showAllRecent, setShowAllRecent] = useState(false);
  const [confirmDel, setConfirmDel] = useState<string | null>(null);
  // Resuming a session that's still running stops it first, which throws away
  // whatever it was mid-way through — worth one click of confirmation.
  const [confirmStop, setConfirmStop] = useState<string | null>(null);
  // Folded auto-sections (Recent list, Project/Date buckets, PROJECTS),
  // keyed "<viewMode>:<label>" so each view remembers its own folds.
  const [collapsedSections, setCollapsedSections] = useState<Set<string>>(() => {
    try {
      return new Set(JSON.parse(localStorage.getItem("aiterm.collapsedSections") ?? "[]"));
    } catch {
      return new Set();
    }
  });
  useEffect(
    () => localStorage.setItem("aiterm.collapsedSections", JSON.stringify([...collapsedSections])),
    [collapsedSections],
  );
  const toggleSection = (key: string) =>
    setCollapsedSections((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  // Searching always shows everything regardless of folds.
  const sectionOpen = (key: string) => !collapsedSections.has(key) || query.trim().length > 0;
  const [emptyConfirm, setEmptyConfirm] = useState(false);
  // Right-click menu. One destructive item, which arms on the first click and
  // acts on the second — a menu you had to summon is deliberate enough that a
  // second click is confirmation, without a separate dialog.
  const [menu, setMenu] = useState<
    null | { x: number; y: number; idle: string; armed: string; note?: string; run: () => void }
  >(null);
  const [menuArmed, setMenuArmed] = useState(false);
  const closeMenu = () => { setMenu(null); setMenuArmed(false); };
  useEffect(() => {
    if (!menu) return;
    // Escape closes it, and so does anything that moves the list underneath it —
    // a menu pinned to a coordinate is wrong the moment the page scrolls.
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") closeMenu(); };
    window.addEventListener("keydown", onKey);
    window.addEventListener("resize", closeMenu);
    window.addEventListener("scroll", closeMenu, true);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("resize", closeMenu);
      window.removeEventListener("scroll", closeMenu, true);
    };
  }, [menu]);

  /** Right-click on a header that stands for a set of sessions.
   *
   *  Only the ones a per-row 🗑 would delete are offered: a running session is
   *  excluded because trashing one does not stick — the process writes its
   *  transcript again seconds later, rebuilt from the deletion point, losing
   *  the history before it. The count says how many are being kept and why, so
   *  "move 12" over a group of 14 is never a silent partial job.
   */
  const openSetMenu = (e: React.MouseEvent, what: string, group: Session[]) => {
    e.preventDefault();
    e.stopPropagation();
    const targets = group.filter(
      (s) =>
        !liveSessions.has(s.id) &&
        !liveSlots.has(s.id) &&
        !liveSlots.has(`shell:${s.project_path}`) &&
        capsOf(s.agent).delete,
    );
    const kept = group.length - targets.length;
    const n = targets.length;
    setMenuArmed(false);
    setMenu({
      x: e.clientX,
      y: e.clientY,
      idle: n === 0 ? "Nothing here can be trashed" : `Move ${n} session${n === 1 ? "" : "s"} to trash`,
      armed: `Move ${n} from ${what} to trash?`,
      note: kept > 0 ? `${kept} still running — kept` : undefined,
      run: () => { if (n > 0) onTrashSessions(targets); },
    });
  };
  const [showSettings, setShowSettings] = useState(false);
  const [viewMode, setViewMode] = useState<ViewMode>(
    () => (localStorage.getItem("aiterm.viewMode") as ViewMode) || "recent",
  );
  const [ftResults, setFtResults] = useState<Session[] | null>(null);

  useEffect(() => localStorage.setItem("aiterm.viewMode", viewMode), [viewMode]);

  // Debounced full-text search (tantivy index over titles + message text).
  useEffect(() => {
    if (!query.trim()) {
      setFtResults(null);
      return;
    }
    const t = setTimeout(() => {
      searchSessions(query).then(setFtResults).catch(() => setFtResults(null));
    }, 250);
    return () => clearTimeout(t);
  }, [query]);
  const [groups, setGroups] = useState<Group[]>(loadGroups);
  const [orders, setOrders] = useState<Record<string, string[]>>(loadOrders);
  const [selected, setSelected] = useState<Set<string>>(new Set());

  // ---- hover flyout ----
  // Opens beside a row after the pointer has rested on it — a flyout that
  // chased the pointer down the list would be noise. Details are read once
  // per session and kept for the panel's life; a transcript changes only
  // while its session is running, and a running one is refetched on each
  // open. Placed from the row's rectangle: fixed, so it escapes the list's
  // scrolling, above the row when the row is in the lower half of the window
  // so the card never runs off the bottom.
  const [fly, setFly] = useState<{ s: Session; top?: number; bottom?: number; left: number } | null>(null);
  const flyTimer = useRef<number | null>(null);
  const flyCache = useRef<Map<string, SessionDetail>>(new Map());
  const [flyDetail, setFlyDetail] = useState<SessionDetail | null>(null);
  const flyEnter = (s: Session, el: HTMLElement, running: boolean) => {
    if (!hoverSummary) return;
    if (flyTimer.current) window.clearTimeout(flyTimer.current);
    flyTimer.current = window.setTimeout(() => {
      const r = el.getBoundingClientRect();
      const panel = el.closest(".panel")?.getBoundingClientRect();
      const left = (panel?.right ?? r.right) + 8;
      const lower = r.top > window.innerHeight / 2;
      setFly(lower
        ? { s, left, bottom: Math.max(8, window.innerHeight - r.bottom) }
        : { s, left, top: Math.max(8, r.top) });
      const cached = flyCache.current.get(s.id);
      setFlyDetail(cached && !running ? cached : null);
      if (!cached || running) {
        sessionDetail(s.id).then((d) => {
          if (!d) return;
          flyCache.current.set(s.id, d);
          setFlyDetail((cur) => (cur === null || cur.id === d.id ? d : cur));
        }).catch(() => {});
      }
    }, 450);
  };
  // Leaving the row does not close the card at once: the pointer needs the
  // gap between row and card to cross into it, and once in it the card is
  // held open so its text can be selected and copied.
  const flyClose = useRef<number | null>(null);
  const flyLeave = () => {
    if (flyTimer.current) { window.clearTimeout(flyTimer.current); flyTimer.current = null; }
    if (flyClose.current) window.clearTimeout(flyClose.current);
    flyClose.current = window.setTimeout(() => setFly(null), 250);
  };
  const flyHold = () => {
    if (flyClose.current) { window.clearTimeout(flyClose.current); flyClose.current = null; }
  };
  useEffect(() => {
    // Anything that moves the row from under the pointer closes the card —
    // except acting inside the card itself, which is selecting its text.
    const inCard = (e: Event) => (e.target as HTMLElement | null)?.closest?.(".sfly") != null;
    const close = (e: Event) => { if (!inCard(e)) setFly(null); };
    window.addEventListener("scroll", close, true);
    window.addEventListener("pointerdown", close, true);
    window.addEventListener("keydown", close, true);
    return () => {
      window.removeEventListener("scroll", close, true);
      window.removeEventListener("pointerdown", close, true);
      window.removeEventListener("keydown", close, true);
    };
  }, []);
  const [dragId, setDragId] = useState<string | null>(null);
  const [dragOver, setDragOver] = useState<string | null>(null);
  const [dragOverItem, setDragOverItem] = useState<string | null>(null);
  const [dragOverAfter, setDragOverAfter] = useState(false);
  const [dragPos, setDragPos] = useState<{ x: number; y: number } | null>(null);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [renameText, setRenameText] = useState("");
  // Pointer-based drag: HTML5 DnD never fires in webkit2gtk on Wayland.
  const dragArm = useRef<DragArm | null>(null);
  const dragActive = useRef(false);
  const suppressClick = useRef(false);
  const selectedRef = useRef(selected);
  selectedRef.current = selected;

  // Only a selection that held the old id moves; one built by ctrl-clicking
  // rows for a drag belongs to the user and is left alone.
  useEffect(() => {
    if (!rekey) return;
    setSelected((prev) => followRekey(prev, rekey.from, rekey.to));
  }, [rekey]);
  // Displayed id sequence per container, read by the drop handler.
  const containerSeq = useRef<Map<string, string[]>>(new Map());

  // The order each list was last drawn in, so a refresh does not rearrange it.
  // Written during render, which is safe because `stableOrder` is idempotent:
  // feeding it back its own output returns the same list.
  const shownOrder = useRef<Map<string, string[]>>(new Map());
  const keepPut = <T,>(key: string, list: T[], id: (t: T) => string, fresh = false): T[] => {
    const out = fresh ? list : stableOrder(list, id, shownOrder.current.get(key));
    shownOrder.current.set(key, out.map(id));
    return out;
  };
  /** Whether the pointer is over the panel — while it is, rows hold still. */
  const [pointerIn, setPointerIn] = useState(false);

  useEffect(() => localStorage.setItem(GROUPS_KEY, JSON.stringify(groups)), [groups]);
  useEffect(() => localStorage.setItem(ORDER_KEY, JSON.stringify(orders)), [orders]);

  const filtered = useMemo(() => {
    // Freeze the order first. Rows arrive sorted by `last_active`, which moves
    // every time an agent writes a line — so with several sessions running the
    // list rearranged itself under the cursor. `keepPut` lets a new session in
    // at the top and then leaves every row where it is.
    // Rows arrive sorted by `last_active`, which moves every time an agent
    // writes a line — so with several sessions running the list rearranged
    // itself under the cursor. The order is therefore FROZEN while the pointer
    // is over the panel (`keepPut` lets a new session in at the top and leaves
    // every row where it is) and follows recency the rest of the time: the
    // session being worked on climbs to the top once the pointer has left,
    // instead of staying wherever it was when the panel was last hovered
    // [John, 2026-09-03 — "it appears to stay locked into place"].
    const stable = keepPut("all", sessions, (s) => s.id, !pointerIn);
    const q = query.toLowerCase();
    if (!q) return stable;
    return stable.filter(
      (s) =>
        s.title.toLowerCase().includes(q) ||
        s.project_path.toLowerCase().includes(q) ||
        (s.branch ?? "").toLowerCase().includes(q),
    );
  }, [sessions, query, pointerIn]);

  // When searching: ranked full-text hits first, then substring matches the
  // index may have missed (still catching up, etc).
  const searchList = useMemo(() => {
    if (!query.trim()) return null;
    const seen = new Set((ftResults ?? []).map((s) => s.id));
    return [...(ftResults ?? []), ...filtered.filter((s) => !seen.has(s.id))];
  }, [query, ftResults, filtered]);

  // Group membership keys off `group_path`, not the raw cwd: a `/fork` can run
  // its agent in a `<repo>/.claude/worktrees/<name>` checkout, which would
  // otherwise strand the fork in a group of its own instead of listing it
  // beside the session it branched from. Rows still spawn in `project_path`.
  const grouped = useMemo(() => new Set(groups.flatMap((g) => g.members)), [groups]);
  /** Glue each master's brought-in agents directly beneath it (left out
   *  while folded); a satellite whose master is not in the list stays where
   *  it is, as a row of its own. Mirrors the phone. */
  const glueCrew = (list: Session[]): Session[] => {
    const out: Session[] = [];
    const placed = new Set<string>();
    for (const s of list) {
      if (placed.has(s.id)) continue;
      const master = broughtIn[s.id];
      if (master && list.some((x) => x.id === master)) continue;
      out.push(s); placed.add(s.id);
      for (const k of list) {
        if (broughtIn[k.id] === s.id && !placed.has(k.id)) {
          placed.add(k.id);
          if (!foldedCrews.has(s.id)) out.push(k);
        }
      }
    }
    return out;
  };
  const ungrouped = useMemo(
    () => glueCrew(applyOrder(filtered.filter((s) => !grouped.has(s.group_path) && !stars.has(s.id)), orders["recent"])),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [filtered, grouped, orders, broughtIn, foldedCrews, stars],
  );
  /** Pinned rows, newest first. Drawn above every view; a search reaches
   *  them through the list like anything else. */
  const pinned = useMemo(
    () => filtered.filter((s) => stars.has(s.id)).sort((a, b) => b.last_active - a.last_active),
    [filtered, stars],
  );
  /** Rows drawn indented: brought-in agents whose master is in the list. */
  const satellites = useMemo(() => {
    const ids = new Set(filtered.map((s) => s.id));
    return new Set(Object.entries(broughtIn).filter(([k, m]) => ids.has(k) && ids.has(m)).map(([k]) => k));
  }, [filtered, broughtIn]);
  /** Master → how many of its brought-in agents still exist. The lineage
   *  store remembers deleted ones; the badge must not. */
  const crewCount = useMemo(() => {
    const ids = new Set(sessions.map((s) => s.id));
    const m = new Map<string, number>();
    for (const [k, master] of Object.entries(broughtIn)) {
      if (ids.has(k)) m.set(master, (m.get(master) ?? 0) + 1);
    }
    return m;
  }, [sessions, broughtIn]);
  const groupMembers = useMemo(() => {
    const m = new Map<string, Session[]>();
    for (const g of groups) {
      m.set(
        g.id,
        applyOrder(
          filtered.filter((s) => g.members.includes(s.group_path)),
          orders[`group:${g.id}`],
        ),
      );
    }
    return m;
  }, [groups, filtered, orders]);
  containerSeq.current = new Map([
    ["recent", ungrouped.map((s) => s.id)],
    ...groups.map(
      (g) => [`group:${g.id}`, (groupMembers.get(g.id) ?? []).map((s) => s.id)] as const,
    ),
  ]);

  const sessionPaths = useMemo(
    () => new Set(sessions.map((s) => s.group_path)),
    [sessions],
  );
  const sessionlessProjects = useMemo(() => {
    const q = query.toLowerCase();
    return projects.filter(
      (p) => !sessionPaths.has(p.path) && (!q || p.name.toLowerCase().includes(q)),
    );
  }, [projects, sessionPaths, query]);

  // Everywhere a session could be started: directories sessions have actually
  // run in, then project directories that have none. Deliberately not filtered
  // by the search box — that box filters the session list, and a menu that
  // emptied itself because you had typed something into a field behind it
  // would be baffling. The menu has its own filter.
  const startPoints = useMemo<StartPoint[]>(() => {
    const seen = new Map<string, StartPoint>();
    for (const s of sessions) {
      const at = seen.get(s.project_path)?.at ?? 0;
      if (at >= s.last_active) continue;
      seen.set(s.project_path, {
        path: s.project_path,
        name: s.project_path.split("/").filter(Boolean).pop() ?? s.project_path,
        at: s.last_active,
      });
    }
    const fromSessions = [...seen.values()].sort((a, b) => b.at - a.at);
    const rest = projects
      .filter((p) => !seen.has(p.path))
      .map<StartPoint>((p) => ({ path: p.path, name: p.name, at: p.last_modified }))
      .sort((a, b) => b.at - a.at);
    return [...fromSessions, ...rest];
  }, [sessions, projects]);

  // Auto sub-grouping for the Project / Date view modes. Project sections
  // drag-reorder by their header; date buckets stay chronological.
  const autoSections = useMemo(() => {
    if (viewMode === "recent" || searchList) return null;
    const map = new Map<string, Session[]>();
    for (const s of filtered) {
      const key = viewMode === "project"
        ? s.group_path
        : dateBucket(s.last_active);
      (map.get(key) ?? map.set(key, []).get(key)!).push(s);
    }
    if (viewMode === "date") {
      const order = ["Today", "Yesterday", "This week", "This month", "Older"];
      return order
        .filter((k) => map.has(k))
        .map((k) => ({ key: k, label: k, sessions: map.get(k)! }));
    }
    const secs = [...map.entries()]
      .sort((a, b) => b[1][0].last_active - a[1][0].last_active)
      .map(([path, ss]) => ({
        key: path,
        label: path.split("/").filter(Boolean).pop() ?? path,
        sessions: ss,
      }));
    // Sections are ranked by their newest session, which moves for the same
    // reason rows do — so hold them still too, and let the hand-drag order win
    // over both.
    return orderBy(keepPut("sections", secs, (s) => s.key), (s) => s.key, orders["sections:project"]);
  }, [viewMode, filtered, searchList, orders]);
  if (viewMode === "project" && autoSections) {
    containerSeq.current.set("sections:project", autoSections.map((s) => s.key));
  }

  const toggle = (k: keyof SessionDisplayOpts) => onOptsChange({ ...opts, [k]: !opts[k] });
  const { format: timeFormat, setFormat: setTimeFormat } = useTimeFormat();

  const filteredTrash = useMemo(() => {
    const q = query.toLowerCase();
    if (!q) return trashed;
    return trashed.filter(
      (t) => t.title.toLowerCase().includes(q) || t.project_path.toLowerCase().includes(q),
    );
  }, [trashed, query]);

  const moveToGroup = (groupId: string | null, projectPath: string) => {
    setGroups((gs) =>
      gs.map((g) => ({
        ...g,
        members:
          g.id === groupId
            ? [...g.members.filter((m) => m !== projectPath), projectPath]
            : g.members.filter((m) => m !== projectPath),
      })),
    );
  };

  const createGroup = (projectPath: string) => {
    setGroups((gs) => [
      ...gs.map((g) => ({ ...g, members: g.members.filter((m) => m !== projectPath) })),
      {
        id: crypto.randomUUID(),
        name: projectPath.split("/").filter(Boolean).pop() ?? `Group ${gs.length + 1}`,
        color: PALETTE[gs.length % PALETTE.length],
        collapsed: false,
        members: [projectPath],
      },
    ]);
  };

  const deleteGroup = (id: string) =>
    setGroups((gs) => gs.filter((g) => g.id !== id));

  const cycleColor = (id: string) =>
    setGroups((gs) =>
      gs.map((g) =>
        g.id === id
          ? { ...g, color: PALETTE[(PALETTE.indexOf(g.color) + 1) % PALETTE.length] }
          : g,
      ),
    );

  const commitRename = () => {
    if (renaming) {
      const name = renameText.trim();
      if (name) {
        setGroups((gs) => gs.map((g) => (g.id === renaming ? { ...g, name } : g)));
      }
    }
    setRenaming(null);
  };

  // Global pointer tracking while a drag is armed/active.
  useEffect(() => {
    const move = (e: PointerEvent) => {
      const arm = dragArm.current;
      if (!arm) return;
      if (!dragActive.current) {
        if (Math.abs(e.clientX - arm.x) + Math.abs(e.clientY - arm.y) < 7) return;
        dragActive.current = true;
        setDragId(arm.id);
      }
      setDragPos({ x: e.clientX, y: e.clientY });
      const el = document.elementFromPoint(e.clientX, e.clientY);
      setDragOver(
        arm.kind === "section"
          ? el?.closest<HTMLElement>("[data-secwrap]")?.dataset.secwrap ?? null
          : el?.closest<HTMLElement>("[data-drop]")?.dataset.drop ?? null,
      );
      // Hovering a sibling row shows the insertion line (reorder).
      const itemEl = el?.closest<HTMLElement>("[data-item]");
      const hoverId =
        arm.kind === "session" &&
        itemEl?.dataset.container === arm.container &&
        itemEl.dataset.item !== arm.id
          ? itemEl.dataset.item ?? null
          : null;
      setDragOverItem(hoverId);
      if (hoverId) {
        const seq = containerSeq.current.get(arm.container) ?? [];
        setDragOverAfter(seq.indexOf(arm.id) < seq.indexOf(hoverId));
      }
    };
    const up = (e: PointerEvent) => {
      const arm = dragArm.current;
      if (!arm) return;
      if (dragActive.current) {
        suppressClick.current = true;
        // The suppressed click (if any) dispatches synchronously after
        // pointerup; clear the flag so the next real click works.
        setTimeout(() => { suppressClick.current = false; }, 0);
        const el = document.elementFromPoint(e.clientX, e.clientY);
        const itemEl = el?.closest<HTMLElement>("[data-item]");
        const target = el?.closest<HTMLElement>("[data-drop]")?.dataset.drop;
        if (arm.kind === "section") {
          // Project-view sections reorder — dropping anywhere on another
          // section (header or its rows) targets that section.
          const sec = el?.closest<HTMLElement>("[data-secwrap]")?.dataset.secwrap;
          if (sec && sec !== arm.id) {
            const seq = containerSeq.current.get("sections:project") ?? [];
            const rest = seq.filter((k) => k !== arm.id);
            const after = seq.indexOf(arm.id) < seq.indexOf(sec);
            rest.splice(rest.indexOf(sec) + (after ? 1 : 0), 0, arm.id);
            setOrders((o) => ({ ...o, "sections:project": rest }));
          }
        } else if (arm.kind === "group") {
          // Dropping a group header on another header reorders the groups.
          if (target && target !== arm.id) {
            setGroups((gs) => {
              const from = gs.findIndex((g) => g.id === arm.id);
              const to = gs.findIndex((g) => g.id === target);
              if (from < 0 || to < 0) return gs;
              const next = [...gs];
              const [moved] = next.splice(from, 1);
              next.splice(to, 0, moved);
              return next;
            });
          }
        } else if (
          itemEl?.dataset.container === arm.container &&
          itemEl.dataset.item &&
          itemEl.dataset.item !== arm.id
        ) {
          // Dropped on a sibling row: rearrange within the container. A drag
          // of a selected row carries the whole selection along.
          const targetId = itemEl.dataset.item;
          const seq = containerSeq.current.get(arm.container) ?? [];
          const sel = selectedRef.current;
          const moving = sel.has(arm.id)
            ? seq.filter((id) => sel.has(id))
            : [arm.id];
          if (!moving.includes(targetId)) {
            const rest = seq.filter((id) => !moving.includes(id));
            const after = seq.indexOf(arm.id) < seq.indexOf(targetId);
            rest.splice(rest.indexOf(targetId) + (after ? 1 : 0), 0, ...moving);
            setOrders((o) => ({ ...o, [arm.container]: rest }));
          }
        } else if (target === "new") createGroup(arm.path);
        else if (target === "ungrouped") moveToGroup(null, arm.path);
        else if (target) moveToGroup(target, arm.path);
      }
      dragArm.current = null;
      dragActive.current = false;
      setDragId(null);
      setDragOver(null);
      setDragOverItem(null);
      setDragPos(null);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    return () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Every foldable section key visible in the current view, for the
  // collapse/expand-all toolbar button.
  const sectionKeys = useMemo(() => {
    const keys: string[] = ["projects", "trash"];
    if (viewMode === "recent") keys.push("recent:all");
    else if (autoSections) keys.push(...autoSections.map((s) => `${viewMode}:${s.label}`));
    return keys;
  }, [viewMode, autoSections]);
  const anyOpen =
    sectionKeys.some((k) => !collapsedSections.has(k)) ||
    (viewMode === "recent" && groups.some((g) => !g.collapsed));
  const toggleAll = () => {
    if (anyOpen) {
      setCollapsedSections((prev) => new Set([...prev, ...sectionKeys]));
      if (viewMode === "recent") {
        setGroups((gs) => gs.map((g) => ({ ...g, collapsed: true })));
      }
    } else {
      setCollapsedSections((prev) => {
        const next = new Set(prev);
        sectionKeys.forEach((k) => next.delete(k));
        return next;
      });
      if (viewMode === "recent") {
        setGroups((gs) => gs.map((g) => ({ ...g, collapsed: false })));
      }
    }
  };

  const renderTrash = (t: TrashedSession) => (
    <div key={t.id} className="session-item trash-item">
      <div className="agent-badge trash-badge">
        <Icon of={Trash2} />
      </div>
      <div className="session-text">
        <div className="session-title-row">
          <span className="session-title">{t.title}</span>
          <span className="session-time" title={fullTime(t.deleted_at)}>{fmtTimeShort(t.deleted_at, timeFormat)}</span>
        </div>
        <div className="session-meta">
          <span className="session-sub">
            {t.project_path ? homeAbbrev(t.project_path) : "unknown project"}
          </span>
        </div>
      </div>
      <div
        className={"session-actions" + (confirmDel === `trash:${t.id}` ? " confirming" : "")}
        onClick={(e) => e.stopPropagation()}
      >
        {confirmDel === `trash:${t.id}` ? (
          <>
            <span className="confirm-label">Delete forever?</span>
            <button
              className="act-btn danger"
              onClick={() => { setConfirmDel(null); onTrashDelete(t.id); }}
            >Delete</button>
            <button className="act-btn" onClick={() => setConfirmDel(null)}>Cancel</button>
          </>
        ) : (
          <>
            {/* An OpenCode entry is a row dump, not a file with a home to go
                back to — restore would refuse it, so it is not offered. The
                backend refuses on the same recognition; this hides a button,
                it does not guard anything. */}
            {!t.id.startsWith("ses_") && (
              <button
                className="act-btn" title="Restore session"
                onClick={() => onRestore(t.id)}
              >↩</button>
            )}
            <button
              className="act-btn danger" title="Delete forever…"
              onClick={() => setConfirmDel(`trash:${t.id}`)}
            ><Icon of={X} size="sm" /></button>
          </>
        )}
      </div>
    </div>
  );

  const renderItem = (s: Session, container?: string) => {
    // Two different facts, deliberately kept apart. `hasTab` is whether aiterm
    // has a terminal open for this row; `isRunning` is whether the session is
    // alive at all. They were one value, which put the live badge — and the
    // fork/exit menu, and the absence of Delete — on whichever row the tab
    // happened to name. After a background-mode resume that's the frozen
    // parent, while the real conversation runs elsewhere wearing no badge and
    // offering a Delete that would truncate it.
    const hasTab = liveSlots.has(s.id) || liveSlots.has(`shell:${s.project_path}`);
    // `liveSessions` is claude's roster, and it is the only word that counts
    // for a claude row — precisely because of the case above. But no other
    // engine appears in it at all, so asking it about a Codex session always
    // answered "not running": a Codex tab could be mid-reply and its row wore
    // no dot. Where there is no roster to consult, a tab whose process is still
    // alive is the evidence, and it is the same evidence a person is using when
    // they look at the terminal.
    const isRunning =
      liveSessions.has(s.id) ||
      (!capsOf(s.agent).roster_liveness && runningSlots.has(s.id));
    const hasAttn =
      attentionSlots.has(s.id) || attentionSlots.has(`shell:${s.project_path}`);
    // Same two slot keys the badge is looked up under, so the sentence and the
    // dot can never disagree about which row is asking.
    const notice = attentionText.get(s.id) ?? attentionText.get(`shell:${s.project_path}`);
    const prog = progressSlots.get(s.id) ?? progressSlots.get(`shell:${s.project_path}`);
    const isShowing = activeSlot !== null &&
      (activeSlot === s.id || activeSlot === `shell:${s.project_path}`);
    // Tighter than `isShowing`: the displayed terminal is *this session's own*
    // tab, not merely a shell in the same project (which would match every row
    // in that project). Only this row has nothing to resume — you are already
    // looking at the conversation.
    const isFocusedSession = activeSlot === s.id;
    // What this row's engine says it can do. Asked once per row: the three
    // lifecycle buttons below used to compare `s.agent` to a name each, which
    // is the check this refactor exists to remove.
    const caps = capsOf(s.agent);
    const isDragging =
      dragId === s.id || (dragId !== null && dragActive.current && selected.has(dragId) && selected.has(s.id));
    return (
      <div
        key={s.id}
        data-item={container ? s.id : undefined}
        data-container={container}
        onMouseEnter={(e) => { if (!isDragging) flyEnter(s, e.currentTarget, isRunning); }}
        onMouseLeave={flyLeave}
        onPointerDown={(e) => {
          if (e.button !== 0 || !container || searchList) return;
          if ((e.target as HTMLElement).closest("button")) return;
          dragArm.current = {
            // group_path, matching how membership is read below — dragging a
            // worktree session must group the repo it belongs to, not the
            // throwaway checkout path, or the drop silently does nothing.
            kind: "session", id: s.id, path: s.group_path,
            container, label: s.title, x: e.clientX, y: e.clientY,
          };
        }}
        className={
          "session-item" + (satellites.has(s.id) ? " satellite" : "") +
          // Only the single focused session tints — not every session that
          // happens to share the active project. Multi-highlight is opt-in
          // via ctrl/shift-click (builds the `selected` set below).
          (isShowing ? " active" : "") +
          (isRunning ? " live" : "") +
          (selected.has(s.id) ? " selected" : "") +
          (isShowing ? " showing" : "") +
          (isDragging ? " dragging" : "") +
          (dragOverItem === s.id ? " drop-mark" + (dragOverAfter ? " drop-after" : "") : "")
        }
        onClick={(e) => {
          if (suppressClick.current) {
            suppressClick.current = false;
            return;
          }
          if (e.ctrlKey || e.metaKey || e.shiftKey) {
            // Ctrl/Shift-click builds a multi-selection without switching
            // sessions; a plain click selects just this one.
            setSelected((prev) => {
              const next = new Set(prev);
              if (next.has(s.id)) next.delete(s.id);
              else next.add(s.id);
              return next;
            });
            return;
          }
          setSelected(new Set([s.id]));
          onSelect(s);
        }}
      >
        <div
          className={"agent-badge" + (s.agent === "claude" ? " claude" : "") + agentTint(s.agent).className}
          style={agentTint(s.agent).style}
        >
          <AgentIcon agent={s.agent} />
          {/* Green: the session is running. Whether aiterm also has a tab for
              it is still tracked (`hasTab`, for the row's actions) but no
              longer drawn — the row already says so by other means. */}
          {(isRunning || hasAttn) && (
            <span
              className={"live-dot badge-dot" + (hasAttn ? " attn" : "")}
              title={hasAttn ? notice ?? "Waiting for your input" : "Session is running"}
            />
          )}
        </div>
        <div className="session-text">
          <div className="session-title-row">
            {satellites.has(s.id) && (
              <span className="sat-mark" title="Brought into the session above">
                <Icon of={CornerDownRight} size="sm" />
              </span>
            )}
            {s.forked && (
              <span
                className="fork-mark"
                title={
                  s.background
                    ? "Forked from another session — carries its context up to the fork"
                    : "Continues an earlier session past a compaction"
                }
              >
                <Icon of={GitFork} size="sm" />
              </span>
            )}
            <span className="session-title">{s.title}</span>
            {(() => {
              const crew = crewCount.get(s.id) ?? 0;
              if (!crew) return null;
              const folded = foldedCrews.has(s.id);
              return (
                <button
                  className={"crew-badge" + (folded ? " folded" : "")}
                  title={folded ? `${crew} brought-in agent${crew === 1 ? "" : "s"} — click to show` : `${crew} brought-in agent${crew === 1 ? "" : "s"} — click to fold`}
                  onClick={(e) => {
                    e.stopPropagation();
                    setFoldedCrews((prev) => {
                      const next = new Set(prev);
                      if (next.has(s.id)) next.delete(s.id); else next.add(s.id);
                      return next;
                    });
                  }}
                ><Icon of={Users} size="sm" />+{crew}</button>
              );
            })()}
            {opts.showTime && <span className="session-time" title={fullTime(s.last_active)}>{fmtTimeShort(s.last_active, timeFormat)}</span>}
          </div>
          {(opts.showPath || (opts.showBranch && s.branch)) && (
            <div className="session-meta">
              {opts.showPath && (
                <span className="session-sub">{homeAbbrev(s.project_path)}</span>
              )}
              {opts.showBranch && s.branch && (
                <span className="branch">
                  <Icon of={GitBranch} size="sm" />
                  {s.branch}
                </span>
              )}
            </div>
          )}
          {/* The sentence the session sent, shown only while it is still
              waiting — once the badge clears this is a stale answer to a
              question nobody is asking any more. */}
          {hasAttn && notice && <div className="session-notice">{notice}</div>}
          {prog && (
            <div
              className={"session-prog s" + prog.state}
              title={
                prog.state === 2 ? "Reported an error"
                  : prog.state === 4 ? "Paused"
                  : prog.pct !== null ? `${prog.pct}% done`
                  : "Working"
              }
            >
              <span
                className="session-prog-fill"
                // Indeterminate has no width to give, so it is shown as a
                // moving sliver rather than a bar pretending to know.
                style={prog.pct !== null ? { width: `${prog.pct}%` } : undefined}
              />
            </div>
          )}
        </div>
        <div
          className={
            "session-actions" +
            (confirmDel === s.id || confirmStop === s.id ? " confirming" : "")
          }
          onClick={(e) => e.stopPropagation()}
        >
          {confirmStop === s.id ? (
            <>
              <span className="confirm-label">Stop it and resume?</span>
              <button
                className="act-btn"
                title="Stop the running session, then reopen it where it left off"
                onClick={() => { setConfirmStop(null); onResume(s); }}
              >Resume</button>
              <button
                className="act-btn" title="Leave it running"
                onClick={() => setConfirmStop(null)}
              >Cancel</button>
            </>
          ) : confirmDel === s.id ? (
            <>
              <span className="confirm-label">Delete session?</span>
              <button
                className="act-btn danger"
                title="Move this session's transcript and task data to ~/.claude/trash (kept 7 days)"
                onClick={() => { setConfirmDel(null); onDelete(s); }}
              >Delete</button>
              <button
                className="act-btn" title="Keep it"
                onClick={() => setConfirmDel(null)}
              >Cancel</button>
            </>
          ) : (
            <>
              <button
                className={"act-btn" + (stars.has(s.id) ? " on" : "")}
                title={stars.has(s.id) ? "Unpin" : "Pin to the top"}
                onClick={() => togglePin(s)}
              ><Icon of={Pin} size="sm" /></button>
              {/* Resume is the way into a session, always — the same move you
                  make in a shell: quit what's running, then `claude --resume`.
                  It used to be hidden whenever the roster called the session
                  live, which left ⑂ as the only door in; every attempt to
                  reopen a conversation branched a copy of it instead.
                  The one exception is the tab you are looking at right now:
                  offering to stop and reopen the conversation you are typing
                  into is never what you meant. Every *other* live row keeps ▶,
                  so the door-in problem above does not come back.
                  The second exception is an engine that cannot reopen anything.
                  This button was ungated, so a Codex row's ▶ ran
                  `claude --resume <codex-id>` against an id claude has never
                  heard of — a black pane. The resolver declines it now; not
                  offering it is the honest end of the same fix. */}
              {!isFocusedSession && caps.resume && (
                <button
                  className="act-btn"
                  title={
                    isRunning || hasTab
                      ? "Resume — stops the running session first"
                      : "Resume this session where it left off"
                  }
                  onClick={() =>
                    isRunning || hasTab ? setConfirmStop(s.id) : onResume(s)
                  }
                ><Icon of={Play} size="sm" /></button>
              )}
              {/* Branching is a deliberate act (two divergent lines from one
                  history), not a workaround for resume being unavailable.
                  Only where the engine has fork machinery in its transcript —
                  an API chat has none. */}
              {caps.fork && (
                <button
                  className="act-btn"
                  title="Branch a copy at this point — appears as a stopped session, resumable"
                  onClick={() => onFork(s)}
                ><Icon of={GitFork} size="sm" /></button>
              )}
              {/* aiterm's own clear — same end shape as typing /clear, built
                  like ⑂: no claude machinery, just a fresh process on a
                  minted id. Needs this session's own live terminal to act on,
                  and an engine that can be told what to call the new
                  conversation (Codex takes no session id, so it declares no
                  clear). */}
              {liveSlots.has(s.id) && caps.clear && (
                <button
                  className="act-btn"
                  title="Clear — fresh conversation in this tab; this one becomes a stopped row"
                  onClick={() => onClear(s)}
                >✦</button>
              )}
              {hasTab && (
                <button
                  className="act-btn" title="Exit — close this tab"
                  onClick={() => onExit(s)}
                >⏻</button>
              )}
              <button
                className="act-btn" title="New shell here"
                onClick={() => onNewShell(s)}
              >＋</button>
              {/* Never offer Delete on a running session: trashing one doesn't
                  stick — the process recreates its transcript seconds later,
                  rebuilt from the deletion point, losing the history before
                  it. Stop it first.
                  And only where the engine declares a delete that takes
                  exactly one session. For file-per-session stores (claude,
                  Codex, chats) that is a move into ~/.claude/trash; OpenCode
                  keeps every conversation in one `opencode.db`, so its delete
                  dumps the session's rows to the trash and removes them from
                  the database — readable for the keep window, but not
                  restorable by a rename, which is why its trash entry offers
                  no ↩. `session_delete` refuses on the same flag — this hides
                  a button, it does not guard anything. */}
              {!isRunning && !hasTab && caps.delete && (
                <button
                  className="act-btn danger" title="Delete session…"
                  onClick={() => setConfirmDel(s.id)}
                >
                  <Icon of={Trash2} size="sm" />
                </button>
              )}
            </>
          )}
        </div>
      </div>
    );
  };

  /** A session that exists as a running terminal and nothing else yet. Kept
   *  visually quieter than a real row — it is a tab you can get back to, not a
   *  conversation with any history to offer — and it carries no ▶/⑂/delete,
   *  none of which mean anything before the transcript exists. */
  const renderPending = (p: PendingSession) => {
    const isShowing = activeSlot === p.id;
    const hasAttn = attentionSlots.has(p.id);
    return (
      <div
        key={p.id}
        className={"session-item pending-item" + (isShowing ? " active showing" : "")}
        onClick={() => onSelectPending(p.id)}
      >
        <div
          className={"agent-badge" + (p.agent === "claude" ? " claude" : "") + agentTint(p.agent).className}
          style={agentTint(p.agent).style}
        >
          <AgentIcon agent={p.agent} />
          <span
            className={"live-dot badge-dot" + (hasAttn ? " attn" : "")}
            title={hasAttn ? "Waiting for your input" : "Session is starting"}
          />
        </div>
        <div className="session-text">
          <div className="session-title-row">
            <span className="session-title">{p.title}</span>
            <span className="session-time pending-tag">new</span>
          </div>
          <div className="session-meta">
            <span className="session-sub">{p.cwd ? homeAbbrev(p.cwd) : "no directory"}</span>
          </div>
        </div>
        <div className="session-actions" onClick={(e) => e.stopPropagation()}>
          <button
            className="act-btn" title="Exit — close this tab"
            onClick={() => onExitPending(p.id)}
          >⏻</button>
        </div>
      </div>
    );
  };

  const renderProject = (p: ProjectInfo) => {
    const isLive = liveSlots.has(`claude:${p.path}`) || liveSlots.has(`shell:${p.path}`);
    const hasAttn =
      attentionSlots.has(`claude:${p.path}`) || attentionSlots.has(`shell:${p.path}`);
    const isShowing = activeSlot === `claude:${p.path}` || activeSlot === `shell:${p.path}`;
    return (
      <div
        key={p.path}
        className={
          "session-item project-item" +
          (p.path === activeProject ? " active" : "") +
          (isShowing ? " showing" : "")
        }
        onClick={() => onSelectProject(p)}
      >
        <div className="agent-badge folder">
          <Icon of={Folder} className="agent-icon" />
          {(isLive || hasAttn) && (
            <span
              className={"live-dot badge-dot" + (hasAttn ? " attn" : "")}
              title={hasAttn ? "Waiting for your input" : "Terminal running"}
            />
          )}
        </div>
        <div className="session-text">
          <div className="session-title-row">
            <span className="session-title">{p.name}</span>
          </div>
          {opts.showPath && (
            <div className="session-meta">
              <span className="session-sub">{homeAbbrev(p.path)}</span>
            </div>
          )}
        </div>
        <div className="session-actions">
          <button
            className="act-btn" title="Start claude here"
            onClick={(e) => { e.stopPropagation(); onProjectClaude(p); }}
          ><Icon of={Play} size="sm" /></button>
          <button
            className="act-btn" title="New shell here"
            onClick={(e) => { e.stopPropagation(); onProjectShell(p); }}
          >＋</button>
        </div>
      </div>
    );
  };

  return (
    <div className="sessions-panel" onPointerEnter={() => setPointerIn(true)} onPointerLeave={() => setPointerIn(false)}>
      {fly && (
        <div
          style={{ position: "fixed", left: fly.left, top: fly.top, bottom: fly.bottom, zIndex: 70 }}
          onMouseEnter={flyHold}
          onMouseLeave={flyLeave}
        >
          <SessionFlyout session={fly.s} detail={flyDetail && flyDetail.id === fly.s.id ? flyDetail : null} />
        </div>
      )}
      <div className="panel-toolbar">
        <div className="search-box">
          <Icon of={Search} size="sm" className="search-icon" />
          <input
            className="search-input"
            placeholder="Search sessions…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => e.key === "Escape" && setQuery("")}
          />
          {query && (
            <button className="search-clear" title="Clear" onClick={() => setQuery("")}><Icon of={X} size="sm" /></button>
          )}
        </div>
        {/* Starting a session used to require a row to start it *from*: ▶ on a
            project only appears for projects with no sessions, and everything
            else in the panel resumes, branches, or opens a shell. So there was
            no way to begin a new conversation anywhere else — including, on a
            machine with no `~/Projects`, no way to begin one at all. */}
        <button
          className={"icon-btn" + (showNewSession ? " on" : "")}
          title="Start a session…"
          data-new-session-trigger=""
          onClick={() => setShowNewSession((v) => !v)}
        >＋</button>
        <button className="icon-btn" title="Refresh" onClick={onRefresh}><Icon of={RefreshCw} /></button>
        <button
          className="icon-btn"
          title={anyOpen ? "Collapse all" : "Expand all"}
          onClick={toggleAll}
        >
          {anyOpen ? <Icon of={ChevronsDownUp} size="sm" /> : <Icon of={ChevronsUpDown} size="sm" />}
        </button>
        <div className="settings-wrap">
          <button
            className={"icon-btn" + (showSettings ? " on" : "")}
            title="Display settings"
            onClick={() => setShowSettings(!showSettings)}
          >
            <Icon of={SettingsIcon} />
          </button>
          {showSettings && (
            <div className="settings-pop">
              <label><input type="checkbox" checked={opts.showPath} onChange={() => toggle("showPath")} /> Project path</label>
              <label><input type="checkbox" checked={opts.showBranch} onChange={() => toggle("showBranch")} /> Git branch</label>
              <label><input type="checkbox" checked={opts.showTime} onChange={() => toggle("showTime")} /> Last active</label>
              <label title="Clock time instead of “3h” — the same setting as Settings → Appearance">
                <input
                  type="checkbox"
                  checked={timeFormat === "absolute"}
                  onChange={() => setTimeFormat(timeFormat === "absolute" ? "relative" : "absolute")}
                /> As clock time
              </label>
            </div>
          )}
        </div>
      </div>
      {/* Anchored to the panel rather than to the ＋, so it still fits when
          the sidebar is dragged narrow — a fixed-width popover hung off a
          button near the right edge spills over the terminal. */}
      {showNewSession && (
        <NewSessionMenu
          places={startPoints}
          onPick={(path, choice) => { setShowNewSession(false); onNewSession(path, choice); }}
          onClose={() => setShowNewSession(false)}
          onOpenModelAccess={onOpenModelAccess}
        />
      )}
      <div className="view-tabs">
        {(["recent", "project", "date"] as ViewMode[]).map((m) => (
          <button
            key={m}
            className={"view-tab" + (viewMode === m ? " on" : "")}
            onClick={() => setViewMode(m)}
          >
            {m === "recent" ? "Recent" : m === "project" ? "Project" : "Date"}
          </button>
        ))}
      </div>
      <div className="sessions-list">
        {/* Above every view and outside the search filter: a session you just
            started is the one row you are certainly looking for, and it has no
            title or text to match a query against yet. */}
        {pending.length > 0 && (
          <div className="session-group">
            {pending.map(renderPending)}
          </div>
        )}
        {!searchList && pinned.length > 0 && (
          <div className="session-group pinned">
            <div
              className="group-header static clickable"
              onClick={() => toggleSection("pinned")}
            >
              <span className={"chevron" + (sectionOpen("pinned") ? " open" : "")}>›</span>
              <span className="group-name"><Icon of={Pin} size="sm" /> Pinned</span>
              <span className="group-count">{pinned.length}</span>
            </div>
            {sectionOpen("pinned") && pinned.map((s) => renderItem(s, "pinned"))}
          </div>
        )}
        {searchList ? (
          <>
            {searchList.map((s) => renderItem(s))}
            {searchList.length === 0 && sessionlessProjects.length === 0 && (
              <div className="empty-note">No matches</div>
            )}
          </>
        ) : autoSections ? (
          autoSections.map((sec) => {
            const key = `${viewMode}:${sec.label}`;
            const open = sectionOpen(key);
            const draggable = viewMode === "project";
            const secKeys = autoSections.map((x) => x.key);
            const isTarget =
              dragArm.current?.kind === "section" &&
              dragId !== null && dragId !== sec.key && dragOver === sec.key;
            const targetCls = isTarget
              ? secKeys.indexOf(dragId!) < secKeys.indexOf(sec.key)
                ? " sec-drop-after"
                : " sec-drop-before"
              : "";
            return (
              <div
                key={sec.key}
                className={"session-group" + targetCls}
                data-secwrap={draggable ? sec.key : undefined}
              >
                <div
                  className={
                    "group-header static clickable" +
                    (dragId === sec.key ? " dragging" : "")
                  }
                  onPointerDown={(e) => {
                    if (e.button !== 0 || !draggable) return;
                    if ((e.target as HTMLElement).closest("button, input")) return;
                    dragArm.current = {
                      kind: "section", id: sec.key, path: "", container: "",
                      label: sec.label, x: e.clientX, y: e.clientY,
                    };
                  }}
                  onClick={() => {
                    if (suppressClick.current) {
                      suppressClick.current = false;
                      return;
                    }
                    toggleSection(key);
                  }}
                  // Only in the project view. A date bucket is not a thing you
                  // own — "trash everything from Older" has no use that is worth
                  // putting one mis-click away from clearing out months.
                  onContextMenu={
                    viewMode === "project"
                      ? (e) => openSetMenu(e, sec.label, sec.sessions)
                      : undefined
                  }
                >
                  <span className={"chevron" + (open ? " open" : "")}>›</span>
                  <span className="group-name">{sec.label}</span>
                  <span className="group-count">{sec.sessions.length}</span>
                </div>
                {open && sec.sessions.map((s) => renderItem(s))}
              </div>
            );
          })
        ) : (
          <>
        {dragId && dragArm.current?.kind === "session" && (
          <div
            className={"drop-zone" + (dragOver === "new" ? " over" : "")}
            data-drop="new"
          >
            ＋ Drop here to create a group
          </div>
        )}
        {groups.map((g) => {
          const members = groupMembers.get(g.id) ?? [];
          if (members.length === 0 && query) return null;
          const open = !g.collapsed || query.length > 0;
          return (
            <div key={g.id} className="session-group">
              <div
                className={
                  "group-header" +
                  (dragOver === g.id ? " over" : "") +
                  (dragId === g.id ? " dragging" : "")
                }
                data-drop={g.id}
                onPointerDown={(e) => {
                  if (e.button !== 0) return;
                  if ((e.target as HTMLElement).closest("button, input")) return;
                  dragArm.current = {
                    kind: "group", id: g.id, path: "", container: "",
                    label: g.name, x: e.clientX, y: e.clientY,
                  };
                }}
                onClick={() => {
                  if (suppressClick.current) {
                    suppressClick.current = false;
                    return;
                  }
                  setGroups((gs) =>
                    gs.map((x) => (x.id === g.id ? { ...x, collapsed: !x.collapsed } : x)));
                }}
                onContextMenu={(e) => openSetMenu(e, g.name, members)}
              >
                <span className={"chevron" + (open ? " open" : "")}>›</span>
                <span
                  className="group-dot"
                  style={{ background: g.color }}
                  title="Change color"
                  onClick={(e) => { e.stopPropagation(); cycleColor(g.id); }}
                />
                {renaming === g.id ? (
                  <input
                    className="group-rename"
                    autoFocus
                    value={renameText}
                    onChange={(e) => setRenameText(e.target.value)}
                    onBlur={commitRename}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") commitRename();
                      if (e.key === "Escape") setRenaming(null);
                    }}
                    onClick={(e) => e.stopPropagation()}
                  />
                ) : (
                  <span
                    className="group-name"
                    title="Double-click to rename"
                    onDoubleClick={(e) => {
                      e.stopPropagation();
                      setRenaming(g.id);
                      setRenameText(g.name);
                    }}
                  >{g.name}</span>
                )}
                <span className="group-count">{members.length}</span>
                <button
                  className="icon-btn group-del"
                  title="Ungroup (items go back to the main list)"
                  onClick={(e) => { e.stopPropagation(); deleteGroup(g.id); }}
                ><Icon of={X} size="sm" /></button>
              </div>
              {open && members.map((s) => renderItem(s, `group:${g.id}`))}
            </div>
          );
        })}
        <div className="session-group">
          <div
            className="group-header static clickable"
            onClick={() => toggleSection("recent:all")}
          >
            <span className={"chevron" + (sectionOpen("recent:all") ? " open" : "")}>›</span>
            <span className="group-name">Recent</span>
            <span className="group-count">{ungrouped.length}</span>
          </div>
          <div
            className={"ungrouped" + (dragOver === "ungrouped" ? " over" : "")}
            data-drop="ungrouped"
          >
            {sectionOpen("recent:all") &&
              (query.trim() || showAllRecent ? ungrouped : ungrouped.slice(0, RECENT_WINDOW))
                .map((s) => renderItem(s, "recent"))}
            {sectionOpen("recent:all") && !query.trim() && ungrouped.length > RECENT_WINDOW && (
              <button
                className="tui-plain session-show-all"
                onClick={() => setShowAllRecent((v) => !v)}
              >
                {showAllRecent
                  ? `Show the first ${RECENT_WINDOW} — the full list is what makes the app slow`
                  : `Show all ${ungrouped.length} — search reaches them regardless`}
              </button>
            )}
            {filtered.length === 0 && <div className="empty-note">No sessions found</div>}
          </div>
        </div>
          </>
        )}
        {sessionlessProjects.length > 0 && (
          <div className="session-group">
            <div
              className="group-header static clickable projects-header"
              onClick={() => toggleSection("projects")}
            >
              <span className={"chevron" + (sectionOpen("projects") ? " open" : "")}>›</span>
              <span className="group-name">Projects</span>
              <span className="group-count">{sessionlessProjects.length}</span>
            </div>
            {sectionOpen("projects") && sessionlessProjects.map(renderProject)}
          </div>
        )}
        {filteredTrash.length > 0 && (
          <div className="session-group trash-group">
            <div
              className="group-header static clickable"
              onClick={() => toggleSection("trash")}
              onContextMenu={(e) => {
                e.preventDefault();
                setMenuArmed(false);
                setMenu({
                  x: e.clientX,
                  y: e.clientY,
                  // The whole trash, never `filteredTrash` — emptying ignores
                  // the search box, and offering to remove the two rows on
                  // screen while taking forty would be a lie.
                  idle: `Empty trash (${trashed.length})`,
                  armed: `Delete ${trashed.length} session${trashed.length === 1 ? "" : "s"} for good?`,
                  run: onTrashEmpty,
                });
              }}
            >
              <span className={"chevron" + (sectionOpen("trash") ? " open" : "")}>›</span>
              <span className="group-name">Trash</span>
              <span className="group-count">{filteredTrash.length}</span>
              {emptyConfirm ? (
                <span className="trash-empty-confirm" onClick={(e) => e.stopPropagation()}>
                  <span className="confirm-label">Empty trash?</span>
                  <button
                    className="act-btn danger"
                    onClick={() => { setEmptyConfirm(false); onTrashEmpty(); }}
                  >Empty</button>
                  <button className="act-btn" onClick={() => setEmptyConfirm(false)}>Cancel</button>
                </span>
              ) : (
                <button
                  className="icon-btn group-del"
                  title="Empty trash…"
                  onClick={(e) => { e.stopPropagation(); setEmptyConfirm(true); }}
                ><Icon of={X} size="sm" /></button>
              )}
            </div>
            {sectionOpen("trash") && filteredTrash.map(renderTrash)}
          </div>
        )}
      </div>
      {menu && (
        // The backdrop is what closes it on a click anywhere else, including a
        // right-click somewhere new.
        <div
          className="ctx-backdrop"
          onMouseDown={closeMenu}
          onContextMenu={(e) => { e.preventDefault(); closeMenu(); }}
        >
          <div
            className="ctx-menu"
            style={{ left: menu.x, top: menu.y }}
            onMouseDown={(e) => e.stopPropagation()}
          >
            <button
              className={"ctx-item danger" + (menuArmed ? " armed" : "")}
              onClick={() => {
                if (!menuArmed) { setMenuArmed(true); return; }
                closeMenu();
                menu.run();
              }}
            >
              {menuArmed ? menu.armed : menu.idle}
            </button>
            {menu.note && <div className="ctx-note">{menu.note}</div>}
          </div>
        </div>
      )}
      {dragId && dragPos && (
        <div className="drag-ghost" style={{ left: dragPos.x + 12, top: dragPos.y + 8 }}>
          {dragArm.current?.label ?? ""}
        </div>
      )}
    </div>
  );
}
