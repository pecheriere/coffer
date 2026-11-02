//! A loopback mock of a provider's upstream.

use std::fmt;
use std::sync::{Arc, Mutex};

use url::Url;

use super::lock;

/// One request a [`MockUpstream`] received. Whether it carried an
/// `Authorization` header is recorded, never its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRequest {
    pub method: axum::http::Method,
    pub path: String,
    pub query: Option<String>,
    pub authorization: bool,
}

/// A provider's upstream, mocked: `router` served over plain HTTP on
/// `127.0.0.1` at an ephemeral port, recording every request. Stops when
/// dropped.
pub struct MockUpstream {
    /// `http://127.0.0.1:<port>/`.
    pub base: Url,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    task: tokio::task::JoinHandle<()>,
}

impl fmt::Debug for MockUpstream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MockUpstream")
            .field("base", &self.base.as_str())
            .finish_non_exhaustive()
    }
}

impl MockUpstream {
    /// Starts serving `router`.
    ///
    /// # Panics
    ///
    /// If no loopback port can be bound.
    pub async fn start(router: axum::Router) -> MockUpstream {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a loopback port");
        let addr = listener.local_addr().expect("bound address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&requests);
        let app = router.layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                lock(&recorder).push(RecordedRequest {
                    method: request.method().clone(),
                    path: request.uri().path().to_owned(),
                    query: request.uri().query().map(str::to_owned),
                    authorization: request
                        .headers()
                        .contains_key(axum::http::header::AUTHORIZATION),
                });
                next.run(request)
            },
        ));
        let task = tokio::spawn(async move {
            if axum::serve(listener, app).await.is_err() {
                tracing::warn!("the mock upstream stopped");
            }
        });
        MockUpstream {
            base: Url::parse(&format!("http://{addr}/")).expect("loopback URL"),
            requests,
            task,
        }
    }

    /// Every request received so far, oldest first.
    #[must_use]
    pub fn requests(&self) -> Vec<RecordedRequest> {
        lock(&self.requests).clone()
    }
}

impl Drop for MockUpstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}
