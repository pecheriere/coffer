//! The capture of every log event in a test process, and the signal for when
//! code under test logs a given event.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use tokio::sync::Notify;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt as _};

/// A signal that fires once code under test, on any thread, logs the event
/// `message` whose field `field` displays as `value`. Await it to know that
/// the code has reached that point, instead of sleeping. Matching a field
/// that identifies the record under test keeps tests running in parallel
/// from tripping each other's signals.
///
/// It relies on the process-wide subscriber [`capture_logs`] installs
/// (installing it if needed). It is global rather than attached to the future under test
/// because tracing caches whether a log statement is enabled the first time
/// any thread reaches it: a scoped subscriber can miss statements first
/// reached by another test. Each call also rebuilds that cache, so a
/// statement reached before the subscriber was installed is watched too.
///
/// # Panics
///
/// If another global subscriber was installed in this process.
#[must_use]
pub fn event_signal(
    message: &'static str,
    field: &'static str,
    value: impl fmt::Display,
) -> Arc<Notify> {
    capture_logs();
    let reached = Arc::new(Notify::new());
    waiters().push(Waiter {
        message,
        field,
        value: value.to_string(),
        reached: Arc::clone(&reached),
    });
    tracing::callsite::rebuild_interest_cache();
    reached
}

/// Installs, once per process, the global subscriber that captures every
/// log event (read with [`captured_log_text`]) and fires
/// [`event_signal`]s. Every [`TestServer`](super::TestServer) constructor
/// calls it.
///
/// # Panics
///
/// If another global subscriber was installed in this process.
pub fn capture_logs() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        tracing::subscriber::set_global_default(tracing_subscriber::registry().with(Signals))
            .expect("no other global subscriber in a test process");
    });
}

/// Every event and span logged in this process since [`capture_logs`]
/// ran, one per line: an event's level, message and other fields; a span's
/// name and fields, and fields recorded on it later. Only the newest 16 MiB
/// are kept. Tests running in parallel share it, so a test looks for what
/// identifies its own records.
#[must_use]
pub fn captured_log_text() -> String {
    captured().text().to_owned()
}

/// Logs a line holding a fresh marker and returns the marker. A test that
/// checks the capture for something that must be absent logs a marker
/// first and checks afterwards that it is still captured: the capture drops
/// its oldest lines first, so while the marker is kept, every line logged
/// after it is kept too, and the absence means something.
#[must_use]
pub fn log_marker() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let marker = format!(
        "[log-marker-{}]",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    tracing::info!(marker = %marker, "test log marker");
    marker
}

/// The most text the capture keeps, in bytes.
const CAPTURE_LIMIT: usize = 16 * 1024 * 1024;

fn captured() -> MutexGuard<'static, CapturedLog> {
    static CAPTURED: Mutex<CapturedLog> = Mutex::new(CapturedLog::with_limit(CAPTURE_LIMIT));
    CAPTURED.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Captured log lines, of which only the newest `limit` bytes are kept.
struct CapturedLog {
    text: String,
    limit: usize,
}

impl CapturedLog {
    const fn with_limit(limit: usize) -> Self {
        Self {
            text: String::new(),
            limit,
        }
    }

    fn text(&self) -> &str {
        &self.text
    }

    /// Appends `line` and a newline, then drops the oldest whole lines
    /// beyond the limit. A line longer than the limit is cut to fit.
    fn push_line(&mut self, line: &str) {
        let room = self.limit.saturating_sub(1);
        let mut end = line.len().min(room);
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        self.text.push_str(&line[..end]);
        self.text.push('\n');
        if let Some(excess) = self.text.len().checked_sub(self.limit).filter(|&e| e > 0) {
            // Cut after the first newline at or after byte `excess - 1`, so
            // that a newline ending exactly at the excess keeps everything
            // after it. Newlines are always character boundaries.
            let from = excess - 1;
            let cut = self.text.as_bytes()[from..]
                .iter()
                .position(|&byte| byte == b'\n')
                .map_or(self.text.len(), |at| from + at + 1);
            self.text.drain(..cut);
        }
    }
}

/// One capture line: `head`, then the message, then the other fields.
fn capture_line(head: &str, fields: &Fields) -> String {
    let mut line = head.to_owned();
    if let Some(message) = fields.0.get("message") {
        line.push(' ');
        line.push_str(message);
    }
    for (name, value) in &fields.0 {
        if *name != "message" {
            line.push(' ');
            line.push_str(name);
            line.push('=');
            line.push_str(value);
        }
    }
    line
}

/// An [`event_signal`] not yet fired.
struct Waiter {
    message: &'static str,
    field: &'static str,
    value: String,
    reached: Arc<Notify>,
}

fn waiters() -> MutexGuard<'static, Vec<Waiter>> {
    static WAITERS: Mutex<Vec<Waiter>> = Mutex::new(Vec::new());
    WAITERS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Appends every event and span to the capture, and fires the
/// [`Waiter`]s an event matches, then forgets them.
struct Signals;

impl<S: Subscriber> Layer<S> for Signals {
    fn on_new_span(&self, attrs: &Attributes<'_>, _id: &Id, _ctx: Context<'_, S>) {
        let mut fields = Fields(HashMap::new());
        attrs.record(&mut fields);
        let head = format!("SPAN {}", attrs.metadata().name());
        captured().push_line(&capture_line(&head, &fields));
    }

    fn on_record(&self, _id: &Id, values: &Record<'_>, _ctx: Context<'_, S>) {
        let mut fields = Fields(HashMap::new());
        values.record(&mut fields);
        captured().push_line(&capture_line("SPAN RECORD", &fields));
    }

    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut fields = Fields(HashMap::new());
        event.record(&mut fields);
        let head = event.metadata().level().to_string();
        captured().push_line(&capture_line(&head, &fields));
        let mut waiters = waiters();
        if waiters.is_empty() {
            return;
        }
        waiters.retain(|waiter| {
            let matched = fields.0.get("message").map(String::as_str) == Some(waiter.message)
                && fields.0.get(waiter.field) == Some(&waiter.value);
            if matched {
                waiter.reached.notify_one();
            }
            !matched
        });
    }
}

/// An event's fields, by name, as their `Debug` text (a `%`-captured value
/// shows its `Display` text).
struct Fields(HashMap<&'static str, String>);

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.0.insert(field.name(), format!("{value:?}"));
    }
}

#[cfg(test)]
mod tests {
    use super::CapturedLog;

    #[test]
    fn capture_keeps_the_newest_lines_within_its_bound() {
        let mut log = CapturedLog::with_limit(100);
        for i in 0..50 {
            log.push_line(&format!("line {i:03}"));
        }
        assert!(log.text().len() <= 100, "the bound holds");
        assert!(
            log.text().ends_with("line 049\n"),
            "the newest line is kept"
        );
        assert!(log.text().starts_with("line "), "cut at a line boundary");
        assert!(!log.text().contains("line 000"), "the oldest lines go");
        // A line longer than the bound is kept alone, cut to it.
        log.push_line(&"x".repeat(300));
        assert_eq!(log.text(), format!("{}\n", "x".repeat(99)));
        // A line that just fits evicts the older lines and is kept whole.
        log.push_line("short");
        log.push_line(&"y".repeat(99));
        assert_eq!(log.text(), format!("{}\n", "y".repeat(99)));
    }
}
