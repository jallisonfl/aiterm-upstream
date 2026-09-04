# aiterm

**One workbench for every AI coding CLI you run — and your phone as a second screen for all of it.**

aiterm puts a real terminal in the middle and surfaces everything the agent keeps on disk — sessions, files, diffs, tasks, usage — as first-class UI around it. Claude Code, Codex, Grok, OpenCode, Antigravity and any OpenAI-compatible API or local model, in one sidebar, one search, one conversation view. Pair an Android phone and carry the same sessions anywhere.

[![Latest release](https://img.shields.io/github/v/release/jallisonfl/aiterm-upstream?label=stable)](https://github.com/jallisonfl/aiterm-upstream/releases/latest)
[![Nightly](https://img.shields.io/github/v/release/jallisonfl/aiterm-upstream?include_prereleases&label=nightly)](https://github.com/jallisonfl/aiterm-upstream/releases)
[![Build](https://github.com/jallisonfl/aiterm-upstream/actions/workflows/build.yml/badge.svg?branch=5lime-dev)](https://github.com/jallisonfl/aiterm-upstream/actions/workflows/build.yml)

Linux-first (Fedora and Ubuntu daily). Tauri 2 + Rust, React, xterm.js. Not a re-implementation of any CLI: **the terminal is the terminal.**

---

## Why aiterm

- **The truth lives on disk, not in a copy.** State is read from where the agent already keeps it — transcripts, config, the drawn screen — so the UI cannot drift from what the CLI is actually doing.
- **Closed-loop, never guessing.** When aiterm drives a TUI (model picker, rewind, permission prompts), every keystroke is verified against what the TUI drew. It refuses rather than lies.
- **Multi-engine by design.** One adapter contract ([HARNESS-CONTRACT.md](docs/architecture/HARNESS-CONTRACT.md)) per engine; the sidebar, search index, fleet board and phone all work the same across all of them.
- **Your phone is a real client, not a viewer.** Four transports, pinned TLS, no third party in the middle, off by default.

## Engines

| Engine | What you get |
|---|---|
| **Claude Code** | New session in any folder, resume, instant fork (no process needed), rename, trash with undo. `/model`, `/rewind`, permission prompts and "Switch model?" become real keyboard-first dialogs. Plan-limit usage bars and per-session context fill. |
| **Codex** | List, resume, fork. Tasks and generated-image artifacts read from Codex's own files. Credits from the ChatGPT usage endpoint. |
| **Grok** | Sessions with an aiterm-minted id (row appears on the first frame), resume from any folder, titles and model from Grok's own summary, image artifacts. |
| **OpenCode** | Sessions read from OpenCode's SQLite store, launched from the same start menu, local providers resolved from `opencode.json`. |
| **Antigravity** (`agy`) | Conversations with agy's own titles and transcripts; resume; usage bars. |
| **API + local models** | Any OpenAI-compatible base URL — OpenRouter, OpenAI, Together, Groq, vLLM, llama.cpp. Add a key, test it, pick a shortlist, chat in a tab like any CLI. Keys live in a 0600 file and never touch argv. |

An engine that isn't installed simply isn't offered.

## Sessions

- **Sidebar** — every session from every engine, grouped by project or date, full-text searchable. Star, rename, fork, preview, trash/restore.
- **Hover card** — rest on a row: opening ask, last exchange, duration, model, mode, context used, tools, files.
- **Home dashboard** — a prompt box with engine/model/effort (type, Enter, the session opens already working) and the **Fleet board**: every session ranked by how much it wants from you right now.
- **File tabs** — browser-style strip; the terminal locked on the left, files from the explorer beside it in a CodeMirror editor with a save-conflict guard so an agent's concurrent write is never clobbered.
- **Previews** — Markdown live from the buffer, HTML as the real page in a sandboxed iframe, PDFs page by page.
- **Panels** — explorer, git (branches, log, per-file diffs on filesystem events), agent tasks and artifacts.
- **Bring in a crew** — pull a second agent (any engine, API or local model) into a live session; choose how long it stays. Five relay prompts, all editable.
- **Librarian** — a cheap model names the sessions the engine didn't; runs on an installed CLI's print mode (no extra spend) or an API provider. Hand-set names always win.
- **Attention** — bell, desktop notifications in the session's own words, taskbar badge, tray alert menu.
- **Changes ledger** — every file an agent created, modified or deleted, attributed to the session, persistent across restarts.

## Phone

Pair once by QR (single-use secret, 5-minute expiry, certificate fingerprint pinned before the phone sends anything, device approved on the desktop). One pairing covers every road.

| Road | When it's used |
|---|---|
| **LAN** | same network |
| **VPN** | Tailscale / WireGuard, MagicDNS names included |
| **Relay** | blind SNI-routed relay when neither side is reachable |
| **iroh** | peer-to-peer QUIC with relay fallback, no server of ours |

Any set of roads on at once, tried in an order you can reorder from either end.

On the phone: read any session as a conversation (markdown, tool cards, tappable file chips, live streaming), send input, interrupt, stop, rename, answer permission dialogs, bring in a second agent, start a new session in any desktop folder, browse and preview what the agent produced (images, video, text, PDF, live web preview), open a plain desktop shell, search and filter the fleet, usage strip, themes, biometric lock.

The phone never receives PTY bytes for agent sessions, never reads a transcript, never owns a process. It asks the desktop.

## Settings

Appearance (8 themes, accent, icon size, time zone, per-panel scale) · Fonts (UI + terminal, GPU/DOM renderer, install from file) · Agents · Model access · Librarian · Bring in · Remote access · Diagnostics.

## Install

Grab a build from [Releases](https://github.com/jallisonfl/aiterm-upstream/releases):

| Channel | Tag | What it is |
|---|---|---|
| **Stable** | `vX.Y.Z` | marked *latest*; cut from `main` |
| **Beta** | `vX.Y.Z-beta.N` | release candidate, fixes only |
| **Alpha** | `vX.Y.Z-alpha.N` | feature-complete snapshot from `5lime-dev` |
| **Nightly** | `nightly-YYYYMMDD` | that day's `5lime-dev`, published every evening |

Each release ships an AppImage, `.deb`, `.rpm`, and the phone APK. You'll want the CLIs you use installed and signed in; aiterm reads their state (`~/.claude/`, `~/.codex/`, `~/.grok/`, …) and drives the real thing.

## Build from source

```bash
npm install
npm run tauri dev                              # desktop, hot reload
npm run tauri build -- --bundles appimage,deb  # release build
```

```bash
cd mobile && ./gradlew assembleDebug           # phone APK → app/build/outputs/apk/debug/
```

Prerequisites: Rust toolchain, Node 22+, Tauri 2 Linux deps (`libwebkit2gtk-4.1-dev`, `libappindicator3-dev`, `librsvg2-dev`, `patchelf`), JDK 21 + Android SDK for the phone app, `sqlite3` binary for OpenCode sessions. Re-run `npm install` after pulling a commit that adds packages.

Large workspaces can exhaust `fs.inotify.max_user_watches`; if diffs or transcripts stop refreshing, raise it (`/etc/sysctl.d/60-inotify-aiterm.conf`: `fs.inotify.max_user_watches=524288`, `fs.inotify.max_user_instances=1024`).

## Branches and versions

```
main          stable — every commit is releasable, tagged vX.Y.Z
5lime-dev     daily development — nightlies and alphas are cut from here
feat/<slug>   one feature each, merged into 5lime-dev with --no-ff
release/X.Y   only while a beta is being stabilised
```

Releases are tags, never branches. `scripts/nightly.sh` freezes today's `5lime-dev` as `nightly-YYYYMMDD`; `scripts/release.sh alpha|beta|final X.Y.Z` bumps the version and tags. CI builds and publishes from the tag. See [CONTRIBUTING.md](CONTRIBUTING.md).

## Design notes

The session/fork model — who owns a session's lifetime, how resume and background mode move ids, why tabs own processes — is in [SESSION-MODEL.md](docs/architecture/SESSION-MODEL.md). The engine adapter contract is [HARNESS-CONTRACT.md](docs/architecture/HARNESS-CONTRACT.md). The remote transport model is [docs/remote/remote-roads.md](docs/remote/remote-roads.md).

Brand marks come from the LobeHub icon set (MIT), vendored under `src/assets/icons`; refresh with `node scripts/sync-icons.mjs`.

## License

Source-available, not open source. Free to build, run and modify for your own use, at home or at work; it may not be sold, offered as a service, or redistributed. See [LICENSE](LICENSE).
