# 07s — Home's cash chart and invest card; forecast page answers first Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Home gains the keep-or-invest card with "Not now" (right after free money) and the 3-month cash chart with the emergency fund line and the low point (after net worth), and ends with a link to the full cash forecast; the forecast page drops its "Surplus" headline and leads with the projection and the suggestion explained (§10.9), so the product has one headline number. Closes #158.

**Architecture:** Pages only, calling the ledger calls the API calls: `Ledger::cash_projection` (07q) and `Ledger::keep_or_invest` / `Ledger::put_off_invest_suggestion` (07r). The chart is server-drawn SVG in integers, in a shared `pages/cash_chart.rs` that Home (small) and the forecast page (wide) both use; its scale maths (`px`, `y_tenths`, `ticks`) move out of `net_worth/chart.rs` into a shared `pages/charts.rs` that 07v's Future chart reuses too. The suggestion's sentences live in one `pages/invest_words.rs`, used by Home's card and the forecast page.

**Tech Stack:** Rust 2024, axum, maud, SVG, CSS tokens.

**Spec:** `docs/superpowers/specs/2026-11-07-coffer-money-redesign-design.md` §2 (anti-goals: one tap to explain, answer first, no jargon), §4.5 (the card's words, "Not now", no "I moved it"), §5 (Home's order; More: "Cash forecast detail, reworded, answer first"; CSP and SVG rules), §10.5, §10.6, §10.9. Overview: planner's call 11.

**Depends on:** 07i (#148), 07r (#157) merged (and so 07q, 07o). **Branch / worktree:** `money-07s-home-cash-chart-invest`, `../coffer-07s`. No migration, no API change.

**Assumed of 07p (Home's Needs a look):** 07p adds `section.card#needs-a-look` after the free money card (07i's `section.card.home-free`) and may fold 07a's `#to-check` card into it. Home's order from §5 is then: free money, `#invest` (this unit, only when shown), `#needs-a-look`, `#net-worth`, `#cash-chart` (this unit), the forecast link. Whichever of 07p and 07s merges second rebases `home.rs`'s body onto that order; this unit anchors its two sections to `.home-free` and `#net-worth`, not to 07p's section, so it works with or without 07p.

## Global Constraints

- Everything in the overview's "Rules for every unit" applies.
- CSP: no inline style or `style=`; SVG coordinates computed in integers (tenths of a unit, as `net_worth/chart.rs`), colours through classes on tokens only: the line `--accent`, the emergency fund line `--tier-emergency` (07f), the low point `--negative` when it dips into the fund, else `--accent`.
- Each chart has a text equivalent (`aria-hidden="true"` on the SVG): the sentence and a `dl` with Today, Low point (date, amount, cause), In 3 months.
- Card words (§4.5, planner's wording within §3): "You could invest {could_invest}. That still leaves {cushion} free, and the next 3 months never dip into the emergency fund, even at the low point ({Dec 14}, {cause})." With no cause: "…even at the low point ({Dec 14})." A low point today: "…at today's low point." No word "cushion", "accrual", "suggestion" on the page; never a name of something to buy.
- The card shows only for the display currency's `suggest` state (others listed one line each: "EUR: you could invest €800."); `not_now`, `below_minimum`, `no_number` show nothing on Home; `no_emergency_fund` shows nothing on Home (the free money card already carries that caution) and is explained on the forecast page.
- "Not now": a plain form, `POST /invest/{currency}/not-now` with the CSRF field, POST-redirect-GET to `/`, calling `put_off_invest_suggestion`; members who can't write see no button. A 422 (nothing to put off any more) redirects to `/` without an error: the card is simply gone.
- A part the viewer may not read (forecast access) is left out; Home still renders (07a's rule).

## Review Focus

1. **The fund line off the chart.** A target above every projected value, or a projection entirely below 0, still draws within the box (the scale spans the line, the target and 0). → Task 1 `the_scale_holds_the_target_and_zero`.
2. **Answer before warnings** on the forecast page: the projection and suggestion card is the first `section.card`; "Surplus" appears nowhere as a heading. → Task 3 `the_forecast_page_leads_with_the_projection`, `no_surplus_headline`.
3. **"Not now" for the wrong household state** (already gone) → redirect, no error page. → Task 2 `not_now_when_nothing_is_suggested_just_returns_home`.
4. **Net worth's chart unchanged** by the extraction. → Task 1: the existing `net_worth::chart` tests pass untouched.
5. **A member without forecast access** sees Home without the card and the chart. → Task 2 `home_leaves_out_the_card_and_chart_when_they_cannot_be_read`.

---

### Task 1: Shared chart maths and the cash chart

**Files:**
- Create: `crates/server/src/http/pages/charts.rs` (`px`, `y_tenths`, `ticks` moved from `net_worth/chart.rs`, `pub(in crate::http::pages)`, with their unit tests), `crates/server/src/http/pages/cash_chart.rs` (with unit tests)
- Modify: `crates/server/src/http/pages/net_worth/chart.rs` (use `charts::*`), `crates/server/src/http/pages/mod.rs`, `crates/server/assets/css/base.css` (`.cash-chart`, `.cash-line`, `.fund-line`, `.low-point`, `.low-point.dips`)

**Interfaces:**
  ```rust
  pub(in crate::http::pages) enum CashChartSize { Small, Wide }   // 320×120 and 640×200
  /// The projection's line as a step path, the emergency fund line, and the low point.
  pub(in crate::http::pages) fn cash_chart(row: &CurrencyProjection, size: CashChartSize) -> Markup;
  /// The text equivalent: the sentence and the Today / Low point / In 3 months list.
  pub(in crate::http::pages) fn cash_chart_text(row: &CurrencyProjection) -> Markup;
  ```
  The y scale spans min and max of the line, the target and 0; x is days since `as_of`. All arithmetic in `i128`.

- [ ] **Step 1: Write the failing tests** — `the_line_steps_once_per_changed_day` (a path with one vertical segment per day whose value changed), `the_scale_holds_the_target_and_zero`, `the_low_point_is_marked_where_it_is` (circle's x at its day), `a_dip_marks_the_low_point_negative` (`low-point dips` class), `classes_only_no_style`, `huge_values_do_not_overflow`, `the_text_names_the_low_point_and_its_cause`.
- [ ] **Step 2: Run** `nice -n 15 env -u CARGO_TARGET_DIR cargo nextest run -p coffer-server --all-features -j 3 -E 'test(charts) | test(cash_chart) | test(net_worth::chart)'`. Expected: FAIL (new tests).
- [ ] **Step 3: Implement**: move the helpers first (net worth tests green), then `cash_chart.rs`.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07s commit -m "Draw the 3-month cash chart, sharing net worth's scale maths"`

### Task 2: Home's invest card, chart and closing link

**Files:**
- Create: `crates/server/src/http/pages/invest_words.rs` (with unit tests), `crates/server/src/http/pages/home/invest.rs`
- Modify: `crates/server/src/http/pages/home.rs` (the two sections in §5's order, the closing link `p.home-more a href="/forecast" "See the full cash forecast"`, the `/invest/{currency}/not-now` route), `crates/server/assets/css/base.css` (`.home-invest`)
- Test: `crates/server/tests/it/pages/home.rs` (if it passes about 600 lines, move the new tests to `tests/it/pages/home/invest.rs`)

**Interfaces:** `invest_words::{card_sentence(s: &InvestSuggestion) -> Option<String>, explain_sentence(s: &InvestSuggestion) -> String}` (the second is the forecast page's: one sentence per state, e.g. `no_emergency_fund` → "Set an emergency fund first: until then coffer won't suggest investing anything."; `below_minimum` → "{could_invest} could be invested, under your minimum of {minimum}, so Home doesn't mention it."; `not_now` → "You put this off at {free_money} free; it comes back once free money moves by more than {minimum}."). `home::invest::card(answer: &KeepOrInvest, display: Option<Currency>, csrf: Option<&str>) -> Option<Markup>`.

- [ ] **Step 1: Write the failing tests** (`pages/home.rs`, seeding as 07r's `the_suggestion_answers_the_rule`):
  - `home_offers_to_invest_after_free_money` — `section#invest` comes right after `.home-free` (string index) and reads the §4.5 sentence with the household's numbers.
  - `not_now_hides_the_card` — POST with CSRF → 303 to `/`; the next `/` has no `#invest`.
  - `not_now_when_nothing_is_suggested_just_returns_home` (Review Focus 3).
  - `a_reader_sees_no_not_now_button`, `not_now_needs_the_csrf_field` (403).
  - `no_card_without_an_emergency_fund`, `no_card_below_the_minimum`.
  - `home_draws_the_cash_chart_after_net_worth` — `#cash-chart` after `#net-worth`, with the text equivalent's low point date and cause.
  - `home_ends_with_the_forecast_link`.
  - `home_leaves_out_the_card_and_chart_when_they_cannot_be_read` (Review Focus 5).
  - Words unit tests for every `SuggestionState` kind against `word_checks::schema_kinds::<SuggestionState>()`, so a new state fails the build until worded.
- [ ] **Step 2: Run** `… -E 'test(pages::home) | test(invest_words)'`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07s commit -m "Offer to invest on Home, and chart the next 3 months"`

### Task 3: The forecast page answers first

**Files:**
- Modify: `crates/server/src/http/pages/forecast.rs` (h1 "Cash forecast"; `headline()` removed; a first `section.card#projection` with the wide chart per currency, its text, and `explain_sentence`; the existing answer card follows with status, blockers and cautions; the per-currency sums keep their rows with "Surplus"/"Shortfall" renamed "Left at the end"/"Short at the end"; the unit test `the_surplus_suggests_investing_only_once_a_target_is_set` replaced by `the_page_explains_the_suggestion_only_with_a_fund`), `crates/server/tests/it/pages/forecast/mod.rs` (its tests reading the "Surplus" row and headline — lines ~169–320 — change deliberately to "Left at the end" and the new card; name each in the PR), `staging/e2e/ui/pages.spec.mjs` (the `/forecast` row: heading "Cash forecast", label no longer "Surplus"), `docs/agent-forecast.md` (the page's description). More already labels the page "Cash forecast".
- If `forecast.rs` passes about 600 lines, move the new card to `forecast/projection.rs`.

- [ ] **Step 1: Write the failing tests** — `the_forecast_page_leads_with_the_projection` (first `section.card` is `#projection`), `no_surplus_headline` (no `h2` "Surplus" or "Shortfall"), `the_page_explains_the_suggestion_only_with_a_fund`, `a_blocked_currency_says_why_instead_of_a_chart`.
- [ ] **Step 2: Run** `… -E 'test(pages::forecast) | test(forecast::tests)'`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the tests, then `uptime` and `nice -n 15 scripts/gate.sh`. Expected: PASS, gate green.
- [ ] **Step 5: Commit, push, PR** — `git -C ../coffer-07s commit -m "Lead the forecast page with the projection and the suggestion"`; update `AGENTS.md`'s crate map (`pages/charts.rs`, `pages/cash_chart.rs`, `pages/invest_words.rs`, `home/invest.rs`); push `money-07s-home-cash-chart-invest`; PR "Home's cash chart and invest card; forecast page answers first", "Closes #158", with how it looked at 375 px and 1280 px, Mochi and Fjord, light and dark (or that it wasn't looked at), and that it changes `staging/e2e/ui/pages.spec.mjs` (CI runs it).

## Owner's decisions (2026-11-08)

Each question below was settled as recommended (owner's decision, 2026-11-08), except where a line says otherwise.

None.
