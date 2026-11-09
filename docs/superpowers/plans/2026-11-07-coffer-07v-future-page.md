# 07v — Future page Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The `Future` tab: the headline ("You reach $2M around 2041, somewhere between 2039 and 2045"), one chart (net worth history, the likely line with its range, the target line, milestones, what-ifs dotted), the inputs as editable chips, the what-ifs with add and remove, and once, calmly, what this month's overspending costs in future money. Milestones and that cost join the Future answer in the API first, since every number the page shows is in the API. Closes #161.

**Architecture:** Task 1 extends 07t's domain answer: `milestones` (goals' due dates and the last payment of must pay bills that end, read from the planned items) and `overspending` (this month's overspend in the display currency from 07k's buckets, grown at the likely rate with 07t's `grow`). Tasks 2–3 are the page: `pages/future.rs` with `future/{chart.rs, words.rs, inputs.rs, what_ifs.rs}`, calling `Ledger::future`, `update_future_settings`, `add_future_what_if` and `remove_future_what_if`, the calls the API makes. The chart is integer SVG built on the shared `pages/charts.rs` scale maths (07s), with a table equivalent. Chips are `<details>` with a one-field form inside: no JS.

**Tech Stack:** Rust 2024, axum, maud, SVG, CSS tokens, utoipa, rmcp.

**Spec:** `docs/superpowers/specs/2026-11-07-coffer-money-redesign-design.md` §4.7 (all of it), §5 (Future tab; CSP, SVG with text equivalents, plain forms, read-only roles), §2 (non-goals: no Monte Carlo; "a calm note, never a nag"). Overview: planner's call 2 (the tab arrives here).

**Depends on:** 07t (#159), 07u (#160), **and 07k (#150)** merged. *Correction to the overview:* the overspending note needs this month's overspend from 07k's buckets; the overview lists only 07t and 07u. `pages/charts.rs` comes from 07s; if 07s hasn't merged when this unit starts, this unit makes that extraction with 07s's exact names and whichever merges second keeps one copy on rebase. **Branch / worktree:** `money-07v-future-page`, `../coffer-07v`. No migration.

**Interface assumed from 07k:** `Ledger::buckets(actor, month: CalendarMonth) -> Buckets` (the call behind `GET /api/v1/buckets`), whose per-currency buckets each carry `over: Money` (spent beyond planned, after covers once 07n has merged, 0 when not over). If 07k names it differently, use its names: this unit needs only "this month's total over, per currency".

## Global Constraints

- Everything in the overview's "Rules for every unit" applies.
- **Milestones** (planner's calls): an active, not completed goal whose `due_on` falls in `(today, horizon end]` ("{name} due"); an active recurring must pay outflow with `end_on` whose last occurrence falls in that window ("{name} ends": the mortgage end, when the mortgage is a bill with an end date). Sorted by date; at most 12 on the chart (all in the list).
- **Overspending cost:** `over` = Σ of this month's buckets' `over` in the display currency only (never converted; other currencies say nothing); shown when `over > 0`; `by` = the likely target date when there is one, else the horizon's end; `grown = grow(over, 0, likely growth, months to by)`. One sentence, `p.fine` at the page's foot: "This month's overspending: $310 more than planned, about $610 by 2040 if left invested." No colour, no icon, no repeat elsewhere.
- **Headline** (words in `future/words.rs`): target reached later: "You reach {target} around {likely year}, somewhere between {earliest year} and {latest year}." (one year when all three agree); already: "You've reached your target of {target}."; not within 25 years: "At this pace you don't reach {target} within 25 years."; no target: "In 25 years: about {likely}, between {low} and {high}, in today's money."; blocked: the blocker's sentence (no display currency: a link to the Household page for an admin, "An admin can choose one" otherwise).
- CSP and themes: no `style=`; SVG in integers, classes on tokens (`.history-line`, `.likely-line`, `.range-band`, `.target-line`, `.what-if-line` dotted by `stroke-dasharray` in `base.css`, `.milestone`); the SVG is `aria-hidden` and a table (every 5 years: low, likely, high), the milestone list and the what-if sentences are its text equivalent.
- Chips (`details.chip`): "Saving {amount} a month" (with "from the last 12 months" or "set by you", and a "Use the last 12 months" button that clears it), "Target {amount}" (or "Set a target"), "Growth {5}% a year" (a percent with up to two decimals → basis points exactly, via `DecimalAmount`), "House counted / left out". Each posts one field to `/future/settings` with CSRF, POST-redirect-GET to `/future`, errors re-rendered beside the chip. Read-only viewers see the values only.
- The tab: `Nav::Future` between Money and Activity (§5: Home, Money, Future, Activity, More), `/future`.

## Review Focus

1. **Every number on the page is in the API**: milestones and the overspending cost come from `getFuture`, never computed in the page. → Task 1 `future_answers_milestones_and_overspending` (API), Task 3 `the_page_shows_what_the_api_answers`.
2. **The nag anti-goal.** The overspending sentence appears once, only when over > 0, and nowhere else. → Task 3 `the_overspending_note_shows_once_and_only_when_over`.
3. **A chart that lies at the edges**: a negative history, a target below today, a what-if never reaching, 300 months of what-ifs all draw inside the box. → Task 2 `the_scale_holds_everything_drawn`, `a_target_below_the_history_still_draws`.
4. **Growth parsing.** "5", "5.5", "5.25" → 500, 550, 525; "5.255" and "abc" re-render with an error; out of range is the API's 422 shown beside the chip. → Task 3 `growth_chip_parses_exactly`.
5. **Five tabs on a phone.** Five tabs fit the bottom bar at 375 px without wrapping (CSS only). → Task 2 `the_nav_has_five_tabs_in_order`, and the PR's 375 px screenshot.

---

### Task 1: Milestones and the overspending cost in the API

**Files:**
- Create: `crates/domain/src/future/milestones.rs`, `crates/domain/src/future/tests/milestones.rs`
- Modify: `crates/domain/src/future/{types.rs, projection.rs}` (`FutureInput.milestone_items`, `FutureInput.over_this_month: Option<Money>`; `Future.milestones: Vec<Milestone>`, `Future.overspending: Option<OverspendingCost>`), `crates/store/src/future.rs` (`future_data` reads the active planned items' id, name, schedule, direction and tier), `crates/server/src/ledger/future.rs` (calls `buckets(actor, this month)` after its own authorization), `crates/server/src/http/api/future.rs` (description), `crates/server/tests/it/future/mod.rs`, `docs/api/openapi.json`, `docs/agent-future.md`
  ```rust
  pub struct Milestone { pub on: Date, pub kind: MilestoneKind /* goal_due | bill_ends */,
                         pub item: PlannedItemId, pub name: String }
  pub struct OverspendingCost { pub this_month: Money, pub by: Date, pub grown: Money }
  ```
- [ ] **Step 1: Write the failing tests** — domain: `a_goal_due_in_the_window_is_a_milestone`, `a_completed_or_overdue_goal_is_not`, `a_must_pay_bill_that_ends_marks_its_last_payment`, `a_flexible_budget_that_ends_is_not_a_milestone`, `overspending_grows_to_the_target_date`, `overspending_without_a_target_grows_to_the_horizon`, `no_overspending_no_note`; API: `future_answers_milestones_and_overspending`; the MCP lead of `get_future` gains "Mention `overspending` at most once, calmly, and only if the member asks about spending or the future."
- [ ] **Step 2: Run** `nice -n 15 env -u CARGO_TARGET_DIR cargo nextest run -p coffer-domain --all-features -j 3 -E 'test(future)'` and `… -p coffer-server … -E 'test(future) | test(parity)'`. Expected: FAIL.
- [ ] **Step 3: Implement**; regenerate `docs/api/openapi.json`.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07v commit -m "Answer Future's milestones and what this month's overspending costs"`

### Task 2: The tab and the chart

**Files:**
- Create: `crates/server/src/http/pages/future/chart.rs` (with unit tests)
- Modify: `crates/server/src/http/pages/layout.rs` (`Nav::Future`, `TABS` of 5), `crates/server/assets/css/base.css` (the chart's classes; the bottom bar holds five tabs at 375 px), `crates/server/assets/themes/{mochi,fjord}.css` and `crates/server/src/themes/contract.rs` only if a new token is needed for the band (prefer existing tokens), `crates/server/tests/it/pages/navigation.rs`
  ```rust
  /// History, the range band, the likely line, the target line, what-ifs and milestones.
  pub(super) fn future_chart(answer: &Future) -> Markup;
  /// The table equivalent: every 5 years, low, likely and high.
  pub(super) fn future_table(answer: &Future) -> Markup;
  ```
  x is months from the first history point to the horizon; y spans history, the three lines, what-ifs, the target and 0; `i128` throughout.
- [ ] **Step 1: Write the failing tests** — `the_nav_has_five_tabs_in_order`, `the_band_lies_between_low_and_high`, `the_target_line_is_drawn_at_its_value`, `what_ifs_are_dotted_lines`, `milestones_are_marked_at_their_month`, `the_scale_holds_everything_drawn`, `a_target_below_the_history_still_draws`, `classes_only_no_style`, `the_table_lists_every_fifth_year`.
- [ ] **Step 2: Run** `… -p coffer-server --all-features -j 3 -E 'test(future::chart) | test(navigation)'`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07v commit -m "Add the Future tab and draw its chart"`

### Task 3: The page: headline, chips, what-ifs, note

**Files:**
- Create: `crates/server/src/http/pages/future.rs` (routes, handler, layout), `crates/server/src/http/pages/future/{words.rs, inputs.rs, what_ifs.rs}` (each with unit tests), `crates/server/tests/it/pages/future.rs` (+ `mod future;` in `tests/it/pages/mod.rs`)
- Modify: `crates/server/src/http/pages/mod.rs` (routes), `AGENTS.md` (crate map), `staging/e2e/ui/pages.spec.mjs` (a `/future` row, heading "Future")

**Interfaces:** routes `GET /future`, `POST /future/settings` (one of `monthly_saving`, `target`, `growth`, `include_house`, `clear`), `POST /future/what-ifs` (`kind`, `label`, `month`, `amount`), `POST /future/what-ifs/{id}/remove`; each POST checks CSRF and redirects to `/future`. `words::{headline(answer) -> String, what_if_sentence(line) -> String, milestone_sentence(m) -> String, overspending_sentence(cost) -> String, blocker_sentence(b, can_manage) -> Markup, caution_sentence(c) -> String}`, each kind tested against `word_checks::schema_kinds`.

- [ ] **Step 1: Write the failing tests** (`pages/future.rs`, seeded as 07t's `future_answers_from_net_worth_and_saving`, clock 2027-01-15):
  - `the_page_leads_with_the_headline` — "You reach $2,000,000 around …, somewhere between … and …" with the API's years.
  - `the_page_shows_what_the_api_answers` — the table's values, milestones and what-if sentences equal `GET /api/v1/future`'s.
  - `each_chip_changes_one_setting` (saving, target, growth, house, and "Use the last 12 months" clears the saving), `growth_chip_parses_exactly`, `a_bad_amount_rerenders_with_the_error`.
  - `a_what_if_is_added_and_removed_from_the_page`, `a_what_if_sentence_says_months_later`.
  - `the_overspending_note_shows_once_and_only_when_over`.
  - `a_reader_sees_values_not_forms`, `posts_need_the_csrf_field`, `a_member_without_forecast_access_is_told_not_shown_numbers` (the page renders with a sentence, no numbers).
  - `no_display_currency_explains_and_links_for_an_admin`.
  - Words unit tests: every `FutureBlocker`, `FutureCaution`, `MilestoneKind` and `WhatIfKind` kind is worded.
- [ ] **Step 2: Run** `… -E 'test(pages::future) | test(future::words)'`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the tests, then `uptime` and `nice -n 15 scripts/gate.sh`. Expected: PASS, gate green.
- [ ] **Step 5: Commit, push, PR** — `git -C ../coffer-07v commit -m "Show where net worth is heading on the Future page"`; push `money-07v-future-page`; PR "Future page", "Closes #161", with how it looked at 375 px and 1280 px, Mochi and Fjord, light and dark (or that it wasn't looked at), that it changes `staging/e2e/ui/pages.spec.mjs`, and what was verified locally versus by CI.

## Open questions for the owner

None (who may change the chips is 07t's open question 3).
