import test from "node:test";
import assert from "node:assert/strict";
import { isCheckDue, fmtBytes, CHECK_INTERVAL_MS } from "./updates.ts";

const now = 1_800_000_000_000;

test("first launch is due", () => {
  assert.equal(isCheckDue(null, now), true);
});

test("a check inside the interval is not due", () => {
  assert.equal(isCheckDue(now - 1000, now), false);
  assert.equal(isCheckDue(now - CHECK_INTERVAL_MS + 1, now), false);
});

test("a check at or past the interval is due", () => {
  assert.equal(isCheckDue(now - CHECK_INTERVAL_MS, now), true);
  assert.equal(isCheckDue(now - 3 * CHECK_INTERVAL_MS, now), true);
});

test("a last check in the future (clock set back) does not block forever", () => {
  assert.equal(isCheckDue(now + 60_000, now), true);
});

test("bytes read as a size", () => {
  assert.equal(fmtBytes(512), "512 B");
  assert.equal(fmtBytes(20 * 1024), "20 KB");
  assert.equal(fmtBytes(25_014_310), "23.9 MB");
});
