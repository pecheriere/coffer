# 07o — Needs a look and the auto-cover threshold (API, MCP) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A derived list of what needs the owner this month: overspends free money covered (silently at or below the auto-cover threshold, listed for a look above it), overspends free money can't absorb with the sources allowed to cover them, bills paid over or under plan within the ±10% window, budgets ahead of pace with their run-out day, and transactions to check. An auto-cover threshold per currency (default 100 major units). `GET /api/v1/needs-a-look`, threshold endpoints, MCP tools. Closes #154.

**Architecture:** Nothing is stored but the threshold (§10.6). The list is a pure domain function, `forecast::needs_a_look`, over this month's `Buckets` (07k, with 07n's covers), this month's `FreeMoney` (07h) and the thresholds; the review count is added by the ledger from `count_transactions_to_review`. Thresholds live in `currency_thresholds`, one row per currency, each column nullable so 07r can add the invest minimum to the same row; a NULL or missing value is the default.

**Tech Stack:** Rust 2024, sqlx, axum, utoipa, rmcp.

**Spec:** §4.6 (1, 4, 5, the pace heads-up), §10.6 (derived), §10.7 (bills over or under), §10.10 (no pace for long-period budgets). Overview: unit 07o, planner's call 10.

**Depends on:** 07n (#153) merged. **Branch / worktree:** `money-07o-needs-a-look`, `../coffer-07o`. **Migration:** `0034_currency_thresholds.sql`.

## Global Constraints

- Everything in the overview's "Rules for every unit" applies.
- Per currency, this month, with `O` = the sum of uncovered overspends (each bucket's `over − covered_in`, positive) and `before` = 07h's `before`:
  - free money's share `F = clamp(before + O − target, 0, O)`: what free money covered before the emergency fund or a shortfall was next.
  - `F` goes to the overspends **smallest first** (planner's call: the most overspends absorbed whole, the fewest asking for a choice), ties by tier then name.
  - An overspend wholly in `F`: `auto_covered` when its uncovered over ≤ the threshold, else `covered_from_free_money`. One partly or not in `F`: `needs_a_source { over, free_money_covers, rest, sources }`, `sources` = every bucket of the currency that may lend to it (`plan::may_lend`) with its `left`, every goal with its `set_aside` (`needs_confirmation: true`), and the emergency fund (`needs_confirmation: true`), each with `available > 0`.
- `bill_differs { item, on, planned, paid, difference }` for each occurrence this month paid within ±10% at a different amount (both over and under, §10.7). Over is already an overspend too; this entry says why.
- `ahead_of_pace { item, spent, planned, runs_out_on }` when `elapsed_permille ≥ 250`, the bucket has a pace (07k: monthly or more often), `spent_permille > elapsed_permille`, and the run-out day falls in this month: `runs_out_on = month_start + ceil(planned × days_elapsed / spent) − 1` days, integer maths, days elapsed including today.
- `transactions_to_check { count }` when the actor's review count > 0.
- Order: needs a source, covered from free money, ahead of pace, bills that differ, transactions to check, auto covered (after the fact, §4.6.5).
- Threshold: `auto_cover` per currency, default `100 × 10^exponent` minor units; 0 to 10^15. `GET /api/v1/household/thresholds` lists every currency in use (07h's set) with its value and `is_default`; `PUT /api/v1/household/thresholds/{currency}/auto-cover` `{ "minor_units": n }` sets it; `DELETE` of the same resets it (REST only). Setting needs an admin, as the emergency fund does; reading as household settings.
- `needs-a-look` authorization as the forecast. Read-only.

## Review Focus

1. **The $640 example.** before − target leaves 180 free before Restaurants' 640 over → one `needs_a_source` with `free_money_covers` 180, `rest` 460 and Hobbies, Clothes, the Japan trip and the emergency fund as sources, never a must pay bucket. → Task 1 `free_money_covers_part_and_the_rest_needs_a_source`.
2. **The threshold boundary.** Over exactly 100.00 with threshold 100.00 → `auto_covered`; 100.01 → `covered_from_free_money`. → Task 1 `the_threshold_is_inclusive`.
3. **Pace noise on the 2nd.** Day 7 of 30 (233‰): nothing; day 8 (267‰) with spending ahead: listed with its run-out day; a quarterly budget never. → Task 1 `no_heads_up_before_a_quarter_of_the_month`, `a_long_period_budget_gets_no_heads_up`.
4. **A cover takes it off the list.** Covering the 460 from Hobbies removes the entry. → Task 3 `a_cover_takes_it_off_the_list`.

---

### Task 1: The list in the domain

**Files:**
- Create: `crates/domain/src/forecast/needs_a_look.rs`, `crates/domain/src/forecast/needs_a_look/tests.rs`
- Modify: `crates/domain/src/forecast.rs` (`mod needs_a_look;`, re-exports), `AGENTS.md` (crate map)

**Interfaces (`coffer_domain::forecast`):**
```rust
pub struct NeedsALookInput<'a> { pub buckets: &'a Buckets, pub free_money: &'a FreeMoney,
                                 pub auto_cover: &'a BTreeMap<Currency, Money>, pub to_review: u64 }
pub fn needs_a_look(input: &NeedsALookInput<'_>) -> Result<NeedsALook, ForecastError>;
pub struct NeedsALook { pub month: CalendarMonth, pub as_of: Date, pub entries: Vec<LookEntry> }
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LookEntry {
    NeedsASource { item: PlannedItemId, over: Money, free_money_covers: Money, rest: Money, sources: Vec<CoverOption> },
    CoveredFromFreeMoney { item: PlannedItemId, over: Money },
    AheadOfPace { item: PlannedItemId, spent: Money, planned: Money, runs_out_on: Date },
    BillDiffers { item: PlannedItemId, on: Date, planned: Money, paid: Money, difference: Money },
    TransactionsToCheck { count: u64 },
    AutoCovered { item: PlannedItemId, over: Money },
}
pub struct CoverOption { pub from: CoverSource, pub available: Money, pub needs_confirmation: bool }
```
- [ ] **Step 1: Write the failing tests:** `free_money_covers_part_and_the_rest_needs_a_source`, `the_threshold_is_inclusive`, `free_money_goes_to_the_smallest_first`, `a_currency_is_never_netted_with_another`, `bills_over_and_under_are_listed`, `no_heads_up_before_a_quarter_of_the_month`, `runs_out_on_is_exact` (planned 300, spent 150 by day 10 → runs out on day 20), `a_long_period_budget_gets_no_heads_up`, `a_covered_overspend_is_gone`, `transactions_to_check_only_when_some`, `the_order_is_fixed`.
- [ ] **Step 2: Run** `nice -n 15 env -u CARGO_TARGET_DIR cargo nextest run -p coffer-domain --all-features -j 3 -E 'test(needs_a_look)'`. Expected: FAIL. **Step 3: Implement.** **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07o commit -m "Work out what needs a look this month"`.

### Task 2: Thresholds: table, store, API, MCP

**Files:**
- Create: `crates/store/migrations/0034_currency_thresholds.sql`, `crates/store/src/thresholds.rs`, `crates/store/tests/it/thresholds.rs`, `crates/domain/src/plan/thresholds.rs` (`default_auto_cover(Currency) -> Money`, `validate_threshold`), `crates/server/src/ledger/thresholds.rs`, `crates/server/tests/it/thresholds.rs` (+ `mod` lines)
- Modify: `crates/store/src/lib.rs`, `crates/server/src/http/api/household.rs` (three operations: `listThresholds`, `setAutoCoverThreshold`, `resetAutoCoverThreshold`; if it passes ~700 lines, `household/thresholds.rs`), `crates/server/src/http/api/{mod.rs, openapi.rs}`, `crates/server/src/http/api/mcp/tools.rs` (`get_thresholds`, `set_auto_cover_threshold`), `crates/server/tests/it/mcp/parity/` (two rows)

**Migration:** `currency_thresholds (currency TEXT PRIMARY KEY CHECK (length(currency) = 3 AND currency NOT GLOB '*[^A-Z]*'), auto_cover_minor INTEGER CHECK (auto_cover_minor BETWEEN 0 AND 1000000000000000)) STRICT, WITHOUT ROWID`. NULL: the default. 07r adds `invest_minimum_minor` the same way.

- [ ] **Step 1: Write the failing tests:** store `thresholds_roundtrip_and_reset`; server `thresholds_list_defaults_for_currencies_in_use` (USD 100.00, JPY 100, `is_default` true), `an_admin_sets_and_resets`, `a_member_cant_set` (403), `out_of_range_is_422`, parity rows.
- [ ] **Step 2: Run** `… -p coffer-store … -E 'test(thresholds)'`, `… -p coffer-server … -E 'test(thresholds) | test(parity)'`. Expected: FAIL. **Step 3: Implement.** **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07o commit -m "Keep an auto-cover threshold per currency (migration 0034)"`.

### Task 3: Ledger, API and MCP for the list

**Files:**
- Create: `crates/server/src/ledger/needs_a_look.rs`, `crates/server/src/http/api/needs_a_look.rs`, `crates/server/tests/it/needs_a_look.rs` (+ `mod` line)
- Modify: `crates/server/src/ledger/mod.rs`, `crates/server/src/http/api/{mod.rs, openapi.rs}`, `crates/server/src/http/api/mcp/tools.rs` (`get_needs_a_look`), `crates/server/tests/it/mcp/parity/`, `docs/api/openapi.json`, `docs/agent-usage.md` (a "Needs a look" section: each kind and what to do about it, e.g. `needs_a_source` → `create_cover` with the member's choice)

**Interfaces:** `Ledger::needs_a_look(actor) -> Result<NeedsALook, LedgerError>`: `authorize_forecast`; this month's buckets and free money from one store read; thresholds; `count_transactions_to_review(actor)`. API `GET /api/v1/needs-a-look` (`getNeedsALook`, tag forecast). MCP `get_needs_a_look`, lead "What needs the member this month: overspends and where they could be covered from, bills paid off plan, buckets running out early, transactions to check. Nothing here is acted on until the member chooses. " + `MONEY`.

- [ ] **Step 1: Write the failing tests:** `needs_a_look_lists_the_seeded_month` (one entry of each kind), `a_cover_takes_it_off_the_list`, `a_raised_threshold_moves_an_entry_to_auto_covered`, `a_restricted_token_is_refused`, parity row.
- [ ] **Step 2: Run** `… -p coffer-server … -E 'test(needs_a_look) | test(parity)'`. Expected: FAIL. **Step 3: Implement**; regenerate `docs/api/openapi.json`; docs.
- [ ] **Step 4: Run** the filtered tests, then `uptime` and `nice -n 15 scripts/gate.sh`. Expected: PASS.
- [ ] **Step 5: Commit, push, PR** — `git -C ../coffer-07o commit -m "Serve what needs a look through the API and MCP"`; push; PR "Needs a look and the auto-cover threshold", "Closes #154", naming the planner's calls (smallest first; thresholds admin-only).

## Open questions for the owner

1. **Who shares out free money among several overspends.** Recommended: smallest first, so the fewest overspends ask for a choice. The alternative, the order they happened, needs the day each bucket crossed its plan, which coffer doesn't keep.
