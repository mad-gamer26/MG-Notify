package com.matthewdovi.mgnotify

import android.Manifest
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage
import org.json.JSONObject
import kotlin.random.Random

/**
 * Receives FCM pushes. Pushes with a notification are shown by the system
 * while the app is in the background; this service sees them only in the
 * foreground, and shows them itself then (iOS shows a banner in the foreground
 * too). Event pushes (signed_out, devices_updated, ...) are data-only and
 * always arrive here.
 */
class PushService : FirebaseMessagingService() {
    override fun onNewToken(token: String) {
        Handler(Looper.getMainLooper()).post { mgNotifyModel.updateDeviceToken(token) }
    }

    override fun onMessageReceived(message: RemoteMessage) {
        IncomingPush.handle(
            this,
            title = message.notification?.title ?: message.data["title"],
            body = message.notification?.body ?: message.data["message"],
            payloadText = message.data["mg_notify"],
        )
    }
}

/** What happens to a push that reaches the app, wherever it came from. */
object IncomingPush {
    fun handle(context: Context, title: String?, body: String?, payloadText: String?) {
        val payload = payloadText?.let { runCatching { JSONObject(it) }.getOrNull() }
        val isEvent = payload?.optString("event").orEmpty().isNotEmpty()
        if (!isEvent && (title != null || body != null)) {
            val model = context.mgNotifyModel
            if (model.isInForeground) {
                // The app is open: speak it and play its sound instead of showing it.
                Handler(Looper.getMainLooper()).post { model.announcePush(payload, title.orEmpty(), body.orEmpty()) }
            } else {
                show(context, title.orEmpty(), body.orEmpty(), payload)
            }
        }
        Handler(Looper.getMainLooper()).post {
            val model = context.mgNotifyModel
            model.launch { model.receiveNotification(payload) }
        }
    }

    private fun show(context: Context, title: String, body: String, payload: JSONObject?) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            return
        }
        // The person's current sound choice decides, whatever channel the server named.
        val selectedSound = context.mgNotifyModel.selectedSound
        NotificationSounds.ensureChannels(context, selectedSound)
        val intent = Intent(context, MainActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
            .apply { payload?.let { putExtra(MainActivity.EXTRA_PUSH_PAYLOAD, it.toString()) } }
        val requestCode = Random.nextInt()
        val contentIntent = PendingIntent.getActivity(
            context,
            requestCode,
            intent,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val notification = NotificationCompat.Builder(context, NotificationSounds.channelIdForSound(selectedSound))
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(title)
            .setContentText(body)
            .setStyle(NotificationCompat.BigTextStyle().bigText(body))
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setAutoCancel(true)
            .setContentIntent(contentIntent)
            .build()
        NotificationManagerCompat.from(context).notify(requestCode, notification)
    }
}
