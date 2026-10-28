package com.matthewdovi.mgnotify.ui

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.isImeVisible
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.matthewdovi.mgnotify.MGNotifyModel
import com.matthewdovi.mgnotify.Navigator
import com.matthewdovi.mgnotify.Screen
import com.matthewdovi.mgnotify.NotificationSounds
import java.io.File

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun SoundsScreen(model: MGNotifyModel, navigator: Navigator) {
    var showingDeleteAllSoundsConfirmation by remember { mutableStateOf(false) }
    var searchText by rememberSaveable { mutableStateOf("") }
    var searchFocused by remember { mutableStateOf(false) }
    var didPerformInitialScroll by rememberSaveable { mutableStateOf(false) }
    val listState = rememberLazyListState()
    val searchActive = searchText.isNotEmpty() || (searchFocused && WindowInsets.isImeVisible)
    val options = model.soundOptions()
    val searchable = if (searchActive) {
        options.filter { it.id != NotificationSounds.NO_SOUND && it.id != NotificationSounds.SYSTEM_DEFAULT }
    } else {
        options
    }
    val query = searchText.trim()
    val filtered = if (query.isEmpty()) searchable else searchable.filter { it.title.contains(query, ignoreCase = true) }

    val importer = rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { uris ->
        if (uris.isEmpty()) return@rememberLauncherForActivityResult
        model.launch {
            if (model.importSounds(uris)) model.saveSelectedSound()
        }
    }

    LaunchedEffect(Unit) { model.reloadImportedSounds() }
    LaunchedEffect(model.importedSounds.size) { if (model.importedSounds.isEmpty()) searchText = "" }
    LaunchedEffect(Unit) {
        if (didPerformInitialScroll) return@LaunchedEffect
        didPerformInitialScroll = true
        val index = options.indexOfFirst { it.id == model.selectedSound }
        // Items before the options: the search field (when shown) and the info section.
        val leading = (if (model.importedSounds.isNotEmpty()) 1 else 0) + 1
        if (index > 3) listState.scrollToItem(leading + index, scrollOffset = -300)
    }

    Scaffold(
        containerColor = Color.Transparent,
        topBar = {
            MGTopBar("Sounds", navigator, actions = {
                if (model.importedSounds.isNotEmpty()) {
                    IconButton(onClick = { showingDeleteAllSoundsConfirmation = true }) {
                        Icon(Icons.Filled.Delete, contentDescription = "Delete All")
                    }
                }
                IconButton(onClick = { importer.launch(arrayOf("audio/*")) }) {
                    Icon(Icons.Filled.Add, contentDescription = "Add")
                }
            })
        },
    ) { padding ->
        LazyColumn(
            Modifier.padding(padding).fillMaxSize(),
            state = listState,
            contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 4.dp, bottom = 24.dp),
        ) {
            if (model.importedSounds.isNotEmpty()) {
                item(key = "search") {
                    SearchField(searchText, { searchText = it }, "Search sounds", { searchFocused = it })
                    Spacer(Modifier.padding(top = 12.dp))
                }
            }
            if (!searchActive) {
                item(key = "info") {
                    GroupedSection {
                        Column(Modifier.padding(RowPadding), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                            Text(
                                "Imported sounds must be 30 seconds or less. Any sound longer than 30 seconds will be ignored.",
                                style = MaterialTheme.typography.bodySmall,
                                color = secondaryText(),
                            )
                            val count = model.importedSounds.size
                            if (count > 0) {
                                Text(
                                    "You currently have $count added sound${if (count == 1) "" else "s"}.",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = secondaryText(),
                                )
                            }
                        }
                    }
                    Spacer(Modifier.padding(top = 20.dp))
                }
            }
            if (filtered.isEmpty()) {
                item(key = "nomatch") {
                    GroupedSection { Text("No matching sounds.", color = secondaryText(), modifier = Modifier.padding(RowPadding)) }
                }
            } else {
                itemsIndexed(filtered, key = { _, option -> option.id }) { index, option ->
                    val selected = model.selectedSound == option.id
                    val imported = option.id in model.importedSounds
                    ActionRow(
                        actions = if (imported) {
                            listOf(RowAction("Delete", destructive = true, swipe = true) { model.deleteImportedSound(option.id) })
                        } else {
                            emptyList()
                        },
                        shape = groupedItemShape(index, filtered.size),
                        selected = selected,
                        onClick = {
                            model.selectedSound = option.id
                            model.previewSoundSelection()
                            model.launch { model.saveSelectedSound() }
                        },
                    ) {
                        Column {
                            Row(
                                Modifier.fillMaxWidth().heightIn(min = 48.dp).padding(RowPadding),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                Text(option.title, modifier = Modifier.weight(1f))
                                if (selected) Icon(Icons.Filled.Check, contentDescription = null, tint = MaterialTheme.colorScheme.primary)
                            }
                            if (index < filtered.lastIndex) RowDivider()
                        }
                    }
                }
            }
        }
    }

    if (showingDeleteAllSoundsConfirmation) {
        ConfirmationDialog(
            title = "Are you sure you wish to delete all sounds?",
            confirmLabel = "Delete All",
            onConfirm = { model.deleteAllImportedSounds() },
            onDismiss = { showingDeleteAllSoundsConfirmation = false },
        )
    }
}

@Composable
fun AddSoundsScreen(model: MGNotifyModel, navigator: Navigator) {
    var pendingSounds by remember { mutableStateOf<List<File>>(emptyList()) }
    var isAdding by remember { mutableStateOf(false) }
    LaunchedEffect(Unit) { pendingSounds = model.pendingSoundImports() }
    val description = if (pendingSounds.size == 1) {
        "You are about to add the following sound to MG Notify:"
    } else {
        "You are about to add the following ${pendingSounds.size} sounds to MG Notify:"
    }

    Scaffold(
        containerColor = Color.Transparent,
        topBar = { MGTopBar("Add Sounds", navigator) },
    ) { padding ->
        Column(
            Modifier.padding(padding).fillMaxSize().padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Text(
                "Add Sounds",
                style = MaterialTheme.typography.headlineLarge,
                fontWeight = FontWeight.Bold,
                modifier = Modifier.semantics { heading() },
            )
            Text(description, color = secondaryText())
            LazyColumn(Modifier.weight(1f).fillMaxWidth()) {
                itemsIndexed(pendingSounds, key = { _, file -> file.name }) { index, file ->
                    Column(Modifier.fillMaxWidth()) {
                        Text(NotificationSounds.displayName(file.name), modifier = Modifier.padding(vertical = 12.dp))
                        if (index < pendingSounds.lastIndex) RowDivider()
                    }
                }
            }
            Button(
                onClick = {
                    if (pendingSounds.isEmpty()) return@Button
                    isAdding = true
                    val selectSingle = pendingSounds.size == 1
                    model.launch {
                        model.addPendingSoundImports(selectSingleImportedSound = selectSingle)
                        pendingSounds = emptyList()
                        isAdding = false
                        navigator.pop()
                        navigator.push(Screen.Sounds)
                    }
                },
                enabled = pendingSounds.isNotEmpty() && !isAdding,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text("Add")
                if (isAdding) CircularProgressIndicator(Modifier.padding(start = 8.dp).heightIn(max = 18.dp), strokeWidth = 2.dp)
            }
            OutlinedButton(
                onClick = {
                    model.clearPendingSoundImports()
                    navigator.pop()
                },
                modifier = Modifier.fillMaxWidth(),
            ) { Text("Cancel") }
        }
    }
}
