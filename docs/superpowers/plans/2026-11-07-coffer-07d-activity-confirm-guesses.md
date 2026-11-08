# 07d — Activity: what's waiting, and confirming rules' guesses Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The Activity tab (the transactions list) says how many transactions wait to be checked, and one button marks every transaction on the page that a rule filed, and nobody has touched since, as reviewed. Closes #143.

**Architecture:** Page-only. The count is 07a's `Ledger::count_transactions_to_review`. The button is a second form on the list that posts hidden `pick_<id>=yes` fields for the rule-filed, unreviewed rows shown, to the existing `/transactions/batch` handler with `review=reviewed`, which calls `Ledger::apply_batch`, the API's batch call. No new route, no API change: `review_transactions` (MCP) and `POST /api/v1/transactions/batch` already do this for agents.

**Tech Stack:** Rust 2024, axum, maud.

**Spec:** `docs/superpowers/specs/2026-11-07-coffer-money-redesign-design.md` §5 (Activity). Overview: `docs/superpowers/plans/2026-11-07-coffer-07-money-redesign-overview.md`.

**Depends on:** 07a (#140). **Branch / worktree:** `money-07d-activity`, `../coffer-07d`. No migration.

## Global Constraints

- Everything in the overview's "Rules for every unit" applies.
- A rule's guess is what `coffer_domain::rules::decide` filled and nobody changed: on the view, `categorized_by.rule.is_some()` or `assigned_by.rule.is_some()` (editing clears the rule, `ledger/transactions.rs:122`). Confirming reviews; it never changes a category or an assignee (spec §8.13 U5, U7 of the original design: a rule never reviews by itself, a person does here).
- Only the rows on the page are confirmed, never a hidden page or a filter's whole result: the button says how many, and that is how many are sent.
- Read-only viewers see neither form (same condition as the existing batch form).
- At most 200 operations per batch (`MAX_BATCH_OPERATIONS`); a page holds at most 200 rows, so one post suffices.

## Review Focus

1. **A row a person already categorized** (rule attribution cleared) must not be in the button's set. → `confirms_only_untouched_rule_guesses`.
2. **A row on the page that is already reviewed** must not be sent (it would be a no-op, but the count on the button must be honest). → same test.
3. **The count when the viewer reads only some accounts.** → `the_count_is_what_the_viewer_reads` (07a's call already scopes; pin it on the page).
4. **Zero guesses**: no button, not a disabled one. → `no_guesses_no_button`.
5. **The redirect keeps the filters** (`back` field), so the person stays in the queue. → `confirming_returns_to_the_same_list`.

---

### Task 1: The count and the confirm button

**Files:**
- Modify: `crates/server/src/http/pages/transactions/list.rs` (content and the new form; if it passes about 300 lines, put the form in `transactions/guesses.rs`)
- Test: `crates/server/tests/it/transactions_page/review.rs` (or a new `transactions_page/guesses.rs` with its `mod` line if `review.rs` is near the limit)

**Interfaces:**
- Consumes: `Ledger::count_transactions_to_review(actor) -> ReviewCount` (07a), the page's `TransactionPage`, the existing `BatchForm` (`csrf`, `review`, `back`, `pick_<id>`).
- Produces: under the heading, `p.lead` "N transactions to check" linking to the queue (or "Nothing to check." at 0); when the shown page has K ≥ 1 rule guesses, a `form method="post" action="/transactions/batch"` with `csrf`, `review=reviewed`, `back=<current filters' path>`, K hidden `pick_<id>=yes`, and a button "Confirm the K guesses rules made on this page" (singular "the guess" for 1).

- [ ] **Step 1: Write the failing tests**
  - `the_page_says_how_many_wait` — 3 unreviewed (one in an account the member can't read: use a second member with an account grant) → the admin sees "3 transactions to check", the limited member sees "2 …".
  - `confirms_only_untouched_rule_guesses` — on the queue page: A filed by a rule, B filed by a rule then recategorized by a member, C filed by a member, D filed by a rule and already reviewed. The button reads "Confirm the guess rules made on this page" and its form holds `pick_<A>` only; posting it marks A reviewed, leaves B and C unreviewed and every category unchanged.
  - `no_guesses_no_button` — no rule-filed row → no form posting `review=reviewed` with hidden picks.
  - `confirming_returns_to_the_same_list` — from `/transactions?review=unreviewed`, the 303 goes back there.
  - `a_viewer_sees_no_button` — a read-only member sees the count and no form.
- [ ] **Step 2: Run** `… -p coffer-server … -E 'test(transactions_page)'`. Expected: FAIL.
- [ ] **Step 3: Implement** in `list.rs` (read the count beside `list_transactions`; a viewer without access gets the list's own refusal, so no new error path).
- [ ] **Step 4: Run** the tests, then `uptime` and `nice -n 15 scripts/gate.sh`. Expected: PASS, gate green.
- [ ] **Step 5: Commit, push, PR** — `git -C ../coffer-07d commit -m "Show what's waiting on Activity and confirm rules' guesses at once"`; push `money-07d-activity`; PR "Activity: what's waiting, and confirming rules' guesses", "Closes #143".
