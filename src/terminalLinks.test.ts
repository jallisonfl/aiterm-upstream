import assert from "node:assert/strict";
import test from "node:test";
import { parseTerminalLink, terminalLinkHandler } from "./terminalLinks.ts";

test("assistant absolute file links open the actual path", () => {
  for (const path of [
    "/home/matt/Projects/mojo/README.md",
    "/home/matt/Projects/mojo/docs/OPTIMIZATION-2026-09-05.md",
    "/tmp/My Report #1%20.md",
  ]) assert.deepEqual(parseTerminalLink(path), { kind: "file", path });
});

test("source line and column suffixes are not passed as part of the filename", () => {
  for (const target of ["/tmp/app.ts:12", "/tmp/app.ts:12:4", "file:///tmp/app.ts:12:4#L12"])
    assert.deepEqual(parseTerminalLink(target), { kind: "file", path: "/tmp/app.ts" });
});

test("file URLs decode spaces while preserving encoded filename punctuation", () => {
  assert.deepEqual(parseTerminalLink("file:///tmp/My%20Report%231.md"), { kind: "file", path: "/tmp/My Report#1.md" });
  assert.deepEqual(parseTerminalLink("file://localhost/tmp/report%3A12"), { kind: "file", path: "/tmp/report:12" });
});

test("web links remain web links", () => {
  const url = "https://example.com/docs?q=one#two";
  assert.deepEqual(parseTerminalLink(url), { kind: "web", url });
});

test("unsupported protocols, foreign file hosts and malformed targets are rejected", () => {
  for (const target of [
    "javascript:alert(1)", "data:text/html,test", "command:run", "mailto:a@example.com",
    "file://remote-host/etc/passwd", "//remote-host/path", "file:////remote-host/path",
    "file:///tmp/%ZZ", "file:///tmp/%00bad", "/tmp/bad\nname", "README.md", "",
  ]) assert.equal(parseTerminalLink(target), null, target);
});

test("xterm activation routes file links into app tabs and web links to the browser", () => {
  const files: string[] = [];
  const urls: string[] = [];
  let prevented = 0;
  const handler = terminalLinkHandler(path => files.push(path), url => urls.push(url));
  assert.equal(handler.allowNonHttpProtocols, true);
  const event = { preventDefault: () => prevented++ } as MouseEvent;
  const range = { start: { x: 1, y: 1 }, end: { x: 5, y: 1 } };
  handler.activate(event, "/home/matt/Projects/mojo/README.md", range);
  handler.activate(event, "https://example.com/", range);
  handler.activate(event, "javascript:alert(1)", range);
  assert.deepEqual(files, ["/home/matt/Projects/mojo/README.md"]);
  assert.deepEqual(urls, ["https://example.com/"]);
  assert.equal(prevented, 3);
});
