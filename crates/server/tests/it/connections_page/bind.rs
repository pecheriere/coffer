//! Binding provider accounts from the page: a form to create the account
//! and one to link an existing account, both through the service.

use axum::http::{StatusCode, header};
use coffer_domain::accounts::AccountKind;
use coffer_domain::identity::Role;
use coffer_domain::ids::{AccountId, ConnectionId, ProviderAccountId};
use coffer_domain::money::Currency;
use coffer_domain::policy::AccountScope;
use coffer_domain::time::Clock as _;
use coffer_server::ledger::accounts::NewAccountInput;
use coffer_server::testkit::browser::Browser;
use coffer_store::{
    ConnectionStatus, ConnectionStatusChange, PageRequest, ProviderAccountRecord,
    ProviderAccountUpsert, TransactionFilter,
};

use super::{World, assert_inline_free, world};

pub(super) fn create_path(id: ProviderAccountId) -> String {
    format!("/connections/provider-accounts/{id}/create-account")
}

pub(super) fn bind_path(id: ProviderAccountId) -> String {
    format!("/connections/provider-accounts/{id}/bind")
}

impl World {
    pub(super) async fn provider_account(&self, name: &str) -> ProviderAccountRecord {
        self.server
            .store
            .list_provider_accounts(None)
            .await
            .unwrap()
            .into_iter()
            .find(|record| record.name == name)
            .unwrap()
    }

    /// Reports one more provider account of `connection`, as a sync does.
    pub(super) async fn report(
        &self,
        connection: ConnectionId,
        name: &str,
        currency: &str,
        kind_hint: Option<AccountKind>,
    ) -> ProviderAccountRecord {
        let mut tx = self.server.store.begin().await.unwrap();
        let record = tx
            .upsert_provider_account(ProviderAccountUpsert {
                connection,
                provider_account_id: format!("ext-{name}"),
                name: name.into(),
                institution: None,
                currency_raw: currency.into(),
                kind_hint,
                balance: None,
                seen_at: self.server.clock.now(),
            })
            .await
            .unwrap();
        tx.commit().await.unwrap();
        record
    }

    pub(super) async fn manual_account(&self, name: &str, currency: &str) -> AccountId {
        self.server
            .ledger
            .create_account(
                &self.alice,
                NewAccountInput {
                    name: name.into(),
                    kind: AccountKind::Savings,
                    currency: Currency::parse(currency).unwrap(),
                    liquidity: None,
                    include_in_net_worth: true,
                    include_in_forecast: true,
                    opening_balance: None,
                },
                None,
            )
            .await
            .unwrap()
            .id
    }

    async fn amounts(&self, account: AccountId) -> Vec<i64> {
        let mut amounts: Vec<i64> = self
            .server
            .store
            .list_transactions(
                &TransactionFilter {
                    scope: AccountScope::All,
                    accounts: vec![account],
                    from: None,
                    to: None,
                    review: None,
                    assignee: None,
                    categories: Vec::new(),
                    uncategorized: false,
                    pending: None,
                    flag: None,
                    min_minor: None,
                    max_minor: None,
                    text: None,
                },
                &PageRequest::default(),
            )
            .await
            .unwrap()
            .items
            .iter()
            .map(|record| record.amount_minor)
            .collect();
        amounts.sort_unstable();
        amounts
    }

    pub(super) async fn accounts(&self) -> Vec<coffer_server::ledger::accounts::AccountView> {
        self.server
            .ledger
            .list_accounts(&self.alice, true)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn forms_appear_for_an_unbound_account_only() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    world.sync_fixture(connection).await;
    let checking = world.provider_account("Checking").await;
    let card = world.provider_account("Credit Card").await;

    let page = world.admin.get("/connections").await;
    assert_eq!(page.status, StatusCode::OK);
    assert_inline_free(&page);
    for id in [checking.id, card.id] {
        assert!(
            page.body
                .contains(&format!("action=\"{}\"", create_path(id))),
            "an unbound account offers the create form"
        );
    }
    assert!(
        !page.body.contains("/bind\""),
        "no account to link to: no link form"
    );
    assert!(
        page.body.contains("name=\"kind\""),
        "the create form has a kind select"
    );
    assert!(
        page.body.contains("value=\"Checking\""),
        "the name defaults to the provider account's"
    );

    let done = world
        .admin
        .post_form(
            &create_path(checking.id),
            &[
                ("csrf", &page.csrf()),
                ("name", "Everyday"),
                ("kind", "checking"),
            ],
        )
        .await;
    assert_eq!(done.status, StatusCode::SEE_OTHER);
    assert_eq!(done.header(header::LOCATION), "/connections");

    let after = world.admin.get("/connections").await;
    assert!(
        !after
            .body
            .contains(&format!("action=\"{}\"", create_path(checking.id))),
        "a bound account offers no form"
    );
    assert!(after.body.contains("Bound to Everyday"));
    assert_eq!(
        after.body.matches("Not bound").count(),
        1,
        "only the other account is still unbound"
    );
    assert!(
        after
            .body
            .contains(&format!("action=\"{}\"", create_path(card.id))),
        "the other account still does"
    );
}

#[tokio::test]
async fn create_moves_held_records_to_the_new_account() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    world.sync_fixture(connection).await;
    let checking = world.provider_account("Checking").await;
    let csrf = world.admin.get("/connections").await.csrf();

    let done = world
        .admin
        .post_form(
            &create_path(checking.id),
            &[("csrf", &csrf), ("name", "Everyday"), ("kind", "checking")],
        )
        .await;
    assert_eq!(done.status, StatusCode::SEE_OTHER);

    let accounts = world.accounts().await;
    assert_eq!(accounts.len(), 1);
    let account = &accounts[0];
    assert_eq!(account.name, "Everyday");
    assert_eq!(account.kind, AccountKind::Checking);
    assert_eq!(account.currency.as_str(), "USD");
    assert!(account.include_in_net_worth);
    assert!(account.include_in_forecast);
    assert_eq!(
        account.latest_balance.as_ref().unwrap().balance.minor_units,
        245_017,
        "the reported balance reaches the account"
    );
    assert_eq!(
        world.amounts(account.id).await,
        [-4250, -1825, 150_000],
        "the held records became its transactions"
    );
    assert_eq!(
        world.provider_account("Checking").await.account,
        Some(account.id)
    );
}

#[tokio::test]
async fn a_blank_name_takes_the_provider_accounts_and_a_hint_picks_the_kind() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    let hinted = world
        .report(connection, "Rainy day", "USD", Some(AccountKind::Savings))
        .await;
    let page = world.admin.get("/connections").await;
    let select = page.between("name=\"kind\"", "</select>");
    assert!(
        select.contains("<option value=\"savings\" selected>"),
        "the provider's hint is preselected"
    );

    let done = world
        .admin
        .post_form(
            &create_path(hinted.id),
            &[("csrf", &page.csrf()), ("name", "  "), ("kind", "savings")],
        )
        .await;

    assert_eq!(done.status, StatusCode::SEE_OTHER);
    let accounts = world.accounts().await;
    assert_eq!(accounts[0].name, "Rainy day");
    assert_eq!(accounts[0].kind, AccountKind::Savings);
}

#[tokio::test]
async fn link_to_an_existing_account_moves_held_records() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    world.sync_fixture(connection).await;
    let checking = world.provider_account("Checking").await;
    let joint = world.manual_account("Joint", "USD").await;
    let euros = world.manual_account("Euro stash", "EUR").await;

    let page = world.admin.get("/connections").await;
    let form = page.between(&format!("action=\"{}\"", bind_path(checking.id)), "</form>");
    assert!(
        form.contains(&format!("<option value=\"{joint}\">Joint</option>")),
        "an open account in the same currency is offered"
    );
    assert!(
        !form.contains(&euros.to_string()),
        "an account in another currency is not"
    );

    let done = world
        .admin
        .post_form(
            &bind_path(checking.id),
            &[("csrf", &page.csrf()), ("account", &joint.to_string())],
        )
        .await;

    assert_eq!(done.status, StatusCode::SEE_OTHER);
    assert_eq!(done.header(header::LOCATION), "/connections");
    assert_eq!(world.amounts(joint).await, [-4250, -1825, 150_000]);
    assert_eq!(
        world.provider_account("Checking").await.account,
        Some(joint)
    );
    let accounts = world.accounts().await;
    let balance = accounts
        .iter()
        .find(|account| account.id == joint)
        .and_then(|account| account.latest_balance.as_ref())
        .unwrap();
    assert_eq!(balance.balance.minor_units, 245_017);
    assert!(
        world
            .admin
            .get("/connections")
            .await
            .body
            .contains("Bound to Joint")
    );
}

#[tokio::test]
async fn an_account_the_connection_already_feeds_is_not_offered() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    world.sync_fixture(connection).await;
    let checking = world.provider_account("Checking").await;
    let card = world.provider_account("Credit Card").await;
    let joint = world.manual_account("Joint", "USD").await;
    world
        .server
        .sync
        .bind(&world.alice, checking.id, joint)
        .await
        .unwrap();

    let page = world.admin.get("/connections").await;

    assert!(
        !page.body.contains(&bind_path(card.id)),
        "the only candidate is fed by this connection already"
    );
    let refused = world
        .admin
        .post_form(
            &bind_path(card.id),
            &[("csrf", &page.csrf()), ("account", &joint.to_string())],
        )
        .await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert!(refused.body.contains("role=\"alert\""));
    assert!(refused.body.contains("already bound"));
    assert_eq!(world.provider_account("Credit Card").await.account, None);
}

#[tokio::test]
async fn a_kind_is_required_when_the_provider_gives_none() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    world.sync_fixture(connection).await;
    let checking = world.provider_account("Checking").await;
    let page = world.admin.get("/connections").await;

    let refused = world
        .admin
        .post_form(
            &create_path(checking.id),
            &[("csrf", &page.csrf()), ("name", "Everyday"), ("kind", "")],
        )
        .await;

    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_inline_free(&refused);
    assert!(refused.body.contains("role=\"alert\""));
    assert!(refused.body.contains("Kind is required"));
    assert_eq!(world.accounts().await.len(), 0);
    assert_eq!(world.provider_account("Checking").await.account, None);

    let unknown = world
        .admin
        .post_form(
            &create_path(checking.id),
            &[("csrf", &page.csrf()), ("name", "Everyday"), ("kind", "x")],
        )
        .await;
    assert_eq!(unknown.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(unknown.body.contains("role=\"alert\""));
    assert_eq!(world.accounts().await.len(), 0);
}

#[tokio::test]
async fn an_invalid_name_is_a_notice() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    world.sync_fixture(connection).await;
    let checking = world.provider_account("Checking").await;
    let page = world.admin.get("/connections").await;
    let long = "n".repeat(101);

    let refused = world
        .admin
        .post_form(
            &create_path(checking.id),
            &[
                ("csrf", &page.csrf()),
                ("name", &long),
                ("kind", "checking"),
            ],
        )
        .await;

    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(refused.body.contains("role=\"alert\""));
    assert!(!refused.body.contains(&long), "the input is not echoed");
    assert_eq!(world.accounts().await.len(), 0);
}

#[tokio::test]
async fn a_currency_mismatch_is_a_notice() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    world.sync_fixture(connection).await;
    let checking = world.provider_account("Checking").await;
    let euros = world.manual_account("Euro stash", "EUR").await;
    let page = world.admin.get("/connections").await;

    // The page never offers this pair, so the request is forged.
    let refused = world
        .admin
        .post_form(
            &bind_path(checking.id),
            &[("csrf", &page.csrf()), ("account", &euros.to_string())],
        )
        .await;

    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_inline_free(&refused);
    assert!(refused.body.contains("role=\"alert\""));
    assert!(refused.body.contains("currency does not match"));
    assert_eq!(world.provider_account("Checking").await.account, None);
    assert_eq!(world.amounts(euros).await.len(), 0);

    let nothing = world
        .admin
        .post_form(
            &bind_path(checking.id),
            &[("csrf", &page.csrf()), ("account", "")],
        )
        .await;
    assert_eq!(nothing.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(nothing.body.contains("role=\"alert\""));
}

#[tokio::test]
async fn csrf_is_required_to_bind() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    world.sync_fixture(connection).await;
    let checking = world.provider_account("Checking").await;
    let joint = world.manual_account("Joint", "USD").await;
    let joint = joint.to_string();

    for (path, fields) in [
        (
            create_path(checking.id),
            vec![("name", "Everyday"), ("kind", "checking")],
        ),
        (bind_path(checking.id), vec![("account", joint.as_str())]),
    ] {
        let missing = world.admin.post_form(&path, &fields).await;
        assert_eq!(
            missing.status,
            StatusCode::FORBIDDEN,
            "a form without its CSRF token is refused"
        );
        let mut wrong = fields.clone();
        wrong.push(("csrf", "AAAA"));
        let refused = world.admin.post_form(&path, &wrong).await;
        assert_eq!(
            refused.status,
            StatusCode::FORBIDDEN,
            "a form with a wrong CSRF token is refused"
        );
    }

    assert_eq!(world.provider_account("Checking").await.account, None);
    assert_eq!(world.accounts().await.len(), 1);
}

#[tokio::test]
async fn a_member_binds_through_the_service_and_a_removed_connection_does_not() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    world.sync_fixture(connection).await;
    let checking = world.provider_account("Checking").await;
    let invite = world
        .server
        .identity
        .create_invite(&world.alice, Role::Member, None)
        .await
        .unwrap();
    let mut bob = Browser::new(&world.server);
    let mut key = world.server.authenticator();
    let done = bob.enroll(&mut key, &invite, "bob", "Bob").await;
    assert_eq!(done.status, StatusCode::OK);

    let page = bob.get("/connections").await;
    assert!(
        page.body
            .contains(&format!("action=\"{}\"", create_path(checking.id))),
        "the service lets a member bind another member's account"
    );
    let done = bob
        .post_form(
            &create_path(checking.id),
            &[
                ("csrf", &page.csrf()),
                ("name", "Bob's view"),
                ("kind", "checking"),
            ],
        )
        .await;
    assert_eq!(done.status, StatusCode::SEE_OTHER);

    let card = world.provider_account("Credit Card").await;
    let mut tx = world.server.store.begin().await.unwrap();
    tx.update_connection_status(
        connection,
        ConnectionStatusChange {
            status: ConnectionStatus::Removed,
            last_error: None,
            attempted_at: None,
            succeeded_at: None,
            at: world.server.clock.now(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let page = world.admin.get("/connections").await;
    assert!(
        !page.body.contains(&create_path(card.id)),
        "no form for a removed connection"
    );
    let refused = world
        .admin
        .post_form(
            &create_path(card.id),
            &[
                ("csrf", &page.csrf()),
                ("name", "Card"),
                ("kind", "credit_card"),
            ],
        )
        .await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert!(refused.body.contains("role=\"alert\""));
    assert_eq!(world.provider_account("Credit Card").await.account, None);
}

#[tokio::test]
async fn unknown_provider_accounts_are_not_found() {
    let mut world = world().await;
    let csrf = world.admin.get("/connections").await.csrf();
    let account = AccountId::new().to_string();
    for (path, fields) in [
        (
            create_path(ProviderAccountId::new()),
            vec![("csrf", csrf.as_str()), ("kind", "checking")],
        ),
        (
            bind_path(ProviderAccountId::new()),
            vec![("csrf", csrf.as_str()), ("account", account.as_str())],
        ),
        (
            "/connections/provider-accounts/nonsense/bind".to_owned(),
            vec![("csrf", csrf.as_str()), ("account", account.as_str())],
        ),
    ] {
        let missing = world.admin.post_form(&path, &fields).await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn provider_account_names_are_escaped() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    let name = "<script>alert(1)</script>\" onfocus=\"x & <b>";
    world.report(connection, name, "USD", None).await;
    let mixed = world.manual_account("<i>Joint</i>", "USD").await;

    let page = world.admin.get("/connections").await;

    assert_eq!(page.status, StatusCode::OK);
    assert_inline_free(&page);
    assert!(!page.body.contains("<script>alert"));
    assert!(!page.body.contains("<b>"));
    assert!(!page.body.contains("<i>Joint"));
    assert!(!page.body.contains("\" onfocus=\""));
    assert!(page.body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(page.body.contains("&lt;i&gt;Joint&lt;/i&gt;"));
    assert!(page.body.contains(&mixed.to_string()));
}

#[tokio::test]
async fn twelve_unbound_accounts_each_get_their_forms() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    let mut ids = Vec::new();
    for number in 0..12 {
        ids.push(
            world
                .report(connection, &format!("Account {number}"), "USD", None)
                .await
                .id,
        );
    }
    world.manual_account("Joint", "USD").await;

    let page = world.admin.get("/connections").await;

    for id in ids {
        assert!(
            page.body
                .contains(&format!("action=\"{}\"", create_path(id)))
        );
        assert!(page.body.contains(&format!("action=\"{}\"", bind_path(id))));
    }
}
