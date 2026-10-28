//! The SQLite database: one table per JSON file the Python server kept.
//! Session, password-reset and verification tokens are stored as SHA-256
//! hashes; API keys stay readable because the apps and website show them.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use crate::util::{hash_token, iso_after, iso_now, make_secret};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS accounts (
    account_id TEXT PRIMARY KEY,
    email TEXT NOT NULL UNIQUE,
    email_verified INTEGER NOT NULL,
    verified_at TEXT,
    password_hash TEXT NOT NULL,
    password_salt TEXT NOT NULL,
    created_at TEXT NOT NULL,
    display_name TEXT
);
CREATE TABLE IF NOT EXISTS sessions (
    token_hash TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts(account_id) ON DELETE CASCADE,
    device_id TEXT,
    created_at TEXT NOT NULL,
    last_used_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS sessions_account ON sessions(account_id, device_id);
CREATE TABLE IF NOT EXISTS devices (
    account_id TEXT NOT NULL REFERENCES accounts(account_id) ON DELETE CASCADE,
    device_id TEXT NOT NULL,
    device_name TEXT NOT NULL,
    platform TEXT NOT NULL,
    apns_token TEXT NOT NULL DEFAULT '',
    apns_environment TEXT NOT NULL DEFAULT '',
    fcm_token TEXT,
    web_endpoint TEXT,
    web_p256dh TEXT,
    web_auth TEXT,
    notification_sound TEXT NOT NULL DEFAULT 'default',
    last_ip_address TEXT NOT NULL DEFAULT '',
    last_seen TEXT NOT NULL DEFAULT '',
    badges INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (account_id, device_id)
);
CREATE TABLE IF NOT EXISTS api_keys (
    key_id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts(account_id) ON DELETE CASCADE,
    label TEXT NOT NULL,
    api_key TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_used_at TEXT
);
CREATE INDEX IF NOT EXISTS api_keys_key ON api_keys(api_key);
CREATE INDEX IF NOT EXISTS api_keys_key_folded ON api_keys(lower(api_key));
CREATE INDEX IF NOT EXISTS api_keys_account ON api_keys(account_id);
CREATE TABLE IF NOT EXISTS password_resets (
    token_hash TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts(account_id) ON DELETE CASCADE,
    email TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    used_at TEXT
);
CREATE TABLE IF NOT EXISTS email_verifications (
    token_hash TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts(account_id) ON DELETE CASCADE,
    email TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    used_at TEXT
);
CREATE TABLE IF NOT EXISTS notifications (
    notification_id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts(account_id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    message TEXT NOT NULL,
    created_at TEXT NOT NULL,
    status TEXT NOT NULL,
    apns_id TEXT,
    email INTEGER NOT NULL DEFAULT 0,
    html TEXT,
    seen INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX IF NOT EXISTS notifications_account ON notifications(account_id, created_at);
CREATE TABLE IF NOT EXISTS inbound_email_messages (
    message_id TEXT PRIMARY KEY,
    processed_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

#[derive(Clone, Debug)]
pub struct Account {
    pub account_id: String,
    pub email: String,
    pub display_name: Option<String>,
    pub email_verified: bool,
    pub password_hash: String,
    pub password_salt: String,
}

impl Account {
    pub fn json(&self) -> Value {
        json!({
            "account_id": self.account_id,
            "email": self.email,
            "email_verified": self.email_verified,
            "display_name": self.display_name,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Session {
    pub token_hash: String,
    pub account_id: String,
    pub device_id: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Device {
    pub device_id: String,
    pub account_id: String,
    pub device_name: String,
    pub platform: String,
    pub apns_token: String,
    pub apns_environment: String,
    pub fcm_token: Option<String>,
    pub web_endpoint: Option<String>,
    pub web_p256dh: Option<String>,
    pub web_auth: Option<String>,
    pub notification_sound: String,
    pub last_ip_address: String,
    pub last_seen: String,
    /// The app keeps its badge up to date (marks notifications seen), so it gets badge numbers.
    pub badges: bool,
}

impl Device {
    pub fn is_android(&self) -> bool {
        self.platform.eq_ignore_ascii_case("android")
    }

    pub fn is_web(&self) -> bool {
        self.platform.eq_ignore_ascii_case("web")
    }

    /// The device as the API shows it: each platform with its own fields, as before.
    pub fn json(&self) -> Value {
        let mut item = json!({
            "device_id": self.device_id,
            "account_id": self.account_id,
            "device_name": self.device_name,
            "platform": self.platform,
            "last_ip_address": self.last_ip_address,
            "last_seen": self.last_seen,
        });
        let map = item.as_object_mut().unwrap();
        if self.is_android() {
            map.insert("fcm_token".into(), json!(self.fcm_token));
            map.insert("notification_sound".into(), json!(self.notification_sound));
        } else if self.is_web() {
            map.insert("notification_sound".into(), json!(self.notification_sound));
        } else {
            map.insert("apns_token".into(), json!(self.apns_token));
            map.insert("apns_environment".into(), json!(self.apns_environment));
            map.insert("notification_sound".into(), json!(self.notification_sound));
        }
        item
    }

    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            account_id: row.get("account_id")?,
            device_id: row.get("device_id")?,
            device_name: row.get("device_name")?,
            platform: row.get("platform")?,
            apns_token: row.get("apns_token")?,
            apns_environment: row.get("apns_environment")?,
            fcm_token: row.get("fcm_token")?,
            web_endpoint: row.get("web_endpoint")?,
            web_p256dh: row.get("web_p256dh")?,
            web_auth: row.get("web_auth")?,
            notification_sound: row.get("notification_sound")?,
            last_ip_address: row.get("last_ip_address")?,
            last_seen: row.get("last_seen")?,
            badges: row.get("badges")?,
        })
    }
}

#[derive(Clone, Debug)]
pub struct ApiKey {
    pub key_id: String,
    pub account_id: String,
    pub label: String,
    pub api_key: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

impl ApiKey {
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            key_id: row.get("key_id")?,
            account_id: row.get("account_id")?,
            label: row.get("label")?,
            api_key: row.get("api_key")?,
            created_at: row.get("created_at")?,
            last_used_at: row.get("last_used_at")?,
        })
    }
}

/// A password reset or email verification.
#[derive(Clone, Debug)]
pub struct OneTimeToken {
    pub account_id: String,
    pub email: String,
    pub expires_at: String,
    pub used_at: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Notification {
    pub notification_id: String,
    pub account_id: String,
    pub title: String,
    pub message: String,
    pub created_at: String,
    pub status: String,
    pub apns_id: Option<String>,
    pub email: bool,
    pub html: Option<String>,
    /// False until the notification list has been opened on any device.
    pub seen: bool,
}

impl Notification {
    pub fn json(&self) -> Value {
        json!({
            "notification_id": self.notification_id,
            "account_id": self.account_id,
            "title": self.title,
            "message": self.message,
            "created_at": self.created_at,
            "status": self.status,
            "apns_id": self.apns_id,
            "email": self.email,
            "html": self.html,
            "seen": self.seen,
        })
    }
}

pub struct Db {
    conn: Mutex<Connection>,
}

pub type DbResult<T> = rusqlite::Result<T>;

impl Db {
    pub fn open(path: &Path) -> DbResult<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.execute_batch(SCHEMA)?;
        // Databases made before display names existed get the column.
        let has_display_name: bool =
            conn.query_row("SELECT COUNT(*) FROM pragma_table_info('accounts') WHERE name = 'display_name'", [], |r| r.get::<_, i64>(0))? > 0;
        if !has_display_name {
            conn.execute_batch("ALTER TABLE accounts ADD COLUMN display_name TEXT")?;
        }
        // Unread tracking: notifications from before it count as seen.
        let has_seen: bool =
            conn.query_row("SELECT COUNT(*) FROM pragma_table_info('notifications') WHERE name = 'seen'", [], |r| r.get::<_, i64>(0))? > 0;
        if !has_seen {
            conn.execute_batch("ALTER TABLE notifications ADD COLUMN seen INTEGER NOT NULL DEFAULT 1")?;
        }
        // Badges only for apps that clear them (2.0 and later say so when registering).
        let has_badges: bool =
            conn.query_row("SELECT COUNT(*) FROM pragma_table_info('devices') WHERE name = 'badges'", [], |r| r.get::<_, i64>(0))? > 0;
        if !has_badges {
            conn.execute_batch("ALTER TABLE devices ADD COLUMN badges INTEGER NOT NULL DEFAULT 0")?;
        }
        conn.execute_batch("CREATE INDEX IF NOT EXISTS notifications_unseen ON notifications(account_id) WHERE seen = 0")?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // Accounts

    pub fn create_account(&self, email: &str, password_hash: &str, password_salt: &str) -> DbResult<Option<Account>> {
        let account_id = uuid::Uuid::new_v4().to_string();
        let changed = self.conn().execute(
            "INSERT OR IGNORE INTO accounts (account_id, email, email_verified, verified_at, password_hash, password_salt, created_at)
             VALUES (?1, ?2, 0, NULL, ?3, ?4, ?5)",
            params![account_id, email, password_hash, password_salt, iso_now()],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        self.account_by_id(&account_id)
    }

    fn account_query(&self, column: &str, value: &str) -> DbResult<Option<Account>> {
        self.conn()
            .query_row(
                &format!("SELECT account_id, email, email_verified, password_hash, password_salt, display_name FROM accounts WHERE {column} = ?1"),
                [value],
                |row| {
                    Ok(Account {
                        account_id: row.get(0)?,
                        email: row.get(1)?,
                        email_verified: row.get(2)?,
                        password_hash: row.get(3)?,
                        password_salt: row.get(4)?,
                        display_name: row.get(5)?,
                    })
                },
            )
            .optional()
    }

    pub fn account_by_email(&self, email: &str) -> DbResult<Option<Account>> {
        self.account_query("email", email)
    }

    pub fn account_by_id(&self, account_id: &str) -> DbResult<Option<Account>> {
        self.account_query("account_id", account_id)
    }

    pub fn set_password(&self, account_id: &str, hash: &str, salt: &str) -> DbResult<()> {
        self.conn().execute(
            "UPDATE accounts SET password_hash = ?2, password_salt = ?3 WHERE account_id = ?1",
            params![account_id, hash, salt],
        )?;
        Ok(())
    }

    /// Changes the email and marks it unverified. None when another account has it.
    pub fn change_email(&self, account_id: &str, email: &str) -> DbResult<Option<Account>> {
        {
            let conn = self.conn();
            let taken: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM accounts WHERE email = ?1 AND account_id != ?2)",
                params![email, account_id],
                |row| row.get(0),
            )?;
            if taken {
                return Ok(None);
            }
            conn.execute(
                "UPDATE accounts SET email = ?2, email_verified = 0, verified_at = NULL WHERE account_id = ?1",
                params![account_id, email],
            )?;
        }
        self.account_by_id(account_id)
    }

    pub fn mark_email_verified(&self, account_id: &str) -> DbResult<()> {
        self.conn().execute(
            "UPDATE accounts SET email_verified = 1, verified_at = ?2 WHERE account_id = ?1",
            params![account_id, iso_now()],
        )?;
        Ok(())
    }

    pub fn set_display_name(&self, account_id: &str, display_name: Option<&str>) -> DbResult<()> {
        self.conn()
            .execute("UPDATE accounts SET display_name = ?2 WHERE account_id = ?1", params![account_id, display_name])?;
        Ok(())
    }

    pub fn delete_account(&self, account_id: &str) -> DbResult<()> {
        self.conn().execute("DELETE FROM accounts WHERE account_id = ?1", [account_id])?;
        Ok(())
    }

    // Sessions

    /// Creates a session and returns its token (only its hash is stored).
    pub fn create_session(&self, account_id: &str) -> DbResult<String> {
        let token = make_secret("mgsess");
        let now = iso_now();
        self.conn().execute(
            "INSERT INTO sessions (token_hash, account_id, device_id, created_at, last_used_at) VALUES (?1, ?2, NULL, ?3, ?3)",
            params![hash_token(&token), account_id, now],
        )?;
        Ok(token)
    }

    /// Looks a session up by its token and records that it was used.
    pub fn use_session(&self, token: &str) -> DbResult<Option<Session>> {
        let token_hash = hash_token(token);
        let conn = self.conn();
        let session = conn
            .query_row(
                "SELECT token_hash, account_id, device_id FROM sessions WHERE token_hash = ?1",
                [&token_hash],
                |row| Ok(Session { token_hash: row.get(0)?, account_id: row.get(1)?, device_id: row.get(2)? }),
            )
            .optional()?;
        if session.is_some() {
            conn.execute("UPDATE sessions SET last_used_at = ?2 WHERE token_hash = ?1", params![token_hash, iso_now()])?;
        }
        Ok(session)
    }

    pub fn delete_session(&self, token_hash: &str) -> DbResult<()> {
        self.conn().execute("DELETE FROM sessions WHERE token_hash = ?1", [token_hash])?;
        Ok(())
    }

    pub fn delete_sessions_for_account(&self, account_id: &str) -> DbResult<()> {
        self.conn().execute("DELETE FROM sessions WHERE account_id = ?1", [account_id])?;
        Ok(())
    }

    pub fn bind_session_to_device(&self, token_hash: &str, device_id: &str) -> DbResult<()> {
        self.conn().execute(
            "UPDATE sessions SET device_id = ?2, last_used_at = ?3 WHERE token_hash = ?1",
            params![token_hash, device_id, iso_now()],
        )?;
        Ok(())
    }

    /// The sessions bound to a device, so their open event streams can be told they're signed out.
    pub fn session_hashes_for_device(&self, account_id: &str, device_id: &str) -> DbResult<Vec<String>> {
        let conn = self.conn();
        let mut statement = conn.prepare("SELECT token_hash FROM sessions WHERE account_id = ?1 AND device_id = ?2")?;
        statement.query_map(params![account_id, device_id], |row| row.get(0))?.collect()
    }

    pub fn delete_sessions_for_device(&self, account_id: &str, device_id: &str) -> DbResult<usize> {
        self.conn()
            .execute("DELETE FROM sessions WHERE account_id = ?1 AND device_id = ?2", params![account_id, device_id])
    }

    /// Removes sessions nobody has used since `cutoff`.
    pub fn delete_sessions_unused_since(&self, cutoff: &str) -> DbResult<usize> {
        self.conn().execute("DELETE FROM sessions WHERE last_used_at < ?1", [cutoff])
    }

    // Devices

    pub fn upsert_device(&self, d: &Device) -> DbResult<()> {
        self.conn().execute(
            "INSERT INTO devices (account_id, device_id, device_name, platform, apns_token, apns_environment, fcm_token,
                                  web_endpoint, web_p256dh, web_auth, notification_sound, last_ip_address, last_seen, badges)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT (account_id, device_id) DO UPDATE SET
                device_name = excluded.device_name, platform = excluded.platform, apns_token = excluded.apns_token,
                apns_environment = excluded.apns_environment, fcm_token = excluded.fcm_token,
                web_endpoint = excluded.web_endpoint, web_p256dh = excluded.web_p256dh, web_auth = excluded.web_auth,
                notification_sound = excluded.notification_sound, last_ip_address = excluded.last_ip_address,
                last_seen = excluded.last_seen, badges = excluded.badges",
            params![
                d.account_id, d.device_id, d.device_name, d.platform, d.apns_token, d.apns_environment, d.fcm_token,
                d.web_endpoint, d.web_p256dh, d.web_auth, d.notification_sound, d.last_ip_address, d.last_seen, d.badges
            ],
        )?;
        Ok(())
    }

    pub fn devices_for_account(&self, account_id: &str) -> DbResult<Vec<Device>> {
        let conn = self.conn();
        let mut statement = conn.prepare("SELECT * FROM devices WHERE account_id = ?1 ORDER BY rowid")?;
        statement.query_map([account_id], Device::from_row)?.collect()
    }

    pub fn device(&self, account_id: &str, device_id: &str) -> DbResult<Option<Device>> {
        self.conn()
            .query_row(
                "SELECT * FROM devices WHERE account_id = ?1 AND device_id = ?2",
                params![account_id, device_id],
                Device::from_row,
            )
            .optional()
    }

    pub fn delete_device(&self, account_id: &str, device_id: &str) -> DbResult<bool> {
        Ok(self
            .conn()
            .execute("DELETE FROM devices WHERE account_id = ?1 AND device_id = ?2", params![account_id, device_id])?
            > 0)
    }

    pub fn delete_devices_for_account(&self, account_id: &str) -> DbResult<()> {
        self.conn().execute("DELETE FROM devices WHERE account_id = ?1", [account_id])?;
        Ok(())
    }

    pub fn set_device_sound(&self, account_id: &str, device_id: &str, sound: &str) -> DbResult<Option<Device>> {
        let changed = self.conn().execute(
            "UPDATE devices SET notification_sound = ?3, last_seen = ?4 WHERE account_id = ?1 AND device_id = ?2",
            params![account_id, device_id, sound, iso_now()],
        )?;
        if changed == 0 { Ok(None) } else { self.device(account_id, device_id) }
    }

    // API keys

    pub fn create_api_key(&self, account_id: &str, label: &str) -> DbResult<ApiKey> {
        let key = ApiKey {
            key_id: uuid::Uuid::new_v4().to_string(),
            account_id: account_id.into(),
            label: label.into(),
            api_key: make_secret("mgkey"),
            created_at: iso_now(),
            last_used_at: None,
        };
        self.conn().execute(
            "INSERT INTO api_keys (key_id, account_id, label, api_key, created_at, last_used_at) VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            params![key.key_id, key.account_id, key.label, key.api_key, key.created_at],
        )?;
        Ok(key)
    }

    pub fn api_keys_for_account(&self, account_id: &str) -> DbResult<Vec<ApiKey>> {
        let conn = self.conn();
        let mut statement = conn.prepare("SELECT * FROM api_keys WHERE account_id = ?1 ORDER BY rowid")?;
        statement.query_map([account_id], ApiKey::from_row)?.collect()
    }

    pub fn api_key(&self, api_key: &str) -> DbResult<Option<ApiKey>> {
        self.conn()
            .query_row("SELECT * FROM api_keys WHERE api_key = ?1", [api_key], ApiKey::from_row)
            .optional()
    }

    /// The key for an inbound email address: mail software may change the
    /// local part's case, so case is ignored, and an ambiguous match counts as none.
    pub fn api_key_ignoring_case(&self, api_key: &str) -> DbResult<Option<ApiKey>> {
        let conn = self.conn();
        let mut statement = conn.prepare("SELECT * FROM api_keys WHERE lower(api_key) = lower(?1) LIMIT 2")?;
        let mut matches: Vec<ApiKey> = statement.query_map([api_key], ApiKey::from_row)?.collect::<DbResult<_>>()?;
        Ok(if matches.len() == 1 { matches.pop() } else { None })
    }

    pub fn delete_api_key(&self, account_id: &str, key_id: &str) -> DbResult<bool> {
        Ok(self
            .conn()
            .execute("DELETE FROM api_keys WHERE account_id = ?1 AND key_id = ?2", params![account_id, key_id])?
            > 0)
    }

    pub fn delete_api_keys_for_account(&self, account_id: &str) -> DbResult<usize> {
        self.conn().execute("DELETE FROM api_keys WHERE account_id = ?1", [account_id])
    }

    pub fn touch_api_key(&self, key_id: &str) -> DbResult<()> {
        self.conn().execute("UPDATE api_keys SET last_used_at = ?2 WHERE key_id = ?1", params![key_id, iso_now()])?;
        Ok(())
    }

    // Password resets and email verifications

    fn create_one_time(&self, table: &str, prefix: &str, account_id: &str, email: &str, lifetime: i64) -> DbResult<String> {
        let token = make_secret(prefix);
        self.conn().execute(
            &format!("INSERT INTO {table} (token_hash, account_id, email, created_at, expires_at, used_at) VALUES (?1, ?2, ?3, ?4, ?5, NULL)"),
            params![hash_token(&token), account_id, email, iso_now(), iso_after(lifetime)],
        )?;
        Ok(token)
    }

    fn one_time(&self, table: &str, token: &str) -> DbResult<Option<OneTimeToken>> {
        self.conn()
            .query_row(
                &format!("SELECT account_id, email, expires_at, used_at FROM {table} WHERE token_hash = ?1"),
                [hash_token(token)],
                |row| Ok(OneTimeToken { account_id: row.get(0)?, email: row.get(1)?, expires_at: row.get(2)?, used_at: row.get(3)? }),
            )
            .optional()
    }

    fn use_one_time(&self, table: &str, token: &str) -> DbResult<()> {
        self.conn().execute(
            &format!("UPDATE {table} SET used_at = ?2 WHERE token_hash = ?1"),
            params![hash_token(token), iso_now()],
        )?;
        Ok(())
    }

    pub fn create_password_reset(&self, account_id: &str, email: &str) -> DbResult<String> {
        self.create_one_time("password_resets", "mgreset", account_id, email, 3600)
    }

    pub fn password_reset(&self, token: &str) -> DbResult<Option<OneTimeToken>> {
        self.one_time("password_resets", token)
    }

    pub fn use_password_reset(&self, token: &str) -> DbResult<()> {
        self.use_one_time("password_resets", token)
    }

    /// Replaces any earlier verification for the account with a new one.
    pub fn create_email_verification(&self, account_id: &str, email: &str) -> DbResult<String> {
        self.conn().execute("DELETE FROM email_verifications WHERE account_id = ?1", [account_id])?;
        self.create_one_time("email_verifications", "mgverify", account_id, email, 60 * 60 * 24)
    }

    pub fn email_verification(&self, token: &str) -> DbResult<Option<OneTimeToken>> {
        self.one_time("email_verifications", token)
    }

    pub fn use_email_verification(&self, token: &str) -> DbResult<()> {
        self.use_one_time("email_verifications", token)
    }

    // Notifications

    pub fn add_notification(&self, n: &Notification) -> DbResult<()> {
        self.conn().execute(
            "INSERT INTO notifications (notification_id, account_id, title, message, created_at, status, apns_id, email, html, seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![n.notification_id, n.account_id, n.title, n.message, n.created_at, n.status, n.apns_id, n.email, n.html, n.seen],
        )?;
        Ok(())
    }

    pub fn set_notification_delivery(&self, notification_id: &str, status: &str, apns_id: Option<&str>) -> DbResult<()> {
        self.conn().execute(
            "UPDATE notifications SET status = ?2, apns_id = ?3 WHERE notification_id = ?1",
            params![notification_id, status, apns_id],
        )?;
        Ok(())
    }

    /// Newest first; notifications from the same second stay in the order they arrived.
    pub fn notifications_for_account(&self, account_id: &str) -> DbResult<Vec<Notification>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT notification_id, account_id, title, message, created_at, status, apns_id, email, html, seen
             FROM notifications WHERE account_id = ?1 ORDER BY created_at DESC, rowid ASC",
        )?;
        statement
            .query_map([account_id], |row| {
                Ok(Notification {
                    notification_id: row.get(0)?,
                    account_id: row.get(1)?,
                    title: row.get(2)?,
                    message: row.get(3)?,
                    created_at: row.get(4)?,
                    status: row.get(5)?,
                    apns_id: row.get(6)?,
                    email: row.get(7)?,
                    html: row.get(8)?,
                    seen: row.get(9)?,
                })
            })?
            .collect()
    }

    /// Deletes one notification: None when there was none, else whether it was still unread.
    pub fn delete_notification(&self, account_id: &str, notification_id: &str) -> DbResult<Option<bool>> {
        let conn = self.conn();
        let seen: Option<bool> = conn
            .query_row(
                "SELECT seen FROM notifications WHERE account_id = ?1 AND notification_id = ?2",
                params![account_id, notification_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(seen) = seen else { return Ok(None) };
        conn.execute("DELETE FROM notifications WHERE account_id = ?1 AND notification_id = ?2", params![account_id, notification_id])?;
        Ok(Some(!seen))
    }

    pub fn unread_count(&self, account_id: &str) -> DbResult<i64> {
        self.conn().query_row("SELECT COUNT(*) FROM notifications WHERE account_id = ?1 AND seen = 0", [account_id], |r| r.get(0))
    }

    /// Marks the account's notifications seen (all, or just `ids`); returns the IDs that changed.
    pub fn mark_seen(&self, account_id: &str, ids: Option<&[String]>) -> DbResult<Vec<String>> {
        let conn = self.conn();
        let mut statement = conn.prepare("SELECT notification_id FROM notifications WHERE account_id = ?1 AND seen = 0")?;
        let unseen: Vec<String> = statement.query_map([account_id], |row| row.get(0))?.collect::<DbResult<_>>()?;
        let changed: Vec<String> = match ids {
            Some(ids) => unseen.into_iter().filter(|id| ids.contains(id)).collect(),
            None => unseen,
        };
        for id in &changed {
            conn.execute("UPDATE notifications SET seen = 1 WHERE notification_id = ?1", [id])?;
        }
        Ok(changed)
    }

    /// A device's badge: unread notifications across every account it's signed in to.
    pub fn device_badge(&self, device_id: &str) -> DbResult<i64> {
        self.conn().query_row(
            "SELECT COUNT(*) FROM notifications n
             WHERE n.seen = 0 AND n.account_id IN (SELECT account_id FROM devices WHERE device_id = ?1)",
            [device_id],
            |r| r.get(0),
        )
    }

    pub fn clear_notifications(&self, account_id: &str) -> DbResult<usize> {
        self.conn().execute("DELETE FROM notifications WHERE account_id = ?1", [account_id])
    }

    // Inbound email

    pub fn inbound_email_processed(&self, message_id: &str) -> DbResult<bool> {
        self.conn().query_row(
            "SELECT EXISTS(SELECT 1 FROM inbound_email_messages WHERE message_id = ?1)",
            [message_id],
            |row| row.get(0),
        )
    }

    /// Remembers a processed message, keeping the newest 5,000 as before.
    pub fn mark_inbound_email_processed(&self, message_id: &str) -> DbResult<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT OR IGNORE INTO inbound_email_messages (message_id, processed_at) VALUES (?1, ?2)",
            params![message_id, iso_now()],
        )?;
        conn.execute(
            "DELETE FROM inbound_email_messages WHERE rowid NOT IN (SELECT rowid FROM inbound_email_messages ORDER BY rowid DESC LIMIT 5000)",
            [],
        )?;
        Ok(())
    }

    // Settings

    pub fn setting(&self, key: &str) -> DbResult<Option<String>> {
        self.conn().query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| row.get(0)).optional()
    }

    pub fn set_setting(&self, key: &str, value: &str) -> DbResult<()> {
        self.conn().execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Writes a consistent copy of the whole database to `path`.
    pub fn snapshot(&self, path: &Path) -> DbResult<()> {
        self.conn().execute("VACUUM INTO ?1", [path.to_string_lossy()])?;
        Ok(())
    }
}
