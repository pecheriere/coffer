# 07i — Home: free money and its breakdown Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Home leads with free money in the display currency, its breakdown in the order of §4.3 under it (a server-drawn stacked bar with a legend that is also its text equivalent), other currencies below, and the forecast's cautions after the answer in plain words. Closes #148.

**Architecture:** Page only. Home calls `Ledger::free_money` (07h), the call `GET /api/v1/free-money` makes. A new `home/free_money.rs` renders the hero card; `home/bar.rs` draws the stacked bar as SVG in integers with tier colour classes (07f); the cautions reuse the forecast page's sentences, made reachable from Home rather than copied.

**Tech Stack:** Rust 2024, axum, maud, SVG, CSS tokens.

**Spec:** `docs/superpowers/specs/2026-11-07-coffer-money-redesign-design.md` §1, §2 (anti-goals: one tap to explain, answer before warnings, no jargon), §4.3 (breakdown in order, display currency first, cautions below in plain words), §4.4 (held, shortfall), §5 (Home), §10.5.

**Depends on:** 07a (#140, Home), 07f (#145, tier tokens), 07h (#147). **Branch / worktree:** `money-07i-home-free-money`, `../coffer-07i`. No migration, no API change.

## Global Constraints

- Everything in the overview's "Rules for every unit" applies.
- CSP: no inline style, no `style=`; the bar's segment widths are SVG attributes computed in integers (as `net_worth/chart.rs`), colours through classes only (`.tier-fill` with `.tier-must-pay` etc., `--positive` for free money).
- Every number on the card has its breakdown one step away: the legend lists each part with its amount, and "How this is worked out" links to `/forecast`.
- Order of the breakdown (§4.3): Cash available; Must pay (this month, then "coming up by {date}" when non-zero); Essentials; Flexible; Set aside for goals; Emergency fund (held, of target); Free money.
- Words: "Free money", "Still needed this month", "Set aside for goals", "Emergency fund". No "provisional", "accrual", "settled". Cautions come after the answer, under a quiet heading "Worth knowing", never above the number.
- Currency order: the display currency first; then the others by code. No display currency: the first by code leads, and a line says "Choose a display currency on the Household page to pick which one leads." only for a viewer who can manage the household.
- A part the viewer may not read (forecast access denied) is left out: Home still renders (07a's rule).

## Review Focus

1. **Shortfall.** When `before < 0`, the hero reads "You're $1,000 short of what's spoken for" in the `negative` token, free money shows $0 only in the legend, and the bar draws spoken-for parts beyond cash without overflowing the SVG. → Task 1 `a_shortfall_draws_within_the_box`, Task 2 `home_words_a_shortfall`.
2. **Tiny parts in the bar**: a 1-cent part next to a 10,000 one must still be listed in the legend; the bar may give it 0 width, but never a negative or fractional width. → Task 1 `widths_are_whole_and_sum_to_the_box`.
3. **A blocked currency** (`free_money: null`): its row says "No number yet:" plus the blocker sentence, never "$0". → Task 2 `a_blocked_currency_says_why`.
4. **No emergency fund set**: the caution sentence with 07c's link for admins appears under the answer, not above. → Task 2 `no_emergency_fund_is_worth_knowing`.
5. **A member without forecast access** sees Home without the card. → Task 2 `home_leaves_out_free_money_when_it_cannot_be_read`.

---

### Task 1: The stacked bar

**Files:**
- Create: `crates/server/src/http/pages/home/bar.rs` (with unit tests)
- Modify: `crates/server/assets/css/base.css` (`.money-bar`, `.money-bar .free-fill`)

**Interfaces:**
- Produces:
  ```rust
  /// One part of the bar: its amount in minor units (≥ 0) and its colour class.
  pub(super) struct Part { pub minor: i64, pub class: &'static str }
  /// Whole-unit widths for `parts` in a box `width` wide, scaled to the larger of their sum and
  /// `whole` (cash available, or what's spoken for when that is more), largest remainder first so
  /// they sum to the scaled total. Negative or zero parts get 0.
  pub(super) fn widths(parts: &[Part], whole: i64, width: u32) -> Vec<u32>;
  /// The bar: `svg.money-bar` `viewBox="0 0 {width} 16"`, one `rect.tier-fill.<class>` per non-zero
  /// width, `aria-hidden="true"` (the legend is the text equivalent).
  pub(super) fn bar(parts: &[Part], whole: i64) -> Markup;
  ```
  Width 320. All arithmetic in `i128`.

- [ ] **Step 1: Write the failing tests** — `widths_are_whole_and_sum_to_the_box` (`[1, 10_000, 4_999]` of 15_000 in 320 → three whole numbers summing to 320, the first may be 0); `a_shortfall_draws_within_the_box` (parts summing to 2× `whole` → sum of widths is 320, never more); `zero_and_negative_parts_get_nothing`; `huge_amounts_do_not_overflow` (`i64::MAX / 2` twice); `bar_uses_classes_only` (the markup holds no `style` and no `#` colour).
- [ ] **Step 2: Run** `… -p coffer-server … -E 'test(home::bar)'`. Expected: FAIL.
- [ ] **Step 3: Implement** `widths` and `bar`, and the CSS.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07i commit -m "Draw a stacked money bar in whole units"`

### Task 2: The free money card on Home

**Files:**
- Create: `crates/server/src/http/pages/home/free_money.rs`, `crates/server/src/http/pages/home/words.rs`
- Modify: `crates/server/src/http/pages/home.rs` (the card first, above net worth), `crates/server/src/http/pages/forecast/words.rs` (make `caution_sentence`, `blocker_sentence` and `Names` `pub(in crate::http::pages)`; if `Names` needs forecast lines Home doesn't have, add `Names::empty()` and let sentences that name an item fall back to "a planned item", tested), `crates/server/assets/css/base.css` (`.hero-number`, `.legend`)
- Test: `crates/server/tests/it/pages/home.rs`

**Interfaces:**
- Consumes: `Ledger::free_money(actor) -> FreeMoney` (07h), `Ledger::household_settings` (display currency), `viewer.can_manage_household`, `bar::{Part, bar}`.
- Produces: `pub(super) fn card(answer: &FreeMoney, display: Option<Currency>, can_manage: bool) -> Markup` and `pub(super) fn legend_rows(row: &FreeMoneyCurrency) -> Vec<(&'static str, Money, &'static str)>` (label, amount, class) in §4.3's order, leaving out zero "coming up" and zero goal rows; words in `home/words.rs`: `hero_sentence(row) -> String` ("$3,850 free" / "You're $1,000 short of what's spoken for"), `held_sentence(row) -> String` ("Emergency fund: $3,000 of $3,000" / "No emergency fund set").
- Card layout: `section.card.home-free`, `h2` "Free money", the leading currency's hero number (`.hero-number`, `--font-num`), the bar, a `dl.legend` with each part (the text equivalent), other currencies as one line each ("EUR: €420 free"), then "Worth knowing" with the cautions' sentences, then the link "How this is worked out" → `/forecast`.

- [ ] **Step 1: Write the failing tests** (`pages/home.rs`, seeding as 07h's `free_money_answers_the_formula` does):
  - `home_leads_with_free_money` — the first `section.card` on `/` is free money, the hero reads "$4,082.00 free" (or the grouping `amounts::money` produces), and the legend lists, in order, Cash available, Must pay, Essentials, Flexible, Set aside for goals, Emergency fund, Free money with their amounts.
  - `home_words_a_shortfall` — cash below what's spoken for → "short of what's spoken for", free money 0 in the legend.
  - `the_display_currency_leads` — USD and EUR rows with display currency EUR → EUR is the hero, USD the line below; without a display currency, EUR (first by code) leads and an admin sees the "Choose a display currency" line.
  - `a_blocked_currency_says_why` — EUR without a balance → "No number yet:" and the blocker's sentence, no "€0".
  - `no_emergency_fund_is_worth_knowing` — the caution sentence appears after the legend (string index), with the household link for an admin and "An admin can set one" for a member.
  - `home_leaves_out_free_money_when_it_cannot_be_read` — a member whose role or grants fall below forecast access: 200, no "Free money" heading.
  - `the_breakdown_links_to_the_forecast`.
- [ ] **Step 2: Run** `… -p coffer-server … -E 'test(pages::home)'`. Expected: FAIL.
- [ ] **Step 3: Implement** the card, the words and the visibility change.
- [ ] **Step 4: Run** the tests, then `uptime` and `nice -n 15 scripts/gate.sh`. Expected: PASS, gate green.
- [ ] **Step 5: Commit, push, PR** — `git -C ../coffer-07i commit -m "Lead Home with free money and its breakdown"`; update `AGENTS.md`'s crate map (`home/free_money.rs`, `home/bar.rs`, `home/words.rs`); push `money-07i-home-free-money`; PR "Home: free money and its breakdown", "Closes #148", with how it looked at 375 px and 1280 px, Mochi and Fjord, light and dark (or that it wasn't looked at).
