package com.adroited.aiterm.terminal

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

sealed interface TerminalColor {
    data object Default : TerminalColor
    data class Indexed(val index: Int) : TerminalColor
    data class Rgb(val red: Int, val green: Int, val blue: Int) : TerminalColor
}

data class CellAttributes(
    val bold: Boolean = false,
    val faint: Boolean = false,
    val italic: Boolean = false,
    val underline: Boolean = false,
    val inverse: Boolean = false,
    val hidden: Boolean = false,
    val strikethrough: Boolean = false,
)

data class ScreenCell(
    val text: String,
    val width: Int = 1,
    val continuation: Boolean = false,
    val foreground: TerminalColor = TerminalColor.Default,
    val background: TerminalColor = TerminalColor.Default,
    val attributes: CellAttributes = CellAttributes(),
)

data class ScreenRow(
    val cells: List<ScreenCell>,
    val wrapped: Boolean = false,
) {
    fun plainText(): String = cells.asSequence()
        .filterNot(ScreenCell::continuation)
        .joinToString(separator = "", transform = ScreenCell::text)
}

enum class CursorShape { Block, Beam, Underline }

data class CursorState(
    val col: Int,
    val row: Int,
    val visible: Boolean,
    val shape: CursorShape = CursorShape.Block,
)

data class TerminalModes(
    val applicationCursor: Boolean = false,
    val bracketedPaste: Boolean = false,
    val lineWrap: Boolean = false,
    val alternateScreen: Boolean = false,
)

data class ScreenSnapshot(
    val tabId: String,
    val revision: Long,
    val cols: Int,
    val rows: Int,
    val visible: List<ScreenRow>,
    val scrollback: List<ScreenRow> = emptyList(),
    val cursor: CursorState,
    val modes: TerminalModes = TerminalModes(),
)

data class RowPatch(val row: Int, val content: ScreenRow)

data class ScreenDiff(
    val tabId: String,
    val baseRevision: Long,
    val revision: Long,
    val rows: List<RowPatch>,
    val cursor: CursorState? = null,
    val modes: TerminalModes? = null,
)

interface TerminalScreenStore {
    val screen: StateFlow<ScreenSnapshot?>
    fun replace(snapshot: ScreenSnapshot)
    fun apply(diff: ScreenDiff): ApplyResult
    fun clear()
}

sealed interface ApplyResult {
    data object Applied : ApplyResult
    data object NeedsSnapshot : ApplyResult
}

class DefaultTerminalScreenStore : TerminalScreenStore {
    private val mutableScreen = MutableStateFlow<ScreenSnapshot?>(null)
    override val screen: StateFlow<ScreenSnapshot?> = mutableScreen.asStateFlow()

    @Synchronized
    override fun replace(snapshot: ScreenSnapshot) {
        require(snapshot.cols in 1..MAX_DIMENSION && snapshot.rows in 1..MAX_DIMENSION)
        require(snapshot.visible.size == snapshot.rows)
        require(snapshot.visible.all { it.cells.size <= snapshot.cols })
        require(snapshot.cursor.col in 0 until snapshot.cols)
        require(snapshot.cursor.row in 0 until snapshot.rows)
        mutableScreen.value = snapshot
    }

    @Synchronized
    override fun apply(diff: ScreenDiff): ApplyResult {
        val current = mutableScreen.value ?: return ApplyResult.NeedsSnapshot
        if (diff.tabId != current.tabId ||
            diff.baseRevision != current.revision ||
            diff.revision <= current.revision
        ) {
            return ApplyResult.NeedsSnapshot
        }
        val rowNumbers = diff.rows.map(RowPatch::row)
        if (rowNumbers.toSet().size != rowNumbers.size ||
            diff.rows.any { it.row !in 0 until current.rows || it.content.cells.size > current.cols } ||
            diff.cursor?.let { it.col !in 0 until current.cols || it.row !in 0 until current.rows } == true
        ) {
            return ApplyResult.NeedsSnapshot
        }
        val visible = current.visible.toMutableList()
        diff.rows.forEach { visible[it.row] = it.content }
        mutableScreen.value = current.copy(
            revision = diff.revision,
            visible = visible,
            cursor = diff.cursor ?: current.cursor,
            modes = diff.modes ?: current.modes,
        )
        return ApplyResult.Applied
    }

    @Synchronized
    override fun clear() {
        mutableScreen.value = null
    }

    private companion object {
        const val MAX_DIMENSION = 512
    }
}
