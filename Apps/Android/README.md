# MG Notify for Android

A native Android app (Kotlin, Jetpack Compose) that mirrors the iOS app screen
for screen and talks to the same server (`https://notify.mad-gamer.com/api`).

## Building

Requires JDK 17+ and the Android SDK (compile SDK 37). On Windows, Android
Studio's bundled JDK works:

    set "JAVA_HOME=C:\Program Files\Android\Android Studio\jbr"
    gradlew assembleDebug

### Push notifications need `app/google-services.json`

Pushes come through Firebase Cloud Messaging, in the Firebase project that the
server's `google-authkey.json` belongs to. Register an Android app
with package name `com.matthewdovi.mgnotify` in that Firebase project, download
its `google-services.json`, and put it at `app/google-services.json`. The build
reads it directly; the google-services Gradle plugin isn't needed. Without the
file the app builds and works normally, except that it gets no FCM token, so
the device never registers with the server and receives no pushes.

### Pointing a build at another server

    gradlew assembleDebug -PmgNotifyServer=http://127.0.0.1:8090

Debug builds allow plain HTTP. With an emulator, `adb reverse tcp:8090 tcp:8090`
makes a server on the host reachable at `127.0.0.1:8090`.

### Release signing

Put `storeFile`, `storePassword`, `keyAlias` and `keyPassword` in
`~/.mgnotify-release/android-signing.properties` (keystore path relative to that
folder). Without it, release builds are unsigned.

## How it maps to the iOS app

| iOS | Android |
| --- | --- |
| `MGNotifyModel` (ContentView.swift) | `MGNotifyModel.kt`: same state, same flows, same API calls |
| `AccountNotification`, link parsing | `Models.kt` |
| APNs token → `/devices/register` | FCM token → `/devices/register` with `platform: "android"`, `fcm_token` |
| `UNUserNotificationCenter` delegate | `PushService.kt` (`IncomingPush`) |
| Swipe actions | Swipe runs the first iOS action; every action is also on long-press and in TalkBack's actions menu |
| Confirmation dialogs | `AlertDialog`s with the same wording |
| `DisclosureGroup` | `DisclosureSection` (expand/collapse actions for TalkBack) |
| Share extension for sounds | "Add to MG Notify Sounds" share target → Add Sounds screen |
| `mgnotify://sounds`, `mgnotify://add-sounds` | Same links |
| AIFF/WAV/CAF sounds | WAV, MP3, OGG/Opus, M4A/AAC, FLAC, MIDI (what Android plays), 30 seconds or less |

## Notification sounds

An Android notification channel's sound is fixed once the channel is created,
so the app makes one channel per sound choice (`NotificationSounds.kt`):
`mg_notify` (system sound), `mg_notify_silent`, and `mg_notify_sound_<hash>`
for each imported sound.

When the app is open it shows pushes itself on the right channel. When it isn't,
the system shows them on whatever channel the server names. Only a server that
derives the channel from the device's sound (`android_channel_id_for_sound` in
`server/src/push.rs`) plays a custom sound or stays silent then. An older
server always names `mg_notify` (the system sound).

## Testing without Firebase

Debug builds include `DebugPushReceiver`, which feeds a pretend push through the
same handling as a real one:

    adb shell am broadcast -n com.matthewdovi.mgnotify/.DebugPushReceiver \
      --es title "Hello" --es message "World" \
      --es mg_notify '{"account_id":"<id>"}'

An event push: `--es mg_notify '{"event":"signed_out","account_id":"<id>"}'`.
