# MG Notify

Push notifications from your own scripts, servers and automations to your
phone and browser. Send one with a single HTTP request or an email, and it
arrives on every device signed in to your account.

    curl -G https://notify.mad-gamer.com/api/notify \
      --data-urlencode "key=YOUR_KEY" \
      --data-urlencode "title=Server" \
      --data-urlencode "msg=Disk usage is at 91%"

The hosted service is at [notify.mad-gamer.com](https://notify.mad-gamer.com),
with [full documentation](https://notify.mad-gamer.com/docs.html). The iPhone
app is on the App Store, and the Android app can be downloaded from the site.

## What's here

| Folder | What it is |
| --- | --- |
| [`server/`](server) | The API: Rust (axum) with SQLite. Delivers through APNs, Firebase Cloud Messaging and Web Push, receives email notifications, and streams live updates to open apps. |
| [`website/`](website) | The website: sign-up, dashboard, documentation and browser notifications. Static HTML, CSS and JavaScript with no build step. |
| [`Apps/iOS/`](Apps/iOS) | The iPhone and iPad app (SwiftUI). |
| [`Apps/Android/`](Apps/Android) | The Android app (Kotlin, Jetpack Compose), which mirrors the iOS app. |

Everything is built with accessibility first: VoiceOver and TalkBack users get
the same app as everyone else, and new notifications are announced while the
apps are open.

## Running your own

1. Build the server (see [`server/README.md`](server/README.md)) and run
   `mg-notify-server --configure` to write a `config.json`. APNs, Firebase and
   SMTP are each optional; whatever isn't configured is skipped.
2. Serve `website/` with the headers in
   [`server/deploy/mg-notify-site.conf`](server/deploy/mg-notify-site.conf),
   and proxy `/api/` to the server.
3. Point the apps at your server. The iOS app's server URL is at the top of
   `ContentView.swift`. For Android, build with `-PmgNotifyServer=<url>` and
   add your own Firebase project's `google-services.json`; see
   [`Apps/Android/README.md`](Apps/Android/README.md).

`server/tests/contract` holds the API contract tests every server change must
pass.

## License

[MIT](LICENSE)
