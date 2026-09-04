# Remote access: topology, pairing, and how to test it

AITerm desktop can be driven from a paired Android phone. This describes what
that connection is, what it deliberately is not, and how to verify it by hand.

## What the connection is

The desktop is the host. It embeds a TLS gateway; the phone is a client. The
same pinned connection can use LAN, VPN, the AITerm relay, or an opportunistic
direct QUIC tunnel:

```text
Android AITerm  ── pinned TLS/WebSocket ──▶  AITerm desktop gateway
   (phone)       over LAN/VPN/relay/QUIC          │
                                              sessions, agents, PTYs
```

The phone never reads a transcript file, never starts an agent process, and
never owns a PTY. It asks the desktop, and the desktop answers.

The phone tries saved LAN and VPN routes before its route-specific relay
hostname. If the relay wins, the already-authenticated connection requests a
short-lived rendezvous. The relay reveals each peer's observed UDP address
only after separate random desktop and phone cookies arrive, and the peers try
to upgrade to QUIC. A failed hole punch leaves the relay connection in place;
no port forwarding is required.

QUIC is only a tunnel underneath the existing pinned TLS/WebSocket protocol.
The relay and QUIC path do not implement a second session API, do not receive
the phone's device key, and do not bypass desktop authorization. Loopback and
link-local addresses are not offered as direct gateway candidates.

The listener is **off by default** and starts nothing on disk until you turn
it on. A desktop that never pairs a phone never grows a trusted-device file.

## Pairing

1. **Settings → Remote access → Turn on.** Pick the address the phone can
   reach. The panel shows the listener's certificate fingerprint, grouped in
   fours so a person can actually compare it.
2. **Pair phone.** A QR appears with a countdown. It carries a single-use
   32-byte secret and stops working after five minutes, whichever comes first.
3. **Scan it in AITerm on the phone.** The phone pins the fingerprint from the
   QR *before* it sends anything, and refuses to continue if the certificate
   presented does not match.
4. **Approve on the desktop.** The phone appears under "Pair a phone" with the
   fingerprint of the key it generated. Nothing is trusted until you approve
   it here. The QR secret is consumed either way — approve or deny, that code
   is spent.
5. The phone stores its private key in Android Keystore, requiring biometric
   or device PIN. Reconnecting later needs no QR: the phone signs a fresh
   challenge with the key you approved.

Pairing the same phone again replaces its row rather than adding a second one,
so the list always shows one row per phone and revoking that row is complete.

## Revoking

**Settings → Remote access → Paired phones → Revoke**, then confirm. Revoking
forgets the key, refuses future handshakes, and drops the phone's live
connection.

Turning remote access **off** is a different statement: it closes the listener
and every connection but keeps your phones trusted, so turning it back on does
not mean scanning a QR again.

## When the fingerprint changes

The phone refuses to connect and says the desktop's identity does not match.
That is the pinning working. It means one of:

- The desktop's TLS identity was regenerated — its key file was deleted, or
  the remote state directory was moved or restored from a backup.
- You are connecting to a different machine that answers on that address.
- Something is intercepting the connection.

There is no "continue anyway". The fix is to revoke the phone on the desktop
and pair again, which is a deliberate act at the desktop keyboard — and if you
did not expect the change, find out why before you do it.

## What a phone cannot do

Not oversights; each is a decision:

- Enable the listener, approve another device, or revoke one. Trust is granted
  at the desktop keyboard only.
- Write settings, install fonts, browse or edit the filesystem, or toggle
  diagnostics. These return `remote.unsupported`.
- Type into a terminal another client is holding input focus on. Two clients
  may watch one terminal; only one may type, and taking focus is explicit and
  visible to everyone attached.
- Keep an agent running after the desktop app exits.

## Rust terminal-screen verification

The desktop remains the only PTY and terminal-emulator owner. It parses the
canonical screen from the first PTY byte; the desktop attachment still receives
the raw stream used by xterm.js, so this work does not change the desktop CSS,
renderer, selection, paste, scrollback, or TUI overlay path.

The transport has explicit bounds: 1..=512 columns and rows, 5,000
scrollback rows, sub-1 MiB serialized frames/chunks split only at row
boundaries, at most one coalesced diff per 16 ms, and a cell with one base
scalar plus at most 32 combining scalars. Remote attachments receive complete
snapshots and revisioned changed-row diffs. A revision mismatch, dropped
queue, invalid/incomplete transfer, or reconnect requests a fresh snapshot;
it never reconstructs the screen from historical PTY bytes.
Scrollback is fetched separately as a bounded page, not carried in a live
viewport snapshot or live diff.

On 2026-08-29 the following fresh checks completed successfully:

```text
git diff --check && npm run test:ui && npm run build
# 68 UI tests passed; production build completed.

CARGO_TARGET_DIR=/tmp/aiterm-android-rust-target cargo test --lib \
  --test remote_protocol --test remote_auth --test remote_server \
  --test remote_terminal --test remote_desktop --test tab_registry \
  --test terminal_screen
# exited 0; 390 library tests passed, with the selected protocol, gateway,
# desktop, registry, and screen suites also passing.
```

`cargo test --test backend` is intentionally not run here: its integration
fixture uses the real HOME and was ruled unsafe after the obsolete test caused
destructive external-state damage. This is one blocked verification item, not
a pass; earlier committed evidence recorded the safe backend fixture result as
13/13. No HOME override is used to evade that constraint.

The native manual smoke was limited to read-only observation. An existing
`/usr/bin/aiterm` process was present, but KWin 6.7.4 is running Wayland and
the available X11 window queries exposed only the Xwayland bridge, not an
AITerm window. KDE Remote Control approval had previously blocked synthetic
pointer/keyboard input, so no typing, paste, resize, focus transfer, or close
action is claimed here. The automated gateway, focus, alternate-screen, and
screen-recovery tests above are supporting evidence only; a person must still
perform the interaction checklist below on an approved desktop/client.

## Manual test

Requires a desktop and a phone on the same LAN or VPN.

**Pairing**
1. Turn remote access on, note the fingerprint, tap **Pair phone**.
2. Scan on the phone. Confirm the name and fingerprint it shows match the
   desktop's.
3. Approve on the desktop. The phone reaches the session list.

**Terminal fidelity**
4. Open a session on the phone and run `printf '✓\n'`. A multi-byte character
   must arrive intact — this catches chunk-boundary corruption.
5. Run something that draws a full screen (`top`, or an agent TUI). Rotate the
   phone and confirm the resize reflows rather than corrupting.

**Reconnection**
6. Put the phone in airplane mode for ten seconds, then restore it. The
   session must resume where it was, not blank and not duplicated.
7. Leave it disconnected long enough to produce more than 1 MiB of output
   (`yes | head -c 2000000`), then reconnect. The phone must redraw from a
snapshot rather than appending a partial stream.

**Direct path and fallback**
8. With the relay connected, move the phone to cellular data. The connection
   label may change from `connected · relay` to `connected · direct`; both are
   valid behind a restrictive carrier NAT.
9. Temporarily block UDP 443 while leaving TCP 443 available. Reconnect and
   confirm the app remains usable as `connected · relay`.

**Focus**
10. With the same terminal open on desktop and phone, type on the desktop.
11. Type on the phone: it must be refused, with a visible way to take focus.
12. Take focus on the phone. The desktop must show that it lost it.

**Lock**
13. Background the phone app for five minutes. Returning must require
    biometric or PIN before any terminal content is shown.

**Revocation**
14. With the phone connected, revoke it on the desktop. Its connection must
    drop immediately, and reconnecting must fail without a new pairing.

**Off is not revoked**
15. Turn remote access off and on again. A trusted phone must reconnect with
    no QR.

## Android native transport build

The Compose app packages a small Rust JNI library for QUIC only. A build host
needs `cargo-ndk`, the `aarch64-linux-android` Rust target, and Android NDK
27.0.12077973. Gradle builds and packages `libaiterm_quic.so` automatically:

```sh
rustup target add aarch64-linux-android
cargo install cargo-ndk --locked
cd android && ./gradlew testDebugUnitTest assembleDebug
```

## Terminal image attachments

Attachments are a remote-terminal convenience, not general-purpose phone file
access. The phone normalizes a selected camera or gallery image into a private,
metadata-free baseline JPEG before it is sent. The desktop is still the only
machine that chooses a destination path and writes to the project.

### What can be attached

- A draft may contain at most four images, each at most 12 MiB after
  normalization, with a 48 MiB combined normalized-data limit.
- Each JPEG has a longest edge no greater than 4096 pixels. The upload client
  streams ordered chunks no larger than 256 KiB, so a single image transfer
  stays well below the WebSocket's 1 MiB frame bound.
- **Gallery** uses Android's system Photo Picker. It does not request broad
  storage or media-library permission.
- **Camera** writes only to an app-private capture URI through the Android
  camera contract. It is a new picture: do not use it for unattended test
  automation that could capture private surroundings.

Chosen items appear above the composer in selection order. Remove one with its
thumbnail control, or remove all of them by discarding the draft. A fifth
selection is refused with an explanation; it never silently replaces an image.

### Submission and recovery

The Android keyboard's Go/Enter action and the terminal key bar's Enter action
take the same path. With text and attachments, AITerm uploads every image in
order and then sends one terminal input containing the text plus an
`Attached images:` list of desktop absolute paths, followed by Enter. With no
text, it sends `Please inspect the attached image(s):` plus that list. The
terminal's bracketed-paste mode is used when the terminal advertises it.

No terminal input is sent until every image has uploaded. A normalization,
storage, focus, connection, or upload failure preserves the complete draft and
shows an actionable message, so retry starts a fresh upload and removal can
target the failed item. Drafts are isolated by terminal tab and survive tab
switching and reconnecting. Beginning an upload requires the phone to own
terminal input focus; an upload already bound to that authenticated connection
may finish after focus changes, but another connection or phone cannot finish
or cancel it.

An older desktop that does not understand the additive upload protocol keeps
the draft and reports that image attachments require a desktop update.
Text-only terminal use continues normally.

### Retention and paths

| Data | Location | Retention |
| --- | --- | --- |
| Android normalized draft | App-private cache | Removed/submitted immediately, or deleted on the next cleanup opportunity after 24 hours |
| Android source snapshot | App-private cache | During normalization; stale snapshots are deleted on the next cleanup opportunity after 15 minutes |
| Android camera capture | App-private FileProvider path | Until normalization completes, or deleted on the next cleanup opportunity after 24 hours |
| Desktop partial upload | Owner-only `.part` staging file | 15 minutes, or immediate cancellation/disconnect cleanup |
| Desktop completed image | `<tab cwd>/.aiterm/attachments/` when safe, otherwise AITerm's owner-only cache | 24 hours, subject to the 256 MiB global attachment budget |

Only generated, owner-controlled paths participate in cleanup. The desktop
does not follow symlinks during staging or maintenance. In a Git worktree it
adds the generated attachment directory to that checkout's local Git exclude,
never to tracked `.gitignore`.

## Pairing-preserving Android install and test procedure

First identify the target. Do not run an unqualified ADB command when more
than one device is connected, and never target a watch.

```bash
adb devices -l
adb -s <PIXEL_SERIAL> shell getprop ro.product.device
adb -s <PIXEL_SERIAL> shell getprop ro.product.model
```

For the paired Pixel used during the 2026-08-31 verification, the serial was
`10.0.0.115:34437`, the product was `mustang`, and the model was
`Pixel_10_Pro_XL` as reported by `adb devices -l`.

Build, install, and test without disturbing the Android Keystore pairing key:

```bash
cd android
./gradlew testDebugUnitTest lintDebug assembleDebug assembleDebugAndroidTest
adb -s <PIXEL_SERIAL> install -r "$PWD/app/build/outputs/apk/debug/app-debug.apk"
adb -s <PIXEL_SERIAL> install -r -t "$PWD/app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk"
adb -s <PIXEL_SERIAL> shell am instrument -w \
  com.adroited.aiterm.test/androidx.test.runner.AndroidJUnitRunner
adb -s <PIXEL_SERIAL> shell pm path com.adroited.aiterm
```

Never uninstall or clear `com.adroited.aiterm`; both actions can destroy the
non-exportable Android Keystore key and require a fresh desktop pairing.
Never use `connectedDebugAndroidTest`, because it can choose the wrong ADB
device. When the tests are no longer needed, it is safe to remove *only* the
test package:

```bash
adb -s <PIXEL_SERIAL> uninstall com.adroited.aiterm.test
```

## Controlled attachment dogfood checklist

The desktop process serving the phone must already be the newly installed
AITerm binary. Installing an RPM does not replace or restart an active AITerm
process; restart it manually before claiming a real upload test against new
desktop code.

1. Unlock the phone normally with its biometric or PIN gate and open a paired
   remote terminal. Do not bypass this lock for testing.
2. Tap **Attach image**, choose **Gallery**, cancel the system picker, and
   verify the existing text and thumbnails stay unchanged.
3. Select one deliberately non-personal test image with text. Confirm the
   normalized thumbnail appears, upload progress only moves forward, terminal
   diffs remain visible during transfer, and the resulting terminal prompt
   contains a readable desktop path.
4. Remove the thumbnail and confirm only that draft image disappears. Add four
   mixed gallery/camera images and confirm the fifth is refused. Submit an
   attachment-only draft with the keyboard Go/Enter action.
5. With the user's physical participation only, exercise Camera once. Verify
   the capture is private, is normalized, and is gone after removal or cleanup.
   Do not automate a real camera capture.
6. Background and restore the app, switch tabs, disconnect/reconnect, and
   deliberately transfer focus. Drafts must remain tab-local; interrupted
   uploads must leave no successful terminal input and must be retryable.
7. Against an older desktop, select a controlled image and confirm the exact
   desktop-update message, with the draft retained. This is a compatibility
   check, not an upload success.
8. Use the injected-clock automated tests for 15-minute snapshot/partial and
   24-hour draft/completed-file expiry. Do not wait for real time to pass.

If a controlled media file is placed on a phone for this checklist, put it
under one uniquely named path such as
`/sdcard/Pictures/AITermTest/aiterm-verification-<date>.png`, record that exact
path, and remove only that exact file afterward. Do not browse or capture
personal gallery content in test screenshots or logs.

## 2026-08-31 verification evidence

- `git diff --check`, `npm run test:ui` (78 tests), and `npm run build`
  completed successfully.
- Rust compiled every test target with `cargo test --no-run`. Safe targets ran
  serially: 464 library tests passed (9 ignored), and the remote auth,
  desktop, operations, protocol, terminal, uploads, registry, and terminal
  screen integration suites all passed. `tests/backend.rs` was intentionally
  not executed because its fixture writes and removes data under the real
  `$HOME/.claude` and `$HOME/.codex` trees. The one known
  `remote_server` live-PTY fixture was reproduced as a bounded 45-second
  timeout; the remaining 46 `remote_server` cases passed serially.
- Android `testDebugUnitTest lintDebug assembleDebug assembleDebugAndroidTest`
  completed successfully. On the identified Pixel, both APKs were installed
  in place and the exact instrumentation package completed `OK (65 tests)` in
  65.085 seconds. The main package remained installed; it was neither
  uninstalled nor cleared.
- The controlled real-picker/upload check remains a manual post-unlock,
  post-desktop-restart step: the phone was found at its normal biometric/PIN
  lock screen, and the live desktop process intentionally was not restarted.
  No personal gallery content was opened and no camera image was captured.

## Diagnostics

Remote access logs listener lifecycle, a device id prefix, connection state,
protocol version, and the reason a connection was denied. It never logs a QR
payload, an enrollment secret, a credential, or a single byte of terminal
input or output.
