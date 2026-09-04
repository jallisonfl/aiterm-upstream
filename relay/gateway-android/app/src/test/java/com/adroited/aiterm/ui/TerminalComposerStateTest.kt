package com.adroited.aiterm.ui

import androidx.compose.ui.text.input.TextFieldValue
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class TerminalComposerStateTest {
    @Test
    fun attachmentSubmissionFormatsTextPathsAndBracketsOnlyThePaste() {
        val pathA = "/project/.aiterm/attachments/a.jpg"
        val pathB = "/project/.aiterm/attachments/b.jpg"

        assertEquals(
            listOf(
                "\u001b[200~Describe the issue\n\nAttached images:\n- $pathA\n- $pathB\u001b[201~",
                "\r",
            ),
            formatTerminalSubmission("Describe the issue", listOf(pathA, pathB), bracketedPaste = true),
        )
    }

    @Test
    fun attachmentOnlySubmissionUsesAnExplicitPrompt() {
        assertEquals(
            listOf(
                "Please inspect the attached image(s):\n\nAttached images:\n- /tmp/one.jpg",
                "\r",
            ),
            formatTerminalSubmission("", listOf("/tmp/one.jpg")),
        )
    }

    @Test
    fun textOnlySubmissionKeepsExistingTerminalBehavior() {
        assertEquals(
            listOf("hello", "\r"),
            formatTerminalSubmission("hello", emptyList(), bracketedPaste = false),
        )
        assertEquals(
            listOf("\u001b[200~hello\u001b[201~", "\r"),
            formatTerminalSubmission("hello", emptyList(), bracketedPaste = true),
        )
    }

    @Test
    fun textDraftSurvivesClosingTheOverlayUntilItIsSent() {
        val initial = TerminalComposerState()
        assertFalse(initial.expanded)

        val opened = initial.open()
        val drafted = opened.updateValue(TextFieldValue("hello phone"))

        assertEquals(emptyList<String>(), drafted.outbound)
        assertEquals("hello phone", drafted.state.value.text)

        val closed = drafted.state.close()
        assertFalse(closed.expanded)
        assertEquals("hello phone", closed.value.text)

        val sent = closed.open().sendText()
        assertEquals(listOf("hello phone", "\r"), sent.outbound)
        assertFalse(sent.state.expanded)
        assertEquals("", sent.state.value.text)
    }

    @Test
    fun emptyTextSubmissionStillSendsTheTerminalEnterAction() {
        val sent = TerminalComposerState().open().sendText()

        assertEquals(listOf("\r"), sent.outbound)
        assertFalse(sent.state.expanded)
    }

    @Test
    fun composerHasOneAutocorrectableTextDraft() {
        val typed = TerminalComposerState().open()
            .updateValue(TextFieldValue("correct this"))

        assertEquals("correct this", typed.state.value.text)
        assertEquals(emptyList<String>(), typed.outbound)
        assertTrue(typed.state.expanded)
    }
}
