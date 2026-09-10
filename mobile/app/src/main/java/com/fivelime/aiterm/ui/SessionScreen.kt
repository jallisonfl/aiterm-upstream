package com.fivelime.aiterm.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.HorizontalDivider
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import coil3.compose.AsyncImage
import com.fivelime.aiterm.FileEntry
import androidx.compose.runtime.key
import androidx.compose.runtime.saveable.rememberSaveable
import com.fivelime.aiterm.parseSubagentMessage
import com.fivelime.aiterm.SubagentMessage
import com.fivelime.aiterm.timeline
import com.fivelime.aiterm.TimelineItem
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Stop
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Build
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Folder
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.KeyboardArrowUp
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Description
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.Language
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Psychology
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Terminal
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.foundation.layout.size
import com.fivelime.aiterm.SessionState
import androidx.compose.material3.Badge
import androidx.compose.material3.Button
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.RadioButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.material3.BadgedBox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.ui.platform.ClipEntry
import androidx.compose.ui.platform.LocalClipboard
import androidx.compose.ui.platform.LocalContext
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.Code
import androidx.compose.material.icons.filled.Share
import androidx.compose.material.icons.filled.Replay
import androidx.compose.material.icons.filled.SelectAll
import androidx.compose.material.icons.filled.Close
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.runtime.Composable
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import kotlinx.coroutines.launch
import androidx.compose.runtime.LaunchedEffect
import com.fivelime.aiterm.Diag
import androidx.compose.ui.layout.boundsInRoot
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.withFrameNanos
import kotlinx.coroutines.flow.first
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.graphics.vector.ImageVector
import com.fivelime.aiterm.AppViewModel
import com.fivelime.aiterm.Item
import com.fivelime.aiterm.Session
import com.fivelime.aiterm.ModelOption
import com.fivelime.aiterm.SpinePhase
import com.fivelime.aiterm.ToolCategory
import com.fivelime.aiterm.ToolStatus

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SessionScreen(vm: AppViewModel, s: Session, outer: PaddingValues) {
    val open = s.id in vm.open
    val running = s.id in vm.running
    // The spine carries the phase on the same stream as the content, so it
    // moves with the transcript instead of trailing a list refresh. It only
    // speaks up when it has something to say: idle falls back to the list's
    // own view, which knows about on-desktop, running, and a just-sent
    // message the desktop has not reported yet.
    val state = when (vm.phase) {
        SpinePhase.Working -> SessionState.Working
        SpinePhase.NeedsYou -> SessionState.NeedsYou
        SpinePhase.Idle -> vm.stateOf(s)
    }
    val working = state == SessionState.Working
    var draft by remember(s.id) { mutableStateOf("") }
    // A long-pressed message: its actions come up in a sheet.
    var heldItem by remember(s.id) { mutableStateOf<Item?>(null) }
    heldItem?.let { held ->
        // "Ask again" on an answer resends the last thing the person said
        // before it.
        val promptAbove = if (held is Item.AgentText) {
            val at = vm.items.indexOfFirst { it.key == held.key }
            (vm.items.take(maxOf(at, 0)).lastOrNull { it is Item.User } as? Item.User)?.let { personSaid(it.text) }?.takeIf { it.isNotEmpty() }
        } else null
        MessageSheet(held, promptAbove, onDismiss = { heldItem = null }, onEdit = { draft = it }, onSend = { vm.send(it) })
    }
    var menu by remember { mutableStateOf(false) }
    var renaming by remember { mutableStateOf(false) }
    var bringingIn by remember { mutableStateOf(false) }
    if (bringingIn) {
        BringInDialog(vm, s, onDismiss = { bringingIn = false })
    }
    if (renaming) {
        RenameDialog(current = s.title, onDone = { vm.rename(s, it); renaming = false }, onDismiss = { renaming = false })
    }
    val list = rememberLazyListState()

    // Tool calls and subagent updates fold into one Activity row each run.

    val rows = remember(vm.items) { timeline(vm.items) }

    val made = vm.files.filter { it.via == "made" || it.via == "edited" || it.via == "wrote" }

    // The list is PINNED to its end, the way a chat is: whatever moves the
    // end out of view — the first fill landing a frame before the layout,
    // a block growing as it streams, a thumbnail or a table taking its
    // height a frame late, and above all the viewport SHRINKING after the
    // landing: a crew banner or the relay banner arriving in the bottom
    // bar a beat after open, the quick keys when the session needs you,
    // the keyboard — is followed, so long as no finger is on the list. The
    // bottom-bar case was the one that hid: nothing in the list changed,
    // so the old follow (keyed on the items) never fired, and a session
    // that had opened at its end sat a banner's height short of it on
    // every harness [observed 2026-09-03].
    //
    // A person scrolling up into history unpins; scrolling back to the
    // end (or the Newest pill) pins again. And the list stays INVISIBLE
    // until a real layout says it is at the end — the first frame is drawn
    // before the jump lands, and the top of the transcript used to flash
    // by on every open.
    var landed by remember(s.id) { mutableStateOf(false) }
    var pinned by remember(s.id) { mutableStateOf(true) }
    var seenViewport by remember(s.id) { mutableStateOf(-1) }
    fun atEnd(): Boolean {
        val info = list.layoutInfo
        val last = info.visibleItemsInfo.lastOrNull() ?: return info.totalItemsCount == 0
        return last.index == info.totalItemsCount - 1 && last.offset + last.size <= info.viewportEndOffset + 1
    }
    // A scroll that ends away from the end unpins; one that ends at it pins.
    LaunchedEffect(s.id) {
        snapshotFlow { list.isScrollInProgress }.collect { moving -> if (!moving) pinned = atEnd() }
    }
    LaunchedEffect(s.id) {
        snapshotFlow { vm.items.size }.first { it > 0 }
        val start = withFrameNanos { it }
        // Whatever the layout is doing, half a second of blank is the most
        // a person should wait to see their session.
        launch { while (!landed) { if (withFrameNanos { it } - start > 500_000_000L) landed = true } }
        snapshotFlow {
            val info = list.layoutInfo
            val last = info.visibleItemsInfo.lastOrNull()
            // Everything that can move the end: how many items, the last
            // visible one's place and extent, and the viewport itself.
            listOf(info.totalItemsCount, last?.index ?: -1, last?.let { it.offset + it.size } ?: 0,
                info.viewportStartOffset, info.viewportEndOffset)
        }.collect {
            val total = list.layoutInfo.totalItemsCount
            if (total == 0 || list.isScrollInProgress) return@collect
            if (atEnd()) { landed = true; return@collect }
            if (!pinned) return@collect
            // Land on the END of the last item: a block taller than the
            // screen shows its newest words, not its first.
            val info = list.layoutInfo
            val end = info.visibleItemsInfo.firstOrNull { it.index == total - 1 }
            val viewport = info.viewportEndOffset - info.viewportStartOffset
            val offset = ((end?.size ?: 0) - viewport).coerceAtLeast(0)
            // The first landing, and every later correction that came with a
            // viewport change (a banner, the keyboard), on the record: this is
            // the trail for "it opened short of the bottom".
            val last = info.visibleItemsInfo.lastOrNull()
            if (!landed || viewport != seenViewport) Diag.log(
                "land", "${s.id.take(8)} total=$total last=${last?.index}@${last?.let { it.offset + it.size }}/${info.viewportEndOffset} " +
                    "viewport=$viewport (was $seenViewport) lastSize=${end?.size} offset=$offset ${if (landed) "correct" else "first"}",
            )
            seenViewport = viewport
            // Before the first landing, jump; after it, glide — that is a
            // block growing under the eye, and a jump every token jitters.
            if (!landed) list.scrollToItem(total - 1, offset) else list.animateScrollToItem(total - 1, offset)
        }
    }

    // Where the composer sits, so a tap there keeps the keyboard.
    var barBounds by remember { mutableStateOf<androidx.compose.ui.geometry.Rect?>(null) }
    Scaffold(
        modifier = Modifier.padding(outer).imePadding().dismissKeyboardOnTapOutside { barBounds },
        containerColor = Bg,
        topBar = {
            TopAppBar(
                navigationIcon = { IconButton(onClick = { vm.select(null) }) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
                title = {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        AgentIcon(s.agent, 26.dp)
                        Spacer(Modifier.width(10.dp))
                        Column {
                            Text(s.title.ifBlank { "Untitled" }, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.titleMedium)
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text(folderName(s.group_path), style = MaterialTheme.typography.labelSmall, color = Muted)
                                Spacer(Modifier.width(10.dp))
                                StateChip(stateLabel(state) ?: "idle", stateColor(state), pulse = working)
                            }
                        }
                    }
                },
                actions = {
                    // Count what the agent produced, not every file the
                    // folder walk caught — "Files 87" full of build noise
                    // says nothing.
                    val produced = vm.files.count { it.via == "made" || it.via == "edited" || it.via == "wrote" }
                    IconButton(onClick = { vm.showFiles = !vm.showFiles }) {
                        BadgedBox(badge = { if (produced > 0) Badge { Text("$produced") } }) {
                            Icon(Icons.Filled.Folder, "Files", tint = if (vm.showFiles) Accent else Muted)
                        }
                    }
                    // A page to look at: a dev server the session started,
                    // or — servers get killed, sandboxed, orphaned — the
                    // index.html it built, served as static files.
                    val servedPort = vm.ports[s.id]?.firstOrNull()
                    val builtPage = vm.files
                        .filter { (it.ext == "html" || it.ext == "htm") && (it.via == "made" || it.via == "edited" || it.via == "wrote") }
                        .minByOrNull { if (it.name == "index.html") 0 else 1 }
                    if (servedPort != null || builtPage != null) {
                        IconButton(onClick = {
                            if (servedPort != null) vm.previewPort(servedPort) else vm.previewFile(builtPage!!.path)
                        }) { Icon(Icons.Filled.Language, "Preview", tint = Accent) }
                    }
                    if (!open) IconButton(onClick = { vm.openOnDesktop(s) }) {
                        Icon(Icons.Filled.PlayArrow, "Open on desktop", tint = Green)
                    }
                    IconButton(onClick = { menu = true }) { Icon(Icons.Filled.MoreVert, "More") }
                    DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                        DropdownMenuItem(
                            text = { Text("Bring in a second agent") },
                            onClick = { menu = false; bringingIn = true },
                        )
                        DropdownMenuItem(
                            text = { Text(if (s.id in vm.stars) "Unstar" else "Star — keep on top") },
                            onClick = { menu = false; vm.toggleStar(s) },
                        )
                        DropdownMenuItem(text = { Text("Rename") }, onClick = { menu = false; renaming = true })
                        if (open) DropdownMenuItem(text = { Text("Interrupt (Esc)") }, onClick = { menu = false; vm.interrupt(s) })
                        if (running || open) DropdownMenuItem(text = { Text("Stop session") }, onClick = { menu = false; vm.stop(s) })
                        DropdownMenuItem(text = { Text("Refresh") }, onClick = { menu = false; vm.select(s) })
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = Bg),
            )
        },
        bottomBar = {
            Column(Modifier.fillMaxWidth().navigationBarsPadding().onGloballyPositioned { barBounds = it.boundsInRoot() }) {
            vm.relays[s.id]?.let { r -> RelayBanner(vm, s, r) }
            // A brought-in agent parked on a prompt is invisible from here —
            // its dialog lives in its own terminal. Say so on the master's
            // screen, where the person actually is, and take them there.
            vm.crewNeedsYou(s).forEach { c -> CrewNeedsYouBanner(c) { vm.select(c) } }
            // This session itself is waiting on a person: the dialog is a TUI
            // the conversation view cannot render, so offer the keys that
            // answer one. Raw, no Enter appended — Enter is one of the keys.
            if (state == SessionState.NeedsYou) QuickKeysBar { k -> vm.sendKeys(s, k) }
            AttachmentChips(vm)
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 6.dp, vertical = 8.dp),
                verticalAlignment = Alignment.Bottom,
            ) {
                AttachButton(vm)
                OutlinedTextField(
                    value = draft, onValueChange = { draft = it },
                    modifier = Modifier.weight(1f),
                    placeholder = { Text(if (open) "Message ${s.agent}…" else "Message ${s.agent} — opens it on the desktop first") },
                    maxLines = 6,
                    shape = RoundedCornerShape(20.dp),
                )
                Spacer(Modifier.width(6.dp))
                when {
                    vm.sending -> Box(Modifier.padding(12.dp)) { CircularProgressIndicator(Modifier.size(24.dp), strokeWidth = 2.dp) }
                    // While the agent works, the button is Stop: one tap ends the
                    // turn (Escape) and keeps the session. Send comes back after.
                    working && draft.isBlank() -> FilledIconButton(
                        onClick = { vm.interrupt(s) },
                        colors = IconButtonDefaults.filledIconButtonColors(containerColor = Amber.copy(alpha = 0.2f), contentColor = Amber),
                    ) { Icon(Icons.Filled.Stop, "Stop the current turn") }
                    else -> IconButton(
                        onClick = { val t = draft.trim(); if (t.isNotEmpty() || vm.attachments.isNotEmpty()) { vm.send(t); draft = "" } },
                        enabled = draft.isNotBlank() || vm.attachments.isNotEmpty(),
                    ) { Icon(Icons.AutoMirrored.Filled.Send, "Send", tint = if (draft.isBlank()) Muted else Accent) }
                }
            }
            }
        },
    ) { outerPad ->
        Column(Modifier.fillMaxSize().padding(outerPad)) {
        CrewStrip(vm, s)
        val padding = PaddingValues(0.dp)
        Box(Modifier.weight(1f)) {
        if (vm.showFiles) {
            FilesList(vm, Modifier.padding(padding))
        } else if (vm.items.isEmpty()) {
            Box(Modifier.fillMaxSize().padding(padding), contentAlignment = Alignment.Center) {
                if (vm.loadingTurns) CircularProgressIndicator() else Text("Nothing here yet — say something.", color = Muted)
            }
        } else {
            // A LazyColumn draws no scrollbar, so a long transcript gives no
            // sense of place. Two quiet cues: a thin thumb along the right
            // edge while scrolling — its height says how much there is, its
            // position says where you are — and a pill at the foot whenever
            // the newest message is out of sight, one tap from the end.
            val scope = rememberCoroutineScope()
            val thumbColor = Muted
            val thumbAlpha by animateFloatAsState(
                targetValue = if (list.isScrollInProgress) 0.5f else 0f,
                animationSpec = tween(if (list.isScrollInProgress) 80 else 900),
                label = "thumb",
            )
            // Pixel truth for the thumb: remember the real height of every
            // message the list has laid out, estimate the unseen with the
            // running average, and place the thumb by pixels — not by item
            // counts, which jump with every tall or short message and made
            // the thumb breathe and jitter. Plain map, not state: it feeds
            // the next draw, it never drives recomposition.
            val heights = remember(s.id) { HashMap<Int, Int>() }
            val spacingPx = with(androidx.compose.ui.platform.LocalDensity.current) { 8.dp.toPx() }
            val awayFromEnd by remember {
                derivedStateOf {
                    val info = list.layoutInfo
                    val last = info.visibleItemsInfo.lastOrNull()?.index ?: 0
                    info.totalItemsCount > 0 && last < info.totalItemsCount - 1
                }
            }
            Box(Modifier.fillMaxSize().padding(padding)) {
                LazyColumn(
                    state = list,
                    modifier = Modifier.fillMaxSize().graphicsLayer { alpha = if (landed) 1f else 0f }.drawWithContent {
                        drawContent()
                        val info = list.layoutInfo
                        val total = info.totalItemsCount
                        for (it in info.visibleItemsInfo) heights[it.index] = it.size
                        val first = info.visibleItemsInfo.firstOrNull()
                        if (thumbAlpha > 0f && first != null && total > info.visibleItemsInfo.size) {
                            var knownSum = 0L; var knownN = 0
                            for ((i, hgt) in heights) if (i < total) { knownSum += hgt; knownN++ }
                            val avg = if (knownN > 0) knownSum.toFloat() / knownN else 0f
                            val contentPx = knownSum + avg * (total - knownN) + spacingPx * (total - 1)
                            val viewport = (info.viewportEndOffset - info.viewportStartOffset).toFloat()
                            if (contentPx > viewport) {
                                var before = -first.offset.toFloat()
                                for (i in 0 until first.index) before += (heights[i]?.toFloat() ?: avg) + spacingPx
                                val h = (size.height * viewport / contentPx).coerceIn(32.dp.toPx(), size.height * 0.9f)
                                val progress = (before / (contentPx - viewport)).coerceIn(0f, 1f)
                                drawRoundRect(
                                    color = thumbColor,
                                    topLeft = Offset(size.width - 7.dp.toPx(), progress * (size.height - h)),
                                    size = Size(3.dp.toPx(), h),
                                    cornerRadius = CornerRadius(2.dp.toPx()),
                                    alpha = thumbAlpha,
                                )
                            }
                        }
                    },
                    contentPadding = PaddingValues(horizontal = 12.dp, vertical = 8.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    // Keyed by the spine's own ids: a block that grows
                    // updates one row in place, it does not re-key the list
                    // under the scroll position.
                    items(rows, key = { it.key }) { row ->
                        when (row) {
                            is TimelineItem.Row -> ItemView(row.item, onOpenPath = vm::openMentioned, onHold = { heldItem = it })
                            is TimelineItem.Activity -> ActivityGroup(row, onOpenPath = vm::openMentioned, onHold = { heldItem = it })
                        }
                    }
                    // What the session made, right where the conversation ends —
                    // the transcript often never prints a path (tool output is
                    // dropped at phone size), but the desktop's ledger knows.
                    if (made.isNotEmpty()) item(key = "made") { MadeStrip(vm, made) }
                    if (working) item(key = "working") { WorkingRow(s.agent, vm.phaseDetail) }
                }
                if (awayFromEnd) {
                    Row(
                        Modifier.align(Alignment.BottomCenter).padding(bottom = 10.dp)
                            .clip(RoundedCornerShape(50))
                            .background(Surface2)
                            .clickable {
                                scope.launch {
                                    val end = list.layoutInfo.totalItemsCount - 1
                                    if (end >= 0) list.animateScrollToItem(end)
                                }
                            }
                            .padding(horizontal = 14.dp, vertical = 7.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text("Newest", style = MaterialTheme.typography.labelMedium, color = Muted)
                        Icon(Icons.Filled.KeyboardArrowDown, "Jump to newest", tint = Muted, modifier = Modifier.size(18.dp))
                    }
                }
            }
        }
        }
        }
    }
}

/** The gap between "start" tapped and the session existing on disk, made to
 *  look like the conversation it is about to become: the ask echoed as a
 *  sent bubble, the engine "working" underneath. The real session replaces
 *  this screen the moment discovery finds it. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun StartingScreen(vm: AppViewModel, s: AppViewModel.Starting, outer: PaddingValues) {
    Scaffold(
        modifier = Modifier.padding(outer),
        containerColor = Bg,
        topBar = {
            TopAppBar(
                navigationIcon = { IconButton(onClick = { vm.cancelStarting() }) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
                title = {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        AgentIcon(s.agentId.removePrefix("api:"), 26.dp)
                        Spacer(Modifier.width(10.dp))
                        Column {
                            Text("Starting ${s.agentName}", style = MaterialTheme.typography.titleMedium)
                            Text(folderName(s.cwd), style = MaterialTheme.typography.labelSmall, color = Muted)
                        }
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = Bg),
            )
        },
    ) { padding ->
        Column(
            Modifier.fillMaxSize().padding(padding).padding(horizontal = 12.dp, vertical = 8.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            if (!s.prompt.isNullOrBlank()) {
                Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.CenterEnd) {
                    Box(
                        Modifier.widthIn(max = 320.dp)
                            .background(MaterialTheme.colorScheme.primaryContainer, RoundedCornerShape(18.dp, 18.dp, 4.dp, 18.dp))
                            .padding(12.dp),
                    ) { Text(s.prompt, color = MaterialTheme.colorScheme.onPrimaryContainer) }
                }
            }
            WorkingRow(s.agentId)
        }
    }
}

/** A brought-in agent is waiting on a person. Its approval dialog lives in
 *  its own terminal, so the master's screen names it and a tap goes there. */
@Composable
private fun CrewNeedsYouBanner(c: com.fivelime.aiterm.Session, onOpen: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().clickable(onClick = onOpen)
            .background(Amber.copy(alpha = 0.12f))
            .padding(horizontal = 12.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        AgentIcon(c.agent, 20.dp)
        Spacer(Modifier.width(8.dp))
        Text(
            "${c.agent} needs you — tap to answer",
            style = MaterialTheme.typography.labelMedium, color = Amber,
            modifier = Modifier.weight(1f),
        )
    }
}

/** The keys that answer a terminal dialog, for a session in "needs you":
 *  approve/deny shortcuts, digits for numbered pickers, arrows and Enter for
 *  selection lists, Esc to back out. Sent raw — the dialog reads keystrokes,
 *  not messages. */
@Composable
private fun QuickKeysBar(onKey: (String) -> Unit) {
    val keys = listOf(
        "Enter" to "\r", "y" to "y", "n" to "n",
        "1" to "1", "2" to "2", "3" to "3",
        "↑" to "\u001B[A", "↓" to "\u001B[B", "Esc" to "\u001B",
    )
    Row(
        Modifier.fillMaxWidth()
            .horizontalScroll(rememberScrollState())
            .padding(horizontal = 8.dp, vertical = 4.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text("Answer:", style = MaterialTheme.typography.labelSmall, color = Muted)
        keys.forEach { (label, seq) ->
            Box(
                Modifier.background(Amber.copy(alpha = 0.15f), RoundedCornerShape(12.dp))
                    .clickable { onKey(seq) }
                    .padding(horizontal = 12.dp, vertical = 6.dp),
            ) { Text(label, style = MaterialTheme.typography.labelMedium, color = Amber) }
        }
    }
}

/** Three dots that breathe. Shown while the desktop reports progress, or
 *  right after a message goes out — before the first progress arrives. */
@Composable
private fun WorkingRow(agent: String, detail: String = "") {
    Row(Modifier.padding(start = 4.dp, top = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        // The engine's own mark leads the row: in a session where a second
        // agent was brought in, "working" needs a face.
        AgentIcon(agent, 16.dp)
        Spacer(Modifier.width(8.dp))
        val t = androidx.compose.animation.core.rememberInfiniteTransition(label = "dots")
        val phase by t.animateFloat(0f, 3f, androidx.compose.animation.core.infiniteRepeatable(
            androidx.compose.animation.core.tween(900, easing = androidx.compose.animation.core.LinearEasing)), label = "phase")
        repeat(3) { i ->
            val on = ((phase.toInt() % 3) == i)
            Box(Modifier.padding(horizontal = 2.dp).size(7.dp).background(Amber.copy(alpha = if (on) 1f else 0.3f), RoundedCornerShape(50)))
        }
        Spacer(Modifier.width(8.dp))
        Text(
            detail.ifBlank { "$agent is working…" }, style = MaterialTheme.typography.labelMedium,
            color = Muted, maxLines = 1, overflow = TextOverflow.Ellipsis,
        )
    }
}

/** Codex folds its AGENTS.md and an environment block into the first user
 *  turn; the person only typed the last line. Hide the harness's part. */
private val HARNESS_BLOCKS = Regex("(?s)<(INSTRUCTIONS|environment_context|user_instructions|recommended_plugins)>.*?</\\1>\\s*")
private val HARNESS_HEADING = Regex("(?m)^#\\s*AGENTS\\.md instructions for \\S+\\s*$")
/** What the person typed, with the harness's preamble gone. Empty means
 *  the whole turn was scaffolding — codex sends AGENTS.md as its own
 *  message before the first real prompt — and deserves no bubble. */
private fun personSaid(text: String): String =
    text.replace(HARNESS_BLOCKS, "").replace(HARNESS_HEADING, "").trim()

/** File paths an agent says out loud — "Saved to: file:///…", "wrote
 *  /home/…/main.rs" — pulled from a turn so they can be tapped. Conservative:
 *  file:// URIs, and absolute paths bearing an extension. */
private val FILE_MENTION = Regex("""file:///\S+|(?:/[\w.@%+~-]+){2,}\.[A-Za-z0-9]{1,8}""")
private fun mentionedFiles(text: String): List<String> =
    FILE_MENTION.findAll(text).map { m ->
        val p = m.value.removePrefix("file://").trimEnd('.', ',', ':', ';', ')', ']', '"', '\'')
        if ('%' in p) runCatching { android.net.Uri.decode(p) }.getOrDefault(p) else p
    }.distinct().take(6).toList()

@Composable
private fun ItemView(item: Item, onOpenPath: (String) -> Unit = {}, onHold: (Item) -> Unit = {}) {
    // A long press on any row brings up its actions — copy, copy as
    // markdown, select text, share, and for the person's own message, edit
    // and send again. Taps (chips, tool-card folds, links) still work.
    when (item) {
        is Item.User -> UserBubble(item) { onHold(item) }
        is Item.AgentText -> AgentBlock(item, onOpenPath) { onHold(item) }
        is Item.Thought -> ThoughtBlock(item) { onHold(item) }
        is Item.Tool -> ToolCard(item) { onHold(item) }
        // A turn boundary: a hairline, so a long session reads as
        // exchanges rather than one wall.
        is Item.TurnEnd -> HorizontalDivider(
            Modifier.padding(vertical = 4.dp),
            color = Muted.copy(alpha = if (item.reason == "completed") 0.12f else 0.3f),
        )
    }
}

/** A user turn sits right in the accent colour. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun UserBubble(item: Item.User, onHold: () -> Unit) {
    val said = personSaid(item.text)
    if (said.isEmpty()) return
    Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.CenterEnd) {
        Box(
            Modifier.widthIn(max = 320.dp)
                .background(MaterialTheme.colorScheme.primaryContainer, RoundedCornerShape(18.dp, 18.dp, 4.dp, 18.dp))
                .combinedClickable(onClick = {}, onLongClick = onHold)
                .padding(12.dp),
        ) { MarkdownText(said, color = MaterialTheme.colorScheme.onPrimaryContainer) }
    }
}

/** The assistant, left, on the ground itself. A block still being written
 *  carries a cursor: the spine says `done:false` while more is coming, and
 *  without it a half-sentence reads as a finished thought. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun AgentBlock(item: Item.AgentText, onOpenPath: (String) -> Unit, onHold: () -> Unit) {
    val sub = remember(item.text) { parseSubagentMessage(item.text) }
    if (sub != null) { SubagentCard(item.id, sub, onHold); return }
    Column(Modifier.fillMaxWidth().combinedClickable(onClick = {}, onLongClick = onHold).padding(end = 12.dp)) {
        MarkdownText(item.text)
        if (!item.done) Caret()
        val paths = remember(item.text) { mentionedFiles(item.text) }
        if (paths.isNotEmpty()) Row(
            Modifier.padding(top = 6.dp),
            horizontalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            paths.forEach { p -> FileChip(p.substringAfterLast('/'), onClick = { onOpenPath(p) }) }
        }
    }
}

/** A subagent's update: who and what, the payload behind a tap. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun SubagentCard(id: String, m: SubagentMessage, onHold: () -> Unit) {
    var expanded by rememberSaveable(id) { mutableStateOf(false) }
    val hasBody = m.payload.isNotBlank()
    Column(
        Modifier.fillMaxWidth().background(Surface1.copy(alpha = 0.6f), RoundedCornerShape(10.dp))
            .combinedClickable(onClick = { if (hasBody) expanded = !expanded }, onLongClick = onHold)
            .padding(horizontal = 10.dp, vertical = 8.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.Filled.Psychology, null, tint = Accent, modifier = Modifier.size(14.dp))
            Spacer(Modifier.width(6.dp))
            Text(m.headline, style = MaterialTheme.typography.labelMedium, color = Accent, maxLines = 1,
                overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
            if (hasBody) Icon(if (expanded) Icons.Filled.KeyboardArrowUp else Icons.Filled.KeyboardArrowDown, null, tint = Muted, modifier = Modifier.size(16.dp))
        }
        if (expanded && hasBody) Box(Modifier.padding(top = 6.dp)) { MarkdownText(m.payload, color = Muted) }
    }
}

/** A run of tool calls and subagent updates, folded to one line: how many
 *  steps, how many still running. Open, each member draws as itself. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun ActivityGroup(group: TimelineItem.Activity, onOpenPath: (String) -> Unit, onHold: (Item) -> Unit) {
    var expanded by rememberSaveable(group.key) { mutableStateOf(false) }
    val n = group.items.size
    val running = group.running
    Column(
        Modifier.fillMaxWidth().background(Surface1.copy(alpha = 0.42f), RoundedCornerShape(10.dp))
            .combinedClickable(onClick = { expanded = !expanded }, onLongClick = { onHold(group.items.first()) })
            .padding(horizontal = 10.dp, vertical = 8.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.Filled.Build, null, tint = Accent, modifier = Modifier.size(14.dp))
            Spacer(Modifier.width(6.dp))
            Text(
                "Activity · $n " + if (n == 1) "step" else "steps",
                style = MaterialTheme.typography.labelMedium, color = Accent, maxLines = 1,
                overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f),
            )
            if (running > 0) {
                PulsingDot(Amber)
                Spacer(Modifier.width(4.dp))
                Text("$running running", style = MaterialTheme.typography.labelSmall, color = Muted)
                Spacer(Modifier.width(6.dp))
            }
            Icon(if (expanded) Icons.Filled.KeyboardArrowUp else Icons.Filled.KeyboardArrowDown, null, tint = Muted, modifier = Modifier.size(16.dp))
        }
        if (expanded) Column(Modifier.padding(top = 8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            group.items.forEach { item -> key(item.key) { ItemView(item, onOpenPath = onOpenPath, onHold = onHold) } }
        }
    }
}

@Composable
private fun Caret() {
    val t = androidx.compose.animation.core.rememberInfiniteTransition(label = "caret")
    val a by t.animateFloat(
        initialValue = 0.15f, targetValue = 0.9f,
        animationSpec = androidx.compose.animation.core.infiniteRepeatable(
            androidx.compose.animation.core.tween(650), androidx.compose.animation.core.RepeatMode.Reverse),
        label = "blink",
    )
    Box(Modifier.padding(top = 2.dp).size(width = 7.dp, height = 14.dp).background(Accent.copy(alpha = a), RoundedCornerShape(2.dp)))
}

/** The agent's reasoning: quiet, italic, folded to a line. Folded, the
 *  markdown marks come off so `**Checking the request**` reads as words;
 *  opened, it renders like any answer. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun ThoughtBlock(item: Item.Thought, onHold: () -> Unit) {
    var expanded by remember { mutableStateOf(false) }
    val mod = Modifier.fillMaxWidth().combinedClickable(onClick = { expanded = !expanded }, onLongClick = onHold).padding(horizontal = 4.dp, vertical = 2.dp)
    if (expanded) {
        Box(mod) { MarkdownText(item.text, color = Muted) }
    } else {
        Text(
            remember(item.text) { markdownPlain(item.text).replace('\n', ' ') },
            style = MaterialTheme.typography.bodySmall, color = Muted,
            fontStyle = androidx.compose.ui.text.font.FontStyle.Italic,
            maxLines = 2, overflow = TextOverflow.Ellipsis, modifier = mod,
        )
    }
}

/** A tool call, live: the mark its category earns, what it was asked to do,
 *  and where it stands. The output is folded behind a tap — the desktop
 *  already clipped it, but a phone screen is not where a diff belongs. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun ToolCard(item: Item.Tool, onHold: () -> Unit) {
    var expanded by remember { mutableStateOf(false) }
    val output = item.output?.takeIf { it.isNotBlank() }
    Column(
        Modifier.fillMaxWidth().background(Surface1.copy(alpha = 0.6f), RoundedCornerShape(10.dp))
            .combinedClickable(onClick = { if (output != null) expanded = !expanded }, onLongClick = onHold)
            .padding(horizontal = 10.dp, vertical = 8.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Icon(toolIcon(item.category), null, tint = Accent, modifier = Modifier.size(14.dp))
            Spacer(Modifier.width(6.dp))
            Text(
                item.title.ifBlank { item.tool }, style = MaterialTheme.typography.labelMedium,
                color = Accent, maxLines = 1, overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            Spacer(Modifier.width(8.dp))
            // The mark sits at the right edge, one column down the card.
            ToolMark(item.status)
        }
        if (item.input.isNotBlank()) Text(
            item.input, style = MaterialTheme.typography.labelSmall, fontFamily = FontFamily.Monospace,
            color = Muted, maxLines = if (expanded) 6 else 1, overflow = TextOverflow.Ellipsis,
            modifier = Modifier.padding(top = 3.dp),
        )
        if (expanded && output != null) Text(
            output, style = MaterialTheme.typography.bodySmall, fontFamily = FontFamily.Monospace, color = Muted,
            modifier = Modifier.padding(top = 6.dp),
        )
    }
}

private fun toolIcon(c: ToolCategory): ImageVector = when (c) {
    ToolCategory.Read -> Icons.Filled.Description
    ToolCategory.Edit -> Icons.Filled.Edit
    ToolCategory.Execute -> Icons.Filled.Terminal
    ToolCategory.Search -> Icons.Filled.Search
    ToolCategory.Fetch -> Icons.Filled.Language
    ToolCategory.Think -> Icons.Filled.Psychology
    ToolCategory.Other -> Icons.Filled.Build
}

/** Where a call stands, in the width of a dot: breathing while it runs, a
 *  tick when it lands, red when it did not. */
@Composable
private fun ToolMark(status: ToolStatus) = when (status) {
    ToolStatus.Pending, ToolStatus.Running -> PulsingDot(Amber)
    ToolStatus.Completed -> Icon(Icons.Filled.Check, "done", tint = Green, modifier = Modifier.size(14.dp))
    ToolStatus.Failed -> Icon(Icons.Filled.Close, "failed", tint = Red, modifier = Modifier.size(14.dp))
    ToolStatus.Cancelled -> Dot(Muted)
}

/** A small tappable pill for a file the conversation mentions. */
@Composable
private fun FileChip(name: String, onClick: () -> Unit) {
    Row(
        Modifier.clip(RoundedCornerShape(10.dp))
            .background(MaterialTheme.colorScheme.surfaceVariant)
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp, vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        val kind = FileEntry(name, name, 0, 0, "").kind
        Icon(iconFor(kind), null, Modifier.size(16.dp), tint = if (kind == "image" || kind == "video") Accent else Muted)
        Spacer(Modifier.width(6.dp))
        Text(name, style = MaterialTheme.typography.labelMedium, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}

/** What the agent produced, at the end of the conversation where the eye
 *  lands. Images show themselves; anything else is a pill. Tap for the full
 *  view. The transcript rarely prints a usable path — the desktop's change
 *  ledger is what knows, and it rides in on /v1/sessions/{id}/files. */
@Composable
private fun MadeStrip(vm: AppViewModel, made: List<FileEntry>) {
    Column(Modifier.fillMaxWidth().padding(top = 4.dp)) {
        Text("Files from this session", style = MaterialTheme.typography.labelSmall, color = Muted)
        Spacer(Modifier.height(6.dp))
        LazyRow(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            items(made.take(12), key = { it.path }) { f ->
                if (f.kind == "image") ImageCard(vm, f) else FileChip(f.name, onClick = { vm.open(f) })
            }
        }
    }
}

/** An image the agent made, visible right in the chat. */
@Composable
private fun ImageCard(vm: AppViewModel, f: FileEntry) {
    LaunchedEffect(f.path) { vm.fetchInline(f) }
    val local = vm.inlineFiles[f.path]
    Box(
        Modifier.size(width = 168.dp, height = 126.dp)
            .clip(RoundedCornerShape(12.dp))
            .background(MaterialTheme.colorScheme.surfaceVariant)
            .clickable { vm.open(f) },
        contentAlignment = Alignment.Center,
    ) {
        if (local != null) AsyncImage(model = local, contentDescription = f.name, contentScale = ContentScale.Crop, modifier = Modifier.fillMaxSize())
        else CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
    }
}

/** Who joins, what they look at, how long they talk — a bottom sheet
 *  built for a thumb: the choices are steps down a card, the note to them
 *  sits last so the keyboard has it in view, the body scrolls, and the
 *  button stays put beneath it whatever the keyboard does. The desktop
 *  runs the relay; the whole exchange appears in this conversation, live. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun BringInDialog(vm: AppViewModel, s: Session, onDismiss: () -> Unit) {
    var agentId by remember {
        mutableStateOf(vm.agents.firstOrNull { it.id != s.agent }?.id ?: vm.agents.firstOrNull()?.id ?: "claude")
    }
    var focus by remember { mutableStateOf("") }
    var rounds by remember { mutableStateOf(2) }
    var auto by remember { mutableStateOf(false) }
    var modelPicker by remember { mutableStateOf(false) }
    // API choices (OpenRouter et al) need a model; CLIs default to their own.
    val sel = vm.agents.firstOrNull { it.id == agentId }
    var model by remember(agentId) {
        mutableStateOf(if (agentId.startsWith("api:")) vm.agents.firstOrNull { it.id == agentId }?.models?.firstOrNull()?.id else null)
    }
    val host = s.agent.replaceFirstChar { it.uppercase() }
    val lengths = listOf(
        "1" to "Quick — they read the session and write once",
        "2" to "Normal — they write, $host replies, they answer",
        "3" to "Long — two replies back and forth",
    )
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = Surface1,
    ) {
        Column(Modifier.fillMaxWidth().imePadding().navigationBarsPadding()) {
            Column(
                Modifier.weight(1f, fill = false).verticalScroll(rememberScrollState()).padding(horizontal = 20.dp),
            ) {
                Text("Bring in a second agent", style = MaterialTheme.typography.titleLarge)
                Spacer(Modifier.height(4.dp))
                Text(
                    "They read this chat and talk it out with $host right here. No files change; you decide after.",
                    style = MaterialTheme.typography.bodySmall, color = Muted,
                )
                Spacer(Modifier.height(16.dp))
                Column(
                    Modifier.fillMaxWidth().background(Bg, RoundedCornerShape(18.dp)).padding(horizontal = 14.dp, vertical = 4.dp),
                ) {
                    ChoiceRow("Agent") {
                        PickerChip(
                            label = sel?.display_name ?: "Choose",
                            options = vm.agents.map { it.id to (if (it.id == s.agent) "${it.display_name}  ·  already here" else it.display_name) },
                            onPick = { agentId = it },
                            leading = { AgentIcon(agentId, 16.dp) },
                            icon = { id -> AgentIcon(id, 20.dp) },
                        )
                    }
                    if (!sel?.models.isNullOrEmpty()) {
                        HorizontalDivider(color = Surface2)
                        ChoiceRow("Model") {
                            ChipButton(
                                label = sel!!.models.firstOrNull { it.id == model }?.display_name ?: "Default",
                                onClick = { modelPicker = true },
                                leading = model?.let { { AgentIcon(modelBrand(it, agentId.removePrefix("api:")), 16.dp) } },
                            )
                        }
                    }
                    HorizontalDivider(color = Surface2)
                    ChoiceRow("Length") {
                        PickerChip(
                            label = lengths.first { it.first == rounds.toString() }.second.substringBefore(" —"),
                            options = lengths,
                            onPick = { rounds = it.toInt() },
                        )
                    }
                    HorizontalDivider(color = Surface2)
                    ChoiceRow("Auto-approve") {
                        androidx.compose.material3.Switch(checked = auto, onCheckedChange = { auto = it })
                    }
                }
                Spacer(Modifier.height(6.dp))
                Text(
                    if (auto) "When they finish, $host proceeds as approved instead of waiting for you."
                    else "When they finish, $host waits for you before going on.",
                    style = MaterialTheme.typography.labelSmall, color = Muted,
                    modifier = Modifier.padding(horizontal = 4.dp),
                )
                Spacer(Modifier.height(14.dp))
                OutlinedTextField(
                    value = focus, onValueChange = { focus = it },
                    label = { Text("What should they look at?") },
                    placeholder = { Text("Optional", color = Muted) },
                    minLines = 2, maxLines = 5,
                    shape = RoundedCornerShape(12.dp),
                    modifier = Modifier.fillMaxWidth(),
                )
                Spacer(Modifier.height(12.dp))
            }
            Button(
                onClick = { vm.bringIn(s, agentId, model, focus.trim(), rounds, auto); onDismiss() },
                modifier = Modifier.fillMaxWidth().padding(horizontal = 20.dp).padding(bottom = 12.dp),
                shape = RoundedCornerShape(14.dp),
            ) { Text("Bring them in", modifier = Modifier.padding(vertical = 4.dp)) }
        }
    }
    if (modelPicker && sel != null) {
        ModelPickerSheet(
            models = sel.models,
            fallbackBrand = agentId.removePrefix("api:"),
            allowDefault = !agentId.startsWith("api:"),
            onPick = { model = it; modelPicker = false },
            onDismiss = { modelPicker = false },
        )
    }
}

/** Every model the source offers — searchable, each wearing its vendor's
 *  mark. The desktop's dropdown, given room to breathe. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ModelPickerSheet(
    models: List<com.fivelime.aiterm.ModelOption>,
    fallbackBrand: String,
    allowDefault: Boolean,
    onPick: (String?) -> Unit,
    onDismiss: () -> Unit,
) {
    var q by remember { mutableStateOf("") }
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = Surface1,
    ) {
        Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp).padding(bottom = 16.dp)) {
            OutlinedTextField(
                value = q, onValueChange = { q = it }, singleLine = true,
                placeholder = { Text("Search models…", color = Muted) },
                shape = RoundedCornerShape(12.dp),
                modifier = Modifier.fillMaxWidth(),
            )
            Spacer(Modifier.height(8.dp))
            val hits = models.filter { q.isBlank() || it.id.contains(q, true) || it.display_name.contains(q, true) }
            LazyColumn(Modifier.heightIn(max = 520.dp)) {
                if (allowDefault && q.isBlank()) item(key = "default") {
                    Row(
                        Modifier.fillMaxWidth().clickable { onPick(null) }.padding(horizontal = 8.dp, vertical = 12.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        AgentIcon(fallbackBrand, 20.dp)
                        Spacer(Modifier.width(12.dp))
                        Text("Default model")
                    }
                }
                items(hits, key = { it.id }) { m ->
                    Row(
                        Modifier.fillMaxWidth().clickable { onPick(m.id) }.padding(horizontal = 8.dp, vertical = 10.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        AgentIcon(modelBrand(m.id, fallbackBrand), 20.dp)
                        Spacer(Modifier.width(12.dp))
                        Column {
                            Text(m.display_name, maxLines = 1, overflow = TextOverflow.Ellipsis)
                            if (m.id != m.display_name) {
                                Text(m.id, style = MaterialTheme.typography.labelSmall, color = Muted, maxLines = 1, overflow = TextOverflow.Ellipsis)
                            }
                        }
                    }
                }
            }
        }
    }
}

/** Where the second-agent exchange stands — visible from the couch. */
@Composable
private fun RelayBanner(vm: AppViewModel, s: Session, r: com.fivelime.aiterm.RelayInfo) {
    val finished = r.phase == "done" || r.phase == "stopped" || r.phase == "error"
    val text = when (r.phase) {
        "opening" -> "Bringing in ${r.bName}…"
        "waitB" -> "${r.bName} is reading and writing… (round ${r.round}/${r.rounds})"
        "waitA" -> "${s.agent.replaceFirstChar { it.uppercase() }} is replying to ${r.bName}… (round ${r.round}/${r.rounds})"
        "done" -> "Done — ${r.note.ifBlank { "both views are in" }}"
        "error" -> "Bring-in failed: ${r.note}"
        "stopped" -> "Stopped"
        else -> r.phase
    }
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 4.dp)
            .clip(RoundedCornerShape(12.dp))
            .background(if (r.phase == "error") Red.copy(alpha = 0.15f) else Surface1)
            .padding(horizontal = 12.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (!finished) {
            CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 2.dp, color = Accent)
        } else {
            Icon(Icons.Filled.Build, null, tint = if (r.phase == "error") Red else Green, modifier = Modifier.size(14.dp))
        }
        Spacer(Modifier.width(8.dp))
        Text(text, style = MaterialTheme.typography.labelMedium, modifier = Modifier.weight(1f), maxLines = 2)
        r.bSessionId?.let { bid ->
            TextButton(onClick = { vm.sessions.firstOrNull { it.id == bid }?.let { vm.select(it) } }) {
                Text("Their side", style = MaterialTheme.typography.labelMedium)
            }
        }
        if (finished) {
            IconButton(onClick = { vm.dismissRelay(s.id) }, modifier = Modifier.size(28.dp)) {
                Icon(Icons.Filled.Close, "Dismiss", tint = Muted, modifier = Modifier.size(14.dp))
            }
        }
    }
}


/** What a held message can do. Copy takes the words (marks off), Copy as
 *  markdown takes it as written, Select text opens it for a partial
 *  selection, Share hands it to another app. The person's own message can
 *  be edited — it lands in the composer to change and send again — or sent
 *  again as it was; an answer can be asked for again, which sends the
 *  prompt above it once more. None of these rewrite history: every harness
 *  here is a CLI whose transcript only grows, so "edit" is a new turn. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun MessageSheet(
    item: Item, promptAbove: String?, onDismiss: () -> Unit,
    onEdit: (String) -> Unit, onSend: (String) -> Unit,
) {
    val clipboard = LocalClipboard.current
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var selecting by remember { mutableStateOf(false) }
    val raw = when (item) {
        is Item.User -> personSaid(item.text)
        is Item.AgentText -> item.text
        is Item.Thought -> item.text
        is Item.Tool -> listOfNotNull(item.title.ifBlank { item.tool }, item.input.takeIf { it.isNotBlank() }, item.output?.takeIf { it.isNotBlank() }).joinToString("\n\n")
        is Item.TurnEnd -> ""
    }
    val plain = if (item is Item.Tool) raw else markdownPlain(raw)
    val who = when (item) {
        is Item.User -> "You"
        is Item.AgentText -> "Answer"
        is Item.Thought -> "Reasoning"
        is Item.Tool -> item.title.ifBlank { item.tool }
        is Item.TurnEnd -> ""
    }
    fun copy(text: String, what: String) {
        scope.launch {
            clipboard.setClipEntry(ClipEntry(android.content.ClipData.newPlainText("aiterm", text)))
            android.widget.Toast.makeText(context, "Copied $what", android.widget.Toast.LENGTH_SHORT).show()
            onDismiss()
        }
    }
    fun share() {
        val i = android.content.Intent(android.content.Intent.ACTION_SEND).apply {
            type = "text/plain"; putExtra(android.content.Intent.EXTRA_TEXT, raw)
        }
        context.startActivity(android.content.Intent.createChooser(i, null))
        onDismiss()
    }
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = Surface1,
    ) {
        Column(Modifier.fillMaxWidth().navigationBarsPadding()) {
            if (selecting) {
                // The whole message, rendered, every word selectable: the
                // system's own handles and Copy do the rest.
                Row(Modifier.padding(horizontal = 20.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text("Select text", style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
                    IconButton(onClick = { selecting = false }) { Icon(Icons.Filled.Close, "Back") }
                }
                androidx.compose.foundation.text.selection.SelectionContainer(
                    Modifier.weight(1f, fill = false).verticalScroll(rememberScrollState()).padding(horizontal = 20.dp, vertical = 8.dp),
                ) {
                    if (item is Item.Tool) Text(raw, fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodySmall)
                    else MarkdownText(raw)
                }
                Spacer(Modifier.height(12.dp))
                return@Column
            }
            Column(Modifier.padding(horizontal = 20.dp)) {
                Text(who, style = MaterialTheme.typography.labelMedium, color = Muted)
                Text(
                    plain.replace('\n', ' '), style = MaterialTheme.typography.bodySmall, color = Muted,
                    maxLines = 2, overflow = TextOverflow.Ellipsis, modifier = Modifier.padding(top = 2.dp),
                )
            }
            Spacer(Modifier.height(8.dp))
            val rowColors = ListItemDefaults.colors(containerColor = Color.Transparent)
            @Composable fun action(label: String, icon: ImageVector, detail: String? = null, onClick: () -> Unit) = ListItem(
                headlineContent = { Text(label) },
                supportingContent = detail?.let { { Text(it, color = Muted, style = MaterialTheme.typography.bodySmall) } },
                leadingContent = { Icon(icon, null, tint = Accent) },
                colors = rowColors,
                modifier = Modifier.clickable(onClick = onClick),
            )
            if (item is Item.User) {
                action("Edit and send again", Icons.Filled.Edit, "Opens in the composer — the original stays as it was") { onEdit(raw); onDismiss() }
                action("Send again", Icons.Filled.Replay) { onSend(raw); onDismiss() }
            }
            if (item is Item.AgentText && promptAbove != null) {
                action("Ask again", Icons.Filled.Replay, "Sends the prompt above this answer once more") { onSend(promptAbove); onDismiss() }
            }
            action(if (item is Item.Tool) "Copy" else "Copy text", Icons.Filled.ContentCopy) { copy(plain, "text") }
            if (item !is Item.Tool) action("Copy as markdown", Icons.Filled.Code, "As the agent wrote it, marks and all") { copy(raw, "markdown") }
            action("Select text", Icons.Filled.SelectAll) { selecting = true }
            action("Share", Icons.Filled.Share) { share() }
            Spacer(Modifier.height(12.dp))
        }
    }
}


/** The session's workspace as a row of tabs: the conversation the crew
 *  gathers around, then every agent brought into it — one tap moves between
 *  them, the way the desktop nests them under one tab. A dot says who is
 *  working (accent) or waiting on you (amber). Only drawn when there is a
 *  crew; a lone session keeps its full height. */
@Composable
private fun CrewStrip(vm: AppViewModel, s: Session) {
    val masterId = vm.broughtIn[s.id] ?: s.id
    val ids = listOf(masterId) + vm.broughtIn.filterValues { it == masterId }.keys.sorted()
    val rows = ids.mapNotNull { id -> vm.sessions.firstOrNull { it.id == id } }
    if (rows.size < 2) return
    Row(
        Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 10.dp, vertical = 6.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        rows.forEach { r ->
            val on = r.id == s.id
            val act = vm.activity[r.id]
            Row(
                Modifier.clip(RoundedCornerShape(16.dp))
                    .background(if (on) Accent.copy(alpha = 0.18f) else Surface1)
                    .clickable(enabled = !on) { vm.select(r) }
                    .padding(horizontal = 10.dp, vertical = 6.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                AgentIcon(r.agent, 14.dp)
                Spacer(Modifier.width(6.dp))
                Text(
                    r.title.ifBlank { if (r.id == masterId) "Session" else r.agent },
                    style = MaterialTheme.typography.labelMedium,
                    color = if (on) Accent else MaterialTheme.colorScheme.onSurface,
                    maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.widthIn(max = 170.dp),
                )
                if (act == "attention" || act == "working") {
                    Spacer(Modifier.width(6.dp))
                    Box(Modifier.size(7.dp).background(if (act == "attention") Amber else Accent, RoundedCornerShape(50)))
                }
            }
        }
    }
}
