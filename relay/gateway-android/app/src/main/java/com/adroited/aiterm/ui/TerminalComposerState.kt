package com.adroited.aiterm.ui

import androidx.compose.ui.text.input.TextFieldValue

internal data class TerminalComposerUpdate(
    val state: TerminalComposerState,
    val outbound: List<String> = emptyList(),
)

internal data class TerminalComposerState(
    val expanded: Boolean = false,
    val value: TextFieldValue = TextFieldValue(),
) {
    fun open(): TerminalComposerState = copy(expanded = true)

    fun close(): TerminalComposerState = copy(expanded = false)

    fun updateValue(next: TextFieldValue) = TerminalComposerUpdate(copy(value = next))

    fun sendText(bracketedPaste: Boolean = false): TerminalComposerUpdate {
        val outbound = formatTerminalSubmission(value.text, emptyList(), bracketedPaste)
        return TerminalComposerUpdate(
            state = copy(expanded = false, value = TextFieldValue()),
            outbound = outbound,
        )
    }
}

/** Formats terminal input after every attachment has reached the desktop. */
internal fun formatTerminalSubmission(
    text: String,
    paths: List<String>,
    bracketedPaste: Boolean = false,
): List<String> {
    val paste = when {
        paths.isEmpty() -> text
        text.isEmpty() -> buildString {
            append("Please inspect the attached image(s):\n\nAttached images:")
            paths.forEach { append("\n- ").append(it) }
        }
        else -> buildString {
            append(text).append("\n\nAttached images:")
            paths.forEach { append("\n- ").append(it) }
        }
    }
    return buildList {
        if (paste.isNotEmpty()) {
            add(if (bracketedPaste) "\u001b[200~$paste\u001b[201~" else paste)
        }
        add("\r")
    }
}
