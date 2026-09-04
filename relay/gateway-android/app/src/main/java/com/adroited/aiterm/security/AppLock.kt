package com.adroited.aiterm.security

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

class AppLock(private val clock: () -> Long) {

    constructor() : this(clock = android.os.SystemClock::elapsedRealtime)

    private val mutableLocked = MutableStateFlow(false)
    val isLocked: StateFlow<Boolean> = mutableLocked.asStateFlow()

    private var backgroundedAtMillis: Long? = null

    @Synchronized
    fun onEnterBackground() {
        if (backgroundedAtMillis == null) {
            backgroundedAtMillis = clock()
        }
    }

    @Synchronized
    fun onEnterForeground() {
        val started = backgroundedAtMillis ?: return
        backgroundedAtMillis = null
        val elapsed = clock() - started
        if (elapsed < 0 || elapsed >= BACKGROUND_LOCK_TIMEOUT_MILLIS) {
            mutableLocked.value = true
        }
    }

    @Synchronized
    fun unlock() {
        backgroundedAtMillis = null
        mutableLocked.value = false
    }

    @Synchronized
    fun lockNow() {
        backgroundedAtMillis = null
        mutableLocked.value = true
    }

    /** Linearization gate shared by every transition to the locked state. */
    @Synchronized
    fun signChallengeWhileUnlocked(signer: () -> ByteArray): ByteArray? {
        if (mutableLocked.value) return null
        return signer()
    }

    companion object {
        const val BACKGROUND_LOCK_TIMEOUT_MILLIS: Long = 5 * 60 * 1_000L
    }
}
