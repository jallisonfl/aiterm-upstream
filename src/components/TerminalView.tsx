import { useCallback, useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { parseOsc9, TermProgress } from "../osc9";
import { Channel } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import {
  AttachmentId, TabId, openPath, tabAttachDesktop, tabDetach, tabList, tabResize, tabTakeFocus, tabWrite,
} from "../ipc";
import { boldWeightFor } from "../settings";
import { createTabExitCatchUp } from "../tabModel";
import { TerminalInputLine } from "../terminalInput";
import { projectTerminalGrid } from "../terminalSizing";
import { terminalLinkHandler } from "../terminalLinks";
import "@xterm/xterm/css/xterm.css";

/** Attach the GPU WebGL renderer. It owns its own surface and clears+repaints
 *  every cell each frame — which is what kills the stale-cell ghosting the
 *  built-in DOM renderer left behind. Guarded so that if WebGL can't init (no
 *  GPU context) the terminal is never worse than the DOM default. On GPU
 *  context loss, dispose so xterm reverts to the DOM renderer rather than
 *  freezing on a dead canvas.
 *
 *  Returns the addon so a settings change can take it away again: disposing a
 *  WebglAddon is how xterm is told to go back to drawing in the DOM, and there
 *  is no other switch. `null` means the terminal is already on the DOM
 *  renderer, whether by choice or because WebGL would not start. */
function attachRenderer(term: Terminal): WebglAddon | null {
  try {
    const webgl = new WebglAddon();
    webgl.onContextLoss(() => webgl.dispose());
    term.loadAddon(webgl);
    return webgl;
  } catch {
    /* WebGL unavailable — xterm's built-in DOM renderer remains */
    return null;
  }
}

export type { TermProgress };

export interface TermTab {
  key: TabId;
  title: string;
  cwd: string | null;
  command: string | null;
  /** Claude session id when this tab was opened via resume. */
  sessionId?: string;
  /** The session id this tab was deliberately opened to reopen — set by resume
   *  and by the ended-tab restart, unset for a fresh start.
   *
   *  What the migration watcher checks before re-keying a tab away from its
   *  pinned id: "this tab asked for that conversation" is a fact the tab knows,
   *  and it used to be inferred by sniffing the command text for `--resume
   *  <id>`. That inference broke the moment the command grew shell quoting. */
  resumedId?: string;
  /** Provider whose key the backend injects into this tab's environment. */
  envProvider?: string;
  /** Model whose routing the backend compiles into this tab's environment.
   *  The id only — the routing itself is read from the provider store in
   *  Rust and never passes through here. */
  envModel?: string;
  /** The engine running in this tab, as `LaunchPlan.agent_id` named it.
   *
   *  Not decoration: it is what the app looks a tab's capabilities up by, and
   *  so what decides whether the screen poll, the `/model` `/effort` `/rewind`
   *  pills and the tasks panel run against it at all. Undefined for a tab with
   *  no engine — a plain shell — which is the same answer as an engine that is
   *  no longer registered: capable of nothing. */
  agentId?: string;
  /** The tab this one was brought into — a second agent's tab nests under
   *  its master in the strip's second row rather than standing in the first.
   *  Renderer-local: the registry does not know parentage. */
  parentKey?: TabId;
  /** Dedupe key linking this terminal to a sidebar item: a session id for
   *  resumes, "shell:<path>" for project shells. One terminal per slot. */
  slotId: string;
  /** A session aiterm started that has not written its transcript yet.
   *
   *  The sidebar is the tab list, so a tab with no row in it is a tab you
   *  cannot get back to — and claude writes nothing to disk until the first
   *  prompt. This marks the window in between, so the panel can list the tab
   *  on the strength of aiterm having started it. It stays set once the
   *  transcript lands; what retires the placeholder is the real session
   *  appearing under the same id, not this flag being cleared. */
  fresh?: boolean;
  /** Rust's canonical grid and safe focus-owner projection. */
  size?: { cols: number; rows: number };
  focus?: "desktop" | "remote" | "unowned";
  /** Set while this tab is still waiting to learn its session id.
   *
   *  Only agents that have no `--session-id` — Codex — get one of these. They
   *  cannot be told what to call themselves, so the tab opens with no session
   *  id at all and watches for the transcript that appears in the directory it
   *  launched in. Cleared the moment that id is adopted, which is also what
   *  retires the placeholder row and stops the conversation owning two. */
  adopt?: {
    agentId: string;
    /** Unix millis at launch; nothing older than this can be ours. */
    since: number;
    /** Session ids the sidebar already listed, so adoption cannot take over a
     *  conversation that was open before this tab existed. */
    known: string[];
  };
}

/** Control surface a mounted terminal registers with the app. */
export interface TermHandle {
  /** Send raw bytes to the PTY. */
  write: (data: string) => void;
  /** Force a clean TUI repaint (SIGWINCH jiggle) — Ctrl+Shift+L. */
  redraw: () => void;
  /** Paste text: bracketed when the running app enabled paste mode, so TUIs
   *  (claude turns image paths into [Image #N]) can tell it from typing. */
  paste: (text: string) => void;
  /** Send composed input from the bottom input box (adds Enter, wraps
   *  multiline text in bracketed paste when the running app supports it). */
  sendComposed: (text: string) => void;
  /** Put the keyboard back in the terminal — used when a chrome element that
   *  took focus (a composer pill panel) is dismissed. */
  focus: () => void;
  /** Whether unsent text appears to be sitting in the running program's input
   *  line. Typing a slash command into a non-empty prompt concatenates onto
   *  what is there and submits the lot, so the pills ask before doing it. */
  pendingInput: () => boolean;
  /** The visible screen as text, one string per row. xterm has already parsed
   *  the escape codes, so this is rendered characters rather than a guess at a
   *  byte stream. Used to recognise TUI screens worth replacing with a real
   *  dialog. */
  screen: () => string[];
}

interface Props {
  tab: TermTab;
  active: boolean;
  /** `code` is the child's exit status: 0 means the user left, anything else
   *  (or null, if it couldn't be reaped) means it died on its own. `signal` is
   *  set instead when it was killed — portable-pty reports code 1 for every
   *  signal, so the code alone cannot tell a SIGKILL from `exit 1`. */
  onExit: (key: TabId, code: number | null, signal: string | null) => void;
  onRegister: (key: TabId, handle: TermHandle | null) => void;
  onActivity: (key: TabId) => void;
  /** Bell = the program wants eyes (claude prompts ring it); typing clears. */
  onAttention: (key: TabId, on: boolean) => void;
  /** What the program wants eyes *for*. A bell is one byte and carries no
   *  payload; OSC 9 carries the sentence. It arrives on this tab's own pty, so
   *  no pid-to-tab lookup is needed to know who is asking. */
  onNotify: (key: TabId, message: string) => void;
  /** OSC 9;4 progress. `null` means the program withdrew it. */
  onProgress: (key: TabId, progress: TermProgress | null) => void;
  /** The line the user just submitted through this terminal. This is kept
   * deliberately narrow: lifecycle watchers may use an explicit command as
   * evidence, but never inspect ordinary prompt text. */
  onLineSubmit: (key: TabId, line: string) => void;
  onOpenFile: (path: string) => void;
  /** Focus the terminal when it becomes active. Once true only while the
   *  composer was hidden, back when the composer held a text input that would
   *  have been fighting for the same keystrokes. It is a pill strip now, so
   *  the terminal is always where typing should land. */
  autoFocus: boolean;
  fontSize: number;
  fontFamily: string;
  /** Row spacing as a multiple of the font's natural line height. */
  lineHeight: number;
  /** Weight for ordinary text; bold is derived from it. */
  fontWeight: number;
  /** Which renderer draws this terminal — see `AppSettings.termRenderer`. */
  renderer: "gpu" | "dom";
  theme: Record<string, string>;
}

export default function TerminalView({
  tab, active, onExit, onRegister, onActivity, onAttention, onNotify, onProgress,
  onLineSubmit, onOpenFile, autoFocus, fontSize, fontFamily, lineHeight, fontWeight, renderer, theme,
}: Props) {
  const elRef = useRef<HTMLDivElement>(null);
  const started = useRef(false);
  const fitRef = useRef<FitAddon | null>(null);
  const attachmentIdRef = useRef<AttachmentId | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const openFileRef = useRef(onOpenFile);
  openFileRef.current = onOpenFile;
  const activeRef = useRef(active);
  activeRef.current = active;
  const focusRef = useRef(tab.focus ?? "desktop");
  const canonicalSizeRef = useRef(tab.size);
  const takeFocusRef = useRef<(() => void) | null>(null);
  /** The live WebGL addon, when one is attached. Held because switching back to
   *  the DOM renderer is done by disposing it. */
  const webglRef = useRef<WebglAddon | null>(null);
  /** Read inside the mount effect, which must not re-run when the setting
   *  changes — a new Terminal would mean a new PTY. The effect below switches
   *  the renderer under the terminal that already exists. */
  const rendererRef = useRef(renderer);
  rendererRef.current = renderer;

  const projectGrid = useCallback(() => {
    const term = termRef.current;
    const fit = fitRef.current;
    if (!term || !fit) return;
    const fitted = fit.proposeDimensions();
    if (!fitted && focusRef.current === "desktop") return;
    if (!canonicalSizeRef.current && focusRef.current !== "desktop") return;
    const projected = projectTerminalGrid(
      focusRef.current,
      canonicalSizeRef.current,
      fitted ?? { cols: term.cols, rows: term.rows },
    );
    if (projected.resizeBackend) {
      fit.fit();
    } else if (term.cols !== projected.size.cols || term.rows !== projected.size.rows) {
      term.resize(projected.size.cols, projected.size.rows);
    }
  }, []);

  useEffect(() => {
    if (!elRef.current || started.current) return;
    started.current = true;

    const term = new Terminal({
      linkHandler: terminalLinkHandler(
        path => openFileRef.current(path),
        url => { void openPath(url).catch(console.error); },
      ),
      fontFamily,
      fontSize,
      // Both settable, because the right answer depends on the font, the
      // screen and the eyes reading it. A fixed 500 was tried to compensate
      // for the WebGL renderer's grayscale antialiasing and read as too heavy
      // — synthesised weight is not the same as a face drawn at that weight.
      lineHeight,
      fontWeight,
      fontWeightBold: boldWeightFor(fontWeight),
      cursorBlink: true,
      allowProposedApi: true,
      theme,
    });
    const fit = new FitAddon();
    fitRef.current = fit;
    termRef.current = term;
    term.loadAddon(fit);
    term.open(elRef.current);
    if (rendererRef.current === "gpu") {
      webglRef.current = attachRenderer(term);
    }
    projectGrid();

    let unlistenExit: UnlistenFn | null = null;
    let disposed = false;
    const exitCatchUp = createTabExitCatchUp(tab.key, onExit);

    // Manual repaint escape hatch (Ctrl+Shift+L): a clean refit + full
    // re-render. With an accelerated renderer this is rarely needed — no
    // window/grid jiggle, no SIGWINCH storm.
    const redraw = () => {
      projectGrid();
      term.refresh(0, term.rows - 1);
    };

    // PTY output arrives as raw bytes over a binary Channel. Write the
    // Uint8Array straight to xterm (it keeps a persistent UTF-8 decoder, so
    // multibyte chars split across chunks are reassembled correctly). Set
    // onmessage before attaching so replayed pre-attach output cannot race the
    // renderer callback.
    const onOutput = new Channel<ArrayBuffer>();
    onOutput.onmessage = (chunk) => {
      term.write(new Uint8Array(chunk));
      onActivity(tab.key);
    };

    (async () => {
      // The authoritative id is known before this component mounts, so even a
      // short-lived child maps directly to its Rust tab with no numeric bind.
      unlistenExit = await listen<{
        tabId: TabId;
        code: number | null;
        signal: string | null;
      }>("tab://exit", (e) => {
        exitCatchUp.event(e.payload);
      });
      if (disposed) {
        unlistenExit();
        return;
      }
      let attachmentId: AttachmentId;
      try {
        attachmentId = await tabAttachDesktop(tab.key, onOutput);
      } catch {
        // The bridge event may have preceded this component's listener. An
        // exited descriptor closes that window through the same one-shot gate.
        // A preparing-exit tab still reports Running, so keep the listener
        // alive for its eventual final exit instead of treating rejection as
        // final state by itself.
        const authoritative = await tabList().catch(() => []);
        if (!disposed) exitCatchUp.reconcile(authoritative);
        return;
      }
      if (disposed) {
        await tabDetach(tab.key, attachmentId).catch(() => {});
        return;
      }
      attachmentIdRef.current = attachmentId;
      // Roughly how much unsent text is sitting in the running program's input
      // line. Every keystroke passes through here on its way to the PTY, which
      // is enough to answer the only question that matters: is the prompt
      // empty right now?
      //
      // Deliberately approximate. Printable keys and pastes add, backspace
      // removes, Enter and Ctrl+C reset. Cursor keys and word-kill are not
      // modelled, so this can report text pending when the line is actually
      // clear. That is the safe direction to be wrong in — the cost is a
      // confirmation you did not need, never a half-written prompt sent.
      let pending = 0;
      const inputLine = new TerminalInputLine();

      term.onBell(() => onAttention(tab.key, true));

      // Returning true claims the sequence. Nothing else in aiterm reads OSC 9,
      // and an unclaimed sequence is passed through to be printed, which would
      // spray notification text into the buffer.
      term.parser.registerOscHandler(9, (data) => {
        const parsed = parseOsc9(data);
        if (parsed?.kind === "progress") {
          onProgress(tab.key, parsed.progress);
        } else if (parsed?.kind === "message") {
          onNotify(tab.key, parsed.message);
          onAttention(tab.key, true);
        }
        return true;
      });
      // Shift+Enter means "new line", not "send". A PTY has no key for that —
      // Shift+Enter arrives as the same \r as Enter — but backslash-return is
      // already the continuation gesture claude's composer understands, the
      // shell treats as an escaped newline, and aiterm chat reads the same
      // way. So the interception simply types it for you.
      term.attachCustomKeyEventHandler((ev) => {
        if (ev.type === "keydown" && ev.key === "Enter" && ev.shiftKey) {
          pending += 1; // the line is definitely not empty now
          tabWrite(tab.key, attachmentId, "\\\r");
          return false;
        }
        return true;
      });
      term.onData((data) => {
        onAttention(tab.key, false);
        const submitted = inputLine.write(data);
        if (data === "\r" || data === "\n") {
          onLineSubmit(tab.key, submitted ?? "");
          pending = 0;
        } else if (data === "\x03") {
          pending = 0;
        } else if (data === "\x7f" || data === "\b") {
          pending = Math.max(0, pending - 1);
        } else if (data >= " ") {
          pending += data.length;
        }
        tabWrite(tab.key, attachmentId, data);
      });
      term.onResize(({ cols, rows }) => {
        if (focusRef.current === "desktop") {
          tabResize(tab.key, attachmentId, cols, rows).catch(() => {});
        }
      });

      let focusPending = false;
      const takeFocus = async () => {
        if (focusPending) return;
        const proposed = fit.proposeDimensions();
        const requested = proposed
          ?? canonicalSizeRef.current
          ?? { cols: term.cols, rows: term.rows };
        focusPending = true;
        try {
          await tabTakeFocus(tab.key, attachmentId, requested.cols, requested.rows);
          if (disposed) return;
          focusRef.current = "desktop";
          canonicalSizeRef.current = requested;
          projectGrid();
        } catch {
          // The registry projection keeps xterm canonical if focus was denied.
        } finally {
          focusPending = false;
        }
      };
      const requestFocus = () => { void takeFocus(); };
      takeFocusRef.current = requestFocus;
      term.textarea?.addEventListener("focus", requestFocus);

      onRegister(tab.key, {
        write: (data) => tabWrite(tab.key, attachmentId, data),
        redraw,
        paste: (text) => {
          inputLine.paste(text);
          pending += text.length;
          tabWrite(
            tab.key,
            attachmentId,
            term.modes.bracketedPasteMode ? `\x1b[200~${text}\x1b[201~` : text,
          );
        },
        sendComposed: (text) => {
          const bracketed = term.modes.bracketedPasteMode;
          let payload: string;
          if (text.includes("\n")) {
            payload = bracketed
              ? `\x1b[200~${text}\x1b[201~\r`
              : text.replace(/\n/g, "\r") + "\r";
          } else {
            payload = text + "\r";
          }
          pending = 0; // this call ends in Enter, so the line is spent
          tabWrite(tab.key, attachmentId, payload);
        },
        focus: () => term.focus(),
        pendingInput: () => pending > 0,
        screen: () => {
          const buf = term.buffer.active;
          const rows: string[] = [];
          for (let i = 0; i < term.rows; i++) {
            const line = buf.getLine(buf.viewportY + i);
            rows.push(line ? line.translateToString(true) : "");
          }
          return rows;
        },
      });
      if (activeRef.current) requestFocus();
    })().catch(() => {});

    // Debounce resize→fit: splitter drags fire the observer continuously, and
    // a SIGWINCH storm makes TUIs (claude's input box) redraw over half-painted
    // frames. One fit at the end; the accelerated renderer repaints itself.
    let fitTimer: number | null = null;
    const ro = new ResizeObserver(() => {
      if (fitTimer !== null) clearTimeout(fitTimer);
      fitTimer = window.setTimeout(() => {
        fitTimer = null;
        if (elRef.current && elRef.current.offsetWidth > 0) projectGrid();
      }, 120);
    });
    ro.observe(elRef.current);

    return () => {
      disposed = true;
      exitCatchUp.dispose();
      if (fitTimer !== null) clearTimeout(fitTimer);
      ro.disconnect();
      unlistenExit?.();
      onRegister(tab.key, null);
      takeFocusRef.current = null;
      const attachmentId = attachmentIdRef.current;
      attachmentIdRef.current = null;
      if (attachmentId !== null) tabDetach(tab.key, attachmentId).catch(() => {});
      term.dispose();
      started.current = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (active) {
      takeFocusRef.current?.();
      if (autoFocus) termRef.current?.focus();
    }
  }, [active, autoFocus]);

  useEffect(() => {
    focusRef.current = tab.focus ?? "desktop";
    canonicalSizeRef.current = tab.size;
    if (focusRef.current !== "desktop") {
      // A phone can take focus while xterm's hidden textarea is still focused.
      // Blur it so the next deliberate click produces a fresh focus event and
      // an explicit desktop focus request instead of silently dropping input.
      termRef.current?.blur();
      projectGrid();
    }
  }, [projectGrid, tab.focus, tab.size?.cols, tab.size?.rows]);

  useEffect(() => {
    if (termRef.current && termRef.current.options.fontSize !== fontSize) {
      termRef.current.options.fontSize = fontSize;
      projectGrid();
    }
  }, [fontSize, projectGrid]);

  /** Swap the renderer under a running terminal.
   *
   *  Neither direction touches the PTY, so the session carries on through the
   *  change — which is the point of making it a setting rather than something
   *  that only applies to the next tab: the two can be compared on the same
   *  screenful of output. A refit and full repaint follow because the new
   *  renderer inherits nothing from the old one's surface. */
  useEffect(() => {
    const term = termRef.current;
    if (!term) return;
    const live = webglRef.current;
    if (renderer === "gpu" && !live) {
      webglRef.current = attachRenderer(term);
    } else if (renderer === "dom" && live) {
      live.dispose();
      webglRef.current = null;
    } else {
      return;
    }
    projectGrid();
    term.refresh(0, term.rows - 1);
  }, [projectGrid, renderer]);

  useEffect(() => {
    const term = termRef.current;
    if (term && term.options.fontFamily !== fontFamily) {
      term.options.fontFamily = fontFamily;
      projectGrid();
    }
  }, [fontFamily, projectGrid]);

  // Refit, because row spacing changes how many rows fit — without it the
  // terminal keeps its old row count and the child is told a size that is no
  // longer true.
  useEffect(() => {
    const term = termRef.current;
    if (term && term.options.lineHeight !== lineHeight) {
      term.options.lineHeight = lineHeight;
      projectGrid();
    }
  }, [lineHeight, projectGrid]);

  // Refit too: a heavier face can be wider, so the columns that fit change
  // with it.
  useEffect(() => {
    const term = termRef.current;
    if (term && term.options.fontWeight !== fontWeight) {
      term.options.fontWeight = fontWeight;
      term.options.fontWeightBold = boldWeightFor(fontWeight);
      projectGrid();
    }
  }, [fontWeight, projectGrid]);

  useEffect(() => {
    if (termRef.current) termRef.current.options.theme = theme;
  }, [theme]);

  return <div ref={elRef} className="term-host" style={{ display: active ? "block" : "none" }} />;
}
