//! The HTTP API: the same routes, request and response JSON as the Python
//! server (tests/contract checks this), plus Web Push for browsers.

use axum::extract::{ConnectInfo, FromRequest, FromRequestParts, Path, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;

use crate::config::Settings;
use crate::db::{Account, Db, Device, Session};
use crate::email::{self, EmailSender};
use crate::events::Hub;
use crate::push::{self, Pusher};
use crate::util::{self, PasswordCheck, epoch_from_iso, iso_now, now_epoch};

pub struct AppState {
    pub settings: Settings,
    pub db: Arc<Db>,
    pub pusher: Arc<Pusher>,
    pub email: EmailSender,
    pub hub: Hub,
}

type Shared = State<Arc<AppState>>;

/// An error in the shape every client already reads: `{"detail": ...}`.
pub struct ApiError(StatusCode, Value);

impl ApiError {
    fn new(status: u16, detail: impl Into<String>) -> Self {
        Self(StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Value::String(detail.into()))
    }

    /// The error's message, for logs.
    pub fn detail(&self) -> String {
        self.1.as_str().map_or_else(|| self.1.to_string(), String::from)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"detail": self.1}))).into_response()
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(error: rusqlite::Error) -> Self {
        tracing::error!("database error: {error}");
        ApiError::new(500, "Internal Server Error")
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

/// A JSON body, or an HTML-form one (`application/x-www-form-urlencoded`,
/// as browsers and many scripts send), read as the same fields with text
/// values. Anything that doesn't fit is a 422 with a readable message.
pub struct Body<T>(T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for Body<T> {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, ApiError> {
        let is_form = request
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/x-www-form-urlencoded"));
        let bytes = axum::body::Bytes::from_request(request, state)
            .await
            .map_err(|_| ApiError::new(422, "The request body could not be read."))?;
        let parsed = if is_form {
            serde_json::from_value(Value::Object(form_fields(&bytes)))
        } else {
            serde_json::from_slice(&bytes)
        };
        parsed.map(Body).map_err(|error| ApiError::new(422, describe_json_error(&error)))
    }
}

/// `a=1&b=2` as `{"a": "1", "b": "2"}`, for body and query parameters alike.
fn form_fields(text: &[u8]) -> serde_json::Map<String, Value> {
    url::form_urlencoded::parse(text).map(|(k, v)| (k.into_owned(), Value::String(v.into_owned()))).collect()
}

fn describe_json_error(error: &serde_json::Error) -> String {
    let text = error.to_string();
    if let Some(field) = text.strip_prefix("missing field `").and_then(|rest| rest.split('`').next()) {
        return format!("{field} is required");
    }
    if error.is_syntax() || error.is_eof() {
        return "The request body must be JSON.".into();
    }
    match text.split(" at line").next() {
        Some(first) => first.replace('`', ""),
        None => text,
    }
}

/// Query parameters, with the same readable 422s as bodies.
pub struct QueryParams<T>(T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequestParts<S> for QueryParams<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, ApiError> {
        let query = parts.uri.query().unwrap_or("");
        serde_json::from_value(Value::Object(form_fields(query.as_bytes())))
            .map(QueryParams)
            .map_err(|error| ApiError::new(422, describe_json_error(&error)))
    }
}

/// `field` must be at least `min` characters (pydantic's min_length).
fn min_length(value: &str, min: usize, field: &str) -> Result<(), ApiError> {
    if value.chars().count() < min {
        let unit = if min == 1 { "character" } else { "characters" };
        return Err(ApiError::new(422, format!("{field} must be at least {min} {unit}")));
    }
    Ok(())
}

fn valid_email(value: &str) -> Result<String, ApiError> {
    util::normalize_email(value).map_err(|e| ApiError::new(422, e))
}

fn valid_password(value: &str, field: &str) -> Result<(), ApiError> {
    if value.chars().count() < 8 {
        return Err(ApiError::new(422, format!("{field} must be at least 8 characters")));
    }
    Ok(())
}

async fn require_session(state: &AppState, headers: &HeaderMap) -> Result<(Account, Session), ApiError> {
    let header = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("");
    let Some(token) = header.strip_prefix("Bearer ") else {
        return Err(ApiError::new(401, "Missing bearer token"));
    };
    let session = state.db.use_session(token.trim())?.ok_or_else(|| ApiError::new(401, "Invalid session"))?;
    let account = state.db.account_by_id(&session.account_id)?.ok_or_else(|| ApiError::new(401, "Invalid session"))?;
    Ok((account, session))
}

async fn require_verified_session(state: &AppState, headers: &HeaderMap) -> Result<(Account, Session), ApiError> {
    let (account, session) = require_session(state, headers).await?;
    if !account.email_verified {
        return Err(ApiError::new(403, "Email verification is required"));
    }
    Ok((account, session))
}

/// Tells everything signed in to the account that `event` happened: open
/// apps and websites over their event streams, closed apps by silent push.
fn event_to_account(state: &AppState, account_id: &str, event: &'static str) {
    state.hub.account(account_id, event, json!({}));
    push::spawn_event_to_account(state.db.clone(), state.pusher.clone(), account_id, event);
}

/// A silent push only, for when the event stream gets a more specific event.
fn push_event_to_account(state: &AppState, account_id: &str, event: &'static str) {
    push::spawn_event_to_account(state.db.clone(), state.pusher.clone(), account_id, event);
}

/// The account changed (email, verification, display name): streams get the new account.
fn account_changed(state: &AppState, account_id: &str, push_event: Option<&'static str>) -> Result<(), ApiError> {
    if let Some(account) = state.db.account_by_id(account_id)? {
        state.hub.account(account_id, "account_updated", json!({"account": account.json()}));
    }
    if let Some(event) = push_event {
        push_event_to_account(state, account_id, event);
    }
    Ok(())
}

fn event_to_device(state: &AppState, device: Device, event: &'static str) {
    push::spawn_event(state.db.clone(), state.pusher.clone(), device, event);
}

/// Sends `event` to every device of the account, waiting until it's gone
/// out: callers delete the devices right after.
async fn event_to_account_now(state: &AppState, account_id: &str, event: &'static str) {
    let devices = state.db.devices_for_account(account_id).unwrap_or_default();
    futures::future::join_all(devices.iter().map(|device| state.pusher.send_event(device, event))).await;
}

async fn issue_verification_email(state: &AppState, account: &Account) -> Result<(), ApiError> {
    if !state.email.enabled {
        return Err(ApiError::new(503, "Email verification is not configured"));
    }
    let token = state.db.create_email_verification(&account.account_id, &account.email)?;
    state.email.send_email_verification(&account.email, &token).await.map_err(|error| {
        tracing::error!("verification email to {} failed: {error}", account.email);
        ApiError::new(502, "The verification email could not be sent. Please try again.")
    })
}

fn client_ip(headers: &HeaderMap, peer: SocketAddr) -> String {
    let forwarded = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .unwrap_or("");
    if forwarded.is_empty() { peer.ip().to_string() } else { forwarded.to_string() }
}

fn signed_in(account: &Account, token: String) -> Json<Value> {
    Json(json!({"account": account.json(), "session": {"session_token": token}}))
}

async fn health(State(state): Shared) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "apns_enabled": state.pusher.apns.is_some(),
        "fcm_enabled": state.pusher.fcm.is_some(),
        "email_enabled": state.email.enabled,
    }))
}

#[derive(Deserialize)]
struct Credentials {
    email: String,
    password: String,
}

async fn register(State(state): Shared, Body(request): Body<Credentials>) -> ApiResult {
    min_length(&request.email, 3, "email")?;
    valid_password(&request.password, "password")?;
    let email = valid_email(&request.email)?;
    if !state.email.enabled {
        return Err(ApiError::new(503, "Email verification is not configured"));
    }
    let (hash, salt) = util::hash_password(&request.password);
    let account = state.db.create_account(&email, &hash, &salt)?.ok_or_else(|| ApiError::new(409, "Account already exists"))?;
    issue_verification_email(&state, &account).await?;
    let token = state.db.create_session(&account.account_id)?;
    Ok(signed_in(&account, token))
}

async fn login(State(state): Shared, Body(request): Body<Credentials>) -> ApiResult {
    min_length(&request.email, 3, "email")?;
    min_length(&request.password, 1, "password")?;
    let email = valid_email(&request.email)?;
    let invalid = || ApiError::new(401, "Invalid email or password");
    let account = state.db.account_by_email(&email)?.ok_or_else(invalid)?;
    match util::check_password(&request.password, &account.password_hash, &account.password_salt) {
        PasswordCheck::Wrong => return Err(invalid()),
        PasswordCheck::Right => {}
        PasswordCheck::RightButOld => {
            let (hash, salt) = util::hash_password(&request.password);
            state.db.set_password(&account.account_id, &hash, &salt)?;
        }
    }
    let token = state.db.create_session(&account.account_id)?;
    Ok(signed_in(&account, token))
}

async fn logout(State(state): Shared, headers: HeaderMap) -> ApiResult {
    let (account, session) = require_session(&state, &headers).await?;
    match &session.device_id {
        Some(device_id) => {
            if let Some(device) = state.db.device(&account.account_id, device_id)? {
                event_to_device(&state, device, "signed_out");
            }
            let sessions = state.db.session_hashes_for_device(&account.account_id, device_id)?;
            state.db.delete_sessions_for_device(&account.account_id, device_id)?;
            state.hub.signed_out(&account.account_id, sessions);
            if state.db.delete_device(&account.account_id, device_id)? {
                state.hub.account(&account.account_id, "devices_updated", json!({}));
            }
        }
        None => {
            state.db.delete_session(&session.token_hash)?;
            state.hub.signed_out(&account.account_id, vec![session.token_hash.clone()]);
        }
    }
    Ok(Json(json!({"logged_out": true})))
}

/// GET /events: this session's realtime event stream (Server-Sent Events).
async fn events(State(state): Shared, headers: HeaderMap) -> Result<Response, ApiError> {
    let (account, session) = require_session(&state, &headers).await?;
    Ok(state.hub.stream(account.json(), account.account_id.clone(), session.token_hash).into_response())
}

#[derive(Deserialize)]
struct DisplayNameRequest {
    display_name: Option<String>,
}

/// POST /account/display-name: sets the optional name shown for the account; empty clears it.
async fn set_display_name(State(state): Shared, headers: HeaderMap, Body(request): Body<DisplayNameRequest>) -> ApiResult {
    let (account, _) = require_session(&state, &headers).await?;
    let name = request.display_name.as_deref().map(|n| n.split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default();
    if name.chars().count() > 60 {
        return Err(ApiError::new(422, "display_name must be 60 characters or fewer"));
    }
    state.db.set_display_name(&account.account_id, (!name.is_empty()).then_some(name.as_str()))?;
    account_changed(&state, &account.account_id, None)?;
    let updated = state.db.account_by_id(&account.account_id)?.ok_or_else(|| ApiError::new(401, "Invalid session"))?;
    Ok(Json(json!({"account": updated.json()})))
}

async fn auth_me(State(state): Shared, headers: HeaderMap) -> ApiResult {
    let (account, _) = require_session(&state, &headers).await?;
    Ok(Json(json!({"account": account.json()})))
}

async fn resend_verification(State(state): Shared, headers: HeaderMap) -> ApiResult {
    let (account, _) = require_session(&state, &headers).await?;
    if !account.email_verified {
        issue_verification_email(&state, &account).await?;
    }
    Ok(Json(json!({"accepted": true})))
}

#[derive(Deserialize)]
struct TokenRequest {
    token: String,
}

async fn verify_email(State(state): Shared, Body(request): Body<TokenRequest>) -> ApiResult {
    min_length(&request.token, 1, "token")?;
    let verification =
        state.db.email_verification(&request.token)?.ok_or_else(|| ApiError::new(404, "Verification token not found"))?;
    if verification.used_at.is_some() {
        return Err(ApiError::new(400, "Verification token has already been used"));
    }
    if epoch_from_iso(&verification.expires_at).unwrap_or(0) < now_epoch() {
        return Err(ApiError::new(400, "Verification token has expired"));
    }
    let account = state.db.account_by_id(&verification.account_id)?;
    let Some(account) = account.filter(|a| a.email == verification.email) else {
        return Err(ApiError::new(400, "Verification token is no longer valid"));
    };
    state.db.mark_email_verified(&account.account_id)?;
    state.db.use_email_verification(&request.token)?;
    account_changed(&state, &account.account_id, Some("email_verified"))?;
    Ok(Json(json!({"verified": true})))
}

#[derive(Deserialize)]
struct EmailRequest {
    email: String,
}

async fn request_password_reset(State(state): Shared, Body(request): Body<EmailRequest>) -> ApiResult {
    min_length(&request.email, 3, "email")?;
    let email = valid_email(&request.email)?;
    let Some(account) = state.db.account_by_email(&email)? else {
        return Ok(Json(json!({"accepted": true})));
    };
    if !state.email.enabled {
        return Err(ApiError::new(503, "Password reset email is not configured"));
    }
    let token = state.db.create_password_reset(&account.account_id, &account.email)?;
    state.email.send_password_reset(&account.email, &token).await.map_err(|error| {
        tracing::error!("reset email to {} failed: {error}", account.email);
        ApiError::new(502, "The password reset email could not be sent. Please try again.")
    })?;
    Ok(Json(json!({"accepted": true})))
}

#[derive(Deserialize)]
struct ResetRequest {
    token: String,
    new_password: String,
}

/// Signs every device of the account out: after a password change or reset.
async fn sign_out_everywhere(state: &AppState, account_id: &str) -> Result<(), ApiError> {
    event_to_account_now(state, account_id, "signed_out").await;
    state.db.delete_sessions_for_account(account_id)?;
    state.hub.signed_out_everywhere(account_id);
    state.db.delete_devices_for_account(account_id)?;
    Ok(())
}

async fn reset_password(State(state): Shared, Body(request): Body<ResetRequest>) -> ApiResult {
    min_length(&request.token, 1, "token")?;
    valid_password(&request.new_password, "new_password")?;
    let reset = state.db.password_reset(&request.token)?.ok_or_else(|| ApiError::new(404, "Reset token not found"))?;
    if reset.used_at.is_some() {
        return Err(ApiError::new(400, "Reset token has already been used"));
    }
    if epoch_from_iso(&reset.expires_at).unwrap_or(0) < now_epoch() {
        return Err(ApiError::new(400, "Reset token has expired"));
    }
    let (hash, salt) = util::hash_password(&request.new_password);
    state.db.set_password(&reset.account_id, &hash, &salt)?;
    state.db.use_password_reset(&request.token)?;
    sign_out_everywhere(&state, &reset.account_id).await?;
    Ok(Json(json!({"password_reset": true})))
}

#[derive(Deserialize)]
struct ChangeEmailRequest {
    new_email: String,
    password: String,
}

fn password_matches(state: &AppState, account: &Account, password: &str) -> Result<bool, ApiError> {
    Ok(match util::check_password(password, &account.password_hash, &account.password_salt) {
        PasswordCheck::Wrong => false,
        PasswordCheck::Right => true,
        PasswordCheck::RightButOld => {
            let (hash, salt) = util::hash_password(password);
            state.db.set_password(&account.account_id, &hash, &salt)?;
            true
        }
    })
}

async fn change_email(State(state): Shared, headers: HeaderMap, Body(request): Body<ChangeEmailRequest>) -> ApiResult {
    min_length(&request.new_email, 3, "new_email")?;
    min_length(&request.password, 1, "password")?;
    let (account, _) = require_session(&state, &headers).await?;
    if !state.email.enabled {
        return Err(ApiError::new(503, "Email verification is not configured"));
    }
    if !password_matches(&state, &account, &request.password)? {
        return Err(ApiError::new(401, "Password is incorrect"));
    }
    let new_email = valid_email(&request.new_email)?;
    let updated =
        state.db.change_email(&account.account_id, &new_email)?.ok_or_else(|| ApiError::new(409, "Account already exists"))?;
    issue_verification_email(&state, &updated).await?;
    account_changed(&state, &updated.account_id, Some("email_verification_required"))?;
    Ok(Json(json!({"account": updated.json()})))
}

#[derive(Deserialize)]
struct ChangePasswordRequest {
    current_password: String,
    new_password: String,
}

async fn change_password(State(state): Shared, headers: HeaderMap, Body(request): Body<ChangePasswordRequest>) -> ApiResult {
    min_length(&request.current_password, 1, "current_password")?;
    valid_password(&request.new_password, "new_password")?;
    let (account, _) = require_session(&state, &headers).await?;
    if !password_matches(&state, &account, &request.current_password)? {
        return Err(ApiError::new(401, "Current password is incorrect"));
    }
    let (hash, salt) = util::hash_password(&request.new_password);
    state.db.set_password(&account.account_id, &hash, &salt)?;
    sign_out_everywhere(&state, &account.account_id).await?;
    Ok(Json(json!({"password_changed": true})))
}

#[derive(Deserialize)]
struct PasswordRequest {
    password: String,
}

async fn delete_account(State(state): Shared, headers: HeaderMap, Body(request): Body<PasswordRequest>) -> ApiResult {
    min_length(&request.password, 1, "password")?;
    let (account, _) = require_session(&state, &headers).await?;
    if !password_matches(&state, &account, &request.password)? {
        return Err(ApiError::new(401, "Password is incorrect"));
    }
    event_to_account_now(&state, &account.account_id, "account_deleted").await;
    state.db.delete_account(&account.account_id)?;
    state.hub.account_deleted(&account.account_id);
    Ok(Json(json!({"deleted": true})))
}

#[derive(Deserialize)]
struct WebPushKeys {
    p256dh: String,
    auth: String,
}

#[derive(Deserialize)]
struct WebPushSubscription {
    endpoint: String,
    keys: WebPushKeys,
}

#[derive(Deserialize)]
struct DeviceRegistration {
    device_id: String,
    device_name: String,
    apns_token: Option<String>,
    fcm_token: Option<String>,
    web_push: Option<WebPushSubscription>,
    #[serde(default = "default_environment")]
    apns_environment: String,
    #[serde(default = "default_platform")]
    platform: String,
    #[serde(default = "default_sound")]
    notification_sound: String,
    /// Sent by apps that keep their badge up to date; older apps don't, and get no badge numbers.
    #[serde(default)]
    badges: bool,
}

fn default_environment() -> String {
    "sandbox".into()
}
fn default_platform() -> String {
    "ios".into()
}
fn default_sound() -> String {
    "default".into()
}

async fn register_device(
    State(state): Shared,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Body(request): Body<DeviceRegistration>,
) -> ApiResult {
    min_length(&request.device_id, 1, "device_id")?;
    min_length(&request.device_name, 1, "device_name")?;
    let (account, session) = require_session(&state, &headers).await?;
    let platform = match request.platform.trim().to_lowercase() {
        p if p.is_empty() => "ios".to_string(),
        p => p,
    };
    let mut device = Device {
        device_id: request.device_id.clone(),
        account_id: account.account_id.clone(),
        device_name: request.device_name.trim().to_string(),
        platform: platform.clone(),
        notification_sound: util::normalize_sound(&request.notification_sound),
        last_ip_address: client_ip(&headers, peer),
        last_seen: iso_now(),
        badges: request.badges,
        ..Default::default()
    };
    match platform.as_str() {
        "android" => {
            let token = request.fcm_token.as_deref().map(str::trim).unwrap_or("");
            if token.is_empty() {
                return Err(ApiError::new(400, "fcm_token is required for Android devices"));
            }
            device.fcm_token = Some(token.to_string());
        }
        "web" => {
            let Some(subscription) = request.web_push else {
                return Err(ApiError::new(400, "web_push is required for browsers"));
            };
            if !subscription.endpoint.starts_with("https://") {
                return Err(ApiError::new(400, "web_push.endpoint must be an https URL"));
            }
            device.web_endpoint = Some(subscription.endpoint);
            device.web_p256dh = Some(subscription.keys.p256dh);
            device.web_auth = Some(subscription.keys.auth);
        }
        _ => {
            let token = request.apns_token.as_deref().map(str::trim).unwrap_or("");
            if token.is_empty() {
                return Err(ApiError::new(400, "apns_token is required for iOS devices"));
            }
            let environment = request.apns_environment.trim().to_lowercase();
            if environment != "sandbox" && environment != "production" {
                return Err(ApiError::new(422, "apns_environment must be 'sandbox' or 'production'"));
            }
            device.apns_token = token.to_string();
            device.apns_environment = environment;
        }
    }
    state.db.upsert_device(&device)?;
    state.db.bind_session_to_device(&session.token_hash, &device.device_id)?;
    event_to_account(&state, &account.account_id, "devices_updated");
    Ok(Json(json!({"device": device.json()})))
}

async fn list_devices(State(state): Shared, headers: HeaderMap) -> ApiResult {
    let (account, _) = require_verified_session(&state, &headers).await?;
    let devices: Vec<Value> = state.db.devices_for_account(&account.account_id)?.iter().map(Device::json).collect();
    Ok(Json(json!({"devices": devices})))
}

#[derive(Deserialize)]
struct SoundRequest {
    device_id: String,
    #[serde(default = "default_sound")]
    notification_sound: String,
}

async fn device_sound(State(state): Shared, headers: HeaderMap, Body(request): Body<SoundRequest>) -> ApiResult {
    min_length(&request.device_id, 1, "device_id")?;
    let (account, _) = require_verified_session(&state, &headers).await?;
    let device = state
        .db
        .set_device_sound(&account.account_id, &request.device_id, &util::normalize_sound(&request.notification_sound))?
        .ok_or_else(|| ApiError::new(404, "Device not found"))?;
    state.hub.account(&account.account_id, "devices_updated", json!({}));
    Ok(Json(json!({"device": device.json()})))
}

async fn remove_device(State(state): Shared, headers: HeaderMap, Path(device_id): Path<String>) -> ApiResult {
    let (account, _) = require_session(&state, &headers).await?;
    if let Some(device) = state.db.device(&account.account_id, &device_id)? {
        event_to_device(&state, device, "signed_out");
    }
    if !state.db.delete_device(&account.account_id, &device_id)? {
        return Err(ApiError::new(404, "Device not found"));
    }
    event_to_account(&state, &account.account_id, "devices_updated");
    Ok(Json(json!({"deleted": true})))
}

async fn sign_out_device(State(state): Shared, headers: HeaderMap, Path(device_id): Path<String>) -> ApiResult {
    let (account, _) = require_verified_session(&state, &headers).await?;
    if let Some(device) = state.db.device(&account.account_id, &device_id)? {
        event_to_device(&state, device, "signed_out");
    }
    let deleted = state.db.delete_device(&account.account_id, &device_id)?;
    let sessions = state.db.session_hashes_for_device(&account.account_id, &device_id)?;
    let deleted_sessions = state.db.delete_sessions_for_device(&account.account_id, &device_id)?;
    state.hub.signed_out(&account.account_id, sessions);
    if !deleted && deleted_sessions == 0 {
        return Err(ApiError::new(404, "Device not found"));
    }
    event_to_account(&state, &account.account_id, "devices_updated");
    Ok(Json(json!({"signed_out": true})))
}

async fn sign_out_other_devices(State(state): Shared, headers: HeaderMap) -> ApiResult {
    let (account, session) = require_verified_session(&state, &headers).await?;
    let others: Vec<Device> = state
        .db
        .devices_for_account(&account.account_id)?
        .into_iter()
        .filter(|device| Some(&device.device_id) != session.device_id.as_ref())
        .collect();
    let mut deleted_devices = 0;
    let mut deleted_sessions = 0;
    for device in others {
        let sessions = state.db.session_hashes_for_device(&account.account_id, &device.device_id)?;
        deleted_sessions += state.db.delete_sessions_for_device(&account.account_id, &device.device_id)?;
        state.hub.signed_out(&account.account_id, sessions);
        if state.db.delete_device(&account.account_id, &device.device_id)? {
            deleted_devices += 1;
        }
        event_to_device(&state, device, "signed_out");
    }
    event_to_account(&state, &account.account_id, "devices_updated");
    Ok(Json(json!({"signed_out": true, "deleted_devices": deleted_devices, "deleted_sessions": deleted_sessions})))
}

async fn list_api_keys(State(state): Shared, headers: HeaderMap) -> ApiResult {
    let (account, _) = require_verified_session(&state, &headers).await?;
    let keys: Vec<Value> = state
        .db
        .api_keys_for_account(&account.account_id)?
        .into_iter()
        .map(|k| json!({"key_id": k.key_id, "label": k.label, "api_key": k.api_key, "created_at": k.created_at, "last_used_at": k.last_used_at}))
        .collect();
    Ok(Json(json!({"api_keys": keys})))
}

#[derive(Deserialize)]
struct LabelRequest {
    label: String,
}

async fn create_api_key(State(state): Shared, headers: HeaderMap, Body(request): Body<LabelRequest>) -> ApiResult {
    min_length(&request.label, 1, "label")?;
    let (account, _) = require_verified_session(&state, &headers).await?;
    let key = state.db.create_api_key(&account.account_id, request.label.trim())?;
    event_to_account(&state, &account.account_id, "api_keys_updated");
    Ok(Json(json!({"api_key": {"key_id": key.key_id, "label": key.label, "api_key": key.api_key, "created_at": key.created_at}})))
}

#[derive(Deserialize)]
struct KeyIdRequest {
    key_id: String,
}

async fn delete_api_key(State(state): Shared, headers: HeaderMap, Body(request): Body<KeyIdRequest>) -> ApiResult {
    min_length(&request.key_id, 1, "key_id")?;
    let (account, _) = require_verified_session(&state, &headers).await?;
    if !state.db.delete_api_key(&account.account_id, &request.key_id)? {
        return Err(ApiError::new(404, "API key not found"));
    }
    event_to_account(&state, &account.account_id, "api_keys_updated");
    Ok(Json(json!({"deleted": true})))
}

async fn delete_all_api_keys(State(state): Shared, headers: HeaderMap) -> ApiResult {
    let (account, _) = require_verified_session(&state, &headers).await?;
    let deleted_count = state.db.delete_api_keys_for_account(&account.account_id)?;
    event_to_account(&state, &account.account_id, "api_keys_updated");
    Ok(Json(json!({"deleted": true, "deleted_count": deleted_count})))
}

async fn list_notifications(State(state): Shared, headers: HeaderMap) -> ApiResult {
    let (account, _) = require_verified_session(&state, &headers).await?;
    let items: Vec<Value> = state.db.notifications_for_account(&account.account_id)?.iter().map(|n| n.json()).collect();
    let unread_count = state.db.unread_count(&account.account_id)?;
    Ok(Json(json!({"notifications": items, "unread_count": unread_count})))
}

#[derive(Deserialize)]
struct MarkSeenRequest {
    /// Only these; all of the account's notifications when absent.
    notification_ids: Option<Vec<String>>,
}

/// POST /notifications/mark-seen: the list was looked at. Updates every
/// device's view and the iOS badges, and returns this device's new badge.
async fn mark_seen(State(state): Shared, headers: HeaderMap, Body(request): Body<MarkSeenRequest>) -> ApiResult {
    let (account, session) = require_verified_session(&state, &headers).await?;
    let changed = state.db.mark_seen(&account.account_id, request.notification_ids.as_deref())?;
    if !changed.is_empty() {
        state.hub.account(&account.account_id, "notifications_seen", json!({"notification_ids": changed}));
        push::spawn_badge_updates(state.db.clone(), state.pusher.clone(), &account.account_id);
    }
    let badge = match &session.device_id {
        Some(device_id) => Some(state.db.device_badge(device_id)?),
        None => None,
    };
    Ok(Json(json!({
        "marked": changed.len(),
        "unread_count": state.db.unread_count(&account.account_id)?,
        "badge": badge,
    })))
}

async fn delete_notification(State(state): Shared, headers: HeaderMap, Path(notification_id): Path<String>) -> ApiResult {
    let (account, _) = require_verified_session(&state, &headers).await?;
    let Some(was_unread) = state.db.delete_notification(&account.account_id, &notification_id)? else {
        return Err(ApiError::new(404, "Notification not found"));
    };
    if was_unread {
        push::spawn_badge_updates(state.db.clone(), state.pusher.clone(), &account.account_id);
    }
    state.hub.account(&account.account_id, "notification_deleted", json!({"notification_id": notification_id}));
    push_event_to_account(&state, &account.account_id, "notifications_updated");
    Ok(Json(json!({"deleted": true})))
}

async fn clear_notifications(State(state): Shared, headers: HeaderMap) -> ApiResult {
    let (account, _) = require_verified_session(&state, &headers).await?;
    let had_unread = state.db.unread_count(&account.account_id)? > 0;
    let deleted_count = state.db.clear_notifications(&account.account_id)?;
    if had_unread {
        push::spawn_badge_updates(state.db.clone(), state.pusher.clone(), &account.account_id);
    }
    state.hub.account(&account.account_id, "notifications_cleared", json!({}));
    push_event_to_account(&state, &account.account_id, "notifications_updated");
    Ok(Json(json!({"deleted_count": deleted_count})))
}

#[derive(Deserialize)]
struct AccountNotifyRequest {
    title: String,
    message: String,
    #[serde(default = "default_sound")]
    sound: String,
}

async fn account_notify(State(state): Shared, headers: HeaderMap, Body(request): Body<AccountNotifyRequest>) -> ApiResult {
    min_length(&request.title, 1, "title")?;
    min_length(&request.message, 1, "message")?;
    let (account, _) = require_verified_session(&state, &headers).await?;
    let delivery = push::deliver_to_account(
        &state.db, &state.pusher, &state.hub, &account.account_id, &request.title, &request.message, &request.sound, false, None,
    )
    .await?;
    Ok(Json(json!({"status": "sent", "title": request.title, "message": request.message, "delivery": delivery})))
}

#[derive(Deserialize)]
struct NotifyRequest {
    key: String,
    msg: String,
    #[serde(default = "default_title")]
    title: String,
    #[serde(default = "default_sound")]
    sound: String,
}

fn default_title() -> String {
    "MG Notify".into()
}

/// GET /notify?key=...&msg=... (and the same fields as a POST body): sends
/// to the account of every valid key in a comma-separated list.
async fn notify(state: &AppState, request: NotifyRequest) -> ApiResult {
    let requested: Vec<&str> = request.key.split(',').map(str::trim).filter(|k| !k.is_empty()).collect();
    let mut valid = Vec::new();
    let mut ignored = Vec::new();
    for key in &requested {
        match state.db.api_key(key)? {
            Some(record) => valid.push(record),
            None => ignored.push(key.to_string()),
        }
    }
    if valid.is_empty() {
        return Err(ApiError::new(401, "Invalid API key"));
    }
    let mut deliveries = Vec::new();
    let mut delivered_accounts = HashSet::new();
    for key in &valid {
        if delivered_accounts.contains(&key.account_id) {
            continue;
        }
        let Some(account) = state.db.account_by_id(&key.account_id)? else { continue };
        if !account.email_verified {
            continue;
        }
        state.db.touch_api_key(&key.key_id)?;
        let delivery = push::deliver_to_account(
            &state.db, &state.pusher, &state.hub, &key.account_id, &request.title, &request.msg, &request.sound, false, None,
        )
        .await?;
        deliveries.push(json!({"account_id": key.account_id, "delivery": delivery}));
        delivered_accounts.insert(key.account_id.clone());
    }
    if deliveries.is_empty() {
        return Err(ApiError::new(401, "Invalid API key"));
    }
    if deliveries.len() == 1 && requested.len() == 1 {
        let delivery = deliveries.pop().map(|d| d["delivery"].clone()).unwrap_or(Value::Null);
        return Ok(Json(json!({"status": "sent", "title": request.title, "message": request.msg, "delivery": delivery})));
    }
    Ok(Json(json!({
        "status": "sent",
        "title": request.title,
        "message": request.msg,
        "requested_keys": requested.len(),
        "valid_keys": valid.len(),
        "ignored_keys": ignored,
        "delivered_accounts": deliveries.len(),
        "deliveries": deliveries,
    })))
}

async fn notify_get(State(state): Shared, QueryParams(request): QueryParams<NotifyRequest>) -> ApiResult {
    notify(&state, request).await
}

async fn notify_post(State(state): Shared, Body(request): Body<NotifyRequest>) -> ApiResult {
    notify(&state, request).await
}

async fn web_push_key(State(state): Shared) -> Json<Value> {
    Json(json!({"public_key": state.pusher.web.public_key()}))
}

async fn inbound_email(State(state): Shared, headers: HeaderMap, body: axum::body::Bytes) -> ApiResult {
    let Some(expected) = state.settings.brevo_webhook_secret() else {
        return Err(ApiError::new(503, "Inbound email is not configured"));
    };
    let given = headers.get("x-mg-notify-webhook-secret").and_then(|v| v.to_str().ok()).unwrap_or("");
    if !util::constant_time_eq(given.as_bytes(), expected.as_bytes()) {
        return Err(ApiError::new(401, "Invalid webhook secret"));
    }
    let payload: Value = serde_json::from_slice(&body).map_err(|_| ApiError::new(400, "Invalid JSON payload"))?;
    let items = match payload.get("items") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.clone(),
        Some(_) => return Err(ApiError::new(400, "items must be an array")),
    };
    let (mut processed, mut duplicates, mut delivered, mut ignored) = (0, 0, 0, 0);
    for item in items.iter().filter(|item| item.is_object()) {
        let outcome = process_inbound_email(&state, item).await?;
        if outcome.duplicate {
            duplicates += 1;
            continue;
        }
        processed += 1;
        delivered += outcome.delivered;
        ignored += outcome.ignored;
    }
    Ok(Json(json!({
        "status": "accepted",
        "processed_messages": processed,
        "duplicate_messages": duplicates,
        "delivered_accounts": delivered,
        "ignored_recipients": ignored,
    })))
}

/// What became of one inbound email.
pub struct InboundOutcome {
    /// Already processed (same Message-ID): nothing was sent.
    pub duplicate: bool,
    pub delivered: usize,
    pub ignored: usize,
}

/// The API key an inbound address names: `<key>@<inbound domain>`, case
/// ignored, belonging to a verified account.
pub fn inbound_recipient(state: &AppState, address: &str) -> Result<Option<crate::db::ApiKey>, ApiError> {
    let Some((local, domain)) = address.trim().rsplit_once('@') else { return Ok(None) };
    if !domain.eq_ignore_ascii_case(&state.settings.inbound_email_domain) || local.is_empty() {
        return Ok(None);
    }
    let Some(key) = state.db.api_key_ignoring_case(local)? else { return Ok(None) };
    Ok(match state.db.account_by_id(&key.account_id)? {
        Some(account) if account.email_verified => Some(key),
        _ => None,
    })
}

/// Turns one inbound email (in the webhook's item format, which the SMTP
/// receiver builds too) into a notification for each account it names.
pub async fn process_inbound_email(state: &AppState, item: &Value) -> Result<InboundOutcome, ApiError> {
    let mut message_id = item.get("MessageId").map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).unwrap_or_default();
    message_id = message_id.trim().to_string();
    if message_id.is_empty() || message_id == "null" {
        use sha2::Digest;
        message_id = format!("sha256:{}", hex::encode(sha2::Sha256::digest(canonical_json(item).as_bytes())));
    }
    if state.db.inbound_email_processed(&message_id)? {
        return Ok(InboundOutcome { duplicate: true, delivered: 0, ignored: 0 });
    }
    let (name, address) = email::sender_mailbox(item.get("From").unwrap_or(&Value::Null));
    let name = email::single_line(&name, "Unknown sender", 160);
    let address = email::single_line(&address, "unknown", 254);
    let subject_text = match item.get("Subject") {
        Some(Value::String(s)) => s.clone(),
        None | Some(Value::Null) => String::new(),
        Some(other) => other.to_string(),
    };
    let subject = email::single_line(&subject_text, "(no subject)", 300);
    let title = format!("{name} <{address}>: {subject}");
    let (message, html) = email::email_content(item).await;

    let recipients = match item.get("Recipients").filter(|v| !is_empty_value(v)).or_else(|| item.get("To")) {
        Some(Value::Array(list)) => list.clone(),
        None | Some(Value::Null) => Vec::new(),
        Some(single) => vec![single.clone()],
    };
    let (mut delivered, mut ignored) = (0, 0);
    let mut delivered_to = HashSet::new();
    for recipient in &recipients {
        let Some(key) = inbound_recipient(state, &email::mailbox_address(recipient))? else {
            ignored += 1;
            continue;
        };
        if !delivered_to.insert(key.account_id.clone()) {
            ignored += 1;
            continue;
        }
        state.db.touch_api_key(&key.key_id)?;
        push::deliver_to_account(&state.db, &state.pusher, &state.hub, &key.account_id, &title, &message, "default", true, Some(html.clone()))
            .await?;
        delivered += 1;
    }
    state.db.mark_inbound_email_processed(&message_id)?;
    Ok(InboundOutcome { duplicate: false, delivered, ignored })
}

fn is_empty_value(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(a) => a.is_empty(),
        Value::String(s) => s.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

/// JSON with sorted keys and no spaces, for hashing messages without an ID.
fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> =
                keys.iter().map(|k| format!("{}:{}", serde_json::to_string(k).unwrap(), canonical_json(&map[*k]))).collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(items) => format!("[{}]", items.iter().map(canonical_json).collect::<Vec<_>>().join(",")),
        other => other.to_string(),
    }
}

async fn not_found(method: Method) -> ApiError {
    let _ = method;
    ApiError::new(404, "Not Found")
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/email/inbound", post(inbound_email))
        .route("/auth/register", post(register))
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(auth_me))
        .route("/events", get(events))
        .route("/account/display-name", post(set_display_name))
        .route("/auth/resend-verification", post(resend_verification))
        .route("/auth/verify-email", post(verify_email))
        .route("/auth/request-password-reset", post(request_password_reset))
        .route("/auth/reset-password", post(reset_password))
        .route("/account/change-email", post(change_email))
        .route("/account/change-password", post(change_password))
        .route("/account/delete", post(delete_account))
        .route("/account/notify", post(account_notify))
        .route("/devices/register", post(register_device))
        .route("/devices", get(list_devices))
        .route("/devices/sound", post(device_sound))
        .route("/devices/sign-out-all", post(sign_out_other_devices))
        .route("/devices/sign-out-other", post(sign_out_other_devices))
        .route("/devices/{device_id}", delete(remove_device))
        .route("/devices/{device_id}/sign-out", post(sign_out_device))
        .route("/api-keys", get(list_api_keys))
        .route("/api-keys/create", post(create_api_key))
        .route("/api-keys/delete", post(delete_api_key))
        .route("/api-keys/delete-all", post(delete_all_api_keys))
        .route("/notifications", get(list_notifications).delete(clear_notifications))
        .route("/api/notifications", get(list_notifications).delete(clear_notifications))
        .route("/notifications/mark-seen", post(mark_seen))
        .route("/notifications/{notification_id}", delete(delete_notification))
        .route("/api/notifications/{notification_id}", delete(delete_notification))
        .route("/notify", get(notify_get).post(notify_post))
        .route("/web-push/public-key", get(web_push_key))
        .fallback(not_found)
        .method_not_allowed_fallback(|| async { ApiError::new(405, "Method Not Allowed") })
        .with_state(state)
}
