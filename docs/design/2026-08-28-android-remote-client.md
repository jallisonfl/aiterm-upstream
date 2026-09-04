# Android Remote Client Design

## Goal

Add a native Android companion app that lets a paired phone securely use the
same AITerm desktop instance over a LAN or user-provided VPN. The phone is a
full interactive client: it can inspect and manage sessions and agents, open
and close terminal tabs, and interact with the desktop-owned terminal screen.

## Scope

The first release is direct-device only. AITerm desktop is the authoritative
host and embeds the gateway; Android never reads desktop transcript files or
starts a local agent process. There is no cloud account, relay, port-forwarding
guide, or remote Internet exposure. Reachability outside a home LAN is the
user's VPN responsibility (for example Tailscale or WireGuard).

The Android application id is `com.adroited.aiterm`. It is a Kotlin /
Jetpack Compose application, with a minimum SDK of 26 and current stable
AndroidX dependencies at implementation time.

## Architecture

```text
Android AITerm (Kotlin / Compose)
        |  HTTPS + WebSocket, certificate pinning
        v
AITerm desktop gateway (Rust, loopback disabled, LAN/VPN listener)
        |  one internal command/service API
        +----------------------------------+
        | Rust tab registry + screen model |
        | session / agent / PTY services   |
        +----------------------------------+
```

The desktop gateway is an adapter over service functions extracted from the
current Tauri commands. Tauri commands and remote RPC handlers must call the
same service functions; neither client may reach the other's transport layer.
PTYs remain owned by the desktop process.

Rust, rather than the React renderer, is authoritative for terminal-tab
identity, lifecycle, metadata, PTY mapping, input ownership, canonical terminal
dimensions, and terminal screen state. The desktop renderer keeps only
presentation state such as the selected tab, open file views, badges, and panel
visibility. It obtains the authoritative tab list and tab changes from Rust.
Android addresses a random, stable `TabId`; neither client sees the internal
numeric PTY id.

## Rust-owned tabs and terminal state

Every terminal starts through a `TabRegistry`. A tab record contains its
`TabId`, title, working directory, launch/session/agent metadata, process state,
canonical rows and columns, input owner, PTY mapping, and terminal screen. The
registry creates and closes PTYs, accepts metadata changes from session hooks,
and broadcasts typed tab events. Existing `pty_*` Tauri commands become thin
adapters during migration and are removed once every desktop call site uses the
registry.

The registry parses every PTY output byte from the moment the PTY starts,
whether Remote Access is enabled or a phone is connected. Parsing on demand is
not sufficient: escape sequences that established the current screen may have
occurred long before a phone attaches. The Rust screen model is bounded by the
configured viewport and scrollback limits and supports the normal and alternate
screen, Unicode wide and combining cells, indexed and RGB colors, text
attributes, cursor state, input modes, title changes, and terminal replies.

The implementation uses an exact, reviewed `alacritty_terminal` version behind
an AITerm-owned adapter and uses only its parser, terminal, grid, damage, and
event interfaces; AITerm keeps its existing `portable-pty` process lifecycle.
This boundary keeps a future emulator upgrade out of the tab, gateway, and wire
protocol APIs.

The desktop continues to render raw PTY bytes with xterm.js so this change does
not replace its mature renderer. Android never receives those bytes. Rust
serializes its canonical screen into snapshots and diffs for the Compose
renderer. Terminal-query replies have one source: xterm.js replies while a
desktop attachment owns input; Rust replies while a remote attachment owns
input or no renderer is attached. Losing focus also disables that attachment's
writes and resizes, so the discarded xterm.js reply cannot race the Rust reply.

Only one attachment may own input and size at a time. The owner selects the
canonical PTY dimensions. Other clients render that grid read-only without
resizing it underneath the owner. Taking focus is explicit and broadcasts the
new owner and dimensions to all attachments. A `TabId` is stable for the life
of its tab but is not persisted across a desktop restart, because the PTY it
identifies does not survive that restart. Conversation session ids remain the
persistent identity used to resume work.

## Pairing and trust

1. The desktop user explicitly enables Remote Access and selects **Pair phone**.
2. Desktop starts (or reuses) a TLS listener bound to selected LAN/VPN
   interfaces, creates a self-signed ECDSA P-256 certificate, and records its
   SHA-256 SPKI fingerprint.
3. Desktop creates a cryptographically random, single-use enrollment secret
   valid for five minutes. The QR contains a versioned `aiterm://pair` payload
   with hostname/IP candidates, port, server fingerprint, enrollment secret,
   and a display name for the desktop.
4. Android scans the QR, confirms the desktop identity, opens TLS only if the
   presented public-key fingerprint matches the QR, then sends its generated
   P-256 public key and requested device name with the enrollment secret.
5. Desktop displays the request and requires explicit approval. On approval it
   assigns a random device id, persists the Android public key and metadata,
   and returns that non-secret device id. Future authentication requires a
   signature from the enrolled private key; there is no bearer refresh token.
   The enrollment secret is consumed whether approval succeeds or fails.
6. Android stores the private key in Android Keystore and stores the device id
   and desktop metadata in private application storage; key use requires
   `BIOMETRIC_STRONG | DEVICE_CREDENTIAL` user authentication.
   The app locks after five minutes in the background and requires biometric or
   device-PIN authentication before reconnecting or displaying terminal data.

The desktop stores its listener key, trusted devices, and pending enrollments
in a private AITerm state directory with owner-only permissions. Device rows
include name, public-key fingerprint, created time, and last-seen time.
Revocation terminates active connections, rejects future handshakes, and
deletes the device record. Pairing has no
implicit approval and no fallback to HTTP, an unpinned certificate, a bearer
token alone, or mDNS-discovered hosts.

## The pairing payload

The QR encodes one versioned URI and nothing else:

```text
aiterm://pair?v=1&h=<host>&h=<host>&p=<port>&f=<fingerprint>&s=<secret>&n=<name>
```

- `v` is the payload version. A phone that does not know the version stops;
  it never guesses at a field layout that governs trust.
- `h` repeats, once per candidate address, in the order the desktop prefers.
  Repetition rather than a delimiter keeps IPv6 literals intact. The phone
  tries them in order and keeps the one that worked.
- `p` is the TCP port; `f` is the base64url SHA-256 of the listener's SPKI,
  which the phone pins before it sends anything; `s` is the base64url of the
  32-byte single-use enrollment secret; `n` is the desktop's display name,
  percent-encoded, shown to the user so they can confirm which machine they
  are pairing with.

The secret appears in the QR and in the `pair.request` frame, and nowhere
else on either side. The desktop renders the QR to an image in the backend
so the payload never becomes a string in its renderer process; the phone
parses it in memory and never writes it to storage or a log.

Pairing then runs over the same `/v1/ws` socket as everything else. The
client opens TLS with `f` pinned and sends CBOR:

```text
-> { kind: "pair.request", enrollment_secret: bytes, device_name: text,
     public_key: bytes }   // SEC1 compressed P-256, 33 bytes
<- { kind: "pair.pending", request_id: text }
<- { kind: "pair.approved", device_id: text }   // or { kind: "pair.denied" }
```

An already-trusted device instead answers the server's opening challenge:

```text
<- { kind: "auth.challenge", nonce: bytes }   // 32 bytes
-> { kind: "auth.proof", device_id: text, signature_der: bytes }
<- { kind: "auth.ok" }
```

## Transport and protocol

All traffic uses TLS 1.3 with the desktop certificate pinned from pairing.
After TLS, Android proves possession of its private key by signing a fresh
server nonce; the server verifies the trusted, non-revoked device key and
creates a short-lived connection session. A reconnect repeats this proof and
does not need another QR scan.

The gateway exposes one versioned WebSocket endpoint, `/v1/ws`. Frames are
binary CBOR envelopes:

```text
{ version: 1, request_id: u64, kind: string, payload: bytes }
```

Client requests include `session.list`, `session.preview`, `session.open`,
`session.delete`, `session.fork`, `session.stop`, `agent.list`, `agent.action`,
`tab.list`, `tab.open`, `tab.close`, `terminal.attach`, `terminal.input`,
`terminal.resize`, `terminal.focus`, `terminal.scrollback`, and
`terminal.detach`. Opening or resuming a session creates or selects a Rust tab;
session ids and tab ids remain different types. The exact first-release command
set is constrained to actions already exposed by AITerm desktop; sensitive
desktop-only actions (settings-file writes, font installation, arbitrary file
system writes, and diagnostic toggles) stay local until their permissions and
UX are separately designed.

Server events include `state.snapshot`, `session.changed`, `agent.changed`,
`tab.changed`, `terminal.snapshot`, `terminal.diff`, `terminal.exited`,
`terminal.title`, `terminal.focus_changed`, and `error`. Terminal frames contain
screen cells, not PTY bytes.

A `terminal.snapshot` contains `tab_id`, monotonically increasing `revision`,
rows, columns, the complete visible viewport, cursor, and terminal modes.
Scrollback is a separate bounded, paged resource; the server may send an initial
page beside the snapshot to populate the phone without making live viewport
diffs depend on history mutation. Each cell contains its bounded UTF-8 grapheme,
display width, foreground/background color, and attribute flags. Continuation
cells for wide glyphs are explicit.

A `terminal.diff` contains `tab_id`, `base_revision`, `revision`, changed rows,
and any changed cursor, mode, title, or focus data. Rust coalesces PTY damage to
at most one event per display frame and initially transmits complete changed
rows; run-level compression may be added without changing the envelope. The
phone applies a diff only when `base_revision` equals its current revision. An
attach, reconnect, unknown revision, dropped event, resize, or revision mismatch
gets a new snapshot instead of replaying terminal bytes. This makes recovery
independent of which historical escape sequences are still buffered.

A semantic snapshot or diff may require multiple wire frames. The server splits
it on complete row boundaries into an ordered transfer whose individual frames
are each smaller than 1 MiB. The phone validates and buffers a bounded transfer,
then replaces/applies it atomically only after every chunk arrives. A missing,
duplicate, out-of-order, or invalid chunk discards the entire transfer and
requests a new snapshot; partial transfers never advance the client's revision.

Attaching a second client remains read-capable but its input and resize requests
are rejected with `terminal.input_not_owned` until it explicitly takes focus.
Taking focus emits an event to every attached client. This avoids accidental
concurrent typing while preserving visibility everywhere.

All protocol parsers enforce maximum frame sizes, strict version checks,
request rate limits, input and terminal-size bounds, and request-id replay
protection. Validation failures return structured errors and never panic or
drop the process.

## Android application

The app has four primary states:

- **No desktops:** scan QR or open the camera pairing view.
- **Locked:** show paired desktops but require biometric/PIN before connecting
  or displaying cached metadata.
- **Disconnected:** show the selected desktop, trusted-device status, and a
  reconnect action; never silently weaken certificate pinning.
- **Connected:** a phone-optimized session drawer, active terminal, agent
  summary, and overflow actions.

The terminal is a native Compose grid that consumes Rust-produced screen
snapshots and diffs. It is a renderer and input surface, not a second terminal
emulator. It supports Unicode wide and combining glyphs, colors and attributes,
cursor styles, copy/paste, paged scrollback, selection, validated links,
canonical resize, soft keyboard input, mouse reporting when the current mode
allows it, and an extra-key row (Escape, Control, Alt, Tab, arrows, Page
Up/Down, and common shell symbols). It must not use a WebView.

The session drawer shows the same session metadata the desktop exposes. The
active terminal presents connection and focus ownership states prominently.
Phone controls map to the server command set; a control absent from the remote
protocol is not simulated locally. The app uses foreground notifications only
while a user-enabled persistent session is active, and stops the service on
disconnect, lock, revoke, or user action.

## Desktop UI

Remote Access appears in Settings with enable/disable, bind addresses, port,
current certificate fingerprint, **Pair phone**, pending approval requests,
and a trusted-device list. Disabling Remote Access closes the listener and all
connections but does not revoke devices; revocation is explicit. The QR dialog
shows the expiry countdown and never logs or copies the enrollment secret.

## Reliability and observability

The listener is off by default and fails closed. Its lifecycle is independent
of a single terminal tab: desktop app restarts preserve trusted devices and
regenerate only expired enrollment tokens. It does not attempt to start at OS
login until that behavior is separately approved.

Screen parsing is part of tab ownership, not listener ownership. Disabling
Remote Access stops network and remote-subscriber work but does not discard tab
state or alter desktop terminal behavior. Screen snapshots and diffs have
explicit cell, row, scrollback, frame, and frequency bounds. Invalid terminal
output must not panic the desktop process.

Structured, redacted diagnostics record listener lifecycle, device id prefix,
connection state, protocol version, and denial reason. They never record QR
payloads, enrollment secrets, credentials, terminal input, or terminal output.

## Testing strategy

The tab registry is tested without Tauri using fake PTY and subscriber
adapters. Tests cover spawn/list/update/close, session-hook rekeying, desktop
and phone attachments, explicit focus transfer, owner-only input and resize,
exit state, and cleanup when a renderer disconnects.

The screen adapter is fed fixed byte fixtures covering split UTF-8, combining
and wide glyphs, RGB and indexed colors, cursor and mode changes, normal and
alternate screens, scrollback, resize, terminal queries, and malformed escape
sequences. Snapshot/diff round-trip tests prove that applying every diff gives
the same typed screen as a fresh snapshot. Random byte and resize sequences
must not panic or exceed configured memory and frame limits.

Desktop integration tests prove that opening, clearing, resuming, focusing,
resizing, and closing a tab still use xterm.js correctly with Remote Access
both off and on. Android JVM tests cover CBOR decoding, revision mismatch,
snapshot replacement, changed-row application, and input encoding. Compose
tests cover wide-cell layout, selection, scrollback, focus state, rotation, and
the extra-key row. The final interoperability run pairs a phone, opens a tab
that predates pairing, exercises a full-screen TUI, transfers focus in both
directions, disconnects during output, reconnects by snapshot, and revokes the
phone.

## Non-goals for the first release

- Cloud relay, hosted account, or NAT traversal.
- iOS support.
- Sharing a desktop with other people or role-based permissions.
- Direct remote filesystem browsing/editing.
- Background agent execution from Android after the desktop app exits.
- Importing, exporting, or copying device credentials between phones.
- Sixel, Kitty graphics, inline images, or arbitrary embedded terminal media on
  Android. Unsupported graphics sequences do not bypass bounds or crash the
  screen parser.

## Acceptance criteria

1. A user can pair one Android device by scanning a five-minute QR and
   approving it on the desktop; it reconnects after an app restart without QR.
2. A wrong/changed TLS public key, expired/consumed QR secret, unapproved
   device, or revoked device cannot connect.
3. A paired and unlocked device can manage the agreed desktop sessions and
   interact with the canonical terminal screen without Unicode or style
   corruption.
4. Desktop and Android receive coherent tab/session/agent changes. Reconnects,
   revision mismatches, and rolled event queues recover from a current screen
   snapshot rather than historical PTY-byte replay.
5. A second client cannot silently interleave terminal input with the current
   input owner or resize that owner's PTY.
6. The Android app needs biometric/PIN after the configured lock timeout and
   stores private material only through Android Keystore.
7. A terminal started before Remote Access is enabled can be opened on Android
   with its current normal or alternate screen intact.
8. The desktop remains usable with Remote Access disabled and preserves its
   existing xterm.js rendering, input, selection, and scrollback behavior.
