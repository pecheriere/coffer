# 07p — Home's Needs a look and the cover sheet Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Home gets its "Needs a look" section after the free money answer; a cover sheet page (works without JS) lists the sources allowed for one overspend with their effect, or offers to change the plan from next month; a bucket's page lists this month's covers with undo. The threshold forms on Household are 07r's (auto-cover and invest minimum together). Closes #155.

**Architecture:** Pages on the ledger calls the API makes: `Ledger::needs_a_look` (07o), `create_cover`/`list_covers`/`delete_cover` (07n). The one new number, the sources for an overspend that isn't in "needs a source" (one free money covered, which the owner may re-source, §4.6.2), gets its own small read so the page and an agent see the same options: `GET /api/v1/covers/options?to={item}` and MCP `get_cover_options`, from 07o's domain function made public. Every action is a plain form with POST-redirect-GET.

**Tech Stack:** Rust 2024, axum, maud, utoipa, rmcp.

**Spec:** §4.6 (1 to 6), §5 (Home order, the cover sheet "a page or `<dialog>` that works without JS"), §10.6. Overview: unit 07p.

**Depends on:** 07l (#151), 07m (#152), 07o (#154) merged. **Branch / worktree:** `money-07p-needs-a-look-pages`, `../coffer-07p`. No migration.

## Global Constraints

- Everything in the overview's "Rules for every unit" applies; CSP, theme, wording rules; read-only viewers see the list but no cover or undo forms, and their POSTs are refused.
- Home's order (§5): free money (07i), Needs a look, net worth. The existing "To check" card (07a) folds into Needs a look as its `transactions_to_check` entry, linking the review queue (planner's call: one place for what's waiting). No entries: the section says "Nothing needs a look" in one line, never a warning.
- Sentences (`home/needs_words.rs`), one per entry, the answer first: "Restaurants went $640 over. Free money covers $180; the rest needs a source. **Choose**", "Restaurants went $64 over · covered from free money · **Change**", "Groceries runs out around Nov 24 at this pace", "Insurance was paid $108, $8 more than planned", "3 transactions to check", and auto-covered ones grouped last ("Covered from free money: Coffee $4, Books $12").
- The cover sheet, `GET /money/cover/{item}`: the overspend in one sentence, then one form per source: a bucket source is a button "Take $460 from Hobbies (leaves $40)"; a goal source shows its effect ("moves the Japan trip back about 4 weeks", from `goal_delay_days`: "about N days" under 14, else weeks rounded) and needs a ticked checkbox; the emergency fund's says "Your emergency fund will be $460 under its target until free money refills it" and needs a ticked checkbox. Then "Or change the plan: raise Restaurants to $350 a month from next month", a link to 07m's edit form with `?from=next_month&amount=` prefilled (planned + over, rounded up to a whole major unit). `POST /money/cover/{item}` creates the cover and redirects to Home with a one-line notice; a refusal re-renders the sheet with the ledger's message.
- Bucket page (07l): a "Covers this month" list (from, to, amount, who, when) with an "Undo" form each for writers, `POST /money/covers/{id}/undo` → 303 back. Past months' covers are listed without undo.
- Home's final order (§5, matching 07s): free money, invest (07s), `#needs-a-look`, net worth, cash chart (07s), forecast link. 07p lands first, so it places `#needs-a-look` right after free money and 07s inserts `#invest` before it.

## Review Focus

1. **Works without JS.** Every action is a form with a CSRF field and a 303; the sheet is a page, not a script-driven dialog. → Task 2 `cover_sheet_posts_and_redirects` (the test client runs no script).
2. **Confirmation can't be skipped.** Posting a goal or emergency fund source without the checkbox is refused with its sentence and nothing stored. → Task 2 `a_goal_source_without_the_tick_is_refused`.
3. **The page offers only what the API allows.** The sheet's sources equal `GET /api/v1/covers/options?to=` for the same item; a must pay bucket is never offered. → Task 1 `cover_options_never_offer_must_pay`, Task 2 `the_sheet_lists_the_apis_options`.
4. **Words.** Every entry kind has a sentence; the word checks cover `LookEntry`'s kinds from the schema. → Task 3 `every_needs_a_look_kind_has_words`.

---

### Task 1: Cover options for any overspend (API, MCP)

**Files:**
- Modify: `crates/domain/src/forecast/needs_a_look.rs` (`pub fn cover_options(buckets: &Buckets, to: PlannedItemId, rest: Money) -> Vec<CoverOption>`, the function `NeedsASource` already uses, made public), `crates/server/src/ledger/covers.rs` (`cover_options(actor, to) -> CoverOptions { to, over, uncovered, options }`), `crates/server/src/http/api/covers.rs` (`GET /api/v1/covers/options`, `getCoverOptions`), `crates/server/src/http/api/{mod.rs, openapi.rs}`, `crates/server/src/http/api/mcp/tools.rs` (`get_cover_options`), `crates/server/tests/it/covers.rs`, `crates/server/tests/it/mcp/parity/covers.rs`, `docs/api/openapi.json`, `docs/agent-usage.md`

- [ ] **Step 1: Write the failing tests:** `cover_options_for_an_overspend_free_money_covered`, `cover_options_never_offer_must_pay`, `no_options_for_a_bucket_not_over` (`options` empty, `uncovered` 0), `a_hidden_item_is_404`, parity row.
- [ ] **Step 2: Run** `nice -n 15 env -u CARGO_TARGET_DIR cargo nextest run -p coffer-server --all-features -j 3 -E 'test(covers) | test(parity)'`. Expected: FAIL. **Step 3: Implement**; regenerate the OpenAPI snapshot. **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07p commit -m "Serve the sources that may cover an overspend"`.

### Task 2: The cover sheet and undo

**Files:**
- Create: `crates/server/src/http/pages/money/cover.rs` (`GET|POST /money/cover/{item}`, `POST /money/covers/{id}/undo`), `crates/server/src/http/pages/money/cover_words.rs` (with unit tests), `crates/server/tests/it/pages/cover_sheet.rs` (+ `mod` line)
- Modify: `crates/server/src/http/pages/money.rs` (routes), `crates/server/src/http/pages/money/bucket.rs` ("Covers this month"), `crates/server/src/http/pages/money/change/*` (the edit form reads `from` and `amount` query values as defaults), `crates/server/assets/css/base.css` (`.cover-option` only if the existing card and form classes don't do)

- [ ] **Step 1: Write the failing tests:** `the_sheet_lists_the_apis_options`, `cover_sheet_posts_and_redirects` (bucket source → 303 to `/`, the cover stored, free money up), `a_goal_source_without_the_tick_is_refused`, `a_goal_source_says_its_delay`, `change_the_plan_links_next_month_prefilled`, `undo_from_the_buckets_page`, `a_reader_sees_no_forms` (and POSTs → 403); words: `delay_words_switch_to_weeks_at_fourteen_days`.
- [ ] **Step 2: Run** `… -E 'test(cover_sheet) | test(cover_words) | test(money_bucket)'`. Expected: FAIL. **Step 3: Implement.** **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07p commit -m "Cover an overspend from a page, and undo it"`.

### Task 3: Home's Needs a look

**Files:**
- Create: `crates/server/src/http/pages/home/needs_a_look.rs`, `crates/server/src/http/pages/home/needs_words.rs` (with unit tests)
- Modify: `crates/server/src/http/pages/home.rs` (the section after the free money card; `to_check_card` goes), `crates/server/src/http/pages/word_checks.rs`, `crates/server/tests/it/pages/home.rs` (the "To check" assertions move to the section's entry)

- [ ] **Step 1: Write the failing tests:** `home_lists_what_needs_a_look_after_free_money` (order of sections; each seeded entry's sentence and link), `nothing_needs_a_look_says_so_in_one_line`, `transactions_to_check_link_the_queue`, `every_needs_a_look_kind_has_words`.
- [ ] **Step 2: Run** `… -E 'test(pages::home) | test(needs_words)'`. Expected: FAIL. **Step 3: Implement.** **Step 4: Run** the filtered tests, then `uptime` and `nice -n 15 scripts/gate.sh`. Expected: PASS.
- [ ] **Step 5: Commit, push, PR** — `git -C ../coffer-07p commit -m "Show what needs a look on Home"`; push; PR "Home's Needs a look and the cover sheet", "Closes #155", noting the "To check" card folded into the section (07s inserts its invest card before the section and the cash chart after net worth).

## Owner's decisions (2026-11-08)

Each question below was settled as recommended (owner's decision, 2026-11-08), except where a line says otherwise.

1. **The "To check" card.** Recommended: fold it into Needs a look as one entry, so Home has one list of what's waiting. The alternative keeps both, which repeats the count.
