//! The server's recent log lines, kept in memory for the admin page so it
//! can show them on any system. Bounded: the oldest lines go first.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{Mutex, OnceLock};

use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

/// Lines kept.
pub const KEEP: usize = 2000;

static LINES: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();

fn lines() -> &'static Mutex<VecDeque<String>> {
    LINES.get_or_init(|| Mutex::new(VecDeque::with_capacity(KEEP)))
}

/// The last `n` lines, oldest first.
pub fn recent(n: usize) -> Vec<String> {
    let g = lines().lock().unwrap_or_else(|e| e.into_inner());
    let skip = g.len().saturating_sub(n);
    g.iter().skip(skip).cloned().collect()
}

fn push(line: String) {
    let mut g = lines().lock().unwrap_or_else(|e| e.into_inner());
    if g.len() == KEEP {
        g.pop_front();
    }
    g.push_back(line);
}

/// The tracing layer that fills the buffer.
pub struct BufferLayer;

struct Fields {
    message: String,
    rest: String,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.rest, " {}={value}", field.name());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.rest, " {}={value:?}", field.name());
        }
    }
}

impl<S: Subscriber> Layer<S> for BufferLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut f = Fields { message: String::new(), rest: String::new() };
        event.record(&mut f);
        push(format!("{} {} {}: {}{}", crate::util::iso_now(), meta.level(), meta.target(), f.message, f.rest));
    }
}
