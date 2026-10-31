# coffer plan 2a — Typed API, accounts, transactions, providers and SimpleFin Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every `/api/v1` endpoint typed and described by a generated OpenAPI 3.1 document; manual and linked accounts with balance snapshots; canonical transactions with source records, splits, categories, review and assignment; a provider-agnostic sync engine with the matching pipeline; SimpleFin as the first provider; and the agent-facing API to review, categorise, assign and sync.

**Architecture:** `coffer-domain` gains pure modules for money, accounts, transactions and matching, plus per-account authorization (`authorize_account` with an `AccountAccess` value) so per-token grants (plan 2b) change only the policy and `AccessResolver`. `coffer-store` gains repositories and migrations `0003`–`0006`. `coffer-server` gains a `ledger` service (accounts, categories, transactions, duplicates), `egress` (the only outbound HTTP client: HTTPS, per-provider host allow-lists, public addresses only, no redirects, optional `HTTPS_PROXY`), `vault` (XChaCha20-Poly1305 sealing of provider credentials under a key derived from the server key), and `sync` (provider trait and registry, apply step, engine, connections service, scheduler, SimpleFin).

**Tech Stack:** as plan 1, plus utoipa 6.0 (`macros`, `uuid`, `jiff_0_2`) and utoipa-axum 0.3; reqwest 0.13 (`default-features = false`, features `rustls-no-provider`, `http2`); rustls 0.23 (`default-features = false`, features `ring`, `std`, `tls12`) and rustls-platform-verifier 0.7; chacha20poly1305 0.11; jiff feature `tzdb-bundle-always`. For every new crate, and every new transitive one (for example `openssl-probe`), take the newest version at least 7 days old, verified with `python3 -I .github/scripts/crate_age.py --base main`, pinning with `cargo update -p <crate> --precise <version>` where needed.

**Spec:** `docs/superpowers/specs/2026-10-30-coffer-design.md` (§3, §4, §5, §6.1 accounts/transactions/categories, §6.2, §6.3 role matrix, §6.4, §6.5, §6.6, §6.11, §6.13, §7), plus the addendum §8 written by Task 1. Issues: #4 (SimpleFin), #5 (per-account grants: policy shape here, grants in plan 2b), #6 (typed OpenAPI).

**Depends on:** the design-system plan (`2026-10-31-coffer-02-design-system.md`) merged first (owner ruling D4). It owns migration `0002_appearance.sql`, `Action::ManageHousehold`, the `/admin/household` page, `crates/server/src/testkit/mod.rs` (formerly `testkit.rs`), `testkit::browser::Browser`, content-hashed assets under `crates/server/assets/`, the layout (`layout::{page, Frame, Nav}`, `empty_state`), themed `PageError` pages and the top-level `security_headers` layer. This plan numbers its migrations from `0003`, adds no household-settings action or page (household settings stay deferred, ruling O6; `COFFER_TIMEZONE` moves to `/admin/household` in plan 3, ruling O5), and builds its one page with the design system's layout.

**Plan split.** The owner's plan 2 scope is two plans: this one (2a) and `2026-10-31-coffer-03b-plaid-grants.md` (2b: per-token account grants, then Plaid). 2b depends on 2a.

**Deferred to plan 3** (ruling O6): household settings, rules and suggested rules, transfer links, a starter category set, unbinding a provider account. Nothing in this plan's API changes shape to add them.

## Global Constraints

- `rust-toolchain.toml` `channel = "1.99"`; edition 2024; workspace lints unchanged (`unsafe_code = "forbid"`, clippy `all` + `pedantic` deny, `unwrap_used`/`expect_used`/`print_*`/`todo`/`dbg_macro` deny). No `#[allow]`/`#[expect]` without `reason = "..."`.
- "every amount is an integer in **minor units** plus its ISO 4217 currency; exponent from ISO 4217 (USD 2, JPY 0, etc.)" "Sign convention: negative = money out of the account." No `f32`/`f64` anywhere on a money path, including parsing provider JSON.
- Time only through `Clock`; instants are `jiff::Timestamp` (stored as Unix ms `INTEGER`), calendar days are `jiff::civil::Date` (stored as `TEXT` `YYYY-MM-DD`). Providers never read a clock: the engine hands them `today`. Ids are UUIDv7 newtypes from `coffer_domain::ids`.
- `sqlx` stays in `coffer-store` only; no `sqlx` type in its public API; runtime-checked queries; new tables are `STRICT`. Migrations are `0003`–`0006` (`0002` belongs to the design system).
- Every mutation goes through `Store::begin` (`BEGIN IMMEDIATE`) together with its audit entry; the audit log stays append-only. Sync changes, including a single split following an imported amount change, are attributed `system:sync`.
- Every handler authorizes through `coffer_domain::policy` (`authorize` for household actions, `authorize_account` for anything tied to an account, with `AccountAccess` from `AccessResolver` only). `Denied` becomes a `Problem` in exactly one place (`impl From<Denied> for Problem`).
- "Credential leakage. SimpleFin access URLs, session cookies or tokens in logs, API responses, error messages or the repo. Tokens stored in clear." Provider credentials exist in clear only in memory (`SecretString`/`Zeroizing`), never in the database, audit, logs, errors or responses.
- Every `/api/v1` route is registered through `ApiRouter` (Task 3) with a utoipa path; no `serde_json::Value` in any request, query, path or response type; every error is `application/problem+json` with a `ProblemCode`; request rejections use fixed messages that name at most a documented parameter, never a supplied value or name.
- Outbound HTTP only through `egress::Egress`. Tests make no network calls: providers are tested against recorded, generic fixtures served by a loopback mock.
- Logs: no secrets, no transaction descriptions or amounts at `info` (spec §6.13).
- "Manual accounts may also hold manually created transactions, which have no external id and are never touched by sync" (§6.11).
- No dead code, no TODO comments. Tests specify behaviour; no tests that restate implementation.
- Public repository: nothing specific to the owner (banks, categories, hosts) in code, fixtures or docs.
- Supply chain: new crate versions at least 7 days old (crate-age), licences within `deny.toml`, `cargo deny check` clean; fix, don't exempt.
- Builds use the configured `CARGO_TARGET_DIR`; never override it.
- Full gate (Definition of done): `cargo fmt --check && cargo clippy --workspace --all-targets --all-features --locked -- -D warnings && cargo test --workspace --all-features --locked && RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked && cargo deny check`. A task that changes the API regenerates `docs/api/openapi.json` with `cargo run -p coffer-server -- openapi > docs/api/openapi.json`.
- Every task that adds a `ProblemCode` edits `crates/server/src/http/problem.rs`; every task that adds a module edits the owning `lib.rs`/`mod.rs`; every task that adds an API route edits `crates/server/src/http/api/mod.rs`. These wiring files are implied in each task's Files.
- Security review (`security-review` skill, findings fixed) on every task marked **[security]**.
- Commits: conventional commit subjects; no `Co-Authored-By`, `Claude-Session` or `Generated with` trailers. The PR description notes: add `openapi-lint` to the required checks after merge (owner, ruling O9); update issue #4's endpoint name to `POST /api/v1/connections/simplefin`.

## Review Focus

1. **Two identical purchases on the same day** (same account, amount, date and merchant, distinct provider ids): two transactions, never merged. → Task 8 test `identical_same_day_purchases_stay_distinct`.
2. **A pending card charge that posts with a tip** (amount up to 20 % higher, new provider id, the pending record gone in the same fetch): the posted record attaches to the pending one's transaction, which keeps its id and user state. → Task 8 tests `pending_with_tip_attaches_to_posted` and `pending_to_posted_keeps_transaction_id_without_user_state`.
3. **A transaction posted at 23:30 local time**: its date is the household's local day (`COFFER_TIMEZONE`), not the UTC day. → Task 14 test `posted_date_uses_household_timezone`.
4. **An agent retrying a create concurrently with the same `Idempotency-Key`**, or reusing a key with a different body: one resource, then replay; a different body is refused. → Task 5 tests `idempotent_retry_replays_response`, `idempotency_key_reuse_with_other_body_refused`, `concurrent_same_key_creates_once`.
5. **One provider account with a non-ISO currency or a currency different from its bound Account**: that account reports an error, every other account commits. → Task 12 test `currency_problem_isolated_to_one_account`.

---

### Task 1: Spec addendum

**Files:**
- Modify: `docs/superpowers/specs/2026-10-30-coffer-design.md` (append `## 8. Addendum (2026-10-31)`; amend §3 and §5 by pointing to it)

**Interfaces:** none (documentation). Later tasks and plan 2b cite §8.

- [ ] **Step 1: Write §8**, each subsection a few sentences of decisions:
  - **8.1 Typed API now.** The OpenAPI 3.1 document (§6.11, §6.14) is generated from code from plan 2 on (issue #6), served unauthenticated at `/api/v1/openapi.json`, committed at `docs/api/openapi.json`, compared by a test and linted in CI. Problem `type` is `urn:coffer:problem:<code>`. Document → router is tested per operation and method; router → document holds by construction: `ApiRouter` is the only way to add an API route, and the application router merges nothing else under `/api`.
  - **8.2 Per-account authorization.** §3's "no tables or code for it" is superseded for agent tokens (issue #5): every account-scoped decision goes through `authorize_account(actor, access, account, level)` with levels `balances < transactions < write`. Plan 2a resolves every actor to uniform access at its scope; plan 2b adds per-token grants. Per-member visibility remains a non-goal. Hidden accounts answer exactly like missing ones (404); a visible account with too low a level answers 403 naming the level needed.
  - **8.3 Provider credentials.** Sealed with XChaCha20-Poly1305 under a key derived (BLAKE3 `derive_key`, context `coffer 2026-10 provider credentials v1`) from the server key in `COFFER_SECRET_KEY_FILE`; the connection id is the associated data; a version byte allows a dedicated key later. Losing the server key means reconnecting every provider. A connection's credentials can be replaced (reconnect) without losing its bindings.
  - **8.4 Matching v1.** Description key: uppercase ASCII letters, every other character a separator, the first two tokens of three or more letters. Fingerprint `fp1:<amount minor units>:<description key>`. A candidate is a canonical transaction of the same Account, imported (manual transactions are never candidates), dated within 4 days, with no live source from the same connection, and the same amount; or, when all its live sources are pending records that vanished from the current fetch, an amount within 20 % and the same non-empty description key (a pending charge posting with a tip). One candidate attaches; several are narrowed to an equal description key and amount, and if still ambiguous the record becomes a new transaction flagged as a possible duplicate. Removal of vanished sources happens after matching. A pending record still present beside its posted form produces a flagged possible duplicate, never a silent second transaction. When an imported amount changes and the transaction has one split, that split follows the amount (a `system:sync` change); with several splits they stay and the transaction is flagged.
  - **8.5 Outbound network.** §5's "limited to SimpleFin and the FX rate source" now reads: SimpleFin Bridge hosts (`COFFER_SIMPLEFIN_HOSTS`, default `bridge.simplefin.org`, `beta-bridge.simplefin.org`), Plaid's API host when Plaid is configured (plan 2b), the FX source (plan 3). HTTPS on port 443 only, no redirects, targets must resolve to public addresses. `HTTPS_PROXY` is honoured when set; then coffer does not resolve target hosts, and the public-address check is the proxy's job. The exact host list lives in `docs/configuration.md`.
  - **8.6 Plaid** (plan 2b): a second provider behind the same interface; Link runs on one page whose CSP alone also allows `https://cdn.plaid.com`; environments `sandbox` and `production` (Plaid retired `development` in 2024); `/transactions/sync` cursors; `/accounts/get` balances; `/item/remove` on disconnect; 730 days of history requested.
  - **8.7 Household timezone.** Provider timestamps become calendar days in `COFFER_TIMEZONE` (IANA name, default `UTC`), an environment variable until household settings arrive (plan 3).
  - **8.8 Deferred.** Rules (a §2 goal), transfer links, household settings, a starter category set and unbinding a provider account move to plan 3.
- [ ] **Step 2:** In §3 append to the per-account bullet "(superseded for agent tokens by §8.2)"; replace "The provider interface (§6.2) is in v1; other implementations are not." with "... other implementations are not, except Plaid (§8.6)."; in §5 append "(extended by §8.5)" to the external network line.
- [ ] **Step 3: Commit** `docs(spec): addendum for typed api, account authz, providers and egress`.

### Task 2: Typed audit changes

**Files:**
- Modify: `crates/domain/src/audit.rs`, `crates/store/src/audit.rs`, every audit emitter (`crates/server/src/auth/{identity,agent_tokens,sessions}.rs`, `crates/server/src/appearance.rs`), `docs/agent-usage.md`
- Create: `crates/store/migrations/0003_audit_changes_version.sql`
- Test: unit tests in `crates/domain/src/audit.rs` and `crates/store/src/audit.rs`; existing server tests updated

**Interfaces:**
- Produces (`coffer_domain::audit`):
  - `enum AuditValue { Null, Bool(bool), Int(i64), Text(String) }` with `From` for `bool`, `i64`, `&str`, `String`, every id type, `Timestamp` and `civil::Date` (as `Text`, RFC 3339 / ISO), `Option<T: Into<AuditValue>>` (`None` → `Null`).
  - `struct FieldChange { pub before: AuditValue, pub after: AuditValue }`.
  - `struct AuditChanges(BTreeMap<String, FieldChange>)` with `new()`, `set(self, field: &str, value) -> Self` (before `Null`), `change(self, field, before, after) -> Self`, `unset(self, field, before) -> Self` (after `Null`), `iter()`, `is_empty()`. Serde: a JSON object `{field: {"before": v, "after": v}}`.
  - `AuditEntry.changes: AuditChanges` (replaces `serde_json::Value`).
- Produces (store): `AuditEvent.changes: AuditChanges`. New rows have `changes_version = 2`. Version-1 rows (0.1.0 and the design system's emitters) are converted on read: if `action` is `member.role` or `household.theme` and the object is exactly `{from, to}`, it becomes `change("role" | "theme", from, to)`; a field whose value is an object with exactly `from` and `to` becomes `change(field, from, to)`; any other scalar becomes `set(field, value)`; a non-scalar becomes `Text(<compact JSON>)`.
- Migration `0003`: `ALTER TABLE audit_events ADD COLUMN changes_version INTEGER NOT NULL DEFAULT 1 CHECK (changes_version IN (1, 2));` (an `ALTER`, so the append-only triggers do not fire).

- [ ] **Step 1: Write failing tests.** Domain: `changes_serialize_as_before_after_objects` (`AuditChanges::new().set("name", "x").change("role", "member", "admin")` → `{"name":{"before":null,"after":"x"},"role":{"before":"member","after":"admin"}}`); `option_none_is_null`. Store: `changes_roundtrip_typed`; `legacy_rows_are_read_as_typed_changes` (raw v1 rows: `{"label":{"from":"k","to":null}}`; `{"from":"member","to":"admin"}` with action `member.role`; `{"from":null,"to":"mochi"}` with action `household.theme`; `{"from":"a","to":"b"}` with action `other.action` → `set("from","a")`, `set("to","b")`; `{"username":"bob"}` → read back as the typed forms above).
- [ ] **Step 2: Run** `cargo test -p coffer-domain -p coffer-store` → FAIL.
- [ ] **Step 3: Implement**, then convert every emitter to `AuditChanges` keeping the field names; `member.role` records field `role`, `household.theme` field `theme`; invite changes record `kind`, `role`, `display_name`, `expires_at` as separate fields. `agent-usage.md`: `changes` is now `{field: {before, after}}` (it was `from`/`to` for some actions).
- [ ] **Step 4: Run** the full gate → PASS (update assertions in `crates/server/tests/*.rs` that read `changes`).
- [ ] **Step 5: Commit** `refactor(audit): typed before/after changes`.

### Task 3: OpenAPI document, typed problems and the existing endpoints

**Files:**
- Modify: `Cargo.toml` (utoipa, utoipa-axum), `crates/domain/Cargo.toml` (feature `openapi = ["dep:utoipa"]`), `crates/domain/src/{ids,identity,audit}.rs` (schemas behind `openapi`), `crates/server/Cargo.toml` (enables `coffer-domain/openapi`)
- Create: `crates/server/src/http/api/router.rs` (`ApiRouter`), `crates/server/src/http/api/extract.rs`, `crates/server/src/http/api/openapi.rs`, `docs/api/openapi.json`, `.spectral.yaml`, `.github/spectral/Dockerfile`
- Modify: `crates/server/src/http/problem.rs`, `crates/server/src/http/mod.rs`, `crates/server/src/http/api/{mod,me,members,audit}.rs`, `crates/server/src/cli.rs` (subcommand `openapi`), `.github/workflows/ci.yml` (job `openapi-lint`), `.github/dependabot.yml` (docker, directory `/.github/spectral`), `docs/agent-usage.md`, `AGENTS.md`
- Test: `crates/server/tests/openapi.rs`; `crates/server/tests/api.rs` updated

**Interfaces:**
- Produces (`http::problem`):
  - `enum ProblemCode` (serde `snake_case`, `ToSchema`): `Unauthenticated, Forbidden, CsrfInvalid, CrossOrigin, NotFound, MethodNotAllowed, Invalid, InvalidCursor, Conflict, InviteUnusable, CeremonyExpired, Busy, PasskeyRejected, Internal`; later tasks add variants. `Problem.code: ProblemCode`; `Problem.retry_after: Option<u32>` (seconds; `IntoResponse` emits `Retry-After`), with `with_retry_after(self, seconds)`.
  - `struct ProblemBody { r#type: String, title: String, status: u16, code: ProblemCode, detail: Option<String> }` (`ToSchema`, the only error body); `type` = `urn:coffer:problem:<code>`.
  - `#[derive(IntoResponses)] enum CommonProblems` documenting 400, 401, 403, 404, 405, 422, 500 as `application/problem+json` `ProblemBody`.
  - `impl From<Denied> for Problem` (the single mapping; `AuthError::Denied` delegates to it).
- Produces (`http::api::router`): `struct ApiRouter(OpenApiRouter<AppState>)` with `new() -> Self`, `op(self, routes: utoipa_axum::router::UtoipaMethodRouter<AppState>) -> Self` (the only way to add an API route) and `split(self) -> (Router<AppState>, utoipa::openapi::OpenApi)`. `http::api::router()` returns only `ApiRouter::split()`'s router, and `http::router` merges nothing else under `/api`.
- Produces (`http::api::extract`): `TypedJson<T>`, `TypedQuery<T>` (serde_urlencoded; structs use `#[serde(deny_unknown_fields)]`, so unknown and repeated parameters are rejected), `TypedPath<T>`. Every rejection is `400 invalid` with a fixed detail: for a query or path, "the `<name>` parameter is not valid" when the name is a documented field of `T`, otherwise "a query parameter is not recognised or is repeated"; for a body, "the request body is not valid JSON for this operation". The serde message is logged at `debug` only, never returned. Body limit for `/api/v1`: 256 KiB.
- Produces (`http::api::openapi`): `fn document() -> utoipa::openapi::OpenApi` (title `coffer`, version = crate version, bearer security scheme `bearer` on every operation except `GET /api/v1/openapi.json`); `GET /api/v1/openapi.json` serves it.
- Produces (domain, feature `openapi`): `ToSchema` for every id (string, `format: uuid`, description "UUIDv7"), `Role`, `Scope`, `AuditValue` (`oneOf` null/boolean/integer/string), `FieldChange`, `AuditChanges` (object of `FieldChange`).
- Response types (`ToSchema`, `Serialize`): `MeResponse { member: MemberView, agent: AgentView }`, `MemberView { id, username, display_name, role, disabled_at: Option<Timestamp> }`, `AgentView { id, name, scope }`, `MemberList { items: Vec<MemberView> }`, `AuditQuery` (`IntoParams`: `entity_type, entity_id, member, agent, from, to, after, limit`), `AuditPage { items: Vec<AuditEventView>, next: Option<String> }`, `AuditEventView { id, at, actor: AuditActorView, action, entity_type, entity_id, changes: AuditChanges }`, `enum AuditActorView` tagged `kind`: `member { member, agent: Option<AgentTokenId> }` | `system { reason }`. JSON stays as today except `type` and Task 2's `changes`.
- Rule for later tasks: small bounded collections (members, accounts, categories, connections, provider accounts) return `{ items }`; unbounded ones (audit, transactions, balances, sync runs) return `{ items, next }` with an opaque cursor.
- CLI: `Command::Openapi` is a non-configured subcommand beside `Healthcheck`: it prints the pretty-printed document plus a trailing newline and reads no configuration.

- [ ] **Step 1: Write failing tests** in `tests/openapi.rs`:
  - `snapshot_matches_generated_document`: `cli::run(["coffer","openapi"].map(OsString::from), |_| None, &mut out, &mut err)` exits 0 and `out` equals `docs/api/openapi.json`; the failure message names the regeneration command.
  - `every_documented_operation_is_routed`: for each path and method in `document()`, send a request (path parameters replaced by fresh UUIDv7s or a valid date, a read token) through `http::router` wrapped in a test layer that copies `MatchedPath` into a response header; the header equals the documented path.
  - `documented_methods_match_routed_methods`: `TRACE` on each documented path → 405 whose `Allow` lists exactly the documented methods (plus `HEAD` where `GET` is documented).
  - `no_untyped_schemas`: no schema in the document is empty or `additionalProperties: true` without a `type`, `$ref`, `oneOf`, `anyOf` or `allOf`.
  - `every_error_response_is_problem_json`: every non-2xx response has only `application/problem+json` content referencing `ProblemBody`.
  - `openapi_served_without_auth`: `GET /api/v1/openapi.json` with no token → 200 `application/json`, body equals `document()`.
  - In `tests/api.rs`: `problem_type_is_urn_of_code`; `query_rejection_does_not_echo` (`/api/v1/audit?limit=SECRETVALUE`, `?SECRETNAME=1`, `?member=SECRETVALUE` → 400 `invalid`, body free of `SECRET`); `path_rejection_does_not_echo` (exercised from Task 5 on with `/api/v1/accounts/SECRETVALUE`; here with a unit test of `TypedPath` over a one-field struct); `json_rejection_does_not_echo` (unit tests of `TypedJson` with an unknown field `SECRETNAME`, a wrong-type value `"SECRETVALUE"` and an unknown enum variant `"SECRETVALUE"` → fixed detail, no `SECRET`); `retry_after_header_emitted` (`Problem` with `retry_after = 30` → `Retry-After: 30`).
- [ ] **Step 2: Run** `cargo test -p coffer-server --test openapi --test api` → FAIL.
- [ ] **Step 3: Implement.** Retrofit `me`, `members`, `audit` with `#[utoipa::path]` and the types above (audit keeps its filter semantics through `TypedQuery<AuditQuery>`). Generate and commit `docs/api/openapi.json`.
- [ ] **Step 4: Lint.** `.spectral.yaml` extends `spectral:oas` with no rule disabled. `.github/spectral/Dockerfile` is `FROM stoplight/spectral:<newest release at least 7 days old>@sha256:<digest>`. CI job `openapi-lint` (`permissions: contents: read`, checkout with `persist-credentials: false`): `docker build -t spectral .github/spectral` then `docker run --rm -v "$PWD:/work:ro" -w /work spectral lint docs/api/openapi.json --fail-severity warn`. Fix every finding in code (descriptions, operation ids, tags), never in the ruleset. `GH_TOKEN=$(gh auth token) uvx zizmor .` → no findings.
- [ ] **Step 5: Docs.** `docs/agent-usage.md`: point to `/api/v1/openapi.json`; document `type` URNs and `Retry-After`. `AGENTS.md` conventions: API routes only via `ApiRouter`; no `serde_json::Value` in API types; regenerate the snapshot with `coffer openapi`.
- [ ] **Step 6: Run** full gate → PASS.
- [ ] **Step 7: Commit** `feat(api): generated openapi document and typed problems`.

### Task 4: Domain — money, accounts and per-account authorization **[security]**

**Files:**
- Create: `crates/domain/src/money.rs`, `crates/domain/src/accounts.rs`
- Modify: `crates/domain/src/ids.rs`, `crates/domain/src/identity.rs` (`label_type!` gains a maximum length), `crates/domain/src/policy.rs`, `crates/server/src/auth/mod.rs` (`denied_message`), `crates/server/src/http/problem.rs` (`From<Denied>` arms)
- Test: unit and property tests in those files

**Interfaces:**
- Produces (`ids`): `AccountId, CategoryGroupId, CategoryId, TransactionId, SplitId, ConnectionId, ProviderAccountId, SourceRecordId, SyncRunId`.
- Produces (`money`):
  - `struct Currency([u8; 3])`: `Currency::parse(&str) -> Result<Currency, MoneyError>` accepts only uppercase codes in the built-in ISO 4217 table (active codes with their minor-unit exponents, copied from the ISO 4217 list; funds and precious-metal codes excluded); `exponent(self) -> u8`; `as_str`; serde as the code.
  - `struct Money { pub minor_units: i64, pub currency: Currency }` (serde `{ "minor_units": -1234, "currency": "USD" }`), `checked_add`.
  - `struct DecimalAmount(String)`: `parse(&str)` accepts `-?[0-9]{1,18}(\.[0-9]{1,18})?` only (no exponent, no `+`, no spaces); `negated(&self) -> DecimalAmount`; `as_str`; `to_minor_units(&self, Currency) -> Result<i64, MoneyError>` — exact; extra fractional digits allowed only if zeros; overflow is an error.
  - `fn format_minor_units(minor: i64, currency: Currency) -> String` (`-1234`, USD → `"-12.34"`).
  - `enum MoneyError { UnknownCurrency, Malformed, TooPrecise, Overflow }`; messages never echo input.
- Produces (`accounts`):
  - `enum AccountKind` (`checking, savings, credit_card, loan, mortgage, brokerage, retirement, equity_comp, property, vehicle, other_asset, other_liability`) and `enum LiquidityClass` (`cash, short_term_liability, invested, illiquid, long_term_liability`), serde `snake_case`, `FromStr`, `as_str`.
  - `fn default_liquidity(kind) -> LiquidityClass`: checking/savings → cash; credit_card → short_term_liability; brokerage/retirement/equity_comp → invested; property/vehicle/other_asset → illiquid; loan/mortgage/other_liability → long_term_liability. (The spec fixes cash accounts and credit cards; the other rows follow its class definitions.)
  - `AccountName` and `ConnectionLabel` via `label_type!(…, max = 100)` (`DisplayName`, `TokenName` stay at 64).
- Produces (`policy`):
  - `enum AccountLevel { Balances, Transactions, Write }` (`Ord`, serde `snake_case`): `Balances` = the account and its balances; `Transactions` adds its transactions and source records; `Write` adds every change to them.
  - `#[non_exhaustive] enum AccountAccess { Uniform(AccountLevel) }` (plan 2b adds grants) with `level_for(&self, AccountId) -> Option<AccountLevel>` and `scope(&self, at_least: AccountLevel) -> AccountScope`.
  - `enum AccountScope { All, Only(Vec<AccountId>) }` — the store filters every account-scoped query by it.
  - `fn account_access_for_scope(actor: &Actor) -> AccountAccess`: effective scope `Read` → `Uniform(Transactions)`, `Write`/`Admin` → `Uniform(Write)`.
  - New `Action` variants: `AccessAccount { needed: AccountLevel }` (used only inside `Denied` from `authorize_account`), `WriteHousehold` (categories; `Scope::Write`), `ManageConnections` (add or reconnect a connection, trigger sync, bind provider accounts; `Scope::Write`), `RemoveConnection { owner: MemberId }` (`Scope::Write` and `actor.member == owner`, or effective scope `Admin`; also governs reconnecting).
  - `fn authorize_account(actor: &Actor, access: &AccountAccess, account: AccountId, needed: AccountLevel) -> Result<(), Denied>` returning `Denied { action: AccessAccount { needed }, reason }` with reason `AccountHidden` (no level) or `AccountLevel { needed }` (level too low).
  - New `DenyReason` variants `AccountHidden`, `AccountLevel { needed: AccountLevel }`. `denied_message` gains "this action needs <level> access to this account"; `From<Denied> for Problem` maps `AccountHidden` to the exact `not_found` problem a missing resource gets, `AccountLevel` to 403 `forbidden` with that detail.

- [ ] **Step 1: Write failing tests.**
  - `currency_exponents_follow_iso4217` (USD 2, JPY 0, KWD 3, EUR 2); `currency_rejects_lowercase_unknown_and_urls` (`"usd"`, `"XYZ"`, `"https://x"`).
  - `decimal_to_minor_units_is_exact` (`"-12.34"` USD → -1234; `"12.340"` → 1234; `"12.345"` → `TooPrecise`; `"1e3"`, `"+1"`, `" 1"` → `Malformed`; `"100"` JPY → 100; `"92233720368547758.08"` USD → `Overflow`).
  - proptest `format_then_parse_roundtrips` over `i64` in ±10^15 and currencies with exponents 0, 2, 3.
  - `default_liquidity_table` (the rows above).
  - `labels_respect_their_maximum` (`AccountName` 100 ok, 101 rejected; `DisplayName` still 64).
  - `read_token_reads_transactions_but_cannot_write`, `write_token_writes_any_account`, `member_cannot_remove_others_connection_but_admin_can`, `read_scope_cannot_manage_connections`.
  - In `problem.rs`: `hidden_account_problem_equals_not_found` (the serialized `Problem` from `Denied { AccessAccount, AccountHidden }` is byte-identical to the one for `AuthError::NotFound`).
- [ ] **Step 2: Run** `cargo test -p coffer-domain -p coffer-server --lib` → FAIL.
- [ ] **Step 3: Implement.** Schemas behind `openapi` for every new public type (`Money` documented as integer minor units plus ISO 4217 code).
- [ ] **Step 4: Run** full gate → PASS; security review of `policy.rs` and the `Denied` mapping.
- [ ] **Step 5: Commit** `feat(domain): money, account classification and per-account authorization`.

### Task 5: Accounts, balance snapshots and idempotency keys

**Files:**
- Create: `crates/store/migrations/0004_accounts.sql`, `crates/store/src/{accounts,balances,idempotency}.rs`
- Create: `crates/server/src/ledger/{mod,accounts}.rs`, `crates/server/src/auth/access.rs`, `crates/server/src/http/idempotency.rs`, `crates/server/src/http/api/accounts.rs`
- Modify: `crates/server/src/http/mod.rs` (`AppState.ledger`, `AppState.access`), `crates/server/src/cli.rs` (`open_state` builds them), `crates/server/src/testkit/mod.rs`, `crates/server/src/secrets.rs` (`ServerKey::derived` becomes `pub(crate)`)
- Test: `crates/store/tests/ledger.rs`, `crates/server/tests/accounts.rs`

**Interfaces:**
- Schema `0004`: `accounts(id, name, kind CHECK(...), currency TEXT CHECK (length(currency) = 3), liquidity CHECK(...), liquidity_overridden INTEGER, include_in_net_worth INTEGER, include_in_forecast INTEGER, closed_at NULL, created_at)`; `balance_snapshots(account_id FK, on_date TEXT, balance_minor INTEGER, source CHECK IN ('sync','manual'), recorded_at, PRIMARY KEY (account_id, on_date))`; `idempotency_keys(member_id FK, key TEXT, request_hash BLOB, state CHECK IN ('pending','done'), status INTEGER NULL, response_json TEXT NULL, created_at, PRIMARY KEY (member_id, key))`.
- Store: `StoreTx::insert_account(NewAccount) -> AccountRecord`, `StoreTx::update_account(AccountId, AccountChanges) -> AccountRecord`, `Store::get_account(AccountId)`, `Store::list_accounts(&AccountScope, include_closed: bool) -> Vec<AccountRecord>`; `StoreTx::upsert_balance(BalanceSnapshot)` (last write wins per day), `Store::list_balances(AccountId, from: Option<Date>, to: Option<Date>, &PageRequest) -> Page<BalanceSnapshot>` (newest first), `Store::latest_balance(AccountId) -> Option<BalanceSnapshot>`; idempotency: `StoreTx::reserve_idempotency(member, key, hash, now) -> Reservation { Fresh | Replay { status, response_json } | Mismatch | InProgress }` (purges rows older than 24 h first; a `pending` row older than 5 minutes counts as abandoned and is replaced: its create rolled back, since completion commits with the create), `StoreTx::complete_idempotency(member, key, status, response_json)`.
- Server:
  - `http::api::extract::TypedJsonBytes<T> { value: T, bytes: Bytes }`: `TypedJson` that also keeps the raw body (same rejections), for idempotent creates.
  - `auth::access::AccessResolver` with `async fn resolve(&self, actor: &Actor) -> Result<AccountAccess, AuthError>`; in this plan it returns `account_access_for_scope(actor)`. Every account-scoped service call obtains access through it (plan 2b changes only this resolver and the policy).
  - `ledger::Ledger { store, clock, access }`; `ledger::LedgerError { Denied(Denied), NotFound, Invalid(&'static str), Conflict(ProblemCode, &'static str), Store(StoreError) }` with `From<LedgerError> for Problem` (the `Denied` arm delegates to `From<Denied>`).
  - `http::idempotency::IdempotencySlot { member, key }` and `async fn reserve(state, actor, key: Option<IdempotencyKey>, method: &Method, path: &str, body: &[u8]) -> Result<Reserved, Problem>` with `enum Reserved { None, Fresh(IdempotencySlot), Replay(Response) }`. The request hash is BLAKE3 keyed with `ServerKey::derived("coffer 2026-10 idempotency request")` over `u32 len ‖ method ‖ u32 len ‖ path ‖ body` (keyed because a body may hold a setup token). `Mismatch` → 422 `idempotency_key_reuse`; `InProgress` → 409 `idempotency_in_progress`. Services that create take `slot: Option<IdempotencySlot>` and, inside their own `StoreTx`, after writing the resource and its audit entry, call `complete_idempotency` with the status and the serialized response, so all three commit together. `IdempotencyKey` header extractor: `Idempotency-Key`, 1..=128 visible ASCII.
  - `Ledger::create_account(actor, NewAccountInput, slot) -> Result<AccountView, LedgerError>` (`WriteHousehold`); `update_account(actor, AccountId, AccountPatch)` (`authorize_account(.., Write)`; currency immutable; setting `kind` without `liquidity` re-defaults liquidity unless overridden); `get_account`, `list_accounts(actor, include_closed)` (scope `Balances`); `set_manual_balance(actor, AccountId, Date, i64)` (`Write`; allowed on linked accounts too, and the next sync overwrites the same day's snapshot — last write wins per §6.1); `list_balances(actor, AccountId, ..)` (`Balances`). Audit `account.create`, `account.update`, `balance.set`.
- API (`TypedJson`/`TypedJsonBytes`/`TypedPath`, all documented):
  - `GET /api/v1/accounts?include_closed` → `AccountList { items: Vec<AccountView> }`.
  - `POST /api/v1/accounts` (`Idempotency-Key` optional) body `NewAccountRequest { name, kind, currency, liquidity: Option, include_in_net_worth: Option<bool> (default true), include_in_forecast: Option<bool> (default true), opening_balance: Option<{ on: Date, minor_units: i64 }> }` → 201 `AccountView`.
  - `GET /api/v1/accounts/{account_id}`; `PATCH /api/v1/accounts/{account_id}` body `AccountPatch { name?, kind?, liquidity?, include_in_net_worth?, include_in_forecast?, closed? }`.
  - `GET /api/v1/accounts/{account_id}/balances?from&to&after&limit` → `BalancePage { items: Vec<BalanceView { on, balance: Money, source }>, next }`.
  - `PUT /api/v1/accounts/{account_id}/balances/{on}` body `{ minor_units }` → `BalanceView`.
  - `AccountView { id, name, kind, currency, liquidity, liquidity_overridden, include_in_net_worth, include_in_forecast, closed: bool, latest_balance: Option<BalanceView>, created_at }`.

- [ ] **Step 1: Write failing tests.** Store: `balance_last_write_wins_per_day`, `list_accounts_honours_scope_and_closed`, `idempotency_completion_rolls_back_with_its_tx` (reserve, then a tx that completes and is dropped → the row is still `pending`). Server (router, write token): `create_manual_account_defaults_liquidity` (`credit_card` → `short_term_liability`, `source == "manual"`); `opening_balance_recorded_as_manual_snapshot`; `currency_is_immutable` (PATCH with `currency` → 400 `invalid`); `path_rejection_does_not_echo` (`/api/v1/accounts/SECRETVALUE` → 400 without `SECRET`); `read_token_cannot_create_account` (403); `account_changes_audited_with_agent`; `idempotent_retry_replays_response` (same key and body twice → same id, one account, one audit entry); `idempotency_key_reuse_with_other_body_refused` (422 `idempotency_key_reuse`); `concurrent_same_key_creates_once` (two concurrent requests → one 201 and one 201 replay or 409 `idempotency_in_progress`; exactly one account).
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** full gate (snapshot regenerated) → PASS.
- [ ] **Step 5: Commit** `feat: manual accounts, balance snapshots and idempotency keys`.

### Task 6: Categories and the transaction model

**Files:**
- Create: `crates/domain/src/transactions.rs`, `crates/store/migrations/0005_transactions.sql`, `crates/store/src/{categories,transactions}.rs`, `crates/server/src/ledger/{categories,transactions}.rs`, `crates/server/src/http/api/categories.rs`
- Test: `crates/domain/src/transactions.rs` (unit), `crates/store/tests/ledger.rs`, `crates/server/tests/categories.rs`, `crates/server/tests/transactions_core.rs`

**Interfaces:**
- Domain (`transactions`): `enum ReviewStatus { Unreviewed, Reviewed }`; `enum CategoryKind { Expense, Income, Transfer }`; `enum TransactionOrigin { Manual, Imported }`; `enum TransactionFlag { PossibleDuplicate, SourceRemoved, SplitMismatch }`; `struct SplitInput { category: CategoryId, minor_units: i64 }`; `fn validate_splits(amount_minor: i64, splits: &[SplitInput]) -> Result<(), SplitError>` (empty is valid = uncategorized; otherwise 1..=50 splits summing exactly to the amount, no zero split); `struct UserState { splits: bool, notes: bool, reviewed: bool, assignee: bool, payee_edited: bool }` with `is_empty()`; text limits: payee 1..=200, notes 0..=2000, description 1..=500, no control characters except `\n` in notes.
- Schema `0005`: `category_groups(id, name, position, archived_at, created_at)`; `categories(id, group_id FK, name, kind CHECK, position, archived_at, created_at)`; `transactions(id, account_id FK, origin CHECK IN ('manual','imported'), posted_on, transacted_on NULL, pending INTEGER, amount_minor, currency, raw_description, imported_payee NULL, payee NULL, payee_edited INTEGER, memo NULL, notes NULL, review CHECK, assignee_id NULL FK, reviewed_by_member NULL, reviewed_by_agent NULL, reviewed_at NULL, categorized_by_member NULL, categorized_by_agent NULL, categorized_by_system NULL, categorized_at NULL, flag_possible_duplicate, flag_source_removed, flag_split_mismatch, created_at, updated_at)`, indexes `(account_id, posted_on)` and `(review, assignee_id)`; `transaction_splits(id, transaction_id FK ON DELETE CASCADE, category_id FK, minor_units, position)`.
- Store: categories (`insert_category_group`, `update_category_group`, `insert_category`, `update_category`, `list_categories() -> Vec<CategoryGroupRecord>` with their categories); `StoreTx::insert_transaction(NewTransaction { origin, account, posted_on, transacted_on, pending, amount_minor, currency, raw_description, imported_payee, memo, notes, created_at }) -> TransactionRecord` (used for manual transactions here and imported ones from Task 8), `StoreTx::update_user_fields(TransactionId, UserFieldChanges) -> TransactionRecord`, `StoreTx::replace_splits(TransactionId, &[SplitInput], Attribution)`, `StoreTx::delete_transaction(TransactionId)`, `Store::get_transaction(TransactionId) -> TransactionRecord` (with splits). `struct Attribution { member: Option<MemberId>, agent: Option<AgentTokenId>, system: Option<String>, at: Timestamp }`.
- Server:
  - `Ledger::{list_categories, create_category_group, update_category_group, create_category, update_category}` (`ReadHousehold` / `WriteHousehold`; creates take an `IdempotencySlot`; archived categories cannot receive new splits: 422 `invalid`). Audit `category_group.*`, `category.*`.
  - `Ledger::create_manual_transaction(actor, NewManualTransactionInput, slot) -> TransactionView` (account must be open, `Write` on it; currency from the account; Task 8 adds the refusal for linked accounts); `Ledger::update_transaction(actor, TransactionId, TransactionPatch) -> TransactionView` (`Write`); `Ledger::delete_transaction(actor, TransactionId)` (manual only, else 409 `conflict`); `Ledger::get_transaction` (`Transactions`). Audit `transaction.create`, `transaction.update` (changed fields only), `transaction.delete`.
  - `TransactionPatch { payee: Patch<String>, notes: Patch<String>, review: Option<ReviewStatus>, assignee: Patch<MemberId>, splits: Option<Vec<SplitInput>> }` where `enum Patch<T> { Absent, Clear, Set(T) }` deserializes from absent / `null` / value. `payee: Clear` restores the imported payee and clears `payee_edited`. The assignee must be an active member. Setting `review: reviewed` records `reviewed_by`; changing splits records `categorized_by`; a no-op patch writes no audit entry.
  - `TransactionView { id, account, amount: Money, posted_on, transacted_on, pending, description, payee, memo, notes, review, assignee, splits: Vec<SplitView { id, category, amount: Money }>, flags: Vec<TransactionFlag>, origin, reviewed_by: Option<AttributionView>, categorized_by: Option<AttributionView>, created_at, updated_at }`; `AttributionView { member, agent, system, at }`.
- API: `GET /api/v1/categories` → `CategoryTree { items: Vec<CategoryGroupView { id, name, position, archived, categories: Vec<CategoryView> }> }`; `POST /api/v1/category-groups`, `PATCH /api/v1/category-groups/{group_id}`, `POST /api/v1/categories`, `PATCH /api/v1/categories/{category_id}` (creates accept `Idempotency-Key`). Transaction endpoints come in Task 7.

- [ ] **Step 1: Write failing tests.** Domain: `splits_must_sum_to_amount` (off by one → error; empty ok; 51 → error; zero split → error); proptest `any_partition_of_the_amount_validates`. Store: `deleting_transaction_cascades_splits`. Server (service level, `TestServer`): `categorize_records_attribution_with_agent`, `review_records_reviewer`, `payee_clear_restores_imported` (seed an imported transaction with `insert_transaction`), `archived_category_rejected`, `assignee_must_be_active_member`, `manual_transaction_refused_on_closed_account`, `delete_imported_refused` (409), `no_op_patch_not_audited`, `category_crud_via_api_audited`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** full gate → PASS.
- [ ] **Step 5: Commit** `feat: categories, splits and the transaction review model`.

### Task 7: Transactions API — list, filter, review in batches

**Files:**
- Create: `crates/server/src/http/api/transactions.rs`
- Modify: `crates/store/src/transactions.rs` (filtered listing), `crates/server/src/ledger/transactions.rs` (batch), `docs/agent-usage.md` (search semantics)
- Test: `crates/server/tests/transactions_api.rs`

**Interfaces:**
- Store: `struct TransactionFilter { scope: AccountScope, accounts: Vec<AccountId>, from: Option<Date> /*incl.*/, to: Option<Date> /*excl.*/, review: Option<ReviewStatus>, assignee: Option<AssigneeFilter /* Member(MemberId) | Unassigned */>, categories: Vec<CategoryId>, uncategorized: bool, pending: Option<bool>, flag: Option<TransactionFlag>, min_minor: Option<i64>, max_minor: Option<i64>, text: Option<String> }`; `Store::list_transactions(&TransactionFilter, &PageRequest) -> Page<TransactionRecord>` ordered `posted_on DESC, id DESC`; cursor opaque (`<date>~<id>`, anything else `InvalidCursor`). `text` matches description, payee and notes with `LIKE` wildcards escaped; case-insensitive for ASCII letters only (SQLite `LIKE`), documented in `agent-usage.md`.
- Server: `Ledger::list_transactions(actor, TransactionQueryInput)` (scope `Transactions`; an explicitly requested account the actor cannot see → 404 as if missing); `Ledger::apply_batch(actor, Vec<BatchOperation { id: TransactionId, patch: TransactionPatch }>) -> Vec<TransactionView>` (1..=200 ops, distinct ids, one store transaction, all or nothing; an error names only the failing index: `operations[3]: ...`).
- API:
  - `GET /api/v1/transactions` with `TransactionQuery` (`IntoParams`): `account` (comma-separated ids), `from`, `to`, `review`, `assignee` (member id or `none`), `category` (comma-separated), `uncategorized`, `pending`, `flag`, `min_amount`, `max_amount` (minor units), `q` (≤ 100 chars), `after`, `limit` → `TransactionPage { items: Vec<TransactionView>, next }`.
  - `POST /api/v1/transactions` (manual account; `Idempotency-Key`) body `NewTransactionRequest { account, posted_on, minor_units, description, payee?, memo?, notes? }` → 201.
  - `GET|PATCH|DELETE /api/v1/transactions/{transaction_id}` (`PATCH` body `TransactionPatch`).
  - `POST /api/v1/transactions/batch` body `{ operations: Vec<BatchOperation> }` → `{ items: Vec<TransactionView> }`.

- [ ] **Step 1: Write failing tests** (router, agent tokens): `filters_combine` (manual transactions across 2 accounts, 3 categories, reviewed/unreviewed, assigned/unassigned; each filter alone and `review=unreviewed&assignee=<bob>&account=<a>` together return exactly the expected ids); `uncategorized_filter`; `text_search_escapes_wildcards` (`q=50%` matches only a description containing `50%`); `pagination_is_stable_across_equal_dates` (5 transactions on one date, `limit=2` pages through all exactly once); `bad_cursor_is_400_invalid_cursor`; `enum_query_value_not_echoed` (`review=SECRETVALUE` → 400 without `SECRET`); `batch_is_atomic` (op 2 references an archived category → 422 naming `operations[1]`, op 1 unchanged); `batch_review_and_assign_attributed_to_agent`; `read_token_cannot_patch` (403).
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** full gate → PASS.
- [ ] **Step 5: Commit** `feat(api): transaction listing, filters and batch review`.

### Task 8: Source records and the matching pipeline

**Files:**
- Create: `crates/domain/src/matching.rs`, `crates/store/migrations/0006_sync.sql`, `crates/store/src/{connections,provider_accounts,sources}.rs`, `crates/server/src/sync/{mod,apply}.rs`
- Modify: `crates/server/src/ledger/transactions.rs` (`TransactionView.sources`; manual transactions refused on linked accounts), `crates/server/src/ledger/accounts.rs` (`AccountView.source`), `crates/store/src/accounts.rs` (`account_is_linked`)
- Test: unit/property tests in `matching.rs`; `crates/server/tests/matching.rs`

**Interfaces:**
- Domain (`matching`), the rule of spec §8.4:
  - `const FINGERPRINT_VERSION: u8 = 1; const DATE_TOLERANCE_DAYS: i64 = 4; const PENDING_SUCCESSOR_TOLERANCE_PERCENT: i64 = 20;`
  - `fn description_key(description: &str) -> String` (`"SQ *BLUE BOTTLE 1234"` → `"BLUE BOTTLE"`; no qualifying token → `""`).
  - `fn fingerprint(amount_minor: i64, description: &str) -> String` = `"fp1:<amount_minor>:<description_key>"`.
  - `struct Incoming { posted_on: Date, amount_minor: i64, description_key: String }`.
  - `struct Candidate { transaction: TransactionId, posted_on: Date, amount_minor: i64, description_key: String, live_source_from_same_connection: bool, only_vanished_pending: bool }`.
  - `enum MatchDecision { Attach(TransactionId), New, NewPossibleDuplicate(Vec<TransactionId>) }`.
  - `fn decide(incoming: &Incoming, candidates: &[Candidate]) -> MatchDecision`:
    ```text
    eligible = candidates where !live_source_from_same_connection
               and |posted_on - incoming.posted_on| <= DATE_TOLERANCE_DAYS
               and (amount equal
                    or (only_vanished_pending and description keys equal and non-empty
                        and |amount - incoming| * 100 <= |candidate amount| * 20))
    if eligible is empty -> New
    if exactly one -> Attach(it)
    narrowed = eligible with equal non-empty description key and equal amount
    if exactly one -> Attach(it) else NewPossibleDuplicate(eligible ids)
    ```
- Schema `0006`: `connections(id, provider TEXT /* validated on read; no CHECK, so providers are added without a table rebuild */, label, owner_id FK, credentials BLOB NULL, provider_state TEXT NULL, status CHECK IN ('active','error','reauth_required','removed'), last_error NULL, last_attempt_at NULL, last_success_at NULL, created_at, removed_at NULL)`; `provider_accounts(id, connection_id FK, provider_account_id, name, institution NULL, currency_raw, kind_hint NULL, account_id NULL FK /* binding */, last_balance_raw NULL, last_balance_on NULL, error NULL, first_seen_at, last_seen_at, UNIQUE(connection_id, provider_account_id))`; `transaction_sources(id, connection_id FK, provider_account_ref FK provider_accounts, provider_txn_id, transaction_id NULL FK, posted_on, transacted_on NULL, amount_raw TEXT, amount_minor NULL, description, payee NULL, memo NULL, pending INTEGER, fingerprint, fingerprint_version, first_seen_at, last_seen_at, UNIQUE(connection_id, provider_txn_id))`; `duplicate_candidates(transaction_id FK ON DELETE CASCADE, candidate_id FK ON DELETE CASCADE, PRIMARY KEY (transaction_id, candidate_id))`; `sync_runs(id, connection_id FK, trigger CHECK IN ('schedule','manual','initial'), requested_by_member NULL, requested_by_agent NULL, status CHECK IN ('queued','running','succeeded','partial','failed'), detail NULL, queued_at, started_at NULL, finished_at NULL)`.
- Store primitives (on `StoreTx` unless noted): connections and provider accounts — `insert_connection(NewConnection { provider: &str, label, owner, credentials: Vec<u8>, created_at }) -> ConnectionRecord`, `Store::get_connection`, `Store::list_connections() -> Vec<ConnectionRecord>`, `update_connection_status(ConnectionId, ConnectionStatusChange)`, `set_connection_credentials(ConnectionId, Option<Vec<u8>>)`, `set_provider_state(ConnectionId, Option<String>)`, `upsert_provider_account(ProviderAccountUpsert) -> ProviderAccountRecord`, `set_provider_account_error(ProviderAccountId, Option<String>)`, `bind_provider_account(ProviderAccountId, AccountId)`, `Store::list_provider_accounts(Option<ConnectionId>) -> Vec<ProviderAccountRecord>`; sources — `find_source(ConnectionId, &str) -> Option<SourceRecord>`, `insert_source(NewSource) -> SourceRecord`, `update_source(SourceRecordId, ImportedFields)`, `attach_source(SourceRecordId, TransactionId)`, `delete_source(SourceRecordId)`, `sources_of(TransactionId) -> Vec<SourceRecord>`, `pending_sources_since(ProviderAccountId, Date) -> Vec<SourceRecord>`, `held_sources(ProviderAccountId) -> Vec<SourceRecord>`; matching — `match_candidates(account, date_from, date_to) -> Vec<CandidateRow>` (imported transactions only, with their sources), `update_imported_fields(TransactionId, ImportedFields)`, `set_flag(TransactionId, TransactionFlag, bool)`, `add_duplicate_candidates(TransactionId, &[TransactionId])`, `clear_duplicate_candidates(TransactionId)`.
- Server (`sync::mod`): `enum SyncError { Denied(Denied), NotFound, Invalid(&'static str), Store(StoreError) }` with `From<SyncError> for Problem` (`Denied` via `From<Denied>`); later tasks add variants.
- Server (`sync::apply`):
  - `struct AccountBatch { provider_account: ProviderAccountRecord, account: Option<AccountRecord>, transactions: Vec<NormalizedTransaction>, removed: Vec<String>, coverage: Coverage }`; `NormalizedTransaction { provider_id, posted_on, transacted_on, amount_minor, amount_raw, description, payee, memo, pending, replaces_provider_id: Option<String> }`; `enum Coverage { Window { start: Date }, Delta }`.
  - `async fn apply_account_batch(tx: &mut StoreTx, now: Timestamp, connection: ConnectionId, batch: AccountBatch) -> Result<ApplyReport, SyncError>`, in this order:
    1. **Vanished set** (computed, nothing deleted yet): sources named in `removed`, plus for `Window` every pending source of this provider account with `posted_on >= start` absent from the batch.
    2. **Match every incoming record.** Same `(connection, provider_id)` → update the source and, unless the transaction is posted and the record pending, its imported fields. Else a `replaces_provider_id` naming a source of this connection (vanished or not) → attach to that source's transaction. Else `matching::decide` over `match_candidates`, where `live_source_from_same_connection` ignores vanished sources and `only_vanished_pending` means every live source of the candidate is pending and vanished. `NewPossibleDuplicate` sets the flag and records candidates. A transaction that gains a source has `SourceRemoved` cleared. Imported-field updates never touch user fields; if the amount changes and the transaction has one split, that split follows (attributed `system:sync`); with several, they stay and `SplitMismatch` is set. For an unbound provider account, sources are stored with `transaction_id NULL` (held) and nothing else happens.
    3. **Remove leftovers.** Each vanished source is detached and deleted. A transaction left without sources is deleted when its `UserState` is empty, otherwise kept and flagged `SourceRemoved`.
    Each created, attached, updated or removed transaction is audited (`transaction.import`, `transaction.attach_source`, `transaction.imported_update`, `transaction.source_removed`, `transaction.delete`) with actor `System { reason: "system:sync" }`; unchanged records write nothing.
  - `async fn apply_held(tx, now, ProviderAccountRecord, AccountRecord) -> Result<ApplyReport, SyncError>` runs held sources through step 2.
- `Store::account_is_linked(AccountId) -> bool` (has a bound provider account); `AccountView` gains `source: "manual" | "linked"`; `Ledger::create_manual_transaction` refuses a linked account with 409 `conflict`.
- `TransactionView` gains `sources: Vec<SourceView { id, connection, provider_account, pending, posted_on, amount_raw, first_seen_at }>` and `duplicate_candidates: Vec<TransactionId>`.

- [ ] **Step 1: Write failing tests.** Domain: `description_key_examples` (above, and `"POS 12/03"` → `"POS"`); `exact_amount_within_tolerance_attaches`; `outside_tolerance_is_new` (5 days); `two_candidates_same_key_flagged`; `live_same_connection_source_never_candidate`; proptest `decide_never_attaches_to_ineligible`. Server (apply step; connections, provider accounts and bindings created with the store primitives above):
  - `reimport_is_idempotent` (same batch twice → same transactions, no audit entries the second time).
  - `user_state_survives_reimport` (categorize, review, assign, edit payee, add notes; re-import with changed description and date → user fields unchanged, imported fields updated).
  - `pending_to_posted_keeps_transaction_id_without_user_state` (pending record in batch 1; batch 2 has its posted form under a new id and lacks the pending → same transaction id, one source, `SourceRemoved` clear, no `transaction.source_removed` audit).
  - `pending_with_tip_attaches_to_posted` (pending 40.00 categorized with one split; posted 46.00, same key, new id → same transaction, split 46.00).
  - `replaces_provider_id_attaches_to_vanished_source` (posted record names the pending id, which is also in `removed` → attaches).
  - `pending_and_posted_both_present_flagged` (the pending record still present beside its posted form → the posted one is a new transaction flagged `PossibleDuplicate` with the pending one as candidate).
  - `identical_same_day_purchases_stay_distinct` (two 5.00 records same day, same description, distinct ids, one batch → two transactions; re-import → still two).
  - `vanished_pending_without_state_deleted_with_state_flagged`.
  - `multi_split_amount_change_flags_mismatch`.
  - `manual_transactions_never_matched` (a provider account bound to a manual account holding an identical manual transaction → the import creates a separate transaction; the manual one is untouched).
  - `held_sources_matched_by_apply_held`.
  - `linked_account_refuses_manual_transactions` (after `bind_provider_account`, `POST /api/v1/transactions` on that account → 409; `AccountView.source == "linked"`).
  - `deleting_a_candidate_cascades` (deleting a transaction listed as another's candidate succeeds and removes the row).
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** full gate → PASS.
- [ ] **Step 5: Commit** `feat(sync): source records and the matching pipeline`.

### Task 9: Duplicate resolution

**Files:**
- Create: `crates/server/src/ledger/duplicates.rs`
- Modify: `crates/server/src/http/api/transactions.rs`
- Test: `crates/server/tests/duplicates.rs`

**Interfaces:**
- Consumes: Task 8 store primitives (`sources_of`, `attach_source`, `clear_duplicate_candidates`, `set_flag`), Task 6 `UserState`.
- Produces (`ledger::duplicates`, all `Write` on the account, audited):
  - `merge(actor, target: TransactionId, duplicate: TransactionId, discard_duplicate_state: bool) -> TransactionView` — same account, both imported; moves the duplicate's sources to the target and deletes the duplicate; refused with 409 `merge_conflict` when the duplicate has user state and `discard_duplicate_state` is false (with it, the discarded values appear in the audit `before`). The target's `PossibleDuplicate` flag clears when it has no candidates left.
  - `unmerge(actor, transaction, source: SourceRecordId) -> TransactionView` (the new transaction) — the transaction must keep at least one source; the new one has no user state.
  - `dismiss_duplicate(actor, transaction) -> TransactionView` — clears the flag and the candidates.
- API: `POST /api/v1/transactions/{transaction_id}/merge` `{ duplicate, discard_duplicate_state? }`; `POST /api/v1/transactions/{transaction_id}/unmerge` `{ source }`; `POST /api/v1/transactions/{transaction_id}/dismiss-duplicate`. New `ProblemCode::MergeConflict`.

- [ ] **Step 1: Write failing tests:** `merge_moves_sources_and_refuses_user_state` (409 without the flag; with it, audit records the discarded notes); `merge_across_accounts_refused` (422); `unmerge_restores_separate_transaction`; `unmerge_last_source_refused`; `dismiss_clears_flag_and_candidates`; `merge_clears_flag_when_last_candidate_goes`; `read_token_cannot_merge` (403).
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** full gate → PASS.
- [ ] **Step 5: Commit** `feat(api): merge, unmerge and dismiss duplicates`.

### Task 10: Outbound HTTP egress **[security]**

**Files:**
- Modify: `Cargo.toml` (reqwest, rustls, rustls-platform-verifier), `crates/server/Cargo.toml`, `crates/server/src/config.rs` (`https_proxy`), `Dockerfile` (CA bundle), `deny.toml` (`[graph] targets`), `docs/configuration.md`
- Create: `crates/server/src/egress.rs`
- Test: unit tests in `egress.rs` and `config.rs`; `crates/server/tests/egress.rs`

**Interfaces:**
- `deny.toml` (ruling O1): `[graph] targets = ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl", "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "aarch64-apple-darwin", "x86_64-apple-darwin"]`, with the comment "Scopes the graph to the platforms coffer is built and developed on; every advisory, licence and ban still applies to them." No licence is added.
- Config: `Config.https_proxy: Option<Url>` from `HTTPS_PROXY` (else `https_proxy`): scheme `http` or `https`, a host, no path beyond `/`, no query or fragment; user information allowed and redacted in `Config`'s `Debug`. Other proxy variables (`NO_PROXY`, `HTTP_PROXY`, `ALL_PROXY`) have no effect: the client has no automatic proxy.
- Produces (`egress`):
  - `struct HostAllowList(Vec<String>)`: `parse(list: &str) -> Result<HostAllowList, EgressError>` (comma-separated DNS names, lowercased, no IP literals, no wildcards, no ports).
  - `struct Egress` (`Clone`): `Egress::new(proxy: Option<&Url>) -> Result<Egress, EgressError>` builds one reqwest client: rustls with the `ring` provider and the platform verifier (system CA bundle; `SSL_CERT_FILE` honoured), TLS 1.2+, `redirect::Policy::none()`, connect timeout 10 s, request timeout 60 s, user agent `coffer/<version>`, no cookies, no automatic proxy (`Proxy::https(proxy)` when configured), and a DNS resolver (`reqwest::dns::Resolve`) that keeps only `is_public_ip` addresses, except for the configured proxy's host, which resolves unfiltered (an egress proxy usually sits on a private address; with a proxy, target hosts are resolved by the proxy, never by coffer).
  - `fn check_url(&self, url: &Url, allowed: &HostAllowList) -> Result<(), EgressError>`: scheme `https`; host a DNS name in `allowed` (case-insensitive, trailing dot ignored); port absent or 443; no user information; no fragment.
  - `async fn send(&self, request: OutboundRequest, allowed: &HostAllowList) -> Result<OutboundResponse, EgressError>`; `OutboundRequest { method: Method, url: Url, basic_auth: Option<(String, SecretString)>, headers: Vec<(HeaderName, HeaderValue)>, body: Option<(&'static str /* content type */, Vec<u8>)> }`; `OutboundResponse { status: StatusCode, body: Vec<u8> }`, body read incrementally and capped at 16 MiB (`EgressError::TooLarge`). `check_url` runs before every request; a 3xx is returned as a status, never followed.
  - `fn is_public_ip(ip: IpAddr) -> bool`, false for every range in the IANA IPv4 and IPv6 Special-Purpose Address Registries that is not globally reachable, including: IPv4 `0.0.0.0/8`, `10.0.0.0/8`, `100.64.0.0/10`, `127.0.0.0/8`, `169.254.0.0/16`, `172.16.0.0/12`, `192.0.0.0/24`, `192.0.2.0/24`, `192.168.0.0/16`, `198.18.0.0/15`, `198.51.100.0/24`, `203.0.113.0/24`, `224.0.0.0/4`, `240.0.0.0/4`, `255.255.255.255`; IPv6 `::`, `::1`, `::/96` (IPv4-compatible), `::ffff:0:0/96` (checked as its IPv4), `64:ff9b::/96` and `64:ff9b:1::/48` (embedded IPv4 checked), `100::/64`, `2001::/32` (Teredo), `2001:db8::/32`, `2002::/16` (embedded IPv4 checked), `fc00::/7`, `fe80::/10`, `ff00::/8`.
  - `enum EgressError { HostNotAllowed, NotHttps, Resolution, Timeout, TooLarge, Transport, Config(&'static str) }`; messages carry no URL, query, header or body; reqwest errors (including proxy errors carrying the proxy URL) are reduced with `without_url()` and kept only as a redacted `#[source]`.
  - testkit only: `Egress::for_tests() -> Egress` and `HostAllowList::loopback(port)` that also accept `http://127.0.0.1:<port>`; neither exists in a non-testkit build.
- Dockerfile (ruling O2): copy `/etc/ssl/certs/ca-certificates.crt` from the builder stage into the final image, set `ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt`, and replace the comment saying coffer makes no outbound TLS connections.

- [ ] **Step 1: Write failing tests.**
  - `check_url_rules` (accepts `https://bridge.example/x`; rejects `http://`, IP literal hosts, `:8443`, user information, unlisted hosts, `bridge.example.evil.test`).
  - `is_public_ip_table` (one row per range above, embedded forms such as `64:ff9b::7f00:1` and `2002:7f00:1::` false, `8.8.8.8` and `2606:4700::1111` true).
  - `proxy_config_validation` (`HTTPS_PROXY=http://user:SECRETPW@proxy:3128` accepted and `{config:?}` lacks `SECRETPW`; `ftp://` and `http://proxy/path` rejected).
  - `resolver_refuses_private_targets` (production `Egress::new(None)`, allow-list `localhost`, request `https://localhost/` → `EgressError::Resolution`).
  - `proxy_on_private_address_is_reachable` (production `Egress::new(Some(http://127.0.0.1:<port>))` with a test TCP listener that records the first request line and answers 403: a request to allow-listed `https://bridge.example/x` reaches it as `CONNECT bridge.example:443`; a request to an unlisted host never reaches it).
  - testkit-mode integration with a loopback mock: `redirects_are_not_followed` (302 → `status == 302`, target never hit); `oversized_body_refused`; `errors_do_not_leak_url_secrets` (request to `/claim/SECRETPART?token=SECRETQ` refused, and a request through a dead proxy `http://user:SECRETPW@127.0.0.1:<closed port>`: `to_string()` and `{:?}` contain no `SECRET`).
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** `cargo tree -i aws-lc-rs` is empty; `cargo deny check` passes with the `[graph] targets` change; crate-age script passes (pin `openssl-probe` and any other transitive crate to its newest version at least 7 days old).
- [ ] **Step 4: Run** full gate and `docker buildx build --platform linux/arm64 .` → PASS; security review.
- [ ] **Step 5: Commit** `feat(server): allow-listed https egress client`.

### Task 11: Provider credential vault **[security]**

**Files:**
- Modify: `Cargo.toml` (chacha20poly1305), `crates/server/Cargo.toml`
- Create: `crates/server/src/vault.rs`
- Test: unit tests in `vault.rs`

**Interfaces:**
- Consumes: `ServerKey::derived` (`pub(crate)` since Task 5).
- Produces: `struct CredentialVault` (key `Zeroizing<[u8; 32]>`, `Debug` redacted): `CredentialVault::new(key: &ServerKey) -> Self` with `key.derived("coffer 2026-10 provider credentials v1")`.
- `fn seal(&self, connection: ConnectionId, plaintext: &SecretString) -> Result<SealedCredentials, VaultError>`: XChaCha20-Poly1305, 24-byte nonce from `getrandom`, associated data `b"coffer connection credentials\0" || connection.as_uuid().as_bytes()`; output `0x01 || nonce || ciphertext_and_tag`.
- `fn open(&self, connection: ConnectionId, sealed: &SealedCredentials) -> Result<SecretString, VaultError>`: decrypts into a `Zeroizing<Vec<u8>>`, validates UTF-8, then builds the `SecretString`; `struct SealedCredentials(Vec<u8>)` (`Debug` prints only the length); `enum VaultError { Malformed, UnknownVersion, Decrypt, Random }`.

- [ ] **Step 1: Write failing tests.** `seal_open_roundtrip`; `sealing_twice_differs`; `tampered_byte_fails` (version, a nonce byte, a ciphertext byte, the tag); `wrong_connection_fails`; `other_server_key_fails`; `unknown_version_rejected`; `debug_never_prints_plaintext_or_ciphertext`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** full gate → PASS; security review.
- [ ] **Step 5: Commit** `feat(server): sealed provider credentials`.

### Task 12: Provider interface and the sync engine **[security]**

**Files:**
- Create: `crates/server/src/sync/{provider,engine}.rs`, `crates/store/src/sync_runs.rs`
- Modify: `Cargo.toml` (jiff feature `tzdb-bundle-always`), `crates/server/src/sync/mod.rs`, `crates/server/src/config.rs` (`timezone`, `sync_interval`), `crates/server/src/testkit/mod.rs` (`FakeProvider`, log capture, `seed_connection`)
- Test: `crates/server/tests/sync_engine.rs`

**Interfaces:**
- Config: `Config.timezone: jiff::tz::TimeZone` from `COFFER_TIMEZONE` (IANA name, default `UTC`; unknown → `ConfigError::Invalid`; the bundled database works without `/usr/share/zoneinfo`). `Config.sync_interval: SignedDuration` from `COFFER_SYNC_INTERVAL_HOURS` (default 6, 1..=24).
- Store (`sync_runs`): `StoreTx::insert_sync_run(NewSyncRun { connection, trigger, requested_by: Option<(MemberId, Option<AgentTokenId>)>, queued_at }) -> SyncRunRecord`, `StoreTx::start_sync_run`, `StoreTx::finish_sync_run(SyncRunId, status, detail, at)`, `Store::get_sync_run`, `Store::list_sync_runs(ConnectionId, &PageRequest) -> Page<SyncRunRecord>`, `Store::sync_run_counts(ConnectionId, since: Timestamp) -> RunCounts { started_or_pending: u32, last_started_at: Option<Timestamp>, in_flight: bool }`, `Store::next_queued_run() -> Option<SyncRunRecord>`, `StoreTx::fail_running_runs(detail, at) -> u32`.
- Produces (`sync::provider`):
  - `#[non_exhaustive] enum ProviderKind { SimpleFin }` (serde/`FromStr` `"simplefin"`).
  - `struct Capabilities { pub pending: bool, pub mode: FetchMode, pub min_interval: SignedDuration, pub min_scheduled_interval: SignedDuration, pub daily_budget: u32 }`; `enum FetchMode { Window { max_days: i64, overlap_days: i64 }, Cursor }`.
  - `#[non_exhaustive] enum ConnectInput { SimpleFinSetupToken(SecretString) }`.
  - `struct FetchRequest<'a> { pub credentials: &'a SecretString, pub window_start: Option<Date>, pub cursor: Option<&'a str>, pub today: Date, pub timezone: &'a TimeZone }`.
  - `struct FetchResult { pub accounts: Vec<ProviderAccountData>, pub connection_errors: Vec<String>, pub cursor: Option<String> }`; `struct ProviderAccountData { pub provider_account_id: String, pub name: String, pub institution: Option<String>, pub currency: String, pub kind_hint: Option<AccountKind>, pub balance: Option<(Date, DecimalAmount)>, pub transactions: Vec<ProviderTransaction>, pub removed: Vec<String>, pub coverage: Coverage, pub error: Option<String> }`; `struct ProviderTransaction { pub provider_id: String, pub posted_on: Date, pub transacted_on: Option<Date>, pub amount: DecimalAmount, pub description: String, pub payee: Option<String>, pub memo: Option<String>, pub pending: bool, pub replaces_provider_id: Option<String> }`.
  - `enum ProviderError { ReauthRequired, RateLimited, Unavailable(String), Invalid(String), Egress(EgressError) }`.
  - `type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;`
  - `trait Provider: Send + Sync { fn kind(&self) -> ProviderKind; fn capabilities(&self) -> Capabilities; fn connect<'a>(&'a self, input: ConnectInput) -> ProviderFuture<'a, Result<SecretString, ProviderError>>; fn fetch<'a>(&'a self, request: FetchRequest<'a>) -> ProviderFuture<'a, Result<FetchResult, ProviderError>>; fn disconnect<'a>(&'a self, credentials: &'a SecretString) -> ProviderFuture<'a, Result<(), ProviderError>>; }` — providers know nothing of categories, review, audit or storage (§6.2).
  - `struct ProviderRegistry` (`HashMap<ProviderKind, Arc<dyn Provider>>`): `ProviderRegistry::from_config(config: &Config, egress: &Egress) -> Result<ProviderRegistry, ConfigError>` is the single registration point (empty in this task; Task 14 adds SimpleFin; plan 2b adds Plaid); `get(kind)`; testkit `with(provider)`.
  - `fn sanitize_provider_message(raw: &str) -> String`: control characters removed, any `scheme://user:pass@` user information replaced by `scheme://***@`, truncated to 500 chars. Every provider text that reaches storage, logs or responses passes through it.
- Produces (`sync::engine`): `SyncError` gains `Vault(VaultError)`, `Provider(ProviderError)`, `RateLimited { retry_after: SignedDuration }`, `InProgress`, `CurrencyMismatch`, mapped to new `ProblemCode`s `ProviderError` (502; `ReauthRequired` → `ReauthRequired`, 409), `SyncRateLimited` (429 with `retry_after`), `SyncInProgress` (409), `CurrencyMismatch` (409). `struct SyncService { store, clock, vault, registry, config, access: Arc<AccessResolver>, ledger: Arc<Ledger> }` with `async fn run(&self, run: SyncRunId) -> Result<SyncRunView, SyncError>`:
  - marks the run `running`, opens the credentials, computes `today` (from `clock` in `config.timezone`) and `window_start` (`Window`: first run → `today − max_days`; else `max(date(last_success_at) − overlap_days, today − max_days)`), calls `fetch`;
  - for each provider account, in its own `StoreTx`: upsert the provider account (balance kept as raw text); if it has an error, a currency `Currency::parse` rejects, or a currency different from its bound Account's, record the sanitized error on it and skip; otherwise convert amounts with `DecimalAmount::to_minor_units`, call `apply_account_batch`, and for a bound account write a `sync` balance snapshot dated by the provider's balance date (falling back to `today`), as that is the day the balance describes;
  - connection status: `ReauthRequired` → `reauth_required`; any other provider error → `error`, sanitized `last_error`, run `failed`; success with `connection_errors` → status `active`, `last_error` = the sanitized errors joined, run `partial`; success with some account errors → `active`, run `partial`; otherwise `active`, `last_error` cleared, run `succeeded`; `last_success_at` and the cursor saved on any success.
  - `SyncRunView { id, connection, trigger, status, requested_by: Option<AttributionView>, queued_at, started_at, finished_at, detail }`.
- Produces (`testkit`): `capture_logs()` installs the global subscriber once (the existing `Signals` layer, which `event_signal` then reuses instead of installing its own, now also appending every event's message and fields to a process-wide buffer) and is called by every `TestServer` constructor; `captured_log_text() -> String`. `FakeProvider` (scripted `VecDeque<Result<FetchResult, ProviderError>>`, records each request's `window_start`, `cursor` and `today`); `TestServer::with_fake_provider()`; `TestServer::seed_connection(kind, label, owner, plaintext) -> ConnectionId` (seals with the server's vault and inserts through the store).

- [ ] **Step 1: Write failing tests** (with `FakeProvider`, runs inserted with `insert_sync_run`):
  - `first_window_is_max_then_overlap` (first request `window_start = today − 90`; after success `last_success − 14`; after a 200-day gap clamped to `today − 90`).
  - `fetch_request_carries_household_today` (`COFFER_TIMEZONE=Pacific/Auckland`, clock at 2026-10-31T12:00Z → `today == 2026-11-01`).
  - `new_provider_accounts_appear_unbound_and_hold_records`.
  - `currency_problem_isolated_to_one_account` (A with currency `"https://custom"`, B bound with `EUR` to a `USD` account, C fine → A and B carry errors, C committed, run `partial`).
  - `account_failure_rolls_back_only_that_account` (a store failure injected while applying one account leaves no partial rows for it).
  - `connection_errors_alongside_data_are_partial` (status `active`, `last_error` set, run `partial`).
  - `reauth_marks_connection`.
  - `provider_error_message_sanitized` (`"failed for https://u:p@bridge.example/x\u{7}"` stored as `"failed for https://***@bridge.example/x"`).
  - `balance_snapshot_uses_provider_balance_date`.
  - `credentials_never_logged` (after a run, `captured_log_text()` contains the connection id (proof the flow was captured) and not the plaintext).
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** full gate → PASS; security review.
- [ ] **Step 5: Commit** `feat(sync): provider interface and sync engine`.

### Task 13: Connections service, rate limits and binding **[security]**

**Files:**
- Create: `crates/server/src/sync/connections.rs`, `crates/server/src/http/api/provider_accounts.rs`
- Modify: `crates/server/src/http/mod.rs` (`AppState.sync`), `crates/server/src/cli.rs` (`open_state` keeps the `ServerKey` `Arc` and builds `Egress::new(config.https_proxy)`, `CredentialVault`, `ProviderRegistry::from_config`, `SyncService`), `crates/server/src/testkit/mod.rs`
- Test: `crates/server/tests/connections.rs`

**Interfaces:**
- Consumes: Task 12 `SyncService`, `ProviderRegistry`; Task 5 `Ledger::create_account`, `AccessResolver`; Task 8 store primitives and `apply_held`.
- Produces (`SyncService`, in `sync::connections`):
  - `ConnectionView { id, provider, label, owner, status, last_error, last_attempt_at, last_success_at, next_sync_at, accounts: Vec<ProviderAccountView> }` (`next_sync_at` = `last_attempt_at + max(config.sync_interval, min_scheduled_interval)` for `active`/`error`, else `None`); `ProviderAccountView { id, connection, name, institution, currency, kind_hint, account: Option<AccountId>, balance: Option<{ on, amount_raw }>, error, last_seen_at }`.
  - `async fn list_connections(&self, actor) -> Vec<ConnectionView>` (`ReadHousehold`; provider accounts bound outside the actor's `Balances` scope are omitted).
  - `async fn create_connection(&self, actor, kind, label: ConnectionLabel, input: ConnectInput, slot: Option<IdempotencySlot>) -> Result<ConnectionView, SyncError>` (`ManageConnections`; `connect` → `vault.seal` → insert; audit `connection.create` with `label` and `provider` only; queues an `initial` run in the same tx).
  - `async fn reconnect(&self, actor, ConnectionId, input: ConnectInput) -> Result<ConnectionView, SyncError>` (`RemoveConnection { owner }`; not for `removed` connections; `connect` → reseal under the same id → status `active`, `last_error` cleared; bindings and sources stay valid; audit `connection.reconnect`; queues a `manual` run).
  - `async fn remove_connection(&self, actor, ConnectionId) -> Result<(), SyncError>` (`RemoveConnection { owner }`; calls `disconnect` first and, if it fails, returns `SyncError::Provider` with nothing changed; then clears `credentials`, status `removed`; accounts, transactions and bindings stay; audit `connection.remove`).
  - `async fn request_sync(&self, actor: Option<&Actor>, ConnectionId, trigger: SyncTrigger) -> Result<SyncRunView, SyncError>` (`ManageConnections` for an actor; `enum SyncTrigger { Schedule, Manual, Initial }`): refuses `InProgress` (409 `sync_in_progress`) while a run is `queued` or `running`; refuses `RateLimited { retry_after }` (429 `sync_rate_limited` with `Retry-After`) when runs queued, running or started in the last 24 h reach `daily_budget`, or the last run started less than `min_interval` (manual) / `min_scheduled_interval` (schedule) ago; otherwise inserts a `queued` run (audit `sync.request` with member and agent for manual triggers).
  - `async fn bind(&self, actor, ProviderAccountId, AccountId)` (`ManageConnections` and `authorize_account(.., Write)` via `access`; same currency or 409 `currency_mismatch`; `bind_provider_account` + `apply_held` in one tx; audit `provider_account.bind`).
  - `async fn create_account_and_bind(&self, actor, ProviderAccountId, NewLinkedAccountInput { name: Option, kind: Option, liquidity, include_in_net_worth, include_in_forecast })`: kind from the input, else `kind_hint`, else 422; name defaults to the provider account's; currency from the provider account (unsupported → 409 `currency_mismatch`); creates through `Ledger::create_account` (same validation, defaults and audit), then `bind`.
- API: `GET /api/v1/provider-accounts?unbound` → `{ items: Vec<ProviderAccountView> }`; `POST /api/v1/provider-accounts/{provider_account_id}/bind` `{ account }`; `POST /api/v1/provider-accounts/{provider_account_id}/create-account` `NewLinkedAccountRequest`. `http::problem::RateLimitedProblem` (`IntoResponses`) documents 429 with the `Retry-After` header.

- [ ] **Step 1: Write failing tests** (with `FakeProvider`): `bind_requires_same_currency`; `bind_applies_held_records` (records held before binding become transactions); `create_account_and_bind_via_api` (write token; kind from `kind_hint`; account audit identical in shape to `account.create`); `read_token_cannot_bind` (403); `bind_needs_write_on_account` (service call with an `AccountAccess::Uniform(Transactions)` actor → 403); `rate_limit_refuses_with_retry_after` (17th request within 24 h, or a manual request 30 min after the last → `RateLimited` with the right `retry_after`); `second_request_while_queued_refused` (409 `sync_in_progress`); `credentials_never_stored_in_clear` (after `create_connection`, raw `connections.credentials` bytes, every audit row and `captured_log_text()` lack the plaintext; `connection.create` changes hold only `label` and `provider`; the log contains the connection id); `reconnect_reseals_and_keeps_bindings`; `remove_fails_cleanly_when_disconnect_fails` (provider `disconnect` error → `SyncError::Provider`, connection unchanged); `member_cannot_remove_or_reconnect_others_connection`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement**, including the production wiring in `open_state`. **Step 4: Run** full gate → PASS; security review.
- [ ] **Step 5: Commit** `feat(sync): connections, rate limits and provider account binding`.

### Task 14: SimpleFin provider and the connections page **[security]**

**Files:**
- Create: `crates/server/src/sync/simplefin.rs`, `crates/server/tests/fixtures/simplefin/{accounts_first.json,accounts_posted.json,accounts_errors.json}`, `crates/server/src/http/api/connections.rs`, `crates/server/src/http/pages/connections.rs`
- Modify: `crates/server/src/config.rs` (`simplefin_hosts`), `crates/server/src/sync/provider.rs` (`from_config` registers SimpleFin), `crates/server/src/testkit/mod.rs` (`MockUpstream`, `with_simplefin`), `crates/server/src/http/pages/{mod,layout}.rs` (`Nav::Connections`)
- Test: `crates/server/tests/simplefin.rs`, `crates/server/tests/connections_page.rs`, `crates/server/tests/serve.rs`

**Interfaces:**
- Config (ruling O7): `Config.simplefin_hosts: HostAllowList` from `COFFER_SIMPLEFIN_HOSTS`, default `bridge.simplefin.org,beta-bridge.simplefin.org`.
- Produces: `struct SimpleFin { egress: Egress, hosts: HostAllowList }` implementing `Provider` with `Capabilities { pending: true, mode: Window { max_days: 90, overlap_days: 14 }, min_interval: 1 h, min_scheduled_interval: 2 h, daily_budget: 16 }` (SimpleFin allows about 24 requests a day; every sync is one request).
  - `connect(SimpleFinSetupToken)`: token ≤ 2048 bytes, standard base64 → UTF-8 → claim URL; `check_url(claim, hosts)`; `POST` with an empty body; a 200 body (≤ 2048 bytes) is the access URL, which must pass `check_url` once its user information is removed and must carry a username and a password. Returns the access URL as the credential. Error mapping: decoding failures, `EgressError::{HostNotAllowed, NotHttps}`, an oversized or malformed claim response, a claim 403 and every other `ProviderError::Invalid` → `ProviderError::Invalid` (→ 422 `setup_token_invalid`); everything else (transport, timeout, 5xx) → `Unavailable` (→ 502 `provider_error`).
  - `fetch`: `GET <access URL without user information>/accounts?start-date=<Unix seconds of window_start 00:00 in the timezone>&pending=1` with basic auth from the user information (percent-decoded). 401/403 → `ReauthRequired`; 402 → `Unavailable("SimpleFin subscription inactive")`; 429 → `RateLimited`. Parsing follows the SimpleFin protocol (`errors` → `connection_errors`; per account `id`, `name`, `org.name` → institution, `currency`, `balance`, `balance-date`; transactions `id`, `posted` (0 or `pending: true` → pending), `amount`, `description`, `payee`, `memo`, `transacted_at`); amounts stay `DecimalAmount`; Unix timestamps become dates in `request.timezone`; a pending transaction's date is `transacted_at`, else `posted`, else `request.today`. `coverage = Window { start: window_start }`. Unknown fields ignored; a malformed account becomes that account's `error`, never a whole-run failure.
  - `disconnect`: no network call (access is revoked at the bridge); `Ok(())`.
- Produces (`testkit`): `MockUpstream::start(router: axum::Router) -> MockUpstream { pub base: Url }` on `127.0.0.1:0`, recording requests (method, path, query, whether `Authorization` was present); `TestServer::with_simplefin(mock: &MockUpstream)` registers a `SimpleFin` built with `Egress::for_tests()` and `HostAllowList::loopback(port)`.
- API: `POST /api/v1/connections/simplefin` (`ManageConnections`; `Idempotency-Key`) body `{ label, setup_token }` → 201 `ConnectionView`; `POST /api/v1/connections/{connection_id}/simplefin/reconnect` body `{ setup_token }` → `ConnectionView`. `setup_token` deserializes into a `SecretString`; never logged, never in errors. New `ProblemCode::SetupTokenInvalid` (422).
- Page: `GET /connections` (session; `layout::page` with `Frame { current: Some(Nav::Connections), .. }`, `empty_state` when there are none) lists connections (provider, label, status, last sync, errors) and their provider accounts by name and binding, without balances or transactions (§6.4: no financial data pages). Forms, each with `csrf_field`: `POST /connections/simplefin` (label, setup token), `POST /connections/{id}/simplefin/reconnect` (shown for `reauth_required`), `POST /connections/{id}/sync`, `POST /connections/{id}/remove`. The default CSP; no inline script or style.
- Fixtures: generic (institution "Example Bank", accounts "Checking", "Credit Card"), shaped like the protocol document's example response; `accounts_posted.json` repeats the first with one pending record replaced by its posted form under a new id; `accounts_errors.json` carries a top-level error string and an account with currency `https://example.com/points`.

- [ ] **Step 1: Write failing tests** (against `MockUpstream`):
  - `claim_stores_sealed_access_url` (claim returns `http://user:pass@127.0.0.1:<port>/simplefin` → connection created; `connections.credentials` lacks `user:pass`; response, audit and `captured_log_text()` lack `pass`, the log contains the connection id).
  - `setup_token_for_unlisted_host_refused` (tokens decoding to `https://internal.example/claim/x`, `https://10.0.0.1/claim/x`, `https://bridge.simplefin.org.evil.test/claim/x` → 422 `setup_token_invalid`, no request made, nothing stored).
  - `access_url_for_unlisted_host_refused`; `claim_redirect_not_followed` (302 → 422, target never requested).
  - `setup_token_never_echoed` (an invalid token `SECRETTOKEN…` → body, audit and `captured_log_text()` lack it).
  - `fetch_sends_window_and_basic_auth` (`start-date` equals the expected Unix seconds, `pending=1`, `Authorization` present, no user information in the request URL).
  - `sync_from_fixtures_then_posted_keeps_state` (sync `accounts_first`, bind both, categorize the pending one, sync `accounts_posted` → one transaction, posted, still categorized, same id).
  - `posted_date_uses_household_timezone` (`COFFER_TIMEZONE=America/Los_Angeles`, `posted` 2026-10-31T06:30Z → `posted_on == 2026-10-30`).
  - `errors_fixture_reports_connection_and_account_errors`; `reauth_on_403`; `rate_limited_on_429`; `amounts_parsed_exactly` (`"-1234567.89"` → -123456789).
  - `reconnect_with_new_setup_token` (connection `reauth_required` → reconnect → `active`, same bindings).
  - Pages: `connections_page_lists_without_balances` (session; contains provider account names, not their balance figures; CSP header is the default; no `<script>` without `src`, no `style=`); `add_form_creates_connection`; `add_form_never_echoes_invalid_token` (HTML and log lack it); `csrf_required_on_every_form` (add, reconnect, sync, remove each → 403 without the token); `member_cannot_remove_others_connection_via_page`.
  - In `tests/serve.rs`: `production_wiring_refuses_unlisted_setup_token` (real `open_state` wiring through `serve_with`; a write token minted through `AgentTokens::create` with a session actor; `POST /api/v1/connections/simplefin` with a token for an unlisted host → 422).
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** full gate → PASS; security review (SSRF, credential handling, page CSRF).
- [ ] **Step 5: Commit** `feat(sync): simplefin provider and connections page`.

### Task 15: Scheduling, on-demand sync, connection API and operator docs

**Files:**
- Create: `crates/server/src/sync/scheduler.rs`
- Modify: `crates/server/src/cli.rs` (`serve_with` spawns the worker and scheduler), `crates/server/src/http/api/connections.rs`
- Modify docs: `docs/configuration.md`, `docs/agent-usage.md`, `docs/threat-model.md`, `docs/container.md`, `README.md`, `AGENTS.md`
- Test: unit tests in `scheduler.rs`; `crates/server/tests/connections_api.rs`; `crates/server/tests/serve.rs`

**Interfaces:**
- Produces (`sync::scheduler`): `fn due(now: Timestamp, entries: &[ScheduleEntry { id, status, last_attempt_at, effective_interval }]) -> Vec<ConnectionId>` (status `active` or `error`; `last_attempt_at` absent or `+ effective_interval <= now`; `reauth_required` and `removed` never due); `fn spawn(state: AppState, shutdown) -> JoinHandle<()>`: at start, `fail_running_runs("interrupted")` (queued runs stay queued and are drained); one worker runs `next_queued_run` one at a time (an interrupted run leaves only whole accounts committed); a ticker every 60 s calls `request_sync(None, id, Schedule)` for due connections (refusals logged at `debug`).
- API:
  - `GET /api/v1/connections` → `{ items: Vec<ConnectionView> }`.
  - `POST /api/v1/connections/{connection_id}/sync` → 202 `SyncRunView`, 409 `sync_in_progress`, or 429 `sync_rate_limited` with `Retry-After` (documented with `RateLimitedProblem`).
  - `GET /api/v1/connections/{connection_id}/sync-runs?after&limit` → `{ items: Vec<SyncRunView>, next }`.
  - `DELETE /api/v1/connections/{connection_id}` → 204, or 502 `provider_error` with nothing changed.
- Docs:
  - `configuration.md`: `COFFER_TIMEZONE`, `COFFER_SIMPLEFIN_HOSTS`, `COFFER_SYNC_INTERVAL_HOURS`, `HTTPS_PROXY`; an **Outbound network** section: coffer is no longer offline; it connects only to the hosts in `COFFER_SIMPLEFIN_HOSTS` (by default `bridge.simplefin.org` and `beta-bridge.simplefin.org`, port 443), plus Plaid's and the FX source's in later plans; targets must resolve to public addresses; allow-list them in an egress proxy (`HTTPS_PROXY`; CONNECT to those hosts only, and the proxy then enforces the public-address rule) or an SNI allow-list; redirects are never followed; `NO_PROXY`/`HTTP_PROXY`/`ALL_PROXY` are ignored. Losing the server key now also means reconnecting every provider.
  - `agent-usage.md`: scopes table (`write` covers accounts, transactions, categories, connections and sync); the review workflow (list `review=unreviewed`, batch categorize/review/assign, merge flagged duplicates); amounts and signs; idempotency keys; sync limits, `sync_in_progress` and `Retry-After`; reconnecting with a new setup token.
  - `threat-model.md`: provider credentials as an asset; trust boundary "coffer to data providers"; the SSRF design (allow-list, no redirects, public-address resolution, the proxy exception); sealed credentials and their key; accepted risk: a compromised SimpleFin Bridge can feed false data but cannot reach other hosts.
  - `container.md`: the image contains a CA bundle; egress requirement.
  - `README.md`: status (accounts, transactions, SimpleFin).
  - `AGENTS.md`: crate map (`ledger`, `sync`, `egress`, `vault`; domain `money`, `accounts`, `transactions`, `matching`), conventions (money; `authorize_account` and `AccessResolver`; `From<Denied> for Problem`; outbound HTTP only via `Egress`; provider text through `sanitize_provider_message`; providers registered only in `ProviderRegistry::from_config`), security review triggers include CSP and provider code.

- [ ] **Step 1: Write failing tests.** `due_rules` (table over statuses and times, including exactly at `last_attempt + interval`); `connections_list_shows_status_and_errors`; `sync_trigger_attributed_to_agent` (202; audit `sync.request` with the token; the worker runs it; run `succeeded`); `second_trigger_within_min_interval_429` (with `Retry-After`); `read_token_cannot_trigger_sync` (403); `delete_connection_keeps_transactions` (status `removed`, credentials `NULL`, transactions remain); `interrupted_run_marked_failed_on_start` (a `running` row before `spawn` → `failed`, detail `interrupted`); in `tests/serve.rs`: `serve_creates_account_and_transaction_end_to_end` (through `serve_with` and a real listener: create a manual account and a transaction with a minted write token, list it back, fetch `/api/v1/openapi.json`).
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement**, then the docs. **Step 4: Run** full gate → PASS.
- [ ] **Step 5: Commit** `feat(sync): scheduling, on-demand sync and connection api`.
- [ ] **Step 6: Whole-branch security review** (`security-review` skill) covering egress, vault, SimpleFin, policy, idempotency and pages; fix findings; commit.
