//! Properties of the grant resolver, and of what no read endpoint reveals.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use super::{LEVELS, grant, ids, on, s, world};
use axum::http::{Method, StatusCode};
use coffer_domain::identity::{Actor, Role, Scope, Via};
use coffer_domain::ids::{AccountId, ConnectionId, TransactionId};
use coffer_domain::policy::{
    AccountLevel, AccountScope, DenyReason, GrantTarget, TokenGrant, authorize_account,
    grant_ceiling,
};
use proptest::strategy::Strategy as _;
use proptest::test_runner::TestCaseError;
use std::collections::BTreeMap;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn level() -> impl proptest::strategy::Strategy<Value = AccountLevel> {
    proptest::sample::select(LEVELS.to_vec())
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(32))]

    /// Grants as stored, unvalidated (above the scope included), resolved
    /// for every role and token scope: `scope()` agrees with
    /// `authorize_account`, no level exceeds the scope's ceiling, and an
    /// account no grant names is hidden. A session ignores stored grants.
    #[test]
    fn resolver_never_exceeds_the_scope_ceiling(
        role in proptest::sample::select(vec![Role::Member, Role::Admin]),
        scope in proptest::sample::select(vec![Scope::Read, Scope::Write, Scope::Admin]),
        all in proptest::option::of(level()),
        named in proptest::collection::vec(proptest::option::of(level()), 3),
    ) {
        runtime().block_on(async {
            let world = world().await;
            let mut accounts = Vec::new();
            for n in 0..4 {
                accounts.push(world.account(&format!("Account {n}")).await);
            }
            let (token, _) = world.token(Scope::Admin, Vec::new()).await;
            let mut grants = BTreeMap::new();
            if let Some(level) = all {
                grants.insert(GrantTarget::All, level);
            }
            // The fourth account is never named.
            for (account, level) in accounts.iter().zip(&named) {
                if let Some(level) = level {
                    grants.insert(GrantTarget::Account(*account), *level);
                }
            }
            let grants: Vec<TokenGrant> = grants
                .into_iter()
                .map(|(target, level)| TokenGrant { target, level })
                .collect();
            let mut tx = world.server.store.begin().await.unwrap();
            tx.replace_token_grants(token, &grants).await.unwrap();
            tx.commit().await.unwrap();

            let actor = Actor { member: world.alice.member, role, via: Via::Agent { token, scope } };
            let access = world.server.access.resolve(&actor).await.unwrap();
            let ceiling = grant_ceiling(actor.effective_scope());
            for (n, account) in accounts.iter().enumerate() {
                let named_level = named.get(n).copied().flatten();
                let best = all.max(named_level);
                let level = access.level_for(*account);
                if grants.is_empty() {
                    proptest::prop_assert_eq!(level, Some(ceiling));
                } else {
                    proptest::prop_assert_eq!(level, best.map(|best| best.min(ceiling)));
                }
                if level.is_none() {
                    let denied = authorize_account(&actor, &access, *account, AccountLevel::Balances);
                    proptest::prop_assert_eq!(denied.map_err(|d| d.reason), Err(DenyReason::AccountHidden));
                }
                for needed in LEVELS {
                    let allowed = authorize_account(&actor, &access, *account, needed).is_ok();
                    let listed = match access.scope(needed) {
                        AccountScope::All => true,
                        AccountScope::Only(ids) => ids.contains(account),
                    };
                    proptest::prop_assert_eq!(listed, allowed);
                    if needed > ceiling {
                        proptest::prop_assert!(!allowed);
                        proptest::prop_assert!(!access.sees_every_account(needed));
                    }
                }
            }
            let session = Actor { via: Via::Session, ..actor };
            let uniform = world.server.access.resolve(&session).await.unwrap();
            proptest::prop_assert!(uniform.sees_every_account(grant_ceiling(session.effective_scope())));
            Ok(())
        })?;
    }
}

/// One generated household: per account, its level for the token (`None`:
/// not named), and the token's `all` grant.
#[derive(Debug, Clone)]
struct Household {
    scope: Scope,
    all: Option<AccountLevel>,
    accounts: Vec<Option<AccountLevel>>,
}

fn capped(scope: Scope, level: AccountLevel) -> AccountLevel {
    level.min(grant_ceiling(scope))
}

/// No `all` grant, at least one visible account and at least one hidden.
fn household_with_hidden_accounts() -> impl proptest::strategy::Strategy<Value = Household> {
    (
        proptest::sample::select(vec![Scope::Read, Scope::Write]),
        proptest::collection::vec(level(), 1..=3),
        1..=2_usize,
    )
        .prop_map(|(scope, visible, hidden)| Household {
            scope,
            all: None,
            accounts: visible
                .into_iter()
                .map(|level| Some(capped(scope, level)))
                .chain(std::iter::repeat_n(None, hidden))
                .collect(),
        })
}

/// An `all` grant, so no account is hidden; below `transactions` the
/// connection-wide data still is.
fn household_with_all_grant() -> impl proptest::strategy::Strategy<Value = Household> {
    (
        proptest::sample::select(vec![Scope::Read, Scope::Write]),
        level(),
        proptest::collection::vec(proptest::option::of(level()), 1..=3),
    )
        .prop_map(|(scope, all, accounts)| Household {
            scope,
            all: Some(capped(scope, all)),
            accounts: accounts
                .into_iter()
                .map(|level| level.map(|level| capped(scope, level)))
                .collect(),
        })
}

/// Every read endpoint, for these accounts and transactions.
fn read_uris(
    connection: ConnectionId,
    accounts: &[AccountId],
    transactions: &[TransactionId],
) -> Vec<String> {
    let mut uris = vec![
        "/api/v1/accounts".to_owned(),
        "/api/v1/accounts?include_closed=true".to_owned(),
        "/api/v1/transactions".to_owned(),
        "/api/v1/transactions?limit=1".to_owned(),
        "/api/v1/provider-accounts".to_owned(),
        "/api/v1/provider-accounts?unbound=true".to_owned(),
        "/api/v1/connections".to_owned(),
        format!("/api/v1/connections/{connection}/sync-runs"),
        "/api/v1/audit".to_owned(),
        "/api/v1/me".to_owned(),
    ];
    for account in accounts {
        uris.push(format!("/api/v1/accounts/{account}"));
        uris.push(format!("/api/v1/accounts/{account}/balances"));
        uris.push(format!("/api/v1/transactions?account={account}"));
    }
    for transaction in transactions {
        uris.push(format!("/api/v1/transactions/{transaction}"));
    }
    uris
}

/// Every read endpoint, called with a token holding `household`'s grants:
/// `/accounts` lists exactly the visible accounts, and no body holds a
/// hidden account's id, name, provider account name, transaction id or
/// description, or source id. Without an `all` grant at `transactions` no
/// body holds an unbound provider account or a connection's last error.
async fn check_no_endpoint_leaks(household: &Household) -> Result<(), TestCaseError> {
    let world = world().await;
    let marker = uuid_marker();
    let mut grants = Vec::new();
    if let Some(level) = household.all {
        grants.push(grant(GrantTarget::All, level));
    }
    let mut accounts = Vec::new();
    let mut visible = Vec::new();
    let mut transactions = Vec::new();
    let mut hidden = Vec::new();
    let mut bindings = Vec::new();
    for (n, level) in household.accounts.iter().enumerate() {
        let name = format!("acct{n}{marker}");
        let account = world.account(&name).await;
        world.balance(account).await;
        let own = vec![
            world
                .transaction(account, 1, &format!("txa{n}{marker}"))
                .await,
            world
                .transaction(account, 2, &format!("txb{n}{marker}"))
                .await,
        ];
        if let Some(level) = level {
            grants.push(on(account, *level));
        }
        if household.all.is_none() && level.is_none() {
            hidden.push(s(account));
            hidden.push(name);
            hidden.push(format!("prov{n}{marker}"));
            hidden.push(format!("txa{n}{marker}"));
            hidden.push(format!("txb{n}{marker}"));
            hidden.extend(own.iter().map(s));
        } else {
            visible.push(s(account));
        }
        bindings.push((format!("prov{n}{marker}"), account));
        accounts.push(account);
        transactions.extend(own);
    }
    let unbound = format!("unbound{marker}");
    let last_error = format!("lasterror{marker}");
    if household
        .all
        .is_none_or(|all| all < AccountLevel::Transactions)
    {
        hidden.push(unbound.clone());
        hidden.push(last_error.clone());
    }
    let mut names: Vec<(&str, Option<AccountId>)> = bindings
        .iter()
        .map(|(name, account)| (name.as_str(), Some(*account)))
        .collect();
    names.push((unbound.as_str(), None));
    let connection = world.connection(&names).await;
    world.fail_connection(connection, &last_error).await;

    // An imported transaction on each account, the last account's listed as
    // a duplicate of it. The store refuses such a cross-account pair, so
    // this is seeded past that check, to test the filter that would keep
    // one from showing.
    let last = accounts.len() - 1;
    let mut imported = Vec::new();
    for (n, account) in accounts.iter().enumerate() {
        let description = format!("imported{n}{marker}");
        let (transaction, source) = world.imported(connection, *account, &description).await;
        if household.all.is_none() && household.accounts[n].is_none() {
            hidden.push(description);
            hidden.push(s(transaction));
            hidden.push(source);
        }
        imported.push(transaction);
        transactions.push(transaction);
    }
    for transaction in &imported[..last] {
        world.flag_duplicate(*transaction, imported[last]).await;
    }
    world.flag_duplicate(imported[last], transactions[0]).await;

    let (_, token) = world.token(household.scope, grants).await;

    let (status, listed) = world.get("/api/v1/accounts", &token).await;
    proptest::prop_assert_eq!(status, StatusCode::OK);
    let mut listed = ids(&listed);
    listed.sort();
    visible.sort();
    proptest::prop_assert_eq!(listed, visible, "every visible account is listed");

    let uris = read_uris(connection, &accounts, &transactions);
    for uri in &uris {
        let (status, body) = world.send(Method::GET, uri, &token, None, None).await;
        proptest::prop_assert!(status != StatusCode::INTERNAL_SERVER_ERROR);
        for secret in &hidden {
            proptest::prop_assert!(
                !body.contains(secret.as_str()),
                "a response reveals something hidden"
            );
        }
    }
    Ok(())
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(16))]

    #[test]
    fn no_endpoint_leaks_hidden_accounts(household in household_with_hidden_accounts()) {
        runtime().block_on(check_no_endpoint_leaks(&household))?;
    }

    #[test]
    fn no_endpoint_leaks_with_an_all_grant(household in household_with_all_grant()) {
        runtime().block_on(check_no_endpoint_leaks(&household))?;
    }
}

/// A suffix no generated id or fixed text contains, so a name is found only
/// where it was put.
fn uuid_marker() -> String {
    format!(
        "zq{}",
        AccountId::new().to_string().replace('-', "")[24..].to_owned()
    )
}
