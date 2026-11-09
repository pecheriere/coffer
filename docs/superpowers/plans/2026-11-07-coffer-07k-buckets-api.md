# 07k — Buckets: this month and past months (API, MCP) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One month's buckets, computed in the domain: per planned outflow its planned amount, spent, left or over, and a pace for budgets repeating monthly or more often; goals' set aside against their target and date; totals per currency; this month's emergency fund. Served at `GET /api/v1/buckets?month=YYYY-MM` and the MCP tool `get_buckets`. Plan versus actual's rollover takes what bills' matched payments took, as the forecast does, and a goal's progress joins the plan items API. Closes #150, #120, #134.

**Architecture:** A child module of `forecast`, `forecast/buckets.rs`, like 07h's free money, so it uses the forecast's reduction and bill matching instead of copying them. A month is read **as the forecast reads it on that month's last day** (today for this month): the store's `forecast_data` at that `as_of`, with spent transactions read through the end of the next month for a past month, so a bill paid late in the following month is paid in its own month exactly as the following month's forecast said (`BILL_LOOK_BACK_MONTHS` is 1). Versions come from 07j: the items are the spans in force in the month. The matched payments per month feed plan versus actual's rollover through one small domain function, `forecast::bills_paid_in`, so the two answers agree (#120).

**Tech Stack:** Rust 2024, axum, utoipa, rmcp, proptest.

**Spec:** §4.2, §6 (buckets read), §10.7 (a bill's spent is its payment), §10.10 (budgets that don't repeat monthly). Overview: unit 07k.

**Depends on:** 07j (#149) merged. **Branch / worktree:** `money-07k-buckets-api`, `../coffer-07k`. No migration.

## Global Constraints

- Everything in the overview's "Rules for every unit" applies.
- A bucket is an active planned outflow with an occurrence in the month (goals listed separately). Per bucket, in its currency, never converted:
  - **Bill or one-off:** `planned` = its occurrences in the month at the version in force. An occurrence paid by a matched payment: spent += the payment, and its difference from plan is over (positive) or saved (negative) (§10.7). Settled as paid: spent += its amount. Skipped: planned stays, spent 0, needs 0. Unpaid: needs its amount. `left = max(planned − spent, 0)` over unpaid occurrences only; `over = Σ max(payment − amount, 0)`.
  - **Budget:** `planned` = its occurrences in the month (weekly budgets count theirs, §10.10); `spent` = what the reduction took for it (`ItemAdjustment.reduced_by`), and the pool's remainder after every budget sharing the category is paid goes to the last of them in date order (planner's call: the excess must show somewhere and that one ran out last). `left = max(planned − spent, 0)`, `over = max(spent − planned, 0)`.
  - **A budget repeating less often than monthly** (`months` with `every > 1`, or `years`): planned and spent for its own period, from its latest occurrence on or before the month's end to the next one; spent read with `Store::category_spent_between` for that period. No pace.
  - **Pace** (this month only, budgets repeating monthly or more often): `elapsed_permille` = days elapsed including today ÷ days in the month, `spent_permille` = spent ÷ planned, both integers in ‰, rounded half-even; `ahead` when `spent_permille > elapsed_permille`.
- Goals: `target`, `start_on`, `due_on`, `completed`, `set_aside` (`goal_need` at `as_of` with what was spent from it), `spent`, `remaining` (`goal_remaining`). "Short" arrives with covers (07n).
- The emergency fund: this month only, per currency, 07h's `FreeMoneyCurrency` target and held; `null` for a past month (the fund is stateless, §4.4).
- `month` defaults to this month; a month after this one → 422 "buckets are for this month or earlier"; older than 36 months back → 422, as plan versus actual's `MAX_MONTHS`.
- Authorization as the forecast (`authorize_forecast`), decided before anything is read. Exact `i128` maths; a total past `i64` → 422 "the buckets' totals are too large to represent".
- Words (§3): "bucket", "planned", "spent", "left", "over", "set aside", "pace". The API keeps `plan item` ids (`item`).

## Review Focus

1. **#120: the two answers agree.** A month where a 1,000 rent bill was paid 1,000 and groceries (same category) spent 300: plan versus actual's rollover for the budget is 300 spent, and buckets say rent spent 1,000, groceries 300. → Task 2 `rollover_takes_matched_payments_not_planned_amounts`, Task 3 `buckets_and_plan_vs_actual_agree_on_a_shared_category`.
2. **A late payment.** October's bill paid on 3 November → October's bucket paid, not unpaid. → Task 3 `a_bill_paid_early_next_month_is_paid_in_its_month`.
3. **Pace only where it means something.** Quarterly budget → planned and spent for its quarter, `pace` null; weekly budget → occurrences in the month and a pace. → Task 3 `a_quarterly_budget_has_its_period_and_no_pace`, `a_weekly_budget_has_a_pace`.
4. **Past months use their version.** Raised from October: September's bucket planned the old amount. → Task 4 `a_past_month_reads_its_version`.
5. **Restricted tokens.** → Task 4 `a_restricted_token_is_refused`.

---

### Task 1: Bills paid in a month, shared

**Files:**
- Create: `crates/domain/src/forecast/bills_paid.rs`, `crates/domain/src/forecast/tests/bills_paid.rs`
- Modify: `crates/domain/src/forecast.rs` (`mod bills_paid;`, re-export), `crates/store/src/forecast.rs` (`forecast_data` gains `payments_through: Date`, the last day spent transactions are read for; the forecast passes `as_of`; `Store::category_spent_between(from, until_exclusive)` from `CATEGORY_SPENT`'s filters; `Store::spent_transactions_between(from, until_exclusive)` from `SPENT_TRANSACTIONS`'s), their callers in `crates/server/src/ledger/{forecast.rs, free_money.rs}`

**Interfaces:**
```rust
/// Each occurrence in `month` of a stored bill or one-off in `input`, and what paid it, as the
/// forecast at `input.as_of` (the month's last day, or today) matches them.
pub fn bills_paid_in(input: &ForecastInput, month: CalendarMonth) -> Result<Vec<PaidOccurrence>, ForecastError>;
pub struct PaidOccurrence { pub source: LineSource, pub on: Date, pub planned: Money,
                            pub paid: Option<PaidBy>, pub settled: Option<SettlementKind> }
```
- [ ] **Step 1: Write the failing tests:** `a_payment_pays_its_occurrence`, `a_settled_occurrence_is_reported_settled`, `a_payment_in_the_next_month_pays_last_months_bill` (as_of 2026-10-31, payment 2026-11-03 read because `payments_through` is 2026-11-30), `it_agrees_with_the_forecasts_payments` (proptest: for `as_of` = today, every occurrence equals the forecast's `ItemLine.payments` entry).
- [ ] **Step 2: Run** `nice -n 15 env -u CARGO_TARGET_DIR cargo nextest run -p coffer-domain --all-features -j 3 -E 'test(bills_paid)'`. Expected: FAIL.
- [ ] **Step 3: Implement** (a wrapper over `reduction::reduce` reading `ItemAdjustment.payments`). **Step 4: Run** domain and store crates. Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07k commit -m "Report what paid each bill in a month, as the forecast matches it"`.

### Task 2: Plan versus actual's rollover uses matched payments (#120)

**Files:**
- Create: `crates/domain/src/plan_report/rollover.rs` (`rollover_rows` and `runs_in` move here: `plan_report.rs` is ~720 lines after 07j)
- Modify: `crates/domain/src/plan_report.rs` (`PlanReportInput.bills_paid: Vec<MonthPaid>` with `MonthPaid { month, category, currency, paid_minor }`; `Planned.due_by_today` replaced by what was paid), `crates/domain/src/plan_report/tests/rollover.rs`, `crates/server/src/ledger/plan_report.rs` (for each reported month, `bills_paid_in` over that month's input; one `spent_transactions_between` read for the window), `docs/agent-forecast.md` (plan versus actual section: the rollover's spent)

- [ ] **Step 1: Write the failing tests:** `rollover_takes_matched_payments_not_planned_amounts` (bill 1,000 paid 1,080 in Groceries' category with a 400 budget: budget spent = actual − 1,080, not − 1,000), `an_unpaid_bill_takes_nothing_from_the_rollover`, and update the existing rollover tests whose expectations assumed planned amounts (name each in the commit body).
- [ ] **Step 2: Run** `… -p coffer-domain … -E 'test(plan_report)'`. Expected: FAIL. **Step 3: Implement.** **Step 4: Run** domain and `-p coffer-server -E 'test(plan_vs_actual)'`. Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07k commit -m "Take what bills were paid, not what they planned, from a budget's rollover"`.

### Task 3: Buckets in the domain

**Files:**
- Create: `crates/domain/src/forecast/buckets.rs` (the function), `crates/domain/src/forecast/buckets/{types.rs, pace.rs}`, `crates/domain/src/forecast/buckets/tests.rs`
- Modify: `crates/domain/src/forecast.rs` (`mod buckets;`, re-exports), `AGENTS.md` (crate map)

**Interfaces (`coffer_domain::forecast`):**
```rust
pub struct BucketsInput { pub forecast: ForecastInput, pub month: CalendarMonth,
                          pub long_period_spent: Vec<PeriodSpent>, pub free_money: Option<FreeMoney> }
pub fn compute_buckets(input: &BucketsInput) -> Result<Buckets, ForecastError>;
pub struct Buckets { pub month: CalendarMonth, pub as_of: Date, pub current: bool,
                     pub buckets: Vec<Bucket>, pub goals: Vec<GoalBucket>,
                     pub totals: Vec<BucketTotals>, pub emergency_funds: Option<Vec<EmergencyFundBucket>>,
                     pub status: ForecastStatus, pub blockers: Vec<Blocker>, pub cautions: Vec<Caution> }
pub struct Bucket { pub item: PlannedItemId, pub name: String, pub tier: PlanTier,
                    pub kind: BucketKind /* bill | budget | one_off */, pub category: CategoryId,
                    pub effective_from: Option<CalendarMonth>, pub planned: Money, pub spent: Money,
                    pub left: Money, pub over: Money, pub period: Option<BucketPeriod>, pub pace: Option<Pace> }
pub struct Pace { pub elapsed_permille: u16, pub spent_permille: u32, pub ahead: bool }
pub struct GoalBucket { pub item: PlannedItemId, pub name: String, pub target: Money, pub start_on: Date,
                        pub due_on: Date, pub completed: bool, pub set_aside: Money, pub spent: Money, pub remaining: Money }
pub struct BucketTotals { pub currency: Currency, pub planned: Money, pub spent: Money, pub left: Money, pub over: Money,
                          /// This month only: 07h's free money for the currency; null for a past month or a blocked currency.
                          pub free_money: Option<Money> }
```
Buckets ordered by tier (`must_pay`, `essentials`, `flexible`), then name, then id.

- [ ] **Step 1: Write the failing tests** (amounts in USD; helpers of `forecast/tests.rs`): `a_bills_spent_is_its_payment` (1,000 paid 1,080 → spent 1,080, left 0, over 80), `a_bill_paid_less_is_a_saving` (over 0, left 0), `an_unpaid_bill_is_left`, `a_skipped_bill_needs_nothing`, `a_budget_over_shows_its_excess`, `two_budgets_in_one_category_give_the_excess_to_the_last`, `a_quarterly_budget_has_its_period_and_no_pace`, `a_weekly_budget_has_a_pace`, `pace_is_in_permille` (today the 10th of a 30-day month, spent 150 of 300 → 333 and 500, ahead), `a_bill_paid_early_next_month_is_paid_in_its_month`, `goals_show_set_aside_and_remaining`, `a_past_month_has_no_emergency_fund_and_no_pace`, `buckets_and_plan_vs_actual_agree_on_a_shared_category`.
- [ ] **Step 2: Run** `… -p coffer-domain … -E 'test(buckets)'`. Expected: FAIL. **Step 3: Implement.** **Step 4: Run** the domain crate. Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07k commit -m "Compute a month's buckets in the domain"`.

### Task 4: Ledger, API, MCP, and a goal's progress (#134)

**Files:**
- Create: `crates/server/src/ledger/buckets.rs`, `crates/server/src/http/api/buckets.rs`, `crates/server/tests/it/buckets.rs` (+ `mod buckets;`), `crates/server/tests/it/mcp/parity/buckets.rs` (+ `mod` line)
- Modify: `crates/server/src/ledger/mod.rs`, `crates/server/src/http/api/{mod.rs, openapi.rs}`, `crates/server/src/http/api/mcp/tools.rs` (610 lines: if the row pushes it past ~700, split into `tools/{read.rs, write.rs}` in this change), `crates/server/src/ledger/plan_items.rs` (`PlannedItemView.goal: Option<GoalProgress { accrued, set_aside, spent, remaining }>` at the household's today, computed with `goal_accrued`/`goal_need`; the goal spent read extracted from `forecast_data`'s `GOAL_SPENT` as `Store::goal_spent()`), `docs/api/openapi.json`, `docs/agent-usage.md` (a "Buckets" section), `docs/agent-forecast.md`

**Interfaces:**
- Ledger: `pub async fn buckets(&self, actor: &Actor, month: Option<CalendarMonth>) -> Result<Buckets, LedgerError>` — `authorize_forecast`; `as_of` = today in this month, else the month's last day; `forecast_data(as_of, payments_through)`; `input(...)` with the forecast's settings at horizon 1; `free_money` only for this month; `category_spent_between` for each long-period budget.
- API: `GET /api/v1/buckets`, query `month`, `operation_id = "getBuckets"`, tag forecast. MCP: `get_buckets`, `Needs::Read`, lead "One month's buckets: what each planned outflow planned, spent and has left, with pace this month, and goals' set aside. " + `MONEY`.

- [ ] **Step 1: Write the failing tests** (real router, clock 2026-11-10): `this_months_buckets` (seeded bill, two budgets, a goal, a target: the JSON's numbers), `a_past_month_reads_its_version`, `a_future_month_is_422`, `a_restricted_token_is_refused` (403, same body with or without items), `a_goal_answers_its_progress` (`GET /plan-items/{goal}` has `goal.set_aside`), parity row for `get_buckets`.
- [ ] **Step 2: Run** `… -p coffer-server … -E 'test(buckets) | test(plan_items) | test(parity)'`. Expected: FAIL.
- [ ] **Step 3: Implement**; regenerate `docs/api/openapi.json`; update the docs.
- [ ] **Step 4: Run** the filtered tests, then `uptime` and `nice -n 15 scripts/gate.sh`. Expected: PASS.
- [ ] **Step 5: Commit, push, PR** — `git -C ../coffer-07k commit -m "Serve a month's buckets through the API and MCP"`; push; PR "Buckets: this month and past months", "Closes #150, closes #120, closes #134", naming the planner's calls (the excess goes to the last budget; a past month read on its last day with the next month's payments).

## Open questions for the owner

1. **Where a shared category's excess shows.** Two budgets in one category overspent: recommended, the last to run out (in date order) carries the whole excess; the alternative, splitting it by planned share, is fairer-looking but invents a rule the reduction doesn't have.
