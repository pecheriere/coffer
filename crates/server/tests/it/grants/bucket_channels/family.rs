//! A household for attacking buckets, and comparing what two such households answer.

use std::time::Duration;

use axum::http::{HeaderMap, StatusCode};
use coffer_domain::accounts::{AccountKind, LiquidityClass};
use coffer_domain::identity::Scope;
use coffer_domain::ids::{AccountId, CategoryId, TransactionId};
use coffer_domain::policy::AccountLevel;
use coffer_domain::transactions::SplitInput;
use coffer_server::ledger::accounts::{AccountPatch, NewAccountInput};
use coffer_server::ledger::transactions::TransactionPatch;
use coffer_server::secrets::SecretToken;
use serde_json::Value;

use super::super::net_worth_channels::{RANGE, TODAY, code};
use super::super::{World, on, world};

pub(in super::super) const LABEL: &str = "Emma Quietfield";
pub(in super::super) const HIDDEN_LABEL: &str = "Noah Hushwood";
pub(in super::super) const HIDDEN_NAME: &str = "Noah college fund";
pub(in super::super) const HOME_MINOR: i64 = 100_000;
pub(in super::super) const BUCKET_MINOR: i64 = 25_000;
pub(in super::super) const SINCE: &str = "2026-10-26";

/// Every read a restricted token might compare, in every shape: the second report is wholly
/// before anything a [`Family`] holds has a value, which a hidden older sibling must not fill.
pub(in super::super) const READS: [&str; 28] = [
    RANGE,
    "/api/v1/reports/net-worth?from=2026-10-01&to=2026-10-20&interval=day",
    "/api/v1/reports/net-worth?from=2026-01-01&interval=week",
    "/api/v1/reports/net-worth?from=1970-01-01&interval=month",
    "/api/v1/reports/net-worth?from=2026-10-26&interval=day&account_points=false",
    "/api/v1/reports/net-worth?from=2026-10-26&to=2026-12-31&interval=day",
    "/api/v1/reports/net-worth",
    "/api/v1/reports/plan-vs-actual?from=2026-10",
    "/api/v1/accounts",
    "/api/v1/accounts?include_closed=true",
    "/api/v1/transactions",
    "/api/v1/transactions?limit=1",
    "/api/v1/provider-accounts",
    "/api/v1/provider-accounts?unbound=true",
    "/api/v1/connections",
    "/api/v1/audit",
    "/api/v1/me",
    "/api/v1/members",
    "/api/v1/plan-items",
    "/api/v1/plan-items?include_inactive=true",
    "/api/v1/forecast",
    "/api/v1/household/settings",
    "/api/v1/categories",
    "/api/v1/category-groups",
    "/api/v1/fx/coverage",
    "/api/v1/fx/rates?currency=JPY&from=2026-01-01&to=2026-12-31",
    "/api/v1/fx/rates?currency=USD&from=2026-01-01&to=2026-12-31",
    "/api/v1/fx/rates?currency=CHF&from=2026-01-01&to=2026-12-31",
];

/// A whole answer: the uri, the status, every header and the body, ids named.
pub(in super::super) type Answer = (String, StatusCode, HeaderMap, String);

/// `text` with each UUID replaced by `<idN>`, N counting the different ones as they appear.
pub(in super::super) fn ids_by_first_use(text: &str) -> String {
    let is_uuid = |rest: &[u8]| {
        rest.len() >= 36
            && rest[..36].iter().enumerate().all(|(at, byte)| match at {
                8 | 13 | 18 | 23 => *byte == b'-',
                _ => byte.is_ascii_hexdigit(),
            })
    };
    let mut seen: Vec<&str> = Vec::new();
    let mut named = String::with_capacity(text.len());
    let mut at = 0;
    while let Some(next) = text[at..].chars().next() {
        if is_uuid(&text.as_bytes()[at..]) {
            let id = &text[at..at + 36];
            let n = seen.iter().position(|one| *one == id).unwrap_or_else(|| {
                seen.push(id);
                seen.len() - 1
            });
            named.push_str("<id");
            named.push_str(&n.to_string());
            named.push('>');
            at += 36;
        } else {
            named.push(next);
            at += next.len_utf8();
        }
    }
    named
}

/// How a [`Family`] starts.
#[derive(Clone, Copy)]
pub(in super::super) struct Setup {
    /// The currency of Emma's account.
    pub(in super::super) currency: &'static str,
    pub(in super::super) display: bool,
    pub(in super::super) built: bool,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            currency: "USD",
            display: true,
            built: true,
        }
    }
}

impl World {
    /// An account that isn't the household's: whose it is follows `kind`, as `beneficiary` says.
    pub(in super::super) async fn bucket_account(
        &self,
        name: &str,
        kind: AccountKind,
        currency: &str,
        liquidity: Option<LiquidityClass>,
        beneficiary: Option<&str>,
    ) -> AccountId {
        let id = self
            .server
            .ledger
            .create_account(
                &self.alice,
                NewAccountInput {
                    name: name.into(),
                    kind,
                    currency: code(currency),
                    liquidity,
                    include_in_net_worth: None,
                    include_in_forecast: None,
                    belongs_to: None,
                    beneficiary: beneficiary.map(str::to_owned),
                    opening_balance: None,
                },
                None,
            )
            .await
            .unwrap()
            .id;
        // Ids are ordered by the millisecond, and a list by them.
        tokio::time::sleep(Duration::from_millis(2)).await;
        id
    }

    /// A 529 for `beneficiary` holding `minor_units` USD since [`SINCE`].
    pub(in super::super) async fn held_for(
        &self,
        beneficiary: &str,
        minor_units: i64,
    ) -> AccountId {
        let id = self
            .bucket_account(
                HIDDEN_NAME,
                AccountKind::Education,
                "USD",
                None,
                Some(beneficiary),
            )
            .await;
        self.balance_on(id, SINCE, minor_units).await;
        id
    }

    /// Splits all of `transaction`, of -1000, into `category`.
    pub(in super::super) async fn categorize(
        &self,
        transaction: TransactionId,
        category: CategoryId,
    ) {
        self.server
            .ledger
            .update_transaction(
                &self.alice,
                transaction,
                TransactionPatch {
                    splits: Some(vec![SplitInput {
                        category,
                        minor_units: -1000,
                        goal: None,
                    }]),
                    ..TransactionPatch::default()
                },
            )
            .await
            .unwrap();
    }

    pub(in super::super) async fn patch_account(&self, id: AccountId, patch: AccountPatch) {
        self.server
            .ledger
            .update_account(&self.alice, id, patch)
            .await
            .unwrap();
    }
}

/// One household: display USD, a household account, a 529 labelled [`LABEL`], a read token seeing
/// both at `balances`, and an admin token seeing everything.
pub(in super::super) struct Family {
    pub(in super::super) world: World,
    pub(in super::super) visible: Vec<AccountId>,
    pub(in super::super) token: SecretToken,
    pub(in super::super) admin: SecretToken,
}

/// As [`family`], with `before` run before the visible accounts are made.
pub(in super::super) async fn family_with(
    setup: Setup,
    before: impl AsyncFnOnce(&World),
) -> Family {
    let world = world().await;
    world.on_today();
    if setup.display {
        world.display("USD").await;
    }
    before(&world).await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    let home = world.holding("Home", "USD").await;
    world.balance_on(home, SINCE, HOME_MINOR).await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    let emma = world
        .bucket_account(
            "Emma 529",
            AccountKind::Education,
            setup.currency,
            None,
            Some(LABEL),
        )
        .await;
    world.balance_on(emma, SINCE, BUCKET_MINOR).await;
    if setup.built {
        world.rebuild(TODAY).await;
    }
    let (_, token) = world
        .token(
            Scope::Read,
            vec![
                on(home, AccountLevel::Balances),
                on(emma, AccountLevel::Balances),
            ],
        )
        .await;
    let (_, admin) = world.token(Scope::Admin, Vec::new()).await;
    Family {
        world,
        visible: vec![home, emma],
        token,
        admin,
    }
}

pub(in super::super) async fn family() -> Family {
    family_with(Setup::default(), async |_| {}).await
}

impl Family {
    pub(in super::super) fn home(&self) -> AccountId {
        self.visible[0]
    }

    pub(in super::super) fn emma(&self) -> AccountId {
        self.visible[1]
    }

    /// `uri` as `token` gets it, ids named.
    pub(in super::super) async fn answer(&self, uri: &str, token: &SecretToken) -> Answer {
        let (status, headers, body) = self.world.get_whole(uri, token).await;
        (
            ids_by_first_use(uri),
            status,
            headers,
            ids_by_first_use(&body),
        )
    }

    /// Everything `token` can ask: every read, and each visible account's own.
    pub(in super::super) async fn answers(&self, token: &SecretToken) -> Vec<Answer> {
        let mut uris: Vec<String> = READS.iter().map(ToString::to_string).collect();
        for id in &self.visible {
            uris.push(format!("/api/v1/accounts/{id}"));
            uris.push(format!("/api/v1/accounts/{id}/balances"));
            uris.push(format!("/api/v1/transactions?account={id}"));
        }
        let mut answers = Vec::with_capacity(uris.len());
        for uri in &uris {
            answers.push(self.answer(uri, token).await);
        }
        answers
    }

    pub(in super::super) async fn text(&self, uri: &str, token: &SecretToken) -> String {
        self.world.get_whole(uri, token).await.2
    }

    pub(in super::super) async fn report(&self, token: &SecretToken) -> Value {
        let (status, _, text) = self.world.get_whole(RANGE, token).await;
        assert_eq!(status, StatusCode::OK, "the report answers");
        serde_json::from_str(&text).unwrap()
    }
}

/// Fails naming the first read to differ.
pub(in super::super) fn assert_same_answers(bare: &[Answer], held: &[Answer]) {
    assert_eq!(bare.len(), held.len());
    for (one, other) in bare.iter().zip(held) {
        assert_eq!(one, other, "not byte-identical");
    }
}

/// The restricted tokens of the two households get the same answers to everything.
pub(in super::super) async fn assert_twins(bare: &Family, held: &Family) {
    assert_same_answers(
        &bare.answers(&bare.token).await,
        &held.answers(&held.token).await,
    );
}
