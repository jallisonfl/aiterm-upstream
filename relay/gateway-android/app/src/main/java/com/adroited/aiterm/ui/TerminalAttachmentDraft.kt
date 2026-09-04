package com.adroited.aiterm.ui

import com.adroited.aiterm.remote.RemoteUploadSource
import java.io.File
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

enum class TerminalAttachmentUploadState {
    Pending,
    Uploading,
    Failed,
}

/**
 * Immutable normalized-image metadata kept by a terminal draft.
 *
 * SHA-256 data never leaves this model by reference. Callers that need it for a remote upload
 * receive a fresh byte array, so a UI state transition cannot accidentally alter validation data.
 */
class TerminalAttachmentImage private constructor(
    val id: String,
    val file: File,
    val width: Int,
    val height: Int,
    val length: Long,
    private val digest: ByteArray,
) {
    val sha256: ByteArray
        get() = digest.copyOf()

    fun asRemoteUploadSource(): RemoteUploadSource = RemoteUploadSource(
        id = id,
        file = file,
        length = length,
        sha256 = digest.copyOf(),
    )

    override fun equals(other: Any?): Boolean = other is TerminalAttachmentImage &&
        id == other.id &&
        file == other.file &&
        width == other.width &&
        height == other.height &&
        length == other.length &&
        digest.contentEquals(other.digest)

    override fun hashCode(): Int {
        var result = id.hashCode()
        result = 31 * result + file.hashCode()
        result = 31 * result + width
        result = 31 * result + height
        result = 31 * result + length.hashCode()
        result = 31 * result + digest.contentHashCode()
        return result
    }

    companion object {
        fun from(normalized: NormalizedTerminalImage): TerminalAttachmentImage =
            TerminalAttachmentImage(
                id = normalized.id,
                file = normalized.file,
                width = normalized.width,
                height = normalized.height,
                length = normalized.length,
                digest = normalized.sha256.copyOf(),
            )
    }
}

data class TerminalAttachmentItem(
    val image: TerminalAttachmentImage,
    val sentBytes: Long = 0,
    val state: TerminalAttachmentUploadState = TerminalAttachmentUploadState.Pending,
    val message: String? = null,
)

/** A pure draft transition. Its removed items are explicitly owned by the UI for local deletion. */
data class TerminalAttachmentDraftUpdate(
    val draft: TerminalAttachmentDraft,
    val removed: List<TerminalAttachmentItem> = emptyList(),
    val accepted: Boolean = true,
)

data class TerminalAttachmentDraft(
    val items: List<TerminalAttachmentItem> = emptyList(),
    val preparing: Boolean = false,
    val submitting: Boolean = false,
    val message: String? = null,
) {
    val totalBytes: Long
        get() = items.sumOf { it.image.length }

    fun add(normalized: NormalizedTerminalImage): TerminalAttachmentDraftUpdate {
        if (submitting) return rejected("Images are uploading.")
        val image = TerminalAttachmentImage.from(normalized)
        if (image.id.isBlank() || image.sha256.size != SHA256_BYTES) {
            return rejected("The selected image is invalid.")
        }
        if (image.length !in 1..MAX_IMAGE_BYTES) return rejected("Each image must be 12 MiB or smaller.")
        if (items.any { it.image.id == image.id || it.image.sha256.contentEquals(image.sha256) }) {
            return rejected("This image is already attached.")
        }
        if (items.size >= MAX_IMAGES) return rejected("You can attach up to 4 images.")
        if (image.length > MAX_TOTAL_BYTES - totalBytes) {
            return rejected("Selected images exceed the 48 MiB limit.")
        }
        return TerminalAttachmentDraftUpdate(copy(items = items + TerminalAttachmentItem(image), message = null))
    }

    fun remove(imageId: String): TerminalAttachmentDraftUpdate {
        if (preparing) return rejected("Wait for the selected image to finish preparing.")
        if (submitting) return rejected("Images are uploading.")
        val item = items.firstOrNull { it.image.id == imageId } ?: return TerminalAttachmentDraftUpdate(this)
        return TerminalAttachmentDraftUpdate(
            draft = copy(items = items.filterNot { it.image.id == imageId }, message = null),
            removed = listOf(item),
        )
    }

    fun beginSubmission(): TerminalAttachmentDraftUpdate {
        if (preparing) return rejected("Wait for the selected image to finish preparing.")
        if (submitting) return rejected("Images are already uploading.")
        if (items.isEmpty()) return rejected("Choose an image before uploading.")
        return TerminalAttachmentDraftUpdate(
            copy(
                items = items.map { it.copy(sentBytes = 0, state = TerminalAttachmentUploadState.Uploading, message = null) },
                submitting = true,
                message = null,
            ),
        )
    }

    fun recordProgress(sourceId: String, sentBytes: Long, totalBytes: Long): TerminalAttachmentDraftUpdate {
        if (!submitting) return TerminalAttachmentDraftUpdate(this)
        val index = items.indexOfFirst { it.image.id == sourceId }
        if (index < 0) return TerminalAttachmentDraftUpdate(this)
        val current = items[index]
        if (totalBytes != current.image.length) return TerminalAttachmentDraftUpdate(this)
        val nextSent = sentBytes.coerceIn(0, current.image.length).coerceAtLeast(current.sentBytes)
        if (nextSent == current.sentBytes) return TerminalAttachmentDraftUpdate(this)
        return TerminalAttachmentDraftUpdate(
            copy(items = items.toMutableList().also { updated -> updated[index] = current.copy(sentBytes = nextSent) }),
        )
    }

    fun failSubmission(sourceId: String, failureMessage: String): TerminalAttachmentDraftUpdate {
        if (!submitting) return TerminalAttachmentDraftUpdate(this)
        return TerminalAttachmentDraftUpdate(
            copy(
                items = items.map { item ->
                    when (item.image.id) {
                        sourceId -> item.copy(state = TerminalAttachmentUploadState.Failed, message = failureMessage)
                        else -> item.copy(state = TerminalAttachmentUploadState.Pending)
                    }
                },
                submitting = false,
                message = failureMessage,
            ),
        )
    }

    /** Handles a submission-level error such as focus loss or an older desktop protocol. */
    fun failSubmission(failureMessage: String): TerminalAttachmentDraftUpdate {
        if (!submitting) return TerminalAttachmentDraftUpdate(this)
        return TerminalAttachmentDraftUpdate(
            copy(
                items = items.map { it.copy(state = TerminalAttachmentUploadState.Pending) },
                submitting = false,
                message = failureMessage,
            ),
        )
    }

    fun retrySubmission(): TerminalAttachmentDraftUpdate = TerminalAttachmentDraftUpdate(
        copy(
            items = items.map {
                it.copy(sentBytes = 0, state = TerminalAttachmentUploadState.Pending, message = null)
            },
            submitting = false,
            message = null,
        ),
    )

    fun beginPreparation(): TerminalAttachmentDraftUpdate {
        if (preparing) return rejected("An image is already being prepared.")
        if (submitting) return rejected("Images are uploading.")
        if (items.size >= MAX_IMAGES) return rejected("You can attach up to 4 images.")
        return TerminalAttachmentDraftUpdate(copy(preparing = true, message = null))
    }

    fun finishPreparation(): TerminalAttachmentDraftUpdate = TerminalAttachmentDraftUpdate(
        copy(preparing = false),
    )

    /** Clears state only after caller has locally accepted the complete terminal submission. */
    fun completeSubmission(): TerminalAttachmentDraftUpdate {
        if (!submitting) return rejected("No image upload is in progress.")
        return TerminalAttachmentDraftUpdate(draft = TerminalAttachmentDraft(), removed = items)
    }

    /** Lets the UI discard a non-uploading draft and delete the returned private files. */
    fun discard(): TerminalAttachmentDraftUpdate {
        if (preparing) return rejected("Wait for the selected image to finish preparing.")
        if (submitting) return rejected("Images are uploading.")
        return TerminalAttachmentDraftUpdate(draft = TerminalAttachmentDraft(), removed = items)
    }

    private fun rejected(reason: String): TerminalAttachmentDraftUpdate =
        TerminalAttachmentDraftUpdate(copy(message = reason), accepted = false)

    companion object {
        const val MAX_IMAGES = 4
        const val MAX_IMAGE_BYTES = 12L * 1024L * 1024L
        const val MAX_TOTAL_BYTES = 48L * 1024L * 1024L
        private const val SHA256_BYTES = 32
    }
}

internal data class TerminalTabDraft(
    val composer: TerminalComposerState = TerminalComposerState(),
    val attachments: TerminalAttachmentDraft = TerminalAttachmentDraft(),
)

/**
 * Per-authoritative-tab drafts. Every map update is compare-and-set, so concurrent progress for
 * tab A cannot overwrite text or attachments changed for tab B.
 */
internal class TerminalDraftStore {
    private val mutableDrafts = MutableStateFlow<Map<String, TerminalTabDraft>>(emptyMap())
    val drafts: StateFlow<Map<String, TerminalTabDraft>> = mutableDrafts.asStateFlow()

    fun draftFor(tabId: String): TerminalTabDraft = mutableDrafts.value[requireTabId(tabId)] ?: TerminalTabDraft()

    fun updateComposer(
        tabId: String,
        transform: (TerminalComposerState) -> TerminalComposerState,
    ): TerminalTabDraft = update(tabId) { it.copy(composer = transform(it.composer)) }

    fun updateAttachments(
        tabId: String,
        transform: (TerminalAttachmentDraft) -> TerminalAttachmentDraft,
    ): TerminalTabDraft = update(tabId) { it.copy(attachments = transform(it.attachments)) }

    /**
     * Applies an attachment transition and returns its removed files from the same CAS update.
     * The caller can then delete only those private files without a racy read-then-update.
     */
    fun transitionAttachments(
        tabId: String,
        transform: (TerminalAttachmentDraft) -> TerminalAttachmentDraftUpdate,
    ): TerminalAttachmentDraftUpdate {
        val id = requireTabId(tabId)
        while (true) {
            val current = mutableDrafts.value
            val previous = current[id] ?: TerminalTabDraft()
            val transition = transform(previous.attachments)
            val next = current + (id to previous.copy(attachments = transition.draft))
            if (mutableDrafts.compareAndSet(current, next)) return transition
        }
    }

    /** Clears composer and attachments in one CAS only after local terminal-input acceptance. */
    fun completeSubmission(tabId: String): TerminalAttachmentDraftUpdate {
        val id = requireTabId(tabId)
        while (true) {
            val current = mutableDrafts.value
            val previous = current[id] ?: return TerminalAttachmentDraftUpdate(
                TerminalAttachmentDraft(message = "No terminal draft is available."),
                accepted = false,
            )
            val transition = previous.attachments.completeSubmission()
            if (!transition.accepted) return transition
            val next = current + (id to TerminalTabDraft())
            if (mutableDrafts.compareAndSet(current, next)) return transition
        }
    }

    fun clear(tabId: String): TerminalTabDraft? {
        val id = requireTabId(tabId)
        while (true) {
            val current = mutableDrafts.value
            val removed = current[id] ?: return null
            if (mutableDrafts.compareAndSet(current, current - id)) return removed
        }
    }

    fun hasDrafts(): Boolean = mutableDrafts.value.values.any { draft ->
        draft.composer.value.text.isNotEmpty() || draft.attachments.items.isNotEmpty() ||
            draft.attachments.preparing || draft.attachments.submitting
    }

    /** Atomically clears every non-uploading draft and returns its app-private image ownership. */
    fun discardAll(): List<TerminalAttachmentItem> {
        while (true) {
            val current = mutableDrafts.value
            if (current.values.any { it.attachments.submitting || it.attachments.preparing }) return emptyList()
            if (mutableDrafts.compareAndSet(current, emptyMap())) {
                return current.values.flatMap { it.attachments.items }
            }
        }
    }

    private fun update(tabId: String, transform: (TerminalTabDraft) -> TerminalTabDraft): TerminalTabDraft {
        val id = requireTabId(tabId)
        while (true) {
            val current = mutableDrafts.value
            val nextDraft = transform(current[id] ?: TerminalTabDraft())
            val next = current + (id to nextDraft)
            if (mutableDrafts.compareAndSet(current, next)) return nextDraft
        }
    }

    private fun requireTabId(tabId: String): String = tabId.also { require(it.isNotBlank()) }
}
