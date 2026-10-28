//! Email: sending verification and reset mail, and turning inbound mail
//! (Brevo's webhook) into notifications.

use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use regex::Regex;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::LazyLock;
use std::time::Duration;

use crate::config::Settings;
use crate::util::{html_escape, truncate_chars};

pub struct EmailSender {
    settings: Settings,
    pub enabled: bool,
}

impl EmailSender {
    pub fn new(settings: &Settings) -> Self {
        let enabled = settings.smtp_host.is_some() && settings.smtp_from_email.is_some() && settings.web_base_url.is_some();
        Self { settings: settings.clone(), enabled }
    }

    fn link(&self, action: &str, token: &str) -> String {
        format!("{}?action={action}&token={token}", self.settings.web_base_url.as_deref().unwrap_or(""))
    }

    pub async fn send_password_reset(&self, to: &str, token: &str) -> Result<(), String> {
        let body = format!(
            "A password reset was requested for your MG Notify account.\n\n\
             Open this link to reset your password:\n{}\n\n\
             If you did not request this, you can ignore this email.\n",
            self.link("reset", token)
        );
        self.send(to, "MG Notify password reset", body).await
    }

    pub async fn send_email_verification(&self, to: &str, token: &str) -> Result<(), String> {
        let body = format!(
            "Verify your email address to finish setting up your MG Notify account.\n\n\
             Open this link to verify your email:\n{}\n\n\
             If you did not create this account, you can ignore this email.\n",
            self.link("verify-email", token)
        );
        self.send(to, "Verify your MG Notify email", body).await
    }

    async fn send(&self, to: &str, subject: &str, body: String) -> Result<(), String> {
        let s = &self.settings;
        let (Some(host), Some(from_email)) = (&s.smtp_host, &s.smtp_from_email) else {
            return Err("SMTP/email configuration is incomplete".into());
        };
        let from: Mailbox = match &s.smtp_from_name {
            Some(name) => format!("{name} <{from_email}>"),
            None => from_email.clone(),
        }
        .parse()
        .map_err(|e| format!("bad from address: {e}"))?;
        let message = Message::builder()
            .from(from)
            .to(to.parse().map_err(|e| format!("bad address: {e}"))?)
            .subject(subject)
            .body(body)
            .map_err(|e| e.to_string())?;
        let mut builder = if s.smtp_use_tls {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host).map_err(|e| e.to_string())?
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)
        }
        .port(s.smtp_port)
        .timeout(Some(Duration::from_secs(20)));
        if let (Some(user), Some(password)) = (&s.smtp_username, &s.smtp_password) {
            builder = builder.credentials(Credentials::new(user.clone(), password.clone()));
        }
        builder.build().send(message).await.map(|_| ()).map_err(|e| e.to_string())
    }
}

/// Whitespace collapsed to single spaces, a fallback when empty, then cut to length.
pub fn single_line(value: &str, fallback: &str, max: usize) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(if collapsed.is_empty() { fallback } else { &collapsed }, max)
}

fn as_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `Name <address>` or a bare address, as Python's email.utils.parseaddr reads it.
fn parse_address(text: &str) -> (String, String) {
    let text = text.trim();
    if let (Some(start), Some(end)) = (text.rfind('<'), text.rfind('>')) {
        if start < end {
            let name = text[..start].trim().trim_matches('"').trim().to_string();
            return (name, text[start + 1..end].trim().to_string());
        }
    }
    (String::new(), text.to_string())
}

/// A mailbox from the webhook: `{"Name": ..., "Address": ...}` or text.
pub fn sender_mailbox(value: &Value) -> (String, String) {
    if let Value::Object(map) = value {
        let get = |a: &str, b: &str| as_text(map.get(a).or_else(|| map.get(b)).unwrap_or(&Value::Null)).trim().to_string();
        return (get("Name", "name"), get("Address", "address"));
    }
    parse_address(&as_text(value))
}

pub fn mailbox_address(value: &Value) -> String {
    sender_mailbox(value).1
}

static BREVO_TRACKING_MARKDOWN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\[[^\]]*\]\((https?://[^)\s]*\.sendibt\d+\.com/tr/op/[^)\s]*)\)\s*").unwrap());
static BREVO_TRACKING_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)https?://[^\s)\]]*\.sendibt\d+\.com/tr/op/[^\s)\]]*\s*").unwrap());
static BREVO_CLICK_TRACKING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)https?://[a-z0-9.-]+\.sendibt\d+\.com/tr/cl/[a-z0-9_-]+").unwrap());
static HEADING_BLOCK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\*{4,}[ \t]*\r?\n([^\r\n]+)\r?\n\*{4,}[ \t]*$").unwrap());
static INLINE_HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\*{4,}\s*(.+?)\s*\*{4,}$").unwrap());
static SPACED_PAREN_URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\(\s+(https?://[^)\s]+)\s+\)").unwrap());
static MANY_NEWLINES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n{3,}").unwrap());
static PARAGRAPH_BREAK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\r?\n\s*\r?\n").unwrap());
static PLAIN_URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)https?://[^\s<>]+").unwrap());
/// Brevo's open-tracking image, whatever its size attributes.
static BREVO_TRACKING_IMAGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)<img\b[^>]*\bsrc\s*=\s*["']?https?://[^"'\s>]*\.sendibt\d+\.com/tr/op/[^>]*>"#).unwrap());
static TRACKING_PIXEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)<img\b[^>]*\b(?:width|height)\s*=\s*["']?\s*1\s*["']?[\s/>][^>]*>?"#).unwrap());

/// Brevo rewrites links into click-tracking redirects; follow each (at most
/// 10) once to put the real destination back.
async fn restore_click_tracking(values: [String; 2]) -> [String; 2] {
    let mut seen = HashSet::new();
    let urls: Vec<String> = values
        .iter()
        .flat_map(|v| BREVO_CLICK_TRACKING.find_iter(v).map(|m| m.as_str().to_string()).collect::<Vec<_>>())
        .filter(|url| seen.insert(url.clone()))
        .take(10)
        .collect();
    if urls.is_empty() {
        return values;
    }
    let Ok(client) = reqwest::Client::builder().timeout(Duration::from_secs(5)).redirect(reqwest::redirect::Policy::none()).build()
    else {
        return values;
    };
    let destinations = futures::future::join_all(urls.iter().map(|url| {
        let client = client.clone();
        async move {
            let response = client.get(url).send().await.ok()?;
            if !response.status().is_redirection() {
                return None;
            }
            let location = response.headers().get("location")?.to_str().ok()?.trim().to_string();
            let parsed = url::Url::parse(&location).ok()?;
            (matches!(parsed.scheme(), "http" | "https") && parsed.host().is_some()).then_some(parsed.to_string())
        }
    }))
    .await;
    values.map(|mut value| {
        for (url, destination) in urls.iter().zip(&destinations) {
            if let Some(destination) = destination {
                value = value.replace(url.as_str(), destination);
            }
        }
        value
    })
}

/// Email HTML reduced to a safe subset: no scripts, styles, forms or frames,
/// links only to web and mail addresses, and no tracking pixels.
pub fn sanitize_email_html(value: &str) -> String {
    let input = truncate_chars(value, 200_000);
    let input = TRACKING_PIXEL.replace_all(&input, "");
    let input = BREVO_TRACKING_IMAGE.replace_all(&input, "");
    let tags: HashSet<&str> = [
        "a", "b", "blockquote", "br", "code", "div", "em", "h1", "h2", "h3", "h4", "h5", "h6", "hr", "i", "img", "li", "ol",
        "p", "pre", "span", "strong", "table", "tbody", "td", "tfoot", "th", "thead", "tr", "u", "ul",
    ]
    .into();
    let mut builder = ammonia::Builder::empty();
    builder
        .tags(tags)
        .clean_content_tags(["script", "style", "embed", "form", "iframe", "object", "svg"].into())
        .add_tag_attributes("a", ["href", "title"])
        .add_tag_attributes("img", ["alt", "height", "src", "title", "width"])
        .add_tag_attributes("td", ["colspan", "rowspan"])
        .add_tag_attributes("th", ["colspan", "rowspan"])
        .url_schemes(["http", "https", "mailto", "cid"].into())
        .url_relative(ammonia::UrlRelative::Deny)
        .link_rel(None)
        .attribute_filter(|element, attribute, value| {
            let lowered = value.trim().to_lowercase();
            match attribute {
                "href" if !["http://", "https://", "mailto:"].iter().any(|s| lowered.starts_with(s)) => None,
                "src" if element == "img" && !["http://", "https://", "cid:"].iter().any(|s| lowered.starts_with(s)) => None,
                "width" | "height" | "colspan" | "rowspan" if !value.chars().all(|c| c.is_ascii_digit()) || value.is_empty() => None,
                _ => Some(value.trim().to_string().into()),
            }
        });
    truncate_chars(builder.clean(&input).to_string().trim(), 200_000)
}

/// Plain text as HTML paragraphs, with web addresses as links.
pub fn plain_text_to_html(value: &str) -> String {
    let mut html = String::new();
    for paragraph in PARAGRAPH_BREAK.split(value.trim()) {
        let mut parts = String::new();
        let mut position = 0;
        for found in PLAIN_URL.find_iter(paragraph) {
            parts.push_str(&html_escape(&paragraph[position..found.start()]));
            let url = found.as_str();
            parts.push_str(&format!("<a href=\"{}\">{}</a>", html_escape(url), html_escape(url)));
            position = found.end();
        }
        parts.push_str(&html_escape(&paragraph[position..]));
        html.push_str(&format!("<p>{}</p>", parts.replace("\r\n", "\n").replace('\n', "<br>")));
    }
    if html.is_empty() { "<p>(No message body)</p>".into() } else { html }
}

/// The notification text and HTML for an inbound email item.
pub async fn email_content(item: &Value) -> (String, String) {
    let field = |name: &str| as_text(item.get(name).unwrap_or(&Value::Null)).trim().to_string();
    let raw_text = field("RawTextBody");
    let source = if raw_text.is_empty() { field("ExtractedMarkdownMessage") } else { raw_text };
    let message = truncate_chars(&source, 20_000);
    let raw_html = truncate_chars(&field("RawHtmlBody"), 200_000);
    let [message, raw_html] = restore_click_tracking([message, raw_html]).await;
    let message = BREVO_TRACKING_MARKDOWN.replace_all(&message, "");
    let message = BREVO_TRACKING_URL.replace_all(&message, "");
    let message = HEADING_BLOCK.replace_all(&message, "$1");
    let message = INLINE_HEADING.replace_all(&message, "$1");
    let message = SPACED_PAREN_URL.replace_all(&message, "($1)");
    let message = MANY_NEWLINES.replace_all(&message, "\n\n");
    let message = message.trim();
    let message = truncate_chars(if message.is_empty() { "(No message body)" } else { message }, 3000);
    let html = if raw_html.is_empty() { plain_text_to_html(&message) } else { sanitize_email_html(&raw_html) };
    (message, html)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracking_images_are_removed() {
        let html = r#"<img src="https://abc.r.af.d.sendibt2.com/tr/op/XYZ"> Hello <img src="https://example.com/logo.png" alt="Logo">"#;
        assert_eq!(sanitize_email_html(html), r#"Hello <img src="https://example.com/logo.png" alt="Logo">"#);
    }
}
