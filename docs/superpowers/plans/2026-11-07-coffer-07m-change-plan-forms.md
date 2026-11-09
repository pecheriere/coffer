# 07m — Change the plan from Money, one form per kind Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The plan is changed from Money: one add form per kind (bill, budget, income, one-off, goal), each kind's edit form with "from this month" or "from next month" where that applies, delete, and income and paused items listed on `/money`. `/plan` redirects to `/money`, and the old combined form goes. Closes #152.

**Architecture:** Pages only, posting to the ledger calls the API uses (`create_planned_item`, `update_planned_item` with 07j's `effective_from`, `delete_planned_item`). The kind decides the fields, so each kind has a small render module and they share one field set and one form reader. The form reader moves from `pages/plan/form.rs` to `pages/money/change/submit.rs`; `pages/plan/` is deleted except `words.rs`, which moves to `pages/money/plan_words.rs` (its schedule and tier words are used by both pages). Every flow is a plain form with POST-redirect-GET (303), and a refusal re-renders the form with the ledger's message, as `/plan` does today.

**Tech Stack:** Rust 2024, axum, maud.

**Spec:** §5 (Money: "Change the plan adds and edits buckets; this replaces today's Plan page, its add form split by kind"), §10.3. Overview: unit 07m.

**Depends on:** 07j (#149), 07l (#151) merged. **Branch / worktree:** `money-07m-change-plan`, `../coffer-07m`. No migration.

## Global Constraints

- Everything in the overview's "Rules for every unit" applies; CSP, theme and wording rules; read-only viewers see no forms and their POSTs are refused (`may_write`).
- Kinds and what they fix (planner's call): **bill** = recurring outflow, spending `bill`, tier must pay by default (essentials or flexible allowed); **budget** = recurring outflow, spending `budget`, tier flexible by default (must pay or essentials allowed); **income** = recurring inflow, no tier; **one-off** = one date, "money out" (tier must pay by default) or "money in"; **goal** = target by a date, always saving toward. The form sends `direction`, `schedule.type`, `spending` and `tier` from the kind; the person never picks them separately.
- Routes: `GET /money/plan/new` (choose a kind: five links with one line each), `GET|POST /money/plan/new/{kind}`, `GET|POST /money/buckets/{id}/edit`, `GET|POST /money/buckets/{id}/delete` (the confirm page moves from `/plan/{id}/delete`). Unknown kind → 404 page.
- "From this month" / "From next month": a radio on a recurring item's edit form, this month checked; sent as `effective_from`. Its hint: "Earlier months keep what they planned." A one-off's and a goal's edit form has no choice and says "Changes apply from this month". When the item already has a version starting next month, the edit form says so ("From December: $1,200 a month is already planned") and a change from this month replaces it, which the hint states (07j's rule).
- `/money` gains, after the emergency fund: "Income" (active planned inflows: name, amount, schedule in words, edit link), "Paused" (inactive items, with resume for writers), and "Change the plan" leading to `/money/plan/new`. Both lists read `Ledger::list_planned_items` as the API does.
- `/plan` → 303 to `/money`; `/plan/{id}` → 303 to `/money/buckets/{id}`; the old POST routes go (404). `forecast.rs`'s "Add them on the plan page" link goes to `/money/plan/new`.
- No file over ~800 lines: each kind's module stays small; the shared fields and the reader are their own files.

## Review Focus

1. **The kind can't be changed by a form.** A posted `type`, `direction` or `spending` that disagrees with the kind is ignored in favour of the route's kind; an edit keeps the item's kind. → Task 2 `a_forged_kind_field_changes_nothing`.
2. **From next month reaches the API.** Editing a budget's amount from next month leaves this month's bucket and makes next month's version. → Task 2 `edit_from_next_month_keeps_this_month`.
3. **Nothing lost in the move.** Every refusal the old form showed is still shown, with the ledger's message, on the kind's form. → Task 2 `refusals_show_on_the_form` (the cases of `tests/it/plan_page/refusals.rs`, moved).
4. **Old links.** `/plan` and `/plan/{id}` redirect; their POSTs are gone. → Task 3 `old_plan_links_redirect`.

---

### Task 1: The form reader and the shared fields move

**Files:**
- Create: `crates/server/src/http/pages/money/change.rs` (routes and `Kind`), `crates/server/src/http/pages/money/change/{submit.rs, fields.rs}`
- Move: `crates/server/src/http/pages/plan/form.rs`'s reading half (`ItemForm`, `ItemFields`, `schedule_of`, `tier_of`, parsers) → `submit.rs`; its shared inputs (`category_field`, `account_field`, name, amount, notes) → `fields.rs`; `plan/words.rs` → `money/plan_words.rs`

**Interfaces:**
```rust
pub(super) enum Kind { Bill, Budget, Income, OneOff, Goal }
impl Kind { pub(super) fn of(item: &PlannedItemView) -> Self; pub(super) fn slug(self) -> &'static str;
            pub(super) fn parse(slug: &str) -> Option<Self>; }
// submit.rs: the kind fixes type, direction and spending
pub(super) fn input_of(kind: Kind, form: &ItemForm) -> Result<PlannedItemInput, PageError>;
pub(super) fn patch_of(kind: Kind, form: &ItemForm, current: &PlannedItemView) -> Result<PlannedItemPatch, PageError>;
```
- [ ] **Step 1: Write the failing tests** (unit, in `submit.rs`): `each_kind_fixes_its_type_direction_and_spending`, `a_one_off_reads_money_in_or_out`, `effective_from_is_read_only_for_recurring`, `kind_of_an_item_round_trips` (every kind's `slug` parses back; `Kind::of` agrees with the fields).
- [ ] **Step 2: Run** `nice -n 15 env -u CARGO_TARGET_DIR cargo nextest run -p coffer-server --all-features -j 3 -E 'test(change::)'`. Expected: FAIL. **Step 3: Implement** (the old page still compiles against the moved code until Task 3). **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07m commit -m "Read a plan form by kind"`.

### Task 2: One add and edit form per kind

**Files:**
- Create: `crates/server/src/http/pages/money/change/{bill.rs, budget.rs, income.rs, one_off.rs, goal.rs}` (each `fn fields(choices, item: Option<&PlannedItemView>) -> Markup`), `crates/server/src/http/pages/money/change/delete.rs` (moved from `plan/delete.rs`), `crates/server/tests/it/pages/money_change/{mod.rs, add.rs, edit.rs, refusals.rs, notes.rs, tiers.rs}` (the `plan_page` tests moved and rewritten for the new routes)
- Modify: `crates/server/src/http/pages/money.rs` (routes; "Income", "Paused" and "Change the plan" sections), `crates/server/src/http/pages/money/bucket.rs` (an "Edit" link and the delete link for writers), `crates/server/src/http/pages/word_checks.rs`

- [ ] **Step 1: Write the failing tests:** `each_kind_adds_its_item` (five POSTs → 303 to `/money`, the items as the API reads them), `a_forged_kind_field_changes_nothing`, `edit_from_next_month_keeps_this_month`, `edit_from_this_month_says_earlier_months_keep_theirs`, `a_planned_next_month_change_is_shown`, `a_goals_edit_has_no_month_choice`, `refusals_show_on_the_form`, `notes_keep_their_line_breaks`, `tiers_offer_only_the_kinds_choices` (bill and budget: three; one-off out: three; income, goal: none), `income_and_paused_items_are_listed`, `delete_confirms_then_removes`, `a_reader_sees_no_forms` (and POSTs → 403).
- [ ] **Step 2: Run** `… -E 'test(money_change)'`. Expected: FAIL. **Step 3: Implement.** **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07m commit -m "Add and change the plan from Money, one form per kind"`.

### Task 3: Retire `/plan`

**Files:**
- Delete: `crates/server/src/http/pages/plan.rs`, `crates/server/src/http/pages/plan/{edit.rs, form.rs, delete.rs, tests.rs}`, `crates/server/tests/it/plan_page/` (moved in Task 2)
- Create: `crates/server/src/http/pages/money/redirects.rs` (`/plan`, `/plan/{id}`)
- Modify: `crates/server/src/http/pages/mod.rs` (routes), `crates/server/src/http/pages/forecast.rs` (the link), `crates/server/src/http/pages/money/bucket.rs` (`set_active`'s shared helper now lives beside it), `crates/server/tests/it/main.rs` (`mod plan_page;` goes), `AGENTS.md` (crate map), `docs/agent-usage.md` (where people change the plan)

- [ ] **Step 1: Write the failing tests:** `old_plan_links_redirect` (`GET /plan` → 303 `/money`, `GET /plan/{id}` → 303 `/money/buckets/{id}`, `POST /plan` → 404), `the_forecast_links_the_add_page`.
- [ ] **Step 2: Run** `… -E 'test(redirect) | test(forecast)'`. Expected: FAIL. **Step 3: Implement.** **Step 4: Run** the filtered tests, then `uptime` and `nice -n 15 scripts/gate.sh`. Expected: PASS.
- [ ] **Step 5: Commit, push, PR** — `git -C ../coffer-07m commit -m "Send the plan page to Money"`; push `money-07m-change-plan`; PR "Change the plan from Money, one form per kind", "Closes #152". It says the e2e browser specs that used `/plan` change with it (if any do: `rg -n "/plan" staging/e2e`), checked only by CI.

## Open questions for the owner

1. **One-off income.** Recommended: the one-off form asks "money out" or "money in", and the income form is for repeating income only, so a bonus is a one-off. The alternative, a sixth kind, adds a form for a rare case.
