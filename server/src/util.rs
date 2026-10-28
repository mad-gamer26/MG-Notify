//! Small helpers shared across the server.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::FormatItem;
use time::macros::format_description;

const ISO: &[FormatItem<'static>] = format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z");

/// Timestamps as the Python server wrote them: `2026-10-27T15:29:00Z`.
pub fn iso_now() -> String {
    iso_after(0)
}

pub fn iso_after(seconds: i64) -> String {
    (OffsetDateTime::now_utc() + time::Duration::seconds(seconds)).format(ISO).unwrap_or_default()
}

pub fn epoch_from_iso(value: &str) -> Option<i64> {
    time::PrimitiveDateTime::parse(value, ISO).ok().map(|t| t.assume_utc().unix_timestamp())
}

pub fn now_epoch() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

/// `prefix_` and 24 random bytes in URL-safe base64, like Python's token_urlsafe(24).
pub fn make_secret(prefix: &str) -> String {
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("{prefix}_{}", URL_SAFE_NO_PAD.encode(bytes))
}

pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Hashes a new password with Argon2id. The salt is inside the hash string,
/// so the separate salt column is left empty.
pub fn hash_password(password: &str) -> (String, String) {
    let salt = SaltString::generate(&mut rand::rngs::OsRng);
    let hash = Argon2::default().hash_password(password.as_bytes(), &salt).expect("argon2 hashing").to_string();
    (hash, String::new())
}

pub enum PasswordCheck {
    Wrong,
    Right,
    /// Right, but stored with the old PBKDF2 scheme: rehash it.
    RightButOld,
}

pub fn check_password(password: &str, stored_hash: &str, stored_salt: &str) -> PasswordCheck {
    if stored_hash.starts_with("$argon2") {
        let ok = PasswordHash::new(stored_hash)
            .map(|parsed| Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
            .unwrap_or(false);
        return if ok { PasswordCheck::Right } else { PasswordCheck::Wrong };
    }
    // The Python server's scheme: PBKDF2-HMAC-SHA256, 200,000 rounds, hex salt and hash.
    let Ok(salt) = hex::decode(stored_salt) else { return PasswordCheck::Wrong };
    let mut derived = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, 200_000, &mut derived);
    let expected = hex::decode(stored_hash).unwrap_or_default();
    if constant_time_eq(&derived, &expected) { PasswordCheck::RightButOld } else { PasswordCheck::Wrong }
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Python's html.escape(): quotes included.
pub fn html_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

/// The first `max` characters of `text`.
pub fn truncate_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

pub fn normalize_email(value: &str) -> Result<String, String> {
    let email = value.trim().to_lowercase();
    if !email.contains('@') || email.starts_with('@') || email.ends_with('@') {
        return Err("Enter a valid email address.".into());
    }
    Ok(email)
}

pub fn normalize_sound(value: &str) -> String {
    let sound = value.trim();
    if sound.is_empty() { "default".into() } else { sound.into() }
}
