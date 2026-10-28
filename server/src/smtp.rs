//! Receiving email directly: a small SMTP server for mail to
//! `<API key>@<inbound domain>`, so email notifications arrive the moment the
//! sender's server hands them over, with no inbound-mail service in between.
//!
//! It only accepts recipients that are API keys of verified accounts, so it
//! can never relay mail anywhere. Each message is answered only after its
//! notifications are sent, so a sender retries if anything fails. STARTTLS is
//! offered when a certificate is configured; the files are read for each
//! connection, so a renewed certificate is used without a restart.

use crate::api::{self, AppState};
use mail_parser::{MessageParser, PartKind};
use serde_json::{Value, json};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::{self, pki_types::pem::PemObject};

/// The largest message accepted, as advertised with the SIZE extension.
const MAX_MESSAGE_BYTES: usize = 25 * 1024 * 1024;
const MAX_RECIPIENTS: usize = 50;
const MAX_COMMAND_BYTES: u64 = 4096;
const MAX_CONNECTIONS: usize = 100;
/// Unknown or out-of-order commands before the connection is dropped.
const MAX_ERRORS: usize = 10;
/// RFC 5321 asks for at least 5 minutes between commands and 10 minutes for data.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const DATA_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Listens for mail if `inbound_email.smtp_bind_port` is set.
pub async fn run(state: Arc<AppState>) {
    let Some(port) = state.settings.inbound_smtp_port else { return };
    let address = format!("{}:{port}", state.settings.inbound_smtp_bind_host);
    let listener = match TcpListener::bind(&address).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!("Cannot receive email on {address}: {error}");
            return;
        }
    };
    tracing::info!("Receiving email for @{} on {address}", state.settings.inbound_email_domain);
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::warn!("SMTP accept failed: {error}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let _ = stream.set_nodelay(true);
        let state = state.clone();
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            tokio::spawn(async move {
                let mut stream = stream;
                let _ = stream.write_all(b"421 4.3.2 Too busy, try again later\r\n").await;
            });
            continue;
        };
        tokio::spawn(async move {
            if let Err(error) = connection(&state, stream, peer).await {
                tracing::debug!("SMTP connection from {peer} ended: {error}");
            }
            drop(slot);
        });
    }
}

/// The certificate for STARTTLS, read now so a renewal takes effect at once.
fn tls_acceptor(state: &AppState) -> Option<TlsAcceptor> {
    let (cert_path, key_path) = (state.settings.inbound_smtp_tls_cert_path.as_ref()?, state.settings.inbound_smtp_tls_key_path.as_ref()?);
    let load = || -> Result<TlsAcceptor, String> {
        let certs = rustls::pki_types::CertificateDer::pem_file_iter(cert_path)
            .and_then(|certs| certs.collect::<Result<Vec<_>, _>>())
            .map_err(|e| format!("{}: {e}", cert_path.display()))?;
        let key = rustls::pki_types::PrivateKeyDer::from_pem_file(key_path).map_err(|e| format!("{}: {e}", key_path.display()))?;
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| e.to_string())?;
        Ok(TlsAcceptor::from(Arc::new(config)))
    };
    load().inspect_err(|error| tracing::warn!("STARTTLS is unavailable: {error}")).ok()
}

async fn connection(state: &AppState, stream: tokio::net::TcpStream, peer: SocketAddr) -> io::Result<()> {
    let acceptor = tls_acceptor(state);
    let mut reader = BufReader::new(stream);
    let greeting = format!("220 {} ESMTP MG Notify\r\n", state.settings.inbound_smtp_hostname);
    reader.write_all(greeting.as_bytes()).await?;
    let Some(acceptor) = acceptor else {
        session(state, &mut reader, peer, false).await?;
        return Ok(());
    };
    if session(state, &mut reader, peer, true).await? == End::Quit {
        return Ok(());
    }
    // Anything the client sent after STARTTLS but before the handshake is
    // dropped with the buffer, as RFC 3207 requires.
    let tls = acceptor.accept(reader.into_inner()).await?;
    let mut reader = BufReader::new(tls);
    session(state, &mut reader, peer, false).await?;
    Ok(())
}

#[derive(PartialEq)]
enum End {
    Quit,
    StartTls,
}

/// One mail transaction in progress.
#[derive(Default)]
struct Envelope {
    from: Option<String>,
    recipients: Vec<String>,
}

/// The commands of one SMTP session, until QUIT or STARTTLS (when `can_start_tls`).
async fn session<S: AsyncRead + AsyncWrite + Unpin>(
    state: &AppState,
    stream: &mut BufReader<S>,
    peer: SocketAddr,
    can_start_tls: bool,
) -> io::Result<End> {
    let hostname = &state.settings.inbound_smtp_hostname;
    let mut greeted = false;
    let mut envelope = Envelope::default();
    let mut errors = 0;
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = tokio::time::timeout(COMMAND_TIMEOUT, (&mut *stream).take(MAX_COMMAND_BYTES).read_until(b'\n', &mut line)).await;
        let Ok(read) = read else {
            reply(stream, "421 4.4.2 Timed out").await?;
            return Ok(End::Quit);
        };
        if read? == 0 {
            return Ok(End::Quit);
        }
        if !line.ends_with(b"\n") {
            reply(stream, "500 5.5.2 Line too long").await?;
            return Ok(End::Quit);
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches(['\r', '\n']);
        let (verb, argument) = text.split_once(' ').unwrap_or((text, ""));
        let argument = argument.trim();
        let response: String = match verb.to_ascii_uppercase().as_str() {
            "EHLO" => {
                greeted = true;
                envelope = Envelope::default();
                let mut lines = vec![hostname.to_string(), format!("SIZE {MAX_MESSAGE_BYTES}"), "8BITMIME".into(), "SMTPUTF8".into()];
                lines.push("PIPELINING".into());
                lines.push("ENHANCEDSTATUSCODES".into());
                if can_start_tls {
                    lines.push("STARTTLS".into());
                }
                let last = lines.len() - 1;
                lines.iter().enumerate().map(|(i, l)| format!("250{}{l}", if i == last { ' ' } else { '-' })).collect::<Vec<_>>().join("\r\n")
            }
            "HELO" => {
                greeted = true;
                envelope = Envelope::default();
                format!("250 {hostname}")
            }
            "STARTTLS" if can_start_tls => {
                reply(stream, "220 2.0.0 Ready to start TLS").await?;
                return Ok(End::StartTls);
            }
            "STARTTLS" => "454 4.7.0 TLS not available".into(),
            "MAIL" if !greeted => "503 5.5.1 Say EHLO first".into(),
            "MAIL" if envelope.from.is_some() => "503 5.5.1 Already in a transaction".into(),
            "MAIL" => match path_argument(argument, "FROM:") {
                Some((address, parameters)) => {
                    let size = parameters.iter().find_map(|p| p.strip_prefix("SIZE=").or_else(|| p.strip_prefix("size=")));
                    if size.and_then(|s| s.parse::<usize>().ok()).is_some_and(|s| s > MAX_MESSAGE_BYTES) {
                        "552 5.3.4 Message too big".into()
                    } else {
                        envelope.from = Some(address);
                        "250 2.1.0 OK".into()
                    }
                }
                None => "501 5.5.4 Syntax: MAIL FROM:<address>".into(),
            },
            "RCPT" if envelope.from.is_none() => "503 5.5.1 Need MAIL first".into(),
            "RCPT" => match path_argument(argument, "TO:") {
                Some(_) if envelope.recipients.len() >= MAX_RECIPIENTS => "452 4.5.3 Too many recipients".into(),
                Some((address, _)) => match api::inbound_recipient(state, &address) {
                    Ok(Some(_)) => {
                        envelope.recipients.push(address);
                        "250 2.1.5 OK".into()
                    }
                    Ok(None) => "550 5.1.1 No such mailbox".into(),
                    Err(_) => "451 4.3.0 Try again later".into(),
                },
                None => "501 5.5.4 Syntax: RCPT TO:<address>".into(),
            },
            "DATA" if envelope.recipients.is_empty() => "503 5.5.1 Need RCPT first".into(),
            "DATA" => {
                reply(stream, "354 End data with <CR><LF>.<CR><LF>").await?;
                let message = match tokio::time::timeout(DATA_TIMEOUT, read_data(stream)).await {
                    Ok(result) => result?,
                    Err(_) => {
                        reply(stream, "421 4.4.2 Timed out").await?;
                        return Ok(End::Quit);
                    }
                };
                let recipients = std::mem::take(&mut envelope.recipients);
                envelope = Envelope::default();
                match message {
                    None => "552 5.3.4 Message too big".into(),
                    Some(raw) => deliver(state, &raw, recipients, peer).await,
                }
            }
            "RSET" => {
                envelope = Envelope::default();
                "250 2.0.0 OK".into()
            }
            "NOOP" => "250 2.0.0 OK".into(),
            "VRFY" => "252 2.5.2 Cannot verify, but will try delivery".into(),
            "QUIT" => {
                reply(stream, "221 2.0.0 Bye").await?;
                return Ok(End::Quit);
            }
            _ => "500 5.5.1 Unrecognized command".into(),
        };
        if response.starts_with('5') && !response.starts_with("550") && !response.starts_with("552") {
            errors += 1;
            if errors >= MAX_ERRORS {
                reply(stream, "421 4.7.0 Too many errors").await?;
                return Ok(End::Quit);
            }
        }
        reply(stream, &response).await?;
    }
}

async fn reply<S: AsyncWrite + Unpin>(stream: &mut S, text: &str) -> io::Result<()> {
    stream.write_all(format!("{text}\r\n").as_bytes()).await?;
    stream.flush().await
}

/// `FROM:<address> PARAM=value ...`: the address (empty for `<>`) and parameters.
fn path_argument(argument: &str, prefix: &str) -> Option<(String, Vec<String>)> {
    if argument.len() < prefix.len() || !argument[..prefix.len()].eq_ignore_ascii_case(prefix) {
        return None;
    }
    let rest = argument[prefix.len()..].trim_start();
    let (path, parameters) = match rest.strip_prefix('<') {
        Some(inner) => {
            let end = inner.find('>')?;
            (&inner[..end], &inner[end + 1..])
        }
        None => rest.split_once(' ').unwrap_or((rest, "")),
    };
    // A source route (`@a,@b:user@host`) is ignored, as RFC 5321 allows.
    let address = path.rsplit_once(':').map_or(path, |(_, address)| address).trim().to_string();
    Some((address, parameters.split_whitespace().map(String::from).collect()))
}

/// The message after DATA, dot-unstuffed, up to the lone `.` line. None if it
/// was too big (the rest is read and dropped, so the session can go on).
async fn read_data<S: AsyncRead + Unpin>(stream: &mut BufReader<S>) -> io::Result<Option<Vec<u8>>> {
    let mut message = Vec::new();
    let mut too_big = false;
    let mut line = Vec::new();
    let mut at_line_start = true;
    loop {
        line.clear();
        if (&mut *stream).take(64 * 1024).read_until(b'\n', &mut line).await? == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed during DATA"));
        }
        if at_line_start && (line == b".\r\n" || line == b".\n") {
            return Ok((!too_big).then_some(message));
        }
        let content = if at_line_start && line.starts_with(b".") { &line[1..] } else { &line[..] };
        at_line_start = line.ends_with(b"\n");
        if message.len() + content.len() > MAX_MESSAGE_BYTES {
            too_big = true;
            message = Vec::new();
        }
        if !too_big {
            message.extend_from_slice(content);
        }
    }
}

/// Parses the message into the webhook's item format and sends it on.
async fn deliver(state: &AppState, raw: &[u8], recipients: Vec<String>, peer: SocketAddr) -> String {
    let item = email_item(raw, recipients);
    match api::process_inbound_email(state, &item).await {
        Ok(outcome) => {
            if outcome.duplicate {
                tracing::info!("Email from {peer} was a duplicate");
            } else {
                tracing::info!("Email from {peer} delivered to {} account(s)", outcome.delivered);
            }
            "250 2.0.0 Delivered".into()
        }
        Err(error) => {
            tracing::error!("Email from {peer} could not be delivered: {}", error.detail());
            "451 4.3.0 Try again later".into()
        }
    }
}

/// An email as the item `api::process_inbound_email` takes: the envelope's
/// recipients (so Bcc works), the sender, subject and text and HTML bodies,
/// with line endings as `\n` like the webhook's.
fn email_item(raw: &[u8], recipients: Vec<String>) -> Value {
    let Some(message) = MessageParser::default().parse(raw) else {
        return json!({"Recipients": recipients, "RawTextBody": String::from_utf8_lossy(raw)});
    };
    let sender = message.from().and_then(|from| from.first().map(|m| (m.name().unwrap_or("").to_string(), m.address().unwrap_or("").to_string())));
    let (name, address) = sender.unwrap_or_default();
    // A real text part if there is one, else the HTML converted to text.
    let text = message
        .text_body()
        .find(|part| matches!(part.kind(), PartKind::Text))
        .and_then(|part| part.text())
        .or_else(|| message.body_text(0))
        .map(|text| text.replace("\r\n", "\n"))
        .unwrap_or_default();
    // Only a real HTML part: plain text is turned into HTML later, the same
    // way for every email.
    let html = message
        .html_body()
        .find(|part| matches!(part.kind(), PartKind::Html))
        .and_then(|part| part.text())
        .map(|html| html.replace("\r\n", "\n"))
        .unwrap_or_default();
    let mut item = json!({
        "From": {"Name": name, "Address": address},
        "Recipients": recipients,
        "Subject": message.subject().unwrap_or(""),
        "RawTextBody": text,
        "RawHtmlBody": html,
    });
    if let Some(id) = message.message_id() {
        item["MessageId"] = json!(format!("<{id}>"));
    }
    item
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_arguments() {
        assert_eq!(path_argument("FROM:<a@b.c> SIZE=10 BODY=8BITMIME", "FROM:"), Some(("a@b.c".into(), vec!["SIZE=10".into(), "BODY=8BITMIME".into()])));
        assert_eq!(path_argument("from: <>", "FROM:"), Some(("".into(), vec![])));
        assert_eq!(path_argument("TO:<@relay:key@notify.example>", "TO:"), Some(("key@notify.example".into(), vec![])));
        assert_eq!(path_argument("TO:key@notify.example", "TO:"), Some(("key@notify.example".into(), vec![])));
        assert_eq!(path_argument("FROM <a@b.c>", "FROM:"), None);
    }

    #[test]
    fn email_items() {
        let raw = b"From: \"Jane Example\" <jane@example.com>\r\nTo: someone@example.com\r\nSubject: =?utf-8?q?Caf=C3=A9?=\r\n\
Message-ID: <abc@example.com>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=b\r\n\r\n\
--b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHello\r\n--b\r\nContent-Type: text/html\r\n\r\n<p>Hello</p>\r\n--b--\r\n";
        let item = email_item(raw, vec!["key@notify.example".into()]);
        assert_eq!(item["From"], json!({"Name": "Jane Example", "Address": "jane@example.com"}));
        assert_eq!(item["Subject"], "Café");
        assert_eq!(item["MessageId"], "<abc@example.com>");
        assert_eq!(item["Recipients"], json!(["key@notify.example"]));
        assert_eq!(item["RawTextBody"].as_str().unwrap().trim(), "Hello");
        assert_eq!(item["RawHtmlBody"].as_str().unwrap().trim(), "<p>Hello</p>");

        let plain = email_item(b"From: bob@example.com\r\nSubject: Hi\r\n\r\nJust text\r\n", vec![]);
        assert_eq!(plain["RawTextBody"].as_str().unwrap().trim(), "Just text");
        assert_eq!(plain["RawHtmlBody"], "");
        assert!(plain.get("MessageId").is_none());
    }
}
