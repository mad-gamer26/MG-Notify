package com.matthewdovi.mgnotify

import androidx.compose.runtime.mutableStateListOf

/** Screens pushed over the main one, like the iOS app's navigation destinations. */
sealed interface Screen {
    data object Settings : Screen
    data object Sounds : Screen
    data object AddSounds : Screen
    data object Accounts : Screen
    data object AccountSettings : Screen
    data object About : Screen
    data class EmailDetail(val notification: AccountNotification) : Screen
}

/** The stack of pushed screens. The model owns it, so it outlives the activity. */
class Navigator {
    val stack = mutableStateListOf<Screen>()
    fun push(screen: Screen) { stack.add(screen) }
    fun pop() { if (stack.isNotEmpty()) stack.removeAt(stack.lastIndex) }
    fun reset() { stack.clear() }
}
