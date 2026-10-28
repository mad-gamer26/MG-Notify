//! `import <folder>`: moves the Python server's JSON files into the database.
//! Applies the same clean-ups the Python server did when loading them.

use rusqlite::params;
use serde_json::Value;
use std::path::Path;

use crate::db::Db;
use crate::email::plain_text_to_html;
use crate::util::hash_token;

fn read_list(dir: &Path, file: &str, key: &str) -> Result<Vec<Value>, String> {
    let path = dir.join(file);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let json: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(json.get(key).and_then(Value::as_array).cloned().unwrap_or_default())
}

fn s(item: &Value, key: &str) -> Option<String> {
    item.get(key).and_then(|v| v.as_str()).map(String::from)
}

fn s_or(item: &Value, key: &str, default: &str) -> String {
    s(item, key).unwrap_or_else(|| default.to_string())
}

pub fn run(db: &Db, dir: &Path) -> Result<(), String> {
    let mut conn = db.conn();
    let existing: i64 = conn.query_row("SELECT COUNT(*) FROM accounts", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    if existing > 0 {
        return Err("the database already has accounts; import only into a new database".into());
    }
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let mut report = Vec::new();
    let e = |err: rusqlite::Error| err.to_string();

    let accounts = read_list(dir, "accounts.json", "accounts")?;
    let mut account_ids = std::collections::HashSet::new();
    for a in &accounts {
        // Accounts from before email verification existed count as verified.
        let verified = a.get("email_verified").and_then(Value::as_bool).unwrap_or(true);
        let verified_at = s(a, "verified_at").or_else(|| verified.then(|| s_or(a, "created_at", "")));
        tx.execute(
            "INSERT INTO accounts (account_id, email, email_verified, verified_at, password_hash, password_salt, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![s_or(a, "account_id", ""), s_or(a, "email", ""), verified, verified_at,
                    s_or(a, "password_hash", ""), s_or(a, "password_salt", ""), s_or(a, "created_at", "")],
        )
        .map_err(e)?;
        account_ids.insert(s_or(a, "account_id", ""));
    }
    report.push(format!("accounts: {}", accounts.len()));

    let known = |item: &Value| account_ids.contains(&s_or(item, "account_id", ""));
    let mut count = |name: &str, all: usize, kept: usize| {
        report.push(if all == kept { format!("{name}: {kept}") } else { format!("{name}: {kept} ({} skipped: no such account)", all - kept) })
    };

    let sessions = read_list(dir, "sessions.json", "sessions")?;
    let kept: Vec<_> = sessions.iter().filter(|x| known(x)).collect();
    for x in &kept {
        tx.execute(
            "INSERT OR IGNORE INTO sessions (token_hash, account_id, device_id, created_at, last_used_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![hash_token(&s_or(x, "session_token", "")), s_or(x, "account_id", ""), s(x, "device_id"),
                    s_or(x, "created_at", ""), s_or(x, "last_used_at", "")],
        )
        .map_err(e)?;
    }
    count("sessions", sessions.len(), kept.len());

    let devices = read_list(dir, "devices.json", "devices")?;
    let kept: Vec<_> = devices.iter().filter(|x| known(x)).collect();
    for x in &kept {
        let platform = s_or(x, "platform", "ios");
        let android = platform.eq_ignore_ascii_case("android");
        tx.execute(
            "INSERT OR REPLACE INTO devices (account_id, device_id, device_name, platform, apns_token, apns_environment, fcm_token,
                                             notification_sound, last_ip_address, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![s_or(x, "account_id", ""), s_or(x, "device_id", ""), s_or(x, "device_name", ""), platform,
                    if android { String::new() } else { s_or(x, "apns_token", "") },
                    if android { String::new() } else { s_or(x, "apns_environment", "sandbox") },
                    s(x, "fcm_token"), s_or(x, "notification_sound", "default"),
                    s_or(x, "last_ip_address", ""), s_or(x, "last_seen", "")],
        )
        .map_err(e)?;
    }
    count("devices", devices.len(), kept.len());

    let keys = read_list(dir, "api_keys.json", "api_keys")?;
    let kept: Vec<_> = keys.iter().filter(|x| known(x)).collect();
    for x in &kept {
        tx.execute(
            "INSERT INTO api_keys (key_id, account_id, label, api_key, created_at, last_used_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![s_or(x, "key_id", ""), s_or(x, "account_id", ""), s_or(x, "label", ""), s_or(x, "api_key", ""),
                    s_or(x, "created_at", ""), s(x, "last_used_at")],
        )
        .map_err(e)?;
    }
    count("api keys", keys.len(), kept.len());

    for (file, key, table, token_field) in [
        ("password_resets.json", "password_resets", "password_resets", "reset_token"),
        ("email_verifications.json", "email_verifications", "email_verifications", "verification_token"),
    ] {
        let items = read_list(dir, file, key)?;
        let kept: Vec<_> = items.iter().filter(|x| known(x)).collect();
        for x in &kept {
            tx.execute(
                &format!("INSERT OR IGNORE INTO {table} (token_hash, account_id, email, created_at, expires_at, used_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)"),
                params![hash_token(&s_or(x, token_field, "")), s_or(x, "account_id", ""), s_or(x, "email", ""),
                        s_or(x, "created_at", ""), s_or(x, "expires_at", ""), s(x, "used_at")],
            )
            .map_err(e)?;
        }
        count(key, items.len(), kept.len());
    }

    let notifications = read_list(dir, "notifications.json", "notifications")?;
    let kept: Vec<_> = notifications.iter().filter(|x| known(x)).collect();
    for x in &kept {
        let mut title = s_or(x, "title", "");
        // Old email notifications were only marked by their title.
        let email = x.get("email").and_then(Value::as_bool).unwrap_or_else(|| title.starts_with("Email from "));
        if email {
            if let Some(rest) = title.strip_prefix("Email from ") {
                title = rest.to_string();
            }
        }
        let message = s_or(x, "message", "");
        let html = s(x, "html").or_else(|| email.then(|| plain_text_to_html(&message)));
        tx.execute(
            "INSERT OR IGNORE INTO notifications (notification_id, account_id, title, message, created_at, status, apns_id, email, html)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![s_or(x, "notification_id", ""), s_or(x, "account_id", ""), title, message, s_or(x, "created_at", ""),
                    s_or(x, "status", "stored"), s(x, "apns_id"), email, html],
        )
        .map_err(e)?;
    }
    count("notifications", notifications.len(), kept.len());

    let path = dir.join("inbound_email_messages.json");
    let ids: Vec<Value> = if path.exists() {
        let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        json.get("message_ids").and_then(Value::as_array).cloned().unwrap_or_default()
    } else {
        Vec::new()
    };
    for id in ids.iter().filter_map(Value::as_str) {
        tx.execute(
            "INSERT OR IGNORE INTO inbound_email_messages (message_id, processed_at) VALUES (?1, ?2)",
            params![id, crate::util::iso_now()],
        )
        .map_err(e)?;
    }
    report.push(format!("inbound email ids: {}", ids.len()));

    tx.commit().map_err(|e| e.to_string())?;
    for line in report {
        println!("{line}");
    }
    Ok(())
}
