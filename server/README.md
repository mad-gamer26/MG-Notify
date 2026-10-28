# MG Notify server

The API behind MG Notify's iOS app, Android app and website: accounts, API
keys, notification history, and delivery to every signed-in device. It's a
single Rust binary (axum and tokio) that keeps everything in one SQLite file.

- **Delivers everywhere:** iPhone and iPad through APNs, Android through
  Firebase Cloud Messaging, and browsers through Web Push. Each kind of
  delivery is optional; anything not configured is skipped.
- **Sends with one request:** `GET` or `POST /notify` with an API key, or an
  email to the key's address, received by the server's own SMTP receiver.
- **Live updates:** open apps and websites hold a Server-Sent Events stream
  and see every change the moment it happens. Closed apps get silent pushes.
- **No moving parts:** no external database, queue or cache. Back up one
  folder and you've backed up everything.

## Building

You need a recent stable Rust toolchain (edition 2024).

    cargo build --release             # target/release/mg-notify-server
    cargo test                        # unit tests

SQLite is compiled in, so the binary has no runtime dependencies. For a fully
static Linux build that runs on any distribution, cross-compile with
[cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild):

    pip install ziglang cargo-zigbuild
    rustup target add x86_64-unknown-linux-musl
    cargo zigbuild --release --target x86_64-unknown-linux-musl

## Running

    mg-notify-server [--config PATH]               run the server
    mg-notify-server --configure [--config PATH]   create or edit the config interactively
    mg-notify-server import FOLDER [--config PATH] import data from the old Python server's JSON files

The config defaults to `config.json` in the current folder. Start with
`--configure`: it asks for each setting, showing the current value, and
writes the file. `GET /health` reports whether APNs, FCM and email are turned on.

## Configuration

`config.json` has one section per service. Relative paths are resolved
against the folder the config file is in.

```json
{
  "server": { "data_dir": "./data", "bind_host": "127.0.0.1", "bind_port": "8080", "log_level": "INFO" },
  "apns":   { "team_id": "ABCDE12345", "key_id": "XYZ987WVU6", "bundle_id": "com.example.notify", "auth_key_path": "AuthKey_XYZ987WVU6.p8" },
  "fcm":    { "project_id": "my-firebase-project", "service_account_path": "firebase-service-account.json" },
  "smtp":   { "host": "smtp.example.com", "port": 587, "username": "…", "password": "…",
              "from_email": "no-reply@example.com", "from_name": "MG Notify", "use_tls": true },
  "web":    { "base_url": "https://notify.example.com" },
  "inbound_email": { "domain": "notify.example.com", "smtp_bind_host": "127.0.0.1", "smtp_bind_port": "2525",
                     "tls_cert_path": "tls/fullchain.pem", "tls_key_path": "tls/privkey.pem" }
}
```

| Section | What it turns on |
| --- | --- |
| `server` | Where data lives and where to listen. `log_level` is `DEBUG`, `INFO`, `WARNING` or `ERROR`. Bind to `127.0.0.1` and put a reverse proxy in front. |
| `apns` | iOS pushes. Create an APNs key (`.p8`) in your Apple Developer account. `bundle_id` is your iOS app's bundle identifier. Each device says whether it's a sandbox (development) or production build, and is sent to the matching APNs server. |
| `fcm` | Android pushes. Use a service account key for your Firebase project; the Android app must be built with the same project's `google-services.json`. |
| `smtp` | Email verification and password-reset emails. Without it, new accounts can't verify their email, so they can't send notifications. |
| `web` | The website's address, used for the links in those emails (`<base_url>/?action=verify&token=…`). |
| `inbound_email` | Email notifications (below). `domain` is the part after the `@`. Leave `smtp_bind_port` empty to turn the SMTP receiver off. |

Web Push needs no setup: the server makes its own VAPID key on first start
and keeps it in the database.

### Email notifications (optional)

Every API key also has an email address: the key followed by `@` and
`inbound_email.domain`. Each email sent there becomes a notification titled
`Sender <address>: Subject`; the HTML body is sanitized (no scripts, styles or
tracking images) and kept so the apps can show it. Repeated deliveries of the
same message are ignored.

The server receives this mail itself, with a built-in SMTP receiver, so the
notification goes out the moment the sender's mail server hands the email
over:

1. Set `inbound_email.smtp_bind_port` and point public port 25 at it. The
   receiver accepts only `<API key>@<domain>` addresses of verified accounts,
   so it can't be used to relay mail.
2. Give it a certificate (`tls_cert_path`, `tls_key_path`) so senders can use
   STARTTLS. The files are read for each connection, so a renewed certificate
   is picked up without a restart.
3. Add an MX record for the domain pointing at the server.

An inbound-email service that posts JSON to `POST /email/inbound` works too.
The code expects [Brevo](https://www.brevo.com)'s inbound parsing webhook
format. Put a random secret in a file called `brevo-webhook-secret` next to
`config.json`, and have the webhook send it in the `X-MG-Notify-Webhook-Secret`
header. Such services add a delay of their own (Brevo's is around 20 seconds),
so they work best as a lower-priority backup MX behind the built-in receiver.

## Data

Everything is in `<data_dir>/mg-notify.sqlite`:

- **accounts:** passwords hashed with Argon2id. Older PBKDF2 hashes are
  upgraded the next time the person signs in.
- **sessions, password resets and email verifications:** only SHA-256 hashes
  of their tokens are stored.
- **API keys:** kept readable, because the apps and website show them.
- **devices, notifications, processed inbound emails, and settings** (such as
  the VAPID key).

Housekeeping runs at start-up and then once a day:

- the database is snapshotted into `backups/`, next to the data folder, keeping
  the newest 14;
- sessions unused for 180 days are deleted.

Notification history is kept until the person deletes it.

## API

The website's [documentation](https://notify.mad-gamer.com/docs.html) covers
sending notifications. Every request and response is JSON. Errors look like
`{"detail": "Invalid API key"}` with a matching status code, and invalid input
gets a `422` with a readable message.

**Sending notifications** (API key):

| Route | |
| --- | --- |
| `GET`/`POST /notify` | `key`, `msg`, and optionally `title` (default "MG Notify") and `sound`. `key` can be a comma-separated list to send to several accounts at once; unknown keys are reported in `ignored_keys`. The response says which devices it reached. |

**Accounts and sessions** (the apps and website sign in, then send
`Authorization: Bearer <session token>`):

| Route | |
| --- | --- |
| `POST /auth/register`, `/auth/login`, `/auth/logout`, `GET /auth/me` | Create an account, sign in and out, and get the current account. |
| `POST /auth/verify-email`, `/auth/resend-verification` | Email verification. |
| `POST /auth/request-password-reset`, `/auth/reset-password` | Password reset by email. |
| `POST /account/change-email`, `/account/change-password`, `/account/display-name`, `/account/delete` | Account management. Changing or resetting the password signs out every session. |
| `POST /account/notify` | Send a notification to your own account, for the website's send form. |
| `GET /devices`, `POST /devices/register`, `/devices/sound`, `/devices/{id}/sign-out`, `/devices/sign-out-other`, `DELETE /devices/{id}` | Devices that receive pushes, each with its own sound. |
| `GET /api-keys`, `POST /api-keys/create`, `/api-keys/delete`, `/api-keys/delete-all` | API keys. |
| `GET`/`DELETE /notifications`, `DELETE /notifications/{id}`, `POST /notifications/mark-seen` | Notification history and unread state. |
| `GET /events` | The live event stream (below). |
| `GET /web-push/public-key` | The VAPID public key for browser subscriptions. |

### Live events

`GET /events` is a Server-Sent Events stream for one session. It starts with a
`ready` event carrying the account, then sends:

| Event | When |
| --- | --- |
| `notification` | A notification arrived (includes it). |
| `notification_deleted`, `notifications_cleared` | One or all were deleted. |
| `notifications_seen` | Notifications were seen on some device. |
| `account_updated` | Email, verification or display name changed (includes the account). |
| `devices_updated`, `api_keys_updated` | Those lists changed. |
| `signed_out`, `account_deleted` | The session ended; the stream closes. |
| `resync` | The client fell behind and should reload. |

A keep-alive comment is sent every 20 seconds. Apps that are closed get the
important events as silent pushes instead.

### Delivery details

- **Every device gets it:** a notification goes to every device signed in to
  the account. Devices that APNs, FCM or the browser report as gone are
  removed.
- **Sounds:** `sound` is `default`, `__none__` (silent), or the file name of a
  sound the person added in the app. Android can't change a notification
  channel's sound later, so the server picks one channel per sound with
  `android_channel_id_for_sound` in `src/push.rs`. The Android app makes the
  same channels.
- **iOS badges:** iOS apps that ask for badges get the unread count. When
  notifications are seen elsewhere, the badge is updated with a silent push.

## Deploying

The server speaks plain HTTP; run it behind a reverse proxy that terminates
TLS. `deploy/` has the nginx configuration used for notify.mad-gamer.com:

- `notify-nginx-block.conf`: the server block. It serves `website/` as static
  files and proxies `/api/` to the server.
- `mg-notify-site.conf`: the website's security headers (including its
  Content-Security-Policy) and the caching rules.
- `mg-notify-smtp.conf`: an nginx `stream` server that passes public port 25
  to the SMTP receiver, so the server doesn't need to run as root.
- `mg-notify-cert.sh`: a certbot deploy hook that copies the certificate to
  where the server's user can read it for STARTTLS.

To upgrade, copy the new binary next to the old one, `mv` it into place and
restart. The database schema upgrades itself on start.

## Tests

`tests/contract` is a black-box test suite for the whole API. It starts the
server with an empty data folder and a local mail sink, so nothing real is
touched:

    cargo build
    python3 tests/contract/run.py --command target/debug/mg-notify-server

Any change to the API should keep these passing.

## Source layout

| File | |
| --- | --- |
| `src/main.rs` | Command line, start-up and the daily housekeeping. |
| `src/config.rs` | `config.json` loading and `--configure`. |
| `src/api.rs` | Every HTTP route. |
| `src/db.rs` | The SQLite schema, migrations and queries. |
| `src/push.rs` | APNs, FCM and Web Push delivery. |
| `src/events.rs` | The live event stream. |
| `src/email.rs` | Outgoing email, and turning inbound email into notification text. |
| `src/smtp.rs` | The built-in SMTP receiver for email notifications. |
| `src/import.rs` | Importing the old Python server's JSON files. |
| `src/util.rs` | Shared helpers: password hashing, tokens and validation. |
