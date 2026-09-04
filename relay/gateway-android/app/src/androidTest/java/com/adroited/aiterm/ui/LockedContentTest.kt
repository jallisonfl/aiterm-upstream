package com.adroited.aiterm.ui

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.adroited.aiterm.testing.ComposeTestActivity
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class LockedContentTest {

    @get:Rule val compose = createAndroidComposeRule<ComposeTestActivity>()

    @Test
    fun lockedApp_hidesDesktopDataUntilAuthenticationStarts() {
        var requestedUnlock = false
        compose.setContent { LockedContent(onUnlock = { requestedUnlock = true }) }

        compose.onNodeWithText("AITerm is locked").assertIsDisplayed()
        compose.onNodeWithText("Unlock with a strong biometric or your device PIN.")
            .assertIsDisplayed()
        compose.onNodeWithText("Unlock AITerm").performClick()

        assertTrue(requestedUnlock)
    }
}
