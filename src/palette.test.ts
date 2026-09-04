import test from "node:test";
import assert from "node:assert/strict";
import { rankItems, scoreMatch, type PaletteItem } from "./palette.ts";

const item = (id: string, title: string, over: Partial<PaletteItem> = {}): PaletteItem =>
  ({ id, group: "Sessions", title, run: () => {}, ...over });

test("every query character must appear in order", () => {
  assert.equal(scoreMatch("git panel", "gti"), null);
  assert.ok(scoreMatch("git panel", "gp"));
  assert.ok(scoreMatch("Network Connectivity Test", "nct"));
});

test("word starts outrank letters buried in words", () => {
  const rows = rankItems([
    item("a", "Understanding the spine architecture"),
    item("b", "Network Connectivity Test"),
  ], "nc");
  assert.equal(rows[0].item.id, "b");
});

test("contiguous beats scattered, and an exact prefix wins", () => {
  const rows = rankItems([
    item("a", "aiterm android branch build test"),
    item("b", "Aiterm upstream sync"),
    item("c", "hooks-live-check"),
  ], "aiterm up");
  assert.equal(rows[0].item.id, "b");
});

test("subtitle and keywords can find a row the title cannot", () => {
  const rows = rankItems([
    item("a", "Ping", { subtitle: "~/AI-OS/projects/aiterm" }),
    item("b", "Quick check", { keywords: "grok" }),
  ], "grok");
  assert.deepEqual(rows.map((r) => r.item.id), ["b"]);
  const byPath = rankItems([item("a", "Ping", { subtitle: "~/AI-OS/projects/aiterm" })], "projects");
  assert.equal(byPath.length, 1);
});

test("an empty query keeps the caller's order within rank", () => {
  const rows = rankItems([
    item("z", "zeta", { rank: 1 }),
    item("a", "alpha", { rank: 0 }),
    item("b", "beta", { rank: 0 }),
  ], "");
  assert.deepEqual(rows.map((r) => r.item.id), ["a", "b", "z"]);
});

test("limit caps the list", () => {
  const many = Array.from({ length: 100 }, (_, i) => item(String(i), `session ${i}`));
  assert.equal(rankItems(many, "session", 10).length, 10);
});
