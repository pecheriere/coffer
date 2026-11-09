# 07l — Money page and a bucket's page Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `/money` shows a month's buckets: totals planned, spent and free money, then one group per tier, each bucket with its bar, pace marker and left or over, then goals and the emergency fund, with a month switcher. `/money/buckets/{id}` shows one bucket: this month, its last 6 months, and pause or resume. The `Money` tab leads to `/money`. Closes #151.

**Architecture:** Pages only, on 07k's `Ledger::buckets` (the call the API makes), so the page and `GET /api/v1/buckets` can't disagree. A bucket's 6 months are six `Ledger::buckets` calls for the months before this one, read one after another; each is a forecast read of one household, so no new endpoint is needed (an agent asks the same six months of the API). Bars are server-drawn SVG in integers, like `net_worth/chart.rs`: classes only, colours from 07f's tier tokens, and each with a text equivalent. Pause and resume post to the existing `/plan/{id}/active` handler logic through `Ledger::update_planned_item`, from a form on the bucket's page.

**Tech Stack:** Rust 2024, axum, maud.

**Spec:** §5 (Money, CSP and theme constraints), §4.2, §10.10. Overview: unit 07l, planner's call 2 (the `Money` tab moves off `/plan` here).

**Depends on:** 07f (#145, tier tokens and classes), 07k (#150) merged. **Branch / worktree:** `money-07l-money-page`, `../coffer-07l`. No migration.

## Global Constraints

- Everything in the overview's "Rules for every unit" applies; AGENTS.md's CSP, theme and wording rules: no inline script or style, no `style=`, colours only from tokens, read-only viewers see no forms.
- `/money?month=YYYY-MM` (default this month). The switcher is two links, earlier and later month; "later" is absent on this month. A month the API refuses shows the page's error state in words, not a 500.
- Order on the page (§5): totals; Must pay, Essentials, Flexible groups (a group with no bucket is left out); goals ("Saving toward"); the emergency fund (this month only). Each group's header gives its planned and spent totals.
- A bucket row: name (link to its page), tier mark, planned, spent, "left $N" or "$N over", the bar, and for a budget with a pace "ahead of pace" or nothing. A long-period budget says its period ("Jul to Sep"). Words in `money/words.rs`; amounts through `amounts::magnitude`.
- The bar: `viewBox` 0 0 1000 12 (tenths of a percent), spent fill `min(spent, planned) × 1000 / planned`, an over segment in `.bar-over` when spent > planned (the planned width scaled down so both fit: `planned × 1000 / spent`), the pace marker a 2-wide line at `elapsed_permille`; integer division in `i128`; `role="img"` with an `aria-label` that says the same as the row's text.
- A bucket's page: this month's row (bar, numbers, its version's `effective_from` as "Since Nov 2026" when not the first), the last 6 months as a table (month, planned, spent, left or over) with a small bar per row, and a pause or resume form for writers ("Pause" sets `active` false: "Paused buckets stay listed here and count nowhere"). A paused item shows no bucket on `/money` (07k lists active items) but its page still answers.
- `/plan` stays as it is (07m moves its forms). `/money` links "Change the plan" to `/plan`.

## Review Focus

1. **Integer bar maths at the edges.** planned 0 is impossible (validation), spent 0, spent = planned, spent 10^15: widths in 0..=1000, no overflow. → Task 1 `bar_widths_hold_at_the_edges` (proptest).
2. **The page agrees with the API.** The same household: every bucket's numbers on `/money` equal `GET /api/v1/buckets`. → Task 2 `money_page_shows_the_apis_numbers`.
3. **Read-only viewers.** A member without plan write sees no pause form, and its POST is refused. → Task 3 `a_reader_sees_no_pause_form`.
4. **The tab.** Every page's `Money` tab links `/money` and is current on `/money` and a bucket's page. → Task 2 `every_page_shows_the_four_tabs` (updated).

---

### Task 1: The bucket bar

**Files:**
- Create: `crates/server/src/http/pages/money/bar.rs` (with unit tests), `crates/server/src/http/pages/money/words.rs` (with unit tests)
- Modify: `crates/server/assets/css/base.css` (`.bucket-bar`, `.bucket-bar .spent` filled through the row's `.tier-*` class, `.bar-over`, `.pace-mark`)

**Interfaces:**
```rust
pub(super) struct BarWidths { pub(super) spent: u16, pub(super) over: u16, pub(super) pace: Option<u16> }
pub(super) fn widths(planned: i64, spent: i64, pace_permille: Option<u16>) -> BarWidths;
pub(super) fn bar(bucket: &Bucket, tier_class: &'static str, label: &str) -> Markup;
// words.rs
pub(super) fn left_or_over(bucket: &Bucket) -> String;   // "left $120" / "$64 over"
pub(super) fn pace_words(pace: Option<Pace>) -> Option<&'static str>;  // "ahead of pace"
pub(super) fn period_words(period: &BucketPeriod) -> String;            // "Jul to Sep"
pub(super) fn month_title(month: CalendarMonth, current: bool) -> String; // "This month" / "October 2026"
```
- [ ] **Step 1: Write the failing tests:** `bar_widths_hold_at_the_edges` (proptest over planned 1..=10^15, spent 0..=10^15: `spent + over <= 1000`), `an_overspent_bar_shows_both`, `the_pace_mark_is_where_the_month_is`, `the_label_says_what_the_bar_shows`, and words: `left_or_over_reads_as_a_phrase`, `period_words_name_both_ends`.
- [ ] **Step 2: Run** `nice -n 15 env -u CARGO_TARGET_DIR cargo nextest run -p coffer-server --all-features -j 3 -E 'test(money::)'`. Expected: FAIL. **Step 3: Implement.** **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07l commit -m "Draw a bucket's bar on the server"`.

### Task 2: The Money page and the tab

**Files:**
- Create: `crates/server/src/http/pages/money.rs` (routes, `/money`), `crates/server/tests/it/pages/money.rs` (+ `mod money;` in `tests/it/pages/mod.rs`)
- Modify: `crates/server/src/http/pages/mod.rs` (`.merge(money::routes())`), `crates/server/src/http/pages/layout.rs` (`TABS`: `(Nav::Money, "/money", "Money")`; `/plan`'s page passes `Nav::Money` still), `crates/server/tests/it/pages/navigation.rs` (the tab's href), `crates/server/src/http/pages/word_checks.rs` (the buckets' kinds and words), `AGENTS.md` (crate map: `pages/money*`)

- [ ] **Step 1: Write the failing tests** (real router, clock 2026-11-10, seeded like 07k's `this_months_buckets`): `money_page_shows_the_apis_numbers`, `groups_follow_the_tiers_and_skip_empty_ones`, `the_switcher_goes_back_and_not_past_this_month`, `a_past_month_has_no_emergency_fund_or_pace`, `a_refused_month_reads_as_words` (`?month=2030-01` → 200 page with the refusal sentence, no 500), `money_page_has_no_inline_style` (the rendering checks of `tests/it/pages/rendering.rs` run on it).
- [ ] **Step 2: Run** `… -p coffer-server … -E 'test(pages::money) | test(navigation)'`. Expected: FAIL. **Step 3: Implement.** **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07l commit -m "Show a month's buckets on the Money page"`.

### Task 3: A bucket's page

**Files:**
- Create: `crates/server/src/http/pages/money/bucket.rs` (`/money/buckets/{id}` and its pause form's POST, `/money/buckets/{id}/active`), `crates/server/tests/it/pages/money_bucket.rs` (+ `mod` line)
- Modify: `crates/server/src/http/pages/money.rs` (routes), `crates/server/src/http/pages/plan/edit.rs` (move `set_active`'s body into a `pub(in crate::http::pages)` helper both pages call, so pausing has one implementation)

- [ ] **Step 1: Write the failing tests:** `a_buckets_page_shows_this_month_and_six_before` (rows for May to October 2026 and this month), `a_version_change_reads_since` ("Since Nov 2026" after a 07j change from this month), `pause_and_resume_post_redirect_get` (303 back to the page, the item inactive, an audit row), `a_reader_sees_no_pause_form` (and its POST → 403 page), `a_hidden_or_unknown_item_is_404` (an item hinted at an account the token can't see answers as missing, as `plan_item_visible` decides).
- [ ] **Step 2: Run** `… -E 'test(money_bucket)'`. Expected: FAIL. **Step 3: Implement.** **Step 4: Run** the filtered tests, then `uptime` and `nice -n 15 scripts/gate.sh`. Expected: PASS.
- [ ] **Step 5: Commit, push, PR** — `git -C ../coffer-07l commit -m "Show one bucket's month and history, and pause it"`; push `money-07l-money-page`; PR "Money page and a bucket's page", "Closes #151". The staging e2e navigation spec may follow the tab to `/money`: CI checks it, and the PR says so.
