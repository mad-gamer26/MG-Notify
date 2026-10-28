package com.matthewdovi.mgnotify.ui

import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.isImeVisible
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.ArrowDropDown
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Clear
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TextField
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.saveable.rememberSaveableStateHolder
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.platform.UriHandler
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.matthewdovi.mgnotify.AccountNotification
import com.matthewdovi.mgnotify.AuthScreenMode
import com.matthewdovi.mgnotify.MGNotifyModel
import com.matthewdovi.mgnotify.Navigator
import com.matthewdovi.mgnotify.Screen
import com.matthewdovi.mgnotify.SoundsNavigation

/** Opens links from notifications with whatever app handles them, ignoring ones nothing can open. */
fun openExternalLink(context: Context, url: String) {
    try {
        context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    } catch (_: ActivityNotFoundException) {
    } catch (_: SecurityException) {
    }
}

fun copyToClipboard(context: Context, plain: String, html: String? = null) {
    val clipboard = context.getSystemService(ClipboardManager::class.java)
    val clip = if (html != null) ClipData.newHtmlText("MG Notify", plain, html) else ClipData.newPlainText("MG Notify", plain)
    clipboard.setPrimaryClip(clip)
}

@Composable
fun MGNotifyApp(model: MGNotifyModel) {
    val context = LocalContext.current
    val navigator = model.navigator
    val stateHolder = rememberSaveableStateHolder()
    val uriHandler = remember(context) {
        object : UriHandler {
            override fun openUri(uri: String) = openExternalLink(context, uri)
        }
    }
    var isStarted by remember { mutableStateOf(false) }
    var hasStartedOnce by remember { mutableStateOf(false) }

    LaunchedEffect(Unit) {
        model.performLaunchChecksIfNeeded()
    }
    LifecycleEventEffect(Lifecycle.Event.ON_START) {
        isStarted = true
        model.updater.checkAutomaticallyIfDue()
        model.refreshPermissionStatus()
        model.reloadImportedSounds()
        if (hasStartedOnce) {
            model.launch { model.refreshServerAndSession() }
        }
        hasStartedOnce = true
    }
    // Back from allowing MG Notify to install apps: carry on with the update.
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { model.updater.resumeAfterPermission() }
    LifecycleEventEffect(Lifecycle.Event.ON_STOP) {
        isStarted = false
        model.forgetNewMarkers()
        model.stopLiveUpdates()
    }
    LaunchedEffect(model.hasInternetConnection) {
        if (model.hasInternetConnection) {
            if (isStarted && hasStartedOnce) model.retryServerConnection()
        } else {
            model.stopLiveUpdates()
        }
    }
    LaunchedEffect(model.navigationResetToken) {
        navigator.reset()
    }
    LaunchedEffect(model.pendingSoundsNavigation) {
        val target = model.pendingSoundsNavigation ?: return@LaunchedEffect
        model.pendingSoundsNavigation = null
        navigator.reset()
        if (target == SoundsNavigation.SOUNDS) model.reloadImportedSounds()
        navigator.push(if (target == SoundsNavigation.ADD_SOUNDS) Screen.AddSounds else Screen.Sounds)
    }
    BackHandler(enabled = navigator.stack.isNotEmpty()) { navigator.pop() }

    MGNotifyTheme {
        CompositionLocalProvider(LocalUriHandler provides uriHandler) {
            Box(Modifier.fillMaxSize()) {
                BackgroundGradient()
                val top = navigator.stack.lastOrNull()
                val key = top?.let { "${navigator.stack.size}:$it" } ?: "root"
                stateHolder.SaveableStateProvider(key) {
                    when (top) {
                        null -> RootScreen(model, navigator)
                        Screen.Settings -> SettingsScreen(model, navigator)
                        Screen.Sounds -> SoundsScreen(model, navigator)
                        Screen.AddSounds -> AddSoundsScreen(model, navigator)
                        Screen.Accounts -> AccountsScreen(model, navigator)
                        Screen.AccountSettings -> AccountSettingsScreen(model, navigator)
                        Screen.About -> AboutScreen(model, navigator)
                        is Screen.EmailDetail -> EmailDetailScreen(top.notification, navigator)
                    }
                }
                UpdateDialogs(model.updater)
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MGTopBar(
    title: String,
    navigator: Navigator? = null,
    navigationIcon: @Composable () -> Unit = { if (navigator != null) BackButton(navigator) },
    actions: @Composable androidx.compose.foundation.layout.RowScope.() -> Unit = {},
) {
    TopAppBar(
        title = { Text(title, maxLines = 1, modifier = Modifier.semantics { heading() }) },
        navigationIcon = navigationIcon,
        actions = actions,
        colors = TopAppBarDefaults.topAppBarColors(containerColor = Color.Transparent),
    )
}

@Composable
fun BackButton(navigator: Navigator) {
    IconButton(onClick = { navigator.pop() }) {
        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
    }
}

@Composable
private fun RootScreen(model: MGNotifyModel, navigator: Navigator) {
    var showingClearConfirmation by remember { mutableStateOf(false) }
    val signedIn = model.hasInternetConnection && !model.isVerifyingStoredSession && !model.remoteSignOutNotice &&
        !model.accountVerificationFailed && model.isAuthenticated && !model.emailVerificationRequired

    Scaffold(
        containerColor = Color.Transparent,
        topBar = {
            MGTopBar(
                title = "MG Notify",
                actions = {
                    if (signedIn) {
                        if (model.notifications.isNotEmpty()) {
                            IconButton(onClick = { showingClearConfirmation = true }) {
                                Icon(Icons.Filled.Delete, contentDescription = "Delete All Notifications")
                            }
                        }
                        IconButton(onClick = { navigator.push(Screen.Settings) }) {
                            Icon(Icons.Filled.Settings, contentDescription = "Settings")
                        }
                    }
                },
            )
        },
    ) { padding ->
        Box(Modifier.padding(padding).fillMaxSize()) {
            when {
                !model.hasInternetConnection -> MessageScreen(
                    title = null,
                    message = "An internet connection is required to use MG Notify. Please connect to the internet and try again.",
                )
                model.isVerifyingStoredSession -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    CircularProgressIndicator()
                }
                model.remoteSignOutNotice -> MessageScreen(
                    title = "You have been signed out.",
                    message = "Please log in again.",
                    buttonTitle = "OK",
                    busy = model.isBusy,
                    onButton = { model.forgetLocalAccountData() },
                )
                model.accountVerificationFailed -> MessageScreen(
                    title = "Server unreachable",
                    message = "MG Notify could not reach the server. Please try again.",
                    buttonTitle = "Try Again",
                    busy = model.isBusy,
                    onButton = { model.launch { model.retryServerConnection() } },
                )
                model.isAuthenticated && model.emailVerificationRequired -> EmailVerificationRequiredView(model)
                model.isAuthenticated -> SignedInView(model, navigator)
                else -> LoginView(model)
            }
        }
    }

    if (showingClearConfirmation) {
        ConfirmationDialog(
            title = "Are you sure you wish to delete all your notifications?",
            confirmLabel = "Delete All Notifications",
            onConfirm = { model.launch { model.clearNotifications() } },
            onDismiss = { showingClearConfirmation = false },
        )
    }
}

@Composable
private fun MessageScreen(
    title: String?,
    message: String,
    buttonTitle: String? = null,
    busy: Boolean = false,
    onButton: () -> Unit = {},
) {
    Column(
        Modifier.fillMaxSize().padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(20.dp, Alignment.CenterVertically),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        if (title != null) {
            Text(
                title,
                style = MaterialTheme.typography.titleLarge,
                fontWeight = FontWeight.SemiBold,
                textAlign = TextAlign.Center,
                modifier = Modifier.semantics { heading() },
            )
        }
        Text(message, textAlign = TextAlign.Center, color = secondaryText())
        if (buttonTitle != null) ActionButton(buttonTitle, busy = busy, onClick = onButton)
    }
}

@Composable
private fun EmailVerificationRequiredView(model: MGNotifyModel) {
    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(20.dp, Alignment.CenterVertically),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(
            "Verify your email",
            style = MaterialTheme.typography.titleLarge,
            fontWeight = FontWeight.SemiBold,
            modifier = Modifier.semantics { heading() },
        )
        Text("You need to verify your email in order to use MG Notify.", textAlign = TextAlign.Center)
        Text(
            "Check the inbox for ${model.email} and follow the verification link.",
            textAlign = TextAlign.Center,
            color = secondaryText(),
        )
        if (model.verificationMessage.isNotEmpty()) {
            AnnouncedText(model.verificationMessage, secondaryText(), textAlign = TextAlign.Center)
        }
        if (model.authErrorMessage.isNotEmpty()) {
            AnnouncedText(model.authErrorMessage, MaterialTheme.colorScheme.error, textAlign = TextAlign.Center)
        }
        ActionButton("Resend Verification Email", busy = model.isBusy) {
            model.launch { model.resendVerificationEmail() }
        }
        TextButton(onClick = { model.launch { model.logout() } }) {
            Text("Sign Out", color = MaterialTheme.colorScheme.onSurface)
        }
    }
}

@Composable
private fun LoginView(model: MGNotifyModel) {
    var showPassword by rememberSaveable { mutableStateOf(false) }
    LaunchedEffect(model.isAuthenticated) { if (model.isAuthenticated) showPassword = false }
    val emailEntered = model.email.isNotBlank()

    Column(
        Modifier
            .fillMaxSize()
            .imePadding()
            .verticalScroll(rememberScrollState())
            .padding(20.dp),
        verticalArrangement = Arrangement.spacedBy(20.dp),
    ) {
        Text(
            if (model.isAddingAccount) "Add Account" else "Welcome to MG Notify",
            style = MaterialTheme.typography.headlineLarge,
            fontWeight = FontWeight.Bold,
            modifier = Modifier.semantics { heading() },
        )
        Surface(shape = CardShape, color = cardColor()) {
            Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(14.dp)) {
                SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
                    AuthScreenMode.entries.forEachIndexed { index, mode ->
                        SegmentedButton(
                            selected = model.authScreenMode == mode,
                            onClick = {
                                model.authScreenMode = mode
                                model.authErrorMessage = ""
                            },
                            shape = SegmentedButtonDefaults.itemShape(index, AuthScreenMode.entries.size),
                            icon = {},
                        ) { Text(mode.title, maxLines = 2, textAlign = TextAlign.Center, style = MaterialTheme.typography.labelMedium) }
                    }
                }
                OutlinedTextField(
                    value = model.email,
                    onValueChange = { model.email = it },
                    label = { Text("Email") },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Email, autoCorrectEnabled = false),
                    modifier = Modifier.fillMaxWidth(),
                )
                if (model.authScreenMode != AuthScreenMode.FORGOT_PASSWORD) {
                    OutlinedTextField(
                        value = model.password,
                        onValueChange = { model.password = it },
                        label = { Text("Password") },
                        singleLine = true,
                        visualTransformation = if (showPassword) VisualTransformation.None else PasswordVisualTransformation(),
                        keyboardOptions = KeyboardOptions(
                            keyboardType = if (showPassword) KeyboardType.Text else KeyboardType.Password,
                            autoCorrectEnabled = false,
                        ),
                        modifier = Modifier.fillMaxWidth(),
                    )
                    SwitchRow("Show Password", showPassword, { showPassword = it })
                    if (model.authScreenMode == AuthScreenMode.CREATE_ACCOUNT) {
                        Text(
                            "Your password must be at least 8 characters.",
                            style = MaterialTheme.typography.bodySmall,
                            color = secondaryText(),
                        )
                    }
                }
                if (model.authErrorMessage.isNotEmpty()) {
                    AnnouncedText(model.authErrorMessage, MaterialTheme.colorScheme.error)
                }
                when (model.authScreenMode) {
                    AuthScreenMode.SIGN_IN -> ActionButton(
                        "Sign In",
                        busy = model.isBusy,
                        enabled = emailEntered && model.password.isNotEmpty(),
                    ) { model.launch { model.login() } }
                    AuthScreenMode.CREATE_ACCOUNT -> ActionButton(
                        "Create Account",
                        busy = model.isBusy,
                        enabled = emailEntered && model.password.isNotEmpty(),
                    ) { model.launch { model.registerAccount() } }
                    AuthScreenMode.FORGOT_PASSWORD -> ActionButton(
                        "Send Reset Email",
                        busy = model.isBusy,
                        enabled = emailEntered,
                    ) { model.launch { model.requestPasswordReset() } }
                }
            }
        }
        if (model.isAddingAccount) {
            TextButton(
                onClick = { model.launch { model.cancelAddingAccount() } },
                enabled = !model.isBusy,
                modifier = Modifier.fillMaxWidth(),
            ) { Text("Cancel") }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun SignedInView(model: MGNotifyModel, navigator: Navigator) {
    val context = LocalContext.current
    var searchText by rememberSaveable { mutableStateOf("") }
    var searchFocused by remember { mutableStateOf(false) }
    var copyTarget by remember { mutableStateOf<AccountNotification?>(null) }
    val searchActive = searchText.isNotEmpty() || (searchFocused && WindowInsets.isImeVisible)
    val notifications = model.notifications
    val query = searchText.trim()
    val filtered = if (query.isEmpty()) notifications else notifications.filter {
        it.title.contains(query, ignoreCase = true) ||
            it.message.contains(query, ignoreCase = true) ||
            it.formattedDate.contains(query, ignoreCase = true)
    }

    LaunchedEffect(Unit) { model.fetchNotifications() }
    LaunchedEffect(model.notificationsRefreshToken) { model.fetchNotifications() }
    // The list is on screen: what's unread here counts as seen.
    LaunchedEffect(model.notifications) { model.scheduleMarkSeen() }
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { model.scheduleMarkSeen() }
    LaunchedEffect(notifications.size) { if (notifications.isEmpty()) searchText = "" }

    LazyColumn(
        Modifier.fillMaxSize(),
        contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 4.dp, bottom = 24.dp),
        verticalArrangement = Arrangement.spacedBy(0.dp),
    ) {
        if (notifications.isNotEmpty()) {
            item(key = "search") {
                SearchField(
                    text = searchText,
                    onTextChange = { searchText = it },
                    prompt = "Search notifications",
                    onFocusChange = { searchFocused = it },
                )
                Spacer(Modifier.padding(top = 12.dp))
            }
        }
        if (model.isNotificationPermissionDenied && !searchActive) {
            item(key = "permission") {
                PermissionBanner { model.openNotificationSettings(context) }
                Spacer(Modifier.padding(top = 16.dp))
            }
        }
        if (!searchActive) {
            item(key = "account") {
                AccountSwitcher(model)
                Spacer(Modifier.padding(top = 16.dp))
            }
        }
        if (notifications.isEmpty()) {
            item(key = "empty") {
                GroupedSection { Text("No notifications yet.", color = secondaryText(), modifier = Modifier.padding(RowPadding)) }
            }
        } else {
            if (!searchActive) {
                item(key = "count") {
                    val noun = if (notifications.size == 1) "notification." else "notifications."
                    GroupedSection {
                        Text(
                            "You currently have ${notifications.size} $noun",
                            color = secondaryText(),
                            modifier = Modifier.padding(RowPadding),
                        )
                    }
                    Spacer(Modifier.padding(top = 16.dp))
                }
                item(key = "header") { SectionHeader("Notifications") }
            }
            if (filtered.isEmpty()) {
                item(key = "nomatch") {
                    GroupedSection { Text("No matching notifications.", color = secondaryText(), modifier = Modifier.padding(RowPadding)) }
                }
            } else {
                itemsIndexed(filtered, key = { _, item -> item.notificationID }) { index, notification ->
                    // A full swipe runs the first of iOS's trailing actions: Copy, or Delete for emails.
                    val delete = RowAction("Delete", destructive = true, swipe = notification.isEmailNotification) {
                        model.launch { model.deleteNotification(notification.notificationID) }
                    }
                    val shape = groupedItemShape(index, filtered.size)
                    val divided = index < filtered.lastIndex
                    if (notification.isEmailNotification) {
                        ActionRow(
                            actions = listOf(delete),
                            shape = shape,
                            onClick = { navigator.push(Screen.EmailDetail(notification)) },
                            onClickLabel = "Open email",
                        ) { NotificationRow(notification, divided, notification.notificationID in model.newNotificationIDs) }
                    } else {
                        ActionRow(
                            actions = listOf(RowAction("Copy", swipe = true) { copyTarget = notification }, delete),
                            shape = shape,
                        ) { NotificationRow(notification, divided, notification.notificationID in model.newNotificationIDs) }
                    }
                }
            }
        }
    }

    copyTarget?.let { notification ->
        AlertDialog(
            onDismissRequest = { copyTarget = null },
            title = { Text("Copy Notification") },
            text = {
                Column {
                    TextButton(onClick = {
                        copyToClipboard(context, notification.plainTextForCopy, notification.richHTMLForCopy)
                        copyTarget = null
                    }, modifier = Modifier.fillMaxWidth()) { Text("Copy Rich Text") }
                    TextButton(onClick = {
                        copyToClipboard(context, notification.plainTextForCopy)
                        copyTarget = null
                    }, modifier = Modifier.fillMaxWidth()) { Text("Copy Plain Text") }
                }
            },
            confirmButton = {},
            dismissButton = { TextButton(onClick = { copyTarget = null }) { Text("Cancel") } },
        )
    }
}

@Composable
fun SearchField(text: String, onTextChange: (String) -> Unit, prompt: String, onFocusChange: (Boolean) -> Unit) {
    TextField(
        value = text,
        onValueChange = onTextChange,
        placeholder = { Text(prompt) },
        singleLine = true,
        leadingIcon = { Icon(Icons.Filled.Search, contentDescription = null) },
        trailingIcon = {
            if (text.isNotEmpty()) {
                IconButton(onClick = { onTextChange("") }) { Icon(Icons.Filled.Clear, contentDescription = "Clear search") }
            }
        },
        shape = CardShape,
        colors = TextFieldDefaults.colors(
            focusedIndicatorColor = Color.Transparent,
            unfocusedIndicatorColor = Color.Transparent,
            focusedContainerColor = cardColor(),
            unfocusedContainerColor = cardColor(),
        ),
        modifier = Modifier.fillMaxWidth().onFocusChanged { onFocusChange(it.isFocused) },
    )
}

@Composable
private fun PermissionBanner(onOpenSettings: () -> Unit) {
    val message = "You did not allow the notification permission, so you will not get push notifications. Go to Settings to enable."
    GroupedSection {
        Row(
            Modifier.padding(RowPadding),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(message, style = MaterialTheme.typography.bodySmall, modifier = Modifier.weight(1f))
            Button(onClick = onOpenSettings, contentPadding = PaddingValues(horizontal = 12.dp, vertical = 6.dp)) {
                Text("Go to Settings", style = MaterialTheme.typography.labelMedium)
            }
        }
    }
}

@Composable
private fun AccountSwitcher(model: MGNotifyModel) {
    var expanded by remember { mutableStateOf(false) }
    GroupedSection {
        Box {
            Row(
                Modifier
                    .fillMaxWidth()
                    .clickable(onClickLabel = "Switch accounts or add another account", role = Role.Button) { expanded = true }
                    .padding(horizontal = 16.dp, vertical = 14.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(Modifier.weight(1f)) {
                    Text("Account", style = MaterialTheme.typography.titleLarge, fontWeight = FontWeight.SemiBold)
                    Text(model.accountName, style = MaterialTheme.typography.bodyMedium, color = secondaryText())
                }
                Icon(Icons.Filled.ArrowDropDown, contentDescription = null, tint = secondaryText())
            }
            DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
                model.savedAccounts.forEach { account ->
                    val active = model.isActiveAccount(account)
                    DropdownMenuItem(
                        text = { AccountName(account) },
                        leadingIcon = {
                            if (active) Icon(Icons.Filled.Check, contentDescription = "Current account")
                            else Spacer(Modifier.width(24.dp))
                        },
                        onClick = {
                            expanded = false
                            if (!active) model.launch { model.switchToAccount(account) }
                        },
                    )
                }
                HorizontalDivider()
                DropdownMenuItem(
                    text = { Text("Add Account") },
                    leadingIcon = { Icon(Icons.Filled.Add, contentDescription = null) },
                    onClick = {
                        expanded = false
                        model.startAddingAccount()
                    },
                )
            }
        }
    }
}

/** Marks a notification that hadn't been seen when this visit to the app began. */
@Composable
private fun NewTag() {
    Surface(shape = RoundedCornerShape(50), color = MaterialTheme.colorScheme.primary) {
        Text(
            "New",
            style = MaterialTheme.typography.labelSmall,
            fontWeight = FontWeight.Bold,
            color = MaterialTheme.colorScheme.onPrimary,
            modifier = Modifier.padding(horizontal = 8.dp, vertical = 2.dp),
        )
    }
}

/** An account's display name, with its email beneath when it has one. */
@Composable
fun AccountName(account: com.matthewdovi.mgnotify.SavedAccount, modifier: Modifier = Modifier) {
    Column(modifier) {
        Text(account.name)
        if (account.displayName.isNotEmpty()) {
            Text(account.email, style = MaterialTheme.typography.bodySmall, color = secondaryText())
        }
    }
}

@Composable
private fun NotificationRow(notification: AccountNotification, divided: Boolean, isNew: Boolean) {
    Column {
        Column(Modifier.padding(RowPadding), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(
                    notification.displayTitle,
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.weight(1f, fill = false),
                )
                if (isNew) NewTag()
            }
            if (!notification.isEmailNotification) {
                Text(notification.parsedMessage.text, style = MaterialTheme.typography.bodyLarge)
            }
            Text(notification.formattedDate, style = MaterialTheme.typography.bodySmall, color = secondaryText())
        }
        if (divided) RowDivider()
    }
}
