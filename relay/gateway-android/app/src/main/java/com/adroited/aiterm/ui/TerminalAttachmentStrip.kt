package com.adroited.aiterm.ui

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** Compact terminal-filmstrip treatment; previews are sampled near their 64 dp display size. */
@Composable
internal fun TerminalAttachmentStrip(
    draft: TerminalAttachmentDraft,
    onRemove: (String) -> Unit,
) {
    if (draft.items.isEmpty() && draft.message == null) return
    Column(
        Modifier.fillMaxWidth()
            .background(Color(0xFF09131F))
            .testTag("terminal-attachments"),
    ) {
        if (draft.items.isNotEmpty()) {
            Row(
                Modifier.fillMaxWidth()
                    .horizontalScroll(rememberScrollState())
                    .padding(horizontal = 4.dp, vertical = 4.dp),
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                draft.items.forEach { item ->
                    TerminalAttachmentTile(item, !draft.preparing && !draft.submitting) {
                        onRemove(item.image.id)
                    }
                }
            }
        }
        draft.message?.let { message ->
            Text(
                message,
                modifier = Modifier.padding(horizontal = 8.dp, vertical = 2.dp),
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.labelSmall,
                fontFamily = androidx.compose.ui.text.font.FontFamily.Monospace,
            )
        }
    }
}

@Composable
private fun TerminalAttachmentTile(
    item: TerminalAttachmentItem,
    removalEnabled: Boolean,
    onRemove: () -> Unit,
) {
    val preview by produceState<Bitmap?>(null, item.image.file, item.image.length) {
        value = withContext(Dispatchers.IO) { decodePreview(item.image.file.path) }
    }
    // Compose can retain this bitmap in a recorded graphics layer for a frame
    // after the tile leaves composition. Recycling it here races that draw and
    // crashes with "Canvas: trying to use a recycled bitmap". These previews
    // are sampled to at most 192 px, so let Android reclaim them normally.
    val borderColor = when (item.state) {
        TerminalAttachmentUploadState.Failed -> MaterialTheme.colorScheme.error
        TerminalAttachmentUploadState.Uploading -> Color(0xFF63D3E1)
        TerminalAttachmentUploadState.Pending -> Color(0xFF315269)
    }
    Box(
        Modifier.size(72.dp)
            .border(1.dp, borderColor, MaterialTheme.shapes.small)
            .background(Color(0xFF07111B), MaterialTheme.shapes.small)
            .testTag("terminal-image-${item.image.id}"),
    ) {
        preview?.let { bitmap ->
            Image(
                bitmap.asImageBitmap(),
                contentDescription = "Attached image",
                contentScale = ContentScale.Crop,
                modifier = Modifier.align(Alignment.Center)
                    .size(64.dp)
                    .clip(MaterialTheme.shapes.extraSmall),
            )
        } ?: Text(
            "IMG",
            modifier = Modifier.align(Alignment.Center),
            color = Color(0xFF75D8B4),
            style = MaterialTheme.typography.labelSmall,
        )
        TextButton(
            onClick = onRemove,
            enabled = removalEnabled,
            modifier = Modifier.align(Alignment.TopEnd)
                .size(48.dp)
                .semantics { contentDescription = "Remove attached image" }
                .testTag("terminal-image-remove-${item.image.id}"),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(0.dp),
        ) {
            Text("×", color = Color.White)
        }
        if (item.state == TerminalAttachmentUploadState.Uploading) {
            val progress = if (item.image.length > 0) {
                item.sentBytes.toFloat() / item.image.length.toFloat()
            } else 0f
            CircularProgressIndicator(
                progress = { progress.coerceIn(0f, 1f) },
                modifier = Modifier.align(Alignment.BottomStart)
                    .padding(4.dp)
                    .size(20.dp)
                    .testTag("terminal-image-progress-${item.image.id}"),
                color = Color(0xFF63D3E1),
                trackColor = Color(0x66315269),
                strokeWidth = 2.dp,
            )
        }
    }
}

private fun decodePreview(path: String): Bitmap? {
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeFile(path, bounds)
    if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return null
    var sample = 1
    while (bounds.outWidth / sample > PREVIEW_DECODE_EDGE ||
        bounds.outHeight / sample > PREVIEW_DECODE_EDGE
    ) {
        sample *= 2
    }
    return BitmapFactory.decodeFile(
        path,
        BitmapFactory.Options().apply {
            inSampleSize = sample
            inPreferredConfig = Bitmap.Config.RGB_565
        },
    )
}

private const val PREVIEW_DECODE_EDGE = 192
