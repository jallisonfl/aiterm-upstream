package com.adroited.aiterm.ui

import com.adroited.aiterm.remote.TerminalSize
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.flow.distinctUntilChanged

internal const val TERMINAL_RESIZE_SETTLE_MILLIS = 150L

@OptIn(FlowPreview::class)
internal fun Flow<TerminalSize>.settledTerminalSizes(): Flow<TerminalSize> =
    distinctUntilChanged()
        .debounce(TERMINAL_RESIZE_SETTLE_MILLIS)
        .distinctUntilChanged()
