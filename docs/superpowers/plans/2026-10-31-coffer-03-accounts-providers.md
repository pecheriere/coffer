# coffer plan 2a — Typed API, accounts, transactions, providers and SimpleFin Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every `/api/v1` endpoint typed and described by a generated OpenAPI 3.1 document; manual and linked accounts with balance snapshots; canonical transactions with source records, splits, categories, review and assignment; a provider-agnostic sync engine with the matching pipeline; SimpleFin as the first provider; and the agent-facing API to review, categorise, assign and sync.

**Architecture:** `coffer-domain` gains pure modules for money, accounts, transactions and matching, plus per-account authorization (`authorize_account` with an `AccountAccess` value) so per-token grants (plan 2b) change only the policy and its resolver. `coffer-store` gains repositories and migrations `0002`–`0005`. `coffer-server` gains `api_types` (utoipa schemas), a `ledger` service (accounts, categories, transactions), `egress` (the only outbound HTTP client: HTTPS, allow-listed hosts, no redirects, optional `HTTPS_PROXY`), `vault` (XChaCha20-Poly1305 sealing of provider credentials under a key derived from the server key), and `sync` (provider trait, engine, apply step, scheduler, SimpleFin).

**Tech Stack:** as plan 1, plus utoipa 6.0 (`macros`, `uuid`, `jiff_0_2`) and utoipa-axum 0.3; reqwest 0.13 (`default-features = false`, features `rustls-no-provider`, `http2`, `json`, `query`); rustls 0.23 (`default-features = false`, features `ring`, `std`, `tls12`, `logging` off) and rustls-platform-verifier 0.7; chacha20poly1305 0.11; jiff feature `tzdb-bundle-always`. Every one of these must pass the crate-age check (released at least 7 days before it lands) and `cargo deny check`; pin an older release if not.

**Spec:** `docs/superpowers/specs/2026-10-30-coffer-design.md` (§3, §4, §5, §6.1 accounts/transactions/categories, §6.2, §6.5, §6.6, §6.11, §6.13, §7), plus the addendum §8 written by Task 1. Issues: #4 (SimpleFin), #5 (per-account grants: policy shape here, grants in plan 2b), #6 (typed OpenAPI).

**Plan split.** The owner's plan 2 scope is too large for one plan (about 18 tasks), so it is two plans: this one (2a: typed API, accounts, transactions, provider trait, sync engine, SimpleFin) and `2026-10-31-coffer-03b-plaid-grants.md` (2b: per-token account grants, then Plaid). 2b depends on 2a.

**Deferred from the plan 1 roadmap's "plan 2"** (to plan 3 unless the owner says otherwise): household settings, rules and suggested rules, transfer links, a starter category set, unbinding a provider account. Nothing in this plan's API has to change shape to add them.

## Global Constraints

- `rust-toolchain.toml` `channel = "1.99"`; edition 2024; workspace lints unchanged (`unsafe_code = "forbid"`, clippy `all` + `pedantic` deny, `unwrap_used`/`expect_used`/`print_*`/`todo`/`dbg_macro` deny). No `#[allow]`/`#[expect]` without `reason = "..."`.
- "every amount is an integer in **minor units** plus its ISO 4217 currency; exponent from ISO 4217 (USD 2, JPY 0, etc.)" "Sign convention: negative = money out of the account." No `f32`/`f64` anywhere on a money path, including parsing provider JSON.
- Time only through `Clock`; instants are `jiff::Timestamp` (stored as Unix ms `INTEGER`), calendar days are `jiff::civil::Date` (stored as `TEXT` `YYYY-MM-DD`). Ids are UUIDv7 newtypes from `coffer_domain::ids`.
- `sqlx` stays in `coffer-store` only; no `sqlx` type in its public API; runtime-checked queries; new tables are `STRICT`.
- Every mutation goes through `Store::begin` (`BEGIN IMMEDIATE`) together with its audit entry; the audit log stays append-only. Sync changes are attributed `system:sync`.
- Every handler authorizes through `coffer_domain::policy` (`authorize` for household actions, `authorize_account` for anything tied to an account).
- "Credential leakage. SimpleFin access URLs, session cookies or tokens in logs, API responses, error messages or the repo. Tokens stored in clear." Provider credentials exist in clear only in memory (`SecretString`), never in the database, audit, logs, errors or responses.
- Every `/api/v1` route is registered through `ApiRouter` (Task 3) with a utoipa path; no `serde_json::Value` in any request, query, path or response type; every error is `application/problem+json` with a `ProblemCode`; request rejections never echo input.
- Outbound HTTP only through `egress::Egress`: `https` only, per-provider host allow-list, no redirects, bounded time and body. Tests make no network calls: providers are tested against recorded, generic fixtures served by a loopback mock.
- Logs: no secrets, no transaction descriptions or amounts at `info` (spec §6.13).
- No dead code, no TODO comments. Tests specify behaviour; no tests that restate implementation.
- Public repository: nothing specific to the owner (banks, categories, hosts) in code, fixtures or docs.
- Supply chain: new crate versions at least 7 days old (crate-age), licences within `deny.toml`, `cargo deny check` clean; fix, don't exempt.
- Builds use the configured `CARGO_TARGET_DIR`; never override it.
- Full gate (Definition of done): `cargo fmt --check && cargo clippy --workspace --all-targets --all-features --locked -- -D warnings && cargo test --workspace --all-features --locked && RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked && cargo deny check`.
- Security review (`security-review` skill, findings fixed) on every task marked **[security]**.
- Commits: conventional commit subjects; no `Co-Authored-By`, `Claude-Session` or `Generated with` trailers.

## Review Focus

1. **Two identical purchases on the same day** (same account, amount, date and merchant, distinct provider ids): two transactions, never merged. → Task 8 test `identical_same_day_purchases_stay_distinct`.
2. **A pending card charge that posts with a tip** (amount up to 20 % higher, new provider id, pending record gone): the posted record attaches to the pending one's transaction and keeps its user state. → Task 8 test `pending_with_tip_attaches_to_posted`.
3. **A transaction posted at 23:30 local time**: its date is the household's local day (`COFFER_TIMEZONE`), not the UTC day. → Task 12 test `posted_date_uses_household_timezone`.
4. **An agent retrying a create concurrently with the same `Idempotency-Key`**, or reusing a key with a different body: one resource, then replay; a different body is refused. → Task 5 tests `idempotent_retry_replays_response`, `idempotency_key_reuse_with_other_body_refused`, `concurrent_same_key_creates_once`.
5. **One provider account with a non-ISO currency or a currency different from its bound Account**: that account reports an error, every other account commits. → Task 11 test `currency_problem_isolated_to_one_account`.

---

### Task 1: Spec addendum

**Files:**
- Modify: `docs/superpowers/specs/2026-10-30-coffer-design.md` (append `## 8. Addendum (2026-10-31)`; amend §3 and §5 by pointing to it)

**Interfaces:** none (documentation). Later tasks and plan 2b cite §8.

- [ ] **Step 1: Write §8** with these subsections, each a few sentences, stating decisions, not options:
  - **8.1 Typed API now.** The OpenAPI document (§6.11, §6.14) is generated from code from plan 2 on (issue #6), served unauthenticated at `/api/v1/openapi.json`, committed at `docs/api/openapi.json`, diffed by a test and linted in CI. Problem `type` is `urn:coffer:problem:<code>`.
  - **8.2 Per-account authorization.** §3's "no tables or code for it" is superseded for agent tokens (issue #5): every account-scoped decision goes through `authorize_account(actor, access, account, level)` with levels `balances < transactions < write`. Plan 2a resolves every actor to uniform access at its scope; plan 2b adds per-token grants. Per-member visibility remains a non-goal. Hidden accounts answer 404; a visible account with too low a level answers 403 naming the level needed.
  - **8.3 Provider credentials.** Sealed with XChaCha20-Poly1305 under a key derived (BLAKE3 `derive_key`, context `coffer 2026-10 provider credentials v1`) from the server key in `COFFER_SECRET_KEY_FILE`; the connection id is the associated data; a version byte allows a dedicated key later. Losing the server key means reconnecting every provider.
  - **8.4 Matching v1.** Fingerprint version 1 and the date tolerance as defined in Task 8, including the pending-successor rule (posted record may differ by up to 20 % from a vanished pending record with the same description key).
  - **8.5 Outbound network.** §5's "limited to SimpleFin and the FX rate source" now reads: SimpleFin Bridge hosts (`COFFER_SIMPLEFIN_HOSTS`), Plaid's API host when Plaid is configured (plan 2b), the FX source (plan 3). HTTPS only, no redirects, `HTTPS_PROXY` honoured when set; the exact host list lives in `docs/configuration.md`.
  - **8.6 Plaid** (plan 2b): a second provider behind the same interface; Link runs on one page whose CSP alone also allows `https://cdn.plaid.com`; environments `sandbox` and `production` (Plaid retired `development` in 2024); `/transactions/sync` cursors; `/accounts/get` balances; `/item/remove` on disconnect.
  - **8.7 Household timezone.** Provider timestamps become calendar days in `COFFER_TIMEZONE` (IANA name, default `UTC`).
- [ ] **Step 2:** In §3 append to the per-account bullet "(superseded for agent tokens by §8.2)"; replace "The provider interface (§6.2) is in v1; other implementations are not." with "... other implementations are not, except Plaid (§8.6)."; in §5 append "(extended by §8.5)" to the external network line.
- [ ] **Step 3: Commit** `docs(spec): addendum for typed api, account authz, providers and egress`.

### Task 2: Typed audit changes

**Files:**
- Modify: `crates/domain/src/audit.rs`, `crates/store/src/audit.rs`, every audit emitter in `crates/server/src/auth/{identity,agent_tokens,sessions}.rs`
- Create: `crates/store/migrations/0002_audit_changes_version.sql`
- Test: unit tests in `crates/domain/src/audit.rs` and `crates/store/src/audit.rs`; existing server tests updated

**Interfaces:**
- Produces (`coffer_domain::audit`):
  - `enum AuditValue { Null, Bool(bool), Int(i64), Text(String) }` with `From` for `bool`, `i64`, `&str`, `String`, every id type, `Timestamp`/`civil::Date` (as `Text`, RFC 3339 / ISO), `Option<T: Into<AuditValue>>` (`None` → `Null`).
  - `struct FieldChange { pub before: AuditValue, pub after: AuditValue }`.
  - `struct AuditChanges(BTreeMap<String, FieldChange>)` with `new()`, `set(self, field: &str, value) -> Self` (before `Null`), `change(self, field, before, after) -> Self`, `unset(self, field, before) -> Self` (after `Null`), `iter()`, `is_empty()`. Serde: a JSON object `{field: {"before": v, "after": v}}`.
  - `AuditEntry.changes: AuditChanges` (replaces `serde_json::Value`).
- Produces (store): `AuditEvent.changes: AuditChanges`. Rows are written with `changes_version = 2`; rows with version 1 (written by 0.1.0) are converted on read: a field whose value is an object with exactly `from` and `to` becomes `change(field, from, to)`; a top-level `{from, to}` object (only `member.role`) becomes field `role`; any other scalar becomes `set(field, value)`; a non-scalar becomes `Text(<compact JSON>)`.
- Migration `0002`: `ALTER TABLE audit_events ADD COLUMN changes_version INTEGER NOT NULL DEFAULT 1 CHECK (changes_version IN (1, 2));` (an `ALTER`, so the append-only triggers do not fire).

- [ ] **Step 1: Write failing tests.** Domain: `changes_serialize_as_before_after_objects` (`AuditChanges::new().set("name", "x").change("role", "member", "admin")` serializes to `{"name":{"before":null,"after":"x"},"role":{"before":"member","after":"admin"}}`); `option_none_is_null`. Store: `changes_roundtrip_typed`; `legacy_rows_are_read_as_typed_changes` (insert raw v1 rows with `{"label":{"from":"k","to":null}}`, `{"from":"member","to":"admin"}` for action `member.role`, and `{"username":"bob"}`; read back as `change("label","k",Null)`, `change("role","member","admin")`, `set("username","bob")`).
- [ ] **Step 2: Run** `cargo test -p coffer-domain -p coffer-store` → FAIL.
- [ ] **Step 3: Implement**, then convert every emitter to `AuditChanges` keeping the same field names; `member.role` now records field `role`; invite changes record `kind`, `role`, `display_name`, `expires_at` as separate fields.
- [ ] **Step 4: Run** the full gate → PASS (update assertions in `crates/server/tests/*.rs` that read `changes`).
- [ ] **Step 5: Commit** `refactor(audit): typed before/after changes`.

### Task 3: OpenAPI document, typed problems and the existing endpoints

**Files:**
- Modify: `Cargo.toml` (utoipa, utoipa-axum), `crates/domain/Cargo.toml` (feature `openapi = ["dep:utoipa"]`), `crates/domain/src/{ids,identity,audit}.rs` (schemas behind `openapi`), `crates/server/Cargo.toml` (enables `coffer-domain/openapi`)
- Create: `crates/server/src/http/api/router.rs` (`ApiRouter`), `crates/server/src/http/api/extract.rs`, `crates/server/src/http/api/openapi.rs`, `docs/api/openapi.json`, `.spectral.yaml`, `.github/spectral/Dockerfile`
- Modify: `crates/server/src/http/problem.rs`, `crates/server/src/http/api/{mod,me,members,audit}.rs`, `crates/server/src/cli.rs` (subcommand `openapi`), `.github/workflows/ci.yml` (job `openapi-lint`), `.github/dependabot.yml` (docker, directory `/.github/spectral`), `docs/agent-usage.md`, `AGENTS.md`
- Test: `crates/server/tests/openapi.rs`; `crates/server/tests/api.rs` updated

**Interfaces:**
- Produces (`http::problem`): `enum ProblemCode` (serde `snake_case`, `ToSchema`): `Unauthenticated, Forbidden, CsrfInvalid, CrossOrigin, NotFound, MethodNotAllowed, Invalid, InvalidCursor, Conflict, InviteUnusable, CeremonyExpired, Busy, PasskeyRejected, Internal`; later tasks add variants. `Problem.code: ProblemCode`. `struct ProblemBody { r#type: String, title: String, status: u16, code: ProblemCode, detail: Option<String> }` (`ToSchema`, the only error body); `type` = `urn:coffer:problem:<code>`. `#[derive(IntoResponses)] enum CommonProblems` documenting 400, 401, 403, 404, 405, 422, 500 as `application/problem+json` `ProblemBody`.
- Produces (`http::api::router`): `struct ApiRouter(OpenApiRouter<AppState>)` with `new() -> Self`, `op(self, routes: utoipa_axum::router::UtoipaMethodRouter<AppState>) -> Self` (the only way to add an API route) and `split(self) -> (Router<AppState>, utoipa::openapi::OpenApi)`. No other method adds routes, so every API route is documented by construction.
- Produces (`http::api::extract`): `TypedJson<T>`, `TypedQuery<T>` (serde_urlencoded; structs use `#[serde(deny_unknown_fields)]`, so unknown and repeated parameters are rejected), `TypedPath<T>`. Rejections are `400 invalid` with detail naming at most the parameter, never a value. Body limit for `/api/v1`: 256 KiB.
- Produces (`http::api::openapi`): `fn document() -> utoipa::openapi::OpenApi` (info title `coffer`, version = crate version, bearer security scheme `bearer`, applied to every operation except `GET /api/v1/openapi.json`); `GET /api/v1/openapi.json` serves it.
- Produces (domain, feature `openapi`): `ToSchema` for every id (string, `format: uuid`, description "UUIDv7"), `Role`, `Scope`, `AuditValue` (`oneOf` null/boolean/integer/string), `FieldChange`, `AuditChanges` (object of `FieldChange`).
- Response types (`ToSchema`, `Serialize`): `MeResponse { member: MemberView, agent: AgentView }`, `MemberView { id, username, display_name, role, disabled_at: Option<Timestamp> }`, `AgentView { id, name, scope }`, `MemberList { items: Vec<MemberView> }`, `AuditQuery` (`IntoParams`: `entity_type, entity_id, member, agent, from, to, after, limit`), `AuditPage { items: Vec<AuditEventView>, next: Option<String> }`, `AuditEventView { id, at, actor: AuditActorView, action, entity_type, entity_id, changes: AuditChanges }`, `enum AuditActorView` tagged `kind`: `member { member, agent: Option<AgentTokenId> }` | `system { reason }`. JSON stays as today except `type`.
- Rule for later tasks: small bounded collections (members, accounts, categories, connections) return `{ items }`; unbounded ones (audit, transactions, balances, sync runs) return `{ items, next }` with an opaque cursor.
- CLI: `coffer openapi` prints the pretty-printed document plus a trailing newline; reads no configuration.

- [ ] **Step 1: Write failing tests** in `tests/openapi.rs`:
  - `snapshot_matches_generated_document`: output of `cli::run(["coffer","openapi"])` equals `docs/api/openapi.json`; the failure message says to run `cargo run -p coffer-server -- openapi > docs/api/openapi.json`.
  - `every_documented_operation_is_routed`: for each path and method in `document()`, send a request (path parameters replaced by fresh UUIDv7s, a read token) through `http::router` wrapped in a test layer that copies `MatchedPath` into a response header; assert the header equals the documented path.
  - `documented_methods_match_routed_methods`: for each documented path, `TRACE` it; the 405 `Allow` header lists exactly the documented methods (plus `HEAD` where `GET` is documented).
  - `no_untyped_schemas`: walking every schema in the document, none is empty or `additionalProperties: true` without a `type`, `$ref`, `oneOf`, `anyOf` or `allOf`.
  - `every_error_response_is_problem_json`: every non-2xx response of every operation has only `application/problem+json` content referencing `ProblemBody`.
  - `openapi_served_without_auth`: `GET /api/v1/openapi.json` with no token → 200 `application/json`, body equals `document()`.
  - In `tests/api.rs`: `problem_type_is_urn_of_code` (401 body `type == "urn:coffer:problem:unauthenticated"`); `query_rejection_does_not_echo` (`/api/v1/audit?limit=SECRETVALUE` and `?bogus=SECRETVALUE` → 400 `invalid`, body free of `SECRETVALUE`).
- [ ] **Step 2: Run** `cargo test -p coffer-server --test openapi --test api` → FAIL.
- [ ] **Step 3: Implement.** Retrofit `me`, `members`, `audit` with `#[utoipa::path]` and the types above (audit keeps its current filter semantics via `TypedQuery<AuditQuery>`); move `problem_bodies` routing fallbacks to `ProblemBody`. Generate and commit `docs/api/openapi.json`.
- [ ] **Step 4: Lint.** `.spectral.yaml` extends `spectral:oas` with no rule disabled. `.github/spectral/Dockerfile` is one line, `FROM stoplight/spectral:<release ≥ 7 days old>@sha256:<digest>`. CI job `openapi-lint` (`permissions: contents: read`, checkout with `persist-credentials: false`): `docker build -t spectral .github/spectral` then `docker run --rm -v "$PWD:/work:ro" -w /work spectral lint docs/api/openapi.json --fail-severity warn`. Fix every finding in code (descriptions, operation ids, tags), never in the ruleset. Run `GH_TOKEN=$(gh auth token) uvx zizmor .` → no findings.
- [ ] **Step 5: Docs.** `docs/agent-usage.md`: point to `/api/v1/openapi.json`, document `type` URNs. `AGENTS.md` conventions: API routes only via `ApiRouter`; no `serde_json::Value` in API types; regenerate the snapshot with `coffer openapi`.
- [ ] **Step 6: Run** full gate → PASS.
- [ ] **Step 7: Commit** `feat(api): generated openapi document and typed problems`.

### Task 4: Domain — money, accounts and per-account authorization **[security]**

**Files:**
- Create: `crates/domain/src/money.rs`, `crates/domain/src/accounts.rs`
- Modify: `crates/domain/src/ids.rs`, `crates/domain/src/policy.rs`, `crates/domain/src/lib.rs`
- Test: unit and property tests in those files

**Interfaces:**
- Produces (`ids`): `AccountId, CategoryGroupId, CategoryId, TransactionId, SplitId, ConnectionId, ProviderAccountId, SourceRecordId, SyncRunId`.
- Produces (`money`):
  - `struct Currency([u8; 3])`: `Currency::parse(&str) -> Result<Currency, MoneyError>` accepts only uppercase codes in the built-in ISO 4217 table (active codes with their minor-unit exponents, copied from the ISO 4217 list; funds and precious-metal codes excluded); `exponent(self) -> u8`; `as_str`; serde as the code.
  - `struct Money { pub minor_units: i64, pub currency: Currency }` (serde `{ "minor_units": -1234, "currency": "USD" }`), `checked_add`.
  - `struct DecimalAmount(String)`: `parse(&str)` accepts `-?[0-9]{1,18}(\.[0-9]{1,18})?` only (no exponent, no `+`, no spaces); `negated(&self) -> DecimalAmount`; `to_minor_units(&self, Currency) -> Result<i64, MoneyError>` — exact; extra fractional digits allowed only if they are zeros; overflow is an error.
  - `fn format_minor_units(minor: i64, currency: Currency) -> String` (`-1234`, USD → `"-12.34"`).
  - `enum MoneyError { UnknownCurrency, Malformed, TooPrecise, Overflow, CurrencyMismatch }`; messages never echo input.
- Produces (`accounts`):
  - `enum AccountKind` (`checking, savings, credit_card, loan, mortgage, brokerage, retirement, equity_comp, property, vehicle, other_asset, other_liability`), `enum LiquidityClass` (`cash, short_term_liability, invested, illiquid, long_term_liability`), both serde `snake_case`, `FromStr`, `as_str`.
  - `fn default_liquidity(kind) -> LiquidityClass`: checking/savings → cash; credit_card → short_term_liability; brokerage/retirement/equity_comp → invested; property/vehicle/other_asset → illiquid; loan/mortgage/other_liability → long_term_liability.
  - `struct AccountName(String)` and `struct ConnectionLabel(String)`: trimmed, 1..=100 chars, no control characters.
- Produces (`policy`):
  - `enum AccountLevel { Balances, Transactions, Write }` (`Ord`, serde `snake_case`); `Balances` = the account and its balances; `Transactions` adds its transactions and source records; `Write` adds every change to them.
  - `enum AccountAccess { Uniform(AccountLevel) }` (`#[non_exhaustive]`; plan 2b adds grants) with `level_for(&self, AccountId) -> Option<AccountLevel>` and `scope(&self, at_least: AccountLevel) -> AccountScope`.
  - `enum AccountScope { All, Only(Vec<AccountId>) }` — the store filters every account-scoped query by it.
  - `fn account_access_for_scope(actor: &Actor) -> AccountAccess`: effective scope `Read` → `Uniform(Transactions)`, `Write`/`Admin` → `Uniform(Write)`.
  - `fn authorize_account(actor: &Actor, access: &AccountAccess, account: AccountId, needed: AccountLevel) -> Result<(), Denied>`: `None` level → `DenyReason::AccountHidden`; level below `needed` → `DenyReason::AccountLevel { needed }`.
  - New `Action` variants: `WriteHousehold` (categories; `Scope::Write`), `ManageConnections` (add a connection, trigger sync, bind provider accounts; `Scope::Write`), `RemoveConnection { owner: MemberId }` (`Scope::Write` and `actor.member == owner`, or effective scope `Admin`).
  - New `DenyReason` variants `AccountHidden`, `AccountLevel { needed: AccountLevel }`; `auth::denied_message` gains their messages ("this action needs <level> access to this account"); `Problem` maps `AccountHidden` to 404 `not_found`.

- [ ] **Step 1: Write failing tests.**
  - `currency_exponents_follow_iso4217` (USD 2, JPY 0, KWD 3, EUR 2); `currency_rejects_lowercase_unknown_and_urls` (`"usd"`, `"XYZ"`, `"https://x"`).
  - `decimal_to_minor_units_is_exact` (`"-12.34"` USD → -1234; `"12.340"` → 1234; `"12.345"` → `TooPrecise`; `"1e3"`, `"+1"`, `" 1"` → `Malformed`; `"100"` JPY → 100; `"92233720368547758.08"` USD → `Overflow`).
  - proptest `format_then_parse_roundtrips` over `i64` in ±10^15 and currencies with exponents 0, 2, 3.
  - `default_liquidity_table` (every kind, values above).
  - `account_name_rules` (trim, 100 chars ok, 101 rejected, control char rejected).
  - `read_token_reads_transactions_but_cannot_write`, `write_token_writes_any_account`, `member_cannot_remove_others_connection_but_admin_can`, `read_scope_cannot_manage_connections`.
- [ ] **Step 2: Run** `cargo test -p coffer-domain` → FAIL.
- [ ] **Step 3: Implement.** Schemas behind the `openapi` feature for every new public type (`Money` documented as integer minor units plus ISO 4217 code).
- [ ] **Step 4: Run** → PASS; clippy clean; security review of `policy.rs`.
- [ ] **Step 5: Commit** `feat(domain): money, account classification and per-account authorization`.

### Task 5: Accounts, balance snapshots and idempotency keys

**Files:**
- Create: `crates/store/migrations/0003_accounts.sql`, `crates/store/src/{accounts,balances,idempotency}.rs`
- Create: `crates/server/src/ledger/{mod,accounts}.rs`, `crates/server/src/auth/access.rs`, `crates/server/src/http/idempotency.rs`, `crates/server/src/http/api/accounts.rs`
- Modify: `crates/server/src/http/mod.rs` (`AppState.ledger`, `AppState.access`), `crates/server/src/cli.rs` (`open_state`), `crates/server/src/testkit.rs`
- Test: `crates/store/tests/ledger.rs`, `crates/server/tests/accounts.rs`

**Interfaces:**
- Schema `0003`: `accounts(id, name, kind CHECK(...), currency TEXT CHECK (length(currency) = 3), liquidity CHECK(...), liquidity_overridden INTEGER, include_in_net_worth INTEGER, include_in_forecast INTEGER, closed_at NULL, created_at)`; `balance_snapshots(account_id FK, on_date TEXT, balance_minor INTEGER, source CHECK IN ('sync','manual'), recorded_at, PRIMARY KEY (account_id, on_date))`; `idempotency_keys(member_id FK, key TEXT, request_hash BLOB, state CHECK IN ('pending','done'), status INTEGER NULL, response_json TEXT NULL, created_at, PRIMARY KEY (member_id, key))`.
- Store: `StoreTx::insert_account(NewAccount) -> AccountRecord`, `StoreTx::update_account(AccountId, AccountChanges) -> AccountRecord`, `Store::get_account(AccountId)`, `Store::list_accounts(&AccountScope, include_closed: bool) -> Vec<AccountRecord>`; `StoreTx::upsert_balance(BalanceSnapshot)` (last write wins per day), `Store::list_balances(AccountId, from: Option<Date>, to: Option<Date>, &PageRequest) -> Page<BalanceSnapshot>` (newest first), `Store::latest_balance(AccountId) -> Option<BalanceSnapshot>`; `StoreTx::reserve_idempotency(member, key, hash, now) -> Reservation { Fresh | Replay { status, response_json } | Mismatch | InProgress }` (purges rows older than 24 h first; a `pending` row older than 5 minutes counts as abandoned and is replaced), `StoreTx::complete_idempotency(member, key, status, response_json)`, `StoreTx::release_idempotency(member, key)`.
- Server:
  - `auth::access::AccessResolver` with `async fn resolve(&self, actor: &Actor) -> Result<AccountAccess, AuthError>`; in this plan it returns `account_access_for_scope(actor)`. Every account-scoped service call obtains access through it (plan 2b changes only this resolver and the policy).
  - `ledger::Ledger { store, clock, access }`; `ledger::LedgerError { Denied(Denied), NotFound, Invalid(&'static str), Conflict(ProblemCode, &'static str), Store(StoreError) }` with `From<LedgerError> for Problem`.
  - `Ledger::create_account(actor, NewAccountInput) -> Result<AccountView, LedgerError>` (`WriteHousehold`), `update_account(actor, AccountId, AccountPatch)` (`authorize_account(.., Write)`; currency immutable; setting `kind` without `liquidity` re-defaults liquidity unless overridden), `get_account`, `list_accounts(actor, include_closed)` (scope `Balances`), `set_manual_balance(actor, AccountId, Date, i64)` (`Write`), `list_balances(actor, AccountId, ..)` (`Balances`). Audit actions `account.create`, `account.update`, `balance.set`.
  - `http::idempotency::run<T: Serialize + DeserializeOwned>(state, actor, key: Option<IdempotencyKey>, request_hash: [u8; 32], create: impl Future<Output = Result<(StatusCode, T), Problem>>) -> Result<(StatusCode, Json<T>), Problem>`; `IdempotencyKey` header extractor (`Idempotency-Key`, 1..=128 visible ASCII). Request hash = BLAKE3 of method, path and body bytes. New `ProblemCode`s `IdempotencyKeyReuse` (422), `IdempotencyInProgress` (409).
- API (`TypedJson`/`TypedPath`, all documented):
  - `GET /api/v1/accounts?include_closed` → `AccountList { items: Vec<AccountView> }`.
  - `POST /api/v1/accounts` (`Idempotency-Key` optional) body `NewAccountRequest { name, kind, currency, liquidity: Option, include_in_net_worth: Option<bool> (default true), include_in_forecast: Option<bool> (default true), opening_balance: Option<{ on: Date, minor_units: i64 }> }` → 201 `AccountView`.
  - `GET /api/v1/accounts/{account_id}`; `PATCH /api/v1/accounts/{account_id}` body `AccountPatch { name?, kind?, liquidity?, include_in_net_worth?, include_in_forecast?, closed? }`.
  - `GET /api/v1/accounts/{account_id}/balances?from&to&after&limit` → `BalancePage { items: Vec<BalanceView { on, balance: Money, source }>, next }`.
  - `PUT /api/v1/accounts/{account_id}/balances/{on}` body `{ minor_units }` → `BalanceView`.
  - `AccountView { id, name, kind, currency, liquidity, liquidity_overridden, include_in_net_worth, include_in_forecast, closed: bool, source: "manual" | "linked", latest_balance: Option<BalanceView>, created_at }` (`source` is `linked` once a provider account is bound, Task 11).

- [ ] **Step 1: Write failing tests.** Store: `balance_last_write_wins_per_day`, `list_accounts_honours_scope_and_closed`. Server (router, write token): `create_manual_account_defaults_liquidity` (kind `credit_card` → `short_term_liability`, response `source == "manual"`); `opening_balance_recorded_as_manual_snapshot`; `currency_is_immutable` (PATCH with `currency` → 400 `invalid`, unknown field); `read_token_cannot_create_account` (403); `account_changes_audited_with_agent`; `idempotent_retry_replays_response` (same key and body twice → same id, one account, one audit entry); `idempotency_key_reuse_with_other_body_refused` (422 `idempotency_key_reuse`); `concurrent_same_key_creates_once` (two concurrent requests → one 201, the other 201 replay or 409 `idempotency_in_progress`; exactly one account).
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** full gate (snapshot regenerated) → PASS.
- [ ] **Step 5: Commit** `feat: manual accounts, balance snapshots and idempotency keys`.

### Task 6: Categories and transactions core

**Files:**
- Create: `crates/domain/src/transactions.rs`, `crates/store/migrations/0004_transactions.sql`, `crates/store/src/{categories,transactions}.rs`, `crates/server/src/ledger/{categories,transactions}.rs`, `crates/server/src/http/api/categories.rs`
- Test: `crates/domain/src/transactions.rs` (unit), `crates/store/tests/ledger.rs`, `crates/server/tests/categories.rs`, `crates/server/tests/transactions_core.rs`

**Interfaces:**
- Domain (`transactions`): `enum ReviewStatus { Unreviewed, Reviewed }`; `enum CategoryKind { Expense, Income, Transfer }`; `enum TransactionFlag { PossibleDuplicate, SourceRemoved, SplitMismatch }`; `struct SplitInput { category: CategoryId, minor_units: i64 }`; `fn validate_splits(amount_minor: i64, splits: &[SplitInput]) -> Result<(), SplitError>` (empty is valid = uncategorized; otherwise 1..=50 splits summing exactly to the amount, no zero split); `struct UserState { splits: bool, notes: bool, reviewed: bool, assignee: bool, payee_edited: bool }` with `is_empty()`; text limits: payee 1..=200, notes 0..=2000, description 1..=500, no control characters except `\n` in notes.
- Schema `0004`: `category_groups(id, name, position, archived_at, created_at)`; `categories(id, group_id FK, name, kind CHECK, position, archived_at, created_at)`; `transactions(id, account_id FK, origin CHECK IN ('manual','imported'), posted_on, transacted_on NULL, pending INTEGER, amount_minor, currency, raw_description, imported_payee NULL, payee NULL, payee_edited INTEGER, memo NULL, notes NULL, review CHECK, assignee_id NULL FK, reviewed_by_member NULL, reviewed_by_agent NULL, reviewed_at NULL, categorized_by_member NULL, categorized_by_agent NULL, categorized_by_system NULL, categorized_at NULL, flag_possible_duplicate, flag_source_removed, flag_split_mismatch, created_at, updated_at)` with indexes `(account_id, posted_on)`, `(review, assignee_id)`; `transaction_splits(id, transaction_id FK ON DELETE CASCADE, category_id FK, minor_units, position)`.
- Store: categories CRUD (`insert_category_group`, `update_category_group`, `insert_category`, `update_category`, `list_categories() -> Vec<CategoryGroupRecord with categories>`); `StoreTx::insert_manual_transaction(NewManualTransaction) -> TransactionRecord`, `StoreTx::update_user_fields(TransactionId, UserFieldChanges) -> TransactionRecord`, `StoreTx::replace_splits(TransactionId, &[SplitInput], Attribution)`, `StoreTx::delete_transaction(TransactionId)`, `Store::get_transaction(TransactionId) -> TransactionRecord` (with splits). `struct Attribution { member: Option<MemberId>, agent: Option<AgentTokenId>, system: Option<String>, at: Timestamp }`.
- Server:
  - `Ledger::{list_categories, create_category_group, update_category_group, create_category, update_category}` (`ReadHousehold` / `WriteHousehold`; archived categories cannot receive new splits: 422 `invalid`). Audit `category_group.*`, `category.*`.
  - `Ledger::create_manual_transaction(actor, NewManualTransactionInput) -> TransactionView` (account must be manual and open, `Write` on it; currency from the account); `Ledger::update_transaction(actor, TransactionId, TransactionPatch) -> TransactionView` (`Write`); `Ledger::delete_transaction(actor, TransactionId)` (manual only, else 409 `conflict`); `Ledger::get_transaction` (`Transactions`). Audit `transaction.create`, `transaction.update` (changed fields only), `transaction.delete`.
  - `TransactionPatch { payee: Patch<String>, notes: Patch<String>, review: Option<ReviewStatus>, assignee: Patch<MemberId>, splits: Option<Vec<SplitInput>> }` where `enum Patch<T> { Absent, Clear, Set(T) }` deserializes from absent / `null` / value. `payee: Clear` restores the imported payee and clears `payee_edited`. Assignee must be an active member. Setting `review: reviewed` records `reviewed_by`; changing splits records `categorized_by`; a no-op patch writes no audit entry.
  - `TransactionView { id, account, amount: Money, posted_on, transacted_on, pending, description, payee, memo, notes, review, assignee, splits: Vec<SplitView { id, category, amount: Money }>, flags: Vec<TransactionFlag>, origin, reviewed_by: Option<AttributionView>, categorized_by: Option<AttributionView>, created_at, updated_at }`; `AttributionView { member, agent, system, at }`.
- API: `GET /api/v1/categories` → `CategoryTree { items: Vec<CategoryGroupView { id, name, position, archived, categories: Vec<CategoryView> }> }`; `POST /api/v1/category-groups`, `PATCH /api/v1/category-groups/{group_id}`, `POST /api/v1/categories`, `PATCH /api/v1/categories/{category_id}` (creates accept `Idempotency-Key`). Transaction endpoints come in Task 7.

- [ ] **Step 1: Write failing tests.** Domain: `splits_must_sum_to_amount` (sum off by one → error; empty ok; 51 splits → error; zero split → error); proptest `any_partition_of_the_amount_validates`. Store: `deleting_transaction_cascades_splits`. Server (service level with `TestServer`): `categorize_records_attribution_with_agent`, `review_records_reviewer`, `payee_clear_restores_imported`, `archived_category_rejected`, `assignee_must_be_active_member`, `manual_transaction_only_in_manual_account`, `no_op_patch_not_audited`, `category_crud_via_api_audited`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** full gate → PASS.
- [ ] **Step 5: Commit** `feat: categories, splits and the transaction review model`.

### Task 7: Transactions API — list, filter, review in batches

**Files:**
- Create: `crates/server/src/http/api/transactions.rs`
- Modify: `crates/store/src/transactions.rs` (filtered listing), `crates/server/src/ledger/transactions.rs` (batch)
- Test: `crates/server/tests/transactions_api.rs`

**Interfaces:**
- Store: `struct TransactionFilter { scope: AccountScope, accounts: Vec<AccountId>, from: Option<Date> /*incl.*/, to: Option<Date> /*excl.*/, review: Option<ReviewStatus>, assignee: Option<AssigneeFilter { Member(MemberId) | Unassigned }>, categories: Vec<CategoryId>, uncategorized: bool, pending: Option<bool>, flag: Option<TransactionFlag>, min_minor: Option<i64>, max_minor: Option<i64>, text: Option<String> }`; `Store::list_transactions(&TransactionFilter, &PageRequest) -> Page<TransactionRecord>` ordered `posted_on DESC, id DESC`; cursor opaque (`<date>~<id>`, anything else `InvalidCursor`); `text` matches case-insensitively in description, payee and notes with `LIKE` wildcards escaped.
- Server: `Ledger::list_transactions(actor, TransactionQueryInput)` (scope `Transactions`; requested accounts outside the scope → treated per `AccountAccess`: hidden → 404); `Ledger::apply_batch(actor, Vec<BatchOperation { id: TransactionId, patch: TransactionPatch }>) -> Vec<TransactionView>` (1..=200 ops, distinct ids, one store transaction, all or nothing; an error names the failing index: `operations[3]: ...`).
- API:
  - `GET /api/v1/transactions` with `TransactionQuery` (`IntoParams`): `account` (comma-separated ids), `from`, `to`, `review`, `assignee` (member id or `none`), `category` (comma-separated), `uncategorized`, `pending`, `flag`, `min_amount`, `max_amount` (minor units), `q` (≤ 100 chars), `after`, `limit` → `TransactionPage { items: Vec<TransactionView>, next }`.
  - `POST /api/v1/transactions` (manual account; `Idempotency-Key`) body `NewTransactionRequest { account, posted_on, minor_units, description, payee?, memo?, notes? }` → 201.
  - `GET|PATCH|DELETE /api/v1/transactions/{transaction_id}` (`PATCH` body `TransactionPatch`).
  - `POST /api/v1/transactions/batch` body `{ operations: Vec<BatchOperation> }` → `{ items: Vec<TransactionView> }`.

- [ ] **Step 1: Write failing tests** (router, agent tokens): `filters_combine` (seed manual transactions across 2 accounts, 3 categories, reviewed/unreviewed, assigned/unassigned; each filter alone and `review=unreviewed&assignee=<bob>&account=<a>` together return exactly the expected ids); `uncategorized_filter`; `text_search_escapes_wildcards` (`q=50%` matches only a description containing `50%`); `pagination_is_stable_across_equal_dates` (5 transactions on one date, `limit=2` pages through all exactly once); `bad_cursor_is_400_invalid_cursor`; `batch_is_atomic` (op 2 references an archived category → 422 naming `operations[1]`, op 1 unchanged); `batch_review_and_assign_attributed_to_agent` (audit entries carry the token); `read_token_cannot_patch` (403); `delete_imported_refused` (seed via store as `imported` → 409).
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** full gate → PASS.
- [ ] **Step 5: Commit** `feat(api): transaction listing, filters and batch review`.

### Task 8: Source records and the matching pipeline

**Files:**
- Create: `crates/domain/src/matching.rs`, `crates/store/migrations/0005_sync.sql`, `crates/store/src/{connections,provider_accounts,sources}.rs`, `crates/server/src/sync/{mod,apply}.rs`, `crates/server/src/ledger/duplicates.rs`
- Modify: `crates/server/src/http/api/transactions.rs` (merge, unmerge, dismiss)
- Test: unit/property tests in `matching.rs`; `crates/server/tests/matching.rs`

**Interfaces:**
- Domain (`matching`):
  - `const FINGERPRINT_VERSION: u8 = 1; const DATE_TOLERANCE_DAYS: i64 = 4; const PENDING_SUCCESSOR_TOLERANCE_PERCENT: i64 = 20;`
  - `fn description_key(description: &str) -> String`: uppercase ASCII letters only (every other char becomes a space), tokens of 3+ letters, the first two joined by one space (`"SQ *BLUE BOTTLE 1234"` → `"BLUE BOTTLE"`; no qualifying token → `""`).
  - `fn fingerprint(amount_minor: i64, description: &str) -> String` = `"fp1:<amount_minor>:<description_key>"`.
  - `struct Incoming { posted_on: Date, amount_minor: i64, description_key: String }`.
  - `struct Candidate { transaction: TransactionId, posted_on: Date, amount_minor: i64, description_key: String, live_record_from_same_connection: bool, only_vanished_pending: bool }`.
  - `enum MatchDecision { Attach(TransactionId), New, NewPossibleDuplicate(Vec<TransactionId>) }`.
  - `fn decide(incoming: &Incoming, candidates: &[Candidate]) -> MatchDecision`, the algorithm (step 1, same connection and provider id, happens before, in the apply step):
    ```text
    eligible = candidates where !live_record_from_same_connection
               and |posted_on - incoming.posted_on| <= DATE_TOLERANCE_DAYS
               and (amount equal
                    or (only_vanished_pending and description keys equal and non-empty
                        and |amount - incoming| * 100 <= |candidate amount| * 20))
    if eligible is empty -> New
    if exactly one -> Attach(it)
    narrowed = eligible with equal non-empty description key and equal amount
    if exactly one -> Attach(it) else NewPossibleDuplicate(eligible ids)
    ```
- Schema `0005`: `connections(id, provider TEXT /* validated on read; no CHECK so providers can be added without a table rebuild */, label, owner_id FK, credentials BLOB NULL, provider_state TEXT NULL, status CHECK IN ('active','error','reauth_required','removed'), last_error NULL, last_attempt_at NULL, last_success_at NULL, created_at, removed_at NULL)`; `provider_accounts(id, connection_id FK, provider_account_id, name, institution NULL, currency_raw, kind_hint NULL, account_id NULL FK /* binding */, last_balance_minor NULL, last_balance_on NULL, error NULL, first_seen_at, last_seen_at, UNIQUE(connection_id, provider_account_id))`; `transaction_sources(id, connection_id FK, provider_account_ref FK provider_accounts, provider_txn_id, transaction_id NULL FK, posted_on, transacted_on NULL, amount_raw TEXT, amount_minor NULL, description, payee NULL, memo NULL, pending INTEGER, fingerprint, fingerprint_version, first_seen_at, last_seen_at, UNIQUE(connection_id, provider_txn_id))`; `duplicate_candidates(transaction_id FK, candidate_id FK, PRIMARY KEY (transaction_id, candidate_id))`; `sync_runs(id, connection_id FK, trigger CHECK IN ('schedule','manual','initial'), requested_by_member NULL, requested_by_agent NULL, status CHECK IN ('queued','running','succeeded','partial','failed'), detail NULL, queued_at, started_at NULL, finished_at NULL)`.
- Store primitives (all on `StoreTx`): `find_source(ConnectionId, &str) -> Option<SourceRecord>`, `insert_source(NewSource) -> SourceRecord`, `update_source(SourceRecordId, ImportedFields)`, `attach_source(SourceRecordId, TransactionId)`, `delete_source(SourceRecordId)`, `match_candidates(account, connection, date_from, date_to) -> Vec<CandidateRow>`, `insert_imported_transaction(NewImportedTransaction) -> TransactionRecord`, `update_imported_fields(TransactionId, ImportedFields)`, `sources_of(TransactionId) -> Vec<SourceRecord>`, `set_flag(TransactionId, TransactionFlag, bool)`, `add_duplicate_candidates(TransactionId, &[TransactionId])`, `pending_sources_since(ConnectionId, ProviderAccountId, Date) -> Vec<SourceRecord>`, `held_sources(ProviderAccountId) -> Vec<SourceRecord>` (unattached).
- Server (`sync::mod`): `enum SyncError { Denied(Denied), NotFound, Invalid(&'static str), Store(StoreError) }` with `From<SyncError> for Problem`; Tasks 10–11 add `Vault(VaultError)`, `Provider(ProviderError)`, `RateLimited { retry_after: SignedDuration }`, `CurrencyMismatch`.
- Server (`sync::apply`):
  - `struct AccountBatch { provider_account: ProviderAccountRecord, account: Option<AccountRecord>, transactions: Vec<NormalizedTransaction>, removed: Vec<String>, coverage: Coverage, balance: Option<(Date, i64)> }`; `NormalizedTransaction { provider_id, posted_on, transacted_on, amount_minor, amount_raw, description, payee, memo, pending, replaces_provider_id: Option<String> }`; `enum Coverage { Window { start: Date }, Delta }`.
  - `async fn apply_account_batch(tx: &mut StoreTx, now: Timestamp, connection: ConnectionId, batch: AccountBatch) -> Result<ApplyReport, SyncError>`, in this order: (a) removals — explicit `removed` ids, plus for `Window` every pending source of this provider account with `posted_on >= start` absent from the batch; each removed source is detached and deleted; a transaction left without sources is deleted when its `UserState` is empty, otherwise kept and flagged `SourceRemoved`; (b) for each incoming record: same `(connection, provider_id)` → update the source and, unless the transaction is posted and the record is pending, its imported fields; else a `replaces_provider_id` that names an existing source of this connection → attach to that source's transaction; else `matching::decide` on `match_candidates`; `NewPossibleDuplicate` sets the flag and records candidates; (c) imported-field updates never touch user fields; if the amount changes and the transaction has one split, that split follows the amount; with several splits, they stay and `SplitMismatch` is set; (d) for an unbound provider account, sources are stored with `transaction_id NULL` (held). Each created/attached/updated/removed transaction is audited (`transaction.import`, `transaction.attach_source`, `transaction.imported_update`, `transaction.source_removed`, `transaction.delete`) with actor `System { reason: "system:sync" }`.
  - `async fn apply_held(tx, now, ProviderAccountRecord, AccountRecord) -> Result<ApplyReport, SyncError>` runs held sources through (b)–(c).
- Duplicate resolution (`ledger::duplicates`, all `Write` on the account, audited): `merge(actor, target, duplicate, discard_duplicate_state: bool)` — same account required; moves the duplicate's sources to the target, deletes the duplicate; refused with 409 `merge_conflict` when the duplicate has user state and `discard_duplicate_state` is false (discarded values recorded in the audit `before`); `unmerge(actor, transaction, source)` — detaches one source (the transaction keeps at least one) into a new transaction with no user state; `dismiss_duplicate(actor, transaction)` — clears the flag and candidates.
- API: `POST /api/v1/transactions/{transaction_id}/merge` `{ duplicate, discard_duplicate_state? }`; `POST /api/v1/transactions/{transaction_id}/unmerge` `{ source }`; `POST /api/v1/transactions/{transaction_id}/dismiss-duplicate`. `TransactionView` gains `sources: Vec<SourceView { id, connection, provider_account, pending, posted_on, amount_raw, first_seen_at }>` and `duplicate_candidates: Vec<TransactionId>`. New `ProblemCode::MergeConflict`.

- [ ] **Step 1: Write failing tests.** Domain: `description_key_examples` (examples above, `"POS 12/03"` → `"POS"`); `exact_amount_within_tolerance_attaches`; `outside_tolerance_is_new` (5 days); `two_candidates_same_key_flagged` (→ `NewPossibleDuplicate` with both); `live_same_connection_record_never_candidate`; proptest `decide_never_attaches_to_ineligible` (random candidate sets: result is `Attach(t)` only for an eligible `t`). Server (apply step with fixtures built in Rust): `reimport_is_idempotent` (same batch twice → same transactions, no new audit entries for unchanged records); `user_state_survives_reimport` (categorize, review, assign, edit payee, add notes, then re-import with changed description and date → user fields unchanged, imported fields updated); `pending_to_posted_new_id_keeps_user_state`; `pending_with_tip_attaches_to_posted` (pending 40.00 vanishes, posted 46.00 same key → same transaction, splits rescaled since one split); `identical_same_day_purchases_stay_distinct` (two 5.00 records same day, same description, distinct ids, one batch → two transactions; re-import → still two); `vanished_pending_without_state_deleted_with_state_flagged`; `multi_split_amount_change_flags_mismatch`; `held_sources_matched_on_bind`; `merge_moves_sources_and_refuses_user_state` (409 without `discard_duplicate_state`, audit records discarded notes with it); `unmerge_restores_separate_transaction`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** full gate → PASS.
- [ ] **Step 5: Commit** `feat(sync): source records, matching pipeline and duplicate resolution`.

### Task 9: Outbound HTTP egress **[security]**

**Files:**
- Modify: `Cargo.toml` (reqwest, rustls, rustls-platform-verifier), `crates/server/Cargo.toml`, `crates/server/src/config.rs` (`https_proxy`), `Dockerfile` (CA bundle), `deny.toml` (only if a licence or target change is needed; see the open question in the report), `docs/configuration.md`
- Create: `crates/server/src/egress.rs`
- Test: unit tests in `egress.rs` and `config.rs`; `crates/server/tests/egress.rs`

**Interfaces:**
- Config: `Config.https_proxy: Option<Url>` from `HTTPS_PROXY` (else `https_proxy`): scheme `http` or `https`, a host, no path beyond `/`, no query or fragment; user information allowed and never printed (`Config`'s `Debug` redacts it). Ambient proxy variables are otherwise ignored (the client is built with no automatic proxy), so `NO_PROXY`, `HTTP_PROXY` and `ALL_PROXY` have no effect: coffer contacts only allow-listed HTTPS hosts.
- Produces (`egress`):
  - `struct HostAllowList(Vec<String>)`: `parse(list: &str) -> Result<HostAllowList, EgressError>` (comma-separated DNS names, lowercased, no IP literals, no wildcards, no ports).
  - `struct Egress` (`Clone`): `Egress::new(proxy: Option<&Url>) -> Result<Egress, EgressError>` builds one reqwest client: rustls with the `ring` provider and the platform verifier (system CA bundle), TLS 1.2+, `redirect::Policy::none()`, connect timeout 10 s, request timeout 60 s, user agent `coffer/<version>`, no cookies, no automatic proxy (`Proxy::https(proxy)` when configured), and a DNS resolver that drops non-public addresses.
  - `fn check_url(&self, url: &Url, allowed: &HostAllowList) -> Result<(), EgressError>`: scheme `https`; host a DNS name in `allowed` (case-insensitive, trailing dot ignored); port absent or 443; no user information; no fragment.
  - `async fn send(&self, request: OutboundRequest, allowed: &HostAllowList) -> Result<OutboundResponse, EgressError>`; `OutboundRequest { method: Method, url: Url, basic_auth: Option<(String, SecretString)>, json_body: Option<Vec<u8>>, form: Option<Vec<(String, String)>> }`; `OutboundResponse { status: StatusCode, body: Vec<u8> }`, body read incrementally and capped at 16 MiB (`EgressError::TooLarge`). Every request is checked with `check_url` first; a 3xx response is returned as a status, never followed.
  - `fn is_public_ip(ip: IpAddr) -> bool`: false for loopback, private (10/8, 172.16/12, 192.168/16), link-local (169.254/16, fe80::/10), CGNAT 100.64/10, ULA fc00::/7, unspecified, multicast, broadcast, documentation ranges, IPv4-mapped forms of all of these.
  - `enum EgressError { HostNotAllowed, NotHttps, Resolution, Timeout, TooLarge, Transport, Config(&'static str) }`; messages carry no URL, query, header or body (reqwest errors are reduced with `without_url()` before being kept as `#[source]`).
  - testkit only: `Egress::for_tests() -> Egress` and `HostAllowList::loopback(port)` that also accept `http://127.0.0.1:<port>`; neither exists in a non-testkit build.
- Dockerfile: copy `/etc/ssl/certs/ca-certificates.crt` from the builder stage into the final image and set `ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt`; replace the comment saying coffer makes no outbound TLS connections. The bundle updates with the digest-pinned base image (Dependabot).

- [ ] **Step 1: Write failing tests.** `check_url_rules` (accepts `https://bridge.example/x`; rejects `http://`, IP literal hosts, `:8443`, user information, hosts not listed, `bridge.example.evil.test`); `is_public_ip_table` (each range above, plus `8.8.8.8` true, `::ffff:127.0.0.1` false); `proxy_config_validation` (`HTTPS_PROXY=http://user:pw@proxy:3128` accepted and `format!("{config:?}")` lacks `pw`; `ftp://` rejected; `http://proxy/path` rejected); integration with a loopback mock in testkit mode: `redirects_are_not_followed` (mock answers 302 to another path → `OutboundResponse.status == 302`, the target never hit); `oversized_body_refused`; `errors_do_not_leak_url_secrets` (request to `/claim/SECRETPART?token=SECRETQ` failing with connection refused → `to_string()` and `{:?}` of the error contain neither `SECRETPART` nor `SECRETQ`).
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** Confirm `cargo tree -i aws-lc-rs` is empty and `cargo deny check` passes; if the licence check trips on a target-specific crate (for example `webpki-root-certs` for wasm), stop and raise it with the owner rather than allowing a licence.
- [ ] **Step 4: Run** full gate and `docker buildx build --platform linux/arm64 .` → PASS; security review.
- [ ] **Step 5: Commit** `feat(server): allow-listed https egress client`.

### Task 10: Provider credential vault **[security]**

**Files:**
- Modify: `Cargo.toml` (chacha20poly1305), `crates/server/Cargo.toml`, `crates/server/src/secrets.rs`
- Create: `crates/server/src/vault.rs`
- Test: unit tests in `vault.rs`

**Interfaces:**
- Produces: `struct CredentialVault` (key zeroized on drop; `Debug` redacted): `CredentialVault::new(key: &ServerKey) -> Self` using `ServerKey::derive_subkey(&self, context: &str) -> Zeroizing<[u8; 32]>` (new, public within the crate) with context `coffer 2026-10 provider credentials v1`.
- `fn seal(&self, connection: ConnectionId, plaintext: &SecretString) -> Result<SealedCredentials, VaultError>`: XChaCha20-Poly1305, 24-byte nonce from `getrandom`, associated data `b"coffer connection credentials\0" || connection.as_uuid().as_bytes()`; output `0x01 || nonce || ciphertext_and_tag`.
- `fn open(&self, connection: ConnectionId, sealed: &SealedCredentials) -> Result<SecretString, VaultError>`; `struct SealedCredentials(Vec<u8>)` (`Debug` prints only the length); `enum VaultError { Malformed, UnknownVersion, Decrypt, Random }` (no detail beyond the variant).

- [ ] **Step 1: Write failing tests.** `seal_open_roundtrip`; `sealing_twice_differs` (fresh nonce); `tampered_byte_fails` (flip each of: version, a nonce byte, a ciphertext byte, the tag → `Err`); `wrong_connection_fails`; `other_server_key_fails`; `unknown_version_rejected`; `debug_never_prints_plaintext_or_ciphertext`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** full gate → PASS; security review.
- [ ] **Step 5: Commit** `feat(server): sealed provider credentials`.

### Task 11: Provider interface, connections and the sync engine **[security]**

**Files:**
- Create: `crates/server/src/sync/{provider,engine,connections}.rs`, `crates/server/src/http/api/provider_accounts.rs`
- Modify: `Cargo.toml` (jiff feature `tzdb-bundle-always`), `crates/server/src/sync/mod.rs`, `crates/server/src/config.rs` (`timezone`), `crates/server/src/http/mod.rs` (`AppState.sync`), `crates/server/src/testkit.rs` (`FakeProvider`), `crates/store/src/{connections,provider_accounts}.rs`, `crates/store/src/sync_runs.rs` (new)
- Test: `crates/server/tests/sync_engine.rs`

**Interfaces:**
- Config: `Config.timezone: jiff::tz::TimeZone` from `COFFER_TIMEZONE` (IANA name, default `UTC`; unknown → `ConfigError::Invalid`). jiff's `tzdb-bundle-always` feature embeds the database (the image has no `/usr/share/zoneinfo`).
- Produces (`sync::provider`):
  - `enum ProviderKind { SimpleFin }` (`#[non_exhaustive]`; serde/`FromStr` `"simplefin"`).
  - `struct Capabilities { pub pending: bool, pub mode: FetchMode, pub min_interval: SignedDuration, pub min_scheduled_interval: SignedDuration, pub daily_budget: u32 }`; `enum FetchMode { Window { max_days: i64, overlap_days: i64 }, Cursor }`.
  - `#[non_exhaustive] enum ConnectInput { SimpleFinSetupToken(SecretString) }`.
  - `struct FetchRequest<'a> { pub credentials: &'a SecretString, pub window_start: Option<Date>, pub cursor: Option<&'a str>, pub timezone: &'a TimeZone }`.
  - `struct FetchResult { pub accounts: Vec<ProviderAccountData>, pub connection_errors: Vec<String>, pub cursor: Option<String> }`; `struct ProviderAccountData { pub provider_account_id: String, pub name: String, pub institution: Option<String>, pub currency: String, pub kind_hint: Option<AccountKind>, pub balance: Option<(Date, DecimalAmount)>, pub transactions: Vec<ProviderTransaction>, pub removed: Vec<String>, pub coverage: Coverage, pub error: Option<String> }`; `struct ProviderTransaction { pub provider_id: String, pub posted_on: Date, pub transacted_on: Option<Date>, pub amount: DecimalAmount, pub description: String, pub payee: Option<String>, pub memo: Option<String>, pub pending: bool, pub replaces_provider_id: Option<String> }`.
  - `enum ProviderError { ReauthRequired, RateLimited, Unavailable(String), Invalid(String), Egress(EgressError) }`.
  - `type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;`
  - `trait Provider: Send + Sync { fn kind(&self) -> ProviderKind; fn capabilities(&self) -> Capabilities; fn connect<'a>(&'a self, input: ConnectInput) -> ProviderFuture<'a, Result<SecretString, ProviderError>>; fn fetch<'a>(&'a self, request: FetchRequest<'a>) -> ProviderFuture<'a, Result<FetchResult, ProviderError>>; fn disconnect<'a>(&'a self, credentials: &'a SecretString) -> ProviderFuture<'a, Result<(), ProviderError>>; }` — providers know nothing of categories, review, audit or storage (spec §6.2).
  - `struct ProviderRegistry` (`HashMap<ProviderKind, Arc<dyn Provider>>`), `get(kind) -> Option<Arc<dyn Provider>>`.
- Produces (`sync::engine`): `enum SyncTrigger { Schedule, Manual, Initial }`; `ConnectionView { id, provider, label, owner, status, last_error, last_attempt_at, last_success_at, next_sync_at, accounts: Vec<ProviderAccountView> }`; `struct SyncService { store, clock, vault, registry, config }` with:
  - `async fn create_connection(&self, actor: &Actor, kind: ProviderKind, label: ConnectionLabel, input: ConnectInput) -> Result<ConnectionView, SyncError>` (`ManageConnections`; `connect` → `vault.seal` → insert, audit `connection.create` with label and provider only; enqueues an `initial` run).
  - `async fn remove_connection(&self, actor, ConnectionId)` (`RemoveConnection { owner }`; calls `disconnect`, then clears `credentials`, status `removed`, keeps accounts, transactions and bindings; audit `connection.remove`).
  - `async fn request_sync(&self, actor: Option<&Actor>, ConnectionId, trigger: SyncTrigger) -> Result<SyncRunView, SyncError>`: refuses with `SyncError::RateLimited { retry_after }` when runs started in the last 24 h ≥ `daily_budget`, or the last run started less than `min_interval` ago (manual) / `min_scheduled_interval` (schedule); records the run `queued` (audit `sync.request` with member and agent for manual triggers).
  - `async fn run(&self, run: SyncRunId) -> Result<SyncRunView, SyncError>`: opens credentials, computes `window_start` (`Window`: first run → today − `max_days`; else date of `last_success_at` − `overlap_days`, both in `config.timezone`), calls `fetch`, then for each provider account in its own `StoreTx`: upsert the provider account; if it has an error, a currency `Currency::parse` rejects, or a currency different from its bound Account's, record the error on it and skip; otherwise convert amounts with `DecimalAmount::to_minor_units`, `apply_account_batch`, and for a bound account write the day's balance snapshot (source `sync`). Connection status: `ReauthRequired` → `reauth_required`; other provider errors → `error` with a sanitized `last_error`; success → `active`, `last_success_at`, cursor saved. Run status `succeeded`, `partial` (some accounts errored) or `failed`.
  - `async fn bind(&self, actor, ProviderAccountId, AccountId)` (`ManageConnections` and `Write` on the account; same currency or 409 `currency_mismatch`; then `apply_held`; audit `provider_account.bind`) and `async fn create_account_and_bind(&self, actor, ProviderAccountId, NewLinkedAccountInput { name: Option, kind: Option (else `kind_hint`, else 422), liquidity, include_* })`.
  - `fn sanitize_provider_message(raw: &str) -> String`: control characters removed, any `scheme://user:pass@` user information replaced by `scheme://***@`, truncated to 500 chars. Every provider text that reaches storage, logs or responses passes through it.
- Produces (`testkit`): `captured_log_text() -> String` — the existing global `Signals` layer also appends every event's message and fields to a process-wide buffer, so secret-leak tests can search everything logged; `FakeProvider` (scripted `VecDeque<Result<FetchResult, ProviderError>>`, records each `FetchRequest`'s window and cursor) registered under `ProviderKind::SimpleFin` by `TestServer::with_fake_provider()`.
- API: `GET /api/v1/provider-accounts?unbound` → `{ items: Vec<ProviderAccountView { id, connection, name, institution, currency, kind_hint, account: Option<AccountId>, balance: Option<{ on, amount_raw }>, error, last_seen_at }> }`; `POST /api/v1/provider-accounts/{provider_account_id}/bind` `{ account }`; `POST /api/v1/provider-accounts/{provider_account_id}/create-account` `NewLinkedAccountRequest`. New `ProblemCode`s `CurrencyMismatch`, `SyncRateLimited` (429 with `Retry-After`), `ProviderError` (502), `ReauthRequired` (409).

- [ ] **Step 1: Write failing tests** (with `FakeProvider`): `first_window_is_max_then_overlap` (first request `window_start = today − 90`, after success `last_success − 14`); `new_provider_accounts_appear_unbound_and_hold_records` (no transactions created; bind creates them); `bind_requires_same_currency`; `currency_problem_isolated_to_one_account` (account A currency `"https://custom"`, account B bound with `EUR` vs Account `USD`, account C fine → A and B carry errors, C committed, run `partial`); `account_failure_rolls_back_only_that_account` (a store failure injected mid-account leaves no partial rows for it); `reauth_marks_connection`; `provider_error_message_sanitized` (`"failed for https://u:p@bridge.example/x\u{7}"` stored as `"failed for https://***@bridge.example/x"`); `balance_snapshot_written_for_bound_accounts`; `rate_limit_refuses_with_retry_after` (17th request within 24 h, or a manual request 30 min after the last → `SyncError::RateLimited` with the right `retry_after`); `credentials_never_stored_in_clear` (after `create_connection`, the raw `connections.credentials` bytes do not contain the plaintext, neither does `captured_log_text()` or any audit row, and the `connection.create` changes contain only `label` and `provider`); `member_cannot_remove_others_connection`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** full gate → PASS; security review.
- [ ] **Step 5: Commit** `feat(sync): provider interface, connections and sync engine`.

### Task 12: SimpleFin provider **[security]**

**Files:**
- Create: `crates/server/src/sync/simplefin.rs`, `crates/server/tests/fixtures/simplefin/{accounts_first.json,accounts_posted.json,accounts_errors.json}`, `crates/server/src/http/api/connections.rs`, `crates/server/src/http/pages/connections.rs`
- Modify: `crates/server/src/config.rs` (`simplefin_hosts`), `crates/server/src/sync/provider.rs` (registration), `crates/server/src/testkit.rs` (`MockUpstream`), `crates/server/src/http/pages/{mod,layout}.rs` (nav link)
- Test: `crates/server/tests/simplefin.rs`, `crates/server/tests/pages.rs`

**Interfaces:**
- Config: `Config.simplefin_hosts: HostAllowList` from `COFFER_SIMPLEFIN_HOSTS`, default `bridge.simplefin.org,beta-bridge.simplefin.org`.
- Produces: `struct SimpleFin { egress: Egress, hosts: HostAllowList }` implementing `Provider` with `Capabilities { pending: true, mode: Window { max_days: 90, overlap_days: 14 }, min_interval: 1 h, min_scheduled_interval: 2 h, daily_budget: 16 }` (SimpleFin allows about 24 requests a day; every sync is one request).
  - `connect(SimpleFinSetupToken)`: token ≤ 2048 bytes, standard base64 → UTF-8 → claim URL; `check_url(claim, hosts)`; `POST` with an empty body; 200 body (≤ 2048 bytes) is the access URL, which must pass `check_url` once its user information is removed and must carry a username and password. 403 → `ProviderError::Invalid("setup token already claimed or invalid")`. Returns the access URL as the credential.
  - `fetch`: `GET <access URL without user information>/accounts?start-date=<unix seconds of window_start 00:00 in the timezone>&pending=1` with basic auth from the user information (percent-decoded). 401/403 → `ReauthRequired`; 402 → `Unavailable("SimpleFin subscription inactive")`; 429 → `RateLimited`. Parsing follows the SimpleFin protocol (`errors` → `connection_errors`; per account: `id`, `name`, `org.name` → institution, `currency`, `balance` and `balance-date`; transactions: `id`, `posted` (0 or `pending: true` → pending), `amount`, `description`, `payee`, `memo`, `transacted_at`); amounts stay `DecimalAmount` strings; Unix timestamps become dates in `request.timezone`; a pending transaction's date is `transacted_at`, else `posted`, else the request day. `coverage = Window { start: window_start }`. Unknown fields ignored; a malformed account becomes that account's `error`, never a whole-run failure.
  - `disconnect`: no network call (SimpleFin access is revoked at the bridge); returns `Ok(())`.
- Produces (`testkit`): `MockUpstream::start(router: axum::Router) -> MockUpstream { pub base: Url }` on `127.0.0.1:0`, recording requests (method, path, query, `Authorization` present); `TestServer::with_simplefin(mock: &MockUpstream)` registers a `SimpleFin` built with `Egress::for_tests()` and `HostAllowList::loopback(port)`.
- API: `POST /api/v1/connections/simplefin` (`ManageConnections`; `Idempotency-Key`) body `{ label, setup_token }` → 201 `ConnectionView`. `setup_token` is a `SecretString` field: never logged, never in errors. New `ProblemCode::SetupTokenInvalid` (422).
- Page: `GET /connections` (session) lists connections (provider, label, status, last sync, errors, provider accounts) with a form `POST /connections/simplefin` (label, setup token; CSRF) and per-connection `POST /connections/{id}/sync` and `POST /connections/{id}/remove`. Same CSP as every page.
- Fixtures: generic (institution "Example Bank", accounts "Checking", "Credit Card"), shaped like the protocol document's example response; `accounts_posted.json` repeats the first with one pending record replaced by its posted form under a new id; `accounts_errors.json` carries a top-level error string and an account with currency `https://example.com/points`.

- [ ] **Step 1: Write failing tests** (against `MockUpstream`):
  - `claim_stores_sealed_access_url`: mock claim returns `http://user:pass@127.0.0.1:<port>/simplefin`; connection created; `connections.credentials` bytes lack `user:pass`; response JSON lacks `pass`; audit lacks it.
  - `setup_token_for_unlisted_host_refused` (token decoding to `https://internal.example/claim/x`, to `https://10.0.0.1/claim/x` and to `https://bridge.simplefin.org.evil.test/claim/x` → 422 `setup_token_invalid`, no request made, nothing stored).
  - `access_url_for_unlisted_host_refused` (claim succeeds but returns an access URL on another host → 422, nothing stored).
  - `claim_redirect_not_followed` (claim answers 302 → 422, redirect target never requested).
  - `setup_token_never_echoed` (invalid token `SECRETTOKEN...` → body, `captured_log_text()` and the audit log lack it).
  - `fetch_sends_window_and_basic_auth` (query `start-date` equals the expected Unix seconds, `pending=1`, `Authorization` present, URL path has no user information).
  - `sync_from_fixtures_then_posted_keeps_state`: sync `accounts_first`, bind both accounts, categorize the pending one, sync `accounts_posted` → one transaction, posted, still categorized.
  - `posted_date_uses_household_timezone`: `COFFER_TIMEZONE=America/Los_Angeles`, record `posted` at 2026-10-31T06:30Z → `posted_on == 2026-10-30`.
  - `errors_fixture_reports_connection_and_account_errors`.
  - `reauth_on_403`; `rate_limited_on_429`.
  - `amounts_parsed_exactly` (`"-1234567.89"` → -123456789).
  - Pages: `connections_page_lists_and_adds` (session, CSRF, form post creates a connection), `connections_post_without_csrf_rejected`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** full gate → PASS; security review (SSRF, credential handling).
- [ ] **Step 5: Commit** `feat(sync): simplefin provider and connections page`.

### Task 13: Scheduling, on-demand sync, connection status and operator docs

**Files:**
- Create: `crates/server/src/sync/scheduler.rs`
- Modify: `crates/server/src/config.rs` (`sync_interval`), `crates/server/src/cli.rs` (`serve_with` spawns the worker and scheduler), `crates/server/src/http/api/connections.rs`
- Modify docs: `docs/configuration.md`, `docs/agent-usage.md`, `docs/threat-model.md`, `docs/container.md`, `README.md`, `AGENTS.md`
- Test: unit tests in `scheduler.rs`; `crates/server/tests/connections_api.rs`

**Interfaces:**
- Config: `Config.sync_interval: SignedDuration` from `COFFER_SYNC_INTERVAL_HOURS` (default 6, 1..=24); the effective interval per connection is `max(sync_interval, capabilities.min_scheduled_interval)`.
- Produces (`sync::scheduler`): `fn due(now: Timestamp, connections: &[ScheduleEntry { id, status, last_attempt_at, effective_interval }]) -> Vec<ConnectionId>` (status `active` or `error`; `last_attempt_at` absent or `+ interval <= now`; `reauth_required` and `removed` never due); `fn spawn(state: AppState, shutdown) -> JoinHandle<()>`: one worker drains queued runs one at a time (a run interrupted by shutdown leaves only whole accounts committed), and a ticker every 60 s calls `request_sync(None, id, Schedule)` for due connections (rate-limit refusals logged at `debug`).
- API:
  - `GET /api/v1/connections` → `{ items: Vec<ConnectionView> }` (`ReadHousehold`; provider accounts bound to accounts outside the actor's `Balances` scope are omitted).
  - `POST /api/v1/connections/{connection_id}/sync` → 202 `SyncRunView { id, connection, trigger, status, requested_by: Option<AttributionView>, queued_at, started_at, finished_at, detail }` or 429 `sync_rate_limited` with `Retry-After`.
  - `GET /api/v1/connections/{connection_id}/sync-runs?after&limit` → `{ items, next }`.
  - `DELETE /api/v1/connections/{connection_id}` → 204 (`RemoveConnection`).
- Docs:
  - `configuration.md`: new variables (`COFFER_TIMEZONE`, `COFFER_SIMPLEFIN_HOSTS`, `COFFER_SYNC_INTERVAL_HOURS`, `HTTPS_PROXY`); an **Outbound network** section: coffer is no longer offline; it connects only to the hosts in `COFFER_SIMPLEFIN_HOSTS` (by default `bridge.simplefin.org` and `beta-bridge.simplefin.org`, port 443) plus, later, Plaid's and the FX source's; DNS must resolve them to public addresses; allow-list them in an egress proxy (set `HTTPS_PROXY`; CONNECT to those hosts only) or an SNI allow-list; redirects are never followed; `NO_PROXY`/`HTTP_PROXY`/`ALL_PROXY` are ignored. Losing the server key now also means reconnecting every provider.
  - `agent-usage.md`: scopes table updated (`write` covers accounts, transactions, categories, connections and sync); the review workflow (list `review=unreviewed`, batch categorize/review/assign, merge flagged duplicates); amounts and signs; idempotency keys; sync rate limits and `Retry-After`.
  - `threat-model.md`: provider credentials as an asset; trust boundary "coffer to data providers"; the SSRF design (allow-list, no redirects, public-address resolution); sealed credentials and their key; accepted risk: a compromised SimpleFin Bridge can feed false data but not reach other hosts.
  - `container.md`: the image now contains a CA bundle; egress requirement.
  - `README.md`: status updated (accounts, transactions, SimpleFin).
  - `AGENTS.md`: crate map (`ledger`, `sync`, `egress`, `vault`; domain `money`, `accounts`, `transactions`, `matching`), conventions (money, `authorize_account` and `AccessResolver`, outbound HTTP only via `Egress`, provider text through `sanitize_provider_message`), security review triggers include CSP and provider code.

- [ ] **Step 1: Write failing tests.** `due_rules` (table over statuses and times, including exactly at `last_attempt + interval`); `connections_list_shows_status_and_errors`; `sync_trigger_attributed_to_agent` (202; audit `sync.request` with token; worker runs it; run `succeeded`); `second_trigger_within_min_interval_429`; `read_token_cannot_trigger_sync` (403); `delete_connection_keeps_transactions` (transactions remain, status `removed`, credentials column `NULL`).
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement**, then the docs. **Step 4: Run** full gate, plus a manual smoke: `serve` with a fresh data dir, create a manual account and transaction with `curl`, check `/api/v1/openapi.json`.
- [ ] **Step 5: Commit** `feat(sync): scheduling, on-demand sync and connection status`.
- [ ] **Step 6: Whole-branch security review** (`security-review` skill) covering egress, vault, SimpleFin, policy and pages; fix findings; commit.
