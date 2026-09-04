package com.adroited.aiterm.ui

import android.content.ContentResolver
import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.ImageDecoder
import android.graphics.Matrix
import android.graphics.Paint
import android.graphics.Rect
import android.net.Uri
import android.os.Build
import androidx.exifinterface.media.ExifInterface
import java.io.BufferedOutputStream
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import java.nio.file.Files
import java.security.MessageDigest
import java.util.UUID
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withContext
import kotlin.coroutines.coroutineContext
import kotlin.math.ceil
import kotlin.math.max

data class NormalizedTerminalImage(
    val id: String,
    val file: File,
    val width: Int,
    val height: Int,
    val length: Long,
    val sha256: ByteArray,
)

class TerminalImageNormalizationError(
    val code: Code,
    message: String,
    cause: Throwable? = null,
) : IOException(message, cause) {
    enum class Code {
        CONTENT_UNAVAILABLE,
        EMPTY_CONTENT,
        INPUT_TOO_LARGE,
        SNAPSHOT_FAILED,
        DECODE_FAILED,
        DIMENSIONS_OUT_OF_BOUNDS,
        OUTPUT_TOO_LARGE,
        OUTPUT_FAILED,
        INCOMPATIBLE_OUTPUT,
    }
}

/**
 * The legacy decoder's power-of-two sampling is deliberately conservative: no API 26–27
 * allocation can exceed the final 4096×4096 budget, even when that trades a little detail.
 */
internal fun legacyTerminalImageSampleSize(width: Int, height: Int): Int {
    if (width !in 1..TerminalImageNormalizer.MAX_SOURCE_EDGE ||
        height !in 1..TerminalImageNormalizer.MAX_SOURCE_EDGE
    ) {
        throw TerminalImageNormalizationError(
            TerminalImageNormalizationError.Code.DIMENSIONS_OUT_OF_BOUNDS,
            "selected image dimensions exceed safe decode bounds",
        )
    }
    var sample = 1
    while (
        ceil(width.toDouble() / sample) > TerminalImageNormalizer.MAX_IMAGE_EDGE ||
        ceil(height.toDouble() / sample) > TerminalImageNormalizer.MAX_IMAGE_EDGE
    ) {
        sample *= 2
    }
    return sample
}

/**
 * Turns untrusted, transient picker or camera URIs into bounded app-private JPEG drafts.
 *
 * A provider URI is opened exactly once. The stream is copied under an input cap to a private
 * snapshot before its bounds, EXIF, or pixels are inspected, preventing a mutable provider from
 * swapping in different bytes between validation and decode.
 */
class TerminalImageNormalizer(
    context: Context,
    private val clockMillis: () -> Long = System::currentTimeMillis,
    private val maxOutputBytes: Long = MAX_OUTPUT_BYTES,
) {
    private val resolver: ContentResolver = context.contentResolver
    private val cacheDirectory = context.cacheDir
    private val draftsDirectory = File(cacheDirectory, DRAFT_DIRECTORY_NAME)
    private val snapshotsDirectory = File(cacheDirectory, SNAPSHOT_DIRECTORY_NAME)

    init {
        require(maxOutputBytes in 1..MAX_OUTPUT_BYTES) { "maxOutputBytes must stay within the JPEG upload limit" }
    }

    suspend fun normalize(uri: Uri): Result<NormalizedTerminalImage> = withContext(Dispatchers.IO) {
        val checkpoint = { coroutineContext.ensureActive() }
        try {
            Result.success(normalizeBlocking(uri, checkpoint))
        } catch (error: CancellationException) {
            throw error
        } catch (error: TerminalImageNormalizationError) {
            Result.failure(error)
        } catch (error: Exception) {
            Result.failure(
                TerminalImageNormalizationError(
                    TerminalImageNormalizationError.Code.DECODE_FAILED,
                    "could not normalize the selected image",
                    error,
                ),
            )
        }
    }

    /** Deletes a bounded batch of only AITerm-generated stale drafts. */
    suspend fun cleanupExpiredDrafts() = withContext(Dispatchers.IO) {
        val checkpoint = { coroutineContext.ensureActive() }
        cleanupExpiredDraftsBlocking(clockMillis(), checkpoint)
    }

    private fun normalizeBlocking(uri: Uri, checkpoint: () -> Unit): NormalizedTerminalImage {
        checkpoint()
        cleanupExpiredDraftsBlocking(clockMillis(), checkpoint)
        cleanupExpiredSnapshotsBlocking(clockMillis(), checkpoint)

        var snapshot: File? = null
        var decoded: Bitmap? = null
        var encoded: Bitmap? = null
        var output: File? = null
        try {
            snapshot = snapshotUri(uri, checkpoint)
            checkpoint()
            decoded = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                decodeModern(snapshot, checkpoint)
            } else {
                decodeLegacy(snapshot, checkpoint)
            }
            checkpoint()
            val outputSize = constrainedSize(decoded.width, decoded.height)
            encoded = Bitmap.createBitmap(outputSize.width, outputSize.height, Bitmap.Config.ARGB_8888)
            Canvas(encoded).apply {
                drawColor(TERMINAL_BACKGROUND)
                drawBitmap(decoded, null, Rect(0, 0, outputSize.width, outputSize.height), SCALE_PAINT)
            }

            checkpoint()
            val directory = ensurePrivateCacheDirectory(draftsDirectory, "terminal image draft")
            val id = UUID.randomUUID().toString()
            output = File(directory, "$id.jpg")
            BufferedOutputStream(FileOutputStream(output)).use { stream ->
                checkpoint()
                if (!encoded.compress(Bitmap.CompressFormat.JPEG, JPEG_QUALITY, stream)) {
                    throw TerminalImageNormalizationError(
                        TerminalImageNormalizationError.Code.OUTPUT_FAILED,
                        "could not encode terminal image as JPEG",
                    )
                }
                checkpoint()
            }

            val length = output.length()
            if (length !in 1..maxOutputBytes) {
                throw TerminalImageNormalizationError(
                    TerminalImageNormalizationError.Code.OUTPUT_TOO_LARGE,
                    "normalized image exceeds the ${MAX_OUTPUT_BYTES}-byte limit",
                )
            }
            if (!isCompleteBaselineJpeg(output, checkpoint)) {
                throw TerminalImageNormalizationError(
                    TerminalImageNormalizationError.Code.INCOMPATIBLE_OUTPUT,
                    "normalized image is not a complete baseline JPEG",
                )
            }
            checkpoint()
            return NormalizedTerminalImage(
                id = id,
                file = output,
                width = outputSize.width,
                height = outputSize.height,
                length = length,
                sha256 = sha256(output, checkpoint),
            )
        } catch (error: CancellationException) {
            output?.delete()
            throw error
        } catch (error: TerminalImageNormalizationError) {
            output?.delete()
            throw error
        } catch (error: Exception) {
            output?.delete()
            throw TerminalImageNormalizationError(
                TerminalImageNormalizationError.Code.DECODE_FAILED,
                "could not decode the selected image",
                error,
            )
        } finally {
            snapshot?.delete()
            encoded?.recycle()
            decoded?.recycle()
        }
    }

    private fun snapshotUri(uri: Uri, checkpoint: () -> Unit): File {
        val directory = ensurePrivateCacheDirectory(snapshotsDirectory, "terminal image snapshot")
        val snapshot = File(directory, "${UUID.randomUUID()}.source")
        try {
            var copied = 0L
            val input = try {
                resolver.openInputStream(uri)
            } catch (error: CancellationException) {
                throw error
            } catch (error: Exception) {
                throw TerminalImageNormalizationError(
                    TerminalImageNormalizationError.Code.CONTENT_UNAVAILABLE,
                    "could not open the selected image",
                    error,
                )
            } ?: throw TerminalImageNormalizationError(
                TerminalImageNormalizationError.Code.CONTENT_UNAVAILABLE,
                "could not open the selected image",
            )
            input.use { source ->
                BufferedOutputStream(FileOutputStream(snapshot)).use { destination ->
                    val buffer = ByteArray(DEFAULT_BUFFER_BYTES)
                    while (true) {
                        checkpoint()
                        val read = source.read(buffer)
                        checkpoint()
                        if (read < 0) break
                        if (read == 0) {
                            throw TerminalImageNormalizationError(
                                TerminalImageNormalizationError.Code.SNAPSHOT_FAILED,
                                "selected image stream made no forward progress",
                            )
                        }
                        copied += read
                        if (copied > MAX_INPUT_BYTES) {
                            throw TerminalImageNormalizationError(
                                TerminalImageNormalizationError.Code.INPUT_TOO_LARGE,
                                "selected image exceeds the ${MAX_INPUT_BYTES}-byte input limit",
                            )
                        }
                        destination.write(buffer, 0, read)
                        checkpoint()
                    }
                    if (copied == 0L) {
                        throw TerminalImageNormalizationError(
                            TerminalImageNormalizationError.Code.EMPTY_CONTENT,
                            "the selected image is empty",
                        )
                    }
                    destination.flush()
                }
            }
            if (snapshot.length() != copied) {
                throw TerminalImageNormalizationError(
                    TerminalImageNormalizationError.Code.SNAPSHOT_FAILED,
                    "private image snapshot was written incompletely",
                )
            }
            checkpoint()
            return snapshot
        } catch (error: CancellationException) {
            snapshot.delete()
            throw error
        } catch (error: TerminalImageNormalizationError) {
            snapshot.delete()
            throw error
        } catch (error: Exception) {
            snapshot.delete()
            throw TerminalImageNormalizationError(
                TerminalImageNormalizationError.Code.SNAPSHOT_FAILED,
                "could not create a private image snapshot",
                error,
            )
        }
    }

    @Suppress("NewApi")
    private fun decodeModern(snapshot: File, checkpoint: () -> Unit): Bitmap {
        var validationError: TerminalImageNormalizationError? = null
        checkpoint()
        val decoded = try {
            ImageDecoder.decodeBitmap(ImageDecoder.createSource(snapshot)) { decoder, info, _ ->
                try {
                    validateSourceDimensions(info.size.width, info.size.height)
                    val target = constrainedSize(info.size.width, info.size.height)
                    checkpoint()
                    decoder.setAllocator(ImageDecoder.ALLOCATOR_SOFTWARE)
                    decoder.setTargetSize(target.width, target.height)
                } catch (error: TerminalImageNormalizationError) {
                    validationError = error
                    throw error
                }
            }
        } catch (error: CancellationException) {
            throw error
        } catch (error: TerminalImageNormalizationError) {
            throw error
        } catch (error: Exception) {
            throw validationError ?: TerminalImageNormalizationError(
                TerminalImageNormalizationError.Code.DECODE_FAILED,
                "could not decode the selected image",
                error,
            )
        }
        checkpoint()
        return decoded
    }

    private fun decodeLegacy(snapshot: File, checkpoint: () -> Unit): Bitmap {
        checkpoint()
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeFile(snapshot.path, bounds)
        checkpoint()
        val options = BitmapFactory.Options().apply {
            inSampleSize = legacyTerminalImageSampleSize(bounds.outWidth, bounds.outHeight)
            inPreferredConfig = Bitmap.Config.ARGB_8888
        }
        val decoded = BitmapFactory.decodeFile(snapshot.path, options)
            ?: throw TerminalImageNormalizationError(
                TerminalImageNormalizationError.Code.DECODE_FAILED,
                "could not decode the selected image",
            )
        checkpoint()
        return applyExifOrientation(snapshot, decoded, checkpoint)
    }

    private fun applyExifOrientation(snapshot: File, bitmap: Bitmap, checkpoint: () -> Unit): Bitmap {
        checkpoint()
        val orientation = try {
            ExifInterface(snapshot.path).getAttributeInt(
                ExifInterface.TAG_ORIENTATION,
                ExifInterface.ORIENTATION_NORMAL,
            )
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            ExifInterface.ORIENTATION_NORMAL
        }
        checkpoint()
        val matrix = Matrix().apply {
            when (orientation) {
                ExifInterface.ORIENTATION_FLIP_HORIZONTAL -> setScale(-1f, 1f)
                ExifInterface.ORIENTATION_ROTATE_180 -> setRotate(180f)
                ExifInterface.ORIENTATION_FLIP_VERTICAL -> setScale(1f, -1f)
                ExifInterface.ORIENTATION_TRANSPOSE -> {
                    setRotate(90f)
                    postScale(-1f, 1f)
                }
                ExifInterface.ORIENTATION_ROTATE_90 -> setRotate(90f)
                ExifInterface.ORIENTATION_TRANSVERSE -> {
                    setRotate(-90f)
                    postScale(-1f, 1f)
                }
                ExifInterface.ORIENTATION_ROTATE_270 -> setRotate(-90f)
            }
        }
        if (matrix.isIdentity) return bitmap
        return try {
            Bitmap.createBitmap(bitmap, 0, 0, bitmap.width, bitmap.height, matrix, true)
                .also {
                    checkpoint()
                    bitmap.recycle()
                }
        } catch (error: CancellationException) {
            bitmap.recycle()
            throw error
        } catch (error: Exception) {
            bitmap.recycle()
            throw TerminalImageNormalizationError(
                TerminalImageNormalizationError.Code.DECODE_FAILED,
                "could not apply the image orientation",
                error,
            )
        }
    }

    private fun validateSourceDimensions(width: Int, height: Int) {
        legacyTerminalImageSampleSize(width, height)
    }

    private fun constrainedSize(width: Int, height: Int): ImageSize {
        if (width <= 0 || height <= 0) {
            throw TerminalImageNormalizationError(
                TerminalImageNormalizationError.Code.DECODE_FAILED,
                "decoded image has invalid dimensions",
            )
        }
        val longest = max(width, height)
        if (longest <= MAX_IMAGE_EDGE) return ImageSize(width, height)
        val scale = MAX_IMAGE_EDGE.toDouble() / longest
        return ImageSize(
            width = max(1, (width * scale).toInt()),
            height = max(1, (height * scale).toInt()),
        )
    }

    private fun ensurePrivateCacheDirectory(directory: File, label: String): File {
        if (directory.exists()) {
            if (!directory.isDirectory || isSymlink(directory)) {
                throw TerminalImageNormalizationError(
                    TerminalImageNormalizationError.Code.OUTPUT_FAILED,
                    "$label path is not a private directory",
                )
            }
        } else if (!directory.mkdirs() && !directory.isDirectory) {
            throw TerminalImageNormalizationError(
                TerminalImageNormalizationError.Code.OUTPUT_FAILED,
                "could not create $label directory",
            )
        }
        if (directory.canonicalFile.parentFile != contextCacheDirectory()) {
            throw TerminalImageNormalizationError(
                TerminalImageNormalizationError.Code.OUTPUT_FAILED,
                "$label path is outside the private cache",
            )
        }
        return directory
    }

    private fun cleanupExpiredDraftsBlocking(nowMillis: Long, checkpoint: () -> Unit) {
        if (
            !draftsDirectory.isDirectory ||
            isSymlink(draftsDirectory) ||
            !isPrivateCacheChild(draftsDirectory)
        ) return
        draftsDirectory.listFiles()
            .orEmpty()
            .asSequence()
            .filter(::isGeneratedDraft)
            .filter { nowMillis - it.lastModified() >= DRAFT_TTL_MILLIS }
            .sortedBy(File::lastModified)
            .take(MAX_CLEANUP_FILES_PER_PASS)
            .forEach {
                checkpoint()
                it.delete()
            }
    }

    /** Best-effort cleanup after a process death interrupted a source snapshot copy. */
    private fun cleanupExpiredSnapshotsBlocking(nowMillis: Long, checkpoint: () -> Unit) {
        if (
            !snapshotsDirectory.isDirectory ||
            isSymlink(snapshotsDirectory) ||
            !isPrivateCacheChild(snapshotsDirectory)
        ) return
        snapshotsDirectory.listFiles()
            .orEmpty()
            .asSequence()
            .filter(::isGeneratedSnapshot)
            .filter { nowMillis - it.lastModified() >= SNAPSHOT_TTL_MILLIS }
            .sortedBy(File::lastModified)
            .take(MAX_CLEANUP_FILES_PER_PASS)
            .forEach {
                checkpoint()
                it.delete()
            }
    }

    private fun isGeneratedDraft(file: File): Boolean =
        file.name.matches(GENERATED_DRAFT_NAME) && file.isFile && !isSymlink(file)

    private fun isGeneratedSnapshot(file: File): Boolean =
        file.name.matches(GENERATED_SNAPSHOT_NAME) && file.isFile && !isSymlink(file)

    private fun isSymlink(file: File): Boolean =
        try {
            Files.isSymbolicLink(file.toPath())
        } catch (_: Exception) {
            true
        }

    private fun isPrivateCacheChild(directory: File): Boolean =
        try {
            directory.canonicalFile.parentFile == contextCacheDirectory()
        } catch (_: Exception) {
            false
        }

    private fun contextCacheDirectory(): File = cacheDirectory.canonicalFile

    private fun isCompleteBaselineJpeg(file: File, checkpoint: () -> Unit): Boolean {
        val bytes = ByteArrayOutputStream(file.length().toInt()).use { output ->
            FileInputStream(file).use { input ->
                val buffer = ByteArray(DEFAULT_BUFFER_BYTES)
                while (true) {
                    checkpoint()
                    val read = input.read(buffer)
                    checkpoint()
                    if (read < 0) break
                    output.write(buffer, 0, read)
                }
            }
            output.toByteArray()
        }
        return isCompleteBaselineJpeg(bytes)
    }

    private fun isCompleteBaselineJpeg(bytes: ByteArray): Boolean {
        if (bytes.size < 4 || bytes[0] != 0xff.toByte() || bytes[1] != 0xd8.toByte()) return false
        var offset = 2
        var sawSof0 = false
        while (offset < bytes.size) {
            if (bytes[offset++].toInt() and 0xff != 0xff) return false
            while (offset < bytes.size && bytes[offset].toInt() and 0xff == 0xff) offset += 1
            if (offset >= bytes.size) return false
            val marker = bytes[offset++].toInt() and 0xff
            if (marker == MARKER_EOI) return sawSof0 && offset == bytes.size
            if (marker == MARKER_SOI || marker in MARKER_RESTART_FIRST..MARKER_RESTART_LAST || marker == MARKER_TEM) {
                return false
            }
            if (offset + 2 > bytes.size) return false
            val length = ((bytes[offset].toInt() and 0xff) shl 8) or (bytes[offset + 1].toInt() and 0xff)
            if (length < 2) return false
            offset += 2
            val segmentEnd = offset + length - 2
            if (segmentEnd > bytes.size) return false
            if (isStartOfFrame(marker)) {
                if (marker != MARKER_SOF0 || sawSof0 || !isValidSof0(bytes, offset, length)) return false
                sawSof0 = true
            }
            if (marker == MARKER_SOS) {
                if (!sawSof0) return false
                return hasOnlyEntropyAndFinalEoi(bytes, segmentEnd)
            }
            offset = segmentEnd
        }
        return false
    }

    private fun isValidSof0(bytes: ByteArray, offset: Int, length: Int): Boolean {
        if (length < 8 || bytes[offset].toInt() and 0xff != 8) return false
        val height = ((bytes[offset + 1].toInt() and 0xff) shl 8) or (bytes[offset + 2].toInt() and 0xff)
        val width = ((bytes[offset + 3].toInt() and 0xff) shl 8) or (bytes[offset + 4].toInt() and 0xff)
        val components = bytes[offset + 5].toInt() and 0xff
        return width > 0 && height > 0 && components > 0 && length == 8 + 3 * components
    }

    private fun hasOnlyEntropyAndFinalEoi(bytes: ByteArray, start: Int): Boolean {
        var offset = start
        while (offset < bytes.size) {
            if (bytes[offset++].toInt() and 0xff != 0xff) continue
            if (offset >= bytes.size) return false
            val marker = bytes[offset++].toInt() and 0xff
            when {
                marker == 0x00 || marker in MARKER_RESTART_FIRST..MARKER_RESTART_LAST -> Unit
                marker == MARKER_EOI -> return offset == bytes.size
                else -> return false
            }
        }
        return false
    }

    private fun isStartOfFrame(marker: Int): Boolean =
        marker in 0xc0..0xc3 || marker in 0xc5..0xc7 || marker in 0xc9..0xcb || marker in 0xcd..0xcf

    private fun sha256(file: File, checkpoint: () -> Unit): ByteArray {
        val digest = MessageDigest.getInstance("SHA-256")
        FileInputStream(file).use { stream ->
            val buffer = ByteArray(DEFAULT_BUFFER_BYTES)
            while (true) {
                checkpoint()
                val count = stream.read(buffer)
                checkpoint()
                if (count < 0) break
                digest.update(buffer, 0, count)
            }
        }
        return digest.digest()
    }

    private data class ImageSize(val width: Int, val height: Int)

    companion object {
        const val MAX_IMAGE_EDGE = 4_096
        const val MAX_SOURCE_EDGE = 32_768
        const val MAX_INPUT_BYTES = 48L * 1_024 * 1_024
        const val MAX_OUTPUT_BYTES = 12L * 1_024 * 1_024
        const val DRAFT_TTL_MILLIS = 24L * 60 * 60 * 1_000
        const val SNAPSHOT_TTL_MILLIS = 15L * 60 * 1_000
        private const val MAX_CLEANUP_FILES_PER_PASS = 64
        private const val JPEG_QUALITY = 90
        private const val DEFAULT_BUFFER_BYTES = 32 * 1_024
        private const val DRAFT_DIRECTORY_NAME = "terminal-image-drafts"
        private const val SNAPSHOT_DIRECTORY_NAME = "terminal-image-snapshots"
        private const val MARKER_SOI = 0xd8
        private const val MARKER_EOI = 0xd9
        private const val MARKER_SOF0 = 0xc0
        private const val MARKER_SOS = 0xda
        private const val MARKER_TEM = 0x01
        private const val MARKER_RESTART_FIRST = 0xd0
        private const val MARKER_RESTART_LAST = 0xd7
        private val GENERATED_DRAFT_NAME = Regex(
            "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\\.jpg",
        )
        private val GENERATED_SNAPSHOT_NAME = Regex(
            "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\\.source",
        )
        private val TERMINAL_BACKGROUND = Color.rgb(0x07, 0x11, 0x1B)
        private val SCALE_PAINT = Paint(Paint.ANTI_ALIAS_FLAG or Paint.FILTER_BITMAP_FLAG)
    }
}
