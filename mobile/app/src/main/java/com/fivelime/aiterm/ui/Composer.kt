package com.fivelime.aiterm.ui

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.AttachFile
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.fivelime.aiterm.AppViewModel
import com.fivelime.aiterm.Attachment
import kotlinx.coroutines.launch

/** The `+` in a composer: pick any file or image; it uploads to the desktop
 *  and appears as a chip until sent. */
@Composable
fun AttachButton(vm: AppViewModel) {
    val pick = rememberLauncherForActivityResult(ActivityResultContracts.GetContent()) { uri -> uri?.let(vm::attach) }
    // The window this app draws, for "attach what I am looking at": a
    // screenshot of the conversation or the terminal, sent like any file.
    val root = androidx.compose.ui.platform.LocalView.current.rootView
    val scope = androidx.compose.runtime.rememberCoroutineScope()
    var menu by androidx.compose.runtime.remember { androidx.compose.runtime.mutableStateOf(false) }
    if (vm.uploading) {
        CircularProgressIndicator(Modifier.padding(12.dp).size(22.dp), strokeWidth = 2.dp)
        return
    }
    androidx.compose.foundation.layout.Box {
        IconButton(onClick = { menu = true }) { Icon(Icons.Filled.Add, "Attach a file, image or screenshot", tint = Muted) }
        androidx.compose.material3.DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
            androidx.compose.material3.DropdownMenuItem(
                text = { Text("File or image") },
                onClick = { menu = false; pick.launch("*/*") },
            )
            androidx.compose.material3.DropdownMenuItem(
                text = { Text("Screenshot of this screen") },
                onClick = {
                    menu = false
                    scope.launch {
                        // Two frames, so the menu is gone before the window is drawn.
                        androidx.compose.runtime.withFrameNanos { }
                        androidx.compose.runtime.withFrameNanos { }
                        val bytes = captureView(root)
                        if (bytes == null) vm.notice = "Could not capture this screen."
                        else vm.attachBytes("screenshot-" + System.currentTimeMillis() + ".jpg", bytes)
                    }
                },
            )
        }
    }
}

/** The view drawn into a JPEG, on the main thread; null when it has no size yet. */
private fun captureView(view: android.view.View): ByteArray? {
    if (!view.isAttachedToWindow || view.width <= 0 || view.height <= 0) return null
    val bmp = android.graphics.Bitmap.createBitmap(view.width, view.height, android.graphics.Bitmap.Config.ARGB_8888)
    return try {
        view.draw(android.graphics.Canvas(bmp))
        java.io.ByteArrayOutputStream().use { out ->
            if (!bmp.compress(android.graphics.Bitmap.CompressFormat.JPEG, 92, out)) return null
            out.toByteArray()
        }
    } catch (_: Exception) { null } finally { bmp.recycle() }
}

@Composable
fun AttachmentChips(vm: AppViewModel) {
    if (vm.attachments.isEmpty()) return
    Row(Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 12.dp, vertical = 4.dp)) {
        vm.attachments.forEach { a ->
            Row(
                Modifier.padding(end = 6.dp).background(Surface2, RoundedCornerShape(12.dp)).padding(start = 10.dp, end = 4.dp, top = 4.dp, bottom = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Icon(Icons.Filled.AttachFile, null, tint = Accent, modifier = Modifier.size(14.dp))
                Spacer(Modifier.width(4.dp))
                Text(a.name, style = MaterialTheme.typography.labelMedium, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.width(140.dp))
                IconButton(onClick = { vm.removeAttachment(a) }, modifier = Modifier.size(24.dp)) { Icon(Icons.Filled.Close, "Remove", tint = Muted, modifier = Modifier.size(14.dp)) }
            }
        }
    }
}

/** A pill that opens a menu — the model / effort / harness pickers. */
/** The look of a choice: a pill with an optional mark, the label and a
 *  chevron. `PickerChip` opens a menu from it; a caller with its own picker
 *  (a searchable sheet) uses it bare. */
@Composable
fun ChipButton(label: String, onClick: () -> Unit, leading: (@Composable () -> Unit)? = null) {
    Row(
        Modifier.background(Surface2, RoundedCornerShape(16.dp)).clickable(onClick = onClick).padding(horizontal = 10.dp, vertical = 7.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        leading?.let { it(); Spacer(Modifier.width(6.dp)) }
        Text(
            label, style = MaterialTheme.typography.labelLarge, color = Color(0xFFE6EAF2),
            maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.widthIn(max = 220.dp),
        )
        Icon(Icons.Filled.KeyboardArrowDown, null, tint = Muted, modifier = Modifier.size(16.dp))
    }
}

/** A chip that opens a menu of `options` (id to name). `leading` draws before
 *  the label; `icon` draws a mark beside each row of the menu, by id. */
@Composable
fun PickerChip(
    label: String,
    options: List<Pair<String, String>>,
    onPick: (String) -> Unit,
    leading: (@Composable () -> Unit)? = null,
    icon: (@Composable (String) -> Unit)? = null,
) {
    var open by remember { mutableStateOf(false) }
    ChipButton(label, onClick = { open = true }, leading = leading)
    DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
        options.forEach { (id, name) ->
            DropdownMenuItem(
                text = { Text(name) },
                leadingIcon = icon?.let { { it(id) } },
                onClick = { open = false; onPick(id) },
            )
        }
    }
}
