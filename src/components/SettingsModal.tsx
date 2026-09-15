import { ReactNode, useEffect, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import ModelAccess from "./ModelAccess";
import Icon from "./Icon";
import { X } from "lucide-react";
import AgentIcon from "./AgentIcon";
import RendererLab from "./RendererLab";
import Row from "./SettingsRow";
import RemoteSettings from "./RemoteSettings";
import LibrarianPane from "./LibrarianPane";
import BringInPane from "./BringInPane";
import UpdatesPane from "./UpdatesPane";
import type { LibrarianCtl } from "../librarian";
import ClaudeConfig from "./agent-config/ClaudeConfig";
import {
  ACCENT_SWATCHES, AppSettings, DEFAULT_SETTINGS, PanelScales, THEMES, themeById,
  TERM_WEIGHTS,
} from "../settings";
import { settingsModalSize } from "../settingsModalSize";
import {
  Caps, FontFamily, FontPackage,
  AgentDetection, detectAgents, homeAbbrev,
  AgentPermissions, agentPermissions, agentPermissionSet,
  fontPackages, installFontFiles, installFontPackage, listFonts,
  diagEnvironment, diagLogPath, diagLogTail, openPath,
  traceSet, traceStatus, type UpdateCheck,
} from "../ipc";

interface Props {
  settings: AppSettings;
  onChange: (s: AppSettings) => void;
  onClose: () => void;
  capsOf: (agent: string) => Caps;
  activeProject: string | null;
  /** Open on this tab rather than the first — for a caller that knows what
   *  the user came to do, like the ＋ menu's setup note. */
  initialTab?: SettingsTab;
  /** With `initialTab: "models"`: the provider to open the model browser on
   *  straight away, so the step that was missing is the one on screen. */
  focusProvider?: string | null;
  librarian: LibrarianCtl;
  /** What the launch-time update check found, if it ran. */
  updateInfo?: UpdateCheck | null;
}

const PANEL_LABELS: { key: keyof PanelScales; label: string }[] = [
  { key: "sessions", label: "Sessions" },
  { key: "explorer", label: "Explorer" },
  { key: "git", label: "Repository" },
  { key: "agent", label: "Agent" },
];

export type SettingsTab =
  | "appearance"
  | "fonts"
  | "agents"
  | "models"
  | "librarian"
  | "bringin"
  | "remote"
  | "updates"
  | "diagnostics";
type Tab = SettingsTab;

const NAV: { key: Tab; label: string }[] = [
  { key: "appearance", label: "Appearance" },
  { key: "fonts", label: "Fonts" },
  { key: "agents", label: "Agents" },
  { key: "models", label: "Model access" },
  { key: "librarian", label: "Librarian" },
  { key: "bringin", label: "Bring in" },
  { key: "remote", label: "Remote access" },
  { key: "updates", label: "Updates" },
  { key: "diagnostics", label: "Diagnostics" },
];

function ThemeCard({ id, active, onPick }: { id: string; active: boolean; onPick: () => void }) {
  const t = themeById(id);
  return (
    <button
      className={"theme-card" + (active ? " on" : "")}
      onClick={onPick}
      style={{ background: t.vars.bgPanel, borderColor: active ? t.vars.accent : t.vars.border }}
    >
      <div className="theme-strip">
        {[t.vars.accent, t.term.green, t.term.yellow, t.term.blue, t.term.magenta].map((c) => (
          <span key={c} style={{ background: c }} />
        ))}
      </div>
      <div className="theme-card-name" style={{ color: t.vars.text }}>{t.name}</div>
      <div className="theme-card-sub" style={{ color: t.vars.textFaint }}>Aa ❯ _</div>
    </button>
  );
}

function Group({ title, children }: { title?: string; children: ReactNode }) {
  return (
    <div className="sgroup">
      {title && <div className="sgroup-title">{title}</div>}
      <div className="sgroup-rows">{children}</div>
    </div>
  );
}

/** A switch, for settings that are on or off. */
function Switch({ checked, onChange, label }: {
  checked: boolean;
  onChange: (on: boolean) => void;
  label: string;
}) {
  return (
    <label className="sw" aria-label={label}>
      <input
        type="checkbox"
        checked={checked}
        onChange={(e) => onChange(e.target.checked)}
      />
      <span className="sw-track"><span className="sw-knob" /></span>
    </label>
  );
}

/** Where the dragged window size lives between opens. */
const SIZE_KEY = "aiterm.settingsModalSize";

export default function SettingsModal({
  settings, onChange, onClose, capsOf, activeProject, initialTab, focusProvider, librarian, updateInfo,
}: Props) {
  const [tab, setTab] = useState<Tab>(initialTab ?? "appearance");
  // Resizable via the CSS corner grip; the size carries over to the next
  // open. Written straight to el.style rather than through React state so a
  // re-render can never fight the browser over the size mid-drag. The CSS
  // max-width/max-height still win, so a size saved on a big screen cannot
  // push the window off a small one.
  const modalRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = modalRef.current;
    if (!el) return;
    try {
      const saved = JSON.parse(localStorage.getItem(SIZE_KEY) ?? "null");
      if (saved?.w > 0 && saved?.h > 0) {
        el.style.width = `${saved.w}px`;
        el.style.height = `${saved.h}px`;
      }
    } catch { /* a bad value just means the default size */ }
    const ro = new ResizeObserver(() => {
      localStorage.setItem(SIZE_KEY, JSON.stringify({ w: el.offsetWidth, h: el.offsetHeight }));
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  const beginResize = (event: ReactPointerEvent<HTMLButtonElement>) => {
    const el = modalRef.current;
    if (!el) return;
    event.preventDefault();
    const pointerId = event.pointerId;
    const grip = event.currentTarget;
    const startX = event.clientX;
    const startY = event.clientY;
    const start = { width: el.offsetWidth, height: el.offsetHeight };
    grip.setPointerCapture(pointerId);
    const move = (next: PointerEvent) => {
      if (next.pointerId !== pointerId) return;
      const size = settingsModalSize(
        start,
        next.clientX - startX,
        next.clientY - startY,
        { width: window.innerWidth, height: window.innerHeight },
      );
      el.style.width = `${size.width}px`;
      el.style.height = `${size.height}px`;
    };
    const finish = (next: PointerEvent) => {
      if (next.pointerId !== pointerId) return;
      grip.removeEventListener("pointermove", move);
      grip.removeEventListener("pointerup", finish);
      grip.removeEventListener("pointercancel", finish);
      if (grip.hasPointerCapture(pointerId)) grip.releasePointerCapture(pointerId);
    };
    grip.addEventListener("pointermove", move);
    grip.addEventListener("pointerup", finish);
    grip.addEventListener("pointercancel", finish);
  };
  /** Engine whose configuration panel is open, drilled into from Agents. */
  const [configFor, setConfigFor] = useState<AgentDetection | null>(null);
  // Each section starts at its top — the pane is one shared scroll element,
  // so without this a scroll in one section opens the next mid-page.
  const paneRef = useRef<HTMLDivElement>(null);
  useEffect(() => { paneRef.current?.scrollTo(0, 0); }, [tab]);
  const [fonts, setFonts] = useState<FontFamily[]>([]);
  const [packages, setPackages] = useState<FontPackage[]>([]);
  /** Package currently installing, so its row can say so and not be clicked twice. */
  const [installing, setInstalling] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  // Detection spawns a process per installed agent, so it runs when this modal
  // opens and when explicitly re-checked — never on a timer. `null` is "not
  // asked yet", which is a different thing to show than an empty list.
  const [agents, setAgents] = useState<AgentDetection[] | null>(null);
  // Permission presets per engine, keyed by agent id for the row that renders
  // them. Loaded alongside detection — it is one small file read.
  const [perms, setPerms] = useState<AgentPermissions[]>([]);
  const refreshAgents = () => {
    detectAgents().then(setAgents).catch(() => setAgents([]));
    agentPermissions().then(setPerms).catch(() => setPerms([]));
  };
  useEffect(() => { if (tab === "agents" && agents === null) refreshAgents(); }, [tab, agents]);
  // A change takes effect on the next session the engine opens; the returned
  // list is authoritative, so the dropdown reflects exactly what was stored.
  const setPermission = (agentId: string, mode: string) => {
    setPerms((prev) => prev.map((p) => (p.agent_id === agentId ? { ...p, selected: mode } : p)));
    agentPermissionSet(agentId, mode).then(setPerms).catch(() => refreshAgents());
  };

  // Diagnostics: what aiterm is and what it has been doing. Read on demand —
  // the log tail is a file read and the environment spawns a detect, so
  // neither belongs on a timer.
  const [env, setEnv] = useState<[string, string][] | null>(null);
  const [logTail, setLogTail] = useState<string | null>(null);
  const [logPath, setLogPath] = useState<string | null>(null);
  const refreshDiag = () => {
    diagEnvironment().then(setEnv).catch(() => setEnv([]));
    diagLogTail(300).then(setLogTail).catch(() => setLogTail(""));
    diagLogPath().then(setLogPath).catch(() => setLogPath(null));
  };
  useEffect(() => { if (tab === "diagnostics" && env === null) refreshDiag(); }, [tab, env]);

  // Verbose trace capture — a runtime switch, so it works in the installed
  // release build, which is the one Matt actually runs when something is odd.
  // Deliberately not persisted: a firehose left on across restarts is a disk
  // full of nothing, so each run starts quiet.
  const [tracing, setTracing] = useState(false);
  const [tracePath, setTracePath] = useState<string | null>(null);
  useEffect(() => {
    if (tab === "diagnostics") traceStatus().then(setTracing).catch(() => {});
  }, [tab]);
  const toggleTrace = async (on: boolean) => {
    try {
      const path = await traceSet(on);
      setTracing(on);
      setTracePath(path);
      setNotice(on && path ? `Tracing to ${homeAbbrev(path)}` : null);
    } catch (e) {
      setNotice(String(e));
    }
  };

  const set = (patch: Partial<AppSettings>) => onChange({ ...settings, ...patch });
  const setScale = (key: keyof PanelScales, v: number) =>
    set({ panelScale: { ...settings.panelScale, [key]: v } });

  const refreshFonts = () => {
    listFonts().then(setFonts).catch(() => {});
    fontPackages().then(setPackages).catch(() => {});
  };
  useEffect(refreshFonts, []);

  const monoFonts = fonts.filter((f) => f.mono);

  const installPackage = async (pkg: FontPackage) => {
    setInstalling(pkg.package);
    setNotice(null);
    try {
      await installFontPackage(pkg.package);
      refreshFonts();
      setNotice(`Installed ${pkg.name} — pick it under Terminal font.`);
    } catch (e) {
      setNotice(String(e));
    } finally {
      setInstalling(null);
    }
  };

  const installFiles = async () => {
    setNotice(null);
    try {
      const picked = await open({
        multiple: true,
        title: "Install font files",
        filters: [{ name: "Fonts", extensions: ["ttf", "otf", "ttc", "otc", "pfb"] }],
      });
      const paths = Array.isArray(picked) ? picked : picked ? [picked] : [];
      if (!paths.length) return;
      const n = await installFontFiles(paths);
      refreshFonts();
      setNotice(`Installed ${n} font file${n === 1 ? "" : "s"} to ~/.local/share/fonts.`);
    } catch (e) {
      setNotice(String(e));
    }
  };

  return (
    <div className="modal-overlay" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="modal settings-modal" ref={modalRef}>

        <nav className="sm-rail">
          <div className="sm-rail-title">Settings</div>
          {NAV.map((n) => (
            <button
              key={n.key}
              className={"sm-nav" + (tab === n.key ? " on" : "")}
              onClick={() => setTab(n.key)}
            >{n.label}</button>
          ))}
          <div className="sm-rail-spacer" />
          <button
            className="sm-reset"
            onClick={() => onChange({ ...DEFAULT_SETTINGS })}
          >Reset to defaults</button>
          {/* Which aiterm this is — version and the commit it was built from.
              Two builds can share a version number (a package and a local
              build); the commit tells them apart. Tap for the Updates pane. */}
          <button
            className="sm-about"
            title={"Version " + __APP_VERSION__ + (__APP_COMMIT__ ? ", built from " + __APP_COMMIT__ : "") + " — open Updates"}
            onClick={() => setTab("updates")}
          >
            aiterm {__APP_VERSION__}
            {__APP_COMMIT__ && <span className="sm-about-sha">{__APP_COMMIT__}</span>}
          </button>
        </nav>

        <div className="sm-pane">
          <div className="sm-pane-head">
            <span className="sm-pane-title">{NAV.find((n) => n.key === tab)?.label}</span>
            {tab === "agents" && (
              <button className="set-recheck" onClick={refreshAgents}>Re-check</button>
            )}
            {tab === "diagnostics" && (
              <button className="set-recheck" onClick={refreshDiag}>Refresh</button>
            )}
            <button className="icon-btn" title="Close (Esc)" onClick={onClose}><Icon of={X} /></button>
          </div>
          <div className="sm-pane-body" ref={paneRef}>

            {tab === "appearance" && <>
              <Group title="Theme">
                <div className="theme-grid">
                  {THEMES.map((t) => (
                    <ThemeCard
                      key={t.id}
                      id={t.id}
                      active={settings.themeId === t.id}
                      onPick={() => set({ themeId: t.id })}
                    />
                  ))}
                </div>
                <Row label="Icon size" desc="Toolbar and panel buttons; row actions follow a step smaller">
                  <input
                    type="range" min={12} max={24} step={1}
                    value={settings.iconSize}
                    onChange={(e) => set({ iconSize: +e.target.value })}
                  />
                  <span className="srow-value">{settings.iconSize}px</span>
                </Row>
                <Row label="Times" desc="How “last active” is written in the session list, the home screen, and the agent panel">
                  <div className="seg">
                    {([["relative", "3h ago"], ["absolute", "Clock time"]] as const).map(([v, name]) => (
                      <button
                        key={v}
                        className={"seg-btn" + (settings.timeFormat === v ? " on" : "")}
                        onClick={() => set({ timeFormat: v })}
                      >{name}</button>
                    ))}
                  </div>
                </Row>
                <Row label="Session summary on hover" desc="Rest the pointer on a session row and a card opens beside it with the summary, files and tasks">
                  <Switch checked={settings.sessionHover} onChange={(on) => set({ sessionHover: on })} label="Session summary on hover" />
                </Row>
                <Row label="Accent" desc="Used for selection, focus, and the active tab">
                  <div className="accent-row">
                    <button
                      className={"accent-swatch default" + (settings.accent === null ? " on" : "")}
                      title="Theme default"
                      onClick={() => set({ accent: null })}
                      style={{ background: themeById(settings.themeId).vars.accent }}
                    >A</button>
                    {ACCENT_SWATCHES.map((c) => (
                      <button
                        key={c}
                        className={"accent-swatch" + (settings.accent === c ? " on" : "")}
                        title={c}
                        style={{ background: c }}
                        onClick={() => set({ accent: c })}
                      />
                    ))}
                  </div>
                </Row>
              </Group>

              <Group title="Panel sizes">
                {PANEL_LABELS.map(({ key, label }) => (
                  <Row key={key} label={label}>
                    <input
                      type="range" min={0.7} max={1.5} step={0.05}
                      value={settings.panelScale[key]}
                      onChange={(e) => setScale(key, +e.target.value)}
                    />
                    <span className="srow-value">{Math.round(settings.panelScale[key] * 100)}%</span>
                  </Row>
                ))}
                <div className="sgroup-foot">Ctrl + / Ctrl − zooms everything at once.</div>
              </Group>
            </>}

            {tab === "fonts" && <>
              <Group title="Faces">
                <Row label="Panel font" desc="Lists, sidebars, and dialogs">
                  <select
                    className="set-select"
                    value={settings.uiFont}
                    onChange={(e) => set({ uiFont: e.target.value })}
                  >
                    <option value="">System default</option>
                    {fonts.map((f) => (
                      <option key={f.name} value={f.name}>{f.name}</option>
                    ))}
                  </select>
                </Row>
                <Row label="Terminal font" desc={`${monoFonts.length} monospace families found`}>
                  <select
                    className="set-select mono"
                    value={settings.termFont}
                    onChange={(e) => set({ termFont: e.target.value })}
                  >
                    <option value="">System default</option>
                    {monoFonts.map((f) => (
                      <option key={f.name} value={f.name}>{f.name}</option>
                    ))}
                  </select>
                </Row>
              </Group>

              <Group title="Terminal text">
                <Row label="Size">
                  <input
                    type="range" min={9} max={22} step={1}
                    value={settings.termFontSize}
                    onChange={(e) => set({ termFontSize: +e.target.value })}
                  />
                  <span className="srow-value">{settings.termFontSize}px</span>
                </Row>
                {/* A multiple of the font's own line height, not pixels: the gap
                    that looks right at 13px is cramped at 20. */}
                <Row label="Line spacing">
                  <input
                    type="range" min={1} max={1.6} step={0.05}
                    value={settings.termLineHeight}
                    onChange={(e) => set({ termLineHeight: +e.target.value })}
                  />
                  <span className="srow-value">
                    {settings.termLineHeight === 1
                      ? "none"
                      : `+${Math.round((settings.termLineHeight - 1) * 100)}%`}
                  </span>
                </Row>
                <Row
                  label="Weight"
                  desc="Bold is drawn heavier than this, so emphasis keeps working"
                >
                  <div className="seg">
                    {TERM_WEIGHTS.map((w) => (
                      <button
                        key={w.value}
                        className={"seg-btn" + (settings.termFontWeight === w.value ? " on" : "")}
                        style={{ fontWeight: w.value }}
                        onClick={() => set({ termFontWeight: w.value })}
                      >{w.label}</button>
                    ))}
                  </div>
                </Row>
              </Group>

              {/* Its own block: a sample that has to sit above its buttons, and
                  a measurement below them, is more than one setting's worth of
                  surface. The sample is a real terminal on the selected
                  renderer — a styled <div> used to stand in for it, but HTML
                  text always goes through the desktop's font stack, so it
                  showed subpixel output a GPU terminal can never produce. */}
              <Group title="Rendering">
                <RendererLab
                  settings={settings}
                  onPick={(value) => set({ termRenderer: value })}
                />
              </Group>

              <Group title="Add fonts">
                <div className="font-pkg-list">
                  {packages.map((p) => (
                    <div key={p.package} className="font-pkg-row">
                      <div className="font-pkg-text">
                        <span
                          className="font-pkg-name"
                          style={p.installed ? { fontFamily: `"${p.name}", monospace` } : undefined}
                        >{p.name}</span>
                        <span className="font-pkg-note">{p.note}</span>
                      </div>
                      {p.installed ? (
                        <span className="font-pkg-done">installed</span>
                      ) : (
                        <button
                          className="font-pkg-install"
                          disabled={installing !== null}
                          onClick={() => installPackage(p)}
                        >{installing === p.package ? "installing…" : "Install"}</button>
                      )}
                    </div>
                  ))}
                </div>
                <Row
                  label="Install from file"
                  desc="Nerd Fonts, anything downloaded — copied to ~/.local/share/fonts"
                >
                  <button className="set-file-install" onClick={installFiles}>
                    Choose files…
                  </button>
                </Row>
                <div className="sgroup-foot">
                  Packaged fonts come from the Fedora repositories and may ask for
                  your password.
                </div>
                {notice && <div className="set-notice">{notice}</div>}
              </Group>
            </>}

            {tab === "models" && <ModelAccess focusProvider={focusProvider} />}

            {/* Everything you would otherwise have to be talked through finding:
                which build this is, what it can see, and what it has been doing.
                The log survives the process, so a crash leaves something to read
                instead of nothing. */}
            {tab === "diagnostics" && <>
              <Group>
                <Row
                  label="Verbose trace"
                  desc={
                    tracing && tracePath
                      ? `Capturing to ${homeAbbrev(tracePath)} — starts fresh each time it's turned on`
                      : "Log every call from the UI, with arguments and timings, to trace.log. Costs nothing while off."
                  }
                >
                  <Switch checked={tracing} onChange={toggleTrace} label="Verbose trace" />
                </Row>
              </Group>
              <Group title="This build">
                {env === null ? (
                  <div className="sgroup-foot">Looking…</div>
                ) : (
                  <div className="diag-table">
                    {env.map(([k, v]) => (
                      <div key={k} className="diag-row">
                        <span className="diag-key">{k}</span>
                        <span className="diag-val">{homeAbbrev(v)}</span>
                      </div>
                    ))}
                  </div>
                )}
              </Group>
              <Group title="Recent activity">
                <div className="diag-acts">
                  <button
                    className="set-recheck"
                    onClick={() => {
                      navigator.clipboard?.writeText(
                        (env ?? []).map(([k, v]) => `${k}: ${v}`).join("\n") +
                          "\n\n" + (logTail ?? ""),
                      );
                      setNotice("Diagnostics copied — paste them into the chat.");
                    }}
                  >Copy all</button>
                  {logPath && (
                    <button className="set-recheck" onClick={() => openPath(logPath)}>
                      Open folder
                    </button>
                  )}
                </div>
                <pre className="diag-log">{logTail || "Nothing logged yet this run."}</pre>
                {logPath && (
                  <div className="sgroup-foot">
                    {homeAbbrev(logPath)} — previous run kept as .log.1
                  </div>
                )}
                {notice && <div className="set-notice">{notice}</div>}
              </Group>
            </>}

            {tab === "remote" && <RemoteSettings />}
            {tab === "updates" && (
              <UpdatesPane
                cfg={settings.updates}
                onChange={(next) => onChange({ ...settings, updates: next })}
                initial={updateInfo}
              />
            )}
            {tab === "bringin" && (
              <BringInPane prompts={settings.bringIn} onChange={(next) => onChange({ ...settings, bringIn: next })} />
            )}
            {tab === "librarian" && (
              <LibrarianPane
                cfg={settings.librarian}
                onChange={(next) => onChange({ ...settings, librarian: next })}
                lib={librarian}
                onOpenModelAccess={() => setTab("models")}
              />
            )}
            {tab === "agents" && configFor && (
              <ClaudeConfig agent={configFor} project={activeProject} onBack={() => setConfigFor(null)} />
            )}
            {tab === "agents" && !configFor && <>
              <Group>
                {/* Agents aiterm does not find are listed too. "Codex — not
                    installed" is the useful answer; an absence would leave you
                    wondering whether aiterm supports it at all. */}
                {agents === null ? (
                  <div className="sgroup-foot">Looking…</div>
                ) : (
                  <div className="agent-list">
                    {agents.map((a) => (
                      <div key={a.id} className="agent-row">
                        <span className={"agent-dot" + (a.available ? " on" : "")} />
                        <div className="agent-text">
                          <div className="agent-name">
                            <AgentIcon agent={a.id} size={14} />
                            {a.display_name}
                            <span className="agent-state">
                              {a.available ? (a.version ?? "installed") : "not installed"}
                            </span>
                          </div>
                          {/* Which copy was found — worth showing when several
                              are installed and the wrong one is on PATH. */}
                          {a.path && <div className="agent-path">{homeAbbrev(a.path)}</div>}
                          {a.id === "grok" && a.available && (
                            <div className="agent-path">
                              Sessions are listed, searchable and resumable. Grok takes
                              the session id aiterm mints, so a new tab has its row at once.
                            </div>
                          )}
                          {a.id === "codex" && a.available && (
                            <div className="agent-path">
                              Sessions are listed and searchable. Codex names its own
                              sessions, so aiterm adopts the id once it starts.
                            </div>
                          )}
                          {a.id === "antigravity" && a.available && (
                            <div className="agent-path">
                              Sessions are listed, searchable and resumable; a new one is
                              adopted from agy's store a moment after launch, and usage
                              comes from its own /usage.
                            </div>
                          )}
                          {/* How much this engine asks before acting, applied
                              to every session aiterm starts or resumes for it.
                              Only shown for an installed engine that has a
                              permission switch — the API chat harness has none. */}
                          {a.available && (() => {
                            const p = perms.find((x) => x.agent_id === a.id);
                            if (!p) return null;
                            const cur = p.modes.find((m) => m.id === p.selected);
                            return (
                              <div className="agent-perm">
                                <label className="agent-perm-label">
                                  Permissions
                                  <select
                                    className="ns-select"
                                    value={p.selected}
                                    onChange={(e) => setPermission(a.id, e.target.value)}
                                  >
                                    {p.modes.map((m) => (
                                      <option key={m.id} value={m.id}>{m.label}</option>
                                    ))}
                                  </select>
                                </label>
                                {cur && <div className="agent-perm-note">{cur.note}</div>}
                              </div>
                            );
                          })()}
                        </div>
                        {capsOf(a.id).config && a.available && (
                          <button className="agent-config-btn" onClick={() => setConfigFor(a)}>
                            Settings
                          </button>
                        )}
                      </div>
                    ))}
                    {agents.length === 0 && (
                      <div className="sgroup-foot">Nothing reported.</div>
                    )}
                  </div>
                )}
                <div className="sgroup-foot">
                  Read from PATH when this page opens, not polled — install
                  something and press Re-check.
                </div>
              </Group>
            </>}

          </div>
        </div>
        <button
          className="sm-resize-grip"
          aria-label="Resize settings"
          title="Drag to resize"
          onPointerDown={beginResize}
        />
      </div>
    </div>
  );
}
