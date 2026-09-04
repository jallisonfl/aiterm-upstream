package com.adroited.aiterm.ui

import android.content.Context
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertFalse
import org.junit.Test

class TerminalKeyBarPreferenceTest {
    @Test
    fun expandedChoiceIsRestoredByTheNextPreferenceInstance() {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val preferences = context.getSharedPreferences("terminal-key-bar-test", Context.MODE_PRIVATE)
        preferences.edit().clear().commit()
        try {
            TerminalKeyBarPreference(preferences).setExpanded(false)

            assertFalse(TerminalKeyBarPreference(preferences).expanded.value)
        } finally {
            preferences.edit().clear().commit()
        }
    }
}
