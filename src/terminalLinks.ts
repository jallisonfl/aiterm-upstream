import type { ILinkHandler } from "@xterm/xterm";

export type TerminalLink =
  | { kind: "file"; path: string }
  | { kind: "web"; url: string };

const controls = /[\u0000-\u001f\u007f]/;
const sourceLocation = /:\d+(?::\d+)?$/;

/** OSC 8 links are supplied by terminal programs. Enabling xterm's non-HTTP
 * links must not hand arbitrary protocols to the system opener. File links
 * use the app's filesystem backend, exactly like the Explorer. */
export function parseTerminalLink(target: string): TerminalLink | null {
  if (controls.test(target)) return null;
  if (target.startsWith("/") && !target.startsWith("//")) {
    return { kind: "file", path: target.replace(sourceLocation, "") };
  }
  try {
    const url = new URL(target);
    if (url.protocol === "http:" || url.protocol === "https:") {
      return { kind: "web", url: url.href };
    }
    if (url.protocol !== "file:" || (url.hostname && url.hostname !== "localhost")) {
      return null;
    }
    // Strip source locations before decoding, preserving an encoded colon
    // that is part of a filename. The editor currently opens at the top.
    const path = decodeURIComponent(url.pathname.replace(sourceLocation, ""));
    if (!path.startsWith("/") || path.startsWith("//") || controls.test(path)) return null;
    return { kind: "file", path };
  } catch {
    return null;
  }
}

export function terminalLinkHandler(
  openFile: (path: string) => void,
  openWeb: (url: string) => void,
): ILinkHandler {
  return {
    allowNonHttpProtocols: true,
    activate(event, target) {
      event.preventDefault();
      const link = parseTerminalLink(target);
      if (link?.kind === "file") openFile(link.path);
      else if (link?.kind === "web") openWeb(link.url);
    },
  };
}
