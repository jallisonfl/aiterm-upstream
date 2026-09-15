/** Launch-time update check: when to ask again, and what to remember.
 *
 *  The last check is kept apart from settings — it is a timestamp that churns,
 *  not a preference, and settings are saved as one blob on every change. */

const LAST_KEY = "aiterm.updates.lastCheck";
/** Once a day is plenty for a desktop app; GitHub's unauthenticated budget is
 *  60 requests an hour per address, shared with everything else on the box. */
export const CHECK_INTERVAL_MS = 24 * 60 * 60 * 1000;

/** True when `now` is at least the interval past `last` (or there is no last). */
export function isCheckDue(last: number | null, now: number, interval = CHECK_INTERVAL_MS): boolean {
  if (last === null || !Number.isFinite(last)) return true;
  // A clock set back would make "next check" land in the future forever;
  // treat a last check in the future as never having happened.
  if (last > now) return true;
  return now - last >= interval;
}

export function readLastCheck(): number | null {
  try {
    const raw = localStorage.getItem(LAST_KEY);
    if (!raw) return null;
    const n = Number(raw);
    return Number.isFinite(n) ? n : null;
  } catch {
    return null;
  }
}

export function writeLastCheck(now: number) {
  try { localStorage.setItem(LAST_KEY, String(now)); } catch { /* private mode */ }
}

/** "12.3 MB" for the download button. */
export function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}
