package com.adroited.aiterm.ui

import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.platform.LocalContext
import androidx.core.content.FileProvider
import java.io.File
import java.nio.file.Files
import java.nio.file.LinkOption
import java.util.UUID

enum class TerminalImageSource { Camera, Gallery }

sealed interface TerminalImagePickerResult {
    data class Selected(
        val uris: List<Uri>,
        val ownedCaptureFiles: Set<File> = emptySet(),
    ) : TerminalImagePickerResult

    data class Failed(val message: String) : TerminalImagePickerResult
    data object Cancelled : TerminalImagePickerResult
}

fun interface TerminalImagePickerLauncher {
    fun launch(
        source: TerminalImageSource,
        remainingSlots: Int,
        destinationTabId: String,
        onResult: (TerminalImagePickerResult) -> Unit,
    )
}

fun interface TerminalImageNormalization {
    suspend fun normalize(uri: Uri): Result<NormalizedTerminalImage>
}

/** Native system pickers only: Photo Picker for media and TakePicture for app-private capture. */
@Composable
internal fun rememberTerminalImagePicker(
    onResult: (String, TerminalImagePickerResult) -> Unit,
): TerminalImagePickerLauncher {
    val context = LocalContext.current
    val currentOnResult by rememberUpdatedState(onResult)
    var pendingTabId by rememberSaveable { mutableStateOf<String?>(null) }
    var pendingCapturePath by rememberSaveable { mutableStateOf<String?>(null) }

    fun publish(result: TerminalImagePickerResult) {
        val tabId = pendingTabId
        pendingTabId = null
        if (tabId != null) currentOnResult(tabId, result)
    }

    LaunchedEffect(context.cacheDir, pendingCapturePath) {
        val directory = File(context.cacheDir, CAPTURE_DIRECTORY)
        cleanupExpiredTerminalCaptures(directory, pendingCapturePath, System.currentTimeMillis())
    }

    val singleGalleryLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.PickVisualMedia(),
    ) { uri ->
        publish(
            if (uri == null) TerminalImagePickerResult.Cancelled
            else TerminalImagePickerResult.Selected(listOf(uri)),
        )
    }
    val galleryLaunchers = (2..TerminalAttachmentDraft.MAX_IMAGES).map { maximum ->
        rememberLauncherForActivityResult(
            ActivityResultContracts.PickMultipleVisualMedia(maximum),
        ) { uris ->
            publish(
                if (uris.isEmpty()) TerminalImagePickerResult.Cancelled
                else TerminalImagePickerResult.Selected(uris.distinct()),
            )
        }
    }
    val cameraLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.TakePicture(),
    ) { captured ->
        val file = pendingCapturePath?.let(::File)
        pendingCapturePath = null
        if (!captured || file == null || !file.isFile || file.length() == 0L) {
            file?.delete()
            publish(TerminalImagePickerResult.Cancelled)
        } else {
            publish(
                TerminalImagePickerResult.Selected(
                    uris = listOf(
                        FileProvider.getUriForFile(
                            context,
                            "${context.packageName}.terminal-images",
                            file,
                        ),
                    ),
                    ownedCaptureFiles = setOf(file),
                ),
            )
        }
    }

    return remember(context, singleGalleryLauncher, galleryLaunchers, cameraLauncher) {
        TerminalImagePickerLauncher { source, requestedSlots, destinationTabId, callback ->
            if (pendingTabId != null) {
                callback(TerminalImagePickerResult.Failed("Finish choosing the current image first."))
                return@TerminalImagePickerLauncher
            }
            val slots = requestedSlots.coerceIn(1, TerminalAttachmentDraft.MAX_IMAGES)
            pendingTabId = destinationTabId
            try {
                when (source) {
                    TerminalImageSource.Gallery -> {
                        val request = PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly)
                        if (slots == 1) singleGalleryLauncher.launch(request)
                        else galleryLaunchers[slots - 2].launch(request)
                    }
                    TerminalImageSource.Camera -> {
                        val directory = File(context.cacheDir, CAPTURE_DIRECTORY).apply {
                            if (!exists() && !mkdirs()) error("capture directory is unavailable")
                        }
                        val file = File(directory, "${UUID.randomUUID()}.jpg")
                        if (!file.createNewFile()) error("capture file is unavailable")
                        pendingCapturePath = file.path
                        cameraLauncher.launch(
                            FileProvider.getUriForFile(
                                context,
                                "${context.packageName}.terminal-images",
                                file,
                            ),
                        )
                    }
                }
            } catch (_: Exception) {
                pendingCapturePath?.let(::File)?.delete()
                pendingCapturePath = null
                publish(TerminalImagePickerResult.Failed("Could not open the image source. Try again."))
            }
        }
    }
}

private const val CAPTURE_DIRECTORY = "terminal-image-captures"
internal const val TERMINAL_CAPTURE_TTL_MILLIS = 24L * 60L * 60L * 1_000L
private const val MAX_CAPTURE_CLEANUP_FILES = 64
private val GENERATED_CAPTURE_NAME = Regex(
    "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\\.jpg$",
)

/** Deletes only the oldest expired files from this picker's exact private capture namespace. */
internal fun cleanupExpiredTerminalCaptures(
    root: File,
    protectedPath: String?,
    nowMillis: Long,
): Int {
    val rootPath = root.toPath()
    if (Files.isSymbolicLink(rootPath) ||
        !Files.isDirectory(rootPath, LinkOption.NOFOLLOW_LINKS) ||
        nowMillis < TERMINAL_CAPTURE_TTL_MILLIS
    ) return 0
    val protected = protectedPath?.let { File(it).toPath().toAbsolutePath().normalize() }
    val expiredBeforeOrAt = nowMillis - TERMINAL_CAPTURE_TTL_MILLIS
    val candidates = try {
        root.listFiles()?.asSequence().orEmpty()
            .filter { file ->
                val path = file.toPath()
                GENERATED_CAPTURE_NAME.matches(file.name) &&
                    path.toAbsolutePath().normalize() != protected &&
                    !Files.isSymbolicLink(path) &&
                    Files.isRegularFile(path, LinkOption.NOFOLLOW_LINKS)
            }
            .mapNotNull { file ->
                runCatching {
                    file to Files.getLastModifiedTime(
                        file.toPath(),
                        LinkOption.NOFOLLOW_LINKS,
                    ).toMillis()
                }.getOrNull()
            }
            .filter { (_, modified) -> modified <= expiredBeforeOrAt }
            .sortedWith(compareBy<Pair<File, Long>> { it.second }.thenBy { it.first.name })
            .take(MAX_CAPTURE_CLEANUP_FILES)
            .map { it.first }
            .toList()
    } catch (_: Exception) {
        return 0
    }
    var deleted = 0
    for (file in candidates) {
        val path = file.toPath()
        val stillOwned = runCatching {
            !Files.isSymbolicLink(rootPath) &&
                Files.isDirectory(rootPath, LinkOption.NOFOLLOW_LINKS) &&
                GENERATED_CAPTURE_NAME.matches(file.name) &&
                path.toAbsolutePath().normalize() != protected &&
                !Files.isSymbolicLink(path) &&
                Files.isRegularFile(path, LinkOption.NOFOLLOW_LINKS) &&
                Files.getLastModifiedTime(path, LinkOption.NOFOLLOW_LINKS).toMillis() <=
                expiredBeforeOrAt
        }.getOrDefault(false)
        if (stillOwned && runCatching { Files.deleteIfExists(path) }.getOrDefault(false)) deleted += 1
    }
    return deleted
}
