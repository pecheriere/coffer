# 07n — Covers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A cover is a stored, audited, undoable record that takes an overspend of this month from another bucket, a goal or the emergency fund instead of free money. Sources follow the tiers; a cover lowers the source's planned amount for the month and raises the target's, a goal cover draws on the goal, and an emergency fund cover changes no number. Buckets, free money and the forecast apply covers; `POST/GET/DELETE /api/v1/covers`, MCP `create_cover` and `list_covers`. Closes #153.

**Architecture:** The rule lives in the domain (`plan/cover.rs`: `check_cover`), pure, on the tiers and the month's buckets. Covers enter the forecast's shared reduction as a per-item adjustment of `as_of`'s month (`ItemAdjustment.covered`), so `still_needed`, free money, the forecast's first month and 07k's buckets read one number; a goal's draws add to what was spent from it, exactly where goal-linked spending already enters `goal_need` (§10.2). The ledger checks a new cover against the buckets of this month as 07k computes them, under the write lock, so two covers can't both take the same source's last dollars.

**Tech Stack:** Rust 2024, sqlx, axum, utoipa, rmcp.

**Spec:** §4.1 (who can lend to whom), §4.6 (2, 3), §10.2 (goal and emergency fund covers), §10.7 (must pay receives, never gives). Overview: unit 07n.

**Depends on:** 07k (#150) merged. **Branch / worktree:** `money-07n-covers`, `../coffer-07n`. **Migration:** `0033_covers.sql`.

## Global Constraints

- Everything in the overview's "Rules for every unit" applies.
- Sources by the source's tier (§4.1): **must pay** never; **essentials** only to essentials; **flexible** to essentials or flexible; **saving toward** (a goal) to anything, only with `confirmed: true`; **the emergency fund** to anything, only with `confirmed: true`. Investments are never a source (they are not buckets). The target is any bucket of this month (must pay included, §10.7), never a goal or income. Source and target in the same currency, and never the same item.
- Amount: positive, at most the target's uncovered over this month, and at most what the source has: a bucket's `left` (after covers), a goal's `set_aside` (after draws). Emergency fund: only the first bound (the fund is stateless, §4.4).
- Month: this month only (planner's call: an overspend needing a source lasts until the month ends, §10.6, and past months are history). Undo: `DELETE` of a cover of this month; a past month's cover is 409 "a past month's cover stays as it was".
- Effects in `as_of`'s month: source bucket planned −amount, target planned +amount; goal source: what was spent from it +amount (so `set_aside` drops and the goal shows `short`); emergency fund source: nothing but the record, and the target's over counts as covered (it leaves 07o's "needs a source").
- `short` on a goal (07k's `GoalBucket` gains it, with `drawn`): what the draws took from what it would have set aside, `goal_need(target, start, due, as_of, spent) − goal_need(target, start, due, as_of, spent + drawn)`. It shrinks as the accrual catches up or the due date moves (§10.2).
- Authorization: create and delete as plan writes (`authorize_plan_write`); list as the forecast (`authorize_forecast`). Each create and delete is audited (`cover.create`, `cover.delete`) with who and when; the row keeps `member` and `agent`.
- MCP: `create_cover` (`Needs::Write`) and `list_covers`; undo stays REST, as MCP deletes nothing. The create tool's lead says to cover only when the member asked, and that saving toward and emergency fund sources need `confirmed`.

## Review Focus

1. **Must pay never lends; restaurants never take the mortgage.** Every refused source-target pair by tier. → Task 1 `sources_follow_the_tiers` (table-driven, all 5×3 pairs).
2. **Double spending a source.** Two concurrent covers each within the source's left but together past it: one is 422. → Task 3 `two_covers_cant_take_the_same_dollars`.
3. **Free money comes back.** Restaurants 64 over, covered 64 from Hobbies (100 left): free money rises by 64 against no cover; buckets show Hobbies planned −64, Restaurants +64, over 0. → Task 2 `a_cover_moves_planned_and_free_money_comes_back`, Task 3 `cover_through_the_api`.
4. **Goal and emergency fund.** A goal cover drops `set_aside` and shows `short`; a fund cover leaves free money and every bucket number unchanged except that the target's over is covered. → Task 2 `a_goal_cover_draws_on_the_goal`, `an_emergency_fund_cover_changes_no_number`.
5. **Forecast and free money stay one.** 07h's proptest `on_the_first_still_need_is_the_forecasts_outflows` holds with arbitrary covers between must pay items. → Task 2 (the proptest extended).

---

### Task 1: The rule in the domain

**Files:**
- Create: `crates/domain/src/plan/cover.rs`, `crates/domain/src/plan/cover/tests.rs`
- Modify: `crates/domain/src/plan.rs` (`mod cover;`, re-exports; `PlanError::{CoverFromMustPay, CoverNotAllowed, CoverNeedsConfirmation, CoverTooLarge, CoverCurrencyDiffers, CoverToItself, CoverTargetNotBucket}`)

**Interfaces:**
```rust
pub enum CoverSource { Bucket(PlannedItemId), EmergencyFund }   // serde tag "kind": bucket | emergency_fund
pub struct CoverRequest { pub from: CoverSource, pub to: PlannedItemId, pub amount: Money, pub confirmed: bool }
pub struct CoverSide { pub tier: Option<PlanTier>, pub currency: Currency, pub has_minor: i64 }  // has: left, set aside, or uncovered over
pub fn may_lend(from: PlanTier, to: PlanTier) -> bool;
pub fn check_cover(request: &CoverRequest, from: Option<&CoverSide>, to: &CoverSide) -> Result<(), PlanError>;
```
- [ ] **Step 1: Write the failing tests:** `sources_follow_the_tiers`, `saving_toward_and_the_fund_need_confirmation`, `a_cover_is_bounded_by_both_sides`, `currency_must_match`, `an_item_cant_cover_itself`.
- [ ] **Step 2: Run** `nice -n 15 env -u CARGO_TARGET_DIR cargo nextest run -p coffer-domain --all-features -j 3 -E 'test(cover)'`. Expected: FAIL. **Step 3: Implement.** **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07n commit -m "Which bucket may cover which, by tier"`.

### Task 2: Covers in the reduction, free money and buckets

**Files:**
- Modify: `crates/domain/src/forecast/types.rs` (`ForecastInput.covers: Vec<CoverInput { from: CoverSource, to: PlannedItemId, amount: Money }>`, this month's only), `forecast/reduction.rs` (`ItemAdjustment.covered: i128`: + to the target, − to a source bucket, for the span in force this month), `forecast/lines.rs` (`still_needed` reads `covered` against this month's occurrences), `forecast/free_money.rs` (goal draws add to the goal's spent), `forecast/buckets.rs` and `buckets/types.rs` (`Bucket.covered_in`, `covered_out`, `covers: Vec<CoverId>`; `GoalBucket.drawn`, `short`), the forecast test helpers (`covers: Vec::new()`)
- Create: `crates/domain/src/forecast/tests/covers.rs` (+ `mod covers;`), `crates/domain/src/forecast/buckets/tests/covers.rs`

- [ ] **Step 1: Write the failing tests:** `a_cover_moves_planned_and_free_money_comes_back`, `a_goal_cover_draws_on_the_goal`, `an_emergency_fund_cover_changes_no_number`, `a_cover_to_a_paid_bill_absorbs_its_overspend` (insurance 100 paid 108, covered 8 from flexible: over 0), `a_cover_never_makes_a_need_negative`, and 07h's proptest extended with covers.
- [ ] **Step 2: Run** `… -p coffer-domain … -E 'test(covers) | test(free_money) | test(buckets)'`. Expected: FAIL. **Step 3: Implement.** **Step 4: Run** the domain crate. Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07n commit -m "Apply covers to the reduction, free money and buckets"`.

### Task 3: Table, store, ledger, API and MCP

**Files:**
- Create: `crates/store/migrations/0033_covers.sql`, `crates/store/src/covers.rs`, `crates/store/tests/it/covers.rs`, `crates/server/src/ledger/covers.rs`, `crates/server/src/http/api/covers.rs`, `crates/server/tests/it/covers.rs`, `crates/server/tests/it/mcp/parity/covers.rs` (+ the `mod` lines)
- Modify: `crates/domain/src/ids.rs` (`CoverId`), `crates/store/src/{lib.rs, forecast.rs}` (this month's covers in `ForecastData`), `crates/server/src/ledger/{mod.rs, forecast.rs, buckets.rs}`, `crates/server/src/http/api/{mod.rs, openapi.rs}`, `crates/server/src/http/api/mcp/tools.rs`, `docs/api/openapi.json`, `docs/agent-usage.md` (a "Covers" section), `docs/agent-forecast.md`, `AGENTS.md` (crate map)

**Migration:** `covers (id TEXT PRIMARY KEY, month TEXT NOT NULL CHECK (GLOB as 0018), source TEXT NOT NULL CHECK (source IN ('bucket', 'emergency_fund')), from_item TEXT REFERENCES planned_items (id) ON DELETE CASCADE, to_item TEXT NOT NULL REFERENCES planned_items (id) ON DELETE CASCADE, amount_minor INTEGER NOT NULL CHECK (amount_minor BETWEEN 1 AND 1000000000000000), currency TEXT NOT NULL CHECK (length(currency) = 3), member TEXT NOT NULL REFERENCES members (id), agent TEXT REFERENCES agent_tokens (id), created_at INTEGER NOT NULL, CHECK ((source IS 'bucket') = (from_item IS NOT NULL)), CHECK (from_item IS NOT to_item)) STRICT`, indexes on `month`, `from_item`, `to_item`.

**Interfaces:**
- Ledger: `create_cover(actor, CoverRequest) -> CoverView` (`authorize_plan_write`; `today_seen_by`; this month's buckets read in the write transaction; `check_cover`; insert; audit), `list_covers(actor, month: Option<CalendarMonth>) -> Vec<CoverView>`, `delete_cover(actor, CoverId)`.
- API: `POST /api/v1/covers` (`createCover`, `Idempotency-Key` as `createPlanItem`), `GET /api/v1/covers?month=` (`listCovers`), `DELETE /api/v1/covers/{cover_id}` (`deleteCover`). `CoverView { id, month, from: CoverSource, to, amount, member, agent, created_at }`. A hidden item (`plan_item_visible`) on either side answers 404.

- [ ] **Step 1: Write the failing tests:** store `covers_roundtrip_and_cascade` and `the_check_refuses_a_bucket_source_without_an_item`; server `cover_through_the_api`, `must_pay_as_a_source_is_422`, `a_goal_source_without_confirmation_is_422`, `two_covers_cant_take_the_same_dollars` (two tasks racing: one 201, one 422), `undo_restores_free_money`, `a_past_months_cover_cant_be_deleted` (409), `a_reader_lists_but_cant_create` (403), `a_hidden_item_is_404`, parity rows for `create_cover` and `list_covers`.
- [ ] **Step 2: Run** `… -p coffer-store … -E 'test(covers)'` and `… -p coffer-server … -E 'test(covers) | test(parity) | test(buckets) | test(free_money)'`. Expected: FAIL.
- [ ] **Step 3: Implement**; regenerate `docs/api/openapi.json`; update the docs.
- [ ] **Step 4: Run** the filtered tests, then `uptime` and `nice -n 15 scripts/gate.sh`. Expected: PASS.
- [ ] **Step 5: Commit, push, PR** — `git -C ../coffer-07n commit -m "Store covers and serve them through the API and MCP"`; push; PR "Covers", "Closes #153", naming the planner's calls (this month only; the two bounds on the amount).

## Owner's decisions (2026-11-08)

Each question below was settled as recommended (owner's decision, 2026-11-08), except where a line says otherwise.

1. **Covers in past months.** Recommended: this month only; once a month ends its overspends simply came from free money (§10.6). Allowing last month would let history be rewritten after the fact.
