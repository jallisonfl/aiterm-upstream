package com.fivelime.aiterm

/** What the app did, kept where it can be read: every line goes to logcat
 *  under one tag — `adb logcat -s aiterm` — and into a ring the Settings
 *  screen can copy out. Records what the app did — a session opened, a
 *  message sent, where the transcript landed — never what anyone wrote. */
object Diag {
    const val TAG = "aiterm"
    private const val KEEP = 600
    /** The file is what survives a reboot and an evening away from the
     *  desk: `files/diag.log`, rolled to `diag.1.log` past half a megabyte.
     *  `adb shell run-as com.fivelime.aiterm cat files/diag.log`. */
    private const val FILE_MAX = 512 * 1024
    private val ring = ArrayDeque<String>()
    private var file: java.io.File? = null

    /** Start writing to disk. Until this is called lines live in the ring only. */
    @Synchronized
    fun attach(context: android.content.Context) {
        if (file != null) return
        val f = java.io.File(context.filesDir, "diag.log")
        file = f
        val boot = "${stamp()} [diag] app up — ${android.os.Build.MANUFACTURER} ${android.os.Build.MODEL} Android ${android.os.Build.VERSION.RELEASE}"
        ring.addLast(boot); append(f, boot)
    }

    @Synchronized
    fun log(area: String, msg: String) {
        android.util.Log.d(TAG, "[$area] $msg")
        val line = "${stamp()} [$area] $msg"
        ring.addLast(line)
        while (ring.size > KEEP) ring.removeFirst()
        file?.let { append(it, line) }
    }

    private fun append(f: java.io.File, line: String) {
        runCatching {
            if (f.length() > FILE_MAX) {
                val old = java.io.File(f.parentFile, "diag.1.log")
                old.delete(); f.renameTo(old)
            }
            f.appendText(line + "\n")
        }
    }

    /** The file's tail plus the ring — for the Settings copy-out. */
    @Synchronized
    fun dumpFile(): String = file?.takeIf { it.isFile }?.let { f ->
        runCatching { f.readText().takeLast(64 * 1024) }.getOrNull()
    } ?: dump()

    @Synchronized
    fun dump(): String = ring.joinToString("\n")

    @Synchronized
    fun clear() = ring.clear()

    private fun stamp(): String =
        java.text.SimpleDateFormat("MM-dd HH:mm:ss.SSS", java.util.Locale.US).format(java.util.Date())
}
