# 07r — Keep or invest Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Per currency, suggest investing `free money − cushion`, where the cushion keeps the 3-month low point at or above the emergency fund (§4.5); show it only at or above the household's invest minimum (default 500 major units), withhold it without an emergency fund (§10.5), and let "Not now" hide it until free money moves past the dismissed amount by more than the minimum (§10.6). API, MCP, and a per-currency thresholds form on the Household page. Closes #157.

**Architecture:** A pure domain function, `forecast/keep_or_invest.rs`, combines free money (07h) and the cash projection (07q) with the minimums and the stored dismissals; it reads nothing. The ledger computes free money and the projection from one `forecast_data` read (one `ForecastInput`, two computations), then applies the stored settings. Migration `0035` adds the invest minimum beside 07o's auto-cover threshold in `currency_thresholds`, and a small `invest_dismissals` table.

**Tech Stack:** Rust 2024, sqlx (SQLite), axum, utoipa, rmcp, maud, proptest.

**Spec:** `docs/superpowers/specs/2026-11-07-coffer-money-redesign-design.md` §4.5 (one rule, minimum, no "I moved it", "Not now"), §5 (Household: invest minimum), §6, §10.5 (withheld without a fund), §10.6 ("Not now" stored as the dismissed amount). Overview: planner's call 10 (thresholds per currency).

**Depends on:** 07c (#142), 07q (#156), **and 07o (#154)**, merged. *Correction to the overview:* `0035` alters 07o's `currency_thresholds` (`0034`), so 07r must follow 07o; the overview lists only 07c and 07q. This moves 07r to wave 8 and 07s to wave 9. **Branch / worktree:** `money-07r-keep-or-invest`, `../coffer-07r`. **Migration:** `0035`.

**Interface from 07o** (plan `2026-11-07-coffer-07o-needs-a-look.md`; check the merged code's names before starting): table `currency_thresholds (currency TEXT PRIMARY KEY, auto_cover_minor INTEGER NULL) STRICT, WITHOUT ROWID`, where a missing row or a NULL column means the default; `GET /api/v1/household/thresholds` (`listThresholds`) and `PUT /api/v1/household/thresholds/{currency}` (`setCurrencyThresholds`, admin only, audited by field) with optional fields using the `Patch` type plan item patches use: a field absent leaves it, a number sets it, `null` puts the default back; MCP `get_thresholds` and `set_currency_thresholds`. 07r adds `invest_minimum` to the table, the body (same absent/number/null rule), the list and both MCP tools. 07p builds no thresholds form: the Household form for both fields is this unit's (Task 3).

## Global Constraints

- Everything in the overview's "Rules for every unit" applies.
- The rule, per currency (planner's call, reading §4.5 against 07q's line, which is already net of what's spoken for):
  ```
  could invest = max(min(free money, low point − emergency fund target), 0)
  cushion      = free money − could invest
  ```
  State, first that applies: `no_number` (free money or the low point is null) → `no_emergency_fund` (no target for the currency: `could_invest` and `cushion` are null, §10.5) → `not_now` (a dismissal whose `free_money` is within the minimum of today's, inclusive) → `below_minimum` (`could_invest < minimum`) → `suggest`.
- Minimum default: `500 × 10^exponent` minor units (`Currency::exponent`), `DEFAULT_INVEST_MINIMUM_MAJOR = 500`; set range 0 to 10^15 minor units, as the emergency fund's.
- "Not now" stores today's free money and `could_invest` for that currency, replacing any earlier one; it needs `WriteHousehold` and the forecast's access (it reveals free money), is refused (422 "there is no suggestion to put off for {code}") unless the state is `suggest`, and is audited (`household.invest.not_now`). Nothing records a transfer: coffer never moves money (§4.5).
- Setting the minimum is the household's (`ManageHousehold`), audited, as 07o's threshold.
- Exact integer maths; a value outside `i64` → `ForecastError::OutOfRange` → 422 as the forecast does.

## Review Focus

1. **Overstating what can be invested without a fund** (§10.5): no target → `no_emergency_fund`, `could_invest` null in the API, no card, no MCP number. → Task 1 `no_emergency_fund_withholds_the_number`, Task 2 `without_a_fund_there_is_no_number`.
2. **The low point under the fund.** Free money 4,000, target 3,000, low point 5,000 → could invest 2,000, cushion 2,000; low point 2,500 → 0, `below_minimum`. → Task 1 `the_low_point_caps_the_suggestion`, `a_low_point_under_the_fund_suggests_nothing`.
3. **"Not now" returning too early or never.** Dismissed at free money 4,000 with minimum 500: hidden at 4,500 and 3,500 (inclusive), back at 4,501 and 3,499. → Task 1 proptest `not_now_hides_within_the_minimum_only`.
4. **Defaults by exponent.** JPY's default minimum is 500 (exponent 0), USD's 50,000. → Task 1 `the_default_minimum_follows_the_exponent`.
5. **A setting freezing the other's default.** Setting only the invest minimum leaves `auto_cover_minor` null. → Task 2 `setting_one_threshold_keeps_the_others_default`.

---

### Task 1: The rule in the domain

**Files:**
- Create: `crates/domain/src/forecast/keep_or_invest.rs` (with `keep_or_invest/types.rs` if it passes about 300 lines), `crates/domain/src/forecast/tests/keep_or_invest.rs`
- Modify: `crates/domain/src/forecast.rs` (`mod keep_or_invest;`, re-exports), 07o's thresholds type in `crates/domain/src/plan/` (add `invest_minimum_minor: Option<i64>`, its validation, `fn invest_minimum(currency, Option<&CurrencyThresholds>) -> i64`), `crates/domain/src/forecast/tests.rs`, `AGENTS.md` (crate map)

**Interfaces:**
- Produces (`coffer_domain::forecast`):
  ```rust
  pub fn keep_or_invest(free: &FreeMoney, projection: &CashProjection,
                        minimums: &BTreeMap<Currency, i64>,        // resolved, defaults applied
                        dismissals: &BTreeMap<Currency, InvestDismissal>)
      -> Result<KeepOrInvest, ForecastError>;
  pub struct KeepOrInvest { pub as_of: Date, pub status: ForecastStatus,
      pub currencies: Vec<InvestSuggestion>, pub blockers: Vec<Blocker>, pub cautions: Vec<Caution> }
  pub struct InvestSuggestion {
      pub currency: Currency,
      pub state: SuggestionState,       // tagged by `kind`: suggest | below_minimum |
                                        // no_emergency_fund | not_now | no_number
      #[schema(required)] pub free_money: Option<Money>,
      #[schema(required)] pub could_invest: Option<Money>,
      #[schema(required)] pub cushion: Option<Money>,
      pub minimum: Money,
      pub emergency_fund_target: Money,
      #[schema(required)] pub low_point: Option<LowPoint>,   // 07q's
      #[schema(required)] pub dismissed: Option<InvestDismissal>,
  }
  pub struct InvestDismissal { pub free_money: Money, pub could_invest: Money, pub on: Date }
  ```
  Status, blockers and cautions: the projection's (its forecast reads 3 months with income at the same day, so its blockers are free money's and its cautions include free money's), so one answer never states two statuses.

- [ ] **Step 1: Write the failing tests** (`tests/keep_or_invest.rs`, built from `FreeMoney` and `CashProjection` values directly):
  - `the_mockups_sentence` — free money 4,180.00, target 3,000.00, low point 6,000.00 on Dec 14 → could invest 3,000.00, cushion 1,180.00, `suggest`.
  - `the_low_point_caps_the_suggestion`, `a_low_point_under_the_fund_suggests_nothing` (Review Focus 2).
  - `free_money_caps_the_suggestion` — low point far above → could invest equals free money, cushion 0.
  - `no_emergency_fund_withholds_the_number`, `a_blocked_currency_has_no_number`.
  - `below_the_minimum_is_not_suggested` — could invest 499.99 with minimum 500.00; exactly 500.00 is suggested.
  - `the_default_minimum_follows_the_exponent` (Review Focus 4).
  - Proptest `not_now_hides_within_the_minimum_only` (Review Focus 3) and `cushion_plus_suggestion_is_free_money` (for every `suggest` and `below_minimum`).
- [ ] **Step 2: Run** `nice -n 15 env -u CARGO_TARGET_DIR cargo nextest run -p coffer-domain --all-features -j 3 -E 'test(keep_or_invest)'`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the domain crate. Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07r commit -m "Suggest what could be invested, keeping the low point above the emergency fund"`

### Task 2: Store, ledger, API and MCP

**Files:**
- Create: `crates/store/migrations/0035_invest_minimum.sql`, `crates/store/src/invest_dismissals.rs`, `crates/server/src/ledger/keep_or_invest.rs`, `crates/server/src/http/api/keep_or_invest.rs`, `crates/server/tests/it/keep_or_invest.rs` (+ `mod keep_or_invest;` in `tests/it/main.rs`)
- Modify: 07o's `crates/store/src/currency_thresholds.rs`, `crates/store/src/lib.rs`, 07o's ledger thresholds setter and `crates/server/src/http/api/household.rs` (`invest_minimum` in the body and the settings), `crates/server/src/ledger/mod.rs`, `crates/server/src/http/api/{mod.rs, openapi.rs}`, `crates/server/src/http/api/mcp/tools.rs` (or its area file after the split), `crates/server/tests/it/mcp/parity/mod.rs`, `docs/api/openapi.json`, `docs/agent-forecast.md` ("Keep or invest"), `docs/agent-usage.md`

**Interfaces:**
- Migration: `ALTER TABLE currency_thresholds ADD COLUMN invest_minimum_minor INTEGER CHECK (invest_minimum_minor BETWEEN 0 AND 1000000000000000);` and `CREATE TABLE invest_dismissals (currency TEXT PRIMARY KEY CHECK (…as 0030…), free_money_minor INTEGER NOT NULL CHECK (free_money_minor >= 0), could_invest_minor INTEGER NOT NULL CHECK (could_invest_minor >= 0), on_date TEXT NOT NULL) STRICT, WITHOUT ROWID;`
- Store: `invest_dismissals() -> BTreeMap<Currency, InvestDismissal>` (on `Store` and the write transaction), `put_invest_dismissal(currency, &InvestDismissal)`.
- Ledger: `pub async fn keep_or_invest(&self, actor) -> Result<KeepOrInvest, LedgerError>` (`authorize_forecast`; one `forecast_data(today)`; free money's settings and the projection's settings over the same data — share a helper with `ledger/free_money.rs` and `ledger/projection.rs` rather than reading twice; then thresholds and dismissals); `pub async fn put_off_invest_suggestion(&self, actor, currency) -> Result<KeepOrInvest, LedgerError>` (`WriteHousehold`, then the forecast's access, both before reading; recompute, refuse unless `suggest`, write and audit in one transaction).
- API: `GET /api/v1/keep-or-invest` (`getKeepOrInvest`, tag forecast); `POST /api/v1/keep-or-invest/{currency}/not-now` (`putOffInvestSuggestion`, needs `write`). Descriptions state the rule, the minimum and its default, that no fund means no number, how "Not now" expires, and that coffer never moves money: the transfer lowers free money once it syncs.
- MCP: `get_keep_or_invest` (`Needs::Read`; lead: "Whether each currency has cash worth investing, after a cushion that keeps the next 3 months above the emergency fund. Report `could_invest` only when `state.kind` is `suggest`; this is not investment advice: never name what to buy."), `put_off_invest_suggestion` (`Needs::Write`; "Only when the member asked to put it off."); 07o's `set_currency_thresholds` gains `invest_minimum`.

- [ ] **Step 1: Write the failing tests** (`tests/it/keep_or_invest.rs`, clock on 2027-01-15, seeded through the API):
  - `the_suggestion_answers_the_rule` — a household whose low point sits above the target → `suggest` with the expected amounts.
  - `without_a_fund_there_is_no_number` — `no_emergency_fund`, `could_invest` null.
  - `not_now_hides_it_until_free_money_moves` — POST → `not_now`; a transaction of minimum + 0.01 out → `suggest` again.
  - `not_now_needs_a_suggestion` — below the minimum → 422.
  - `not_now_is_audited`, `a_restricted_token_is_refused` (GET and POST, same body whatever exists), `a_read_token_cannot_put_it_off` (403).
  - `setting_one_threshold_keeps_the_others_default` (Review Focus 5), `the_minimum_is_audited`.
  - Migration test beside 07o's: an existing threshold row survives `0035` with `invest_minimum_minor` null.
  - Parity rows for both tools.
- [ ] **Step 2: Run** `… -p coffer-store … -j 3` and `… -p coffer-server --all-features -j 3 -E 'test(keep_or_invest) | test(threshold) | test(parity)'`. Expected: FAIL.
- [ ] **Step 3: Implement**; regenerate `docs/api/openapi.json`; update the docs.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07r commit -m "Serve keep or invest, its minimum and Not now through the API and MCP"`

### Task 3: Thresholds form on the Household page

**Files:**
- Create: `crates/server/src/http/pages/household/thresholds.rs`, `crates/server/tests/it/pages/household/thresholds.rs` (+ `mod` line)
- Modify: `crates/server/src/http/pages/household.rs` (the section after the emergency fund's, `id="thresholds"`)

**Interfaces:** one form per currency in use (the emergency fund form's list), fields "Cover overspends silently up to" (auto-cover) and "Suggest investing from" (invest minimum), both prefilled with the default when unset, posting to `/admin/household/thresholds/{currency}`, which calls 07o's ledger setter. Admins only see the form; others see the values.

- [ ] **Step 1: Write the failing tests** — `an_admin_sets_both_thresholds`, `a_member_sees_values_not_forms`, `a_bad_amount_rerenders_with_the_error`, `the_page_shows_the_defaults`.
- [ ] **Step 2: Run** `… -E 'test(pages::household)'`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run**, then `uptime` and `nice -n 15 scripts/gate.sh`. Expected: PASS, gate green.
- [ ] **Step 5: Commit, push, PR** — `git -C ../coffer-07r commit -m "Set thresholds per currency on the Household page"`; push `money-07r-keep-or-invest`; PR "Keep or invest", "Closes #157", naming the dependency on 07o and what was verified locally versus by CI.

## Cross-unit decision

The Household thresholds form (both fields) is built here, not in 07p: 07p's "threshold form on Household" moves to this unit, since 07r follows 07o and has both setters.

## Open questions for the owner

1. **Who may press "Not now"?** Recommended: any member who can write household data (`WriteHousehold`) and see the forecast, since it hides a card on everyone's Home; admins only would leave members with a card they can't dismiss.
