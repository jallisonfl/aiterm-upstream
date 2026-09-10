package com.fivelime.aiterm

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class LinksTest {
    @Test fun `absolute paths and localhost file urls name desktop files`() {
        assertEquals("/home/j/out/report.md", conversationFilePath("/home/j/out/report.md"))
        assertEquals("/home/j/a.rs", conversationFilePath("/home/j/a.rs:12:5"))
        assertEquals("/home/j/a.rs", conversationFilePath("file:///home/j/a.rs:12"))
        assertEquals("/home/j/a.rs", conversationFilePath("file://localhost/home/j/a.rs"))
        assertEquals("/home/a.rs", conversationFilePath("/home/j/../a.rs"))
    }

    @Test fun `relative remote and decorated targets are not files`() {
        assertNull(conversationFilePath("./out/report.md"))
        assertNull(conversationFilePath("//server/share/a"))
        assertNull(conversationFilePath("file://evil.example/etc/passwd"))
        assertNull(conversationFilePath("file:///home/j/a.rs?x=1"))
        assertNull(conversationFilePath("file:///home/j/a.rs#frag"))
        assertNull(conversationFilePath("https://example.com/a"))
    }

    @Test fun `only http and https go to the browser`() {
        assertTrue(isWebLink("https://example.com/x?y=1"))
        assertFalse(isWebLink("javascript:alert(1)"))
        assertFalse(isWebLink("https://exa mple.com"))
    }
}
