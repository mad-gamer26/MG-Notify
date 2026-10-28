package com.matthewdovi.mgnotify.ui

import android.annotation.SuppressLint
import android.view.ViewGroup
import android.webkit.WebResourceRequest
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import com.matthewdovi.mgnotify.AccountNotification
import com.matthewdovi.mgnotify.BuildConfig
import com.matthewdovi.mgnotify.MGNotifyModel
import com.matthewdovi.mgnotify.Navigator
import com.matthewdovi.mgnotify.Screen
import com.matthewdovi.mgnotify.SavedAccount

/** A pushed screen: back button, title, and a scrolling list of sections. */
@Composable
fun ListScreen(
    title: String,
    navigator: Navigator,
    actions: @Composable androidx.compose.foundation.layout.RowScope.() -> Unit = {},
    content: androidx.compose.foundation.lazy.LazyListScope.() -> Unit,
) {
    Scaffold(
        containerColor = Color.Transparent,
        topBar = { MGTopBar(title, navigator, actions = actions) },
    ) { padding ->
        LazyColumn(
            Modifier.padding(padding).fillMaxSize(),
            contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 4.dp, bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(20.dp),
            content = content,
        )
    }
}

/** A row that opens another screen, with an optional second line (iOS NavigationLink). */
@Composable
fun NavigationRow(title: String, subtitle: String? = null, onClick: () -> Unit) {
    ActionRow(actions = emptyList(), onClick = onClick, background = Color.Transparent) {
        Column(Modifier.fillMaxWidth().heightIn(min = 48.dp).padding(RowPadding), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(title, style = MaterialTheme.typography.bodyLarge)
            if (subtitle != null) Text(subtitle, style = MaterialTheme.typography.bodySmall, color = secondaryText())
        }
    }
}

@Composable
fun SettingsScreen(model: MGNotifyModel, navigator: Navigator) {
    var showingSignOutConfirmation by remember { mutableStateOf(false) }
    ListScreen("Settings", navigator) {
        item {
            GroupedSection {
                NavigationRow("Notification Sound", model.selectedSoundTitle()) { navigator.push(Screen.Sounds) }
                RowDivider()
                NavigationRow("Accounts", model.email) { navigator.push(Screen.Accounts) }
                RowDivider()
                NavigationRow("Account Settings") { navigator.push(Screen.AccountSettings) }
                RowDivider()
                TextRow("Sign Out", color = MaterialTheme.colorScheme.error) { showingSignOutConfirmation = true }
                RowDivider()
                NavigationRow("About") { navigator.push(Screen.About) }
            }
        }
    }
    if (showingSignOutConfirmation) {
        ConfirmationDialog(
            title = "Are you sure you wish to sign out?",
            confirmLabel = "Sign Out",
            onConfirm = { model.launch { model.logout() } },
            onDismiss = { showingSignOutConfirmation = false },
        )
    }
}

@Composable
fun AccountsScreen(model: MGNotifyModel, navigator: Navigator) {
    var accountPendingSignOut by remember { mutableStateOf<SavedAccount?>(null) }
    ListScreen("Accounts", navigator) {
        item {
            GroupedSection(footer = "This device receives notifications for every account you are signed in to. Tap an account to switch to it.") {
                model.savedAccounts.forEachIndexed { index, account ->
                    val active = model.isActiveAccount(account)
                    ActionRow(
                        actions = listOf(RowAction("Sign Out", destructive = true, swipe = true) { accountPendingSignOut = account }),
                        shape = groupedItemShape(index, model.savedAccounts.size),
                        onClick = { if (!active) model.launch { model.switchToAccount(account) } },
                        selected = active,
                    ) {
                        Column {
                            Row(
                                Modifier.fillMaxWidth().heightIn(min = 48.dp).padding(RowPadding),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                AccountName(account, Modifier.weight(1f))
                                if (active) Icon(Icons.Filled.Check, contentDescription = null, tint = MaterialTheme.colorScheme.primary)
                            }
                            if (index < model.savedAccounts.lastIndex) RowDivider()
                        }
                    }
                }
            }
        }
        item {
            GroupedSection {
                ActionRow(actions = emptyList(), onClick = { model.startAddingAccount() }, background = Color.Transparent) {
                    Row(Modifier.fillMaxWidth().heightIn(min = 48.dp).padding(RowPadding), verticalAlignment = Alignment.CenterVertically) {
                        Icon(Icons.Filled.Add, contentDescription = null, tint = MaterialTheme.colorScheme.primary)
                        Text("Add Account", color = MaterialTheme.colorScheme.primary, modifier = Modifier.padding(start = 12.dp))
                    }
                }
            }
        }
    }
    accountPendingSignOut?.let { account ->
        ConfirmationDialog(
            title = "Are you sure you wish to sign out of ${account.email}?",
            confirmLabel = "Sign Out",
            onConfirm = { model.launch { model.signOutSavedAccount(account) } },
            onDismiss = { accountPendingSignOut = null },
        )
    }
}

@Composable
fun AboutScreen(model: MGNotifyModel, navigator: Navigator) {
    val context = LocalContext.current
    ListScreen("About", navigator) {
        if (BuildConfig.UPDATES_URL.isNotEmpty()) {
            item { UpdatesSection(model.updater) }
        }
        item {
            GroupedSection {
                Column(Modifier.padding(RowPadding), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text("MG Notify", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
                    Text("Version ${BuildConfig.VERSION_NAME} Build ${BuildConfig.VERSION_CODE}", color = secondaryText())
                }
            }
        }
        item {
            GroupedSection {
                TextRow("Open Web Portal") { openExternalLink(context, BuildConfig.PORTAL_URL) }
                RowDivider()
                TextRow("Email Developer") { openExternalLink(context, "mailto:mattdovi@icloud.com") }
            }
        }
    }
}

@Composable
fun EmailDetailScreen(notification: AccountNotification, navigator: Navigator) {
    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            MGTopBar(
                title = notification.emailSubject,
                navigationIcon = {
                    IconButton(onClick = { navigator.pop() }) { Icon(Icons.Filled.Close, contentDescription = "Close") }
                },
            )
        },
    ) { padding ->
        Column(Modifier.padding(padding).fillMaxSize()) {
            GroupedSection(Modifier.padding(16.dp), color = MaterialTheme.colorScheme.surfaceContainerHigh) {
                Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    EmailDetailRow("Sender Name", notification.emailSenderName)
                    EmailDetailRow("Sender Address", notification.emailSenderAddress.ifEmpty { "Unknown" })
                    EmailDetailRow("Subject", notification.emailSubject)
                    EmailDetailRow("Date", notification.formattedDate)
                }
            }
            HorizontalDivider()
            EmailWebView(notification.emailHTML, Modifier.fillMaxSize())
        }
    }
}

@Composable
private fun EmailDetailRow(label: String, value: String) {
    Column(
        Modifier.fillMaxWidth().clearAndSetSemantics { contentDescription = "$label, $value" },
        verticalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        Text(label, style = MaterialTheme.typography.bodySmall, color = secondaryText())
        SelectionContainer { Text(value, style = MaterialTheme.typography.bodyLarge) }
    }
}

/** Shows an email's HTML; tapped links open in the browser or mail app. */
@SuppressLint("SetJavaScriptEnabled")
@Composable
private fun EmailWebView(html: String, modifier: Modifier = Modifier) {
    val document = remember(html) { emailHTMLDocument(html) }
    val background = MaterialTheme.colorScheme.background.toArgb()
    AndroidView(
        modifier = modifier,
        factory = { context ->
            WebView(context).apply {
                layoutParams = ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT)
                settings.javaScriptEnabled = false
                settings.allowFileAccess = false
                setBackgroundColor(background)
                webViewClient = object : WebViewClient() {
                    override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean {
                        openExternalLink(view.context, request.url.toString())
                        return true
                    }
                }
                tag = document
                loadDataWithBaseURL(null, document, "text/html", "utf-8", null)
            }
        },
        update = { webView ->
            if (webView.tag != document) {
                webView.tag = document
                webView.loadDataWithBaseURL(null, document, "text/html", "utf-8", null)
            }
        },
    )
}

private fun emailHTMLDocument(html: String): String {
    val trimmed = html.trim()
    val lowercased = trimmed.lowercase()
    if ("<html" in lowercased || "<!doctype html" in lowercased) return trimmed
    return """
        <!doctype html>
        <html>
        <head>
        <meta charset="utf-8">
        <meta name="viewport" content="width=device-width, initial-scale=1.0">
        <style>
        :root { color-scheme: light dark; }
        html { -webkit-text-size-adjust: 100%; background: transparent; }
        body {
            margin: 16px;
            overflow-wrap: anywhere;
            background: transparent;
            color: CanvasText;
            font-family: sans-serif;
            font-size: 1rem;
            line-height: 1.4;
        }
        img, video, iframe { max-width: 100%; height: auto; }
        table { max-width: 100%; }
        pre { white-space: pre-wrap; }
        </style>
        </head>
        <body>
        $trimmed
        </body>
        </html>
    """.trimIndent()
}
