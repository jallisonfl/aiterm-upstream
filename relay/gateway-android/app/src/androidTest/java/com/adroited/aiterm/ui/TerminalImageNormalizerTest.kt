package com.adroited.aiterm.ui

import android.content.Context
import android.os.Bundle
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.graphics.Color
import android.net.Uri
import androidx.core.content.FileProvider
import androidx.exifinterface.media.ExifInterface
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import java.io.DataOutputStream
import java.io.ByteArrayOutputStream
import java.io.File
import java.nio.file.Files
import java.security.MessageDigest
import java.util.UUID
import java.util.zip.CRC32
import java.util.zip.DeflaterOutputStream
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeoutOrNull
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.assertThrows
import org.junit.Assume.assumeNoException
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class TerminalImageNormalizerTest {
    private val context = ApplicationProvider.getApplicationContext<Context>()
    private val capturesRoot = File(
        context.cacheDir,
        "terminal-image-captures/normalizer-test-${UUID.randomUUID()}",
    ).apply { mkdirs() }
    private val normalizedOutputs = mutableListOf<File>()
    private val snapshotFixtures = mutableListOf<File>()

    @After
    fun removeOnlyThisTestsFixtures() {
        capturesRoot.deleteRecursively()
        normalizedOutputs.forEach(File::delete)
        snapshotFixtures.reversed().forEach(File::delete)
        context.contentResolver.call(mutableUri, "reset", null, null)
    }

    @Test
    fun landscapeImage_isBoundedAndStoredAsMetadataFreePrivateJpeg() = runBlocking {
        val source = writeBitmap("landscape.jpg", 6_000, 3_000, Bitmap.CompressFormat.JPEG) { canvas ->
            canvas.drawColor(Color.rgb(90, 140, 210))
        }

        val image = normalizer().normalize(uriFor(source)).getOrThrow().also { normalizedOutputs += it.file }

        assertEquals(4_096, image.width)
        assertEquals(2_048, image.height)
        assertTrue(image.file.parentFile == File(context.cacheDir, "terminal-image-drafts"))
        assertThrows(IllegalArgumentException::class.java) {
            FileProvider.getUriForFile(context, "${context.packageName}.terminal-images", image.file)
        }
        assertTrue(image.file.name.matches(Regex("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\\.jpg")))
        assertEquals(image.file.length(), image.length)
        assertArrayEquals(sha256(image.file), image.sha256)
        val bytes = image.file.readBytes()
        assertTrue(bytes.size >= 4)
        assertEquals(0xff.toByte(), bytes.first())
        assertEquals(0xd8.toByte(), bytes[1])
        assertEquals(0xff.toByte(), bytes[bytes.lastIndex - 1])
        assertEquals(0xd9.toByte(), bytes.last())
        assertTrue(hasBaselineSof0(bytes))
        assertTrue(image.length in 1..TerminalImageNormalizer.MAX_OUTPUT_BYTES)

        val outputExif = ExifInterface(image.file.absolutePath)
        assertNull(outputExif.getAttribute(ExifInterface.TAG_GPS_LATITUDE))
        assertNull(outputExif.getAttribute(ExifInterface.TAG_GPS_LONGITUDE))
        assertNull(outputExif.getAttribute(ExifInterface.TAG_GPS_PROCESSING_METHOD))
        assertNull(outputExif.getAttribute(ExifInterface.TAG_MAKE))
        assertNull(outputExif.getAttribute(ExifInterface.TAG_MODEL))
    }

    @Test
    fun encodedExifRotation_isAppliedBeforeTheNormalizedDimensionsAreReported() = runBlocking {
        val source = writeBitmap("rotated.jpg", 30, 10, Bitmap.CompressFormat.JPEG) { canvas ->
            canvas.drawColor(Color.RED)
        }
        ExifInterface(source.absolutePath).apply {
            setAttribute(ExifInterface.TAG_ORIENTATION, ExifInterface.ORIENTATION_ROTATE_90.toString())
            saveAttributes()
        }

        val image = normalizer().normalize(uriFor(source)).getOrThrow().also { normalizedOutputs += it.file }

        assertEquals(10, image.width)
        assertEquals(30, image.height)
    }

    @Test
    fun transparentPng_isCompositedOntoTheTerminalBackgroundBeforeJpegEncoding() = runBlocking {
        val source = writeBitmap("transparent.png", 32, 32, Bitmap.CompressFormat.PNG) { canvas ->
            canvas.drawColor(Color.TRANSPARENT)
        }

        val image = normalizer().normalize(uriFor(source)).getOrThrow().also { normalizedOutputs += it.file }
        val color = BitmapFactory.decodeFile(image.file.absolutePath).getPixel(16, 16)

        assertTrue("red was ${Color.red(color)}", Color.red(color) in 0..25)
        assertTrue("green was ${Color.green(color)}", Color.green(color) in 5..35)
        assertTrue("blue was ${Color.blue(color)}", Color.blue(color) in 12..42)
    }

    @Test
    fun emptyContent_isRejectedWithoutCreatingADraft() = runBlocking {
        val source = File(capturesRoot, "empty.jpg").apply { writeBytes(ByteArray(0)) }

        val failure = normalizer().normalize(uriFor(source)).exceptionOrNull() as? TerminalImageNormalizationError

        assertNotNull(failure)
        assertEquals(TerminalImageNormalizationError.Code.EMPTY_CONTENT, failure?.code)
        assertFalse(File(context.cacheDir, "terminal-image-drafts").listFiles().orEmpty()
            .any { it.name.startsWith("empty") })
    }

    @Test
    fun corruptContent_isRejectedWithoutCreatingADraft() = runBlocking {
        val source = File(capturesRoot, "corrupt.jpg").apply { writeBytes("not an image".encodeToByteArray()) }

        val failure = normalizer().normalize(uriFor(source)).exceptionOrNull() as? TerminalImageNormalizationError

        assertNotNull(failure)
        assertEquals(TerminalImageNormalizationError.Code.DECODE_FAILED, failure?.code)
    }

    @Test
    fun claimedSourcePastTheDecodeBound_isRejectedBeforeBitmapAllocation() = runBlocking {
        val source = File(capturesRoot, "over-bound.png")
        writePngHeader(source, width = TerminalImageNormalizer.MAX_SOURCE_EDGE + 1, height = 1)

        val failure = normalizer().normalize(uriFor(source)).exceptionOrNull() as? TerminalImageNormalizationError

        assertNotNull(failure)
        assertEquals(TerminalImageNormalizationError.Code.DIMENSIONS_OUT_OF_BOUNDS, failure?.code)
    }

    @Test
    fun mutableProvider_isOpenedOnceAndCannotSwapInAnOversizedImageAfterSnapshot() = runBlocking {
        val initial = jpegBytes(80, 40)
        val later = pngBytes(TerminalImageNormalizer.MAX_SOURCE_EDGE + 1, 1)
        configureMutable(first = initial, later = later)

        val image = normalizer().normalize(mutableUri).getOrThrow().also { normalizedOutputs += it.file }

        assertEquals(80, image.width)
        assertEquals(40, image.height)
        assertEquals(1, mutableStats().getInt("opens"))
        assertNoSnapshots()
    }

    @Test
    fun inputBeyondFortyEightMiB_isRejectedAndLeavesNoSnapshot() = runBlocking {
        context.contentResolver.call(
            mutableUri,
            "configure-generated",
            null,
            Bundle().apply { putInt("length", TerminalImageNormalizer.MAX_INPUT_BYTES.toInt() + 1) },
        )

        val failure = normalizer().normalize(mutableUri).exceptionOrNull() as? TerminalImageNormalizationError

        assertEquals(TerminalImageNormalizationError.Code.INPUT_TOO_LARGE, failure?.code)
        assertEquals(1, mutableStats().getInt("opens"))
        assertNoSnapshots()
    }

    @Test
    fun cancellationDuringSnapshotCopy_propagatesCancellationAndDeletesTemporaryFile() = runBlocking {
        context.contentResolver.call(
            mutableUri,
            "configure-slow",
            null,
            Bundle().apply {
                putInt("length", 512 * 1_024)
                putInt("chunk", 32 * 1_024)
                putLong("delay", 10)
                putLong("start-delay", 250)
            },
        )

        val completed = withTimeoutOrNull(100) {
            async { normalizer().normalize(mutableUri) }.await()
        }

        assertNull(completed)
        assertNoSnapshots()
    }

    @Test
    fun legacySampling_neverAllocatesPastTheFinalImageEdgeBudget() {
        assertEquals(1, legacyTerminalImageSampleSize(4_096, 4_096))
        assertEquals(2, legacyTerminalImageSampleSize(4_097, 4_097))
        assertEquals(4, legacyTerminalImageSampleSize(8_193, 8_193))
    }

    @Test
    fun jpegWhoseNormalizedOutputExceedsTwelveMiB_isRemovedAndReported() = runBlocking {
        val source = writeNoisyBitmap("output-too-large.jpg", 4_096, 4_096)
        val drafts = File(context.cacheDir, "terminal-image-drafts")
        val before = drafts.listFiles().orEmpty().map(File::getName).toSet()

        val failure = normalizer().normalize(uriFor(source))
            .exceptionOrNull() as? TerminalImageNormalizationError

        assertNotNull(failure)
        assertEquals(TerminalImageNormalizationError.Code.OUTPUT_TOO_LARGE, failure?.code)
        assertEquals(before, drafts.listFiles().orEmpty().map(File::getName).toSet())
    }

    @Test
    fun lazyCleanup_deletesOnlyExpiredUuidDrafts() = runBlocking {
        val drafts = File(context.cacheDir, "terminal-image-drafts").apply { mkdirs() }
        val now = System.currentTimeMillis()
        val oldGenerated = File(drafts, "${UUID.randomUUID()}.jpg").apply {
            writeBytes(byteArrayOf(1))
            setLastModified(now - TerminalImageNormalizer.DRAFT_TTL_MILLIS - 1)
        }
        val freshGenerated = File(drafts, "${UUID.randomUUID()}.jpg").apply {
            writeBytes(byteArrayOf(2))
            setLastModified(now)
        }
        val exactExpiry = File(drafts, "${UUID.randomUUID()}.jpg").apply {
            writeBytes(byteArrayOf(4))
            setLastModified(now - TerminalImageNormalizer.DRAFT_TTL_MILLIS)
        }
        val unrelated = File(drafts, "user-photo.jpg").apply {
            writeBytes(byteArrayOf(3))
            setLastModified(now - TerminalImageNormalizer.DRAFT_TTL_MILLIS - 1)
        }

        TerminalImageNormalizer(context, clockMillis = { now }).cleanupExpiredDrafts()

        assertFalse(oldGenerated.exists())
        assertFalse(exactExpiry.exists())
        assertTrue(freshGenerated.exists())
        assertTrue(unrelated.exists())
        freshGenerated.delete()
        unrelated.delete()
        Unit
    }

    @Test
    fun normalizeStart_expiresOnlyStaleGeneratedSnapshotsAtTheExactFifteenMinuteBoundary() = runBlocking {
        val now = 1_700_000_000_000L
        val stale = snapshotFixture("${UUID.randomUUID()}.source", now - SNAPSHOT_EXPIRY_MILLIS - 1)
        val exactExpiry = snapshotFixture("${UUID.randomUUID()}.source", now - SNAPSHOT_EXPIRY_MILLIS)
        val fresh = snapshotFixture("${UUID.randomUUID()}.source", now)
        val unrelated = snapshotFixture("not-a-normalizer-snapshot.source", now - SNAPSHOT_EXPIRY_MILLIS - 1)

        triggerSnapshotMaintenance(now)

        assertFalse(stale.exists())
        assertFalse(exactExpiry.exists())
        assertTrue(fresh.exists())
        assertTrue(unrelated.exists())
    }

    @Test
    fun normalizeStart_limitsStaleSnapshotCleanupToSixtyFourFilesPerPass() = runBlocking {
        val now = 1_700_000_000_000L
        repeat(65) { index ->
            snapshotFixture(
                "${UUID.randomUUID()}.source",
                now - SNAPSHOT_EXPIRY_MILLIS - index - 1,
            )
        }

        triggerSnapshotMaintenance(now)

        assertEquals(1, snapshotRoot.listFiles().orEmpty().count { it.name.endsWith(".source") })
    }

    @Test
    fun normalizeStart_preservesAStaleGeneratedSnapshotSymlink() = runBlocking {
        val now = 1_700_000_000_000L
        val target = snapshotFixture("symlink-target", now - SNAPSHOT_EXPIRY_MILLIS - 1)
        val link = File(snapshotRoot, "${UUID.randomUUID()}.source").also(snapshotFixtures::add)
        try {
            Files.createSymbolicLink(link.toPath(), target.toPath())
        } catch (error: Exception) {
            assumeNoException("symbolic links unavailable on this device", error)
        }

        triggerSnapshotMaintenance(now)

        assertTrue(Files.isSymbolicLink(link.toPath()))
        assertTrue(target.exists())
    }

    @Test
    fun normalizeStart_refusesASnapshotSymlinkRootWithoutDeletingItsTarget() = runBlocking {
        val now = 1_700_000_000_000L
        assertTrue(snapshotRoot.listFiles().orEmpty().isEmpty())
        assertTrue(snapshotRoot.delete())
        val targetRoot = File(context.cacheDir, "normalizer-snapshot-symlink-target-${UUID.randomUUID()}")
            .apply { mkdirs() }
        val target = File(targetRoot, "${UUID.randomUUID()}.source").apply {
            writeBytes(byteArrayOf(1))
            setLastModified(now - SNAPSHOT_EXPIRY_MILLIS - 1)
        }
        try {
            try {
                Files.createSymbolicLink(snapshotRoot.toPath(), targetRoot.toPath())
            } catch (error: Exception) {
                assumeNoException("symbolic links unavailable on this device", error)
            }
            val source = File(capturesRoot, "symlink-root-trigger.jpg").apply { writeBytes(ByteArray(0)) }

            val failure = normalizer { now }.normalize(uriFor(source))
                .exceptionOrNull() as? TerminalImageNormalizationError

            assertEquals(TerminalImageNormalizationError.Code.OUTPUT_FAILED, failure?.code)
            assertTrue(Files.isSymbolicLink(snapshotRoot.toPath()))
            assertTrue(target.exists())
        } finally {
            Files.deleteIfExists(snapshotRoot.toPath())
            targetRoot.deleteRecursively()
            snapshotRoot.mkdirs()
        }
    }

    private fun normalizer(clockMillis: () -> Long = System::currentTimeMillis) =
        TerminalImageNormalizer(context, clockMillis = clockMillis)

    private val snapshotRoot: File
        get() = File(context.cacheDir, "terminal-image-snapshots").apply { mkdirs() }

    private fun snapshotFixture(name: String, lastModified: Long): File =
        File(snapshotRoot, name).apply {
            writeBytes(byteArrayOf(1))
            setLastModified(lastModified)
            snapshotFixtures += this
        }

    private suspend fun triggerSnapshotMaintenance(now: Long) {
        val source = File(capturesRoot, "snapshot-maintenance-trigger-${UUID.randomUUID()}.jpg")
            .apply { writeBytes(ByteArray(0)) }
        val failure = normalizer { now }.normalize(uriFor(source)).exceptionOrNull() as? TerminalImageNormalizationError
        assertEquals(TerminalImageNormalizationError.Code.EMPTY_CONTENT, failure?.code)
    }

    private fun configureMutable(first: ByteArray, later: ByteArray) {
        context.contentResolver.call(
            mutableUri,
            "configure",
            null,
            Bundle().apply {
                putByteArray("first", first)
                putByteArray("later", later)
            },
        )
    }

    private fun mutableStats(): Bundle = requireNotNull(
        context.contentResolver.call(mutableUri, "stats", null, null),
    )

    private fun assertNoSnapshots() {
        assertTrue(File(context.cacheDir, "terminal-image-snapshots").listFiles().orEmpty().isEmpty())
    }

    private fun writeBitmap(
        name: String,
        width: Int,
        height: Int,
        format: Bitmap.CompressFormat,
        draw: (Canvas) -> Unit,
    ): File {
        val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
        try {
            draw(Canvas(bitmap))
            return File(capturesRoot, name).also { file ->
                file.outputStream().use { output ->
                    assertTrue(bitmap.compress(format, 100, output))
                }
            }
        } finally {
            bitmap.recycle()
        }
    }

    private fun writeNoisyBitmap(name: String, width: Int, height: Int): File {
        val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
        try {
            val pixels = IntArray(width * height)
            var state = 0x13579BDF
            for (index in pixels.indices) {
                state = state * 1_103_515_245 + 12_345
                pixels[index] = 0xff000000.toInt() or (state and 0x00ffffff)
            }
            bitmap.setPixels(pixels, 0, width, 0, 0, width, height)
            return File(capturesRoot, name).also { file ->
                file.outputStream().use { output ->
                    assertTrue(bitmap.compress(Bitmap.CompressFormat.JPEG, 100, output))
                }
            }
        } finally {
            bitmap.recycle()
        }
    }

    private fun jpegBytes(width: Int, height: Int): ByteArray {
        val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
        return try {
            Canvas(bitmap).drawColor(Color.CYAN)
            ByteArrayOutputStream().use { output ->
                assertTrue(bitmap.compress(Bitmap.CompressFormat.JPEG, 90, output))
                output.toByteArray()
            }
        } finally {
            bitmap.recycle()
        }
    }

    private fun uriFor(file: File): Uri = FileProvider.getUriForFile(
        context,
        "${context.packageName}.terminal-images",
        file,
    )

    private fun sha256(file: File): ByteArray = MessageDigest.getInstance("SHA-256").digest(file.readBytes())

    private fun writePngHeader(file: File, width: Int, height: Int) {
        file.writeBytes(pngBytes(width, height))
    }

    private fun pngBytes(width: Int, height: Int): ByteArray {
        val rawPixels = ByteArray(1 + width * 4)
        val compressed = ByteArrayOutputStream().use { bytes ->
            DeflaterOutputStream(bytes).use { it.write(rawPixels) }
            bytes.toByteArray()
        }
        return ByteArrayOutputStream().use { bytes ->
            DataOutputStream(bytes).use { output ->
            output.write(byteArrayOf(137.toByte(), 80, 78, 71, 13, 10, 26, 10))
            ByteArrayOutputStream().use { header ->
                DataOutputStream(header).use {
                    it.writeInt(width)
                    it.writeInt(height)
                    it.writeByte(8)
                    it.writeByte(6)
                    it.writeByte(0)
                    it.writeByte(0)
                    it.writeByte(0)
                }
                writePngChunk(output, "IHDR", header.toByteArray())
            }
            writePngChunk(output, "IDAT", compressed)
            writePngChunk(output, "IEND", ByteArray(0))
            }
            bytes.toByteArray()
        }
    }

    private fun writePngChunk(output: DataOutputStream, type: String, data: ByteArray) {
        val typeBytes = type.encodeToByteArray()
        val crc = CRC32().apply {
            update(typeBytes)
            update(data)
        }
        output.writeInt(data.size)
        output.write(typeBytes)
        output.write(data)
        output.writeInt(crc.value.toInt())
    }

    private fun hasBaselineSof0(bytes: ByteArray): Boolean = bytes.indices.any { index ->
        index + 1 < bytes.size && bytes[index] == 0xff.toByte() && bytes[index + 1] == 0xc0.toByte()
    }

    private companion object {
        const val SNAPSHOT_EXPIRY_MILLIS = 15L * 60 * 1_000
        val mutableUri: Uri = Uri.parse("content://com.adroited.aiterm.test.mutable-image/image")
    }
}
