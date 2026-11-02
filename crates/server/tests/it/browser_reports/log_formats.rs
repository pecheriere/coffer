//! A report cannot forge log lines in either format of the real binary. Each
//! test runs `coffer serve` as its own process, logging as in production.

use std::io::{BufRead as _, BufReader};
use std::net::SocketAddr;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use coffer_server::assets::JS_COFFER_JS;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

const TIME_LIMIT: Duration = Duration::from_secs(15);

/// Text meant to break out of a log line in every way a log format allows.
const HOSTILE: &str = "hostile-start\r\n2030-01-01T00:00:00Z ERROR forged line\n\
                       \u{1b}[31mred\u{1b}[0m\u{7}\"} {\"level\":\"ERROR\",\"fields\":{\"message\":\"forged json\"}}\\\
                       \u{2028}kind=\"forged\" hostile-end";

/// What is left of [`HOSTILE`]: the newlines and line separator became
/// spaces, the escape and bell characters are gone.
const CLEANED: &str = "hostile-start 2030-01-01T00:00:00Z ERROR forged line \
                       [31mred[0m\"} {\"level\":\"ERROR\",\"fields\":{\"message\":\"forged json\"}}\\ \
                       kind=\"forged\" hostile-end";

/// The real server, killed on drop, and the lines it logged so far.
struct Serving {
    child: Child,
    lines: Arc<Mutex<Vec<String>>>,
    _dir: tempfile::TempDir,
}

impl Drop for Serving {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Serving {
    fn start(env: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(
            dir.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_coffer"))
            .arg("serve")
            .env_clear()
            .env("COFFER_PUBLIC_URL", "https://coffer.test")
            .env("COFFER_DATA_DIR", dir.path())
            .env("COFFER_LISTEN", "127.0.0.1:0")
            .envs(env.iter().copied())
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let stderr = child.stderr.take().unwrap();
        let lines = Arc::new(Mutex::new(Vec::new()));
        let filling = Arc::clone(&lines);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                filling.lock().unwrap().push(line);
            }
        });
        Self {
            child,
            lines,
            _dir: dir,
        }
    }

    fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }

    /// Waits for a line holding `text`.
    async fn wait_for(&self, text: &str) -> String {
        let started = Instant::now();
        loop {
            if let Some(line) = self.lines().into_iter().find(|line| line.contains(text)) {
                return line;
            }
            assert!(
                started.elapsed() < TIME_LIMIT,
                "no line with {text}: {:?}",
                self.lines()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// The address in the server's "coffer is serving" line.
fn listen_address(line: &str) -> SocketAddr {
    let from = line.find("127.0.0.1:").expect("the listen address");
    let rest = &line[from..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != ':')
        .unwrap_or(rest.len());
    rest[..end].parse().unwrap()
}

/// Posts `body` from the public origin; returns the status line.
async fn post(addr: SocketAddr, body: &str) -> String {
    let request = format!(
        "POST /browser-reports HTTP/1.1\r\nHost: localhost\r\nOrigin: https://coffer.test\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).await.unwrap();
    reply.lines().next().unwrap_or_default().to_owned()
}

/// Serves with `env`, posts the hostile report and returns the server's whole
/// log, ending with the report's event.
async fn hostile_log(env: &[(&str, &str)]) -> Vec<String> {
    let serving = Serving::start(env);
    let addr = listen_address(&serving.wait_for("coffer is serving").await);
    let body = json!({
        "kind": "error",
        "message": HOSTILE,
        "script": JS_COFFER_JS.url,
        "page": "/login",
    })
    .to_string();
    assert_eq!(post(addr, &body).await, "HTTP/1.1 204 No Content");
    serving.wait_for("hostile-start").await;
    serving.lines()
}

#[tokio::test]
async fn hostile_text_stays_inside_one_text_line() {
    let lines = hostile_log(&[]).await;
    let event: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains("hostile"))
        .collect();
    assert_eq!(event.len(), 1, "{lines:#?}");
    assert!(event[0].contains(" WARN "), "{}", event[0]);
    assert!(event[0].contains("hostile-end"), "{}", event[0]);
    // The quotes it holds are escaped, so a field cannot be forged either.
    assert!(event[0].contains(r#"\"forged\""#), "{}", event[0]);
    assert!(
        !lines.iter().any(|line| line.starts_with("2030-")),
        "{lines:#?}"
    );
    let whole = lines.join("\n");
    assert!(!whole.contains('\u{1b}') && !whole.contains('\u{7}') && !whole.contains('\r'));
    assert!(!whole.contains('\u{2028}'));
}

#[tokio::test]
async fn hostile_text_stays_inside_one_json_line() {
    let lines = hostile_log(&[("COFFER_LOG_FORMAT", "json")]).await;
    let events: Vec<Value> = lines
        .iter()
        .map(|line| serde_json::from_str(line).expect("every line is one JSON document"))
        .collect();
    let reports: Vec<&Value> = events
        .iter()
        .filter(|event| event["fields"]["trust"] == json!("untrusted"))
        .collect();
    assert_eq!(reports.len(), 1, "{lines:#?}");
    assert_eq!(reports[0]["level"], json!("WARN"));
    assert_eq!(reports[0]["fields"]["kind"], json!("error"));
    assert_eq!(
        reports[0]["fields"]["message"],
        json!("browser report (client-reported, untrusted)")
    );
    assert_eq!(reports[0]["fields"]["client_message"], json!(CLEANED));
    let forged = events
        .iter()
        .filter(|event| event["level"] == json!("ERROR"))
        .count();
    assert_eq!(forged, 0, "{lines:#?}");
}

/// This process's whole log, shared with no other test, shows that what goes
/// over the limit adds no line at all.
#[tokio::test]
async fn drops_are_not_logged_one_by_one() {
    let serving = Serving::start(&[]);
    let addr = listen_address(&serving.wait_for("coffer is serving").await);
    let before = serving.lines().len();
    let body = json!({
        "kind": "error",
        "message": "quiet-report",
        "script": JS_COFFER_JS.url,
        "page": "/login",
    })
    .to_string();
    for _ in 0..12 {
        post(addr, &body).await;
    }
    // After a report's own line, nothing could still be on its way but drops.
    let last = json!({ "kind": "plaid", "message": "x" }).to_string();
    post(addr, &last).await;
    let started = Instant::now();
    while serving.lines().len() < before + 5 && started.elapsed() < TIME_LIMIT {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let lines = serving.lines();
    assert_eq!(lines.len(), before + 5, "{lines:#?}");
}
