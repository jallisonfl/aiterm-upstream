package com.adroited.aiterm.security

import org.junit.Assert.assertFalse
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

/**
 * The five-minute background lock. The biometric prompt itself needs a device,
 * but the decision of *when* to demand it is plain arithmetic and is pinned
 * down here.
 */
class AppLockTest {

    private var now = 1_000_000L
    private val lock = AppLock(clock = { now })

    @Test
    fun freshlyStartedApp_isNotLocked() {
        assertFalse(lock.isLocked.value)
    }

    @Test
    fun fiveMinutesInTheBackground_locksTheApp() {
        lock.onEnterBackground()
        now += AppLock.BACKGROUND_LOCK_TIMEOUT_MILLIS
        lock.onEnterForeground()

        assertTrue(lock.isLocked.value)
    }

    @Test
    fun aShortTripToTheBackground_doesNotLockTheApp() {
        lock.onEnterBackground()
        now += AppLock.BACKGROUND_LOCK_TIMEOUT_MILLIS - 1
        lock.onEnterForeground()

        assertFalse(lock.isLocked.value)
    }

    @Test
    fun unlocking_clearsTheLockAndRestartsTheClock() {
        lock.onEnterBackground()
        now += AppLock.BACKGROUND_LOCK_TIMEOUT_MILLIS
        lock.onEnterForeground()
        lock.unlock()

        assertFalse(lock.isLocked.value)

        lock.onEnterBackground()
        now += 1_000
        lock.onEnterForeground()

        assertFalse(lock.isLocked.value)
    }

    @Test
    fun lockNow_locksWithoutWaiting() {
        lock.lockNow()

        assertTrue(lock.isLocked.value)
    }

    @Test
    fun repeatedForegrounding_withoutBackgrounding_doesNotLock() {
        lock.onEnterForeground()
        now += AppLock.BACKGROUND_LOCK_TIMEOUT_MILLIS * 10
        lock.onEnterForeground()

        assertFalse(lock.isLocked.value)
    }

    @Test
    fun monotonicClockMovingBackwards_locksFailClosed() {
        lock.onEnterBackground()
        now -= 1

        lock.onEnterForeground()

        assertTrue(lock.isLocked.value)
    }

    @Test
    fun lockTransitionAndKeystoreUseShareOneLinearGate() {
        val signerEntered = CountDownLatch(1)
        val releaseSigner = CountDownLatch(1)
        val lockRequested = CountDownLatch(1)
        val executor = Executors.newFixedThreadPool(2)
        try {
            val signature = executor.submit<ByteArray?> {
                lock.signChallengeWhileUnlocked {
                    signerEntered.countDown()
                    assertTrue(releaseSigner.await(1, TimeUnit.SECONDS))
                    byteArrayOf(1, 2, 3)
                }
            }
            assertTrue(signerEntered.await(1, TimeUnit.SECONDS))
            val locking = executor.submit {
                lockRequested.countDown()
                lock.lockNow()
            }
            assertTrue(lockRequested.await(1, TimeUnit.SECONDS))
            assertFalse(locking.isDone)

            releaseSigner.countDown()
            assertArrayEquals(byteArrayOf(1, 2, 3), signature.get(1, TimeUnit.SECONDS))
            locking.get(1, TimeUnit.SECONDS)
            assertTrue(lock.isLocked.value)
            assertTrue(lock.signChallengeWhileUnlocked { byteArrayOf(9) } == null)
        } finally {
            releaseSigner.countDown()
            executor.shutdownNow()
        }
    }
}
