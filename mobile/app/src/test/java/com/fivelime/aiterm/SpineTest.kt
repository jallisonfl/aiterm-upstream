package com.fivelime.aiterm

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/** The phone's half of docs/architecture/spine.md. Everything here is the wire shape the
 *  desktop's `SpineEvent` serialises to: flat, with `kind` as the tag. */
class SpineTest {
    private fun obj(s: String): JsonObject = Json.parseToJsonElement(s).jsonObject

    private fun ev(seq: Long, kindJson: String, epoch: Long = 7L): SpineEvent =
        SpineEvent.parse(obj("""{"seq":$seq,"epoch":$epoch,"session_id":"s1","agent":"claude","ts":100,$kindJson"""))!!

    private val text1 = """"kind":"agent_text","id":"m1","text":"Hel","done":false}"""
    private val text1Full = """"kind":"agent_text","id":"m1","text":"Hello there","done":true}"""
    private val user1 = """"kind":"user_message","id":"u1","text":"do it"}"""

    // ---- parsing

    @Test fun parsesEveryKind() {
        val u = ev(1, user1)
        assertEquals(1L, u.seq); assertEquals(7L, u.epoch)
        assertEquals("s1", u.sessionId); assertEquals("claude", u.agent); assertEquals(100L, u.ts)
        assertEquals(SpineKind.UserMessage("u1", "do it"), u.kind)

        assertEquals(SpineKind.AgentText("m1", "Hel", false), ev(2, text1).kind)
        assertEquals(
            SpineKind.AgentThought("t1", "hmm", true),
            ev(3, """"kind":"agent_thought","id":"t1","text":"hmm","done":true}""").kind,
        )
        assertEquals(
            SpineKind.ToolCall("c1", "Bash", "Run tests", ToolCategory.Execute, "cargo test", ToolStatus.Running),
            ev(4, """"kind":"tool_call","id":"c1","tool":"Bash","title":"Run tests","category":"execute","input":"cargo test","status":"running"}""").kind,
        )
        assertEquals(
            SpineKind.ToolCallUpdate("c1", ToolStatus.Completed, "ok"),
            ev(5, """"kind":"tool_call_update","id":"c1","status":"completed","output":"ok"}""").kind,
        )
        // output is optional on the wire.
        assertEquals(
            SpineKind.ToolCallUpdate("c1", ToolStatus.Failed, null),
            ev(6, """"kind":"tool_call_update","id":"c1","status":"failed"}""").kind,
        )
        assertEquals(SpineKind.TurnStarted("t7"), ev(7, """"kind":"turn_started","turn":"t7"}""").kind)
        assertEquals(SpineKind.TurnEnded("t7", "completed"), ev(8, """"kind":"turn_ended","turn":"t7","reason":"completed"}""").kind)
        assertEquals(
            SpineKind.PhaseChanged(SpinePhase.NeedsYou, "permission: Edit foo.rs"),
            ev(9, """"kind":"phase","phase":"needs_you","detail":"permission: Edit foo.rs"}""").kind,
        )
        assertSame(SpineKind.Reset, ev(10, """"kind":"reset"}""").kind)
    }

    @Test fun unknownValuesFallBackRatherThanThrow() {
        val k = ev(1, """"kind":"tool_call","id":"c1","tool":"X","title":"X","category":"telepathy","input":"","status":"levitating"}""").kind
        assertEquals(ToolCategory.Other, (k as SpineKind.ToolCall).category)
        assertEquals(ToolStatus.Pending, k.status)
        assertEquals(SpinePhase.Working, (ev(2, """"kind":"phase","phase":"dreaming","detail":""}""").kind as SpineKind.PhaseChanged).phase)
    }

    @Test fun unknownKindIsIgnored() {
        assertNull(SpineEvent.parse(obj("""{"seq":1,"epoch":7,"session_id":"s1","agent":"claude","ts":1,"kind":"telemetry","x":1}""")))
        // …and a whole response keeps the events it does understand.
        val r = SpineResponse.parse(obj(
            """{"epoch":7,"live":true,"events":[
               {"seq":1,"epoch":7,"session_id":"s1","agent":"claude","ts":1,"kind":"telemetry"},
               {"seq":2,"epoch":7,"session_id":"s1","agent":"claude","ts":1,$user1]}""",
        ))
        assertEquals(1, r.events.size)
        assertEquals(2L, r.events[0].seq)
    }

    @Test fun responseCarriesEpochAndLive() {
        val r = SpineResponse.parse(obj("""{"epoch":42,"live":false,"events":[]}"""))
        assertEquals(42L, r.epoch)
        assertEquals(false, r.live)
    }

    // ---- the store

    @Test fun upsertByIdKeepsPositionAndUpdatesText() {
        val st = ConversationStore()
        st.replay(listOf(ev(1, user1), ev(2, text1), ev(3, """"kind":"agent_thought","id":"t1","text":"hmm","done":true}""")))
        assertEquals(listOf("u1", "m1", "t1"), st.items.map { it.key })
        st.replay(listOf(ev(4, text1Full)))
        assertEquals(listOf("u1", "m1", "t1"), st.items.map { it.key })
        val block = st.items[1] as Item.AgentText
        assertEquals("Hello there", block.text)
        assertTrue(block.done)
    }

    @Test fun toolCallThenUpdateIsOneRow() {
        val st = ConversationStore()
        st.replay(listOf(
            ev(1, """"kind":"tool_call","id":"c1","tool":"Edit","title":"Edit main.rs","category":"edit","input":"main.rs","status":"pending"}"""),
            ev(2, """"kind":"tool_call_update","id":"c1","status":"running"}"""),
            ev(3, """"kind":"tool_call_update","id":"c1","status":"completed","output":"2 lines"}"""),
        ))
        assertEquals(1, st.items.size)
        val t = st.items[0] as Item.Tool
        assertEquals(ToolStatus.Completed, t.status)
        assertEquals("2 lines", t.output)
        assertEquals(ToolCategory.Edit, t.category)
        assertEquals("Edit main.rs", t.title)
        // An update for a call we never saw draws no blank card.
        st.replay(listOf(ev(4, """"kind":"tool_call_update","id":"ghost","status":"completed"}""")))
        assertEquals(1, st.items.size)
    }

    @Test fun seqRule() {
        val st = ConversationStore()
        assertEquals(Offer.Applied, st.offer(ev(1, user1)))
        assertEquals(Offer.Applied, st.offer(ev(2, text1)))
        assertEquals(Offer.Stale, st.offer(ev(2, text1)))
        assertEquals(Offer.Stale, st.offer(ev(1, user1)))
        assertEquals(Offer.Gap, st.offer(ev(9, text1Full)))
        assertEquals(2L, st.lastSeq)             // a gap applies nothing
        assertEquals("Hel", (st.items[1] as Item.AgentText).text)
        assertEquals(Offer.EpochChanged, st.offer(ev(3, text1Full, epoch = 8L)))
        assertEquals(2L, st.lastSeq)
    }

    @Test fun replayDedupesBySeq() {
        val st = ConversationStore()
        st.offer(ev(1, user1))
        st.offer(ev(2, text1))
        // The refetch after a gap overlaps what the WebSocket already applied.
        st.replay(SpineResponse(7, true, listOf(ev(1, user1), ev(2, text1), ev(3, text1Full), ev(4, """"kind":"user_message","id":"u2","text":"more"}"""))))
        assertEquals(4L, st.lastSeq)
        assertEquals(listOf("u1", "m1", "u2"), st.items.map { it.key })
        assertEquals("Hello there", (st.items[1] as Item.AgentText).text)
    }

    @Test fun replayOnANewEpochStartsClean() {
        val st = ConversationStore()
        st.offer(ev(1, user1))
        st.replay(SpineResponse(9, true, listOf(ev(1, """"kind":"user_message","id":"u9","text":"after restart"}""", epoch = 9L))))
        assertEquals(9L, st.epoch)
        assertEquals(1L, st.lastSeq)
        assertEquals(listOf("u9"), st.items.map { it.key })
    }

    @Test fun resetClears() {
        val st = ConversationStore()
        st.replay(listOf(ev(1, user1), ev(2, text1Full), ev(3, """"kind":"turn_started","turn":"t1"}""")))
        assertEquals(2, st.items.size)
        st.replay(listOf(ev(4, """"kind":"reset"}""")))
        assertEquals(0, st.items.size)
        assertNull(st.currentTurn)
        // The seq cursor survives: the rebuilt history follows on the stream.
        assertEquals(4L, st.lastSeq)
        st.replay(listOf(ev(5, user1)))
        assertEquals(listOf("u1"), st.items.map { it.key })
    }

    @Test fun phaseAndTurns() {
        val st = ConversationStore()
        assertEquals(SpinePhase.Idle, st.phase)
        assertEquals(false, st.phaseSeen)
        st.replay(listOf(
            ev(1, """"kind":"turn_started","turn":"t1"}"""),
            ev(2, """"kind":"phase","phase":"working","detail":"running Bash"}"""),
        ))
        assertEquals("t1", st.currentTurn)
        assertEquals(SpinePhase.Working, st.phase)
        assertEquals("running Bash", st.phaseDetail)
        assertTrue(st.phaseSeen)
        st.replay(listOf(
            ev(3, """"kind":"turn_ended","turn":"t1","reason":"completed"}"""),
            ev(4, """"kind":"phase","phase":"idle","detail":""}"""),
        ))
        assertNull(st.currentTurn)
        assertEquals(SpinePhase.Idle, st.phase)
        assertEquals(Item.TurnEnd("t1", "completed"), st.items.last())
    }

    @Test fun theEchoOfASentMessageRetiresOnTheRealOne() {
        val st = ConversationStore()
        st.echoUser("do it", 1234)
        assertEquals(1, st.items.size)
        st.replay(listOf(ev(1, user1)))
        assertEquals(listOf("u1"), st.items.map { it.key })
    }

    @Test fun clearForgetsEverything() {
        val st = ConversationStore()
        st.replay(SpineResponse(7, false, listOf(ev(1, user1))))
        st.clear()
        assertEquals(0, st.items.size)
        assertEquals(0L, st.lastSeq)
        assertEquals(0L, st.epoch)
        assertTrue(st.live)
    }

    @Test fun legacyTranscriptMapsOntoRowsByOrdinal() {
        val st = ConversationStore()
        st.legacy(listOf(Turn("user", "hi"), Turn("assistant", "one moment"), Turn("Bash", "ls -la\ntotal 4")))
        assertEquals(listOf("legacy-0", "legacy-1", "legacy-2"), st.items.map { it.key })
        val tool = st.items[2] as Item.Tool
        assertEquals(ToolCategory.Execute, tool.category)
        assertEquals("ls -la", tool.input)
        // The last block grows in place, keeping its key.
        st.legacy(listOf(Turn("user", "hi"), Turn("assistant", "one moment — done")))
        assertEquals(listOf("legacy-0", "legacy-1"), st.items.map { it.key })
        assertEquals("one moment — done", (st.items[1] as Item.AgentText).text)
    }
}
