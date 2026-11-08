# 07e — Tiers on planned items Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every planned outflow carries a tier (must pay, essentials, flexible, saving toward), defaulted by kind for existing and new items, in the domain, store, API, MCP and the forecast's inputs; an emergency fund of N months counts must pay and essentials only. Closes #144.

**Architecture:** A `PlanTier` string enum and one pure function, `resolved_tier`, decide what tier an item ends up with on create and on patch; the ledger only calls it. A nullable `tier` column with a partial CHECK holds it (see Review Focus 1), and the store refuses an outflow read without one. `ItemInput` carries the tier so 07h can split "still need" by tier, and the forecast's months target filters on it.

**Tech Stack:** Rust 2024, sqlx (SQLite), axum, utoipa, rmcp, proptest.

**Spec:** `docs/superpowers/specs/2026-11-07-coffer-money-redesign-design.md` §3 (words), §4.1 (tiers and defaults), §4.4 (months of must pay plus essentials). Overview: planner's calls 6 and 7.

**Depends on:** 07b (#141) merged (the months target lives there). **Branch / worktree:** `money-07e-tiers`, `../coffer-07e`. **Owns migration `0031`.**

## Global Constraints

- Everything in the overview's "Rules for every unit" applies.
- Wire values: `must_pay`, `essentials`, `flexible`, `saving_toward`. Page words (07f): "Must pay", "Essentials", "Flexible", "Saving toward". API descriptions say "tier" and, once, that pages call these items buckets (§3).
- Rules (planner's call 6): income has no tier; a goal is always `saving_toward`, and nothing else is; a recurring or one-off outflow is `must_pay`, `essentials` or `flexible`. Defaults: bill → `must_pay`, budget → `flexible`, one-off → `must_pay`, goal → `saving_toward`.
- On a patch the tier changes only when sent, or when the item stops being an outflow (it loses it) or starts being one (it takes the default for its kind unless one is sent). Changing a bill to a budget keeps the tier.
- Refusals are 422 with fixed messages: `"income has no tier"`, `"a goal is always saving toward"`, `"only a goal is saving toward"`. Never echo input.
- The emergency fund is not a tier value (planner's call 6).
- Audit: `tier` in the create entry and in an update's changes when it changes.

## Review Focus

1. **The migration leaving an outflow without a tier**, or giving one to income. SQLite refuses a cross-column CHECK on `ADD COLUMN` over existing rows (tried on 3.54: "CHECK constraint failed"), and rebuilding `planned_items` would cascade-delete `planned_item_settlements` (`ON DELETE CASCADE` fires on `DROP TABLE` with foreign keys on). So the CHECK is partial and the migration fills every outflow in the same transaction. → Task 1 `migration_tiers_every_outflow` through the real `Migrator`, and `an_outflow_without_a_tier_is_corrupt`.
2. **A direction change crossing the CHECK**: outflow → inflow must clear `tier` in the same `UPDATE`, or the row fails the CHECK. → Task 2 `becoming_income_drops_the_tier`, `becoming_an_outflow_takes_the_default`.
3. **What-if items**: a hypothetical outflow takes the same default and rules, so a what-if's months target matches a stored item's. → Task 3 `hypothetical_items_take_a_tier`.
4. **The months target shrinking**: from 07b's "all recurring outflows" to must pay plus essentials. A household with only flexible budgets gets a 0 target and the existing `EmergencyFundMonthsIsZero` caution. → Task 3 `months_target_counts_must_pay_and_essentials`.
5. **Restricted tokens**: `tier` is a field of an item the token can already see; nothing new is visible. No test beyond the existing grants tests passing.

---

### Task 1: `PlanTier`, `resolved_tier` and the column

**Files:**
- Create: `crates/domain/src/plan/tier.rs`, `crates/store/migrations/0031_planned_item_tier.sql`
- Modify: `crates/domain/src/plan.rs` (module, re-exports, `PlanError::{UnknownTier, IncomeHasNoTier, GoalIsSavingToward, OnlyGoalSavesToward}`), `crates/store/src/planned_items.rs` (`tier` in `PlannedItemRecord`, `NewPlannedItem`, `PlannedItemChanges: Option<Option<PlanTier>>`, the columns, row parsing, insert, update), `crates/store/tests/it/plan.rs`

**Interfaces:**
- Produces (`coffer_domain::plan`):
  ```rust
  string_enum!(
      /// Which bucket a planned outflow's money is set in, deciding what may cover an
      /// overspend: never from must pay, and from saving toward only when a person chooses.
      PlanTier, PlanError::UnknownTier,
      { MustPay => "must_pay", Essentials => "essentials", Flexible => "flexible",
        SavingToward => "saving_toward" }
  );
  /// The tier an item ends up with: `sent`, or the default for its kind when `sent` is `None`.
  /// # Errors
  /// `IncomeHasNoTier`, `GoalIsSavingToward`, `OnlyGoalSavesToward`.
  pub fn resolved_tier(
      sent: Option<PlanTier>,
      direction: PlanDirection,
      schedule: &PlanSchedule,
      spending: Option<PlanSpending>,
  ) -> Result<Option<PlanTier>, PlanError>;
  #[must_use]
  pub fn default_tier(direction: PlanDirection, schedule: &PlanSchedule, spending: Option<PlanSpending>) -> Option<PlanTier>;
  ```
  `string_enum!` gives `ToSchema` under `openapi` as the other enums do.
- Schema `0031` (tried in this form on SQLite 3.54; coffer bundles 3.51.3, so the store test runs the real `Migrator`):
  ```sql
  -- A planned outflow's tier (money redesign §4.1). SQLite can't add a CHECK across columns to
  -- a table that has rows, and rebuilding planned_items would cascade-delete its settlements, so
  -- this CHECK allows NULL and the store refuses an outflow read without a tier.
  ALTER TABLE planned_items ADD COLUMN tier TEXT
      CHECK (tier IN ('must_pay', 'essentials', 'flexible', 'saving_toward'))
      CHECK (tier IS NULL
          OR (direction = 'outflow' AND (tier = 'saving_toward') = (item_type = 'goal')));

  UPDATE planned_items SET tier = CASE
      WHEN item_type = 'goal' THEN 'saving_toward'
      WHEN spending = 'budget' THEN 'flexible'
      ELSE 'must_pay'
  END
  WHERE direction = 'outflow';
  ```

- [ ] **Step 1: Write the failing tests**
  - Domain (`tier.rs`): `defaults_by_kind` (bill → must pay, budget → flexible, one-off outflow → must pay, goal → saving toward, any inflow → none); `rules_refuse_each_case` (inflow + `Some(Flexible)` → `IncomeHasNoTier`; goal + `Some(MustPay)` → `GoalIsSavingToward`; bill + `Some(SavingToward)` → `OnlyGoalSavesToward`); `a_bill_may_be_any_spending_tier` (must pay, essentials, flexible all accepted); proptest `resolved_tier_always_passes_the_table` over every direction × schedule kind × spending × sent: an `Ok` result satisfies the SQL CHECK's predicate, written in Rust in the test.
  - Store (`tests/it/plan.rs`): `migration_tiers_every_outflow` (apply migrations below 31 with `sqlx::raw_sql`, seed a bill, a budget, a one-off, a goal and an income item, a settlement on the bill; apply 31; read: must pay, flexible, must pay, saving toward, none; the settlement still there); `an_outflow_without_a_tier_is_corrupt` (`UPDATE planned_items SET tier = NULL` on an outflow by raw SQL → `get_planned_item` is a `StoreError::Backend` from `convert::corrupt`, as `lib.rs:644` checks for other rows); `the_check_refuses_a_goal_marked_flexible` (raw SQL update fails).
- [ ] **Step 2: Run** `… -p coffer-domain … -E 'test(tier)'`, `… -p coffer-store … -E 'test(plan)'`. Expected: FAIL.
- [ ] **Step 3: Implement** the enum, the two functions, the migration, the store fields. Callers in `coffer-server` won't compile until Task 2; run the two crates only.
- [ ] **Step 4: Run** the filtered tests. Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07e commit -m "Give every planned outflow a tier"` (WIP; the gate runs at the end).

### Task 2: Ledger, API and MCP

**Files:**
- Modify: `crates/server/src/ledger/plan_items.rs` (if it passes about 780 lines, move `CheckedPatch` and its `changes` into `ledger/plan_items/patch.rs` in this change), `crates/server/src/http/api/plan_items.rs` (descriptions), `crates/server/src/http/api/mcp/tools.rs` (the `list_plan_items`/`get_plan_item` leads mention tiers), `docs/api/openapi.json`, `docs/agent-forecast.md`, `docs/agent-usage.md`
- Test: `crates/server/tests/it/plan_items/tiers.rs` (new, `mod tiers;` in `plan_items/mod.rs`), `crates/server/tests/it/plan_items/validation.rs` (refusals)

**Interfaces:**
- `PlannedItemView.tier: Option<PlanTier>` (`#[schema(required)]`, "null for income"); `PlannedItemInput.tier: Option<PlanTier>`; `PlannedItemPatch.tier: Option<PlanTier>` (absent keeps; there is no `null`: an outflow always has one).
- Create: `resolved_tier(input.tier, direction, &schedule, spending)`. Patch: the item's resulting direction, schedule and spending decide; `sent = patch.tier`, else keep the current tier when the item stays an outflow, else default (becoming an outflow) or none (becoming income). Map the `PlanError`s to the three messages in `invalid`.

- [ ] **Step 1: Write the failing tests** (`plan_items/tiers.rs`): `new_items_take_their_default` (POST a bill, a budget, a one-off, a goal, an income without `tier`: answers must pay, flexible, must pay, saving toward, null); `a_chosen_tier_is_kept` (budget with `essentials`); `refusals_are_422_with_their_message` (the three cases); `a_patch_changes_only_what_it_sends` (patch name only: tier unchanged; patch `tier: essentials`: changed, audited `tier: [flexible, essentials]`); `becoming_income_drops_the_tier`; `becoming_an_outflow_takes_the_default`; `bill_to_budget_keeps_the_tier` (a bill set to essentials, patched to `spending: budget` stays essentials). Parity unchanged (no new tool), but the MCP parity tests for plan items must still pass with the new field.
- [ ] **Step 2: Run** `… -p coffer-server … -E 'test(plan_items)'`. Expected: FAIL.
- [ ] **Step 3: Implement**; regenerate `docs/api/openapi.json`; docs describe tiers and defaults.
- [ ] **Step 4: Run** the tests. Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07e commit -m "Read and set a planned item's tier through the API"`

### Task 3: The forecast carries tiers; months targets count must pay and essentials

**Files:**
- Modify: `crates/domain/src/forecast/types.rs` (`ItemInput.tier: Option<PlanTier>`), `crates/domain/src/forecast.rs` (`add_emergency_funds`), domain forecast tests' item builders (one helper each, `rg -n 'fn item' crates/domain/src/forecast/tests`), `crates/server/src/ledger/forecast.rs` (`input` maps the record's tier; `HypotheticalItem.tier: Option<PlanTier>` resolved with `resolved_tier`), `crates/server/src/http/api/forecast.rs` (what-if description), `docs/api/openapi.json`, `docs/agent-forecast.md`
- Test: `crates/domain/src/forecast/tests/goals_and_emergency_fund.rs`, `crates/server/tests/it/forecast/what_if.rs`

**Interfaces:**
- Consumes: `PlanTier`, `resolved_tier` (Task 1).
- Produces: `ItemInput.tier` (07h reads it). A months target sums recurring outflows whose tier is `MustPay` or `Essentials`, in its currency, over `[as_of, months_after(as_of, months))`.

- [ ] **Step 1: Write the failing tests**
  - Domain: `months_target_counts_must_pay_and_essentials` — USD monthly: rent 1000.00 must pay, groceries 400.00 essentials, restaurants 200.00 flexible; `{USD: Months 3}` → `emergency_fund` 4200.00. `only_flexible_makes_a_zero_months_target` — only restaurants → 0 and `EmergencyFundMonthsIsZero { USD, 3 }`.
  - Server: `hypothetical_items_take_a_tier` — a what-if adding a monthly essentials bill of 100.00 with `{USD: Months 2}` raises `emergency_fund` by 200.00; with `tier: flexible` it doesn't; with `tier: saving_toward` on a bill → 422.
- [ ] **Step 2: Run** `… -p coffer-domain … -E 'test(emergency_fund)'` and `… -p coffer-server … -E 'test(what_if)'`. Expected: FAIL.
- [ ] **Step 3: Implement**; regenerate the snapshot; docs say what a months target counts.
- [ ] **Step 4: Run** the tests, then `uptime` and `nice -n 15 scripts/gate.sh`. Expected: PASS, gate green.
- [ ] **Step 5: Commit, push, PR** — `git -C ../coffer-07e commit -m "Count must pay and essentials in a months emergency fund"`; push `money-07e-tiers`; PR "Tiers on planned items", "Closes #144", naming the partial CHECK decision and the migration test.
