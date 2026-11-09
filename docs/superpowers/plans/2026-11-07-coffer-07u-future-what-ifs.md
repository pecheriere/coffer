# 07u — Future what-ifs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stored what-ifs on the Future projection, per household so agents can read and add them (§4.7): a one-off purchase, a change in monthly saving from a month on, a year without income. Each gets its own line on the likely growth, the date it reaches the target, and the change in months against the plain projection (or, without a target, the change in value at 25 years). Closes #160.

**Architecture:** Domain: `future/what_if.rs` turns each what-if into per-month adjustments to 07t's monthly step (one stepper, `grow` generalised to take adjustments, so the plain line is the what-if line with none). Store: `future_what_ifs` (migration `0037`). Ledger: the what-ifs join 07t's `future_data` read, and `Future` answers them. API: create and remove; the list is part of `GET /api/v1/future`. MCP: add (removing stays REST, as MCP deletes nothing).

**Tech Stack:** Rust 2024, sqlx (SQLite), axum, utoipa, rmcp, proptest.

**Spec:** `docs/superpowers/specs/2026-11-07-coffer-money-redesign-design.md` §4.7 (what-ifs: a one-off purchase, a change in monthly saving from a year, a year without income; a dotted line each; the change in months; stored per household).

**Depends on:** 07t (#159) merged. **Branch / worktree:** `money-07u-future-what-ifs`, `../coffer-07u`. **Migration:** `0037`.

## Global Constraints

- Everything in the overview's "Rules for every unit" applies.
- The kinds and what each does to the likely line (07t's step, month `m` counted from this calendar month, `k` = the what-if's month's index):
  ```
  purchase { month, amount > 0 }           v(k + 1) −= amount, once
  saving_change { month, monthly_change }  saving += monthly_change for every m ≥ k (signed, ≠ 0)
  year_without_income { month }            saving −= monthly_income for m in k..k+12
                                           (07t's `inputs.monthly_income`, so spending goes on)
  ```
  A month from this one to the horizon's last; a later or earlier one is 422. "From a year" in §4.7 is offered as a month, which covers it (planner's call).
- Each what-if's answer: yearly points (as 07t's lines), `reaches` (date or null), `months_later = reaches − likely reach` in whole months (positive is later; null when there is no target or either line never reaches it), `value_at_horizon` and `change_at_horizon` (against the plain likely line's last point).
- Amounts are in the display currency when created (422 otherwise, or without a display currency). One whose currency is no longer the display currency is answered with no line and the caution `what_if_in_other_currency { what_if }`, never converted.
- A label of 1 to 64 characters, the same validation as an account's name. At most 20 what-ifs (planner's call; 422 past it).
- Adding or removing needs `WriteHousehold` and the forecast's access, decided before anything is read; audited (`household.future.what_if.add` / `.remove`, with the canonical text of the what-if). Removing a missing one is 404, the same whatever exists.

## Review Focus

1. **The plain line moving.** With no what-ifs, every 07t test still passes untouched: the generalised stepper with empty adjustments is 07t's `grow`. → Task 1 proptest `no_adjustments_is_the_plain_line`.
2. **Months later, sign and edges.** A 50,000.00 purchase two years out delays a reachable target; a saving rise brings it forward (negative); a what-if that never reaches gives null, not a huge number. → Task 1 `a_purchase_delays_the_target`, `a_saving_rise_brings_it_forward`, `never_reaching_is_null`.
3. **A year without income** takes `monthly_income` for exactly 12 months, then saving resumes. → Task 1 `a_year_without_income_lasts_twelve_months`.
4. **The cross-column CHECK** keeps a purchase from having no amount. → Task 2 `the_table_refuses_a_purchase_without_an_amount`.
5. **Restricted tokens and MCP deleting nothing.** → Task 3 `a_restricted_token_is_refused`, `there_is_no_remove_tool`.

---

### Task 1: What-ifs in the domain

**Files:**
- Create: `crates/domain/src/future/what_if.rs`, `crates/domain/src/future/tests/what_if.rs`
- Modify: `crates/domain/src/future.rs` (`mod what_if;`, re-exports), `crates/domain/src/future/projection.rs` (`grow` takes `&[i128]` adjustments per month; the plain line passes none), `crates/domain/src/future/types.rs` (`FutureInput.what_ifs`, `Future.what_ifs`, the new caution), `crates/domain/src/ids.rs` (`id_type!(FutureWhatIfId …)`), `crates/domain/src/future/tests.rs`

**Interfaces:**
  ```rust
  pub enum WhatIfKind {                     // Deserialize + Serialize, tagged `kind`, ToSchema
      Purchase { month: CalendarMonth, amount: Money },
      SavingChange { month: CalendarMonth, monthly_change: Money },
      YearWithoutIncome { month: CalendarMonth },
  }
  impl WhatIfKind { pub fn validate(&self, today: Date) -> Result<(), WhatIfError>; pub fn canonical(&self) -> String; }
  pub struct StoredWhatIf { pub id: FutureWhatIfId, pub label: String, pub what: WhatIfKind }
  pub struct WhatIfLine { pub id: FutureWhatIfId, pub label: String, pub what: WhatIfKind,
      pub points: Vec<FuturePoint>, #[schema(required)] pub reaches: Option<Date>,
      #[schema(required)] pub months_later: Option<i32>,
      pub value_at_horizon: Money, pub change_at_horizon: Money }
  ```

- [ ] **Step 1: Write the failing tests** (`tests/what_if.rs`, 07t's helpers): `a_purchase_delays_the_target`, `a_saving_rise_brings_it_forward`, `a_saving_cut_to_below_zero_still_steps` (negative saving), `a_year_without_income_lasts_twelve_months`, `never_reaching_is_null`, `without_a_target_the_change_is_in_value`, `a_month_past_the_horizon_is_refused`, `a_month_before_this_one_is_refused`, `a_what_if_in_another_currency_has_no_line`, `canonical_text_round_trips`; proptests `no_adjustments_is_the_plain_line`, `a_purchase_never_raises_the_line`.
- [ ] **Step 2: Run** `nice -n 15 env -u CARGO_TARGET_DIR cargo nextest run -p coffer-domain --all-features -j 3 -E 'test(future)'`. Expected: FAIL.
- [ ] **Step 3: Implement**: generalise `grow` first (07t's tests green), then `what_if.rs`.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07u commit -m "Draw what-ifs on the Future projection"`

### Task 2: The store

**Files:**
- Create: `crates/store/migrations/0037_future_what_ifs.sql`
- Modify: `crates/store/src/future.rs` (what-if reads and writes; `future_data` reads them; split into `future/what_ifs.rs` if it passes about 400 lines)

**Interfaces:**
- Migration: `future_what_ifs (id INTEGER PRIMARY KEY, kind TEXT NOT NULL CHECK (kind IN ('purchase', 'saving_change', 'year_without_income')), label TEXT NOT NULL CHECK (length(label) BETWEEN 1 AND 64), month TEXT NOT NULL CHECK (month GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]'), amount_minor INTEGER CHECK (amount_minor BETWEEN -1000000000000000 AND 1000000000000000), currency TEXT CHECK (…as 0030…), created_at TEXT NOT NULL, CHECK ((kind IS 'purchase' AND amount_minor > 0 AND currency IS NOT NULL) OR (kind IS 'saving_change' AND amount_minor IS NOT 0 AND amount_minor IS NOT NULL AND currency IS NOT NULL) OR (kind IS 'year_without_income' AND amount_minor IS NULL AND currency IS NULL))) STRICT;`
- Store: `future_what_ifs() -> Vec<StoredWhatIf>` (by id), `insert_future_what_if(label, &WhatIfKind, created_at) -> FutureWhatIfId`, `delete_future_what_if(id) -> bool`, `count_future_what_ifs() -> u32`; a row that no longer parses is corrupt, as elsewhere.

- [ ] **Step 1: Write the failing tests** — `what_ifs_round_trip_in_id_order`, `the_table_refuses_a_purchase_without_an_amount`, `the_table_refuses_an_amount_on_a_year_without_income`, `delete_says_whether_it_removed_one`, `future_data_holds_the_what_ifs`.
- [ ] **Step 2: Run** `… -p coffer-store --all-features -j 3 -E 'test(future)'`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07u commit -m "Store Future what-ifs"`

### Task 3: Ledger, API and MCP

**Files:**
- Create: `crates/server/tests/it/future/what_ifs.rs` (turn 07t's `tests/it/future.rs` into `tests/it/future/mod.rs` with this child)
- Modify: `crates/server/src/ledger/future.rs` (`add_future_what_if`, `remove_future_what_if`; split into `ledger/future/what_ifs.rs` if past about 400 lines), `crates/server/src/http/api/future.rs`, `crates/server/src/http/api/{mod.rs, openapi.rs}`, `crates/server/src/http/api/mcp/tools.rs` (or its area file), `crates/server/tests/it/mcp/parity/mod.rs`, `docs/api/openapi.json`, `docs/agent-future.md` ("What-ifs")

**Interfaces:**
- Ledger: `pub async fn add_future_what_if(&self, actor, label: String, what: WhatIfKind) -> Result<Future, LedgerError>`; `pub async fn remove_future_what_if(&self, actor, id: FutureWhatIfId) -> Result<Future, LedgerError>` — both authorize first, write and audit in one transaction, then answer the whole `Future` (as the emergency fund setter answers the settings).
- API: `POST /api/v1/future/what-ifs` (`addFutureWhatIf`, body `{ "label": …, "what": { "kind": "purchase", "month": "2029-06", "amount": {…} } }`, needs `write`); `DELETE /api/v1/future/what-ifs/{id}` (`removeFutureWhatIf`, needs `write`). `getFuture`'s description gains the what-ifs.
- MCP: `add_future_what_if` (`Needs::Write`; lead: "Add a what-if only when the member asked for one. Report `months_later` as 'about N months later/sooner', and `null` as 'doesn't reach the target within 25 years'.").

- [ ] **Step 1: Write the failing tests** — `a_what_if_is_added_and_answered` (a purchase → `what_ifs[0].months_later` > 0), `a_what_if_is_removed`, `removing_a_missing_one_is_404`, `adding_is_audited`, `a_twenty_first_is_refused`, `an_amount_in_another_currency_is_refused`, `a_restricted_token_is_refused` (POST and DELETE, same body whatever exists), `a_read_token_cannot_add`, `there_is_no_remove_tool` (no MCP tool names `removeFutureWhatIf`); parity row for `add_future_what_if`.
- [ ] **Step 2: Run** `… -p coffer-server --all-features -j 3 -E 'test(future) | test(parity)'`. Expected: FAIL.
- [ ] **Step 3: Implement**; regenerate `docs/api/openapi.json`; update the docs.
- [ ] **Step 4: Run**, then `uptime` and `nice -n 15 scripts/gate.sh`. Expected: PASS, gate green.
- [ ] **Step 5: Commit, push, PR** — `git -C ../coffer-07u commit -m "Add and remove Future what-ifs through the API, and add them through MCP"`; push `money-07u-future-what-ifs`; PR "Future what-ifs", "Closes #160", with the planner's calls (a month for "from a year", at most 20, value change without a target) and what was verified locally versus by CI.

## Open questions for the owner

None.
