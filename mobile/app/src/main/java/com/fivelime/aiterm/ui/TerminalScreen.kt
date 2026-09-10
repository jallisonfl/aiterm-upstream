package com.fivelime.aiterm.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.imeNestedScroll
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.KeyboardArrowUp
import androidx.compose.material.icons.filled.Keyboard
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.layout.boundsInRoot
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.fivelime.aiterm.AppViewModel
import kotlinx.coroutines.delay

/** A plain shell on the desktop, driven from the phone: the same blank
 *  terminal the desktop's home launcher opens. The screen is text — the
 *  desktop renders the real thing — polled while this is on screen; the
 *  input row sends a line at a time, and the key strip sends the control
 *  characters a line cannot carry. */
@OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
fun TerminalScreen(vm: AppViewModel, tab: String, outer: PaddingValues) {
    var line by remember { mutableStateOf("") }
    val scroll = rememberScrollState()
    // The key strip folds away when the person wants the screen for output.
    var keysShown by rememberSaveable { mutableStateOf(true) }
    val focus = remember { FocusRequester() }
    var barBounds by remember { mutableStateOf<Rect?>(null) }
    val imeUp = WindowInsets.ime.getBottom(LocalDensity.current) > 0

    // The screen lives while it is looked at: poll fast, and stop the moment
    // the tab is gone (a 404 clears vm.terminalTab and this screen with it).
    LaunchedEffect(tab) {
        while (vm.terminalTab == tab) {
            vm.pollTerminal()
            delay(700)
        }
    }
    // New output lands at the bottom, which is where a terminal is read —
    // unless the person has scrolled up to read older output, in which case
    // the screen holds still under them.
    LaunchedEffect(vm.terminalLines) {
        val wasAtEnd = scroll.value >= scroll.maxValue - 24
        if (wasAtEnd) scroll.scrollTo(scroll.maxValue)
    }
    // The shell is what this screen is for: the field is ready to type into
    // the moment it opens.
    LaunchedEffect(tab) { runCatching { focus.requestFocus() } }

    val send = {
        // An empty Send is still Enter: the terminal may already hold a
        // prompt, or sit at a confirmation. No filler text is required.
        if (line.isEmpty()) vm.sendTerminal("\r", enter = false)
        else if (line.isNotBlank()) vm.sendTerminal(line)
        line = ""
    }

    Scaffold(
        modifier = Modifier.padding(outer).imePadding().dismissKeyboardOnTapOutside { barBounds },
        topBar = {
            TopAppBar(
                navigationIcon = {
                    IconButton(onClick = { vm.terminalTab = null }) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back — the shell keeps running on the desktop")
                    }
                },
                title = { Text(vm.terminalTitle) },
                actions = {
                    IconButton(onClick = { vm.closeTerminal() }) {
                        Icon(Icons.Filled.Close, "End the shell", tint = Muted)
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = Bg),
            )
        },
        containerColor = Bg,
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            Column(
                Modifier
                    .weight(1f)
                    .fillMaxWidth()
                    // Dragging the output while the keyboard is up puts the
                    // keyboard away — the screen is being read, not typed at.
                    .then(if (imeUp) Modifier.imeNestedScroll() else Modifier)
                    .verticalScroll(scroll)
                    .padding(horizontal = 10.dp, vertical = 6.dp),
            ) {
                val text = vm.terminalLines.joinToString("\n").trimEnd('\n')
                Text(
                    if (text.isEmpty()) "…" else text,
                    fontFamily = FontFamily.Monospace,
                    fontSize = 12.sp,
                    lineHeight = 15.sp,
                    color = MaterialTheme.colorScheme.onSurface,
                    softWrap = false,
                    modifier = Modifier.horizontalScroll(rememberScrollState()),
                )
            }
            Column(Modifier.onGloballyPositioned { barBounds = it.boundsInRoot() }) {
            if (keysShown) TerminalKeys { k -> vm.sendTerminal(k, enter = false) }
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 6.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                IconButton(onClick = { keysShown = !keysShown }) {
                    Icon(if (keysShown) Icons.Filled.KeyboardArrowDown else Icons.Filled.Keyboard,
                        if (keysShown) "Hide the console keys" else "Show the console keys", tint = Muted)
                }
                OutlinedTextField(
                    value = line,
                    onValueChange = { line = it },
                    modifier = Modifier.weight(1f).focusRequester(focus),
                    placeholder = { Text("command", color = Muted) },
                    textStyle = MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace),
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
                    keyboardActions = KeyboardActions(onSend = { send() }),
                )
                IconButton(onClick = send) { Icon(Icons.AutoMirrored.Filled.Send, if (line.isEmpty()) "Enter" else "Run", tint = Accent) }
            }
            }
        }
    }
}

/** The keys a line cannot carry, in xterm's encoding: the editing and
 *  cursor keys a TUI dialog or a shell line needs, and the control chords a
 *  phone keyboard has no way to type. Two rows, each scrolling sideways. */
@Composable
private fun TerminalKeys(onKey: (String) -> Unit) {
    val rows = listOf(
        listOf(
            "Esc" to "\u001B", "Tab" to "\t", "\u21E7Tab" to "\u001B[Z", "\u2190" to "\u001B[D", "\u2191" to "\u001B[A",
            "\u2193" to "\u001B[B", "\u2192" to "\u001B[C", "Home" to "\u001B[H", "End" to "\u001B[F",
            "PgUp" to "\u001B[5~", "PgDn" to "\u001B[6~", "Del" to "\u001B[3~",
        ),
        listOf(
            "Ctrl+C" to "\u0003", "Ctrl+D" to "\u0004", "Ctrl+Z" to "\u001A", "Ctrl+L" to "\u000C",
            "Ctrl+A" to "\u0001", "Ctrl+E" to "\u0005", "Ctrl+K" to "\u000B", "Ctrl+U" to "\u0015",
            "Ctrl+W" to "\u0017", "Ctrl+R" to "\u0012", "Ctrl+\\" to "\u001C",
        ),
    )
    Column(Modifier.fillMaxWidth().padding(vertical = 2.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        rows.forEach { keys ->
            Row(
                Modifier.fillMaxWidth()
                    .horizontalScroll(rememberScrollState())
                    .padding(horizontal = 8.dp),
                horizontalArrangement = Arrangement.spacedBy(6.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                keys.forEach { (label, seq) ->
                    Box(
                        Modifier.background(Accent.copy(alpha = 0.12f), RoundedCornerShape(12.dp))
                            .clickable { onKey(seq) }
                            .padding(horizontal = 12.dp, vertical = 6.dp),
                    ) { Text(label, style = MaterialTheme.typography.labelMedium, color = Accent) }
                }
            }
        }
    }
}
