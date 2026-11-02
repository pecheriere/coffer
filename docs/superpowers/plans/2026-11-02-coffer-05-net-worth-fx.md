# coffer plan 4 — Net worth history and exchange rates Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish issue #58: answer "what are we worth, and how has that moved", per day, week or month, in one display currency, fast whatever the range, with every rate date and every missing rate stated, and a restricted token seeing only the sum of what it may see.

**Architecture:** Conversion, the daily values and the report are pure functions in `coffer-domain`. `coffer-store` holds the rates, the display currency and a derived table of one row per account per day, updated in the same transaction as whatever changes it. `crates/server/src/fx` holds the pluggable rate provider (ECB by default, over `egress::Egress`) and the jobs that fetch rates and fill days forward. `Ledger` authorizes and gathers; `http/api` and a session-only page with a server-rendered SVG chart show the same answer.

**Tech Stack:** Rust 2024, axum, sqlx (SQLite), jiff, maud, utoipa, proptest. No new crates: the ECB's answer is a CSV of a fixed shape, read strictly.

**Spec:** `docs/superpowers/specs/2026-10-30-coffer-design.md`: §1, §6.1, §6.4, §6.5, §6.9, §6.10, §6.11, §8.2, §8.5, §8.7, §8.9 R1 and R2, and **§8.10, which this branch adds** (rulings N1–N15). No task writes spec text; if the owner overrules a ruling, change §8.10 and this plan first.

**Depends on:** `main` at `bd29955` or later for Tasks 1, 2 and 4. **Task 3 starts only after PR #77 has merged**, because it changes that PR's `kept_balances.rs`. `main` holds migrations up to `0016`; #77 takes `0017` (`kept_balances`) and #54 takes `0018`, so this plan owns `0019`–`0022`. sqlx 0.9 applies any unapplied migration on start whatever its number (`Migrator::run`), so if #54 lands after these, its `0018` runs after them on a deployed database, which is harmless as long as it doesn't touch these tables. If a task finds one of these numbers taken, it renumbers and says so in its PR. #77 also edits the spec, so a rebase may touch §8.10's neighbours; keep both.

**Before deploying (owner's approval needed):** the Pi must allow one more host, on port 443, by SNI: **`data-api.ecb.europa.eu`**. Nothing else. A firewall that works from DNS answers must also allow its CNAME, `data--api-ecb-europa-eu.ax4z.com` (`80.90.16.29`, both seen on 2026-11-02; don't pin the address). Checked on 2026-11-02 with `curl -A coffer/0.2.1`: 200, no redirect, no key, `text/csv`, 408,646 bytes for the whole USD history. Until that is allowed the rate job only logs failures, and reports say `rate_missing`; sync and everything else carry on. A household with one currency never contacts it (N7).

## Global Constraints

- Money is exact: integer minor units in `i64`, intermediates in `i128`, checked arithmetic, never a float, including the chart's coordinates. Rates are exact decimals. Rounding is half-even, once per account per day (N4).
- A day is the household's day: `clock.now().to_zoned(timezone).date()`. Never `Timestamp::now()`. Providers and the store read no clock: callers hand them `today`.
- Hidden answers exactly like missing, and more: **nothing in a net worth answer may depend on an account the actor can't see at `balances`** (N2). Filter in the query. `AccountAccess` comes only from `AccessResolver`. Never mark an answer partial.
- Every change to accounts, snapshots and settings still goes through a `StoreTx` with its audit entry. Rates and rows are the exception (N10). Reads, the report and the pages write nothing.
- Outbound HTTP only through `egress::Egress` with a `HostAllowList`. No text from a provider is stored, logged or shown: only fixed codes and a status. Log events carry a fixed message, the currency code and the status; never a body, and never an amount.
- API routes only through `ApiRouter::op` with `#[utoipa::path]`, types deriving `ToSchema` or `IntoParams`, no `serde_json::Value`, extraction through `TypedJson` and `TypedQuery`. Dates in a query or body are refused unless exactly `YYYY-MM-DD` (`coffer_domain::plan::strict_date`). After any API change run `cargo run -p coffer-server -- openapi > docs/api/openapi.json`. On a rebase regenerate it; never hand-merge it.
- Migrations are STRICT tables whose CHECKs mirror the domain types; `WITHOUT ROWID` where the key is the read path. The store parses every value it reads.
- Pages: no inline script or style, no `style=` attribute, no colour literal outside a theme file, the default `Content-Security-Policy`. `/net-worth` stays out of `PAGES` in `http/browser_reports/fields.rs`.
- Integration tests go under `crates/*/tests/it/<area>` with a `mod <area>;` line in `tests/it/main.rs`. `grants/plan_channels.rs` is at 715 lines of code and `pages/forecast/words.rs` at 735: new tests get their own files.
- While iterating: `cargo nextest run -p <crate> --all-features`. Before each commit `cargo fmt` and `scripts/gate.sh`, the full gate; worktrees share its lock. Don't start full-workspace builds in parallel.
- Lints are strict; no `#[allow]`/`#[expect]` without `reason = "..."`. No `unwrap`/`expect` outside tests. No dead code: a field or type no task reads does not get added. In particular, nothing is added for #38 or #44.
- Never push without the owner's confirmation. Security review (the `security-review` skill or equivalent, findings in the PR) is required on every task marked **[security]**.

**Rules every implementer must follow, repeated in each task:** keep files under about 800 lines of code, splitting in the same change; write the tests first and watch them fail; never set `CARGO_TARGET_DIR`; run git only as `git -C <path>`, never `cd ... git`, `git -c` or `git tag`; no Claude attribution in commits or PRs (no `Co-Authored-By`, `Claude-Session` or "Generated with"), since this is a public repo.

## Review Focus

Where this is most likely to be wrong in a way tests written by the implementer won't catch. Reviewers start here.

1. **A hidden account showing through something other than its balance:** its currency in `by_currency`, a `rate_missing` for a currency only it uses, the point dates it extends, `first_on`, the FX endpoints saying which currencies are in use. → Task 8 `net_worth_channels.rs`; the tokens there hold a hidden account in JPY.
2. **A discontinued currency converted for ever at its last rate:** "most recent earlier rate" is correct for a weekend and wrong for the rouble, whose ECB series really ends on 2022-03-01, and ISK has a real gap from 2008 to 2018. → Task 1 `an_old_rate_is_stale`, Task 6 `a_discontinued_currency_says_so`.
3. **A number served as current when it isn't:** just after midnight before the job has filled today; straight after the migration, when the table is empty; after a display currency change. → Task 3 `reads_stop_at_through`, `a_row_in_another_currency_fails_loudly`, Task 6 `until_the_rebuild_there_is_no_number`.
4. **Incremental and full disagreeing:** a same-day overwrite, a snapshot back-dated between two others, a back-dated first snapshot, a correction to the display currency's own rate, which moves every other currency's accounts. → Task 3 proptest `incremental_matches_a_full_rebuild`.
5. **A rate correction that isn't a change:** the ECB prints `1.115` and could print `1.1150`. A re-fetch every 6 hours must rewrite nothing. → Task 2 `an_unchanged_rate_changes_nothing`.
6. **The ECB's real answers aren't all errors or all data:** an empty window is 200 with a 0-byte body; an unknown series is 404 with `application/problem+json`; a `startPeriod` in the future is 400 with an HTML page; integer rates such as `136` appear. All seen on 2026-11-02. → Task 4.
7. **A snapshot written without `upsert_balance`:** PR #77's `bring_in_kept_balances` inserts years of a provider account's kept days straight into `balance_snapshots` with one SQL statement, so a hook in `upsert_balance` alone never sees a bind's history, and the table would stay wrong until the next full rebuild. → Task 3 `a_bind_s_kept_balances_rebuild_the_account`, the proptest's bind operation, `every_snapshot_writer_is_known`; Task 6 `a_bind_s_history_reaches_net_worth`.
8. **A manual balance dated year 0001** (nothing bounds `on` today) making a million rows; one dated next month. → Task 3 `rows_start_no_earlier_than_1970`, `a_future_snapshot_has_no_row_until_its_day`.
9. **A closed account's last balance counting for ever,** or a card's debt vanishing when it closes (N9). → Task 1 `closed_account_rules`.
10. **A credit card balance with the wrong sign** inflating net worth, as in the forecast. → Task 1 `positive_liability_balance_is_a_caution`.
11. **The chart under the strict CSP on a phone:** an inline `style`, a colour literal or a font too small once a wide `viewBox` is scaled down. → Task 7. Look at it in a real browser at 375 px, both themes, both schemes.

## Rulings for the owner to review

Each is written up in spec §8.10. Most consequential first. "If wrong" is the cost of reversing it later.

| # | Ruling | Why | If wrong |
|---|---|---|---|
| N1 | **The forecast's `mixed_currencies` blocker stays.** | The owner's goal is what can be invested. Cash is held per currency, a floor is in one currency and a conversion's rate isn't known in advance, so one converted surplus would be a confident number hiding that; each currency's own surplus stays true. Needs rules of its own, so its own issue. | Purely additive later. Task 9 pins it with a test and fixes the texts saying "not built yet". |
| N2 | **A token's net worth sums accounts visible at `balances`, unmarked. FX endpoints need `all` at `balances`.** | The owner's decision. `balances` is what a balance grant is for. Stored rates depend on the currencies in use, so listing them would reveal a hidden account's currency. | A restricted token can't read raw rates, only the rate dates in its report. Loosening is one row. |
| N3 | **Every update happens in the same transaction as its change. `through` and `rebuild_needed` cover what can't.** | The simplest of the owner's two options that can't serve a stale number, with no background state machine. A full rebuild holds the write lock: see Task 3's measurement against the 15 s busy timeout. | If the Pi measures too slow for a display currency change, that one path moves to `rebuild_needed` (the state and the read answer exist anyway). |
| N5 | **One missing rate makes a period's `total` null, not a partial sum.** Sums per currency stay, and the account is named. | A dip in the line because a rate is missing looks like lost money. The chart shows a gap. | An agent gets a null on a household with one unpublished currency until N15's manual rates exist. One branch to change. |
| N6 | **`data-api.ecb.europa.eu`, per currency, CSV.** Not `www.ecb.europa.eu`'s `eurofxref-hist.xml`. | One host either way. Asks only for currencies in use (§6.10); 400 KB per currency against 8.2 MB for all 41; no XML crate. | The ECB sees the currencies asked for. The XML file would hide that, at the price of a new crate and 8.2 MB on each first use. It's one provider file. Owner decides, since it names the host. |
| N8 | **The display currency starts unset and is never guessed.** | Guessing from accounts leaks a currency to restricted tokens. A silent default is a wrong number in the wrong units. | One step for the owner after deploying, on `/admin/household`. |
| N9 | **A closed account stops counting on its closing day, unless it still owes.** | Its last balance is otherwise counted for ever, and §6.9 doesn't say. Closing has only a timestamp, so reopening rewrites those days. | Applied at read: one rule in Task 1. |
| N4 | **Rounded per account and day.** | Breakdowns add up, and a token's sum is the sum of its rows. | Totals differ from a one-go conversion by a few minor units. |
| N7 | **One currency contacts nobody; fetch every 6 hours.** | No outbound traffic that nothing needs. The ECB publishes once a working day around 16:00 CET, so four tries find it the same day in any zone. | One constant. |
| N14 | **Derived balances: Task 10, unbuilt until asked, then per account and a dry run by default.** | Not clearly right: it can't see pending charges a balance includes, a provider's history limit (90 days on SimpleFin) caps it, and a duplicate skews every day before it. | Nothing else depends on it. |

**Designed to fit later:** `include_in_net_worth`, liquidity and closing apply at read and rows are per account, so #38's buckets add a filter at read and a `NotCounted` reason, with no table change. #44's vests add accounts or snapshots; unvested grants simply have no snapshot. `mixed_currencies` removal only needs the rates this plan stores. A manual rate is one more `source` value on `fx_rates` and an endpoint.

## Task order and parallelism

| Wave | Tasks | Run how | Separate reviewer focuses on |
|---|---|---|---|
| 0 | 1 | Alone, merged first: everything consumes its types. | Recompute the golden example and the conversions by hand; the policy table **[security]** |
| 1 | 2, 4 | **In parallel**, one worktree and branch each. No shared files. | 2: canonical rates, no-op writes. 4: untrusted input, egress **[security]** |
| 2 | 3 | After 2 **and after PR #77 has merged**. May overlap 4. | Incremental ranges against N3; every path that writes a snapshot; query plans |
| 3 | 5, 6 | **In parallel**, after 3 (5 also after 4). Overlap is a line or two each in `cli.rs` and `testkit/mod.rs` (5 adds `AppState.fx`, 6 an argument to `Ledger::new`). | 5: the jobs can't delay sync; shutdown. 6: the answer against §6.9 and N2 **[security]** |
| 4 | 7, 8 | **In parallel**, after 6. | 7: the page against the API; CSP; the chart **[security]**. 8 is itself adversarial: a fresh agent given only §8.2, §8.10 N2 and the task **[security]** |
| 5 | 9 | **Alone**, after everything above. It reads everything and reviews the whole branch. | Docs against behaviour **[security]** |
| 6 | 10 | **Optional. Alone, after 9, only on the owner's word.** Nothing depends on it. | Whether it is right at all **[security]** |

Each task is one PR, in wave order. Worktrees: `git -C ~/Code/coffer worktree add -b <branch> ../coffer-<task> origin/main`. Merge `0019` and `0020` before `0021`.

---

### Task 1: Rates, conversion, daily values and the report, pure **[security]**

**Implementer rules:** files under about 800 lines of code; tests fail first; never set `CARGO_TARGET_DIR`; git only as `git -C <path>`, never `cd ... git`, `git -c` or `git tag`; no Claude attribution, public repo.

**Files:**
- Create: `crates/domain/src/fx.rs`, `crates/domain/src/fx/tests.rs`, `crates/domain/src/net_worth.rs`, `crates/domain/src/net_worth/{types,daily,periods,report,tests}.rs`
- Modify: `crates/domain/src/lib.rs`, `crates/domain/src/money.rs` (`Currency::all()`), `crates/domain/src/policy.rs` (table and module docs only), `crates/domain/src/policy/tests.rs`

**Interfaces:**
- Consumes: `Money`, `Currency`, `BalanceSource`, `LiquidityClass`, `AccountId`, `string_enum!`, `AccountAccess::scope`.
- Produces (`coffer_domain::fx`):
  - `FxRate`: units of a currency per one euro. `FromStr`/`Display`/serde as text, `ToSchema` with a pattern. Accepts `^[0-9]{1,10}(\.[0-9]{1,6})?$` with at most 10 digits in all, greater than zero. Canonical form trims trailing zeros, so `1.1150` and `1.115` are equal and print `1.115`. Exponents, signs and spaces are refused. Messages never echo input.
  - `RateHistory`: one currency's rates, `fn on_or_before(&self, on: Date) -> Option<(Date, FxRate)>`, never a later date.
  - `RateBook`: `fn rate(&self, currency: Currency, on: Date) -> RateLookup`; EUR is always 1 with no date. `RateLookup::{Euro, Found(Date, FxRate), Missing}`.
  - `fn convert(minor: i64, from: Currency, to: Currency, on: Date, book: &RateBook) -> Conversion`. `Conversion::{Same { minor }, Converted { minor, rate_on: Date }, Missing, TooLarge}`. `minor × to_rate / from_rate × 10^(to.exponent − from.exponent)` in checked `i128`, half-even, symmetric for negatives; `rate_on` is the older leg's date.
  - `string_enum` `FxSource { Ecb => "ecb" }`, `FxFetchStatus { Ready, NotPublished, Failing }`, `FxErrorCode { Unavailable, Malformed, Network }`. `RateObservation { on: Date, rate: FxRate }`. `FxCoverage { currency, status: FxFetchStatus, first_on: Option<Date>, last_on: Option<Date>, last_attempt_at: Timestamp, last_success_at: Option<Timestamp>, last_error: Option<FxErrorCode> }`.
  - Consts: `RATE_STALE_AFTER_DAYS: i32 = 7`.
- Produces (`coffer_domain::net_worth`):
  - `MIN_VALUE_DATE` = 1970-01-01. `string_enum` `ConversionState { Same, Converted, Missing, TooLarge, NoDisplayCurrency }`.
  - `DailyValue { account, on, balance_minor, balance_on, balance_source, display: Option<Currency>, display_minor: Option<i64>, rate_on: Option<Date>, conversion: ConversionState }`.
  - `fn daily_values(account: AccountId, currency: Currency, snapshots: &[SnapshotPoint], display: Option<Currency>, book: &RateBook, from: Date, through: Date) -> Vec<DailyValue>`: one row per day of `[max(from, first snapshot, MIN_VALUE_DATE), through]`, from the latest snapshot on or before it. The one function behind incremental updates, the daily fill and the full rebuild. `snapshots` holds the latest one before `from`, if any, and those up to `through`.
  - `periods.rs`: `string_enum` `NetWorthInterval { Day, Week, Month }`; `MAX_POINTS: usize = 1000`; `fn period_ends(from: Date, to: Date, interval) -> Result<Vec<PeriodEnd { on: Date, start: Date, partial: bool }>, NetWorthError>`. Weeks end on Sunday. `start` never precedes `from`. `partial` means `on` is not the natural end of its period.
  - `report.rs`: `fn net_worth_report(input: &NetWorthInput) -> Result<NetWorthReport, NetWorthError>`, whose points are `period_ends(from, min(to, today, through), interval)`. `NetWorthError { TooManyPoints, RangeInverted, OutOfRange }`, messages fixed.
  - `NetWorthInput { today, from, to, interval, through: Option<Date>, rebuild_needed: bool, display: Option<Currency>, stale_after_days: u16, account_points: bool, accounts: Vec<ReportAccount { id, name, currency, liquidity, include_in_net_worth, closed_on: Option<Date> }>, rows: Vec<DailyValue>, first_on: Vec<(AccountId, Date)>, coverage: Vec<CurrencyState { currency, reason: Option<RateMissingReason> }> }`. It is given visible accounts only, so it cannot leak what it never holds.
  - Output, `Serialize` and `ToSchema` under `openapi`, tagged enums as in `forecast::types`: `NetWorthReport { display_currency: Option<Currency>, interval, from, to, through: Option<Date>, status: NetWorthStatus /* firm, provisional, unavailable */, points: Vec<NetWorthPoint>, accounts: Vec<AccountSeries>, blockers: Vec<NetWorthBlocker>, cautions: Vec<NetWorthCaution> }`.
  - `NetWorthPoint { on, period_start, partial, total: Option<Money>, by_liquidity: Vec<LiquidityLine { liquidity, amount: Option<Money> }>, by_currency: Vec<Money>, unconverted: Vec<AccountId> }`. `by_currency` holds native sums, always. `total` and a class's `amount` are `null` when a counted account in them has no amount.
  - `AccountSeries { account, name, liquidity, currency, counted: bool, not_counted: Option<NotCountedReason /* ExcludedByFlag */>, first_on: Option<Date>, closed_on: Option<Date>, points: Vec<AccountPoint { on, balance: Money, balance_on, balance_source, counted: bool, amount: Option<Money>, rate_on: Option<Date>, conversion }> }`. `first_on` lets a reader see a jump is an account arriving, not money.
  - `NetWorthBlocker`: `DisplayCurrencyNotSet`, `Rebuilding`, `NoCountedAccounts`.
  - `NetWorthCaution`: `RateMissing { currency, reason: RateMissingReason /* NotFetchedYet, NotPublished, FetchFailing, ProviderOff, NoneThatEarly */, first_on: Date, last_on: Date }`, `RateStale { currency, on: Date, rate_on: Date, age_days: u32 }` (worst per currency), `AmountTooLarge { account, on }`, `BalanceStale { account, on: Date, age_days: u32 }`, `ToDateNotYetFilled { through: Date }`, `DailyValueMissing { account, first_on: Date, last_on: Date }`, `NothingCountedAfterLastPoint { from: Date, to: Date }`, `LiabilityBalancePositive { account, balance: Money }`, `ClosedAccountOwing { account, balance: Money }`.
- Rules the tests pin:
  - A row counts on day `d` when `include_in_net_worth`, and either not closed, or `d < closed_on`, or its balance is negative (N9).
  - With `rebuild_needed`: `points` and `accounts` empty, blocker `Rebuilding`. With `display` `None`: points with `by_currency` and null totals, blocker `DisplayCurrencyNotSet`. No visible counted account: blocker `NoCountedAccounts`, `points` empty.
  - `status`: `Unavailable` iff `blockers` non-empty, else `Firm` iff `cautions` empty, else `Provisional`.
  - Dates after `through` are not points; `ToDateNotYetFilled` fires when `min(to, today)` is later than `through`.
  - A day before an account's first row: no line for it, no caution. An included account with `first_on` on or before a point's day and no row on it is a hole, not money leaving: `total` and its class are null, `unconverted` names it, `by_currency` can't hold it, and `DailyValueMissing` says when. A period with no counted row and no hole is left out of `points`, so `all` from 1970 doesn't answer 680 empty months. When the last point returned ends before `min(to, today, through)`, `NothingCountedAfterLastPoint` says from when, and makes the report `provisional`, since a reader would take the last point as now; `closed_on` on the series explains a drop, with or without `account_points`. Totals past `i64`: `Err(OutOfRange)`.
  - A missing amount with no reason in `coverage` (the currency has rates, none that early) is `NoneThatEarly`.
- `policy.rs`: the rule table gains `Read FX rates and coverage | Balances`, with a paragraph: net worth sums what `AccountAccess::scope(AccountLevel::Balances)` holds, unmarked, and answers from visible accounts alone (spec §8.10 N2).

- [ ] **Step 1: Write failing tests.**
  - `fx/tests.rs`, exact values, rates from the ECB on 2026-10-30 unless marked: `rate_canonical_form` (`1.1150` = `1.115`, `136` parses, `0`, `-1`, `1e3`, ` 1`, `1.`, `.5`, 11 digits, 7 decimals refused); `euro_to_dollar` (100,000 EUR minor at 1.1172 → 111,720); `dollar_to_euro_rounds` (550,000 → 492,302; −30,000 → −26,853); `cross_rate_goes_through_euro` (GBP 100,000 at 0.8338, USD 1.1172 → 133,989); `exponents_are_respected` (JPY 1,000,000 at 175.68 → USD 635,929; USD 100,000 → JPY 157,250; KWD 1,234 at fixture 0.34 → JPY 638; EUR 100,000 → ISK 136,000 at `136`); `half_even_both_signs` (fixture rate 0.5 from EUR: 5 → 2, 7 → 4, −5 → −2, −7 → −4); `same_currency_needs_no_rate`; `no_later_rate_is_used` (first rate 1999-01-04, asked 1999-01-03 → `Missing`); `weekend_uses_friday` (Saturday 2026-10-31 → `rate_on` 10-30); `cross_rate_reports_the_older_date` (USD up to 10-30, GBP up to 10-28 → `rate_on` 10-28); `missing_either_leg_is_missing`; `limits_cannot_overflow` (`i64::MIN`, largest rate on one side, smallest on the other, JPY to KWD: `TooLarge`, no panic); proptest `conversion_is_antisymmetric` (`convert(-x) == -convert(x)`).
  - `net_worth/tests.rs`, `daily_values`: `one_row_per_day_from_the_first_snapshot`; `row_names_its_snapshot` (`balance_on`, `balance_source`); `rows_start_no_earlier_than_1970` (snapshot dated 0001-01-01 → first row 1970-01-01 with that `balance_on`); `no_display_currency_keeps_the_balance`; `too_large_is_a_state_not_a_failure`.
  - `periods`: `week_ends_on_sunday` (from Wed 2026-10-28 to Tue 2026-11-03 → 11-01 with `start` 10-28 then 11-03 partial); `month_ends_on_its_last_day` (Feb 2028 → 02-29); `last_period_is_partial`; `too_many_points_is_refused` (1,001 days; 1,000 passes); `inverted_range_is_refused`.
  - **Golden** `golden_week_of_net_worth`. `today` 2026-11-02, `through` 2026-11-02, display USD. EUR to USD rates (real ECB): 10-26 1.1188, 10-27 1.115, 10-28 1.1133, 10-29 1.113, 10-30 1.1172, none at the weekend. Accounts: Checking USD `cash`, 5,000.00 on 10-26 and 5,500.00 on 10-30. Livret EUR `cash`, 1,000.00 on 10-26. Card USD `short_term_liability`, −300.00 on 10-28. Side USD `cash`, `include_in_net_worth` false, 999.00 on 10-26. From 10-26 to 11-01, by day, `total`: 611,880; 611,500; 581,330; 581,300; 631,720; 631,720; 631,720. On 10-30: `by_liquidity` cash 661,720 and short-term −30,000, `by_currency` USD 520,000 and EUR 100,000, Livret's `rate_on` 10-30. On 10-31: Livret's `rate_on` 10-30, Card's `balance_on` 10-28. Side is listed `excluded_by_flag` with no points and in no sum. `Firm`.
  - `golden_by_week_and_month`: by week, one point on 11-01, 631,720, not partial. By month, 10-31 (631,720, not partial, `period_start` 10-26) and 11-01 (631,720, partial). `golden_in_euros`: display EUR on 10-30: Checking 492,302, Card −26,853, Livret 100,000 with `conversion` `same`, `total` 565,449, which is the sum of the rows.
  - `missing_rate_nulls_the_total_and_names_the_account` (a THB account with no rate: `total` null, its class null, others present, `by_currency` holds THB, `unconverted` names it, `RateMissing`, `Provisional`); `each_missing_reason_is_reported`; `an_old_rate_is_stale` (age 7 no caution, age 8 caution, 2022 rouble rate at 2026-11-02 says 1,707 days); `display_currency_not_set_has_sums_but_no_total`; `rebuild_needed_answers_no_number`; `no_counted_accounts_has_no_number_not_zero`; `include_flag_applies_at_read` (same rows, flag off: excluded from every past point); `closed_account_rules` (closed 10-29: an asset counts on 10-28 and not on 10-29, a negative balance still counts on 11-01 with `ClosedAccountOwing`, listed only for negative); `positive_liability_balance_is_a_caution`; `stale_balance_only_on_the_latest_point`; `dates_after_through_are_left_out_and_said`; `first_on_marks_an_account_arriving`; `totals_past_i64_are_out_of_range`; `account_points_false_keeps_the_summaries`.
  - Proptest over random rows: `totals_explain_themselves` (a non-null `total` is the sum of its accounts' `amount`s and of `by_liquidity`; `status_matches_lists`; `by_currency` sums the balances of counted rows).
  - `policy/tests.rs`: `net_worth_scope_is_what_balances_show` (uniform → `All`; named accounts at `balances` and above → those; `all` at `balances` → `All`).
- [ ] **Step 2: Run** `cargo nextest run -p coffer-domain --all-features` → FAIL.
- [ ] **Step 3: Implement.** Check the golden numbers by hand first; if one looks wrong, stop and say so rather than changing it.
- [ ] **Step 4: Run** → PASS, then `scripts/gate.sh`; security review of `policy.rs`.
- [ ] **Step 5: Commit** `feat(domain): exchange rates and the net worth calculation`.

### Task 2: Rates and the display currency in the store

**Implementer rules:** files under about 800 lines of code; tests fail first; never set `CARGO_TARGET_DIR`; git only as `git -C <path>`, never `cd ... git`, `git -c` or `git tag`; no Claude attribution, public repo.

**Files:**
- Create: `crates/store/migrations/0019_fx_rates.sql`, `crates/store/migrations/0020_display_currency.sql`, `crates/store/src/fx.rs`, `crates/store/tests/it/fx.rs`
- Modify: `crates/store/src/lib.rs`, `crates/store/src/household.rs`, `crates/store/tests/it/main.rs`

**Interfaces:**
- Consumes (Task 1): `FxRate`, `RateObservation`, `FxSource`, `FxFetchStatus`, `FxErrorCode`, `FxCoverage`, `RateBook`.
- Schema `0019`: `fx_rates (quote TEXT NOT NULL CHECK (length(quote) = 3 AND quote NOT GLOB '*[^A-Z]*' AND quote <> 'EUR'), on_date TEXT NOT NULL` with the repo's date `GLOB`, `rate TEXT NOT NULL CHECK (length(rate) BETWEEN 1 AND 11 AND rate NOT GLOB '*[^0-9.]*'), source TEXT NOT NULL CHECK (source IN ('ecb')), fetched_at INTEGER NOT NULL, PRIMARY KEY (quote, on_date)) STRICT, WITHOUT ROWID`. `fx_coverage (currency TEXT PRIMARY KEY` with the same CHECK`, status TEXT NOT NULL CHECK (status IN ('ready', 'not_published', 'failing')), first_on TEXT, last_on TEXT, last_attempt_at INTEGER NOT NULL, last_success_at INTEGER, last_error TEXT CHECK (last_error IN ('unavailable', 'malformed', 'network'))) STRICT`. No row means not fetched yet.
- Schema `0020`: `ALTER TABLE household_settings ADD COLUMN display_currency TEXT CHECK (length(display_currency) = 3 AND display_currency NOT GLOB '*[^A-Z]*')`.
- Store (`coffer_store`):
  - `StoreTx::upsert_fx_rates(&mut self, currency: Currency, source: FxSource, rates: &[RateObservation], now: Timestamp) -> Result<Option<Date>, StoreError>`: batched; writes only rows whose value differs; returns the earliest date that changed; updates `fx_coverage` to `ready`.
  - `StoreTx::record_fx_failure(&mut self, currency, status: FxFetchStatus, error: Option<FxErrorCode>, now)`: keeps `first_on` and `last_on`.
  - `Store::fx_coverage() -> Vec<FxCoverage>`; `Store::fx_rates(currency, from: Date, to: Date) -> Vec<RateObservation>`, newest first, at most 1,000 rows per call by the caller's range; `Store::currencies_in_use() -> Vec<Currency>`, distinct from `accounts`, closed included.
  - `pub(crate) async fn load_rate_book(conn, currencies, from, to) -> Result<RateBook, StoreError>`: for each currency, rates in `[from, to]` plus its latest before `from`, so a one-day update doesn't read 7,000 rows.
  - `Store::display_currency() -> Option<Currency>`, `StoreTx::display_currency()`, `StoreTx::set_display_currency(Currency)`: inserts the row with `ForecastSettings::default()`'s values when none exists, and leaves it otherwise. `set_forecast_settings` doesn't touch the column; test it. `settings_from_row` ignores it.

- [ ] **Step 1: Write failing tests** (`tests/it/fx.rs`): `rates_roundtrip_exactly` (`1.1172`, `136`, `20275.17`); `an_unchanged_rate_changes_nothing` (`1.115` then `1.1150`: returns `None`, `fetched_at` unchanged); `a_correction_returns_its_date`; `earliest_changed_date_is_returned`; `coverage_tracks_first_and_last`; `failure_keeps_what_is_known`; `no_rate_for_the_euro` (CHECK refuses `EUR`); `corrupt_rate_is_a_backend_error`; `rate_book_holds_the_rate_before_the_window` (asked for one Sunday: Friday's rate is there, Thursday's isn't loaded); `currencies_in_use_includes_closed_accounts`; `display_currency_starts_unset`; `setting_it_with_no_settings_row_keeps_the_defaults`; `forecast_settings_leave_the_display_currency` (and the other way round).
- [ ] **Step 2: Run** `cargo nextest run -p coffer-store --all-features -E 'test(fx)'` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** → PASS, then `scripts/gate.sh`.
- [ ] **Step 5: Commit** `feat(store): exchange rates and the display currency`.

### Task 3: The daily table

**Implementer rules:** files under about 800 lines of code; tests fail first; never set `CARGO_TARGET_DIR`; git only as `git -C <path>`, never `cd ... git`, `git -c` or `git tag`; no Claude attribution, public repo.

**Files:**
- Create: `crates/store/migrations/0021_account_daily_values.sql`, `crates/store/src/net_worth.rs`, `crates/store/tests/it/net_worth/{mod,incremental,rebuild,reads}.rs`
- Modify: `crates/store/src/lib.rs`, `crates/store/src/balances.rs`, `crates/store/src/kept_balances.rs` (from PR #77), `crates/store/src/fx.rs`, `crates/store/src/household.rs`, `crates/store/tests/it/main.rs`

**Interfaces:**
- Consumes (Task 1): `daily_values`, `DailyValue`, `ConversionState`, `MIN_VALUE_DATE`; (Task 2): `load_rate_book`, `upsert_fx_rates`, `set_display_currency`. Existing: `AccountScope`, `snapshot_from_row`, `account_from_row`.
- Schema `0021`:
  ```sql
  CREATE TABLE account_daily_values (
      account_id       TEXT NOT NULL REFERENCES accounts (id),
      on_date          TEXT NOT NULL, -- with the repo's date GLOB
      balance_minor    INTEGER NOT NULL,
      balance_on       TEXT NOT NULL, -- GLOB; CHECK (balance_on <= on_date)
      balance_source   TEXT NOT NULL CHECK (balance_source IN ('sync', 'manual')),
      display_currency TEXT,          -- as in 0020
      display_minor    INTEGER,
      rate_on          TEXT,          -- GLOB; CHECK (rate_on <= on_date)
      conversion       TEXT NOT NULL CHECK (conversion IN
          ('same', 'converted', 'missing', 'too_large', 'no_display_currency')),
      -- A CHECK for each state's shape: an amount only with same or converted,
      -- a rate date only with converted, no currency only with no_display_currency.
      PRIMARY KEY (account_id, on_date)
  ) STRICT, WITHOUT ROWID;
  CREATE TABLE net_worth_state (
      id             INTEGER PRIMARY KEY CHECK (id = 1),
      filled_through TEXT,
      rebuild_needed INTEGER NOT NULL CHECK (rebuild_needed IN (0, 1))
  ) STRICT;
  INSERT INTO net_worth_state (id, filled_through, rebuild_needed) VALUES (1, NULL, 1);
  ```
- Incremental, all private to `store/src/net_worth.rs` and called inside the same `StoreTx`, so no caller can forget them:
  - `upsert_balance`: when the value or source changed, and `rebuild_needed` is 0, and `on <= filled_through`: rebuild the account for `[on, next snapshot's date − 1 or filled_through]`. An unchanged write touches no row. Its callers (`Ledger::insert_account`, `set_manual_balance`, `write_balance`, the last of which a bind uses for the balance last reported) stay as they are.
  - `bring_in_kept_balances` (PR #77), which inserts with its own SQL and never passes through `upsert_balance`: read the earliest kept date for that provider account and currency before the insert and, when it wrote at least one row, and `rebuild_needed` is 0, rebuild the account for `[that date, filled_through]`. Kept days interleave with existing snapshots, so "up to the next snapshot" doesn't apply. It stays in the bind's transaction, so a bind and its history commit together. Its signature and audit count don't change.
  - `upsert_fx_rates`: when it returns a date `d`: every account in that currency, or, if it is the display currency, every account in another currency, for `[d, filled_through]`.
  - `set_display_currency`: when it changed, every account for its whole range, in that one transaction.
- Produces: `StoreTx::fill_net_worth(&mut self, through: Date) -> Result<u64, StoreError>`: rows for `(filled_through, through]`, then `filled_through = through`; never moves it back, so a changed time zone can't lose a day. `StoreTx::rebuild_net_worth(&mut self, through: Date) -> Result<u64, StoreError>`: deletes every row, computes every account from its first snapshot, sets `filled_through = max(through, the old one)` and `rebuild_needed = 0`. Inserts in batches of at most 500 rows. Each account is computed with `daily_values`, so all paths share one rule.
- Produces: `Store::net_worth_state() -> NetWorthState { filled_through: Option<Date>, rebuild_needed: bool }`, and the same on `StoreTx`. `Store::net_worth_data(&self, scope: &AccountScope, from: Date, to: Date, interval: NetWorthInterval) -> Result<NetWorthData, StoreError>`, **one read transaction**, which reads the state first and then the rows of `period_ends(from, min(to, filled_through), interval)`, so a fill or a rebuild can't split an answer: `NetWorthData { state, display: Option<Currency>, stale_after_days: u16, accounts: Vec<AccountRecord> /* in scope, oldest first */, rows: Vec<DailyValue>, first_on: Vec<(AccountId, Date)>, coverage: Vec<FxCoverage> }`. `rows` query is `account_id IN (…) AND on_date IN (…)`, always with explicit visible ids, never a scan for `All`. It reads rows of accounts with `include_in_net_worth = 1` only, and `coverage` only for currencies of `accounts` plus the display currency. A row whose `display_currency` isn't the current setting is `StoreError` corruption: loud, never shown.

- [ ] **Step 1: Write failing tests.**
  - `incremental.rs`: `a_new_snapshot_rebuilds_up_to_the_next` (snapshots on the 1st and the 20th, one written on the 10th: rows 10–19 change, the 20th doesn't); `a_same_day_overwrite_changes_that_stretch`; `a_back_dated_first_snapshot_makes_earlier_rows`; `an_unchanged_snapshot_writes_no_row`; `a_future_snapshot_has_no_row_until_its_day` (and the fill picks it up); `rows_start_no_earlier_than_1970` (a snapshot dated 0001-01-01 makes rows from 1970-01-01 only); `a_rate_rebuilds_its_currency_from_that_date` (others untouched); `a_rate_of_the_display_currency_rebuilds_the_others`; `a_new_rate_fills_a_weekend` (Monday's rate arriving doesn't touch Sunday); `a_correction_reaches_days_carried_from_it`; `a_new_display_currency_rebuilds_everything_in_its_transaction` (rolled back: rows unchanged); `a_failed_change_leaves_rows_as_they_were` (a transaction dropped without commit, for each of them); `a_bind_s_kept_balances_rebuild_the_account` (kept days 1–3 and 8, a snapshot already on day 5: rows for days 1–4 and 8 on come from the kept days, day 5 keeps its own value, in the one transaction); `a_kept_balance_in_another_currency_changes_nothing`; `a_bind_with_nothing_kept_touches_no_row`; `a_bind_s_days_after_through_wait_for_the_fill`; `every_snapshot_writer_is_known` (reads every file in `crates/store/src` through `CARGO_MANIFEST_DIR` for `INTO balance_snapshots`: only `balances.rs` and `kept_balances.rs`, so a new writer fails here until it refreshes the table); `nothing_is_written_while_a_rebuild_is_needed`.
  - `rebuild.rs`: the proptest **`incremental_matches_a_full_rebuild`**: random sequences of up to 12 of: create account (USD, EUR, JPY), upsert snapshot on a random date in a 40-day window (overwrites, back-dates, future dates), upsert rates (new, corrected, same), set display currency, fill to a later day, keep balances for a provider account and bring them in at bind (kept days before, between and on existing snapshots, some in another currency). After each sequence, dump the table, run `rebuild_net_worth` with the same `through`, and assert the dumps are equal, row for row. Set `cases` so it fits the 30 s limit; don't raise the limit. `rebuild_clears_the_flag`; `rebuild_is_idempotent`; `fill_is_idempotent`; `fill_never_moves_back`; `table_can_be_dropped_and_rebuilt_from_the_sources` (`DELETE` all rows, set the flag: rebuild reproduces them).
  - `reads.rs`: `reads_stop_at_through` (no row after it, whatever the dates asked); `scope_filters_in_the_query` (`Only`: rows, accounts, first dates and coverage of others absent); `excluded_accounts_have_no_rows_but_are_listed`; `a_row_in_another_currency_fails_loudly` (`execute_unchecked_for_tests` rewrites a row's currency: error, no number); `reads_use_the_primary_key` (`EXPLAIN QUERY PLAN` for 365 dates over `All`: uses `PRIMARY KEY`, contains no `SCAN`).
- [ ] **Step 2: Run** `cargo nextest run -p coffer-store --all-features -E 'test(net_worth)'` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Measure, and record it in the PR.** With a release build, a one-off test or scratch binary outside the repo: 30 accounts, 10 years of daily rows (about 110,000), two currencies. Time a display currency change and a read of 120 months with accounts. The rebuild must be well inside the store's 15 s busy timeout on the Pi or a machine as slow, and the read well under a second. If the rebuild isn't, stop and tell the owner: N3's fallback moves that path to `rebuild_needed`.
- [ ] **Step 5: Run** → PASS, then `scripts/gate.sh`.
- [ ] **Step 6: Commit** `feat(store): a daily table of account values`.

### Task 4: The rate provider and the ECB **[security]**

**Implementer rules:** files under about 800 lines of code; tests fail first; never set `CARGO_TARGET_DIR`; git only as `git -C <path>`, never `cd ... git`, `git -c` or `git tag`; no Claude attribution, public repo.

**Files:**
- Create: `crates/server/src/fx/mod.rs`, `crates/server/src/fx/ecb.rs`, `crates/server/tests/it/fx_ecb.rs`, `crates/server/tests/it/fixtures/ecb-usd.csv` (a few real lines)
- Modify: `crates/server/src/lib.rs`, `crates/server/src/config.rs`, `crates/server/src/config/tests.rs`, `crates/server/tests/it/egress_relay.rs`, `crates/server/tests/it/main.rs`, `docs/configuration.md`, `docs/container.md`

**Interfaces:**
- Consumes (Task 1): `FxRate`, `RateObservation`, `FxSource`, `FxErrorCode`; existing: `Egress::send_limited`, `HostAllowList`, `OutboundRequest`, `ProviderFuture`, `MockUpstream`, `Egress::for_tests`, `HostAllowList::loopback`.
- Produces (`crate::fx`):
  ```rust
  pub trait RateProvider: Send + Sync {
      fn source(&self) -> FxSource;
      /// Units of `currency` per euro, oldest first, from `since` when given.
      /// Reads no clock: `today` bounds what it accepts.
      fn fetch<'a>(&'a self, currency: Currency, since: Option<Date>, today: Date)
          -> ProviderFuture<'a, Result<Vec<RateObservation>, RateError>>;
  }
  ```
  `enum RateError { NotPublished, Unavailable { status: u16 }, Malformed, Network }`, `thiserror`, fixed messages, holding no text; `fn code(&self) -> Option<FxErrorCode>`, `None` for `NotPublished`.
  `fn rate_provider(config: &Config, egress: &Egress) -> Result<Option<Arc<dyn RateProvider>>, ConfigError>`: the one place rate providers are registered.
- Config: `COFFER_FX_PROVIDER`, `ecb` (default) or `none`; anything else is refused, naming the variable and not echoing the value.
- `fx/ecb.rs`: `pub const ECB_HOST: &str = "data-api.ecb.europa.eu"`. `Ecb::new(egress, allowed: HostAllowList)`, `Ecb::with_base(egress, allowed, base: Url)` under `testkit`.
  - Request: `GET https://data-api.ecb.europa.eu/service/data/EXR/D.<CCY>.EUR.SP00.A?format=csvdata&detail=dataonly`, with `&startPeriod=YYYY-MM-DD` when `since` is given. No key, no credential. Asked for `EUR` it answers `Ok(vec![])` with no request, since the ECB has no such series. The caller never passes a future `since`, which the ECB answers with a 400 HTML page.
  - Limits: `MAX_RATE_BODY = 2 MiB` through `send_limited`, at most 20,000 rows.
  - Answers: 200 with an empty body is `Ok(vec![])`, as for a window with no rates. 404 is `NotPublished`. Any other non-200, a redirect included, is `Unavailable { status }`. A body is read only on 200.
  - Parsing, strict: the first line must equal exactly `KEY,FREQ,CURRENCY,CURRENCY_DENOM,EXR_TYPE,EXR_SUFFIX,TIME_PERIOD,OBS_VALUE`; each row has exactly 8 fields, and fields 1–6 must equal `EXR.D.<CCY>.EUR.SP00.A`, `D`, `<CCY>`, `EUR`, `SP00`, `A`; `TIME_PERIOD` is a strict date, not after `today` + 1, not before 1990-01-01, strictly ascending; `OBS_VALUE` parses as `FxRate`. Any failure refuses the whole answer as `Malformed`. A line ending of `\n` or `\r\n` is accepted; nothing else is trimmed. Not UTF-8 is `Malformed`.
  - No part of a response reaches an error, a log line or the store.

- [ ] **Step 1: Write failing tests** (`fx_ecb.rs`, against `MockUpstream` with the shapes seen on 2026-11-02): `asks_for_the_series_and_nothing_else` (exact path and query with and without `since`, no `Authorization`, the `coffer/` user agent); `reads_the_published_format` (the fixture: `1.1188`, `1.115`, `136`); `an_empty_answer_is_no_new_rates`; `no_series_is_not_published` (404 with the real `application/problem+json` body); `an_html_error_page_is_unavailable` (400 with HTML: `Unavailable { status: 400 }`, and neither the `Debug` nor the `Display` of the error holds any of its text); `a_200_html_page_is_malformed`; `a_redirect_is_not_followed` (302 to a second mock, which sees no request); one test per refusal: `wrong_header`, `wrong_currency_in_a_row`, `wrong_key`, `wrong_denominator`, `too_few_or_many_fields`, `date_after_tomorrow`, `date_before_1990`, `dates_out_of_order_or_repeated`, `rate_with_an_exponent`, `zero_rate`, `not_utf8`; `one_bad_row_refuses_the_whole_answer`; `a_body_over_the_limit_is_malformed` (`EgressError::TooLarge` maps to `Malformed`, nothing parsed); `the_euro_is_never_asked_for` (no request reaches the mock); `too_many_rows_are_refused`; proptest `no_input_panics` (arbitrary bytes on 200 → `Ok` or `Malformed`).
  - Beside `ecb.rs`: `the_host_is_the_documented_one` (`ECB_HOST` appears in `docs/configuration.md` and spec §8.10 through `include_str!`); `the_allow_list_refuses_every_other_host` (`check_url` on `www.ecb.europa.eu` and `data-api.ecb.europa.eu.evil.example`).
  - `egress_relay.rs`: `the_rate_host_goes_through_the_relay_under_its_own_name` (as the existing relay tests: SNI and certificate are `data-api.ecb.europa.eu`, connection to the relay).
  - `config/tests.rs`: `fx_provider_defaults_to_ecb`; `fx_provider_none_is_accepted`; `unknown_fx_provider_is_refused_without_echo`.
- [ ] **Step 2: Run** `cargo nextest run -p coffer-server --all-features -E 'test(fx_ecb) | test(fx_provider)'` → FAIL.
- [ ] **Step 3: Implement.** Docs in this PR: `docs/configuration.md` gains the variable and, under *Outbound network*, replaces "in a later release, the exchange-rate source's host" with the host, what is sent (a currency code and a date), that the CNAME is the ECB's CDN, and that one currency contacts nobody. `docs/container.md`'s allow-list lines name it.
- [ ] **Step 4: Run** → PASS, then `scripts/gate.sh`; security review, as AGENTS.md requires for outbound HTTP.
- [ ] **Step 5: Commit** `feat(fx): a rate provider and the ECB's reference rates`.

### Task 5: The daily jobs and the rebuild command

**Implementer rules:** files under about 800 lines of code; tests fail first; never set `CARGO_TARGET_DIR`; git only as `git -C <path>`, never `cd ... git`, `git -c` or `git tag`; no Claude attribution, public repo.

**Files:**
- Create: `crates/server/src/tasks.rs` (`supervise`, `pause`, `stopped`, `Backoff` moved from `sync/scheduler.rs` with their tests), `crates/server/src/fx/job.rs`, `crates/server/tests/it/fx_job.rs`
- Modify: `crates/server/src/lib.rs`, `crates/server/src/sync/scheduler.rs`, `crates/server/src/fx/mod.rs`, `crates/server/src/cli.rs`, `crates/server/src/http/mod.rs` (`AppState.fx`), `crates/server/src/testkit/mod.rs` (`AppState`; keep it under 800, helpers go in the test module), `crates/server/tests/it/cli.rs`, `crates/server/tests/it/main.rs`, `docs/configuration.md`

**Interfaces:**
- Consumes: `RateProvider`, `rate_provider` (Task 4); `upsert_fx_rates`, `record_fx_failure`, `fx_coverage`, `currencies_in_use`, `display_currency` (Task 2); `fill_net_worth`, `rebuild_net_worth`, `net_worth_state` (Task 3).
- Produces (`crate::fx`): `FxService::new(store, clock, timezone, provider: Option<Arc<dyn RateProvider>>)`.
  - `#[must_use] pub fn needed(accounts: &[Currency], display: Option<Currency>) -> Vec<Currency>`: the distinct currencies, without EUR, and empty unless they came to more than one before EUR was removed (N7).
  - `#[must_use] pub fn due(now: Timestamp, needed: &[Currency], coverage: &[FxCoverage]) -> Vec<Currency>`, pure, the shape of `scheduler::due`: no row is due at once; `ready` after 6 h; `failing` after 1 h; `not_published` after 24 h. `const` each.
  - `FxService::fetch_due(&self) -> Result<u32, StoreError>`: one currency at a time, the provider's call outside any transaction, then one `StoreTx` for its rates, its rows and its coverage. `since` is `None` while it has no rate, else `last_on − 7 days`. `NotPublished` records `not_published`; another error records `failing` with its code and logs a `warn` with fixed text, the currency and the status.
  - `FxService::keep_table_current(&self) -> Result<(), StoreError>`: when `rebuild_needed`, rebuild to today; else fill to today when `filled_through < today`. Cheap when there's nothing to do. It logs `info` with the row count on a rebuild.
  - `pub fn spawn(fx: Arc<FxService>, shutdown) -> JoinHandle<()>`: two tasks under `tasks::supervise`, each with its own `TICK` of 60 s and backoff: the table first at start, then rates. Rates are a separate task because one fetch can take 60 s, and a day must not wait for it. With no provider, the rate task doesn't start. Neither shares anything with the sync worker: a stalled rate fetch cannot delay a sync.
- `serve_with` spawns it beside `scheduler::spawn`, stops it the same way, and logs only `err.is_panic()` on a join error.
- CLI: `ConfiguredCommand::RebuildNetWorth`, `coffer rebuild-net-worth`. As `backup`, it never creates or migrates a database just to do this: no database found is exit 1. It opens the store, computes today from `COFFER_TIMEZONE`, rebuilds in one transaction and prints the rows written and the last day. Safe beside a running server: it takes the write lock like any writer.

- [ ] **Step 1: Write failing tests.**
  - Beside `job.rs`: `one_currency_needs_nothing` (USD accounts, display USD → empty; JPY only → empty); `two_currencies_need_both_without_the_euro` (USD and EUR → USD; USD and GBP, display EUR → both); `display_currency_counts` (USD accounts, display EUR → USD); `due_rules` (a table as in `scheduler::due_rules`: each status either side of its interval; a currency no longer needed is not due).
  - `fx_job.rs`, with a scripted `RateProvider` in the test module: `first_use_fetches_the_whole_history_once` (`since` `None`, then `last_on − 7` on the next due run; a second tick inside 6 h asks nothing); `a_new_account_in_a_new_currency_is_backfilled_on_the_next_tick`; `single_currency_household_contacts_nobody`; `rates_and_rows_commit_together` (after one fetch, rows for that currency hold the new amounts); `an_unchanged_answer_rewrites_no_row`; `a_failure_is_recorded_and_retried_after_an_hour` (and logs no provider text, by `log_capture`); `not_published_is_recorded_not_failing`; `a_failure_for_one_currency_doesn_t_stop_the_next`; `the_table_fills_at_midnight` (`ManualClock` past household midnight: `filled_through` moves; zones differ as in the forecast's `today_uses_the_household_time_zone`); `a_start_after_downtime_fills_every_missed_day`; `the_migration_s_rebuild_runs_at_start`; `a_stalled_provider_does_not_stop_the_fill` (a provider that never answers: `filled_through` still moves); `a_stalled_provider_does_not_stop_a_sync` (with `FakeProvider`: a sync run still finishes); `no_provider_means_no_rate_task_and_still_fills`.
  - `cli.rs`: `rebuild_net_worth_reports_what_it_wrote`; `rebuild_net_worth_never_creates_a_database`.
  - Moved tests keep their names and still pass in `tasks.rs`.
- [ ] **Step 2: Run** `cargo nextest run -p coffer-server --all-features -E 'test(fx_job) | test(rebuild_net_worth) | test(job::)'` → FAIL.
- [ ] **Step 3: Implement.** `docs/configuration.md`: the command, under *Commands*.
- [ ] **Step 4: Run** → PASS, then `scripts/gate.sh`.
- [ ] **Step 5: Commit** `feat(fx): fetch rates and fill days forward`.

### Task 6: The net worth API and the display currency setting **[security]**

**Implementer rules:** files under about 800 lines of code; tests fail first; never set `CARGO_TARGET_DIR`; git only as `git -C <path>`, never `cd ... git`, `git -c` or `git tag`; no Claude attribution, public repo.

**Files:**
- Create: `crates/server/src/ledger/net_worth.rs`, `crates/server/src/http/api/fx.rs`, `crates/server/tests/it/net_worth/{mod,answer,conversion,settings,fx_api}.rs`
- Modify: `crates/server/src/ledger/{mod,settings}.rs`, `crates/server/src/cli.rs` and `crates/server/src/testkit/mod.rs` (`Ledger::new`'s new argument), `crates/server/src/http/api/{mod,openapi,reports,household}.rs`, `crates/server/tests/it/main.rs`, `crates/server/tests/it/household_settings.rs`, `docs/api/openapi.json`

**Interfaces:**
- Consumes: `net_worth_report`, `period_ends` and types (Task 1); `net_worth_data`, `Store::fx_coverage`, `Store::fx_rates` (Tasks 2, 3); `Ledger::today`, `AccessResolver`, `strict_date`.
- Produces (`ledger::net_worth`):
  - `NetWorthQuery { from: Option<Date>, to: Option<Date>, interval: Option<NetWorthInterval>, account_points: Option<bool> }` (`IntoParams`). Defaults: `to` today, `from` one year before `to`, `month`, `true`. Dates through `strict_date`, in the years 1970 to 2200, else 422.
  - `Ledger::net_worth(&self, actor: &Actor, query: NetWorthQuery) -> Result<NetWorthReport, LedgerError>`. First and before any read: `authorize(actor, Action::ReadHousehold)?`, then `access.scope(AccountLevel::Balances)`, with `AccountAccess` from the resolver. `period_ends(from, min(to, today), interval)` is checked first, from the query alone, so whether a range is refused can't depend on any data. `Ledger::new` gains whether a rate provider is configured, for `ProviderOff`. `closed_on` is `closed_at` in the household's zone. Maps `coverage` and that flag to `RateMissingReason`, per currency of a visible account or the display currency. `NetWorthError` → `LedgerError::Invalid` with its fixed text.
  - `Ledger::fx_coverage(&self, actor) -> FxCoverageList`, `Ledger::fx_rates(&self, actor, query: FxRatesQuery { currency: Currency, from: Date, to: Date })`: `ReadHousehold` then `authorize_every_account_access(actor, AccountLevel::Balances)`, the same refusal whatever exists (N2). At most 1,000 days in a range, else 422.
- Produces (`ledger::settings`): `HouseholdSettings { floor, include_income, default_horizon_months, stale_after_days, display_currency: Option<Currency> /* required */ }`, answered by both `/household/settings` routes in place of `ForecastSettings`, whose fields don't change. `ForecastSettingsPatch` gains `display_currency: Option<Currency>`: `null` or absent keeps it, as with a floor. Admin only, audited in `household.settings` as a before and after. A patch not changing it rebuilds nothing. An unknown code is 400 from `Currency`'s `Deserialize`. It reads the same for every token: it says how numbers are shown, not what the household holds.
- API: `GET /api/v1/reports/net-worth` (`getNetWorth`, `tags::REPORTS`, `read`), `GET /api/v1/fx/coverage` (`getFxCoverage`), `GET /api/v1/fx/rates` (`getFxRates`), tag `tags::FX` (new, described in `base()`). The descriptions teach: what a point means, that `total` is null with a reason and how to read `by_currency`, `rate_on`, `balance_on`, `through`, each blocker and caution, and that a token with account grants gets the sum of what it can see, unmarked.

- [ ] **Step 1: Write failing tests.**
  - `answer.rs` (built through the real API and router, with rates and the fill through a store handle in `mod.rs`): `the_api_matches_the_golden_week` (Task 1's household: the seven totals by day; by week and by month; `by_liquidity` and `by_currency` on 10-30; `rate_on` and `balance_on`; `firm`); `the_golden_week_in_euros` (one `PATCH`: `total` on 10-30 is 565,449, with no other call); `defaults_are_a_year_by_month`; `today_uses_the_household_time_zone`; `to_after_through_is_left_out_and_said`; `until_the_rebuild_there_is_no_number` (a store with `rebuild_needed`: 200, `unavailable`, `rebuilding`, no points); `include_in_net_worth_applies_with_no_rebuild` (a `PATCH` of the flag moves past totals at once, with no row written); `changing_liquidity_moves_the_breakdown_with_no_rebuild`; `a_closed_account_stops_counting` (through the API, in the household's zone); `an_empty_household_has_no_number`; `too_many_points_is_422`; `inverted_range_is_422`; `a_datetime_is_refused` (400 or 422 as `TypedQuery` answers, without echo); `reading_writes_nothing` (audit count and table dump unchanged); `a_new_snapshot_shows_at_once` (a `PUT` balance for today, then the report, same request order, new number); **`a_bind_s_history_reaches_net_worth`** (with `FakeProvider` and a `ManualClock`: an unbound provider account synced on three days, the job's fill running between them, is in no answer and raises no caution; bound on the fourth day, the very next report has those three days' balances for it, with `balance_on` and `balance_source` `sync`, in the totals of those past dates; and `rebuild_net_worth` then changes no row); `a_bind_into_a_new_currency_reports_a_missing_rate_until_fetched`.
  - `conversion.rs`: `a_missing_rate_is_reported_not_omitted` (a THB account: `total` null, `by_currency` holds THB, `unconverted` names it, the `rate_missing` reason `not_fetched_yet`, then `not_published` after a recorded 404, then `fetch_failing`, and `provider_off` on a server with `COFFER_FX_PROVIDER=none`); `a_day_before_the_first_rate_is_none_that_early`; `a_discontinued_currency_says_so` (RUB rates ending 2022-03-01, today 2026-11-02: an amount, with `rate_stale` and its age); `weekend_points_report_friday`; `a_display_currency_with_no_rates_reports_every_foreign_account`; `display_currency_not_set_has_sums_only`; `amount_too_large_is_a_caution_not_a_failure`; `a_total_too_large_is_422`.
  - `settings.rs`: `display_currency_starts_null`; `admin_sets_it_and_it_is_audited`; `write_token_is_refused`; `null_or_absent_keeps_it`; `unknown_code_is_400_without_echo`; `same_value_writes_nothing`; `forecast_fields_are_unchanged` (existing `household_settings.rs` assertions, with the new field added).
  - `fx_api.rs`: `coverage_and_rates_for_a_full_token`; `all_at_balances_may_read`; `account_grants_are_refused_alike` (403 "this action needs balances access to every account", byte-identical with and without hidden accounts); `range_too_long_is_422`; `unknown_currency_is_400`.
- [ ] **Step 2: Run** `cargo nextest run -p coffer-server --all-features -E 'test(net_worth) | test(household_settings)'` → FAIL.
- [ ] **Step 3: Implement;** regenerate `docs/api/openapi.json`. Fix any Spectral finding in the code that generates the document.
- [ ] **Step 4: Run** → PASS, then `scripts/gate.sh`; security review.
- [ ] **Step 5: Commit** `feat(api): net worth history and the display currency`.

### Task 7: The net worth page and its chart **[security]**

**Implementer rules:** files under about 800 lines of code; tests fail first; never set `CARGO_TARGET_DIR`; git only as `git -C <path>`, never `cd ... git`, `git -c` or `git tag`; no Claude attribution, public repo.

**Files:**
- Create: `crates/server/src/http/pages/net_worth.rs`, `crates/server/src/http/pages/net_worth/{chart,words}.rs`, `crates/server/tests/it/pages/net_worth.rs`
- Modify: `crates/server/src/http/pages/{mod,layout,household}.rs`, `crates/server/assets/css/base.css`, `crates/server/src/themes/tests.rs`, `crates/server/tests/it/pages/mod.rs`

**Interfaces:**
- Consumes (Task 6): `Ledger::net_worth`, `NetWorthQuery`, `NetWorthReport`, `update_forecast_settings`; existing: `PageSession`, `viewer`, `Frame`, `page`, `csrf_field`, `format_minor_units`, `PageForm`.
- Produces: `GET /net-worth`, merged in `pages::router`: session cookie only, a bearer token never. `Nav::NetWorth`, a "Net worth" link beside "Forecast" for every signed-in member. Not added to `PAGES` in `http/browser_reports/fields.rs`.
- Handler: `state.ledger.net_worth(&session.actor(), query)`, and it computes nothing else. A refusal renders the usual error page.
- `?range=` parses into a typed enum: `3m` by day, `1y` (the default) by week, `5y` by month, and `all` from `MIN_VALUE_DATE` by month, which stays under `MAX_POINTS` since periods with no row are left out and 1,000 months is 83 years. Anything else is 400. No free dates: a range is chosen by plain links in one row above the chart. No form and no script.
- Renders, all text through maud's escaping and through an exhaustive `match` on blockers and cautions, so a new one fails the build until it has words:
  - The answer as a hero figure (proportional figures, the same font), the status in words, every caution, and when each rate and balance dates from where it matters.
  - The chart (below), then a **table of the same numbers** (period, total, marked partial or naming what couldn't convert), which is how the values are read without a pointer or sight.
  - The latest point by liquidity class and by currency, and an accounts table: name, balance, its date, amount, rate date, and first day. A note when an account arrives inside the range, so a jump reads as what it is.
  - With no display currency: sums per currency, no chart, and for an admin a link to `/admin/household`.
- `chart.rs`, pure: `pub fn line_chart(points: &[ChartPoint { on: Date, value: Option<i64>, label: String, title: String }], size: ChartSize) -> Markup`.
  - **Integers only.** `y = top + (max − v) × height / (max − min)` in checked `i128`, in tenths of a pixel; `max == min` draws a flat line in the middle. Ticks at 1, 2 or 5 × 10^k of minor units, at most 5, labelled through `format_minor_units`. A range crossing zero draws the zero line; otherwise no false baseline is drawn, as a line chart needs none.
  - Per the dataviz guidance: one series and one hue, so no legend (the heading names it); a 2 px line; solid hairline grid and axis in `--border`; a marker of at least 8 px with a 2 px surface ring on the latest point, and one direct label there, not a number on every point; at most 6 x labels. A null value breaks the line, and is never bridged or drawn as 0. A lone point draws a marker. No dual axis, no area under a line that may go negative.
  - Hover without script: each period has a transparent hit band at least 24 px wide (`.chart-hit`, `pointer-events: all`) holding an SVG `<title>` with date and amount. Tooltips only add to the table.
  - The page renders the chart twice, `ChartSize::Wide` (720 × 280) and `ChartSize::Narrow` (360 × 240, fewer ticks), and CSS shows one by width, so a phone doesn't scale 12 px labels down to 6. `display: none` also takes the other out of assistive technology. Each has `role="img"` and an `aria-label` stating range, first and last values.
  - No `style=`, no `fill="#…"`, no `<script>`, no external reference. Classes only (`.chart-line`, `.chart-grid`, `.chart-zero`, `.chart-mark`, `.chart-label`) in `base.css`, reading `--accent`, `--border`, `--muted`, `--text`, `--surface`, so both themes and both schemes work with no new token. Status colours aren't used for the line.
- `/admin/household`: a second card with a `select` of `Currency::all()`, the current one selected, anti-forgery token, `ManageHousehold` checked first, through `Ledger::update_forecast_settings`, so it's audited the same. A note that changing it recomputes history. It shows no amount, so that page stays in `PAGES`.

- [ ] **Step 1: Write failing tests.**
  - Beside `chart.rs`: `coordinates_are_exact` (points 0, 50, 100 on a 200-tall plot: y 2000, 1000, 0 in tenths, plus `top`); `a_flat_series_draws_in_the_middle`; `a_gap_breaks_the_line` (two `M` commands, no `L` across it); `a_lone_point_draws_a_marker`; `all_null_draws_no_line_and_says_so`; `largest_values_cannot_overflow` (`i64::MIN` and `i64::MAX` in one series); `ticks_are_round` (1, 2 or 5 × 10^k, at most 5, for ranges from 1 to 10^18); `zero_line_only_when_crossing`; `negative_net_worth_stays_in_the_plot`; `at_most_six_x_labels`; `titles_are_escaped`; `no_style_colour_or_script` (no ` style=`, no `#` colour, no `<script`, no `href` in the markup).
  - `pages/net_worth.rs`: `page_matches_the_api_on_the_golden_week` (same household, same query: hero 6,317.20, each period's total in the table, formatted, `firm`; then a missing rate: the chart has a gap, the table says which account, and `provisional` is in words); `each_blocker_and_caution_has_words`; `no_display_currency_shows_sums_and_a_way_forward` (an admin gets the link, a member doesn't); `names_are_escaped`; `signed_out_visitor_is_sent_to_log_in`; `bearer_token_cannot_read_the_page`; `page_has_the_default_csp_and_no_inline_script_or_style`; `page_offers_no_write_action` (the only form is sign-out); `bad_range_is_400`; `ranges_link_to_each_other`; `all_range_stays_under_the_point_limit` (a household from 1970); `nav_links_to_net_worth_for_every_member`; `reading_the_page_writes_nothing`; `browser_reports_withhold_this_page` (a report from `/net-worth` has `page` `other` and `[withheld]`).
  - Household page: `admin_sets_the_display_currency` (audited; the report now converts); `member_is_refused_whatever_is_sent`; `forged_form_fails_whole`; `csrf_is_required`.
  - `themes/tests.rs`: `chart_line_contrasts_with_its_surface` (`--accent` on `--surface`, at least 3:1, every theme and scheme).
- [ ] **Step 2: Run** `cargo nextest run -p coffer-server --all-features -E 'test(pages::net_worth) | test(chart::)'` → FAIL.
- [ ] **Step 3: Implement.** No API change, so `docs/api/openapi.json` stays as it is.
- [ ] **Step 4: Look at it.** Run a real server against a manual household with several currencies. In a real browser at 1280 px and 375 px, on both themes in light and dark, check for label collisions, overflow and the console (no CSP violation). Then read the result against the dataviz skill's `references/anti-patterns.md`. Put what was checked in the PR.
- [ ] **Step 5: Run** → PASS, then `scripts/gate.sh`; security review (it touches a page with financial data and a new write form).
- [ ] **Step 6: Commit** `feat(pages): a net worth page with a chart`.

### Task 8: Adversarial proof that a token sees only its own sum **[security]**

**Implementer rules:** files under about 800 lines of code; tests fail first; never set `CARGO_TARGET_DIR`; git only as `git -C <path>`, never `cd ... git`, `git -c` or `git tag`; no Claude attribution, public repo.

Give this to a fresh agent with only §8.2, §8.10 N2, the `policy.rs` rule table and this task, so it attacks the code instead of confirming it. Tests only, unless one fails; a failure is fixed in the service that leaked, in this PR.

**Files:**
- Create: `crates/server/tests/it/grants/net_worth_channels.rs`
- Modify: `crates/server/tests/it/grants/{mod,proptests}.rs` (`mod`, `Household`, `read_uris`)

**Interfaces:**
- Consumes: the API of Task 6; existing `world()`, `grant`, `on`, `Household`, `read_uris`, `check_no_body_reveals`.
- Produces: no new interface.

- [ ] **Step 1: Extend `read_uris`** with `/api/v1/reports/net-worth?from=2026-01-01&interval=day`, the same by `month`, `/api/v1/fx/coverage` and `/api/v1/fx/rates?currency=JPY&from=2026-01-01&to=2026-12-31`. Extend `Household` so a hidden account may hold a large distinctive balance in a currency no visible account uses, with rates stored. `no_endpoint_leaks_hidden_accounts` then also asserts no body holds that balance, its name, its id or its currency code.
- [ ] **Step 2: Add tests** (`net_worth_channels.rs`), each comparing a token's answer with one from a **second world that really lacks the hidden account**, same clock and settings, byte for byte: `a_token_s_sum_is_what_it_can_see`; `hidden_account_changes_no_byte` (hidden balance, its currency, a hidden account closed or excluded, a hidden account first seen earlier so its days would extend the range); `a_hidden_currency_raises_no_caution` (hidden JPY with no rate: the token's `total` is a number, status unchanged); `a_hidden_stale_balance_raises_no_caution`; `a_hidden_positive_liability_raises_no_caution`; `a_token_seeing_nothing_reads_like_an_empty_household`; `balances_level_is_enough_and_shows_no_transaction_data`; `grant_change_applies_at_the_next_request`; `new_account_needs_an_all_grant_to_count`; `fx_endpoints_refuse_alike` (byte-identical with and without a hidden account); `settings_read_the_same_for_every_token` (`display_currency`); `an_admin_token_with_grants_gets_the_setting_not_the_sum` (changing the display currency returns settings only); `a_session_sees_everything` (uniform access, for contrast); `no_problem_body_names_an_account` (a 422 on the same query is the same for every token).
- [ ] **Step 3: Run** `cargo nextest run -p coffer-server --all-features -E 'test(grants)'`. Any failure is a leak: fix it in the service and keep the test. A needed `.config/nextest.toml` override is measured, per AGENTS.md; `no_endpoint_leaks` already has one.
- [ ] **Step 4: Run** `scripts/gate.sh`; security review.
- [ ] **Step 5: Commit** `test(grants): net worth shows a token only its own accounts`.

### Task 9: The forecast's words, docs and whole-branch review **[security]**

**Implementer rules:** files under about 800 lines of code; tests fail first; never set `CARGO_TARGET_DIR`; git only as `git -C <path>`, never `cd ... git`, `git -c` or `git tag`; no Claude attribution, public repo.

**Files:**
- Create: `docs/agent-net-worth.md`
- Modify: `crates/server/src/http/pages/forecast/words.rs` (the sentence at line 160, staying under 800), `crates/server/tests/it/forecast/unknowns.rs`, `docs/agent-forecast.md`, `docs/agent-usage.md`, `docs/threat-model.md`, `docs/configuration.md`, `README.md`, `AGENTS.md`

**Interfaces:** none (a sentence, one pinning test, documentation and review).

- [ ] **Step 1: Pin N1, test first.** `unknowns.rs`: `rates_don_t_lift_the_mixed_currencies_blocker` (USD and EUR accounts, rates stored and a display currency set: still `unavailable`, `mixed_currencies`, `surplus: null`, each currency's own surplus unchanged). It passes at once by design; it fails only if someone wires FX in. Then the texts that say FX isn't built: `blocker_sentence` becomes "A forecast doesn't convert between currencies, so there is no single total" (its test at line 507 follows), and `docs/agent-forecast.md` line 469 says a forecast doesn't convert, with net worth as where conversion lives.
- [ ] **Step 2: `docs/agent-net-worth.md`** (linked from `docs/agent-usage.md`, as the forecast's is): the report, with one example; each period and `partial`; `total` null and `by_currency`; `rate_on`, `balance_on`, `first_on`, `through`; every blocker and caution with what to do; "never report a `provisional` number without its cautions"; a restricted token's answer is the sum of what it sees; the FX endpoints and who may call them; the display currency. `agent-usage.md`'s *Account grants* gains the rule.
- [ ] **Step 3: `docs/threat-model.md`.**
  - Boundary 5 names the rate source: public data, no credential, untrusted input, read strictly.
  - Design: *Account grants* gains net worth (N2) and why it differs from a forecast (R2): what could show (currency, rate state, age, dates) and how each is closed; the FX endpoints' `all` at `balances`. "One page holds financial data" becomes two (N13), and says what the display currency form adds: admin only, anti-forgery token, audited.
  - Accepted residual risks, each its own: **a compromised or wrong rate source can feed false rates**, moving reported net worth until corrected. Rates are bounded in form, not in value, since a real rate can move. A correction to a currency's last 7 days of rates arrives by itself; an older one isn't fetched. The forecast never reads rates. **The ECB learns which currencies a household asks about, and when,** with its address or the relay's; the relay learns only that it asks. **A restricted token is not told its sum is partial,** by design: an agent holding `balances` on one account may report that as the household's net worth, and the docs tell it not to. **A display currency change holds the write lock while it rebuilds** (with the number from Task 3's measurement), and **a back-dated manual balance rewrites at most that account's days since 1970**; only a `write` token can do it, and it is audited.
- [ ] **Step 4: The rest.** `docs/configuration.md`: `COFFER_TIMEZONE` also decides net worth's days; that nothing breaks while the host isn't allowed. `README.md`: the status lines say net worth and FX exist. `AGENTS.md`: crate map gains the domain's `fx` and `net_worth`, the store's rates and daily table with the rule that **only `store/src/net_worth.rs` writes `account_daily_values`**, `fx` and `tasks` in the server, and the page; and under Conventions, that rates and rows are the audited-mutation rule's only exception (N10).
- [ ] **Step 5: Track what this plan defers.** Check `gh issue list` and, **with the owner's confirmation**, open an issue for each of: a forecast across currencies (N1); and entering a rate by hand for a currency the ECB doesn't publish. #58's unbound accounts are PR #77's, so they need no issue. Then, with confirmation, close #58 or trim it to what's left. No TODOs in source.
- [ ] **Step 6: Run** `cargo run -p coffer-server -- openapi | diff - docs/api/openapi.json` (no output) and `scripts/gate.sh` → PASS.
- [ ] **Step 7: Commit** `docs: net worth and exchange rates`.
- [ ] **Step 8: Whole-branch review** with a reviewer who didn't implement it: `security-review` over the plan's diff, a read of the golden week against §6.9 by hand, and each row of *Spec coverage*. Fix findings and commit. Record in the PR what was and wasn't verified. That includes a real run through `cargo run -p coffer-server -- serve` with a manual household in two currencies **against the real ECB host**, and whether the Pi's firewall has been changed: a CI pass proves neither.

### Task 10 (optional, off until the owner asks): Balances derived before the first sync **[security]**

**Implementer rules:** files under about 800 lines of code; tests fail first; never set `CARGO_TARGET_DIR`; git only as `git -C <path>`, never `cd ... git`, `git -c` or `git tag`; no Claude attribution, public repo.

Not clearly right, so it stays a member's explicit act on one account, and nothing else depends on it. Skip it and the plan is complete. PR #77's kept balances shrink the need: only days before the first sight of an account are left. Its limits, which its docs must state: a provider's balance may or may not include pending charges; history stops where transactions do (90 days on SimpleFin, 730 on Plaid); and a later change to a transaction doesn't move a derived balance until it is derived again.

**Files:**
- Create: `crates/store/migrations/0022_derived_balances.sql`, `crates/domain/src/net_worth/derive.rs`, `crates/server/src/ledger/derived_balances.rs`, `crates/server/tests/it/net_worth/derived.rs`
- Modify: `crates/domain/src/accounts.rs` (`BalanceSource::Derived`), `crates/domain/src/net_worth/{types,report}.rs` (caution), `crates/store/src/{balances,kept_balances,forecast,net_worth}.rs`, `crates/server/src/sync/connections/binding.rs`, `crates/server/src/ledger/mod.rs`, `crates/server/src/http/api/{accounts,mod}.rs`, `crates/server/src/http/pages/net_worth/words.rs`, `crates/store/tests/it/net_worth/mod.rs`, `docs/api/openapi.json`, `docs/agent-net-worth.md`, `docs/threat-model.md`

**Interfaces:**
- Consumes: `upsert_balance`, the rebuild from Task 3, `AccountLevel`, the posted transactions of `crates/store/src/transactions.rs`.
- Schema `0022`: `balance_snapshots` can't change a CHECK in place, so: create a new table with `source IN ('sync', 'manual', 'derived')`, copy every row, drop the old, rename; nothing references it. `DROP TABLE account_daily_values`, recreate it with the same three in `balance_source`, and `UPDATE net_worth_state SET rebuild_needed = 1`, which is what a derived table is for.
- Domain: `fn derive_balances(anchor: DerivationAnchor { on: Date, balance_minor: i64 }, days: &[DayTotal { on: Date, net_minor: i64 }], earliest: Date) -> Result<Vec<DerivedBalance { on: Date, balance_minor: i64 }>, NetWorthError>`. The balance at the end of day `d` is that of `d + 1` less the net posted on `d + 1`. A balance holds until the next snapshot, so one is written for each day whose end differs from the day before's, and for the day before the earliest transaction. Checked arithmetic.
- Store: `StoreTx::replace_derived_balances(account, &[DerivedBalance], now) -> Result<u32, StoreError>`: deletes this account's `derived` rows, writes the new ones, **never replacing a `sync` or `manual` row** (`ON CONFLICT DO NOTHING`), and rebuilds its rows, all in one transaction. A later sync or manual write on a day replaces a derived row as last write wins. So must a bind, whose two paths (PR #77) refuse to overwrite: `bring_in_kept_balances` and `binding.rs`'s `balance_on` check treat a `derived` row as absent, so an estimate never outranks a reported balance. `forecast.rs`'s two balance joins and `latest_balance` read `source <> 'derived'`.
- `Ledger::derive_balances(&self, actor, account, request: DeriveRequest { dry_run: Option<bool> /* default true */ })`: `authorize_account(.., Write)`, hidden answers like missing. Refused with 422 and a fixed text unless liquidity is `cash` or `short_term_liability`, and a `sync` or `manual` balance exists to work back from. The anchor is the oldest reported balance. Counts posted, non-pending transactions, without `flag_source_removed`. Any `flag_possible_duplicate` in the range is 409 "resolve the possible duplicates first", since one would skew every earlier day. Audited as `balance.derive`, with the account, the first and last dates and the count; a dry run writes nothing. `DELETE /api/v1/accounts/{id}/balances/derived` removes them, audited, with the same access.
- API: `POST /api/v1/accounts/{account_id}/balances/derive` answers `{ dry_run, balances: [...] , anchor }`. Report: `NetWorthCaution::DerivedBalances { account, first_on: Date, last_on: Date }` when a point in the range rests on one; `balance_source` `derived` on its points; page words through the exhaustive match.

- [ ] **Step 1: Write failing tests.** Domain: `walks_back_exactly` (anchor 1,000 at the end of the 10th; posted −100 on the 10th and +50 on the 8th, the 8th the earliest: the 9th and the 8th end on 1,100 and the 7th on 1,050, so it writes the 7th at 1,050 and the 8th at 1,100); proptest `walking_forward_returns_to_the_anchor`. Store: `migration_keeps_every_snapshot`; `derived_never_replaces_a_reported_balance`; `a_later_sync_replaces_a_derived_day`; `a_bind_s_kept_balance_replaces_a_derived_day`; `incremental_matches_a_full_rebuild` still passes with derived rows in its operations. API (`derived.rs`): `dry_run_is_the_default_and_writes_nothing`; `derive_writes_and_audits`; `reports_say_derived` (the `balance_source`, the caution and the page's words); `a_forecast_ignores_derived_balances` (as of a past date with only a derived balance: `balance_missing`); `account_list_never_shows_a_derived_balance_as_latest`; `investment_accounts_are_refused` (`invested`, `illiquid`, `long_term_liability`); `no_reported_balance_is_refused`; `possible_duplicates_are_refused`; `pending_and_removed_transactions_are_not_counted`; `removal_is_audited_and_rebuilds`; `running_it_twice_replaces`; `hidden_account_answers_like_a_missing_one`; `read_token_is_refused`.
- [ ] **Step 2: Run** `cargo nextest run -p coffer-server --all-features -E 'test(derived)'` → FAIL.
- [ ] **Step 3: Implement;** regenerate `docs/api/openapi.json`. Docs: what it can't know, as above; threat model: a `write` token can write estimates into history, marked, audited and removable.
- [ ] **Step 4: Run** → PASS, then `scripts/gate.sh`; security review.
- [ ] **Step 5: Commit** `feat(api): balances derived before the first sync`.

## Spec coverage

| Requirement | Task |
|---|---|
| §6.9 net worth: per day, week, month; latest snapshot on or before; that date's rate | 1, 3, 6 |
| §6.9 breakdown by liquidity class and by account | 1, 6, 7 |
| #58 per-account series with the snapshot date | 1, 6 |
| #58 daily table, incremental updates, full rebuild, test they match, days filled forward | 3, 5 |
| `include_in_net_worth` applied at read | 1, 6 |
| §6.10 pluggable provider, ECB, daily | 4, 5 |
| §6.10 backfill on first use; only currencies in use | 5 |
| §6.10 cross rates via EUR; earlier rate reported; missing rate reported | 1, 6 |
| §6.1 exact rates, half-even | 1; N4 |
| §6.5, §8.2 grants; hidden accounts | 1, 3, 6, 8; N2 |
| §6.6 audit | 6, 7; exception N10 |
| §8.5 outbound network, the exact host | 4, 9 |
| §6.4, §3 the page; strict CSP | 7; N13 |
| Forecast's `mixed_currencies` | Stays: N1; pinned in 9 |
| #58 backfill before the first sync | 10, optional; N14 |
| #58 unbound accounts' history | PR #77 keeps it and brings it in at bind; 3 and 6 carry it into the table and the report |
| #38 buckets, #44 RSUs, manual rates, member display currency | Not in scope: N15 |
