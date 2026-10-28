package com.matthewdovi.mgnotify

import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.media.AudioAttributes
import android.media.MediaMetadataRetriever
import android.net.Uri
import android.provider.Settings
import androidx.core.content.FileProvider
import java.io.File
import java.security.MessageDigest

/**
 * Notification sounds on Android live in notification channels, and a
 * channel's sound can't change once it exists. So each sound choice gets its
 * own channel, named after the sound; the server picks the channel the same
 * way (see android_channel_id_for_sound in server/src/push.rs), so pushes shown while
 * the app isn't running play the chosen sound too.
 */
object NotificationSounds {
    const val NO_SOUND = "__none__"
    const val SYSTEM_DEFAULT = "default"

    /** The channel for the system's notification sound; servers that don't pick a channel use it. */
    const val DEFAULT_CHANNEL_ID = "mg_notify"
    private const val SILENT_CHANNEL_ID = "mg_notify_silent"
    private const val SOUND_CHANNEL_PREFIX = "mg_notify_sound_"

    /** What Android's notification channels can play. */
    val supportedExtensions = setOf("wav", "mp3", "ogg", "oga", "opus", "m4a", "aac", "flac", "mid", "midi")

    const val MAX_DURATION_MS = 30_000L

    fun channelIdForSound(sound: String): String = when (sound) {
        SYSTEM_DEFAULT, "" -> DEFAULT_CHANNEL_ID
        NO_SOUND -> SILENT_CHANNEL_ID
        else -> SOUND_CHANNEL_PREFIX + MessageDigest.getInstance("SHA-256")
            .digest(sound.toByteArray(Charsets.UTF_8))
            .take(8)
            .joinToString("") { "%02x".format(it) }
    }

    fun soundsDirectory(context: Context): File =
        File(context.filesDir, "Sounds").apply { mkdirs() }

    fun pendingImportsDirectory(context: Context): File =
        File(context.filesDir, "PendingSoundImports").apply { mkdirs() }

    fun soundUri(context: Context, name: String): Uri =
        FileProvider.getUriForFile(context, "${context.packageName}.files", File(soundsDirectory(context), name))

    fun displayName(fileName: String): String = fileName.substringBeforeLast('.', fileName)

    /** The length of an audio file in milliseconds, or null when it can't be read as audio. */
    fun durationMs(file: File): Long? {
        val retriever = MediaMetadataRetriever()
        return try {
            retriever.setDataSource(file.absolutePath)
            retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_DURATION)?.toLongOrNull()
        } catch (_: Exception) {
            null
        } finally {
            retriever.release()
        }
    }

    fun isImportable(file: File): Boolean {
        if (file.extension.lowercase() !in supportedExtensions) return false
        val duration = durationMs(file) ?: return false
        return duration <= MAX_DURATION_MS
    }

    /**
     * Makes sure the channel for [selectedSound] exists (and the default one,
     * which every server falls back to), and removes channels for sounds that
     * are no longer chosen.
     */
    fun ensureChannels(context: Context, selectedSound: String) {
        val manager = context.getSystemService(NotificationManager::class.java)
        val attributes = AudioAttributes.Builder()
            .setUsage(AudioAttributes.USAGE_NOTIFICATION)
            .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
            .build()

        manager.createNotificationChannel(
            NotificationChannel(DEFAULT_CHANNEL_ID, "Notifications", NotificationManager.IMPORTANCE_HIGH).apply {
                setSound(Settings.System.DEFAULT_NOTIFICATION_URI, attributes)
            },
        )

        val selectedChannelId = channelIdForSound(selectedSound)
        when (selectedSound) {
            SYSTEM_DEFAULT -> Unit
            NO_SOUND -> manager.createNotificationChannel(
                NotificationChannel(SILENT_CHANNEL_ID, "Notifications (No Sound)", NotificationManager.IMPORTANCE_HIGH).apply {
                    setSound(null, null)
                },
            )
            else -> {
                val uri = soundUri(context, selectedSound)
                // The system plays channel sounds itself, so it needs to read the file.
                context.grantUriPermission("com.android.systemui", uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
                manager.createNotificationChannel(
                    NotificationChannel(
                        selectedChannelId,
                        "Notifications (${displayName(selectedSound)})",
                        NotificationManager.IMPORTANCE_HIGH,
                    ).apply { setSound(uri, attributes) },
                )
            }
        }

        manager.notificationChannels
            .map { it.id }
            .filter { it != DEFAULT_CHANNEL_ID && it != selectedChannelId && it.startsWith(DEFAULT_CHANNEL_ID) }
            .forEach { manager.deleteNotificationChannel(it) }
    }
}
