# 07j — Plan history: effective-dated versions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A planned item's amount, currency, direction, category, spending kind and tier become effective-dated versions. Past months read the version in force then; a change applies from this month (the default) or from next month, never to a past month. The forecast, free money and plan versus actual resolve each occurrence's version, and a bill already paid this month stays paid when its amount changes. Closes #149.

**Architecture:** A new table `planned_item_versions` is the only home of the versioned values: append-only rows `(item_id, seq, effective_from, written_on, values)`. **The version in force in month `m` is the row with the highest `seq` among those with `effective_from ≤ m`** (the first row when none is). Since every write is effective from this month or next, no write can change what a past month reads, by construction, and a change from this month replaces one already planned from next month without deleting anything. The domain owns the rule (`plan/version.rs`: `PlanVersions::in_force`, `timeline`, `held_during`). The forecast is not rewritten reducer by reducer: the ledger hands it **one `ItemInput` per span of the item's timeline** (`ItemInput.in_force: MonthSpan`), and every occurrence the forecast counts goes through `ItemInput::occurrences`, which clips to that span. Plan versus actual looks the version up per month. Bill matching accepts, for an occurrence in month `m`, the amounts the item held during `m` (`held_during`), which keeps the owner's decision: lowering a paid bill doesn't un-pay it.

**Tech Stack:** Rust 2024, sqlx (SQLite), axum, utoipa, proptest.

**Spec:** §10.3 (plan history), §10.7 (a bill's spent is its payment), §4.3. Overview: unit 07j, "Owner's decisions on plan history" 1 and 2, planner's call 7 (the tier's CHECK moves into the versions table).

**Depends on:** 07e (#144), 07h (#147) merged. Parallel with 07i. **Branch / worktree:** `money-07j-plan-history`, `../coffer-07j`. **Migration:** `0032_planned_item_versions.sql`.

## Global Constraints

- Everything in the overview's "Rules for every unit" applies.
- Versioned: `amount` (with its currency), `direction`, `category`, `spending`, `tier`. Not versioned (apply to the whole item, as today): `name`, `notes`, `account_hint`, `active`, `schedule`. **`direction` is versioned although the overview's list omits it** (planner's call): `spending` and `tier` depend on it, so a version without it could hold an inflow with a tier.
- `effective_from` on `PATCH /api/v1/plan-items/{id}`: `"this_month"` (default), `"next_month"`, or a month `"YYYY-MM"` that must be this month or next (an explicit month lets an agent avoid a race at midnight on the 31st). Any other month → 422 "only this month's or next month's plan can change". `next_month` (or next month's `YYYY-MM`) only on a recurring item → else 422 "only a recurring item can change from next month": a goal's target and a one-off's amount change from this month. A one-off dated before this month refuses a change to a versioned field → 422 "a one-off in a past month keeps its amount, category and tier". `effective_from` with no versioned field changing is accepted and ignored. Garbage that isn't a keyword or a month stays the typed-JSON rejection (400), as an unknown enum value is today.
- The store offers no way to write a past version: `add_plan_version` takes the household's `today` and an `EffectiveFrom` and computes the month itself; there is no update or delete of a version row (item deletion cascades).
- A new item's first version is effective from its anchor's month (recurring: `anchor_on`; one-off: `on`; goal: `start_on`), as the migration does; `in_force` of a month before the first row is the first row, so moving an anchor earlier needs nothing.
- Settlements from the effective month on are ended when the amount or the schedule changes (today: from this month on for both); a next-month amount change keeps this month's settlements.
- The read-modify-write takes `today_seen_by` under the write lock (AGENTS.md), and the item's top-level fields in every answer are the version in force in the household's current month.
- Bill matching: an occurrence in month `m` matches a payment within ±10% of **any amount the item held during `m`** (same currency), and `tells_apart` accepts equality with any of them. `PaidBy.difference` stays the payment less the occurrence's current amount. So a 1,000 bill paid 1,000, lowered to 800 from this month, needs 0 and shows `difference` 200.00 (owner's decision 1). A row was held during `m` when it was in force for `m` at some moment of `m`: `effective_from ≤ m`, written on or before the last day of `m`, and not replaced for `m` by a later row written before `m` began.
- Words: "from this month", "from next month", "version" only in API descriptions; pages are 07m's.

## Review Focus

1. **The owner's paid bill.** 1,000 bill due on the 5th, paid 1,000 on the 5th, lowered to 800 from this month on the 10th: still need 0 in free money and the forecast, no `bill_unpaid` caution, `paid_by.difference` 200.00; plan versus actual's row planned 800, actual 1,000. → Task 2 `a_paid_bill_lowered_this_month_stays_paid`, Task 4 `lowering_a_paid_bill_keeps_it_paid`.
2. **Past months never move.** Any sequence of writes made in month `M` leaves `in_force(m)` unchanged for every `m < M`. → Task 1 proptest `writes_never_change_a_past_month`; Task 4 `a_past_month_is_422` and `plan_vs_actual_reads_last_months_version`.
3. **One item, two spans, one of everything.** A bill rising from next month gives two lines (each with its `effective_from`), its cautions once, no "budgets share a category" with itself, and the same totals as two separate items with clipped schedules. → Task 2 `two_spans_are_two_lines_and_one_item`, `an_items_cautions_are_raised_once`, proptest `spans_add_up_like_clipped_items`.
4. **An occurrence counted outside the forecast's clip.** → Task 2 step 3's `rg` check: no `schedule.occurrences(` or `occurrences_in(&item.schedule` left under `crates/domain/src/forecast/`.
5. **The migration loses nothing.** Every item gets one version equal to its old values; settlements and goal links survive the table rebuild. → Task 3 `migration_versions_every_item_and_keeps_settlements`.

---

### Task 1: Versions in the domain

**Files:**
- Create: `crates/domain/src/plan/version.rs`, `crates/domain/src/plan/version/tests.rs`
- Modify: `crates/domain/src/plan.rs` (`mod version;`, re-exports; four `PlanError`s: `PastVersion`, `NextMonthNeedsRecurring`, `PastOneOffKeepsValues`, `EmptyVersions`)

**Interfaces (`coffer_domain::plan`):**
```rust
pub struct PlanValues { pub amount: Money, pub direction: PlanDirection, pub category: CategoryId,
                        pub spending: Option<PlanSpending>, pub tier: Option<PlanTier> }
pub struct PlanVersion { pub seq: u32, pub effective_from: CalendarMonth, pub written_on: Date,
                         pub values: PlanValues }
/// Non-empty, ordered by `seq`, strictly increasing.
pub struct PlanVersions(Vec<PlanVersion>);
impl PlanVersions {
    pub fn new(rows: Vec<PlanVersion>) -> Result<Self, PlanError>;
    pub fn in_force(&self, month: CalendarMonth) -> &PlanValues;
    /// Each row that is ever in force, with the months it is: `from` None on the first span.
    pub fn timeline(&self) -> Vec<VersionSpan<'_>>;
    /// Every row in force for `month` at some moment of it (bill matching's amounts).
    pub fn held_during(&self, month: CalendarMonth) -> Vec<&PlanValues>;
    pub fn next_seq(&self) -> u32;
}
pub struct VersionSpan<'a> { pub span: MonthSpan, pub version: &'a PlanVersion }
/// Months `from` (inclusive, None: since ever) to `until` (exclusive, None: for ever).
pub struct MonthSpan { pub from: Option<CalendarMonth>, pub until: Option<CalendarMonth> }
impl MonthSpan { pub const ALWAYS: Self; pub fn contains(self, d: Date) -> bool;
                 pub fn overlaps(self, other: Self) -> bool; }
/// What a patch asks for; serde: "this_month" | "next_month" | "YYYY-MM"; ToSchema oneOf.
pub enum EffectiveFrom { ThisMonth, NextMonth, Month(CalendarMonth) }
impl EffectiveFrom {
    /// The month a change takes effect from, for an item of `item_type`.
    pub fn resolve(self, today: Date, item_type: PlanItemType) -> Result<CalendarMonth, PlanError>;
}
```
All derive `Debug, Clone, PartialEq, Eq` (and `Copy` where fields allow); `EffectiveFrom` and `MonthSpan` also `Serialize` and `ToSchema` under `openapi`.

- [ ] **Step 1: Write the failing tests** (`version/tests.rs`):
  - `the_latest_written_row_that_has_started_is_in_force` — rows A(2026-01, seq 1, 1,000), B(2026-12, seq 2, 1,200), C(2026-11, seq 3, 900): Oct reads A, Nov C, Dec C (C replaced the planned B), and with B and C swapped in seq, Dec reads B.
  - `before_the_first_row_the_first_is_in_force`.
  - `timeline_drops_a_row_never_in_force` — B above has no span; A spans `None..2026-11`, C `2026-11..None`.
  - `held_during_keeps_the_amount_a_month_opened_with` — A written 2026-01-03, C (eff 2026-11) written 2026-11-10: `held_during(2026-11)` = {A, C}; `held_during(2026-12)` = {C}; a row planned in October for November and replaced in October is not held in November.
  - `resolve_refuses_the_past_and_next_month_for_non_recurring` — today 2026-11-30: `Month(2026-10)` → `PastVersion`; `NextMonth` on a goal or one-off → `NextMonthNeedsRecurring`; `Month(2026-12)` on recurring → 2026-12.
  - `effective_from_parses_keywords_and_months` — JSON `"this_month"`, `"next_month"`, `"2026-12"`; `"soon"` fails.
  - Proptest `writes_never_change_a_past_month` — any history of writes, each in a month ≥ the previous and effective from its month or the next: after each write, `in_force(m)` for every earlier month equals what it was before.
- [ ] **Step 2: Run** `nice -n 15 env -u CARGO_TARGET_DIR cargo nextest run -p coffer-domain --all-features -j 3 -E 'test(version)'`. Expected: FAIL to compile.
- [ ] **Step 3: Implement.** **Step 4: Run** again. Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07j commit -m "Effective-dated versions of a planned item's values"`.

### Task 2: The forecast and plan versus actual read versions

**Files:**
- Modify: `crates/domain/src/forecast/types.rs` (`ItemInput.in_force: MonthSpan`, `ItemInput.also_matches: BTreeMap<CalendarMonth, Vec<Money>>`, `ItemInput::occurrences(&self, from, until_exclusive) -> Vec<Date>` and `occurrences_in(&self, month)`, both clipped to `in_force`; `ItemLine.effective_from: Option<CalendarMonth>`, the span's `from`; if the file passes ~780 lines, move `ItemInput`, `LineSource` and `CategorySpent` to `forecast/types/items.rs`), `forecast.rs` (line 256), `forecast/{reduction.rs, lines.rs, settled.rs, bill_matching.rs, free_money.rs}` (every `item.schedule.occurrences(` and `occurrences_in(&item.schedule, …)` becomes the clipped call; `Bill` gains the month's held amounts for `fits` and `tells_apart`), `forecast/cautions.rs` (category sharing compares `LineSource`, so an item's spans never share with themselves; item-level cautions deduplicated), `crates/domain/src/plan_report.rs` (`ReportItem`'s `category`, `amount`, `spending` replaced by `versions: PlanVersions`, read with `in_force(month)` in `planned_by_month`, the rollover and the goal line), the test helpers that build `ItemInput` (`forecast/tests.rs`, `forecast/tests/properties/oracle.rs`: `in_force: MonthSpan::ALWAYS`, empty `also_matches`) and `ReportItem` (`plan_report/tests.rs`: one version)
- Create: `crates/domain/src/forecast/tests/versions.rs` (+ `mod versions;`), `crates/domain/src/plan_report/tests/versions.rs`

- [ ] **Step 1: Write the failing tests:**
  - `a_paid_bill_lowered_this_month_stays_paid` — today 2026-11-12, rent span A (1,000, until 2026-11) and C (800, from 2026-11) with `also_matches[2026-11] = [1,000]`, a 1,000.00 payment on the 5th → C's payment `paid_by` set, `difference` 200.00, `counted` 0, no `BillUnpaid`; `compute_free_money`'s must pay still need 0.
  - `two_spans_are_two_lines_and_one_item` — rent 1,000 until 2026-12, 1,200 from 2026-12, horizon 3 from 2026-11-01 → two `ItemLine`s, `effective_from` None and 2026-12, gross 1,000 and 2,400.
  - `a_category_change_moves_the_budget_pool` — a budget in Groceries until next month, Household from it: this month's Groceries spending reduces it; next month's occurrence is in Household's pool.
  - `an_items_cautions_are_raised_once` — an item hinted at a closed account with two spans → one caution; two spans of one budget raise no shared-category caution.
  - `a_span_ended_last_month_still_answers_for_last_month` — last month's unpaid bill caution comes from the span in force last month.
  - Proptest `spans_add_up_like_clipped_items` — arbitrary recurring items with a second version from next month: totals equal those of the same items given as two separate items whose schedules are cut at the month (end_on / a later anchor).
  - Plan report: `plan_vs_actual_reads_each_months_version` — rent 1,000 until 2026-10, 1,100 from it: September's row planned 1,000, October's 1,100; a category change moves the planned amount between categories by month.
- [ ] **Step 2: Run** `… -p coffer-domain … -E 'test(versions) | test(forecast) | test(plan_report)'`. Expected: FAIL to compile.
- [ ] **Step 3: Implement**, then check `rg -n 'schedule\.occurrences\(|occurrences_in\(&item' crates/domain/src/forecast` prints nothing outside `types.rs`'s clipped helpers.
- [ ] **Step 4: Run** the whole domain crate. Expected: PASS (the oracle and golden tests unchanged with `MonthSpan::ALWAYS`).
- [ ] **Step 5: Commit** — `git -C ../coffer-07j commit -m "Count each occurrence at the version in force in its month"`.

### Task 3: The table, the migration and the store

**Files:**
- Create: `crates/store/migrations/0032_planned_item_versions.sql`, `crates/store/src/planned_items/versions.rs` (rows, `add_plan_version`, the batch read), `crates/store/tests/it/plan_versions.rs` (+ `mod plan_versions;`)
- Modify: `crates/store/src/planned_items.rs` (`columns!` without the moved columns; `from_row` validates each version against the schedule as it validates the item today; `PlannedItemRecord` keeps its flat `amount, direction, category, spending, tier` as **the version in force in the month the read named**, plus `versions: PlanVersions`; `NewPlannedItem` gains `written_on: Date`; `PlannedItemChanges` loses the five fields; `get_planned_item(id, month)`, `list_planned_items(include_inactive, month)`; `category_has_planned_items` reads versions, every one), `crates/store/src/{forecast.rs, plan_report.rs}` (items with versions; values at `as_of`'s month), `crates/store/src/lib.rs` (re-exports), `crates/store/tests/it/plan_tiers.rs` (its tier assertions read the version)

**Migration** (0022's method, `PRAGMA defer_foreign_keys = ON`): create `planned_item_versions (item_id TEXT NOT NULL REFERENCES planned_items (id) ON DELETE CASCADE, seq INTEGER NOT NULL CHECK (seq >= 1), effective_from TEXT NOT NULL CHECK (effective_from GLOB as 0018's month), written_on TEXT NOT NULL CHECK (written_on GLOB as 0015's dates), amount_minor …, currency …, direction …, category_id TEXT NOT NULL REFERENCES categories (id), spending …, tier …, CHECK ((direction IS 'outflow') = (tier IS NOT NULL)), CHECK (direction IS 'outflow' OR spending IS NULL), PRIMARY KEY (item_id, seq)) STRICT, WITHOUT ROWID` plus an index on `category_id`; copy one row per item (seq 1, `effective_from` = the anchor's month, `written_on` = `date(created_at / 1000, 'unixepoch')`, tier `COALESCE`d with 0031's defaults); hold `planned_item_settlements` in a copy (DROP's implicit delete cascades to it), rebuild `planned_items` without `category_id, amount_minor, currency, direction, spending, tier` and with its other CHECKs and indexes, re-insert the items, then the settlements, then the versions; drop the holding tables. The header comment says why the settlements are held and that `transaction_splits.goal_id` is uncounted by re-inserting the parents (0022).

- [ ] **Step 1: Write the failing tests** (`plan_versions.rs`, the `migrations_up_to` helper of `plan_tiers.rs` moved to `tests/it/main.rs` or a shared module):
  - `migration_versions_every_item_and_keeps_settlements` — at 0031: a recurring bill with a settlement, a goal with a linked split, an income one-off; migrate → one version each, values equal to the old columns, effective from the anchor's month; the settlement and the split's `goal_id` intact; `PRAGMA foreign_key_check` empty.
  - `a_version_from_next_month_leaves_this_month` — `add_plan_version(id, today, NextMonth, values)` → `get_planned_item(id, this)` old values, `(id, next)` new ones.
  - `no_month_outside_this_and_next_can_be_written` — `EffectiveFrom::Month(last month)` → `StoreError::Invalid`.
  - `the_check_refuses_an_inflow_with_a_tier` — a raw insert of an inflow version with a tier fails.
  - `deleting_an_item_deletes_its_versions`.
- [ ] **Step 2: Run** `… -p coffer-store … -E 'test(plan_versions) | test(plan_tiers) | test(plan)'`. Expected: FAIL.
- [ ] **Step 3: Implement.** **Step 4: Run** the store crate. Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07j commit -m "Store a planned item's values as versions (migration 0032)"`.

### Task 4: Ledger, API, MCP and docs

**Files:**
- Modify: `crates/server/src/ledger/plan_items.rs` (`PlannedItemPatch.effective_from: Option<EffectiveFrom>`; `PlannedItemView.versions: Vec<PlanVersionView>` from `timeline()`: `from` (null on the first), `until`, the five values, `written_on`; reads pass this month), `crates/server/src/ledger/plan_items/patch.rs` (split into item changes and a `NewPlanVersion` built from the values in force at the effective month; spending and tier resolved against those; audit `effective_from`), `crates/server/src/ledger/forecast.rs` (`stored_item` → `stored_items`, one `ItemInput` per `timeline()` span overlapping last month through the window's end, `also_matches` from `held_during` for last month, this month and next; hypothetical items `MonthSpan::ALWAYS`), `crates/server/src/ledger/plan_report.rs` (`ReportItem.versions`), `crates/server/src/http/api/plan_items.rs` (descriptions), every caller of `get_planned_item`/`list_planned_items` (pages `http/pages/plan*.rs` pass the household's month), `docs/api/openapi.json`, `docs/agent-forecast.md` (spans, lines per version, matching across held amounts), `docs/agent-usage.md` (`effective_from`), `AGENTS.md` (crate map: `plan/version.rs`, `planned_items/versions.rs`)
- Create: `crates/server/tests/it/plan_items/versions.rs` (+ `mod versions;`), `crates/server/tests/it/forecast/versions.rs`

- [ ] **Step 1: Write the failing tests** (real router, clock fixed on 2026-11-10):
  - `a_change_applies_from_this_month_by_default` — PATCH amount → top-level amount new, `versions` two spans, `from` 2026-11.
  - `next_month_leaves_this_month_and_keeps_its_settlement` — a settled bill raised from next month: this month's settlement stays; `GET /forecast` (horizon 2) has two lines.
  - `a_past_month_is_422` — `"effective_from": "2026-10"` → 422 with its message; `"2026-12"` → 200; `"later"` → 400.
  - `next_month_on_a_goal_or_one_off_is_422`, `a_past_one_off_keeps_its_amount` (422), `effective_from_alone_changes_nothing` (200, no audit row).
  - `lowering_a_paid_bill_keeps_it_paid` — the owner's case through the API: free money's must pay still need excludes it, the forecast has no unpaid caution, the payment's `difference` is 200.00, `GET /plan-vs-actual` shows planned 800 and actual 1,000.
  - `plan_vs_actual_reads_last_months_version` — raised in October from October, read for September → the old amount.
  - MCP: `get_plan_item` answers `versions` (the existing parity row compares the bodies; no new tool, since plan writes are REST-only today).
- [ ] **Step 2: Run** `… -p coffer-server … -E 'test(versions) | test(plan_items) | test(forecast) | test(free_money) | test(plan_vs_actual) | test(parity)'`. Expected: FAIL.
- [ ] **Step 3: Implement**; regenerate `docs/api/openapi.json`; update the docs.
- [ ] **Step 4: Run** the filtered tests, then `uptime` and `nice -n 15 scripts/gate.sh`. Expected: PASS, gate green.
- [ ] **Step 5: Commit, push, PR** — `git -C ../coffer-07j commit -m "Change a plan item from this month or next, through the API"`; push `money-07j-plan-history`; PR "Plan history: effective-dated versions", "Closes #149", naming the planner's calls (direction versioned; next month only on recurring items; the held-amounts matching rule; the table rebuild) and that the staging seed is unchanged (CI's e2e checks the API shape).

## Owner's decisions (2026-11-08)

Each question below was settled as recommended (owner's decision, 2026-11-08), except where a line says otherwise.

1. **Plan writes in MCP.** Today MCP reads the plan but can't change it; `effective_from` is a parameter of a REST-only action, so 07j adds no tool. Recommended: keep it so here, and open an issue for `create_plan_item`/`update_plan_item` tools before 07m makes plan changes a main flow.
2. **A goal's target from next month.** Recommended: no (a goal's target is a total by a date, and a change from this month already leaves past months' set aside as it was).
