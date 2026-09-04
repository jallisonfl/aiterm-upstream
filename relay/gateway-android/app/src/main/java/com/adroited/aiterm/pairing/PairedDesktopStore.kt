package com.adroited.aiterm.pairing

import android.annotation.SuppressLint
import android.content.Context
import android.content.SharedPreferences
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json

interface PairedDesktopStore {
    fun all(): List<PairedDesktop>
    fun save(desktop: PairedDesktop)
    fun remove(deviceId: String)
}

class PairedDesktopStoreException(message: String, cause: Throwable? = null) :
    IllegalStateException(message, cause)

object PairedDesktopJson {
    private val json = Json {
        encodeDefaults = true
        explicitNulls = true
        ignoreUnknownKeys = false
    }

    fun encode(desktops: List<PairedDesktop>): String {
        validate(desktops)
        return json.encodeToString(
            Document.serializer(),
            Document(version = 1, desktops = desktops),
        )
    }

    fun decode(value: String): List<PairedDesktop> {
        val document = try {
            json.decodeFromString(Document.serializer(), value)
        } catch (error: SerializationException) {
            throw PairedDesktopStoreException("paired desktop storage is corrupt", error)
        } catch (error: IllegalArgumentException) {
            throw PairedDesktopStoreException("paired desktop storage is corrupt", error)
        }
        if (document.version != 1) {
            throw PairedDesktopStoreException("paired desktop storage has an unsupported version")
        }
        validate(document.desktops)
        return document.desktops
    }

    private fun validate(desktops: List<PairedDesktop>) {
        if (desktops.size > 64 || desktops.map { it.deviceId }.toSet().size != desktops.size) {
            throw PairedDesktopStoreException("paired desktop storage contains invalid records")
        }
        desktops.forEach { desktop ->
            if (
                desktop.deviceId.isBlank() ||
                desktop.deviceId.length > 128 ||
                desktop.deviceId.any(Char::isISOControl) ||
                desktop.displayName.isBlank() ||
                desktop.displayName.length > 128 ||
                desktop.displayName.any(Char::isISOControl) ||
                desktop.hosts.isEmpty() ||
                desktop.hosts.size > 8 ||
                desktop.hosts.any { !PairingPayload.isValidHost(it) } ||
                desktop.port !in 1..65_535 ||
                !PairingPayload.isValidFingerprint(desktop.serverSpkiFingerprint) ||
                desktop.lastSeenEpochMillis?.let { it < 0 } == true ||
                ((desktop.relayHost == null) != (desktop.relayPort == null)) ||
                desktop.relayHost?.let { !PairingPayload.isValidHost(it) } == true ||
                desktop.relayPort?.let { it !in 1..65_535 } == true
            ) {
                throw PairedDesktopStoreException("paired desktop storage contains invalid records")
            }
        }
    }

    @Serializable
    private data class Document(
        val version: Int,
        @SerialName("desktops") val desktops: List<PairedDesktop>,
    )
}

class SharedPreferencesPairedDesktopStore(private val preferences: SharedPreferences) :
    PairedDesktopStore {

    constructor(context: Context) : this(
        context.getSharedPreferences(PREFERENCES_NAME, Context.MODE_PRIVATE),
    )

    @Synchronized
    override fun all(): List<PairedDesktop> {
        val encoded = try {
            preferences.getString(RECORDS_KEY, null)
        } catch (error: RuntimeException) {
            throw PairedDesktopStoreException("paired desktop storage could not be read", error)
        } ?: return emptyList()
        return PairedDesktopJson.decode(encoded)
    }

    @Synchronized
    override fun save(desktop: PairedDesktop) {
        val updated = all().filterNot { it.deviceId == desktop.deviceId } + desktop
        persist(updated)
    }

    @Synchronized
    override fun remove(deviceId: String) {
        persist(all().filterNot { it.deviceId == deviceId })
    }

    @SuppressLint("ApplySharedPref", "UseKtx")
    private fun persist(desktops: List<PairedDesktop>) {
        val encoded = PairedDesktopJson.encode(desktops)
        // Pairing cannot be reported as successful until persistence is known
        // to have completed; apply() has no failure result and is unsuitable.
        val committed = try {
            preferences.edit().putString(RECORDS_KEY, encoded).commit()
        } catch (error: RuntimeException) {
            throw PairedDesktopStoreException("paired desktop storage could not be written", error)
        }
        if (!committed) {
            throw PairedDesktopStoreException("paired desktop storage could not be written")
        }
    }

    companion object {
        private const val PREFERENCES_NAME = "paired_desktops"
        internal const val RECORDS_KEY = "records"
    }
}
