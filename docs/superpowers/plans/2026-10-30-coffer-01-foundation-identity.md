# coffer plan 1 — Foundation & identity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A running `coffer` server with the repository, tooling and agent docs in place, a storage layer with audit, passkey-only member authentication (bootstrap, invites, re-enroll, sessions), agent tokens, the authorization policy, minimal web pages and the first JSON API endpoints.

**Architecture:** Three crates. `coffer-domain` is pure (ids, identity, policy, audit entry types, clock) with no I/O. `coffer-store` is the only crate that depends on `sqlx`; its public API speaks domain types only, so a Postgres backend later changes only this crate. `coffer-server` holds config, secrets, auth services, HTTP (pages + `/api/v1`) and the `coffer` binary; it never imports `sqlx`.

**Tech Stack:** Rust 1.99 (edition 2024), axum 0.8, tokio 1.54, sqlx 0.9 (SQLite), webauthn-rs =0.5.5, maud 0.27, jiff 0.2, uuid 1 (v7), blake3 1.8, clap 4, thiserror 2, tracing; tests: webauthn-authenticator-rs =0.5.5 (`softpasskey`), tempfile, tower, http-body-util, proptest.

**Spec:** `docs/superpowers/specs/2026-10-30-coffer-design.md` (§5, §6.1 identity entities, §6.3–§6.6, §6.13, §6.14).

**Roadmap (later plans, not this one):** 2 — household settings, accounts, categories, transactions, provider interface, sync engine, SimpleFin, rules, review API. 3 — plan items, forecast/what-if, FX, reports. 4 — MCP server, OpenAPI document + CI drift check, container image, deployment docs.

## Global Constraints

- `rust-toolchain.toml`: `channel = "1.99"`, components `rustfmt`, `clippy`; workspace `edition = "2024"`, `rust-version = "1.99"`.
- Workspace lints: `unsafe_code = "forbid"`; clippy `all` and `pedantic` at `deny`. No `#[allow]`/`#[expect]` without a `reason = "..."`.
- License `MIT OR Apache-2.0`; `cargo-deny` config modeled on a reference project's `deny.toml` (no project-specific ignores).
- `sqlx` is a dependency of `coffer-store` only. No `sqlx` type appears in `coffer-store`'s public API.
- No secrets (tokens, session ids, invite tokens, key material) in logs, errors, audit entries or API responses, except a newly created token shown once to its creator.
- Tokens are stored only as keyed BLAKE3 hashes; comparisons happen by hash lookup.
- Every mutation writes its audit entry in the same database transaction.
- Members are disabled, never deleted. The last active admin can't be disabled or demoted.
- Agent tokens can never create, list or revoke tokens and can never manage passkeys (session-only actions).
- Effective rights = min(token scope, member role); creating a token with a scope above the member's role is rejected.
- Invite, re-enroll and bootstrap links: single use, expire after `COFFER_INVITE_TTL_MINUTES` (default 30).
- Session cookie: `HttpOnly; Secure; SameSite=Strict; Path=/`, name `coffer_session`; idle expiry default 7 days, absolute default 30 days.
- No dead code, no TODO comments. Tests specify behavior; no tests that restate implementation or exist for coverage.
- Build artifacts go to the external SSD via the existing `CARGO_TARGET_DIR`; never override it.

## Review Focus

1. **Double use of a single-use link under concurrency** (two tabs finish the same invite): exactly one member is created. → Task 6 test `invite_consumed_once_under_race`.
2. **Clock-boundary expiry** (invite/session used at exactly `expires_at`): treated as expired. → Task 4 test `invite_expires_at_boundary` and Task 7 test `session_idle_boundary`.
3. **Disabled member still holding a live session or token**: every request after disable is 401. → Task 8 test `disabled_member_loses_session_and_tokens`.
4. **Removing the last passkey / last admin through a race or self-action**: refused with a clear error. → Task 6 tests `cannot_remove_last_passkey`, `cannot_disable_last_admin`, `admin_cannot_demote_self_if_last`.
5. **Token-shaped strings leaking into logs/errors** (bad bearer header echoed): problem responses never contain the presented credential. → Task 8 test `bad_bearer_not_echoed`.

---

### Task 1: Workspace, tooling, agent docs, and domain ids/clock

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `deny.toml`, `.gitignore`, `LICENSE-MIT`, `LICENSE-APACHE`, `README.md`, `AGENTS.md`, `CLAUDE.md`, `CONTRIBUTING.md`, `SECURITY.md`, `.github/workflows/ci.yml`
- Create: `crates/domain/Cargo.toml` (package `coffer-domain`), `crates/domain/src/lib.rs`, `crates/domain/src/ids.rs`, `crates/domain/src/time.rs`
- Test: `crates/domain/src/ids.rs` (unit)

**Interfaces:**
- Produces: `coffer_domain::ids::{MemberId, PasskeyId, InviteId, SessionId, AgentTokenId, AuditEventId}` — each a `Copy` newtype over `uuid::Uuid` (v7), with `new() -> Self`, `from_uuid(Uuid) -> Self`, `as_uuid(&self) -> Uuid`, `Display`/`FromStr` (hyphenated lowercase), `serde` transparent.
- Produces: `coffer_domain::time::{Clock, SystemClock}`; `trait Clock: Send + Sync { fn now(&self) -> jiff::Timestamp; }`. Feature `testkit` adds `ManualClock { fn new(Timestamp) -> Self; fn advance(&self, jiff::SignedDuration); fn set(&self, Timestamp) }`.

- [ ] **Step 1: Workspace files.** Root `Cargo.toml` with `members = ["crates/domain"]` (Tasks 3 and 5 add `crates/store` and `crates/server` when they create them), `[workspace.package]` (version `0.1.0`, edition, rust-version, license, `repository = "https://github.com/pecheriere/coffer"`), `[workspace.lints]` per Global Constraints, `[workspace.dependencies]` for every crate in Tech Stack at the versions listed. `webauthn-rs` and `webauthn-authenticator-rs` pinned `=0.5.5`; a crate-level vendored-openssl feature. `.gitignore`: `/target`, `*.db`, `*.db-*`, `/data`.
- [ ] **Step 2: CI.** `.github/workflows/ci.yml` modeled on a reference project's CI workflow (pinned action SHAs, `permissions: contents: read`, `persist-credentials: false`): jobs `check` (fmt check, clippy `--workspace --all-targets --all-features --locked -- -D warnings`, `cargo test --workspace --all-features --locked`, `cargo doc --workspace --no-deps` with `RUSTDOCFLAGS=-D warnings`) and `deny`. Toolchain `1.99`. No image job yet (plan 4).
- [ ] **Step 3: Agent and contributor docs.** `AGENTS.md` sections: Purpose (2 lines + link to spec), Crate map (the three crates and the sqlx boundary rule), Commands (`cargo fmt`, `cargo clippy …`, `cargo test --workspace --all-features`, `cargo deny check`), Conventions (ids are UUIDv7 newtypes; time via `Clock`, never `Timestamp::now()` outside `SystemClock`; every mutation goes through a `StoreTx` with its audit entry; every handler authorizes through `coffer_domain::policy::authorize`; errors via `thiserror`, no `unwrap`/`expect` outside tests), Testing rules (the Global Constraints test rule, verbatim; integration tests drive the real router with soft passkeys), Security review (required before merge for changes to auth, sessions, tokens, policy, secrets, outbound HTTP — run the `security-review` skill or equivalent and record findings in the PR), Definition of done (fmt, clippy, tests, deny all green; docs updated). `CLAUDE.md` contains one line: `See AGENTS.md.` `CONTRIBUTING.md`: setup, the same commands, PR expectations, commit style (conventional commits), DCO not required. `SECURITY.md`: report privately via GitHub security advisories. `README.md`: one-paragraph intent from spec §1, status "pre-alpha", license.
- [ ] **Step 4: Write failing tests** in `ids.rs`:
  - `ids_roundtrip_display_fromstr`: `MemberId::new()` → `to_string()` → `parse::<MemberId>()` equals original.
  - `ids_are_time_ordered`: two ids created in sequence compare `a.as_uuid() < b.as_uuid()`.
  - `ids_reject_garbage`: `"nope".parse::<MemberId>()` is `Err`.
  - `id_serde_is_plain_string`: `serde_json::to_string(&id)` equals `format!("\"{id}\"")`.
- [ ] **Step 5: Run** `cargo test -p coffer-domain` → FAIL (types missing).
- [ ] **Step 6: Implement** ids via one private `macro_rules! id_type` in `ids.rs`; `time.rs` with `Clock`, `SystemClock`, and `ManualClock` behind `testkit` (internally `Mutex<Timestamp>`).
- [ ] **Step 7: Verify** `cargo test -p coffer-domain`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo fmt --check`, `cargo deny check` → all pass.
- [ ] **Step 8: Commit** `chore: workspace, tooling, agent docs and domain ids`.

### Task 2: Identity types and the authorization policy

**Files:**
- Create: `crates/domain/src/identity.rs`, `crates/domain/src/policy.rs`, `crates/domain/src/audit.rs`
- Test: unit tests in `policy.rs` and `identity.rs`

**Interfaces:**
- Consumes: Task 1 ids.
- Produces (`identity.rs`):
  - `enum Role { Admin, Member }`, `enum Scope { Read, Write, Admin }` (both `serde` lowercase, `FromStr`, `as_str()`); `Scope: Ord` with `Read < Write < Admin`.
  - `fn role_ceiling(role: Role) -> Scope` (Admin→Admin, Member→Write).
  - `struct Username(String)`: `Username::parse(&str) -> Result<Username, IdentityError>`; lowercases; allowed `[a-z0-9_-]`, length 1..=32.
  - `struct DisplayName(String)`: `parse` trims; 1..=64 chars; rejects control characters.
  - `struct TokenName(String)`: same rules as `DisplayName`.
  - `enum Via { Session, Agent { token: AgentTokenId, scope: Scope } }`
  - `struct Actor { pub member: MemberId, pub role: Role, pub via: Via }` with `fn effective_scope(&self) -> Scope` = `min(role_ceiling(role), scope)` for agents, `role_ceiling(role)` for sessions.
- Produces (`policy.rs`):
  - `enum Action { ReadHousehold, ManageOwnCredentials, ManageMembers, ManageInvites }` — later plans add variants.
  - `fn authorize(actor: &Actor, action: Action) -> Result<(), Denied>`
  - `struct Denied { pub action: Action, pub reason: DenyReason }`, `enum DenyReason { SessionRequired, InsufficientScope { needed: Scope } }`.
  - Requirements table (the only place rules live): `ReadHousehold` → Read; `ManageOwnCredentials` → session only (any role); `ManageMembers`, `ManageInvites` → Admin.
  - `fn check_token_scope(role: Role, requested: Scope) -> Result<(), Denied>` (rejects scope above `role_ceiling`).
- Produces (`audit.rs`):
  - `enum AuditActor { Member { member: MemberId, agent: Option<AgentTokenId> }, System { reason: String } }` with `From<&Actor>`.
  - `struct AuditEntry { pub actor: AuditActor, pub action: String, pub entity_type: String, pub entity_id: String, pub changes: serde_json::Value }`.

- [ ] **Step 1: Write failing tests** in `policy.rs`:
  - `member_session_reads_but_cannot_manage_members`: Member+Session → `ReadHousehold` Ok, `ManageMembers` Err `InsufficientScope { needed: Admin }`.
  - `admin_with_write_token_cannot_manage_invites`: Admin+Agent(Write) → `ManageInvites` Err `InsufficientScope { needed: Admin }`.
  - `member_with_admin_token_is_capped`: Member+Agent(Admin) → `effective_scope() == Write`, `ManageMembers` Err.
  - `agents_never_manage_credentials`: Admin+Agent(Admin) → `ManageOwnCredentials` Err `SessionRequired`.
  - `read_token_can_read`: Member+Agent(Read) → `ReadHousehold` Ok.
  - `token_scope_above_role_rejected`: `check_token_scope(Member, Admin)` Err; `(Member, Write)` Ok; `(Admin, Admin)` Ok.
  In `identity.rs`: `username_normalizes_and_validates` (`"Alice"`→`"alice"`; `""`, 33 chars, `"a b"`, `"é"` rejected); `display_name_rejects_control_chars` (`"a\u{0007}"` Err, `"  Alice  "` → `"Alice"`).
- [ ] **Step 2: Run** `cargo test -p coffer-domain` → FAIL.
- [ ] **Step 3: Implement** the types above; `authorize` is a `match` on `action` producing a requirement enum `{ Session, Scope(Scope) }`, then compares with `actor.via` / `effective_scope()`.
- [ ] **Step 4: Run** tests → PASS; clippy clean.
- [ ] **Step 5: Commit** `feat(domain): identity types and authorization policy`.

### Task 3: Store foundation — open, migrations, transactions, audit, backup

**Files:**
- Create: `crates/store/Cargo.toml` (package `coffer-store`), `crates/store/src/lib.rs`, `crates/store/src/error.rs`, `crates/store/src/audit.rs`, `crates/store/src/backup.rs`, `crates/store/migrations/0001_identity.sql`
- Test: `crates/store/tests/foundation.rs`

**Interfaces:**
- Consumes: domain ids, `AuditEntry`, `AuditActor`.
- Produces:
  - `struct Store` (`Clone`, wraps the pool). `Store::open(path: &Path) -> Result<Store, StoreError>`: creates the file if missing, `journal_mode=WAL`, `foreign_keys=ON`, `busy_timeout=5s`, `synchronous=NORMAL`, runs embedded migrations.
  - `Store::begin(&self) -> Result<StoreTx, StoreError>`; `StoreTx::commit(self) -> Result<(), StoreError>`; dropping without commit rolls back.
  - `StoreTx::append_audit(&mut self, at: Timestamp, entry: &AuditEntry) -> Result<AuditEventId, StoreError>`.
  - `struct AuditEvent { id, at, actor: AuditActor, action, entity_type, entity_id, changes }`; `struct AuditFilter { entity_type: Option<String>, entity_id: Option<String>, member: Option<MemberId>, agent: Option<AgentTokenId>, from: Option<Timestamp>, to: Option<Timestamp> }`; `struct PageRequest { after: Option<String>, limit: u32 }` (limit clamped 1..=200, default 50); `struct Page<T> { items: Vec<T>, next: Option<String> }`.
  - `Store::list_audit(&self, filter: &AuditFilter, page: &PageRequest) -> Result<Page<AuditEvent>, StoreError>` newest first, cursor = opaque string encoding the last id.
  - `Store::backup_to(&self, dest: &Path) -> Result<(), StoreError>` via `VACUUM INTO`; refuses an existing `dest`.
  - `enum StoreError { NotFound, Conflict(&'static str), Backend(#[source] Box<dyn std::error::Error + Send + Sync>) }`.
- Schema (`0001_identity.sql`), all ids `TEXT PRIMARY KEY`, timestamps `INTEGER` (Unix ms), `STRICT` tables:
  - `members(id, username UNIQUE, display_name, role CHECK IN ('admin','member'), created_at, disabled_at NULL)`
  - `passkeys(id, member_id FK, label, credential_id BLOB UNIQUE, credential_json TEXT, created_at, last_used_at NULL)`
  - `invites(id, token_hash BLOB UNIQUE, kind CHECK IN ('bootstrap','new_member','reenroll'), role NULL, display_name NULL, target_member_id NULL FK, created_by NULL FK, created_at, expires_at, used_at NULL, used_by NULL FK, revoked_at NULL)`
  - `sessions(id, token_hash BLOB UNIQUE, member_id FK, created_at, last_seen_at, idle_expires_at, absolute_expires_at)`
  - `agent_tokens(id, member_id FK, name, scope CHECK IN ('read','write','admin'), token_hash BLOB UNIQUE, prefix, created_at, expires_at NULL, last_used_at NULL, revoked_at NULL)`
  - `audit_events(id, at, actor_kind CHECK IN ('member','system'), actor_member_id NULL, actor_agent_id NULL, actor_system_reason NULL, action, entity_type, entity_id, changes_json)` plus indexes on `(entity_type, entity_id)`, `actor_member_id`, `actor_agent_id`, `at`.
  - Triggers rejecting `UPDATE` and `DELETE` on `audit_events` (append-only).

- [ ] **Step 1: Write failing tests** in `tests/foundation.rs` (each opens a store in a `tempfile::TempDir`):
  - `open_twice_is_idempotent`: open, drop, open same path → Ok.
  - `uncommitted_tx_leaves_no_audit`: begin, `append_audit`, drop → `list_audit` empty.
  - `committed_audit_is_listed_newest_first_with_cursor`: append 3 entries at t, t+1, t+2 in one tx; `list_audit(limit 2)` returns entries 3,2 and `next`; following page returns entry 1 and `next == None`.
  - `audit_filter_by_agent`: entries from `Member{agent: Some(a)}` and `Member{agent: None}` → filter `agent = a` returns only the first.
  - `audit_is_append_only` (unit test in `crates/store/src/audit.rs`, which can reach the pool): after appending one event, `UPDATE audit_events SET action='x'` and `DELETE FROM audit_events` both return an error and the row is unchanged.
  - `backup_produces_openable_copy`: append one event, `backup_to(dir/backup.db)`, `Store::open(backup)` → `list_audit` has it; second `backup_to` same path → Err.
- [ ] **Step 2: Run** `cargo test -p coffer-store` → FAIL.
- [ ] **Step 3: Implement**. `sqlx::migrate!("./migrations")`; runtime-checked queries (`sqlx::query`), no compile-time macros (no `DATABASE_URL` needed in CI). Timestamps converted at the boundary with `Timestamp::as_millisecond()` / `Timestamp::from_millisecond()`.
- [ ] **Step 4: Run** tests → PASS; clippy clean; confirm `cargo tree -p coffer-domain` has no `sqlx`.
- [ ] **Step 5: Commit** `feat(store): sqlite store with migrations, audit log and backup`.

### Task 4: Store repositories for members, passkeys, invites, sessions, agent tokens

**Files:**
- Create: `crates/store/src/members.rs`, `passkeys.rs`, `invites.rs`, `sessions.rs`, `agent_tokens.rs`
- Test: `crates/store/tests/identity.rs`

**Interfaces:**
- Consumes: Task 3 `Store`, `StoreTx`, `StoreError`; domain identity types.
- Produces (records are plain structs with `pub` fields mirroring the schema, domain-typed):
  - Members: `StoreTx::insert_member(&mut self, NewMember { username, display_name, role, created_at }) -> Result<Member>` (`Conflict("username taken")`); `Store::get_member(MemberId) -> Result<Member>`; `Store::get_member_by_username(&Username) -> Result<Member>`; `Store::list_members() -> Result<Vec<Member>>`; `StoreTx::set_member_role(MemberId, Role)`; `StoreTx::set_member_disabled(MemberId, at: Timestamp)`; `StoreTx::count_active_admins() -> Result<u32>`.
  - Passkeys: `StoreTx::insert_passkey(NewPasskey { member, label, credential_id: Vec<u8>, credential_json: String, created_at }) -> Result<PasskeyRecord>`; `Store::list_passkeys(MemberId) -> Result<Vec<PasskeyRecord>>`; `StoreTx::update_passkey_after_use(PasskeyId, credential_json: String, at)`; `StoreTx::delete_passkey(MemberId, PasskeyId)` (`NotFound` if not that member's); `StoreTx::count_passkeys(MemberId) -> Result<u32>`.
  - Invites: `enum InviteKind { Bootstrap, NewMember { role: Role, display_name: Option<DisplayName> }, Reenroll { member: MemberId } }`; `StoreTx::insert_invite(NewInvite { token_hash: [u8; 32], kind, created_by: Option<MemberId>, created_at, expires_at }) -> Result<Invite>`; `Store::find_usable_invite(token_hash: &[u8; 32], now) -> Result<Invite>` (`NotFound` if unknown, used, revoked or `now >= expires_at`); `StoreTx::consume_invite(InviteId, used_by: MemberId, now) -> Result<()>` — single conditional `UPDATE … WHERE used_at IS NULL AND revoked_at IS NULL AND expires_at > now`, `Conflict("invite already used or expired")` if 0 rows; `StoreTx::revoke_invite(InviteId, at)`; `Store::list_pending_invites(now) -> Result<Vec<Invite>>`; `Store::any_admin_exists() -> Result<bool>`.
  - Sessions: `StoreTx::insert_session(NewSession { token_hash, member, created_at, idle_expires_at, absolute_expires_at }) -> Result<SessionRecord>`; `Store::find_live_session(token_hash, now) -> Result<SessionRecord>` (`NotFound` when `now >= min(idle, absolute)`); `StoreTx::touch_session(SessionId, now, new_idle_expires_at)`; `StoreTx::delete_session(SessionId)`; `StoreTx::delete_sessions_for_member(MemberId)`.
  - Agent tokens: `StoreTx::insert_agent_token(NewAgentToken { member, name: TokenName, scope, token_hash, prefix: String, created_at, expires_at: Option<Timestamp> }) -> Result<AgentTokenRecord>`; `Store::find_live_agent_token(token_hash, now) -> Result<AgentTokenRecord>` (`NotFound` if revoked or expired); `Store::list_agent_tokens(Option<MemberId>) -> Result<Vec<AgentTokenRecord>>` (never returns hashes); `StoreTx::revoke_agent_token(AgentTokenId, at)`; `StoreTx::revoke_tokens_for_member(MemberId, at)`; `StoreTx::touch_agent_token(AgentTokenId, at)`.

- [ ] **Step 1: Write failing tests** in `tests/identity.rs`:
  - `username_unique`: second `insert_member` with same username → `Conflict`.
  - `count_active_admins_ignores_disabled`: two admins, disable one → 1.
  - `delete_passkey_of_other_member_is_not_found`.
  - `invite_expires_at_boundary`: invite with `expires_at = t`; `find_usable_invite(hash, t - 1ms)` Ok, `(hash, t)` NotFound.
  - `consume_invite_twice_conflicts`: first Ok, second `Conflict`.
  - `revoked_invite_unusable`.
  - `session_not_found_after_absolute_expiry_even_if_touched`: touch pushes idle beyond absolute; at absolute → NotFound.
  - `revoked_token_not_found` and `expired_token_not_found`.
  - `list_agent_tokens_by_member`: tokens of A and B; `Some(A)` returns only A's.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS; clippy clean.
- [ ] **Step 5: Commit** `feat(store): identity repositories`.

### Task 5: Server crate — config, secret key, token minting and hashing

**Files:**
- Create: `crates/server/Cargo.toml` (package `coffer-server`, `[[bin]] name = "coffer"`), `crates/server/src/lib.rs`, `crates/server/src/main.rs` (stub `clap` CLI with `serve` only, filled in Task 10), `crates/server/src/config.rs`, `crates/server/src/secrets.rs`
- Test: unit tests in `config.rs`, `secrets.rs`

**Interfaces:**
- Produces (`config.rs`): `struct Config { listen: SocketAddr, public_url: Url, data_dir: PathBuf, secret_key_file: PathBuf, invite_ttl: SignedDuration, session_idle: SignedDuration, session_absolute: SignedDuration }`; `Config::from_env(vars: impl Fn(&str) -> Option<String>) -> Result<Config, ConfigError>`. Variables: `COFFER_LISTEN` (default `127.0.0.1:8080`), `COFFER_PUBLIC_URL` (required; must be `https`, except `http://localhost` / `http://127.0.0.1` allowed for development), `COFFER_DATA_DIR` (required), `COFFER_SECRET_KEY_FILE` (default `<data_dir>/secret.key`), `COFFER_INVITE_TTL_MINUTES` (default 30, 1..=1440), `COFFER_SESSION_IDLE_HOURS` (default 168), `COFFER_SESSION_ABSOLUTE_DAYS` (default 30). `Config::database_path(&self) -> PathBuf` = `<data_dir>/coffer.db`.
- Produces (`secrets.rs`):
  - `struct ServerKey` (32 bytes, zeroized on drop, no `Debug` output of bytes). `ServerKey::load_or_create(path: &Path) -> Result<ServerKey, SecretError>`: creates with mode `0600` if absent; refuses if the existing file is not exactly 32 bytes or is group/world-readable.
  - `enum TokenKind { Session, Invite, Agent }`.
  - `struct SecretToken(SecretString)`; `fn mint(kind: TokenKind) -> SecretToken`: 32 random bytes, base64url no padding; agent tokens prefixed `coffer_` (format `coffer_<43 chars>`).
  - `ServerKey::hash(&self, kind: TokenKind, token: &str) -> [u8; 32]`: BLAKE3 keyed hash with a per-kind key from `blake3::derive_key("coffer 2026-10 token hash <kind>", key)`.
  - `fn agent_token_prefix(token: &str) -> String`: first 12 chars (`coffer_` + 5).
  - `ServerKey::csrf_token(&self, session_hash: &[u8; 32]) -> String`: base64url of BLAKE3 keyed hash under a derived `"coffer 2026-10 csrf"` key.

- [ ] **Step 1: Write failing tests**:
  - `config_requires_public_url_https`: `http://coffer.example` → Err; `https://coffer.example` Ok; `http://localhost:8080` Ok.
  - `config_defaults`: only required vars set → listen `127.0.0.1:8080`, invite TTL 30 min, key file under data dir.
  - `config_rejects_invite_ttl_zero`.
  - `key_created_with_0600_and_reloaded_identically` (unix): hash of a fixed token equal across two loads.
  - `key_rejects_world_readable` (unix): chmod 0644 → Err.
  - `hash_differs_by_kind`: same token, `Session` vs `Agent` → different.
  - `agent_token_format`: starts with `coffer_`, length 50, prefix length 12.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement** (`getrandom` for bytes, `secrecy`, `zeroize`, `blake3`). **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `feat(server): config and secret key handling`.

### Task 6: Identity service — passkeys, bootstrap, invites, re-enroll, member management

**Files:**
- Create: `crates/server/src/auth/mod.rs`, `auth/passkeys.rs`, `auth/identity.rs`, `crates/server/src/testkit.rs` (feature `testkit`)
- Test: `crates/server/tests/identity.rs`

**Interfaces:**
- Consumes: Store repos (Task 4), `ServerKey`, `mint`, `Clock`, policy.
- Produces:
  - `Passkeys::new(public_url: &Url) -> Result<Passkeys, AuthError>` — RP id = host, origin = URL origin, RP name `coffer`, user verification required. Holds in-memory ceremonies `HashMap<CeremonyId, Pending>` (TTL 120 s, max 64, single use).
  - `struct Identity { store, key, passkeys, clock, config }` with:
    - `bootstrap_link(&self) -> Result<Option<Url>, AuthError>`: if no admin exists, revokes any prior unused bootstrap invites, creates a new one, returns `<public_url>/setup/<token>`; `None` if an admin exists. Audit `invite.create` actor `System{reason:"system:bootstrap"}`.
    - `create_invite(&self, actor: &Actor, role: Role, display_name: Option<DisplayName>) -> Result<Url, AuthError>` (authorize `ManageInvites`) → `<public_url>/invite/<token>`.
    - `create_reenroll(&self, actor: &Actor, member: MemberId) -> Result<Url, AuthError>` (`ManageMembers`) → `/invite/<token>`.
    - `cli_admin_reenroll(&self, member: &Username) -> Result<Url, AuthError>` (no actor; audit `System{"system:cli"}`; target must be an admin).
    - `revoke_invite(&self, actor: &Actor, invite: InviteId)`; `list_pending_invites(&self, actor: &Actor)`.
    - `begin_enrollment(&self, invite_token: &str, username: Option<Username>, display_name: Option<DisplayName>) -> Result<(CeremonyId, CreationChallengeResponse), AuthError>`: invite must be usable; for bootstrap/new-member, username and display name are required (`display_name` falls back to the invite's suggestion) and the username must be free; for re-enroll they are ignored.
    - `finish_enrollment(&self, ceremony: CeremonyId, label: TokenName, cred: RegisterPublicKeyCredential) -> Result<MemberId, AuthError>`: one `StoreTx`: `consume_invite`, insert member (bootstrap ⇒ Admin; new-member ⇒ invite role) or use the re-enroll target, insert passkey, audit (`member.create`, `passkey.add`). Concurrent second finish fails with `InviteUsed`.
    - `begin_add_passkey(&self, actor: &Actor)` / `finish_add_passkey(&self, actor: &Actor, ceremony, label, cred)` (`ManageOwnCredentials`; excludes existing credential ids).
    - `remove_passkey(&self, actor: &Actor, id: PasskeyId)` — refuses the last one (`LastPasskey`).
    - `begin_login(&self, username: &Username) -> Result<(CeremonyId, RequestChallengeResponse), AuthError>`: unknown or disabled username → `LoginFailed` (same error and message as a failed assertion).
    - `finish_login(&self, ceremony, cred: PublicKeyCredential) -> Result<MemberId, AuthError>`: verifies, updates stored credential counter (`update_passkey_after_use`), audit `session.login`.
    - `set_role(&self, actor, member, role)`, `disable_member(&self, actor, member)` (`ManageMembers`): refuse when it would leave zero active admins (`LastAdmin`); disable also deletes sessions and revokes tokens in the same tx.
  - `enum AuthError { Denied(Denied), InviteInvalid, InviteUsed, UsernameTaken, LastPasskey, LastAdmin, LoginFailed, CeremonyExpired, Busy, Invalid(IdentityError), Webauthn(WebauthnError) [message generic], Store(StoreError) }` — every `Display` safe to show users.
  - testkit: `TestServer` (temp dir, `ManualClock`, public URL `https://coffer.test`), `SoftAuthenticator` helpers `enroll(&mut self, link: &Url, username, display_name) -> MemberId`, `login(&mut self, username) -> SessionCookie` (the latter used from Task 7 on).

- [ ] **Step 1: Write failing tests** in `tests/identity.rs` (soft passkey with UV):
  - `bootstrap_creates_first_admin_then_disappears`: `bootstrap_link()` Some; enroll → member role Admin; `bootstrap_link()` None.
  - `new_bootstrap_link_revokes_previous`: two calls before enrolling; first link → `InviteInvalid`.
  - `invite_creates_member_with_invite_role`: admin creates a Member invite; enroll "bob" → role Member; audit has `invite.create` with actor `Member { member: admin, agent: None }`, then `member.create` with actor `Member { member: bob, agent: None }` (the new member is the actor of their own enrollment).
  - `invite_consumed_once_under_race`: two soft authenticators both `begin_enrollment` with the same link, both finish concurrently (`tokio::join!`) → exactly one Ok, the other `InviteUsed`; `list_members` length 2.
  - `expired_invite_rejected`: advance clock by `invite_ttl` → `begin_enrollment` `InviteInvalid`.
  - `member_cannot_create_invite`: Member session actor → `Denied`.
  - `reenroll_adds_passkey_to_existing_member`: re-enroll link for bob, new authenticator enrolls → bob has 2 passkeys, can log in with the new one.
  - `cannot_remove_last_passkey`; `cannot_disable_last_admin`; `admin_cannot_demote_self_if_last`.
  - `login_unknown_user_and_bad_assertion_same_error`: both `LoginFailed` with identical `to_string()`.
  - `login_requires_user_verification`: authenticator `SoftPasskey::new(false)` enroll fails.
  - `cli_admin_reenroll_only_for_admins`.
- [ ] **Step 2: Run** `cargo test -p coffer-server --features testkit` → FAIL.
- [ ] **Step 3: Implement.** Follow an existing webauthn-rs integration's pattern for ceremony storage and error mapping. WebAuthn user handle = `MemberId` UUID; for enrollment of a new member, generate the `MemberId` at `begin_enrollment` and keep it in the ceremony state.
- [ ] **Step 4: Run** → PASS. Run the `security-review` skill on the diff; fix findings.
- [ ] **Step 5: Commit** `feat(server): passkey identity, invites and member management`.

### Task 7: Sessions and CSRF

**Files:**
- Create: `crates/server/src/auth/sessions.rs`
- Test: `crates/server/tests/sessions.rs`

**Interfaces:**
- Produces:
  - `Sessions::create(&self, member: MemberId) -> Result<SecretToken, AuthError>` (idle/absolute from config).
  - `Sessions::resolve(&self, cookie_value: &str) -> Result<Actor, AuthError>`: live session, member not disabled → `Actor { via: Session, role from member }`; touches `last_seen_at` and extends idle expiry (capped at absolute) at most once per minute.
  - `Sessions::csrf_for(&self, cookie_value: &str) -> String`; `Sessions::check_csrf(&self, cookie_value: &str, presented: &str) -> Result<(), AuthError>` (constant-time compare via `subtle`).
  - `Sessions::logout(&self, cookie_value: &str)`.
  - `fn session_cookie(token: &SecretToken, max_age: SignedDuration) -> HeaderValue` with the attributes from Global Constraints.

- [ ] **Step 1: Write failing tests:** `session_resolves_to_member_actor`; `session_idle_boundary` (advance exactly `session_idle` without activity → `resolve` fails); `activity_extends_idle_not_absolute` (touch every 6 days for 31 days → fails at day 30); `logout_invalidates`; `csrf_bound_to_session` (token of session A rejected for session B); `cookie_attributes` (header contains `HttpOnly`, `Secure`, `SameSite=Strict`, `Path=/`).
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS; security review.
- [ ] **Step 5: Commit** `feat(server): sessions and csrf`.

### Task 8: Agent tokens and request authentication

**Files:**
- Create: `crates/server/src/auth/agent_tokens.rs`, `crates/server/src/http/mod.rs`, `http/extract.rs`, `http/problem.rs`
- Test: `crates/server/tests/agent_tokens.rs`

**Interfaces:**
- Produces:
  - `AgentTokens::create(&self, actor: &Actor, name: TokenName, scope: Scope, expires_at: Option<Timestamp>) -> Result<(AgentTokenRecord, SecretToken), AuthError>` (`ManageOwnCredentials` + `check_token_scope`); `list_own(&self, actor)`; `revoke(&self, actor, id)` (own tokens; admins may revoke anyone's via `ManageMembers`); `list_all(&self, actor)` (`ManageMembers`). Audit actions `token.create` and `token.revoke` (changes include name, scope, expiry — never the hash or secret).
  - `AgentTokens::resolve(&self, bearer: &str) -> Result<Actor, AuthError>`: live token, member not disabled → `Actor { via: Agent { token, scope } }`; touches `last_used_at` at most once per minute.
  - `http::extract::ApiActor` — axum extractor reading `Authorization: Bearer …` (API routes) and `http::extract::PageActor` reading the session cookie (page routes). The API router accepts only `ApiActor` in this plan.
  - `http::problem::Problem { status, code: &'static str, title: String, detail: Option<String> }` → `application/problem+json`; `impl From<AuthError> for Problem` mapping: `Denied` → 403 `forbidden`, `LoginFailed`/unauthenticated → 401 `unauthenticated` (with `WWW-Authenticate: Bearer`), `InviteInvalid`/`InviteUsed` → 410 `invite_unusable`, `LastAdmin`/`LastPasskey`/`UsernameTaken` → 409, `Invalid` → 422 `invalid`, `Store`/`Webauthn` → 500 `internal` (detail omitted; error logged without secrets).

- [ ] **Step 1: Write failing tests** (router-level, `tower::ServiceExt::oneshot`):
  - `token_shown_once_and_stored_hashed`: create; `list_own` has prefix but no secret; DB row hash ≠ token bytes.
  - `revoked_token_is_401`; `expired_token_is_401`.
  - `disabled_member_loses_session_and_tokens`: admin disables bob → bob's cookie and bob's token both 401.
  - `agent_cannot_create_tokens`: no route accepts bearer auth for token creation (POST `/api/v1/tokens` does not exist; page route rejects bearer) → 401/404, and `AgentTokens::create` with an agent actor → `Denied(SessionRequired)`.
  - `member_cannot_mint_admin_token` → `Denied`.
  - `bad_bearer_not_echoed`: `Authorization: Bearer coffer_SECRETVALUE…` invalid → 401 body does not contain `SECRETVALUE`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS; security review.
- [ ] **Step 5: Commit** `feat(server): agent tokens and request authentication`.

### Task 9: Web pages

**Files:**
- Create: `crates/server/src/http/pages/mod.rs`, `pages/layout.rs`, `pages/setup.rs` (bootstrap + invite + re-enroll share one handler set), `pages/login.rs`, `pages/account.rs` (my passkeys, my tokens), `pages/admin.rs` (members, invites), `crates/server/src/http/pages/coffer.js`
- Test: `crates/server/tests/pages.rs`

**Interfaces:**
- Routes (server-rendered with maud; JSON endpoints under `/auth/*` used only by `coffer.js` for ceremonies):
  - `GET /setup/{token}`, `GET /invite/{token}` → form (username, display name; hidden for re-enroll) + passkey button. `POST /auth/enroll/begin` `{token, username?, display_name?}` → `{ceremony, options}`; `POST /auth/enroll/finish` `{ceremony, label, credential}` → sets session cookie, `{redirect: "/account"}`.
  - `GET /login`; `POST /auth/login/begin` `{username}`; `POST /auth/login/finish` `{ceremony, credential}` → cookie. `POST /logout` (CSRF).
  - `GET /account` → passkeys list (add/remove), tokens list (create form: name, scope ≤ role, optional expiry days; revoke). Created token rendered once on the response page with a copy button.
  - `GET /admin/members` (role change, disable, re-enroll link), `GET /admin/invites` (create with role + optional display name, list pending, revoke). Admin-only (`ManageMembers`/`ManageInvites`); others get 403 page.
  - All state-changing form posts and `/auth/*` calls from logged-in pages require the CSRF token (`csrf` form field or `X-CSRF-Token` header). Responses set `Content-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; frame-ancestors 'none'`, `Referrer-Policy: no-referrer`, `X-Content-Type-Options: nosniff`. Invite/setup pages additionally `Cache-Control: no-store`.
  - `coffer.js`: `navigator.credentials.create/get` with base64url conversion; no inline scripts.
- Pages render no financial data and use no external assets.

- [ ] **Step 1: Write failing tests:** `full_journey`: bootstrap link → enroll admin via `/auth/enroll/*` with soft passkey → `GET /account` 200 with cookie → create invite via form → second soft authenticator enrolls "bob" → bob `GET /admin/members` 403 → admin creates write token for self via `/account` form → token appears once in response and not on reload. `post_without_csrf_rejected` (403). `security_headers_present` on `/login`. `invite_page_no_store`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS; security review.
- [ ] **Step 5: Commit** `feat(server): auth and admin web pages`.

### Task 10: JSON API v1 (identity), health, CLI and operations docs

**Files:**
- Create: `crates/server/src/http/api/mod.rs`, `api/me.rs`, `api/members.rs`, `api/audit.rs`, `crates/server/src/http/health.rs`, `crates/server/src/cli.rs`; modify `crates/server/src/main.rs`
- Create: `docs/configuration.md`, `docs/threat-model.md`, `docs/agent-usage.md`
- Test: `crates/server/tests/api.rs`, `crates/server/tests/cli.rs`

**Interfaces:**
- API (bearer only; all `ReadHousehold`):
  - `GET /api/v1/me` → `{ member: {id, username, display_name, role}, agent: {id, name, scope} }`.
  - `GET /api/v1/members` → `{ items: [ {id, username, display_name, role, disabled_at} ] }`.
  - `GET /api/v1/audit?entity_type&entity_id&member&agent&from&to&after&limit` → `{ items: [AuditEvent], next }`; timestamps RFC 3339.
  - Unknown `/api/v1/*` → 404 problem `not_found`.
- `GET /healthz` → 200 `ok` when the DB answers `SELECT 1`, else 503.
- CLI (`clap`): `coffer serve`; `coffer bootstrap-link` (prints link or "an admin already exists", exit 1); `coffer recover-admin <username>`; `coffer backup <dest>`. All read the same env config. `serve` logs the bootstrap link at startup when no admin exists (the only place a link is logged, because there is no other channel and it is single-use and short-lived).
- Docs: `configuration.md` (every env var, default, constraints; reverse-proxy requirement; Tailscale Service as one example), `threat-model.md` (assets, trust boundaries, tailnet exposure, usernames not secret, bootstrap link in logs, host access = root of trust, token/hash design), `agent-usage.md` (create a token at `/account`, call `/api/v1/me`, scope meanings).

- [ ] **Step 1: Write failing tests:** `me_identifies_agent_and_member`; `audit_shows_agent_attribution` (bob's write token can't mutate anything yet in this plan — instead assert the `member.create` and `token.create` events carry the right actors); `audit_pagination_via_api`; `unknown_api_route_problem_json`; `healthz_ok`. CLI (via `assert_cmd`-free approach: call `cli::run(args, env)` in-process): `bootstrap_link_cli_exit_codes`; `backup_cli_writes_file`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** Graceful shutdown on SIGTERM/ctrl-c. Tracing via `RUST_LOG`, default `info`, JSON format when `COFFER_LOG_FORMAT=json`.
- [ ] **Step 4: Run** full gate: `cargo fmt --check && cargo clippy --workspace --all-targets --all-features --locked -- -D warnings && cargo test --workspace --all-features --locked && cargo deny check` → all pass. Manual smoke: `COFFER_PUBLIC_URL=http://localhost:8080 COFFER_DATA_DIR=$(mktemp -d) cargo run -p coffer-server -- serve`, open the logged setup link in a browser, register a passkey, create a token, `curl -H "Authorization: Bearer …" localhost:8080/api/v1/me`.
- [ ] **Step 5: Commit** `feat(server): identity api, health, cli and ops docs`.
- [ ] **Step 6: Whole-branch security review** with the `security-review` skill; fix findings; commit.

### Task 11: Public repository baseline

Added after the plan was written, from the owner's "Public repository baseline" standard (it must land in the first PR). Reference implementations: the owner's other public Rust projects (crate-age and CI). Copy their files; don't rewrite them.

**Files:**
- Modify: `.github/workflows/ci.yml` (add the `crate-age` job; keep the existing jobs)
- Create: `.github/scripts/crate_age.py`, `.github/scripts/test_crate_age.py`, `.github/crate-age-allow.toml`, `.github/requirements-lint.txt`, `.github/ruff.toml` (copied from the reference project — `MIN_AGE = timedelta(days=7)`, allow-list accepts RUSTSEC and GHSA ids — with the allow-list emptied of entries for crates coffer doesn't use and the user-agent repo name changed)
- Already on `main` (PR #1, 438a375): `.github/workflows/zizmor.yml`, `.github/workflows/scorecard.yml`, `.github/dependabot.yml` (github-actions only). Rebase onto `origin/main` first, keep zizmor.yml and scorecard.yml as they are, and *extend* dependabot.yml with the cargo ecosystem (`cooldown: default-days: 7`, minor+patch grouped).
- Create: `.github/workflows/advisories.yml` (nightly `cargo deny check advisories`), `.github/pull_request_template.md` (from the reference project)
- Modify: `README.md` ("Dependency age" section, as in the reference project), `AGENTS.md` (mention crate-age, zizmor and the "fix, don't exempt" rule)

**Rules:**
- Every action pinned to the full commit SHA of a release at least 7 days old (comment with the tag). `permissions: contents: read` at the top (or `{}` with per-job grants). `persist-credentials: false` on every checkout. `--locked` everywhere.
- Every crate version in `Cargo.lock` must be at least 7 days old, or the crate-age check fails. Fix by downgrading to an older version, not by adding allow entries. Allow entries are only for security fixes, each naming its RUSTSEC or GHSA advisory.
- Fix, don't exempt: no zizmor config, no `deny.toml` ignores, no `#[allow]`.
- No release workflow yet (no releases in plan 1).

- [ ] **Step 1:** Copy the crate-age files and wire the job (the base is `origin/main` for PRs, as in the reference project). Run `python3 -I -m unittest discover -s .github/scripts -p 'test_*.py'`, then `python3 -I .github/scripts/crate_age.py --base main`, and fix any too-young crate by pinning older versions. Re-run the full cargo gate.
- [ ] **Step 2:** Add the advisories workflow and PR template, and extend dependabot.yml with cargo. Run `GH_TOKEN=$(gh auth token) uvx zizmor .` and expect no findings.
- [ ] **Step 3:** Update the README and AGENTS.md. Commit `ci: public repository baseline`.

Repository settings are mostly in place already: a ruleset on `main` (PR required, squash only, signed commits, linear history, required check `zizmor`), a tag ruleset, CodeQL default setup, Dependabot, private vulnerability reporting, SHA pinning, and an actions allow-list (GitHub-owned plus dtolnay/rust-toolchain, Swatinem/rust-cache, EmbarkStudios/cargo-deny-action, zizmorcore/zizmor-action, ossf/scorecard-action). Any action outside that list will be refused. This branch lands as a PR. After it merges, the CI job names and `crate-age` are added to the required checks, plus a CodeQL code-scanning rule, through `gh api` after the owner confirms.
