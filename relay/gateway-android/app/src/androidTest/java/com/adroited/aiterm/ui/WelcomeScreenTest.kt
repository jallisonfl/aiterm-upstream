package com.adroited.aiterm.ui

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.adroited.aiterm.testing.ComposeTestActivity
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class WelcomeScreenTest {
    @get:Rule val compose = createAndroidComposeRule<ComposeTestActivity>()

    @Test
    fun lockedLaunchExplainsTheProductAndStartsUnlock() {
        var unlockRequests = 0
        compose.setContent {
            WelcomeScreen(onUnlock = { unlockRequests++ })
        }

        compose.onNodeWithText("Leave the desk.\nKeep the session.").assertIsDisplayed()
        compose.onNodeWithText("Continue live work").performScrollTo().assertIsDisplayed()
        compose.onNodeWithText("Send what the task needs").performScrollTo().assertIsDisplayed()
        compose.onNodeWithText("Connect your way").performScrollTo().assertIsDisplayed()
        compose.onNodeWithText("AITerm is locked").assertIsDisplayed()
        compose.onNodeWithText("Unlock AITerm").assertIsDisplayed().performClick()

        compose.runOnIdle { assertEquals(1, unlockRequests) }
    }

    @Test
    fun unlockFailureIsVisibleWithoutReplacingTheWelcome() {
        compose.setContent {
            WelcomeScreen(onUnlock = {}, unlockError = "Authentication was not recognized. Try again.")
        }

        compose.onNodeWithText("Leave the desk.\nKeep the session.").assertIsDisplayed()
        compose.onNodeWithText("Authentication was not recognized. Try again.").assertIsDisplayed()
    }
}
