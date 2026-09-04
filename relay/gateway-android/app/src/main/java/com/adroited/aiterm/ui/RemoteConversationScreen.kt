package com.adroited.aiterm.ui

import android.graphics.BitmapFactory
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.imeNestedScroll
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.union
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DrawerValue
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalDrawerSheet
import androidx.compose.material3.ModalNavigationDrawer
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.material3.rememberDrawerState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Language
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.adroited.aiterm.remote.ConnectionState
import com.adroited.aiterm.pairing.PairedDesktop
import com.adroited.aiterm.remote.RemoteAgentChoice
import com.adroited.aiterm.remote.RemoteClientState
import com.adroited.aiterm.remote.RemotePreviewMessage
import com.adroited.aiterm.remote.RemoteSession
import com.adroited.aiterm.remote.RemoteSessionChange
import com.adroited.aiterm.remote.RemoteTab
import com.adroited.aiterm.remote.RemoteUploadProgress
import com.adroited.aiterm.remote.RemoteUsageSource
import java.io.File
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

private const val PAGE_SESSIONS = "sessions"
private const val PAGE_CONVERSATION = "conversation"
private const val PAGE_TERMINAL = "terminal"
private const val PAGE_WEB_PREVIEW = "web_preview"

/** Conversation-first shell inspired by the 5lime client, backed only by our remote protocol. */
@Composable
fun RemoteDesktopScreen(
    viewModel: RemoteTerminalViewModel,
    desktop: PairedDesktop,
    pairedDesktops: List<PairedDesktop>,
    onBack: () -> Unit,
    onOpenDesktop: (PairedDesktop) -> Unit,
    onPairDesktop: () -> Unit,
    onForgetDesktop: () -> Boolean,
    keyBarPreference: TerminalKeyBarPreference,
) {
    val state by viewModel.client.state.collectAsStateWithLifecycle()
    var page by rememberSaveable { mutableStateOf(PAGE_SESSIONS) }
    var selectedSessionId by rememberSaveable { mutableStateOf<String?>(null) }
    var webPreviewUrl by rememberSaveable { mutableStateOf<String?>(null) }
    val selected = selectedSessionId?.let { id -> state.sessions.firstOrNull { it.id == id } }

    when (page) {
        PAGE_WEB_PREVIEW -> {
            val url = webPreviewUrl
            if (url == null) {
                LaunchedEffect(Unit) { page = PAGE_CONVERSATION }
            } else {
                RemoteWebPreviewScreen(
                    url = url,
                    serverSpkiFingerprint = viewModel.desktopSpkiFingerprint(),
                    onClose = {
                        webPreviewUrl = null
                        page = PAGE_CONVERSATION
                    },
                )
            }
        }

        PAGE_TERMINAL -> RemoteTerminalScreen(
            viewModel = viewModel,
            onBack = {
                selectedSessionId?.let(viewModel::previewSession)
                page = if (selectedSessionId == null) PAGE_SESSIONS else PAGE_CONVERSATION
            },
            keyBarPreference = keyBarPreference,
        )

        PAGE_CONVERSATION -> if (selected == null) {
            LaunchedEffect(selectedSessionId) {
                selectedSessionId = null
                page = PAGE_SESSIONS
            }
        } else {
            RemoteConversationContent(
                state = state,
                session = selected,
                onBack = { page = PAGE_SESSIONS },
                onRefresh = { viewModel.previewSession(selected.id) },
                onSend = viewModel::sendConversationPrompt,
                onBringIn = viewModel.client::bringInSession,
                onLoadFiles = viewModel::sessionChanges,
                onLoadFile = viewModel::sessionFilePreview,
                onProbeWebPreview = viewModel::hasWebPreview,
                onOpenWebPreview = viewModel::openWebPreview,
                onShowWebPreview = { url ->
                    webPreviewUrl = url
                    page = PAGE_WEB_PREVIEW
                },
            )
        }

        else -> RemoteSessionDashboard(
            state = state,
            desktop = desktop,
            pairedDesktops = pairedDesktops,
            onOpenDesktop = onOpenDesktop,
            onManageDesktops = onBack,
            onPairDesktop = onPairDesktop,
            onForgetDesktop = onForgetDesktop,
            onRefresh = { viewModel.client.refreshSessions() },
            onLoadUsage = viewModel.client::refreshUsage,
            onStarSession = viewModel.client::starSession,
            onRenameSession = viewModel.client::renameSession,
            onOpenSession = { session ->
                selectedSessionId = session.id
                viewModel.previewSession(session.id)
                page = PAGE_CONVERSATION
            },
            onOpenTerminal = {
                selectedSessionId = null
                page = PAGE_TERMINAL
            },
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun RemoteSessionDashboard(
    state: RemoteClientState,
    desktop: PairedDesktop,
    pairedDesktops: List<PairedDesktop>,
    onOpenDesktop: (PairedDesktop) -> Unit,
    onManageDesktops: () -> Unit,
    onPairDesktop: () -> Unit,
    onForgetDesktop: () -> Boolean,
    onRefresh: () -> Unit,
    onLoadUsage: () -> Unit,
    onStarSession: (String, Boolean) -> Unit,
    onRenameSession: (String, String) -> Unit,
    onOpenSession: (RemoteSession) -> Unit,
    onOpenTerminal: () -> Unit,
) {
    var query by rememberSaveable { mutableStateOf("") }
    var agentFilter by rememberSaveable { mutableStateOf<String?>(null) }
    var filesOnly by rememberSaveable { mutableStateOf(false) }
    var activeOnly by rememberSaveable { mutableStateOf(false) }
    var foldedCrews by remember { mutableStateOf(emptySet<String>()) }
    var renameTarget by remember { mutableStateOf<RemoteSession?>(null) }
    var showUsage by remember { mutableStateOf(false) }
    var forgetDesktop by remember { mutableStateOf(false) }
    var forgetDesktopFailed by remember { mutableStateOf(false) }
    val drawerState = rememberDrawerState(DrawerValue.Closed)
    val drawerScope = rememberCoroutineScope()
    val agents = remember(state.sessions) { state.sessions.map { it.agent }.distinct().sorted() }
    val sessions = remember(
        state.sessions,
        state.tabs,
        state.sessionsWithFiles,
        state.starredSessions,
        state.broughtInSessions,
        query,
        agentFilter,
        filesOnly,
        activeOnly,
        foldedCrews,
    ) {
        conversationSessions(
            sessions = state.sessions,
            tabs = state.tabs,
            query = query,
            starred = state.starredSessions,
            withFiles = state.sessionsWithFiles,
            broughtIn = state.broughtInSessions,
            agentFilter = agentFilter,
            filesOnly = filesOnly,
            activeOnly = activeOnly,
            foldedCrews = foldedCrews,
        )
    }
    renameTarget?.let { session ->
        SessionRenameDialog(
            session = session,
            onRename = { title ->
                onRenameSession(session.id, title)
                renameTarget = null
            },
            onDismiss = { renameTarget = null },
        )
    }
    if (showUsage) {
        UsageDialog(sources = state.usage, onDismiss = { showUsage = false })
    }
    if (forgetDesktop) {
        AlertDialog(
            onDismissRequest = {
                forgetDesktop = false
                forgetDesktopFailed = false
            },
            title = { Text("Forget ${desktop.displayName}?") },
            text = {
                Text(
                    if (forgetDesktopFailed) {
                        "The saved desktop could not be removed. Nothing was changed."
                    } else {
                        "This removes the desktop key from this phone. You can pair it again later."
                    },
                )
            },
            confirmButton = {
                TextButton(onClick = { forgetDesktopFailed = !onForgetDesktop() }) {
                    Text("Forget desktop")
                }
            },
            dismissButton = {
                TextButton(onClick = {
                    forgetDesktop = false
                    forgetDesktopFailed = false
                }) { Text("Keep desktop") }
            },
        )
    }
    LaunchedEffect(state.connection) {
        while (state.connection == ConnectionState.Connected) {
            delay(3_000)
            onRefresh()
        }
    }
    ModalNavigationDrawer(
        drawerState = drawerState,
        drawerContent = {
            RemoteAppDrawer(
                state = state,
                desktop = desktop,
                pairedDesktops = pairedDesktops,
                onClose = { drawerScope.launch { drawerState.close() } },
                onOpenDesktop = { target ->
                    drawerScope.launch { drawerState.close() }
                    onOpenDesktop(target)
                },
                onShowUsage = {
                    onLoadUsage()
                    showUsage = true
                    drawerScope.launch { drawerState.close() }
                },
                onRefresh = {
                    onRefresh()
                    onLoadUsage()
                    drawerScope.launch { drawerState.close() }
                },
                onOpenTerminal = {
                    drawerScope.launch { drawerState.close() }
                    onOpenTerminal()
                },
                onManageDesktops = {
                    drawerScope.launch { drawerState.close() }
                    onManageDesktops()
                },
                onPairDesktop = {
                    drawerScope.launch { drawerState.close() }
                    onPairDesktop()
                },
                onForgetDesktop = {
                    forgetDesktop = true
                    drawerScope.launch { drawerState.close() }
                },
            )
        },
    ) {
        Scaffold(
            topBar = {
                TopAppBar(
                    navigationIcon = {
                        IconButton(
                            onClick = { drawerScope.launch { drawerState.open() } },
                            modifier = Modifier.semantics { contentDescription = "Open menu" },
                        ) {
                            Text("☰", style = MaterialTheme.typography.titleLarge)
                        }
                    },
                    title = {
                        Column {
                            Text(desktop.displayName, maxLines = 1, overflow = TextOverflow.Ellipsis)
                            ConnectionLabel(state.connection, state.connectedEndpoint?.path)
                        }
                    },
                    colors = TopAppBarDefaults.topAppBarColors(containerColor = MaterialTheme.colorScheme.background),
                )
            },
            containerColor = MaterialTheme.colorScheme.background,
        ) { padding ->
            Column(Modifier.fillMaxSize().padding(padding)) {
            OutlinedTextField(
                value = query,
                onValueChange = { query = it },
                modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 8.dp),
                placeholder = { Text("Search sessions") },
                singleLine = true,
                shape = RoundedCornerShape(14.dp),
            )
            LazyRow(
                modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp),
                horizontalArrangement = Arrangement.spacedBy(7.dp),
                contentPadding = PaddingValues(bottom = 7.dp),
            ) {
                items(agents, key = { "agent:$it" }) { agent ->
                    FilterChip(
                        selected = agentFilter == agent,
                        onClick = { agentFilter = agent.takeUnless { it == agentFilter } },
                        label = { Text(agent.replaceFirstChar(Char::uppercase), maxLines = 1) },
                    )
                }
                item(key = "files") {
                    FilterChip(
                        selected = filesOnly,
                        onClick = { filesOnly = !filesOnly },
                        label = { Text("Has files") },
                    )
                }
                item(key = "active") {
                    FilterChip(
                        selected = activeOnly,
                        onClick = { activeOnly = !activeOnly },
                        label = { Text("Active") },
                    )
                }
            }
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text("SESSIONS", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.primary)
                Spacer(Modifier.weight(1f))
                Text(
                    "${state.tabs.count { it.sessionId != null }} live · ${state.sessions.size} total",
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            when {
                state.sessions.isEmpty() -> DashboardEmptyState(state.connection)
                sessions.isEmpty() -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    Text("No sessions match that search.", color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
                else -> LazyColumn(
                    modifier = Modifier.fillMaxSize(),
                    contentPadding = PaddingValues(bottom = 20.dp),
                ) {
                    items(sessions, key = RemoteSession::id) { session ->
                        SessionDashboardRow(
                            session = session,
                            live = isConversationSessionLive(session, state.tabs),
                            activity = state.sessionActivity[session.id],
                            starred = session.id in state.starredSessions,
                            hasFiles = session.id in state.sessionsWithFiles,
                            satellite = state.broughtInSessions[session.id]?.let { parent ->
                                sessions.any { it.id == parent }
                            } == true,
                            crewAgents = state.broughtInSessions
                                .filterValues { it == session.id }
                                .keys
                                .mapNotNull { child -> state.sessions.firstOrNull { it.id == child }?.agent },
                            crewFolded = session.id in foldedCrews,
                            onToggleCrew = {
                                foldedCrews = if (session.id in foldedCrews) {
                                    foldedCrews - session.id
                                } else {
                                    foldedCrews + session.id
                                }
                            },
                            onToggleStar = { onStarSession(session.id, session.id !in state.starredSessions) },
                            onRename = { renameTarget = session },
                            onClick = { onOpenSession(session) },
                        )
                    }
                }
            }
            }
        }
    }
}

@Composable
private fun RemoteAppDrawer(
    state: RemoteClientState,
    desktop: PairedDesktop,
    pairedDesktops: List<PairedDesktop>,
    onClose: () -> Unit,
    onOpenDesktop: (PairedDesktop) -> Unit,
    onShowUsage: () -> Unit,
    onRefresh: () -> Unit,
    onOpenTerminal: () -> Unit,
    onManageDesktops: () -> Unit,
    onPairDesktop: () -> Unit,
    onForgetDesktop: () -> Unit,
) {
    ModalDrawerSheet(
        modifier = Modifier.fillMaxHeight().widthIn(max = 340.dp),
        drawerContainerColor = MaterialTheme.colorScheme.surface,
    ) {
        Column(
            Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(vertical = 14.dp),
        ) {
            Row(
                modifier = Modifier.fillMaxWidth().padding(start = 20.dp, end = 8.dp, bottom = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                ConnectionDot(state.connection)
                Spacer(Modifier.width(12.dp))
                Column(Modifier.weight(1f)) {
                    Text(
                        desktop.displayName,
                        style = MaterialTheme.typography.titleLarge,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    ConnectionLabel(state.connection, state.connectedEndpoint?.path)
                }
                TextButton(onClick = onClose) { Text("Close") }
            }

            if (pairedDesktops.size > 1) {
                DrawerSectionLabel("Desktops")
                pairedDesktops.forEach { candidate ->
                    DrawerRow(
                        title = candidate.displayName,
                        detail = if (candidate.deviceId == desktop.deviceId) "Current desktop" else "Paired desktop",
                        selected = candidate.deviceId == desktop.deviceId,
                        onClick = { if (candidate.deviceId != desktop.deviceId) onOpenDesktop(candidate) },
                    )
                }
            }

            HorizontalDivider(Modifier.padding(vertical = 10.dp), color = MaterialTheme.colorScheme.surfaceVariant)
            DrawerRow("Usage", "Limits, balances, and account status", onClick = onShowUsage)
            DrawerRow("Refresh", "Update sessions and usage", onClick = onRefresh)
            DrawerRow("Open terminal", "View the raw desktop session", onClick = onOpenTerminal)

            HorizontalDivider(Modifier.padding(vertical = 10.dp), color = MaterialTheme.colorScheme.surfaceVariant)
            DrawerRow("Manage desktops", "View and remove trusted computers", onClick = onManageDesktops)
            DrawerRow("Add a desktop", "Scan another pairing code", onClick = onPairDesktop)
            DrawerRow(
                title = "Forget this desktop",
                detail = "Remove its key from this phone",
                titleColor = MaterialTheme.colorScheme.error,
                onClick = onForgetDesktop,
            )
        }
    }
}

@Composable
private fun ConnectionDot(connection: ConnectionState) {
    val color = when (connection) {
        ConnectionState.Connected -> MaterialTheme.colorScheme.tertiary
        ConnectionState.Connecting, ConnectionState.Reconnecting -> MaterialTheme.colorScheme.primary
        ConnectionState.Locked, ConnectionState.Revoked -> MaterialTheme.colorScheme.error
        ConnectionState.Disconnected -> MaterialTheme.colorScheme.onSurfaceVariant
    }
    Box(Modifier.size(10.dp).background(color, CircleShape))
}

@Composable
private fun DrawerSectionLabel(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.labelLarge,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(horizontal = 20.dp, vertical = 8.dp),
    )
}

@Composable
private fun DrawerRow(
    title: String,
    detail: String,
    selected: Boolean = false,
    titleColor: Color = MaterialTheme.colorScheme.onSurface,
    onClick: () -> Unit,
) {
    val background = if (selected) MaterialTheme.colorScheme.primary.copy(alpha = 0.12f) else Color.Transparent
    Column(
        Modifier.fillMaxWidth()
            .padding(horizontal = 10.dp, vertical = 2.dp)
            .background(background, RoundedCornerShape(12.dp))
            .clickable(onClick = onClick)
            .padding(horizontal = 14.dp, vertical = 11.dp),
    ) {
        Text(title, style = MaterialTheme.typography.bodyLarge, color = titleColor)
        Text(
            detail,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun UsageDialog(sources: List<RemoteUsageSource>, onDismiss: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Usage") },
        text = {
            if (sources.isEmpty()) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                    Spacer(Modifier.width(10.dp))
                    Text("Reading usage from the desktop…")
                }
            } else {
                LazyColumn(Modifier.fillMaxWidth().heightIn(max = 520.dp)) {
                    items(sources, key = { it.id }) { source ->
                        Column(Modifier.fillMaxWidth().padding(vertical = 9.dp)) {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text(
                                    source.name,
                                    style = MaterialTheme.typography.titleMedium,
                                    modifier = Modifier.weight(1f),
                                )
                                source.plan.takeIf(String::isNotBlank)?.let {
                                    Text(it, style = MaterialTheme.typography.labelMedium)
                                }
                            }
                            source.account.takeIf(String::isNotBlank)?.let {
                                Text(
                                    it,
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                            if (source.state != "ok" && source.state != "no_balance") {
                                Text(
                                    source.detail.ifBlank { source.state.replace('_', ' ') },
                                    color = MaterialTheme.colorScheme.error,
                                    style = MaterialTheme.typography.bodySmall,
                                )
                            }
                            source.bars.forEach { bar ->
                                Spacer(Modifier.height(7.dp))
                                Row {
                                    Text(bar.label, style = MaterialTheme.typography.labelMedium, modifier = Modifier.weight(1f))
                                    Text("${bar.percent.toInt()}%", style = MaterialTheme.typography.labelMedium)
                                }
                                LinearProgressIndicator(
                                    progress = { (bar.percent / 100.0).toFloat() },
                                    modifier = Modifier.fillMaxWidth(),
                                    color = when (bar.severity) {
                                        "critical" -> MaterialTheme.colorScheme.error
                                        "warning" -> Color(0xFFFFC857)
                                        else -> MaterialTheme.colorScheme.primary
                                    },
                                )
                            }
                            source.amounts.forEach { amount ->
                                Text(
                                    buildString {
                                        append(amount.label)
                                        append(": ")
                                        if (amount.currency == "USD") append('$')
                                        append("%.2f".format(amount.amount))
                                        amount.of?.let { append(" of %.2f".format(it)) }
                                    },
                                    style = MaterialTheme.typography.bodySmall,
                                )
                            }
                            source.notes.forEach {
                                Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                            }
                        }
                        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.45f))
                    }
                }
            }
        },
        confirmButton = { TextButton(onClick = onDismiss) { Text("Done") } },
    )
}

@Composable
private fun DashboardEmptyState(connection: ConnectionState) {
    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            if (connection == ConnectionState.Connecting || connection == ConnectionState.Reconnecting) {
                CircularProgressIndicator(Modifier.size(28.dp), strokeWidth = 2.dp)
                Spacer(Modifier.height(12.dp))
                Text("Reading sessions from the desktop…", color = MaterialTheme.colorScheme.onSurfaceVariant)
            } else {
                Text("No sessions on this desktop yet.", color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun SessionDashboardRow(
    session: RemoteSession,
    live: Boolean,
    activity: String?,
    starred: Boolean,
    hasFiles: Boolean,
    satellite: Boolean,
    crewAgents: List<String>,
    crewFolded: Boolean,
    onToggleCrew: () -> Unit,
    onToggleStar: () -> Unit,
    onRename: () -> Unit,
    onClick: () -> Unit,
) {
    Row(
        Modifier.fillMaxWidth()
            .combinedClickable(onClick = onClick, onLongClick = onRename)
            .padding(start = if (satellite) 30.dp else 14.dp, end = 14.dp, top = 11.dp, bottom = 11.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (satellite) {
            Text("↳", color = MaterialTheme.colorScheme.onSurfaceVariant)
            Spacer(Modifier.width(6.dp))
        }
        Box(
            Modifier.size(if (satellite) 30.dp else 38.dp)
                .background(agentColor(session.agent).copy(alpha = 0.16f), CircleShape),
            contentAlignment = Alignment.Center,
        ) {
            Text(
                session.agent.take(1).uppercase(),
                color = agentColor(session.agent),
                fontWeight = FontWeight.Bold,
            )
        }
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (starred) {
                    Text(
                        "★",
                        color = Color(0xFFFFC857),
                        modifier = Modifier.clickable(onClick = onToggleStar).padding(end = 5.dp),
                    )
                }
                Text(
                    session.title.ifBlank { "Untitled session" },
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    style = MaterialTheme.typography.titleMedium,
                    modifier = Modifier.weight(1f),
                )
            }
            Text(
                buildString {
                    append(relativeSessionTime(session.lastActive).lowercase())
                    append(" · ")
                    append(session.projectPath.trimEnd('/').substringAfterLast('/').ifBlank { session.projectPath })
                    session.branch?.takeIf(String::isNotBlank)?.let { append(" · "); append(it) }
                    if (session.forked) append(" · fork")
                    if (hasFiles) append(" · files")
                },
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            if (!starred) {
                Text(
                    "☆",
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.clickable(onClick = onToggleStar).padding(4.dp),
                )
            }
            if (crewAgents.isNotEmpty()) {
                Text(
                    crewAgents.take(3).joinToString("") { it.take(1).uppercase() } +
                        (if (crewAgents.size > 3) "+${crewAgents.size - 3}" else "") +
                        (if (crewFolded) " ›" else " ⌄"),
                    color = MaterialTheme.colorScheme.primary,
                    style = MaterialTheme.typography.labelMedium,
                    modifier = Modifier.background(
                        MaterialTheme.colorScheme.primary.copy(alpha = 0.12f),
                        RoundedCornerShape(9.dp),
                    ).clickable(onClick = onToggleCrew).padding(horizontal = 7.dp, vertical = 6.dp),
                )
            }
            Column(horizontalAlignment = Alignment.End) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Box(
                        Modifier.size(7.dp).background(
                            when {
                                activity == "attention" -> MaterialTheme.colorScheme.error
                                activity == "output" -> MaterialTheme.colorScheme.primary
                                live -> MaterialTheme.colorScheme.tertiary
                                else -> MaterialTheme.colorScheme.outline
                            },
                            CircleShape,
                        ),
                    )
                    Spacer(Modifier.width(6.dp))
                    Text(
                        when {
                            activity == "attention" -> "NEEDS YOU"
                            activity == "output" -> "WORKING"
                            live -> "OPEN"
                            else -> relativeSessionTime(session.lastActive)
                        },
                        style = MaterialTheme.typography.labelMedium,
                        color = when {
                            activity == "attention" -> MaterialTheme.colorScheme.error
                            activity == "output" -> MaterialTheme.colorScheme.primary
                            live -> MaterialTheme.colorScheme.tertiary
                            else -> MaterialTheme.colorScheme.onSurfaceVariant
                        },
                    )
                }
            }
        }
    }
    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.45f))
}

@Composable
private fun SessionRenameDialog(
    session: RemoteSession,
    onRename: (String) -> Unit,
    onDismiss: () -> Unit,
) {
    var draft by remember(session.id) { mutableStateOf(session.title) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Rename session") },
        text = {
            OutlinedTextField(
                value = draft,
                onValueChange = { draft = it },
                singleLine = true,
                supportingText = { Text("Leave empty to restore the generated name.") },
            )
        },
        confirmButton = { TextButton(onClick = { onRename(draft) }) { Text("Rename") } },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
private fun RemoteConversationContent(
    state: RemoteClientState,
    session: RemoteSession,
    onBack: () -> Unit,
    onRefresh: () -> Unit,
    onSend: suspend (
        String,
        String,
        List<TerminalAttachmentImage>,
        (RemoteUploadProgress) -> Unit,
    ) -> Result<Unit>,
    onBringIn: (String, String, String?, String?, String, Int, Boolean) -> Unit,
    onLoadFiles: suspend (String) -> Result<List<RemoteSessionChange>>,
    onLoadFile: suspend (String, String, Int) -> Result<RemoteSessionFilePreview>,
    onProbeWebPreview: suspend (String) -> Result<Boolean>,
    onOpenWebPreview: suspend (String) -> Result<String>,
    onShowWebPreview: (String) -> Unit,
) {
    var draft by rememberSaveable(session.id) { mutableStateOf("") }
    var sending by remember(session.id) { mutableStateOf(false) }
    var sendError by remember(session.id) { mutableStateOf<String?>(null) }
    var attachments by remember(session.id) { mutableStateOf(TerminalAttachmentDraft()) }
    var showImageSources by remember(session.id) { mutableStateOf(false) }
    var showFiles by remember(session.id) { mutableStateOf(false) }
    var showBringIn by remember(session.id) { mutableStateOf(false) }
    var filesLoading by remember(session.id) { mutableStateOf(false) }
    var files by remember(session.id) { mutableStateOf<List<RemoteSessionChange>>(emptyList()) }
    var filesError by remember(session.id) { mutableStateOf<String?>(null) }
    var filePreviewTarget by remember(session.id) { mutableStateOf<RemoteSessionChange?>(null) }
    var filePreviewLoading by remember(session.id) { mutableStateOf(false) }
    var filePreview by remember(session.id) { mutableStateOf<RemoteSessionFilePreview?>(null) }
    var filePreviewError by remember(session.id) { mutableStateOf<String?>(null) }
    var webPreviewAvailable by remember(session.id) { mutableStateOf(false) }
    var webPreviewOpening by remember(session.id) { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    val normalizer = remember(context) { TerminalImageNormalizer(context) }
    val messages = if (state.previewSessionId == session.id) state.previewMessages else emptyList()
    val timeline = remember(messages) { conversationTimeline(messages) }
    val listState = rememberLazyListState()
    val working = state.sessionActivity[session.id] == "output"
    val imeBottom = WindowInsets.ime.getBottom(LocalDensity.current)
    var positionedAtNewest by remember(session.id) { mutableStateOf(false) }
    val awayFromNewest by remember(session.id) {
        derivedStateOf {
            val layout = listState.layoutInfo
            val last = layout.visibleItemsInfo.lastOrNull()?.index ?: 0
            layout.totalItemsCount > 0 && last < layout.totalItemsCount - 1
        }
    }
    val latestAttachments by rememberUpdatedState(attachments)

    BackHandler(enabled = !sending && !attachments.preparing, onBack = onBack)
    DisposableEffect(session.id) {
        onDispose { latestAttachments.items.forEach { it.image.file.delete() } }
    }

    LaunchedEffect(session.id) { onRefresh() }
    LaunchedEffect(session.id, isConversationSessionLive(session, state.tabs)) {
        if (isConversationSessionLive(session, state.tabs)) {
            while (true) {
                delay(1_500)
                onRefresh()
            }
        }
    }
    LaunchedEffect(session.id, state.connection) {
        if (state.connection != ConnectionState.Connected) {
            webPreviewAvailable = false
            return@LaunchedEffect
        }
        while (true) {
            onProbeWebPreview(session.id).onSuccess { webPreviewAvailable = it }
            delay(5_000)
        }
    }
    LaunchedEffect(messages.size, timeline.size, working) {
        val itemCount = timeline.size + if (working) 1 else 0
        if (itemCount == 0) return@LaunchedEffect
        if (!positionedAtNewest) {
            listState.scrollToItem(itemCount - 1)
            positionedAtNewest = true
        } else {
            val lastVisible = listState.layoutInfo.visibleItemsInfo.lastOrNull()?.index ?: 0
            if (lastVisible >= itemCount - 3) listState.animateScrollToItem(itemCount - 1)
        }
    }
    LaunchedEffect(imeBottom) {
        if (imeBottom > 0) {
            val newest = timeline.size + if (working) 1 else 0
            if (newest > 0) listState.scrollToItem(newest - 1)
        }
    }

    fun updateAttachments(
        transition: (TerminalAttachmentDraft) -> TerminalAttachmentDraftUpdate,
    ): TerminalAttachmentDraftUpdate = transition(attachments).also { attachments = it.draft }

    fun handlePickerResult(result: TerminalImagePickerResult) {
        when (result) {
            TerminalImagePickerResult.Cancelled -> Unit
            is TerminalImagePickerResult.Failed -> {
                attachments = attachments.copy(message = result.message)
            }
            is TerminalImagePickerResult.Selected -> scope.launch(start = CoroutineStart.UNDISPATCHED) {
                val preparation = updateAttachments { it.beginPreparation() }
                if (!preparation.accepted) {
                    result.ownedCaptureFiles.forEach(File::delete)
                    return@launch
                }
                try {
                    val distinct = result.uris.distinct()
                    val remaining = TerminalAttachmentDraft.MAX_IMAGES - attachments.items.size
                    var message: String? = null
                    for (uri in distinct.take(remaining.coerceAtLeast(0))) {
                        val normalized = normalizer.normalize(uri).getOrElse { error ->
                            message = terminalImageErrorMessage(error)
                            continue
                        }
                        val added = updateAttachments { it.add(normalized) }
                        if (!added.accepted) {
                            message = added.draft.message
                            normalized.file.delete()
                        }
                    }
                    attachments = attachments.copy(
                        message = when {
                            result.uris.size != distinct.size -> "This image is already attached."
                            distinct.size > remaining -> "You can attach up to 4 images."
                            else -> message
                        },
                    )
                } finally {
                    result.ownedCaptureFiles.forEach(File::delete)
                    updateAttachments { it.finishPreparation() }
                }
            }
        }
    }
    val picker = rememberTerminalImagePicker { _, result -> handlePickerResult(result) }

    fun submit() {
        val text = draft.trim()
        if ((text.isEmpty() && attachments.items.isEmpty()) || sending || attachments.preparing) return
        sending = true
        sendError = null
        if (attachments.items.isNotEmpty()) {
            val began = updateAttachments { it.beginSubmission() }
            if (!began.accepted) {
                sending = false
                return
            }
        }
        val submittedImages = attachments.items.map { it.image }
        scope.launch {
            onSend(session.id, text, submittedImages) { progress ->
                updateAttachments { it.recordProgress(progress.sourceId, progress.sent, progress.total) }
            }.fold(
                onSuccess = {
                    draft = ""
                    val removed = attachments.items
                    attachments = TerminalAttachmentDraft()
                    removed.forEach { it.image.file.delete() }
                },
                onFailure = {
                    sendError = it.message ?: "The desktop did not accept the message."
                    if (attachments.submitting) {
                        updateAttachments { draftState -> draftState.failSubmission(sendError!!) }
                    }
                },
            )
            sending = false
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                navigationIcon = {
                    TextButton(
                        onClick = onBack,
                        enabled = !sending && !attachments.preparing,
                    ) { Text("Sessions") }
                },
                title = {
                    Column {
                        Text(session.title.ifBlank { "Untitled session" }, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        Text(
                            "${session.agent} · ${if (isConversationSessionLive(session, state.tabs)) "live" else "history"}",
                            style = MaterialTheme.typography.labelMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                },
                actions = {
                    TextButton(onClick = {
                        showFiles = true
                        filesLoading = true
                        filesError = null
                        scope.launch {
                            onLoadFiles(session.id).fold(
                                onSuccess = { files = it },
                                onFailure = { filesError = it.message ?: "Could not load files." },
                            )
                            filesLoading = false
                        }
                    }) { Text("Files") }
                    if (webPreviewAvailable) {
                        IconButton(
                            onClick = {
                                if (webPreviewOpening) return@IconButton
                                webPreviewOpening = true
                                scope.launch {
                                    onOpenWebPreview(session.id).fold(
                                        onSuccess = onShowWebPreview,
                                        onFailure = {
                                            sendError = it.message ?: "Could not open the webpage preview."
                                        },
                                    )
                                    webPreviewOpening = false
                                }
                            },
                            enabled = !webPreviewOpening,
                        ) {
                            if (webPreviewOpening) {
                                CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                            } else {
                                Icon(Icons.Filled.Language, contentDescription = "Preview webpage")
                            }
                        }
                    }
                    TextButton(onClick = { showBringIn = true }) { Text("Bring in") }
                },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = MaterialTheme.colorScheme.background),
            )
        },
        bottomBar = {
            Column(
                Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.surface)
                    .windowInsetsPadding(WindowInsets.navigationBars.union(WindowInsets.ime))
                    .padding(horizontal = 10.dp, vertical = 8.dp),
            ) {
                sendError?.let {
                    Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
                    Spacer(Modifier.height(4.dp))
                }
                TerminalAttachmentStrip(
                    draft = attachments,
                    onRemove = { imageId ->
                        val removed = updateAttachments { it.remove(imageId) }
                        removed.removed.forEach { it.image.file.delete() }
                    },
                )
                Row(verticalAlignment = Alignment.Bottom) {
                    TextButton(
                        onClick = { showImageSources = true },
                        enabled = !sending && !attachments.preparing &&
                            attachments.items.size < TerminalAttachmentDraft.MAX_IMAGES,
                    ) { Text("＋") }
                    OutlinedTextField(
                        value = draft,
                        onValueChange = { draft = it },
                        modifier = Modifier.weight(1f),
                        placeholder = { Text("Message ${session.agent}") },
                        maxLines = 5,
                        shape = RoundedCornerShape(18.dp),
                        keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
                        keyboardActions = KeyboardActions(onSend = { submit() }),
                        enabled = !sending,
                        trailingIcon = {
                            if (sending) {
                                CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                            }
                        },
                    )
                }
            }
        },
        containerColor = MaterialTheme.colorScheme.background,
    ) { padding ->
        when {
            state.previewSessionId != session.id && state.previewLoadingSessionId == session.id -> Box(
                Modifier.fillMaxSize().padding(padding),
                contentAlignment = Alignment.Center,
            ) { CircularProgressIndicator() }
            state.previewSessionId != session.id -> Column(
                Modifier.fillMaxSize().padding(padding).padding(24.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.Center,
            ) {
                Text(
                    state.previewError ?: "The conversation could not be loaded.",
                    color = MaterialTheme.colorScheme.error,
                )
                Spacer(Modifier.height(8.dp))
                TextButton(onClick = onRefresh) { Text("Retry") }
            }
            messages.isEmpty() && !working -> Box(
                Modifier.fillMaxSize().padding(padding),
                contentAlignment = Alignment.Center,
            ) { Text("No conversation history yet.", color = MaterialTheme.colorScheme.onSurfaceVariant) }
            else -> Box(Modifier.fillMaxSize().padding(padding)) {
                LazyColumn(
                    state = listState,
                    modifier = Modifier.fillMaxSize().then(
                        if (imeBottom > 0) Modifier.imeNestedScroll() else Modifier,
                    ),
                    contentPadding = PaddingValues(horizontal = 12.dp, vertical = 10.dp),
                    verticalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                    itemsIndexed(timeline) { _, item ->
                        when (item) {
                            is ConversationTimelineItem.Turn -> ConversationTurn(item.message)
                            is ConversationTimelineItem.ActivityGroup -> ConversationActivityGroup(item.messages)
                        }
                    }
                    if (working) {
                        item(key = "working") { ConversationWorkingRow(session.agent) }
                    }
                }
                if (awayFromNewest) {
                    Text(
                        "Newest ↓",
                        style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.align(Alignment.BottomCenter)
                            .padding(bottom = 10.dp)
                            .background(MaterialTheme.colorScheme.surfaceVariant, RoundedCornerShape(50))
                            .clickable {
                                scope.launch {
                                    val end = listState.layoutInfo.totalItemsCount - 1
                                    if (end >= 0) listState.animateScrollToItem(end)
                                }
                            }
                            .padding(horizontal = 14.dp, vertical = 7.dp),
                    )
                }
            }
        }
    }

    if (showImageSources) {
        AlertDialog(
            onDismissRequest = { showImageSources = false },
            title = { Text("Attach image") },
            text = { Text("Choose a source") },
            confirmButton = {
                TextButton(onClick = {
                    showImageSources = false
                    picker.launch(
                        TerminalImageSource.Gallery,
                        TerminalAttachmentDraft.MAX_IMAGES - attachments.items.size,
                        session.id,
                    ) { handlePickerResult(it) }
                }) { Text("Gallery") }
            },
            dismissButton = {
                TextButton(onClick = {
                    showImageSources = false
                    picker.launch(
                        TerminalImageSource.Camera,
                        TerminalAttachmentDraft.MAX_IMAGES - attachments.items.size,
                        session.id,
                    ) { handlePickerResult(it) }
                }) { Text("Camera") }
            },
        )
    }
    if (showBringIn) {
        BringInDialog(
            session = session,
            agents = state.agents,
            onBringIn = { agent, model, effort, focus, rounds, auto ->
                onBringIn(session.id, agent, model, effort, focus, rounds, auto)
                showBringIn = false
            },
            onDismiss = { showBringIn = false },
        )
    }
    if (showFiles) {
        AlertDialog(
            onDismissRequest = { showFiles = false },
            title = { Text("Session files") },
            text = {
                when {
                    filesLoading -> Box(
                        Modifier.fillMaxWidth().height(120.dp),
                        contentAlignment = Alignment.Center,
                    ) { CircularProgressIndicator() }
                    filesError != null -> Text(filesError!!, color = MaterialTheme.colorScheme.error)
                    files.isEmpty() -> Text("No files recorded for this session yet.")
                    else -> LazyColumn(Modifier.fillMaxWidth().heightIn(max = 440.dp)) {
                        items(files, key = { "${it.path}:${it.at}:${it.kind}" }) { file ->
                            Column(
                                Modifier.fillMaxWidth()
                                    .clickable(enabled = file.kind != "deleted") {
                                        showFiles = false
                                        filePreviewTarget = file
                                        filePreview = null
                                        filePreviewError = null
                                        filePreviewLoading = true
                                        scope.launch {
                                            onLoadFile(session.id, file.path, 8 * 1024 * 1024).fold(
                                                onSuccess = { filePreview = it },
                                                onFailure = {
                                                    filePreviewError = it.message ?: "Could not read this file."
                                                },
                                            )
                                            filePreviewLoading = false
                                        }
                                    }
                                    .padding(vertical = 7.dp),
                            ) {
                                Text(file.name, maxLines = 1, overflow = TextOverflow.Ellipsis)
                                Text(
                                    "${file.kind} · ${file.bytes} bytes\n${file.path}",
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    maxLines = 2,
                                    overflow = TextOverflow.Ellipsis,
                                )
                            }
                        }
                    }
                }
            },
            confirmButton = { TextButton(onClick = { showFiles = false }) { Text("Done") } },
        )
    }
    filePreviewTarget?.let { target ->
        AlertDialog(
            onDismissRequest = {
                filePreviewTarget = null
                filePreview = null
            },
            title = { Text(target.name, maxLines = 1, overflow = TextOverflow.Ellipsis) },
            text = {
                SessionFilePreviewBody(
                    loading = filePreviewLoading,
                    preview = filePreview,
                    error = filePreviewError,
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    filePreviewTarget = null
                    filePreview = null
                }) { Text("Done") }
            },
        )
    }
}

@Composable
private fun BringInDialog(
    session: RemoteSession,
    agents: List<RemoteAgentChoice>,
    onBringIn: (String, String?, String?, String, Int, Boolean) -> Unit,
    onDismiss: () -> Unit,
) {
    val choices = remember(agents, session.agent) { agents.filter { it.id != session.agent } }
    var agentId by remember(choices) { mutableStateOf(choices.firstOrNull()?.id) }
    val agent = choices.firstOrNull { it.id == agentId }
    var model by remember(agentId) { mutableStateOf<String?>(null) }
    var effort by remember(agentId, model) { mutableStateOf<String?>(null) }
    var focus by remember(session.id) { mutableStateOf("") }
    var rounds by remember(session.id) { mutableStateOf(2) }
    var auto by remember(session.id) { mutableStateOf(false) }
    val efforts = agent?.models?.firstOrNull { it.id == model }?.efforts.orEmpty()

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Bring in a second agent") },
        text = {
            Column {
                Text(
                    "They read this session and talk it through in a desktop tab. The exchange appears here as it lands.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Spacer(Modifier.height(12.dp))
                if (choices.isEmpty()) {
                    Text("No other agent is available on this desktop.")
                } else {
                    LazyRow(horizontalArrangement = Arrangement.spacedBy(7.dp)) {
                        items(choices, key = RemoteAgentChoice::id) { choice ->
                            FilterChip(
                                selected = agentId == choice.id,
                                onClick = { agentId = choice.id },
                                label = { Text(choice.displayName) },
                            )
                        }
                    }
                    if (agent?.models?.isNotEmpty() == true) {
                        Text("Model", style = MaterialTheme.typography.labelMedium)
                        LazyRow(horizontalArrangement = Arrangement.spacedBy(7.dp)) {
                            item(key = "default") {
                                FilterChip(
                                    selected = model == null,
                                    onClick = { model = null },
                                    label = { Text("Default") },
                                )
                            }
                            items(agent.models, key = { it.id }) { option ->
                                FilterChip(
                                    selected = model == option.id,
                                    onClick = { model = option.id },
                                    label = { Text(option.displayName) },
                                )
                            }
                        }
                    }
                    if (efforts.isNotEmpty()) {
                        Text("Effort", style = MaterialTheme.typography.labelMedium)
                        LazyRow(horizontalArrangement = Arrangement.spacedBy(7.dp)) {
                            item(key = "default") {
                                FilterChip(
                                    selected = effort == null,
                                    onClick = { effort = null },
                                    label = { Text("Default") },
                                )
                            }
                            items(efforts, key = { it }) { option ->
                                FilterChip(
                                    selected = effort == option,
                                    onClick = { effort = option },
                                    label = { Text(option) },
                                )
                            }
                        }
                    }
                    OutlinedTextField(
                        value = focus,
                        onValueChange = { focus = it },
                        modifier = Modifier.fillMaxWidth(),
                        placeholder = { Text("What should they look at? (optional)") },
                        minLines = 2,
                        maxLines = 4,
                    )
                    Spacer(Modifier.height(8.dp))
                    LazyRow(horizontalArrangement = Arrangement.spacedBy(7.dp)) {
                        items(listOf(1, 2, 3), key = { it }) { count ->
                            FilterChip(
                                selected = rounds == count,
                                onClick = { rounds = count },
                                label = { Text(if (count == 1) "Quick" else if (count == 2) "Normal" else "Long") },
                            )
                        }
                        item(key = "auto") {
                            FilterChip(
                                selected = auto,
                                onClick = { auto = !auto },
                                label = { Text("Auto-approve") },
                            )
                        }
                    }
                }
            }
        },
        confirmButton = {
            TextButton(
                onClick = {
                    agentId?.let { onBringIn(it, model, effort, focus.trim(), rounds, auto) }
                },
                enabled = agentId != null,
            ) { Text("Bring them in") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

@Composable
private fun ConversationWorkingRow(agent: String) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 4.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 2.dp)
        Spacer(Modifier.width(9.dp))
        Text(
            "${agent.replaceFirstChar(Char::uppercase)} is working…",
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun SessionFilePreviewBody(
    loading: Boolean,
    preview: RemoteSessionFilePreview?,
    error: String?,
) {
    when {
        loading -> Box(
            Modifier.fillMaxWidth().height(180.dp),
            contentAlignment = Alignment.Center,
        ) { CircularProgressIndicator() }
        error != null -> Text(error, color = MaterialTheme.colorScheme.error)
        preview == null -> Text("No preview available.")
        preview.mime.startsWith("image/") && preview.truncated -> Text(
            "This image is larger than the 8 MB phone preview limit (${preview.total} bytes).",
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        preview.mime.startsWith("image/") -> {
            val bitmap = remember(preview.data) { decodeBoundedPreviewBitmap(preview.data) }
            if (bitmap == null) {
                Text("Android could not decode this image.", color = MaterialTheme.colorScheme.onSurfaceVariant)
            } else {
                Image(
                    bitmap = bitmap.asImageBitmap(),
                    contentDescription = preview.path.substringAfterLast('/'),
                    modifier = Modifier.fillMaxWidth().heightIn(max = 480.dp),
                    contentScale = ContentScale.Fit,
                )
            }
        }
        preview.mime.startsWith("text/") -> LazyColumn(
            Modifier.fillMaxWidth().heightIn(max = 480.dp),
        ) {
            item {
                Text(
                    preview.data.decodeToString() + if (preview.truncated) "\n\n…preview truncated…" else "",
                    fontFamily = FontFamily.Monospace,
                    style = MaterialTheme.typography.bodySmall,
                )
            }
        }
        else -> Text(
            "No inline preview for ${preview.mime}. The file is ${preview.total} bytes.",
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

private fun decodeBoundedPreviewBitmap(data: ByteArray): android.graphics.Bitmap? {
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeByteArray(data, 0, data.size, bounds)
    if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return null
    var sample = 1
    while (bounds.outWidth / sample > 2_048 || bounds.outHeight / sample > 2_048) sample *= 2
    return BitmapFactory.decodeByteArray(
        data,
        0,
        data.size,
        BitmapFactory.Options().apply { inSampleSize = sample },
    )
}

@Composable
private fun ConversationTurn(message: RemotePreviewMessage) {
    when (message.role.lowercase()) {
        "user" -> Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.CenterEnd) {
            val content = remember(message.text) { splitConversationAttachments(message.text) }
            Box(
                Modifier.widthIn(max = 330.dp)
                    .background(MaterialTheme.colorScheme.primaryContainer, RoundedCornerShape(18.dp, 18.dp, 5.dp, 18.dp))
                    .padding(horizontal = 13.dp, vertical = 10.dp),
            ) {
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    if (content.text.isNotBlank()) {
                        ConversationMarkdown(content.text, MaterialTheme.colorScheme.onPrimaryContainer)
                    }
                    content.imagePaths.forEach { path ->
                        ConversationActivityRow(
                            label = "Image attachment",
                            summary = path.substringAfterLast('/').ifBlank { path },
                            detail = path,
                            foreground = MaterialTheme.colorScheme.onPrimaryContainer,
                        )
                    }
                }
            }
        }
        "assistant" -> Column(Modifier.fillMaxWidth().padding(end = 14.dp)) {
            ConversationMarkdown(message.text)
        }
        "thinking" -> Text(
            message.text,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            fontStyle = FontStyle.Italic,
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.fillMaxWidth().padding(horizontal = 4.dp),
        )
        "system" -> Text(
            message.text,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            fontFamily = FontFamily.Monospace,
            style = MaterialTheme.typography.labelMedium,
            modifier = Modifier.fillMaxWidth().padding(horizontal = 4.dp),
        )
        else -> ConversationActivityRow(
            label = conversationActivityLabel(message.role),
            summary = conversationActivitySummary(message.text),
            detail = message.text,
        )
    }
}

@Composable
private fun ConversationActivityRow(
    label: String,
    summary: String,
    detail: String,
    foreground: Color = MaterialTheme.colorScheme.onSurfaceVariant,
) {
    var expanded by rememberSaveable(label, detail) { mutableStateOf(false) }
    Column(
        Modifier.fillMaxWidth()
            .background(MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.58f), RoundedCornerShape(7.dp))
            .clickable { expanded = !expanded }
            .padding(horizontal = 10.dp, vertical = 8.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                label,
                color = MaterialTheme.colorScheme.primary,
                fontFamily = FontFamily.Monospace,
                fontWeight = FontWeight.SemiBold,
                style = MaterialTheme.typography.labelSmall,
                maxLines = 1,
            )
            Spacer(Modifier.width(8.dp))
            Text(
                summary,
                color = foreground,
                fontFamily = FontFamily.Monospace,
                style = MaterialTheme.typography.labelSmall,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            Spacer(Modifier.width(8.dp))
            Text(
                if (expanded) "⌃" else "⌄",
                color = foreground,
                style = MaterialTheme.typography.labelMedium,
            )
        }
        if (expanded) {
            HorizontalDivider(
                modifier = Modifier.padding(top = 7.dp, bottom = 7.dp),
                color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.55f),
            )
            SelectionContainer {
                Text(
                    detail,
                    color = foreground,
                    fontFamily = FontFamily.Monospace,
                    style = MaterialTheme.typography.bodySmall,
                )
            }
        }
    }
}

@Composable
private fun ConversationActivityGroup(messages: List<RemotePreviewMessage>) {
    var expanded by rememberSaveable(
        messages.firstOrNull()?.role,
        messages.firstOrNull()?.text,
    ) { mutableStateOf(false) }
    Column(
        Modifier.fillMaxWidth()
            .background(MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.30f), RoundedCornerShape(7.dp)),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth()
                .clickable { expanded = !expanded }
                .padding(horizontal = 10.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                "Activity",
                color = MaterialTheme.colorScheme.primary,
                fontFamily = FontFamily.Monospace,
                fontWeight = FontWeight.SemiBold,
                style = MaterialTheme.typography.labelSmall,
            )
            Spacer(Modifier.width(8.dp))
            Text(
                "${messages.size} steps",
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                fontFamily = FontFamily.Monospace,
                style = MaterialTheme.typography.labelSmall,
                modifier = Modifier.weight(1f),
            )
            Text(
                if (expanded) "⌃" else "⌄",
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                style = MaterialTheme.typography.labelMedium,
            )
        }
        if (expanded) {
            HorizontalDivider(
                color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.45f),
            )
            Column(
                modifier = Modifier.padding(start = 8.dp, top = 6.dp, end = 6.dp, bottom = 6.dp),
                verticalArrangement = Arrangement.spacedBy(5.dp),
            ) {
                messages.forEach { message ->
                    ConversationActivityRow(
                        label = conversationActivityLabel(message.role),
                        summary = conversationActivitySummary(message.text),
                        detail = message.text,
                    )
                }
            }
        }
    }
}

internal sealed interface ConversationTimelineItem {
    data class Turn(val message: RemotePreviewMessage) : ConversationTimelineItem
    data class ActivityGroup(val messages: List<RemotePreviewMessage>) : ConversationTimelineItem
}

/** Consecutive machine activity is one transcript group; human-readable turns remain independent. */
internal fun conversationTimeline(messages: List<RemotePreviewMessage>): List<ConversationTimelineItem> {
    val output = mutableListOf<ConversationTimelineItem>()
    val activity = mutableListOf<RemotePreviewMessage>()
    fun flushActivity() {
        when (activity.size) {
            0 -> Unit
            1 -> output += ConversationTimelineItem.Turn(activity.single())
            else -> output += ConversationTimelineItem.ActivityGroup(activity.toList())
        }
        activity.clear()
    }
    messages.forEach { message ->
        if (isConversationActivity(message.role)) {
            activity += message
        } else {
            flushActivity()
            output += ConversationTimelineItem.Turn(message)
        }
    }
    flushActivity()
    return output
}

internal fun isConversationActivity(role: String): Boolean = role.lowercase() !in setOf(
    "user",
    "assistant",
    "thinking",
    "system",
)

internal data class ConversationAttachmentContent(
    val text: String,
    val imagePaths: List<String>,
)

/** Pulls the terminal submission's generated path list out of the human message. */
internal fun splitConversationAttachments(text: String): ConversationAttachmentContent {
    val lines = text.lines()
    val body = mutableListOf<String>()
    val paths = mutableListOf<String>()
    var index = 0
    while (index < lines.size) {
        if (lines[index].trim() != "Attached images:") {
            body += lines[index]
            index += 1
            continue
        }
        var cursor = index + 1
        val found = mutableListOf<String>()
        while (cursor < lines.size) {
            val line = lines[cursor].trim()
            if (!line.startsWith("- ") || line.length <= 2) break
            found += line.removePrefix("- ").trim()
            cursor += 1
        }
        if (found.isEmpty()) {
            body += lines[index]
            index += 1
        } else {
            paths += found
            index = cursor
        }
    }
    return ConversationAttachmentContent(body.joinToString("\n").trim(), paths)
}

internal fun conversationActivityLabel(role: String): String = when (role.lowercase()) {
    "exec", "exec_command", "bash", "shell" -> "Command"
    "apply_patch", "edit", "write" -> "File edit"
    "image" -> "Image generation"
    "tool_output" -> "Output"
    "agent_message" -> "Agent message"
    else -> role.replace('_', ' ').trim().replaceFirstChar(Char::uppercase).ifBlank { "Tool" }
}

internal fun conversationActivitySummary(text: String): String {
    val compact = text.trim().replace(Regex("\\s+"), " ")
    if (compact.isEmpty()) return "No details"
    return if (compact.length <= 110) compact else compact.take(109).trimEnd() + "…"
}

@Composable
private fun ConnectionLabel(connection: ConnectionState, path: com.adroited.aiterm.remote.RemotePath?) {
    val (label, color) = when (connection) {
        ConnectionState.Connected -> when (path) {
            com.adroited.aiterm.remote.RemotePath.DIRECT -> "connected · direct"
            com.adroited.aiterm.remote.RemotePath.RELAY -> "connected · relay"
            com.adroited.aiterm.remote.RemotePath.LAN -> "connected · LAN"
            com.adroited.aiterm.remote.RemotePath.VPN -> "connected · VPN"
            else -> "connected"
        } to MaterialTheme.colorScheme.tertiary
        ConnectionState.Connecting -> "connecting" to MaterialTheme.colorScheme.primary
        ConnectionState.Reconnecting -> "reconnecting" to MaterialTheme.colorScheme.primary
        ConnectionState.Locked -> "locked" to MaterialTheme.colorScheme.error
        ConnectionState.Revoked -> "revoked" to MaterialTheme.colorScheme.error
        ConnectionState.Disconnected -> "offline" to MaterialTheme.colorScheme.onSurfaceVariant
    }
    Text(label, style = MaterialTheme.typography.labelMedium, color = color)
}

internal fun isConversationSessionLive(session: RemoteSession, tabs: List<RemoteTab>): Boolean =
    tabs.any { it.sessionId == session.id }

internal fun conversationSessions(
    sessions: List<RemoteSession>,
    tabs: List<RemoteTab>,
    query: String,
    starred: Set<String> = emptySet(),
    withFiles: Set<String> = emptySet(),
    broughtIn: Map<String, String> = emptyMap(),
    agentFilter: String? = null,
    filesOnly: Boolean = false,
    activeOnly: Boolean = false,
    foldedCrews: Set<String> = emptySet(),
): List<RemoteSession> {
    val needle = query.trim().lowercase()
    val sorted = sessions.asSequence()
        .filter { session ->
            needle.isEmpty() || listOf(session.title, session.agent, session.projectPath, session.groupPath)
                .any { needle in it.lowercase() }
        }
        .filter { agentFilter == null || it.agent == agentFilter }
        .filter { !filesOnly || it.id in withFiles }
        .filter { !activeOnly || isConversationSessionLive(it, tabs) }
        .sortedWith(
            compareByDescending<RemoteSession> { it.id in starred }
                .thenByDescending { isConversationSessionLive(it, tabs) }
                .thenByDescending { it.lastActive },
        )
        .toList()
    if (broughtIn.isEmpty()) return sorted
    val visibleIds = sorted.mapTo(hashSetOf()) { it.id }
    val result = ArrayList<RemoteSession>(sorted.size)
    val placed = hashSetOf<String>()
    for (session in sorted) {
        if (session.id in placed) continue
        if (broughtIn[session.id] in visibleIds) continue
        result += session
        placed += session.id
        if (session.id !in foldedCrews) {
            sorted.filterTo(result) { child ->
                broughtIn[child.id] == session.id && placed.add(child.id)
            }
        } else {
            sorted.filter { child -> broughtIn[child.id] == session.id }.forEach { placed += it.id }
        }
    }
    return result
}

private fun relativeSessionTime(lastActive: Long, nowMillis: Long = System.currentTimeMillis()): String {
    if (lastActive <= 0) return "RECENT"
    val timestamp = if (lastActive > 100_000_000_000L) lastActive else lastActive * 1_000
    val seconds = ((nowMillis - timestamp) / 1_000).coerceAtLeast(0)
    return when {
        seconds < 60 -> "NOW"
        seconds < 3_600 -> "${seconds / 60}M"
        seconds < 86_400 -> "${seconds / 3_600}H"
        else -> "${seconds / 86_400}D"
    }
}

private fun agentColor(agent: String): Color = when (agent.lowercase()) {
    "claude", "anthropic" -> Color(0xFFE8956B)
    "codex", "openai" -> Color(0xFF7DB7FF)
    "grok", "xai" -> Color(0xFFB0BEC5)
    "opencode" -> Color(0xFFB39DDB)
    else -> Color(0xFF75D8B4)
}
