package com.adroited.aiterm.ui

import com.adroited.aiterm.remote.TerminalSize
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class TerminalViewportSizingTest {
    @Test
    fun sideObstructionsAreExcludedFromAdvertisedColumns() {
        val size = terminalViewportSizePx(
            viewportWidthPx = 211,
            viewportHeightPx = 200,
            leftObstructionPx = 13,
            rightObstructionPx = 17,
            bottomObstructionPx = 0,
            horizontalPaddingPx = 6,
            verticalPaddingPx = 5,
            cellWidthPx = 10,
            lineHeightPx = 20,
        )

        assertEquals(TerminalSize(cols = 16, rows = 9), size)
        val unsafeFullWidthColumns = (211 - 2 * 6) / 10
        assertTrue(
            "fixture must distinguish safe columns from full-width columns",
            size.cols < unsafeFullWidthColumns,
        )
    }

    @Test
    fun fractionalDensityRoundedHorizontalPaddingDoesNotAdvertiseABoundaryColumn() {
        val size = terminalViewportSizePx(
            viewportWidthPx = 119,
            viewportHeightPx = 100,
            leftObstructionPx = 0,
            rightObstructionPx = 0,
            bottomObstructionPx = 0,
            horizontalPaddingPx = 5,
            verticalPaddingPx = 0,
            cellWidthPx = 10,
            lineHeightPx = 20,
        )

        // At density 1.125, each 4 dp layout padding rounds from 4.5 to 5 px.
        assertEquals(TerminalSize(cols = 10, rows = 5), size)
    }

    @Test
    fun fractionalDensityRoundedVerticalPaddingKeepsABoundaryRow() {
        val size = terminalViewportSizePx(
            viewportWidthPx = 100,
            viewportHeightPx = 165,
            leftObstructionPx = 0,
            rightObstructionPx = 0,
            bottomObstructionPx = 100,
            horizontalPaddingPx = 0,
            verticalPaddingPx = 5,
            cellWidthPx = 10,
            lineHeightPx = 20,
        )

        // At density 1.5, 3 dp layout padding rounds from 4.5 to 5 px.
        assertEquals(TerminalSize(cols = 10, rows = 3), size)
    }
}
