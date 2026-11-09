# 07t — Future projection, range and target Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Project net worth 25 years ahead in today's money, in the display currency: from today's net worth (the house left out by default), monthly saving from the last 12 months' income minus spending, and real growth (default 5%, with a range of ±1.5 points), with the date the target is reached and its range; stored settings (target, growth, a monthly saving override, whether the house counts); API and MCP. Closes #159.

**Architecture:** A new pure domain module `coffer_domain::future` (exact integer maths, rates in basis points) that takes today's net worth and its history (from the net worth report the ledger already builds), the last 12 months' income and spending per currency (from the plan report's actuals queries, reused), the rate book and the stored settings, and answers the three lines, the target's reach and the inputs it used. The store adds `future_settings` (migration `0036`) and a `future_data` read; the ledger joins them; `GET /api/v1/future`, `PATCH /api/v1/future/settings`, MCP `get_future` and `set_future_settings`. No page (07v).

**Tech Stack:** Rust 2024, sqlx (SQLite), axum, utoipa, rmcp, proptest.

**Spec:** `docs/superpowers/specs/2026-11-07-coffer-money-redesign-design.md` §4.7 (chart, range, target headline, inputs with defaults from real data, the house), §2 (non-goals: no Monte Carlo, no tax), §6 ("Later: Future projection, target"). Overview: 07t.

**Depends on:** 07a (#140) only; may run in parallel with phases 2–6. **Branch / worktree:** `money-07t-future-projection`, `../coffer-07t`. **Migration:** `0036`.

## Global Constraints

- Everything in the overview's "Rules for every unit" applies.
- Constants (planner's calls): `HORIZON_YEARS = 25`, `DEFAULT_GROWTH_BP = 500`, `GROWTH_SPREAD_BP = 150`, settable growth `-500..=1500` bp, amounts up to 10^15 minor units.
- The line, one step per month from today, for `g` = growth − spread, growth, growth + spread:
  ```
  v(0)     = start
  v(m + 1) = round_half_away_from_zero(v(m) × (120_000 + g) / 120_000) + monthly saving
  ```
  (a twelfth of the annual real rate each month), all in `i128`, `OutOfRange` past `i64`. The answer gives one point per year (`today + k years`, k = 0..=25) per line; the reach dates are found month by month: the first month whose value is at or above the target, on the likely line (`likely`), the optimistic (`earliest`) and the pessimistic (`latest`); `null` when not within 25 years; `already: true` when `start ≥ target`.
- **The house** (no link between a loan and an asset exists in coffer today): left out when `include_house` is false = every account in the `illiquid` liquidity class plus every `mortgage` account, one rule `future::is_house(liquidity, kind)` (planner's call, open question 1). Left-out value is reported (`house_left_out`).
- **Monthly saving from data:** over the last 12 complete months (`[first of this month − 12 months, first of this month)`), counting only months that start on or after the household's first transaction's month: per currency, income = the net of `income` categories, spending = −(net of `expense` categories) + uncategorized outflows on spendable household accounts (the plan report's `REPORT_ACTUALS` and `REPORT_UNCATEGORIZED`, so Future and plan versus actual agree on what spending is); each converted to the display currency at today with `fx::convert`; monthly = total ÷ months, rounded half away from zero. Transfers and accounts not the household's never count.
- **Today's money:** everything is in the display currency at today's rates; growth is real, so no inflation step.
- Authorization: reading needs the forecast's access (`authorize_forecast`: it sums every account and reads transactions), decided before anything is read; changing settings needs `WriteHousehold` and that access (planning data, as plan items; the answer reveals the projection), audited (`household.future.update`).
- Words: "net worth", "monthly saving", "real growth", "target", "today's money". No "Monte Carlo", "probability", "%" of success.

## Review Focus

1. **Exactness and rounding.** Growth 0 → `v(m) = start + m × saving` exactly; one step of 100,000.00 at 500 bp → 100,416.67 (41,666.666… rounds up); negative values round away from zero symmetrically. → Task 1 `zero_growth_is_linear`, `one_step_rounds_half_away_from_zero`, `negative_values_round_symmetrically`.
2. **The range's order.** For `start ≥ 0` and saving ≥ 0, pessimistic ≤ likely ≤ optimistic at every point, and earliest ≤ likely ≤ latest. → Task 1 proptest `the_range_is_ordered`.
3. **The house.** A 400,000 property and its 250,000 mortgage are left out by default (start lower by 150,000, `house_left_out` 150,000); a car loan (kind `loan`) stays. → Task 1 `the_house_and_its_mortgage_are_left_out`, Task 3 `the_house_is_left_out_by_default`.
4. **Saving from too little data, or in a currency without a rate.** 5 months of history → average over 5 with `saving_from_fewer_months`; EUR without a rate → left out with `saving_currency_not_converted`. → Task 1 `saving_averages_the_months_there_are`, `a_currency_without_a_rate_is_left_out`.
5. **Settings in an old display currency.** A target set in USD after the display currency becomes EUR is ignored with `target_in_other_currency`, never converted silently. → Task 1 `a_target_in_another_currency_is_ignored`.
6. **Restricted tokens.** → Task 3 `a_restricted_token_is_refused`.

---

### Task 1: The projection in the domain

**Files:**
- Create: `crates/domain/src/future.rs` (module docs, constants, `is_house`, re-exports), `crates/domain/src/future/types.rs`, `crates/domain/src/future/projection.rs`, `crates/domain/src/future/saving.rs`, `crates/domain/src/future/tests.rs`, `crates/domain/src/future/tests/{projection.rs, saving.rs, properties.rs}`
- Modify: `crates/domain/src/lib.rs` (`pub mod future;`), `AGENTS.md` (crate map)

**Interfaces:**
  ```rust
  pub fn compute_future(input: &FutureInput) -> Result<Future, FutureError>;   // FutureError::OutOfRange
  pub fn is_house(liquidity: LiquidityClass, kind: AccountKind) -> bool;
  pub fn grow(start: i128, saving: i128, growth_bp: i32, months: u32) -> Result<i128, FutureError>;
  pub struct FutureSettings { pub target: Option<Money>, pub growth_bp: Option<i32>,
                              pub monthly_saving: Option<Money>, pub include_house: bool }   // + validate()
  pub struct FutureInput { pub today: Date, pub display: Option<Currency>, pub settings: FutureSettings,
      pub history: Vec<HistoryPoint>,      // month ends, oldest first, today last
      pub saving_months: Vec<SavingMonth>, // per month and currency: income_minor, spending_minor
      pub first_transaction: Option<Date>, pub rates: RateBook }
  pub struct HistoryPoint { pub on: Date, pub total_minor: Option<i64>, pub house_minor: i64 }
  pub struct Future {                      // Serialize, ToSchema
      pub as_of: Date, #[schema(required)] pub display_currency: Option<Currency>,
      pub status: FutureStatus,            // firm | provisional | unavailable
      pub settings: FutureSettings,        // as stored
      #[schema(required)] pub inputs: Option<FutureInputs>,
      pub history: Vec<FuturePoint>,       // house rule applied, gaps left out
      pub likely: Vec<FuturePoint>, pub pessimistic: Vec<FuturePoint>, pub optimistic: Vec<FuturePoint>,
      #[schema(required)] pub target: Option<TargetReach>,
      pub blockers: Vec<FutureBlocker>, pub cautions: Vec<FutureCaution> }
  pub struct FutureInputs { pub start: Money, pub house_left_out: Money, pub include_house: bool,
      pub monthly_saving: Money, pub saving_source: SavingSource /* data | set */,
      pub monthly_income: Money, pub monthly_spending: Money, pub saving_months: u8,
      pub growth_bp: i32, pub spread_bp: i32, pub horizon_years: u8 }
  pub struct TargetReach { pub target: Money, pub already: bool, #[schema(required)] pub likely: Option<Date>,
                           #[schema(required)] pub earliest: Option<Date>, #[schema(required)] pub latest: Option<Date> }
  // FutureBlocker (tagged `kind`): no_display_currency, no_net_worth_today
  // FutureCaution (tagged `kind`): saving_from_fewer_months { months }, no_saving_history,
  //   saving_currency_not_converted { currency }, target_in_other_currency { currency },
  //   saving_in_other_currency { currency }
  ```
  `monthly_income` and `monthly_spending` are answered separately because 07u's "a year without income" needs them.

- [ ] **Step 1: Write the failing tests** (USD minor units unless said):
  - `projection.rs`: `zero_growth_is_linear`, `one_step_rounds_half_away_from_zero`, `negative_values_round_symmetrically`, `the_mockups_headline` (start 600,000.00, saving 3,000.00, 500 bp, target 2,000,000.00 → `likely`, `earliest`, `latest` as computed by `grow`, earliest ≤ likely ≤ latest), `a_target_already_reached`, `a_target_never_reached_is_null`, `one_point_per_year_for_25_years` (26 points, `on` = today + k years), `the_house_and_its_mortgage_are_left_out`, `include_house_counts_them`, `a_target_in_another_currency_is_ignored`, `a_saving_set_overrides_the_data`, `no_display_currency_blocks`, `no_net_worth_today_blocks`, `too_large_is_out_of_range`.
  - `saving.rs`: `saving_is_income_minus_spending_over_twelve_months` (12 months of 5,000 income and 4,200 spending incl. 200 uncategorized → 800.00), `saving_averages_the_months_there_are`, `a_partial_first_month_is_left_out`, `no_history_saves_nothing_with_a_caution`, `a_currency_without_a_rate_is_left_out`, `two_currencies_convert_at_today`.
  - `properties.rs`: `the_range_is_ordered`, `zero_growth_is_linear_for_any_inputs`, `history_never_includes_the_house_when_left_out`.
- [ ] **Step 2: Run** `nice -n 15 env -u CARGO_TARGET_DIR cargo nextest run -p coffer-domain --all-features -j 3 -E 'test(future)'`. Expected: FAIL to compile.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the domain crate. Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07t commit -m "Project net worth 25 years ahead in today's money, with a range and a target"`

### Task 2: Settings and the read in the store

**Files:**
- Create: `crates/store/migrations/0036_future_settings.sql`, `crates/store/src/future.rs` (+ unit tests beside the code as the crate does)
- Modify: `crates/store/src/lib.rs`, `crates/store/src/plan_report.rs` (`REPORT_ACTUALS`, `REPORT_UNCATEGORIZED` and their row readers `pub(crate)`), `crates/store/src/fx.rs` (none if `load_rate_book` is already `pub(crate)`)

**Interfaces:**
- Migration: `future_settings (id INTEGER PRIMARY KEY CHECK (id = 1), target_minor INTEGER CHECK (target_minor BETWEEN 1 AND 1000000000000000), target_currency TEXT CHECK (…as 0030…), growth_bp INTEGER CHECK (growth_bp BETWEEN -500 AND 1500), saving_minor INTEGER CHECK (saving_minor BETWEEN -1000000000000000 AND 1000000000000000), saving_currency TEXT CHECK (…), include_house INTEGER NOT NULL DEFAULT 0 CHECK (include_house IN (0, 1)), CHECK ((target_minor IS NULL) = (target_currency IS NULL)), CHECK ((saving_minor IS NULL) = (saving_currency IS NULL))) STRICT;` and `INSERT INTO future_settings (id) VALUES (1);` (a fresh table, so the cross-column CHECKs are allowed).
- Store: `future_settings() -> FutureSettings` (on `Store` and the write transaction), `set_future_settings(&FutureSettings)`, `future_data(today) -> FutureData { settings, saving_months, first_transaction, rates }` in one read transaction, reading the two report queries over the 12-month window.

- [ ] **Step 1: Write the failing tests** — `fresh_settings_are_the_defaults`, `settings_round_trip`, `a_target_without_its_currency_is_refused_by_the_table`, `future_data_reads_twelve_complete_months` (a transaction this month is not in it), `future_data_sees_the_first_transaction`.
- [ ] **Step 2: Run** `… -p coffer-store --all-features -j 3 -E 'test(future)'`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07t commit -m "Store Future's settings and read a year of income and spending"`

### Task 3: Ledger, API and MCP

**Files:**
- Create: `crates/server/src/ledger/future.rs`, `crates/server/src/http/api/future.rs`, `crates/server/tests/it/future.rs` (+ `mod future;` in `tests/it/main.rs`)
- Modify: `crates/server/src/ledger/mod.rs`, `crates/server/src/ledger/net_worth.rs` (split `net_worth_by_day` so the report-building half, after authorization, is callable from `future.rs`; `ledger/net_worth.rs` stays under 800 lines), `crates/server/src/http/api/{mod.rs, openapi.rs}` (tag `future`), `crates/server/src/http/api/mcp/tools.rs` (or its area file), `crates/server/tests/it/mcp/parity/mod.rs`, `docs/api/openapi.json`, `docs/agent-future.md` (new: the line, the defaults, the house rule, saving from data, what isn't modelled: tax, inflation beyond real growth, Monte Carlo), `docs/agent-usage.md`

**Interfaces:**
- Ledger: `pub async fn future(&self, actor) -> Result<Future, LedgerError>` — `authorize_forecast`; the net worth report by month from the first value (capped at `MAX_POINTS`) with account points; `HistoryPoint.house_minor` = Σ of each point's `amount` over accounts where `is_house(liquidity, kind)` (kinds from the store's accounts); `future_data(today)`; `compute_future`. `pub async fn update_future_settings(&self, actor, patch: FutureSettingsPatch) -> Result<Future, LedgerError>` — `Patch` fields (absent keeps, `null` clears); an amount must be in the display currency (422 "set a display currency first" without one; 422 for another currency); same settings again writes nothing; audited.
- API: `GET /api/v1/future` (`getFuture`), `PATCH /api/v1/future/settings` (`updateFutureSettings`, needs `write`). The description states the formula, the defaults and ranges, the house rule, how saving is found, today's money, that it is not a prediction or advice, and the status rules.
- MCP: `get_future` (`Needs::Read`; lead: "Where net worth is heading over 25 years in today's money, with a range and when the target is reached. Give the range with the likely date, never the likely date alone; never report it as a promise or advice."), `set_future_settings` (`Needs::Write`; "Only values the member asked for.").

- [ ] **Step 1: Write the failing tests** (`tests/it/future.rs`, clock on 2027-01-15, display currency USD):
  - `future_answers_from_net_worth_and_saving` — accounts and a year of income and spending seeded through the API → `inputs.start`, `inputs.monthly_saving`, 26 points per line.
  - `the_house_is_left_out_by_default`, `include_house_puts_it_back`.
  - `settings_round_trip_and_are_audited`, `the_same_settings_write_nothing`, `an_amount_in_another_currency_is_refused`, `growth_out_of_range_is_refused`.
  - `a_restricted_token_is_refused` (GET and PATCH, same body whatever exists), `a_read_token_cannot_change_settings`.
  - `no_display_currency_answers_unavailable`.
  - Parity rows for `get_future` and `set_future_settings`.
- [ ] **Step 2: Run** `… -p coffer-server --all-features -j 3 -E 'test(future) | test(net_worth) | test(parity)'`. Expected: FAIL.
- [ ] **Step 3: Implement**; regenerate `docs/api/openapi.json`; write the docs.
- [ ] **Step 4: Run**, then `uptime` and `nice -n 15 scripts/gate.sh`. Expected: PASS, gate green.
- [ ] **Step 5: Commit, push, PR** — `git -C ../coffer-07t commit -m "Serve the Future projection and its settings through the API and MCP"`; push `money-07t-future-projection`; PR "Future projection, range and target", "Closes #159", listing the planner's calls (monthly compounding at a twelfth of the rate, yearly points, the house rule, growth range) and what was verified locally versus by CI.

## Open questions for the owner

1. **What is "the house"?** Coffer has no link between a loan and the asset it is secured by. Recommended: every `illiquid` account plus every `mortgage` account (a car stays out, its car loan stays in, which errs low). The alternative, a "secured by" link on loan accounts, is a change to accounts, their API and form, worth its own issue if the rule proves wrong.
2. **Growth compounding.** Recommended: a twelfth of the annual real rate each month (5% reads as 5.12% a year), simple and exact; the alternative, the exact monthly root, needs fixed-point roots for no visible difference at this precision.
