import { makeWriteQueue } from "./writeQueue";
import { invoke, Channel } from "@tauri-apps/api/core";

export interface Session {
  id: string;
  agent: string;
  title: string;
  /** True cwd — what a resumed tab spawns in. */
  project_path: string;
  /** Project this row groups under; differs from project_path only for
   *  sessions inside a Claude Code worktree, which group under their repo. */
  group_path: string;
  branch: string | null;
  /** Continues a conversation held in another transcript (a /fork child or a
   *  compact continuation) — its parent stays on disk, frozen at the fork. */
  forked: boolean;
  /** Ran as a background agent under the daemon (/fork or --bg). */
  background: boolean;
  /** Session this was forked from, from Claude Code job state — known as soon
   *  as /fork runs, before the fork's transcript has any messages. */
  fork_parent: string | null;
  last_active: number;
}

export interface DirEntry {
  name: string;
  path: string;
  is_dir: boolean;
}

export interface FileStatus {
  path: string;
  status: string;
  staged: boolean;
}

export interface BranchInfo {
  name: string;
  is_head: boolean;
  upstream: string | null;
}

export interface CommitInfo {
  id: string;
  short_id: string;
  summary: string;
  author: string;
  time: number;
  refs: string[];
  parents: string[];
}

export interface RepoState {
  is_repo: boolean;
  branch: string | null;
  ahead: number;
  behind: number;
}

export interface SessionStatus {
  exists: boolean;
  permission_mode: string | null;
  mode: string | null;
}

export interface SessionTask {
  id: string;
  subject: string;
  status: "pending" | "in_progress" | "completed" | string;
  active_form: string | null;
  blocked_by: string[];
}

export interface Artifact {
  path: string;
  tool: string;
  at: string;
}

export interface AgentRun {
  id: string;
  agent_type: string;
  description: string;
  status: "running" | "done" | string;
  started_at: string | null;
  result: string | null;
}

export interface PreviewMsg {
  role: "user" | "assistant" | string;
  text: string;
  at: string | null;
}

export const listSessions = () => invoke<Session[]>("list_sessions");
export const sessionPreview = (sessionId: string) =>
  invoke<PreviewMsg[]>("session_preview", { sessionId });

/** Everything worth remembering about a session, from one read of its
 *  transcript — the sidebar's hover flyout. See `detail.rs`. */
export interface SessionDetail {
  id: string;
  started: string | null;
  last_active: string | null;
  cwd: string | null;
  branch: string | null;
  cli_version: string | null;
  /** In order of first use; more than one means the model was switched. */
  models: string[];
  effort: string | null;
  permission_mode: string | null;
  user_messages: number;
  assistant_messages: number;
  tool_calls: number;
  tools: { name: string; count: number }[];
  /** What the context window held at the last assistant turn. */
  context_tokens: number | null;
  context_window: number | null;
  output_tokens: number;
  title: string | null;
  first_prompt: string | null;
  last_user: string | null;
  last_assistant: string | null;
  /** Written or edited, most recent first. */
  files: string[];
  pr_links: string[];
  compactions: number;
}
export const sessionDetail = (sessionId: string) =>
  invoke<SessionDetail | null>("session_detail", { sessionId });
/** A person-chosen title for the session; empty restores the engine title. */
export const sessionRename = (sessionId: string, title: string) =>
  invoke<void>("session_rename", { sessionId, title });
export const sessionTitles = () =>
  invoke<Record<string, string>>("session_titles");
export const sessionStars = () => invoke<string[]>("session_stars");
export const sessionBroughtIn = () =>
  invoke<Record<string, string>>("session_brought_in");
export const sessionStar = (sessionId: string, on: boolean) =>
  invoke<void>("session_star", { sessionId, on });
export const relayReport = (
  sessionId: string,
  bSessionId: string | null,
  bName: string,
  phase: string,
  round: number,
  rounds: number,
  note: string,
) => invoke<void>("relay_report", {
  sessionId, bSessionId, bName, phase, round, rounds, note,
});
export const sessionDelete = (sessionId: string) =>
  invoke<void>("session_delete", { sessionId });

export interface TrashedSession {
  id: string;
  title: string;
  project_path: string;
  deleted_at: number;
}

export const trashList = () => invoke<TrashedSession[]>("trash_list");
export const trashRestore = (sessionId: string) =>
  invoke<void>("trash_restore", { sessionId });
export const trashDelete = (sessionId: string) =>
  invoke<void>("trash_delete", { sessionId });
export const trashEmpty = () => invoke<void>("trash_empty");
export const sessionTasks = (sessionId: string) =>
  invoke<SessionTask[]>("session_tasks", { sessionId });
export const sessionArtifacts = (sessionId: string) =>
  invoke<Artifact[]>("session_artifacts", { sessionId });
export const sessionAgents = (sessionId: string) =>
  invoke<AgentRun[]>("session_agents", { sessionId });
export const runningSessionIds = () =>
  invoke<string[]>("running_session_ids");
// Session UUIDs held by the Claude Code daemon as background agents (incl.
// prompt-less /fork stubs). These can't be resumed as-is — reach them via
// the agent view (`claude agents`).
export const bgAgentSessionIds = () =>
  invoke<string[]>("bg_agent_session_ids");
// Sessions aiterm can't reliably stop — daemon-held with no pid, or background
// agents whose roster pid may be a helper rather than the conversation. The
// resume path asks before it closes anything, so a stop it can't win doesn't
// cost a tab. See `unstoppable_session_ids`.
export const unstoppableSessionIds = () =>
  invoke<string[]>("unstoppable_session_ids");
/** How a conversation left the session id a tab is pinned to.
 *  - `background`: the agents view moved it to the daemon; the old id is a dead
 *    end that nothing will write again.
 *  - `cleared`: `/clear` started a fresh conversation in the same terminal; the
 *    old id is a finished conversation that stays resumable on its own. */
export type MoveKind = "background" | "cleared";
export interface SessionMove {
  id: string;
  kind: MoveKind;
}
// The session that took over this one's conversation, or null — the normal
// answer. A tab pinned to the old id shows live text over dead panels until it
// re-keys, and its live conversation sits unowned in the sidebar.
export const sessionMovedTo = (sessionId: string) =>
  invoke<SessionMove | null>("session_moved_to", { sessionId });
/** A session start one of our claudes reported through its SessionStart hook,
 *  already resolved to the authoritative tab it happened in. `source` is claude's own word
 *  for why: "startup", "resume", "clear", "compact". */
export interface SessionEvent {
  tabId: TabId;
  tab: TabDescriptor;
  sessionId: string;
  source: string;
}
// Collect (and consume) the hook reports since last asked. The exact
// counterpart to the sessionMovedTo heuristic: no inference, just what each
// claude process said about itself, tied to the tab aiterm ran it in.
export const drainSessionEvents = () =>
  invoke<SessionEvent[]>("drain_session_events");
/** Whether verbose trace capture is on (Settings → Diagnostics). */
export const traceStatus = () => invoke<boolean>("trace_status");
/** Toggle verbose trace capture. On: returns the trace.log path (truncated
 *  fresh); off: returns null. Works in release builds — the filter is
 *  runtime-reloadable. */
export const traceSet = (on: boolean) =>
  invoke<string | null>("trace_set", { on });
// Journal-visible logging from the webview. Release builds drop the console,
// so errors the UI catches quietly would otherwise vanish — send the ones
// worth keeping to stderr, where journalctl already collects them.
export const uiLog = (msg: string) =>
  invoke<void>("ui_log", { msg }).catch(() => {});
// Every session the daemon currently holds, background AND interactive — i.e.
// "is this session alive right now". Distinct from "aiterm has a tab open for
// it": after a background-mode resume the tab holds the parent id while the
// conversation runs under a new one, so the two point at different rows.
export const liveSessionIds = () =>
  invoke<string[]>("live_session_ids");
// Stop a running session (SIGTERM its process tree, SIGKILL what survives) so
// `claude --resume` will take it. Resolves only once it's actually dead;
// rejects if it can't be stopped. Same move as quitting claude in a shell
// before resuming it.
export const stopSession = (sessionId: string) =>
  invoke<void>("stop_session", { sessionId });
// Resolve a pinned session id to one `claude --resume` can open now (follows
// the fork family). null → the original was cleared/superseded and nothing
// resumable survives.
export const resolveResumableId = (sessionId: string) =>
  invoke<string | null>("resolve_resumable_id", { sessionId });
// Branch a session on disk: copies its transcript under a fresh id and returns
// that id. Starts nothing — the branch appears as an inactive row, and the
// session you forked from keeps running untouched. Rejects if the transcript
// is gone or was cleared.
export const sessionFork = (sessionId: string) =>
  invoke<string>("session_fork", { sessionId });
// Write the conversation a `/fork` only promised: copies the parent's history
// up to the fork boundary under this session's id. Rejects unless the session
// really is an empty /fork stub with its parent still on disk.
export const materializeFork = (sessionId: string) =>
  invoke<void>("materialize_fork", { sessionId });
// The permission mode Claude Code's own config asks for in this directory.
// null when nothing is configured (or it names a mode the CLI would reject).
export const claudePermissionMode = (projectPath: string) =>
  invoke<string | null>("claude_permission_mode", { projectPath });
// Claude Code's global default model for new sessions (~/.claude/settings.json).
export const claudeModelDefault = () =>
  invoke<string | null>("claude_model_default");
// Put that global default back after a `/model` command changed it. Typing
// `/model <name>` retargets the running session AND rewrites the global
// default; the pill is a per-session control, so it undoes the second half.
export const restoreClaudeModelDefault = (previous: string | null) =>
  invoke<boolean>("restore_claude_model_default", { previous });
export interface ModelChoice {
  /** Full id as recorded, e.g. "claude-opus-5". null before the first reply. */
  model: string | null;
  effort: string | null;
  /** Timestamp of the record these came from — tells a pending request from a
   *  settled fact. Unchanged means no turn has run since you clicked. */
  at: string | null;
  /** Context-window fill after the last main-chain reply, in tokens. */
  context_tokens: number | null;
}
// What the session last actually ran with, read from its transcript — so it
// stays right whether the pill, a typed /model, or a launch flag changed it.
export const sessionModel = (sessionId: string) =>
  invoke<ModelChoice>("session_model", { sessionId });

// A classifier refusal recorded in a session's transcript — the trigger for the
// downgrade one-tap. null when the tail holds none.
export interface Refusal {
  /** Unique id of the refusal record; dedupe on it so an old one doesn't re-fire. */
  uuid: string;
  /** "model_refusal_fallback" (soft switch) or "model_refusal_no_fallback" (hard block). */
  subtype: string;
  /** The hard-block case: nothing auto-switched, so restoring is the only move. */
  hard: boolean;
  /** The classifier notice, shown verbatim. */
  content: string;
  /** The model in use before the switch — the restore target, e.g. "claude-fable-5". */
  original_model: string | null;
  /** What it switched to, e.g. "claude-opus-4-8". */
  fallback_model: string | null;
  /** The category the classifier assigned, e.g. "cyber". */
  category: string | null;
  /** The flagged user message's text — the prompt to hand OpenCode. */
  refused_prompt: string | null;
  at: string | null;
}
export const sessionRefusal = (sessionId: string) =>
  invoke<Refusal | null>("session_refusal", { sessionId });

// Run a task on OpenCode headlessly and get its report back. Blocks for the whole
// run, so call it off the UI path and show progress meanwhile.
export interface OpencodeReport {
  session_id: string;
  text: string;
}
export const opencodeDispatch = (
  prompt: string,
  cwd: string,
  provider: string | null,
  model: string | null,
) => invoke<OpencodeReport>("opencode_dispatch", { prompt, cwd, provider, model });

// The provider + model OpenCode is configured to launch with (its live startup
// pin), so a UI dispatch names the same model an interactive tab would open on.
export interface AgentTarget {
  provider: string | null;
  model: string | null;
}
export const opencodeDefaultTarget = () =>
  invoke<AgentTarget>("opencode_default_target");
export interface UsageBar {
  kind: string;
  label: string;
  percent: number;
  severity: string;
  resets_at: string;
}
export interface UsageAmount {
  label: string;
  amount: number;
  /** The total `amount` counts against, when the service names one. */
  of: number | null;
  /** ISO code when the amount is known to be money in one: Anthropic says
   *  "USD" itself, and OpenRouter's credits are dollars by its own docs. Codex
   *  and Grok name nothing, so their balances print bare. */
  currency: string;
  /** "remaining" — `amount` is what is left. "used" — it is what was spent. */
  sense: string;
}
/** One service's answer. Always present, even when it could not be reached:
 *  see `usage.rs` on why an absent row is not an acceptable way to say "no". */
export interface UsageSource {
  /** "anthropic" | "codex" | "grok" | "antigravity" | "provider:<id>". */
  id: string;
  name: string;
  /** "ok" | "signed_out" | "unreachable" | "rejected" | "limited" | "no_balance".
   *  "limited" is a 429: the service will answer again shortly, nothing is
   *  wrong with the login. */
  state: string;
  /** What to do about a non-"ok" state. Empty when "ok". */
  detail: string;
  plan: string;
  account: string;
  bars: UsageBar[];
  amounts: UsageAmount[];
  notes: string[];
}
// Plan limits and credit balances for every service aiterm can see, in one
// call. Exactly one caller polls this (App) and everything that shows usage
// renders from its result — /api/oauth/usage rate limits, and a second poller
// would both double the request rate and let the two views disagree.
export const usageReport = () => invoke<UsageSource[]>("usage_report");

/** What a session's spine log says it is doing — `spine/ipc.rs`. */
export type SpinePhase = "working" | "needs_you" | "idle";

export interface SpineTool {
  /** The adapter's human title: "Edit foo.rs", "Bash". */
  title: string;
  /** "pending" | "running" | "completed" | "failed" | "cancelled". */
  status: string;
}

/** One session as the fleet board draws it. A snapshot of the registry's
 *  ring, not a feed: no transcript is read and no tail is started, so the
 *  home screen can poll it while it is on screen. */
export interface SpineOverview {
  session_id: string;
  agent: string;
  phase: SpinePhase;
  /** The phase's human half: "running Bash", "permission: Edit foo.rs". */
  detail: string;
  turn_open: boolean;
  /** Ms; null when no turn is open — nothing for a timer to count from. */
  turn_started_ts: number | null;
  /** Last line of the most recent assistant block, clipped to 120. */
  last_text: string | null;
  last_tool: SpineTool | null;
}

/** Every session the spine holds a log for. Cheap — see `spine/ipc.rs`. */
export const spineOverview = () => invoke<SpineOverview[]>("spine_overview");

export interface FontFamily {
  name: string;
  /** Fixed-pitch per fontconfig — the set worth offering for a terminal. */
  mono: boolean;
}
export interface FontPackage {
  name: string;
  package: string;
  note: string;
  installed: boolean;
}
// Every installed font family, straight from fontconfig. Replaces the old
// canvas width-probe, which could only find fonts we had thought to name.
export const listFonts = () => invoke<FontFamily[]>("list_fonts");
// Coding fonts installable from the distro repos, each flagged with whether
// it is already present.
export const fontPackages = () => invoke<FontPackage[]>("font_packages");
// Install one of those packages. Only names from `font_packages` are accepted;
// the backend refuses anything else rather than passing it to dnf.
export const installFontPackage = (pkg: string) =>
  invoke<string>("install_font_package", { package: pkg });
// Copy font files into ~/.local/share/fonts — no privileges, no network.
// Returns how many were installed.
export const installFontFiles = (paths: string[]) =>
  invoke<number>("install_font_files", { paths });
export const sessionStatus = (sessionId: string) =>
  invoke<SessionStatus>("session_status", { sessionId });
export interface ProjectInfo {
  name: string;
  path: string;
  is_git: boolean;
  last_modified: number;
}

export const watchProject = (path: string) => invoke<void>("watch_project", { path });
export const listDir = (path: string) => invoke<DirEntry[]>("list_dir", { path });
export const openPath = (path: string) => invoke<void>("open_path", { path });

/** A text file for the in-app viewer — see `fsx.rs`. */
export interface TextFile {
  content: string;
  /** mtime the content was read at; the save's compare-and-swap token. */
  mtime_ms: number;
  /** Only the head of a >2 MB file — shown read-only. */
  truncated: boolean;
}

export const readTextFile = (path: string) =>
  invoke<TextFile>("read_text_file", { path });
/** Returns the new mtime. Rejects with "changed-on-disk" when the file moved
 *  past `expectedMtimeMs`; pass null to overwrite deliberately. */
export const writeTextFile = (
  path: string, content: string, expectedMtimeMs: number | null,
) => invoke<number>("write_text_file", { path, content, expectedMtimeMs });
export const listProjects = () => invoke<ProjectInfo[]>("list_projects");
export const searchSessions = (query: string) =>
  invoke<Session[]>("search_sessions", { query });
export const reindexSessions = () =>
  invoke<{ indexed: number; total: number }>("reindex_sessions");

export type TabId = string;
export type AttachmentId = string;

export interface TabDescriptor {
  id: TabId;
  title: string;
  cwd: string | null;
  command: string | null;
  sessionId?: string;
  resumedId?: string;
  agentId?: string;
  slotId: string;
  fresh?: boolean;
  envProvider?: string;
  envModel?: string;
  size?: { cols: number; rows: number };
  /** Safe process-wide focus projection; never contains an attachment id. */
  focus?: "desktop" | "remote" | "unowned";
  state?: "running" | "exited";
  exit?: { code: number | null; signal: string | null; requested: boolean };
}

export interface TabRegistrySnapshot {
  revision: number;
  tabs: TabDescriptor[];
}

export type TabRegistryEvent =
  | { change: "snapshot"; revision: number; tabs: TabDescriptor[] }
  | { change: "opened" | "changed"; revision: number; tabId: TabId; tab: TabDescriptor }
  | { change: "removed"; revision: number; tabId: TabId; requested: boolean };

export interface TabLaunch {
  title: string;
  cwd: string | null;
  command: string | null;
  sessionId?: string;
  resumedId?: string;
  agentId?: string;
  slotId: string;
  fresh?: boolean;
  envProvider?: string;
  envModel?: string;
  size: { cols: number; rows: number };
}

export interface TabUpdate {
  title?: string;
  sessionId?: string;
  resumedId?: string;
  agentId?: string;
  slotId?: string;
  fresh?: boolean;
}

export const tabOpen = (launch: TabLaunch) =>
  invoke<TabDescriptor>("tab_open", { launch });
export const tabList = () => invoke<TabDescriptor[]>("tab_list");
export const tabRegistrySnapshot = () =>
  invoke<TabRegistrySnapshot>("tab_registry_snapshot");
export const tabUpdate = (tabId: TabId, update: TabUpdate) =>
  invoke<TabDescriptor>("tab_update", { tabId, update });
export const tabAttachDesktop = (
  tabId: TabId, onOutput: Channel<ArrayBuffer>,
) => invoke<AttachmentId>("tab_attach_desktop", { tabId, onOutput });
export const tabDetach = (tabId: TabId, attachmentId: AttachmentId) =>
  invoke<void>("tab_detach", { tabId, attachmentId });

type TabWriteTarget = { tabId: TabId; attachmentId: AttachmentId };
const queuedTabWrite = makeWriteQueue<TabWriteTarget>(
  (target, data) => invoke<void>("tab_write", { ...target, data }),
  (target) => `${target.tabId}\0${target.attachmentId}`,
);
export const tabWrite = (tabId: TabId, attachmentId: AttachmentId, data: string) =>
  queuedTabWrite({ tabId, attachmentId }, data);

const pendingTabSize = new Map<string, {
  target: TabWriteTarget;
  cols: number;
  rows: number;
}>();
const tabResizing = new Map<string, Promise<void>>();
export const tabResize = (
  tabId: TabId, attachmentId: AttachmentId, cols: number, rows: number,
): Promise<void> => {
  const identity = `${tabId}\0${attachmentId}`;
  pendingTabSize.set(identity, { target: { tabId, attachmentId }, cols, rows });
  const running = tabResizing.get(identity);
  if (running) return running;
  const chain = (async () => {
    try {
      for (;;) {
        const next = pendingTabSize.get(identity);
        if (!next) return;
        pendingTabSize.delete(identity);
        await invoke<void>("tab_resize", { ...next.target, cols: next.cols, rows: next.rows });
      }
    } finally {
      tabResizing.delete(identity);
    }
  })();
  tabResizing.set(identity, chain);
  return chain;
};

export const tabTakeFocus = (
  tabId: TabId, attachmentId: AttachmentId, cols: number, rows: number,
) => invoke<void>("tab_take_focus", { tabId, attachmentId, cols, rows });
export const tabClose = (tabId: TabId) => invoke<void>("tab_close", { tabId });

export const gitRepoState = (path: string) => invoke<RepoState>("git_repo_state", { path });
export const gitStatus = (path: string) => invoke<FileStatus[]>("git_status", { path });
export const gitBranches = (path: string) => invoke<BranchInfo[]>("git_branches", { path });
export const gitLog = (path: string, limit: number) =>
  invoke<CommitInfo[]>("git_log", { path, limit });
export interface TreeEntry {
  name: string;
  is_dir: boolean;
}

export const gitBranchFiles = (path: string, branch: string, subpath: string) =>
  invoke<TreeEntry[]>("git_branch_files", { path, branch, subpath });
export const gitBranchLog = (path: string, branch: string, limit: number) =>
  invoke<CommitInfo[]>("git_branch_log", { path, branch, limit });
export const gitDiffFile = (path: string, file: string) =>
  invoke<string>("git_diff_file", { path, file });
export const gitCommitDiff = (path: string, commitId: string) =>
  invoke<string>("git_commit_diff", { path, commitId });

export function homeAbbrev(p: string): string {
  return p.replace(/^\/home\/[^/]+/, "~");
}

export { relTime } from "./timefmt";

/** What aiterm found for one agent on this machine — see `agents.rs`. */
export interface AgentDetection {
  id: string;
  display_name: string;
  /** Usable here. For a CLI agent, its binary is on PATH. */
  available: boolean;
  /** First line of `--version`, when it answered. Absent does not imply
   *  unavailable — some tools just don't report one. */
  version: string | null;
  path: string | null;
}

/** Every agent aiterm knows about, present or not. Spawns at most one process
 *  per installed agent, so call it when the answer is wanted (opening
 *  settings) rather than on a timer. */
export const detectAgents = () => invoke<AgentDetection[]>("detect_agents");

/** A configured API provider, as the UI sees it. The key never crosses this
 *  boundary — there is no command that returns it. */
export interface ProviderView {
  id: string;
  name: string;
  base_url: string;
  has_key: boolean;
  /** Last four characters, for telling two keys apart. Empty when there is no
   *  key or it is too short to redact meaningfully. */
  key_hint: string;
  /** Whether an OpenRouter management key is stored, and its last four. The
   *  activity endpoint refuses an inference key, so this is the second
   *  credential a provider can hold — never returned, same as the first. */
  has_management_key: boolean;
  management_key_hint: string;
  /** Model ids picked for the new-session menu — the shortlist, not the
   *  catalog. */
  startup_models: string[];
  /** Account-wide routing rules. Present on every provider; only OpenRouter
   *  does anything with it. */
  policy: Policy;
  /** Per-model routing, keyed by model id. A route outlives its star, so
   *  re-adding a model to the shortlist restores its pin. */
  routes: Record<string, Route>;
}

/** A ceiling in USD per *million* tokens — OpenRouter's unit for `max_price`,
 *  which is not the per-token unit `/models` and `/endpoints` quote. Either
 *  field absent means no ceiling on that side; Rust omits an unset one rather
 *  than sending null. */
export interface MaxPrice {
  prompt?: number;
  completion?: number;
}

/** Which hosts this account will not use. `resolved_ignore` maps a provider
 *  slug to why it is out — compiled when the policy is saved, sent verbatim as
 *  OpenRouter's `ignore`. */
export interface Policy {
  blocked_countries: string[];
  block_unknown_country: boolean;
  blocked_providers: string[];
  max_price: MaxPrice;
  resolved_ignore: Record<string, string>;
  /** Epoch seconds. 0 means never compiled. */
  resolved_at: number;
}

/** What one model prefers. An empty `order` is "no pin". */
export interface Route {
  order: string[];
  allow_fallbacks: boolean;
  max_price: MaxPrice;
}

export const providersList = () => invoke<ProviderView[]>("providers_list");
/** Empty `apiKey` on an existing provider keeps the stored one. */
export const providerSave = (
  id: string | null, name: string, baseUrl: string, apiKey: string,
) => invoke<ProviderView[]>("provider_save", { id, name, baseUrl, apiKey });
export const providerDelete = (id: string) =>
  invoke<ProviderView[]>("provider_delete", { id });
/** Ask the provider for its model list — proves the key and URL actually work. */
export const providerModels = (id: string) =>
  invoke<string[]>("provider_models", { id });

/** What a provider says about one model. Everything past the id is optional —
 *  OpenRouter fills all of it, a bare llama.cpp fills none. Prices are USD per
 *  token as quoted; scale by 1e6 to show $/M. */
export interface ModelCard {
  id: string;
  name: string | null;
  description: string | null;
  context_length: number | null;
  prompt_price: number | null;
  completion_price: number | null;
  modalities: string[];
  /** Epoch seconds the provider listed it, where it says. */
  created: number | null;
}
export const providerModelCards = (id: string) =>
  invoke<ModelCard[]>("provider_model_cards", { id });
/** Replace a provider's startup shortlist — what the new-session menu offers. */
export const providerStartupSet = (id: string, models: string[]) =>
  invoke<ProviderView[]>("provider_startup_set", { id, models });

/** One host's offer of one model. Prices are USD per token as quoted; scale by
 *  1e6 to show $/M. `excluded` is why the stored policy rules the row out. */
export interface EndpointCard {
  provider_name: string;
  /** The routing slug — `novita`. What `order` and `ignore` take. */
  slug: string;
  /** The full tag — `novita/fp8`. Shown, never sent: a pin cannot name a
   *  quantization. */
  tag: string;
  quantization: string | null;
  context_length: number | null;
  prompt_price: number | null;
  completion_price: number | null;
  max_completion_tokens: number | null;
  uptime_30m: number | null;
  excluded: string | null;
}

/** One provider in OpenRouter's directory. `headquarters` is often missing,
 *  which is why `block_unknown_country` is its own decision. */
export interface DirectoryEntry {
  slug: string;
  name: string;
  headquarters: string | null;
  datacenters: string[];
}

/** One day of one model on one host. Account-wide, not app-wide: it includes
 *  traffic aiterm never launched. */
export interface ActivityRow {
  date: string;
  model: string;
  provider_name: string;
  requests: number;
  prompt_tokens: number;
  completion_tokens: number;
  /** USD. */
  usage: number;
}

/** Who hosts this model, annotated against the stored policy. OpenRouter
 *  only — `/endpoints` on a bare llama.cpp is a 404 nobody can act on. */
export const providerModelEndpoints = (id: string, model: string) =>
  invoke<EndpointCard[]>("provider_model_endpoints", { id, model });
/** Every host OpenRouter knows, with the countries the policy filters on. */
export const providerDirectory = (id: string) =>
  invoke<DirectoryEntry[]>("provider_directory", { id });
/** Compiles the policy against the live directory before saving — a directory
 *  that will not load fails the save rather than storing an empty rule. */
export const providerPolicySet = (id: string, policy: Policy) =>
  invoke<ProviderView[]>("provider_policy_set", { id, policy });
/** Replace one model's route. An empty `order` clears the pin. */
export const providerRouteSet = (id: string, model: string, route: Route) =>
  invoke<ProviderView[]>("provider_route_set", { id, model, route });
/** Daily spend per model per host. Needs a management key — an inference key
 *  comes back with OpenRouter's own sentence saying so. */
export const providerActivity = (id: string) =>
  invoke<ActivityRow[]>("provider_activity", { id });
/** Store the management key `/activity` wants, or clear it with `""`. Blank
 *  does *not* keep the stored one here — unlike `providerSave`, this field has
 *  a Forget button instead of a rule to remember. */
export const providerManagementKeySet = (id: string, key: string) =>
  invoke<ProviderView[]>("provider_management_key_set", { id, key });

/** What an engine supports, so the UI gates on a declaration rather than on an
 *  agent's name. Snake_case because it arrives straight off `Caps` in Rust. */
export interface Caps {
  /** ⑂ in the sidebar. */
  fork: boolean;
  /** ✦ re-key. */
  clear: boolean;
  /** ▶ — reopen where it left off. */
  resume: boolean;
  /** The screen poll in `term/screen.ts` and the Tui* dialogs. */
  tui_drive: boolean;
  /** Transcript panels and the `/model` `/effort` `/rewind` pills. */
  panels: boolean;
  /** The Agent panel's Tasks and Artifacts tabs — the engine records a task
   *  list and file edits aiterm can read. Separate from `panels`, which also
   *  turns on pills that speak claude's slash commands. */
  tasks: boolean;
  /** 🗑 — only where the store is one file per session and aiterm's to move.
   *  Off for OpenCode, whose "file" is the one database holding every OpenCode
   *  conversation. Hiding the button is the courtesy; `session_delete` refuses
   *  on the same flag, which is the actual guard. */
  delete: boolean;
  /** The engine has configuration aiterm can read — its Settings button. */
  config: boolean;
  /** Something outside aiterm reports this engine's liveness (claude's own
   *  `claude agents` roster). Only claude has one — so for every other engine a
   *  live tab in this window is the only evidence a session is running. */
  roster_liveness: boolean;
}

/** Every registered engine's capabilities, keyed by agent id.
 *
 *  The same flags `detectAgents` carries, but free: that one probes PATH and
 *  runs `--version` per agent, and these gate a render — which buttons a row
 *  offers, whether the claude-shaped subsystems run against the active tab.
 *  Fetched once on mount. An id absent from the map is an engine that is no
 *  longer registered, and the caller treats it as capable of nothing. */
export const agentCaps = () => invoke<Record<string, Caps>>("agent_caps");

/** Which file set a value. `injected` is aiterm's own --settings file. */
export type ClaudeLayerId = "user" | "project" | "projectLocal" | "injected";

export interface ClaudeLayer {
  id: ClaudeLayerId;
  path: string;
  present: boolean;
  /** Why an existing file could not be used. */
  error: string | null;
  /** Both the raw editor's starting content and the token a save is compared
   *  against to detect collisions (file moved under us). */
  text: string;
}

export interface ClaudeSetting {
  key: string;
  concern: string;
  effective: unknown;
  winner: ClaudeLayerId;
  /** Lowest precedence first, so the last entry is the winner. */
  setIn: { layer: ClaudeLayerId; value: unknown }[];
  /** Claude collects this key from every layer that sets it, so the earlier
   *  entries in `setIn` are also in force — not overridden. */
  merged: boolean;
  /** True when a segment of this key's own path contained a literal `.` —
   *  the dotted `key` string cannot be split back into the path it came
   *  from, so an inline edit must not be routed through it. */
  ambiguous: boolean;
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
  /** One per source that exists but is malformed. */
  errors: string[];
}

export interface ClaudeSkill {
  name: string;
  description: string;
  source: string;
  path: string;
}

export interface ClaudeSkillsView {
  skills: ClaudeSkill[];
  /** Installed plugins switched off in settings, whose skills are therefore
   *  absent from `skills` — a short list needs that number to be readable. */
  disabledPlugins: number;
  errors: string[];
}

export const claudeSettings = (project: string | null) =>
  invoke<ClaudeSettingsView>("claude_settings", { project });
export const claudeInstructions = (project: string | null) =>
  invoke<ClaudeDoc[]>("claude_instructions", { project });
export const claudeMcp = (project: string | null) =>
  invoke<ClaudeMcpView>("claude_mcp", { project });
export const claudeSkills = (project: string | null) =>
  invoke<ClaudeSkillsView>("claude_skills", { project });

/** A hook fires a shell command on its own, unattended, so the row it shows
 *  up in has to carry enough to judge it: which event, which layer put it
 *  there, and the command in full. */
export interface ClaudeHook {
  event: string;
  matcher: string | null;
  command: string;
  layer: string;
  isAiterm: boolean;
}

export interface ClaudeHooksView {
  hooks: ClaudeHook[];
  errors: string[];
}

/** Hooks are additive across every layer that sets them — none of them
 *  "overrides" another — so this exists to show each one with the layer it
 *  actually came from rather than collapsing them into one winner the way
 *  `claudeSettings` does for ordinary keys. */
export const claudeHooks = (project: string | null) =>
  invoke<ClaudeHooksView>("claude_hooks", { project });

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

/** One reading of the web process's cost counters. Cumulative since it
 *  started, so a measurement is always the difference between two calls —
 *  `ok: false` means no web process was found and the numbers mean nothing. */
export type RendererProbe = { cpuMs: number; gpuMs: number | null; ok: boolean };

/** Sampled either side of a known repaint burst, this is what turns the
 *  GPU/DOM trade from a claim into a number for the machine in front of you. */
export const rendererProbe = () => invoke<RendererProbe>("renderer_probe");

/** Put a number on the taskbar icon (0 clears it). Emitted as the Unity
 *  LauncherEntry D-Bus signal, which Plasma honours — Tauri's own
 *  `setBadgeCount` goes through libunity and does nothing outside Unity. */
export const taskbarBadge = (count: number) => invoke<void>("taskbar_badge", { count });

/** Rebuild the tray icon's menu: one row per waiting session. The taskbar icon
 *  cannot do this — Plasma takes its menu from the .desktop file's static
 *  Actions — so the count goes there and the list goes in the tray. */
/** Raise a desktop popup, or update one already on screen by passing its id.
 *  Returns the daemon's id. Unlike the taskbar count and the tray menu, this
 *  needs nothing from the desktop — GNOME and Plasma both implement it. */
export const desktopNotify = (summary: string, body: string, replaces: number) =>
  invoke<number | null>("desktop_notify", { summary, body, replaces });

/** Take a popup down once the session it was about stops waiting. */
export const desktopNotifyClose = (id: number) =>
  invoke<void>("desktop_notify_close", { id });

export const trayAlerts = (alerts: { key: TabId; title: string; message?: string }[]) =>
  invoke<void>("tray_alerts", { alerts });


/** What the user asked for, in the terms the UI actually has. Which engine
 *  answers is `launch.rs`'s business — nothing here names one. Sent in
 *  camelCase, which is what `LaunchRequest` deserializes. */
export type LaunchRequest =
  | { kind: "agent"; agentId: string; model: string | null; effort: string | null; prompt?: string | null; permissionFlags?: string | null }
  | { kind: "apiModel"; providerId: string; modelId: string; prompt?: string | null }
  | { kind: "resume"; sessionId: string }
  | { kind: "restart"; sessionId: string }
  | { kind: "clear"; sessionId: string };

/** Everything a tab needs to open, and nothing about who produced it. */
export interface LaunchPlan {
  command: string;
  /** Provider id whose key tab opening injects into the tab environment.
   *  `null` means no key is needed. */
  env_provider: string | null;
  /** Model id whose routing tab opening compiles into the tab environment,
   *  in the provider catalog's spelling. Set only alongside `env_provider`. */
  env_model: string | null;
  /** Non-null = a real session id panels may key to. `null` = the tab needs a
   *  handle of its own, and nothing should be keyed to it as a session. */
  session_id: string | null;
  agent_id: string;
  caps: Caps;
}

/** Turn intent into a plan. Rejects when nothing here can start it — the
 *  caller keeps whatever fallback it had rather than inventing a command. */
export const resolveLaunch = (request: LaunchRequest) =>
  invoke<LaunchPlan>("resolve_launch", { request });

/** A model a backend can start on, with the effort levels *that model* takes. */
export interface ModelOption {
  id: string;
  display_name: string;
  efforts: string[];
  default_effort: string | null;
}

/** An agent that is actually installed, and what it can be started as. */
export interface AgentChoice {
  id: string;
  display_name: string;
  models: ModelOption[];
  /** Whether aiterm can pre-mint the session id. Where false, the id it
   *  generates is a tab handle only and no panel should be keyed to it. */
  mints_session_id: boolean;
}

export const agentChoices = () => invoke<AgentChoice[]>("agent_choices");

/** One permission/approval preset an engine can start under. `flags` is what
 *  it adds to the command; empty for the engine's own default. */
export interface PermissionMode {
  id: string;
  label: string;
  note: string;
  flags: string[];
}
/** An engine's permission presets and the one currently in force. Only engines
 *  that have a permission switch are returned. */
export interface AgentPermissions {
  agent_id: string;
  display_name: string;
  modes: PermissionMode[];
  /** The id of the mode in force — stored, or the first when nothing is. */
  selected: string;
}
/** Every engine with a permission switch, its modes, and the one in force. */
export const agentPermissions = () => invoke<AgentPermissions[]>("agent_permissions");
/** Store the mode an engine starts in; returns the refreshed list. Rejects an
 *  id the engine does not list. */
export const agentPermissionSet = (agentId: string, mode: string) =>
  invoke<AgentPermissions[]>("agent_permission_set", { agentId, mode });


/** The id of the session an agent just started in `cwd`, once it exists.
 *
 *  For an agent with no `--session-id` the id cannot be minted up front, so
 *  the tab starts with none and finds out by watching: the session that turns
 *  up in the directory we launched in, that was not there before, is ours.
 *  `known` is what the sidebar already listed, so adoption never steals a
 *  conversation that was already open. */
export const adoptAgentSession = (
  agentId: string,
  cwd: string,
  sinceMs: number,
  known: string[],
) => invoke<string | null>("adopt_agent_session", { agentId, cwd, sinceMs, known });

/** The one session an agent created after this terminal explicitly cleared its
 * current conversation. `null` includes ambiguity: the caller must never
 * guess which of two new conversations belongs to its terminal. */
export const clearSuccessorSession = (
  agentId: string,
  previousId: string,
  cwd: string,
  sinceMs: number,
  known: string[],
) => invoke<string | null>("clear_successor_session", {
  agentId, previousId, cwd, sinceMs, known,
});

/** Where aiterm writes its diagnostics. */
export const diagLogPath = () => invoke<string | null>("diag_log_path");

/** The tail of that log, for pasting into a bug report. */
export const diagLogTail = (lines: number) => invoke<string>("diag_log_tail", { lines });

/** Build, desktop and which agents aiterm can see — the first questions of any
 *  "it is behaving oddly" conversation, answered without a scavenger hunt. */
export const diagEnvironment = () => invoke<[string, string][]>("diag_environment");

// --- Librarian: names for sessions ----------------------------------------

export interface LibEntry {
  /** The name; "" when the engine titled the session itself and the
   *  librarian left it alone. */
  name: string;
  seen: number;
  at: number;
  model: string;
}

export interface LibStore {
  sessions: Record<string, LibEntry>;
  spent: number;
}

export interface LibRunReport {
  done: number;
  skipped: number;
  remaining: number;
  cost: number;
  errors: string[];
}

export const EMPTY_LIB: LibStore = { sessions: {}, spent: 0 };

export type LibEngine =
  | { kind: "api"; providerId: string; model: string }
  | { kind: "cli"; agent: string; model: string | null };

export const librarianState = () => invoke<LibStore>("librarian_state");
export const librarianRun = (
  engine: LibEngine,
  sessions: { id: string; lastActive: number }[],
  max: number,
) => invoke<LibRunReport>("librarian_run", { engine, sessions, max });
export const librarianForget = () => invoke<void>("librarian_forget");

export const sessionConversation = (sessionId: string, maxChars: number) =>
  invoke<[string, string][]>("session_conversation", { sessionId, maxChars });
/** Where the session's transcript is on disk — for an agent to read itself. */
export const sessionTranscriptPath = (sessionId: string) =>
  invoke<string>("session_transcript_path", { sessionId });

// --- Files produced by sessions ----------------------------------------

export interface Change {
  path: string;
  name: string;
  kind: "created" | "modified" | "deleted" | string;
  at: number;
  session_id: string | null;
  bytes: number;
}

export const sessionChanges = (sessionId: string) =>
  invoke<Change[]>("session_changes", { sessionId });
export const readFileBase64 = (path: string) =>
  invoke<{ mime: string; data: string }>("read_file_base64", { path });
export const isImagePath = (path: string) => /\.(png|jpe?g|webp|gif|svg)$/i.test(path);
export const isVideoPath = (path: string) => /\.(mp4|webm|m4v)$/i.test(path);

// --- Remote Access -----------------------------------------------------
//
// The phone gateway. Every one of these is a desktop-only decision: the
// gateway deliberately exposes no way for a paired phone to enable itself,
// approve another device, or revoke one. Trust is granted at this keyboard.

export type {
  RemoteStatus,
  PairingInvite,
  PendingPairing,
  TrustedDevice,
} from "./remoteAccess.ts";
import type {
  RemoteStatus,
  PairingInvite,
  PendingPairing,
  TrustedDevice,
} from "./remoteAccess.ts";

/** Whether the gateway is listening, and on what, with its pinned fingerprint. */
export const remoteStatus = () => invoke<RemoteStatus>("remote_status");

/** Addresses the gateway may bind. Loopback is excluded: a phone cannot reach
 *  it, so offering it would only ever be a mistake the user has to debug. */
export const remoteInterfaces = () => invoke<string[]>("remote_interfaces");

export const remoteStart = (address: string, port: number) =>
  invoke<RemoteStatus>("remote_start", { address, port });

/** Closes the listener and every live connection. Does not revoke devices —
 *  turning remote access off is not the same statement as distrusting a phone. */
export const remoteStop = () => invoke<RemoteStatus>("remote_stop");

/** Persist whether the saved relay route should return when AITerm opens. */
export const remoteStartOnLaunchSet = (
  enabled: boolean,
  address: string,
  port: number,
) => invoke<RemoteStatus>("remote_start_on_launch_set", { enabled, address, port });

export const remoteRelayConfigure = (
  connectorUrl: string,
  publicHost: string,
  publicPort: number,
  routeId: string,
  token: string | null,
) => invoke<RemoteStatus>("remote_relay_configure", {
  connectorUrl, publicHost, publicPort, routeId, token,
});

export const remoteRelayServerSet = (server: string) =>
  invoke<RemoteStatus>("remote_relay_server_set", { server });

/** Asks a managed relay to mint one private route for this desktop. The
 * connector secret is returned directly to Rust and never enters JavaScript. */
export const remoteRelayClear = () => invoke<RemoteStatus>("remote_relay_clear");

/** A single-use, five-minute enrollment QR, rendered to SVG by the backend so
 *  the secret never exists as a string in this process. */
export const remoteBeginPairing = () => invoke<PairingInvite>("remote_begin_pairing");

/** Phones that have scanned a QR and are waiting for a decision here. */
export const remotePendingPairings = () =>
  invoke<PendingPairing[]>("remote_pending_pairings");

export const remoteApproveDevice = (requestId: string) =>
  invoke<TrustedDevice>("remote_approve_device", { requestId });

export const remoteDenyDevice = (requestId: string) =>
  invoke<boolean>("remote_deny_device", { requestId });

export const remoteDevices = () => invoke<TrustedDevice[]>("remote_devices");

/** Forgets the device's key and drops its live connections. */
export const remoteRevokeDevice = (deviceId: string) =>
  invoke<boolean>("remote_revoke_device", { deviceId });

// ------------------------------------------------- phone listener (remote_api)
// The phone-protocol listener with its iroh tunnel — separate from the
// remote gateway above, and off unless enabled in Settings → Phone remote.
// Wire names keep their remote_* spelling except `remote_api_status`, which
// the gateway's own `remote_status` forced to move.

export interface PhoneRemoteStatus {
  enabled: boolean;
  running: boolean;
  port: number;
  name: string;
  /** Addresses a phone might reach this machine on, best first. */
  addresses: string[];
  /** What the router said: "off" | "searching" | "mapped" | "no_router" | "refused". */
  upnp: string;
  /** The address the internet sees, when the router told us. */
  public_address: string | null;
  /** SHA-256 of the listener certificate — what a paired phone pins. */
  fingerprint: string | null;
  /** Phones holding the event socket open right now. */
  clients: PhoneRemoteClient[];
  error: string | null;
  /** Whether the iroh tunnel is configured to ride alongside the listener. */
  iroh_enabled: boolean;
  /** The reach-from-anywhere address: this desktop's iroh node id. */
  iroh_node: string | null;
  /** Which roads are on — see docs/remote/remote-roads.md. */
  roads: PhoneRemoteRoads;
  /** What the VPN road sees on this machine right now. */
  vpn: PhoneRemoteVpn;
  /** The relay road: enrolled route, connector state, pending draft. */
  relay: PhoneRemoteRelay;
  /** Custom iroh relay URL; null = iroh's default relays. */
  iroh_relay_url: string | null;
  /** The order phones try the roads, most preferred first — published in
   *  /v1/status; a phone that has not set its own order adopts it. */
  road_order: PhoneRemoteRoad[];
}
export type PhoneRemoteRoad = "lan" | "vpn" | "relay" | "iroh";
export interface PhoneRemoteRoads {
  lan: boolean;
  vpn: boolean;
  relay: boolean;
  iroh: boolean;
}
export interface PhoneRemoteVpn {
  detected: boolean;
  kind: "tailscale" | "wireguard" | "other" | null;
  interface: string | null;
  address: string | null;
  /** Tailscale's MagicDNS name for this machine, when the CLI answers. */
  magic_dns: string | null;
}
export interface PhoneRemoteRelay {
  configured: boolean;
  state: "off" | "connecting" | "connected" | "retrying";
  host: string | null;
  port: number | null;
  /** The relay control server routes are enrolled with. */
  server: string;
  /** An enrollment draft is waiting: any paired phone signs it from
   *  /v1/status (no new pairing), and a QR carries it as `ta`. */
  pending_enrollment: boolean;
  /** The relay server could not be reached when a draft was wanted; no
   *  draft is waiting until the next try. */
  error: string | null;
}
export interface PhoneRemoteClient {
  id: number;
  device: string;
  os: string;
  app: string;
  address: string;
  /** Unix seconds. */
  since: number;
}
export interface PhonePairPayload {
  uri: string;
  /** The QR, as SVG markup from the backend — the token never becomes a string here. */
  svg: string;
}
export const phoneRemoteStatus = () => invoke<PhoneRemoteStatus>("remote_api_status");
export const phoneRemoteSetEnabled = (on: boolean) =>
  invoke<PhoneRemoteStatus>("remote_set_enabled", { on });
/** Forget every paired phone: a new token, so each must scan again. */
export const phoneRemoteRotateToken = () => invoke<PhoneRemoteStatus>("remote_rotate_token");
export const phoneRemoteSetName = (name: string) =>
  invoke<PhoneRemoteStatus>("remote_set_name", { name });
export const phoneRemoteSetPort = (port: number) =>
  invoke<PhoneRemoteStatus>("remote_set_port", { port });
export const phoneRemotePairPayload = () => invoke<PhonePairPayload>("remote_pair_payload");
/** iroh on/off, live — the LAN route is untouched either way. */
export const phoneRemoteSetIroh = (on: boolean) =>
  invoke<PhoneRemoteStatus>("remote_set_iroh", { on });
/** One road on or off, live. Relay on with no route prepares an enrollment
 *  draft that a paired phone signs by itself. */
export const phoneRemoteSetRoad = (road: PhoneRemoteRoad, on: boolean) =>
  invoke<PhoneRemoteStatus>("remote_set_road", { road, on });
/** The order phones try the roads: all four, each once. Phones that have
 *  not set their own order pick it up on their next status read. */
export const phoneRemoteSetRoadOrder = (order: PhoneRemoteRoad[]) =>
  invoke<PhoneRemoteStatus>("remote_set_road_order", { order });
/** A custom iroh relay (null = default). Restarts a running tunnel. */
export const phoneRemoteSetIrohRelayUrl = (url: string | null) =>
  invoke<PhoneRemoteStatus>("remote_set_iroh_relay_url", { url });
/** Forget the phone relay route: release it at the relay, delete the file, stop the connector. */
export const phoneRemoteRelayClear = () => invoke<PhoneRemoteStatus>("remote_phone_relay_clear");
/** One QR that pairs either phone app: the gateway invite with the phone
 *  listener's fields riding behind under their own names. */
export const remoteBeginPairingCombined = () => invoke<PairingInvite>("remote_begin_pairing_combined");
