//! What a run reports: logs without credentials, the run view and traces.

use super::{PLAINTEXT, SECRET_PART, fetched, provider_account, txn, world};
use coffer_domain::accounts::ConnectionLabel;
use coffer_domain::ids::{ConnectionId, SyncRunId};
use coffer_domain::sync::{SyncRunStatus, SyncTrigger};
use coffer_server::sync::provider::{ProviderError, ProviderKind};
use coffer_server::testkit::{TestServer, captured_log_text};
use coffer_store::{AuditFilter, PageRequest};
use secrecy::ExposeSecret as _;

#[tokio::test]
async fn credentials_never_logged() {
    let world = world().await;
    world.fake.push(Ok(fetched(vec![provider_account(
        "acct-1",
        "USD",
        world.days_ago(90),
        Vec::new(),
    )])));
    // A provider error quoting the credentials, as a careless one might.
    world.fake.push(Err(ProviderError::Invalid(format!(
        "request to {PLAINTEXT} failed"
    ))));

    let succeeded = world.sync().await;
    let failed = world.sync().await;
    assert_eq!(failed.status, SyncRunStatus::Failed);

    let requests = world.fake.requests();
    assert!(
        requests
            .iter()
            .all(|request| request.credentials.expose_secret() == PLAINTEXT),
        "the provider receives the opened credentials"
    );

    let logs = captured_log_text();
    assert!(
        logs.contains(&world.connection.to_string()),
        "the log captured the sync of the connection"
    );
    assert!(
        !logs.contains(SECRET_PART),
        "the credentials are never logged"
    );

    let connection = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    let last_error = connection.last_error.unwrap_or_default();
    assert!(
        !last_error.contains(SECRET_PART),
        "last_error lacks the credentials"
    );
    assert!(
        !connection
            .credentials
            .unwrap()
            .windows(SECRET_PART.len())
            .any(|window| window == SECRET_PART.as_bytes()),
        "the stored credentials are sealed"
    );
    for run in [succeeded, failed] {
        let detail = run.detail.clone().unwrap_or_default();
        assert!(
            !detail.contains(SECRET_PART),
            "run detail lacks the credentials"
        );
        assert!(
            !format!("{run:?}").contains(SECRET_PART),
            "run view lacks the credentials"
        );
    }
    let audit = world
        .server
        .store
        .list_audit(&AuditFilter::default(), &PageRequest::default())
        .await
        .unwrap();
    assert!(
        audit
            .items
            .iter()
            .all(|event| !format!("{event:?}").contains(SECRET_PART)),
        "the audit log lacks the credentials"
    );
}

#[tokio::test]
async fn run_view_reports_attribution_and_records() {
    let world = world().await;
    world.fake.push(Ok(fetched(vec![provider_account(
        "acct-1",
        "USD",
        world.days_ago(90),
        vec![
            txn("t-1", world.days_ago(1), "-1"),
            txn("", world.days_ago(1), "-1"),
        ],
    )])));
    let run = world.sync().await;
    assert_eq!(run.connection, world.connection);
    assert_eq!(run.trigger, SyncTrigger::Manual);
    let requested_by = run.requested_by.clone().unwrap();
    assert_eq!(requested_by.member, Some(world.alice.member));
    assert_eq!(requested_by.agent, None);
    assert!(run.started_at.is_some() && run.finished_at.is_some());
    assert_eq!(
        run.status,
        SyncRunStatus::Partial,
        "a skipped malformed record makes the run partial"
    );
    let detail = run.detail.clone().unwrap();
    assert!(
        detail.contains("1 skipped"),
        "the detail counts skipped records"
    );
    let stored = world.server.store.get_sync_run(run.id).await.unwrap();
    assert_eq!(
        (stored.status, stored.detail.as_deref(), stored.finished_at),
        (run.status, Some(detail.as_str()), run.finished_at)
    );
    let connection = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(connection.provider, ProviderKind::SimpleFin.as_str());
    assert_eq!(
        connection.label,
        ConnectionLabel::parse("Family bank").unwrap()
    );

    assert!(
        world.server.sync.run(run.id).await.is_err(),
        "a finished run does not run again"
    );
    assert!(world.server.sync.run(SyncRunId::new()).await.is_err());
}

#[tokio::test]
async fn invalid_provider_account_id_leaves_a_trace() {
    let world = world().await;
    world.fake.push(Ok(fetched(vec![
        provider_account("", "USD", world.days_ago(90), Vec::new()),
        provider_account("acct-ok", "USD", world.days_ago(90), Vec::new()),
    ])));
    let run = world.sync().await;
    assert_eq!(run.status, SyncRunStatus::Partial);
    let detail = run.detail.unwrap();
    assert!(
        detail.starts_with("2 accounts, 1 with errors"),
        "the run counts the rejected account"
    );
    let logs = captured_log_text();
    let connection = world.connection.to_string();
    assert!(
        logs.lines().any(|line| {
            line.starts_with("WARN")
                && line.contains("provider account with an invalid id skipped")
                && line.contains(&connection)
        }),
        "a warning names the connection"
    );
}

#[tokio::test]
async fn captured_logs_include_span_fields() {
    let _server = TestServer::new().await;
    let marker = format!("span-marker-{}", ConnectionId::new());
    let span = tracing::info_span!("probe", secret = %marker, later = tracing::field::Empty);
    let recorded = format!("recorded-marker-{}", ConnectionId::new());
    span.record("later", recorded.as_str());
    let logs = captured_log_text();
    assert!(logs.contains(&marker), "span fields are captured");
    assert!(
        logs.contains(&recorded),
        "fields recorded later are captured"
    );
}
