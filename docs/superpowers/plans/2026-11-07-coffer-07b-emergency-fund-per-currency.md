# 07b — Emergency fund per currency Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the household's single safety floor with an emergency fund target per currency (a fixed amount in that currency, or N months of its outflows), stored, audited, in the API and MCP, and read by the forecast currency by currency. Closes #141.

**Architecture:** A new domain type `EmergencyFundTarget` keyed by `Currency` replaces `SafetyFloor` everywhere (no alias, no dead type). The store keeps one row per currency in a new `emergency_funds` table and drops the floor columns from `household_settings` by rebuilding it (no table references it). The forecast's book takes each currency's target where today it takes the one floor. The `FloorNotSet` blocker becomes `EmergencyFundNotSet { currency }`, still a blocker: 07h turns it into a caution.

**Tech Stack:** Rust 2024, sqlx (SQLite), axum, utoipa, rmcp, proptest. No new crates.

**Spec:** `docs/superpowers/specs/2026-11-07-coffer-money-redesign-design.md` §4.4, §10.4 (and §10.5 for what 07h does next). Overview: `docs/superpowers/plans/2026-11-07-coffer-07-money-redesign-overview.md` (planner's calls 3, 4, 5).

**Branch / worktree:** `money-07b-emergency-fund`, `../coffer-07b`. **Owns migration `0030`.**

## Global Constraints

- Everything in the overview's "Rules for every unit" applies.
- Words (§3): "emergency fund" in API descriptions, MCP texts and pages; "safety floor" disappears from user-facing text, and "floor" from the API.
- Money is exact: integer minor units, `i128` intermediates, checked; never a float. A fixed target is 0 to 10^15 minor units; months are 1 to 60.
- An emergency fund is a figure of the whole household's plan: reading it needs the forecast's access (`ReadHousehold` and `all` at `transactions`), as the floor did; a token below that reads `emergency_funds: null`. Setting or removing one needs `ManageHousehold` (admin), audited with the old and new canonical text.
- Amounts are never netted across currencies: one target per currency, applied only to that currency's totals.
- Migrations are STRICT tables whose CHECKs mirror the domain types; the store parses every value it reads.
- The months variant keeps today's meaning in this unit: N months of that currency's recurring outflows. 07e narrows it to must pay and essentials.

## Review Focus

1. **The carry-over losing the owner's floor.** A fixed floor must become exactly its currency's target; a months floor must become a months target for every currency in use; no floor writes no row. → Task 1 `migration_carries_the_floor_over` (three seeded cases, run through the real `Migrator`).
2. **A currency in use without a target.** With USD targeted and EUR not, the forecast must block EUR's number (still a blocker in this unit) and must not apply USD's target to EUR. → Task 2 `a_target_applies_only_to_its_currency`.
3. **A restricted token learning the fund.** `GET /household/settings` and the `PUT` answer must read `emergency_funds: null` for a token below forecast access, whether or not any is set. → Task 3 `a_restricted_token_reads_no_emergency_fund`.
4. **A target for a currency the household doesn't use.** It is allowed (a household may plan a new account) and changes no answer. → Task 2 `an_unused_currency_target_changes_nothing`.
5. **Removing what isn't there** is a 404 with nothing audited; setting the same target again writes nothing. → Task 3 `removing_a_missing_target_is_not_found`, `setting_the_same_target_writes_nothing`.

---

### Task 1: The domain type, the table and the carry-over

**Files:**
- Create: `crates/domain/src/plan/emergency_fund.rs`, `crates/store/migrations/0030_emergency_funds.sql`, `crates/store/src/emergency_funds.rs`, `crates/store/tests/it/emergency_funds.rs` (+ `mod emergency_funds;` in `tests/it/main.rs`)
- Modify: `crates/domain/src/plan.rs` (module, re-exports; rename `PlanError::InvalidFloor` → `InvalidEmergencyFund`, `InvalidFloorMonths` → `InvalidEmergencyFundMonths`, `MAX_FLOOR_MONTHS` → `MAX_EMERGENCY_FUND_MONTHS`), `crates/domain/src/plan/settings.rs` (remove `SafetyFloor` and `ForecastSettings::floor`), `crates/store/src/household.rs` (floor columns gone), `crates/store/src/lib.rs` (module), `crates/store/tests/it/foundation.rs` and `crates/store/tests/it/forecast/*` where they build settings with a floor

**Interfaces:**
- Produces (`coffer_domain::plan`):
  ```rust
  /// How much cash to keep untouched in one currency.
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
  #[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
  #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
  pub enum EmergencyFundTarget {
      /// A fixed amount in the currency's minor units, 0 to 10^15.
      Fixed { minor_units: i64 },
      /// This many months, 1 to 60, of the currency's planned outflows.
      MonthsOfOutflows { months: u8 },
  }
  impl EmergencyFundTarget {
      pub fn validate(&self) -> Result<(), PlanError>;   // InvalidEmergencyFund / InvalidEmergencyFundMonths
      pub fn canonical(&self) -> String;                 // "fixed:100000", "months_of_outflows:3"
  }
  /// One currency's target, as the API lists it.
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
  #[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
  #[serde(deny_unknown_fields)]
  pub struct EmergencyFund { pub currency: Currency, pub target: EmergencyFundTarget }
  /// Every currency's target; a currency missing has none.
  pub type EmergencyFunds = std::collections::BTreeMap<Currency, EmergencyFundTarget>;
  ```
  `ForecastSettings` keeps `include_income`, `default_horizon_months`, `stale_after_days` and stays `Copy`.
- Produces (`coffer_store`): `Store::emergency_funds(&self) -> Result<EmergencyFunds, StoreError>`; `StoreTx::emergency_funds(&mut self)`, `StoreTx::put_emergency_fund(&mut self, currency: Currency, target: &EmergencyFundTarget) -> Result<(), StoreError>`, `StoreTx::delete_emergency_fund(&mut self, currency: Currency) -> Result<bool, StoreError>` (true when a row went).
- Schema `0030`:
  ```sql
  -- The emergency fund (money redesign §4.4, §10.4): at most one target per currency, a fixed
  -- amount in it or some months of its planned outflows. No row: none set for that currency.
  CREATE TABLE emergency_funds (
      currency TEXT PRIMARY KEY
          CHECK (length(currency) = 3 AND currency NOT GLOB '*[^A-Z]*'),
      kind     TEXT NOT NULL CHECK (kind IN ('fixed', 'months_of_outflows')),
      minor    INTEGER CHECK (minor BETWEEN 0 AND 1000000000000000),
      months   INTEGER CHECK (months BETWEEN 1 AND 60),
      CHECK ((kind IS 'fixed' AND minor IS NOT NULL AND months IS NULL)
          OR (kind IS 'months_of_outflows' AND months IS NOT NULL AND minor IS NULL))
  ) STRICT, WITHOUT ROWID;

  -- The old floor: a fixed one is its currency's target.
  INSERT INTO emergency_funds (currency, kind, minor)
  SELECT floor_currency, 'fixed', floor_minor FROM household_settings
  WHERE floor_kind = 'fixed';

  -- A months floor applied to every currency; it becomes a target for each one in use.
  INSERT INTO emergency_funds (currency, kind, months)
  SELECT used.currency, 'months_of_outflows', s.floor_months
  FROM household_settings AS s,
       (SELECT currency FROM accounts WHERE include_in_forecast = 1 AND closed_at IS NULL
        UNION SELECT currency FROM planned_items WHERE direction = 'outflow' AND active = 1
        UNION SELECT display_currency FROM household_settings WHERE display_currency IS NOT NULL)
       AS used
  WHERE s.floor_kind = 'months_of_outflows';

  -- household_settings without the floor. Nothing references it, so the plain rebuild is safe.
  CREATE TABLE household_settings_new (
      id                     INTEGER PRIMARY KEY CHECK (id = 1),
      include_income         INTEGER NOT NULL CHECK (include_income IN (0, 1)),
      default_horizon_months INTEGER NOT NULL CHECK (default_horizon_months BETWEEN 1 AND 60),
      stale_after_days       INTEGER NOT NULL CHECK (stale_after_days BETWEEN 1 AND 365),
      display_currency       TEXT CHECK (length(display_currency) = 3 AND display_currency NOT GLOB '*[^A-Z]*'),
      timezone               TEXT CHECK (length(timezone) BETWEEN 1 AND 64)
  ) STRICT;
  INSERT INTO household_settings_new
  SELECT id, include_income, default_horizon_months, stale_after_days, display_currency, timezone
  FROM household_settings;
  DROP TABLE household_settings;
  ALTER TABLE household_settings_new RENAME TO household_settings;
  ```
  Before writing it, check `0020_display_currency.sql` for the exact `display_currency` CHECK and copy it verbatim; `rg household_settings crates/store/migrations` must show no other column.

- [ ] **Step 1: Write the failing tests**
  - Domain (`emergency_fund.rs` unit tests): `fixed_and_months_bounds` (0 and 10^15 ok, −1 and 10^15+1 refused; months 1 and 60 ok, 0 and 61 refused), `canonical_text`, `serde_is_tagged_and_strict` (`{"kind":"fixed","minor_units":100}`; unknown field, `"percent"`, `3.5` months refused).
  - Store (`tests/it/emergency_funds.rs`): `put_get_and_delete` (put USD fixed, EUR months, read both; put USD again replaces; delete EUR → true, again → false); `migration_carries_the_floor_over` — following the pattern of `crates/store/src/lib.rs:566` (apply migrations below 30 with `sqlx::raw_sql`, seed, apply 30): (a) fixed floor 1000.00 EUR → `{EUR: Fixed{100000}}`; (b) months floor 3 with an open counted USD account, an active EUR outflow and display currency CHF → three months rows; (c) no floor → no row; and in each case `include_income`, horizon, stale days, display currency and time zone are unchanged.
- [ ] **Step 2: Run** `… -p coffer-domain … -E 'test(emergency_fund)'`, `… -p coffer-store … -E 'test(emergency_funds)'`. Expected: FAIL to compile.
- [ ] **Step 3: Implement** the type, the migration, the store module, and remove the floor from `ForecastSettings`, `household.rs` (`floor_from_row`, `floor_columns` go) and their tests. The workspace won't compile past the store until Task 2; run the store's tests only.
- [ ] **Step 4: Run** the two filtered runs. Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07b commit -m "Keep an emergency fund target per currency instead of one floor"` (a WIP commit; the gate runs at the end of Task 3).

### Task 2: The forecast reads one target per currency

**Files:**
- Modify: `crates/domain/src/forecast.rs` (`add_floor` → `add_emergency_funds`, `blockers`), `crates/domain/src/forecast/{types.rs, cautions.rs, book.rs}`, every domain forecast test that sets `floor` (`rg -l 'floor' crates/domain/src/forecast/tests`), `crates/store/src/forecast.rs` (`ForecastData.emergency_funds`), `crates/server/src/ledger/forecast.rs` (`ForecastOptions` and `WhatIfRequest`), `crates/server/src/http/api/forecast.rs` (descriptions), `crates/server/src/http/pages/forecast.rs` and `forecast/words.rs` (+ `words/tests.rs`), `crates/server/tests/it/forecast/*`
- Test: `crates/domain/src/forecast/tests/goals_and_floor.rs` → rename to `goals_and_emergency_fund.rs`; `crates/domain/src/forecast/tests/blockers.rs`

**Interfaces:**
- `ForecastInput.floor: Option<SafetyFloor>` → `emergency_funds: EmergencyFunds`.
- `Forecast.floor` → `emergency_funds: Vec<EmergencyFund>` (the targets of the currencies in `totals`, sorted by currency). `CurrencyTotals.floor` → `emergency_fund: Money`, and `cash_needed` is `planned_outflows + goals + emergency_fund - planned_inflows`.
- `Blocker::FloorNotSet` → `Blocker::EmergencyFundNotSet { currency: Currency }`, one per currency in the book without a target, sorted.
- `Caution::MonthsFloorIsZero` → `Caution::EmergencyFundMonthsIsZero { currency, months }`; `Caution::CashDipsBelowFloor` → `Caution::CashDipsBelowEmergencyFund { month, currency, projected_cash }`.
- `ForecastOptions.floor` → `emergency_funds: Option<EmergencyFunds>`; `WhatIfRequest.floor` → `emergency_funds: Option<Vec<EmergencyFund>>` (replaces the household's whole set for that what-if; a currency named twice is 422 "each currency's emergency fund once"; each target validated).
- `add_emergency_funds(input, book)`: for each currency the book already has a tally for, `Fixed` sets `tally.floor` (rename the field to `emergency_fund`) to the amount; `MonthsOfOutflows` sums that currency's recurring outflows in `[as_of, months_after(as_of, months))`, as `add_floor` does today but filtered to the currency. A target never creates a tally.

- [ ] **Step 1: Write the failing tests** (domain):
  - `a_target_applies_only_to_its_currency` — USD account 5000.00 and a USD bill, EUR account 100.00; targets `{USD: Fixed 1000.00}`: USD's `emergency_fund` is 1000.00, a blocker `EmergencyFundNotSet { EUR }`, and no USD blocker.
  - `months_target_counts_its_own_currency` — USD bill 100.00 monthly and EUR bill 50.00 monthly, targets `{USD: Months 3, EUR: Months 3}`: USD 300.00, EUR 150.00.
  - `an_unused_currency_target_changes_nothing` — a `{GBP: Fixed 1}` target added to a USD-only input gives the same answer as without it, `emergency_funds` lists only USD's.
  - Update the golden and property tests mechanically (`floor: Some(SafetyFloor::Fixed { amount })` → `emergency_funds: [(amount.currency, Fixed { minor_units })]`); the proptest that the surplus falls as the floor rises keeps its claim per currency.
- [ ] **Step 2: Run** `… -p coffer-domain … -E 'test(forecast)'`. Expected: FAIL to compile, then FAIL on the new tests.
- [ ] **Step 3: Implement** the domain change, then the store's `ForecastData` (read `emergency_funds` in the same read transaction), the ledger's options and what-if, the page's words: blocker sentence `"No emergency fund is set for {code}. Without one, coffer would overstate what could be invested. It is set through the API."` (07c replaces the last sentence with a link) and the two renamed cautions in plain words. The words tests' `schema_kinds` checks cover the new kinds.
- [ ] **Step 4: Run** `… -p coffer-domain …` and `… -p coffer-server … -E 'test(forecast)'`. Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07b commit -m "Apply each currency's emergency fund in the forecast"`

### Task 3: Ledger, API and MCP

**Files:**
- Create: `crates/server/src/ledger/emergency_funds.rs`, `crates/server/tests/it/emergency_funds.rs` (+ `mod`)
- Modify: `crates/server/src/ledger/{mod.rs, settings.rs}`, `crates/server/src/http/api/{household.rs, mod.rs, openapi.rs}`, `crates/server/src/http/api/mcp/tools.rs`, `crates/server/tests/it/mcp/parity.rs`, `crates/server/tests/it/household_settings.rs`, `crates/server/tests/it/grants/settings.rs`, `docs/api/openapi.json`, `staging/seed/household.mjs:255`, `staging/e2e/api/numbers.spec.mjs`, `docs/agent-forecast.md`, `docs/agent-usage.md`, `docs/threat-model.md` (where it names the floor), `AGENTS.md` (crate map: `ledger/emergency_funds`, store `emergency_funds.rs`)

**Interfaces:**
- `HouseholdSettings.floor` → `emergency_funds: Option<Vec<EmergencyFund>>` (`null` below forecast access, sorted by currency otherwise; `#[schema(required)]`). `ForecastSettingsPatch.floor` is removed (a patch naming it is now 400 as an unknown field).
- Ledger:
  ```rust
  pub async fn set_emergency_fund(&self, actor: &Actor, currency: Currency, target: EmergencyFundTarget)
      -> Result<HouseholdSettings, LedgerError>;   // ManageHousehold; validate; no-op when equal; audit "household.emergency_fund.set"
  pub async fn remove_emergency_fund(&self, actor: &Actor, currency: Currency)
      -> Result<HouseholdSettings, LedgerError>;   // ManageHousehold; NotFound when none; audit "household.emergency_fund.remove"
  ```
  Both answer the settings as `household_settings` shows them to that actor. The audit's entity is `household`, its changes `{"emergency_fund.<CODE>": [old canonical or null, new canonical or null]}`, following `forecast_changes` in `settings.rs`.
- API: `PUT /api/v1/household/emergency-funds/{currency}` (body `EmergencyFundTarget`, `operation_id = "setEmergencyFund"`), `DELETE /api/v1/household/emergency-funds/{currency}` (`removeEmergencyFund`); the path's currency through `TypedPath` with `pattern = "^[A-Z]{3}$"`. Descriptions say what a months target counts, that it needs `admin`, and that reading needs the forecast's access.
- MCP: `set_emergency_fund` (`Needs::Write`), arguments `currency` and `target`. No remove tool (overview, planner's call 3).

- [ ] **Step 1: Write the failing tests** (`tests/it/emergency_funds.rs`): `admin_sets_and_reads_a_target` (PUT then GET settings lists it; audit entry with old `null`, new `fixed:100000`); `setting_the_same_target_writes_nothing` (no second audit entry); `removing_a_missing_target_is_not_found`; `a_member_cannot_set_one` (403); `a_restricted_token_reads_no_emergency_fund` (an admin's token with account grants below `all` at `transactions`: its GET reads `emergency_funds: null`, and its PUT succeeds and answers `null` too, with or without a target already set); `out_of_range_is_422` (−1, 61 months); `a_bad_currency_in_the_path_is_400`; `the_forecast_uses_it` (after PUT, `GET /api/v1/forecast` has no `emergency_fund_not_set` blocker for that currency). Update `household_settings.rs` and `grants/settings.rs` for the removed `floor`. Parity row for `set_emergency_fund`.
- [ ] **Step 2: Run** `… -p coffer-server … -E 'test(emergency_fund) | test(household_settings) | test(parity)'`. Expected: FAIL.
- [ ] **Step 3: Implement** the ledger calls, handlers, MCP row; regenerate `docs/api/openapi.json`; update the seed (`PUT /api/v1/household/emergency-funds/EUR` with `{"kind":"fixed","minor_units":100000}`) and the e2e expectation; update the docs.
- [ ] **Step 4: Run** the filtered tests, then `uptime` and `nice -n 15 scripts/gate.sh`. Expected: PASS, gate green.
- [ ] **Step 5: Commit, push, PR** — `git -C ../coffer-07b commit -m "Set and remove an emergency fund per currency through the API and MCP"`; push `money-07b-emergency-fund`; PR "Emergency fund per currency" with "Closes #141", the API break named (planner's call 5), what ran locally, and that the seed and e2e changes are checked by CI only.
