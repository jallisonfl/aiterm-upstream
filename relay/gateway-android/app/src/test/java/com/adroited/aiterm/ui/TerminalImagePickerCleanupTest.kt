package com.adroited.aiterm.ui

import java.io.File
import java.nio.file.Files
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class TerminalImagePickerCleanupTest {
    @Test
    fun cleanupFiltersExpiredGeneratedCapturesBeforeDeletingTheOldestBoundedBatch() {
        val root = Files.createTempDirectory("aiterm-captures-").toFile()
        val now = 2_000_000_000L
        val stale = (0 until 66).map { index ->
            generatedCapture(root, index).apply {
                setLastModified(now - TERMINAL_CAPTURE_TTL_MILLIS - 10_000L - index)
            }
        }
        val protected = stale.last()
        val fresh = (100 until 170).map { index ->
            generatedCapture(root, index).apply {
                setLastModified(now - TERMINAL_CAPTURE_TTL_MILLIS + 1_000L)
            }
        }
        val unrelated = File(root, "notes.jpg").apply {
            writeText("keep")
            setLastModified(1L)
        }

        val firstDeleted = cleanupExpiredTerminalCaptures(root, protected.path, now)

        assertEquals(64, firstDeleted)
        assertTrue(protected.exists())
        assertTrue(unrelated.exists())
        assertTrue(fresh.all(File::exists))
        assertEquals(1, stale.count { it != protected && it.exists() })
        assertTrue(stale.first().exists())

        val secondDeleted = cleanupExpiredTerminalCaptures(root, protected.path, now)

        assertEquals(1, secondDeleted)
        assertTrue(protected.exists())
        assertTrue(unrelated.exists())
        root.deleteRecursively()
    }

    @Test
    fun cleanupExpiresAtTheExactBoundaryAndNeverTouchesBroaderNamesOrSymlinks() {
        val root = Files.createTempDirectory("aiterm-capture-boundary-").toFile()
        val now = 3_000_000_000L
        val boundary = generatedCapture(root, 1).apply {
            setLastModified(now - TERMINAL_CAPTURE_TTL_MILLIS)
        }
        val oneMillisecondFresh = generatedCapture(root, 2).apply {
            setLastModified(now - TERMINAL_CAPTURE_TTL_MILLIS + 1L)
        }
        val uppercase = File(root, "00000003-0000-4000-8000-000000000003.JPG").apply {
            writeText("keep")
            setLastModified(1L)
        }
        val wrongUuid = File(root, "not-a-uuid.jpg").apply {
            writeText("keep")
            setLastModified(1L)
        }
        val outside = Files.createTempFile("aiterm-outside-", ".jpg")
        val linked = root.toPath().resolve("00000004-0000-4000-8000-000000000004.jpg")
        Files.createSymbolicLink(linked, outside)

        assertEquals(1, cleanupExpiredTerminalCaptures(root, null, now))
        assertFalse(boundary.exists())
        assertTrue(oneMillisecondFresh.exists())
        assertTrue(uppercase.exists())
        assertTrue(wrongUuid.exists())
        assertTrue(Files.isSymbolicLink(linked))

        root.deleteRecursively()
        Files.deleteIfExists(outside)
    }

    @Test
    fun cleanupFailsClosedForANonDirectoryOrSymlinkRoot() {
        val now = 4_000_000_000L
        val regularRoot = Files.createTempFile("aiterm-capture-root-", ".tmp").toFile()
        assertEquals(0, cleanupExpiredTerminalCaptures(regularRoot, null, now))
        assertTrue(regularRoot.exists())

        val parent = Files.createTempDirectory("aiterm-capture-link-parent-")
        val realRoot = Files.createDirectory(parent.resolve("real"))
        val capture = generatedCapture(realRoot.toFile(), 9).apply { setLastModified(1L) }
        val linkedRoot = parent.resolve("linked")
        Files.createSymbolicLink(linkedRoot, realRoot)

        assertEquals(0, cleanupExpiredTerminalCaptures(linkedRoot.toFile(), null, now))
        assertTrue(capture.exists())

        parent.toFile().deleteRecursively()
        regularRoot.delete()
    }

    private fun generatedCapture(root: File, index: Int): File {
        val name = "%08x-0000-4000-8000-%012x.jpg".format(index, index)
        return File(root, name).apply { writeBytes(byteArrayOf(1, 2, 3)) }
    }
}
