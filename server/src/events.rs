//! Realtime events for open apps and websites: `GET /events`, a Server-Sent
//! Events stream per signed-in session. Clients get changes the moment they
//! happen instead of polling; closed apps still get silent pushes.

use axum::response::sse::{Event, KeepAlive, Sse};
use futures::Stream;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;

/// Who an event is for: every open session of the account, or only some.
#[derive(Clone, Debug)]
pub enum Audience {
    Account,
    Sessions(HashSet<String>),
}

#[derive(Debug)]
pub struct HubEvent {
    account_id: String,
    audience: Audience,
    name: &'static str,
    data: Value,
    /// The receiving session no longer exists: close the stream after this.
    ends_session: bool,
}

/// An open event stream, for the admin page.
#[derive(Clone, Debug, serde::Serialize)]
pub struct OpenStream {
    pub account_id: String,
    pub device_id: Option<String>,
    pub address: String,
    pub user_agent: String,
    pub since: String,
}

type Open = Arc<Mutex<HashMap<u64, OpenStream>>>;

/// Forgets an open stream when the stream (and so this guard) is dropped:
/// the client went away or the stream ended.
struct OpenGuard {
    id: u64,
    open: Open,
}

impl Drop for OpenGuard {
    fn drop(&mut self) {
        self.open.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.id);
    }
}

#[derive(Clone)]
pub struct Hub {
    sender: broadcast::Sender<Arc<HubEvent>>,
    open: Open,
    next_id: Arc<AtomicU64>,
}

impl Hub {
    pub fn new() -> Self {
        Self { sender: broadcast::channel(1024).0, open: Arc::default(), next_id: Arc::default() }
    }

    /// Every open event stream now.
    pub fn open_streams(&self) -> Vec<OpenStream> {
        let mut list: Vec<OpenStream> = self.open.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
        list.sort_by(|a, b| a.since.cmp(&b.since));
        list
    }

    fn send(&self, account_id: &str, audience: Audience, name: &'static str, data: Value, ends_session: bool) {
        // No open streams is normal; nothing to do then.
        let _ = self.sender.send(Arc::new(HubEvent { account_id: account_id.into(), audience, name, data, ends_session }));
    }

    /// Something changed for the whole account (all its open sessions hear it).
    pub fn account(&self, account_id: &str, name: &'static str, data: Value) {
        self.send(account_id, Audience::Account, name, data, false);
    }

    /// These sessions were signed out: tell their open streams, then close them.
    pub fn signed_out(&self, account_id: &str, sessions: Vec<String>) {
        if !sessions.is_empty() {
            self.send(account_id, Audience::Sessions(sessions.into_iter().collect()), "signed_out", json!({}), true);
        }
    }

    /// Every session of the account was signed out (password change or reset).
    pub fn signed_out_everywhere(&self, account_id: &str) {
        self.send(account_id, Audience::Account, "signed_out", json!({}), true);
    }

    pub fn account_deleted(&self, account_id: &str) {
        self.send(account_id, Audience::Account, "account_deleted", json!({}), true);
    }

    /// The stream for one session. It starts with `ready` (the account, so the
    /// client can resynchronize after a reconnect), sends a keep-alive comment
    /// every 20 seconds so proxies keep it open, and sends `resync` if it ever
    /// falls too far behind to have every event.
    pub fn stream(
        &self,
        account: Value,
        account_id: String,
        session_hash: String,
        info: OpenStream,
    ) -> Sse<impl Stream<Item = Result<Event, Infallible>> + use<>> {
        let receiver = self.sender.subscribe();
        let first = Event::default().event("ready").data(json!({"account": account}).to_string());
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.open.lock().unwrap_or_else(|e| e.into_inner()).insert(id, info);
        let guard = Arc::new(OpenGuard { id, open: self.open.clone() });
        let state = (receiver, Some(first), false);
        let stream = futures::stream::unfold(state, move |(mut receiver, pending, finished)| {
            let _guard = guard.clone();
            let account_id = account_id.clone();
            let session_hash = session_hash.clone();
            async move {
                if let Some(event) = pending {
                    return Some((Ok(event), (receiver, None, finished)));
                }
                if finished {
                    return None;
                }
                loop {
                    match receiver.recv().await {
                        Ok(event) => {
                            if event.account_id != account_id {
                                continue;
                            }
                            if let Audience::Sessions(sessions) = &event.audience {
                                if !sessions.contains(&session_hash) {
                                    continue;
                                }
                            }
                            let message = Event::default().event(event.name).data(event.data.to_string());
                            return Some((Ok(message), (receiver, None, event.ends_session)));
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            let message = Event::default().event("resync").data("{}");
                            return Some((Ok(message), (receiver, None, false)));
                        }
                        Err(broadcast::error::RecvError::Closed) => return None,
                    }
                }
            }
        });
        Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(20)).text("keep-alive"))
    }
}
