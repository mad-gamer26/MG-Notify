package com.matthewdovi.mgnotify

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller
import android.net.Uri
import android.os.Build
import android.provider.Settings
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.core.content.IntentCompat
import androidx.core.content.edit
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.io.File
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.security.MessageDigest

/** A release described by the website's android.json. */
data class AppRelease(
    val versionName: String,
    val versionCode: Long,
    val url: String,
    val sha256: String,
    val size: Long,
    val notes: String,
)

/**
 * Keeps the app up to date from notify.mad-gamer.com: checks
 * /downloads/android.json (by itself at most every 12 hours, or when asked),
 * downloads a newer release, checks its SHA-256, and hands it to Android's
 * installer, which asks the person to confirm.
 */
class Updater(private val context: Context, private val scope: CoroutineScope) {
    sealed interface State {
        data object Idle : State
        data object Checking : State
        data object UpToDate : State
        data class Available(val release: AppRelease) : State
        /** Android needs the person to allow MG Notify to install apps first. */
        data class NeedsPermission(val release: AppRelease) : State
        data class Downloading(val release: AppRelease, val fraction: Float) : State
        data class Installing(val release: AppRelease) : State
        data class Failed(val message: String) : State
    }

    private val prefs = context.getSharedPreferences("mg_notify", Context.MODE_PRIVATE)

    var state by mutableStateOf<State>(State.Idle)
        private set

    /** Shown as a dialog when an automatic check finds an update (a manual check shows its result in About). */
    var showPrompt by mutableStateOf(false)

    var automaticChecks: Boolean
        get() = prefs.getBoolean("automaticUpdateChecks", true)
        set(value) = prefs.edit { putBoolean("automaticUpdateChecks", value) }

    private var job: Job? = null

    private val installedVersionCode: Long
        get() {
            val info = context.packageManager.getPackageInfo(context.packageName, 0)
            return if (Build.VERSION.SDK_INT >= 28) info.longVersionCode else @Suppress("DEPRECATION") info.versionCode.toLong()
        }

    /** At app start: checks if automatic checks are on and the last one was 12 hours ago or more. */
    fun checkAutomaticallyIfDue() {
        if (!automaticChecks || BuildConfig.UPDATES_URL.isEmpty()) return
        val last = prefs.getLong("lastUpdateCheck", 0)
        if (System.currentTimeMillis() - last < 12 * 3600 * 1000L) return
        check(prompt = true)
    }

    fun check(prompt: Boolean = false) {
        if (job?.isActive == true || BuildConfig.UPDATES_URL.isEmpty()) return
        state = State.Checking
        job = scope.launch {
            state = try {
                val release = withContext(Dispatchers.IO) { fetchRelease() }
                prefs.edit { putLong("lastUpdateCheck", System.currentTimeMillis()) }
                if (release.versionCode > installedVersionCode) {
                    if (prompt) showPrompt = true
                    State.Available(release)
                } else {
                    State.UpToDate
                }
            } catch (error: CancellationException) {
                throw error
            } catch (_: Exception) {
                if (prompt) State.Idle else State.Failed("Couldn't check for updates. Check your connection and try again.")
            }
        }
    }

    private fun fetchRelease(): AppRelease {
        val connection = URL(BuildConfig.UPDATES_URL).openConnection() as HttpURLConnection
        connection.connectTimeout = 15_000
        connection.readTimeout = 30_000
        connection.useCaches = false
        try {
            if (connection.responseCode != 200) throw IOException("HTTP ${connection.responseCode}")
            val json = JSONObject(connection.inputStream.bufferedReader().use { it.readText() })
            return AppRelease(
                versionName = json.getString("version_name"),
                versionCode = json.getLong("version_code"),
                url = json.getString("url"),
                sha256 = json.getString("sha256").lowercase(),
                size = json.optLong("size", -1),
                notes = json.optString("notes", ""),
            )
        } finally {
            connection.disconnect()
        }
    }

    /** Downloads and installs [release], asking for the install permission first if needed. */
    fun install(release: AppRelease) {
        showPrompt = false
        if (!context.packageManager.canRequestPackageInstalls()) {
            state = State.NeedsPermission(release)
            return
        }
        if (job?.isActive == true) return
        state = State.Downloading(release, 0f)
        job = scope.launch {
            try {
                val apk = withContext(Dispatchers.IO) { download(release) }
                state = State.Installing(release)
                withContext(Dispatchers.IO) { startInstall(apk) }
            } catch (error: CancellationException) {
                state = State.Available(release)
                throw error
            } catch (error: Exception) {
                state = State.Failed(error.message ?: "The update couldn't be installed.")
            }
        }
    }

    fun dismissFailure() {
        state = State.Idle
    }

    fun cancel() {
        val current = state
        job?.cancel()
        if (current is State.Downloading) state = State.Available(current.release)
    }

    /** The system screen where the person allows MG Notify to install apps. */
    fun installPermissionIntent(): Intent =
        Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${context.packageName}"))

    /** Back from that screen: carry on if it was allowed. */
    fun resumeAfterPermission() {
        val current = state
        if (current is State.NeedsPermission && context.packageManager.canRequestPackageInstalls()) install(current.release)
    }

    private suspend fun download(release: AppRelease): File {
        val file = File(context.cacheDir, "update.apk")
        val connection = URL(release.url).openConnection() as HttpURLConnection
        connection.connectTimeout = 15_000
        connection.readTimeout = 60_000
        try {
            if (connection.responseCode != 200) throw IOException("The download failed (HTTP ${connection.responseCode}).")
            val total = connection.contentLengthLong.takeIf { it > 0 } ?: release.size
            val digest = MessageDigest.getInstance("SHA-256")
            var done = 0L
            var lastReported = -1
            connection.inputStream.use { input ->
                file.outputStream().use { output ->
                    val buffer = ByteArray(64 * 1024)
                    while (true) {
                        val read = input.read(buffer)
                        if (read < 0) break
                        output.write(buffer, 0, read)
                        digest.update(buffer, 0, read)
                        done += read
                        val percent = if (total > 0) (done * 100 / total).toInt() else 0
                        if (percent != lastReported) {
                            lastReported = percent
                            withContext(Dispatchers.Main) { state = State.Downloading(release, percent / 100f) }
                        }
                    }
                }
            }
            val sha256 = digest.digest().joinToString("") { "%02x".format(it) }
            if (sha256 != release.sha256) {
                file.delete()
                throw IOException("The download was damaged. Please try again.")
            }
            return file
        } finally {
            connection.disconnect()
        }
    }

    private fun startInstall(apk: File) {
        val installer = context.packageManager.packageInstaller
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL).apply {
            setAppPackageName(context.packageName)
        }
        val sessionId = installer.createSession(params)
        installer.openSession(sessionId).use { session ->
            session.openWrite("mg-notify.apk", 0, apk.length()).use { out ->
                apk.inputStream().use { it.copyTo(out) }
                session.fsync(out)
            }
            val flags = PendingIntent.FLAG_UPDATE_CURRENT or
                if (Build.VERSION.SDK_INT >= 31) PendingIntent.FLAG_MUTABLE else 0
            val callback = PendingIntent.getBroadcast(
                context,
                sessionId,
                Intent(context, UpdateInstallReceiver::class.java).setPackage(context.packageName),
                flags,
            )
            session.commit(callback.intentSender)
        }
        apk.delete()
    }

    /** What the installer reported back (see UpdateInstallReceiver). */
    internal fun installerResult(status: Int, message: String?) {
        val current = state
        state = when (status) {
            PackageInstaller.STATUS_SUCCESS -> State.UpToDate
            PackageInstaller.STATUS_FAILURE_ABORTED -> if (current is State.Installing) State.Available(current.release) else State.Idle
            else -> State.Failed(message?.let { "The update couldn't be installed: $it" } ?: "The update couldn't be installed.")
        }
    }
}

/**
 * Android's installer reports here. When it needs the person to confirm, it
 * hands over its confirmation screen, which is opened straight away.
 */
class UpdateInstallReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)
        if (status == PackageInstaller.STATUS_PENDING_USER_ACTION) {
            val confirm = IntentCompat.getParcelableExtra(intent, Intent.EXTRA_INTENT, Intent::class.java) ?: return
            context.startActivity(confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
            return
        }
        context.mgNotifyModel.updater.installerResult(status, intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE))
    }
}
