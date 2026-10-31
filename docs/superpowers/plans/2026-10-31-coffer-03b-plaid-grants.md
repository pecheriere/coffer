# coffer plan 2b — Per-token account grants and Plaid Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Agent tokens limited to chosen accounts at chosen levels (balances, transactions, write), enforced everywhere plan 2a scoped by account; and Plaid as a second provider (Link on one page, `/transactions/sync` cursors, balances), proving the provider interface.

**Architecture:** Grants extend `coffer_domain::policy::AccountAccess` with a `Granted` variant and are loaded by `auth::access::AccessResolver`, the single point plan 2a routed every account-scoped decision through; handlers and store queries do not change shape. Plaid is a `sync::plaid` module implementing plan 2a's `Provider` trait with a `Cursor` fetch mode, talking to Plaid only through `egress::Egress`; its Link page is the only page whose CSP also allows `https://cdn.plaid.com`.

**Tech Stack:** plan 2a's, plus serde_json feature `raw_value` (to read Plaid's numeric amounts as exact text, never as floats). No new crates.

**Spec:** `docs/superpowers/specs/2026-10-30-coffer-design.md`, especially §8.2 (per-account authorization) and §8.6 (Plaid) added by plan 2a Task 1; §6.2, §6.5, §6.6, §6.11. Issues #2 (Plaid) and #5 (grants).

**Depends on:** plan 2a (`2026-10-31-coffer-03-accounts-providers.md`) merged, and through it the design-system plan: pages use `layout::{page, Frame, Nav}`, tests use `testkit::browser::Browser` and `crates/server/src/testkit/mod.rs`, scripts are content-hashed assets under `crates/server/assets/`, and `security_headers` is the top-level layer in `http/mod.rs`. Migrations continue from 2a's last (`0006`), so this plan's is `0007`.

**Why grants come here, not in 2a:** 2a already routes every account-scoped decision through `authorize_account` with an `AccountAccess` from `AccessResolver`, filters every account-scoped query by `AccountScope`, answers 404 for hidden and 403 (naming the level) for insufficient access, and returns `{ items }` lists that can be filtered. Grants therefore add data and a resolver branch, not a new API shape. 2a was already 13 tasks; grants bring a UI, a migration and a leak property test that deserve their own review.

## Global Constraints

All of plan 2a's Global Constraints apply unchanged. In addition:

- Effective account level = min(token grant, token scope ceiling, member's own access). A member's own access stays uniform `write` (per-member visibility remains a non-goal, spec §3).
- A token with no grant rows (every token created before this plan, and any created without grants) keeps uniform access at its scope: existing tokens behave exactly as before.
- Hidden accounts never leak: no list, aggregate, audit entry, provider account or error reveals an account the token cannot see. Hidden → 404 `not_found`; visible but too low → 403 `forbidden` with detail "this action needs <level> access to this account".
- Plaid configuration (`COFFER_PLAID_CLIENT_ID`, the secret from `COFFER_PLAID_SECRET_FILE`) and Plaid access tokens are credentials: never logged, never in errors, audit or responses; access tokens are sealed by `vault::CredentialVault` like SimpleFin's access URL.
- Plaid environments: `sandbox` and `production` only (Plaid retired `development` in 2024); hosts `sandbox.plaid.com` and `production.plaid.com`.
- No CSP relaxation beyond the Link page, and never `'unsafe-inline'` or `'unsafe-eval'`. If Plaid Link cannot run without them, stop and ask the owner.
- Plaid amounts are positive for money out; coffer's are negative for money out. Converted as exact decimals; never through a float.

## Review Focus

1. **A grant on an account that is later closed**, or an account created after the token: closed stays visible at its level; a new account is visible only through an `all` grant. → Task 2 test `new_accounts_visible_only_through_all_grant`.
2. **A restricted token reading aggregates and side channels** (connections list, provider accounts, audit log, batch errors naming hidden ids): nothing about hidden accounts appears. → Task 2 test `no_endpoint_leaks_hidden_accounts` (property test).
3. **Plaid reports `TRANSACTIONS_SYNC_MUTATION_DURING_PAGINATION`** mid-pagination: the sync restarts from the cursor it started with, up to three times, and commits nothing from the abandoned pass. → Task 4 test `mutation_during_pagination_restarts`.
4. **A Plaid credit card balance** (Plaid reports the amount owed as positive): stored negative, like SimpleFin's. → Task 4 test `liability_balances_are_negative`.
5. **An Item needing re-login** (`ITEM_LOGIN_REQUIRED`): the connection moves to `reauth_required`, scheduled syncs stop spending requests on it, and the Link page offers update mode. → Task 4 test `item_login_required_marks_reauth` and Task 5 test `reconnect_uses_update_mode`.

---

### Task 1: Grants in the domain **[security]**

**Files:**
- Modify: `crates/domain/src/policy.rs`, `docs/superpowers/specs/2026-10-30-coffer-design.md` (§8.2: the grant model below, two paragraphs)
- Test: unit and property tests in `policy.rs`

**Interfaces:**
- Consumes (2a Task 4): `AccountLevel`, `AccountAccess`, `AccountScope`, `authorize_account`, `account_access_for_scope`.
- Produces:
  - `enum GrantTarget { All, Account(AccountId) }` (serde: `"all"` or the account id); `struct TokenGrant { pub target: GrantTarget, pub level: AccountLevel }`.
  - `fn grant_ceiling(scope: Scope) -> AccountLevel`: `Read` → `Transactions`; `Write`, `Admin` → `Write`.
  - `fn validate_grants(scope: Scope, grants: &[TokenGrant]) -> Result<(), GrantError>`: 1..=100 grants, no repeated target, every level ≤ `grant_ceiling(scope)`. `enum GrantError { Empty, TooMany, DuplicateTarget, AboveScope }`.
  - `AccountAccess::Granted { ceiling: AccountLevel, all: Option<AccountLevel>, accounts: BTreeMap<AccountId, AccountLevel> }`; `fn account_access_with_grants(actor: &Actor, grants: &[TokenGrant]) -> AccountAccess` (`Uniform` when `grants` is empty or the actor is a session).
  - `level_for` on `Granted`: `max(all, accounts[id])`, then `min(ceiling)`; `None` if both absent. `scope(at_least)`: `All` when `all >= at_least`, else `Only(accounts with level >= at_least)`.
  - `fn sees_every_account(&self, at_least: AccountLevel) -> bool`: `Uniform(l)` → `l >= at_least`; `Granted` → `all >= at_least`.
  - Rule table (stated in the `policy` module docs and used by Task 2): the audit log, unbound provider accounts, creating an account, binding a provider account and triggering a sync need `sees_every_account(Transactions)` (`Write` for the mutations), because each touches more than one account or accounts not yet granted.

- [ ] **Step 1: Write failing tests.** `uniform_without_grants`; `all_grant_plus_specific_takes_max` (`all: Balances`, `X: Write` → X `Write`, Y `Balances`); `ceiling_caps_grants` (read token's grants never exceed `Transactions`); `no_grant_hides_account` (`authorize_account` → `AccountHidden`); `scope_lists_only_sufficient_accounts`; `validate_grants_rules` (each `GrantError`); proptest `level_for_never_exceeds_ceiling_or_grant` (random grants and ids).
- [ ] **Step 2: Run** `cargo test -p coffer-domain` → FAIL. **Step 3: Implement**, and write the §8.2 paragraphs. **Step 4: Run** → PASS; security review.
- [ ] **Step 5: Commit** `feat(domain): per-token account grants`.

### Task 2: Grants on tokens, enforced everywhere **[security]**

**Files:**
- Create: `crates/store/migrations/0007_token_grants.sql`, `crates/store/src/token_grants.rs`
- Modify: `crates/server/src/auth/{agent_tokens,access}.rs`, `crates/server/src/http/pages/account.rs`, `crates/server/src/http/api/{me,audit,accounts,transactions,provider_accounts,connections}.rs`, `crates/server/src/ledger/*.rs`, `crates/server/src/sync/connections.rs`, `docs/api/openapi.json`, `docs/agent-usage.md`, `docs/threat-model.md`
- Test: `crates/server/tests/grants.rs`

**Interfaces:**
- Schema `0007`: `agent_token_grants(token_id FK, target TEXT NOT NULL /* 'all' or account id */, level CHECK IN ('balances','transactions','write'), PRIMARY KEY (token_id, target))`.
- Store: `StoreTx::replace_token_grants(AgentTokenId, &[TokenGrant])`, `Store::token_grants(AgentTokenId) -> Vec<TokenGrant>`.
- `AgentTokens::create(&self, actor, name, scope, expires_at, grants: Vec<TokenGrant>)` (validated with `validate_grants` when non-empty; each `Account` target must exist); `AgentTokens::replace_grants(&self, actor, AgentTokenId, Vec<TokenGrant>)` (passkey session; own token, or any with `ManageMembers`; audit `token.grants` with `before`/`after` as a canonical text form `all=balances,<id>=write`).
- `AccessResolver::resolve`: session → `Uniform`; agent → `account_access_with_grants(actor, store.token_grants(token))`.
- Required tests (carried from plan 2a Task 4 review): the resolver never grants a level above the actor's scope ceiling (`AccountAccess::scope()` must agree with `authorize_account`'s cap for every `Granted` value; property test over role × token scope × grants), and `authorize_account` returns `AccountHidden` (→ byte-identical 404) for an account absent from the grants.
- Idempotency (carried from plan 2a Task 5 review): once grants exist, a narrower token must never replay a wider token's response. Include the agent token id (or `session`) in the idempotency key scope, or re-authorize before every replay; test that token B with fewer grants reusing token A's key gets its own outcome, not A's stored response.
- Hidden-account tests through real grants (carried from plan 2a Task 7 review): a token whose grants hide an account gets 404 for `GET /transactions?account=<hidden>`, never sees the hidden account's rows or cursor effects in an unfiltered list, gets `operations[i]: not found` (404) for a hidden transaction in a batch, and gets 404 before any idempotency key is reserved when creating a transaction on a hidden account.
- Enforcement (each listed handler or service already calls `AccessResolver`; this task adds the rules of Task 1's table): audit list → 403 unless `sees_every_account(Transactions)`; provider accounts list → bound ones filtered by `Balances` scope, unbound ones only with `sees_every_account(Transactions)`; connections list → provider accounts filtered the same way; `create_account`, `bind`, `create_account_and_bind`, `request_sync` → `sees_every_account(Write)`; batch errors name the operation index, never a hidden id.
- `MeResponse` gains `access: AccessView { ceiling: AccountLevel, all: Option<AccountLevel>, accounts: Vec<AccountGrantView { account, level }> }` (uniform access shows `all = ceiling`, `accounts = []`).
- Page `/account`: the token form gains "All accounts" (none / balances / transactions / write) and one select per open account; each listed token shows its grants and has an edit form posting to `/account/tokens/{id}/grants` (CSRF). Defaults reproduce uniform access.

- [ ] **Step 1: Write failing tests.**
  - `legacy_token_keeps_uniform_access` (token created without grants sees every account).
  - `restricted_token_sees_only_granted_accounts` (accounts list, account get → 404 for hidden, transactions list).
  - `balances_level_hides_transactions` (account and balances visible; its transactions excluded from lists; `GET /transactions/{id}` → 403 with detail naming `transactions`).
  - `write_needs_write_grant` (PATCH on a `transactions`-level account → 403).
  - `new_accounts_visible_only_through_all_grant`.
  - `restricted_token_cannot_read_audit_or_trigger_sync` (403).
  - `grants_change_audited_and_session_only` (agent → 403/404; session → audit `token.grants` before/after).
  - `me_reports_access`.
  - `no_endpoint_leaks_hidden_accounts`: proptest (16 cases, each a fresh `TestServer`) over 1..=4 accounts with transactions, provider accounts and a random valid grant set; through the router with that token, every response of `GET /accounts`, `/accounts/{id}`, `/accounts/{id}/balances`, `/transactions`, `/transactions/{id}`, `/provider-accounts`, `/connections`, `/me` is checked: no hidden account id, name or transaction id appears anywhere in the body.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement**; regenerate the OpenAPI snapshot; update `agent-usage.md` (grants, levels, 404 vs 403) and `threat-model.md` (grant model, the side channels closed).
- [ ] **Step 4: Run** full gate → PASS; security review.
- [ ] **Step 5: Commit** `feat(auth): per-account grants for agent tokens`.

### Task 3: Plaid configuration and API client **[security]**

**Files:**
- Modify: `Cargo.toml` (serde_json `raw_value`), `crates/server/src/config.rs`
- Create: `crates/server/src/sync/plaid/{mod,client}.rs`, `crates/server/tests/fixtures/plaid/{link_token_create.json,public_token_exchange.json,accounts_get.json,transactions_sync_page1.json,transactions_sync_page2.json,transactions_sync_posted.json,error_item_login_required.json,error_mutation_during_pagination.json,item_remove.json}`
- Test: unit tests in `config.rs` and `client.rs`; `crates/server/tests/plaid_client.rs`

**Interfaces:**
- Config: `Config.plaid: Option<PlaidConfig>`; `PlaidConfig { client_id: String, secret: SecretString, environment: PlaidEnvironment, country_codes: Vec<String> }`. Variables: `COFFER_PLAID_CLIENT_ID` and `COFFER_PLAID_SECRET_FILE` (both or neither; the file holds the secret, trailing newline trimmed, refused if group- or world-readable like the server key), `COFFER_PLAID_ENV` (`sandbox` | `production`, required when Plaid is configured; `development` refused with "Plaid retired the development environment; use sandbox or production"), `COFFER_PLAID_COUNTRY_CODES` (default `US`, comma-separated ISO 3166-1 alpha-2). `PlaidEnvironment::host()` → `sandbox.plaid.com` / `production.plaid.com`. `Debug` of `PlaidConfig` redacts the secret and the client id.
- Produces (`sync::plaid::client`): `struct PlaidClient { egress: Egress, config: PlaidConfig, base: Url, hosts: HostAllowList }` with:
  - `link_token(&self, member: MemberId, update_for: Option<&SecretString>) -> Result<SecretString, PlaidError>` (`/link/token/create`: `client_name` `coffer`, `user.client_user_id` = member id, `language` `en`, `country_codes`; new Items: `products: ["transactions"]`, `transactions.days_requested: 730`; update mode: `access_token` instead of `products`).
  - `exchange(&self, public_token: &SecretString) -> Result<PlaidItem { access_token: SecretString, item_id: String }, PlaidError>` (`/item/public_token/exchange`).
  - `accounts(&self, access_token) -> Result<Vec<PlaidAccount>, PlaidError>` (`/accounts/get`).
  - `sync_page(&self, access_token, cursor: Option<&str>) -> Result<SyncPage { added, modified, removed: Vec<String>, next_cursor: String, has_more: bool }, PlaidError>` (`/transactions/sync`, `count: 500`).
  - `remove_item(&self, access_token) -> Result<(), PlaidError>` (`/item/remove`).
  - Every request: `POST`, JSON body with `client_id` and `secret` (request structs implement `Debug` by hand, redacting both and any token), header `Plaid-Version: 2020-09-14`, through `Egress::send` with `hosts` = the one environment host.
  - `PlaidAccount { account_id, name, official_name, kind: String /* type */, subtype: Option<String>, current: Option<DecimalAmount>, iso_currency_code: Option<String>, unofficial_currency_code: Option<String> }`; `PlaidTransaction { transaction_id, account_id, amount: DecimalAmount, iso_currency_code, unofficial_currency_code, date: Date, authorized_date: Option<Date>, name: String, merchant_name: Option<String>, pending: bool, pending_transaction_id: Option<String> }`. Numeric fields are read as `Box<RawValue>` and parsed with `DecimalAmount::parse` (an exponent or non-decimal form is a `PlaidError::Invalid`).
  - `enum PlaidError { ItemLoginRequired, MutationDuringPagination, RateLimited, Api { error_type: String, error_code: String } /* message and request id kept only for debug logs after sanitizing */, Invalid(&'static str), Egress(EgressError) }`, from Plaid's error body (`error_type`, `error_code`).
  - testkit: `PlaidClient::for_tests(mock: &MockUpstream, config)` using `Egress::for_tests()`.
- Fixtures: generic, shaped like Plaid's documented sandbox responses ("First Platypus Bank"-style sandbox names are fine, no owner data); they include an integer amount (`12`), a one-decimal amount (`4.5`), a credit card account, a pending transaction and, in `transactions_sync_posted.json`, its posted successor with `pending_transaction_id`.

- [ ] **Step 1: Write failing tests.** Config: `plaid_requires_both_id_and_secret_file`, `plaid_env_rejects_development_with_explanation`, `plaid_secret_file_permissions_checked`, `plaid_config_debug_redacted`. Client (against `MockUpstream`): `requests_carry_credentials_in_body_and_version_header`; `amounts_parsed_from_raw_numbers` (`12` → `"12"`, `4.5` → `"4.5"`, `1e2` → `Invalid`); `error_body_mapped` (`ITEM_LOGIN_REQUIRED` → `ItemLoginRequired`; `TRANSACTIONS_SYNC_MUTATION_DURING_PAGINATION` → `MutationDuringPagination`; `RATE_LIMIT_EXCEEDED` → `RateLimited`); `secrets_absent_from_errors_and_logs` (every error's `Display`/`Debug` and `captured_log_text()` lack the secret, the client id and the access token); `only_the_environment_host_is_reachable` (a base URL on another host → `EgressError::HostNotAllowed` in non-test egress).
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** full gate → PASS; security review.
- [ ] **Step 5: Commit** `feat(sync): plaid configuration and api client`.

### Task 4: Plaid provider

**Files:**
- Create: `crates/server/src/sync/plaid/provider.rs`
- Modify: `crates/server/src/sync/provider.rs` (`ProviderKind::Plaid`, `ConnectInput::PlaidPublicToken`), `ProviderRegistry::from_config` in the same file (registers Plaid when `Config.plaid` is set; the single registration point), `crates/server/src/testkit/mod.rs` (`TestServer::with_plaid`)
- Test: `crates/server/tests/plaid_sync.rs`

**Interfaces:**
- Consumes: `PlaidClient` (Task 3); 2a's `Provider`, `FetchRequest`, `FetchResult`, `ProviderAccountData`, `ProviderTransaction`, `Coverage::Delta`, `apply_account_batch` (removals by id, `replaces_provider_id`).
- Produces: `ProviderKind::Plaid` (`"plaid"`); `ConnectInput::PlaidPublicToken(SecretString)`; `struct Plaid { client: PlaidClient }` implementing `Provider` with `Capabilities { pending: true, mode: Cursor, min_interval: 15 min, min_scheduled_interval: 1 h, daily_budget: 48 }`.
  - `connect(PlaidPublicToken)` → `exchange`; the credential is the JSON `{"access_token": ..., "item_id": ...}` (a private serde struct, sealed by the engine).
  - `fetch`: `accounts`, then `sync_page` from `request.cursor` until `has_more` is false. On `MutationDuringPagination`, discard everything gathered and restart from `request.cursor`, at most 3 passes, then `ProviderError::Unavailable("Plaid data changed during sync; try again")`. Returns `cursor = next_cursor` of the last page, every account with `coverage: Delta`, its `added`+`modified` as transactions and its `removed` ids. Mapping: `amount` → `DecimalAmount::negated`; `date` → `posted_on`; `authorized_date` → `transacted_on`; `merchant_name` → `payee`; `name` → `description`; `pending_transaction_id` → `replaces_provider_id`; currency from `iso_currency_code`, else the `unofficial_currency_code` text (which 2a's engine rejects as an account error); balance `current` dated `request.today`, negated for `type` `credit` and `loan`. `kind_hint`: depository/checking → checking, depository/savings → savings, credit → credit_card, loan/mortgage → mortgage, other loan → loan, investment/brokerage → brokerage, investment with a retirement subtype (`401k`, `403b`, `ira`, `roth`, `pension`) → retirement, otherwise none.
  - `ItemLoginRequired` → `ProviderError::ReauthRequired`; `RateLimited` → `ProviderError::RateLimited`.
  - `disconnect` → `remove_item`; a failure is returned, so `remove_connection` (2a) fails without changing anything and the member can retry (Plaid bills Items until removed).

- [ ] **Step 1: Write failing tests** (against `MockUpstream`, `TestServer::with_plaid`): `first_sync_pages_until_done_and_saves_cursor` (two pages; second request carries page 1's `next_cursor`; connection `provider_state` = final cursor); `next_sync_starts_from_saved_cursor`; `plaid_amount_sign_inverted` (Plaid `12` → -1200 minor units USD); `liability_balances_are_negative`; `posted_successor_attaches_via_pending_transaction_id` (categorize the pending transaction; next sync's posted record with a different amount and a new id attaches to it, user state kept); `removed_ids_remove_sources`; `mutation_during_pagination_restarts` (page 2 fails once with the mutation error → restart from the original cursor; only the second pass's data committed); `item_login_required_marks_reauth` (status `reauth_required`; 2a's `scheduler::due` skips it); `disconnect_removes_item_or_fails_cleanly` (mock `/item/remove` 500 → `DELETE /api/v1/connections/{id}` (2a Task 15) 502 `provider_error`, connection unchanged (2a Task 13 `remove_connection`); then 200 → removed).
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** full gate → PASS.
- [ ] **Step 5: Commit** `feat(sync): plaid provider`.

### Task 5: Plaid Link page **[security]**

**Files:**
- Create: `crates/server/src/http/pages/plaid.rs`, `crates/server/assets/js/plaid-link.js` (served by the design system's asset pipeline at its hashed URL, `assets::JS_PLAID_LINK_JS`)
- Modify: `crates/server/src/http/pages/{mod,connections}.rs`, `crates/server/src/http/mod.rs` (`security_headers` keeps a page-set CSP), `crates/server/src/sync/connections.rs` (`confirm_reauthorized`)
- Test: `crates/server/tests/plaid_link.rs`

**Interfaces:**
- Routes (registered only when `Config.plaid` is set; otherwise 404), all session-authenticated:
  - Both pages render with `layout::page` (`Frame { current: Some(Nav::Connections), .. }`) and load `assets::JS_PLAID_LINK_JS.url` plus Plaid's script.
  - `GET /connections/plaid/new` → page with a label field and a "Connect with Plaid" button; the link token is created server-side and placed in a `data-link-token` attribute; the CSRF token as on every page.
  - `GET /connections/{id}/plaid/reconnect` → same page in update mode (link token created with the connection's access token; only for Plaid connections the actor may manage).
  - `POST /connections/plaid/exchange` (JSON `{ label, public_token }`, CSRF header) → `SyncService::create_connection(.., ProviderKind::Plaid, .., ConnectInput::PlaidPublicToken)`; response `{ redirect: "/connections" }`.
  - `POST /connections/{id}/plaid/reconnected` (CSRF) → `SyncService::confirm_reauthorized(actor, ConnectionId)` (new; `RemoveConnection { owner }`; Plaid update mode keeps the same access token, so nothing is resealed; status `active`, a manual run queued, audit `connection.reconnect`).
  - `plaid-link.js`: loads nothing itself; calls `Plaid.create({ token, onSuccess, onExit })` and posts the result with `fetch` (same origin, CSRF header). No inline script.
- CSP for these two pages only (`const PLAID_LINK_CSP`), set by the handler and preserved by the top-level `security_headers` layer (which now sets the default only when the response has none; a test keeps every other route on the default): `default-src 'self'; script-src 'self' https://cdn.plaid.com/link/v2/stable/link-initialize.js; frame-src https://cdn.plaid.com; connect-src 'self' https://<environment host>; style-src 'self'; img-src 'self'; frame-ancestors 'none'; form-action 'self'; base-uri 'none'`. Exchange endpoints keep the default CSP. The `/connections` page links to the Plaid page when Plaid is configured and shows "Reconnect" for `reauth_required` Plaid connections.
- Manual check (in the PR description, not CI): Link opens in a real browser against sandbox with no CSP violation in the console; adjust only by adding exact Plaid origins.

- [ ] **Step 1: Write failing tests.** `plaid_pages_absent_without_config` (404); `link_page_has_plaid_csp_only_there` (Link page CSP equals `PLAID_LINK_CSP`; `/connections`, `/account`, `/login` keep the default CSP without `plaid`); `link_page_has_no_inline_script` (every `<script>` has a `src`); `exchange_requires_session_and_csrf` (bearer → 401; no CSRF → 403; cross-origin → 403); `exchange_stores_sealed_access_token` (mock exchange returns `access-sandbox-SECRET`; DB bytes, audit, `captured_log_text()` and response lack it; a sync is queued); `public_token_never_echoed_on_error`; `reconnect_uses_update_mode` (mock sees `access_token` and no `products` in `/link/token/create`); `member_cannot_reconnect_others_connection`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** full gate → PASS; security review (CSP, CSRF, token handling).
- [ ] **Step 5: Commit** `feat(pages): plaid link with a page-scoped csp`.

### Task 6: Operator and agent docs, whole-branch review

**Files:**
- Modify: `docs/configuration.md`, `docs/agent-usage.md`, `docs/threat-model.md`, `docs/container.md`, `README.md`, `AGENTS.md`

**Interfaces:** none (documentation and review).

- [ ] **Step 1: Docs.** `configuration.md`: the Plaid variables; the outbound host list gains `sandbox.plaid.com` or `production.plaid.com` (only the configured one) on 443, and notes that Link itself runs in the member's browser, which must reach `cdn.plaid.com` and Plaid's hosts (coffer's server does not). `threat-model.md`: Plaid as a provider (sealed access tokens, `/item/remove` on disconnect); accepted risk: Plaid's script runs on the Link page with that page's DOM (it holds only a link token and the CSRF token, no financial data), with no subresource integrity because Plaid serves a moving `stable` script. `agent-usage.md`: Plaid connections appear in `/connections`; connecting one is a browser step. `README.md` and `AGENTS.md` updated.
- [ ] **Step 2: Run** full gate and `GH_TOKEN=$(gh auth token) uvx zizmor .` → PASS, no findings.
- [ ] **Step 3: Commit** `docs: plaid and account grants`.
- [ ] **Step 4: Whole-branch security review** (`security-review` skill): grants enforcement, Plaid credential handling, CSP; fix findings; commit.
