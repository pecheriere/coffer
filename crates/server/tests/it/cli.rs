//! The `coffer` commands, run in-process against a temporary data directory.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::collections::HashMap;
use std::ffi::OsString;
use std::process::ExitCode;

use coffer_domain::accounts::{AccountKind, AccountName, BalanceSource, BelongsTo, LiquidityClass};
use coffer_domain::identity::{Actor, Role, Username, Via};
use coffer_domain::ids::AccountId;
use coffer_domain::money::Currency;
use coffer_domain::time::{Clock as _, SystemClock};
use coffer_server::cli;
use coffer_server::testkit::{TestServer, capture_logs, captured_log_text};
use coffer_store::{BalanceSnapshot, NewAccount, Store};
use jiff::civil::{Date, date};

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

/// A household with a day's history, in a database nothing has rebuilt.
async fn populated_database(dir: &tempfile::TempDir) -> (Store, AccountId, Date) {
    let store = Store::open(&dir.path().join("coffer.db")).await.unwrap();
    let first = date(2026, 1, 1);
    let id = holding(&store).await;
    snapshot(&store, id, first).await;
    (store, id, first)
}

async fn holding(store: &Store) -> AccountId {
    let id = AccountId::new();
    let mut tx = store.begin().await.unwrap();
    tx.insert_account(NewAccount {
        id,
        name: AccountName::parse("Holding").unwrap(),
        kind: AccountKind::Checking,
        currency: Currency::parse("USD").unwrap(),
        liquidity: LiquidityClass::Cash,
        liquidity_overridden: false,
        include_in_net_worth: true,
        include_in_forecast: true,
        created_at: SystemClock.now(),
        belongs_to: BelongsTo::Household,
        belongs_to_overridden: false,
        beneficiary: None,
    })
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

async fn snapshot(store: &Store, account: AccountId, on: Date) {
    let mut tx = store.begin().await.unwrap();
    tx.upsert_balance(BalanceSnapshot {
        account,
        on,
        balance_minor: 100,
        source: BalanceSource::Manual,
        recorded_at: SystemClock.now(),
    })
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn rebuild_net_worth_reports_what_it_wrote() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(&dir);
    let (store, account, first) = populated_database(&dir).await;

    let done = run(&["rebuild-net-worth"], &env).await;

    assert_eq!(done.code, ExitCode::SUCCESS);
    let state = store.net_worth_state().await.unwrap();
    assert!(!state.rebuild_needed);
    let last = state.filled_through.unwrap();
    let rows = store.daily_values_for_tests().await.unwrap();
    assert_eq!(
        rows.iter().filter(|row| row.account == account).count(),
        rows.len()
    );
    assert_eq!(rows.first().unwrap().on, first);
    assert_eq!(rows.last().unwrap().on, last);
    let count = rows.len();
    assert_eq!(
        done.out,
        format!("net worth rebuilt through {last}: {count} rows written\n")
    );
    assert_eq!(done.err, "");
}

/// Two zones 25 hours apart are never on the same day.
#[tokio::test]
async fn rebuild_net_worth_uses_the_household_s_day() {
    let dir = tempfile::tempdir().unwrap();
    let mut env = env(&dir);
    let (store, _account, _first) = populated_database(&dir).await;

    env.insert("COFFER_TIMEZONE", "Pacific/Kiritimati".to_owned());
    assert_eq!(
        run(&["rebuild-net-worth"], &env).await.code,
        ExitCode::SUCCESS
    );
    let east = store
        .net_worth_state()
        .await
        .unwrap()
        .filled_through
        .unwrap();
    env.insert("COFFER_TIMEZONE", "Pacific/Pago_Pago".to_owned());
    assert_eq!(
        run(&["rebuild-net-worth"], &env).await.code,
        ExitCode::SUCCESS
    );
    let west = store
        .net_worth_state()
        .await
        .unwrap()
        .filled_through
        .unwrap();

    assert!(west < east, "{west} is not before {east}");
}

#[tokio::test]
async fn rebuild_net_worth_never_creates_a_database() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(&dir);

    let failed = run(&["rebuild-net-worth"], &env).await;

    assert_eq!(failed.code, ExitCode::from(1));
    assert!(failed.err.contains("no database"));
    assert_eq!(failed.out, "");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

/// Beside a running server it queues behind a writer, and so sees what that
/// writer committed: it can't lose a change or read half of one.
#[tokio::test]
async fn rebuild_net_worth_waits_for_a_writer() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(&dir);
    let (store, _account, first) = populated_database(&dir).await;
    let later = holding(&store).await;
    let mut writer = store.begin().await.unwrap();
    writer
        .upsert_balance(BalanceSnapshot {
            account: later,
            on: first,
            balance_minor: 7,
            source: BalanceSource::Manual,
            recorded_at: SystemClock.now(),
        })
        .await
        .unwrap();

    let command = run(&["rebuild-net-worth"], &env);
    tokio::pin!(command);
    tokio::select! {
        _ = &mut command => panic!("the command did not wait"),
        () = tokio::time::sleep(std::time::Duration::from_millis(300)) => {}
    }
    writer.commit().await.unwrap();

    assert_eq!(command.await.code, ExitCode::SUCCESS);
    let rows = store.daily_values_for_tests().await.unwrap();
    assert!(
        rows.iter()
            .any(|row| row.account == later && row.balance_minor == 7)
    );
}

/// Waiting with no end in sight says what for, and then does the work.
#[tokio::test]
async fn rebuild_net_worth_says_it_is_waiting_for_another() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(&dir);
    let (store, _account, _first) = populated_database(&dir).await;
    let held = store.hold_net_worth_rebuild_for_tests().unwrap().unwrap();

    let command = run(&["rebuild-net-worth"], &env);
    tokio::pin!(command);
    tokio::select! {
        _ = &mut command => panic!("the command did not wait"),
        () = tokio::time::sleep(std::time::Duration::from_millis(300)) => {}
    }
    drop(held);

    let done = command.await;
    assert_eq!(done.code, ExitCode::SUCCESS);
    let (waiting, result) = done.out.split_once('\n').unwrap();
    assert_eq!(waiting, "waiting for another net worth rebuild to finish");
    assert!(result.starts_with("net worth rebuilt through "), "{result}");
    assert!(!store.net_worth_state().await.unwrap().rebuild_needed);
}

/// An empty file isn't a database: opening it would migrate it into an empty one,
/// and a backup or a rebuild of that would read as if all were well.
#[tokio::test]
async fn an_empty_database_file_is_refused() {
    let dest_dir = tempfile::tempdir().unwrap();
    let dest = dest_dir.path().join("backup.db");
    let commands = [
        vec!["rebuild-net-worth"],
        vec!["backup", dest.to_str().unwrap()],
    ];
    for command in commands {
        let dir = tempfile::tempdir().unwrap();
        let env = env(&dir);
        let database = dir.path().join("coffer.db");
        std::fs::write(&database, b"").unwrap();

        let failed = run(&command, &env).await;

        assert_eq!(failed.code, ExitCode::from(1), "{command:?}");
        assert!(failed.err.contains("empty"), "{command:?}");
        assert_eq!(
            std::fs::metadata(&database).unwrap().len(),
            0,
            "{command:?}"
        );
        assert!(!dest.exists(), "{command:?}");
    }
}
