package com.matthewdovi.mgnotify

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/**
 * Debug builds only: feeds a pretend push through the same handling as a
 * real one, for testing without Firebase. For example:
 *   adb shell am broadcast -n com.matthewdovi.mgnotify/.DebugPushReceiver \
 *     --es title "Hello" --es message "World" --es mg_notify '{"account_id":"..."}'
 */
class DebugPushReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        IncomingPush.handle(
            context,
            title = intent.getStringExtra("title"),
            body = intent.getStringExtra("message"),
            payloadText = intent.getStringExtra("mg_notify"),
        )
    }
}
