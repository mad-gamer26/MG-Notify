package com.matthewdovi.mgnotify

import android.Manifest
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.content.IntentCompat
import kotlinx.coroutines.CompletableDeferred
import com.matthewdovi.mgnotify.ui.MGNotifyApp
import org.json.JSONObject

class MainActivity : ComponentActivity() {
    private val model get() = mgNotifyModel

    private val permissionLauncher = registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        pendingPermissionResult?.complete(granted)
        pendingPermissionResult = null
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            model.notificationPermissionRequester = {
                val result = CompletableDeferred<Boolean>()
                pendingPermissionResult = result
                permissionLauncher.launch(Manifest.permission.POST_NOTIFICATIONS)
                result.await()
            }
        }
        setContent { MGNotifyApp(model) }
        if (savedInstanceState == null) handleIntent(intent)
    }

    override fun onStart() {
        super.onStart()
        model.isInForeground = true
    }

    override fun onStop() {
        model.isInForeground = false
        super.onStop()
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleIntent(intent)
    }

    private fun handleIntent(intent: Intent) {
        // A tapped notification: ours carry the payload under EXTRA_PUSH_PAYLOAD,
        // the system's (app in the background) carry the push's data keys.
        val payloadText = intent.getStringExtra(EXTRA_PUSH_PAYLOAD) ?: intent.getStringExtra("mg_notify")
        if (payloadText != null) {
            val payload = runCatching { JSONObject(payloadText) }.getOrNull()
            model.launch { model.receiveNotification(payload, openedByUser = true) }
            intent.removeExtra(EXTRA_PUSH_PAYLOAD)
            intent.removeExtra("mg_notify")
            return
        }
        when (intent.action) {
            Intent.ACTION_VIEW -> {
                val uri = intent.data ?: return
                if (uri.scheme != "mgnotify") return
                when (uri.host) {
                    "add-sounds" -> model.pendingSoundsNavigation = SoundsNavigation.ADD_SOUNDS
                    "sounds" -> model.pendingSoundsNavigation = SoundsNavigation.SOUNDS
                }
            }
            Intent.ACTION_SEND -> {
                val uri = IntentCompat.getParcelableExtra(intent, Intent.EXTRA_STREAM, Uri::class.java) ?: return
                model.launch { model.stagePendingSoundImports(listOf(uri)) }
            }
            Intent.ACTION_SEND_MULTIPLE -> {
                val uris = IntentCompat.getParcelableArrayListExtra(intent, Intent.EXTRA_STREAM, Uri::class.java) ?: return
                model.launch { model.stagePendingSoundImports(uris) }
            }
        }
    }

    companion object {
        // Outlives the activity, so a prompt answered after a rotation still
        // reaches the coroutine waiting on it.
        private var pendingPermissionResult: CompletableDeferred<Boolean>? = null

        const val EXTRA_PUSH_PAYLOAD = "com.matthewdovi.mgnotify.PUSH_PAYLOAD"
    }
}
