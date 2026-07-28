package com.sonus.cosmog

import android.content.Intent
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.provider.DocumentsContract

/**
 * ACTION_OPEN_DOCUMENT_TREE bridge driven by a polling protocol. The Rust side
 * (nw_pick_tree) calls launch() then poll() every ~250ms until state is
 * terminal, then reset().
 *
 * States: 0 idle, 1 pending, 2 done (result set), 3 canceled/error.
 * On success result = "<treeUri>\n<displayName>".
 *
 * The actual system picker requires an Activity + a launcher registered before
 * RESUMED. MainActivity registers the launcher in onCreate and hands its own
 * reference plus a launch callback to this object.
 */
object NwTreePicker {
    @Volatile
    var state: Int = 0

    @Volatile
    var result: String? = null

    // Set by MainActivity.onCreate, cleared in onDestroy. The launcher must be
    // registered before RESUMED, so we cannot register it lazily here.
    @Volatile
    var activity: MainActivity? = null

    private val mainHandler = Handler(Looper.getMainLooper())

    // Kicked off from a JNI thread. Marks pending, then hops to the UI thread to
    // fire the pre-registered launcher on the current activity.
    @JvmStatic
    fun launch() {
        state = 1
        result = null
        mainHandler.post {
            val act = activity
            if (act == null) {
                // No activity to host the picker: fail rather than hang.
                result = null
                state = 3
                return@post
            }
            try {
                act.launchNwTreePicker()
            } catch (t: Throwable) {
                android.util.Log.w("NwTreePicker", "launch failed: $t")
                result = null
                state = 3
            }
        }
    }

    // Called on the UI thread from MainActivity's ActivityResult callback.
    fun onResult(uri: Uri?) {
        if (uri == null) {
            result = null
            state = 3
            return
        }
        val act = activity
        try {
            act?.contentResolver?.takePersistableUriPermission(
                uri,
                Intent.FLAG_GRANT_READ_URI_PERMISSION,
            )
        } catch (t: Throwable) {
            android.util.Log.w("NwTreePicker", "persist grant failed: $t")
        }
        val name = resolveTreeDisplayName(act, uri) ?: ""
        result = "$uri\n$name"
        state = 2
    }

    // Resolve the human display name of a tree uri via DocumentsContract (a
    // framework API, no extra dependency). Mirrors the Rust query_display_name
    // cursor pattern: build the tree document uri, query _display_name.
    private fun resolveTreeDisplayName(act: MainActivity?, treeUri: Uri): String? {
        if (act == null) return null
        return try {
            val docId = DocumentsContract.getTreeDocumentId(treeUri)
            val docUri = DocumentsContract.buildDocumentUriUsingTree(treeUri, docId)
            act.contentResolver.query(
                docUri,
                arrayOf(DocumentsContract.Document.COLUMN_DISPLAY_NAME),
                null,
                null,
                null,
            )?.use { c ->
                if (c.moveToFirst() && !c.isNull(0)) c.getString(0) else null
            }
        } catch (t: Throwable) {
            android.util.Log.w("NwTreePicker", "display name query failed: $t")
            null
        }
    }

    // Called from a JNI thread; touches only volatiles. On cancel/error
    // (state 3) return the NUL-prefixed sentinel the Rust side maps to a
    // "canceled" result, so it does not hang until the poll timeout.
    @JvmStatic
    fun poll(): String? = if (state == 3) "__NW_CANCELED__" else result

    @JvmStatic
    fun reset() {
        state = 0
        result = null
    }
}
