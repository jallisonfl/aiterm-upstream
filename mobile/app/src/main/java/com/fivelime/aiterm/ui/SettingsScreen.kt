package com.fivelime.aiterm.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.height
import kotlinx.coroutines.launch
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.KeyboardArrowUp
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Switch
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.fivelime.aiterm.AppViewModel
import com.fivelime.aiterm.Desktop
import com.fivelime.aiterm.Road
import com.fivelime.aiterm.Roads

private val THEMES = listOf("dark" to "Deep blue", "black" to "Black", "nord" to "Nord", "light" to "Light")

/** The short list a person actually picks from; anything else can be added
 *  when someone needs it. Empty id = the phone's own zone. */
private val ZONES = listOf(
    "" to "Phone's time zone",
    "America/New_York" to "Eastern (New York)",
    "America/Chicago" to "Central (Chicago)",
    "America/Denver" to "Mountain (Denver)",
    "America/Los_Angeles" to "Pacific (Los Angeles)",
    "UTC" to "UTC",
)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(vm: AppViewModel, outer: PaddingValues) {
    Scaffold(
        modifier = Modifier.padding(outer),
        containerColor = Bg,
        topBar = {
            TopAppBar(
                navigationIcon = { IconButton(onClick = { vm.showSettings = false }) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
                title = { Text("Settings") },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = Bg),
            )
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding).verticalScroll(rememberScrollState())) {
            Text("THEME", style = MaterialTheme.typography.labelSmall, color = Muted,
                modifier = Modifier.padding(start = 20.dp, top = 8.dp, bottom = 4.dp))
            THEMES.forEach { (id, label) ->
                OptionRow(label, selected = vm.themeName == id) { vm.setTheme(id) }
            }
            HorizontalDivider(Modifier.padding(vertical = 12.dp), color = Surface1)
            Text("TIME ZONE", style = MaterialTheme.typography.labelSmall, color = Muted,
                modifier = Modifier.padding(start = 20.dp, bottom = 4.dp))
            Text(
                "How dates and reset times are written. Useful when the desktop lives in another zone.",
                style = MaterialTheme.typography.bodySmall, color = Muted,
                modifier = Modifier.padding(horizontal = 20.dp),
            )
            ZONES.forEach { (id, label) ->
                OptionRow(label, selected = vm.timeZone == id) { vm.setTz(id) }
            }
            HorizontalDivider(Modifier.padding(vertical = 12.dp), color = Surface1)
            Text("SECURITY", style = MaterialTheme.typography.labelSmall, color = Muted,
                modifier = Modifier.padding(start = 20.dp, bottom = 4.dp))
            Row(
                Modifier.fillMaxWidth().clickable { vm.setBiometricEnabled(!vm.biometric) }
                    .padding(horizontal = 20.dp, vertical = 6.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(Modifier.weight(1f)) {
                    Text("Require fingerprint")
                    Text("Lock the app on open and after 5 minutes away",
                        style = MaterialTheme.typography.bodySmall, color = Muted)
                }
                Switch(checked = vm.biometric, onCheckedChange = { vm.setBiometricEnabled(it) })
            }
            vm.desktop?.let { d ->
                HorizontalDivider(Modifier.padding(vertical = 12.dp), color = Surface1)
                Text("CONNECTION ORDER", style = MaterialTheme.typography.labelSmall, color = Muted,
                    modifier = Modifier.padding(start = 20.dp, bottom = 4.dp))
                Text(
                    "How ${d.name} is reached, first choice on top. Every road is tried at once; the highest one that answers wins.",
                    style = MaterialTheme.typography.bodySmall, color = Muted,
                    modifier = Modifier.padding(horizontal = 20.dp),
                )
                RoadOrder(d, onChange = { vm.setRoadOrder(d, it) })
                Text(
                    if (d.roadOrderCustom) "Your own order. ${d.name} publishes one too."
                    else "Following the order set on ${d.name}. Move a road to set your own.",
                    style = MaterialTheme.typography.bodySmall, color = Muted,
                    modifier = Modifier.padding(horizontal = 20.dp),
                )
                TextButton(enabled = d.roadOrderCustom, onClick = { vm.useDesktopRoadOrder(d) }, modifier = Modifier.padding(start = 8.dp)) {
                    Text("Use desktop's order")
                }
            }
            Text("DIAGNOSTICS", style = MaterialTheme.typography.labelSmall, color = Muted,
                modifier = Modifier.padding(start = 20.dp, top = 16.dp, bottom = 4.dp))
            Text(
                "The last few hundred things the app did — sessions opened, messages sent, where a transcript landed. Nothing you or an agent wrote. The same lines reach logcat under the tag \"aiterm\".",
                style = MaterialTheme.typography.bodySmall, color = Muted, modifier = Modifier.padding(horizontal = 20.dp),
            )
            val clipboard = androidx.compose.ui.platform.LocalClipboard.current
            val scope = androidx.compose.runtime.rememberCoroutineScope()
            Row(Modifier.padding(start = 8.dp)) {
                TextButton(onClick = {
                    scope.launch {
                        clipboard.setClipEntry(androidx.compose.ui.platform.ClipEntry(android.content.ClipData.newPlainText("aiterm diagnostics", com.fivelime.aiterm.Diag.dumpFile())))
                    }
                }) { Text("Copy log") }
                TextButton(onClick = { com.fivelime.aiterm.Diag.clear() }) { Text("Clear") }
            }
            Spacer(Modifier.height(16.dp))
        }
    }
}

/** The four roads in this desktop's order, each with an up and a down. A
 *  road the desktop cannot offer stays in the list — the order is the
 *  person's preference, and the desktop may grow into it. */
@Composable
private fun RoadOrder(d: Desktop, onChange: (List<String>) -> Unit) {
    val order = Roads.order(d.roadOrder)
    order.forEachIndexed { i, road ->
        Row(
            Modifier.fillMaxWidth().padding(start = 20.dp, end = 4.dp, top = 4.dp, bottom = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column(Modifier.weight(1f)) {
                Text(road.title)
                Text(road.blurb, style = MaterialTheme.typography.bodySmall, color = Muted)
                Text(
                    if (Roads.offers(d, road)) "Offered by this desktop" else "Not offered by this desktop yet",
                    style = MaterialTheme.typography.labelSmall, color = Muted,
                )
            }
            IconButton(enabled = i > 0, onClick = { onChange(swapped(order, i, i - 1)) }) {
                Icon(Icons.Filled.KeyboardArrowUp, "Move ${road.title} up")
            }
            IconButton(enabled = i < order.size - 1, onClick = { onChange(swapped(order, i, i + 1)) }) {
                Icon(Icons.Filled.KeyboardArrowDown, "Move ${road.title} down")
            }
        }
    }
}

private fun swapped(order: List<Road>, a: Int, b: Int): List<String> =
    order.toMutableList().apply { val t = this[a]; this[a] = this[b]; this[b] = t }.map { it.id }

@Composable
private fun OptionRow(label: String, selected: Boolean, onClick: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().clickable(onClick = onClick).padding(horizontal = 12.dp, vertical = 2.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        RadioButton(selected = selected, onClick = onClick)
        Spacer(Modifier.width(4.dp))
        Text(label)
    }
}
