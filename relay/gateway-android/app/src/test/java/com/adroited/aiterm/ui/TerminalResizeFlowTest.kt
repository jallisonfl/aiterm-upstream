package com.adroited.aiterm.ui

import com.adroited.aiterm.remote.TerminalSize
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.toList
import kotlinx.coroutines.launch
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class TerminalResizeFlowTest {
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
        runCurrent()
        assertEquals(listOf(TerminalSize(80, 12)), seen)
    }

    @Test
    fun repeatedStableMeasurementsPublishOnlyOnce() = runTest {
        val source = MutableSharedFlow<TerminalSize>(extraBufferCapacity = 2)
        val seen = mutableListOf<TerminalSize>()
        backgroundScope.launch(UnconfinedTestDispatcher(testScheduler)) {
            source.settledTerminalSizes().toList(seen)
        }

        source.tryEmit(TerminalSize(80, 24))
        advanceTimeBy(150)
        source.tryEmit(TerminalSize(80, 24))
        advanceTimeBy(150)

        assertEquals(listOf(TerminalSize(80, 24)), seen)
    }

    @Test
    fun returningToTheLastPublishedSizeWithinTheSettlingWindowDoesNotRepublishIt() = runTest {
        val source = MutableSharedFlow<TerminalSize>(extraBufferCapacity = 3)
        val seen = mutableListOf<TerminalSize>()
        backgroundScope.launch(UnconfinedTestDispatcher(testScheduler)) {
            source.settledTerminalSizes().toList(seen)
        }

        source.tryEmit(TerminalSize(80, 24))
        advanceTimeBy(150)
        runCurrent()
        source.tryEmit(TerminalSize(80, 20))
        advanceTimeBy(50)
        source.tryEmit(TerminalSize(80, 24))
        advanceTimeBy(150)
        runCurrent()

        assertEquals(listOf(TerminalSize(80, 24)), seen)
    }
}
