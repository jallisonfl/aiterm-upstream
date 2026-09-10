package com.fivelime.aiterm.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Arrangement
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
import androidx.compose.ui.layout.Layout
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.withLink
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** Enough markdown to read an agent's answer or a README: headings by
 *  level, bullets, fenced code, quotes, rules, tables — and inline code,
 *  bold, italics, strikethrough and tappable links. Anything else is shown
 *  as written.
 *
 *  Blocks are spaced the way a rendered markdown page is: a paragraph's
 *  worth of air between paragraphs, more above a heading than below it,
 *  list items tight within their list. A wall of text read on a phone
 *  with 2 dp between paragraphs is a wall; with the blank lines honoured
 *  it is an article. */
@Composable
fun MarkdownText(text: String, color: Color = MaterialTheme.colorScheme.onSurface) {
    val blocks = remember(text) { splitBlocks(text) }
    Column {
        var prev: Block? = null
        blocks.forEach { b ->
            val gap = gapBefore(prev, b)
            if (gap > 0.dp) Spacer(Modifier.height(gap))
            when (b) {
                is Block.Code -> Text(
                    b.text, fontFamily = FontFamily.Monospace, fontSize = 12.sp, color = color, lineHeight = 17.sp,
                    modifier = Modifier.fillMaxWidth()
                        .background(Bg, RoundedCornerShape(6.dp)).horizontalScroll(rememberScrollState())
                        .padding(10.dp),
                    softWrap = false,
                )
                is Block.Quote -> Row(Modifier.height(IntrinsicSize.Min)) {
                    Box(Modifier.width(3.dp).fillMaxHeight().background(Muted.copy(alpha = 0.5f), RoundedCornerShape(2.dp)))
                    Spacer(Modifier.width(10.dp))
                    Text(inline(b.text), color = Muted, fontStyle = FontStyle.Italic, style = MaterialTheme.typography.bodyMedium, lineHeight = 21.sp)
                }
                is Block.Rule -> HorizontalDivider(color = Muted.copy(alpha = 0.3f))
                is Block.Table -> MarkdownTable(b, color)
                is Block.ListBlock -> Column(verticalArrangement = Arrangement.spacedBy(3.dp)) {
                    b.items.forEach { it ->
                        // A hanging indent: the marker in its own column, the
                        // text beside it, wrapped lines staying under the text
                        // — an item three levels deep still reads as one.
                        Row(Modifier.padding(start = (it.level * 16).dp)) {
                            Text(
                                it.marker, color = color, style = MaterialTheme.typography.bodyMedium, lineHeight = 20.sp,
                                modifier = Modifier.width(if (it.marker.length > 1) 24.dp else 16.dp),
                            )
                            Text(
                                inline(it.text), color = color, style = MaterialTheme.typography.bodyMedium, lineHeight = 20.sp,
                                modifier = Modifier.weight(1f),
                            )
                        }
                    }
                }
                is Block.Para -> Text(
                    inline(b.text), color = color, style = MaterialTheme.typography.bodyMedium,
                    fontWeight = if (b.heading > 0) FontWeight.SemiBold else null,
                    fontSize = when (b.heading) {
                        1 -> 20.sp
                        2 -> 18.sp
                        3 -> 16.sp
                        else -> 14.sp
                    },
                    lineHeight = when (b.heading) {
                        1 -> 27.sp
                        2 -> 25.sp
                        3 -> 22.sp
                        else -> 21.sp
                    },
                )
            }
            prev = b
        }
    }
}

/** The air between two blocks. Headings pull toward what follows them
 *  (more above, less below); everything else gets a paragraph's worth. */
private fun gapBefore(prev: Block?, cur: Block): Dp {
    if (prev == null) return 0.dp
    if (cur is Block.Para && cur.heading > 0) return if (cur.heading == 1) 18.dp else 14.dp
    if (prev is Block.Para && prev.heading > 0) return 6.dp
    if (prev is Block.Rule || cur is Block.Rule) return 12.dp
    if (cur is Block.Code || prev is Block.Code || cur is Block.Table || prev is Block.Table) return 10.dp
    // A list right after the line that introduces it ("…they serve as
    // canopy engineers:") hangs closer than a fresh paragraph would.
    if (cur is Block.ListBlock && prev is Block.Para && prev.text.trimEnd().endsWith(":")) return 6.dp
    return 10.dp
}

private sealed class Block {
    data class Para(val text: String, val heading: Int = 0) : Block()
    data class ListBlock(val items: List<ListItem>) : Block()
    data class Quote(val text: String) : Block()
    data class Code(val text: String) : Block()
    data class Table(val header: List<String>, val rows: List<List<String>>, val align: List<TextAlign>) : Block()
    data object Rule : Block()
}

private data class ListItem(val level: Int, val marker: String, val text: String)

private val BULLET = Regex("^(\\s*)[-*+•]\\s+(.*)")
private val NUMBERED = Regex("^(\\s*)(\\d+)[.)]\\s+(.*)")

/** Nesting from leading whitespace. Agents indent by two or by four; either
 *  way one step is one level, and a tab is four. */
private fun level(ws: String): Int = (ws.replace("\t", "    ").length + 3) / 4

private val TABLE_SEP = Regex("^\\|?\\s*:?-+:?\\s*(\\|\\s*:?-+:?\\s*)*\\|?$")

/** One pipe-delimited row into its cells. A leading/trailing pipe is the
 *  frame, not a cell; `\|` inside a cell is a literal pipe. */
private fun cells(line: String): List<String> {
    val out = mutableListOf<String>()
    val cur = StringBuilder()
    var i = 0
    val s = line.trim()
    while (i < s.length) {
        val c = s[i]
        when {
            c == '\\' && i + 1 < s.length && s[i + 1] == '|' -> { cur.append('|'); i++ }
            c == '|' -> { out += cur.toString(); cur.clear() }
            else -> cur.append(c)
        }
        i++
    }
    out += cur.toString()
    if (s.startsWith("|")) out.removeAt(0)
    if (s.endsWith("|") && !s.endsWith("\\|") && out.isNotEmpty()) out.removeAt(out.size - 1)
    return out.map { it.trim() }
}

private fun splitBlocks(text: String): List<Block> {
    val out = mutableListOf<Block>()
    val para = StringBuilder()
    var code: StringBuilder? = null
    var table: MutableList<String>? = null
    val list = mutableListOf<ListItem>()
    fun flushList() { if (list.isNotEmpty()) out += Block.ListBlock(list.toList()); list.clear() }
    fun flush() {
        flushList()
        if (para.isNotBlank()) out += Block.Para(para.toString().trimEnd())
        para.clear()
    }
    fun flushTable() {
        val t = table ?: return
        table = null
        val rows = t.map(::cells)
        val sepAt = rows.indices.firstOrNull { i -> i > 0 && TABLE_SEP.matches(t[i].trim()) }
        if (sepAt == null) {
            // Pipes without a header rule: not a table, keep the lines as written.
            out += Block.Code(t.joinToString("\n"))
            return
        }
        val header = rows[sepAt - 1]
        val align = rows[sepAt].map { c ->
            val l = c.startsWith(":"); val r = c.endsWith(":")
            when { l && r -> TextAlign.Center; r -> TextAlign.End; else -> TextAlign.Start }
        }
        val width = maxOf(header.size, align.size)
        fun pad(r: List<String>) = if (r.size >= width) r.take(width) else r + List(width - r.size) { "" }
        val body = rows.filterIndexed { i, _ -> i != sepAt && i != sepAt - 1 }.map(::pad)
        out += Block.Table(pad(header), body, (align + List(width) { TextAlign.Start }).take(width))
    }
    for (raw in text.lines()) {
        val line = raw.trimEnd()
        if (line.trimStart().startsWith("```")) {
            flushTable()
            if (code == null) { flush(); code = StringBuilder() } else { out += Block.Code(code.toString().trimEnd()); code = null }
            continue
        }
        if (code != null) { code.append(raw).append('\n'); continue }
        if (line.trimStart().startsWith("|")) {
            flush()
            (table ?: mutableListOf<String>().also { table = it }).add(line.trim())
            continue
        }
        flushTable()
        val h = Regex("^(#{1,6})\\s+(.*?)\\s*#*$").find(line)
        when {
            h != null -> { flush(); out += Block.Para(h.groupValues[2], heading = h.groupValues[1].length) }
            Regex("^\\s*([-*_])\\s*\\1\\s*\\1[-*_\\s]*$").matches(line) -> { flush(); out += Block.Rule }
            line.startsWith("> ") || line == ">" -> { flush(); out += Block.Quote(line.removePrefix(">").trimStart()) }
            // A blank line inside a list is the list's own air, not its end:
            // the next item continues it. Anything else ends it.
            line.isBlank() -> if (list.isEmpty()) flush() else if (para.isNotEmpty()) flush()
            else -> {
                val bullet = BULLET.find(line)
                val num = NUMBERED.find(line)
                when {
                    bullet != null -> {
                        if (para.isNotEmpty()) flush()
                        val lv = level(bullet.groupValues[1])
                        list += ListItem(lv, if (lv == 0) "•" else if (lv == 1) "◦" else "▪", bullet.groupValues[2])
                    }
                    num != null -> {
                        if (para.isNotEmpty()) flush()
                        list += ListItem(level(num.groupValues[1]), num.groupValues[2] + ".", num.groupValues[3])
                    }
                    // An indented line under an item is the item, wrapped by
                    // hand; it joins the item rather than starting a paragraph.
                    list.isNotEmpty() && raw.startsWith(" ") && para.isEmpty() -> {
                        val last = list.removeAt(list.size - 1)
                        list += last.copy(text = last.text + " " + line.trim())
                    }
                    else -> {
                        flushList()
                        if (para.isNotEmpty()) para.append('\n')
                        para.append(line)
                    }
                }
            }
        }
    }
    code?.let { out += Block.Code(it.toString().trimEnd()) }
    flushTable()
    flush()
    return out
}

private val CELL_MAX = 220.dp
private val CELL_MIN = 56.dp

/** A real grid: header bold above a rule, rows striped, columns as wide
 *  as their widest cell up to a cap (then the cell wraps). Wider than the
 *  screen scrolls sideways as one piece. */
@Composable
private fun MarkdownTable(t: Block.Table, color: Color) {
    val cols = t.header.size
    val all = listOf(t.header) + t.rows
    Box(Modifier.fillMaxWidth().horizontalScroll(rememberScrollState())) {
        Layout(
            content = {
                all.forEachIndexed { r, row ->
                    row.forEachIndexed { c, cell ->
                        Box(
                            Modifier.background(if (r == 0) Surface1 else if (r % 2 == 0) Surface1.copy(alpha = 0.35f) else Color.Transparent)
                                .padding(horizontal = 10.dp, vertical = 7.dp),
                        ) {
                            Text(
                                inline(cell), color = color, style = MaterialTheme.typography.bodySmall, lineHeight = 18.sp,
                                fontWeight = if (r == 0) FontWeight.SemiBold else null,
                                textAlign = t.align.getOrNull(c) ?: TextAlign.Start,
                                modifier = Modifier.fillMaxWidth(),
                            )
                        }
                    }
                }
            },
            modifier = Modifier.background(Surface1.copy(alpha = 0.25f), RoundedCornerShape(8.dp)),
        ) { measurables, _ ->
            val max = CELL_MAX.roundToPx(); val min = CELL_MIN.roundToPx()
            val widths = IntArray(cols)
            measurables.forEachIndexed { i, m -> val c = i % cols; widths[c] = maxOf(widths[c], m.maxIntrinsicWidth(Constraints.Infinity)) }
            for (c in 0 until cols) widths[c] = widths[c].coerceIn(min, max)
            // Row heights come from intrinsics before anything is measured,
            // so every cell can be measured to its row's full height and the
            // stripe runs unbroken across short cells.
            val rowH = IntArray(all.size)
            measurables.forEachIndexed { i, m -> val r = i / cols; rowH[r] = maxOf(rowH[r], m.minIntrinsicHeight(widths[i % cols])) }
            val placeables = measurables.mapIndexed { i, m -> m.measure(Constraints.fixed(widths[i % cols], rowH[i / cols])) }
            val w = widths.sum(); val h = rowH.sum()
            layout(w, h) {
                var y = 0
                for (r in all.indices) {
                    var x = 0
                    for (c in 0 until cols) {
                        val p = placeables[r * cols + c]
                        p.place(x, y)
                        x += widths[c]
                    }
                    y += rowH[r]
                }
            }
        }
    }
}

/** The inline HTML markdown allows and agents actually write — `<br>` in a
 *  table cell above all (114 of them in one Ads comparison, 2026-09-03),
 *  plus bold, italic, code and strike tags — turned into the markdown
 *  marks the renderer already knows; the common entities decoded. Anything
 *  else in angle brackets is left as written: `<USER_REQUEST>`, `List<T>`
 *  and a stray `<truncated …>` are text, not markup. */
private val HTML_BR = Regex("<br\\s*/?>", RegexOption.IGNORE_CASE)
private val HTML_MARK = Regex("</?(b|strong|i|em|code|s|del|strike|u|sub|sup)>", RegexOption.IGNORE_CASE)
private val ENTITY = Regex("&(amp|lt|gt|quot|apos|nbsp|#39|#x27);")
fun htmlToMarkdown(s: String): String {
    if ('<' !in s && '&' !in s) return s
    return s.replace(HTML_BR, "\n")
        .replace(HTML_MARK) { m ->
            when (m.groupValues[1].lowercase()) {
                "b", "strong" -> "**"
                "i", "em" -> "*"
                "code" -> "`"
                "s", "del", "strike" -> "~~"
                else -> "" // u, sub, sup: no mark here; the words stay
            }
        }
        .replace(ENTITY) { m ->
            when (m.groupValues[1]) {
                "amp" -> "&"; "lt" -> "<"; "gt" -> ">"; "quot" -> "\""
                "apos", "#39", "#x27" -> "'"; "nbsp" -> "\u00A0"
                else -> m.value
            }
        }
}

/** Inline spans: `code`, ***both***, **bold**, __bold__, *italic*, _italic_,
 *  ~~gone~~, and [links](https://…) that open in the browser. HTML that
 *  markdown allows inline is folded in first, see [htmlToMarkdown]. */
private val INLINE = Regex(
    "(`[^`]+`)" +
        "|(\\*\\*\\*[^*]+\\*\\*\\*)" +
        "|(\\*\\*[^*]+\\*\\*)" +
        "|(__[^_]+__)" +
        "|(\\*[^*\\s][^*]*\\*)" +
        "|((?<![\\w])_[^_\\s][^_]*_(?![\\w]))" +
        "|(~~[^~]+~~)" +
        "|(\\[[^\\]]+]\\((?:<[^>\\r\\n]+>|[^)\\s]+)\\))" +
        "|(https?://[^\\s<>\"]+)",
)

/** The text with its markdown marks taken off — for a one-line fold where
 *  `**Checking the request**` should read as its words. */
fun markdownPlain(s: String): String = htmlToMarkdown(s)
    .replace(Regex("(?m)^#{1,6}\\s+"), "")
    .replace(Regex("(?m)^\\s*[-*+]\\s+"), "• ")
    .replace(Regex("\\*{1,3}([^*]+)\\*{1,3}"), "$1")
    .replace(Regex("__([^_]+)__"), "$1")
    .replace(Regex("`([^`]+)`"), "$1")
    .replace(Regex("~~([^~]+)~~"), "$1")
    .trim()

private fun inline(raw: String): AnnotatedString = buildAnnotatedString {
    val s = htmlToMarkdown(raw)
    var i = 0
    for (m in INLINE.findAll(s)) {
        if (m.range.first < i) continue
        append(s.substring(i, m.range.first))
        val t = m.value
        when {
            t.startsWith("`") -> withStyle(SpanStyle(fontFamily = FontFamily.Monospace, background = Bg, fontSize = 13.sp)) { append(t.trim('`')) }
            t.startsWith("***") -> withStyle(SpanStyle(fontWeight = FontWeight.Bold, fontStyle = FontStyle.Italic)) { append(t.removeSurrounding("***")) }
            t.startsWith("**") -> withStyle(SpanStyle(fontWeight = FontWeight.Bold)) { append(t.removeSurrounding("**")) }
            t.startsWith("__") -> withStyle(SpanStyle(fontWeight = FontWeight.Bold)) { append(t.removeSurrounding("__")) }
            t.startsWith("~~") -> withStyle(SpanStyle(textDecoration = TextDecoration.LineThrough)) { append(t.removeSurrounding("~~")) }
            t.startsWith("[") -> {
                val label = t.substringAfter('[').substringBefore(']')
                // The target starts after `](`, not at the first `(` — a
                // label may hold one — and may sit in angle brackets when it
                // has spaces in it.
                val url = t.substringAfter("](").dropLast(1).removeSurrounding("<", ">")
                withLink(
                    LinkAnnotation.Url(url, TextLinkStyles(SpanStyle(color = Accent, textDecoration = TextDecoration.Underline))),
                ) { append(label) }
            }
            t.startsWith("http") -> {
                val url = t.trimEnd('.', ',', ')', ']', ';')
                withLink(
                    LinkAnnotation.Url(url, TextLinkStyles(SpanStyle(color = Accent, textDecoration = TextDecoration.Underline))),
                ) { append(url) }
                if (url.length < t.length) append(t.substring(url.length))
            }
            t.startsWith("_") -> withStyle(SpanStyle(fontStyle = FontStyle.Italic)) { append(t.removeSurrounding("_")) }
            else -> withStyle(SpanStyle(fontStyle = FontStyle.Italic)) { append(t.removeSurrounding("*")) }
        }
        i = m.range.last + 1
    }
    append(s.substring(i))
}
