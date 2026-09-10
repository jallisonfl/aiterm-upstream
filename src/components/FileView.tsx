import { useCallback, useEffect, useRef, useState } from "react";
import { EditorView, keymap } from "@codemirror/view";
import { Compartment, EditorState } from "@codemirror/state";
import { basicSetup } from "codemirror";
import { indentWithTab } from "@codemirror/commands";
import { LanguageDescription, syntaxHighlighting } from "@codemirror/language";
import { classHighlighter } from "@lezer/highlight";
import { languages } from "@codemirror/language-data";
import { homeAbbrev, openPath, readTextFile, renderMarkdown, writeTextFile } from "../ipc";
import { convertFileSrc } from "@tauri-apps/api/core";
import Icon from "./Icon";
import { Code, Eye } from "lucide-react";

/** Files that have a rendered form worth looking at. */
export function isMarkdown(path: string): boolean {
  return /\.(md|markdown|mdx)$/i.test(path);
}

/** An HTML file previews as the PAGE, in a sandboxed iframe over the asset
 *  protocol — which is what makes its relative `civic.jpg` and stylesheet
 *  load: they resolve against the asset URL and pass through the same
 *  scoped handler. The iframe reads the DISK, so the preview shows the last
 *  save, not the buffer — the price of real subresources. */
export function isHtml(path: string): boolean {
  return /\.x?html?$/i.test(path);
}

export type FileMode = "code" | "preview";
const MODE_KEY = "aiterm.markdownMode";
const HTML_MODE_KEY = "aiterm.htmlMode";

/** The mode markdown files open in: whatever was chosen last, anywhere.
 *  One setting, not per file — the preference is "I read markdown rendered"
 *  (or not), and a file is not the unit of that. Preview until chosen
 *  otherwise: a markdown file is written to be read, and the toggle is one
 *  click away when it is not. */
export function loadMarkdownMode(): FileMode {
  try {
    return localStorage.getItem(MODE_KEY) === "code" ? "code" : "preview";
  } catch {
    return "preview";
  }
}
function saveMarkdownMode(m: FileMode) {
  try { localStorage.setItem(MODE_KEY, m); } catch { /* private mode */ }
}

/** Same one-setting rule as markdown, its own memory: reading rendered
 *  markdown and reading rendered pages are different habits. */
export function loadHtmlMode(): FileMode {
  try {
    return localStorage.getItem(HTML_MODE_KEY) === "code" ? "code" : "preview";
  } catch {
    return "preview";
  }
}
function saveHtmlMode(m: FileMode) {
  try { localStorage.setItem(HTML_MODE_KEY, m); } catch { /* private mode */ }
}

/**
 * A file open in a center tab — CodeMirror over the real file on disk.
 *
 * The one thing this must never do is clobber somebody else's write: the
 * files it shows are exactly the files agents are editing in the terminal
 * beside it. So every save carries the mtime the buffer was loaded at, the
 * backend refuses a save over a file that moved past it, and the refusal is
 * surfaced as a choice (reload / overwrite) rather than resolved silently.
 * The project watcher's refresh tick keeps a clean buffer following the disk
 * for free; a dirty buffer is never touched behind the cursor.
 */
export default function FileView({
  path, active, refreshKey, onDirty,
}: {
  path: string;
  /** Whether this tab is the one on screen. CodeMirror measures nothing at
   *  display:none, so becoming visible schedules a re-measure. */
  active: boolean;
  /** The project watcher's tick — the same one the file tree refreshes on. */
  refreshKey: number;
  onDirty: (dirty: boolean) => void;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  /** mtime the buffer content was loaded or last saved at — the save's
   *  compare-and-swap token. */
  const mtimeRef = useRef<number>(0);
  const dirtyRef = useRef(false);
  /** True while a programmatic replace is dispatching, so the update
   *  listener doesn't read our own reload as the user typing. */
  const settingRef = useRef(false);
  const [err, setErr] = useState<string | null>(null);
  const [truncated, setTruncated] = useState(false);
  const [conflict, setConflict] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [saveErr, setSaveErr] = useState<string | null>(null);

  // ---- code / preview ----
  // A markdown file previews as rendered markdown (from the BUFFER — an edit
  // shows the moment you flip); an html file previews as the page in an
  // iframe (from the DISK — save to see a change). Every other file is
  // always code. The editor stays mounted underneath, hidden, so flipping
  // back costs nothing and loses nothing.
  const md = isMarkdown(path);
  const html = isHtml(path);
  const [mode, setModeState] = useState<FileMode>(() => (md ? loadMarkdownMode() : html ? loadHtmlMode() : "code"));
  const modeRef = useRef(mode);
  const [previewHtml, setPreviewHtml] = useState("");
  const [previewErr, setPreviewErr] = useState<string | null>(null);
  const previewSequence = useRef(0);
  /** Bumped whenever the disk moved (save, watcher reload), so the page
   *  iframe re-fetches — its src carries the nonce as a cache-buster. */
  const [pageNonce, setPageNonce] = useState(0);
  const renderPreview = useCallback(() => {
    if (isHtml(path)) { setPageNonce((n) => n + 1); return; }
    const view = viewRef.current;
    if (!view) return;
    const sequence = ++previewSequence.current;
    renderMarkdown(view.state.doc.toString()).then(
      (rendered) => {
        if (sequence !== previewSequence.current) return;
        setPreviewHtml(rendered);
        setPreviewErr(null);
      },
      (error) => {
        if (sequence !== previewSequence.current) return;
        setPreviewErr(String(error));
      },
    );
  }, [path]);
  const setMode = useCallback((m: FileMode) => {
    modeRef.current = m;
    setModeState(m);
    if (isHtml(path)) saveHtmlMode(m); else saveMarkdownMode(m);
    if (m === "preview") renderPreview();
    else requestAnimationFrame(() => { viewRef.current?.requestMeasure(); viewRef.current?.focus(); });
  }, [renderPreview, path]);
  const toggleMode = useCallback(() => setMode(modeRef.current === "preview" ? "code" : "preview"), [setMode]);

  const markDirty = useCallback((d: boolean) => {
    dirtyRef.current = d;
    setDirty(d);
    onDirty(d);
  }, [onDirty]);

  /** Replace the buffer with what is on disk. */
  const loadFromDisk = useCallback(async () => {
    const view = viewRef.current;
    if (!view) return;
    try {
      const f = await readTextFile(path);
      mtimeRef.current = f.mtime_ms;
      setTruncated(f.truncated);
      setErr(null);
      settingRef.current = true;
      view.dispatch({
        changes: { from: 0, to: view.state.doc.length, insert: f.content },
      });
      settingRef.current = false;
      markDirty(false);
      setConflict(false);
    } catch (e) {
      setErr(String(e));
    }
  }, [path, markDirty]);

  const save = useCallback(async (overwrite = false) => {
    const view = viewRef.current;
    if (!view) return;
    setSaveErr(null);
    try {
      const mtime = await writeTextFile(
        path,
        view.state.doc.toString(),
        overwrite ? null : mtimeRef.current,
      );
      mtimeRef.current = mtime;
      markDirty(false);
      setConflict(false);
      // The page iframe reads the disk; the disk just changed.
      if (isHtml(path)) setPageNonce((n) => n + 1);
    } catch (e) {
      if (String(e).includes("changed-on-disk")) setConflict(true);
      else setSaveErr(String(e));
    }
  }, [path, markDirty]);
  const saveRef = useRef(save);
  saveRef.current = save;

  useEffect(() => {
    if (!hostRef.current || viewRef.current) return;
    const lang = new Compartment();
    const lock = new Compartment();
    const view = new EditorView({
      state: EditorState.create({
        doc: "",
        extensions: [
          basicSetup,
          keymap.of([
            {
              key: "Mod-s",
              run: () => {
                saveRef.current();
                return true;
              },
            },
            indentWithTab,
          ]),
          syntaxHighlighting(classHighlighter),
          lang.of([]),
          lock.of([]),
          EditorView.updateListener.of((u) => {
            if (u.docChanged && !settingRef.current && !dirtyRef.current) {
              markDirty(true);
            }
            // A disk reload while previewing must show the new text.
            if (u.docChanged && modeRef.current === "preview") renderPreview();
          }),
        ],
      }),
      parent: hostRef.current,
    });
    viewRef.current = view;
    (async () => {
      try {
        const f = await readTextFile(path);
        mtimeRef.current = f.mtime_ms;
        setTruncated(f.truncated);
        settingRef.current = true;
        view.dispatch({ changes: { from: 0, insert: f.content } });
        settingRef.current = false;
        if (modeRef.current === "preview") renderPreview();
        if (f.truncated) {
          // A truncated read must not be saved back: it IS data loss.
          view.dispatch({
            effects: lock.reconfigure([
              EditorState.readOnly.of(true),
              EditorView.editable.of(false),
            ]),
          });
        }
      } catch (e) {
        setErr(String(e));
      }
      const name = path.split("/").pop() ?? path;
      const desc = LanguageDescription.matchFilename(languages, name);
      if (desc) {
        desc.load().then(
          (support) => view.dispatch({ effects: lang.reconfigure(support) }),
          () => {},
        );
      }
    })();
    return () => {
      view.destroy();
      viewRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // The project watcher ticked: something under the project wrote. A clean
  // buffer follows the disk; a dirty one only raises the conflict flag when
  // this file is what moved.
  useEffect(() => {
    if (!refreshKey || !viewRef.current) return;
    (async () => {
      try {
        const f = await readTextFile(path);
        if (f.mtime_ms === mtimeRef.current) return;
        if (dirtyRef.current) {
          setConflict(true);
          return;
        }
        const view = viewRef.current;
        if (!view) return;
        mtimeRef.current = f.mtime_ms;
        setTruncated(f.truncated);
        settingRef.current = true;
        view.dispatch({
          changes: { from: 0, to: view.state.doc.length, insert: f.content },
        });
        settingRef.current = false;
      } catch {
        /* deleted mid-view — the next explicit action will say so */
      }
    })();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refreshKey]);

  useEffect(() => {
    if (active) viewRef.current?.requestMeasure();
  }, [active]);

  // Ctrl+Shift+V flips the mode (the convention editors use for a markdown
  // preview), and Ctrl+S still saves from the preview, where the editor's own
  // keymap is not listening.
  useEffect(() => {
    if (!active || (!md && !html)) return;
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.shiftKey && e.key.toLowerCase() === "v") {
        e.preventDefault();
        toggleMode();
      } else if ((e.ctrlKey || e.metaKey) && !e.shiftKey && e.key.toLowerCase() === "s" && modeRef.current === "preview") {
        e.preventDefault();
        saveRef.current();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [active, md, html, toggleMode]);

  /** Links in the preview go to the system browser; the page must not
   *  navigate away from the app. */
  const onPreviewClick = (e: React.MouseEvent) => {
    const a = (e.target as HTMLElement).closest("a");
    if (!a) return;
    e.preventDefault();
    const href = a.getAttribute("href") ?? "";
    if (/^https?:\/\//i.test(href)) openPath(href).catch(() => {});
  };

  return (
    <div className="file-view">
      <div className="file-bar">
        <span className="file-bar-path" title={path}>{homeAbbrev(path)}</span>
        {truncated && <span className="file-bar-note">first 2 MB — read-only</span>}
        {(md || html) && (
          <div className="file-mode" role="group" aria-label="View as">
            <button
              className={"file-mode-btn" + (mode === "code" ? " on" : "")}
              title="Code (Ctrl+Shift+V toggles)"
              onClick={() => setMode("code")}
            ><Icon of={Code} size="sm" /> Code</button>
            <button
              className={"file-mode-btn" + (mode === "preview" ? " on" : "")}
              title="Preview (Ctrl+Shift+V toggles)"
              onClick={() => setMode("preview")}
            ><Icon of={Eye} size="sm" /> Preview</button>
          </div>
        )}
        <button
          className="tui-plain file-bar-btn"
          disabled={!dirty || truncated}
          title="Save (Ctrl+S)"
          onClick={() => save()}
        >
          Save{dirty ? " ●" : ""}
        </button>
        <button
          className="icon-btn"
          title="Open with the system app"
          onClick={() => openPath(path).catch(() => {})}
        >↗</button>
      </div>
      {conflict && (
        <div className="file-banner">
          <span>
            Changed on disk since you opened it — likely the agent in this
            session.
          </span>
          <button className="tui-plain" onClick={() => loadFromDisk()}>
            Reload theirs
          </button>
          <button className="tui-plain danger" onClick={() => save(true)}>
            Overwrite with mine
          </button>
        </div>
      )}
      {saveErr && <div className="file-banner error">{saveErr}</div>}
      {err ? (
        <div className="empty-note">
          Can't open {homeAbbrev(path)}: {err}
        </div>
      ) : (
        <>
          <div className="file-editor" ref={hostRef} hidden={mode === "preview"} />
          {mode === "preview" && (html ? (
            // The page itself, relative assets and all, via the scoped asset
            // protocol. Sandboxed: its scripts run, but it is not the app's
            // origin and cannot reach the IPC. Shows the last SAVE — the
            // nonce re-fetches after every write and watcher reload.
            <iframe
              className="page-preview"
              title={homeAbbrev(path)}
              sandbox="allow-scripts"
              src={convertFileSrc(path) + "?v=" + pageNonce}
            />
          ) : (
            previewErr ? <div className="empty-note">Can't render Markdown: {previewErr}</div> :
              <div
                className="md-preview"
                onClick={onPreviewClick}
                onDoubleClick={() => setMode("code")}
                title="Double-click to edit"
                dangerouslySetInnerHTML={{ __html: previewHtml }}
              />
          ))}
        </>
      )}
    </div>
  );
}
