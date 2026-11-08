# 07a — Navigation, Home shell and More Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cut the navigation to Home, Money, Activity and More, make Home the landing page with net worth and the count of transactions to check, and add the More page. Closes #140.

**Architecture:** `Nav` becomes the four tabs; every page names the tab it lives under. One `nav` element is the top bar's links on wide screens and a fixed bottom tab bar on phones, by CSS alone. Home is a read-only page at `/` that calls the ledger's net worth report and a new review-queue count, the same calls the API makes. The count is a new small store query reusing the transaction list's filter.

**Tech Stack:** Rust 2024, axum, maud, sqlx (SQLite), utoipa, rmcp. No new crates.

**Spec:** `docs/superpowers/specs/2026-11-07-coffer-money-redesign-design.md` §5 (navigation, Home, More), §7 phase 1. Overview: `docs/superpowers/plans/2026-11-07-coffer-07-money-redesign-overview.md`.

**Branch / worktree:** `money-07a-navigation`, `../coffer-07a`. No migration.

## Global Constraints

- Everything in the overview's "Rules for every unit" applies.
- CSP `default-src 'self'; script-src 'self'; style-src 'self'`: no inline script or style, no `style=` attribute. The sparkline is server-drawn SVG in integers, classes only, with a text equivalent.
- Colours and fonts only from theme tokens; nothing new in a theme file in this unit.
- Words: "Home", "Money", "Activity", "More", "You" (§5). No jargon on Home.
- Read-only roles see no forms. A part of Home the viewer may not read is left out, never an error page.
- `/` is not added to `PAGES` in `http/browser_reports/fields.rs` (Home shows amounts). `/more` may be added: it shows no amounts or names.
- `/account` keeps its URL (overview, planner's call 1); only its label changes to "You".

## Review Focus

1. **A viewer who sees only some accounts.** `Ledger::net_worth` is scoped, not refused: it sums the accounts the actor sees at `balances` and says nothing of the others (it is `Denied` only without `ReadHousehold`). Home must show exactly what `/net-worth` shows that viewer, never a household total, and a viewer who sees no counted account gets the card's empty state, not an error. → Task 3 `home_net_worth_follows_what_the_viewer_sees`.
2. **No display currency set**: net worth points have `total: null`. Home must say where to set it, not show 0. → Task 3 `home_without_a_display_currency_says_so`.
3. **The bottom tab bar covering content** on a 375 px phone: the last form button, the skip link, the setup notice. → Task 2 CSS gives `main` bottom padding equal to the bar's height under the phone media query; check in a real browser at 375 px, both themes, both schemes.
4. **The count leaking hidden accounts**: a token with grants on one account must count only that account's unreviewed transactions. → Task 1 `review_count_counts_only_readable_accounts`.
5. **Old bookmarks and redirects**: `/` used to redirect to `/account`, sign-in answered `{"redirect": "/account"}`. Both now go to `/`; `/account` still works. → Task 3 `sign_in_lands_on_home`, `account_page_still_answers`.

---

### Task 1: The review-queue count in the store, ledger, API and MCP

**Files:**
- Create: `crates/store/src/transactions/count.rs` (a child module of `transactions.rs`, so it reuses the private `push_filter`; `transactions.rs` is already past 800 lines, so nothing is added there but `mod count;`)
- Modify: `crates/store/src/transactions.rs` (`mod count;`), `crates/server/src/ledger/transactions.rs` (or new `crates/server/src/ledger/review_count.rs` if `transactions.rs` would grow; it is at 923 lines, so **create `ledger/review_count.rs`** and add `mod review_count;` in `ledger/mod.rs`), `crates/server/src/http/api/transactions.rs`, `crates/server/src/http/api/mod.rs` (route), `crates/server/src/http/api/mcp/tools.rs`, `crates/server/tests/it/mcp/parity.rs`, `docs/api/openapi.json`
- Test: `crates/store/tests/it/ledger.rs` (or a new `crates/store/tests/it/review_count.rs` with its `mod` line), `crates/server/tests/it/transactions_api.rs`

**Interfaces:**
- Produces (store): `impl Store { pub async fn count_transactions(&self, filter: &TransactionFilter) -> Result<u64, StoreError> }` — `SELECT count(*) FROM transactions WHERE 1 = 1` plus `push_filter`; an empty `AccountScope::Only` answers 0 without a query.
- Produces (ledger): `impl Ledger { pub async fn count_transactions_to_review(&self, actor: &Actor) -> Result<ReviewCount, LedgerError> }` with
  ```rust
  /// How many transactions wait for someone to review them, in the accounts the actor reads at
  /// `transactions`.
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
  pub struct ReviewCount { pub unreviewed: u64 }
  ```
  Authorization as `list_transactions`: resolve access, scope `AccountLevel::Transactions`, filter `review: Some(ReviewStatus::Unreviewed)`, every other criterion empty.
- Produces (API): `GET /api/v1/transactions/review-count`, `operation_id = "countReviewQueue"`, tag transactions, needs `read`. Route it before `/api/v1/transactions/{transaction_id}` so the path doesn't parse as an id (check `router.rs` ordering; axum matches static segments first, but test it).
- Produces (MCP): row `count_review_queue`, `Needs::Read`, lead "How many transactions wait to be reviewed. `list_review_queue` lists them."

- [ ] **Step 1: Write the failing tests**

Store: `count_follows_the_filter` — insert 3 transactions in account A (2 unreviewed) and 1 unreviewed in B; `count_transactions(&filter{scope: All, review: Some(Unreviewed)})` is 3; with `scope: Only([A])` it is 2; with `scope: Only([])` it is 0.

API (`transactions_api.rs`): `review_count_counts_only_readable_accounts` — an admin token reads `{"unreviewed": 3}`; a token granted `transactions` on A only reads `{"unreviewed": 2}`; a token without `read` gets 403. `review_count_drops_after_review` — review one through `POST /api/v1/transactions/batch`, the count falls by one.

MCP (`parity.rs`): a row comparing `count_review_queue` with `GET /api/v1/transactions/review-count`. `parity.rs` is at 755 lines: if this unit merges before the other of 07a/07b, split it by area into `tests/it/mcp/parity/` first (overview, "Order and parallelism").

- [ ] **Step 2: Run them and see them fail**

Run: `nice -n 15 env -u CARGO_TARGET_DIR cargo nextest run -p coffer-store --all-features -j 3 -E 'test(count_follows_the_filter)'` and `… -p coffer-server … -E 'test(review_count)'`
Expected: compile errors (no `count_transactions`, no route).

- [ ] **Step 3: Implement** the store query, the ledger call, the handler with `#[utoipa::path]` through `ApiRouter::op`, and the MCP row.

- [ ] **Step 4: Regenerate the OpenAPI snapshot and run the tests**

Run: `cargo run -p coffer-server -- openapi > docs/api/openapi.json`, then the two filtered test runs and `… -p coffer-server … -E 'test(parity)'`.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git -C ../coffer-07a add crates/store crates/server docs/api/openapi.json
git -C ../coffer-07a commit -m "Count the transactions waiting for review, in the API and MCP"
```

### Task 2: The four tabs, the bottom bar and the More page

**Files:**
- Create: `crates/server/src/http/pages/more.rs`
- Modify: `crates/server/src/http/pages/layout.rs`, `crates/server/src/http/pages/mod.rs` (`mod more;`, merge its routes), `crates/server/assets/css/base.css`, the `current:` line of every page (`rg -n 'current: Some\(Nav::' crates/server/src/http/pages`), `crates/server/src/http/browser_reports/fields.rs` (`/more` into `PAGES`, the array grows to 11)
- Test: `crates/server/tests/it/pages/navigation.rs` (new, `mod navigation;` in `tests/it/pages/mod.rs`), and the existing assertions on old links: `tests/it/pages/net_worth.rs:585` (`nav_links_to_net_worth_for_every_member` now looks on `/more`), `tests/it/accounts_page.rs:194,278` (`aria-current="page">More<`)

**Interfaces:**
- Produces: `pub enum Nav { Home, Money, Activity, More }` (07v adds `Future` between `Money` and `Activity`). Mapping every page keeps: `/plan` → `Money`; `/transactions*` → `Activity`; `/accounts*`, `/net-worth`, `/forecast`, `/categories`, `/rules*`, `/connections*`, `/admin/*`, `/account` → `More`.
- Topbar links in this order: Home `/`, Money `/plan`, Activity `/transactions`, More `/more`. The brand links `/`. The member's name and "Sign out" stay in the top bar.
- `/more` (GET, any signed-in member): sections "Your money" (Accounts `/accounts`, Net worth history `/net-worth`, Cash forecast `/forecast`), "Setup" (Categories `/categories`, Rules `/rules`, Connections `/connections`), "Household" only when `viewer.can_manage_household` (Household `/admin/household`) and `viewer.can_manage_members` (Members `/admin/members`, Invites `/admin/invites`), and "You" `/account` ("Passkeys, tokens and appearance").
- CSS: under the existing phone breakpoint (the one where `.nav` wraps today, `base.css:24` vs `:152`), `.nav` becomes `position: fixed; inset: auto 0 0 0;` with four equal flex items at least 44 px tall, `background: var(--nav-bg)`, a top border in `--nav-border-color`, and `main.page` gets matching bottom padding. Wide screens keep today's inline links. Use only existing tokens.

- [ ] **Step 1: Write the failing tests** in `pages/navigation.rs`:
  - `every_page_shows_the_four_tabs` — for `/`, `/plan`, `/transactions`, `/more`, `/accounts`, `/account`, the body contains, in order, `href="/">Home<`, `href="/plan">Money<`, `href="/transactions">Activity<`, `href="/more">More<`, and exactly one `aria-current="page"`, on the expected tab (`/accounts` and `/account` → More).
  - `more_lists_the_rest` — an admin's `/more` links all ten destinations above; a member's lacks `/admin/household`, `/admin/members`, `/admin/invites`.
  - `the_brand_goes_home` — `<a class="brand" href="/"`.
- [ ] **Step 2: Run** `… -p coffer-server … -E 'test(navigation)'`. Expected: FAIL (old links).
- [ ] **Step 3: Implement** `Nav`, `topbar`, `more.rs` (its own `routes()`), the CSS, and every page's `current`. `error_body`'s "Back to coffer" points at `/`.
- [ ] **Step 4: Run** the navigation tests and the updated `net_worth`/`accounts_page` assertions. Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07a commit -am "Cut the navigation to Home, Money, Activity and More"` (add `more.rs` and `navigation.rs` first).

### Task 3: Home at `/`

**Files:**
- Create: `crates/server/src/http/pages/home.rs`, `crates/server/src/http/pages/home/sparkline.rs`
- Modify: `crates/server/src/http/pages/mod.rs` (drop the `home` redirect, merge `home::routes()`, sign-in's JSON `redirect` at `mod.rs:399` becomes `"/"`), `crates/server/assets/css/base.css` (`.sparkline`, `.home-card`), `staging/e2e/ui/first-admin.spec.mjs:18,37` and `staging/e2e/ui/seeded.spec.mjs:12` (land on `/`)
- Test: `crates/server/tests/it/pages/home.rs` (new, `mod home;`), `crates/server/tests/it/pages/sign_in.rs:27,118,197` (now `/`); unit tests in `sparkline.rs`

**Interfaces:**
- Consumes: `Ledger::net_worth_by_day(actor, |today| NetWorthQuery { from: Some(first day of the month 12 months before today), to: None, interval: Some(NetWorthInterval::Month), account_points: Some(false) })` (the `_by_day` form, so the range and the report use one day, as `pages/net_worth.rs` does); `Ledger::count_transactions_to_review` (Task 1).
- Produces: `pub(super) fn sparkline(values: &[i64], label: &str) -> Markup` in `home/sparkline.rs`: an `svg.sparkline` with `viewBox="0 0 120 32"`, one `polyline` whose points are integers (x spread evenly with integer division, y scaled from min..max into 2..30, a flat series drawn at 16), `role="img"` and `aria-label` = `label`. Fewer than two values draws nothing.
- Home content, in order (later units add above and between): a `section.card` "Net worth": the latest total, "up/down $N this month" (latest point minus the previous point; both must be non-null), the sparkline over the points' totals, and a link "See its history" → `/net-worth`; a `section.card` "To check": "N transactions to check" linking `/transactions?review=unreviewed`, or "Nothing to check." at 0.
- When `net_worth_by_day` answers `Denied` (no `ReadHousehold`), the net worth card is left out. When the report has the `NoCountedAccounts` blocker (the viewer sees no counted account), the card says "No accounts count towards net worth yet." with a link to `/accounts`. When the latest total is null because no display currency is set, the card says "Choose a display currency to see net worth in one number." with a link to `/admin/household` for a viewer who can manage the household, and to `/net-worth` otherwise.

- [ ] **Step 1: Write the failing tests**
  - `sparkline.rs` unit tests: `points_fit_the_box` (`[0, 50, 100]` → `points="0,30 60,16 120,2"`), `a_flat_series_is_a_flat_line` (`[7, 7, 7]` → every y is 16), `one_value_draws_nothing`, `extreme_values_do_not_overflow` (`[i64::MIN, i64::MAX]` maps into 2..30: use `i128`).
  - `pages/home.rs`: `root_is_home_not_a_redirect` (GET `/` is 200 with `<h1>Home</h1>`); `sign_in_lands_on_home` (finish answers `{"redirect": "/"}`); `account_page_still_answers` (GET `/account` 200, tab `More` current); `home_shows_net_worth_and_its_change_this_month` (golden household from the net worth tests' world, display currency set: body has the latest total, "this month" with the expected difference, and one `<polyline`); `home_without_a_display_currency_says_so`; `home_net_worth_follows_what_the_viewer_sees` (a member granted `balances` on one of two accounts: 200, the card's latest total equals that member's `/net-worth` latest total and differs from the admin's; a member granted no account: the empty-state sentence, "To check" present); `home_counts_transactions_to_check` (3 unreviewed → "3 transactions to check" linking the queue; 0 → "Nothing to check.").
- [ ] **Step 2: Run** `… -E 'test(home) | test(sparkline) | test(sign_in)'`. Expected: FAIL.
- [ ] **Step 3: Implement** `home.rs` (`routes()` with `.route("/", get(home_page))`, `Frame { current: Some(Nav::Home), .. }`), `sparkline.rs`, the CSS, the sign-in redirect, the e2e specs.
- [ ] **Step 4: Run** the filtered tests. Expected: PASS.
- [ ] **Step 5: Commit** — `git -C ../coffer-07a commit -m "Land on a Home page with net worth and what's left to check"`

### Task 4: Docs and the gate

**Files:** `AGENTS.md` (crate map: `pages/` gains `home.rs`, Home with its `sparkline.rs`, and `more.rs`; the review count in `ledger/review_count.rs` and `store/src/transactions/count.rs`), `docs/agent-usage.md` (the count and the MCP tool).

- [ ] **Step 1:** Update the docs.
- [ ] **Step 2:** `uptime`; if the one-minute load is under 30, run `nice -n 15 scripts/gate.sh`. Expected: it ends green (fmt, clippy, nextest, doc, deny, OpenAPI check).
- [ ] **Step 3:** If a local `serve` against a scratch database is practical (no staging stack, no Docker), look at `/`, `/more` and one long page at 375 px and 1280 px, Mochi and Fjord, light and dark. Say in the PR what was looked at, or that nothing was.
- [ ] **Step 4: Commit** the docs; push `git -C ../coffer-07a push -u origin money-07a-navigation`; open the PR "Navigation: Home, Money, Activity and More" with "Closes #140", what was verified locally and what only CI ran (e2e).
