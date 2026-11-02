//! What a hidden bucket can be, each compared whole with a household lacking it.

use coffer_domain::accounts::{AccountKind, LiquidityClass};
use coffer_domain::ids::AccountId;

use super::super::World;
use super::super::net_worth_channels::{HIDDEN_MINOR, RANGE};
use super::family::{HIDDEN_LABEL, HIDDEN_NAME, LABEL, SINCE, assert_twins, family};

/// What a hidden bucket can be.
#[derive(Clone, Copy)]
pub(super) enum Hidden {
    Beside(Beside),
    Apart(Apart),
}

/// A hidden account in the bucket the token holds part of.
#[derive(Clone, Copy)]
pub(super) enum Beside {
    Balance,
    Closing,
    ClosedOwing,
    Age,
    BackDated,
    BeforeTheFirstDay,
    Hole,
    RowInAnotherCurrency,
    TooLargeToConvert,
    TooLargeTotal,
    PositiveLiability,
}

/// A hidden account elsewhere: a bucket of its own, or the household's.
#[derive(Clone, Copy)]
pub(super) enum Apart {
    Label,
    Currency,
    FutureSnapshot,
    NoValueYet,
    GivenAway,
    NoLabel,
    SortsFirst,
    OddLabel,
    /// A household account switched out, for contrast.
    Excluded,
}

impl World {
    /// A 529 for `label` in `currency`, holding `minor_units` since `on_day`.
    async fn education(
        &self,
        label: &str,
        currency: &str,
        on_day: &str,
        minor_units: i64,
    ) -> AccountId {
        let id = self
            .bucket_account(
                HIDDEN_NAME,
                AccountKind::Education,
                currency,
                None,
                Some(label),
            )
            .await;
        self.balance_on(id, on_day, minor_units).await;
        id
    }
}

impl Beside {
    /// Puts it in the household, in Emma's own bucket, which it must not change. Answers what its
    /// effect on the whole household's report holds, so a case that stopped doing anything fails.
    async fn place(self, world: &World) -> &'static str {
        match self {
            Self::Balance => {
                world.held_for(LABEL, HIDDEN_MINOR).await;
                "7346195280"
            }
            Self::Closing => {
                world.close(world.held_for(LABEL, HIDDEN_MINOR).await).await;
                "\"closed_on\":\"2026-11-02\""
            }
            Self::ClosedOwing => {
                world
                    .close(world.held_for(LABEL, -HIDDEN_MINOR).await)
                    .await;
                "closed_account_owing"
            }
            Self::Age => {
                world
                    .education(LABEL, "USD", "2020-01-01", HIDDEN_MINOR)
                    .await;
                "balance_stale"
            }
            Self::BackDated => {
                world
                    .education(LABEL, "USD", "1970-01-01", HIDDEN_MINOR)
                    .await;
                "\"first_on\":\"1970-01-01\""
            }
            Self::BeforeTheFirstDay => {
                world
                    .education(LABEL, "USD", "1961-06-01", HIDDEN_MINOR)
                    .await;
                "\"first_on\":\"1970-01-01\""
            }
            Self::Hole => {
                world
                    .hole_today(world.held_for(LABEL, HIDDEN_MINOR).await)
                    .await;
                "daily_value_missing"
            }
            Self::RowInAnotherCurrency => {
                world
                    .row_in_chf(world.held_for(LABEL, HIDDEN_MINOR).await)
                    .await;
                "internal"
            }
            Self::TooLargeToConvert => {
                world.education(LABEL, "HUF", SINCE, i64::MAX).await;
                world.rates("USD", &[("2026-10-01", "1.1")]).await;
                world.rates("HUF", &[("2026-10-01", "0.0001")]).await;
                "amount_too_large"
            }
            Self::TooLargeTotal => {
                world.held_for(LABEL, i64::MAX).await;
                world.held_for(LABEL, i64::MAX).await;
                "too large"
            }
            Self::PositiveLiability => {
                let liability = Some(LiquidityClass::LongTermLiability);
                let owed = world
                    .bucket_account(
                        HIDDEN_NAME,
                        AccountKind::Custodial,
                        "USD",
                        liability,
                        Some(LABEL),
                    )
                    .await;
                world.balance_on(owed, SINCE, HIDDEN_MINOR).await;
                "liability_balance_positive"
            }
        }
    }
}

impl Apart {
    /// Puts it in the household, and answers as [`Beside::place`] does.
    async fn place(self, world: &World) -> &'static str {
        match self {
            Self::Label => {
                world.held_for(HIDDEN_LABEL, HIDDEN_MINOR).await;
                HIDDEN_LABEL
            }
            Self::Currency => {
                world
                    .education(HIDDEN_LABEL, "JPY", SINCE, HIDDEN_MINOR)
                    .await;
                "\"currency\":\"JPY\""
            }
            Self::FutureSnapshot => {
                world
                    .education(HIDDEN_LABEL, "USD", "2027-03-01", HIDDEN_MINOR)
                    .await;
                HIDDEN_LABEL
            }
            Self::NoValueYet => {
                world
                    .bucket_account(
                        HIDDEN_NAME,
                        AccountKind::Education,
                        "USD",
                        None,
                        Some(HIDDEN_LABEL),
                    )
                    .await;
                HIDDEN_LABEL
            }
            Self::GivenAway => {
                let gift = world
                    .bucket_account(
                        HIDDEN_NAME,
                        AccountKind::Charitable,
                        "USD",
                        None,
                        Some(HIDDEN_LABEL),
                    )
                    .await;
                world.balance_on(gift, SINCE, HIDDEN_MINOR).await;
                "given_away"
            }
            Self::NoLabel => {
                let bare = world
                    .bucket_account(HIDDEN_NAME, AccountKind::Custodial, "USD", None, None)
                    .await;
                world.balance_on(bare, SINCE, HIDDEN_MINOR).await;
                "\"beneficiary\":null"
            }
            Self::SortsFirst => {
                world.held_for("Aaron", HIDDEN_MINOR).await;
                "Aaron"
            }
            Self::OddLabel => {
                world.held_for("Zoë \"quoted\" <b>", HIDDEN_MINOR).await;
                "Zoë"
            }
            Self::Excluded => {
                let off = world
                    .holding_as(HIDDEN_NAME, AccountKind::Checking, "USD", None, false)
                    .await;
                world.balance_on(off, SINCE, HIDDEN_MINOR).await;
                "excluded_by_flag"
            }
        }
    }
}

impl Hidden {
    async fn place(self, world: &World) -> &'static str {
        match self {
            Self::Beside(case) => case.place(world).await,
            Self::Apart(case) => case.place(world).await,
        }
    }
}

pub(super) async fn changes_no_byte(hidden: Hidden) {
    let (bare, held) = (family().await, family().await);

    let effect = hidden.place(&held.world).await;

    let whole_household = held.text(RANGE, &held.admin).await;
    assert!(
        whole_household.contains(effect),
        "the case must reach the whole household's answer: {effect}"
    );
    assert_twins(&bare, &held).await;
}

macro_rules! changes_no_byte {
    ($($test:ident => $case:expr,)*) => {$(
        #[tokio::test]
        async fn $test() {
            changes_no_byte($case).await;
        }
    )*};
}

changes_no_byte! {
    a_hidden_bucket_back_dated_to_1970_extends_no_range => Hidden::Beside(Beside::BackDated),
    a_hidden_bucket_before_1970_extends_no_range => Hidden::Beside(Beside::BeforeTheFirstDay),
    a_hidden_closed_bucket_still_owing_raises_no_caution => Hidden::Beside(Beside::ClosedOwing),
    a_hidden_hole_in_a_bucket_raises_no_caution => Hidden::Beside(Beside::Hole),
    a_hidden_corrupt_bucket_row_fails_no_read => Hidden::Beside(Beside::RowInAnotherCurrency),
    a_hidden_bucket_amount_too_large_raises_no_caution => Hidden::Beside(Beside::TooLargeToConvert),
    a_hidden_bucket_total_too_large_refuses_nothing => Hidden::Beside(Beside::TooLargeTotal),
    a_hidden_bucket_s_future_snapshot_changes_no_byte => Hidden::Apart(Apart::FutureSnapshot),
    a_hidden_positive_liability_in_a_bucket_raises_no_caution => Hidden::Beside(Beside::PositiveLiability),
    a_hidden_bucket_with_no_value_yet_adds_no_empty_bucket => Hidden::Apart(Apart::NoValueYet),
    a_hidden_given_away_bucket_changes_no_byte => Hidden::Apart(Apart::GivenAway),
    a_hidden_bucket_with_no_label_changes_no_byte => Hidden::Apart(Apart::NoLabel),
    a_hidden_label_sorting_first_takes_no_place => Hidden::Apart(Apart::SortsFirst),
    a_hidden_label_with_odd_characters_changes_no_byte => Hidden::Apart(Apart::OddLabel),
    a_hidden_excluded_account_beside_buckets_changes_no_byte => Hidden::Apart(Apart::Excluded),
}
