//! How background tasks stay up: each runs supervised in a task of its own, so
//! a panic is logged, touches nothing else and starts it again. The log says a
//! panic happened and never what it said, since a [`JoinError`]'s text holds
//! the panic's message (the panic hook has logged where it was).
//!
//! [`JoinError`]: tokio::task::JoinError

use std::future::Future;
use std::time::Duration;

use tokio::sync::watch;

/// The first and the longest wait after an error.
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Runs what `start` makes in its own task until it ends without a panic, and
/// returns its result. After a panic, logs it, waits and starts it again with
/// `restarted` true. `None` if told to stop during that wait.
pub(crate) async fn supervise<S, F>(
    task: &'static str,
    mut stop: watch::Receiver<bool>,
    start: S,
) -> Option<F::Output>
where
    S: Fn(bool) -> F,
    F: Future<Output: Send + 'static> + Send + 'static,
{
    let mut backoff = Backoff::default();
    let mut restarted = false;
    loop {
        match tokio::spawn(start(restarted)).await {
            Ok(result) => return Some(result),
            Err(err) => {
                tracing::error!(
                    task,
                    panicked = err.is_panic(),
                    "a background task ended abnormally; starting it again"
                );
                if !pause(&mut stop, backoff.next()).await {
                    return None;
                }
                restarted = true;
            }
        }
    }
}

/// Resolves once the task is told to stop, or its sender is gone.
pub(crate) async fn stopped(stop: &mut watch::Receiver<bool>) {
    // An error means the sender is gone: stop as well.
    let _ = stop.wait_for(|stop| *stop).await;
}

/// Waits `delay`; `false` if the task was told to stop meanwhile.
pub(crate) async fn pause(stop: &mut watch::Receiver<bool>, delay: Duration) -> bool {
    tokio::select! {
        () = stopped(stop) => false,
        () = tokio::time::sleep(delay) => true,
    }
}

/// Doubling waits after consecutive errors, from [`BACKOFF_START`] up to
/// [`BACKOFF_MAX`].
#[derive(Debug, Default)]
pub(crate) struct Backoff {
    next: Option<Duration>,
}

impl Backoff {
    /// The wait after one more error.
    pub(crate) fn next(&mut self) -> Duration {
        let delay = self.next.map_or(BACKOFF_START, |delay| {
            delay.saturating_mul(2).min(BACKOFF_MAX)
        });
        self.next = Some(delay);
        delay
    }

    pub(crate) fn reset(&mut self) {
        self.next = None;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tokio::sync::watch;

    use super::{BACKOFF_MAX, BACKOFF_START, Backoff, supervise};

    #[tokio::test]
    async fn supervise_starts_a_panicked_task_again_and_says_so() {
        let (_stop, stopped) = watch::channel(false);
        let starts = Mutex::new(Vec::new());
        let result = supervise("test", stopped, |restarted| {
            let first = {
                let mut starts = starts.lock().unwrap();
                starts.push(restarted);
                starts.len() == 1
            };
            async move {
                assert!(!first, "scripted panic");
                "finished"
            }
        })
        .await;
        assert_eq!(result, Some("finished"));
        assert_eq!(*starts.lock().unwrap(), [false, true]);
    }

    #[tokio::test]
    async fn supervise_gives_up_when_told_to_stop_while_waiting() {
        let (stop, stopped) = watch::channel(false);
        stop.send(true).unwrap();
        let result = supervise("test", stopped, |_| async {
            panic!("scripted panic");
        })
        .await;
        assert_eq!(result, None);
    }

    #[test]
    fn backoff_doubles_up_to_the_maximum_and_resets() {
        let mut backoff = Backoff::default();
        assert_eq!(backoff.next(), BACKOFF_START);
        assert_eq!(backoff.next(), BACKOFF_START * 2);
        for _ in 0..10 {
            backoff.next();
        }
        assert_eq!(backoff.next(), BACKOFF_MAX);
        backoff.reset();
        assert_eq!(backoff.next(), BACKOFF_START);
    }
}
