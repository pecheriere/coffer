//! The `coffer` commands, run in-process against a temporary data directory.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::collections::HashMap;
use std::ffi::OsString;
use std::process::ExitCode;

use coffer_domain::identity::{Actor, Role, Username, Via};
use coffer_server::cli;
use coffer_server::testkit::{TestServer, capture_logs, captured_log_text};

struct Outcome {
    code: ExitCode,
    out: String,
    err: String,
}

async fn run(args: &[&str], env: &HashMap<&str, String>) -> Outcome {
    let args: Vec<OsString> = std::iter::once("coffer")
        .chain(args.iter().copied())
        .map(OsString::from)
        .collect();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = cli::run(args, |name| env.get(name).cloned(), &mut out, &mut err).await;
    Outcome {
        code,
        out: String::from_utf8(out).unwrap(),
        err: String::from_utf8(err).unwrap(),
    }
}

/// The environment for a data directory in `dir`, which is made private
/// (mode 0700) as coffer requires; `tempfile` creates it with the umask.
fn env(dir: &tempfile::TempDir) -> HashMap<&'static str, String> {
    #[cfg(unix)]
    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    HashMap::from([
        ("COFFER_PUBLIC_URL", "https://coffer.test".to_owned()),
        ("COFFER_DATA_DIR", dir.path().to_str().unwrap().to_owned()),
    ])
}

#[tokio::test]
async fn bootstrap_link_cli_exit_codes() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(&dir);

    let first = run(&["bootstrap-link"], &env).await;
    assert_eq!(first.code, ExitCode::SUCCESS);
    assert!(first.out.trim().starts_with("https://coffer.test/setup/"));
    assert_eq!(first.err, "");

    // Enroll the first admin through the same data directory.
    let server = TestServer::open_at(dir.path()).await;
    let link = url::Url::parse(first.out.trim()).unwrap();
    server.authenticator().enroll(&link, "alice", "Alice").await;

    let second = run(&["bootstrap-link"], &env).await;
    assert_eq!(second.code, ExitCode::from(1));
    assert_eq!(second.out, "");
    assert!(second.err.contains("an admin already exists"));
}

#[tokio::test]
async fn recover_admin_prints_a_reenroll_link() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(&dir);
    let server = TestServer::open_at(dir.path()).await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    server.authenticator().enroll(&link, "alice", "Alice").await;
    let admin = Actor {
        member: server
            .store
            .get_member_by_username(&Username::parse("alice").unwrap())
            .await
            .unwrap()
            .id,
        role: Role::Admin,
        via: Via::Session,
    };
    let invite = server
        .identity
        .create_invite(&admin, Role::Member, None)
        .await
        .unwrap();
    server.authenticator().enroll(&invite, "bob", "Bob").await;

    let ok = run(&["recover-admin", "alice"], &env).await;
    assert_eq!(ok.code, ExitCode::SUCCESS);
    assert!(ok.out.trim().starts_with("https://coffer.test/invite/"));

    let not_admin = run(&["recover-admin", "bob"], &env).await;
    assert_eq!(not_admin.code, ExitCode::from(1));
    assert_eq!(not_admin.out, "");
    let unknown = run(&["recover-admin", "nobody"], &env).await;
    assert_eq!(unknown.code, ExitCode::from(1));
}

fn staging_leftovers(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with(".coffer-backup-"))
        .collect()
}

#[tokio::test]
async fn backup_cli_writes_file() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(&dir);
    let dest_dir = tempfile::tempdir().unwrap();
    let dest = dest_dir.path().join("coffer-backup.db");

    // A household with one admin.
    let server = TestServer::open_at(dir.path()).await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    server.authenticator().enroll(&link, "alice", "Alice").await;

    let done = run(&["backup", dest.to_str().unwrap()], &env).await;

    assert_eq!(done.code, ExitCode::SUCCESS);
    #[cfg(unix)]
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&dest).unwrap().permissions())
            & 0o777,
        0o600
    );
    // The directory holds only the backup: no staging leftovers.
    assert_eq!(std::fs::read_dir(dest_dir.path()).unwrap().count(), 1);

    // The backup is a usable database with the household in it.
    let restored = coffer_store::Store::open(&dest).await.unwrap();
    let members = restored.list_members().await.unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].username.as_str(), "alice");
    assert!(restored.any_admin_exists().await.unwrap());
    let audit = restored
        .list_audit(
            &coffer_store::AuditFilter::default(),
            &coffer_store::PageRequest::default(),
        )
        .await
        .unwrap();
    assert!(
        audit
            .items
            .iter()
            .any(|event| event.action == "member.create")
    );
}

#[tokio::test]
async fn backup_refuses_existing_dest_and_leaves_no_staging_dir() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(&dir);
    let dest_dir = tempfile::tempdir().unwrap();
    let dest = dest_dir.path().join("existing.db");
    std::fs::write(&dest, b"precious").unwrap();
    assert_eq!(run(&["bootstrap-link"], &env).await.code, ExitCode::SUCCESS);

    let refused = run(&["backup", dest.to_str().unwrap()], &env).await;

    assert_eq!(refused.code, ExitCode::from(1));
    assert!(refused.err.contains("already exists"));
    assert_eq!(std::fs::read(&dest).unwrap(), b"precious");
    assert_eq!(staging_leftovers(dest_dir.path()), Vec::<String>::new());
}

#[tokio::test]
async fn backup_without_a_database_fails_and_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(&dir);
    let dest_dir = tempfile::tempdir().unwrap();
    let dest = dest_dir.path().join("backup.db");

    let failed = run(&["backup", dest.to_str().unwrap()], &env).await;

    assert_eq!(failed.code, ExitCode::from(1));
    assert!(failed.err.contains("no database"));
    assert!(!dest.exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(dest_dir.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn bad_config_and_usage_are_exit_code_2_and_never_echo_values() {
    let dir = tempfile::tempdir().unwrap();
    let mut env = env(&dir);
    env.insert("COFFER_PUBLIC_URL", "http://hunter2.example".to_owned());

    let bad = run(&["bootstrap-link"], &env).await;
    assert_eq!(bad.code, ExitCode::from(2));
    assert!(bad.err.contains("COFFER_PUBLIC_URL"));
    assert!(!bad.err.contains("hunter2"));

    let usage = run(&["frobnicate"], &env).await;
    assert_eq!(usage.code, ExitCode::from(2));
}

#[cfg(unix)]
fn mode(path: &std::path::Path) -> u32 {
    std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(path).unwrap().permissions()) & 0o777
}

#[cfg(unix)]
#[tokio::test]
async fn commands_refuse_a_data_dir_open_to_group_or_others() {
    let dir = tempfile::tempdir().unwrap();
    let mut env = env(&dir);
    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o750),
    )
    .unwrap();
    env.insert("COFFER_LISTEN", "127.0.0.1:0".to_owned());
    let dest_dir = tempfile::tempdir().unwrap();
    let dest = dest_dir.path().join("backup.db");
    let shown = dir.path().display().to_string();

    for args in [
        vec!["bootstrap-link"],
        vec!["recover-admin", "alice"],
        vec!["backup", dest.to_str().unwrap()],
        vec!["serve"],
    ] {
        let refused = tokio::time::timeout(std::time::Duration::from_secs(10), run(&args, &env))
            .await
            .unwrap_or_else(|_| panic!("{args:?} did not refuse the data directory"));
        assert_eq!(refused.code, ExitCode::from(1), "{args:?}");
        assert!(refused.err.contains(&shown), "{args:?}");
        assert!(refused.err.contains("chmod 700"), "{args:?}");
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    assert!(!dest.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn data_dir_is_created_private_and_the_database_is_owner_only() {
    let parent = tempfile::tempdir().unwrap();
    let data_dir = parent.path().join("data");
    let env = HashMap::from([
        ("COFFER_PUBLIC_URL", "https://coffer.test".to_owned()),
        ("COFFER_DATA_DIR", data_dir.to_str().unwrap().to_owned()),
    ]);

    let done = run(&["bootstrap-link"], &env).await;

    assert_eq!(done.code, ExitCode::SUCCESS);
    assert_eq!(mode(&data_dir), 0o700);
    assert_eq!(mode(&data_dir.join("coffer.db")), 0o600);
    for name in ["coffer.db-wal", "coffer.db-shm"] {
        let path = data_dir.join(name);
        if path.exists() {
            assert_eq!(mode(&path), 0o600, "{name}");
        }
    }
}

/// A malformed `RUST_LOG` stops `serve` as bad configuration instead of
/// silently logging at another level, and the value is never echoed.
#[tokio::test]
async fn invalid_rust_log_fails_startup() {
    let dir = tempfile::tempdir().unwrap();
    let mut env = env(&dir);
    env.insert("RUST_LOG", "coffer_server=loudest[".to_owned());
    env.insert("COFFER_LISTEN", "127.0.0.1:0".to_owned());

    let refused = tokio::time::timeout(std::time::Duration::from_secs(10), run(&["serve"], &env))
        .await
        .unwrap_or_else(|_| panic!("serve did not refuse the filter"));
    assert_eq!(refused.code, ExitCode::from(2));
    assert!(refused.err.contains("RUST_LOG"), "names the variable");
    assert!(!refused.err.contains("loudest"), "never echoes the value");
}

/// When the process already installed a subscriber (as this test harness
/// does), `serve` keeps it and says so at debug level.
#[cfg(unix)]
#[tokio::test]
async fn serve_keeps_an_installed_subscriber_and_says_so() {
    capture_logs();
    let dir = tempfile::tempdir().unwrap();
    let mut env = env(&dir);
    // A data directory open to others makes serve stop right after logging
    // is set up.
    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o750),
    )
    .unwrap();
    env.insert("COFFER_LISTEN", "127.0.0.1:0".to_owned());
    let refused = tokio::time::timeout(std::time::Duration::from_secs(10), run(&["serve"], &env))
        .await
        .unwrap_or_else(|_| panic!("serve did not stop"));
    assert_eq!(refused.code, ExitCode::from(1));
    assert!(
        captured_log_text().contains("an installed tracing subscriber is kept"),
        "the kept subscriber is reported"
    );
}
