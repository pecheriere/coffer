# 07f — Tiers on the plan page, with tier colours Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The plan page lists items grouped by tier, each with a tier mark, and its add and change forms choose a tier where one applies; five tier colours become theme tokens in Mochi and Fjord. Closes #145.

**Architecture:** Page and theme only. The page reads `PlannedItemView.tier` (07e) and sends `tier` in the input and patch the API takes. Tier colours are new colour tokens, defined in both theme files with `light-dark()`, added to the theme contract and its contrast pairs as graphics (`Min::Ui`, 3:1), and read by `base.css` classes only. Home (07i) and Money (07l) reuse them.

**Tech Stack:** Rust 2024, axum, maud, CSS custom properties.

**Spec:** `docs/superpowers/specs/2026-11-07-coffer-money-redesign-design.md` §3, §4.1, §5 (theme rules: "tier colours become new tokens in both themes, AA-checked"), §7 phase 2. §8 leaves the colour values to the builder.

**Depends on:** 07e (#144). **Branch / worktree:** `money-07f-tier-page`, `../coffer-07f`. No migration, no API change.

## Global Constraints

- Everything in the overview's "Rules for every unit" applies.
- No colour literal outside `assets/themes/*.css`; components read tokens only. Every token in both schemes, AA-checked by `crates/server/src/themes/tests.rs`.
- Tier colour is never the only signal: each mark carries the tier's word (WCAG 1.4.1).
- Words: "Must pay", "Essentials", "Flexible", "Saving toward"; the form's hint per tier, one line each: Must pay "Never used to cover other spending.", Essentials "Can cover other essentials.", Flexible "Can cover essentials and other flexible spending.", Saving toward "Used only when you choose." (§4.1's "can lend to" column).
- The form offers only the tiers the item may have: none for income, Saving toward alone (shown, not chosen) for a goal, the other three for a recurring or one-off outflow. The server's rules (07e) stay the authority; the page shows the API's refusal if a crafted form sends another.

## Review Focus

1. **A form switching direction or kind** (the combined form has every fieldset): a tier sent for income must not make the page fail with a generic error; send no `tier` when the chosen direction is inflow or the kind is goal. → Task 2 `income_sends_no_tier`, `goal_sends_no_tier`.
2. **Contrast in dark mode**: tier fills against `card-bg` and `surface` at 3:1. → Task 1's theme tests.
3. **Grouping order and empty groups**: Must pay, Essentials, Flexible, Saving toward, then Income, then Paused last within each? Decision: groups in that order, empty groups not shown, paused items last inside their group. → Task 2 `groups_follow_the_tier_order`.
4. **Mochi and Fjord drifting**: the same five token names in both files. → Task 1 `every_theme_defines_the_tier_tokens` (the contract test covers it once the names are in `COLOUR_TOKENS`).

---

### Task 1: Tier colour tokens

**Files:**
- Modify: `crates/server/assets/themes/mochi.css`, `crates/server/assets/themes/fjord.css`, `crates/server/src/themes/contract.rs` (`COLOUR_TOKENS` and `PAIRS`), `crates/server/assets/css/base.css` (`.tier-mark`, `.tier-must-pay` … classes)
- Test: `crates/server/src/themes/tests.rs` (existing tests pick the tokens up from the contract)

**Interfaces:**
- Produces tokens: `tier-must-pay`, `tier-essentials`, `tier-flexible`, `tier-saving`, `tier-emergency`. Pairs added, each with `Min::Ui`: every tier token against `card-bg` and against `surface`.
- Produces classes: `.tier-mark` (a small filled square before the tier word, `background: var(--tier-…)` through `.tier-must-pay`, `.tier-essentials`, `.tier-flexible`, `.tier-saving`, `.tier-emergency`), so SVG and HTML marks share one set of classes (SVG uses `fill` in the same rules: `.tier-must-pay { --tier: var(--tier-must-pay) }` and `.tier-mark { background: var(--tier) } svg .tier-fill { fill: var(--tier) }`).
- Colour choice: distinct hues that read as calm, not alarming (no red: `negative` stays for overspend); Must pay the deepest, Emergency fund neutral slate. Keep each at least 0.15 apart in Oklab from `negative` (the contract's `NEGATIVE_ACCENT_MIN_DISTANCE` idea; add a test `tier_colours_are_not_read_as_negative` beside the existing distance test).

- [ ] **Step 1: Write the failing test** — add the five names to `COLOUR_TOKENS` and the ten pairs to `PAIRS`, plus `tier_colours_are_not_read_as_negative`.
- [ ] **Step 2: Run** `… -p coffer-server … -E 'test(themes)'`. Expected: FAIL (tokens missing in both themes).
- [ ] **Step 3: Implement** the tokens in both theme files and the classes in `base.css`.
- [ ] **Step 4: Run** the theme tests. Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07f commit -m "Add tier colours to both themes"`

### Task 2: Tiers on the plan page

**Files:**
- Modify: `crates/server/src/http/pages/plan.rs` (grouping), `crates/server/src/http/pages/plan/form.rs` (a tier fieldset; `ItemForm.tier`, into input and patch), `crates/server/src/http/pages/plan/words.rs` (`tier_word(PlanTier) -> &'static str`, `tier_hint(PlanTier) -> &'static str`, `tier_class(PlanTier) -> &'static str`)
- Test: `crates/server/tests/it/plan_page/tiers.rs` (new, `mod tiers;` in `plan_page/mod.rs`), and `plan_page/forms.rs` where it posts the full form

**Interfaces:**
- Consumes: `PlannedItemView.tier`, `PlannedItemInput.tier`, `PlannedItemPatch.tier` (07e).
- Produces: the items card becomes one `section.card` per non-empty group, headed by the tier word with its `.tier-mark`, in the order Must pay, Essentials, Flexible, Saving toward, Income; paused items last inside each. Each row keeps today's content. The form gets a `fieldset.choices` "Tier" (legend "Which bucket this money is in") with three radios and their hints, inside the recurring and one-off fieldsets' area; a goal shows "Goals are always saving toward." instead; income shows nothing. The posted `tier` is sent only when the chosen direction is outflow and the kind is recurring or one-off; the change form preselects the item's tier, the add form preselects nothing (the server defaults by kind).

- [ ] **Step 1: Write the failing tests** (`plan_page/tiers.rs`):
  - `groups_follow_the_tier_order` — items: a flexible budget, an income, a must pay bill, a goal, a paused essentials budget and an active one; headings appear in the order Must pay, Essentials, Flexible, Saving toward, Income, and inside Essentials the paused one is last; no Saving toward heading when there is no goal (second household).
  - `adding_a_budget_with_a_tier` — post the add form for a recurring outflow, spending budget, `tier=essentials` → the API answers `essentials`.
  - `adding_without_a_tier_takes_the_default` — no `tier` field → budget is flexible.
  - `income_sends_no_tier` — the add form with direction inflow and a stray `tier=flexible` → saved, tier null (the page drops it).
  - `goal_sends_no_tier` — same for a goal → saving toward.
  - `changing_the_tier` — the change form with `tier=must_pay` on a flexible budget → must pay, audited.
  - `a_viewer_sees_tiers_but_no_form`.
- [ ] **Step 2: Run** `… -p coffer-server … -E 'test(plan_page)'`. Expected: FAIL.
- [ ] **Step 3: Implement** the grouping, the fieldset and the words.
- [ ] **Step 4: Run** the tests, then `uptime` and `nice -n 15 scripts/gate.sh`. Expected: PASS, gate green.
- [ ] **Step 5: Commit, push, PR** — `git -C ../coffer-07f commit -m "Show and set tiers on the plan page"`; push `money-07f-tier-page`; PR "Tiers on the plan page, with tier colours", "Closes #145", with the chosen colour values and how they looked at 375 px in both themes and schemes (or that they weren't looked at).
