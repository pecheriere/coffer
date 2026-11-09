# 07q — The 3-month cash projection and its low point Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Project each currency's unclaimed cash day by day over the next 3 months (planned income counted whatever `include_income` says, goals' set-aside growing as it accrues), find the low point with its date and cause, and serve it at `GET /api/v1/cash-projection` and the MCP tool `get_cash_projection`. Keep or invest (07r) and Home's chart (07s) read it. Closes #156.

**Architecture:** A child module of `forecast`, `forecast/projection.rs`, beside `free_money.rs`, so it reuses the forecast's private pieces: the counted cash (`lines::not_counted`), the shared reduction (`reduction::reduce`, `is_stored_budget`, `counts_from`), what an item still needs (`lines::still_needed`), `goal_need`, `emergency_fund`, and free money's per-currency blocking. It reads the same `ForecastInput` the forecast and free money read, built by the same ledger call (`ledger::forecast::input`), so the three answers can't drift. Its own endpoint rather than a field of free money: free money stays small and its schema unchanged, and the projection is ~92 points per currency.

**Tech Stack:** Rust 2024, axum, utoipa, rmcp, proptest.

**Spec:** `docs/superpowers/specs/2026-11-07-coffer-money-redesign-design.md` §4.5 (low point and its date; from the domain; in the API), §10.1 (the projection counts planned income), §10.8 (paid early needs nothing), §10.9 (the projection subtracts goal accrual as it happens). Overview: 07q.

**Depends on:** 07h (#147), 07j (#149, versions) merged. **Branch / worktree:** `money-07q-cash-projection`, `../coffer-07q`. No migration.

**Interface from 07j** (plan `2026-11-07-coffer-07j-plan-history.md`; check the merged code's names before starting): the ledger passes **one `ItemInput` per timeline span** of an item's versions (`ItemInput.in_force: MonthSpan`, `until` exclusive; hypothetical items `MonthSpan::ALWAYS`), each with that span's flat `amount`, `tier`, `category` and `spending`, so an item can enter the projection as two inputs with the same `source`. Occurrences are read **only** through `ItemInput::occurrences(from, until_exclusive)` / `ItemInput::occurrences_in(month)`, which clip to the span, never `item.schedule.occurrences(..)`. `ledger/forecast.rs`'s `stored_items(item, …) -> Vec<ItemInput>` builds the spans from last month through the window's end, so the projection's input must be built by the same `input()` with the window ending at `as_of` + 3 months. `ItemAdjustment` keeps its shape; a goal has a single span within any window, so `goal_need` on its `amount` is unchanged.

## Global Constraints

- Everything in the overview's "Rules for every unit" applies.
- The line, per currency, from `as_of` (the household's today) to `through` = 3 months on, less a day (planner's call: the window is `[as_of, same_day_after(as_of, 3))`):
  ```
  projected(as_of) = cash available
                   − what stored budgets still need this month (whatever their tier: the month's
                     remainder is spoken for from today, since when it is spent is unknown)
                   − what bills and one-offs dated before today still owe (`unpaid`)
                   − what goals have set aside by today
                   − outflow occurrences due today, not paid, settled or paid early
  projected(d)     = projected(d − 1)
                   − outflow occurrences due on d (later months' budgets on their own dates),
                     each at its span's amount (07j: one input per version span)
                   + planned income due on d (from tomorrow, `counts_from`), whatever
                     `include_income` says, except an inflow hinted to an account not counted
                   − goal accrual on d: goal_need(d + 1) − goal_need(d), per active goal
  low point        = the earliest day of the lowest `projected`; its causes are the outflow
                     occurrences due that day, largest first, at most 3 (none when it is today)
  ```
  So `projected(as_of)` differs from free money's `before` only by outflows dated after today that free money already holds back (must pay through 30 days, this month's essentials and flexible bills and one-offs); the line takes those off on their own dates instead. The emergency fund is a line beside it, not part of it.
- Status, blockers and cautions: those of `compute_forecast(horizon 3, include_income true)` at the same day, less `MixedCurrencies` and `CashDipsBelowEmergencyFund` (the projection states its own dip); a blocker leaves its currency without days or low point, exactly as it leaves free money without a number (share free money's per-currency rule, don't copy it).
- Exact integer maths in `i128`; a value that doesn't fit `i64` answers `ForecastError::OutOfRange` → 422 as the forecast does. Authorization as the forecast (`authorize_forecast`), before anything is read; read-only.
- Words (§3): "projected cash", "low point", "emergency fund", "set aside". No "accrual" in descriptions meant for people.
- `types.rs` is at 730 lines: the new types go in `forecast/projection/types.rs`, never in `types.rs`.

## Review Focus

1. **Disagreeing with the forecast.** With no goals and no budgets, at an `as_of` on day 1–28, `projected` on the last day of each forecast month equals that month's `projected_cash` of `compute_forecast(horizon 3, include_income true)`. → Task 1 proptest `month_ends_match_the_forecasts_projected_cash`.
2. **Disagreeing with free money.** With only essentials and flexible budgets and goals, `projected(as_of)` equals free money's `cash_available − still_need.essentials − still_need.flexible − set_aside`. → Task 1 proptest `today_matches_free_money`.
3. **A bill paid early counted twice** (§10.8): a bill due in 5 days and paid yesterday never lowers the line. → Task 1 `a_bill_paid_early_never_lowers_the_line`.
4. **Income ignored when `include_income` is off.** → Task 1 `income_counts_whatever_the_setting`.
5. **Versions.** A bill of 1,000 lowered to 800 from next month (two inputs, spans `..2027-02` and `2027-02..`) lowers the line by 800 on next month's date, never by both amounts; the low point's causes name the item once. → Task 1 `each_occurrence_takes_its_months_version`, Task 2 `a_version_from_next_month_lowers_the_line_by_its_amount`.
6. **Restricted tokens** refused before reading, same body whatever exists. → Task 2 `a_restricted_token_is_refused`.

---

### Task 1: The projection in the domain

**Files:**
- Create: `crates/domain/src/forecast/projection.rs`, `crates/domain/src/forecast/projection/types.rs`, `crates/domain/src/forecast/tests/projection.rs`, `crates/domain/src/forecast/tests/projection_properties.rs`
- Modify: `crates/domain/src/forecast.rs` (`mod projection;`, re-exports, `same_day_after` visible to the child), `crates/domain/src/forecast/free_money.rs` (extract `pub(super) fn blocks_currency(blocker: &Blocker, input: &ForecastInput, currency: Currency) -> bool` from its `unblocked` closure and call it), `crates/domain/src/forecast/tests.rs` (`mod projection; mod projection_properties;`), `AGENTS.md` (crate map: `forecast/projection.rs`)

**Interfaces:**
- Produces (`coffer_domain::forecast`):
  ```rust
  /// The 3-month cash projection for `input` at `input.as_of`, whatever `horizon_months` and
  /// `include_income` say. # Errors: [`ForecastError::OutOfRange`].
  pub fn compute_cash_projection(input: &ForecastInput) -> Result<CashProjection, ForecastError>;
  pub const PROJECTION_MONTHS: u8 = 3;

  pub struct CashProjection {          // Serialize, ToSchema
      pub as_of: Date,
      pub through: Date,               // the last day projected
      pub status: ForecastStatus,
      pub currencies: Vec<CurrencyProjection>,  // sorted by currency, never added together
      pub blockers: Vec<Blocker>,
      pub cautions: Vec<Caution>,
  }
  pub struct CurrencyProjection {
      pub currency: Currency,
      pub emergency_fund_target: Money,          // 0 when none is set
      pub days: Vec<ProjectedDay>,               // one per day, as_of..=through; empty when blocked
      #[schema(required)] pub low_point: Option<LowPoint>,   // null when blocked
      pub dips_into_emergency_fund: bool,        // low_point.projected < target
  }
  pub struct ProjectedDay { pub on: Date, pub projected: Money, pub outflows: Money,
                            pub inflows: Money, pub set_aside: Money }   // flows of that day
  pub struct LowPoint { pub on: Date, pub projected: Money, pub causes: Vec<LowPointCause> }
  pub struct LowPointCause { pub source: LineSource, pub name: String, pub amount: Money }
  ```
- One pass per item: its occurrence dates in the window (`still_needed` for the first month's budget remainder and `unpaid`; `ItemInput::occurrences(counts_from(item, as_of), end)` for the rest (clipped to the input's span; never `schedule.occurrences`), filtered by `settled::is_settled` and `paid_ahead`, as `still_needed` filters), added into a per-day `Vec<i128>` per currency; then one running sum. Goals: `goal_need` at each day boundary, the difference added to that day.

- [ ] **Step 1: Write the failing tests** (`tests/projection.rs`, USD minor units, the forecast tests' helpers):
  - `the_line_in_order` — today 2027-01-15; checking 10,000.00; rent 2,000.00 on the 20th monthly; groceries 600.00 budget with 250.00 spent; salary 4,000.00 on the 25th (`include_income: false`); a 730.00 goal from 2026-07-15 to 2027-07-15 → `projected(01-15)` = 10,000 − 350 − 368 = 9,282.00; on 01-20 it drops by 2,000.00; on 01-25 rises by 4,000.00; groceries' February occurrence lands on 02-01; each day the goal's accrual (2.00) comes off.
  - `the_low_point_is_the_earliest_lowest_day_with_its_causes` — property tax 3,000.00 one-off on 2027-03-14 and insurance 200.00 the same day → low point 03-14, causes `[property tax 3,000, insurance 200]`; a later equal low doesn't move it.
  - `a_low_point_today_has_no_cause` — only income ahead → low point is `as_of`, no causes.
  - `a_bill_paid_early_never_lowers_the_line` — bill due 01-20, paid 01-14 → no drop on 01-20.
  - `income_counts_whatever_the_setting` — same line with `include_income` true and false.
  - `income_hinted_to_an_uncounted_account_is_left_out`.
  - `each_occurrence_takes_its_months_version` — rent as two inputs with one source, 1,000 in force until February and 800 from February → −1,000 on 01-20, −800 on 02-20; a low point on 02-20 lists rent once at 800.
  - `a_settled_month_needs_nothing` and `an_unpaid_bill_from_before_today_comes_off_today`.
  - `dips_into_the_emergency_fund` — target Fixed 9,000.00 and a low of 8,500.00 → true; `false` with no target set and the caution `EmergencyFundNotSet`.
  - `a_missing_balance_blocks_only_its_currency` — EUR without a balance: EUR `days` empty and `low_point` null, USD answered, blocker `BalanceMissing`.
  - `the_window_is_three_months_less_a_day` — as_of 2027-01-31 → `through` 2027-04-29 (`same_day_after` clamps to 04-30, exclusive).
  - Proptests (`projection_properties.rs`): `month_ends_match_the_forecasts_projected_cash` (Review Focus 1, arbitrary bills, one-offs and income, `as_of` day 1–28), `today_matches_free_money` (Review Focus 2: essentials and flexible budgets, goals, arbitrary spending), `the_low_point_is_the_minimum` (`low_point.projected == min(days.projected)` and no earlier day equals it).
- [ ] **Step 2: Run** `nice -n 15 env -u CARGO_TARGET_DIR cargo nextest run -p coffer-domain --all-features -j 3 -E 'test(projection)'`. Expected: FAIL to compile.
- [ ] **Step 3: Implement** the `blocks_currency` extraction first (the free money tests stay green), then `projection.rs`.
- [ ] **Step 4: Run** `… -p coffer-domain --all-features -j 3`. Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07q commit -m "Project cash day by day over three months, with its low point"`

### Task 2: Ledger, API, MCP and docs

**Files:**
- Create: `crates/server/src/ledger/projection.rs`, `crates/server/src/http/api/projection.rs`, `crates/server/tests/it/forecast/projection.rs` (+ `mod projection;` in `tests/it/forecast/mod.rs`)
- Modify: `crates/server/src/ledger/mod.rs`, `crates/server/src/http/api/{mod.rs, openapi.rs}`, `crates/server/src/http/api/mcp/tools.rs` (if it passes about 700 lines, split by area as the overview says, in this change), `crates/server/tests/it/mcp/parity/{mod.rs, schemas.rs}`, `docs/api/openapi.json`, `docs/agent-forecast.md` (a "Cash projection" section: the line, the window, what counts, the low point), `docs/agent-usage.md`

**Interfaces:**
- Ledger: `pub async fn cash_projection(&self, actor: &Actor) -> Result<CashProjection, LedgerError>` — as `Ledger::free_money` (`ledger/free_money.rs`): `authorize_forecast`, `today()`, `validate_as_of`, `forecast_data(today)`, `input(today, today, settings{include_income: true, default_horizon_months: PROJECTION_MONTHS, ..}, funds, data, Vec::new())` (so 07j's `stored_items` builds version spans through `as_of` + 3 months), `compute_cash_projection`; `OutOfRange` → `LedgerError::Invalid(TOO_LARGE)`. `forecast_data(today)` already reads every active item with its `versions` and the forecast's look-back, so no store change; tests pin it (`the_projection_sees_three_months`, `a_version_from_next_month_lowers_the_line_by_its_amount`).
- API: `GET /api/v1/cash-projection`, `operation_id = "getCashProjection"`, tag forecast, `read` plus the forecast's access. The description gives the line in words (start from cash, less what this month's budgets still need, what goals set aside, and what is owed; then each day's planned outflows and income, whatever the forecast's income setting, and goals' set-aside as it grows), the window, the low point and its causes, `dips_into_emergency_fund`, currencies never added together, and that `status` and the cautions are the forecast's.
- MCP: `get_cash_projection`, `Needs::Read`, lead: "Where each currency's cash is heading over the next 3 months, day by day, and its low point with what causes it. Never report a `provisional` answer without its cautions, nor a line for a currency whose `low_point` is `null`: its `blockers` say why. Go by each blocker's and caution's `kind`, as for `get_forecast`."

- [ ] **Step 1: Write the failing tests** (`tests/it/forecast/projection.rs`, real router, clock on 2027-01-15):
  - `the_projection_answers_the_line` — Task 1's `the_line_in_order` household seeded through the API → `days[0].projected` 9,282.00 and the drop on 01-20.
  - `the_projection_sees_three_months` — a one-off on `through` is in the line; one the day after is not.
  - `a_version_from_next_month_lowers_the_line_by_its_amount` — rent 1,000 patched to 800 with `effective_from: next_month` through the API → the line drops 1,000 on 01-20 and 800 on 02-20.
  - `a_restricted_token_is_refused` — grants below `all` at `transactions` → 403, the same body with and without items.
  - `a_member_reads_it` — member token with full access → 200.
  - Parity row for `get_cash_projection` in `parity/mod.rs`; add it to the "status, provisional, cautions, blockers, `kind`" list in `parity/schemas.rs`.
- [ ] **Step 2: Run** `… -p coffer-server --all-features -j 3 -E 'test(projection) | test(parity)'`. Expected: FAIL.
- [ ] **Step 3: Implement**; regenerate `docs/api/openapi.json`; update the docs.
- [ ] **Step 4: Run** the filtered tests, then `uptime` and `nice -n 15 scripts/gate.sh`. Expected: PASS, gate green.
- [ ] **Step 5: Commit, push, PR** — `git -C ../coffer-07q commit -m "Serve the cash projection through the API and MCP"`; push `money-07q-cash-projection`; PR "The 3-month cash projection and its low point", "Closes #156", stating the planner's calls below and what was verified locally versus by CI only.

## Planner's calls

1. **Budgets' remaining month is spoken for today**, not spread over the month: when it is spent is unknown, and a line that assumes later spending would show a higher low point than is safe.
2. **The line is net of goals' set-aside**, so the emergency fund line is the only line it is compared with on Home (07s), and the keep-or-invest suggestion (07r) is `min(free money, low point − target)`.
3. **Own endpoint**, not a field of free money.

## Open questions for the owner

None.
