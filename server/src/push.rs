//! Delivering pushes: APNs for iOS, FCM for Android, Web Push for browsers.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::{URL_SAFE_NO_PAD, STANDARD};
use futures::future::join_all;
use hkdf::Hkdf;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::pkcs8::DecodePrivateKey;
use p256::{PublicKey, SecretKey};
use rand::RngCore;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use crate::config::Settings;
use crate::db::{Db, Device, Notification};
use crate::events::Hub;
use crate::util::{iso_now, now_epoch};

pub const EMAIL_PUSH_PREVIEW_BODY: &str = "Open MG Notify to view the full email.";
const APNS_SANDBOX_URL: &str = "https://api.sandbox.push.apple.com";
const APNS_PRODUCTION_URL: &str = "https://api.push.apple.com";

/// What became of one push to one device.
pub enum Outcome {
    Sent { apns_id: Option<String>, fcm_name: Option<String> },
    /// The push service says this device is gone for good: forget it.
    Gone(String),
    Failed(String),
}

/// An ES256 JSON Web Token, as APNs and Web Push (VAPID) both want.
fn es256_jwt(key: &SigningKey, header: Value, claims: Value) -> String {
    let encode = |v: &Value| URL_SAFE_NO_PAD.encode(v.to_string());
    let input = format!("{}.{}", encode(&header), encode(&claims));
    let signature: Signature = key.sign(input.as_bytes());
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()))
}

pub struct Apns {
    team_id: String,
    key_id: String,
    bundle_id: String,
    key: SigningKey,
    token: Mutex<Option<(String, i64)>>,
}

impl Apns {
    fn new(settings: &Settings) -> Option<Self> {
        let (team_id, key_id, bundle_id, path) = (
            settings.apns_team_id.clone()?,
            settings.apns_key_id.clone()?,
            settings.apns_bundle_id.clone()?,
            settings.apns_auth_key_path.clone()?,
        );
        let pem = std::fs::read_to_string(&path).map_err(|e| tracing::error!("APNs key {}: {e}", path.display())).ok()?;
        let key = SigningKey::from_pkcs8_pem(&pem).map_err(|e| tracing::error!("APNs key is not usable: {e}")).ok()?;
        Some(Self { team_id, key_id, bundle_id, key, token: Mutex::new(None) })
    }

    async fn authorization(&self) -> String {
        let mut token = self.token.lock().await;
        let now = now_epoch();
        if let Some((value, issued)) = token.as_ref() {
            if now - issued < 3000 {
                return value.clone();
            }
        }
        let value = es256_jwt(&self.key, json!({"alg": "ES256", "kid": self.key_id}), json!({"iss": self.team_id, "iat": now}));
        *token = Some((value.clone(), now));
        value
    }

    async fn send(&self, http: &reqwest::Client, device: &Device, payload: Value, push_type: &str, priority: &str) -> Outcome {
        let base = if device.apns_environment == "sandbox" { APNS_SANDBOX_URL } else { APNS_PRODUCTION_URL };
        let response = http
            .post(format!("{base}/3/device/{}", device.apns_token))
            .header("authorization", format!("bearer {}", self.authorization().await))
            .header("apns-topic", &self.bundle_id)
            .header("apns-push-type", push_type)
            .header("apns-priority", priority)
            .json(&payload)
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => return Outcome::Failed(format!("APNs request failed: {error}")),
        };
        let status = response.status().as_u16();
        let apns_id = response.headers().get("apns-id").and_then(|v| v.to_str().ok()).map(String::from);
        if status < 400 {
            return Outcome::Sent { apns_id, fcm_name: None };
        }
        let body: Value = response.json().await.unwrap_or(Value::Null);
        let reason = body.get("reason").and_then(Value::as_str).unwrap_or("").to_string();
        let error = format!("APNs delivery failed ({status} {reason})");
        if status == 410 || reason == "BadDeviceToken" || reason == "Unregistered" { Outcome::Gone(error) } else { Outcome::Failed(error) }
    }
}

struct FcmAccount {
    client_email: String,
    token_uri: String,
    key: jsonwebtoken::EncodingKey,
}

pub struct Fcm {
    project_id: String,
    account: FcmAccount,
    token: Mutex<Option<(String, i64)>>,
}

impl Fcm {
    fn new(settings: &Settings) -> Option<Self> {
        let project_id = settings.fcm_project_id.clone()?;
        let path = settings.fcm_service_account_path.clone()?;
        let load = || -> Result<FcmAccount, String> {
            let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let json: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            let field = |name: &str| json.get(name).and_then(Value::as_str).map(String::from);
            Ok(FcmAccount {
                client_email: field("client_email").ok_or("no client_email")?,
                token_uri: field("token_uri").unwrap_or_else(|| "https://oauth2.googleapis.com/token".into()),
                key: jsonwebtoken::EncodingKey::from_rsa_pem(field("private_key").ok_or("no private_key")?.as_bytes())
                    .map_err(|e| e.to_string())?,
            })
        };
        match load() {
            Ok(account) => Some(Self { project_id, account, token: Mutex::new(None) }),
            Err(error) => {
                tracing::error!("FCM is configured but its service account could not be loaded: {error}");
                None
            }
        }
    }

    /// An OAuth access token for FCM, from the service account; reused for 50 minutes.
    async fn access_token(&self, http: &reqwest::Client) -> Result<String, String> {
        let mut token = self.token.lock().await;
        let now = now_epoch();
        if let Some((value, expires)) = token.as_ref() {
            if now < *expires {
                return Ok(value.clone());
            }
        }
        let claims = json!({
            "iss": self.account.client_email,
            "scope": "https://www.googleapis.com/auth/firebase.messaging",
            "aud": self.account.token_uri,
            "iat": now,
            "exp": now + 3600,
        });
        let assertion = jsonwebtoken::encode(&jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256), &claims, &self.account.key)
            .map_err(|e| e.to_string())?;
        let response: Value = http
            .post(&self.account.token_uri)
            .form(&[("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"), ("assertion", assertion.as_str())])
            .send()
            .await
            .map_err(|e| format!("FCM sign-in failed: {e}"))?
            .json()
            .await
            .map_err(|e| format!("FCM sign-in failed: {e}"))?;
        let value = response
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("FCM sign-in failed: {response}"))?
            .to_string();
        *token = Some((value.clone(), now + 3000));
        Ok(value)
    }

    async fn send(&self, http: &reqwest::Client, message: Value) -> Outcome {
        let token = match self.access_token(http).await {
            Ok(token) => token,
            Err(error) => return Outcome::Failed(error),
        };
        let response = http
            .post(format!("https://fcm.googleapis.com/v1/projects/{}/messages:send", self.project_id))
            .bearer_auth(token)
            .json(&json!({"message": message}))
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => return Outcome::Failed(format!("FCM request failed: {error}")),
        };
        let status = response.status().as_u16();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if status < 400 {
            let name = body.get("name").and_then(Value::as_str).map(String::from);
            return Outcome::Sent { apns_id: None, fcm_name: name };
        }
        let unregistered = body["error"]["details"]
            .as_array()
            .is_some_and(|details| details.iter().any(|d| d["errorCode"] == "UNREGISTERED"))
            || body["error"]["status"] == "UNREGISTERED";
        let error = format!("FCM delivery failed ({status} {})", body["error"]["message"].as_str().unwrap_or(""));
        if status == 404 || unregistered { Outcome::Gone(error) } else { Outcome::Failed(error) }
    }
}

/// The Android app's notification channel for a sound. Android fixes a
/// channel's sound when it's created, so the app makes one channel per sound
/// choice, named exactly like this (NotificationSounds.channelIdForSound).
pub fn android_channel_id_for_sound(sound: &str) -> String {
    match sound {
        "" | "default" => "mg_notify".into(),
        "__none__" => "mg_notify_silent".into(),
        _ => format!("mg_notify_sound_{}", &hex::encode(Sha256::digest(sound.as_bytes()))[..16]),
    }
}

/// Web Push to browsers, with this server's own VAPID key (no Firebase involved).
pub struct WebPush {
    key: SigningKey,
    public_key: String,
    subject: String,
}

impl WebPush {
    /// Loads the server's VAPID key from the database, making one the first time.
    pub fn load(db: &Db, subject: String) -> Self {
        let stored = db.setting("vapid_private_key").ok().flatten().and_then(|value| URL_SAFE_NO_PAD.decode(value).ok());
        let secret = match stored.and_then(|bytes| SecretKey::from_slice(&bytes).ok()) {
            Some(secret) => secret,
            None => {
                let secret = SecretKey::random(&mut rand::rngs::OsRng);
                let _ = db.set_setting("vapid_private_key", &URL_SAFE_NO_PAD.encode(secret.to_bytes()));
                secret
            }
        };
        let public_key = URL_SAFE_NO_PAD.encode(secret.public_key().to_encoded_point(false).as_bytes());
        Self { key: SigningKey::from(&secret), public_key, subject }
    }

    pub fn public_key(&self) -> &str {
        &self.public_key
    }

    async fn send(&self, http: &reqwest::Client, device: &Device, payload: &Value) -> Outcome {
        let (Some(endpoint), Some(p256dh), Some(auth)) = (&device.web_endpoint, &device.web_p256dh, &device.web_auth) else {
            return Outcome::Gone("Browser subscription is incomplete".into());
        };
        let Ok(url) = url::Url::parse(endpoint) else { return Outcome::Gone("Browser subscription is invalid".into()) };
        let body = match encrypt_web_push(payload.to_string().as_bytes(), p256dh, auth) {
            Ok(body) => body,
            Err(error) => return Outcome::Gone(error),
        };
        let audience = url.origin().ascii_serialization();
        let jwt = es256_jwt(
            &self.key,
            json!({"typ": "JWT", "alg": "ES256"}),
            json!({"aud": audience, "exp": now_epoch() + 12 * 3600, "sub": self.subject}),
        );
        let response = http
            .post(endpoint)
            .header("Authorization", format!("vapid t={jwt}, k={}", self.public_key))
            .header("Content-Encoding", "aes128gcm")
            .header("Content-Type", "application/octet-stream")
            .header("TTL", "86400")
            .header("Urgency", "high")
            .body(body)
            .send()
            .await;
        match response {
            Ok(response) if response.status().is_success() => Outcome::Sent { apns_id: None, fcm_name: None },
            Ok(response) => {
                let status = response.status().as_u16();
                let error = format!("Web Push delivery failed ({status})");
                if status == 404 || status == 410 { Outcome::Gone(error) } else { Outcome::Failed(error) }
            }
            Err(error) => Outcome::Failed(format!("Web Push request failed: {error}")),
        }
    }
}

fn decode_b64(value: &str) -> Option<Vec<u8>> {
    let trimmed = value.trim_end_matches('=');
    URL_SAFE_NO_PAD.decode(trimmed).ok().or_else(|| STANDARD.decode(value).ok())
}

/// Encrypts a push message for a browser (RFC 8291, aes128gcm content coding).
pub fn encrypt_web_push(plaintext: &[u8], p256dh: &str, auth: &str) -> Result<Vec<u8>, String> {
    let mut salt = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt);
    let sender = SecretKey::random(&mut rand::rngs::OsRng);
    encrypt_web_push_with(plaintext, p256dh, auth, &sender, salt)
}

pub fn encrypt_web_push_with(plaintext: &[u8], p256dh: &str, auth: &str, sender: &SecretKey, salt: [u8; 16]) -> Result<Vec<u8>, String> {
    let receiver_bytes = decode_b64(p256dh).ok_or("Browser key is invalid")?;
    let auth_secret = decode_b64(auth).ok_or("Browser auth secret is invalid")?;
    let receiver = PublicKey::from_sec1_bytes(&receiver_bytes).map_err(|_| "Browser key is invalid")?;
    let sender_public = sender.public_key().to_encoded_point(false);
    let shared = p256::ecdh::diffie_hellman(sender.to_nonzero_scalar(), receiver.as_affine());

    let mut key_info = b"WebPush: info\0".to_vec();
    key_info.extend_from_slice(&receiver_bytes);
    key_info.extend_from_slice(sender_public.as_bytes());
    let mut ikm = [0u8; 32];
    Hkdf::<Sha256>::new(Some(&auth_secret), shared.raw_secret_bytes())
        .expand(&key_info, &mut ikm)
        .map_err(|_| "key derivation failed")?;
    let prk = Hkdf::<Sha256>::new(Some(&salt), &ikm);
    let mut cek = [0u8; 16];
    let mut nonce = [0u8; 12];
    prk.expand(b"Content-Encoding: aes128gcm\0", &mut cek).map_err(|_| "key derivation failed")?;
    prk.expand(b"Content-Encoding: nonce\0", &mut nonce).map_err(|_| "key derivation failed")?;

    let mut record = plaintext.to_vec();
    record.push(2); // the last (and only) record
    let ciphertext = Aes128Gcm::new_from_slice(&cek)
        .map_err(|_| "encryption failed")?
        .encrypt(&Nonce::from(nonce), record.as_slice())
        .map_err(|_| "encryption failed")?;

    let mut body = salt.to_vec();
    body.extend_from_slice(&4096u32.to_be_bytes());
    body.push(sender_public.as_bytes().len() as u8);
    body.extend_from_slice(sender_public.as_bytes());
    body.extend_from_slice(&ciphertext);
    Ok(body)
}

pub struct Pusher {
    http: reqwest::Client,
    pub apns: Option<Apns>,
    pub fcm: Option<Fcm>,
    pub web: WebPush,
}

impl Pusher {
    pub fn new(settings: &Settings, db: &Db) -> Self {
        let apns = Apns::new(settings);
        if apns.is_none() {
            tracing::warn!("APNs is not configured. iOS push delivery is disabled.");
        }
        let fcm = Fcm::new(settings);
        if fcm.is_none() {
            tracing::warn!("FCM is not configured. Android push delivery is disabled.");
        }
        let subject = format!("mailto:{}", settings.smtp_from_email.clone().unwrap_or_else(|| "no-reply@mad-gamer.com".into()));
        Self {
            http: reqwest::Client::builder().timeout(Duration::from_secs(15)).build().expect("HTTP client"),
            apns,
            fcm,
            web: WebPush::load(db, subject),
        }
    }

    /// A visible notification to one device, with the payloads each platform expects.
    pub async fn send_notification(
        &self,
        device: &Device,
        notification_id: &str,
        title: &str,
        message: &str,
        sound: &str,
        email: bool,
        badge: Option<i64>,
    ) -> Outcome {
        let body = if email { EMAIL_PUSH_PREVIEW_BODY } else { message };
        // notification_id lets an open app tell this push apart from the same
        // notification arriving over its event stream.
        let info = json!({
            "device_id": device.device_id,
            "account_id": device.account_id,
            "notification_id": notification_id,
            "sent_at": iso_now(),
            "email": email,
        });
        if device.is_android() {
            let Some(fcm) = &self.fcm else { return Outcome::Failed("FCM is not configured".into()) };
            let Some(token) = &device.fcm_token else { return Outcome::Gone("Device has no FCM token".into()) };
            let mut android_notification = json!({"channel_id": android_channel_id_for_sound(sound)});
            if sound != "__none__" {
                android_notification["sound"] = json!("default");
            }
            fcm.send(
                &self.http,
                json!({
                    "token": token,
                    "notification": {"title": title, "body": body},
                    "data": {"title": title, "message": body, "mg_notify": info.to_string()},
                    "android": {"priority": "HIGH", "notification": android_notification},
                }),
            )
            .await
        } else if device.is_web() {
            self.web.send(&self.http, device, &json!({"title": title, "body": body, "mg_notify": info})).await
        } else {
            let Some(apns) = &self.apns else { return Outcome::Failed("APNs is not configured".into()) };
            let mut aps = json!({"alert": {"title": title, "body": body}});
            if let Some(badge) = badge {
                aps["badge"] = json!(badge);
            }
            if sound != "__none__" {
                aps["sound"] = json!(sound);
            }
            apns.send(&self.http, device, json!({"aps": aps, "mg_notify": info}), "alert", "10").await
        }
    }

    /// Sets an iOS device's app badge without showing anything.
    pub async fn send_badge(&self, device: &Device, badge: i64) -> Outcome {
        if device.is_android() || device.is_web() {
            return Outcome::Sent { apns_id: None, fcm_name: None };
        }
        let Some(apns) = &self.apns else { return Outcome::Failed("APNs is not configured".into()) };
        apns.send(&self.http, device, json!({"aps": {"badge": badge}}), "alert", "5").await
    }

    /// A silent event (signed_out, devices_updated, ...) the apps act on.
    /// Browsers aren't sent these: they must show something for every push.
    pub async fn send_event(&self, device: &Device, event: &str) -> Outcome {
        let info = json!({"event": event, "device_id": device.device_id, "account_id": device.account_id, "sent_at": iso_now()});
        if device.is_android() {
            let (Some(fcm), Some(token)) = (&self.fcm, &device.fcm_token) else { return Outcome::Failed("FCM is not configured".into()) };
            fcm.send(&self.http, json!({"token": token, "data": {"mg_notify": info.to_string()}, "android": {"priority": "HIGH"}})).await
        } else if device.is_web() {
            Outcome::Sent { apns_id: None, fcm_name: None }
        } else {
            let Some(apns) = &self.apns else { return Outcome::Failed("APNs is not configured".into()) };
            apns.send(&self.http, device, json!({"aps": {"content-available": 1}, "mg_notify": info}), "background", "5").await
        }
    }
}

/// Stores a notification and pushes it to all of the account's devices at once.
pub async fn deliver_to_account(
    db: &Arc<Db>,
    pusher: &Arc<Pusher>,
    hub: &Hub,
    account_id: &str,
    title: &str,
    message: &str,
    sound: &str,
    email: bool,
    html: Option<String>,
) -> Result<Value, rusqlite::Error> {
    let devices = db.devices_for_account(account_id)?;
    let notification = Notification {
        notification_id: uuid::Uuid::new_v4().to_string(),
        account_id: account_id.into(),
        title: title.into(),
        message: message.into(),
        created_at: iso_now(),
        status: "stored".into(),
        apns_id: None,
        email,
        html,
        seen: false,
    };
    db.add_notification(&notification)?;
    hub.account(account_id, "notification", json!({"notification": notification.json()}));
    if devices.is_empty() {
        return Ok(json!({"delivered": 0, "devices": [], "stored": true}));
    }
    let notification_id = notification.notification_id.as_str();
    let sends = devices.iter().map(|device| {
        let device_sound = if device.notification_sound.is_empty() { sound.to_string() } else { device.notification_sound.clone() };
        // iOS shows the device's unread total (all its accounts) on the app icon.
        let badge = if device.badges && !device.is_android() && !device.is_web() { db.device_badge(&device.device_id).ok() } else { None };
        async move {
            let outcome = pusher.send_notification(device, notification_id, title, message, &device_sound, email, badge).await;
            (device, device_sound, outcome)
        }
    });
    let mut results = Vec::new();
    let mut apns_ids = Vec::new();
    let mut delivered = 0;
    for (device, device_sound, outcome) in join_all(sends).await {
        let mut entry = json!({
            "device_id": device.device_id,
            "device_name": device.device_name,
            "sound": device_sound,
            "apns_id": null,
            "fcm_name": null,
            "status": "sent",
        });
        match outcome {
            Outcome::Sent { apns_id, fcm_name } => {
                delivered += 1;
                if let Some(id) = &apns_id {
                    apns_ids.push(id.clone());
                }
                entry["apns_id"] = json!(apns_id);
                entry["fcm_name"] = json!(fcm_name);
            }
            Outcome::Gone(error) => {
                tracing::info!("Removing device {} ({}): {error}", device.device_id, device.platform);
                let _ = db.delete_device(account_id, &device.device_id);
                entry["status"] = json!("removed");
                entry["error"] = json!(error);
            }
            Outcome::Failed(error) => {
                tracing::warn!("Delivery to device {} failed: {error}", device.device_id);
                entry["status"] = json!("failed");
                entry["error"] = json!(error);
            }
        }
        results.push(entry);
    }
    let apns_id = if apns_ids.len() == 1 { Some(apns_ids[0].as_str()) } else { None };
    db.set_notification_delivery(&notification.notification_id, if delivered > 0 { "sent" } else { "failed" }, apns_id)?;
    Ok(json!({"delivered": delivered, "devices": results}))
}

/// Sends an event to one device in the background; failures are only logged.
pub fn spawn_event(db: Arc<Db>, pusher: Arc<Pusher>, device: Device, event: &'static str) {
    tokio::spawn(async move {
        match pusher.send_event(&device, event).await {
            Outcome::Sent { .. } => {}
            Outcome::Gone(error) => {
                tracing::info!("Removing device {}: {error}", device.device_id);
                let _ = db.delete_device(&device.account_id, &device.device_id);
            }
            Outcome::Failed(error) => tracing::warn!("Failed to send {event} to device {}: {error}", device.device_id),
        }
    });
}

/// The account's unread count changed: give each of its iPhones and iPads its
/// new badge (a device's badge covers every account signed in on it).
pub fn spawn_badge_updates(db: Arc<Db>, pusher: Arc<Pusher>, account_id: &str) {
    let Ok(devices) = db.devices_for_account(account_id) else { return };
    for device in devices.into_iter().filter(|d| d.badges && !d.is_android() && !d.is_web()) {
        let Ok(badge) = db.device_badge(&device.device_id) else { continue };
        let (db, pusher) = (db.clone(), pusher.clone());
        tokio::spawn(async move {
            match pusher.send_badge(&device, badge).await {
                Outcome::Sent { .. } => {}
                Outcome::Gone(error) => {
                    tracing::info!("Removing device {}: {error}", device.device_id);
                    let _ = db.delete_device(&device.account_id, &device.device_id);
                }
                Outcome::Failed(error) => tracing::warn!("Badge update for device {} failed: {error}", device.device_id),
            }
        });
    }
}

/// Sends an event to every device of an account in the background.
pub fn spawn_event_to_account(db: Arc<Db>, pusher: Arc<Pusher>, account_id: &str, event: &'static str) {
    if let Ok(devices) = db.devices_for_account(account_id) {
        for device in devices {
            spawn_event(db.clone(), pusher.clone(), device, event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decrypts as a browser would (RFC 8291), to check encrypt_web_push.
    fn browser_decrypt(body: &[u8], receiver: &SecretKey, auth: &[u8]) -> Vec<u8> {
        let salt = &body[..16];
        let key_length = body[20] as usize;
        let sender = PublicKey::from_sec1_bytes(&body[21..21 + key_length]).unwrap();
        let ciphertext = &body[21 + key_length..];
        let shared = p256::ecdh::diffie_hellman(receiver.to_nonzero_scalar(), sender.as_affine());
        let mut info = b"WebPush: info\0".to_vec();
        info.extend_from_slice(receiver.public_key().to_encoded_point(false).as_bytes());
        info.extend_from_slice(sender.to_encoded_point(false).as_bytes());
        let mut ikm = [0u8; 32];
        Hkdf::<Sha256>::new(Some(auth), shared.raw_secret_bytes()).expand(&info, &mut ikm).unwrap();
        let prk = Hkdf::<Sha256>::new(Some(salt), &ikm);
        let (mut cek, mut nonce) = ([0u8; 16], [0u8; 12]);
        prk.expand(b"Content-Encoding: aes128gcm\0", &mut cek).unwrap();
        prk.expand(b"Content-Encoding: nonce\0", &mut nonce).unwrap();
        let mut plain = Aes128Gcm::new_from_slice(&cek).unwrap().decrypt(&Nonce::from(nonce), ciphertext).unwrap();
        assert_eq!(plain.pop(), Some(2), "last-record delimiter");
        plain
    }

    #[test]
    fn web_push_round_trip() {
        let receiver = SecretKey::random(&mut rand::rngs::OsRng);
        let auth = [7u8; 16];
        let p256dh = URL_SAFE_NO_PAD.encode(receiver.public_key().to_encoded_point(false).as_bytes());
        let message = br#"{"title":"Hello","body":"When I grow up, I want to be a watermelon"}"#;
        let body = encrypt_web_push(message, &p256dh, &URL_SAFE_NO_PAD.encode(auth)).unwrap();
        assert_eq!(&body[16..20], &4096u32.to_be_bytes());
        assert_eq!(browser_decrypt(&body, &receiver, &auth), message);
    }

    #[test]
    fn android_channels_match_the_app() {
        assert_eq!(android_channel_id_for_sound("default"), "mg_notify");
        assert_eq!(android_channel_id_for_sound("__none__"), "mg_notify_silent");
        // What the Android app created for Chime.wav on the emulator.
        assert_eq!(android_channel_id_for_sound("Chime.wav"), "mg_notify_sound_93aa41a7cdfd85d4");
    }
}
