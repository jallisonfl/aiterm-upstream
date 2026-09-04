package com.adroited.aiterm

import android.content.Context
import com.adroited.aiterm.pairing.OkHttpPairingTransport
import com.adroited.aiterm.pairing.PairingRepository
import com.adroited.aiterm.pairing.SharedPreferencesPairedDesktopStore
import com.adroited.aiterm.pairing.PairedDesktopStoreException
import com.adroited.aiterm.security.AndroidDeviceKeyStore
import com.adroited.aiterm.security.AppLock
import com.adroited.aiterm.ui.TerminalKeyBarPreference

/** Process-scoped dependencies; no pairing secret is ever retained here. */
class AppContainer(context: Context) {
    val terminalKeyBarPreference = TerminalKeyBarPreference(context.applicationContext)
    val pairedDesktopStore = SharedPreferencesPairedDesktopStore(context.applicationContext)
    val deviceKeys = AndroidDeviceKeyStore()
    val relayAuthorityKeys = AndroidDeviceKeyStore("aiterm-relay-authority-p256-v1")
    val pairingRepository = PairingRepository(
        transport = OkHttpPairingTransport(),
        deviceKeys = deviceKeys,
        relayAuthorityKeys = relayAuthorityKeys,
        store = pairedDesktopStore,
    )
    val appLock = AppLock().apply {
        // A process restart must not reveal a previously paired desktop merely
        // because the in-memory background timestamp died with the process.
        val hasPairedOrUnreadableData = try {
            pairedDesktopStore.all().isNotEmpty()
        } catch (_: PairedDesktopStoreException) {
            true
        }
        if (hasPairedOrUnreadableData) lockNow()
    }
}
