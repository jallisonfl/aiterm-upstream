/** App-wide appearance settings: theme, fonts, per-panel sizing. */
import type { TimeFormat } from "./timefmt";
import { setDisplayZone } from "./timefmt";

export interface PanelScales {
  sessions: number;
  explorer: number;
  git: number;
  agent: number;
}

/** The librarian: a small model that names sessions whose engine did not
 *  name them itself. Off until it is switched on — it spends a little. */
export interface LibrarianSettings {
  /** The master switch. Off: no runs, and the rest of the pane is not
   *  shown. Names already written stay on disk and in the list. */
  enabled: boolean;
  /** How the model is reached: an installed CLI in its print mode — which
   *  runs on the plan already paid for — or an API provider. */
  engine: "claude" | "codex" | "grok" | "antigravity" | "api";
  /** Provider id from Model access; only for `engine: "api"`. */
  providerId: string;
  /** Model id in the engine's spelling; "" means the CLI's default. */
  model: string;
  /** Name new sessions on its own, a little after they go quiet. */
  auto: boolean;
}

/** What the two agents are told when one is brought in. Placeholders in
 *  braces: {a} the first agent, {b} the second, {path} the other agent's
 *  transcript on disk, {focus} what the user asked for, {text} the message
 *  being relayed. "" means the one shipped. */
export interface BringInPrompts {
  /** The second agent's launch prompt. */
  opening: string;
  /** To the first agent, when a reply is expected. */
  toFirst: string;
  /** To the first agent, the last message: carry on. */
  toFirstLast: string;
  /** To the second agent: the first agent's reply. */
  toSecond: string;
  /** Appended to the last message when the user pre-approved the outcome. */
  approved: string;
}

export const BRING_IN_DEFAULTS: BringInPrompts = {
  opening: `You've been brought into a live session as a second agent, alongside {a}.
Their conversation with the user is in the transcript at {path}. Read it.
The user asks: {focus}
Then write your response to {a} directly. The user is reading along.`,
  toFirst: `{b} was brought in by the user and wrote this to you (their transcript: {path}):
---
{text}
---
Reply to them directly.`,
  toFirstLast: `{b} wrote back:
---
{text}
---
That's the end of the exchange. Take it into account and carry on with the user.`,
  toSecond: `{a} replied:
---
{text}
---
Respond to them.`,
  approved: `The user has already approved the outcome of this exchange. Proceed with it now; do not wait for further sign-off.`,
};

export interface AppSettings {
  themeId: string;
  /** Rest the pointer on a session row and a card opens beside it with the
   *  session's summary, files and tasks. Off, the list is just a list. */
  sessionHover: boolean;
  /** Accent override; null = theme default. */
  accent: string | null;
  /** UI font family for panels; "" = system default stack. */
  uiFont: string;
  /** Terminal font family; "" = default mono stack. */
  termFont: string;
  /** Terminal base font size in px (global zoom multiplies this). */
  termFontSize: number;
  /** Terminal line spacing as a multiple of the font's natural line height.
   *  1 is the font's own metrics; 1.1 adds a tenth of a line between rows.
   *  A multiplier rather than pixels so it holds at any font size — the gap
   *  that looks right at 13px is cramped at 20. */
  termLineHeight: number;
  /** Weight for ordinary terminal text. Bold stays heavier than whatever this
   *  is, so emphasis keeps working — see `termFontWeightBold`. */
  termFontWeight: number;
  /** Which renderer draws the terminal.
   *
   *  `"gpu"` rasterises its own glyph atlas, so text is always grayscale
   *  antialiased no matter what the desktop is set to — it never sees the
   *  system's subpixel rendering or hinting. `"dom"` is drawn by the browser
   *  engine and does, which some displays render noticeably sharper.
   *
   *  Not a free choice: the GPU renderer was adopted to fix stale cells the DOM
   *  one left behind. This is here so the trade can be looked at rather than
   *  argued about, and it stays on GPU unless someone changes it. */
  termRenderer: "gpu" | "dom";
  panelScale: PanelScales;
  /** Pixel size of the toolbar and panel icons (Lucide set). Row actions and
   *  inline marks scale with it, a step under. */
  iconSize: number;
  /** How "last active" and the like are written: "3h ago", or the clock
   *  time. See `timefmt.ts`. */
  timeFormat: TimeFormat;
  /** IANA zone id stamps are written in; "" = this machine's own. */
  timeZone: string;
  librarian: LibrarianSettings;
  bringIn: BringInPrompts;
  updates: UpdateSettings;
}

/** How the app looks for newer releases of itself (Settings → Updates). */
export interface UpdateSettings {
  /** Ask GitHub once a day at launch and show a topbar pill when newer exists.
   *  The check is one small unauthenticated request; nothing is downloaded
   *  until asked. */
  checkOnLaunch: boolean;
  /** Also offer alpha/beta tags, not just the stable release. */
  prerelease: boolean;
}

export const DEFAULT_SETTINGS: AppSettings = {
  themeId: "warp-dark",
  sessionHover: true,
  accent: null,
  uiFont: "",
  termFont: "",
  termFontSize: 13,
  // A touch of air by default. Monospace faces are drawn to sit tightly and a
  // terminal packs every line against the next, which is what makes a wall of
  // output hard to track a line across.
  termLineHeight: 1.1,
  termFontWeight: 400,
  termRenderer: "gpu",
  panelScale: { sessions: 1, explorer: 1, git: 1, agent: 1 },
  iconSize: 16,
  timeFormat: "relative",
  timeZone: "",
  bringIn: { opening: "", toFirst: "", toFirstLast: "", toSecond: "", approved: "" },
  updates: { checkOnLaunch: true, prerelease: false },
  librarian: {
    enabled: false,
    engine: "claude",
    providerId: "",
    model: "haiku",
    auto: true,
  },
};

/** Weights offered for terminal text, and what to call them.
 *
 *  Stops at 600. Bold is drawn at one step heavier than the chosen weight, and
 *  most monospace families stop at Bold — pick 700 for body text and emphasis
 *  has nowhere left to go, so a TUI that uses bold to mean something loses the
 *  distinction entirely.
 */
export const TERM_WEIGHTS: { value: number; label: string }[] = [
  { value: 300, label: "Light" },
  { value: 400, label: "Regular" },
  { value: 500, label: "Medium" },
  { value: 600, label: "SemiBold" },
];

/** The weight bold text is drawn at, given the weight body text uses. */
export function boldWeightFor(weight: number): number {
  return Math.min(900, weight + 200);
}

const SETTINGS_KEY = "aiterm.settings";

export function loadSettings(): AppSettings {
  try {
    const raw = localStorage.getItem(SETTINGS_KEY);
    if (!raw) return DEFAULT_SETTINGS;
    const parsed = JSON.parse(raw);
    return {
      ...DEFAULT_SETTINGS,
      ...parsed,
      panelScale: { ...DEFAULT_SETTINGS.panelScale, ...(parsed.panelScale ?? {}) },
      librarian: { ...DEFAULT_SETTINGS.librarian, ...(parsed.librarian ?? {}) },
      bringIn: { ...DEFAULT_SETTINGS.bringIn, ...(parsed.bringIn ?? {}) },
      updates: { ...DEFAULT_SETTINGS.updates, ...(parsed.updates ?? {}) },
    };
  } catch {
    return DEFAULT_SETTINGS;
  }
}

export function saveSettings(s: AppSettings) {
  localStorage.setItem(SETTINGS_KEY, JSON.stringify(s));
}

/* ---------- themes ---------- */

export interface Theme {
  id: string;
  name: string;
  vars: {
    bg: string; bgPanel: string; bgRaised: string; bgHover: string; bgActive: string;
    border: string; text: string; textDim: string; textFaint: string;
    accent: string; green: string; red: string; yellow: string; blue: string; cyan: string;
  };
  term: {
    red: string; green: string; yellow: string; blue: string;
    magenta: string; cyan: string; selection: string;
  };
}

export const THEMES: Theme[] = [
  {
    id: "warp-dark",
    name: "Warp Dark",
    vars: {
      bg: "#121317", bgPanel: "#16171d", bgRaised: "#1c1e26", bgHover: "#22242e",
      bgActive: "#2a2d39", border: "#262833", text: "#d8dae5", textDim: "#8b8fa3",
      textFaint: "#5c6070", accent: "#da7756", green: "#98c379", red: "#e06c75",
      yellow: "#e5c07b", blue: "#61afef", cyan: "#56b6c2",
    },
    term: {
      red: "#e06c75", green: "#98c379", yellow: "#e5c07b", blue: "#61afef",
      magenta: "#c678dd", cyan: "#56b6c2", selection: "#33364180",
    },
  },
  {
    id: "one-dark",
    name: "One Dark",
    vars: {
      bg: "#21252b", bgPanel: "#282c34", bgRaised: "#2c313a", bgHover: "#323842",
      bgActive: "#3a404b", border: "#3a3f4b", text: "#abb2bf", textDim: "#7f848e",
      textFaint: "#5c6370", accent: "#61afef", green: "#98c379", red: "#e06c75",
      yellow: "#e5c07b", blue: "#61afef", cyan: "#56b6c2",
    },
    term: {
      red: "#e06c75", green: "#98c379", yellow: "#e5c07b", blue: "#61afef",
      magenta: "#c678dd", cyan: "#56b6c2", selection: "#3e445180",
    },
  },
  {
    id: "dracula",
    name: "Dracula",
    vars: {
      bg: "#1e1f29", bgPanel: "#282a36", bgRaised: "#343746", bgHover: "#3c3f51",
      bgActive: "#44475a", border: "#3c3f51", text: "#f8f8f2", textDim: "#a3aed3",
      textFaint: "#6272a4", accent: "#bd93f9", green: "#50fa7b", red: "#ff5555",
      yellow: "#f1fa8c", blue: "#8be9fd", cyan: "#8be9fd",
    },
    term: {
      red: "#ff5555", green: "#50fa7b", yellow: "#f1fa8c", blue: "#6272a4",
      magenta: "#ff79c6", cyan: "#8be9fd", selection: "#44475a99",
    },
  },
  {
    id: "gruvbox",
    name: "Gruvbox",
    vars: {
      bg: "#1d2021", bgPanel: "#282828", bgRaised: "#32302f", bgHover: "#3c3836",
      bgActive: "#504945", border: "#3c3836", text: "#ebdbb2", textDim: "#a89984",
      textFaint: "#7c6f64", accent: "#fe8019", green: "#b8bb26", red: "#fb4934",
      yellow: "#fabd2f", blue: "#83a598", cyan: "#8ec07c",
    },
    term: {
      red: "#fb4934", green: "#b8bb26", yellow: "#fabd2f", blue: "#83a598",
      magenta: "#d3869b", cyan: "#8ec07c", selection: "#50494580",
    },
  },
  {
    id: "nord",
    name: "Nord",
    vars: {
      bg: "#232831", bgPanel: "#2e3440", bgRaised: "#3b4252", bgHover: "#434c5e",
      bgActive: "#4c566a", border: "#3b4252", text: "#d8dee9", textDim: "#9aa4b5",
      textFaint: "#616e88", accent: "#88c0d0", green: "#a3be8c", red: "#bf616a",
      yellow: "#ebcb8b", blue: "#81a1c1", cyan: "#88c0d0",
    },
    term: {
      red: "#bf616a", green: "#a3be8c", yellow: "#ebcb8b", blue: "#81a1c1",
      magenta: "#b48ead", cyan: "#88c0d0", selection: "#4c566a80",
    },
  },
  {
    id: "tokyo-night",
    name: "Tokyo Night",
    vars: {
      bg: "#16161e", bgPanel: "#1a1b26", bgRaised: "#24283b", bgHover: "#292e42",
      bgActive: "#343a55", border: "#2a2f45", text: "#c0caf5", textDim: "#787c99",
      textFaint: "#565a6e", accent: "#7aa2f7", green: "#9ece6a", red: "#f7768e",
      yellow: "#e0af68", blue: "#7aa2f7", cyan: "#7dcfff",
    },
    term: {
      red: "#f7768e", green: "#9ece6a", yellow: "#e0af68", blue: "#7aa2f7",
      magenta: "#bb9af7", cyan: "#7dcfff", selection: "#343a5580",
    },
  },
  {
    id: "carbon",
    name: "Carbon",
    vars: {
      bg: "#000000", bgPanel: "#0f0f0f", bgRaised: "#181818", bgHover: "#1f1f1f",
      bgActive: "#2a2a2a", border: "#222222", text: "#e4e4e4", textDim: "#9a9a9a",
      textFaint: "#666666", accent: "#e8e8e8", green: "#0dbc79", red: "#f14c4c",
      yellow: "#cca700", blue: "#3b8eea", cyan: "#29b8db",
    },
    term: {
      red: "#cd3131", green: "#0dbc79", yellow: "#e5e510", blue: "#2472c8",
      magenta: "#bc3fbc", cyan: "#11a8cd", selection: "#ffffff22",
    },
  },
  {
    id: "solarized",
    name: "Solarized",
    vars: {
      bg: "#001a21", bgPanel: "#002b36", bgRaised: "#073642", bgHover: "#0a4252",
      bgActive: "#10505f", border: "#0a3d4a", text: "#adbcbc", textDim: "#839496",
      textFaint: "#586e75", accent: "#268bd2", green: "#859900", red: "#dc322f",
      yellow: "#b58900", blue: "#268bd2", cyan: "#2aa198",
    },
    term: {
      red: "#dc322f", green: "#859900", yellow: "#b58900", blue: "#268bd2",
      magenta: "#d33682", cyan: "#2aa198", selection: "#07364299",
    },
  },
];

export const ACCENT_SWATCHES = [
  "#e8e8e8", "#9aa4b5", "#da7756", "#fe8019", "#e5c07b", "#cca700", "#98c379",
  "#50fa7b", "#2aa198", "#56b6c2", "#61afef", "#7aa2f7", "#bd93f9", "#c678dd",
  "#ff79c6", "#e06c75",
];

export function themeById(id: string): Theme {
  return THEMES.find((t) => t.id === id) ?? THEMES[0];
}

const UI_FALLBACK = `-apple-system, "Segoe UI", "Inter", system-ui, sans-serif`;
const MONO_FALLBACK = `"JetBrainsMono Nerd Font", "JetBrains Mono", "Fira Code", monospace`;

/** Push the current theme + fonts into CSS custom properties on :root. */
export function applySettings(s: AppSettings) {
  setDisplayZone(s.timeZone);
  const t = themeById(s.themeId);
  const r = document.documentElement.style;
  r.setProperty("--bg", t.vars.bg);
  r.setProperty("--bg-panel", t.vars.bgPanel);
  r.setProperty("--bg-raised", t.vars.bgRaised);
  r.setProperty("--bg-hover", t.vars.bgHover);
  r.setProperty("--bg-active", t.vars.bgActive);
  r.setProperty("--border", t.vars.border);
  r.setProperty("--text", t.vars.text);
  r.setProperty("--text-dim", t.vars.textDim);
  r.setProperty("--text-faint", t.vars.textFaint);
  r.setProperty("--accent", s.accent ?? t.vars.accent);
  r.setProperty("--green", t.vars.green);
  r.setProperty("--red", t.vars.red);
  r.setProperty("--yellow", t.vars.yellow);
  r.setProperty("--blue", t.vars.blue);
  r.setProperty("--cyan", t.vars.cyan);
  r.setProperty("--magenta", t.term.magenta);
  r.setProperty("--font-ui", s.uiFont ? `"${s.uiFont}", ${UI_FALLBACK}` : UI_FALLBACK);
  r.setProperty("--font-mono", s.termFont ? `"${s.termFont}", ${MONO_FALLBACK}` : MONO_FALLBACK);
  // Three plain pixel values rather than one and a calc(): WebKitGTK does not
  // apply calc() as an SVG root's width, and an unapplied width leaves the
  // element at the 24px Lucide writes on it — every small icon drawn larger
  // than the large ones.
  r.setProperty("--icon-size", `${s.iconSize}px`);
  r.setProperty("--icon-size-sm", `${Math.round(s.iconSize * 0.8)}px`);
  r.setProperty("--icon-size-lg", `${Math.round(s.iconSize * 1.2)}px`);
}

export function termFontFamily(s: AppSettings): string {
  return s.termFont ? `"${s.termFont}", ${MONO_FALLBACK}` : MONO_FALLBACK;
}

/**
 * Mix a hex colour towards white by `amount` (0–1).
 *
 * How the bright half of the ANSI palette is produced. Hand-authoring it would
 * mean eight more colours for each of eight themes, and every one of them a
 * chance to pick a shade that fights the theme it belongs to; derived, a new
 * theme gets a matching bright set for free.
 */
function lighten(hex: string, amount: number): string {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
  if (!m) return hex;
  const n = parseInt(m[1], 16);
  const mix = (c: number) => Math.round(c + (255 - c) * amount);
  const r = mix((n >> 16) & 255), g = mix((n >> 8) & 255), b = mix(n & 255);
  return "#" + [r, g, b].map((c) => c.toString(16).padStart(2, "0")).join("");
}

/** xterm theme derived from the app theme. */
export function termTheme(s: AppSettings) {
  const t = themeById(s.themeId);
  // The bright half of the palette used to be left undefined, so xterm fell
  // back to its own built-in colours — a generic set matching none of these
  // themes. That is not a corner case: xterm draws bold text in the bright
  // variant by default, so most of what a TUI emphasises was being painted in
  // colours from outside the theme entirely, which reads as the terminal
  // looking dull and slightly wrong next to the rest of the app.
  const BRIGHT = 0.25;
  return {
    background: t.vars.bg,
    foreground: t.vars.text,
    cursor: s.accent ?? t.vars.accent,
    selectionBackground: t.term.selection,
    black: t.vars.bgRaised,
    red: t.term.red,
    green: t.term.green,
    yellow: t.term.yellow,
    blue: t.term.blue,
    magenta: t.term.magenta,
    cyan: t.term.cyan,
    white: t.vars.text,
    brightBlack: t.vars.textFaint,
    brightRed: lighten(t.term.red, BRIGHT),
    brightGreen: lighten(t.term.green, BRIGHT),
    brightYellow: lighten(t.term.yellow, BRIGHT),
    brightBlue: lighten(t.term.blue, BRIGHT),
    brightMagenta: lighten(t.term.magenta, BRIGHT),
    brightCyan: lighten(t.term.cyan, BRIGHT),
    brightWhite: lighten(t.vars.text, BRIGHT),
  };
}

/* ---------- installed-font detection ---------- */

/**
 * Font discovery lives in the backend now — see `src-tauri/src/fonts.rs`.
 *
 * This used to be a canvas width-probe against a hardcoded candidate list:
 * render a sample string in `"Name", monospace` and in plain `monospace`, and
 * call the font installed when the widths differed. It worked, but it could
 * only ever find fonts someone had thought to list, so the picker quietly
 * offered a fraction of what was actually on the machine. fontconfig knows
 * the real answer — `listFonts()` in ipc.ts asks it.
 */
