package com.matthewdovi.mgnotify.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.matthewdovi.mgnotify.AppRelease
import com.matthewdovi.mgnotify.BuildConfig
import com.matthewdovi.mgnotify.Updater

/** "2.1", or "2.0 build 2" when it's a new build of the version that's installed. */
private fun releaseLabel(release: AppRelease): String =
    if (release.versionName == BuildConfig.VERSION_NAME) "${release.versionName} build ${release.versionCode}" else release.versionName

private val installedLabel = "${BuildConfig.VERSION_NAME} build ${BuildConfig.VERSION_CODE}"

private fun releaseText(release: AppRelease): String {
    val notes = release.notes.trim()
    val installed = if (release.versionName == BuildConfig.VERSION_NAME) installedLabel else BuildConfig.VERSION_NAME
    val intro = "MG Notify ${releaseLabel(release)} is available. You have $installed."
    return if (notes.isEmpty()) intro else "$intro\n\n$notes"
}

/**
 * The update dialogs, over whatever screen is showing: the offer after an
 * automatic check, then the download, the install permission and errors.
 */
@Composable
fun UpdateDialogs(updater: Updater) {
    val context = LocalContext.current
    when (val state = updater.state) {
        is Updater.State.Available -> if (updater.showPrompt) {
            AlertDialog(
                onDismissRequest = { updater.showPrompt = false },
                title = { Text("Update available") },
                text = { Text(releaseText(state.release)) },
                confirmButton = { TextButton(onClick = { updater.install(state.release) }) { Text("Update") } },
                dismissButton = { TextButton(onClick = { updater.showPrompt = false }) { Text("Later") } },
            )
        }
        is Updater.State.NeedsPermission -> AlertDialog(
            onDismissRequest = { updater.cancel() },
            title = { Text("Allow updates") },
            text = {
                Text("To update itself, MG Notify needs permission to install apps. Allow it on the next screen (\"Always allow\" or \"Allow from this source\", depending on your Android version), then come back.")
            },
            confirmButton = {
                TextButton(onClick = { context.startActivity(updater.installPermissionIntent()) }) { Text("Open Settings") }
            },
            dismissButton = { TextButton(onClick = { updater.cancel() }) { Text("Cancel") } },
        )
        is Updater.State.Downloading -> AlertDialog(
            onDismissRequest = {},
            title = { Text("Updating MG Notify") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    val percent = (state.fraction * 100).toInt()
                    Text(
                        "Downloading version ${releaseLabel(state.release)}… $percent%",
                        modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
                    )
                    LinearProgressIndicator(progress = { state.fraction }, modifier = Modifier.fillMaxWidth())
                }
            },
            confirmButton = {},
            dismissButton = { TextButton(onClick = { updater.cancel() }) { Text("Cancel") } },
        )
        is Updater.State.Installing -> AlertDialog(
            onDismissRequest = {},
            title = { Text("Updating MG Notify") },
            text = {
                Text(
                    "Installing version ${releaseLabel(state.release)}. Confirm when Android asks; MG Notify restarts when it's done.",
                    modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
                )
            },
            confirmButton = {},
        )
        is Updater.State.Failed -> AlertDialog(
            onDismissRequest = { updater.dismissFailure() },
            title = { Text("Update failed") },
            text = { Text(state.message) },
            confirmButton = { TextButton(onClick = { updater.dismissFailure() }) { Text("OK") } },
        )
        else -> Unit
    }
}

/** The Updates section of About: what's installed, checking, and the automatic-check switch. */
@Composable
fun UpdatesSection(updater: Updater) {
    var automatic by remember { mutableStateOf(updater.automaticChecks) }
    val status = when (val state = updater.state) {
        Updater.State.Checking -> "Checking for updates…"
        Updater.State.UpToDate -> "MG Notify is up to date."
        is Updater.State.Available -> "Version ${releaseLabel(state.release)} is available."
        else -> null
    }
    GroupedSection(header = "Updates", footer = "Updates come from notify.mad-gamer.com. Android asks you to confirm each one.") {
        if (status != null) {
            Text(
                status,
                color = secondaryText(),
                modifier = Modifier.padding(RowPadding).semantics { liveRegion = LiveRegionMode.Polite },
            )
            RowDivider()
        }
        val available = updater.state as? Updater.State.Available
        if (available != null) {
            // Through the prompt, so the release notes are seen first.
            TextRow("Update to ${releaseLabel(available.release)}…") { updater.showPrompt = true }
        } else {
            TextRow("Check for Updates", enabled = updater.state != Updater.State.Checking) { updater.check() }
        }
        RowDivider()
        Column(Modifier.padding(horizontal = 16.dp)) {
            SwitchRow("Check for updates automatically", automatic, {
                automatic = it
                updater.automaticChecks = it
            })
        }
    }
}
