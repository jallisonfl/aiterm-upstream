package com.adroited.aiterm.ui

import android.content.Context
import android.content.SharedPreferences
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

class TerminalKeyBarPreference(private val preferences: SharedPreferences) {
    constructor(context: Context) : this(
        context.getSharedPreferences("terminal_ui", Context.MODE_PRIVATE),
    )

    private val mutableExpanded = MutableStateFlow(
        preferences.getBoolean(EXPANDED_KEY, true),
    )
    val expanded: StateFlow<Boolean> = mutableExpanded.asStateFlow()

    fun setExpanded(expanded: Boolean) {
        if (!preferences.edit().putBoolean(EXPANDED_KEY, expanded).commit()) return
        mutableExpanded.value = expanded
    }

    private companion object {
        const val EXPANDED_KEY = "extra_keys_expanded"
    }
}
