# coffer plan 5 — Accounts for other people and purposes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish issue #38, and #39 with it: a household can bind its 529, custodial and donor-advised accounts as what they are, see them in buckets of their own, and have them touch neither net worth, the cash forecast, a later allocation, nor plan versus actual. A contribution reads as money leaving. A suggested kind speeds up binding and is always confirmed by a member.

**Architecture:** `AccountKind` gains three kinds, and a small `BelongsTo` (`household`, `someone_else`, `given_away`), kept apart from the kind, answers whose money it is. One pure function resolves it and the two inclusion flags for every create, bind and patch, and a table CHECK holds the result, so every reader of those flags stays right untouched. The net worth report runs its one calculation again per bucket. Everything still applies at read: the daily table doesn't change.

**Tech Stack:** Rust 2024, axum, sqlx (SQLite), jiff, maud, utoipa, proptest. No new crates.

**Spec:** `docs/superpowers/specs/2026-10-30-coffer-design.md`: §6.1, §6.5, §6.9, §8.2, §8.9 R2, R6, R9, R9a, R10, R16, R17, §8.10 N2, N3, N4, N7, N9, N13, and **§8.11, which this branch adds** (rulings B1–B13). No task writes spec text; if the owner overrules a ruling, change §8.11 and this plan first.

**Depends on:** plan 4 (PR #82, branch `plan-4`), which ends at migration `0021`. Start the `plan-5` integration branch from `main` once #82 has merged, or from `plan-4` until then. This plan owns `0022`. Plan 4's optional Task 10 also names `0022`; it wasn't built, and if it ever is, it renumbers.

**Before deploying:** run `coffer backup <dest>`. `0022` rebuilds `accounts` and `provider_accounts` at start, which should take milliseconds: rebuilding `accounts` alone with 30 accounts, 109,500 daily rows and as many snapshots took 16 ms in the `sqlite3` CLI on a Mac. Not measured on a Pi; Task 1 says what it finds. No new host, no new configuration. A household holding an account in a second currency only in a bucket starts fetching rates for it (N7), through the host already allowed.

## Global Constraints

- **Whose money it is decides what counts:** an account that isn't `household` has both inclusion flags false, held by a CHECK, and asking for `true` is 422. Never add a way to count one without changing `belongs_to`, and no third flag (B4, B8).
- **Nothing in a net worth answer may depend on an account the actor can't see at `balances`** (N2, B7), and nothing about a bucket may change the household's numbers, list or `status` (B6). Filter in the query. `AccountAccess` comes only from `AccessResolver`.
- No total adds a bucket to the household's or to another bucket, in the API or on a page (B6).
- A beneficiary and a provider's name are never in a log line, an error or a problem body. Audit entries hold a beneficiary as they hold an account's name. Messages are fixed and never echo input.
- Docs and tests use generic names only: `Child A`, `Child B`, `Family fund`, `College fund`. Nothing personal, anywhere.
- Money is exact: integer minor units, `i128` intermediates, checked arithmetic, never a float. Rounding stays per account per day (N4).
- A kind is never inferred without a member's confirmation, and an existing account is never reclassified by a migration (B11, B12).
- Every change to an account goes through a `StoreTx` with its audit entry. Reads, reports and pages write nothing, apart from each form's own write. Only `store/src/net_worth.rs` writes `account_daily_values`; this plan doesn't change that table.
- API routes only through `ApiRouter::op` with `#[utoipa::path]`, types deriving `ToSchema` or `IntoParams`, no `serde_json::Value`, extraction through `TypedJson` and `TypedQuery`. After any API change run `cargo run -p coffer-server -- openapi > docs/api/openapi.json`. On a rebase regenerate it; never hand-merge it.
- Migrations are STRICT tables whose CHECKs mirror the domain types. The store parses every value it reads.
- Pages: no inline script or style, no `style=` attribute, no colour literal outside a theme file, the default `Content-Security-Policy`. `/net-worth` stays out of `PAGES` in `http/browser_reports/fields.rs`, and its one rule, no form (N13), stands.
- Integration tests go under `crates/*/tests/it/<area>` with a `mod` line. Near the limit already, so new tests get their own files: `grants/net_worth_channels.rs` (739 lines of code), `connections/binding.rs` (698), `plan_vs_actual.rs` (665), `http/pages/connections/mod.rs` (746), `http/pages/net_worth.rs` (650), `sync/plaid/provider.rs` (795).
- While iterating: `cargo nextest run -p <crate> --all-features`. Before each commit `cargo fmt` and `scripts/gate.sh`, the full gate; worktrees share its lock. Don't start full-workspace builds in parallel.
- Lints are strict; no `#[allow]`/`#[expect]` without `reason = "..."`. No `unwrap`/`expect` outside tests. No dead code, no TODOs: what's left goes in an issue.
- Never push without the owner's confirmation. Security review (the `security-review` skill or equivalent, findings in the PR) is required on every task marked **[security]**.

**Rules every implementer must follow, repeated in each task:** keep files under about 800 lines of code, splitting in the same change; write the tests first and watch them fail; never set `CARGO_TARGET_DIR`; run git only as `git -C <path>`, never `cd ... git`, `git -c` or `git tag`; keep scratch files in a folder of the task's own (`scratchpad/plan5-task<N>/`), never committed; no Claude attribution in commits or PRs (no `Co-Authored-By`, `Claude-Session` or "Generated with"), since this is a public repo.

## Review Focus

Where this is most likely to be wrong in a way tests written by the implementer won't catch. Reviewers start here.

1. **The migration losing or orphaning rows.** SQLite's usual recipe (create new, copy, drop old, rename) **fails at `COMMIT`** with foreign keys on, even with `PRAGMA defer_foreign_keys`: the drop counts every child row as a violation, and a rename doesn't uncount them. Only re-inserting into a table of the same name does. Tried on a seeded copy of all 21 migrations on 2026-11-02 with SQLite 3.54: the recipe below keeps every row and enforces foreign keys after, while the rename fails. Coffer's bundled SQLite is 3.51.3 and the sqlx wrapper wasn't part of that trial, so Task 1's test runs the real `Migrator`. Nothing cascades from either table: the `ON DELETE CASCADE`s are all on `transactions`, `planned_items` and `agent_tokens`. → Task 1 `migration_keeps_every_row`.
2. **A literal `true` counting a 529 at bind.** `http/pages/connections/bind.rs:237` passes `include_in_net_worth: true`, and both API handlers do `unwrap_or(true)` before the kind is known. Any of them left alone either counts a child's account or fails the CHECK on every bind. → Task 2 `a_page_bind_of_a_529_stays_out`, `an_api_bind_with_no_flags_stays_out`.
3. **A reader that doesn't go through the flags.** `liquidity = 'invested'` is what #3 will reach for, and it holds all three kinds. `REPORT_ACTUALS` and `REPORT_UNCATEGORIZED` in `store/src/forecast.rs` read accounts by neither flag, the first by nothing and the second by class. → Task 4 `a_grant_from_a_fund_is_not_spending_twice`, `uncategorized_money_in_a_held_savings_account_is_left_out`.
4. **A bucket reaching the household's answer:** its currency in `by_currency`, its missing rate or stale balance making net worth `provisional`, its dates extending the range, its account in `accounts`. → Task 3 proptest `buckets_never_touch_the_household_answer`, which compares with a report that really lacks them.
5. **A bucket account read as a hole.** `gather` reports an included account with a `first_on` and no row as `daily_value_missing`, and `net_worth_data` reads rows only for `include_in_net_worth`, which is false on these. Forget the store's half and every bucket answers null. → Task 3 `a_bucket_has_its_rows`.
6. **A hidden account showing through a bucket:** its label as a bucket that exists, a sum that includes it, a bucket's position or its count. → Task 8.
7. **A child's name escaping:** a `Debug` of a record in a log line, a problem body echoing a bad label, a browser report from `/connections`, which is in `PAGES`, so its free text is logged (`assets/js/coffer.js` must not put a form value in a message). → Task 2 `a_beneficiary_is_never_logged_or_echoed`, and Task 5 Step 4's read of that script.
8. **The transitions:** a chosen `belongs_to` surviving a change of kind; a defaulted one following it; a move back to `household` restoring flags; a `kind` change and a `liquidity` override crossing. → Task 2 proptest `resolved_belonging_always_passes_the_table`.
9. **Counting a contribution twice, or not at all:** once on the way in and again as a grant; a contribution in a `transfer` category vanishing from `actual` while the bill it paid stays unpaid. → Task 4 and the docs of Task 9.
10. **A guess applied without a member.** `create_account_and_bind` falls back to `kind_hint` when `kind` is absent; a name's guess must never join that fallback, and a page's empty `kind` must not be filled from it. A 3-digit match inside a mask: `Checking x5291` is not a 529. → Task 7 `no_kind_is_refused_even_with_a_suggestion`, `digits_inside_a_longer_word_do_not_match`.
11. **The bucket section on a phone:** long labels, several buckets, null totals, under the strict CSP. → Task 6. Look at it in a real browser at 375 px, both themes, both schemes.

## Rulings

Each design question, with the recommendation taken and where it is written. "If wrong" is the cost of reversing it later. The ones only the owner can settle are at the end of this section.

| Question | Ruling | Why | If wrong |
|---|---|---|---|
| Kinds, or kind plus a concept? | **Both: three kinds, and `belongs_to`, a typed concept apart from the kind** (B1, B2). `someone_else` and `given_away` beside `household`. | The owner's words need both: a brokerage held for a child is the same answer as a 529 and a different kind, and charity is "not my money anymore". A `beneficiary` alone would make a label decide what counts, and a DAF has nobody to name. | Two more enum values or one fewer: one migration. |
| Owner: text or record? | **A free-text `beneficiary`, optional, 1 to 64 characters, only on an account that isn't `household`** (B5). | A record means endpoints, a delete rule and a household-wide name list a restricted token must not read, for two or three accounts. A later record can be filled from the labels. | Two spellings make two buckets. Forms offer existing labels through a `datalist`. |
| Defaults by kind; class | **`education` and `custodial` are `someone_else`, `charitable` is `given_away`; all three are `invested`, and out of both flags** (B2, B3). | A class says what the money is. Investments in these accounts are investments, and the forecast counts `cash` and `short_term_liability` only. | One table. |
| Can a member override? How does it stay honest? | **Only by saying whose money it is.** `belongs_to: household` is audited and the account then counts; "held for someone, but counted" can't be written, and a CHECK holds that (B4). A chosen value stays over a change of kind, as a chosen liquidity class does. | One switch, no contradictory state. Existing readers of the flags need no change, which is safer than teaching six SQL sites and two pure functions a new rule. | One CHECK, and a `Some(true)` branch. |
| Reports | **`buckets` beside the household's answer, each the same calculation over its own accounts, with its own status. No total across them, ever.** Page section under net worth, table only (B6, Task 6). Rows keep accruing. | The owner: "a separate bucket". A sum would be one more number that reads like net worth. Reusing the calculation keeps N4, N5 and N9 true without new rules. | Adding a total later is additive. |
| Grants | **A bucket is built from accounts visible at `balances`, sums those, unmarked** (B7), as N2. | It's a partial answer that is true, and marking it would reveal hidden accounts. | Loosening is one row. |
| Allocation groundwork | **None to build: no code exists. A rule in the spec: #3 reads `belongs_to = household`** (B8). A comment on #3, with confirmation (Task 9). | No dead code. | None. |
| Contribution into a bucket | **An `expense`, by rule in the docs: real outflow, reads in `actual`, pays its bill, lowers the surplus** (B9). `transfer` means within the household, and R17 already refuses it on a planned item. Tests pin both readings. | No code can tell until transfer links (#72); a comment on #72 says how they must read this. | Docs and a sentence on #72. |
| Transactions inside a bucket | **Out of plan versus actual, shown once in `not_the_households`. Goal links stay** (B10). Narrows R6. | A grant from a DAF would otherwise count a second time. | One filter in each of two queries. |
| Migration | **Rebuild `accounts` and `provider_accounts` with `belongs_to`; every row stays `household`, nothing is guessed** (B11). `kind_hint`'s CHECK widens too, since CHECKs mirror the domain type and Task 7 writes those values. | "A guess here moves net worth." The owner's three accounts are unbound and need no migration: binding creates them. | — |
| #39 | **It fits: Task 7, last of the feature, on its own branch, droppable without touching the rest** (B12). Computed at read, never stored, not logged, `kind_hint` still the only fallback. | It's a pure function, a view field and two lines in a form, and it is the new kinds that make a wrong manual pick costly. | Skip the task. |

**Questions only the owner can settle.** Each is reversible by changing §8.11 first:

1. Strict gate, B4: no "held for someone, but counted". Recommended: yes.
2. Free-text beneficiary, B5, not a person record. Recommended: yes.
3. No total adding buckets to net worth, B6. Recommended: yes.
4. Splits inside a bucket stay out of plan versus actual, B10. Recommended: yes.
5. A name's suggestion preselects the kind, with a sentence saying it's a guess (Task 7), rather than only being named beside an empty select. Recommended: preselect.
6. #39 in this plan, Task 7. Recommended: yes.
7. A form on a bound account's row on `/connections` to change its kind and whose money it is (Task 5). Recommended: yes.

**Designed to fit later:** #3 filters on `belongs_to`. An effective date on `belongs_to` would be a table of its own, applied at read. A person record could replace the label by filling it from the labels. #72's links read `belongs_to` on the far side.

## Task order and parallelism

Everything lands on an integration branch, `plan-5`, and reaches `main` as one PR, as plan 4 did: after Task 1 alone, a new kind would still count as the household's, so no task before Task 4 is safe to release by itself. Each task has its own worktree and branch: `git -C ~/Code/coffer worktree add -b plan-5-task<N> ../coffer-p5t<N> plan-5`.

| Wave | Tasks | Depends on | Run how | Separate reviewer focuses on |
|---|---|---|---|---|
| 0 | 1 | Plan 4 | Alone, merged first: every struct it changes has tests across the workspace. | The migration on a populated database. |
| 1 | 2 | 1 | Alone: it changes `NewAccountInput`, which about 20 test files build. | Every path into an account; the transitions **[security]** |
| 2 | 3, 4, 5 | 2 | **In parallel**, one worktree each. No shared source files; 3 and 4 both regenerate `docs/api/openapi.json`, so the second to merge regenerates it. | 3: household untouched **[security]**. 4: the money reads right. 5: the forms **[security]** |
| 3 | 6, 7 | 6 after 3; 7 after 5 | **In parallel.** | 6: page against API, phone. 7: nothing applied unconfirmed, nothing logged **[security]** |
| 4 | 8 | 3, 6 | Alone, and itself adversarial: a fresh agent given only §8.2, N2, B6, B7 and the task. | **[security]** |
| 5 | 9 | All | **Alone.** It reads everything and reviews the whole branch. | Docs against behaviour **[security]** |

If the owner drops #39, skip Task 7; nothing else reads it. `0022` still widens `kind_hint`, since the CHECK mirrors `AccountKind`.

---

### Task 1: The kinds, `belongs_to` and the migration

**Implementer rules:** files under about 800 lines of code; tests fail first; never set `CARGO_TARGET_DIR`; git only as `git -C <path>`, never `cd ... git`, `git -c` or `git tag`; scratch files in `scratchpad/plan5-task1/`; no Claude attribution, public repo.

**Files:**
- Create: `crates/store/migrations/0022_account_belonging.sql`, `crates/store/tests/it/account_belonging.rs`
- Modify: `crates/domain/src/accounts.rs`, `crates/store/src/accounts.rs`, `crates/store/tests/it/main.rs`, `crates/server/src/ledger/accounts.rs` (the new store fields only), `crates/server/src/http/pages/connections/bind.rs` (`kind_label`), every test building `NewAccount { .. }` (about 30 sites; `rg 'NewAccount \{' crates`), `docs/api/openapi.json`

**Interfaces:**
- Consumes: `string_enum!`, `label_type!`, `AccountKind`, `default_liquidity`, `AccountRecord`, `NewAccount`, `AccountChanges`.
- Produces (`coffer_domain::accounts`):
  - `AccountKind::{Education, Custodial, Charitable}`, listed after `Retirement`; `ALL: [Self; 15]`; `as_str` and `FromStr`.
  - `default_liquidity` gives `Invested` for all three.
  - `BelongsTo` (through `string_enum!`, with `ToSchema` under `openapi`): `Household => "household"`, `SomeoneElse => "someone_else"`, `GivenAway => "given_away"`. Error `AccountError::UnknownBelongsTo`.
  - `#[must_use] pub fn default_belongs_to(kind: AccountKind) -> BelongsTo`.
  - `label_type!` `Beneficiary`, `AccountError::InvalidBeneficiary`, `max = 64`. Message: `"beneficiary must be 1 to 64 characters without control characters"`.
- Produces (`coffer_store`): `AccountRecord` and `NewAccount` gain `belongs_to: BelongsTo`, `belongs_to_overridden: bool`, `beneficiary: Option<Beneficiary>`. `AccountChanges` gains `belongs_to: Option<BelongsTo>`, `belongs_to_overridden: Option<bool>`, `beneficiary: Option<Option<Beneficiary>>` (`Some(None)` clears, as `closed_at`). `columns!`, `account_from_row`, `insert_account` and `update_account` follow.
- Until Task 2, `Ledger::insert_account` passes `BelongsTo::Household`, `false`, `None`. That is why this branch isn't released alone.
- Schema `0022`, tried in this exact form (see Review Focus 1). The holding tables are how the foreign key counter gets uncounted: don't "simplify" to a rename.
  ```sql
  -- A CHECK can't change in place. With foreign keys on, DROP TABLE counts every child row
  -- as a violation, and only inserting parents into a table of the same name uncounts them:
  -- the usual rename fails at COMMIT. Deferred violations are checked at COMMIT, so a row
  -- lost here fails the whole migration. Nothing cascades from these two tables.
  PRAGMA defer_foreign_keys = ON;

  CREATE TABLE accounts_held AS SELECT * FROM accounts;
  DROP TABLE accounts;
  CREATE TABLE accounts (
      id                    TEXT PRIMARY KEY,
      name                  TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
      kind                  TEXT NOT NULL CHECK (kind IN (
          'checking', 'savings', 'credit_card', 'loan', 'mortgage', 'brokerage',
          'retirement', 'education', 'custodial', 'charitable', 'equity_comp',
          'property', 'vehicle', 'other_asset', 'other_liability')),
      -- An ISO 4217 code; never changes once the account exists.
      currency              TEXT NOT NULL CHECK (length(currency) = 3),
      liquidity             TEXT NOT NULL CHECK (liquidity IN (
          'cash', 'short_term_liability', 'invested', 'illiquid',
          'long_term_liability')),
      -- 1 once a liquidity class was chosen rather than defaulted from kind.
      liquidity_overridden  INTEGER NOT NULL CHECK (liquidity_overridden IN (0, 1)),
      include_in_net_worth  INTEGER NOT NULL CHECK (include_in_net_worth IN (0, 1)),
      include_in_forecast   INTEGER NOT NULL CHECK (include_in_forecast IN (0, 1)),
      closed_at             INTEGER,
      created_at            INTEGER NOT NULL,
      belongs_to            TEXT NOT NULL DEFAULT 'household'
          CHECK (belongs_to IN ('household', 'someone_else', 'given_away')),
      -- 1 once it was chosen rather than defaulted from kind.
      belongs_to_overridden INTEGER NOT NULL DEFAULT 0 CHECK (belongs_to_overridden IN (0, 1)),
      beneficiary           TEXT CHECK (length(beneficiary) BETWEEN 1 AND 64),
      -- Money that isn't the household's counts nowhere: spec §8.11 B4.
      CHECK (belongs_to = 'household' OR (include_in_net_worth = 0 AND include_in_forecast = 0)),
      CHECK (belongs_to <> 'household' OR beneficiary IS NULL)
  ) STRICT;
  INSERT INTO accounts (id, name, kind, currency, liquidity, liquidity_overridden,
      include_in_net_worth, include_in_forecast, closed_at, created_at)
  SELECT id, name, kind, currency, liquidity, liquidity_overridden,
      include_in_net_worth, include_in_forecast, closed_at, created_at FROM accounts_held;
  DROP TABLE accounts_held;

  CREATE TABLE provider_accounts_held AS SELECT * FROM provider_accounts;
  DROP TABLE provider_accounts;
  CREATE TABLE provider_accounts (
      -- Every column as it stands after 0013, in that order, with kind_hint's CHECK
      -- widened to the same fifteen kinds. Keep each comment and CHECK from 0006.
      ...
  ) STRICT;
  INSERT INTO provider_accounts SELECT * FROM provider_accounts_held;
  DROP TABLE provider_accounts_held;
  CREATE INDEX provider_accounts_account_id ON provider_accounts (account_id);
  ```
  The columns after `last_seen_at` are, in order: `last_applied_at INTEGER`, then `held_syncs`, `hold_released`, `released_records`, `release_withdrawn`, `held_records`, each `INTEGER NOT NULL DEFAULT 0`. Compare with `.schema provider_accounts` after 0021, and keep the table's `UNIQUE` and the balance `CHECK`.

- [ ] **Step 1: Write failing tests.**
  - `accounts.rs`: `default_liquidity_table` gains the three (`Invested`); `kinds_and_classes_roundtrip_as_snake_case` also covers `BelongsTo`; `belongs_to_defaults_from_the_kind` (`Education`, `Custodial` → `SomeoneElse`; `Charitable` → `GivenAway`; each of the other 12 → `Household`); `beneficiary_respects_its_maximum` (64 `é` fine, 65 refused, blank refused, trimmed, a control character refused, and the error's text holds none of the input).
  - `tests/it/account_belonging.rs`:
    - **`migration_keeps_every_row`**: on a file database with foreign keys on, as `Store::open` connects. Run the real `Migrator` with `migrations` cut to those before 22 (`let mut upto = sqlx::migrate!("./migrations"); upto.migrations = Cow::Owned(..)`). Seed with raw SQL: a member, a connection; accounts with each old kind, one closed, one with `include_in_net_worth = 0`, one with a chosen class; provider accounts bound and unbound, with a hint, a last balance and non-zero `held_*`; a snapshot, a kept balance, a transaction with a source and a split, a planned item hinted at an account, a daily row and a token grant. Dump every column of every one of those tables. Run the full `Migrator`. Assert: the same dumps, row for row; every account `household`, not overridden, no beneficiary; `PRAGMA foreign_key_check` empty; a snapshot for a missing account and a kept balance for a missing provider account are both still refused; `provider_accounts_account_id` exists.
    - `every_kind_passes_the_tables` (each of `AccountKind::ALL` as `kind` and as `kind_hint`: a CHECK and the enum can't drift).
    - `every_value_of_belongs_to_passes` and `an_unknown_belongs_to_is_refused`.
    - `not_the_household_can_t_be_counted` (raw SQL: `someone_else` with `include_in_net_worth = 1`, `given_away` with `include_in_forecast = 1`, and an `UPDATE` turning a flag on: each refused).
    - `the_household_has_no_beneficiary` (raw insert and update refused).
    - `belonging_roundtrips` (insert, read, change, clear with `Some(None)`).
    - `changing_belonging_writes_no_daily_row` (the table's rows, compared before and after).
- [ ] **Step 2: Run** `cargo nextest run -p coffer-domain -p coffer-store --all-features` → FAIL.
- [ ] **Step 3: Implement,** then update the test fixtures. Fix `kind_label` in `bind.rs`: `Education` → "Education savings (529)", `Custodial` → "Custodial (UTMA, UGMA)", `Charitable` → "Charitable (donor-advised fund)". Regenerate `docs/api/openapi.json`.
- [ ] **Step 4: Run** → PASS, then `scripts/gate.sh`. Then run the real server once against a copy of a database from plan 4's migrations, to see it start and read its accounts; say in the PR what it was run against.
- [ ] **Step 5: Commit** `feat(store): three more kinds, and whose money an account is`.

### Task 2: One rule for every create, bind and patch **[security]**

**Implementer rules:** files under about 800 lines of code; tests fail first; never set `CARGO_TARGET_DIR`; git only as `git -C <path>`, never `cd ... git`, `git -c` or `git tag`; scratch files in `scratchpad/plan5-task2/`; no Claude attribution, public repo.

**Files:**
- Create: `crates/domain/src/accounts/belonging.rs`, `crates/domain/src/accounts/tests.rs` if the test module needs it to stay under the limit, `crates/server/tests/it/accounts/belonging.rs`
- Modify: `crates/domain/src/accounts.rs`, `crates/server/src/ledger/accounts.rs`, `crates/server/src/ledger/net_worth.rs` (nothing but what compiles), `crates/server/src/sync/connections/mod.rs`, `crates/server/src/sync/connections/binding.rs`, `crates/server/src/http/api/accounts.rs`, `crates/server/src/http/api/provider_accounts.rs`, `crates/server/src/http/pages/connections/bind.rs` (the two literal flags), `crates/server/tests/it/accounts/mod.rs`, every test building `NewAccountInput` or `NewLinkedAccountInput` (about 20 files; `rg 'NewAccountInput \{|NewLinkedAccountInput \{' crates`), `docs/api/openapi.json`

**Interfaces:**
- Consumes (Task 1): `BelongsTo`, `Beneficiary`, `default_belongs_to`, the store's fields. Existing: `authorize_account`, `AuditChanges`, `ValidNewAccount`, `Ledger::insert_account`.
- Produces (`coffer_domain::accounts::belonging`), pure:
  ```rust
  /// What an account says about whose money it is, and what follows from that.
  pub struct Belonging {
      pub belongs_to: BelongsTo,
      pub overridden: bool,
      pub beneficiary: Option<Beneficiary>,
      pub include_in_net_worth: bool,
      pub include_in_forecast: bool,
  }
  pub enum BeneficiaryChange { Keep, Clear, Set(Beneficiary) }
  pub struct BelongingAsked {
      pub belongs_to: Option<BelongsTo>,
      pub beneficiary: BeneficiaryChange,
      pub include_in_net_worth: Option<bool>,
      pub include_in_forecast: Option<bool>,
  }
  /// The one rule, for an account being created (`current` is `None`) or changed (spec §8.11 B4).
  pub fn resolve_belonging(
      current: Option<&Belonging>,
      kind: AccountKind,
      asked: &BelongingAsked,
  ) -> Result<Belonging, AccountError>;
  ```
  The rule:
  - `belongs_to` is what is asked, else the current one if it was chosen, else `default_belongs_to(kind)`. `overridden` is whether it was asked now or chosen before. Asking for the kind's own default still counts as choosing.
  - Not `household`: both flags false. Either asked as `Some(true)` → `AccountError::CountedButNotTheHouseholds`. `Set` takes the label, `Clear` drops it, `Keep` keeps it.
  - `household`: `Set` → `AccountError::BeneficiaryOnTheHousehold`; `beneficiary` is `None`. A flag is what is asked, else, when coming from an account that was `household`, what it was, else `true`.
  - Fixed messages, no input: `"an account that isn't the household's can't be counted: set belongs_to to household first"`, `"only an account that isn't the household's has a beneficiary"`.
- `NewAccountInput` and `NewLinkedAccountInput`: `include_in_net_worth` and `include_in_forecast` become `Option<bool>`; both gain `belongs_to: Option<BelongsTo>`, `beneficiary: Option<String>`. `insert_account` resolves through `resolve_belonging`, so a bind and a create share it, after the hint has decided the kind.
- `AccountPatch` gains `belongs_to: Option<BelongsTo>` and `beneficiary: Option<String>`, documented: `""` removes it, since absent keeps it. After trimming, empty is `Clear`, otherwise `Beneficiary::parse`. `update_account` resolves with `kind` as it will be, and adds to the audit only what changed: `belongs_to`, `belongs_to_overridden`, `beneficiary`, and each flag. A patch that changes nothing writes nothing.
- `AccountView` gains `belongs_to: BelongsTo` and `beneficiary: Option<String>` (`#[schema(required)]`).
- API: `NewAccountRequest` and `NewLinkedAccountRequest` gain both fields and stop defaulting flags at the handler: the doc reads `Count the account in net worth (default true, and always false for an account that isn't the household's)`. `""` for `beneficiary` on a create is the same as absent. `updateAccount`'s description says what a new `kind` now moves.
- `bind.rs` passes `None` for both flags and both new fields. Its form changes in Task 5.
- Refusals are `LedgerError::Invalid` / `SyncError::Invalid`, a 422.

- [ ] **Step 1: Write failing tests.**
  - `belonging.rs`: `a_new_account_follows_its_kind`; `each_kind_defaults_out_of_both_flags`; `asking_to_count_money_that_isn_t_the_household_s_is_refused`; `asking_not_to_count_it_is_fine`; `a_chosen_household_stays_over_a_change_of_kind` and counts; `a_defaulted_one_follows_a_change_of_kind` (both directions, with the flags: `other_asset` with flags on → `education`: `someone_else`, both off; → `brokerage`: `household`, both on); `moving_away_turns_both_off`; `moving_back_turns_on_what_isn_t_named` (and one named `false` stays off); `a_household_account_keeps_its_flag_over_a_change_of_kind` (`include_in_net_worth: false`, `checking` → `savings`: still false); `a_beneficiary_on_the_household_is_refused`; `moving_back_clears_the_beneficiary`; `moving_between_the_two_keeps_the_label`.
  - Proptest **`resolved_belonging_always_passes_the_table`**: arbitrary current, kind and asked. `Ok` satisfies both CHECKs of `0022`; resolving a second time with nothing asked and the same kind changes nothing.
  - `tests/it/accounts/belonging.rs`, through the real router:
    - `creating_a_529_defaults_it_out` (`POST /accounts` with `kind: education`: answer and store agree: `someone_else`, `invested`, both flags false); one each for `custodial` and `charitable` (`given_away`).
    - `asking_to_count_it_is_422` (`include_in_net_worth: true`; nothing created, and the message is the fixed one).
    - `a_brokerage_held_for_a_child` (`kind: brokerage, belongs_to: someone_else, beneficiary: "Child A"`: flags off, `belongs_to` chosen, kind stays).
    - `a_529_counted_as_the_household_s` (`kind: education, belongs_to: household`: both flags on, liquidity still `invested`).
    - `patching_the_kind_moves_it_out_and_audits` (`other_asset` → `education`: audit holds `kind`, `liquidity`, `belongs_to`, `include_in_net_worth`, `include_in_forecast`, each before and after).
    - `patching_a_flag_on_while_held_is_422`; `patching_to_the_household_and_a_flag_together_works`.
    - `an_empty_beneficiary_removes_it`; `an_absent_one_keeps_it`; `a_bad_one_is_422_without_echo`.
    - **`a_page_bind_of_a_529_stays_out`** (the page's form with `kind=education`: no CHECK failure, `someone_else`, both off); **`an_api_bind_with_no_flags_stays_out`**; `an_api_bind_of_a_checking_account_still_counts` (`include_*` both true, as before).
    - `a_bound_529_brings_its_kept_balances` (kept days, a bind, then the balances exist and the audit says how many: nothing lost by binding at last).
    - `a_token_reads_it_at_balances_and_changes_it_at_write` (`read` token with a `balances` grant sees `belongs_to` and `beneficiary`; a `PATCH` with `balances` is 403; a hidden account is 404).
    - **`a_beneficiary_is_never_logged_or_echoed`** (`capture_logs`, `log_marker`; create, patch, bind and a refusal with `Child A` plus the marker: `captured_log_text` holds neither, and no problem body does).
- [ ] **Step 2: Run** `cargo nextest run -p coffer-domain -p coffer-server --all-features -E 'test(belonging)'` → FAIL.
- [ ] **Step 3: Implement.** Regenerate `docs/api/openapi.json`. Keep `docs/agent-usage.md`'s account section in step with it (the kinds, the fields, the defaults); Task 9 finishes the rest.
- [ ] **Step 4: Run** → PASS, then `scripts/gate.sh`; security review of the three paths in.
- [ ] **Step 5: Commit** `feat(api): whose money an account is, decided in one place`.

### Task 3: Buckets in the net worth report **[security]**

**Implementer rules:** files under about 800 lines of code; tests fail first; never set `CARGO_TARGET_DIR`; git only as `git -C <path>`, never `cd ... git`, `git -c` or `git tag`; scratch files in `scratchpad/plan5-task3/`; no Claude attribution, public repo.

**Files:**
- Create: `crates/domain/src/net_worth/tests/buckets.rs`, `crates/server/tests/it/net_worth/buckets.rs`
- Modify: `crates/domain/src/net_worth/types.rs`, `crates/domain/src/net_worth/report.rs` (480 lines of code: if a section's extraction takes it near the limit, move the cautions to `net_worth/cautions.rs` in this change), `crates/domain/src/net_worth/tests.rs`, `crates/domain/src/net_worth/tests/properties.rs`, `crates/store/src/net_worth/reads.rs`, `crates/store/tests/it/net_worth/reads.rs`, `crates/server/src/ledger/net_worth.rs`, `crates/server/src/http/api/reports.rs`, `crates/server/tests/it/net_worth/mod.rs`, `docs/api/openapi.json`

**Interfaces:**
- Consumes (Tasks 1, 2): `BelongsTo`, `AccountRecord.belongs_to`, `.beneficiary`. Existing: `net_worth_report`, `gather`, `point`, `series`, `blockers`, `cautions`, `accounts_in_scope`.
- `ReportAccount` gains `belongs_to: BelongsTo` and `beneficiary: Option<String>`.
- `report.rs`: the current body becomes `fn section(input, members: &[&ReportAccount], through, periods) -> Section { points, accounts, blockers, cautions, status }`, where an account's position is its position in `members`. For the household, `members` holds what is `household`, and one is included when its flag says so, as now. For a bucket, `members` holds that bucket's and every one is included. `net_worth_report` builds the household's, then one per bucket. While a rebuild is needed: no points anywhere, `buckets` empty, and the blocker as today.
- Produces:
  ```rust
  pub struct NetWorthReport {
      // ... as now, about the household's money alone ...
      /// Money that isn't the household's, in no number above and added to nothing.
      pub buckets: Vec<NetWorthBucket>,
  }
  pub struct NetWorthBucket {
      pub belongs_to: BelongsTo,
      /// `null` when no name was given.
      #[schema(required)] pub beneficiary: Option<String>,
      pub status: NetWorthStatus,
      pub points: Vec<NetWorthPoint>,
      pub accounts: Vec<AccountSeries>,
      pub blockers: Vec<NetWorthBlocker>,
      pub cautions: Vec<NetWorthCaution>,
  }
  ```
  - Order: `someone_else` before `given_away`; within one, no label first, then labels by bytes. Nothing about the order depends on an account.
  - `accounts` at the top level changes meaning to "every visible account that is the household's". `AccountSeries` and `NotCountedReason` don't change: `excluded_by_flag` stays the only reason, and a bucket's account never needs one.
  - A bucket's `status`, `blockers` and `cautions` come from its own section; `no_counted_accounts` there means nothing in that bucket has a value in this range.
- `store/net_worth/reads.rs`: `included` becomes `one.include_in_net_worth || one.belongs_to != BelongsTo::Household`, so a bucket has its rows.
- `ledger/net_worth.rs`: `report_account` fills both fields. `coverage` already covers every visible account.
- `reports.rs`: the description says what `buckets` is and that nothing adds it to `total`.

- [ ] **Step 1: Write failing tests.**
  - `net_worth/tests/buckets.rs`:
    - **`golden_week_with_buckets`**. Plan 4's golden week unchanged (`golden_week_of_net_worth`'s accounts, rates and dates), plus: *College fund*, USD, `education`, `someone_else`, `Child A`, 20,000.00 on 10-26; *UTMA*, EUR, `custodial`, `someone_else`, `Child A`, 1,000.00 on 10-26; *Giving*, USD, `charitable`, `given_away`, no label, 5,000.00 on 10-28. Household `total` by day exactly as plan 4: 611,880; 611,500; 581,330; 581,300; 631,720; 631,720; 631,720, `Firm`. `buckets` is exactly two. First, `someone_else`, `Child A`, by day: 2,111,880; 2,111,500; 2,111,330; 2,111,300; 2,111,720; 2,111,720; 2,111,720, `by_currency` USD 2,000,000 and EUR 100,000, UTMA's `rate_on` 10-30 on 10-31. Second, `given_away`, `null`: five points from 10-28, each 500,000. Recompute these by hand; if one looks wrong, stop and say so rather than changing it.
    - `a_bucket_s_missing_rate_leaves_the_household_firm` (a THB bucket account with no rate: the bucket `Provisional`, `RateMissing`, its `total` null; household `Firm`, same numbers).
    - `a_bucket_s_old_balance_is_its_own` (`BalanceStale` in the bucket only). `a_bucket_s_positive_liability_is_its_own`.
    - `a_closed_bucket_account_follows_n9` (counts until its closing day; a negative balance goes on with `ClosedAccountOwing`).
    - `a_household_with_only_buckets_is_blocked_but_they_answer` (`NoCountedAccounts`, `Unavailable`, no points; the bucket `Firm` with its own).
    - `display_currency_not_set_keeps_their_sums`; `rebuild_needed_answers_no_bucket`.
    - `two_spellings_are_two_buckets` (`Child A`, `child a`); `no_label_comes_first`; `the_order_is_fixed` (shuffled input, same answer).
    - `an_excluded_household_account_stays_at_the_top` (`include_in_net_worth: false`, `household`: `excluded_by_flag` at the top level, in no bucket).
    - `moving_an_account_changes_its_whole_history` (same rows, `belongs_to` changed: out of every past household point, in every past bucket point).
    - `account_points_false_keeps_bucket_summaries`.
  - `properties.rs`: **`buckets_never_touch_the_household_answer`**: arbitrary household accounts, rows and bucket accounts. The report with the bucket accounts in `accounts` and `rows`, with `buckets` cleared, equals the report without them, field for field. Also `a_bucket_explains_itself` (`total_equals_the_sum_of_its_accounts` and `status_matches_lists` within each).
  - `store/tests/it/net_worth/reads.rs`: **`a_bucket_has_its_rows`** (rows for a `someone_else` account though its flag is false); `excluded_accounts_have_no_rows_but_are_listed` still passes; `scope_filters_buckets_in_the_query` (`Only` holding one of a bucket's two accounts: only that one's rows).
  - `server/tests/it/net_worth/buckets.rs`, through the API: `a_bound_529_reaches_its_bucket_not_net_worth` (provider account, kept balances, a bind: the bucket holds the kept days, `first_on` is the earliest, household `total` unchanged at every point); `the_answer_has_the_documented_shape` (`buckets` present and `[]` on a household with none; a bucket's every key); `changing_belongs_to_writes_no_row_and_moves_the_answer`; `a_bucket_in_another_currency_converts` (N7: its rates are fetched and it answers in the display currency).
- [ ] **Step 2: Run** `cargo nextest run -p coffer-domain -p coffer-store -p coffer-server --all-features -E 'test(bucket)'` → FAIL.
- [ ] **Step 3: Implement.** Regenerate `docs/api/openapi.json`.
- [ ] **Step 4: Run** → PASS, and every existing net worth test unchanged. Then `scripts/gate.sh`; security review.
- [ ] **Step 5: Commit** `feat(domain): buckets beside net worth, added to nothing`.

### Task 4: Forecast and plan versus actual

**Implementer rules:** files under about 800 lines of code; tests fail first; never set `CARGO_TARGET_DIR`; git only as `git -C <path>`, never `cd ... git`, `git -c` or `git tag`; scratch files in `scratchpad/plan5-task4/`; no Claude attribution, public repo.

**Files:**
- Create: `crates/server/tests/it/forecast/belonging.rs`, `crates/server/tests/it/plan_vs_actual/belonging.rs`
- Modify: `crates/domain/src/forecast.rs` (690 lines of code, the change is small), `crates/domain/src/forecast/types.rs`, `crates/domain/src/forecast/tests.rs`, `crates/domain/src/plan_report.rs`, `crates/domain/src/plan_report/tests.rs`, `crates/store/src/forecast.rs`, `crates/server/src/ledger/forecast.rs`, `crates/server/src/ledger/plan_report.rs`, `crates/server/tests/it/forecast/mod.rs`, `crates/server/tests/it/plan_vs_actual.rs` (the `mod` line only), `docs/api/openapi.json`

**Interfaces:**
- Consumes (Task 1): `BelongsTo`, `AccountRecord.belongs_to`. Existing: `not_counted`, `REPORT_ACTUALS`, `REPORT_UNCATEGORIZED`, `PlanReportInput`.
- Forecast: `AccountInput` gains `belongs_to: BelongsTo`. `NotCountedReason::NotHousehold` (`"not_household"`), checked first in `not_counted`, so a closed custodial card reads the same and a held `cash` account doesn't read as `excluded_by_flag`, whose advice is to set a flag the API would refuse. The money doesn't move: the flag was false already, and `CATEGORY_SPENT`, `LOOSE_ENDS` and `NEWER_SPENDING` read that flag, unchanged.
- `REPORT_ACTUALS` joins `accounts a` and adds `a.belongs_to = 'household'`. `REPORT_UNCATEGORIZED` adds the same. `GOAL_SPENT` and `REPORT_GOAL_SPENT` don't change (B10).
- New query `REPORT_NOT_HOUSEHOLD`: posted, non-goal-linked splits in `expense` and `income` categories on accounts that aren't the household's, `SUM` by month and currency, over the reported months. Flags are ignored: it says what is left out, not what it meant.
- `coffer_domain::plan_report`: `MonthNotHousehold { month, currency, net_minor }` on `PlanReportInput`. Output `PlanReport.not_the_households: Vec<NotHouseholdRow { month: CalendarMonth, net: Money }>`, ordered by month and currency, only months reported, documented: "Categorized money on accounts that aren't the household's. Never in `actual`." Past `i64` it refuses as any other total.
- `docs/agent-forecast.md` line 378's list gains `not_household`, in this task.

- [ ] **Step 1: Write failing tests.**
  - `forecast/tests.rs`: `not_household_is_the_reason_whatever_else_holds` (`cash`, `invested`, closed, each with `SomeoneElse`: `NotHousehold`, not counted); `a_household_account_keeps_its_reasons`.
  - `forecast/belonging.rs`, through the API and the store's real queries:
    - `a_custodial_account_doesn_t_move_the_number` (same household with and without it: same `surplus`, its line `counted: false`, `not_household`).
    - `a_held_cash_account_stays_out` (`kind: savings`, `belongs_to: someone_else`, class `cash`: its balance, its spending and its uncategorized money in nothing, and no `balance_missing` when it has no balance).
    - `a_bucket_in_another_currency_raises_no_mixed_currencies`.
    - `binding_a_529_lifts_the_unbound_caution` (an unbound provider account makes the forecast `provisional`; after binding as `education`, that caution is gone and the number is the same): this is what lets the owner bind at last.
    - **`a_contribution_is_money_leaving`**: checking at 3,000.00, floor 0; planned monthly bill *College fund contribution*, 500.00, `expense` category, hinted at checking, due the 5th, `as_of` the 10th. Before the charge: cash 3,000.00 and a gap of 500.00. After a posted −500.00 split in that category, with a balance dated after it at 2,500.00: cash 2,500.00, no gap. The surplus is the same both times: the 500.00 is held as a gap, then gone from the balance, never both, and it is 500.00 lower than the same household with no such bill.
    - `a_contribution_in_a_transfer_category_stays_unpaid` (same, category `transfer`: the bill stays unpaid with its caution, the safe direction, and cash still falls). Pins B9's reading.
    - `a_planned_transfer_is_refused` (R17, already true: a contribution can't be planned as one).
    - `a_goal_paid_from_a_529_lowers_the_need` (an `expense` split on the `education` account linked to a goal: the goal's remaining need falls; R10's balance date doesn't apply).
    - `a_bill_paid_from_a_bucket_stays_unpaid_until_settled` (split on the `education` account in the bill's category: unpaid; settled per R9a: gone).
    - `a_hinted_contribution_still_counts_with_a_caution` (hinted at the `custodial` account: R16's caution, and counted).
  - `plan_vs_actual/belonging.rs`:
    - `a_contribution_reads_as_actual` (−500.00 on checking in an `expense` category: `actual` 500.00).
    - **`a_grant_from_a_fund_is_not_spending_twice`** (−500.00 contribution on checking, −300.00 grant on the `charitable` account, both in *Giving*: `actual` 500.00; `not_the_households` says −300.00 for that month).
    - `an_income_split_in_a_bucket_is_left_out` and says so.
    - `an_excluded_household_account_keeps_counting` (`include_in_net_worth: false`, `household`: as R6 says, still in `actual`).
    - **`uncategorized_money_in_a_held_savings_account_is_left_out`** (class `cash`, `someone_else`).
    - `transfers_inside_a_bucket_say_nothing` (`transfer` category: in nothing).
    - `a_goal_split_in_a_bucket_stays_on_its_line`.
    - `months_outside_the_range_are_left_out` (rolling averages read further back; `not_the_households` doesn't).
    - `moving_an_account_moves_its_past` (R21's "no history": change `belongs_to`, past `actual` changes).
- [ ] **Step 2: Run** `cargo nextest run -p coffer-domain -p coffer-store -p coffer-server --all-features -E 'test(belonging) | test(not_household)'` → FAIL.
- [ ] **Step 3: Implement.** Regenerate `docs/api/openapi.json`.
- [ ] **Step 4: Run** → PASS, then `scripts/gate.sh`.
- [ ] **Step 5: Commit** `feat(forecast): money that isn't the household's stays out of plans`.

### Task 5: Saying whose money it is on the Connections page **[security]**

**Implementer rules:** files under about 800 lines of code; tests fail first; never set `CARGO_TARGET_DIR`; git only as `git -C <path>`, never `cd ... git`, `git -c` or `git tag`; scratch files in `scratchpad/plan5-task5/`; no Claude attribution, public repo.

**Files:**
- Create: `crates/server/src/http/pages/connections/belonging.rs`, `crates/server/tests/it/connections_page/belonging.rs`
- Modify: `crates/server/src/http/pages/connections/bind.rs`, `crates/server/src/http/pages/connections/mod.rs` (746 lines of code: a route, a call in `account_item` and the accounts it reads; if that nears the limit, move `account_item` out in this change), `crates/server/tests/it/connections_page.rs`

**Interfaces:**
- Consumes (Task 2): `NewLinkedAccountInput`, `AccountPatch`, `Ledger::update_account`, `AccountView.belongs_to`, `.beneficiary`. Existing: `Binding`, `csrf_field`, `PageForm`, `refused`, `kind_label`.
- Bind form gains, under Kind, a `Whose money is it?` select named `belongs_to`: `""` "Follow the kind", then "The household's", "Someone else's, held for them", "Given away"; and a `Held for (optional)` input named `beneficiary`, `maxlength=64`, `list="beneficiaries"`. `CreateForm` gains both, `deny_unknown_fields` staying. An unknown value is a 422 notice with fixed words, as `KIND_UNKNOWN`.
- The fine print under Kind becomes: "Checking and savings count as cash for planning. Education and custodial accounts are held for someone else, and charitable ones are given away: they count in neither net worth nor planning unless you say they're the household's."
- `<datalist id="beneficiaries">` rendered once on the page, from the labels of `list_accounts`, which is what the actor sees. No script.
- `belonging.rs`: `POST /connections/accounts/{account_id}/belonging`, CSRF first, fields `kind`, `belongs_to` (`""` leaves it), `beneficiary` (blank removes it). Through `Ledger::update_account`, nothing of its own, so hidden is 404 and a low level is 403 as anywhere. A `details` on a bound row, offered only with write access to that account (`authorize_account`, as `Binding::load` does), showing the current kind and whose money it is. `account_item` needs the `AccountView`, so `account_names` becomes a map to the view.
- Words: the row's meta line says "Held for Child A", "Held for someone else" or "Given away" when it isn't the household's.
- Manual accounts have no page anywhere (#49), so the API stays their way; say that in the PR.

- [ ] **Step 1: Write failing tests** (`connections_page/belonging.rs`, with `Browser`): `binding_a_529_from_the_page` (kind `education`, nothing else: `someone_else`, flags off); `binding_with_a_name` (`beneficiary=Child A`: in the view and on the row); `binding_a_brokerage_for_a_child` (`belongs_to=someone_else`); `saying_it_is_the_household_s_counts_it`; `an_unknown_belongs_to_is_a_notice_and_changes_nothing`; `a_blank_name_is_none`; `a_too_long_name_is_a_notice_without_echo`; `forged_form_fails_whole` (an extra field); `csrf_is_required`; `the_row_form_changes_kind_and_belonging` (audited, as `account.update`); `the_row_form_removes_a_name`; `the_row_form_is_only_for_who_may_write` (a member sees it; a read-only role doesn't and is refused when posting anyway); `names_are_escaped`; `the_datalist_holds_only_labels_of_visible_accounts`; `page_has_the_default_csp_and_no_inline_script_or_style` via `assert_inline_free`.
- [ ] **Step 2: Run** `cargo nextest run -p coffer-server --all-features -E 'test(connections_page::belonging)'` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Look at it.** A real server, a real browser at 1280 px and 375 px, a long label, both schemes, and the console (no CSP violation). Read `assets/js/coffer.js`, which `/connections` loads and whose reports are logged because the page is in `PAGES`: no form value may reach a message. Put what was checked in the PR.
- [ ] **Step 5: Run** → PASS, then `scripts/gate.sh`; security review (a new write form).
- [ ] **Step 6: Commit** `feat(pages): say whose money an account is when binding it`.

### Task 6: Buckets on the net worth page

**Implementer rules:** files under about 800 lines of code; tests fail first; never set `CARGO_TARGET_DIR`; git only as `git -C <path>`, never `cd ... git`, `git -c` or `git tag`; scratch files in `scratchpad/plan5-task6/`; no Claude attribution, public repo.

**Files:**
- Create: `crates/server/src/http/pages/net_worth/buckets.rs`, `crates/server/tests/it/pages/net_worth/buckets.rs`
- Modify: `crates/server/src/http/pages/net_worth.rs` (650 lines of code: one call, and `pub(super)` on what it reuses), `crates/server/src/http/pages/net_worth/words.rs`, `crates/server/tests/it/pages/net_worth.rs`, `crates/server/assets/css/base.css` only if an existing class won't do

**Interfaces:**
- Consumes (Task 3): `NetWorthReport.buckets`, `NetWorthBucket`. Existing: `Names::of_accounts`, `status`, `blocker_sentence`, `caution_sentence`, `no_amount_sentence`, `accounts`, `account_cells`, `grouped`, `NO_TOTAL`.
- `buckets::section(report) -> Markup`, after `accounts`, nothing when there are none. A `section.card id="held"`:
  - `h2` "Not part of your net worth", and one line: "Held for someone else or given away. None of this is in the number above, and it isn't added to it."
  - For each bucket, in the report's order, an `h3`: "Held for Child A", "Held for someone else" with no label, "Given away" or "Given away: Family fund". The latest point's total with its day, `NO_TOTAL` when null, and the status in words only when it isn't firm. Its own reasons, using a `Names` of its own accounts. Then the accounts table through the same cells as `accounts`.
  - A `details` "Every value, as a table", through the same periods as `history`. No chart: one line on the page stays net worth, and a chart per bucket would invite adding them by eye.
- No total across buckets, on any path. No form: N13 stands.
- `words.rs`'s exhaustive matches gain nothing, since no enum changed. `bucket_heading(belongs_to, beneficiary) -> String` lives here, exhaustive over `BelongsTo`; `Household` can't reach it, and a report's type doesn't rule it out, so it reads "The household's" rather than panicking.

- [ ] **Step 1: Write failing tests** (`pages/net_worth/buckets.rs`): `page_matches_the_api_on_the_golden_week` (the Task 3 golden: hero 6,317.20; `Child A` 21,117.20; `Given away` 5,000.00, and the page's numbers are the API's); `no_number_on_the_page_adds_them` (no 6,317.20 + anything: every grouped figure on the page is one the report holds); `the_hero_is_the_household_s` with only buckets (`No number yet`, and the buckets shown); `a_null_bucket_total_says_why` (missing rate: its reasons name the account, the household's remain `Firm`); `no_buckets_no_section`; `names_are_escaped` (a label with markup); `a_token_can_t_read_the_page`; `page_offers_no_new_write_action`; `page_has_the_default_csp_and_no_inline_script_or_style`; `browser_reports_withhold_this_page` still holds; `a_session_sees_every_bucket`.
- [ ] **Step 2: Run** `cargo nextest run -p coffer-server --all-features -E 'test(pages::net_worth::buckets)'` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Look at it.** A real server with a household holding two named buckets, one given away, one with a missing rate, a long label. A real browser at 1280 px and 375 px, on both themes in light and dark: wrapping, the tables' `data-label`s, and the console. Put what was checked in the PR.
- [ ] **Step 5: Run** → PASS, then `scripts/gate.sh`.
- [ ] **Step 6: Commit** `feat(pages): show what isn't part of net worth`.

### Task 7: Suggesting a kind when binding (#39) **[security]**

**Implementer rules:** files under about 800 lines of code; tests fail first; never set `CARGO_TARGET_DIR`; git only as `git -C <path>`, never `cd ... git`, `git -c` or `git tag`; scratch files in `scratchpad/plan5-task7/`; no Claude attribution, public repo.

**Files:**
- Create: `crates/domain/src/accounts/suggest.rs`, `crates/server/src/sync/plaid/kinds.rs` (`kind_hint` and its `kind_hints` test, moved: `provider.rs` is at 795 lines of code), `crates/server/tests/it/connections_page/suggestion.rs`
- Modify: `crates/domain/src/accounts.rs`, `crates/server/src/sync/plaid/provider.rs`, `crates/server/src/sync/plaid/mod.rs`, `crates/server/src/sync/connections/mod.rs` (`ProviderAccountView`), `crates/server/src/http/pages/connections/bind.rs`, `crates/server/src/http/api/provider_accounts.rs` (descriptions), `crates/server/tests/it/connections_page.rs`, `crates/server/tests/it/connections_api.rs` (one test; 671 lines of code), `docs/api/openapi.json`

**Interfaces:**
- Consumes (Task 1): the kinds; (Task 5): the form. Existing: `ProviderAccountRecord`, `create_account_and_bind`.
- Produces: `#[must_use] pub fn suggest_kind(name: &str) -> Option<AccountKind>`, pure:
  - Words are maximal runs of ASCII letters and digits, lowercased. A term is one word, or adjacent words.
  - **Specific:** `529`, `coverdell`, `college savings`, `education savings` → `Education`; `utma`, `ugma`, `custodial` → `Custodial`; `daf`, `donor advised`, `charitable` → `Charitable`; `ira`, `roth`, `401k`, `401 k`, `403b`, `457b`, `pension`, `retirement` → `Retirement`; `rsu`, `espp`, `stock plan` → `EquityComp`; `mortgage`, `heloc` → `Mortgage`; `credit card` → `CreditCard`.
  - **Generic:** `checking`, `chequing` → `Checking`; `savings`, `hysa` → `Savings`; `loan` → `Loan`; `brokerage` → `Brokerage`; `visa`, `mastercard`, `amex` → `CreditCard`.
  - One specific kind matching wins. Two specific kinds: `None`. No specific: one generic kind, or `None`.
  - Reads no clock, logs nothing, stores nothing.
- `ProviderAccountView` gains `suggested_kind: Option<AccountKind>` (`#[schema(required)]`): `suggest_kind(&record.name)` when unbound and with no `kind_hint`, else `None`. Documented: "A guess from the provider's name. Never applied: send `kind`."
- `create_account_and_bind` doesn't change, and that is the point: `input.kind.or(current.kind_hint)` stays the only fallback, and a test holds it.
- `plaid/kinds.rs`: `("investment", Some("529" | "education savings account")) => Education`, `("investment", Some("ugma" | "utma")) => Custodial`. Plaid has no subtype for a donor-advised fund, so that stays to the name.
- Page, per the owner's decision (recommended: preselect): a `kind_hint` preselects, as now. Otherwise a suggestion preselects too, and a sentence under the select says: "Suggested from the account's name, so check it: the kind decides what counts as cash." Submitting is the member's confirmation, of a value they can see. With neither, "Choose a kind" as now. The posted `kind` is what a person chose; a page's empty `kind` is never filled from the name. If the owner answers "don't preselect": the select stays on "Choose a kind" and the sentence names the suggestion.

- [ ] **Step 1: Write failing tests.**
  - `suggest.rs`, table `suggestions`: `Everyday Checking` → `Checking`; `High Yield Savings` → `Savings`; `My 529 Plan` → `Education`; `College Savings 529` → `Education`; `College Savings` → `Education`; `UTMA Brokerage` → `Custodial`; `Roth IRA` → `Retirement`; `IRA Brokerage` → `Retirement`; `401(k)` → `Retirement`; `Donor-Advised Fund` → `Charitable`; `Visa Signature` → `CreditCard`; `Visa Checking` → `None`; `Education First Checking` → `Checking`; `Savings and Checking` → `None`; `Roth 529` → `None`; `Loan Savings` → `None`; empty and punctuation → `None`; non-ASCII → `None`, no panic.
  - **`digits_inside_a_longer_word_do_not_match`** (`Checking x5291` → `Checking`; `Acct 15290` → `None`; `529x` → `None`). `case_does_not_matter`. Proptest `suggest_never_panics`, and `a_suggestion_is_deterministic`.
  - `plaid/kinds.rs`: `kind_hints` gains `529`, `education savings account`, `utma`, `ugma`, and `("investment", Some("hsa"), None)` stays.
  - `connections_page/suggestion.rs`: `the_form_preselects_a_suggestion_and_says_so`; `a_provider_hint_wins_over_the_name` (Plaid `checking`, name `529 Plan`: `checking`, no sentence); `no_suggestion_keeps_choose_a_kind`; `an_empty_kind_is_not_filled_from_the_name` (an empty post on a name that suggests one: the existing `KIND_REQUIRED` notice, no account); `the_kind_posted_is_the_kind_kept` (suggestion `Education`, posted `brokerage`: `brokerage`); `a_suggested_529_binds_out_of_net_worth`.
  - `connections_api.rs`: **`no_kind_is_refused_even_with_a_suggestion`** (a name that suggests a kind, no hint, no `kind`: 422, no account, no bind); `suggested_kind_is_in_the_view_only_while_unbound`; `a_plaid_529_hint_binds_with_no_kind_and_stays_out`.
  - **`a_suggestion_is_neither_logged_nor_audited`** (`capture_logs` with a name holding `log_marker`: listing, the page and a bind: `captured_log_text` doesn't hold it, and the `account.create` audit has `kind` and no suggestion field).
- [ ] **Step 2: Run** `cargo nextest run -p coffer-domain -p coffer-server --all-features -E 'test(suggest) | test(kind_hints)'` → FAIL.
- [ ] **Step 3: Implement.** Regenerate `docs/api/openapi.json`. `docs/agent-usage.md`: the field, that it's a guess, and that an agent must ask a member or say what it chose.
- [ ] **Step 4: Run** → PASS, then `scripts/gate.sh`; security review, as AGENTS.md requires for provider code.
- [ ] **Step 5: Commit** `feat(sync): suggest a kind from the account's name, for a member to confirm`.

### Task 8: Adversarial proof that a bucket shows only what a token can see **[security]**

**Implementer rules:** files under about 800 lines of code; tests fail first; never set `CARGO_TARGET_DIR`; git only as `git -C <path>`, never `cd ... git`, `git -c` or `git tag`; scratch files in `scratchpad/plan5-task8/`; no Claude attribution, public repo.

Give this to a fresh agent with only §8.2, §8.10 N2, §8.11 B6 and B7, the `policy.rs` rule table and this task, so it attacks the code instead of confirming it. Tests only, unless one fails; a failure is fixed in the service that leaked, in this PR.

**Files:**
- Create: `crates/server/tests/it/grants/bucket_channels.rs`
- Modify: `crates/server/tests/it/grants/mod.rs`, `crates/server/tests/it/grants/proptests.rs` (`Household`)

**Interfaces:**
- Consumes: the API of Tasks 2 and 3; existing `world()`, `grant`, `on`, `Household`, `read_uris`, `check_no_body_reveals`.
- Produces: no new interface.

- [ ] **Step 1: Extend `Household`** so a hidden account may be a bucket account with a distinctive label and balance in a currency no visible account uses, with rates stored. `no_endpoint_leaks_hidden_accounts` then also asserts no body holds that label.
- [ ] **Step 2: Add tests** (`bucket_channels.rs`), each comparing a token's answer with one from a **second world that really lacks the hidden account**, same clock and settings, byte for byte: `a_hidden_bucket_changes_no_byte` (its label, balance, currency, closing and age); `a_token_holding_one_of_two_sees_a_bucket_of_one` (the sum is the visible one's, the answer identical to a world with only that one); `a_hidden_account_doesn_t_change_a_bucket_s_position`; `a_hidden_bucket_s_currency_raises_no_caution`; `a_token_seeing_only_a_bucket_reads_like_a_household_with_only_that`; `balances_level_is_enough_and_shows_no_transaction_data`; `a_grant_change_applies_at_the_next_request`; `a_new_bucket_account_needs_an_all_grant_to_show`; `a_session_sees_every_bucket` (uniform access, for contrast); `a_forecast_stays_all_or_nothing` (R2: still 403 for a token with grants); `plan_vs_actual_stays_all_or_nothing` (`not_the_households` included); `a_patch_at_balances_is_refused_alike` (403 on a visible account; 404 on a hidden one, byte-identical to a missing one); `a_label_lists_nowhere_else` (`GET /accounts`, `/connections` and the audit log at each level: only with its account, or `all` at `transactions`); `no_problem_body_names_a_label`.
- [ ] **Step 3: Run** `cargo nextest run -p coffer-server --all-features -E 'test(grants)'`. Any failure is a leak: fix it in the service and keep the test. A needed `.config/nextest.toml` override is measured, per AGENTS.md.
- [ ] **Step 4: Run** `scripts/gate.sh`; security review.
- [ ] **Step 5: Commit** `test(grants): a bucket shows a token only its own accounts`.

### Task 9: Docs and whole-branch review **[security]**

**Implementer rules:** files under about 800 lines of code; tests fail first; never set `CARGO_TARGET_DIR`; git only as `git -C <path>`, never `cd ... git`, `git -c` or `git tag`; scratch files in `scratchpad/plan5-task9/`; no Claude attribution, public repo.

**Files:**
- Modify: `docs/agent-usage.md`, `docs/agent-net-worth.md`, `docs/agent-forecast.md`, `docs/threat-model.md`, `README.md` (only if its status lines name accounts), `AGENTS.md`, `crates/server/tests/it/net_worth/documented.rs`

**Interfaces:** none.

- [ ] **Step 1: `docs/agent-usage.md`, Accounts** (lines 182 to 189 and the *provider accounts* section). All fifteen kinds. `belongs_to`, what each value means and what it isn't (not a legal statement). Which kinds default out, and that a member says otherwise only through `belongs_to`. `beneficiary`, and `""` removing it. Flags: `true` on money that isn't the household's is 422. A new `kind` moves class and `belongs_to` unless chosen. **Never set `belongs_to` from an account's name alone**: ask.
- [ ] **Step 2: `docs/agent-usage.md`, categorizing** (B9, B10, in plain rules with an example in generic names): a contribution to a 529, a custodial account or a donor-advised fund is an `expense`, never a `transfer`, and a monthly one is a planned bill hinted at the account paying it. Money back from one is a refund in that category, or `income`. Transactions inside one can be categorized, and stay out of `actual`, in `not_the_households`; a bill paid from one is settled (R9a).
- [ ] **Step 3: `docs/agent-net-worth.md`.** `buckets`, with the golden example in generic names; its `status` and `cautions` are its own; **never add a bucket to `total`, and never call a sum of them net worth**; `accounts` is the household's alone. Line 41 and lines 197 to 200 follow: an account can leave net worth by a flag or by whose money it is, and which one means what. The account in a bucket a token doesn't see isn't mentioned, as N2. Extend `net_worth/documented.rs` so each caution and the bucket's keys in the example are checked as the rest are.
- [ ] **Step 4: `docs/agent-forecast.md`.** `not_household` with what to do: nothing, unless it really is the household's, then `belongs_to`. A `balance_missing` or `no_counted_accounts` isn't fixed by a flag on such an account. `unbound_provider_account`'s advice now covers these kinds.
- [ ] **Step 5: `docs/threat-model.md`.** Design: *Account grants* gains buckets (B7). Sensitive data gains the beneficiary: possibly a child's name, never logged, in the audit log with its account, and it stays in the audit log after it is removed, as an old account name does; **accepted risk**. A restricted `write` token on one account can move that account out of or into net worth, as it already could with `include_in_net_worth`, and it is audited. A provider's name is read to suggest a kind and goes nowhere else. Page count unchanged, but `/connections` now holds a form with a person's label, and stays in `PAGES` because its script puts no form value in a message (Task 5's read).
- [ ] **Step 6: `AGENTS.md`.** Crate map: the domain's `accounts` holds whose money and the suggestion; the convention: **an account that isn't the household's is out of every count through its flags, held by a CHECK, and anything reading by class alone must also read `belongs_to`**.
- [ ] **Step 7: Track what's left, with the owner's confirmation.** Comment on #3: reads `belongs_to = household`. On #72: a link out of the household reads as an outflow (B9). On #49: manual accounts have no form for this. Then close #38 and #39, or trim them to what's left. No TODOs in source.
- [ ] **Step 8: Run** `cargo run -p coffer-server -- openapi | diff - docs/api/openapi.json` (no output) and `scripts/gate.sh` → PASS.
- [ ] **Step 9: Commit** `docs: accounts for other people and purposes`.
- [ ] **Step 10: Whole-branch review** with a reviewer who didn't implement it: `security-review` over the plan's diff, the golden week by hand, each row of *Spec coverage* and each of *Review Focus*. Fix findings and commit. Then a real run: a copy of a real plan 4 database through `0022` with `cargo run -p coffer-server -- serve`, and binding a 529, a custodial and a donor-advised account from the page with their kept balances. Record in the PR what was and wasn't verified: a CI pass proves neither.

## Spec coverage

| Requirement | Task |
|---|---|
| #38 `education`, `custodial` kinds; charitable decided on the issue | 1; B1 |
| #38 an owner or beneficiary who isn't a member | 1, 2, 5; B5 |
| #38 a separate bucket per owner or purpose, reported on its own | 3, 6; B6 |
| #38 stays out of net worth | 1, 2, 3; B4 |
| #38 stays out of the cash forecast | 2, 4; B3, B4, B8 |
| #38 stays out of allocation and rebalancing (#3) | No code yet; rule B8, comment in 9 |
| #38 open: bucket follows from the kind or is explicit | Kind's default, then chosen: 1, 2; B2 |
| #38 open: relation to the two flags | 1, 2; B4 |
| #38 open: is a total across buckets ever shown | No: 3, 6; B6 |
| #38 `default_liquidity`, `AccountKind` in the API | 1; B3 |
| Binding the owner's unbound accounts, with their kept balances | 2, 3, 5; run in 9 |
| Rows keep accruing; flags apply at read | No table change; 1, 3 |
| Grants | 2, 3, 8; B7 |
| Contributions as real outflows; plan vs actual and forecast | 4, 9; B9 |
| Transactions inside a bucket | 4, 9; B10 |
| Migration of existing accounts | 1; B11 |
| OpenAPI enums, `docs/agent-usage.md` | 1, 2, 7, 9 |
| #39 suggestions, confirmed by a member, nothing logged | 7; B12 |
| Target allocation, holdings, transfer links, manual accounts' page, effective date | Not in scope: B13 |
