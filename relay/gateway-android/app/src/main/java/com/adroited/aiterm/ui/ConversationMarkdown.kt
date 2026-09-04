package com.adroited.aiterm.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.withLink
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** The readable subset of 5lime's renderer: headings, lists, quotes, code, tables and links. */
@Composable
internal fun ConversationMarkdown(
    text: String,
    color: Color = MaterialTheme.colorScheme.onSurface,
) {
    val blocks = remember(text) { splitConversationBlocks(text) }
    Column {
        blocks.forEach { block ->
            when (block) {
                is ConversationBlock.Code -> Text(
                    block.text,
                    fontFamily = FontFamily.Monospace,
                    fontSize = 12.sp,
                    color = color,
                    modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp)
                        .background(MaterialTheme.colorScheme.surfaceVariant, RoundedCornerShape(6.dp))
                        .horizontalScroll(rememberScrollState()).padding(8.dp),
                    softWrap = false,
                )
                is ConversationBlock.Quote -> Row(Modifier.padding(vertical = 2.dp).height(IntrinsicSize.Min)) {
                    Box(
                        Modifier.width(3.dp).fillMaxHeight()
                            .background(MaterialTheme.colorScheme.outline, RoundedCornerShape(2.dp)),
                    )
                    Spacer(Modifier.width(8.dp))
                    Text(
                        conversationInline(block.text),
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        fontStyle = FontStyle.Italic,
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
                ConversationBlock.Rule -> HorizontalDivider(
                    Modifier.padding(vertical = 6.dp),
                    color = MaterialTheme.colorScheme.outlineVariant,
                )
                is ConversationBlock.Paragraph -> Text(
                    conversationInline(block.text),
                    color = color,
                    style = MaterialTheme.typography.bodyMedium,
                    fontWeight = if (block.heading > 0) FontWeight.SemiBold else null,
                    fontSize = when (block.heading) {
                        1 -> 19.sp
                        2 -> 17.sp
                        3 -> 15.sp
                        else -> 14.sp
                    },
                    modifier = Modifier.padding(vertical = if (block.heading > 0) 4.dp else 2.dp),
                )
            }
        }
    }
}

private sealed interface ConversationBlock {
    data class Paragraph(val text: String, val heading: Int = 0) : ConversationBlock
    data class Quote(val text: String) : ConversationBlock
    data class Code(val text: String) : ConversationBlock
    data object Rule : ConversationBlock
}

private fun splitConversationBlocks(text: String): List<ConversationBlock> {
    val output = mutableListOf<ConversationBlock>()
    val paragraph = StringBuilder()
    var code: StringBuilder? = null
    var table: StringBuilder? = null
    fun flushParagraph() {
        if (paragraph.isNotBlank()) output += ConversationBlock.Paragraph(paragraph.toString().trimEnd())
        paragraph.clear()
    }
    fun flushTable() {
        table?.let { output += ConversationBlock.Code(it.toString().trimEnd()) }
        table = null
    }
    for (raw in text.lines()) {
        val line = raw.trimEnd()
        if (line.trimStart().startsWith("```")) {
            flushTable()
            if (code == null) {
                flushParagraph()
                code = StringBuilder()
            } else {
                output += ConversationBlock.Code(code.toString().trimEnd())
                code = null
            }
            continue
        }
        if (code != null) {
            code.append(raw).append('\n')
            continue
        }
        if (line.trimStart().startsWith("|")) {
            flushParagraph()
            if (!Regex("^\\|[-\\s|:]+\\|?$").matches(line.trim())) {
                (table ?: StringBuilder().also { table = it }).append(line.trim()).append('\n')
            }
            continue
        }
        flushTable()
        val heading = Regex("^(#{1,6})\\s+(.*)").find(line)
        when {
            heading != null -> {
                flushParagraph()
                output += ConversationBlock.Paragraph(
                    heading.groupValues[2],
                    heading = heading.groupValues[1].length,
                )
            }
            Regex("^\\s*([-*_])\\s*\\1\\s*\\1[-*_\\s]*$").matches(line) -> {
                flushParagraph()
                output += ConversationBlock.Rule
            }
            line.startsWith("> ") || line == ">" -> {
                flushParagraph()
                output += ConversationBlock.Quote(line.removePrefix(">").trimStart())
            }
            line.isBlank() -> flushParagraph()
            else -> {
                val bullet = Regex("^(\\s*)[-*]\\s+(.*)").find(line)
                val numbered = Regex("^(\\s*)(\\d+)[.)]\\s+(.*)").find(line)
                val shown = when {
                    bullet != null -> bullet.groupValues[1] + "• " + bullet.groupValues[2]
                    numbered != null -> numbered.groupValues[1] + numbered.groupValues[2] + ". " + numbered.groupValues[3]
                    else -> line
                }
                if (paragraph.isNotEmpty()) paragraph.append('\n')
                paragraph.append(shown)
            }
        }
    }
    code?.let { output += ConversationBlock.Code(it.toString().trimEnd()) }
    flushTable()
    flushParagraph()
    return output
}

private val CONVERSATION_INLINE = Regex(
    "(`[^`]+`)" +
        "|(\\*\\*\\*[^*]+\\*\\*\\*)" +
        "|(\\*\\*[^*]+\\*\\*)" +
        "|(\\*[^*\\s][^*]*\\*)" +
        "|(~~[^~]+~~)" +
        "|(\\[[^\\]]+]\\([^)\\s]+\\))" +
        "|(https?://[^\\s<>\"]+)",
)

private fun conversationInline(text: String): AnnotatedString = buildAnnotatedString {
    var index = 0
    for (match in CONVERSATION_INLINE.findAll(text)) {
        if (match.range.first < index) continue
        append(text.substring(index, match.range.first))
        val token = match.value
        when {
            token.startsWith("`") -> withStyle(
                SpanStyle(
                    fontFamily = FontFamily.Monospace,
                    background = Color(0x332B90A8),
                    fontSize = 13.sp,
                ),
            ) { append(token.trim('`')) }
            token.startsWith("***") -> withStyle(
                SpanStyle(fontWeight = FontWeight.Bold, fontStyle = FontStyle.Italic),
            ) { append(token.removeSurrounding("***")) }
            token.startsWith("**") -> withStyle(SpanStyle(fontWeight = FontWeight.Bold)) {
                append(token.removeSurrounding("**"))
            }
            token.startsWith("~~") -> withStyle(SpanStyle(textDecoration = TextDecoration.LineThrough)) {
                append(token.removeSurrounding("~~"))
            }
            token.startsWith("[") -> {
                val label = token.substringAfter('[').substringBefore(']')
                val url = token.substringAfter('(').substringBeforeLast(')')
                withLink(
                    LinkAnnotation.Url(
                        url,
                        TextLinkStyles(SpanStyle(color = Color(0xFF2B90A8), textDecoration = TextDecoration.Underline)),
                    ),
                ) { append(label) }
            }
            token.startsWith("http") -> {
                val url = token.trimEnd('.', ',', ')', ']', ';')
                withLink(
                    LinkAnnotation.Url(
                        url,
                        TextLinkStyles(SpanStyle(color = Color(0xFF2B90A8), textDecoration = TextDecoration.Underline)),
                    ),
                ) { append(url) }
                if (url.length < token.length) append(token.substring(url.length))
            }
            else -> withStyle(SpanStyle(fontStyle = FontStyle.Italic)) {
                append(token.removeSurrounding("*"))
            }
        }
        index = match.range.last + 1
    }
    append(text.substring(index))
}
