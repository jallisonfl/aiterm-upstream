package com.adroited.aiterm.ui

import android.graphics.Bitmap
import android.net.Uri
import android.view.WindowInsets
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onAllNodesWithContentDescription
import androidx.compose.ui.test.onFirst
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performImeAction
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextInput
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.adroited.aiterm.remote.ConnectionState
import com.adroited.aiterm.remote.FocusOwner
import com.adroited.aiterm.remote.RemoteClientState
import com.adroited.aiterm.remote.RemoteAgentChoice
import com.adroited.aiterm.remote.RemoteModelOption
import com.adroited.aiterm.remote.RemoteSession
import com.adroited.aiterm.remote.RemoteUploadException
import com.adroited.aiterm.remote.RemoteUploadProgress
import com.adroited.aiterm.terminal.CursorState
import com.adroited.aiterm.terminal.ScreenCell
import com.adroited.aiterm.terminal.ScreenRow
import com.adroited.aiterm.terminal.ScreenSnapshot
import com.adroited.aiterm.terminal.TerminalModes
import com.adroited.aiterm.testing.ComposeTestActivity
import kotlin.math.roundToInt
import java.io.File
import kotlinx.coroutines.CompletableDeferred
import org.junit.Assert.assertTrue
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class TerminalScreenTest {
    @get:Rule val compose = createAndroidComposeRule<ComposeTestActivity>()

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
        assertTrue(compose.onAllNodesWithText("Esc").fetchSemanticsNodes().isEmpty())
        compose.onNodeWithTag("expand-extra-keys").assertIsDisplayed().performClick()
        compose.onNodeWithText("Esc").assertIsDisplayed()
    }

    @Test
    fun imageChooserAddsAtMostFourGalleryImagesInSelectionOrderAndExplainsTheLimit() {
        val store = TerminalDraftStore()
        val picker = FakeTerminalImagePickerLauncher().apply {
            galleryResult = TerminalImagePickerResult.Selected(
                (1..5).map { Uri.parse("content://gallery/image-$it") },
            )
        }
        compose.setContent {
            TerminalScreenContent(
                state = connectedState(),
                screen = oneCellScreen("tab-gallery"),
                draftStore = store,
                imagePickerLauncher = picker,
                imageNormalizer = fakeNormalizer(),
            )
        }

        compose.onNodeWithText("Type").performClick()
        compose.onNodeWithTag("terminal-add-image").performClick()
        compose.onNodeWithTag("terminal-image-source-camera").assertIsDisplayed()
        compose.onNodeWithTag("terminal-image-source-gallery").performClick()

        (1..4).forEach { index ->
            compose.onNodeWithTag("terminal-image-image-$index").assertIsDisplayed()
        }
        assertTrue(compose.onAllNodesWithTag("terminal-image-image-5").fetchSemanticsNodes().isEmpty())
        compose.onNodeWithText("You can attach up to 4 images.").assertIsDisplayed()
        compose.runOnIdle {
            assertEquals(
                listOf("image-1", "image-2", "image-3", "image-4"),
                store.draftFor("tab-gallery").attachments.items.map { it.image.id },
            )
        }
    }

    @Test
    fun cameraCaptureIsDeletedAfterNormalizationAndRemovingDeletesOnlyTheNormalizedDraft() {
        val store = TerminalDraftStore()
        val capture = File(compose.activity.cacheDir, "terminal-image-captures/test-capture.jpg")
            .apply { parentFile?.mkdirs(); writeBytes(byteArrayOf(1, 2, 3)) }
        val normalized = normalizedImage("camera-owned")
        val picker = FakeTerminalImagePickerLauncher().apply {
            cameraResult = TerminalImagePickerResult.Selected(
                uris = listOf(Uri.fromFile(capture)),
                ownedCaptureFiles = setOf(capture),
            )
        }
        compose.setContent {
            TerminalScreenContent(
                state = connectedState(),
                screen = oneCellScreen("tab-camera"),
                draftStore = store,
                imagePickerLauncher = picker,
                imageNormalizer = TerminalImageNormalization { Result.success(normalized) },
            )
        }

        compose.onNodeWithText("Type").performClick()
        compose.onNodeWithTag("terminal-add-image").performClick()
        compose.onNodeWithTag("terminal-image-source-camera").performClick()
        compose.onNodeWithTag("terminal-image-camera-owned").assertIsDisplayed()
        compose.runOnIdle { assertTrue(!capture.exists() && normalized.file.exists()) }

        compose.onNodeWithTag("terminal-image-remove-camera-owned").performClick()

        compose.runOnIdle {
            assertTrue(store.draftFor("tab-camera").attachments.items.isEmpty())
            assertTrue(!normalized.file.exists())
        }
    }

    @Test
    fun removingADecodedAttachmentLeavesComposeInChargeOfBitmapLifetime() {
        val store = TerminalDraftStore()
        val image = normalizedImage("preview-lifetime")
        store.updateComposer("tab-preview-lifetime") { it.open() }
        store.updateAttachments("tab-preview-lifetime") { it.add(image).draft }
        compose.setContent {
            TerminalScreenContent(
                state = connectedState(),
                screen = oneCellScreen("tab-preview-lifetime"),
                draftStore = store,
            )
        }

        compose.waitUntil(5_000) {
            compose.onAllNodesWithContentDescription("Attached image")
                .fetchSemanticsNodes().isNotEmpty()
        }
        compose.onNodeWithTag("terminal-image-remove-preview-lifetime").performClick()
        compose.waitForIdle()

        compose.runOnIdle {
            assertTrue(store.draftFor("tab-preview-lifetime").attachments.items.isEmpty())
            assertTrue(!image.file.exists())
        }
    }

    @Test
    fun delayedNormalizationPreventsASecondPickerOrPrematureTextSubmission() {
        val store = TerminalDraftStore()
        store.updateComposer("tab-preparing") { it.open() }
        val normalized = normalizedImage("prepared")
        val release = CompletableDeferred<Result<NormalizedTerminalImage>>()
        val sent = mutableListOf<String>()
        val picker = FakeTerminalImagePickerLauncher().apply {
            galleryResult = TerminalImagePickerResult.Selected(
                listOf(Uri.parse("content://gallery/prepared")),
            )
        }
        compose.setContent {
            TerminalScreenContent(
                state = connectedState(),
                screen = oneCellScreen("tab-preparing"),
                draftStore = store,
                imagePickerLauncher = picker,
                imageNormalizer = TerminalImageNormalization { release.await() },
                onInput = sent::add,
            )
        }

        compose.onNodeWithTag("terminal-add-image").performClick()
        compose.onNodeWithTag("terminal-image-source-gallery").performClick()
        compose.waitUntil(5_000) { store.draftFor("tab-preparing").attachments.preparing }
        compose.onNodeWithTag("terminal-add-image").assertIsNotEnabled()
        compose.onNodeWithTag("terminal-enter").performScrollTo().assertIsNotEnabled()
        compose.runOnIdle { assertTrue(sent.isEmpty()) }

        release.complete(Result.success(normalized))
        compose.waitUntil(5_000) { !store.draftFor("tab-preparing").attachments.preparing }
        compose.onNodeWithTag("terminal-image-prepared").assertIsDisplayed()
        compose.onNodeWithTag("terminal-enter").assertIsEnabled()
    }

    @Test
    fun multiSelectKeepsAnActionableFailureAfterALaterImageSucceeds() {
        val store = TerminalDraftStore()
        val picker = FakeTerminalImagePickerLauncher().apply {
            galleryResult = TerminalImagePickerResult.Selected(
                listOf(Uri.parse("content://gallery/broken"), Uri.parse("content://gallery/good")),
            )
        }
        compose.setContent {
            TerminalScreenContent(
                state = connectedState(),
                screen = oneCellScreen("tab-partial-selection"),
                draftStore = store,
                imagePickerLauncher = picker,
                imageNormalizer = TerminalImageNormalization { uri ->
                    if (uri.lastPathSegment == "broken") {
                        Result.failure(
                            TerminalImageNormalizationError(
                                TerminalImageNormalizationError.Code.DECODE_FAILED,
                                "bad image",
                            ),
                        )
                    } else {
                        Result.success(normalizedImage("good"))
                    }
                },
            )
        }

        compose.onNodeWithText("Type").performClick()
        compose.onNodeWithTag("terminal-add-image").performClick()
        compose.onNodeWithTag("terminal-image-source-gallery").performClick()

        compose.onNodeWithTag("terminal-image-good").assertIsDisplayed()
        compose.onNodeWithText("The image could not be prepared. Choose a different image.")
            .assertIsDisplayed()
    }

    @Test
    fun uploadProgressDisablesRepeatSubmissionAndFailurePreservesTheEntireDraft() {
        val store = TerminalDraftStore()
        val normalized = normalizedImage("slow")
        store.updateComposer("tab-upload") {
            it.open().updateValue(androidx.compose.ui.text.input.TextFieldValue("inspect this")).state
        }
        store.updateAttachments("tab-upload") { it.add(normalized).draft }
        val release = CompletableDeferred<Result<List<String>>>()
        compose.setContent {
            TerminalScreenContent(
                state = connectedState(),
                screen = oneCellScreen("tab-upload"),
                draftStore = store,
                onUploadImages = { _, images, progress ->
                    progress(RemoteUploadProgress(images.single().id, 2, images.single().length))
                    release.await()
                },
            )
        }

        compose.onNodeWithTag("terminal-enter").performScrollTo().performClick()
        compose.onNodeWithTag("terminal-image-progress-slow").assertIsDisplayed()
        compose.onNodeWithTag("terminal-enter").assertIsNotEnabled()
        release.complete(Result.failure(IllegalStateException("Desktop disconnected.")))
        compose.waitForIdle()

        compose.onNodeWithTag("terminal-enter").assertIsEnabled()
        compose.onNodeWithTag("terminal-image-slow").assertIsDisplayed()
        compose.onNodeWithTag("terminal-composer", useUnmergedTree = true).assertTextEquals("inspect this")
        compose.onNodeWithText("Desktop disconnected.").assertIsDisplayed()
        compose.runOnIdle { assertTrue(normalized.file.exists()) }
    }

    @Test
    fun disposingTheScreenDuringUploadReturnsTheExactTabDraftToRetryableState() {
        val store = TerminalDraftStore()
        val image = normalizedImage("cancelled")
        store.updateComposer("tab-cancel") {
            it.open().updateValue(androidx.compose.ui.text.input.TextFieldValue("keep this")).state
        }
        store.updateAttachments("tab-cancel") { it.add(image).draft }
        val visible = mutableStateOf(true)
        compose.setContent {
            if (visible.value) {
                TerminalScreenContent(
                    state = connectedState(),
                    screen = oneCellScreen("tab-cancel"),
                    draftStore = store,
                    onUploadImages = { _, _, _ ->
                        kotlinx.coroutines.suspendCancellableCoroutine { }
                    },
                )
            }
        }

        compose.onNodeWithTag("terminal-enter").performScrollTo().performClick()
        compose.waitUntil(5_000) { store.draftFor("tab-cancel").attachments.submitting }
        compose.runOnIdle { visible.value = false }
        compose.waitUntil(5_000) { !store.draftFor("tab-cancel").attachments.submitting }

        compose.runOnIdle {
            assertEquals("keep this", store.draftFor("tab-cancel").composer.value.text)
            assertEquals(listOf("cancelled"), store.draftFor("tab-cancel").attachments.items.map { it.image.id })
            assertTrue(image.file.exists())
        }
    }

    @Test
    fun successfulImageSubmissionSendsTextAndOrderedPathsThenClearsOwnedDraftFiles() {
        val store = TerminalDraftStore()
        val first = normalizedImage("first")
        val second = normalizedImage("second")
        store.updateComposer("tab-submit") {
            it.open().updateValue(androidx.compose.ui.text.input.TextFieldValue("compare them")).state
        }
        store.updateAttachments("tab-submit") { it.add(first).draft.add(second).draft }
        val accepted = mutableListOf<List<String>>()
        compose.setContent {
            TerminalScreenContent(
                state = connectedState(),
                screen = oneCellScreen("tab-submit").copy(modes = TerminalModes(bracketedPaste = true)),
                draftStore = store,
                onUploadImages = { _, _, _ -> Result.success(listOf("/project/one.jpg", "/project/two.jpg")) },
                onInputBatch = { _, inputs -> accepted += inputs; true },
            )
        }

        compose.onNodeWithTag("terminal-composer", useUnmergedTree = true).performImeAction()
        compose.waitForIdle()

        compose.runOnIdle {
            assertEquals(
                listOf(
                    listOf(
                        "\u001b[200~compare them\n\nAttached images:\n- /project/one.jpg\n- /project/two.jpg\u001b[201~",
                        "\r",
                    ),
                ),
                accepted,
            )
            assertTrue(store.draftFor("tab-submit").composer.value.text.isEmpty())
            assertTrue(store.draftFor("tab-submit").attachments.items.isEmpty())
            assertTrue(!first.file.exists() && !second.file.exists())
        }
    }

    @Test
    fun delayedImageSubmissionUsesTheLatestSameTabBracketedPasteMode() {
        val store = TerminalDraftStore()
        val image = normalizedImage("mode-change")
        store.updateComposer("tab-mode-change") {
            it.open().updateValue(androidx.compose.ui.text.input.TextFieldValue("inspect mode")).state
        }
        store.updateAttachments("tab-mode-change") { it.add(image).draft }
        val currentScreen = mutableStateOf(
            oneCellScreen("tab-mode-change").copy(
                modes = TerminalModes(bracketedPaste = true),
            ),
        )
        val upload = CompletableDeferred<Result<List<String>>>()
        val accepted = mutableListOf<Pair<String, List<String>>>()
        compose.setContent {
            TerminalScreenContent(
                state = connectedState(),
                screen = currentScreen.value,
                draftStore = store,
                onUploadImages = { _, _, _ -> upload.await() },
                onInputBatch = { tabId, inputs -> accepted += tabId to inputs; true },
            )
        }

        compose.onNodeWithTag("terminal-enter").performScrollTo().performClick()
        compose.waitUntil(5_000) { store.draftFor("tab-mode-change").attachments.submitting }
        compose.runOnIdle {
            currentScreen.value = currentScreen.value.copy(
                revision = currentScreen.value.revision + 1,
                modes = TerminalModes(bracketedPaste = false),
            )
        }
        compose.waitForIdle()
        upload.complete(Result.success(listOf("/project/current-mode.jpg")))
        compose.waitUntil(5_000) { accepted.isNotEmpty() }

        compose.runOnIdle {
            assertEquals(
                listOf(
                    "tab-mode-change" to listOf(
                        "inspect mode\n\nAttached images:\n- /project/current-mode.jpg",
                        "\r",
                    ),
                ),
                accepted,
            )
        }
    }

    @Test
    fun attachmentOnlySubmissionUsesSharedEnterPathAndLocalRejectionKeepsTheDraft() {
        val store = TerminalDraftStore()
        val image = normalizedImage("only")
        store.updateComposer("tab-only") { it.open() }
        store.updateAttachments("tab-only") { it.add(image).draft }
        val attempted = mutableListOf<List<String>>()
        compose.setContent {
            TerminalScreenContent(
                state = connectedState(),
                screen = oneCellScreen("tab-only"),
                draftStore = store,
                onUploadImages = { _, _, _ -> Result.success(listOf("/project/only.jpg")) },
                onInputBatch = { _, inputs -> attempted += inputs; false },
            )
        }

        compose.onNodeWithTag("terminal-enter").performScrollTo().performClick()
        compose.waitForIdle()

        compose.runOnIdle {
            assertEquals(
                listOf("Please inspect the attached image(s):\n\nAttached images:\n- /project/only.jpg", "\r"),
                attempted.single(),
            )
            assertEquals(listOf("only"), store.draftFor("tab-only").attachments.items.map { it.image.id })
            assertTrue(image.file.exists())
        }
        compose.onNodeWithText("Terminal input was not accepted. Take focus and try again.").assertIsDisplayed()
    }

    @Test
    fun unsupportedDesktopAndPickerCancellationKeepIndependentTabDrafts() {
        val store = TerminalDraftStore()
        val selectedTab = mutableStateOf("tab-a")
        val first = normalizedImage("tab-a-image")
        store.updateComposer("tab-a") {
            it.open().updateValue(androidx.compose.ui.text.input.TextFieldValue("A text")).state
        }
        store.updateAttachments("tab-a") { it.add(first).draft }
        store.updateComposer("tab-b") {
            it.open().updateValue(androidx.compose.ui.text.input.TextFieldValue("B text")).state
        }
        val picker = FakeTerminalImagePickerLauncher().apply {
            galleryResult = TerminalImagePickerResult.Cancelled
        }
        compose.setContent {
            TerminalScreenContent(
                state = connectedState(),
                screen = oneCellScreen(selectedTab.value),
                draftStore = store,
                imagePickerLauncher = picker,
                imageNormalizer = fakeNormalizer(),
                onUploadImages = { _, _, _ ->
                    Result.failure(RemoteUploadException("protocol.unknown_request", "unknown request kind"))
                },
            )
        }

        compose.onNodeWithTag("terminal-composer", useUnmergedTree = true).assertTextEquals("A text")
        compose.onNodeWithTag("terminal-enter").performScrollTo().performClick()
        compose.waitForIdle()
        compose.onNodeWithText("Update AITerm on the desktop to attach images.").assertIsDisplayed()
        compose.runOnIdle { selectedTab.value = "tab-b" }
        compose.onNodeWithTag("terminal-composer", useUnmergedTree = true).assertTextEquals("B text")
        compose.onNodeWithTag("terminal-add-image").performClick()
        compose.onNodeWithTag("terminal-image-source-gallery").performClick()
        compose.runOnIdle { assertTrue(store.draftFor("tab-b").attachments.items.isEmpty()) }
        compose.runOnIdle { selectedTab.value = "tab-a" }
        compose.onNodeWithTag("terminal-image-tab-a-image").assertIsDisplayed()
        compose.onNodeWithTag("terminal-composer", useUnmergedTree = true).assertTextEquals("A text")
    }

    @Test
    fun acceptedTextOnlySubmissionClearsAnObsoleteAttachmentErrorWithTheWholeTabDraft() {
        val store = TerminalDraftStore()
        store.updateComposer("tab-text-clean") {
            it.open().updateValue(androidx.compose.ui.text.input.TextFieldValue("status")).state
        }
        store.updateAttachments("tab-text-clean") {
            it.copy(message = "The previous image could not be prepared.")
        }
        val accepted = mutableListOf<List<String>>()
        compose.setContent {
            TerminalScreenContent(
                state = connectedState(),
                screen = oneCellScreen("tab-text-clean"),
                draftStore = store,
                onInputBatch = { _, inputs -> accepted += inputs; true },
            )
        }

        compose.onNodeWithTag("terminal-composer", useUnmergedTree = true).performImeAction()

        compose.runOnIdle {
            assertEquals(listOf(listOf("status", "\r")), accepted)
            assertFalse(store.hasDrafts())
            assertEquals(null, store.draftFor("tab-text-clean").attachments.message)
        }
    }

    @Test
    fun rapidTextOnlyImeRepeatsAcceptOnlyOnePromptBatch() {
        val store = TerminalDraftStore()
        store.updateComposer("tab-text-repeat") {
            it.open().updateValue(androidx.compose.ui.text.input.TextFieldValue("run once")).state
        }
        val accepted = mutableListOf<List<String>>()
        compose.setContent {
            TerminalScreenContent(
                state = connectedState(),
                screen = oneCellScreen("tab-text-repeat"),
                draftStore = store,
                onInputBatch = { _, inputs -> accepted += inputs; true },
            )
        }
        val imeAction = compose.onNodeWithTag("terminal-composer", useUnmergedTree = true)
            .fetchSemanticsNode().config[SemanticsActions.OnImeAction].action

        compose.runOnUiThread {
            imeAction?.invoke()
            imeAction?.invoke()
        }

        compose.runOnIdle {
            assertEquals(listOf(listOf("run once", "\r")), accepted)
            assertFalse(store.hasDrafts())
        }
    }

    @Test
    fun backKeepsOrDiscardsAllDraftsOnlyAfterExplicitChoice() {
        val store = TerminalDraftStore()
        val image = normalizedImage("discard")
        store.updateAttachments("tab-back") { it.add(image).draft }
        var left = 0
        compose.setContent {
            TerminalScreenContent(
                state = connectedState(),
                screen = oneCellScreen("tab-back"),
                draftStore = store,
                onBack = { left += 1 },
            )
        }

        compose.onNodeWithText("Back").performClick()
        compose.onNodeWithTag("terminal-draft-discard-dialog").assertIsDisplayed()
        compose.onNodeWithText("Keep editing").performClick()
        compose.runOnIdle { assertEquals(0, left); assertTrue(image.file.exists()) }

        compose.onNodeWithText("Back").performClick()
        compose.onNodeWithText("Discard drafts and leave").performClick()
        compose.runOnIdle {
            assertEquals(1, left)
            assertTrue(!image.file.exists())
            assertFalse(store.hasDrafts())
        }
    }

    @Test
    fun textComposerKeepsTheDraftVisibleUntilSend() {
        val sent = mutableListOf<String>()
        compose.setContent {
            TerminalScreenContent(
                state = RemoteClientState(
                    connection = ConnectionState.Connected,
                    focus = FocusOwner.Self,
                    activeTabId = "tab-compose",
                    activeTitle = "Prompt",
                ),
                screen = ScreenSnapshot(
                    tabId = "tab-compose",
                    revision = 1,
                    cols = 5,
                    rows = 1,
                    visible = listOf(ScreenRow("ready".map { ScreenCell(it.toString()) })),
                    cursor = CursorState(0, 0, true),
                ),
                onInput = sent::add,
            )
        }

        assertTrue(compose.onAllNodesWithTag("terminal-composer", useUnmergedTree = true).fetchSemanticsNodes().isEmpty())
        compose.onNodeWithText("Type").performClick()
        val composer = compose.onNodeWithTag("terminal-composer", useUnmergedTree = true)
        composer.assertIsDisplayed().performTextInput("hello phone")
        composer.assertTextEquals("hello phone")
        compose.runOnIdle { assertTrue(sent.isEmpty()) }
        assertTrue(compose.onAllNodesWithText("Send").fetchSemanticsNodes().isEmpty())

        composer.performImeAction()

        compose.runOnIdle { assertEquals(listOf("hello phone", "\r"), sent) }
        assertTrue(compose.onAllNodesWithTag("terminal-composer", useUnmergedTree = true).fetchSemanticsNodes().isEmpty())
        compose.onNodeWithText("Type").assertIsDisplayed()
    }

    @Test
    fun imeSubmitUsesBracketedPasteBeforeTheToolbarEnterAction() {
        val sent = mutableListOf<String>()
        compose.setContent {
            TerminalScreenContent(
                state = RemoteClientState(
                    connection = ConnectionState.Connected,
                    focus = FocusOwner.Self,
                    activeTabId = "tab-ime-submit",
                ),
                screen = ScreenSnapshot(
                    tabId = "tab-ime-submit",
                    revision = 1,
                    cols = 1,
                    rows = 1,
                    visible = listOf(ScreenRow(listOf(ScreenCell("$")))),
                    cursor = CursorState(0, 0, true),
                    modes = TerminalModes(bracketedPaste = true),
                ),
                onInput = sent::add,
            )
        }

        compose.onNodeWithText("Type").performClick()
        val composer = compose.onNodeWithTag("terminal-composer", useUnmergedTree = true)
        composer.performTextInput("hello phone")
        composer.performImeAction()

        compose.runOnIdle {
            assertEquals(listOf("\u001b[200~hello phone\u001b[201~", "\r"), sent)
        }
    }

    @Test
    fun terminalKeyEnterSubmitsAnOrdinaryComposerDraft() {
        val sent = mutableListOf<String>()
        compose.setContent {
            TerminalScreenContent(
                state = connectedState(),
                screen = oneCellScreen("tab-key-enter-ordinary"),
                onInput = sent::add,
            )
        }

        compose.onNodeWithText("Type").performClick()
        compose.onNodeWithTag("terminal-composer", useUnmergedTree = true)
            .performTextInput("status")
        compose.onNodeWithText("Enter").performScrollTo().performClick()

        compose.runOnIdle { assertEquals(listOf("status", "\r"), sent) }
        assertTrue(
            compose.onAllNodesWithTag("terminal-composer", useUnmergedTree = true)
                .fetchSemanticsNodes().isEmpty(),
        )
    }

    @Test
    fun terminalKeyEnterSubmitsBracketedDraftBeforeCarriageReturn() {
        val sent = mutableListOf<String>()
        compose.setContent {
            TerminalScreenContent(
                state = connectedState(),
                screen = oneCellScreen("tab-key-enter-bracketed").copy(
                    modes = TerminalModes(bracketedPaste = true),
                ),
                onInput = sent::add,
            )
        }

        compose.onNodeWithText("Type").performClick()
        compose.onNodeWithTag("terminal-composer", useUnmergedTree = true)
            .performTextInput("git status")
        compose.onNodeWithText("Enter").performScrollTo().performClick()

        compose.runOnIdle {
            assertEquals(listOf("\u001b[200~git status\u001b[201~", "\r"), sent)
        }
    }

    @Test
    fun terminalKeyEnterSendsRawCarriageReturnWhenComposerIsClosed() {
        val sent = mutableListOf<String>()
        compose.setContent {
            TerminalScreenContent(
                state = connectedState(),
                screen = oneCellScreen("tab-key-enter-closed"),
                onInput = sent::add,
            )
        }

        compose.onNodeWithText("Enter").performScrollTo().performClick()

        compose.runOnIdle { assertEquals(listOf("\r"), sent) }
    }

    @Test
    fun terminalKeyEnterKeepsAnEmptyComposerOpenAfterRawCarriageReturn() {
        val sent = mutableListOf<String>()
        compose.setContent {
            TerminalScreenContent(
                state = connectedState(),
                screen = oneCellScreen("tab-key-enter-empty"),
                onInput = sent::add,
            )
        }

        compose.onNodeWithText("Type").performClick()
        compose.onNodeWithTag("terminal-composer", useUnmergedTree = true).assertIsDisplayed()
        compose.onNodeWithText("Enter").performScrollTo().performClick()

        compose.runOnIdle { assertEquals(listOf("\r"), sent) }
        compose.onNodeWithTag("terminal-composer", useUnmergedTree = true).assertIsDisplayed()
    }

    @Test
    fun composerOverlaysTheTerminalWhileAdvertisedRowsExcludeBottomChrome() {
        val sizes = mutableListOf<Pair<Int, Int>>()
        compose.setContent {
            TerminalScreenContent(
                state = RemoteClientState(
                    connection = ConnectionState.Connected,
                    focus = FocusOwner.Self,
                    activeTabId = "tab-overlay",
                ),
                screen = ScreenSnapshot(
                    tabId = "tab-overlay",
                    revision = 1,
                    cols = 1,
                    rows = 1,
                    visible = listOf(ScreenRow(listOf(ScreenCell("$")))),
                    cursor = CursorState(0, 0, true),
                ),
                onResize = { cols, rows -> sizes += cols to rows },
            )
        }

        compose.waitUntil(5_000) { sizes.isNotEmpty() }
        compose.runOnIdle { sizes.clear() }
        compose.onNodeWithText("Type").performClick()
        compose.waitUntil(5_000) {
            compose.activity.window.decorView.rootWindowInsets
                ?.isVisible(WindowInsets.Type.ime()) == true
        }
        compose.waitUntil(5_000) { sizes.isNotEmpty() }

        val render = compose.onNodeWithTag("terminal-render-content", useUnmergedTree = true)
            .fetchSemanticsNode().boundsInRoot
        val chrome = compose.onNodeWithTag("terminal-bottom-chrome", useUnmergedTree = true)
            .fetchSemanticsNode().boundsInRoot
        val overlay = compose.onNodeWithTag("terminal-composer-overlay", useUnmergedTree = true)
            .fetchSemanticsNode().boundsInRoot
        val field = compose.onNodeWithTag("terminal-composer", useUnmergedTree = true)
            .fetchSemanticsNode().boundsInRoot
        val placeholder = compose.onNodeWithText("Type a command or prompt…", useUnmergedTree = true)
            .fetchSemanticsNode().boundsInRoot
        val maxSingleRowHeight = 60f * compose.activity.resources.displayMetrics.density

        assertTrue(
            "composer input must remain a compact single row",
            field.height <= maxSingleRowHeight,
        )
        assertTrue("composer must overlay the stable terminal render", render.bottom > overlay.top)
        val rowHeight = compose.onNodeWithTag("terminal-row", useUnmergedTree = true)
            .fetchSemanticsNode().boundsInRoot.height
        val expectedRows = ((chrome.top - render.top) / rowHeight).toInt().coerceIn(1, 512)
        compose.runOnIdle {
            assertEquals(
                "advertised rows must exclude the terminal area hidden by bottom chrome",
                expectedRows,
                sizes.last().second,
            )
        }
        assertTrue(
            "placeholder must be vertically centered in the input",
            kotlin.math.abs(field.center.y - placeholder.center.y) < 2f,
        )

        assertTrue(
            compose.onAllNodesWithTag("input-mode-direct", useUnmergedTree = true)
                .fetchSemanticsNodes().isEmpty(),
        )
        assertTrue(kotlin.math.abs(field.center.y - placeholder.center.y) < 2f)
    }

    @Test
    fun terminalSurfaceStaysFixedWhileBottomChromeConsumesNativeImeInsets() {
        compose.runOnIdle {
            WindowCompat.getInsetsController(
                compose.activity.window,
                compose.activity.window.decorView,
            ).hide(WindowInsetsCompat.Type.ime())
        }
        compose.waitUntil(5_000) {
            compose.activity.window.decorView.rootWindowInsets
                ?.isVisible(WindowInsets.Type.ime()) != true
        }
        compose.setContent {
            TerminalScreenContent(
                state = RemoteClientState(
                    connection = ConnectionState.Connected,
                    focus = FocusOwner.Self,
                    activeTabId = "tab-native-insets",
                ),
                screen = oneCellScreen("tab-native-insets"),
            )
        }

        val surfaceBefore = compose.onNodeWithTag("terminal-surface", useUnmergedTree = true)
            .fetchSemanticsNode().boundsInRoot
        compose.onNodeWithText("Type").performClick()
        val composer = compose.onNodeWithTag("terminal-composer", useUnmergedTree = true)
        composer.performClick().performTextInput("native")
        compose.waitUntil(5_000) {
            compose.activity.window.decorView.rootWindowInsets
                ?.isVisible(WindowInsets.Type.ime()) == true
        }

        val surfaceDuring = compose.onNodeWithTag("terminal-surface", useUnmergedTree = true)
            .fetchSemanticsNode().boundsInRoot
        assertEquals(surfaceBefore.top, surfaceDuring.top, 1f)
        assertEquals(surfaceBefore.bottom, surfaceDuring.bottom, 1f)

        val chrome = compose.onNodeWithTag("terminal-bottom-chrome", useUnmergedTree = true)
            .fetchSemanticsNode().boundsInRoot
        val decorHeight = compose.activity.window.decorView.height.toFloat()
        assertTrue(
            "surface ${surfaceBefore.top}..${surfaceBefore.bottom} -> " +
                "${surfaceDuring.top}..${surfaceDuring.bottom}; bottom chrome " +
                "${chrome.top}..${chrome.bottom} must own decor bottom $decorHeight",
            chrome.bottom >= decorHeight,
        )
    }

    @Test
    fun textComposerStaysAboveTheSoftwareKeyboard() {
        compose.setContent {
            TerminalScreenContent(
                state = RemoteClientState(
                    connection = ConnectionState.Connected,
                    focus = FocusOwner.Self,
                    activeTabId = "tab-ime",
                ),
                screen = ScreenSnapshot(
                    tabId = "tab-ime",
                    revision = 1,
                    cols = 1,
                    rows = 1,
                    visible = listOf(ScreenRow(listOf(ScreenCell("$")))),
                    cursor = CursorState(0, 0, true),
                ),
            )
        }

        compose.onNodeWithText("Type").performClick()
        val composer = compose.onNodeWithTag("terminal-composer", useUnmergedTree = true)
        composer.performClick().performTextInput("visible")
        compose.waitUntil(5_000) {
            compose.activity.window.decorView.rootWindowInsets
                ?.isVisible(WindowInsets.Type.ime()) == true
        }

        val composerBottom = composer.fetchSemanticsNode().boundsInRoot.bottom
        val decor = compose.activity.window.decorView
        val keyboardTop = decor.height - decor.rootWindowInsets
            .getInsets(WindowInsets.Type.ime()).bottom
        assertTrue(
            "composer bottom $composerBottom must be above keyboard top $keyboardTop",
            composerBottom <= keyboardTop + 1f,
        )
    }

    @Test
    fun sessionsDrawerOmitsTheAgentLauncherForTheRemoteClient() {
        compose.setContent {
            TerminalScreenContent(
                state = RemoteClientState(
                    connection = ConnectionState.Connected,
                    sessions = listOf(
                        RemoteSession(
                            id = "session-1",
                            agent = "codex",
                            title = "AITerm",
                            projectPath = "/projects/aiterm",
                            groupPath = "/projects/aiterm",
                            forked = false,
                            background = false,
                            lastActive = 1,
                        ),
                    ),
                    agents = listOf(
                        RemoteAgentChoice(
                            id = "codex",
                            displayName = "Codex",
                            models = listOf(
                                RemoteModelOption(
                                    id = "gpt-5",
                                    displayName = "GPT-5",
                                    efforts = listOf("high"),
                                ),
                            ),
                            mintsSessionId = true,
                        ),
                    ),
                ),
                screen = null,
            )
        }

        compose.onNodeWithText("Sessions").performClick()
        compose.waitForIdle()

        compose.onNodeWithText("LIVE TABS").assertIsDisplayed()
        compose.onNodeWithText("SESSIONS").assertIsDisplayed()
        assertTrue(compose.onAllNodesWithText("NEW AGENT").fetchSemanticsNodes().isEmpty())
        assertTrue(compose.onAllNodesWithText("Start Codex · GPT-5 · high").fetchSemanticsNodes().isEmpty())
    }

    @Test
    fun nativeGridRemainsVisibleWhileReadOnlyAndOffersFocusAndExtraKeys() {
        var focusRequested = false
        compose.setContent {
            TerminalScreenContent(
                state = RemoteClientState(
                    connection = ConnectionState.Connected,
                    focus = FocusOwner.Other,
                    readOnly = true,
                    showTakeFocus = true,
                    activeTabId = "tab-1",
                    activeTitle = "Storm shell",
                ),
                screen = ScreenSnapshot(
                    tabId = "tab-1",
                    revision = 1,
                    cols = 5,
                    rows = 1,
                    visible = listOf(ScreenRow("hello".map { ScreenCell(it.toString()) })),
                    cursor = CursorState(0, 0, true),
                ),
                onTakeFocus = { _, _ -> focusRequested = true },
            )
        }

        compose.onNodeWithTag("terminal-grid").assertIsDisplayed()
        compose.onNodeWithText("hello").assertIsDisplayed()
        compose.onAllNodesWithText("CONNECTED").onFirst().assertIsDisplayed()
        compose.onNodeWithText("Esc").assertIsDisplayed()
        compose.onNodeWithText("Take Focus").performClick()

        assertTrue(focusRequested)
    }

    @Test
    fun portraitToLandscapeConstraintsKeepTheScreenAndReportANewCanonicalViewport() {
        val sizes = mutableListOf<Pair<Int, Int>>()
        val landscape = mutableStateOf(false)
        compose.setContent {
            Box(Modifier.size(if (landscape.value) 800.dp else 400.dp, if (landscape.value) 400.dp else 800.dp)) {
                TerminalScreenContent(
                    state = RemoteClientState(connection = ConnectionState.Connected),
                    screen = ScreenSnapshot(
                        tabId = "tab-rotation",
                        revision = 8,
                        cols = 6,
                        rows = 1,
                        visible = listOf(ScreenRow("rotate".map { ScreenCell(it.toString()) })),
                        cursor = CursorState(0, 0, true),
                    ),
                    onResize = { cols, rows -> sizes += cols to rows },
                )
            }
        }
        compose.waitUntil(5_000) { sizes.isNotEmpty() }
        val initial = sizes.last()

        compose.runOnIdle { landscape.value = true }
        compose.waitUntil(8_000) { sizes.any { it != initial } }

        compose.onNodeWithText("rotate").assertIsDisplayed()
        assertTrue(sizes.any { it != initial })
    }

    @Test
    fun resizeStormPublishesOnlyTheFinalStableViewport() {
        val sizes = mutableListOf<Pair<Int, Int>>()
        val height = mutableStateOf(480.dp)
        val originalAutoAdvance = compose.mainClock.autoAdvance
        compose.mainClock.autoAdvance = false
        try {
            compose.setContent {
                Box(Modifier.size(400.dp, height.value)) {
                    TerminalScreenContent(
                        state = RemoteClientState(connection = ConnectionState.Connected),
                        screen = ScreenSnapshot(
                            tabId = "tab-resize-storm",
                            revision = 1,
                            cols = 1,
                            rows = 1,
                            visible = listOf(ScreenRow(listOf(ScreenCell("M")))),
                            cursor = CursorState(0, 0, true),
                        ),
                        onResize = { cols, rows -> sizes += cols to rows },
                    )
                }
            }
            compose.mainClock.advanceTimeByFrame()
            compose.runOnIdle { sizes.clear() }

            repeat(10) { index ->
                compose.runOnIdle { height.value = (480 + (index + 1) * 24).dp }
                compose.mainClock.advanceTimeBy(10)
            }

            val render = compose.onNodeWithTag("terminal-render-content", useUnmergedTree = true)
                .fetchSemanticsNode().boundsInRoot
            val cell = compose.onNodeWithTag("terminal-cell-0-0", useUnmergedTree = true)
                .fetchSemanticsNode().boundsInRoot
            val row = compose.onNodeWithTag("terminal-row", useUnmergedTree = true)
                .fetchSemanticsNode().boundsInRoot
            val chrome = compose.onNodeWithTag("terminal-bottom-chrome", useUnmergedTree = true)
                .fetchSemanticsNode().boundsInRoot
            val finalViewport = (render.width / cell.width).toInt().coerceIn(1, 512) to
                ((chrome.top - render.top) / row.height).toInt().coerceIn(1, 512)
            compose.runOnIdle { assertTrue(sizes.isEmpty()) }
            compose.mainClock.advanceTimeBy(TERMINAL_RESIZE_SETTLE_MILLIS)
            compose.runOnIdle { assertEquals(listOf(finalViewport), sizes) }
        } finally {
            compose.mainClock.autoAdvance = originalAutoAdvance
        }
    }

    @Test
    fun advertisedRowsIncludeTheViewportBottomPaddingAtALineThreshold() {
        val sizes = mutableListOf<Pair<Int, Int>>()
        val height = mutableStateOf(480.dp)
        compose.setContent {
            Box(Modifier.size(400.dp, height.value)) {
                TerminalScreenContent(
                    state = connectedState(),
                    screen = oneCellScreen("tab-bottom-padding-threshold"),
                    onResize = { cols, rows -> sizes += cols to rows },
                )
            }
        }

        val density = compose.activity.resources.displayMetrics.density
        val viewportBottomPaddingPx = 3f * density
        var thresholdHeight = 0.dp
        var rowStep = 0.dp
        for (candidateHeight in 480..504) {
            compose.runOnIdle { height.value = candidateHeight.dp }
            compose.waitForIdle()
            val render = compose.onNodeWithTag("terminal-render-content", useUnmergedTree = true)
                .fetchSemanticsNode().boundsInRoot
            val chrome = compose.onNodeWithTag("terminal-bottom-chrome", useUnmergedTree = true)
                .fetchSemanticsNode().boundsInRoot
            val rowHeight = compose.onNodeWithTag("terminal-row", useUnmergedTree = true)
                .fetchSemanticsNode().boundsInRoot.height
            val visibleHeight = chrome.top - render.top
            val visibleRows = (visibleHeight / rowHeight).toInt().coerceIn(1, 512)
            val undercountedRows = ((visibleHeight - viewportBottomPaddingPx) / rowHeight)
                .toInt().coerceIn(1, 512)
            if (visibleRows > undercountedRows) {
                thresholdHeight = candidateHeight.dp
                rowStep = (rowHeight / density).dp
                break
            }
        }
        assertTrue("test geometry must cross a row boundary within the bottom 3 dp", rowStep > 0.dp)

        compose.runOnIdle {
            sizes.clear()
            height.value = thresholdHeight + rowStep
        }
        compose.waitUntil(5_000) { sizes.isNotEmpty() }

        val render = compose.onNodeWithTag("terminal-render-content", useUnmergedTree = true)
            .fetchSemanticsNode().boundsInRoot
        val chrome = compose.onNodeWithTag("terminal-bottom-chrome", useUnmergedTree = true)
            .fetchSemanticsNode().boundsInRoot
        val rowHeight = compose.onNodeWithTag("terminal-row", useUnmergedTree = true)
            .fetchSemanticsNode().boundsInRoot.height
        val visibleHeight = chrome.top - render.top
        val expectedRows = (visibleHeight / rowHeight).toInt().coerceIn(1, 512)
        val undercountedRows = ((visibleHeight - viewportBottomPaddingPx) / rowHeight)
            .toInt().coerceIn(1, 512)
        assertTrue("threshold must distinguish the old undercount", expectedRows > undercountedRows)
        compose.runOnIdle {
            assertEquals(
                "advertised rows must include the render's bottom padding above chrome",
                expectedRows,
                sizes.last().second,
            )
        }
    }

    @Test
    fun sideNavigationInsetsKeepRenderedAndAdvertisedColumnsInsideTheSafeWidth() {
        val sizes = mutableListOf<Pair<Int, Int>>()
        val leftNavigationPx = 47
        val rightNavigationPx = 79
        compose.setContent {
            Box(Modifier.size(400.dp, 480.dp)) {
                TerminalScreenContent(
                    state = connectedState(),
                    screen = oneCellScreen("tab-side-navigation"),
                    onResize = { cols, rows -> sizes += cols to rows },
                    imeInsets = androidx.compose.foundation.layout.WindowInsets(0, 0, 0, 0),
                    navigationInsets = androidx.compose.foundation.layout.WindowInsets(
                        leftNavigationPx,
                        0,
                        rightNavigationPx,
                        0,
                    ),
                )
            }
        }

        compose.waitUntil(5_000) { sizes.isNotEmpty() }
        val surface = compose.onNodeWithTag("terminal-surface", useUnmergedTree = true)
            .fetchSemanticsNode().boundsInRoot
        val render = compose.onNodeWithTag("terminal-render-content", useUnmergedTree = true)
            .fetchSemanticsNode().boundsInRoot
        val cellWidthPx = compose.onNodeWithTag("terminal-cell-0-0", useUnmergedTree = true)
            .fetchSemanticsNode().boundsInRoot.width.toInt()
        val viewportPaddingPx = (4f * compose.activity.resources.displayMetrics.density)
            .roundToInt()
        val surfaceWidthPx = surface.width.roundToInt()
        val expectedColumns = (
            surfaceWidthPx - leftNavigationPx - rightNavigationPx - 2 * viewportPaddingPx
        ) / cellWidthPx
        val unsafeFullWidthColumns = (surfaceWidthPx - 2 * viewportPaddingPx) / cellWidthPx

        assertTrue(
            "fixture must distinguish side-safe columns from full-width columns",
            expectedColumns < unsafeFullWidthColumns,
        )
        compose.runOnIdle { assertEquals(expectedColumns, sizes.last().first) }
        assertEquals(surface.left + leftNavigationPx + viewportPaddingPx, render.left, 1f)
        assertEquals(surface.right - rightNavigationPx - viewportPaddingPx, render.right, 1f)
    }

    @Test
    fun navigationBarOnlyObstructionLimitsRowsWhileImeIsHidden() {
        val sizes = mutableListOf<Pair<Int, Int>>()
        compose.setContent {
            Box(Modifier.size(400.dp, 800.dp)) {
                TerminalScreenContent(
                    state = connectedState(),
                    screen = oneCellScreen("tab-navigation-only"),
                    onResize = { cols, rows -> sizes += cols to rows },
                    imeInsets = androidx.compose.foundation.layout.WindowInsets(0, 0, 0, 0),
                    navigationInsets = androidx.compose.foundation.layout.WindowInsets(0, 0, 0, 61),
                )
            }
        }

        compose.waitUntil(5_000) { sizes.isNotEmpty() }
        val render = compose.onNodeWithTag("terminal-render-content", useUnmergedTree = true)
            .fetchSemanticsNode().boundsInRoot
        val chrome = compose.onNodeWithTag("terminal-bottom-chrome", useUnmergedTree = true)
            .fetchSemanticsNode().boundsInRoot
        val rowHeightPx = compose.onNodeWithTag("terminal-row", useUnmergedTree = true)
            .fetchSemanticsNode().boundsInRoot.height.roundToInt()
        val visibleHeightPx = (chrome.top - render.top).roundToInt()
        val expectedRows = (visibleHeightPx / rowHeightPx).coerceIn(1, 512)

        compose.runOnIdle { assertEquals(expectedRows, sizes.last().second) }
    }

    @Test
    fun measuredGridKeepsWideCombiningAndCursorOnTheSameFontScaledGeometry() {
        compose.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, 1.6f)) {
                TerminalScreenContent(
                    state = RemoteClientState(connection = ConnectionState.Connected),
                    screen = ScreenSnapshot(
                        tabId = "tab-geometry",
                        revision = 1,
                        cols = 3,
                        rows = 1,
                        visible = listOf(
                            ScreenRow(
                                listOf(
                                    ScreenCell("界", width = 2),
                                    ScreenCell("", continuation = true),
                                    ScreenCell("e\u0301"),
                                ),
                            ),
                        ),
                        cursor = CursorState(2, 0, true),
                    ),
                )
            }
        }

        val wide = compose.onNodeWithTag("terminal-cell-0-0", useUnmergedTree = true)
            .fetchSemanticsNode().boundsInRoot
        val combining = compose.onNodeWithTag("terminal-cell-0-2", useUnmergedTree = true)
            .fetchSemanticsNode().boundsInRoot
        val cursor = compose.onNodeWithTag("terminal-cursor", useUnmergedTree = true)
            .fetchSemanticsNode().boundsInRoot
        assertTrue(kotlin.math.abs(wide.width - combining.width * 2f) < 2f)
        assertTrue(kotlin.math.abs(cursor.left - combining.left) < 2f)
        assertTrue(kotlin.math.abs(cursor.height - combining.height) < 2f)
        compose.onNodeWithText("界é").assertIsDisplayed()
    }

    @Test
    fun advertisedViewportUsesTheFontScaledPaddedRenderBoundsAcrossRotation() {
        val sizes = mutableListOf<Pair<Int, Int>>()
        val dimensions = mutableStateOf(400.dp to 800.dp)
        compose.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, 1.6f)) {
                Box(
                    Modifier.size(dimensions.value.first, dimensions.value.second),
                ) {
                    TerminalScreenContent(
                        state = RemoteClientState(connection = ConnectionState.Connected),
                        screen = ScreenSnapshot(
                            tabId = "tab-render-bounds",
                            revision = 1,
                            cols = 1,
                            rows = 1,
                            visible = listOf(ScreenRow(listOf(ScreenCell("M")))),
                            cursor = CursorState(0, 0, true),
                        ),
                        onResize = { cols, rows -> sizes += cols to rows },
                    )
                }
            }
        }

        fun assertLatestViewportMatchesGrid() {
            val advertised = sizes.last()
            val grid = compose.onNodeWithTag("terminal-render-content", useUnmergedTree = true)
                .fetchSemanticsNode().boundsInRoot
            val cell = compose.onNodeWithTag("terminal-cell-0-0", useUnmergedTree = true)
                .fetchSemanticsNode().boundsInRoot
            val row = compose.onNodeWithTag("terminal-row", useUnmergedTree = true)
                .fetchSemanticsNode().boundsInRoot
            val chrome = compose.onNodeWithTag("terminal-bottom-chrome", useUnmergedTree = true)
                .fetchSemanticsNode().boundsInRoot
            assertEquals(
                "advertised columns must come from the padded grid width",
                (grid.width / cell.width).toInt().coerceIn(1, 512),
                advertised.first,
            )
            assertEquals(
                "advertised rows must come from the unobscured padded grid height",
                ((chrome.top - grid.top) / row.height).toInt().coerceIn(1, 512),
                advertised.second,
            )
            assertTrue(advertised.first * cell.width <= grid.width + 1f)
            assertTrue(advertised.second * row.height <= chrome.top - grid.top + 1f)
        }

        compose.waitUntil(5_000) { sizes.isNotEmpty() }
        val callbacksBeforeResizeStorm = sizes.size
        for (width in 380..410) {
            compose.runOnIdle { dimensions.value = width.dp to 800.dp }
            compose.waitForIdle()
        }
        compose.waitUntil(8_000) { sizes.size > callbacksBeforeResizeStorm }
        assertLatestViewportMatchesGrid()
        val portrait = sizes.last()
        compose.runOnIdle { dimensions.value = 800.dp to 400.dp }
        compose.waitUntil(8_000) { sizes.lastOrNull() != portrait }
        assertLatestViewportMatchesGrid()
    }

    @Test
    fun largeScrollbackComposesOnlyTheBoundedVisibleRowWindow() {
        val history = List(5_000) { index ->
            ScreenRow("history-$index".map { ScreenCell(it.toString()) })
        }
        compose.setContent {
            Box(Modifier.size(400.dp, 800.dp)) {
                TerminalScreenContent(
                    state = RemoteClientState(connection = ConnectionState.Connected),
                    screen = ScreenSnapshot(
                        tabId = "tab-history",
                        revision = 1,
                        cols = 4,
                        rows = 1,
                        visible = listOf(ScreenRow("live".map { ScreenCell(it.toString()) })),
                        cursor = CursorState(0, 0, true),
                    ),
                    scrollback = history,
                )
            }
        }

        compose.onNodeWithTag("terminal-grid").assertIsDisplayed()
        val composedRows = compose.onAllNodesWithTag("terminal-row", useUnmergedTree = true)
            .fetchSemanticsNodes().size
        assertTrue(composedRows > 0)
        assertTrue("composed $composedRows rows", composedRows < 100)
        assertEquals(5_000, history.size)
    }

    private fun connectedState() = RemoteClientState(
        connection = ConnectionState.Connected,
        focus = FocusOwner.Self,
    )

    private fun oneCellScreen(tabId: String) = ScreenSnapshot(
        tabId = tabId,
        revision = 1,
        cols = 1,
        rows = 1,
        visible = listOf(ScreenRow(listOf(ScreenCell("$")))),
        cursor = CursorState(0, 0, true),
    )

    private fun normalizedImage(id: String): NormalizedTerminalImage {
        val file = File(compose.activity.cacheDir, "terminal-image-drafts/test-$id.jpg")
            .apply {
                parentFile?.mkdirs()
                val bitmap = Bitmap.createBitmap(80, 40, Bitmap.Config.ARGB_8888)
                outputStream().use { output ->
                    check(bitmap.compress(Bitmap.CompressFormat.JPEG, 90, output))
                }
                bitmap.recycle()
            }
        return NormalizedTerminalImage(
            id = id,
            file = file,
            width = 80,
            height = 40,
            length = file.length(),
            sha256 = ByteArray(32) { index -> (id.hashCode() + index).toByte() },
        )
    }

    private fun fakeNormalizer() = TerminalImageNormalization { uri ->
        Result.success(normalizedImage(uri.lastPathSegment ?: "selected"))
    }

    private class FakeTerminalImagePickerLauncher : TerminalImagePickerLauncher {
        var galleryResult: TerminalImagePickerResult = TerminalImagePickerResult.Cancelled
        var cameraResult: TerminalImagePickerResult = TerminalImagePickerResult.Cancelled

        override fun launch(
            source: TerminalImageSource,
            remainingSlots: Int,
            destinationTabId: String,
            onResult: (TerminalImagePickerResult) -> Unit,
        ) {
            onResult(if (source == TerminalImageSource.Camera) cameraResult else galleryResult)
        }
    }
}
