/**
 * The command palette's matching and ranking — pure, so `npm run test:ui`
 * can hold it to account without a renderer.
 *
 * One box, everything reachable: sessions, open tabs, projects, and the
 * actions the top bar and menus hide behind clicks. The person types a few
 * letters and presses Enter; what they meant has to be the first row far
 * more often than not, so the scoring is deliberately opinionated:
 *
 * - Every query character must appear in order (a subsequence), or the item
 *   is out. No fuzzy-by-edit-distance: "gti" should not find "git", and a
 *   user who typed it will correct it faster than they would read a wrong
 *   list.
 * - Matches at word starts are worth far more than matches inside words —
 *   "nc" should be "Network Connectivity Test", not any title with an n
 *   followed somewhere by a c.
 * - Contiguous runs beat scattered letters; an early first match beats a
 *   late one; a shorter haystack beats a longer one when all else ties.
 * - With no query at all, every item is a match and the caller's own order
 *   (recency for sessions, a fixed order for actions) is what you see.
 */

export interface PaletteItem {
  id: string;
  /** Section the row sits under: "Needs you", "Sessions", "Actions"… */
  group: string;
  title: string;
  subtitle?: string;
  /** Extra text a query may match against — a path, an engine name — that
   *  is not worth drawing in the row. */
  keywords?: string;
  /** Section order: lower first. Items keep their given order within it. */
  rank?: number;
  run: () => void;
}

export interface Scored<T> {
  item: T;
  score: number;
  /** Indices into the title that matched, for highlighting. */
  hits: number[];
}

const isWordStart = (s: string, i: number) =>
  i === 0 || !/[a-z0-9]/i.test(s[i - 1]);

/**
 * Score one haystack against a query. Null when not every character is
 * found in order. The score is a positive number; bigger is better.
 */
export function scoreMatch(hay: string, query: string): { score: number; hits: number[] } | null {
  const q = query.toLowerCase();
  const h = hay.toLowerCase();
  if (q.length === 0) return { score: 1, hits: [] };
  const hits: number[] = [];
  let score = 0;
  let from = 0;
  let prev = -2;
  for (const ch of q) {
    // Prefer a word-start occurrence when one exists ahead; otherwise the
    // next occurrence. Greedy, but the tie-breaks below make it good enough
    // for titles a person is looking at while they type.
    let at = -1;
    for (let i = from; i < h.length; i++) {
      if (h[i] !== ch) continue;
      if (isWordStart(h, i)) { at = i; break; }
      if (at < 0) at = i;
    }
    if (at < 0) return null;
    hits.push(at);
    score += 10;
    if (isWordStart(h, at)) score += 20;
    if (at === prev + 1) score += 15;
    prev = at;
    from = at + 1;
  }
  // Earlier first hit and a shorter haystack both nudge upward.
  score += Math.max(0, 20 - hits[0]);
  score += Math.max(0, 40 - h.length) / 4;
  return { score, hits };
}

/**
 * Rank items for a query. The title is what a match is scored on; the
 * subtitle and keywords count too, at a discount, so a path or an engine
 * name can find a session whose title says nothing of them.
 */
export function rankItems<T extends PaletteItem>(items: T[], query: string, limit = 40): Scored<T>[] {
  const q = query.trim();
  const out: Scored<T>[] = [];
  for (const item of items) {
    if (!q) { out.push({ item, score: 0, hits: [] }); continue; }
    const t = scoreMatch(item.title, q);
    const sub = item.subtitle ? scoreMatch(item.subtitle, q) : null;
    const kw = item.keywords ? scoreMatch(item.keywords, q) : null;
    const best = Math.max(t?.score ?? -1, (sub?.score ?? -1) * 0.6, (kw?.score ?? -1) * 0.5);
    if (best < 0) continue;
    out.push({ item, score: best, hits: t?.hits ?? [] });
  }
  if (q) {
    // Stable: equal scores keep the caller's order, which is recency.
    out.sort((a, b) => b.score - a.score || (a.item.rank ?? 0) - (b.item.rank ?? 0));
  } else {
    out.sort((a, b) => (a.item.rank ?? 0) - (b.item.rank ?? 0));
  }
  return out.slice(0, limit);
}
