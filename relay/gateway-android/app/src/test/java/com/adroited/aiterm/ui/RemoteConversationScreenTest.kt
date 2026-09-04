package com.adroited.aiterm.ui

import com.adroited.aiterm.remote.RemotePreviewMessage
import com.adroited.aiterm.remote.RemoteSession
import com.adroited.aiterm.remote.RemoteTab
import com.adroited.aiterm.remote.TerminalSize
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class RemoteConversationScreenTest {
    private val liveTab = RemoteTab(
        id = "tab-live",
        title = "Live terminal",
        sessionId = "live",
        size = TerminalSize(80, 24),
    )

    @Test
    fun liveSessionsLeadTheDashboardAndRecentSessionsFollow() {
        val sessions = listOf(
            session("old", "Older", lastActive = 10),
            session("live", "Live", lastActive = 5),
            session("new", "Newer", lastActive = 20),
        )

        assertEquals(
            listOf("live", "new", "old"),
            conversationSessions(sessions, listOf(liveTab), "").map { it.id },
        )
    }

    @Test
    fun dashboardSearchUsesTitleAgentAndProjectWithoutCaseSensitivity() {
        val sessions = listOf(
            session("one", "Release prep", agent = "codex", project = "/work/aiterm"),
            session("two", "Notes", agent = "claude", project = "/work/docs"),
        )

        assertEquals(listOf("one"), conversationSessions(sessions, emptyList(), "AITERM").map { it.id })
        assertEquals(listOf("two"), conversationSessions(sessions, emptyList(), "CLAUDE").map { it.id })
        assertEquals(listOf("one"), conversationSessions(sessions, emptyList(), "release").map { it.id })
    }

    @Test
    fun liveStateComesOnlyFromARealTabForThatSession() {
        assertTrue(isConversationSessionLive(session("live", "Live"), listOf(liveTab)))
        assertFalse(isConversationSessionLive(session("other", "Other"), listOf(liveTab)))
    }

    @Test
    fun dashboardFiltersComposeAndStarsStayFirst() {
        val sessions = listOf(
            session("claude", "Claude", agent = "claude", lastActive = 30),
            session("live", "Live", lastActive = 20),
            session("star", "Star", lastActive = 10),
        )

        assertEquals(
            listOf("star", "live", "claude"),
            conversationSessions(sessions, listOf(liveTab), "", starred = setOf("star")).map { it.id },
        )
        assertEquals(
            listOf("live"),
            conversationSessions(sessions, listOf(liveTab), "", activeOnly = true).map { it.id },
        )
        assertEquals(
            listOf("claude"),
            conversationSessions(sessions, listOf(liveTab), "", agentFilter = "claude").map { it.id },
        )
        assertEquals(
            listOf("star"),
            conversationSessions(sessions, listOf(liveTab), "", withFiles = setOf("star"), filesOnly = true)
                .map { it.id },
        )
    }

    @Test
    fun broughtInSessionsSitBelowTheirMasterAndCanBeFolded() {
        val sessions = listOf(
            session("child", "Second agent", lastActive = 30),
            session("other", "Other", lastActive = 20),
            session("master", "Main work", lastActive = 10),
        )
        val lineage = mapOf("child" to "master")

        assertEquals(
            listOf("other", "master", "child"),
            conversationSessions(sessions, emptyList(), "", broughtIn = lineage).map { it.id },
        )
        assertEquals(
            listOf("other", "master"),
            conversationSessions(
                sessions,
                emptyList(),
                "",
                broughtIn = lineage,
                foldedCrews = setOf("master"),
            ).map { it.id },
        )
    }

    @Test
    fun attachedImagePathsBecomeCompactRowsWithoutHidingTheMessage() {
        val content = splitConversationAttachments(
            """Compare these two screenshots.

                Attached images:
                - /home/matt/Projects/aiterm/.aiterm/attachments/one.jpg
                - /home/matt/Projects/aiterm/.aiterm/attachments/two.jpg""".trimIndent(),
        )

        assertEquals("Compare these two screenshots.", content.text)
        assertEquals(
            listOf(
                "/home/matt/Projects/aiterm/.aiterm/attachments/one.jpg",
                "/home/matt/Projects/aiterm/.aiterm/attachments/two.jpg",
            ),
            content.imagePaths,
        )
    }

    @Test
    fun ordinaryListsAreNotMistakenForAttachments() {
        val text = "Files to inspect:\n- first.kt\n- second.kt"

        assertEquals(ConversationAttachmentContent(text, emptyList()), splitConversationAttachments(text))
    }

    @Test
    fun toolRowsUseReadableLabelsAndSingleLineSummaries() {
        assertEquals("Command", conversationActivityLabel("exec"))
        assertEquals("File edit", conversationActivityLabel("apply_patch"))
        assertEquals("Output", conversationActivityLabel("tool_output"))
        assertEquals("Agent message", conversationActivityLabel("agent_message"))
        assertEquals("Read file", conversationActivityLabel("read_file"))
        assertEquals("cargo test --all", conversationActivitySummary("cargo test\n  --all"))
        assertTrue(conversationActivitySummary("x".repeat(200)).endsWith("…"))
        assertTrue(conversationActivitySummary("x".repeat(200)).length <= 110)
    }

    @Test
    fun consecutiveToolCallsBecomeOneExpandableTimelineGroup() {
        val user = RemotePreviewMessage("user", "Please test it.")
        val command = RemotePreviewMessage("exec", "cargo test")
        val edit = RemotePreviewMessage("apply_patch", "src/main.rs")
        val read = RemotePreviewMessage("read_file", "src/main.rs")
        val assistant = RemotePreviewMessage("assistant", "Everything passes.")

        val timeline = conversationTimeline(listOf(user, command, edit, read, assistant))

        assertEquals(3, timeline.size)
        assertEquals(ConversationTimelineItem.Turn(user), timeline[0])
        assertEquals(
            ConversationTimelineItem.ActivityGroup(listOf(command, edit, read)),
            timeline[1],
        )
        assertEquals(ConversationTimelineItem.Turn(assistant), timeline[2])
    }

    @Test
    fun aSingleToolCallDoesNotGainAnUnnecessarySecondDisclosureLayer() {
        val command = RemotePreviewMessage("exec", "pwd")

        assertEquals(
            listOf(ConversationTimelineItem.Turn(command)),
            conversationTimeline(listOf(command)),
        )
    }

    private fun session(
        id: String,
        title: String,
        agent: String = "codex",
        project: String = "/work/project",
        lastActive: Long = 0,
    ) = RemoteSession(
        id = id,
        agent = agent,
        title = title,
        projectPath = project,
        groupPath = project,
        branch = null,
        forked = false,
        background = false,
        forkParent = null,
        lastActive = lastActive,
    )
}
