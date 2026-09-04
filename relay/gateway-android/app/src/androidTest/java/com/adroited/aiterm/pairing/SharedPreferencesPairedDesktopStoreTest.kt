package com.adroited.aiterm.pairing

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class SharedPreferencesPairedDesktopStoreTest {

    @Test
    fun privateStore_roundTripsMetadataAndSurfacesCorruption() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val preferences = context.getSharedPreferences(
            "paired-desktop-instrumentation-${System.nanoTime()}",
            Context.MODE_PRIVATE,
        )
        val store = SharedPreferencesPairedDesktopStore(preferences)
        val desktop = PairedDesktop(
            deviceId = "device-7",
            displayName = "Workshop PC",
            hosts = listOf("192.168.1.7"),
            port = 8443,
            serverSpkiFingerprint = java.util.Base64.getUrlEncoder().withoutPadding()
                .encodeToString(ByteArray(32) { 7 }),
            lastSeenEpochMillis = null,
        )

        try {
            store.save(desktop)
            assertEquals(listOf(desktop), store.all())

            check(
                preferences.edit()
                    .putString(SharedPreferencesPairedDesktopStore.RECORDS_KEY, "not-json")
                    .commit(),
            )
            assertThrows(PairedDesktopStoreException::class.java) { store.all() }
        } finally {
            preferences.edit().clear().commit()
        }
    }
}
