package com.adroited.aiterm.ui

import com.adroited.aiterm.remote.TerminalSize

internal fun terminalViewportSizePx(
    viewportWidthPx: Int,
    viewportHeightPx: Int,
    leftObstructionPx: Int,
    rightObstructionPx: Int,
    bottomObstructionPx: Int,
    horizontalPaddingPx: Int,
    verticalPaddingPx: Int,
    cellWidthPx: Int,
    lineHeightPx: Int,
): TerminalSize {
    val safeCellWidthPx = cellWidthPx.coerceAtLeast(1)
    val safeLineHeightPx = lineHeightPx.coerceAtLeast(1)
    val availableWidthPx = (
        viewportWidthPx - leftObstructionPx.coerceAtLeast(0) -
            rightObstructionPx.coerceAtLeast(0) - 2 * horizontalPaddingPx.coerceAtLeast(0)
    ).coerceAtLeast(safeCellWidthPx)
    val availableHeightPx = (
        viewportHeightPx - bottomObstructionPx.coerceAtLeast(0) -
            verticalPaddingPx.coerceAtLeast(0)
    ).coerceAtLeast(safeLineHeightPx)
    return TerminalSize(
        cols = (availableWidthPx / safeCellWidthPx).coerceIn(1, 512),
        rows = (availableHeightPx / safeLineHeightPx).coerceIn(1, 512),
    )
}
