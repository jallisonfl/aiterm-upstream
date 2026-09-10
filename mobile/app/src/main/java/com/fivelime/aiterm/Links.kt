package com.fivelime.aiterm

import java.net.URI
import java.nio.file.Paths

/** A link in the conversation that names a file on the desktop, or null
 *  when it does not. Absolute POSIX paths and `file:` URLs on localhost
 *  qualify; anything relative, remote, or carrying a query, fragment or
 *  control character does not. A trailing `:line` or `:line:col` comes
 *  off, and the result is normalised, so `..` cannot walk anywhere the
 *  path did not already name. A desktop path is never handed to Android's
 *  intent system — the desktop serves it, this app draws it. */
fun conversationFilePath(target: String): String? {
    if (target.any { it.isISOControl() }) return null
    val path = when {
        target.startsWith("/") && !target.startsWith("//") -> target
        target.startsWith("file:", ignoreCase = true) -> {
            val uri = runCatching { URI(target) }.getOrNull() ?: return null
            if (uri.isOpaque || uri.rawQuery != null || uri.rawFragment != null) return null
            val host = uri.rawAuthority
            if (!host.isNullOrEmpty() && !host.equals("localhost", ignoreCase = true)) return null
            uri.path ?: return null
        }
        else -> return null
    }.replace(Regex(":\\d+(?::\\d+)?$"), "")
    if (!path.startsWith("/") || path.startsWith("//") || path.any { it.isISOControl() }) return null
    return runCatching { Paths.get(path).normalize().toString() }.getOrNull()
}

/** A web link the system browser may open. */
fun isWebLink(target: String): Boolean =
    (target.startsWith("http://", ignoreCase = true) || target.startsWith("https://", ignoreCase = true)) &&
        target.none { it.isISOControl() || it.isWhitespace() }
