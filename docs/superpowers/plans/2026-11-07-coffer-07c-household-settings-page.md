# 07c — Household page: emergency fund and forecast settings Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An admin sets each currency's emergency fund and the forecast settings on `/admin/household`, so nothing about the forecast needs the API any more. Closes #142.

**Architecture:** Two new cards on the household page, each in its own file, each posting to a route that calls the ledger call the API uses (`set_emergency_fund`, `remove_emergency_fund`, `update_forecast_settings`) and redirects back (POST-redirect-GET). The forecast page's blocker sentence links to the card.

**Tech Stack:** Rust 2024, axum, maud. No new crates.

**Spec:** `docs/superpowers/specs/2026-11-07-coffer-money-redesign-design.md` §4.4 ("editable on a page: fixes the current 'set it through the API' blocker"), §5 (More → Household). Overview: `docs/superpowers/plans/2026-11-07-coffer-07-money-redesign-overview.md`.

**Depends on:** 07a (#140, `Nav::More`) and 07b (#141, the ledger calls) merged. **Branch / worktree:** `money-07c-household-page`, `../coffer-07c`. No migration, no API change.

## Global Constraints

- Everything in the overview's "Rules for every unit" applies.
- No inline script or style, no `style=`; forms work as plain POST with the session's `csrf` field.
- Only an admin reaches `/admin/household` (it already authorizes `ManageHousehold`); nothing here widens that.
- Amounts are typed as decimals in the currency's units and parsed with `coffer_domain::money::DecimalAmount::parse(..).to_minor_units(currency)`, shown with `format_minor_units`, as `pages/plan/form.rs:205` does. A refusal never echoes the input.
- `/admin/household` is in `PAGES` (`http/browser_reports/fields.rs`) and already shows amounts (manual rates). No script reads the new fields; Task 1's Step 4 checks `assets/js/coffer.js` puts no form value in a message.
- Words: "Emergency fund", "months of planned spending" (not "outflows"), "planned income".

## Review Focus

1. **A typed amount with too many decimals or a sign** ("100.005", "-5", "1e3") for a currency with 2 decimals, and one with 0 (JPY). → Task 1 `amounts_follow_the_currency`.
2. **Switching a currency's target between fixed and months**: the form sends both fields; only the chosen kind's field counts, the other is ignored even if filled. → Task 1 `only_the_chosen_kind_counts`.
3. **A stale form removing a target already removed** by another admin: a friendly "already gone" page state, not a 500. → Task 1 `removing_twice_says_it_is_gone`.
4. **Unchecked checkbox**: "Count planned income" unticked must send `false`, not leave it as is. → Task 2 `unticking_income_turns_it_off`.
5. **The forecast page sending a non-admin to a page they can't open.** → Task 2 `blocker_links_only_for_admins`.

---

### Task 1: The emergency fund card

**Files:**
- Create: `crates/server/src/http/pages/household/emergency_fund.rs`, `crates/server/tests/it/pages/household.rs` (+ `mod household;` in `tests/it/pages/mod.rs`)
- Modify: `crates/server/src/http/pages/household.rs` (module, routes, card placed after the display currency card; `current: Some(Nav::More)` if 07a didn't already)

**Interfaces:**
- Consumes: `Ledger::household_settings` (its `emergency_funds`), `Ledger::set_emergency_fund(actor, currency, target)`, `Ledger::remove_emergency_fund(actor, currency)` (07b).
- Produces: routes `POST /admin/household/emergency-fund` (form `csrf`, `currency`, `kind` = `fixed` | `months`, `amount`, `months`) and `POST /admin/household/emergency-fund/{currency}/remove` (form `csrf`). Both redirect to `/admin/household#emergency-fund`.
- Card (`section.card id="emergency-fund"`): heading "Emergency fund"; one sentence: "Money kept aside for surprises, per currency. Free money counts what's left after it."; a list of the targets set, each "USD: 10,000.00" or "USD: 3 months of planned spending" with a Remove button (its own small form); then one form "Set an emergency fund": a currency `select` (every `Currency::all()`, the display currency preselected), a `fieldset.choices` with two radios (fixed amount, months), an amount input (`inputmode="decimal"`), a months input (`type="number" min="1" max="60"`), and "Save emergency fund". Setting a currency that has one replaces it.

- [ ] **Step 1: Write the failing tests** (`tests/it/pages/household.rs`, using `testkit::browser::with_admin`):
  - `admin_sets_a_fixed_target` — post `currency=USD, kind=fixed, amount=10000, months=` → 303 to `/admin/household#emergency-fund`; `GET /api/v1/household/settings` through the ledger shows `Fixed { minor_units: 1000000 }`; the page lists "USD: 10,000.00".
  - `admin_sets_a_months_target` — `kind=months, months=3` → `MonthsOfOutflows { months: 3 }`, listed as "3 months of planned spending".
  - `only_the_chosen_kind_counts` — `kind=months, months=3, amount=500` → months target, no error.
  - `amounts_follow_the_currency` — USD "100.005" and "-5" and "1e3" refused with the page's refusal and nothing saved; JPY "1000" saved as 1000 minor units; JPY "1000.5" refused.
  - `removing_twice_says_it_is_gone` — remove USD → 303; remove again → the page answers 404 with the fixed message "That emergency fund was already removed." (map `LedgerError::NotFound`).
  - `a_member_cannot_post` — a member's post is refused as the other household forms are (403), nothing saved.
  - `csrf_is_required` — a post without `csrf` is refused.
- [ ] **Step 2: Run** `… -p coffer-server … -E 'test(pages::household)'`. Expected: FAIL (404 routes).
- [ ] **Step 3: Implement** `emergency_fund.rs` (`routes()`, `card(csrf, funds)`, two handlers with `PageForm`, `session.verify(&state, &form.csrf)`), merged from `household::routes()`.
- [ ] **Step 4: Run** the tests; read `crates/server/assets/js/coffer.js` and confirm no handler puts a form value into an error message. Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07c commit -m "Set each currency's emergency fund on the Household page"`

### Task 2: The forecast settings card and the forecast page's link

**Files:**
- Create: `crates/server/src/http/pages/household/forecast_settings.rs`
- Modify: `crates/server/src/http/pages/household.rs`, `crates/server/src/http/pages/forecast/words.rs` (+ `words/tests.rs`), `crates/server/src/http/pages/forecast.rs` (pass whether the viewer may manage the household)
- Test: `crates/server/tests/it/pages/household.rs`, `crates/server/tests/it/pages/forecast.rs`

**Interfaces:**
- Consumes: `Ledger::update_forecast_settings(actor, ForecastSettingsPatch { include_income, default_horizon_months, stale_after_days, .. })`.
- Produces: `POST /admin/household/forecast` (form `csrf`, `include_income` = `on` or absent, `horizon` 1–60, `stale_after_days` 1–365) → 303 to `/admin/household#forecast`. All three fields are always sent, so an unticked box is `Some(false)`.
- Card (`section.card id="forecast"`): heading "Cash forecast"; a checkbox "Count planned income in the forecast's totals", "How many months the forecast looks ahead" (number 1–60), "Flag a balance older than this many days" (number 1–365), "Save forecast settings".
- Forecast words: `blocker_sentence(&Blocker::EmergencyFundNotSet { currency }, can_manage: bool)`. For an admin the page renders the sentence followed by a link "Set one on the Household page" → `/admin/household#emergency-fund`; for anyone else: "An admin can set one on the Household page." No more "It is set through the API."

- [ ] **Step 1: Write the failing tests**
  - `admin_saves_forecast_settings` — post `include_income=on, horizon=6, stale_after_days=10` → settings read back `true, 6, 10`, one audit entry.
  - `unticking_income_turns_it_off` — from `true`, post without `include_income` → `false`.
  - `out_of_range_numbers_are_refused` — horizon 0 and 61, stale 0 and 366 → refusal, nothing saved.
  - `pages/forecast.rs`: `blocker_links_only_for_admins` — no target set: an admin's `/forecast` contains `href="/admin/household#emergency-fund"`; a member's has "An admin can set one" and no such link; neither contains "through the API".
- [ ] **Step 2: Run** `… -E 'test(pages::household) | test(pages::forecast)'`. Expected: FAIL.
- [ ] **Step 3: Implement** the card, the handler, the words and the page change.
- [ ] **Step 4: Run** the tests, then `uptime` and `nice -n 15 scripts/gate.sh`. Expected: PASS, gate green.
- [ ] **Step 5: Commit, push, PR** — `git -C ../coffer-07c commit -m "Set the forecast settings on the Household page and link there from the forecast"`; push `money-07c-household-page`; PR "Household page: emergency fund and forecast settings", "Closes #142"; look at the page at 375 px in both themes if a local `serve` is practical, otherwise say it wasn't looked at.
