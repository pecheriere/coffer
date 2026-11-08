# 07g — A bill paid early needs nothing in the forecast Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A bill occurrence dated today or within the next 7 days that a payment already made matches needs nothing from the day it is matched, in the shared reduction, so the payment is no longer counted twice (gone from the balance and still needed). Closes #146.

**Architecture:** Domain only, in `forecast/reduction.rs` and `forecast/lines.rs`. Today `reduce` hands `match_bills` the occurrences from the look-back up to `as_of` plus later ones settled as paid, and `bills_counted` and `dated_line` count every occurrence dated `as_of` or later in full. After this change `reduce` also hands it the unsettled occurrences in `(as_of, as_of + BILL_EARLY_PAYMENT_DAYS]`, each occurrence a payment pays (`Seen::Paid`) needs 0 whatever its date, and an occurrence after `as_of` that nothing pays simply counts in full, with no gap and no caution (it isn't due yet). Matching itself doesn't change: transactions oldest first, each paying the oldest occurrence it fits, so an older unpaid occurrence still wins a payment that fits both.

**Tech Stack:** Rust 2024, proptest.

**Spec:** `docs/superpowers/specs/2026-11-07-coffer-money-redesign-design.md` §10.8 (binding). Overview: `docs/superpowers/plans/2026-11-07-coffer-07-money-redesign-overview.md`.

**Depends on:** nothing. **Parallel with** 07a to 07f: it touches only `forecast/{reduction.rs, lines.rs, bill_matching.rs}` and their tests, which 07b and 07e don't edit (07b edits `forecast.rs`, `types.rs`, `cautions.rs`, `book.rs`; if 07b merges first, rebase: the conflict is limited to test helpers). **Branch / worktree:** `money-07g-bills-paid-early`, `../coffer-07g`. No migration, no API shape change (only answers).

## Global Constraints

- Everything in the overview's "Rules for every unit" applies.
- The early window is `BILL_EARLY_PAYMENT_DAYS` (7) and the amount tolerance `BILL_AMOUNT_TOLERANCE_PERCENT` (10): unchanged (§10.7, §10.8 "as today").
- "From the day it is matched": the forecast at a day before the payment posts still counts the bill; at the payment's day and after, it doesn't.
- A payment pays one occurrence at most; a matched payment is taken out of its category's budget pool (as for past bills), so an early bill payment no longer lowers a budget in the same category.
- The answer's `payments` for an item lists the paid-ahead occurrence with its `paid_by`, so the API and the page can say "paid on Jan 27".
- Exact integer maths, no change to `ForecastError` cases.

## Review Focus

1. **The comment and logic that say today's bill "counts in full whatever pays it"** (`reduction.rs` module docs, `bills_counted`, `pools_settled`'s docs). The settled-bill guard in `pools_settled` compares counted needs with and without settlements; it must use the new counting in both arms or settling could raise the need again. → `settling_never_raises_the_need` (existing proptest in `tests/settling_need.rs`, kept green) and Task 1 `a_bill_settled_and_one_paid_early_in_one_pool`.
2. **A payment that fits last month's unpaid bill and next week's**: it pays last month's (oldest), and next week's still counts. → `an_older_unpaid_bill_keeps_its_payment`.
3. **Monthly bill on the 1st, forecast on the 30th, paid on the 28th:** the occurrence on the 1st needs 0, the next month's 1st still counts in a 2-month horizon. → `only_the_paid_occurrence_needs_nothing`.
4. **Free money's window (07h) relies on this:** a mortgage due on the 1st, paid on the 29th, must not be counted in "still need" on the 30th. → covered by Task 1's tests; 07h adds its own.
5. **A refund after the early payment**: the refund rules (`REFUND_UNDOES_*`) apply to the paid-ahead occurrence as to any other; a refund undoing the payment makes it count again. → `a_refund_undoes_an_early_payment`.

---

### Task 1: Paid ahead needs nothing

**Files:**
- Modify: `crates/domain/src/forecast/reduction.rs` (`reduce`: the extra occurrences; `record`: an occurrence after `as_of` adds a paid-ahead date when `Seen::Paid` and nothing otherwise; `bills_counted`: an occurrence counts unless settled, before history, or paid; module docs), `crates/domain/src/forecast/lines.rs` (`dated_line` leaves out the paid-ahead occurrences in the window), `crates/domain/src/forecast/bill_matching.rs` (docs only, if they say the look-back ends at `as_of`)
- Test: `crates/domain/src/forecast/tests/paid_early.rs` (new, `mod paid_early;` in `tests.rs`), existing tests that pin the old behaviour (find them with `rg -n 'in full' crates/domain/src/forecast/tests` and `rg -n "today's bill|todays_bill" crates/domain/src/forecast/tests`; `settling_need.rs:142,159` are two), `crates/server/tests/it/forecast/bill_payments.rs`

**Interfaces:**
- `ItemAdjustment` gains `pub(super) paid_ahead: Vec<Date>`: the occurrences dated on or after `as_of` that a payment pays, in date order. `dated_line` subtracts `amount × paid_ahead.len()` for those inside the window and doesn't put them in the month buckets' amounts; it still lists them in `payments`.
- `reduce` adds `item.schedule.occurrences(through_as_of, early_until)` for `early_until = as_of + BILL_EARLY_PAYMENT_DAYS + 1 day` (exclusive), and no later than the window's `end_exclusive`; it may cross into next month (2027-01-30 reaches 2027-02-06), unlike `reducible_until`; unsettled ones only (settled-as-paid later ones are already added).

- [ ] **Step 1: Write the failing tests** (`tests/paid_early.rs`, using the helpers in `tests.rs`):
  - `a_bill_paid_early_needs_nothing` — as_of 2027-01-30, bill 1000.00 on 2027-02-01 in category Housing, a 1000.00 Housing payment posted 2027-01-28: the item's line counts 0 in the window, `payments` shows the 2027-02-01 occurrence paid by that transaction, `planned_outflows` is 0.
  - `a_bill_paid_today_needs_nothing` — bill and payment both on as_of → 0 (this replaces the old "counts in full" expectation).
  - `before_the_payment_it_still_counts` — same data, as_of 2027-01-27 (payment not yet posted, so not in `spent_transactions`) → 1000.00.
  - `eight_days_early_pays_nothing` — payment posted 2027-01-24 for the 2027-02-01 bill → 1000.00 still counted, and the payment stays in its pool.
  - `only_the_paid_occurrence_needs_nothing` — monthly bill, 2-month horizon from 2027-01-30: 2027-02-01 needs 0, 2027-03-01 counts.
  - `an_older_unpaid_bill_keeps_its_payment` — last month's occurrence unpaid and the next one 3 days ahead, one fitting payment: it pays last month's; the next one counts in full; no "unpaid from last month" caution.
  - `an_early_payment_no_longer_lowers_the_budget` — a budget in the same category: the payment is taken out of the pool, so the budget's reduction doesn't include it.
  - `a_refund_undoes_an_early_payment` — a refund of 100% after the payment → the occurrence counts again.
  - `a_bill_settled_and_one_paid_early_in_one_pool` — one bill settled as paid this month and another paid early in the same category with a budget: the need is never above the need with nothing settled (the property `settling_need.rs` states, as a fixed case).
- [ ] **Step 2: Run** `… -p coffer-domain … -E 'test(paid_early)'`. Expected: FAIL (the occurrence still counts).
- [ ] **Step 3: Implement** the change in `reduce`, `record`, `bills_counted`, `dated_line`; rewrite the module docs' second paragraph to say a bill counts in full until paid, by a payment up to 7 days before its date, or until its date passes unpaid (then a gap). Update the old tests named above to the new behaviour, each with a one-line comment citing §10.8.
- [ ] **Step 4: Run** `… -p coffer-domain --all-features -j 3` (whole crate: the proptests in `tests/properties.rs` and `tests/settling_need.rs` must hold) and `… -p coffer-server … -E 'test(forecast)'`. Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07g commit -m "Count a bill paid up to a week early as paid in the forecast"` with a body naming §10.8 and the double count it removes.

### Task 2: Docs and the gate

**Files:** `docs/agent-forecast.md` (how a bill is matched: paid early now needs nothing), `AGENTS.md` only if its forecast lines describe the old rule.

- [ ] **Step 1:** Update the docs.
- [ ] **Step 2:** `uptime`; `nice -n 15 scripts/gate.sh`. Expected: green.
- [ ] **Step 3: Commit, push, PR** — push `money-07g-bills-paid-early`; PR "Count a bill paid early as paid in the forecast", "Closes #146", naming each old test whose expectation changed and why.
