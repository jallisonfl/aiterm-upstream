# Android Terminal Composer, Attachments, and IME Design

## Goal

Make the Android remote terminal feel native while typing, simplify its input
model, make the terminal-key bar collapsible, and let a paired phone attach up
to four camera or gallery images to a terminal prompt. The desktop remains the
authoritative terminal and filesystem host, and image transfer uses the same
authenticated TLS WebSocket connection as terminal traffic.

## Scope

This change covers the Android terminal composer and terminal-key bar, Android
IME/inset behavior, a bounded image-upload extension to the remote protocol,
desktop-side staging and cleanup, and end-to-end submission of text plus image
paths to the active terminal.

It does not add a cloud relay, general-purpose file browser, arbitrary phone
file upload, desktop-to-phone download, agent-specific attachment API, or a
second terminal-input mode. Images are delivered to terminal applications as
desktop-readable absolute paths, so the feature works with Codex, Claude, and
ordinary shells rather than depending on one agent's private protocol.

## Composer behavior

The composer has one Android-native text mode. The Direct toggle and its
immediate-key path are removed. Autocorrect, sentence capitalization, and the
keyboard's Go/Enter action remain enabled. The existing terminal-key bar is the
place for raw control keys, cursor sequences, Escape, Tab, and modifiers.

An image icon in the composer opens a small choice surface containing Camera
and Gallery. Camera launches Android's system camera capture contract into an
app-private content URI. Gallery uses Android's system Photo Picker and does
not request broad media-library access. The existing camera permission remains
available for QR scanning; capture must not introduce storage permission.

Selected images appear as removable thumbnails above the text field in
selection order. The composer accepts at most four attachments and explains
the limit rather than silently replacing an existing item. Attachments remain
local drafts until submission and survive a failed upload. Changing tabs keeps
drafts isolated by tab; closing the composer retains its draft, while leaving
the remote-terminal destination may discard drafts after confirmation if
necessary.

The keyboard Go/Enter action submits the current text and attachments. The
terminal-key bar's Enter button submits the composer when the composer is open
and contains a draft; otherwise it sends a raw carriage return exactly as it
does today. An attachment-only submission is valid. There is no separate Send
or Direct button.

Submission is atomic from the user's perspective:

1. Disable repeat submission and show per-image upload progress.
2. Upload every pending attachment in selection order.
3. If any upload fails, send no terminal input, preserve the entire draft, and
   offer retry or removal of the failed item.
4. Once every upload succeeds, create one terminal input containing the user's
   text followed by an `Attached images:` list of absolute desktop paths. If
   there is no user text, use `Please inspect the attached image(s):`.
5. Send the content as bracketed paste when the terminal advertises bracketed
   paste mode, then send carriage return as a separate terminal input.
6. Clear the submitted draft only after the upload completions and terminal
   input requests have been accepted locally.

## Image normalization

Android treats picker URIs as untrusted, short-lived input. It reads through a
`ContentResolver`, applies the encoded orientation, bounds decode allocation,
scales the image so its longest edge is no more than 4096 pixels and its total
pixel count stays within the corresponding bounded area, composites
transparency onto a neutral background, and encodes a new JPEG at quality 90.
Re-encoding strips EXIF metadata, including location and device details, and
gives the desktop one predictable input format.

Each normalized image must be non-empty and no larger than 12 MiB. At most four
images and 48 MiB of normalized draft data may be pending for one submission.
Normalization runs off the main thread and is cancellable. Temporary Android
files are private to the application and are deleted when the draft is removed,
submitted, or expired.

## Remote upload protocol

Image bytes use additive version-1 request kinds on the authenticated `/v1/ws`
connection:

- `terminal.upload.begin` identifies the active `tab_id`, `attachment_id`, a
  random submission id shared by this draft's images, declared submission
  image count and total bytes, normalized image byte length, JPEG media type,
  and SHA-256 digest. A successful response returns an opaque upload id and the
  required next chunk index. Submission metadata lets the server independently
  enforce the four-image/48 MiB boundary rather than trusting the UI.
- `terminal.upload.chunk` carries the opaque upload id, monotonically
  increasing chunk index, and at most 256 KiB of bytes.
- `terminal.upload.finish` verifies length, digest, image format, and dimensions,
  atomically publishes the staged file, and returns its absolute desktop path.
- `terminal.upload.cancel` removes an unfinished upload and is best effort on
  disconnect or user cancellation.

Each operation is an ordinary correlated request/response and remains well
below the existing 1 MiB wire-frame limit. The phone sends one chunk at a time,
so connection backpressure bounds memory and a terminal screen transfer cannot
be buried behind an unbounded queue of image frames.

All four operations authorize the original terminal attachment. Begin also
requires that attachment to own terminal focus. Upload state is bound to the
authenticated device, connection, tab, and attachment; another device or
connection cannot resume or finish it. The server chooses every path and file
name. The client cannot supply a directory, extension, or path component.

The desktop enforces the same per-file and per-submission limits independently,
requires exact chunk order, rejects extra or duplicate bytes, verifies SHA-256,
recognizes a complete JPEG stream, and rejects dimensions above the normalized
limits. Incomplete files use owner-only permissions and a `.part` suffix. A
successful finish flushes and atomically renames the file before returning its
path. Protocol errors remove the staged upload without affecting the terminal
or other transfers.

## Desktop staging and cleanup

For a tab with an authoritative working directory, the server stages images
under:

```text
<tab cwd>/.aiterm/attachments/<random-id>.jpg
```

The working directory comes only from the Rust tab registry. It is canonicalized
and revalidated before creation so symlinks or a replaced directory cannot
escape the intended project. The server creates directories and files with
owner-only permissions and collision-resistant random identifiers.

For a Git worktree, AITerm adds `.aiterm/attachments/` to that checkout's local
Git exclude file when necessary. It never edits tracked `.gitignore` content.
For a tab without a known project directory, the file goes into an owner-only
AITerm cache directory and the returned absolute path remains usable by the
shell.

Completed attachments remain for 24 hours so an agent can inspect them after
the prompt returns or the phone disconnects. Startup and periodic maintenance
delete expired files and abandoned `.part` uploads. A global 256 MiB attachment
budget removes the oldest expired or completed attachment first; active uploads
are cancelled rather than allowing the budget to be exceeded. Cleanup never
follows symlinks and only removes files matching AITerm's generated layout.

## Terminal-key bar and system insets

The horizontally scrolling terminal-key bar gains a chevron control. Expanded
shows all existing keys; collapsed shows only a thin, clearly tappable restore
strip. The choice is global, stored in private Android preferences, and restored
across process launches and every paired desktop. Collapsing does not discard a
composer draft or change terminal focus.

The bar's background extends through Android's navigation/gesture inset. Its
interactive content still observes the safe inset, so the current blank-looking
area becomes visually continuous without placing controls under the system
gesture target. Scaffold content insets must have one owner to prevent the
navigation inset from being applied twice.

## Native-speed IME behavior

The current screen applies `imePadding()` to the entire terminal column. During
keyboard animation that repeatedly lays out the terminal, recalculates rows,
and sends a PTY resize for intermediate heights. The new layout separates the
stable terminal viewport from bottom chrome:

- The terminal surface does not receive whole-column IME padding.
- Composer and terminal-key chrome are bottom overlays that consume Android's
  animated IME and navigation-bar insets directly, so they track the platform
  animation without a second custom animation.
- The terminal is locally clipped/covered during intermediate inset frames;
  it does not wait for desktop screen diffs to make each animation frame.
- Terminal row/column measurement is observed separately and distinct sizes
  are coalesced after a short 150 ms settling window. Only the final stable size
  is sent to `terminal.resize`.
- Composer state and thumbnail/progress updates are isolated from the terminal
  grid so ordinary typing does not recompose terminal rows.

Opening or closing the keyboard, composer, or key bar may therefore produce one
authoritative PTY resize after motion settles, rather than dozens during the
animation. Focus acquisition continues to include the final measured size.

## Failure and compatibility behavior

Picker cancellation leaves the draft unchanged. Decode, normalization, upload,
focus-loss, connection-loss, storage-limit, and cleanup errors use specific
messages near the affected attachment. A reconnect does not pretend a partial
upload succeeded; the draft remains on the phone and retries from a fresh
`begin` request.

An older desktop that rejects the additive upload request reports that image
attachments require a desktop update. Text-only terminal operation continues
unchanged. Unknown or unsolicited upload responses are handled by the existing
strict correlated-response rules.

## Testing and verification

Rust tests cover strict CBOR payloads, request authorization, focus ownership,
path derivation, canonicalization, local Git exclusion, chunk order, size and
pixel limits, digest mismatch, atomic publication, disconnect cleanup, TTL
cleanup, storage budget, and symlink/path traversal attempts.

Android unit tests cover normalization bounds, metadata-free JPEG output,
attachment draft transitions, the four-item limit, upload ordering, retry,
attachment-only prompts, bracketed-paste formatting, and resize coalescing.
Compose tests cover Direct removal, picker actions, thumbnails and removal,
disabled/progress states, keyboard and toolbar submission, collapsed-bar
persistence, and the absence of duplicated bottom inset space.

The full Rust, web, Android unit, lint, and build suites run before deployment.
The debug APK is installed on the paired Pixel with `adb install -r`; the main
application is never uninstalled, so its non-exportable Android Keystore key
and desktop pairing survive. Final dogfood verification covers camera, gallery,
four images, retry after disconnect, keyboard motion, bar collapse, and text-
only regression behavior on the physical phone.
