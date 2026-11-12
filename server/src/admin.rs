//! The admin page: accounts, their devices, sign-ins, API keys and
//! notifications, a test send, apps with the live connection open, the
//! settings and the recent log.
//!
//! It has its own listener (`admin` in config.json, 127.0.0.1:5001 by
//! default), never the public one. Without `admin.password_hash` it may
//! only listen on this machine; with one, browsers ask for the password
//! (any user name). Changes need the page's own header and an Origin that
//! matches the Host. Every change is logged with the address it came from.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};

use axum::Json;
use axum::Router;
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::{self, ApiError, AppState};
use crate::util::{self, PasswordCheck};

type Shared = State<Arc<AppState>>;
type AdminResult = Result<Json<Value>, ApiError>;

const PAGE: &str = include_str!("../admin/index.html");
const SCRIPT: &str = include_str!("../admin/app.js");
const STYLE: &str = include_str!("../admin/style.css");
const NOTIFICATIONS_SHOWN: usize = 200;

static STARTED: OnceLock<String> = OnceLock::new();

pub fn router(state: Arc<AppState>) -> Router {
    STARTED.get_or_init(util::iso_now);
    Router::new()
        .route("/", get(|| async { asset(PAGE, "text/html; charset=utf-8") }))
        .route("/app.js", get(|| async { asset(SCRIPT, "text/javascript; charset=utf-8") }))
        .route("/style.css", get(|| async { asset(STYLE, "text/css; charset=utf-8") }))
        .route("/api/overview", get(overview))
        .route("/api/accounts", get(accounts).post(create_account))
        .route("/api/accounts/{id}", get(account))
        .route("/api/accounts/{id}/password", post(set_password))
        .route("/api/accounts/{id}/verify", post(verify))
        .route("/api/accounts/{id}/email", post(set_email))
        .route("/api/accounts/{id}/name", post(set_name))
        .route("/api/accounts/{id}/sign-out-everywhere", post(sign_out_everywhere))
        .route("/api/accounts/{id}/delete", post(delete_account))
        .route("/api/accounts/{id}/devices/{device}/sign-out", post(sign_out_device))
        .route("/api/accounts/{id}/sessions/{session}/delete", post(delete_session))
        .route("/api/accounts/{id}/keys", post(create_key))
        .route("/api/accounts/{id}/keys/{key}", get(reveal_key))
        .route("/api/accounts/{id}/keys/{key}/delete", post(delete_key))
        .route("/api/accounts/{id}/notifications/{nid}/delete", post(delete_notification))
        .route("/api/accounts/{id}/notifications/clear", post(clear_notifications))
        .route("/api/accounts/{id}/test", post(test_send))
        .route("/api/live", get(live))
        .route("/api/log", get(log))
        .layer(axum::middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

fn asset(body: &'static str, kind: &'static str) -> Response {
    let mut r = body.into_response();
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
    r
}

fn not_found(what: &str) -> ApiError {
    ApiError::new(404, format!("{what} no longer exists. The page has been refreshed."))
}

// ─── protection ─────────────────────────────────────────────────────────────

/// The last Authorization header that passed, so the slow password check
/// runs once per browser.
static GOOD_AUTH: Mutex<Option<String>> = Mutex::new(None);

async fn guard(State(state): Shared, req: Request, next: Next) -> Response {
    if let Some(hash) = state.settings.admin_password_hash.clone()
        && !password_ok(req.headers(), &hash).await
    {
        let mut r = (StatusCode::UNAUTHORIZED, "The admin password is needed.").into_response();
        r.headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"MG Notify admin\", charset=\"UTF-8\""));
        return r;
    }
    if req.method() != Method::GET && req.method() != Method::HEAD && !same_page(req.headers()) {
        return (StatusCode::FORBIDDEN, Json(json!({"detail": "Requests from other sites are not allowed."}))).into_response();
    }
    let mut r = next.run(req).await;
    let h = r.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; form-action 'none'; frame-ancestors 'none'; base-uri 'none'",
        ),
    );
    r
}

async fn password_ok(headers: &HeaderMap, hash: &str) -> bool {
    let Some(value) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let seen = util::hash_token(value);
    if GOOD_AUTH.lock().unwrap_or_else(|e| e.into_inner()).as_deref() == Some(seen.as_str()) {
        return true;
    }
    let Some(text) = value
        .strip_prefix("Basic ")
        .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b.trim()).ok())
        .and_then(|raw| String::from_utf8(raw).ok())
    else {
        return false;
    };
    let password = text.split_once(':').map_or(text.as_str(), |(_, p)| p).to_owned();
    let hash = hash.to_owned();
    let ok = tokio::task::spawn_blocking(move || {
        matches!(util::check_password(&password, &hash, ""), PasswordCheck::Right | PasswordCheck::RightButOld)
    })
    .await
    .unwrap_or(false);
    if ok {
        *GOOD_AUTH.lock().unwrap_or_else(|e| e.into_inner()) = Some(seen);
    }
    ok
}

fn same_page(h: &HeaderMap) -> bool {
    let header_ok = h.get("x-mg-notify-admin").and_then(|v| v.to_str().ok()) == Some("1");
    let host = h.get(header::HOST).and_then(|v| v.to_str().ok());
    let origin_host = h.get(header::ORIGIN).and_then(|v| v.to_str().ok()).and_then(|o| o.split_once("://")).map(|(_, r)| r);
    header_ok && host.is_some() && origin_host == host
}

/// Who made a change: the reverse proxy's X-Real-IP, else the connection.
fn who(headers: &HeaderMap, peer: SocketAddr) -> String {
    headers
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .unwrap_or_else(|| peer.ip().to_string())
}

fn audit(address: &str, what: impl std::fmt::Display) {
    tracing::info!(target: "mg_notify_server::admin", %address, "admin: {what}");
}

fn ok(message: impl Into<String>) -> AdminResult {
    Ok(Json(json!({"message": message.into()})))
}

fn find(state: &AppState, id: &str) -> Result<crate::db::Account, ApiError> {
    state.db.account_by_id(id)?.ok_or_else(|| not_found("That account"))
}

fn masked(key: &str) -> String {
    let start: String = key.chars().take(8).collect();
    format!("{start}…")
}

// ─── reading ────────────────────────────────────────────────────────────────

async fn overview(State(state): Shared) -> AdminResult {
    let s = &state.settings;
    let hidden = |v: &Option<String>| v.as_ref().map(|_| "(set, hidden)");
    let path = |v: &Option<std::path::PathBuf>| v.as_ref().map(|p| p.display().to_string());
    Ok(Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "started": STARTED.get(),
        "counts": state.db.admin_counts()?,
        "open_streams": state.hub.open_streams().len(),
        "providers": {
            "apns": state.pusher.apns.is_some(),
            "fcm": state.pusher.fcm.is_some(),
            "web_push": !state.pusher.web.public_key().is_empty(),
            "email": state.email.enabled,
            "inbound_smtp": s.inbound_smtp_port.is_some(),
        },
        "settings": [
            ["Listens on", format!("{}:{}", s.bind_host, s.bind_port)],
            ["Data folder", s.data_dir.display().to_string()],
            ["Log level", s.log_level.clone()],
            ["Website address", s.web_base_url.clone()],
            ["APNs team", s.apns_team_id.clone()],
            ["APNs key ID", s.apns_key_id.clone()],
            ["APNs app", s.apns_bundle_id.clone()],
            ["APNs key file", path(&s.apns_auth_key_path)],
            ["Firebase project", s.fcm_project_id.clone()],
            ["Firebase service account file", path(&s.fcm_service_account_path)],
            ["Mail server", s.smtp_host.as_ref().map(|h| format!("{h}:{}", s.smtp_port))],
            ["Mail server user", s.smtp_username.clone()],
            ["Mail server password", hidden(&s.smtp_password)],
            ["Email from", s.smtp_from_email.as_ref().map(|e| format!("{} <{e}>", s.smtp_from_name.clone().unwrap_or_default()))],
            ["Mail server uses TLS", s.smtp_use_tls.to_string()],
            ["Inbound email domain", s.inbound_email_domain.clone()],
            ["Inbound SMTP", s.inbound_smtp_port.map(|p| format!("{}:{p}", s.inbound_smtp_bind_host))],
            ["Inbound SMTP host name", s.inbound_smtp_hostname.clone()],
            ["Inbound SMTP certificate", path(&s.inbound_smtp_tls_cert_path)],
            ["Admin page listens on", format!("{}:{}", s.admin_bind_host, s.admin_bind_port)],
            ["Admin page password", if s.admin_password_hash.is_some() { "Yes" } else { "No" }],
        ],
    })))
}

async fn accounts(State(state): Shared) -> AdminResult {
    let open = state.hub.open_streams();
    let list: Vec<Value> = state
        .db
        .admin_accounts()?
        .into_iter()
        .map(|mut a| {
            let id = a["account_id"].as_str().unwrap_or_default().to_owned();
            a["open"] = json!(open.iter().filter(|o| o.account_id == id).count());
            a
        })
        .collect();
    Ok(Json(json!({"accounts": list})))
}

async fn account(State(state): Shared, Path(id): Path<String>) -> AdminResult {
    let a = find(&state, &id)?;
    let open = state.hub.open_streams();
    let summary = state.db.admin_accounts()?.into_iter().find(|x| x["account_id"] == id.as_str());
    let devices: Vec<Value> = state
        .db
        .devices_for_account(&id)?
        .into_iter()
        .map(|d| {
            let push = if d.is_android() {
                if d.fcm_token.is_some() { "Firebase (Android)" } else { "None" }
            } else if d.is_web() {
                if d.web_endpoint.is_some() { "Web Push" } else { "None" }
            } else if d.apns_token.is_empty() {
                "None"
            } else if d.apns_environment == "development" {
                "Apple (development)"
            } else {
                "Apple"
            };
            let live = open.iter().any(|o| o.device_id.as_deref() == Some(d.device_id.as_str()));
            json!({
                "device_id": d.device_id, "name": d.device_name, "platform": d.platform, "push": push,
                "sound": d.notification_sound, "last_ip_address": d.last_ip_address, "last_seen": d.last_seen,
                "badges": d.badges, "live": live,
            })
        })
        .collect();
    let keys: Vec<Value> = state
        .db
        .api_keys_for_account(&id)?
        .into_iter()
        .map(|k| json!({"key_id": k.key_id, "label": k.label, "start": masked(&k.api_key), "created_at": k.created_at, "last_used_at": k.last_used_at}))
        .collect();
    let all = state.db.notifications_for_account(&id)?;
    let total = all.len();
    let notifications: Vec<Value> = all.into_iter().take(NOTIFICATIONS_SHOWN).map(|n| n.json()).collect();
    let streams: Vec<_> = open.into_iter().filter(|o| o.account_id == id).collect();
    Ok(Json(json!({
        "account": a.json(), "summary": summary, "devices": devices, "sessions": state.db.admin_sessions(&id)?,
        "api_keys": keys, "notifications": notifications, "notifications_total": total, "streams": streams,
        "unread": state.db.unread_count(&id)?,
    })))
}

async fn live(State(state): Shared) -> AdminResult {
    let streams: Vec<Value> = state
        .hub
        .open_streams()
        .into_iter()
        .map(|o| {
            let email = state.db.account_by_id(&o.account_id).ok().flatten().map(|a| a.email);
            let device = o
                .device_id
                .as_deref()
                .and_then(|d| state.db.device(&o.account_id, d).ok().flatten())
                .map(|d| d.device_name);
            json!({"stream": o, "email": email, "device_name": device})
        })
        .collect();
    Ok(Json(json!({"streams": streams})))
}

#[derive(Deserialize)]
struct LogQuery {
    lines: Option<usize>,
}

async fn log(Query(q): Query<LogQuery>) -> Json<Value> {
    let n = q.lines.unwrap_or(300).clamp(1, crate::logbuf::KEEP);
    Json(json!({"lines": crate::logbuf::recent(n), "kept": crate::logbuf::KEEP}))
}

// ─── accounts ───────────────────────────────────────────────────────────────

fn password_rule(p: &str) -> Result<(), ApiError> {
    if p.chars().count() < 8 { Err(ApiError::new(422, "The password must be at least 8 characters long.")) } else { Ok(()) }
}

#[derive(Deserialize)]
struct NewAccount {
    email: String,
    password: String,
    #[serde(default)]
    verified: bool,
    display_name: Option<String>,
}

async fn create_account(State(state): Shared, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Json(r): Json<NewAccount>) -> AdminResult {
    let email = util::normalize_email(&r.email).map_err(|e| ApiError::new(422, e))?;
    password_rule(&r.password)?;
    let (hash, salt) = tokio::task::spawn_blocking(move || util::hash_password(&r.password))
        .await
        .map_err(|e| ApiError::new(500, e.to_string()))?;
    let account = state
        .db
        .create_account(&email, &hash, &salt)?
        .ok_or_else(|| ApiError::new(409, format!("There is already an account for {email}.")))?;
    if r.verified {
        state.db.mark_email_verified(&account.account_id)?;
    }
    if let Some(name) = r.display_name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        state.db.set_display_name(&account.account_id, Some(name))?;
    }
    audit(&who(&headers, peer), format!("created the account {email}"));
    Ok(Json(json!({"message": format!("Created the account {email}."), "account_id": account.account_id})))
}

#[derive(Deserialize)]
struct NewPassword {
    password: String,
}

async fn set_password(State(state): Shared, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Path(id): Path<String>, Json(r): Json<NewPassword>) -> AdminResult {
    let a = find(&state, &id)?;
    password_rule(&r.password)?;
    let (hash, salt) = tokio::task::spawn_blocking(move || util::hash_password(&r.password))
        .await
        .map_err(|e| ApiError::new(500, e.to_string()))?;
    state.db.set_password(&id, &hash, &salt)?;
    // As when people change their own password: signed out everywhere.
    api::sign_out_everywhere(&state, &id).await?;
    audit(&who(&headers, peer), format!("set a new password for {}", a.email));
    ok(format!("Set a new password for {}. It is signed out everywhere and its devices were removed; they sign in again with the new password.", a.email))
}

async fn verify(State(state): Shared, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Path(id): Path<String>) -> AdminResult {
    let a = find(&state, &id)?;
    state.db.mark_email_verified(&id)?;
    api::event_to_account(&state, &id, "account_updated");
    audit(&who(&headers, peer), format!("marked {} as verified", a.email));
    ok(format!("Marked {} as verified.", a.email))
}

#[derive(Deserialize)]
struct NewEmail {
    email: String,
    #[serde(default)]
    verified: bool,
}

async fn set_email(State(state): Shared, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Path(id): Path<String>, Json(r): Json<NewEmail>) -> AdminResult {
    let a = find(&state, &id)?;
    let email = util::normalize_email(&r.email).map_err(|e| ApiError::new(422, e))?;
    if email == a.email {
        return ok("The email address is unchanged.");
    }
    state.db.change_email(&id, &email)?.ok_or_else(|| ApiError::new(409, format!("Another account already uses {email}.")))?;
    if r.verified {
        state.db.mark_email_verified(&id)?;
    }
    api::event_to_account(&state, &id, "account_updated");
    audit(&who(&headers, peer), format!("changed the email of {} to {email}", a.email));
    ok(format!("Changed the email address from {} to {email}{}.", a.email, if r.verified { " (verified)" } else { " (not verified yet)" }))
}

#[derive(Deserialize)]
struct NewName {
    display_name: Option<String>,
}

async fn set_name(State(state): Shared, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Path(id): Path<String>, Json(r): Json<NewName>) -> AdminResult {
    let a = find(&state, &id)?;
    let name = r.display_name.as_deref().map(str::trim).filter(|n| !n.is_empty()).map(|n| util::truncate_chars(n, 100));
    state.db.set_display_name(&id, name.as_deref())?;
    api::event_to_account(&state, &id, "account_updated");
    audit(&who(&headers, peer), format!("set the display name of {}", a.email));
    ok(match name {
        Some(n) => format!("Set {}'s display name to {n}.", a.email),
        None => format!("Removed {}'s display name.", a.email),
    })
}

async fn sign_out_everywhere(State(state): Shared, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Path(id): Path<String>) -> AdminResult {
    let a = find(&state, &id)?;
    api::sign_out_everywhere(&state, &id).await?;
    audit(&who(&headers, peer), format!("signed {} out everywhere", a.email));
    ok(format!("Signed {} out everywhere: every sign-in ended and its devices were removed.", a.email))
}

async fn delete_account(State(state): Shared, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Path(id): Path<String>) -> AdminResult {
    let a = find(&state, &id)?;
    // As when people delete their own account: the apps hear first.
    api::event_to_account_now(&state, &id, "account_deleted").await;
    state.db.delete_account(&id)?;
    state.hub.account_deleted(&id);
    audit(&who(&headers, peer), format!("deleted the account {}", a.email));
    ok(format!("Deleted the account {} with its devices, keys and notifications.", a.email))
}

async fn sign_out_device(State(state): Shared, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Path((id, device)): Path<(String, String)>) -> AdminResult {
    let a = find(&state, &id)?;
    let d = state.db.device(&id, &device)?.ok_or_else(|| not_found("That device"))?;
    let name = d.device_name.clone();
    // The same as signing a device out from the app.
    api::event_to_device(&state, d, "signed_out");
    state.db.delete_device(&id, &device)?;
    let sessions = state.db.session_hashes_for_device(&id, &device)?;
    state.db.delete_sessions_for_device(&id, &device)?;
    state.hub.signed_out(&id, sessions);
    api::event_to_account(&state, &id, "devices_updated");
    audit(&who(&headers, peer), format!("signed out {}'s device {name}", a.email));
    ok(format!("Signed out {}'s device {name}. It gets no more notifications until it signs in again.", a.email))
}

async fn delete_session(State(state): Shared, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Path((id, session)): Path<(String, String)>) -> AdminResult {
    let a = find(&state, &id)?;
    let hash = state.db.admin_delete_session(&id, &session)?.ok_or_else(|| not_found("That sign-in"))?;
    state.hub.signed_out(&id, vec![hash]);
    audit(&who(&headers, peer), format!("ended a sign-in of {}", a.email));
    ok(format!("Ended that sign-in of {}.", a.email))
}

// ─── API keys ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct NewKey {
    label: String,
}

async fn create_key(State(state): Shared, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Path(id): Path<String>, Json(r): Json<NewKey>) -> AdminResult {
    let a = find(&state, &id)?;
    let label = r.label.trim();
    if label.is_empty() {
        return Err(ApiError::new(422, "Give the key a label."));
    }
    let key = state.db.create_api_key(&id, &util::truncate_chars(label, 100))?;
    api::event_to_account(&state, &id, "api_keys_updated");
    audit(&who(&headers, peer), format!("made an API key \"{}\" for {}", key.label, a.email));
    Ok(Json(json!({"message": format!("Made the API key {} for {}.", key.label, a.email), "api_key": key.api_key, "key_id": key.key_id})))
}

async fn reveal_key(State(state): Shared, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Path((id, key)): Path<(String, String)>) -> AdminResult {
    let a = find(&state, &id)?;
    let k = state.db.api_keys_for_account(&id)?.into_iter().find(|k| k.key_id == key).ok_or_else(|| not_found("That API key"))?;
    audit(&who(&headers, peer), format!("showed {}'s API key \"{}\"", a.email, k.label));
    Ok(Json(json!({"api_key": k.api_key})))
}

async fn delete_key(State(state): Shared, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Path((id, key)): Path<(String, String)>) -> AdminResult {
    let a = find(&state, &id)?;
    let label = state.db.api_keys_for_account(&id)?.into_iter().find(|k| k.key_id == key).map(|k| k.label);
    if !state.db.delete_api_key(&id, &key)? {
        return Err(not_found("That API key"));
    }
    api::event_to_account(&state, &id, "api_keys_updated");
    let label = label.unwrap_or_default();
    audit(&who(&headers, peer), format!("revoked {}'s API key \"{label}\"", a.email));
    ok(format!("Revoked the API key {label}. Anything using it stops sending."))
}

// ─── notifications ──────────────────────────────────────────────────────────

async fn delete_notification(State(state): Shared, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Path((id, nid)): Path<(String, String)>) -> AdminResult {
    let a = find(&state, &id)?;
    if state.db.delete_notification(&id, &nid)?.is_none() {
        return Err(not_found("That notification"));
    }
    api::event_to_account(&state, &id, "notifications_updated");
    crate::push::spawn_badge_updates(state.db.clone(), state.pusher.clone(), &id);
    audit(&who(&headers, peer), format!("deleted a notification of {}", a.email));
    ok("Deleted the notification.")
}

async fn clear_notifications(State(state): Shared, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Path(id): Path<String>) -> AdminResult {
    let a = find(&state, &id)?;
    let n = state.db.clear_notifications(&id)?;
    api::event_to_account(&state, &id, "notifications_updated");
    crate::push::spawn_badge_updates(state.db.clone(), state.pusher.clone(), &id);
    audit(&who(&headers, peer), format!("cleared {n} notifications of {}", a.email));
    ok(format!("Cleared {n} notification{} of {}.", if n == 1 { "" } else { "s" }, a.email))
}

#[derive(Deserialize)]
struct TestSend {
    title: String,
    message: String,
}

async fn test_send(State(state): Shared, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Path(id): Path<String>, Json(r): Json<TestSend>) -> AdminResult {
    let a = find(&state, &id)?;
    let title = util::truncate_chars(r.title.trim(), 200);
    let message = util::truncate_chars(r.message.trim(), 4000);
    if message.is_empty() {
        return Err(ApiError::new(422, "Type a message to send."));
    }
    let title = if title.is_empty() { "MG Notify".to_owned() } else { title };
    let result = crate::push::deliver_to_account(&state.db, &state.pusher, &state.hub, &id, &title, &message, "default", false, None).await?;
    audit(&who(&headers, peer), format!("sent a test notification to {}", a.email));
    let delivered = result["delivered"].as_i64().unwrap_or(0);
    let devices = result["devices"].as_array().map_or(0, Vec::len);
    Ok(Json(json!({
        "message": if devices == 0 { format!("Stored the notification for {}; it has no devices to deliver to.", a.email) }
            else { format!("Sent to {delivered} of {devices} device{} of {}.", if devices == 1 { "" } else { "s" }, a.email) },
        "result": result,
    })))
}
