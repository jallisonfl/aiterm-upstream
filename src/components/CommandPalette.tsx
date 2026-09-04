/**
 * One box for everything: Ctrl+Shift+P.
 *
 * Sessions, open tabs, projects and actions in one ranked list. Arrow keys
 * move, Enter runs, Escape closes; the pointer works too but is never
 * needed. The list is rebuilt from `items` on every keystroke — a few
 * hundred rows scored by `rankItems` is well under a millisecond, and
 * caching would only add a way for the list to be stale.
 *
 * The palette owns no data. The caller decides what is reachable and what
 * each row does; this file draws rows and keeps a cursor.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import { rankItems, type PaletteItem } from "../palette";
import Icon from "./Icon";
import { Search } from "lucide-react";

interface Props {
  items: PaletteItem[];
  onClose: () => void;
  /** Placeholder for the box — the caller may hint what is reachable. */
  placeholder?: string;
}

function Highlight({ text, hits }: { text: string; hits: number[] }) {
  if (hits.length === 0) return <>{text}</>;
  const set = new Set(hits);
  return (
    <>
      {[...text].map((ch, i) => (set.has(i) ? <mark key={i}>{ch}</mark> : ch))}
    </>
  );
}

export default function CommandPalette({ items, onClose, placeholder }: Props) {
  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const rows = useMemo(() => rankItems(items, query), [items, query]);

  useEffect(() => { inputRef.current?.focus(); }, []);
  useEffect(() => { setCursor(0); }, [query]);
  useEffect(() => {
    const el = listRef.current?.querySelector<HTMLElement>(`[data-idx="${cursor}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }, [cursor, rows]);

  const run = (i: number) => {
    const r = rows[i];
    if (!r) return;
    onClose();
    // After the palette is gone, so whatever the action focuses keeps focus.
    requestAnimationFrame(() => r.item.run());
  };

  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") { e.preventDefault(); onClose(); return; }
    if (e.key === "ArrowDown" || (e.ctrlKey && e.key === "n")) {
      e.preventDefault(); setCursor((c) => Math.min(rows.length - 1, c + 1)); return;
    }
    if (e.key === "ArrowUp" || (e.ctrlKey && e.key === "p")) {
      e.preventDefault(); setCursor((c) => Math.max(0, c - 1)); return;
    }
    if (e.key === "Enter") { e.preventDefault(); run(cursor); }
  };

  // Group headers are drawn where the group changes, not per row: the list
  // is one flat cursor space, so ↓ never has to skip a header.
  let lastGroup: string | null = null;

  return (
    <div className="palette-veil" onMouseDown={onClose}>
      <div className="palette" onMouseDown={(e) => e.stopPropagation()} role="dialog" aria-label="Command palette">
        <div className="palette-box">
          <Icon of={Search} size="sm" />
          <input
            ref={inputRef}
            className="palette-input"
            value={query}
            placeholder={placeholder ?? "Jump to a session, start one, or run an action…"}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={onKey}
            spellCheck={false}
          />
          <kbd className="palette-hint">esc</kbd>
        </div>
        <div className="palette-list" ref={listRef}>
          {rows.length === 0 && <div className="empty-note">Nothing matches</div>}
          {rows.map((r, i) => {
            const head = r.item.group !== lastGroup ? r.item.group : null;
            lastGroup = r.item.group;
            return (
              <div key={r.item.id}>
                {head && <div className="palette-group">{head}</div>}
                <button
                  data-idx={i}
                  className={"palette-row" + (i === cursor ? " on" : "")}
                  onMouseEnter={() => setCursor(i)}
                  onClick={() => run(i)}
                >
                  <span className="palette-title"><Highlight text={r.item.title} hits={r.hits} /></span>
                  {r.item.subtitle && <span className="palette-sub">{r.item.subtitle}</span>}
                </button>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
