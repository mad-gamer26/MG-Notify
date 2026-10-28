//! config.json: the same file and keys the Python server used, so an
//! existing ~/notify/config.json works unchanged.

use serde_json::{Map, Value, json};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// The file's values as text, before checking: what --configure edits.
#[derive(Clone, Debug)]
pub struct ConfigValues {
    pub data_dir: String,
    pub bind_host: String,
    pub bind_port: String,
    pub log_level: String,
    pub apns_team_id: String,
    pub apns_key_id: String,
    pub apns_bundle_id: String,
    pub apns_auth_key_path: String,
    pub fcm_project_id: String,
    pub fcm_service_account_path: String,
    pub smtp_host: String,
    pub smtp_port: String,
    pub smtp_username: String,
    pub smtp_password: String,
    pub smtp_from_email: String,
    pub smtp_from_name: String,
    pub smtp_use_tls: String,
    pub web_base_url: String,
}

impl Default for ConfigValues {
    fn default() -> Self {
        Self {
            data_dir: "./data".into(),
            bind_host: "127.0.0.1".into(),
            bind_port: "8080".into(),
            log_level: "INFO".into(),
            apns_team_id: String::new(),
            apns_key_id: String::new(),
            apns_bundle_id: String::new(),
            apns_auth_key_path: String::new(),
            fcm_project_id: String::new(),
            fcm_service_account_path: String::new(),
            smtp_host: String::new(),
            smtp_port: "587".into(),
            smtp_username: String::new(),
            smtp_password: String::new(),
            smtp_from_email: String::new(),
            smtp_from_name: String::new(),
            smtp_use_tls: "true".into(),
            web_base_url: "http://127.0.0.1/web/index.php".into(),
        }
    }
}

/// Checked settings, with paths resolved against the config file's folder.
#[derive(Clone, Debug)]
pub struct Settings {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub bind_host: String,
    pub bind_port: u16,
    pub log_level: String,
    pub apns_team_id: Option<String>,
    pub apns_key_id: Option<String>,
    pub apns_bundle_id: Option<String>,
    pub apns_auth_key_path: Option<PathBuf>,
    pub fcm_project_id: Option<String>,
    pub fcm_service_account_path: Option<PathBuf>,
    pub smtp_host: Option<String>,
    pub smtp_port: u16,
    pub smtp_username: Option<String>,
    pub smtp_password: Option<String>,
    pub smtp_from_email: Option<String>,
    pub smtp_from_name: Option<String>,
    pub smtp_use_tls: bool,
    pub web_base_url: Option<String>,
}

impl Settings {
    /// The secret Brevo sends with inbound email, kept next to config.json.
    pub fn brevo_webhook_secret(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.config_dir.join("brevo-webhook-secret")).ok()?;
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_string())
    }
}

fn get_str(section: Option<&Value>, key: &str, default: &str) -> String {
    match section.and_then(|s| s.get(key)) {
        Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
        _ => default.to_string(),
    }
}

/// Like get_str, but numbers and booleans count too (the Python server read
/// smtp.port and smtp.use_tls through str()).
fn get_scalar(section: Option<&Value>, key: &str, default: &str) -> String {
    match section.and_then(|s| s.get(key)) {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => if *b { "True" } else { "False" }.to_string(),
        _ => get_str(section, key, default),
    }
}

pub fn load_values(path: &Path) -> Result<ConfigValues, String> {
    let defaults = ConfigValues::default();
    if !path.exists() {
        return Ok(defaults);
    }
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let payload: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let section = |name: &str| payload.get(name);
    let (server, apns, fcm, smtp, web) = (section("server"), section("apns"), section("fcm"), section("smtp"), section("web"));
    let d = &defaults;
    Ok(ConfigValues {
        data_dir: get_str(server, "data_dir", &d.data_dir),
        bind_host: get_str(server, "bind_host", &d.bind_host),
        bind_port: get_str(server, "bind_port", &d.bind_port),
        log_level: get_str(server, "log_level", &d.log_level),
        apns_team_id: get_str(apns, "team_id", ""),
        apns_key_id: get_str(apns, "key_id", ""),
        apns_bundle_id: get_str(apns, "bundle_id", ""),
        apns_auth_key_path: get_str(apns, "auth_key_path", ""),
        fcm_project_id: get_str(fcm, "project_id", ""),
        fcm_service_account_path: get_str(fcm, "service_account_path", ""),
        smtp_host: get_str(smtp, "host", ""),
        smtp_port: get_scalar(smtp, "port", &d.smtp_port),
        smtp_username: get_str(smtp, "username", ""),
        smtp_password: get_str(smtp, "password", ""),
        smtp_from_email: get_str(smtp, "from_email", ""),
        smtp_from_name: get_str(smtp, "from_name", ""),
        smtp_use_tls: get_scalar(smtp, "use_tls", &d.smtp_use_tls),
        web_base_url: get_str(web, "base_url", &d.web_base_url),
    })
}

fn resolve(base: &Path, value: &str) -> PathBuf {
    let expanded = match value.strip_prefix("~/") {
        Some(rest) => std::env::var("HOME").map(|home| Path::new(&home).join(rest)).unwrap_or_else(|_| value.into()),
        None => PathBuf::from(value),
    };
    if expanded.is_absolute() { expanded } else { base.join(expanded) }
}

fn required(value: &str, option: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() { Err(format!("{option} cannot be empty")) } else { Ok(value.to_string()) }
}

pub fn parse_port(value: &str, option: &str) -> Result<u16, String> {
    let port: i64 = value.trim().parse().map_err(|_| format!("{option} must be an integer"))?;
    if (1..=65535).contains(&port) { Ok(port as u16) } else { Err(format!("{option} must be between 1 and 65535")) }
}

pub fn parse_bool(value: &str, option: &str) -> Result<bool, String> {
    match value.trim().to_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(format!("{option} must be true or false")),
    }
}

pub fn parse_log_level(value: &str) -> Result<String, String> {
    let level = value.trim().to_uppercase();
    if ["CRITICAL", "ERROR", "WARNING", "INFO", "DEBUG"].contains(&level.as_str()) {
        Ok(level)
    } else {
        Err("log_level must be one of: CRITICAL, DEBUG, ERROR, INFO, WARNING".into())
    }
}

fn optional(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

pub fn to_settings(path: &Path, v: &ConfigValues) -> Result<Settings, String> {
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let dir = if dir.as_os_str().is_empty() { PathBuf::from(".") } else { dir };
    Ok(Settings {
        data_dir: resolve(&dir, &required(&v.data_dir, "data_dir")?),
        bind_host: required(&v.bind_host, "bind_host")?,
        bind_port: parse_port(&v.bind_port, "bind_port")?,
        log_level: parse_log_level(&v.log_level)?,
        apns_team_id: optional(&v.apns_team_id),
        apns_key_id: optional(&v.apns_key_id),
        apns_bundle_id: optional(&v.apns_bundle_id),
        apns_auth_key_path: optional(&v.apns_auth_key_path).map(|p| resolve(&dir, &p)),
        fcm_project_id: optional(&v.fcm_project_id),
        fcm_service_account_path: optional(&v.fcm_service_account_path).map(|p| resolve(&dir, &p)),
        smtp_host: optional(&v.smtp_host),
        smtp_port: parse_port(&v.smtp_port, "smtp_port")?,
        smtp_username: optional(&v.smtp_username),
        smtp_password: optional(&v.smtp_password),
        smtp_from_email: optional(&v.smtp_from_email),
        smtp_from_name: optional(&v.smtp_from_name),
        smtp_use_tls: parse_bool(&v.smtp_use_tls, "smtp_use_tls")?,
        web_base_url: optional(&v.web_base_url),
        config_dir: dir,
    })
}

pub fn load(path: &Path) -> Result<Settings, String> {
    to_settings(path, &load_values(path)?)
}

pub fn write_values(path: &Path, v: &ConfigValues) -> std::io::Result<()> {
    let payload = json!({
        "server": {"data_dir": v.data_dir, "bind_host": v.bind_host, "bind_port": v.bind_port, "log_level": v.log_level},
        "apns": {"team_id": v.apns_team_id, "key_id": v.apns_key_id, "bundle_id": v.apns_bundle_id, "auth_key_path": v.apns_auth_key_path},
        "fcm": {"project_id": v.fcm_project_id, "service_account_path": v.fcm_service_account_path},
        "smtp": {
            "host": v.smtp_host, "port": v.smtp_port, "username": v.smtp_username, "password": v.smtp_password,
            "from_email": v.smtp_from_email, "from_name": v.smtp_from_name, "use_tls": v.smtp_use_tls,
        },
        "web": {"base_url": v.web_base_url},
    });
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let sorted: Map<String, Value> = serde_json::from_value(payload).unwrap_or_default();
    std::fs::write(path, serde_json::to_string_pretty(&sorted)? + "\n")
}

/// `--configure`: walks through every setting, Enter keeps the current value.
pub fn run_configure(path: &Path, existing_error: Option<&str>) -> Result<(), String> {
    let mut v = load_values(path).unwrap_or_default();
    println!("Interactive configuration for {}", path.display());
    if let Some(error) = existing_error {
        println!("Current config could not be loaded cleanly: {error}");
    }
    println!("Press Enter to keep current values. Type 'none' to clear optional fields.\n");
    let stdin = std::io::stdin();
    let ask = |label: &str, current: &str, allow_clear: bool, check: &dyn Fn(&str) -> Result<String, String>| -> String {
        loop {
            let shown = if current.is_empty() { "(empty)" } else { current };
            let hint = if allow_clear { " Type 'none' to clear." } else { "" };
            print!("{label} [{shown}]:{hint} ");
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 {
                return current.to_string();
            }
            let answer = line.trim();
            if answer.is_empty() {
                return current.to_string();
            }
            if allow_clear && ["none", "clear"].contains(&answer.to_lowercase().as_str()) {
                return String::new();
            }
            match check(answer) {
                Ok(value) => return value,
                Err(error) => println!("Invalid value: {error}"),
            }
        }
    };
    let any = |s: &str| Ok(s.to_string());
    v.data_dir = ask("Data directory", &v.data_dir, false, &|s| required(s, "data_dir"));
    v.bind_host = ask("Bind host", &v.bind_host, false, &|s| required(s, "bind_host"));
    v.bind_port = ask("Bind port", &v.bind_port, false, &|s| parse_port(s, "bind_port").map(|p| p.to_string()));
    v.log_level = ask("Log level", &v.log_level, false, &parse_log_level);
    v.apns_team_id = ask("APNs team ID", &v.apns_team_id, true, &any);
    v.apns_key_id = ask("APNs key ID", &v.apns_key_id, true, &any);
    v.apns_bundle_id = ask("APNs bundle ID", &v.apns_bundle_id, true, &any);
    v.apns_auth_key_path = ask("APNs auth key path", &v.apns_auth_key_path, true, &any);
    v.fcm_project_id = ask("Firebase project ID", &v.fcm_project_id, true, &any);
    v.fcm_service_account_path = ask("Firebase service account path", &v.fcm_service_account_path, true, &any);
    v.smtp_host = ask("SMTP host", &v.smtp_host, true, &any);
    v.smtp_port = ask("SMTP port", &v.smtp_port, false, &|s| parse_port(s, "smtp_port").map(|p| p.to_string()));
    v.smtp_username = ask("SMTP username", &v.smtp_username, true, &any);
    v.smtp_password = ask("SMTP password", &v.smtp_password, true, &any);
    v.smtp_from_email = ask("SMTP from email", &v.smtp_from_email, true, &any);
    v.smtp_from_name = ask("SMTP from name", &v.smtp_from_name, true, &any);
    v.smtp_use_tls = ask("SMTP use TLS", &v.smtp_use_tls, false, &|s| parse_bool(s, "smtp_use_tls").map(|b| b.to_string()));
    v.web_base_url = ask("Web base URL", &v.web_base_url, true, &any);
    write_values(path, &v).map_err(|e| e.to_string())?;
    load(path)?;
    println!("Saved configuration to {}", path.display());
    Ok(())
}
