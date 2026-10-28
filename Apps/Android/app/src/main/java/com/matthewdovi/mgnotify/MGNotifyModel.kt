package com.matthewdovi.mgnotify

import android.Manifest
import android.annotation.SuppressLint
import android.app.Application
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.media.AudioAttributes
import android.media.MediaPlayer
import android.media.Ringtone
import android.media.RingtoneManager
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.Uri
import android.os.Build
import android.provider.OpenableColumns
import android.provider.Settings
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityManager
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import androidx.core.content.edit
import com.google.firebase.FirebaseApp
import com.google.firebase.messaging.FirebaseMessaging
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.MainScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.async
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.io.File
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.util.UUID
import kotlin.coroutines.resume

/**
 * The app's state and everything it does with the server: the Android
 * counterpart of the iOS app's MGNotifyModel, kept step for step alike.
 * It lives for the whole process (MGNotifyApplication creates it), so pushes
 * handled while no screen is showing update the same state.
 */
class MGNotifyModel(private val app: Application) {
    private val prefs = app.getSharedPreferences("mg_notify", Context.MODE_PRIVATE)
    private val scope = MainScope()

    val serverURL = BuildConfig.SERVER_URL
    val portalURL = BuildConfig.PORTAL_URL

    var email by mutableStateOf(prefs.getString("email", "") ?: "")
    var password by mutableStateOf("")
    var authScreenMode by mutableStateOf(AuthScreenMode.SIGN_IN)
    var permissionStatus by mutableStateOf("Not requested")
        private set
    var isNotificationPermissionDenied by mutableStateOf(false)
        private set
    var deviceToken by mutableStateOf("")
        private set
    var notifications by mutableStateOf<List<AccountNotification>>(emptyList())
        private set
    var importedSounds by mutableStateOf<List<String>>(emptyList())
        private set
    var selectedSound by mutableStateOf(prefs.getString("selectedSound", null) ?: NotificationSounds.SYSTEM_DEFAULT)
    var isAuthenticated by mutableStateOf(false)
        private set
    var isBusy by mutableStateOf(false)
        private set
    var isVerifyingStoredSession by mutableStateOf(false)
        private set
    var accountVerificationFailed by mutableStateOf(false)
        private set
    var remoteSignOutNotice by mutableStateOf(false)
        private set
    var emailVerificationRequired by mutableStateOf(false)
        private set
    var hasInternetConnection by mutableStateOf(true)
        private set
    var navigationResetToken by mutableIntStateOf(0)
        private set
    var devicesRefreshToken by mutableIntStateOf(0)
        private set
    var apiKeysRefreshToken by mutableIntStateOf(0)
        private set
    var notificationsRefreshToken by mutableIntStateOf(0)
        private set
    var authErrorMessage by mutableStateOf("")
    var verificationMessage by mutableStateOf("")
        private set
    /** The active account's optional display name; empty when it has none. */
    var displayName by mutableStateOf(prefs.getString("displayName", "") ?: "")
        private set
    var savedAccounts by mutableStateOf(SavedAccount.listFromJson(prefs.getString("savedAccounts", null)))
        private set
    var isAddingAccount by mutableStateOf(false)
        private set

    /** Notifications that were unread when this visit to the app saw them: shown as "New" until the app is left. */
    var newNotificationIDs by mutableStateOf<Set<String>>(emptySet())
        private set
    private var markSeenJob: Job? = null

    val navigator = Navigator()

    /** Checks for and installs new releases of the app from notify.mad-gamer.com. */
    val updater = Updater(app, scope)

    /** Set when something outside the app (a share, a link) asks for a sounds screen. */
    var pendingSoundsNavigation by mutableStateOf<SoundsNavigation?>(null)

    var sessionToken = prefs.getString("sessionToken", "") ?: ""
        private set
    var accountID = prefs.getString("accountID", "") ?: ""
        private set
    private var sessionTokenBeforeAddingAccount: String? = null
    val deviceID: String
    private var mediaPlayer: MediaPlayer? = null
    private var ringtone: Ringtone? = null
    private var didAttemptLaunchBootstrap = false
    private var isDeletingAccountLocally = false

    /** True while an activity is on screen (MainActivity sets it). */
    var isInForeground = false

    /** One live connection per signed-in account while the app is open. */
    private val eventStreams = mutableMapOf<String, EventStream>()
    private var liveUpdatesWanted = false

    /** Notifications already announced, so a push and a stream event aren't both spoken. */
    private val announcedNotificationIDs = LinkedHashSet<String>()

    /** Shows the system's notification permission prompt; MainActivity provides it. */
    var notificationPermissionRequester: (suspend () -> Boolean)? = null

    init {
        val existingID = prefs.getString("deviceID", null)
        deviceID = if (!existingID.isNullOrEmpty()) existingID else UUID.randomUUID().toString().lowercase().also {
            prefs.edit { putString("deviceID", it) }
        }
        isAuthenticated = sessionToken.isNotEmpty() && email.isNotEmpty()
        if (savedAccounts.isEmpty() && isAuthenticated) {
            upsertActiveSavedAccount()
        } else if (!isAuthenticated && savedAccounts.isNotEmpty()) {
            val account = savedAccounts.first()
            sessionToken = account.sessionToken
            accountID = account.accountID
            email = account.email
            isAuthenticated = true
            saveSettings()
        }
        refreshPermissionStatus()
        reloadImportedSounds()
        startNetworkMonitoring()
    }

    suspend fun performLaunchChecksIfNeeded() {
        if (didAttemptLaunchBootstrap) return
        didAttemptLaunchBootstrap = true
        isVerifyingStoredSession = true
        try {
            if (!checkServerHealth()) {
                resetNavigationHierarchy()
                accountVerificationFailed = true
                return
            }
            startLiveUpdates()
            if (sessionToken.isEmpty() || email.isEmpty()) {
                accountVerificationFailed = false
                return
            }
            verifyStoredSession(showLoadingState = false)
        } finally {
            isVerifyingStoredSession = false
        }
    }

    suspend fun retryServerConnection() {
        isVerifyingStoredSession = true
        try {
            if (!checkServerHealth()) {
                resetNavigationHierarchy()
                accountVerificationFailed = true
                return
            }
            accountVerificationFailed = false
            startLiveUpdates()
            if (sessionToken.isNotEmpty() && email.isNotEmpty()) {
                verifyStoredSession(showLoadingState = false)
            }
        } finally {
            isVerifyingStoredSession = false
        }
    }

    suspend fun refreshServerAndSession() {
        if (!checkServerHealth()) {
            resetNavigationHierarchy()
            accountVerificationFailed = true
            stopLiveUpdates()
            return
        }
        accountVerificationFailed = false
        startLiveUpdates()
        if (sessionToken.isNotEmpty() && email.isNotEmpty()) {
            verifyStoredSession(showLoadingState = false)
        }
    }

    suspend fun verifyStoredSession(showLoadingState: Boolean = true) {
        if (sessionToken.isEmpty() || email.isEmpty()) return
        if (showLoadingState) isVerifyingStoredSession = true
        try {
            val account = send("/auth/me", "GET", authenticated = true).getJSONObject("account")
            applyAccount(account)
            isAuthenticated = true
            accountVerificationFailed = false
            remoteSignOutNotice = false
            emailVerificationRequired = !account.getBoolean("email_verified")
            authErrorMessage = ""
            verificationMessage = ""
            saveSettings()
            if (emailVerificationRequired) {
                notifications = emptyList()
                resetNavigationHierarchy()
            }
            completeSignedInSetup()
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            if (!remoteSignOutNotice) {
                resetNavigationHierarchy()
                accountVerificationFailed = true
            }
        } finally {
            if (showLoadingState) isVerifyingStoredSession = false
        }
    }

    /** The account fields the server sends (login, /auth/me, events). */
    private fun applyAccount(account: JSONObject) {
        email = account.getString("email")
        accountID = account.getString("account_id")
        displayName = account.optStringOrNull("display_name").orEmpty()
    }

    /** What to call the active account: its display name, or else its email. */
    val accountName: String get() = displayName.ifEmpty { email }

    /** Sets (or, when blank, removes) the active account's display name. */
    suspend fun setDisplayName(name: String) {
        val account = send(
            "/account/display-name",
            "POST",
            JSONObject().put("display_name", name.trim()),
            authenticated = true,
        ).getJSONObject("account")
        applyAccount(account)
        saveSettings()
    }

    fun refreshPermissionStatus() {
        val enabled = NotificationManagerCompat.from(app).areNotificationsEnabled()
        val requested = prefs.getBoolean("notificationPermissionRequested", false)
        val needsRuntimePermission = Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU
        permissionStatus = when {
            enabled -> "Enabled"
            needsRuntimePermission && !requested -> "Not requested"
            else -> "Denied"
        }
        isNotificationPermissionDenied = permissionStatus == "Denied"
    }

    fun openNotificationSettings(context: Context) {
        val intent = Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
            .putExtra(Settings.EXTRA_APP_PACKAGE, app.packageName)
        context.startActivity(intent)
    }

    suspend fun login() = signIn("/auth/login")

    suspend fun registerAccount() = signIn("/auth/register")

    private suspend fun signIn(path: String) {
        if (email.isBlank() || password.isEmpty()) return
        isBusy = true
        try {
            val response = send(
                path,
                "POST",
                JSONObject().put("email", email).put("password", password),
            )
            val account = response.getJSONObject("account")
            sessionToken = response.getJSONObject("session").getString("session_token")
            applyAccount(account)
            isAuthenticated = true
            isAddingAccount = false
            sessionTokenBeforeAddingAccount = null
            authScreenMode = AuthScreenMode.SIGN_IN
            accountVerificationFailed = false
            remoteSignOutNotice = false
            emailVerificationRequired = !account.getBoolean("email_verified")
            authErrorMessage = ""
            verificationMessage = ""
            password = ""
            saveSettings()
            if (emailVerificationRequired) {
                notifications = emptyList()
                resetNavigationHierarchy()
            }
            completeSignedInSetup()
        } catch (error: CancellationException) {
            throw error
        } catch (error: Exception) {
            authErrorMessage = error.message ?: "Unknown server error"
        } finally {
            isBusy = false
        }
    }

    suspend fun resendVerificationEmail() {
        if (!isAuthenticated || sessionToken.isEmpty()) return
        isBusy = true
        try {
            send("/auth/resend-verification", "POST", authenticated = true)
            authErrorMessage = ""
            verificationMessage = "A new verification email has been sent."
        } catch (error: CancellationException) {
            throw error
        } catch (error: Exception) {
            verificationMessage = ""
            authErrorMessage = error.message ?: "Unknown server error"
        } finally {
            isBusy = false
        }
    }

    suspend fun requestPasswordReset() {
        if (email.isBlank()) return
        runRequest {
            send("/auth/request-password-reset", "POST", JSONObject().put("email", email))
        }
    }

    suspend fun logout() {
        if (sessionToken.isEmpty()) {
            forgetLocalAccountData()
            return
        }
        runRequest {
            runCatching { send("/devices/$deviceID", "DELETE", authenticated = true) }
            runCatching { send("/auth/logout", "POST", authenticated = true) }
            forgetLocalAccountData()
        }
    }

    fun forgetLocalAccountData() {
        val forgottenSessionToken = sessionToken
        isDeletingAccountLocally = false
        isAddingAccount = false
        sessionTokenBeforeAddingAccount = null
        sessionToken = ""
        accountID = ""
        email = ""
        displayName = ""
        password = ""
        notifications = emptyList()
        deviceToken = ""
        isAuthenticated = false
        accountVerificationFailed = false
        remoteSignOutNotice = false
        emailVerificationRequired = false
        authErrorMessage = ""
        verificationMessage = ""
        stopLiveUpdates()
        resetNavigationHierarchy()
        prefs.edit {
            remove("sessionToken")
            remove("email")
            remove("accountID")
            remove("displayName")
        }
        removeSavedAccount(forgottenSessionToken)
        savedAccounts.firstOrNull()?.let { nextAccount ->
            scope.launch { switchToAccount(nextAccount) }
        }
    }

    suspend fun switchToAccount(account: SavedAccount) {
        isAddingAccount = false
        sessionTokenBeforeAddingAccount = null
        sessionToken = account.sessionToken
        accountID = account.accountID
        email = account.email
        displayName = account.displayName
        password = ""
        notifications = emptyList()
        newNotificationIDs = emptySet()
        isAuthenticated = true
        accountVerificationFailed = false
        remoteSignOutNotice = false
        emailVerificationRequired = false
        authErrorMessage = ""
        verificationMessage = ""
        saveSettings()
        resetNavigationHierarchy()
        startLiveUpdates()
        verifyStoredSession()
    }

    fun startAddingAccount() {
        if (isAddingAccount) return
        sessionTokenBeforeAddingAccount = sessionToken
        isAddingAccount = true
        sessionToken = ""
        accountID = ""
        email = ""
        displayName = ""
        password = ""
        notifications = emptyList()
        isAuthenticated = false
        emailVerificationRequired = false
        authScreenMode = AuthScreenMode.SIGN_IN
        authErrorMessage = ""
        verificationMessage = ""
        resetNavigationHierarchy()
    }

    suspend fun cancelAddingAccount() {
        val previousSessionToken = sessionTokenBeforeAddingAccount
        val account = savedAccounts.firstOrNull { it.sessionToken == previousSessionToken } ?: savedAccounts.firstOrNull()
        if (account == null) {
            isAddingAccount = false
            sessionTokenBeforeAddingAccount = null
            return
        }
        switchToAccount(account)
    }

    fun isActiveAccount(account: SavedAccount): Boolean =
        sessionToken.isNotEmpty() && account.sessionToken == sessionToken

    suspend fun signOutSavedAccount(account: SavedAccount) {
        if (isActiveAccount(account)) {
            logout()
            return
        }
        runRequest {
            runCatching {
                send("/devices/$deviceID", "DELETE", authenticated = true, remoteSignOutOn401 = false, sessionTokenOverride = account.sessionToken)
            }
            runCatching {
                send("/auth/logout", "POST", authenticated = true, remoteSignOutOn401 = false, sessionTokenOverride = account.sessionToken)
            }
            removeSavedAccount(account.sessionToken)
        }
    }

    fun handleRemoteSignOut() {
        if (isDeletingAccountLocally) {
            forgetLocalAccountData()
            return
        }
        if (!isAuthenticated && sessionToken.isEmpty() && email.isEmpty()) return
        isAuthenticated = false
        isBusy = false
        isVerifyingStoredSession = false
        accountVerificationFailed = false
        remoteSignOutNotice = true
        emailVerificationRequired = false
        authErrorMessage = ""
        verificationMessage = ""
        notifications = emptyList()
        deviceToken = ""
        stopLiveUpdates()
        resetNavigationHierarchy()
    }

    fun updateDeviceToken(token: String) {
        deviceToken = token
        scope.launch {
            if (isAuthenticated) {
                registerCurrentDevice()
                if (!emailVerificationRequired) fetchNotifications()
            }
            registerOtherSavedAccountDevices()
        }
    }

    /**
     * A push arrived. [payload] is the push's `mg_notify` object. Pushes for
     * another signed-in account switch to it when opened, as on iOS.
     */
    suspend fun receiveNotification(payload: JSONObject?, openedByUser: Boolean = false): PushHandlingResult {
        val pushAccountID = payload?.optString("account_id")?.takeIf { it.isNotEmpty() }
        if (pushAccountID != null && accountID.isNotEmpty() && pushAccountID != accountID) {
            return receiveNotificationForOtherAccount(pushAccountID, payload, openedByUser)
        }
        // With the app open, the live connection has usually brought the same
        // change already; only refetch what it hasn't.
        val streaming = liveUpdatesWanted && eventStreams.containsKey(sessionToken)
        val event = PushEvent.from(payload?.optString("event"))
        if (event != null) {
            val isRefreshOnly = event in setOf(PushEvent.DEVICES_UPDATED, PushEvent.API_KEYS_UPDATED, PushEvent.NOTIFICATIONS_UPDATED)
            if (!(streaming && isRefreshOnly)) handlePushEvent(event)
            return PushHandlingResult.REMOTE_EVENT
        }
        if (!isAuthenticated) return PushHandlingResult.IGNORED
        val id = payload?.optString("notification_id").orEmpty()
        if (id.isEmpty() || notifications.none { it.notificationID == id }) fetchNotifications()
        return PushHandlingResult.REFRESHED
    }

    suspend fun registerCurrentDevice() {
        if (!isAuthenticated || deviceToken.isEmpty()) return
        runRequest {
            send("/devices/register", "POST", deviceRegistrationBody(), authenticated = true)
        }
    }

    suspend fun registerOtherSavedAccountDevices() {
        if (deviceToken.isEmpty()) return
        for (account in savedAccounts.filter { !isActiveAccount(it) }) {
            try {
                send(
                    "/devices/register",
                    "POST",
                    deviceRegistrationBody(),
                    authenticated = true,
                    remoteSignOutOn401 = false,
                    sessionTokenOverride = account.sessionToken,
                )
            } catch (error: RequestError) {
                if (error.statusCode == 401) removeSavedAccount(account.sessionToken)
            } catch (error: CancellationException) {
                throw error
            } catch (_: Exception) {
            }
        }
    }

    private fun deviceRegistrationBody(): JSONObject = JSONObject()
        .put("device_id", deviceID)
        .put("device_name", deviceName())
        .put("fcm_token", deviceToken)
        .put("platform", "android")
        .put("notification_sound", selectedSound)

    private fun deviceName(): String =
        Settings.Global.getString(app.contentResolver, Settings.Global.DEVICE_NAME)?.takeIf { it.isNotBlank() }
            ?: "${Build.MANUFACTURER} ${Build.MODEL}"

    suspend fun saveSelectedSound() {
        saveSelectedSoundPreference()
        for (account in savedAccounts.filter { !isActiveAccount(it) }) {
            runCatching {
                send(
                    "/devices/sound",
                    "POST",
                    deviceSoundBody(),
                    authenticated = true,
                    remoteSignOutOn401 = false,
                    sessionTokenOverride = account.sessionToken,
                )
            }
        }
        if (!isAuthenticated || emailVerificationRequired) return
        runRequest {
            send("/devices/sound", "POST", deviceSoundBody(), authenticated = true)
        }
    }

    private fun deviceSoundBody(): JSONObject = JSONObject()
        .put("device_id", deviceID)
        .put("notification_sound", selectedSound)

    fun previewSoundSelection() = playSound(selectedSound, playSystemDefault = false)

    /**
     * Plays a sound choice as a notification would (so it follows the
     * notification volume and silent mode). "System Default" is Android's
     * current default notification sound.
     */
    private fun playSound(sound: String, playSystemDefault: Boolean) {
        mediaPlayer?.release()
        mediaPlayer = null
        ringtone?.stop()
        ringtone = null
        val attributes = AudioAttributes.Builder()
            .setUsage(AudioAttributes.USAGE_NOTIFICATION)
            .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
            .build()
        if (sound == NotificationSounds.NO_SOUND) return
        if (sound == NotificationSounds.SYSTEM_DEFAULT) {
            if (!playSystemDefault) return
            ringtone = RingtoneManager.getRingtone(app, Settings.System.DEFAULT_NOTIFICATION_URI)?.apply {
                audioAttributes = attributes
                play()
            }
            return
        }
        val file = File(NotificationSounds.soundsDirectory(app), sound)
        if (!file.exists()) return
        mediaPlayer = runCatching {
            MediaPlayer().apply {
                setAudioAttributes(attributes)
                setDataSource(file.absolutePath)
                setOnCompletionListener { it.release(); if (mediaPlayer === it) mediaPlayer = null }
                prepare()
                start()
            }
        }.getOrNull()
    }

    suspend fun fetchNotifications() {
        if (!isAuthenticated || emailVerificationRequired) return
        runRequest {
            val response = send("/notifications", "GET", authenticated = true)
            val items = response.getJSONArray("notifications")
            notifications = (0 until items.length()).map { AccountNotification.fromJson(items.getJSONObject(it)) }
            newNotificationIDs = newNotificationIDs + notifications.filter { !it.seen }.map { it.notificationID }
        }
    }

    suspend fun deleteNotification(notificationID: String) {
        if (!isAuthenticated || emailVerificationRequired) return
        runRequest {
            send("/notifications/$notificationID", "DELETE", authenticated = true)
            notifications = notifications.filter { it.notificationID != notificationID }
        }
    }

    suspend fun clearNotifications() {
        if (!isAuthenticated || emailVerificationRequired) return
        runRequest {
            send("/notifications", "DELETE", authenticated = true)
            notifications = emptyList()
        }
    }

    suspend fun loadAccountDevices(): List<AccountDevice> {
        val devices = send("/devices", "GET", authenticated = true).getJSONArray("devices")
        return (0 until devices.length()).map { AccountDevice.fromJson(devices.getJSONObject(it)) }
    }

    suspend fun signOutManagedDevice(deviceID: String) {
        send("/devices/$deviceID/sign-out", "POST", authenticated = true)
        if (deviceID == this.deviceID) handleRemoteSignOut()
    }

    suspend fun signOutOtherManagedDevices() {
        send("/devices/sign-out-other", "POST", authenticated = true)
    }

    suspend fun loadAPIKeys(): List<ManagedAPIKey> {
        val keys = send("/api-keys", "GET", authenticated = true).getJSONArray("api_keys")
        return (0 until keys.length()).map { ManagedAPIKey.fromJson(keys.getJSONObject(it)) }
    }

    suspend fun createManagedAPIKey(label: String): ManagedAPIKey =
        ManagedAPIKey.fromJson(
            send("/api-keys/create", "POST", JSONObject().put("label", label), authenticated = true)
                .getJSONObject("api_key"),
        )

    suspend fun deleteManagedAPIKey(keyID: String) {
        send("/api-keys/delete", "POST", JSONObject().put("key_id", keyID), authenticated = true)
    }

    suspend fun deleteAllManagedAPIKeys() {
        send("/api-keys/delete-all", "POST", authenticated = true)
    }

    suspend fun changeAccountEmail(newEmail: String, password: String) {
        val account = send(
            "/account/change-email",
            "POST",
            JSONObject().put("new_email", newEmail).put("password", password),
            authenticated = true,
            remoteSignOutOn401 = false,
        ).getJSONObject("account")
        applyAccount(account)
        emailVerificationRequired = !account.getBoolean("email_verified")
        notifications = emptyList()
        saveSettings()
        resetNavigationHierarchy()
    }

    suspend fun changeAccountPassword(currentPassword: String, newPassword: String) {
        send(
            "/account/change-password",
            "POST",
            JSONObject().put("current_password", currentPassword).put("new_password", newPassword),
            authenticated = true,
            remoteSignOutOn401 = false,
        )
        handleRemoteSignOut()
    }

    suspend fun deleteAccount(password: String) {
        isDeletingAccountLocally = true
        try {
            send(
                "/account/delete",
                "POST",
                JSONObject().put("password", password),
                authenticated = true,
                remoteSignOutOn401 = false,
            )
            forgetLocalAccountData()
        } catch (error: Exception) {
            // The account_deleted push may have signed this device out already.
            if (!isDeletingAccountLocally && sessionToken.isEmpty() && email.isEmpty()) return
            isDeletingAccountLocally = false
            throw error
        }
    }

    fun reloadImportedSounds() {
        importedSounds = NotificationSounds.soundsDirectory(app).listFiles()
            .orEmpty()
            .filter { it.isFile }
            .map { it.name }
            .sortedWith(String.CASE_INSENSITIVE_ORDER)
        if (selectedSound != NotificationSounds.NO_SOUND &&
            selectedSound != NotificationSounds.SYSTEM_DEFAULT &&
            selectedSound !in importedSounds
        ) {
            selectedSound = NotificationSounds.SYSTEM_DEFAULT
        }
        NotificationSounds.ensureChannels(app, selectedSound)
    }

    /** Copies picked files in as sounds; true when at least one was added. */
    suspend fun importSounds(uris: List<Uri>): Boolean {
        val imported = withContext(Dispatchers.IO) {
            uris.mapNotNull { uri ->
                runCatching { copyIntoSounds(uri) }.getOrNull()
            }
        }
        reloadImportedSounds()
        return imported.isNotEmpty()
    }

    /** Copies a file into the sounds folder if it's a supported sound of 30 seconds or less. */
    private fun copyIntoSounds(uri: Uri): String? {
        val name = displayNameFor(uri) ?: return null
        val staging = File(app.cacheDir, "import-$name")
        app.contentResolver.openInputStream(uri)?.use { input ->
            staging.outputStream().use { input.copyTo(it) }
        } ?: return null
        try {
            if (!NotificationSounds.isImportable(staging)) return null
            val destination = File(NotificationSounds.soundsDirectory(app), name)
            staging.copyTo(destination, overwrite = true)
            return name
        } finally {
            staging.delete()
        }
    }

    private fun displayNameFor(uri: Uri): String? {
        val fromProvider = runCatching {
            app.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
                if (cursor.moveToFirst()) cursor.getString(0) else null
            }
        }.getOrNull()
        val name = fromProvider ?: uri.lastPathSegment?.substringAfterLast('/')
        return name?.replace('/', '_')?.takeIf { it.isNotBlank() }
    }

    fun deleteImportedSound(name: String) {
        File(NotificationSounds.soundsDirectory(app), name).delete()
        if (selectedSound == name) {
            selectedSound = NotificationSounds.SYSTEM_DEFAULT
            saveSelectedSoundPreference()
            if (isAuthenticated) scope.launch { saveSelectedSound() }
        }
        reloadImportedSounds()
    }

    fun deleteAllImportedSounds() {
        importedSounds.forEach { File(NotificationSounds.soundsDirectory(app), it).delete() }
        selectedSound = NotificationSounds.SYSTEM_DEFAULT
        saveSelectedSoundPreference()
        reloadImportedSounds()
        if (isAuthenticated) scope.launch { saveSelectedSound() }
    }

    fun soundOptions(): List<SoundOption> = buildList {
        add(SoundOption(NotificationSounds.NO_SOUND, "No Sound"))
        add(SoundOption(NotificationSounds.SYSTEM_DEFAULT, "System Default"))
        importedSounds.forEach { add(SoundOption(it, NotificationSounds.displayName(it))) }
    }

    fun selectedSoundTitle(): String =
        soundOptions().firstOrNull { it.id == selectedSound }?.title ?: "System Default"

    fun pendingSoundImports(): List<File> =
        NotificationSounds.pendingImportsDirectory(app).listFiles()
            .orEmpty()
            .filter { it.isFile }
            .sortedWith(compareBy(String.CASE_INSENSITIVE_ORDER) { it.name })

    /** Files shared to the app wait here until the person confirms on the Add Sounds screen. */
    suspend fun stagePendingSoundImports(uris: List<Uri>) {
        withContext(Dispatchers.IO) {
            val directory = NotificationSounds.pendingImportsDirectory(app)
            directory.listFiles()?.forEach { it.delete() }
            for (uri in uris) {
                runCatching {
                    val name = displayNameFor(uri) ?: return@runCatching
                    app.contentResolver.openInputStream(uri)?.use { input ->
                        File(directory, name).outputStream().use { input.copyTo(it) }
                    }
                }
            }
        }
        pendingSoundsNavigation = SoundsNavigation.ADD_SOUNDS
    }

    suspend fun addPendingSoundImports(selectSingleImportedSound: Boolean) {
        val pending = pendingSoundImports()
        if (pending.isEmpty()) return
        val importedFileNames = withContext(Dispatchers.IO) {
            pending.mapNotNull { file ->
                runCatching {
                    if (!NotificationSounds.isImportable(file)) return@runCatching null
                    file.copyTo(File(NotificationSounds.soundsDirectory(app), file.name), overwrite = true)
                    file.name
                }.getOrNull()
            }
        }
        clearPendingSoundImports()
        reloadImportedSounds()
        if (selectSingleImportedSound && importedFileNames.size == 1) {
            selectedSound = importedFileNames.first()
            saveSelectedSound()
        }
    }

    fun clearPendingSoundImports() {
        NotificationSounds.pendingImportsDirectory(app).listFiles()?.forEach { it.delete() }
    }

    private suspend fun completeSignedInSetup() {
        ensurePushSetup()
        registerCurrentDevice()
        if (!emailVerificationRequired) fetchNotifications()
        registerOtherSavedAccountDevices()
    }

    private suspend fun checkServerHealth(): Boolean =
        runCatching { send("/health", "GET") }.isSuccess

    /**
     * Opens a live connection (EventStream) for every signed-in account while
     * the app is open, so changes made anywhere show up at once. Nothing is
     * polled; a dropped connection leads to one health check.
     */
    fun startLiveUpdates() {
        liveUpdatesWanted = true
        syncEventStreams()
    }

    fun stopLiveUpdates() {
        liveUpdatesWanted = false
        eventStreams.values.forEach { it.stop() }
        eventStreams.clear()
    }

    private fun syncEventStreams() {
        val tokens = savedAccounts.map { it.sessionToken }.toMutableSet()
        if (isAuthenticated && sessionToken.isNotEmpty()) tokens += sessionToken
        (eventStreams.keys - tokens).forEach { eventStreams.remove(it)?.stop() }
        for (token in tokens - eventStreams.keys) {
            val stream = EventStream(serverURL, token) { name, data -> handleStreamEvent(token, name, data) }
            eventStreams[token] = stream
            stream.start(scope)
        }
    }

    private suspend fun handleStreamEvent(token: String, name: String, data: JSONObject) {
        val active = isAuthenticated && token == sessionToken
        when (name) {
            "ready", "account_updated" -> {
                val account = data.optJSONObject("account") ?: return
                if (!active) {
                    updateSavedAccount(token, account)
                    return
                }
                val wasVerificationRequired = emailVerificationRequired
                applyAccount(account)
                emailVerificationRequired = !account.optBoolean("email_verified", true)
                saveSettings()
                if (emailVerificationRequired && !wasVerificationRequired) {
                    notifications = emptyList()
                    resetNavigationHierarchy()
                } else if (!emailVerificationRequired && (wasVerificationRequired || data.optBoolean("reconnected"))) {
                    // Newly verified, or back after a dropped connection: catch up.
                    fetchNotifications()
                    devicesRefreshToken++
                    apiKeysRefreshToken++
                }
            }
            "notification" -> {
                val notification = AccountNotification.fromJson(data.getJSONObject("notification"))
                if (active && notifications.none { it.notificationID == notification.notificationID }) {
                    notifications = listOf(notification) + notifications
                    if (!notification.seen) newNotificationIDs = newNotificationIDs + notification.notificationID
                }
                announceNotification(notification.accountID(token), notification.notificationID, notification.displayTitle, notification.message, notification.email)
            }
            "notification_deleted" -> if (active) {
                val id = data.optString("notification_id")
                notifications = notifications.filter { it.notificationID != id }
            }
            "notifications_cleared" -> if (active) notifications = emptyList()
            "notifications_seen" -> if (active) {
                val ids = data.optJSONArray("notification_ids")
                val seenIDs = (0 until (ids?.length() ?: 0)).map { ids!!.getString(it) }.toSet()
                notifications = notifications.map { if (it.notificationID in seenIDs) it.copy(seen = true) else it }
            }
            "devices_updated" -> if (active) devicesRefreshToken++
            "api_keys_updated" -> if (active) apiKeysRefreshToken++
            "resync" -> if (active) {
                fetchNotifications()
                devicesRefreshToken++
                apiKeysRefreshToken++
            }
            "signed_out", "account_deleted", "unauthorized" -> {
                // The stream has ended itself; just forget it.
                eventStreams.remove(token)
                if (active) handleRemoteSignOut() else removeSavedAccount(token)
            }
            "disconnected" -> if (active && data.optInt("failures") == 1 && hasInternetConnection && !checkServerHealth()) {
                resetNavigationHierarchy()
                accountVerificationFailed = true
                stopLiveUpdates()
            }
        }
    }

    private fun AccountNotification.accountID(token: String): String =
        savedAccounts.firstOrNull { it.sessionToken == token }?.accountID ?: accountID

    /**
     * The notification list is on screen: once it has been for a moment,
     * unread notifications are marked seen everywhere (which also updates
     * iPhone badges). They stay marked "New" here until the app is left.
     */
    fun scheduleMarkSeen() {
        markSeenJob?.cancel()
        markSeenJob = scope.launch {
            delay(1500)
            if (!isInForeground || !isAuthenticated || emailVerificationRequired) return@launch
            if (notifications.none { !it.seen }) return@launch
            runCatching { send("/notifications/mark-seen", "POST", JSONObject(), authenticated = true) }.onSuccess {
                notifications = notifications.map { it.copy(seen = true) }
            }
        }
    }

    /** Leaving the app: next time, only notifications that are new by then are marked. */
    fun forgetNewMarkers() {
        markSeenJob?.cancel()
        newNotificationIDs = emptySet()
    }

    /** Another signed-in account's email or display name changed. */
    private fun updateSavedAccount(token: String, account: JSONObject) {
        val index = savedAccounts.indexOfFirst { it.sessionToken == token }
        if (index < 0) return
        val updated = savedAccounts[index].copy(
            email = account.getString("email"),
            displayName = account.optStringOrNull("display_name").orEmpty(),
        )
        if (updated == savedAccounts[index]) return
        savedAccounts = savedAccounts.toMutableList().also { it[index] = updated }
        persistSavedAccounts()
    }

    /** A push that arrived while the app is open: announced instead of shown. */
    fun announcePush(payload: JSONObject?, title: String, body: String) {
        val email = payload?.optBoolean("email") ?: false
        val displayTitle = AccountNotification("", title, body, "", email, null).displayTitle
        val pushAccountID = payload?.optString("account_id")?.takeIf { it.isNotEmpty() } ?: accountID
        val id = payload?.optString("notification_id")?.takeIf { it.isNotEmpty() }
        announceNotification(pushAccountID, id, displayTitle, body, email)
    }

    /**
     * While the app is open, a new notification is spoken ("New notification
     * in <account>: <title>, <message>", naming the account only when several
     * are signed in) and its sound played, instead of showing a notification.
     * Each one is announced once, however it arrived.
     */
    private fun announceNotification(forAccountID: String, notificationID: String?, title: String, message: String, email: Boolean) {
        if (!isInForeground) return
        if (notificationID != null) {
            if (!announcedNotificationIDs.add(notificationID)) return
            if (announcedNotificationIDs.size > 200) announcedNotificationIDs.remove(announcedNotificationIDs.first())
        }
        val where = if (savedAccounts.size > 1) {
            " in " + (savedAccounts.firstOrNull { it.accountID == forAccountID }?.name ?: accountName)
        } else {
            ""
        }
        announce("New notification$where: $title, ${if (email) "email" else message}")
        playNotificationSound()
    }

    // Android 16 deprecates announcement events in favour of live regions,
    // which only work for text on screen; a notification arriving needs a
    // one-off announcement, and this is still how to make one.
    @Suppress("DEPRECATION")
    private fun announce(text: String) {
        val manager = app.getSystemService(AccessibilityManager::class.java)
        if (!manager.isEnabled) return
        val event = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            AccessibilityEvent(AccessibilityEvent.TYPE_ANNOUNCEMENT)
        } else {
            @Suppress("DEPRECATION")
            AccessibilityEvent.obtain(AccessibilityEvent.TYPE_ANNOUNCEMENT)
        }
        event.packageName = app.packageName
        event.className = MainActivity::class.java.name
        event.text.add(text)
        manager.sendAccessibilityEvent(event)
    }

    /** The chosen sound for a notification arriving while the app is open; "System Default" is Android's own notification sound. */
    fun playNotificationSound() = playSound(selectedSound, playSystemDefault = true)

    private suspend fun ensurePushSetup() {
        val granted = notificationsPermitted()
        val requested = prefs.getBoolean("notificationPermissionRequested", false)
        when {
            granted -> registerForRemoteNotifications()
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU && !requested -> {
                val requester = notificationPermissionRequester
                if (requester != null) {
                    prefs.edit { putBoolean("notificationPermissionRequested", true) }
                    val nowGranted = requester()
                    refreshPermissionStatus()
                    if (nowGranted) registerForRemoteNotifications()
                } else {
                    refreshPermissionStatus()
                }
            }
            else -> refreshPermissionStatus()
        }
    }

    private fun notificationsPermitted(): Boolean {
        val runtimeGranted = Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
            ContextCompat.checkSelfPermission(app, Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED
        return runtimeGranted && NotificationManagerCompat.from(app).areNotificationsEnabled()
    }

    /** Fetches this device's FCM token; it arrives through updateDeviceToken, like the APNs token on iOS. */
    private suspend fun registerForRemoteNotifications() {
        if (FirebaseApp.getApps(app).isEmpty()) return
        val token = suspendCancellableCoroutine<String?> { continuation ->
            FirebaseMessaging.getInstance().token.addOnCompleteListener { task ->
                continuation.resume(if (task.isSuccessful) task.result else null)
            }
        } ?: return
        if (token != deviceToken) {
            updateDeviceToken(token)
        }
    }

    private fun saveSelectedSoundPreference() {
        prefs.edit { putString("selectedSound", selectedSound) }
        NotificationSounds.ensureChannels(app, selectedSound)
    }

    private fun saveSettings() {
        prefs.edit {
            putString("email", email)
            putString("sessionToken", sessionToken)
            putString("accountID", accountID)
            putString("displayName", displayName)
        }
        upsertActiveSavedAccount()
        saveSelectedSoundPreference()
    }

    private fun persistSavedAccounts() {
        prefs.edit { putString("savedAccounts", SavedAccount.listToJson(savedAccounts)) }
        if (liveUpdatesWanted) syncEventStreams()
    }

    private fun upsertActiveSavedAccount() {
        if (sessionToken.isEmpty() || email.isEmpty()) return
        val account = SavedAccount(accountID, email, sessionToken, displayName)
        val index = savedAccounts.indexOfFirst {
            it.sessionToken == sessionToken || (accountID.isNotEmpty() && it.accountID == accountID)
        }
        savedAccounts = if (index >= 0) {
            savedAccounts.toMutableList().also { it[index] = account }
        } else {
            savedAccounts + account
        }
        persistSavedAccounts()
    }

    private fun removeSavedAccount(token: String) {
        if (token.isEmpty()) return
        savedAccounts = savedAccounts.filter { it.sessionToken != token }
        persistSavedAccounts()
    }

    private suspend fun runRequest(action: suspend () -> Unit) {
        isBusy = true
        try {
            action()
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
        } finally {
            isBusy = false
        }
    }

    /**
     * One call to the server. Failures throw RequestError carrying the
     * server's `detail`; a 401 for the active account signs it out, and a 403
     * asking for email verification shows the verification screen, as on iOS.
     */
    private suspend fun send(
        path: String,
        method: String,
        body: JSONObject? = null,
        authenticated: Boolean = false,
        remoteSignOutOn401: Boolean = true,
        sessionTokenOverride: String? = null,
    ): JSONObject {
        val token = sessionTokenOverride ?: sessionToken
        if (authenticated && token.isEmpty()) throw RequestError(null, "The current account session is missing.")
        val (statusCode, responseText) = withContext(Dispatchers.IO) {
            val url = URL(serverURL.trimEnd('/') + "/" + path.trim('/'))
            val connection = url.openConnection() as HttpURLConnection
            try {
                connection.requestMethod = method
                connection.connectTimeout = 30_000
                connection.readTimeout = 60_000
                connection.setRequestProperty("Accept", "application/json")
                if (authenticated) connection.setRequestProperty("Authorization", "Bearer $token")
                if (body != null) {
                    connection.doOutput = true
                    connection.setRequestProperty("Content-Type", "application/json")
                    connection.outputStream.use { it.write(body.toString().toByteArray(Charsets.UTF_8)) }
                }
                val code = connection.responseCode
                val stream = if (code in 200..299) connection.inputStream else connection.errorStream
                code to (stream?.bufferedReader(Charsets.UTF_8)?.use { it.readText() } ?: "")
            } catch (error: IOException) {
                throw RequestError(null, "Could not connect to the server.")
            } finally {
                connection.disconnect()
            }
        }
        if (statusCode !in 200..299) {
            val detailText = serverErrorDetail(responseText)
            if (sessionTokenOverride == null) {
                if (statusCode == 401 && remoteSignOutOn401 && sessionToken.isNotEmpty()) {
                    handleRemoteSignOut()
                } else if (statusCode == 403 && detailText == "Email verification is required") {
                    emailVerificationRequired = true
                    accountVerificationFailed = false
                    remoteSignOutNotice = false
                    notifications = emptyList()
                    verificationMessage = ""
                    resetNavigationHierarchy()
                }
            }
            throw RequestError(statusCode, detailText)
        }
        return runCatching { JSONObject(responseText) }.getOrElse {
            throw RequestError(statusCode, "The server did not return a valid HTTP response.")
        }
    }

    private fun serverErrorDetail(text: String): String {
        val json = runCatching { JSONObject(text) }.getOrNull() ?: return "Unknown server error"
        val detail = json.opt("detail")
        return if (detail is String) {
            detail
        } else if (detail is JSONObject) {
            detail.optStringOrNull("message") ?: "Server error"
        } else {
            "Unknown server error"
        }
    }

    private suspend fun receiveNotificationForOtherAccount(
        otherAccountID: String,
        payload: JSONObject,
        openedByUser: Boolean,
    ): PushHandlingResult {
        val otherAccount = savedAccounts.firstOrNull { it.accountID == otherAccountID }
        val event = PushEvent.from(payload.optString("event"))
        if (event != null) {
            if (otherAccount != null && (event == PushEvent.SIGNED_OUT || event == PushEvent.ACCOUNT_DELETED)) {
                removeSavedAccount(otherAccount.sessionToken)
            }
            return PushHandlingResult.REMOTE_EVENT
        }
        if (!openedByUser || otherAccount == null) return PushHandlingResult.IGNORED
        switchToAccount(otherAccount)
        return PushHandlingResult.REFRESHED
    }

    private fun handlePushEvent(event: PushEvent) {
        when (event) {
            PushEvent.SIGNED_OUT, PushEvent.ACCOUNT_DELETED -> handleRemoteSignOut()
            PushEvent.DEVICES_UPDATED -> devicesRefreshToken++
            PushEvent.API_KEYS_UPDATED -> apiKeysRefreshToken++
            PushEvent.NOTIFICATIONS_UPDATED -> notificationsRefreshToken++
            PushEvent.EMAIL_VERIFIED, PushEvent.EMAIL_VERIFICATION_REQUIRED ->
                scope.launch { verifyStoredSession(showLoadingState = false) }
        }
    }

    private fun resetNavigationHierarchy() {
        navigationResetToken++
    }

    @SuppressLint("MissingPermission")
    private fun startNetworkMonitoring() {
        // Only the callback's own arguments are trustworthy: while the app is
        // in the background Android may block its network, and activeNetwork
        // then reads as null even though the device is online.
        val connectivity = app.getSystemService(ConnectivityManager::class.java)
        fun update(connected: Boolean) {
            scope.launch { hasInternetConnection = connected }
        }
        connectivity.registerDefaultNetworkCallback(object : ConnectivityManager.NetworkCallback() {
            override fun onCapabilitiesChanged(network: Network, networkCapabilities: NetworkCapabilities) =
                update(networkCapabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET))
            override fun onLost(network: Network) = update(false)
            override fun onUnavailable() = update(false)
        })
    }

    /** Runs [block] on the model's scope; for callers without one, like the push service. */
    fun launch(block: suspend () -> Unit) {
        scope.launch { block() }
    }

    /** Loads devices and API keys together, as the Account Settings screen does on iOS. */
    suspend fun loadDevicesAndAPIKeys(): Pair<List<AccountDevice>, List<ManagedAPIKey>> {
        val devices = scope.async { loadAccountDevices() }
        val keys = scope.async { loadAPIKeys() }
        return devices.await() to keys.await()
    }
}

enum class SoundsNavigation { SOUNDS, ADD_SOUNDS }
