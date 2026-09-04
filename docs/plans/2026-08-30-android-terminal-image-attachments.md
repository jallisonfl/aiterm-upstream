# Android Terminal Image Attachments Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the Android remote client attach up to four camera or gallery images to a terminal prompt through a bounded, authenticated desktop upload pipeline.

**Architecture:** Android normalizes selected content to private JPEG files, then uploads them sequentially in correlated 256 KiB requests over the existing pinned TLS WebSocket. A connection-scoped Rust upload manager authorizes the active terminal attachment, stages files beneath the server-derived tab working directory, validates and atomically publishes them, and returns absolute paths that the composer includes in one bracketed-paste submission.

**Tech Stack:** Rust 2021, Axum WebSocket gateway, serde/ciborium CBOR, SHA-256, `image` 0.25.10 with JPEG-only features, Kotlin 2.4, Android Activity Result APIs, Jetpack Compose/Material 3, coroutines, JUnit 4, Rust integration tests.

**Spec:** `docs/design/2026-08-30-android-terminal-composer-attachments.md`

## Global Constraints

- Uploads use the existing authenticated TLS 1.3 `/v1/ws`; there is no HTTP upload endpoint or bearer token.
- The server, never Android, derives the target directory and random filename.
- Input accepts exactly normalized `image/jpeg`, at most 12 MiB and 4096 pixels on the longest edge, with no more than four images/48 MiB per submission.
- Chunks are ordered, at most 256 KiB, bound to one device connection, tab, terminal attachment, and focus owner.
- Published project images live in `.aiterm/attachments/`, are locally Git-excluded, expire after 24 hours, and share a 256 MiB global budget.
- Picker input is orientation-corrected and re-encoded at JPEG quality 90; metadata is not copied.
- Submission sends nothing to the terminal until every image finishes; failures retain the Android draft.
- Installing on the paired Pixel must use `adb install -r`; never uninstall `com.adroited.aiterm`.

---

## File Structure

- `src-tauri/src/remote/uploads.rs`: upload constants, storage, authorization-independent chunk state, JPEG verification, atomic publish, manifest, and cleanup.
- `src-tauri/src/remote/model.rs`: additive upload request-kind allowlist.
- `src-tauri/src/remote/server.rs`: strict upload CBOR payloads, terminal authorization, connection-scoped upload lifetime, and responses.
- `src-tauri/tests/remote_uploads.rs`: storage/security/cleanup integration coverage.
- `src-tauri/tests/remote_server.rs`: authenticated wire-protocol coverage.
- `android/app/src/main/java/com/adroited/aiterm/remote/RemoteCommands.kt`: strict upload payload/reply CBOR.
- `android/app/src/main/java/com/adroited/aiterm/remote/RemoteClient.kt`: sequential request orchestration and progress.
- `android/app/src/main/java/com/adroited/aiterm/ui/TerminalImageNormalizer.kt`: bounded URI decode, orientation, resize, metadata-free JPEG, and private draft files.
- `android/app/src/main/java/com/adroited/aiterm/ui/TerminalImagePicker.kt`: native camera/gallery launchers and FileProvider URI lifecycle.
- `android/app/src/main/java/com/adroited/aiterm/ui/TerminalAttachmentDraft.kt`: attachment and submission state machine.
- `android/app/src/main/java/com/adroited/aiterm/ui/TerminalAttachmentStrip.kt`: thumbnails, removal, progress, and errors.
- `android/app/src/main/java/com/adroited/aiterm/ui/TerminalScreen.kt`: attachment button, picker integration, Enter behavior, and final prompt submission.

### Task 1: Build the bounded desktop attachment store

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/src/remote/mod.rs`
- Create: `src-tauri/src/remote/uploads.rs`
- Create: `src-tauri/tests/remote_uploads.rs`

**Interfaces:**
- Consumes: authoritative `TabDescriptor.cwd()`, `TabId`, `AttachmentId`, SHA-256 digest, and owner-only filesystem APIs.
- Produces: `AttachmentStore`, connection-local `UploadSet`, `UploadBegin`, `UploadBegan`, `UploadChunk`, and `PublishedUpload`.

- [ ] **Step 1: Add failing storage tests**

Create tests with unique directories beneath `std::env::temp_dir()` and explicit teardown. Cover:

```rust
#[test]
fn ordered_jpeg_chunks_publish_atomically_under_the_tab_cwd() {
    let fixture = UploadFixture::new("publish");
    let jpeg = fixture.jpeg(640, 480);
    let digest = Sha256::digest(&jpeg).into();
    let began = fixture.uploads.begin(fixture.begin(jpeg.len(), digest)).unwrap();
    for (index, chunk) in jpeg.chunks(MAX_UPLOAD_CHUNK_BYTES).enumerate() {
        fixture.uploads.chunk(&began.upload_id, index as u32, chunk).unwrap();
    }
    let published = fixture.uploads.finish(&began.upload_id).unwrap();

    assert!(published.path.starts_with(fixture.cwd.join(".aiterm/attachments")));
    assert_eq!(std::fs::read(&published.path).unwrap(), jpeg);
    assert!(!published.path.with_extension("jpg.part").exists());
}
```

Add separate tests for duplicate/out-of-order chunks, declared/actual length mismatch, digest mismatch, a non-JPEG stream, 4097-pixel dimension, 12 MiB + 1 byte declaration, symlinked `.aiterm`, unknown upload id, and cancellation deleting `.part`.

- [ ] **Step 2: Run the new integration test and verify it fails**

Run: `cd src-tauri && cargo test --test remote_uploads`

Expected: FAIL because `aiterm_lib::remote::uploads` and its types do not exist.

- [ ] **Step 3: Add the exact JPEG-only dependency**

```toml
image = { version = "=0.25.10", default-features = false, features = ["jpeg"] }
```

- [ ] **Step 4: Implement limits and public request types**

```rust
pub const MAX_UPLOAD_BYTES: u64 = 12 * 1024 * 1024;
pub const MAX_UPLOAD_CHUNK_BYTES: usize = 256 * 1024;
pub const MAX_IMAGE_EDGE: u32 = 4096;
pub const MAX_UPLOADS_PER_SUBMISSION: usize = 4;
pub const MAX_SUBMISSION_BYTES: u64 = 48 * 1024 * 1024;
pub const ATTACHMENT_TTL: Duration = Duration::from_secs(24 * 60 * 60);
pub const ATTACHMENT_BUDGET_BYTES: u64 = 256 * 1024 * 1024;

pub struct UploadBegin {
    pub tab_id: TabId,
    pub attachment_id: AttachmentId,
    pub submission_id: String,
    pub submission_count: u8,
    pub submission_bytes: u64,
    pub length: u64,
    pub sha256: [u8; 32],
}
pub struct UploadBegan { pub upload_id: String, pub next_chunk: u32 }
pub struct PublishedUpload { pub path: PathBuf }
```

- [ ] **Step 5: Implement server-derived staging and strict chunk state**

`AttachmentStore::begin` takes an optional canonical tab cwd rather than a client path. Canonicalize a present cwd before creating `.aiterm`; reject an existing `.aiterm` or `attachments` symlink; use the canonical owner-only AITerm cache when cwd is absent; create directories and `.part` files with mode `0o600`/`0o700` on Unix. `UploadSet` groups entries by submission id, validates one consistent declared count/total per group, permits at most four images/48 MiB, checks exact `next_chunk`, streams SHA-256 while writing, refuses bytes past the declaration, and removes the `.part` file on every terminal error. Completing or cancelling the group closes that submission id so it cannot be reused.

`finish` must flush and `sync_all`, compare actual length and digest, open with `image::ImageReader::with_format(..., ImageFormat::Jpeg)`, call `into_dimensions`, enforce both dimensions in `1..=4096`, decode once to prove the complete JPEG stream, require a `.jpg` destination generated from UUID v4, and atomically rename before returning.

- [ ] **Step 6: Add local Git exclusion without tracked changes**

Use `git2::Repository::discover(cwd)`, then `repo.commondir().join("info/exclude")`. Preserve existing bytes and line endings; append exactly one newline-delimited `.aiterm/attachments/` entry only when no normalized matching line exists. Failure to update the local exclude aborts project publication and removes the staged file so uploads cannot unexpectedly dirty `git status`.

- [ ] **Step 7: Run storage tests and commit**

Run: `cd src-tauri && cargo test --test remote_uploads`

Expected: all storage and validation tests PASS.

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/remote/mod.rs src-tauri/src/remote/uploads.rs src-tauri/tests/remote_uploads.rs
git commit -m "feat(remote): add bounded image attachment store"
```

### Task 2: Add manifest-backed expiry and storage-budget cleanup

**Files:**
- Modify: `src-tauri/src/remote/uploads.rs`
- Modify: `src-tauri/tests/remote_uploads.rs`

**Interfaces:**
- Consumes: successful `PublishedUpload`, AITerm cache directory, wall-clock injection.
- Produces: `AttachmentStore::maintain(now)`, atomic `attachments.json` manifest, and startup cleanup.

- [ ] **Step 1: Write failing cleanup tests**

Use an injected `Clock`/`SystemTime` argument and assert that:

```rust
#[test]
fn maintenance_removes_only_manifested_expired_generated_files() {
    let fixture = UploadFixture::new("ttl");
    let published = fixture.publish_at(SystemTime::UNIX_EPOCH);
    let unrelated = fixture.cwd.join(".aiterm/attachments/keep-me.jpg");
    std::fs::write(&unrelated, b"user file").unwrap();

    fixture.store.maintain(SystemTime::UNIX_EPOCH + ATTACHMENT_TTL + Duration::from_secs(1)).unwrap();

    assert!(!published.path.exists());
    assert!(unrelated.exists());
}
```

Add tests for abandoned `.part` entries, oldest-first 256 MiB budget eviction, corrupt manifest quarantine/rebuild without deleting arbitrary paths, and symlink replacement between publish and cleanup.

- [ ] **Step 2: Run the cleanup tests and verify they fail**

Run: `cd src-tauri && cargo test --test remote_uploads maintenance`

Expected: FAIL because persistent maintenance is not implemented.

- [ ] **Step 3: Implement an atomic private manifest**

Store versioned JSON at `<cache>/aiterm/remote-attachments/attachments.json` with generated id, canonical path, byte length, created timestamp, and state (`partial` or `complete`). Write to a sibling temporary file, `sync_all`, rename, and keep the directory owner-only. Never load a manifest path that is outside a canonical recorded `.aiterm/attachments` directory or AITerm's own canonical fallback cache.

- [ ] **Step 4: Implement startup and periodic maintenance**

Run `maintain(SystemTime::now())` when `AttachmentStore` is constructed, before each begin, after each finish, and from a 15-minute task owned by `GatewayHandle`. Delete completed records older than 24 hours, partial records older than 15 minutes, then evict oldest completed records until total recorded bytes are at most 256 MiB. Use `symlink_metadata`, refuse symlinks, and remove only UUID-named `.jpg`/`.part` files recorded in the manifest.

- [ ] **Step 5: Run tests and commit**

Run: `cd src-tauri && cargo test --test remote_uploads`

Expected: PASS.

```bash
git add src-tauri/src/remote/uploads.rs src-tauri/tests/remote_uploads.rs
git commit -m "feat(remote): expire staged terminal images"
```

### Task 3: Expose authenticated upload requests through the gateway

**Files:**
- Modify: `src-tauri/src/remote/model.rs`
- Modify: `src-tauri/src/remote/server.rs`
- Modify: `src-tauri/tests/remote_protocol.rs`
- Modify: `src-tauri/tests/remote_server.rs`

**Interfaces:**
- Consumes: `AttachmentStore`, per-connection `UploadSet`, and existing `authorize_attachment`/focus ownership.
- Produces: `terminal.upload.begin`, `.chunk`, `.finish`, and `.cancel` CBOR operations.

- [ ] **Step 1: Add strict request-kind and wire tests**

Extend the known-kind test with all four names. In `remote_server.rs`, add serializable request fixtures matching:

```rust
#[derive(Serialize)]
struct UploadBeginRequest<'a> {
    tab_id: &'a str,
    attachment_id: &'a str,
    submission_id: &'a str,
    submission_count: u8,
    submission_bytes: u64,
    length: u64,
    media_type: &'a str,
    #[serde(with = "serde_bytes")]
    sha256: &'a [u8],
}
#[derive(Serialize)]
struct UploadChunkRequest<'a> {
    upload_id: &'a str,
    index: u32,
    #[serde(with = "serde_bytes")]
    data: &'a [u8],
}
```

Test successful begin/chunk/finish, cancel, inconsistent submission metadata, fifth image, submission total above 48 MiB, reused completed submission id, unknown fields, wrong media type, non-32-byte digest, 256 KiB + 1 chunk, upload from another authenticated connection, begin without focus, focus loss before chunk/finish, and disconnect cleanup.

- [ ] **Step 2: Run focused gateway tests and verify they fail**

Run: `cd src-tauri && cargo test --test remote_protocol upload --test remote_server upload`

Expected: FAIL with `protocol.unknown_request`/missing response types.

- [ ] **Step 3: Add strict serde payloads and replies**

Define `#[serde(deny_unknown_fields)]` request structs. Begin reply is `{ upload_id, next_chunk }`; chunk and cancel reply with `{ ok: true }`; finish reply is `{ path }`. Bound upload ids and returned paths with existing protocol helpers, and keep every frame below `MAX_SCREEN_FRAME_BYTES`.

- [ ] **Step 4: Bind one upload set to one authenticated connection**

Create `Arc<std::sync::Mutex<UploadSet>>` after authentication in `authenticated_connection`. Pass it to `RemoteServices::dispatch`. On socket exit, cancel every unfinished upload before dropping the set. Do not place active upload ids in shared `RemoteServices`; this makes cross-connection resume impossible by construction.

- [ ] **Step 5: Enforce terminal ownership on every operation**

At begin, `authorize_attachment`, require `TabDescriptor.input_owner() == attachment_id`, derive cwd from `registry.get(tab_id).cwd()`, and pass that server value to the store. For chunk, finish, and cancel, resolve the upload's recorded tab/attachment and repeat attachment authorization plus current focus ownership before touching bytes. Map failures to stable codes including `terminal.input_not_owned`, `terminal.upload_too_large`, `terminal.upload_invalid_image`, `terminal.upload_out_of_order`, and `terminal.upload_not_found`.

- [ ] **Step 6: Run remote suites and commit**

Run:

```bash
cd src-tauri
cargo test --test remote_protocol
cargo test --test remote_server
cargo test --test remote_uploads
```

Expected: PASS.

```bash
git add src-tauri/src/remote/model.rs src-tauri/src/remote/server.rs src-tauri/tests/remote_protocol.rs src-tauri/tests/remote_server.rs
git commit -m "feat(remote): expose terminal image uploads"
```

### Task 4: Add Android upload codecs and sequential client orchestration

**Files:**
- Modify: `android/app/src/main/java/com/adroited/aiterm/remote/RemoteCommands.kt`
- Modify: `android/app/src/main/java/com/adroited/aiterm/remote/RemoteClient.kt`
- Modify: `android/app/src/test/java/com/adroited/aiterm/remote/RemoteWireCodecTest.kt`
- Modify: `android/app/src/test/java/com/adroited/aiterm/remote/RemoteClientTest.kt`

**Interfaces:**
- Consumes: normalized JPEG file, `[32-byte SHA-256]`, active tab/attachment, current focus, and `RemoteTransport.request`.
- Produces: `RemoteUploadSource`, `RemoteUploadProgress`, and `suspend fun RemoteClient.uploadImages(...) : Result<List<String>>`.

- [ ] **Step 1: Write exact codec tests**

Assert deterministic CBOR for begin/chunk/finish/cancel, strict response decoding, rejected unknown response fields, non-32-byte digest, overlong ids/path, and chunk payload above 256 KiB. Use the Rust field names exactly: `tab_id`, `attachment_id`, `submission_id`, `submission_count`, `submission_bytes`, `length`, `media_type`, `sha256`, `upload_id`, `next_chunk`, `index`, `data`, and `path`.

- [ ] **Step 2: Write failing client sequencing tests**

Use `FakeRemoteTransport` and two small sources. Assert request order:

```kotlin
assertEquals(
    listOf(
        "terminal.upload.begin", "terminal.upload.chunk", "terminal.upload.finish",
        "terminal.upload.begin", "terminal.upload.chunk", "terminal.upload.finish",
    ),
    transport.requests.map(RemoteRequest::kind),
)
```

Add tests for progress monotonicity, begin returning nonzero `next_chunk`, server error issuing best-effort cancel, disconnect preserving source files, inactive focus rejecting before a request, four-image/48 MiB client bounds, and paths returned in source order.

- [ ] **Step 3: Run focused tests and verify they fail**

Run:

```bash
cd android
./gradlew testDebugUnitTest --tests com.adroited.aiterm.remote.RemoteWireCodecTest --tests com.adroited.aiterm.remote.RemoteClientTest
```

Expected: FAIL because upload codecs and client API do not exist.

- [ ] **Step 4: Implement upload wire types**

```kotlin
data class RemoteUploadSource(
    val id: String,
    val file: File,
    val length: Long,
    val sha256: ByteArray,
)
data class RemoteUploadProgress(val sourceId: String, val sent: Long, val total: Long)
```

Add `RemoteCommands.uploadBegin`, `uploadChunk`, `uploadFinish`, `uploadCancel`, `uploadBegan`, and `uploadedPath`, all through the existing definite-length strict CBOR codec and `MAX_FRAME_BYTES` checks.

- [ ] **Step 5: Implement sequential, cancellable uploads**

Capture the active target and lifecycle generation under `lifecycleLock`, require `FocusOwner.Self`, generate one UUID submission id, and compute the source count/total bytes once. Include that same submission id/count/total in every begin. For each source await begin, stream 256 KiB chunks from `FileInputStream`, await each response before reading the next chunk, finish, and collect the returned path. Recheck lifecycle/target/focus between operations. On failure, best-effort cancel every upload id already begun for the submission and return `Result.failure` without sending terminal input or deleting local draft files.

- [ ] **Step 6: Run tests and commit**

Run: `cd android && ./gradlew testDebugUnitTest --tests com.adroited.aiterm.remote.RemoteWireCodecTest --tests com.adroited.aiterm.remote.RemoteClientTest`

Expected: PASS.

```bash
git add android/app/src/main/java/com/adroited/aiterm/remote/RemoteCommands.kt android/app/src/main/java/com/adroited/aiterm/remote/RemoteClient.kt android/app/src/test/java/com/adroited/aiterm/remote/RemoteWireCodecTest.kt android/app/src/test/java/com/adroited/aiterm/remote/RemoteClientTest.kt
git commit -m "feat(android): upload terminal images"
```

### Task 5: Normalize camera and gallery content privately

**Files:**
- Create: `android/app/src/main/java/com/adroited/aiterm/ui/TerminalImageNormalizer.kt`
- Create: `android/app/src/androidTest/java/com/adroited/aiterm/ui/TerminalImageNormalizerTest.kt`
- Modify: `android/app/src/main/AndroidManifest.xml`
- Create: `android/app/src/main/res/xml/terminal_image_paths.xml`

**Interfaces:**
- Consumes: Android `content://` URI and app cache directory.
- Produces: `NormalizedTerminalImage(id, file, width, height, length, sha256)` and `TerminalImageNormalizationError`.

- [ ] **Step 1: Write instrumented normalization tests**

Generate fixtures in the test cache and expose them through a test ContentProvider or FileProvider. Cover landscape resize from 6000×3000 to 4096×2048, EXIF rotation, transparent PNG conversion, no EXIF GPS tags in output, corrupt content, empty content, decoded pixel bounds, and a forced output-size violation. Assert JPEG SOI/EOI, SHA-256, private cache location, and dimensions.

- [ ] **Step 2: Build and run the instrumentation test to verify it fails**

Run:

```bash
cd android
./gradlew assembleDebug assembleDebugAndroidTest
adb install -r app/build/outputs/apk/debug/app-debug.apk
adb install -r -t app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk
adb shell am instrument -w -e class com.adroited.aiterm.ui.TerminalImageNormalizerTest com.adroited.aiterm.test/androidx.test.runner.AndroidJUnitRunner
```

Expected: FAIL because the normalizer does not exist. These installs preserve the paired main app's data.

- [ ] **Step 3: Implement bounded decode and re-encode**

On API 28+, use `ImageDecoder` with an allocator and target sample size chosen before allocation. On API 26–27, probe with `BitmapFactory.Options.inJustDecodeBounds`, calculate a power-of-two sample, decode, and apply `android.media.ExifInterface` orientation with a `Matrix`. Reject nonpositive/over-bound dimensions before a full-resolution allocation.

Scale to at most 4096 on the longest edge, draw into an RGB bitmap on a neutral `#07111B` background, and compress JPEG quality 90 into `cacheDir/terminal-image-drafts/<UUID>.jpg`. Stream SHA-256 after writing and reject/delete output outside `1..12 MiB`. Run all work on `Dispatchers.IO`; recycle intermediate bitmaps and close streams in `use` blocks.

- [ ] **Step 4: Add FileProvider confinement**

Register `androidx.core.content.FileProvider` at `${applicationId}.terminal-images`, non-exported with URI grants. `terminal_image_paths.xml` exposes only `cache-path name="terminal-image-captures" path="terminal-image-captures/"`; normalized draft files do not need to be shared outside the app.

- [ ] **Step 5: Run the normalizer tests and commit**

Repeat the manual instrumentation command. Expected: PASS.

```bash
git add android/app/src/main/AndroidManifest.xml android/app/src/main/java/com/adroited/aiterm/ui/TerminalImageNormalizer.kt android/app/src/main/res/xml/terminal_image_paths.xml android/app/src/androidTest/java/com/adroited/aiterm/ui/TerminalImageNormalizerTest.kt
git commit -m "feat(android): normalize private terminal images"
```

### Task 6: Add attachment draft and submission state

**Files:**
- Create: `android/app/src/main/java/com/adroited/aiterm/ui/TerminalAttachmentDraft.kt`
- Create: `android/app/src/test/java/com/adroited/aiterm/ui/TerminalAttachmentDraftTest.kt`
- Modify: `android/app/src/main/java/com/adroited/aiterm/ui/TerminalComposerState.kt`
- Modify: `android/app/src/test/java/com/adroited/aiterm/ui/TerminalComposerStateTest.kt`
- Modify: `android/app/src/main/java/com/adroited/aiterm/ui/RemoteTerminalViewModel.kt`

**Interfaces:**
- Consumes: `NormalizedTerminalImage`, `RemoteClient.uploadImages`, text, bracketed-paste mode, and progress callbacks.
- Produces: immutable `TerminalAttachmentDraft`, per-tab `TerminalDraftStore`, `formatTerminalSubmission(text, paths, bracketedPaste)`, and `RemoteTerminalViewModel.uploadImages`.

- [ ] **Step 1: Write failing pure state tests**

Cover add/remove in selection order, duplicate URI/image id rejection, fifth-image rejection with an explicit message, submitting/progress/failure transitions, retry preserving all items, success clearing/deleting local draft files, and independent drafts for tab A/tab B that return when each tab is reselected. Add formatting assertions:

```kotlin
assertEquals(
    listOf(
        "\u001b[200~Describe the issue\n\nAttached images:\n- /project/.aiterm/attachments/a.jpg\n- /project/.aiterm/attachments/b.jpg\u001b[201~",
        "\r",
    ),
    formatTerminalSubmission("Describe the issue", listOf(pathA, pathB), true),
)
```

Also assert attachment-only copy starts `Please inspect the attached image(s):` and a text-only draft retains current behavior.

- [ ] **Step 2: Run focused unit tests and verify they fail**

Run:

```bash
cd android
./gradlew testDebugUnitTest --tests com.adroited.aiterm.ui.TerminalAttachmentDraftTest --tests com.adroited.aiterm.ui.TerminalComposerStateTest
```

Expected: FAIL because draft and formatting APIs do not exist.

- [ ] **Step 3: Implement immutable draft transitions**

Use `TerminalAttachmentItem(image, sentBytes, state, message)` and `TerminalAttachmentDraft(items, submitting, message)`. Add `TerminalTabDraft(composer: TerminalComposerState, attachments: TerminalAttachmentDraft)` and a `TerminalDraftStore` whose `StateFlow<Map<String, TerminalTabDraft>>` is keyed only by authoritative tab id. Expose `updateComposer`, `updateAttachments`, `clear`, and `hasDrafts`; selecting another tab changes the observed key without deleting either value. Enforce four items and 48 MiB in `add`; keep filesystem deletion outside the pure state object so state tests remain deterministic.

- [ ] **Step 4: Implement submission formatting and ViewModel bridge**

`formatTerminalSubmission` produces the exact text/path list and returns `[paste, "\r"]`; bracket only the paste element. Make `RemoteTerminalViewModel` own `TerminalDraftStore` so tab switches and configuration changes preserve each draft. Add a suspend ViewModel method which maps each `NormalizedTerminalImage` to `RemoteUploadSource`, delegates to `client.uploadImages`, and forwards progress without sending input. The composable sends the formatted terminal inputs only after `Result.success(paths)`.

- [ ] **Step 5: Run tests and commit**

Run the focused unit command. Expected: PASS.

```bash
git add android/app/src/main/java/com/adroited/aiterm/ui/TerminalAttachmentDraft.kt android/app/src/main/java/com/adroited/aiterm/ui/TerminalComposerState.kt android/app/src/main/java/com/adroited/aiterm/ui/RemoteTerminalViewModel.kt android/app/src/test/java/com/adroited/aiterm/ui/TerminalAttachmentDraftTest.kt android/app/src/test/java/com/adroited/aiterm/ui/TerminalComposerStateTest.kt
git commit -m "feat(android): model terminal image drafts"
```

### Task 7: Build native picker and thumbnail UI

**Files:**
- Create: `android/app/src/main/java/com/adroited/aiterm/ui/TerminalImagePicker.kt`
- Create: `android/app/src/main/java/com/adroited/aiterm/ui/TerminalAttachmentStrip.kt`
- Modify: `android/app/src/main/java/com/adroited/aiterm/ui/TerminalScreen.kt`
- Modify: `android/app/src/androidTest/java/com/adroited/aiterm/ui/TerminalScreenTest.kt`

**Interfaces:**
- Consumes: `TerminalImageNormalizer`, `TerminalAttachmentDraft`, camera/gallery result URIs, and ViewModel upload callback.
- Produces: attachment icon tagged `terminal-add-image`, Camera/Gallery choices, thumbnail nodes `terminal-image-<id>`, remove controls, progress, and Enter submission.

- [ ] **Step 1: Add failing Compose attachment tests**

Inject a fake picker/normalizer and fake upload callback. Test icon → choice surface, gallery adding up to four thumbnails, camera result, remove, fifth-item message, upload progress disabling repeat submit, upload failure preserving text/thumbnails, successful text-plus-path terminal input, attachment-only submission, toolbar Enter dispatching submit while composer is open, tab switching restoring independent drafts, and Back asking before discarding any remaining local image draft.

Use stable tags:

```text
terminal-add-image
terminal-image-source-camera
terminal-image-source-gallery
terminal-attachments
terminal-image-<id>
terminal-image-remove-<id>
terminal-image-progress-<id>
```

- [ ] **Step 2: Build/run the screen test and verify it fails**

Use `assembleDebug`/`assembleDebugAndroidTest`, install both APKs with `adb install -r`, and run only `TerminalScreenTest` with `adb shell am instrument`. Expected: FAIL because attachment controls do not exist.

- [ ] **Step 3: Implement native launchers**

`rememberTerminalImagePicker` owns `ActivityResultContracts.PickMultipleVisualMedia(maxItems = remainingSlots)` and `TakePicture`. Before camera launch, create a unique file only under `cacheDir/terminal-image-captures`, obtain its FileProvider URI, and grant through the contract. Delete capture files on cancellation and after normalization. Picker cancellation must not mutate the draft.

- [ ] **Step 4: Implement thumbnail strip and source choice**

Use a Material 3 modal bottom sheet or alert choice surface for Camera/Gallery. Render a horizontally scrolling thumbnail strip above the compact field, with fixed 64 dp previews, an accessible remove button, progress overlay, and one concise inline error. Decode preview bitmaps to the displayed size rather than holding full-resolution images in Compose.

- [ ] **Step 5: Connect Enter and atomic submission**

`RemoteTerminalScreen` collects `viewModel.drafts.drafts`, selects the entry for `screen.tabId`, and passes the immutable tab draft plus `updateComposer`/`updateAttachments` callbacks into `TerminalScreenContent`; Compose tests pass a map-backed test store through the same parameters. The IME Go callback and terminal-key Enter callback call the same guarded coroutine. If there are attachments, upload all first, format text plus returned paths, send both outbound inputs in order, then delete local normalized files and clear/close the composer. On failure, leave text, images, and composer open. When the composer is not open or has no draft, terminal-key Enter continues to send raw `\r`. Route Back through `TerminalDraftStore.hasDrafts`; confirm `Discard drafts and leave` before deleting normalized files and navigating, while `Keep editing` stays on the terminal.

- [ ] **Step 6: Run Android tests and commit**

Run:

```bash
cd android
./gradlew testDebugUnitTest lintDebug assembleDebug assembleDebugAndroidTest
adb install -r app/build/outputs/apk/debug/app-debug.apk
adb install -r -t app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk
adb shell am instrument -w -e class com.adroited.aiterm.ui.TerminalScreenTest com.adroited.aiterm.test/androidx.test.runner.AndroidJUnitRunner
```

Expected: all commands PASS and pairing data remains intact.

```bash
git add android/app/src/main/java/com/adroited/aiterm/ui/TerminalImagePicker.kt android/app/src/main/java/com/adroited/aiterm/ui/TerminalAttachmentStrip.kt android/app/src/main/java/com/adroited/aiterm/ui/TerminalScreen.kt android/app/src/androidTest/java/com/adroited/aiterm/ui/TerminalScreenTest.kt
git commit -m "feat(android): attach photos to terminal prompts"
```

### Task 8: Full verification and paired-phone dogfood

**Files:**
- Modify if required by verified defects: files owned by Tasks 1–7 only.
- Update: `docs/remote/android-remote-testing.md`

**Interfaces:**
- Consumes: completed desktop upload protocol and Android attachment UI.
- Produces: reproducible deployment/testing instructions and a verified in-place phone build.

- [ ] **Step 1: Run repository verification**

```bash
npm run test:ui
npm run build
cd src-tauri && cargo test
cd ../android && ./gradlew testDebugUnitTest lintDebug assembleDebug assembleDebugAndroidTest
```

Expected: every command exits 0.

- [ ] **Step 2: Run manual Android instrumentation without uninstalling the app**

```bash
cd android
adb install -r app/build/outputs/apk/debug/app-debug.apk
adb install -r -t app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk
adb shell am instrument -w com.adroited.aiterm.test/androidx.test.runner.AndroidJUnitRunner
adb uninstall com.adroited.aiterm.test
```

Expected: `OK`; only the test package is removed. Confirm `adb shell pm path com.adroited.aiterm` still returns the installed main package.

- [ ] **Step 3: Dogfood every approved path**

On the paired Pixel and current desktop, verify one gallery image with text, one camera image with text, four mixed images, attachment-only Enter, item removal, fifth-item refusal, picker cancellation, disconnect/retry, focus loss, older-desktop unsupported response, app background/restore, and 24-hour cleanup using an injected test clock. Confirm paths are readable by Codex and absent from `git status`.

- [ ] **Step 4: Verify resource and security bounds**

Observe that UI remains responsive during 4096-pixel normalization, upload progress is monotonic, terminal diffs continue during upload, no WebSocket frame reaches 1 MiB, no `.part` remains after cancellation/disconnect, and another paired connection cannot finish an upload id.

- [ ] **Step 5: Document the in-place workflow and commit**

Add camera/gallery cases, cache/TTL locations, failure recovery, and the exact pairing-preserving ADB commands to `docs/remote/android-remote-testing.md`.

```bash
git add docs/remote/android-remote-testing.md
git commit -m "docs: cover Android terminal image testing"
```
