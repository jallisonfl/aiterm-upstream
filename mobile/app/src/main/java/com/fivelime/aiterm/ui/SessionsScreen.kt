package com.fivelime.aiterm.ui

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.ui.draw.clip
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Star
import androidx.compose.material.icons.filled.SubdirectoryArrowRight
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Terminal
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DrawerValue
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.ModalDrawerSheet
import androidx.compose.material3.ModalNavigationDrawer
import androidx.compose.material3.NavigationDrawerItem
import androidx.compose.material3.rememberDrawerState
import androidx.compose.material.icons.filled.LinkOff
import androidx.compose.material.icons.filled.Menu
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.rememberCoroutineScope
import kotlinx.coroutines.launch
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.TextFieldDefaults
import com.fivelime.aiterm.SessionState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.DropdownMenu
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.KeyboardArrowUp
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.animation.core.animateFloat
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.foundation.clickable
import com.fivelime.aiterm.AppViewModel
import com.fivelime.aiterm.Session
import com.fivelime.aiterm.UsageAmount
import com.fivelime.aiterm.UsageBar

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SessionsScreen(vm: AppViewModel, outer: PaddingValues) {
    var renaming by remember { mutableStateOf<Session?>(null) }
    val drawer = rememberDrawerState(DrawerValue.Closed)
    val scope = rememberCoroutineScope()
    val visible = vm.visibleSessions

    renaming?.let { s ->
        RenameDialog(current = s.title, onDone = { vm.rename(s, it); renaming = null }, onDismiss = { renaming = null })
    }
    // Opening the drawer is also the moment to freshen what it shows.
    LaunchedEffect(drawer.isOpen) { if (drawer.isOpen) { vm.loadUsage(); vm.checkDesktops() } }
    ModalNavigationDrawer(
        drawerState = drawer,
        drawerContent = { AppDrawer(vm, close = { scope.launch { drawer.close() } }) },
    ) {
    Scaffold(
        modifier = Modifier.padding(outer).dismissKeyboardOnTap(),
        topBar = {
            TopAppBar(
                navigationIcon = {
                    IconButton(onClick = { scope.launch { drawer.open() } }) { Icon(Icons.Filled.Menu, "Menu") }
                },
                title = { DesktopSwitcher(vm) },
                actions = {
                    // A blank shell on the desktop, like the home launcher's —
                    // driven from here.
                    IconButton(onClick = { vm.openTerminal() }, enabled = vm.connected && !vm.terminalOpening) {
                        Icon(Icons.Filled.Terminal, "Open a terminal", tint = if (vm.connected) Accent else Muted)
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = Bg),
            )
        },
        floatingActionButton = {
            FloatingActionButton(onClick = { vm.composingNew = true }) { Icon(Icons.Filled.Add, "New session") }
        },
        containerColor = Bg,
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            Dashboard(vm)
            if (vm.sessions.isEmpty()) {
                Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    Text(if (vm.connected) "No sessions yet" else "Connecting…", color = Muted)
                }
            } else if (visible.isEmpty()) {
                Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    Text("Nothing matches these filters", color = Muted)
                }
            } else {
                LazyColumn(Modifier.fillMaxSize()) {
                    items(visible, key = { it.id }) { s ->
                        SessionRow(
                            s, vm.stateOf(s), showFolder = true,
                            starred = s.id in vm.stars,
                            satellite = vm.broughtIn[s.id] != null && visible.any { it.id == vm.broughtIn[s.id] },
                            crewAgents = vm.broughtIn.filterValues { it == s.id }.keys
                                .mapNotNull { id -> vm.sessions.find { it.id == id }?.agent },
                            folded = s.id in vm.foldedCrews,
                            onCrewTap = { vm.toggleCrew(s.id) },
                            crewNeedsYou = vm.broughtIn.any { it.value == s.id && vm.activity[it.key] == "attention" },
                            onLongClick = { renaming = s },
                        ) { vm.select(s) }
                    }
                }
            }
        }
    }
    }
}

/** Search plus the filter chips. Usage lives in the drawer now — this
 *  strip is for finding sessions. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun Dashboard(vm: AppViewModel) {
    Column(Modifier.fillMaxWidth().padding(horizontal = 12.dp)) {
        OutlinedTextField(
            value = vm.query, onValueChange = { vm.search(it) },
            placeholder = { Text("Search sessions…", color = Muted) },
            leadingIcon = { Icon(Icons.Filled.Search, null, tint = Muted) },
            trailingIcon = { if (vm.query.isNotEmpty() && vm.results == null) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp) },
            singleLine = true, shape = RoundedCornerShape(12.dp),
            colors = TextFieldDefaults.colors(focusedContainerColor = Surface1, unfocusedContainerColor = Surface1,
                focusedIndicatorColor = Color.Transparent, unfocusedIndicatorColor = Color.Transparent),
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(8.dp))
        // One tap on, one tap off: an engine, sessions that made files,
        // sessions alive right now. They combine.
        val agents = remember(vm.sessions) { vm.sessions.map { it.agent }.distinct().sorted() }
        // The engine chips arrive after the first frame (sessions load
        // async) and a keyed LazyRow anchors to what it was already
        // showing — leaving the row scrolled past the new first chip.
        // Snap back to the start whenever the set changes.
        val chipRow = rememberLazyListState()
        LaunchedEffect(agents) { chipRow.scrollToItem(0) }
        LazyRow(state = chipRow, horizontalArrangement = androidx.compose.foundation.layout.Arrangement.spacedBy(6.dp)) {
            items(agents, key = { it }) { a ->
                FilterChip(
                    selected = vm.agentFilter == a,
                    onClick = { vm.agentFilter = if (vm.agentFilter == a) null else a },
                    label = { Text(a.replaceFirstChar { it.uppercase() }) },
                    leadingIcon = { AgentIcon(a, 16.dp) },
                    colors = filterColors(),
                )
            }
            item(key = "files") {
                FilterChip(
                    selected = vm.filesOnly, onClick = { vm.filesOnly = !vm.filesOnly },
                    label = { Text("Has files") }, colors = filterColors(),
                )
            }
            item(key = "active") {
                FilterChip(
                    selected = vm.activeOnly, onClick = { vm.activeOnly = !vm.activeOnly },
                    label = { Text("Active") }, colors = filterColors(),
                )
            }
        }
    }
}

@Composable
private fun filterColors() = FilterChipDefaults.filterChipColors(
    selectedContainerColor = Accent.copy(alpha = 0.2f),
    selectedLabelColor = Accent,
)

/** The app's one menu: who we're connected to, every usage source in
 *  full — bars with resets, balances, the error line when a source is
 *  failing — then the few actions. Usage lives here, not on the home
 *  page: the list is for sessions. */
@Composable
private fun AppDrawer(vm: AppViewModel, close: () -> Unit) {
    /** Usage sources opened up for the full picture; closed rows show only
     *  the weekly line. */
    var expandedUsage by remember { mutableStateOf<Set<String>>(emptySet()) }
    ModalDrawerSheet(drawerContainerColor = Bg) {
        Column(Modifier.verticalScroll(rememberScrollState()).padding(bottom = 16.dp)) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth().padding(start = 20.dp, top = 24.dp, end = 8.dp, bottom = 4.dp),
            ) {
                Dot(if (vm.connected) Green else Muted)
                Spacer(Modifier.width(10.dp))
                Column {
                    Text(vm.desktop?.label ?: "Desktop", style = MaterialTheme.typography.titleLarge)
                    Text(if (vm.connected) "connected" else "connecting…", style = MaterialTheme.typography.labelSmall, color = Muted)
                }
                Spacer(Modifier.weight(1f))
                // Tapping the dimmed strip also closes, but nothing says so;
                // an X does.
                IconButton(onClick = close) { Icon(Icons.Filled.Close, "Close menu") }
            }
            // Every paired desktop, when there is more than one: tap to
            // switch. Every dot is a status — the shown one live, the rest
            // from the probe the drawer's opening fired.
            if (vm.desktops.size > 1) {
                HorizontalDivider(Modifier.padding(vertical = 12.dp), color = Surface1)
                Text("DESKTOPS", style = MaterialTheme.typography.labelSmall, color = Muted, modifier = Modifier.padding(horizontal = 20.dp))
                vm.desktops.forEach { d ->
                    val active = d.fingerprint == vm.desktop?.fingerprint
                    NavigationDrawerItem(
                        label = { Text(d.label, fontWeight = if (active) FontWeight.SemiBold else FontWeight.Normal) },
                        icon = {
                            Dot(
                                if (active) { if (vm.connected) Green else Muted }
                                else when (vm.reachable[d.fingerprint]) {
                                    true -> Green
                                    false -> Surface1
                                    null -> Muted // probing, no answer yet
                                },
                            )
                        },
                        selected = active,
                        onClick = { if (!active) { vm.switchTo(d); close() } },
                        modifier = Modifier.padding(horizontal = 12.dp),
                    )
                }
            }
            HorizontalDivider(Modifier.padding(vertical = 12.dp), color = Surface1)
            // The whole section folds, and starts folded: the menu is for
            // getting somewhere; usage is a look when wanted.
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth().clickable { vm.usageOpen = !vm.usageOpen }
                    .padding(horizontal = 20.dp, vertical = 8.dp),
            ) {
                Text("USAGE", style = MaterialTheme.typography.labelSmall, color = Muted)
                Spacer(Modifier.weight(1f))
                Icon(
                    if (vm.usageOpen) Icons.Filled.KeyboardArrowDown else Icons.Filled.KeyboardArrowRight,
                    if (vm.usageOpen) "Hide usage" else "Show usage",
                    tint = Muted, modifier = Modifier.size(18.dp),
                )
            }
            if (vm.usageOpen && vm.usage.isEmpty()) {
                Text("Nothing reported yet", color = Muted, style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.padding(horizontal = 20.dp, vertical = 8.dp))
            }
            if (vm.usageOpen) vm.usage.forEach { u ->
                val expanded = u.id in expandedUsage
                val weekly = u.bars.firstOrNull {
                    it.kind.startsWith("weekly") || it.kind == "grok_period" || it.label.contains("week", ignoreCase = true)
                }
                // A local router that publishes no balance is not failing —
                // there is nothing to bill. It is simply active.
                val healthy = u.state == "ok" || u.state == "no_balance"
                val credit = if (weekly == null) u.amounts.firstOrNull() else null
                // Amounts worth a line: a balance that is not the one already
                // on the closed row, and not zero.
                val amounts = u.amounts.filter { it !== credit && it.amount != 0.0 }
                // A tap only where it shows something the closed row does not:
                // a source with one weekly bar and nothing else is already told.
                val hasMore = u.bars.any { it !== weekly } || amounts.isNotEmpty() || (!healthy && u.detail.isNotBlank())
                Column(
                    Modifier.fillMaxWidth()
                        .let { m ->
                            if (hasMore) m.clickable {
                                expandedUsage = if (expanded) expandedUsage - u.id else expandedUsage + u.id
                            } else m
                        }
                        .padding(horizontal = 28.dp, vertical = 6.dp),
                ) {
                    // The closed row is a name and one value in a shared
                    // right-hand column: the week gone, the balance, or the
                    // trouble. Sized like the menu items below it.
                    val value: String?
                    val tint: androidx.compose.ui.graphics.Color
                    when {
                        !healthy -> { value = u.state.replace('_', ' '); tint = Amber }
                        weekly != null -> {
                            value = "${weekly.percent.toInt()}%"
                            tint = when { weekly.percent < 50 -> Muted; weekly.percent < 80 -> Amber; else -> Red }
                        }
                        credit != null -> { value = (if (credit.currency == "USD") "$" else "") + "%.2f".format(credit.amount); tint = Muted }
                        else -> { value = null; tint = Muted }
                    }
                    Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth().heightIn(min = 40.dp)) {
                        AgentIcon(u.id.removePrefix("provider:"), 20.dp)
                        Spacer(Modifier.width(16.dp))
                        Text(
                            u.name, style = MaterialTheme.typography.bodyLarge, maxLines = 1,
                            overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f),
                        )
                        if (expanded && u.plan.isNotBlank()) {
                            Text(u.plan, style = MaterialTheme.typography.labelSmall, color = Muted, maxLines = 1)
                            Spacer(Modifier.width(12.dp))
                        }
                        if (value != null) {
                            Text(value, style = MaterialTheme.typography.bodyMedium, color = tint, maxLines = 1)
                        }
                    }
                    if (expanded) {
                        Column(Modifier.padding(start = 36.dp, bottom = 4.dp)) {
                            u.bars.forEach { b -> UsageBarRow(b) }
                            amounts.forEach { am -> UsageAmountRow(am) }
                            if (!healthy && u.detail.isNotBlank()) {
                                Text(u.detail, style = MaterialTheme.typography.labelSmall, color = Amber,
                                    modifier = Modifier.padding(top = 4.dp))
                            }
                        }
                    }
                }
            }
            HorizontalDivider(Modifier.padding(vertical = 12.dp), color = Surface1)
            NavigationDrawerItem(
                label = { Text("Refresh") },
                icon = { Icon(Icons.Filled.Refresh, null) },
                selected = false,
                onClick = { vm.refreshNow(); vm.loadUsage(); close() },
                modifier = Modifier.padding(horizontal = 12.dp),
            )
            NavigationDrawerItem(
                label = { Text("Settings") },
                icon = { Icon(Icons.Filled.Settings, null) },
                selected = false,
                onClick = { close(); vm.showSettings = true },
                modifier = Modifier.padding(horizontal = 12.dp),
            )
            NavigationDrawerItem(
                label = { Text("Add a desktop") },
                icon = { Icon(Icons.Filled.Add, null) },
                selected = false,
                onClick = { close(); vm.showPair = true },
                modifier = Modifier.padding(horizontal = 12.dp),
            )
            NavigationDrawerItem(
                label = { Text("Forget this desktop") },
                icon = { Icon(Icons.Filled.LinkOff, null) },
                selected = false,
                onClick = { close(); vm.forget() },
                modifier = Modifier.padding(horizontal = 12.dp),
            )
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun SessionRow(
    s: Session, state: SessionState, showFolder: Boolean, starred: Boolean = false,
    satellite: Boolean = false, crewAgents: List<String> = emptyList(), folded: Boolean = false, onCrewTap: () -> Unit = {},
    crewNeedsYou: Boolean = false,
    onLongClick: () -> Unit = {}, onClick: () -> Unit,
) {
    ListItem(
        modifier = Modifier.combinedClickable(onClick = onClick, onLongClick = onLongClick)
            .padding(start = if (satellite) 26.dp else 0.dp),
        colors = ListItemDefaults.colors(containerColor = Color.Transparent),
        leadingContent = {
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (satellite) {
                    Icon(Icons.Filled.SubdirectoryArrowRight, "Brought in", tint = Muted, modifier = Modifier.size(16.dp))
                    Spacer(Modifier.width(4.dp))
                }
                AgentIcon(s.agent, if (satellite) 24.dp else 30.dp)
            }
        },
        headlineContent = {
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (starred) {
                    Icon(Icons.Filled.Star, "Starred", tint = Amber, modifier = Modifier.size(15.dp))
                    Spacer(Modifier.width(5.dp))
                }
                Text(s.title.ifBlank { "Untitled" }, maxLines = 2, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = false))
            }
        },
        supportingContent = {
            Text(
                buildString {
                    append(relativeTime(s.last_active))
                    if (showFolder) { append(" · "); append(folderName(s.group_path)) }
                    s.branch?.let { append(" · "); append(it) }
                    if (s.forked) append(" · fork")
                },
                color = Muted, maxLines = 1, overflow = TextOverflow.Ellipsis,
            )
        },
        trailingContent = {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = androidx.compose.foundation.layout.Arrangement.spacedBy(6.dp)) {
                // The state indicator is always the rightmost thing, so the
                // dots right-align down the list; the crew fold sits beside
                // it — who is in the crew, by their marks, and a caret,
                // padded into a real touch target.
                if (crewAgents.isNotEmpty()) {
                    Row(
                        Modifier.clip(RoundedCornerShape(10.dp))
                            .background(Accent.copy(alpha = if (folded) 0.08f else 0.15f))
                            .clickable(onClick = onCrewTap)
                            .padding(start = 8.dp, end = 4.dp, top = 7.dp, bottom = 7.dp),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = androidx.compose.foundation.layout.Arrangement.spacedBy(3.dp),
                    ) {
                        crewAgents.take(3).forEach { AgentIcon(it, 16.dp) }
                        if (crewAgents.size > 3) {
                            Text("+${crewAgents.size - 3}", style = MaterialTheme.typography.labelMedium, color = Accent)
                        }
                        Icon(
                            if (folded) Icons.Filled.KeyboardArrowRight else Icons.Filled.KeyboardArrowDown,
                            if (folded) "Show crew" else "Hide crew",
                            tint = Accent, modifier = Modifier.size(18.dp),
                        )
                    }
                }
                // A brought-in agent of THIS session is parked on a prompt —
                // its own row may be folded away, so the master's row says so.
                if (crewNeedsYou) StateChip("crew needs you", stateColor(SessionState.NeedsYou), pulse = true)
                // Open on the desktop is ambient, not news — a quiet dot, no words.
                if (state == SessionState.OnDesktop) Dot(stateColor(state))
                else stateLabel(state)?.let { StateChip(it, stateColor(state), pulse = state == SessionState.Working) }
            }
        },
    )
}

@Composable
fun Dot(color: Color) = Box(Modifier.size(8.dp).background(color, CircleShape))

@Composable
fun StateChip(label: String, color: Color, pulse: Boolean = false) {
    // Label first, dot last: the dot is the indicator, and it sits at the
    // right edge so every row's indicator lines up in one column.
    Row(verticalAlignment = Alignment.CenterVertically) {
        Text(label, style = MaterialTheme.typography.labelSmall, color = color)
        Spacer(Modifier.width(6.dp))
        if (pulse) PulsingDot(color) else Dot(color)
    }
}

@Composable
fun PulsingDot(color: Color) {
    val t = androidx.compose.animation.core.rememberInfiniteTransition(label = "pulse")
    val a by t.animateFloat(
        initialValue = 0.3f, targetValue = 1f,
        animationSpec = androidx.compose.animation.core.infiniteRepeatable(
            androidx.compose.animation.core.tween(700), androidx.compose.animation.core.RepeatMode.Reverse),
        label = "alpha",
    )
    Box(Modifier.size(8.dp).background(color.copy(alpha = a), CircleShape))
}

/** "1h 56m" / "6d" — the reset, sized for the end of a row. */
private fun resetsIn(iso: String): String = runCatching {
    val t = java.time.OffsetDateTime.parse(iso).toInstant()
    val mins = java.time.Duration.between(java.time.Instant.now(), t).toMinutes()
    when {
        mins <= 0 -> "soon"
        mins < 60 -> "${mins}m"
        mins < 10 * 60 -> "${mins / 60}h ${mins % 60}m"
        mins < 48 * 60 -> "${mins / 60}h"
        else -> "${mins / (60 * 24)}d"
    }
}.getOrDefault("")

/** One rename dialog for everywhere a session can be renamed. Clearing the
 *  field and saving restores the engine's own name. */
@Composable
internal fun RenameDialog(current: String, onDone: (String) -> Unit, onDismiss: () -> Unit) {
    var draft by remember { mutableStateOf(current) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Rename session") },
        text = {
            OutlinedTextField(
                value = draft, onValueChange = { draft = it },
                singleLine = true,
                placeholder = { Text("Leave empty to restore the original name", color = Muted) },
            )
        },
        confirmButton = { TextButton(onClick = { onDone(draft) }) { Text("Rename") } },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

/** One limit line: what it is, how full, when it lets go. */
@Composable
private fun UsageBarRow(b: UsageBar) {
    Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(top = 5.dp)) {
        Text(
            b.label.replace(" limit", ""), style = MaterialTheme.typography.labelSmall, color = Muted,
            modifier = Modifier.width(64.dp), maxLines = 1, overflow = TextOverflow.Ellipsis,
        )
        LinearProgressIndicator(
            progress = { (b.percent / 100.0).toFloat().coerceIn(0f, 1f) },
            modifier = Modifier.weight(1f),
            color = when { b.percent < 50 -> Muted; b.percent < 80 -> Amber; else -> Red },
            trackColor = Surface1,
        )
        Spacer(Modifier.width(10.dp))
        Text(
            "${b.percent.toInt()}%", style = MaterialTheme.typography.labelSmall,
            modifier = Modifier.width(36.dp), textAlign = TextAlign.End,
        )
        val reset = resetsIn(b.resets_at)
        Text(
            if (reset.isBlank()) "" else "· $reset", style = MaterialTheme.typography.labelSmall, color = Muted.copy(alpha = 0.7f),
            modifier = Modifier.width(64.dp), textAlign = TextAlign.End, maxLines = 1,
        )
    }
}

@Composable
private fun UsageAmountRow(am: UsageAmount) {
    Row(modifier = Modifier.padding(top = 5.dp)) {
        Text(am.label, style = MaterialTheme.typography.labelSmall, color = Muted, modifier = Modifier.width(64.dp))
        Text(
            (if (am.currency == "USD") "$" else "") + "%.2f".format(am.amount) + (am.of?.let { " of %.0f".format(it) } ?: ""),
            style = MaterialTheme.typography.labelSmall,
        )
    }
}

/** The desktop's name in the top bar is the switcher: tap it for every
 *  paired desktop, each with its status dot, the shown one checked; a
 *  rename and "Add a desktop" sit under them. Switching keeps you on this
 *  screen — the list simply becomes the other desktop's. */
@Composable
private fun DesktopSwitcher(vm: AppViewModel) {
    var open by remember { mutableStateOf(false) }
    var renaming by remember { mutableStateOf(false) }
    val current = vm.desktop
    // Fresh dots for the other desktops each time the menu opens.
    LaunchedEffect(open) { if (open) vm.checkDesktops() }
    Box {
        Row(
            Modifier.clickable { open = !open }.padding(top = 6.dp, bottom = 6.dp, end = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Dot(if (vm.connected) Green else Muted)
            Spacer(Modifier.width(10.dp))
            Text(current?.label ?: "Desktop", maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = false))
            Spacer(Modifier.width(4.dp))
            Icon(
                if (open) Icons.Filled.KeyboardArrowUp else Icons.Filled.KeyboardArrowDown,
                "Switch desktop", tint = Muted, modifier = Modifier.size(18.dp),
            )
        }
        DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
            vm.desktops.forEach { d ->
                val active = d.fingerprint == current?.fingerprint
                DropdownMenuItem(
                    leadingIcon = {
                        Dot(
                            if (active) { if (vm.connected) Green else Muted }
                            else when (vm.reachable[d.fingerprint]) { true -> Green; false -> Surface1; null -> Muted },
                        )
                    },
                    text = {
                        Column {
                            Text(d.label, fontWeight = if (active) FontWeight.SemiBold else FontWeight.Normal, maxLines = 1, overflow = TextOverflow.Ellipsis)
                            if (d.friendlyName.isNotBlank() && d.friendlyName != d.name)
                                Text(d.name, style = MaterialTheme.typography.labelSmall, color = Muted, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        }
                    },
                    trailingIcon = { if (active) Icon(Icons.Filled.Check, "Shown now", tint = Accent, modifier = Modifier.size(18.dp)) },
                    onClick = { open = false; if (!active) vm.switchTo(d) },
                )
            }
            HorizontalDivider(Modifier.padding(vertical = 4.dp), color = Surface1)
            if (current != null) DropdownMenuItem(
                text = { Text("Rename this desktop") },
                onClick = { open = false; renaming = true },
            )
            DropdownMenuItem(
                text = { Text("Add a desktop") },
                onClick = { open = false; vm.showPair = true },
            )
        }
    }
    if (renaming && current != null) {
        var name by remember(current.fingerprint) { mutableStateOf(current.friendlyName) }
        AlertDialog(
            onDismissRequest = { renaming = false },
            title = { Text("Rename this desktop") },
            text = {
                Column {
                    OutlinedTextField(
                        value = name, onValueChange = { name = it.take(64) }, singleLine = true,
                        placeholder = { Text(current.name, color = Muted) },
                        label = { Text("Name on this phone") },
                    )
                    Spacer(Modifier.height(6.dp))
                    Text("Blank goes back to the desktop's own name: " + current.name, style = MaterialTheme.typography.labelSmall, color = Muted)
                }
            },
            confirmButton = { TextButton(onClick = { vm.renameDesktop(current, name); renaming = false }) { Text("Save") } },
            dismissButton = { TextButton(onClick = { renaming = false }) { Text("Cancel") } },
        )
    }
}
