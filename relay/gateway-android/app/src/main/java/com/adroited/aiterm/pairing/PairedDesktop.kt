package com.adroited.aiterm.pairing

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * A desktop this phone has already enrolled with.
 *
 * Only non-secret metadata lives here. The device private key and the desktop
 * credential stay in Android Keystore (Task 8); [serverSpkiFingerprint] is the
 * pinned SHA-256 SPKI fingerprint from the pairing QR and is safe to display.
 */
@Serializable
data class PairedDesktop(
    @SerialName("device_id")
    val deviceId: String,
    @SerialName("display_name")
    val displayName: String,
    val hosts: List<String>,
    val port: Int,
    @SerialName("server_spki_fingerprint")
    val serverSpkiFingerprint: String,
    @SerialName("last_seen_epoch_millis")
    val lastSeenEpochMillis: Long?,
    @SerialName("relay_host")
    val relayHost: String? = null,
    @SerialName("relay_port")
    val relayPort: Int? = null,
)
