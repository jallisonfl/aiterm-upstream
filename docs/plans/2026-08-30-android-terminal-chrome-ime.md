# Android Terminal Chrome and IME Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Simplify Android terminal text input, add a persistent collapsible terminal-key bar, remove the bottom inset gap, and make keyboard transitions track Android natively without resize storms.

**Architecture:** Keep the terminal viewport as a stable surface and move composer/key chrome in a bottom overlay driven by platform IME/navigation insets. Store one global key-bar preference, isolate composer state from terminal rendering, and debounce measured PTY sizes for 150 ms before sending one authoritative resize.

**Tech Stack:** Kotlin 2.4, Jetpack Compose/Material 3, Android SharedPreferences, Kotlin coroutines Flow, JUnit 4, Compose instrumentation tests.

**Spec:** `docs/design/2026-08-30-android-terminal-composer-attachments.md`

## Global Constraints

- Android application id remains `com.adroited.aiterm`; minimum SDK remains 26.
- There is one Android-native composer mode with autocorrect, sentence capitalization, and IME Go enabled.
- There is no Direct or Send button; raw terminal keys remain in the terminal-key bar.
- The key-bar expanded state is global and survives app process restarts.
- System gesture/navigation space must share the key-bar background while controls remain outside the unsafe inset.
- Only stable, distinct terminal sizes are sent after a 150 ms settling window.
- Installing on the paired Pixel must use `adb install -r`; never uninstall `com.adroited.aiterm`.

---

## File Structure

- `android/app/src/main/java/com/adroited/aiterm/ui/TerminalComposerState.kt`: one-mode text draft and terminal submission formatting.
- `android/app/src/main/java/com/adroited/aiterm/ui/TerminalKeyBarPreference.kt`: persistent global expanded/collapsed state.
- `android/app/src/main/java/com/adroited/aiterm/ui/TerminalResizeFlow.kt`: testable 150 ms distinct-size coalescing.
- `android/app/src/main/java/com/adroited/aiterm/ui/TerminalScreen.kt`: stable terminal surface, bottom chrome overlay, and key-bar controls.
- `android/app/src/main/java/com/adroited/aiterm/AppContainer.kt`: process-scoped preference construction.
- `android/app/src/main/java/com/adroited/aiterm/ui/AitermApp.kt`: passes the preference to the terminal route.
- Corresponding unit and Compose test files own behavioral regressions.

### Task 1: Remove Direct mode and keep one text composer

**Files:**
- Modify: `android/app/src/main/java/com/adroited/aiterm/ui/TerminalComposerState.kt`
- Modify: `android/app/src/main/java/com/adroited/aiterm/ui/TerminalScreen.kt`
- Modify: `android/app/src/test/java/com/adroited/aiterm/ui/TerminalComposerStateTest.kt`
- Modify: `android/app/src/androidTest/java/com/adroited/aiterm/ui/TerminalScreenTest.kt`

**Interfaces:**
- Consumes: existing `TerminalComposerUpdate(state, outbound)` and `ScreenSnapshot.modes.bracketedPaste`.
- Produces: `TerminalComposerState.value`, `updateValue(TextFieldValue)`, and `sendText(Boolean)` with no mode flag.

- [ ] **Step 1: Replace the direct-mode unit test with a one-mode draft test**

```kotlin
@Test
fun composerHasOneAutocorrectableTextDraft() {
    val typed = TerminalComposerState().open()
        .updateValue(TextFieldValue("correct this"))

    assertEquals("correct this", typed.state.value.text)
    assertEquals(emptyList<String>(), typed.outbound)
    assertTrue(typed.state.expanded)
}
```

Update the other assertions from `visibleValue` to `value` and remove every assertion about `direct`.

- [ ] **Step 2: Run the focused unit test and verify it fails**

Run: `cd android && ./gradlew testDebugUnitTest --tests com.adroited.aiterm.ui.TerminalComposerStateTest`

Expected: FAIL because `TerminalComposerState.value` does not exist and Direct-specific state still exists.

- [ ] **Step 3: Simplify the composer state**

```kotlin
internal data class TerminalComposerState(
    val expanded: Boolean = false,
    val value: TextFieldValue = TextFieldValue(),
) {
    fun open() = copy(expanded = true)
    fun close() = copy(expanded = false)
    fun updateValue(next: TextFieldValue) = TerminalComposerUpdate(copy(value = next))

    fun sendText(bracketedPaste: Boolean = false): TerminalComposerUpdate {
        val outbound = buildList {
            if (value.text.isNotEmpty()) {
                add(if (bracketedPaste) "\u001b[200~${value.text}\u001b[201~" else value.text)
            }
            add("\r")
        }
        return TerminalComposerUpdate(copy(expanded = false, value = TextFieldValue()), outbound)
    }
}
```

- [ ] **Step 4: Remove Direct UI and hardware-mode branches**

Delete `direct`, `onToggleDirect`, and `onDirectKey` from `TerminalInputBar`; delete `InputModeButton`; set `KeyboardOptions` unconditionally to `Sentences`, autocorrect enabled, `Text`, and `ImeAction.Go`. Remove the Direct placeholder and `onPreviewKeyEvent` branch. Bind the field to `composer.value`.

- [ ] **Step 5: Update the Compose regression tests**

Delete `directModeSendsCommittedTextImmediately` and `directModeForwardsHardwareTerminalKeys`. In `composerFloatsOverTheTerminalWithoutCoveringItsRenderArea`, remove the Direct half and add:

```kotlin
compose.onNodeWithTag("input-mode-direct").assertDoesNotExist()
assertTrue(kotlin.math.abs(field.center.y - placeholder.center.y) < 2f)
```

- [ ] **Step 6: Run unit and instrumentation tests**

Run:

```bash
cd android
./gradlew testDebugUnitTest --tests com.adroited.aiterm.ui.TerminalComposerStateTest
./gradlew assembleDebug assembleDebugAndroidTest
adb install -r app/build/outputs/apk/debug/app-debug.apk
adb install -r -t app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk
adb shell am instrument -w -e class com.adroited.aiterm.ui.TerminalScreenTest com.adroited.aiterm.test/androidx.test.runner.AndroidJUnitRunner
```

Expected: unit tests and instrumentation PASS while the paired main package remains installed in place.

- [ ] **Step 7: Commit**

```bash
git add android/app/src/main/java/com/adroited/aiterm/ui/TerminalComposerState.kt android/app/src/main/java/com/adroited/aiterm/ui/TerminalScreen.kt android/app/src/test/java/com/adroited/aiterm/ui/TerminalComposerStateTest.kt android/app/src/androidTest/java/com/adroited/aiterm/ui/TerminalScreenTest.kt
git commit -m "refactor(android): remove direct composer mode"
```

### Task 2: Persist and render the collapsible terminal-key bar

**Files:**
- Create: `android/app/src/main/java/com/adroited/aiterm/ui/TerminalKeyBarPreference.kt`
- Create: `android/app/src/androidTest/java/com/adroited/aiterm/ui/TerminalKeyBarPreferenceTest.kt`
- Modify: `android/app/src/main/java/com/adroited/aiterm/AppContainer.kt`
- Modify: `android/app/src/main/java/com/adroited/aiterm/ui/AitermApp.kt`
- Modify: `android/app/src/main/java/com/adroited/aiterm/ui/TerminalScreen.kt`
- Modify: `android/app/src/androidTest/java/com/adroited/aiterm/ui/TerminalScreenTest.kt`

**Interfaces:**
- Consumes: Android private `SharedPreferences` and existing `ExtraKeys` actions.
- Produces: `TerminalKeyBarPreference.expanded: StateFlow<Boolean>` and `setExpanded(Boolean)`; `RemoteTerminalScreen(..., keyBarPreference)`.

- [ ] **Step 1: Write a failing preference persistence test**

```kotlin
@Test
fun expandedChoiceIsRestoredByTheNextPreferenceInstance() {
    val context = InstrumentationRegistry.getInstrumentation().targetContext
    val preferences = context.getSharedPreferences("terminal-key-bar-test", Context.MODE_PRIVATE)
    preferences.edit().clear().commit()
    try {
        TerminalKeyBarPreference(preferences).setExpanded(false)

        assertFalse(TerminalKeyBarPreference(preferences).expanded.value)
    } finally {
        preferences.edit().clear().commit()
    }
}
```

- [ ] **Step 2: Run the focused test and verify it fails**

Run:

```bash
cd android
./gradlew assembleDebug assembleDebugAndroidTest
adb install -r app/build/outputs/apk/debug/app-debug.apk
adb install -r -t app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk
adb shell am instrument -w -e class com.adroited.aiterm.ui.TerminalKeyBarPreferenceTest com.adroited.aiterm.test/androidx.test.runner.AndroidJUnitRunner
```

Expected: FAIL because `TerminalKeyBarPreference` does not exist.

- [ ] **Step 3: Implement the global preference**

```kotlin
class TerminalKeyBarPreference(private val preferences: SharedPreferences) {
    constructor(context: Context) : this(
        context.getSharedPreferences("terminal_ui", Context.MODE_PRIVATE),
    )

    private val mutableExpanded = MutableStateFlow(
        preferences.getBoolean(EXPANDED_KEY, true),
    )
    val expanded: StateFlow<Boolean> = mutableExpanded.asStateFlow()

    fun setExpanded(expanded: Boolean) {
        if (!preferences.edit().putBoolean(EXPANDED_KEY, expanded).commit()) return
        mutableExpanded.value = expanded
    }

    private companion object { const val EXPANDED_KEY = "extra_keys_expanded" }
}
```

- [ ] **Step 4: Wire the preference from process scope to the terminal screen**

Add `val terminalKeyBarPreference = TerminalKeyBarPreference(context.applicationContext)` to `AppContainer`. Pass it through `AitermApp` into `RemoteTerminalScreen`, collect `expanded` with lifecycle awareness, and add `keyBarExpanded` / `onKeyBarExpandedChange` parameters to `TerminalScreenContent` with test-friendly defaults.

- [ ] **Step 5: Write failing Compose tests for collapse and restore controls**

```kotlin
@Test
fun terminalKeyBarCollapsesToARestoreStrip() {
    val expanded = mutableStateOf(true)
    compose.setContent {
        TerminalScreenContent(
            state = connectedState(),
            screen = oneCellScreen("tab-keys"),
            keyBarExpanded = expanded.value,
            onKeyBarExpandedChange = { expanded.value = it },
        )
    }

    compose.onNodeWithTag("collapse-extra-keys").performClick()
    compose.onNodeWithText("Esc").assertDoesNotExist()
    compose.onNodeWithTag("expand-extra-keys").assertIsDisplayed().performClick()
    compose.onNodeWithText("Esc").assertIsDisplayed()
}
```

Use local test fixtures rather than adding production helpers solely for tests.

- [ ] **Step 6: Implement expanded and collapsed key-bar surfaces**

Keep the existing horizontally scrolling key row when expanded and add a trailing chevron tagged `collapse-extra-keys`. When collapsed, render a full-width 28 dp restore strip tagged `expand-extra-keys` with an upward chevron and content description `Show terminal keys`. Give both states the same `surfaceVariant` background.

- [ ] **Step 7: Run tests and commit**

Run:

```bash
cd android
./gradlew assembleDebug assembleDebugAndroidTest
adb install -r app/build/outputs/apk/debug/app-debug.apk
adb install -r -t app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk
adb shell am instrument -w -e class com.adroited.aiterm.ui.TerminalKeyBarPreferenceTest,com.adroited.aiterm.ui.TerminalScreenTest com.adroited.aiterm.test/androidx.test.runner.AndroidJUnitRunner
```

Expected: PASS.

```bash
git add android/app/src/main/java/com/adroited/aiterm/AppContainer.kt android/app/src/main/java/com/adroited/aiterm/ui/AitermApp.kt android/app/src/main/java/com/adroited/aiterm/ui/TerminalKeyBarPreference.kt android/app/src/main/java/com/adroited/aiterm/ui/TerminalScreen.kt android/app/src/androidTest/java/com/adroited/aiterm/ui/TerminalKeyBarPreferenceTest.kt android/app/src/androidTest/java/com/adroited/aiterm/ui/TerminalScreenTest.kt
git commit -m "feat(android): add collapsible terminal key bar"
```

### Task 3: Coalesce terminal resize measurements

**Files:**
- Create: `android/app/src/main/java/com/adroited/aiterm/ui/TerminalResizeFlow.kt`
- Create: `android/app/src/test/java/com/adroited/aiterm/ui/TerminalResizeFlowTest.kt`
- Modify: `android/app/src/main/java/com/adroited/aiterm/ui/TerminalScreen.kt`

**Interfaces:**
- Consumes: `Flow<TerminalSize>` and the existing `onResize(cols, rows)` callback.
- Produces: `internal fun Flow<TerminalSize>.settledTerminalSizes(): Flow<TerminalSize>` using a 150 ms debounce and distinct sizes.

- [ ] **Step 1: Write virtual-time tests for burst and distinct behavior**

```kotlin
@Test
fun rapidMeasurementsPublishOnlyTheFinalStableSize() = runTest {
    val source = MutableSharedFlow<TerminalSize>(extraBufferCapacity = 8)
    val seen = mutableListOf<TerminalSize>()
    backgroundScope.launch(UnconfinedTestDispatcher(testScheduler)) {
        source.settledTerminalSizes().toList(seen)
    }

    source.tryEmit(TerminalSize(80, 24))
    advanceTimeBy(50)
    source.tryEmit(TerminalSize(80, 18))
    advanceTimeBy(50)
    source.tryEmit(TerminalSize(80, 12))
    advanceTimeBy(149)
    assertTrue(seen.isEmpty())
    advanceTimeBy(1)
    assertEquals(listOf(TerminalSize(80, 12)), seen)
}
```

Add a second test that emits the same stable size twice and observes it once.

- [ ] **Step 2: Run the focused test and verify it fails**

Run: `cd android && ./gradlew testDebugUnitTest --tests com.adroited.aiterm.ui.TerminalResizeFlowTest`

Expected: FAIL because `settledTerminalSizes` does not exist.

- [ ] **Step 3: Implement the flow boundary**

```kotlin
internal const val TERMINAL_RESIZE_SETTLE_MILLIS = 150L

@OptIn(FlowPreview::class)
internal fun Flow<TerminalSize>.settledTerminalSizes(): Flow<TerminalSize> =
    distinctUntilChanged().debounce(TERMINAL_RESIZE_SETTLE_MILLIS)
```

- [ ] **Step 4: Replace per-frame `LaunchedEffect` resize dispatch**

Inside the terminal measurement scope, keep immediate `cols` and `rows` state for opening sessions, but collect measured `TerminalSize` through `snapshotFlow`, `settledTerminalSizes()`, and call `onResize` only for a non-null screen. Key the collection by `screen?.tabId` so changing attachments cannot publish the previous tab's pending size.

- [ ] **Step 5: Add a Compose resize-storm regression**

Drive a mutable container through at least ten height values without advancing the Compose test clock past 150 ms, then advance it and assert `onResize` received one final distinct size. Retain the rotation tests and adapt their waits to the settling delay.

- [ ] **Step 6: Run focused tests and commit**

Run:

```bash
cd android
./gradlew testDebugUnitTest --tests com.adroited.aiterm.ui.TerminalResizeFlowTest
./gradlew assembleDebug assembleDebugAndroidTest
adb install -r app/build/outputs/apk/debug/app-debug.apk
adb install -r -t app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk
adb shell am instrument -w -e class com.adroited.aiterm.ui.TerminalScreenTest com.adroited.aiterm.test/androidx.test.runner.AndroidJUnitRunner
```

Expected: PASS with one callback for the artificial resize burst.

```bash
git add android/app/src/main/java/com/adroited/aiterm/ui/TerminalResizeFlow.kt android/app/src/main/java/com/adroited/aiterm/ui/TerminalScreen.kt android/app/src/test/java/com/adroited/aiterm/ui/TerminalResizeFlowTest.kt android/app/src/androidTest/java/com/adroited/aiterm/ui/TerminalScreenTest.kt
git commit -m "perf(android): coalesce terminal resizes"
```

### Task 4: Move bottom chrome with native insets and remove the gap

**Files:**
- Modify: `android/app/src/main/java/com/adroited/aiterm/ui/TerminalScreen.kt`
- Modify: `android/app/src/androidTest/java/com/adroited/aiterm/ui/TerminalScreenTest.kt`

**Interfaces:**
- Consumes: Compose `WindowInsets.ime`, `WindowInsets.navigationBars`, stable terminal measurements, and the persisted key-bar state.
- Produces: terminal content tagged `terminal-surface` and bottom overlay tagged `terminal-bottom-chrome`, with one owner for system insets.

- [ ] **Step 1: Write failing inset/layout assertions**

Add tests which open the IME and assert:

```kotlin
val surfaceBefore = compose.onNodeWithTag("terminal-surface").fetchSemanticsNode().boundsInRoot
// Open composer and wait for WindowInsets.Type.ime() visibility.
val surfaceDuring = compose.onNodeWithTag("terminal-surface").fetchSemanticsNode().boundsInRoot
assertEquals(surfaceBefore.top, surfaceDuring.top, 1f)
assertEquals(surfaceBefore.bottom, surfaceDuring.bottom, 1f)

val chrome = compose.onNodeWithTag("terminal-bottom-chrome").fetchSemanticsNode().boundsInRoot
assertTrue(chrome.bottom >= compose.activity.window.decorView.height.toFloat())
```

Also preserve the existing assertion that the composer sits above the IME. Compare colors or bounds so no separate Scaffold bottom spacer exists below the chrome background.

- [ ] **Step 2: Run the instrumentation class and verify the new test fails**

Run:

```bash
cd android
./gradlew assembleDebug assembleDebugAndroidTest
adb install -r app/build/outputs/apk/debug/app-debug.apk
adb install -r -t app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk
adb shell am instrument -w -e class com.adroited.aiterm.ui.TerminalScreenTest com.adroited.aiterm.test/androidx.test.runner.AndroidJUnitRunner
```

Expected: FAIL because whole-column `imePadding()` changes the terminal surface and Scaffold owns a second bottom inset.

- [ ] **Step 3: Restructure the Scaffold content**

Set `contentWindowInsets = WindowInsets(0, 0, 0, 0)` on `Scaffold`. Apply its top-bar content padding once. Replace the root padded `Column` with a stable `Box` containing:

```kotlin
Column(Modifier.fillMaxSize().testTag("terminal-surface")) {
    ConnectionRail(state, onReconnect)
    TerminalViewport(Modifier.weight(1f), ...)
}

Column(
    Modifier.align(Alignment.BottomCenter)
        .fillMaxWidth()
        .background(MaterialTheme.colorScheme.surfaceVariant)
        .windowInsetsPadding(WindowInsets.ime.union(WindowInsets.navigationBars))
        .testTag("terminal-bottom-chrome"),
) {
    // focus/history actions, composer overlay anchor, and ExtraKeys
}
```

Keep the composer visually over the terminal. Derive the resize candidate with `availableHeightPx = terminalSurfaceHeightPx - imeBottomPx - chromeInteractiveHeightPx`, then divide by the measured line height and feed only that `TerminalSize` to `settledTerminalSizes()`. Do not resize or pad `TerminalGrid` on intermediate IME frames, do not restore whole-column `imePadding()`, and do not add a custom keyboard animation.

- [ ] **Step 4: Isolate terminal rendering from composer recomposition**

Extract the current `BoxWithConstraints` plus `TerminalGrid` into a private `TerminalViewport` composable whose parameters are screen, scrollback, metrics, stable bottom obstruction in pixels, and callbacks. Keep `TerminalInputBar` and thumbnail-free composer state outside it so `TextFieldValue` changes do not enter the terminal-row composable's parameter set. Replace the old `render.bottom <= overlay.top` test because the requested composer is now a true overlay; instead assert that the advertised stable row count is computed from the unobscured height while the terminal surface bounds remain fixed.

- [ ] **Step 5: Run all Android verification**

Run:

```bash
cd android
./gradlew testDebugUnitTest lintDebug assembleDebug assembleDebugAndroidTest
```

Install the debug and test APKs with `adb install -r` / `adb install -r -t`, then run the test package manually. Expected: unit tests, lint, build, and `TerminalScreenTest` PASS; the main app data remains intact.

- [ ] **Step 6: Dogfood on the paired Pixel**

Verify keyboard show/hide, interactive back gesture, rapid repeat show/hide, portrait/landscape, collapsed and expanded key bars, toolbar Enter, and scrollback. Confirm the terminal top edge is stationary, chrome follows the keyboard at platform speed, one final resize arrives, and no differently colored strip remains below the bar.

- [ ] **Step 7: Commit**

```bash
git add android/app/src/main/java/com/adroited/aiterm/ui/TerminalScreen.kt android/app/src/androidTest/java/com/adroited/aiterm/ui/TerminalScreenTest.kt
git commit -m "perf(android): use native terminal keyboard insets"
```
