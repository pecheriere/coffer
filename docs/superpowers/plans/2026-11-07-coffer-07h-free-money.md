# 07h — Free money: calculation, API and MCP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Compute free money per currency in the domain (cash available, less what this month's must pay, essentials and flexible items still need and every must pay due in the next 30 days, less what goals have set aside, less the emergency fund target), with the emergency fund held, any shortfall and the forecast's status; serve it at `GET /api/v1/free-money` and the MCP tool `get_free_money`; turn the forecast's "no emergency fund" blocker into a caution. Closes #147.

**Architecture:** A child module of `forecast`, `forecast/free_money.rs`, so it shares the forecast's private pieces instead of copying them: the counted accounts (`lines::not_counted`), the reduction (`reduction::reduce`, which 07g made count paid-ahead bills as paid), the occurrence counting, the months target (`add_emergency_funds`), and the status (blockers and cautions of a one-month forecast at today). One function computes "what an item still needs between two days", and both `dated_line` and free money call it, so the two answers can't drift. It reads the same `ForecastInput` the forecast reads, gathered by the same store read, so the API, MCP and the page agree.

**Tech Stack:** Rust 2024, axum, utoipa, rmcp, proptest.

**Spec:** `docs/superpowers/specs/2026-11-07-coffer-money-redesign-design.md` §4.3, §4.4, and §10.1 (window, no income), §10.4 (per currency), §10.5 (no fund set), §10.8 (paid early), §10.9 (goals: set aside so far). Overview: planner's call 8.

**Depends on:** 07c (#142, the blocker's household link this unit moves onto the caution), 07e (#144, tiers on `ItemInput`), 07g (#146) merged. **Branch / worktree:** `money-07h-free-money`, `../coffer-07h`. No migration.

## Global Constraints

- Everything in the overview's "Rules for every unit" applies.
- The formula, per currency, at the household's today (§4.3, §4.4, §10.1):
  ```
  still need  = Σ over must pay items: what each still needs from today through today + 30 days
              + Σ over essentials and flexible items: what each still needs in this calendar month
  set aside   = Σ over active, not completed goals: goal_need(target, start, due, today, spent)
  before      = cash available − still need − set aside
  held        = clamp(before, 0, target)          (target 0 when none is set)
  free money  = max(before − target, 0)
  shortfall   = max(−before, 0)
  ```
  "Through today + 30 days" includes the 30th day (planner's call). "What an item still needs" is `max(planned − spent, 0)` per item: a budget's occurrences this month less its reduction, a bill's or one-off's occurrences not paid (matched, settled, or paid up to 7 days early) plus what is unpaid from before today. Inflows never count (§10.1), whatever `include_income` says. Investments never count: only what the forecast counts as cash.
- No target for a currency: `target` 0 (so all of `before` is free) and the caution `EmergencyFundNotSet { currency }` (§10.5). The forecast gets the same caution in place of its blocker, and its surplus is computed with a 0 target for that currency. The commit that removes the blocker says why (§10.5: the reason was not overstating what can be invested; the keep-or-invest suggestion, 07r, is withheld instead).
- One answer per currency in use, sorted by currency; never netted, never converted.
- Authorization as the forecast (`Ledger::authorize_forecast`: `ReadHousehold` and `all` at `transactions`), decided before anything is read. Read-only: writes nothing.
- Exact integer maths in `i128`; a total that doesn't fit in `i64` answers `ForecastError::OutOfRange` → 422 "the forecast's totals are too large to represent", as the forecast does.
- Words in API descriptions and MCP texts (§3): "free money", "still need", "set aside", "emergency fund". No "accrual", "provisional" in descriptions meant for people; the `status` enum keeps its wire values.

## Review Focus

1. **The mortgage on the 1st.** At 2027-01-30, a must pay bill due 2027-02-01 is in "still need"; paid on 2027-01-29 (07g) it isn't. A flexible budget's February occurrence is never in it. → Task 1 `a_must_pay_due_next_month_is_still_needed`, `a_bill_paid_early_is_not_needed`, `next_months_flexible_budget_is_not_needed`.
2. **Two currencies, one blocked.** EUR's account has no balance: EUR's `free_money` is `null`, USD's still answers, the status is `provisional` and the blocker names the account. → Task 1 `a_missing_balance_blocks_only_its_currency`.
3. **Free money disagreeing with the forecast.** On the 1st of a 31-day month the window is the forecast's one-month window, so for a plan with only must pay outflows "still need" equals the forecast's `planned_outflows`. → Task 1 proptest `on_the_first_still_need_is_the_forecasts_outflows`.
4. **Overspend.** A flexible budget spent past its amount needs 0, never negative, and doesn't lower another item's need; the cash is simply gone. → Task 1 `an_overspent_budget_needs_nothing_and_takes_nothing`.
5. **Restricted tokens.** A token below forecast access is refused (403) before anything is read, and the refusal is the same whether or not a plan exists. → Task 2 `a_restricted_token_is_refused`.

---

### Task 1: The calculation

**Files:**
- Create: `crates/domain/src/forecast/free_money.rs` (the function and its types; if it passes about 400 lines, put the types in `free_money/types.rs`), `crates/domain/src/forecast/free_money/tests.rs`
- Modify: `crates/domain/src/forecast.rs` (`mod free_money;`, re-exports, `add_emergency_funds` and the blockers used by both; `EmergencyFundNotSet` moves from `blockers` to a caution), `crates/domain/src/forecast/lines.rs` (extract `pub(super) fn still_needed(item: &ItemInput, adjustment: &ItemAdjustment, from: Date, until_exclusive: Date) -> i128` from `dated_line`'s arithmetic and make `dated_line` call it), `crates/domain/src/forecast/reduction.rs` (`reduce` visible to the sibling: it already is `pub(super)` within `forecast`), `crates/domain/src/forecast/types.rs` (`Blocker::EmergencyFundNotSet` removed; `Caution::EmergencyFundNotSet { currency: Currency }` added), `crates/domain/src/forecast/cautions.rs`, `crates/domain/src/forecast/tests/blockers.rs` (the blocker's tests become caution tests), `AGENTS.md` (crate map: `forecast/free_money.rs`)

**Interfaces:**
- Consumes: `ForecastInput` with `emergency_funds` (07b) and `ItemInput.tier` (07e); `ItemAdjustment.paid_ahead` (07g); `goal_need`; `compute_forecast`.
- Produces (`coffer_domain::forecast`):
  ```rust
  /// Computes free money for `input` at `input.today`, which must equal `input.as_of`.
  /// # Errors
  /// [`ForecastError::OutOfRange`] when a total doesn't fit in `i64`.
  pub fn compute_free_money(input: &ForecastInput) -> Result<FreeMoney, ForecastError>;

  #[derive(Debug, Clone, PartialEq, Eq, Serialize)]
  #[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
  pub struct FreeMoney {
      pub as_of: Date,
      /// The last day a must pay occurrence counts on: today + 30 days.
      pub must_pay_through: Date,
      /// `unavailable` when no currency has a number, `provisional` with any blocker or caution.
      pub status: ForecastStatus,
      /// One per currency in use, sorted by currency. Never added together.
      pub currencies: Vec<FreeMoneyCurrency>,
      pub blockers: Vec<Blocker>,
      pub cautions: Vec<Caution>,
  }

  #[derive(Debug, Clone, PartialEq, Eq, Serialize)]
  #[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
  pub struct FreeMoneyCurrency {
      pub currency: Currency,
      /// What the forecast counts as cash: counted balances, less short-term debts.
      pub cash_available: Money,
      pub still_need: StillNeed,
      /// What goals have set aside by today, less what was spent from them.
      pub set_aside: Money,
      /// The target as set; `null` when none is set for this currency.
      #[cfg_attr(feature = "openapi", schema(required))]
      pub emergency_fund: Option<EmergencyFundTarget>,
      /// The target in money: 0 when none is set.
      pub emergency_fund_target: Money,
      /// How much of the target the cash covers now, 0 to the target.
      pub emergency_fund_held: Money,
      /// Null when a blocker leaves this currency without a number.
      #[cfg_attr(feature = "openapi", schema(required))]
      pub free_money: Option<Money>,
      /// How far cash falls short of what's spoken for; 0 when it doesn't.
      pub shortfall: Money,
  }

  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
  #[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
  pub struct StillNeed {
      /// Must pay occurrences in this calendar month, from today.
      pub must_pay_this_month: Money,
      /// Must pay occurrences after this month, through `must_pay_through`.
      pub must_pay_coming: Money,
      pub essentials: Money,
      pub flexible: Money,
      pub total: Money,
  }
  ```
- Status: compute `compute_forecast(&ForecastInput { horizon_months: 1, include_income: false, ..input.clone() })` once; keep its blockers except `MixedCurrencies` (free money is per currency), and its cautions except `CashDipsBelowEmergencyFund` (a forecast-horizon statement). `NoCountedAccounts` leaves every `free_money` null; `BalanceMissing { account }` leaves that account's currency's `free_money` null.
- The reduction: call `reduction::reduce(input, today + 31 days)` once, not with a one-month forecast's `end_exclusive` (from the 30th that is the 28th or 1st of next month, short of the must pay window). `reduce` caps budgets at the first of next month and the paid-ahead window at 7 days on its own, so the longer end changes nothing else; a test pins it (`a_must_pay_due_next_month_is_still_needed` with today on 2027-01-30).
- Per item still need: `still_needed(item, adjustment, today, until)` with `until` = first day of next month for essentials and flexible, and `today + 31 days` (exclusive) for must pay; split must pay at the first of next month into `must_pay_this_month` and `must_pay_coming`. Goals and inflows contribute nothing to still need.

- [ ] **Step 1: Write the failing tests** (`free_money/tests.rs`; amounts in USD minor units; build inputs with the forecast tests' helpers, made `pub(super)` in `forecast/tests.rs` if needed):
  - `the_formula_in_order` — today 2027-01-15; checking 10,000.00; rent 2,000.00 must pay on the 20th; groceries 600.00 essentials budget with 250.00 spent; restaurants 300.00 flexible budget with 100.00 spent; a goal of 730.00 from 2026-07-15 to 2027-07-15, nothing spent from it (184 of 365 days: set aside exactly 368.00); target Fixed 3,000.00 → still need 2,000 + 350 + 200 = 2,550.00, set aside 368.00, before 7,082.00, held 3,000.00, free 4,082.00, shortfall 0.
  - `a_must_pay_due_next_month_is_still_needed` — today 2027-01-30, mortgage 1,500.00 on the 1st monthly → `must_pay_coming` 1,500.00.
  - `a_bill_paid_early_is_not_needed` — the same, paid 2027-01-29 → 0.
  - `the_window_ends_on_the_thirtieth_day` — today 2027-01-10: a must pay one-off on 2027-02-09 (today + 30) is in `must_pay_coming`; one on 2027-02-10 is not.
  - `a_bill_settled_or_paid_this_month_needs_nothing` — rent settled as paid this month needs 0; insurance 100.00 matched by a 108.00 payment (inside ±10%) needs 0 and the 8.00 is not added anywhere (§10.7: the cash is gone, so free money already shows it).
  - `next_months_flexible_budget_is_not_needed` — today 2027-01-30, flexible budget monthly on the 1st → contributes only January's unspent remainder.
  - `an_overspent_budget_needs_nothing_and_takes_nothing` — restaurants 300.00 with 450.00 spent → its need 0; groceries' need unchanged.
  - `no_income_counts` — a salary inflow tomorrow, `include_income: true` → free money unchanged.
  - `no_target_counts_everything_as_free_with_a_caution` — no target → held 0, free = before, caution `EmergencyFundNotSet { USD }`.
  - `months_target_uses_must_pay_and_essentials` — `{USD: Months 2}` with rent 2,000 and groceries 600 monthly → target 5,200.00.
  - `shortfall_when_spoken_for_exceeds_cash` — cash 1,000.00, rent 2,000.00 → before −1,000.00, free 0, held 0, shortfall 1,000.00.
  - `a_missing_balance_blocks_only_its_currency` — USD and EUR accounts, EUR without a balance → EUR `free_money` null, USD a number, status provisional, blocker `BalanceMissing` for the EUR account.
  - `investments_never_count` — a brokerage account (invested) adds nothing to cash.
  - `a_completed_goal_sets_aside_nothing`.
  - Proptest `on_the_first_still_need_is_the_forecasts_outflows` — as_of on the 1st of a 31-day month, arbitrary must pay recurring bills and one-offs (no budgets, no goals, no income): `still_need.total == compute_forecast(horizon 1, include_income false).totals[c].planned_outflows` per currency.
  - Forecast side (`tests/blockers.rs` → `tests/emergency_fund_caution.rs`): `no_target_is_a_caution_not_a_blocker` — the forecast with no target has a surplus (0 target) and the caution; status provisional.
- [ ] **Step 2: Run** `… -p coffer-domain … -E 'test(free_money) | test(emergency_fund)'`. Expected: FAIL to compile.
- [ ] **Step 3: Implement** the extraction in `lines.rs` first (run the whole domain crate: the forecast's tests must stay green with `dated_line` calling `still_needed`), then `free_money.rs`, then the blocker-to-caution move.
- [ ] **Step 4: Run** `… -p coffer-domain --all-features -j 3`. Expected: PASS.
- [ ] **Step 5: Commit** — two commits: `git -C ../coffer-07h commit -m "Share what an item still needs between the forecast and its callers"` (the extraction alone), then `git -C ../coffer-07h commit -m "Compute free money per currency, and make a missing emergency fund a caution"` whose body says: the blocker existed so coffer would not overstate what can be invested; free money now shows everything as free with a caution, and the keep-or-invest suggestion (#157) is withheld until a target is set (spec §10.5).

### Task 2: Ledger, API, MCP and the forecast page's words

**Files:**
- Create: `crates/server/src/ledger/free_money.rs`, `crates/server/src/http/api/free_money.rs`, `crates/server/tests/it/free_money.rs` (+ `mod free_money;` in `tests/it/main.rs`)
- Modify: `crates/server/src/ledger/{mod.rs, forecast.rs}` (share `input()` and the store read; make `authorize_forecast` reachable from `free_money.rs`), `crates/server/src/http/api/{mod.rs, openapi.rs}`, `crates/server/src/http/api/mcp/tools.rs` (if it passes about 700 lines, split by area as the overview says), `crates/server/tests/it/mcp/parity.rs`, `crates/server/src/http/pages/forecast/words.rs` (+ tests: the blocker's sentence becomes a caution's, keeping 07c's admin link), `crates/server/tests/it/forecast/*` and `tests/it/pages/forecast.rs` where they expected the blocker, `docs/api/openapi.json`, `docs/agent-forecast.md` (a "Free money" section: the formula, the window, what never counts), `docs/agent-usage.md`

**Interfaces:**
- Ledger: `pub async fn free_money(&self, actor: &Actor) -> Result<FreeMoney, LedgerError>` — `authorize_forecast`, today from `self.today()` (`ledger/mod.rs`), `store.forecast_data(today)`, the forecast's `input(today, today, &settings_with(horizon 1, include_income false), data, Vec::new())`, `compute_free_money`; `OutOfRange` → `LedgerError::Invalid(TOO_LARGE)`.
- API: `GET /api/v1/free-money`, `operation_id = "getFreeMoney"`, tag forecast, needs `read` and the forecast's access; the description gives the formula in words, the window ("must pay through `must_pay_through`; essentials and flexible this month only"), that income and investments never count, that a currency without an emergency fund counts everything as free with a caution, and that currencies are never added together.
- MCP: `get_free_money`, `Needs::Read`, lead: "How much cash is free now in each currency, after what the plan still needs, what goals have set aside and the emergency fund. " + `MONEY`.
- Forecast words: `caution_sentence(Caution::EmergencyFundNotSet { currency }, can_manage)` → "No emergency fund is set for {code}, so everything left counts as free." plus 07c's link or "An admin can set one on the Household page."

- [ ] **Step 1: Write the failing tests** (`tests/it/free_money.rs`, real router, agent tokens):
  - `free_money_answers_the_formula` — seed through the API the household of Task 1's `the_formula_in_order` (accounts, balance, items, transactions, target) with the server's clock on 2027-01-15 → the JSON's numbers match.
  - `a_restricted_token_is_refused` — a token with grants below `all` at `transactions` → 403, the same body with and without items.
  - `a_member_reads_it` — a member's token (not admin) with full access → 200.
  - `no_target_shows_the_caution` — without a target: `cautions` holds `{"kind":"emergency_fund_not_set","currency":"USD"}`, `free_money` equals `cash_available − still_need − set_aside`.
  - `the_forecast_no_longer_blocks_without_a_target` — `GET /api/v1/forecast` has a `surplus` and the caution.
  - Parity row for `get_free_money`.
- [ ] **Step 2: Run** `… -p coffer-server … -E 'test(free_money) | test(forecast) | test(parity)'`. Expected: FAIL.
- [ ] **Step 3: Implement**; regenerate `docs/api/openapi.json`; update the docs.
- [ ] **Step 4: Run** the filtered tests, then `uptime` and `nice -n 15 scripts/gate.sh`. Expected: PASS, gate green.
- [ ] **Step 5: Commit, push, PR** — `git -C ../coffer-07h commit -m "Serve free money through the API and MCP"`; push `money-07h-free-money`; PR "Free money: calculation, API and MCP", "Closes #147", stating the formula, the planner's calls it relies on (30 days inclusive, completed goals, per-currency blocking), and that the e2e numbers spec (`staging/e2e/api/numbers.spec.mjs`) may need the new caution: CI checks it.
