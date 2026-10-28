package com.matthewdovi.mgnotify.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import com.matthewdovi.mgnotify.AccountDevice
import com.matthewdovi.mgnotify.MGNotifyModel
import com.matthewdovi.mgnotify.Navigator
import com.matthewdovi.mgnotify.Screen
import com.matthewdovi.mgnotify.ManagedAPIKey
import kotlinx.coroutines.CancellationException

@Composable
fun AccountSettingsScreen(model: MGNotifyModel, navigator: Navigator) {
    val context = LocalContext.current
    var devices by remember { mutableStateOf<List<AccountDevice>>(emptyList()) }
    var apiKeys by remember { mutableStateOf<List<ManagedAPIKey>>(emptyList()) }
    var apiKeyLabel by remember { mutableStateOf("Default") }
    var showingCreateAPIKeyPrompt by remember { mutableStateOf(false) }
    var newEmail by rememberSaveable { mutableStateOf("") }
    var emailPassword by remember { mutableStateOf("") }
    var currentPassword by remember { mutableStateOf("") }
    var newPassword by remember { mutableStateOf("") }
    var deletePassword by remember { mutableStateOf("") }
    var errorMessage by remember { mutableStateOf("") }
    var isWorking by remember { mutableStateOf(false) }
    var showDevicesSection by rememberSaveable { mutableStateOf(false) }
    var showAPIKeysSection by rememberSaveable { mutableStateOf(false) }
    var showChangeEmailSection by rememberSaveable { mutableStateOf(false) }
    var showDisplayNameSection by rememberSaveable { mutableStateOf(false) }
    var displayNameText by rememberSaveable { mutableStateOf(model.displayName) }
    var showChangePasswordSection by rememberSaveable { mutableStateOf(false) }
    var showDeleteAccountSection by rememberSaveable { mutableStateOf(false) }
    var showEmailPassword by remember { mutableStateOf(false) }
    var showChangePasswordFields by remember { mutableStateOf(false) }
    var showDeletePassword by remember { mutableStateOf(false) }
    var showingSignOutOtherDevicesConfirmation by remember { mutableStateOf(false) }
    var showingDeleteAllAPIKeysConfirmation by remember { mutableStateOf(false) }
    val otherDevices = devices.filter { it.deviceID != model.deviceID }

    fun setErrorMessage(error: Exception) {
        if (error is CancellationException) return
        errorMessage = error.message ?: "Unknown server error"
    }

    suspend fun reloadData() {
        if (isWorking) return
        isWorking = true
        try {
            val (loadedDevices, loadedKeys) = model.loadDevicesAndAPIKeys()
            devices = loadedDevices
            apiKeys = loadedKeys
            errorMessage = ""
        } catch (error: Exception) {
            setErrorMessage(error)
        } finally {
            isWorking = false
        }
    }

    fun performAction(action: suspend () -> Unit) {
        if (isWorking) return
        isWorking = true
        model.launch {
            try {
                action()
                errorMessage = ""
                if (model.isAuthenticated && !model.emailVerificationRequired) {
                    devices = model.loadAccountDevices()
                    apiKeys = model.loadAPIKeys()
                }
            } catch (error: Exception) {
                setErrorMessage(error)
            } finally {
                isWorking = false
            }
        }
    }

    LaunchedEffect(Unit) { reloadData() }
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { model.launch { reloadData() } }
    LaunchedEffect(model.devicesRefreshToken) {
        if (isWorking || !model.isAuthenticated || model.emailVerificationRequired) return@LaunchedEffect
        try {
            devices = model.loadAccountDevices()
            errorMessage = ""
        } catch (error: Exception) {
            setErrorMessage(error)
        }
    }
    LaunchedEffect(model.apiKeysRefreshToken) {
        if (isWorking || !model.isAuthenticated || model.emailVerificationRequired) return@LaunchedEffect
        try {
            apiKeys = model.loadAPIKeys()
            errorMessage = ""
        } catch (error: Exception) {
            setErrorMessage(error)
        }
    }

    ListScreen("Account Settings", navigator) {
        if (errorMessage.isNotEmpty()) {
            item {
                GroupedSection {
                    AnnouncedText(errorMessage, MaterialTheme.colorScheme.error, Modifier.padding(RowPadding))
                }
            }
        }

        item {
            DisclosureSection("Devices", showDevicesSection, { showDevicesSection = it }) {
                if (otherDevices.isEmpty()) {
                    Text("No other devices registered.", color = secondaryText(), modifier = Modifier.padding(vertical = 8.dp))
                } else {
                    TextRow("Sign Out Other Devices", horizontalPadding = 0.dp, color = MaterialTheme.colorScheme.error) {
                        showingSignOutOtherDevicesConfirmation = true
                    }
                    otherDevices.forEach { device ->
                        ActionRow(
                            actions = listOf(
                                RowAction("Sign Out", destructive = true, swipe = true) {
                                    performAction {
                                        model.signOutManagedDevice(device.deviceID)
                                        devices = devices.filter { it.deviceID != device.deviceID }
                                    }
                                },
                            ),
                            shape = RoundedCornerShape(8.dp),
                        ) {
                            Column(Modifier.fillMaxWidth().padding(vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                                Text(device.deviceName, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
                                Text(
                                    "${device.platform} • ${device.lastIPAddress}",
                                    style = MaterialTheme.typography.bodyMedium,
                                    color = secondaryText(),
                                )
                                Text(device.formattedLastSeen, style = MaterialTheme.typography.bodySmall, color = secondaryText())
                            }
                        }
                    }
                }
            }
        }

        item {
            DisclosureSection("API Keys", showAPIKeysSection, { showAPIKeysSection = it }) {
                TextRow("Create API Key", horizontalPadding = 0.dp) {
                    apiKeyLabel = "Default"
                    showingCreateAPIKeyPrompt = true
                }
                if (apiKeys.isEmpty()) {
                    Text("No API keys yet.", color = secondaryText(), modifier = Modifier.padding(vertical = 8.dp))
                } else {
                    TextRow("Delete All API Keys", horizontalPadding = 0.dp, color = MaterialTheme.colorScheme.error) {
                        showingDeleteAllAPIKeysConfirmation = true
                    }
                    apiKeys.forEach { apiKey ->
                        ActionRow(
                            actions = listOf(
                                RowAction("Copy", swipe = true) { copyToClipboard(context, apiKey.apiKey) },
                                RowAction("Delete", destructive = true) {
                                    performAction {
                                        model.deleteManagedAPIKey(apiKey.keyID)
                                        apiKeys = apiKeys.filter { it.keyID != apiKey.keyID }
                                    }
                                },
                            ),
                            shape = RoundedCornerShape(8.dp),
                        ) {
                            Column(Modifier.fillMaxWidth().padding(vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                                Text(apiKey.label, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
                                SelectionContainer {
                                    Text(apiKey.apiKey, style = MaterialTheme.typography.bodySmall, fontFamily = FontFamily.Monospace)
                                }
                            }
                        }
                    }
                }
            }
        }

        item {
            DisclosureSection("Display Name", showDisplayNameSection, { showDisplayNameSection = it }) {
                OutlinedTextField(
                    value = displayNameText,
                    onValueChange = { if (it.length <= 60) displayNameText = it },
                    label = { Text("Display name (optional)") },
                    supportingText = { Text("Shown instead of your email in the account menu and in announcements.") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                TextRow("Save Display Name", horizontalPadding = 0.dp, enabled = displayNameText.trim() != model.displayName) {
                    performAction { model.setDisplayName(displayNameText) }
                }
            }
        }

        item {
            DisclosureSection("Change Email", showChangeEmailSection, { showChangeEmailSection = it }) {
                OutlinedTextField(
                    value = newEmail,
                    onValueChange = { newEmail = it },
                    label = { Text("New email") },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Email, autoCorrectEnabled = false),
                    modifier = Modifier.fillMaxWidth(),
                )
                PasswordField("Current password", emailPassword, { emailPassword = it }, showEmailPassword)
                SwitchRow("Show Password", showEmailPassword, { showEmailPassword = it })
                TextRow("Change Email", horizontalPadding = 0.dp, enabled = newEmail.isNotBlank() && emailPassword.isNotEmpty()) {
                    performAction { model.changeAccountEmail(newEmail, emailPassword) }
                }
            }
        }

        item {
            DisclosureSection("Change Password", showChangePasswordSection, { showChangePasswordSection = it }) {
                PasswordField("Current password", currentPassword, { currentPassword = it }, showChangePasswordFields)
                PasswordField("New password", newPassword, { newPassword = it }, showChangePasswordFields)
                SwitchRow("Show Password", showChangePasswordFields, { showChangePasswordFields = it })
                TextRow("Change Password", horizontalPadding = 0.dp, enabled = currentPassword.isNotEmpty() && newPassword.isNotEmpty()) {
                    performAction { model.changeAccountPassword(currentPassword, newPassword) }
                }
            }
        }

        item {
            DisclosureSection("Delete Account", showDeleteAccountSection, { showDeleteAccountSection = it }) {
                PasswordField("Confirm password", deletePassword, { deletePassword = it }, showDeletePassword)
                SwitchRow("Show Password", showDeletePassword, { showDeletePassword = it })
                TextRow("Delete Account", horizontalPadding = 0.dp, color = MaterialTheme.colorScheme.error, enabled = deletePassword.isNotEmpty()) {
                    performAction { model.deleteAccount(deletePassword) }
                }
            }
        }
    }

    if (showingCreateAPIKeyPrompt) {
        AlertDialog(
            onDismissRequest = { showingCreateAPIKeyPrompt = false },
            title = { Text("Create API Key") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    Text("Enter a label for this API key.")
                    OutlinedTextField(
                        value = apiKeyLabel,
                        onValueChange = { apiKeyLabel = it },
                        label = { Text("Key label") },
                        singleLine = true,
                    )
                }
            },
            confirmButton = {
                TextButton(onClick = {
                    showingCreateAPIKeyPrompt = false
                    val label = apiKeyLabel.trim().ifEmpty { "Default" }
                    performAction {
                        val createdKey = model.createManagedAPIKey(label)
                        apiKeys = listOf(createdKey) + apiKeys
                    }
                }) { Text("Create") }
            },
            dismissButton = { TextButton(onClick = { showingCreateAPIKeyPrompt = false }) { Text("Cancel") } },
        )
    }
    if (showingSignOutOtherDevicesConfirmation) {
        ConfirmationDialog(
            title = "Are you sure you wish to sign out other devices?",
            confirmLabel = "Sign Out Other Devices",
            onConfirm = {
                performAction {
                    model.signOutOtherManagedDevices()
                    devices = devices.filter { it.deviceID == model.deviceID }
                }
            },
            onDismiss = { showingSignOutOtherDevicesConfirmation = false },
        )
    }
    if (showingDeleteAllAPIKeysConfirmation) {
        ConfirmationDialog(
            title = "Are you sure you wish to delete all API keys?",
            confirmLabel = "Delete All API Keys",
            onConfirm = {
                performAction {
                    model.deleteAllManagedAPIKeys()
                    apiKeys = emptyList()
                }
            },
            onDismiss = { showingDeleteAllAPIKeysConfirmation = false },
        )
    }
}

@Composable
private fun PasswordField(label: String, value: String, onValueChange: (String) -> Unit, visible: Boolean) {
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        label = { Text(label) },
        singleLine = true,
        visualTransformation = if (visible) VisualTransformation.None else PasswordVisualTransformation(),
        keyboardOptions = KeyboardOptions(
            keyboardType = if (visible) KeyboardType.Text else KeyboardType.Password,
            autoCorrectEnabled = false,
        ),
        modifier = Modifier.fillMaxWidth(),
    )
}
