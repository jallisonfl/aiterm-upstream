# Android Remote Client Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a native Android AITerm client that securely pairs with and controls a desktop AITerm instance on a LAN or VPN.

**Architecture:** Extract desktop operations behind Rust services, then expose them through a TLS-pinned WebSocket gateway with device-key authentication. A Kotlin/Compose Android app consumes that protocol and renders a native terminal; the desktop remains the only owner of PTYs, sessions, and credentials.

**Tech Stack:** Rust 2021, Tauri 2, Tokio/Axum, rustls, CBOR/serde, Kotlin, Jetpack Compose, Android Keystore/BiometricPrompt, CameraX/ML Kit, OkHttp WebSocket, a native Kotlin terminal emulator.

**Spec:** `docs/design/2026-08-28-android-remote-client.md`

## Execution status

- Task 1 is complete: `54d9b6e`.
- Task 2 is complete: `0c29627`, hardened by `7ae5246`.
- Task 3 is complete: `779cfa7`.
- Task 4 is complete: `2fc0b9c`.
- Task 6 is complete: `fa9c4e2`, completed by `5d1dda9`.
- Task 7 is complete: `9a05126`.
- The Rust-owned-tabs and screen-diffs subplan is complete: typed protocol
  `f548773`/`50452d8`; canonical screen `9ecb344`/`5d8339f`; tab ownership
  `00dae84`/`6376fd9`/`cc18cc6`/`95aa81e`; desktop migration
  `e530e3d`/`7fb3fc3`/`bc48ce7`; and screen-diff transport
  `7fc57c4`/`83eea25`/`6d484b3`/`4a8547f`/`e28a597`/`e47bcea`/`a2bb863`.
- Its verification documentation is recorded in `88657a6`.
- Task 8 is in progress. Its untracked Android files remain outside this
  completed desktop/protocol subplan.

## Global Constraints

- Bind Remote Access only to an explicitly selected LAN/VPN address; it is disabled by default.
- Require TLS 1.3 and SHA-256 SPKI pinning from QR pairing on every Android connection.
- QR enrollment secrets are 32 random bytes, single-use, and expire after five minutes.
- Android private keys and device credentials remain in Android Keystore and require `BIOMETRIC_STRONG | DEVICE_CREDENTIAL` authentication.
- Do not send terminal data to a relay, cloud service, analytics service, or log.
- Existing Tauri commands and remote handlers must use shared service functions, not call each other.
- Never expose internal PTY ids to the remote protocol.
- Android application id is exactly `com.adroited.aiterm`; minimum SDK is 26.

---

## File structure

- `src-tauri/src/remote/mod.rs` — gateway lifecycle and Tauri commands.
- `src-tauri/src/remote/model.rs` — versioned protocol, validated requests and events.
- `src-tauri/src/remote/auth.rs` — keys, enrollment state, device trust, proof verification and revocation.
- `src-tauri/src/remote/server.rs` — TLS listener, WebSocket connection lifecycle and frame limits.
- `src-tauri/src/remote/terminal.rs` — opaque stream ids, PTY subscriptions, focus ownership and replay buffer.
- `src-tauri/src/services/*.rs` — transport-independent operations promoted from existing Tauri command modules.
- `src-tauri/tests/remote_*.rs` — protocol, pairing, authorization and terminal-stream integration tests.
- `src/components/RemoteAccessSettings.tsx` — desktop enable/pair/devices settings panel.
- `src/ipc.ts`, `src/components/SettingsModal.tsx` — desktop bindings and settings integration.
- `android/` — Gradle project for the native Android application.
- `android/app/src/main/java/com/adroited/aiterm/...` — Compose UI, pairing camera, keystore, client, terminal and view models.
- `android/app/src/test/...`, `android/app/src/androidTest/...` — Kotlin unit and Compose UI tests.

### Task 1: Establish protocol types and Rust service boundaries (complete: `54d9b6e`)

**Files:**
- Create: `src-tauri/src/remote/model.rs`
- Create: `src-tauri/src/services/mod.rs`
- Modify: `src-tauri/src/lib.rs`
- Test: `src-tauri/tests/remote_protocol.rs`

**Interfaces:**
- Produces `RemoteRequest`, `RemoteEvent`, `ProtocolError`, `PROTOCOL_VERSION`, and validated `TerminalSize`.
- Produces `services` modules consumed by both Tauri and remote handlers.

- [ ] **Step 1: Write the failing protocol-validation tests**

```rust
#[test]
fn rejects_unsupported_protocol_version() {
    let err = RemoteRequest::decode(&cbor_request(99, "session.list", b"{}")).unwrap_err();
    assert_eq!(err.code(), "protocol.unsupported_version");
}

#[test]
fn rejects_terminal_size_outside_bounds() {
    assert!(TerminalSize::try_new(0, 24).is_err());
    assert!(TerminalSize::try_new(3000, 24).is_err());
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test remote_protocol`

Expected: FAIL because `remote` protocol types do not exist.

- [ ] **Step 3: Implement the minimal serializable protocol model**

```rust
pub const PROTOCOL_VERSION: u16 = 1;
pub struct TerminalSize { pub cols: u16, pub rows: u16 }
impl TerminalSize {
    pub fn try_new(cols: u16, rows: u16) -> Result<Self, ProtocolError> { /* 1..=512, 1..=512 */ }
}
```

Add `pub mod remote;` and `pub mod services;` to `lib.rs`; use `ciborium` for envelopes and reject unknown kinds/fields deliberately.

- [ ] **Step 4: Run the focused test and full Rust test suite**

Run: `cargo test --test remote_protocol && cargo test --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/remote src-tauri/src/services src-tauri/src/lib.rs src-tauri/tests/remote_protocol.rs src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "feat(remote): add validated protocol foundation"
```

### Task 2: Build device enrollment and mutual authentication (complete: `0c29627`, `7ae5246`)

**Files:**
- Create: `src-tauri/src/remote/auth.rs`
- Test: `src-tauri/tests/remote_auth.rs`

**Interfaces:**
- Consumes `ProtocolError` from Task 1.
- Produces `DeviceStore::{begin_enrollment, approve, verify_proof, revoke}` and `EnrollmentQr`.

- [ ] **Step 1: Write failing enrollment tests**

```rust
#[test]
fn enrollment_can_be_approved_exactly_once_before_expiry() { /* begin, approve, reject repeat */ }

#[test]
fn revoked_device_proof_is_rejected() { /* pair, revoke, verify signed nonce */ }

#[test]
fn wrong_public_key_cannot_prove_device_identity() { /* same id, different key */ }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test remote_auth`

Expected: FAIL because no device store exists.

- [ ] **Step 3: Implement private trusted-device storage and enrollment state**

Use `p256`, `rand_core::OsRng`, `sha2`, and OS-private AITerm state files. Generate 32-byte enrollment secrets, compare in constant time, expire after `Duration::from_secs(300)`, and delete the secret in every terminal path. Persist device id, public key, fingerprint, display name, and timestamps; do not persist raw enrollment secrets. Revocation removes the trusted key and closes that device's active connections.

- [ ] **Step 4: Run authentication tests**

Run: `cargo test --test remote_auth`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/remote/auth.rs src-tauri/tests/remote_auth.rs src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "feat(remote): add device pairing and revocation"
```

### Task 3: Add TLS gateway and safe WebSocket framing (complete: `779cfa7`)

**Files:**
- Create: `src-tauri/src/remote/server.rs`
- Modify: `src-tauri/src/remote/mod.rs`
- Test: `src-tauri/tests/remote_server.rs`

**Interfaces:**
- Consumes `DeviceStore` and `RemoteRequest`.
- Produces `RemoteGateway::{start, stop, status}` and authenticated `RemoteConnection`.

- [ ] **Step 1: Write failing gateway tests**

```rust
#[tokio::test]
async fn websocket_rejects_connection_without_device_nonce_proof() { /* TLS test client */ }

#[tokio::test]
async fn websocket_rejects_frame_larger_than_one_mebibyte() { /* send 1 MiB + 1 */ }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test remote_server`

Expected: FAIL because the gateway is absent.

- [ ] **Step 3: Implement a disabled-by-default Rustls/Axum listener**

Generate/load an ECDSA P-256 certificate, expose only `/v1/ws`, set TLS 1.3 minimum, apply frame and rate bounds before decoding CBOR, issue nonces, call `verify_proof`, and close on failed authentication. Return the QR candidate addresses and SPKI SHA-256 fingerprint from `begin_pairing`.

- [ ] **Step 4: Run server tests**

Run: `cargo test --test remote_server`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/remote src-tauri/tests/remote_server.rs src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "feat(remote): add pinned TLS websocket gateway"
```

### Task 4: Stream desktop PTYs with opaque ids and input ownership (complete: `2fc0b9c`)

**Files:**
- Create: `src-tauri/src/remote/terminal.rs`
- Modify: `src-tauri/src/pty.rs`
- Test: `src-tauri/tests/remote_terminal.rs`

**Interfaces:**
- Consumes authenticated `RemoteConnection` and `TerminalSize`.
- Produces `TerminalBroker::{attach, input, resize, detach}` with opaque `StreamId`.

- [ ] **Step 1: Write failing terminal-broker tests**

```rust
#[test]
fn second_attached_client_cannot_write_until_it_takes_focus() { /* attach A/B; A input; B denied */ }

#[test]
fn reconnect_after_replay_window_gets_snapshot_not_partial_output() { /* overflow buffer */ }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test remote_terminal`

Expected: FAIL because no broker exists.

- [ ] **Step 3: Implement the minimal PTY event fan-out**

Add an internal PTY observer interface; preserve existing Tauri channels. Map PTYs to random stream ids, buffer a maximum of 1 MiB per stream, attach subscribers, and serialize writes through the existing PTY writer. Never serialize or transmit the numeric PTY id.

- [ ] **Step 4: Run broker tests plus existing backend tests**

Run: `cargo test --test remote_terminal && cargo test --test backend`

Expected: PASS, except any documented pre-existing failure must be reported separately.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/remote/terminal.rs src-tauri/src/pty.rs src-tauri/tests/remote_terminal.rs
git commit -m "feat(remote): stream terminals with exclusive input focus"
```

### Task 5: Route session and agent operations through shared services

**Files:**
- Create: `src-tauri/src/services/sessions.rs`
- Create: `src-tauri/src/services/agents.rs`
- Modify: `src-tauri/src/sessions.rs`, `src-tauri/src/agents.rs`, `src-tauri/src/remote/server.rs`
- Test: `src-tauri/tests/remote_operations.rs`

**Interfaces:**
- Consumes authenticated remote requests and existing session/agent models.
- Produces identical service responses for a Tauri command and a remote RPC request.

- [ ] **Step 1: Write failing parity tests**

```rust
#[test]
fn remote_session_list_matches_desktop_service_result() { /* fixture session root */ }

#[test]
fn remote_delete_rejects_an_unknown_session_without_touching_disk() { /* fixture */ }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test remote_operations`

Expected: FAIL because session functions are tied to Tauri commands.

- [ ] **Step 3: Extract services and expose only allowed remote operations**

Move pure session/agent work into `services`; keep existing command names as thin Tauri adapters. Implement remote handlers for list/preview/open/close/delete/fork/stop and existing agent actions. Explicitly return `remote.unsupported` for settings, arbitrary filesystem operations, font installation, diagnostics, and unknown commands.

- [ ] **Step 4: Run parity tests and frontend typecheck**

Run: `cargo test --test remote_operations && npm run build`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/services src-tauri/src/sessions.rs src-tauri/src/agents.rs src-tauri/src/remote/server.rs src-tauri/tests/remote_operations.rs
git commit -m "feat(remote): share desktop session services with gateway"
```

### Task 6: Add desktop Remote Access settings and QR/device controls (complete: `fa9c4e2`, `5d1dda9`)

**Files:**
- Create: `src/components/RemoteAccessSettings.tsx`
- Modify: `src/components/SettingsModal.tsx`, `src/ipc.ts`, `src-tauri/src/remote/mod.rs`, `src-tauri/src/lib.rs`
- Test: `src/components/RemoteAccessSettings.test.tsx`

**Interfaces:**
- Consumes `remoteStatus`, `remoteStart`, `remoteStop`, `remoteBeginPairing`, `remoteApproveDevice`, and `remoteRevokeDevice` IPC bindings.
- Produces the desktop controls specified in the design.

- [ ] **Step 1: Write failing UI tests**

```tsx
it("does not reveal a pairing QR until remote access is enabled", async () => { /* render + click */ });
it("asks for confirmation before revoking a trusted device", async () => { /* render + confirm */ });
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `npm run test:ui -- RemoteAccessSettings`

Expected: FAIL because the component is absent.

- [ ] **Step 3: Implement the settings panel**

Render listener status, selected address/port, certificate fingerprint, QR expiry countdown, pending enrollment approvals, trusted device name/last seen/fingerprint and explicit revoke. Keep secrets out of accessibility labels, logs, clipboard, and persisted renderer state.

- [ ] **Step 4: Run UI and build verification**

Run: `npm run test:ui && npm run build && cargo test --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/components/RemoteAccessSettings.tsx src/components/RemoteAccessSettings.test.tsx src/components/SettingsModal.tsx src/ipc.ts src-tauri/src/remote/mod.rs src-tauri/src/lib.rs
git commit -m "feat(remote): manage phone pairing from desktop settings"
```

### Task 7: Scaffold the signed native Android application (complete: `9a05126`)

**Files:**
- Create: `android/settings.gradle.kts`, `android/build.gradle.kts`, `android/app/build.gradle.kts`
- Create: `android/app/src/main/AndroidManifest.xml`
- Create: `android/app/src/main/java/com/adroited/aiterm/MainActivity.kt`
- Test: `android/app/src/test/java/com/adroited/aiterm/AppIdentityTest.kt`

**Interfaces:**
- Produces package `com.adroited.aiterm` and a Compose navigation shell.

- [ ] **Step 1: Write the failing identity test**

```kotlin
@Test fun packageName_isAdroitedAitermAndroid() {
    assertEquals("com.adroited.aiterm", BuildConfig.APPLICATION_ID)
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `./gradlew :app:testDebugUnitTest --tests '*AppIdentityTest'`

Expected: FAIL because the Android project does not exist.

- [ ] **Step 3: Create the minimal Compose app**

Use `compileSdk 37`, stable Compose BOM `2026.08.00`, CameraX, ML Kit barcode scanning, OkHttp, AndroidX Biometric, and lifecycle ViewModels. Declare Camera and Internet permissions; do not add broad storage permissions. Make a single Activity whose initial destination is the paired-desktop list.

- [ ] **Step 4: Run Android unit tests and assemble debug**

Run: `cd android && ./gradlew :app:testDebugUnitTest :app:assembleDebug`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add android
git commit -m "feat(android): scaffold native companion app"
```

### Task 8: Implement QR enrollment, pinning, and biometric lock (in progress)

**Files:**
- Create: `android/app/src/main/java/com/adroited/aiterm/pairing/PairingRepository.kt`
- Create: `android/app/src/main/java/com/adroited/aiterm/security/DeviceKeyStore.kt`
- Create: `android/app/src/main/java/com/adroited/aiterm/ui/PairingScreen.kt`
- Test: `android/app/src/test/java/com/adroited/aiterm/pairing/PairingRepositoryTest.kt`
- Test: `android/app/src/androidTest/java/com/adroited/aiterm/ui/PairingScreenTest.kt`

**Interfaces:**
- Consumes QR `aiterm://pair` payload and desktop enrollment endpoint.
- Produces an unlocked, certificate-pinned paired desktop record.

- [ ] **Step 1: Write failing tests for expiry, fingerprint mismatch, and keystore lock**

```kotlin
@Test fun expiredPairingPayload_isRejectedBeforeNetworkCall() { /* payload expired */ }
@Test fun certificatePinMismatch_doesNotPersistDesktop() { /* fake TLS peer */ }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd android && ./gradlew :app:testDebugUnitTest --tests '*PairingRepositoryTest'`

Expected: FAIL because pairing classes are absent.

- [ ] **Step 3: Implement pairing**

Scan only the versioned scheme, show the desktop name/fingerprint, pin its SPKI in OkHttp, generate a Keystore P-256 key requiring biometric/device credential authentication, submit the public key, wait for desktop approval, and store only non-secret desktop metadata outside Keystore. Lock after five minutes of backgrounding.

- [ ] **Step 4: Run unit and instrumented tests**

Run: `cd android && ./gradlew :app:testDebugUnitTest :app:connectedDebugAndroidTest`

Expected: PASS on an emulator/device with biometric test support.

- [ ] **Step 5: Commit**

```bash
git add android/app/src
git commit -m "feat(android): pair desktops with pinned device keys"
```

### Task 9: Implement remote state, WebSocket reconnection, and terminal UI

**Files:**
- Create: `android/app/src/main/java/com/adroited/aiterm/remote/RemoteClient.kt`
- Create: `android/app/src/main/java/com/adroited/aiterm/terminal/TerminalScreenStore.kt`
- Create: `android/app/src/main/java/com/adroited/aiterm/ui/TerminalScreen.kt`
- Test: `android/app/src/test/java/com/adroited/aiterm/remote/RemoteClientTest.kt`
- Test: `android/app/src/androidTest/java/com/adroited/aiterm/ui/TerminalScreenTest.kt`

**Interfaces:**
- Consumes authenticated protocol envelopes, `ScreenSnapshot`, and `ScreenDiff`.
- Produces session drawer state, typed screen replacement/application, input ownership handling, resize and snapshot recovery.

```kotlin
interface TerminalScreenStore {
    val screen: StateFlow<ScreenSnapshot?>
    fun replace(snapshot: ScreenSnapshot)
    fun apply(diff: ScreenDiff): ApplyResult
}

sealed interface ApplyResult {
    data object Applied : ApplyResult
    data object NeedsSnapshot : ApplyResult
}
```

- [ ] **Step 1: Write failing reconnection and focus tests**

```kotlin
@Test fun revisionMismatchRequestsSnapshotInsteadOfByteReplay() { /* screen diff stream */ }
@Test fun inputNotOwned_keepsTerminalReadOnlyAndShowsTakeFocusAction() { /* server error */ }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd android && ./gradlew :app:testDebugUnitTest --tests '*RemoteClientTest'`

Expected: FAIL because the remote client is absent.

- [ ] **Step 3: Implement the native client and terminal**

Decode CBOR only after connection proof succeeds. Reassemble each ordered
row-boundary screen transfer before changing local state, replace the store
from a complete `ScreenSnapshot`, and apply a `ScreenDiff` only when its base
revision matches. `NeedsSnapshot` requests recovery; neither an incomplete
transfer nor a historical PTY-byte replay may advance the screen. Render the
typed screen state, send resize/input through the broker, and provide the
extra-key row. Display focus ownership and connection state without hiding
terminal output.

- [ ] **Step 4: Run Android tests and manual interoperability smoke test**

Run: `cd android && ./gradlew :app:testDebugUnitTest :app:connectedDebugAndroidTest :app:assembleDebug`

Expected: PASS, then manually pair a physical/emulated phone with a desktop gateway and verify `printf '✓\n'`, resize, disconnect/reconnect, focus takeover, and revoke.

- [ ] **Step 5: Commit**

```bash
git add android/app/src
git commit -m "feat(android): control paired desktop sessions"
```

### Task 10: End-to-end security and release verification

**Files:**
- Create: `docs/remote/android-remote-testing.md`
- Modify: `README.md`
- Test: `src-tauri/tests/remote_e2e.rs`

**Interfaces:**
- Consumes the completed desktop gateway and Android client.
- Produces reproducible pairing/revocation/terminal test evidence.

- [ ] **Step 1: Write failing end-to-end security tests**

```rust
#[tokio::test]
async fn revoked_device_cannot_reconnect_and_active_socket_is_closed() { /* pair, connect, revoke */ }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --test remote_e2e`

Expected: FAIL until active-connection revocation is wired.

- [ ] **Step 3: Implement the smallest missing behavior and test harness**

Ensure revoke closes matching sockets, desktop shutdown closes the listener, and Android presents a clear revoked/unpaired state. Document LAN/VPN-only topology, pairing, revocation, certificate mismatch recovery, and manual test steps.

- [ ] **Step 4: Run release verification**

Run: `npm run test:ui && npm run build && cargo test --lib && cargo test --test backend && cargo test --test remote_protocol && cargo test --test remote_auth && cargo test --test remote_server && cargo test --test remote_terminal && cargo test --test remote_operations && cargo test --test remote_e2e && cd android && ./gradlew :app:testDebugUnitTest :app:assembleDebug`

Expected: PASS, with any pre-existing backend failure separately documented rather than attributed to this feature.

- [ ] **Step 5: Commit**

```bash
git add README.md docs/remote/android-remote-testing.md src-tauri/tests/remote_e2e.rs
git commit -m "docs: document Android remote access"
```

## Plan self-review

Spec coverage: Tasks 1–6 cover the desktop gateway, authentication, protocol, existing operations, terminal streams, focus, and settings. Tasks 7–9 cover native Android packaging, QR pairing, Keystore/biometrics, UI, and terminal functionality. Task 10 covers revocation, documentation, and release verification.

Placeholder scan: no implementation task uses an unspecified future action; each provides files, an interface, a failing test, a command, a concrete implementation direction, a passing command, and a commit.

Type consistency: `RemoteRequest`/`ProtocolError` feed auth/server; `DeviceStore` feeds the gateway; authenticated connections feed `TerminalBroker` and service adapters; Android consumes the versioned protocol only.
