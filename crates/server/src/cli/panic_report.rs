//! The process's panic hook: a panic becomes one error event with where it
//! happened and a backtrace, in the configured log format.
//!
//! The panic's message is never read, because it can hold anything and logs
//! carry no financial data. So nothing here prints it, and neither does the
//! default hook this replaces. A backtrace holds only code locations, and a
//! location is a source file and line fixed at compile time.
//!
//! The hook runs while panicking, where a second panic aborts the process, so
//! everything in it is infallible or has its error ignored.

use std::backtrace::Backtrace;
use std::io::Write as _;
use std::panic::Location;

use tracing::Level;

/// Replaces the process's panic hook. The binary calls it before anything else;
/// [`super::run`] does not, because tests drive that in-process, where the
/// hook would swallow the failure messages of every test beside them.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| report(info.location())));
}

fn report(location: Option<&Location<'_>>) {
    // Whatever `RUST_BACKTRACE` says: a panic is rare, and it may not recur.
    let backtrace = Backtrace::force_capture();
    let location = location.map_or_else(|| "unknown".to_owned(), ToString::to_string);
    // False with no subscriber yet (the commands other than `serve` set up
    // none) and with logs filtered out. A panic inside the global subscriber
    // is not one of these: tracing has no re-entry guard for a global
    // dispatcher, so this event goes back into it, and a second panic there
    // aborts the process.
    if tracing::enabled!(Level::ERROR) {
        tracing::error!(%location, %backtrace, "coffer panicked");
    } else {
        // Nothing sensible is left to do if stderr is gone.
        let _ = writeln!(
            std::io::stderr(),
            "coffer panicked at {location}\n{backtrace}"
        );
    }
}
