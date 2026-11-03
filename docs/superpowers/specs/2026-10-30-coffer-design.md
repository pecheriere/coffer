# coffer — design

Status: ready for owner approval · 2026-10-30

## 1. Intent

coffer is a self-hosted **household financial hub**: a backend that holds a
family's accounts, balances, transactions and spending plan, and that AI agents
(and later a UI) query to answer questions and help plan.

The motivating question is not day-to-day budgeting. It is a periodic one —
at a regular planning point, for example every quarter when a bonus or equity
compensation pays out:

> "Over the next N months, how much cash do we actually need — recurring bills,
> known big expenses, money set aside for things coming later, plus a safety
> floor? Everything above that can be invested."

Getting this wrong in one direction leaves idle cash; in the other it means
selling investments to cover a commitment that was foreseeable. YNAB answers it
well (targets, set-aside money, categories) but is built around day-to-day
envelope budgeting and is not self-hosted or agent-first. Monarch gives net
worth and categorization but not the forward-looking cash plan.

Secondary purposes:

- **Net worth over time** across all accounts, in a preferred currency.
- **Categorized spending** to understand where money goes, with a review flow
  the owner enjoys doing (often through an agent, conversationally).
- **Plan vs. actuals**, so the plan stays honest — and, later, self-correcting.

Agents are first-class users: they get their own identities, scoped tokens,
a documented REST API and an MCP server. A full UI comes later; v1 ships only
the minimal web pages that passkey authentication requires.

The project will be open source. It must be generic: nothing specific to the
owner (banks, categories, Tailscale hostnames, Pi layout) lives in the repo.
Owner-specific deployment lives in the owner's own infrastructure repo.

## 2. Goals (v1)

1. **Household & identity.** One household per deployment, several members.
   Members authenticate **only with passkeys**. Admins invite members via
   single-use, expiring links. First admin is bootstrapped via a one-time setup
   link.
2. **Agent identities.** Members create named, revocable API tokens for their
   agents. A token acts *on behalf of* its member with a scope no greater than
   the member's role. Every change records both the member and the agent.
3. **Accounts.** Linked (SimpleFin) and manual accounts, each with its own
   currency and a classification that drives forecasting and net worth.
4. **Sync.** Scheduled and on-demand import of accounts, balances and
   transactions from one or more SimpleFin connections.
5. **Transactions & review.** Categorize (with splits), mark transfers, mark
   reviewed, assign to a member for review, add notes — all by API. Rules
   auto-categorize on import.
6. **Plan.** Planned items: recurring on flexible schedules, one-off, and
   savings goals (sinking funds). Expected income optionally modeled.
7. **Forecast.** "Cash needed over the next N months vs. cash available →
   surplus to invest", with full breakdown, plus what-if simulation with
   hypothetical items.
8. **Reports.** Spending by category/period, plan vs. actual by category/month,
   net worth history.
9. **Multi-currency.** Per-account currency, exact amounts, daily FX rates,
   conversion to each member's display currency.
10. **Agent-friendly interfaces.** REST JSON API with a generated OpenAPI spec,
    and an MCP server over streamable HTTP, both authenticated by agent token.
11. **Auditability.** Every mutation is in an append-only audit log.

## 3. Non-goals (v1)

- A real UI (dashboards, transaction lists). Only the auth/admin pages and the
  forecast page, which can settle a bill, and read-only net worth page in §6.4.
- Per-account visibility permissions (balance-only vs. full). v1: every member
  sees everything. The authorization design (§6.5) must make adding this a
  local change, but no tables or code for it exist in v1 (superseded for agent
  tokens by §8.2).
- Automatic plan adjustment. v1 provides the plan-vs-actual data; suggestions
  come later and will never change the plan without confirmation.
- Investment holdings, performance, cost basis, vest schedules. Investment and
  equity accounts are tracked by balance only.
- Moving money, bill pay, any write access to financial institutions.
- Zero-based / envelope budgeting à la YNAB ("give every dollar a job").
- Multiple households per deployment (multi-tenancy).
- Data sources other than SimpleFin and manual entry (CSV/OFX import later).
  The provider interface (§6.2) is in v1; other implementations are not,
  except Plaid (§8.6).
- OAuth for MCP (needed for hosted clients like claude.ai). v1 uses bearer
  tokens; the token design must not preclude adding OAuth later.
- Native mobile apps, notifications.

## 4. Anti-goals (failure even if it "works")

- **Floating-point money.** Any amount stored or computed as a float.
- **Silent changes.** The system changing a user's plan, categorization or
  review status without an attributable actor. Rules apply only to
  uncategorized imports and are themselves attributed.
- **Indistinguishable agents.** Any mutation where you cannot tell whether a
  human or which agent made it.
- **Credential leakage.** SimpleFin access URLs, session cookies or tokens in
  logs, API responses, error messages or the repo. Tokens stored in clear.
- **Owner-specific code.** Anything about the owner's banks, categories, hosts
  or network in the repository.
- **Clobbering user work on sync.** A re-import overwriting a user's category,
  notes, splits or review state.
- **Duplicate transactions** from overlapping sync windows or pending→posted
  transitions.
- **Wrong forecast silently.** The forecast hiding what it assumed. Every
  number must be explainable from its breakdown (which items, which accounts,
  which FX rates, rates' dates).

## 5. Constraints

- **Language:** Rust (edition 2024), single static-ish binary.
- **Code quality** (the owner's standard):
  `unsafe_code = "forbid"`, clippy `all` + `pedantic` at **deny**, no
  `#[allow]` without a written justification, `cargo-deny` for licenses and
  advisories, warnings fail CI, no dead code, no TODOs in source.
- **License:** MIT OR Apache-2.0.
- **Storage:** SQLite for v1, behind a storage interface so Postgres can be
  added without touching domain or API code.
- **Deployment target:** a container on a small ARM64 Linux host (Raspberry Pi
  class), reached over Tailscale as a Tailscale Service which terminates TLS.
  coffer itself must not depend on Tailscale: it is a plain HTTP server behind
  any TLS-terminating proxy. Config via environment and/or a config file; all
  state under one data directory.
- **Passkeys require HTTPS and a stable origin.** WebAuthn Relying Party ID and
  origin are configuration, set to the externally visible hostname.
- **External network access** is limited to SimpleFin and the FX rate source
  (extended by §8.5).
- **SimpleFin limits:** the SimpleFin Bridge rate-limits (roughly 24 requests
  per connection per day) and returns at most ~90 days per request. Sync
  scheduling must stay well within this.

## 6. Design

### 6.1 Core model

Entities (names indicative; the builder chooses exact schema):

- **Household** — singleton. Settings: base currency, forecast defaults (§6.8).
- **Member** — display name, role (`admin` | `member`), display currency,
  created/disabled timestamps. Members are disabled, never hard-deleted
  (audit references them).
- **Passkey** — belongs to a member; WebAuthn credential, friendly name,
  created/last-used.
- **Invite** — created by an admin; token hash, intended role, optional
  suggested display name, expiry, used-at, used-by.
- **Session** — browser session for a member (pages in §6.4 only).
- **AgentToken** — belongs to a member; name, scope (`read` | `write` |
  `admin`), token hash, prefix for display, created, last used, expires
  (optional), revoked.
- **Connection** — a link to an external data source: provider type
  (`simplefin` in v1), encrypted provider credentials (opaque to the core),
  owner member (who set it up), status, last sync, last error.
- **Account** — name, currency, kind, liquidity class, source (connection +
  external id | `manual`), include-in-net-worth flag,
  include-in-forecast flag, closed flag.
- **BalanceSnapshot** — account, date, balance, source (`sync` | `manual`).
  One per account per day (last write wins for that day).
- **Transaction** — account, external id (if imported), posted date,
  transacted date (if provided), pending flag, amount, raw description, payee
  (cleaned/user-editable), memo, review status (`unreviewed` | `reviewed`),
  assignee (member, optional), notes, transfer link (optional), and one or
  more **splits** (category, amount, optional goal link) summing to the
  amount. An uncategorized transaction has zero splits.
- **CategoryGroup / Category** — user-defined, ordered, archivable. Category
  kind: `expense` | `income` | `transfer`. Ships with no categories (an
  optional, generic starter set may be offered via an API call; the builder
  decides).
- **Rule** — ordered; conditions on description/payee (contains, equals,
  regex), account, amount range, direction; actions: set category, set payee.
- **PlannedItem** — see §6.7.
- **FxRate** — date, base, quote, rate (exact decimal), source.
- **AuditEvent** — see §6.6.

**Money:** every amount is an integer in **minor units** plus its ISO 4217
currency; exponent from ISO 4217 (USD 2, JPY 0, etc.). FX rates are exact
decimals; conversion rounds once, half-even, at the end, and the rounding
rule is documented. Sign convention: negative = money out of the account.

**Account classification** (drives forecast and net worth):

- kind: `checking`, `savings`, `credit_card`, `loan`, `mortgage`, `brokerage`,
  `retirement`, `equity_comp`, `property`, `vehicle`, `other_asset`,
  `other_liability`.
- liquidity class: `cash` (counts as available cash), `short_term_liability`
  (credit cards: subtracted from available cash, since it will be paid from
  cash), `invested`, `illiquid`, `long_term_liability`. Defaulted from kind,
  user-overridable.

### 6.2 Sync

**Data sources are pluggable.** The core never talks to SimpleFin directly.
A *provider* interface covers: connect (turn user-supplied setup input into
stored, encrypted credentials), fetch accounts with balances, fetch
transactions for a date window, and report per-account errors and its own
rate limits. Providers return a normalized shape (accounts, balances,
transactions with stable external ids, pending flag, currency) and know
nothing about categories, review, rules, audit or storage. Everything below
that is not marked SimpleFin-specific — windowed upsert, user-state
preservation, pending→posted reconciliation, rule application, balance
snapshots, error recording — lives in the provider-agnostic sync engine, so
adding a provider (Plaid, GoCardless, a CSV/OFX importer, a broker API) means
implementing that interface only. Each provider declares what it supports
(e.g. pending transactions, holdings later) and its connect flow's input.

**Source records vs. canonical data.** What a provider reports is stored as
a *source record*, separate from the canonical Account/Transaction the rest of
coffer uses:

- An **account binding** links (connection, provider account id) to a coffer
  Account. An Account may have several bindings (the same real bank account
  seen through two providers, or re-linked after a provider change); new
  provider accounts can be bound to an existing Account instead of creating
  one.
- A **transaction source record** holds (connection, provider transaction id,
  imported fields, a normalized **fingerprint**) and links to one canonical
  Transaction. A Transaction may have several source records. User state
  (splits, notes, review, assignee, payee edits) lives only on the canonical
  Transaction, so it survives provider changes.
- **Matching** an incoming record is a pipeline in the sync engine: (1) same
  connection + provider id → same transaction; (2) otherwise, fingerprint
  match against canonical transactions of the same Account within a date
  tolerance → attach to it; (3) otherwise create a new transaction. The
  fingerprint is built from normalized fields (account, amount, date,
  normalized description); its exact definition and tolerance are the
  builder's, and it is versioned so it can evolve.
- v1 uses this pipeline for **pending→posted reconciliation** (a posted
  record matching a pending one attaches to the same canonical transaction,
  keeping user state). Cross-provider matching uses the same steps and needs
  no schema change when a second provider is added. Ambiguous matches
  (several candidates) never auto-merge; they create a new transaction and
  are flagged as a possible duplicate for review. Merge and un-merge of
  canonical transactions are API operations, audited.

**SimpleFin (the v1 provider):**

- **Connect:** a member pastes a SimpleFin setup token; coffer claims the
  access URL and stores it **encrypted at rest** with a key supplied via
  config (file or env), never in the database in clear and never logged.
- **Schedule:** periodic sync (default a few times per day, configurable, and
  bounded to respect SimpleFin limits), plus an on-demand trigger via API/MCP
  that is rate-limited the same way.
- **Window:** each sync requests from (last successful sync − overlap, default
  14 days) to now, so late-posting and pending→posted changes are caught. First
  sync uses the maximum window SimpleFin allows.
- **Upsert via the matching pipeline above.** Imported fields (amounts,
  dates, pending, raw description) update; user fields (payee if
  user-edited, splits, notes, review status, assignee, transfer link) are
  never touched.
- **Pending → posted:** SimpleFin may give the posted transaction a new id;
  the fingerprint step attaches it to the pending one's canonical
  transaction. A pending record that disappears without a posted match is
  removed from its transaction; a transaction left with no source records
  and no user state is deleted, one with user state is kept and flagged for
  review.
- **New accounts** from a provider appear unbound; the API reports them so an
  agent can ask the user whether to create a new Account (defaults from
  kind) or bind to an existing one. Source records for unbound accounts are
  kept and run through matching when the account is bound.
- **Balances:** each sync writes today's BalanceSnapshot for linked accounts,
  and keeps the balance of an unbound one, one per day, for the bind to bring in.
- **Errors:** SimpleFin per-account errors and connection failures are stored
  on the Connection and exposed via API; sync never partially commits one
  account's data.
- **Rules** run on newly imported, uncategorized transactions only. Applied
  rules are attributed in the audit log as `system:rule:<id>`.

### 6.3 Authentication

**Members (humans): passkeys only.** No passwords, no email.

- **Bootstrap:** on first start with an empty database, the server logs (and
  the CLI can print) a one-time setup link to register the first admin. It is
  single-use and expires (default 30 minutes; a CLI command can mint a new
  one while no admin exists).
- **Staging seed:** `serve` refuses a database where an admin holds the staging seed's
  public passkey unless the public URL is on localhost, and warns about it on a listen
  address that isn't loopback. Away from localhost, registering a seeded key is refused, by
  id or public key. This guards against an accident; it is not a security boundary.
- **Invites:** admins create invite links: single-use, expiring after a
  configurable number of minutes (default 30), carrying the role. Opening it
  registers a passkey and creates the member. Admins can list and revoke
  pending invites.
- **Passkey management:** members can add and remove passkeys; removing the
  last passkey is refused. Admins can disable a member (kills sessions and
  revokes their tokens).
- **Recovery:** an admin can issue a "re-enroll" link for a member who lost all
  passkeys (same properties as an invite). If the only admin loses all
  passkeys, a CLI command on the host mints an admin re-enroll link (host
  access is the root of trust).
- **Sessions:** cookie-based, `HttpOnly`, `Secure`, `SameSite=Strict`, CSRF
  protection on forms, idle and absolute expiry. Sessions are only for the
  web pages; the JSON API and MCP accept agent tokens. (A future UI will call
  the API with the session; the builder may design the API auth layer to
  accept either.)

**Agents: tokens.**

- Created by a logged-in member on the token page; shown once; stored as a
  hash (fast keyed hash is fine — tokens are high-entropy); display prefix
  kept for identification.
- Scope: `read` (GET only), `write` (household data mutations, see matrix
  below), `admin` (also admin-only actions). Effective rights =
  min(token scope, member role). An agent token can never create tokens.

**Role matrix** *(designer's call)*:

| Action | member | admin |
|---|---|---|
| Read all household data | ✓ | ✓ |
| Transactions, splits, review, assign, notes | ✓ | ✓ |
| Accounts (classify, rename, manual balances/transactions) | ✓ | ✓ |
| Categories, rules, planned items, goals | ✓ | ✓ |
| Add a SimpleFin connection; trigger sync | ✓ | ✓ |
| Remove/replace a connection | own only | any |
| Own passkeys, own tokens, own display currency | ✓ | ✓ |
| Household settings (base currency, forecast defaults, floor) | | ✓ |
| Members, invites, re-enroll links, others' tokens | | ✓ |

The last admin cannot be disabled or demoted.
- Revocable instantly; optional expiry; last-used tracked.
- Disabling a member revokes their tokens.

### 6.4 Web pages (v1, minimal)

Server-rendered, no SPA, minimal JS only for WebAuthn ceremonies, Plaid Link,
small page helpers such as copy buttons, and reporting browser errors to
coffer's own log (`docs/configuration.md`, "Browser reports"). Pages:
setup (bootstrap), accept invite / re-enroll, log in, log out, my passkeys,
my agent tokens, SimpleFin connections, and for admins: members and invites.
No financial data pages in v1, except a small forecast page, read-only but for settling a bill (§8.9,
R14) and a read-only net worth page (§8.10, N13). (Connections get a page because pasting a setup token is a human step.)

### 6.5 Authorization

All authorization decisions go through one policy component that receives
the actor (member, optional agent token, scope) and the action/resource. In
v1, resource-level checks are trivial (every member can see all household
data); the point is that per-account visibility later is a change to that
component plus the queries it scopes, not a sweep through every handler.

### 6.6 Audit

Every mutation writes an AuditEvent in the same database transaction:
timestamp, actor member (or `system` with a reason such as
`system:sync`/`system:rule:<id>`), agent token (nullable), action, entity type
and id, and a before/after JSON diff of changed fields. Credentials never
appear in it. Readable via API (filter by entity, actor, agent, time).
Transaction responses include who last changed the categorization and who
marked it reviewed (member + agent).

### 6.7 Plan

A **PlannedItem** has: name, category, amount and currency, direction
(outflow/inflow), optional account hint, notes, active flag, and a type:

- **Recurring** — a schedule: every N days | weeks | months | years, an anchor
  date, optional end date (covers monthly, bi-monthly water bills,
  semi-annual, annual). Optional day-of-month semantics for monthly items
  (e.g. "last day of month") are the builder's call.
- **One-off** — a single date.
- **Goal (sinking fund)** — target amount, due date, start date. Money is
  "set aside" linearly from start to due date: the **earmarked amount** at any
  date = target × elapsed / total period, minus spending already linked to
  the goal (splits can link to a goal). No manual "assign money" step.
  *(This linear accrual is the designer's call, not something the owner
  specified; it mirrors the owner's YNAB habit of putting a bit aside every
  month.)* Goals can be marked complete. A goal and a one-off describing the
  same expense would be counted twice; the API documents this and the
  forecast warns when a goal and a one-off share a category and fall within
  a month of each other, but does not de-duplicate.

Expected **income** items (salary, bonuses, equity vests) are planned items with
direction inflow; whether they count in the forecast is a forecast option.

### 6.8 Forecast

The horizon is a **rolling window** from `as_of` to the same date N months
later (end exclusive), matching "the next 3 months from vest day"; the
monthly breakdown buckets by calendar month, so the first and last buckets
are partial. *(Designer's call.)*

`forecast(as_of = today, horizon = N months, options)` returns, in the
requester's display currency (with original currencies preserved in the
breakdown):

- **Cash available** = Σ balances of `cash` accounts − Σ owed on
  `short_term_liability` accounts (latest snapshots, FX at latest rate). Each
  account listed with balance, snapshot date and rate used; stale balances
  (older than a threshold) flagged.
- **Cash needed** =
  - planned outflows (recurring occurrences + one-offs) falling in the
    horizon; for the current month, a recurring item's remaining amount is
    reduced by actual spending already in its category this month (never
    below zero) — *designer's call*;
  - goals: for goals due within the horizon, the full remaining target; for
    goals due after it, the earmarked amount at horizon end;
  - the **safety floor** (§ below);
  - minus planned inflows in the horizon, if the "include income" option is
    on (household default configurable).
- **Surplus** = cash available − cash needed (negative = shortfall).
- **Breakdown** by month and by item, each line traceable to a PlannedItem,
  goal or account.

**Safety floor (rainy-day fund):** a minimum cash amount that must stay
untouched *on top of* the horizon's planned spending and goal savings. The
owner's model is exactly: next 3 months of expenses + money set aside for
known future spending + a rainy-day fund held at a fixed value. So the floor
is configurable as a fixed amount (the owner's case, and the default kind)
or as "M months of planned recurring outflows". Household default stored in
settings; per-call override allowed.

**What-if:** the same computation accepting extra hypothetical items and
overrides (horizon, floor, include income) in the request, persisting nothing.
This is the agent's main planning tool ("what if we also do a $15k roof in
March?").

### 6.9 Reports

- **Spending** by category / group / payee / member-assignee over a period,
  excluding `transfer` categories; income separately.
- **Plan vs. actual** per category per month: planned (occurrences normalized
  into months), actual, difference; and a rolling average of actuals per
  category over a window. This is the data the future self-correcting plan
  will use. **Goals** appear as their own lines (accrued vs. spent-against),
  and splits linked to a goal count against that goal, not as actuals of
  their category, so planned big-ticket spending never shows up as category
  overspend. *(Designer's call.)*
- **Net worth history:** per day/week/month, sum of `include_in_net_worth`
  accounts using each account's latest snapshot on or before that date,
  converted at **that date's** FX rate. Breakdown by liquidity class and by
  account.
- **Review queue:** unreviewed transactions, filterable by assignee.

### 6.10 FX

- Rates fetched daily from a pluggable provider. Default: the European Central
  Bank reference rates (free, no key, daily, history available). Only the
  currencies in use are needed. ECB publishes EUR-based rates; cross rates are
  derived exactly via EUR.
- Historical backfill on first use of a currency, so net worth history can be
  converted.
- If no rate exists for a date (weekends, holidays), use the most recent
  earlier one; responses report the rate date used.
- If a needed rate is missing entirely, the response says so explicitly
  rather than silently omitting the account.

### 6.11 API

- REST JSON under `/api/v1`, bearer agent token. OpenAPI document generated
  from code and served at a stable path.
- Errors as `application/problem+json` with stable machine-readable codes.
- Cursor pagination for lists. Transactions filterable by date range,
  accounts, categories, uncategorized, review status, assignee, amount range,
  text search on description/payee/notes, pending.
- **Batch mutations** for the review flow (categorize/mark reviewed/assign
  many transactions in one call), applied atomically.
- Idempotency keys accepted on create endpoints (agents retry).
- When a user correction makes a recurring pattern obvious (the same payee
  recategorized), the categorize response may include a **suggested rule**
  that the agent can offer to create; it is never created automatically.
- Resource areas: household & settings, members, invites, tokens, connections
  & sync, accounts & balances (incl. manual balance entry; manual accounts
  may also hold manually created transactions, which have no external id and
  are never touched by sync — *designer's call*), transactions &
  splits & transfers, categories, rules (incl. "dry-run against existing
  transactions"), plan items, forecast & what-if, reports, FX rates, audit.

### 6.12 MCP

An MCP server over streamable HTTP at `/mcp`, same bearer tokens and same
authorization as the API. Tools mirror the high-value API operations (search
transactions, review/categorize batch, accounts & balances, forecast &
what-if, reports, plan items CRUD, sync trigger, set manual balance). Tool
descriptions should teach the agent the domain (sign conventions, liquidity
classes, what the forecast means). Read-only tokens see only read tools.

### 6.13 Operations

- Single binary with subcommands: `serve`, plus admin CLI for bootstrap link,
  admin re-enroll link, and a **consistent backup** (`VACUUM INTO`-style
  snapshot of the SQLite database to a path) so external backup tools copy a
  safe file.
- Schema migrations embedded and run at startup.
- Structured logs (no secrets, no full transaction descriptions at info
  level), health endpoint.
- Container image: non-root, minimal base, multi-arch (amd64 + arm64).
- Documentation in the repo: README, configuration reference, deployment
  example behind a TLS proxy (Tailscale Service given as one example, not a
  requirement), agent usage guide, threat model.

### 6.14 Development practices (repository requirements)

The repo is developed largely by AI agents, so it must be agent-friendly and
held to a high bar:

- **Latest stable Rust**, pinned via `rust-toolchain.toml`, edition 2024, and
  current major versions of dependencies at the time of writing.
- **`AGENTS.md`** at the root: architecture map, crate responsibilities,
  commands to build/lint/test, conventions (money, errors, audit, authz policy
  usage), what "done" means, and the rules below. `CLAUDE.md` is a pointer to
  it, not a copy.
- **`CONTRIBUTING.md`** for humans and agents alike, plus `SECURITY.md`
  (vulnerability reporting) and the threat model document.
- **CI (GitHub Actions)** gating every PR: fmt check, clippy at deny,
  tests, `cargo-deny`, docs build, and a check that the committed OpenAPI
  document matches the generated one. Warnings fail the build.
- **Tests that earn their place.** Tests specify behavior (forecast math,
  schedule occurrences, FX conversion and rounding, sync idempotency and
  user-state preservation, authz decisions, passkey/invite flows). No tests
  that restate the implementation, assert on mocks of the code under test,
  or exist for coverage numbers. Property tests where the domain invites them
  (money arithmetic, schedules).
- **Security review** is required for any change touching authentication,
  sessions, tokens, authorization policy, credential storage/encryption, or
  outbound requests (SimpleFin, FX) — documented in `AGENTS.md` as a step
  before merge.
- Clear module boundaries: domain logic (money, plan, forecast, reports) is
  pure and independent of HTTP and storage.

## 7. Left to the builder

- Crate/workspace layout, exact schema, ID format, library choices (axum, sqlx/SQLite, webauthn-rs and maud are a proven combination, and
  rmcp is the usual choice for MCP — reuse is encouraged but not required).
- Encryption scheme for the SimpleFin access URL and how the key is supplied.
- Pending→posted matching heuristic, transfer auto-detection (if any).
- Whether to offer a generic starter category set.
- Sync schedule defaults within SimpleFin limits.
- Test strategy, provided: domain logic (money, schedules, forecast, FX,
  net worth) is unit-tested; SimpleFin is tested against a mock server;
  passkey flows are tested end-to-end with a software authenticator.

## 8. Addendum (2026-10-31)

Decisions made while planning the typed API, accounts, transactions, matching
and provider sync (plan 2a, with Plaid in plan 2b). Where this section differs
from an earlier one, this section governs.

### 8.1 Typed API now

The OpenAPI 3.1 document (§6.11, §6.14) is generated from code from plan 2 on
(issue #6). It is served unauthenticated at `/api/v1/openapi.json`, committed at
`docs/api/openapi.json`, compared with the generated document by a test, and
linted in CI. Problem `type` is `urn:coffer:problem:<code>`. Document to router
is tested per operation and method; router to document holds by construction:
`ApiRouter` is the only way to add an API route, and the application router
merges nothing else under `/api`.

### 8.2 Per-account authorization

§3's "no tables or code for it" is superseded for agent tokens (issue #5):
every account-scoped decision goes through
`authorize_account(actor, access, account, level)` with levels
`balances < transactions < write`. Plan 2a resolves every actor to uniform
access at its scope; plan 2b adds per-token grants. Per-member visibility
remains a non-goal. Hidden accounts answer exactly like missing ones (404); a
visible account with too low a level answers 403 naming the level needed.

An agent token may carry grants: up to 100, each naming `all` or one account
and a level, no target twice, none above the token scope's ceiling (`read` →
`transactions`; `write` and `admin` → `write`). The effective level on an
account is min(best grant naming it or `all`, the ceiling of the token's
effective scope, the member's own access), the member's own access being
uniform `write`. An account no grant names is hidden: 404 `not_found`,
exactly like a missing account; a visible account below the level needed
answers 403 `forbidden` with "this action needs <level> access to this
account". A token without grants, every token created before grants, and
every passkey session have uniform access at their scope, as in plan 2a. An
account created after a token is visible to it only through an `all` grant.
Grants change only from a passkey session, by the token's member or a member
who may manage members, audited with the grants before and after. They can
never be emptied, since that would silently widen the token: widening takes
an explicit `all` grant.

Actions that touch more than one account, or accounts not yet granted, need
an `all` grant at the level shown: reading the audit log, listing unbound
provider accounts and reading a connection's sync runs (whose counts and
errors cover all its accounts) and a connection's last error (which could
name any of them; without it the field is empty and the status remains) need
`transactions`; creating an account, binding a provider account, triggering
a sync, adding a connection (whose first sync queues work on accounts not yet
granted), reconnecting or removing one (which changes the feed of every
account it serves) and creating or changing a category or category group
(shared by every account) need `write`. Using an existing category on a split
is governed by write access to that transaction's account. Without it the action answers 403 `forbidden` with "this action
needs <level> access to every account". Idempotency keys belong to the
credential that sent them (the session or one agent token), so no token
replays a response another credential earned. Lists of account-scoped
data (accounts, transactions, bound provider accounts) are filtered to the
accounts granted at the level they show, and no list, aggregate, audit entry
or error reveals a hidden account. A connection's label is a member's free text,
which could name who an account is for, so a connection follows what it serves:
a token sees it, at whatever level, only if it serves an account the token sees,
or with an `all` grant, which is also the only way to see one serving no account
yet. Otherwise it is left out of the list, and the routes naming a connection,
which all need an `all` grant, refuse it as they refuse a missing one.

### 8.3 Provider credentials

Provider credentials are sealed with XChaCha20-Poly1305 under a key derived
(BLAKE3 `derive_key`, context `coffer 2026-10 provider credentials v1`) from
the server key in `COFFER_SECRET_KEY_FILE`. A sealed value is
`version || nonce || ciphertext || tag`; a version byte allows a dedicated key
later. The associated data is `"coffer connection credentials\0" || version ||
connection id`, so a value opens only for its connection and under the version
it was sealed with. Losing the server key
means reconnecting every provider. A connection's credentials can be replaced
(reconnect) without losing its bindings.

### 8.4 Matching v1

The description key is the uppercase ASCII letters of the description, every
other character being a separator, reduced to the first two tokens of three or
more letters. The fingerprint is `fp1:<amount minor units>:<description key>`.

A candidate is a canonical transaction of the same Account that is imported
(manual transactions are never candidates), dated within 4 days, and has no
live source from the same connection. It must have the same amount; or, when
all its live sources are pending records that vanished from the current fetch,
an amount within 20 % and the same non-empty description key (a pending charge
posting with a tip). One candidate attaches. Several candidates are narrowed to
those with an equal description key and amount; if still ambiguous the record
becomes a new transaction flagged as a possible duplicate.

Removal of vanished sources happens after matching. A pending record still
present beside its posted form produces a flagged possible duplicate, never a
silent second transaction. When an imported amount changes and the transaction
has one split, that split follows the amount (a `system:sync` change); with
several splits they stay and the transaction is flagged.

A transaction fed by several connections takes its imported fields (date,
pending state, amount, description, payee, memo) from one of them, its owner:
the connection of the earliest live posted source, or without a posted one, of
the earliest live source. Earliest is by first sighting; live means not
vanished from the current fetch and not of a removed connection. Only the
owner's records write to the transaction. The others are kept up to date as
sources and write nothing, so two providers describing one purchase differently
never rewrite it in turn. Ownership moves only when a source changes: a record
posts, a record attaches, or the owner's last record vanishes, in which case
the transaction takes the next owner's fields once. A next owner with only
pending records takes over too, but a pending record never overwrites a posted
transaction, so a posted transaction keeps its fields until that owner's record
posts. A removed connection's fields stay until the owner next reports a change. With one connection nothing
differs: all its records write as they always did.

### 8.5 Outbound network

§5's "limited to SimpleFin and the FX rate source" now reads: SimpleFin Bridge
hosts (`COFFER_SIMPLEFIN_HOSTS`, default `bridge.simplefin.org`,
`beta-bridge.simplefin.org`), Plaid's API host when Plaid is configured (plan
2b), and the FX source (`data-api.ecb.europa.eu`, §8.10, N6). HTTPS on port 443 only, no redirects, and
targets must resolve to public addresses. `HTTPS_PROXY` is honoured when set;
then coffer does not resolve target hosts, and the public-address check is the
proxy's job. `COFFER_EGRESS_RELAY`, one IP address and refused together with
`HTTPS_PROXY`, is for a network with no DNS and no direct internet: every
connection goes to that operator-configured SNI relay on port 443, nothing is
resolved, and the public-address check is the relay's job. The allow-list, HTTPS,
no redirects and TLS verified against the real host name still apply. The exact
host list lives in `docs/configuration.md`.

### 8.6 Plaid

Plan 2b adds Plaid as a second provider behind the same interface. Link runs on
two pages, a new connection and a new login for an existing one, whose CSP alone
also allows Plaid's script at its exact URL, frames from `https://cdn.plaid.com`
and requests to the environment's API host. Plaid's script adds a stylesheet of
its own, which Link needs for its layout on a phone: each response mints a nonce
of at least 128 bits, put in `style-src` and on Plaid's script tag only, and never
`'unsafe-inline'` or `'unsafe-eval'`. Every other page keeps the default policy,
which has no nonce. The environments
are `sandbox` and `production` (Plaid retired `development` in 2024).
Transactions use `/transactions/sync` cursors, balances use `/accounts/get`,
disconnect calls `/item/remove`, and 730 days of history are requested. Only
`ITEM_NOT_FOUND` counts as already gone on disconnect: any other refusal keeps
the connection, because the Item may still be billing. A connection that needs
a new login goes through Link's update mode, which keeps the access token and
the cursor; it is never reconnected with new credentials, which would orphan a
billing Item.

A delta is reported once, so the cursor moves on only when every record it
covers was stored or can never be. An account that fails while its records are
in the fetch holds the saved cursor, and the next sync fetches them again; the
apply step is idempotent, so the other accounts lose and repeat nothing. A
currency coffer does not support cannot become storable by fetching again, and
does not hold it; an account reported with no currency at all may get one, and
does. A record that cannot be read is skipped and counted without failing its
account.

Balances are valuations, not ledger amounts, for every provider. One with more
decimals than its currency allows is rounded half-even to minor units, and the
provider account keeps the exact text. One that cannot be stored for any other
reason (too large) is left out and noted on the provider account, while its
records still apply and the cursor can move. Transaction amounts are never
rounded: a record that does not convert exactly is unusable.

A provider chooses the day of the balance it reports, and a balance's date may
honestly be when it last changed, so an old date alone doesn't lose the amount. What
must not happen is a report rewriting or extending history backward. A date within
31 days of the household's today (§8.7) is believed, except that a repeat of the latest
amount on a day holding a balance writes nothing, for a change of zone (§8.7); a later one
is dated today. For
an older date on a bound account:

1. **No snapshot yet, or the latest is before the reported day:** the amount is
   taken, dated 31 days back. This dates it later than reported, on purpose: a date
   in 1970 adds no history and a long-past row can't change a year of carried net
   worth, and the account has a balance, so the forecast isn't blocked for want of
   one. So a dormant account's history starts on that day, and the age shown for its
   balance can be less than its real age. The audit entry gives `reported_on`.
2. **The amount of the latest snapshot, or the day and amount of any snapshot:**
   nothing is written and nothing said. The first is needed as well as the second,
   because an account taken under rule 1 goes on reporting its real, earlier date.
3. **Anything else** would add or change history at or before the latest snapshot.
   It is refused, and the provider account shows a fixed note, `old_balance_note`.

A bind judges the balance last reported the same way, never over a snapshot already
there. An unbound account's balance is kept only if its date is believed, as #77 set.
A refused report's records still apply and the cursor can move. The note is not
`error`, because the bank reported no error: it raises no forecast caution, and the
age of the balance kept stays the only signal (`balance_stale`). A report not refused
clears it, and so does a balance set by hand on or after the latest day. A balance a
member or a token sets by hand is no report, and none of this limits it.

### 8.7 Household timezone

The household has one time zone, an IANA name. An admin chooses it in the household
settings (the page and `PATCH /api/v1/household/settings`, audited); until then, and
after they clear it, `COFFER_TIMEZONE` (default `UTC`) applies. Anything else is
refused, by the copy of the time zone database built into the binary, so what counts
as a name doesn't depend on the machine.

Provider timestamps become calendar days in this zone, and every "today" and current
month is the household's day in it: the forecast, the plan report, net worth, sync
windows, settlements, the daily jobs, page times and `coffer rebuild-net-worth`. Each
reads the zone at the time it asks, from the database, and nothing keeps it: a change
applies to the next request and the next job with no restart.

**A change moves nothing already stored.** A transaction's day, a balance snapshot and
a kept balance were right when they were recorded, and rewriting them could move money
between months. Only new days use the new zone. So that a provider's next report can't
undo this, a source record remembers the chosen zone each of its two days was worked out
in (`dated_in`, `transacted_in`; none means `COFFER_TIMEZONE`, as for every record from
before this setting). A record dated by an instant, reported again, keeps each day unless
its instant falls on another day in that same zone, which means the provider moved it;
only then does that day take the zone now in force. A pending record missing from a sync's
window isn't taken as gone if another zone dated it and it is within a day of the window's
start, since it may be from before the window.

A provider's report of the same moment can land on an earlier day once the zone moved back.
If that day already holds a balance, and the report's amount is that of the latest
provider-written balance, at most 2 days later, it writes nothing, since it would replace
what the day really ended on (the most a change of zone can move a day is 2). An empty day
is written. The rule can drop a provider's real correction of a day within those 2 days to
the latest amount. Past today the opposite applies: a report replaces a provider-written
balance dated up to 2 days after it and after today, so the old amount doesn't come back as
the household's latest when that day arrives. A member's own balance is never replaced.

A sync's window comes from the zone at the start of its run. Everything an account's
transaction then writes uses the zone that transaction sees, and no balance is dated after
that today. A zone changed during a run can therefore date two accounts differently, each
one consistently.

**The daily table.** A change that leaves today on or after the table's last day needs
nothing, as the table is right as far as it goes, and the job adds any new day within a
minute, as at midnight. A change that puts today before the last day would leave rows
for days still to come, so in its own transaction those rows are removed and the table
ends today. That is no rebuild: a row depends only on balances and rates of its own day
and before, so the rows kept are right, and a report goes on answering with no
`rebuilding` and no null `through`. The days removed return one at a time as they
arrive, from the same snapshots: no gap, no day twice. A rebuild under way (§8.10 N3)
goes on with the accounts it has left, up to the new last day. Rebuilds and fills work
out the day inside each transaction, in the zone that transaction sees, so a runner that
started before a change can't write a day from the zone before. A `COFFER_TIMEZONE`
changed at a restart is not a setting and has no such step;
`coffer rebuild-net-worth` resets a table left ahead, and reports never answer past
today in any case.

### 8.8 Deferred

Rules (a §2 goal), transfer links, a starter category set and unbinding a
provider account move to a later plan (FX: §8.10), as does MCP (§6.12). Household settings
arrive in plan 3, the time zone later (§8.7).

### 8.9 Plan and forecast

Decisions made while planning the planned items, sinking funds and cash
forecast of §6.7 to §6.9 (plan 3). Where they differ from an earlier section,
they govern.

Amounts stay exact: integer minor units in `i64`, intermediates in `i128`, never
a float. Rounding is half-even, once, at the end of a calculation. Every "today"
is the household's day (§8.7). A planned item's amount is a positive magnitude
with a direction; `Money` on accounts and splits keeps its sign.

**R1. Several currencies report; they are not refused.** FX is not built. A
forecast with several currencies answers 200 with `surplus: null`, status
`unavailable`, blocker `mixed_currencies`, and full per-currency totals, each
with its own surplus. Currencies are never netted. FX exists now (§8.10), and
the blocker stays until a forecast across currencies has rules of its own (N1).

**R2. Forecast access.** A forecast, a what-if or a plan-versus-actual report
sums every account's balances and category spending, so it needs an `all` grant
at `transactions` (§8.2); without it the answer is 403 with "this action needs
transactions access to every account". There is no partial forecast: a number
over the visible accounts would be confidently wrong, and marking it partial
would reveal that hidden accounts exist. The refusal is the same whether or not
any account is hidden. The safety floor, a figure of that same plan, takes the same
access to read: below it the settings answer, on a read or a patch, says `floor:
null`, the same as none set. The other settings read the same for every token.

**R3. Planned item access.** A planned item hinting at an account the actor
cannot see at `transactions` answers like a missing item: left out of lists, 404
on `GET`, not 403, which would confirm it exists. A `balances` grant shows only
the account and its balances; an item's name, amount, cadence and notes reveal
much more. Unhinted items are household data, which need `transactions` on at
least one account, named or through an `all` grant, for the same reason. A token
holding only `balances` sees no planned items at all: an empty list, not a 403.
Uniform access always qualifies. Creating,
changing, deleting or settling an item needs `WriteHousehold` and an `all` grant at
`write`, like a category. An unhinted item's name can mention an account: that
is an accepted risk, and hinting the item hides it.

**R4. Status and severity.** A forecast's status is `firm`, `provisional` or
`unavailable`. `blockers` mean no number at all: `mixed_currencies`,
`floor_not_set`, `no_counted_accounts`, `balance_missing`. Everything else is a
`caution`, which keeps the number but makes it not firm, including stale
balances and unbound provider accounts. A counted account without any balance
could hold anything; an unbound account never blocks, since a member can leave it
unbound on purpose by dismissing it (§8.11 B12a), and then it raises no caution either.

**R5. The floor must be set.** An unset floor blocks the number, because a silent
floor of 0 overstates what can be invested. An explicit fixed floor of 0 is
allowed. A floor of some months of outflows that comes to 0 keeps the number, with
a caution.

**R6. Plan versus actual** is defined by category (§6.9): actuals are categorized
splits, and `transfer` is a category kind. A transfer link takes a movement between counted
accounts out of them whatever it is filed as (§8.12 T5).
Uncategorized money is never spending and is reported separately, without
invested and illiquid accounts.

A report says what happened, so money stays visible. A charge in an `expense`
category or a deposit in an `income` one, on a transaction its provider removed or
revised away from its splits, stays out of `actual`, and the row's `left_out` says
how much. Money that lowers `actual`, such as a refund, keeps counting, as in R10a. A
possible duplicate counts, and the row's `possible_duplicate` says how much of
`actual` it is. A split linked to a goal counts only on the goal's line, whose
`left_out` takes what R10a keeps from a goal, and an inactive goal with linked money
keeps a line, since that money counts nowhere else. Actuals cover every account: R10's
balance dates decide what a forecast may trust, not what happened.

Unlike the forecast, which carries no leftovers over, the report shows a rollover per
category and currency with a budget (R9): what was budgeted over the reported months up
to today's, less what they spent on it. Spending pays each month's bills and one-offs
dated up to and including today first, those settled (R9a) excepted, floored at 0 unless the month is a net refund, which adds to
what is left. Items share a category's pool, so it isn't per item. A bill has none.
A month's spending counts only while a budget in that category and currency is running,
from the month of its first occurrence through the month of its end date, so a
bi-monthly budget's off-month counts and what came before or after doesn't.

The rollover is a window over the reported months, not a running balance, so widening or
moving `from` changes it, and today's month counts all its budget occurrences. A known
limit: a refund in a month where a bill absorbs the spending is lost to the rollover,
because of the floor. `left_out` holds the stale splits, not the transaction's revised
amount. A category's kind can't change while a goal-linked split uses it, so a linked
split can't vanish from every report.

A sum too large for the database's integers answers 422, as any other total too large to
represent does, on the forecast and the report alike. A number too big for the query's
integer type answers 400, as on the forecast.

**R7. The time zone stays an environment variable** (§8.7). Everything reads one
zone, and changing it at runtime would raise what happens to days already
assigned, which is a separate question. A later change settled that question and moved
it into the household settings, with the variable as the default: §8.7 governs.

**R8. Goal links on splits.** `SplitInput` gains an optional `goal`, whose
currency must equal the transaction's, so goal arithmetic stays exact without
FX. A purchase in another currency cannot be linked until FX exists, which
overstates that goal's need: the safe direction. Only a split in an `expense`
category may be linked, and only those lower the need: moving money to savings for
a goal is not spending it, since the money is still cash. Every split names a
category, so uncategorized money has no split to link. A split linked to a goal the
actor can't see (R3) reads `goal: null`, and naming that goal is refused like a
missing one even on that transaction; a patch sending a split of the same amount
with no goal keeps the link, since a token can't be asked to send what it isn't
shown.

**R9. Bills and budgets.** A recurring outflow has a `spending` of `bill` or
`budget`, which replaces §6.8's reduction of every recurring item. Only a budget is
reduced by this month's spending, and it counts its whole calendar month, as a household
reads a budget: its occurrences anywhere in `as_of`'s month, those before `as_of` too, are
on its line and in the first month. Later months count only inside the window, and an
occurrence before the budget's start or after its end date does not count. Each category
and currency has one pool: the month's actual spending, minus the transactions in it that
paid a recurring bill or a one-off (R9b), floored at 0. It reduces the month's budget
occurrences in date order, and then in the deterministic order items load in, since applied
per item, one purchase would reduce every item in the category. So this month's budget
amount, less the category's spending that paid no bill, floored at 0, is still needed. A paid
bill therefore never eats a budget. A recurring bill or a one-off, a specific expected
payment, counts in full until its date passes, so one dated `as_of` counts in full even if
paid that day, when a balance of that date already shows it: the forecast holds it twice for
that day, the safe direction. Past its date it is still needed until a transaction pays it,
with a caution naming the category and the amount. If it was paid under another category the
forecast shows less to invest, the safe direction. A budget can simply underspend, so what it
still needs is never a gap and raises no caution. Spending in a category before a budget's
first occurrence this month still reduces it, since a budget is a month's envelope for its
category. Goals and hypothetical items never owe, and hypothetical items neither reduce nor
consume, nor look back: an imagined budget counts only inside the window. What a person
settled for a month is out of all of this (R9a).

**R9b. What paid a bill (issue #76).** A surplus that reads high can send away money still
needed, so a dated bill is paid by a transaction, never by its category's total. The forecast
looks at occurrences of recurring bills and one-offs from the first of the month before
`as_of`'s (`BILL_LOOK_BACK_MONTHS`, 1) up to and including `as_of`, and at each transaction
R10 counts as spending in their category and currency from 7 days before that first day (`BILL_EARLY_PAYMENT_DAYS`),
since a bill on the 1st may be debited on the 30th. Transactions go oldest first, and each pays
the oldest unpaid occurrence it fits, then the closest in amount, then the first in the order items
load in. It pays one at most. An October bill paid in November therefore pays October's, and one
payment for two months pays the older.

A transaction fits an occurrence when it is in the same category and currency, within 10% of
its amount either way (`BILL_AMOUNT_TOLERANCE_PERCENT`), so a utility that varies still pays,
and posted no more than 7 days before its date, for a debit moved around a weekend. A transaction
posted before the first day looked at fits only if it is nearer that occurrence than the item's one
before: otherwise it was that one's, and a weekly bill's row exactly 7 days before would shift
every payment along by one and hide a missed week. A smaller or larger transaction pays none of it,
however close the category's total: part-payments are for a person to settle. An occurrence nothing
paid counts in full. An occurrence that is paid counts as paid whatever the difference within the
tolerance: 1,800 pays a bill of 2,000, and `paid_by.difference` shows the -200. It raises no
caution, since a bill that varies would then never be firm.

**What coffer has no evidence for.** An occurrence dated before the first posted transaction on a
counted account, or with none yet, can't be called paid or unpaid. So a household's first month, or
a bill anchored before its history, doesn't lose a month of bills from its surplus. It takes no
payment, isn't in `unpaid` and is in no total; its `payments` entry has `before_history`, and one
`bill_history_missing` per currency gives the first day, how many and how much. One known limit: it
reads all counted accounts together, so an account linked later has its earlier bills read as
unpaid. Then `last_month_loose_ends` and settling are the ways out.

A payment comes out of the budget's pool, whole, and what paid no bill stays in it, so money
spent on a budget can't hide a bill. A transaction that fits but might be the budget's is said
rather than guessed: when its category's budgets plan as much or more for that calendar month, and
it isn't the exact amount, it pays nothing and raises `bill_payment_unclear` for the occurrence, with
the transaction and its amount. No more than the bill's amount of it stays in the pool, so in
`as_of`'s month the need is never below either truth: a budget of 500, a bill of 450 and a charge
of 480 hold 500. The exact amount, or more than the budgets plan, is the bill. A possible
duplicate pays nothing, since it may be the same charge again (R10a), and stays in the pool with
its caution. A refund within the tolerance of an earlier charge in its category counts against the
newest one, which then pays nothing; any other refund only lowers the pool, so hardware coming back
doesn't unpay the rent. A known limit, the other way: three quarters of a rent refunded leaves it
paid.

What nothing paid is in `unpaid` and counts in the first month. This month's raises
`bill_not_seen`, summed by category and naming its `items`, and last month's `bill_unpaid_last_month`,
naming the item, the date and the amount. If last month has an uncategorized transaction or a
possible duplicate, which pay nothing, and a bill from last month is unpaid in that currency,
`last_month_loose_ends` says so, since one may be the payment waiting to be filed. `payments` on the
item's line lists each occurrence looked at, by date, with `paid_by`: the transaction, the day it
posted, its amount and the difference. It is null for one nothing paid. A paid item with nothing
else to count still has a line, with `counted` 0, for that list, and adds nothing to the number: not
even its currency to `totals`. A wrong match is corrected without new machinery: settling the month
(R9a) takes the occurrence out, so its transaction goes on to the next, and filing the transaction
elsewhere takes the transaction out. The page shows the cautions and buttons to settle (R14);
`payments` is in the API.

Known limits. A bill's past occurrences are read at the item's amount and category today, so
changing either can leave last month's unpaid, the safe direction. A coincidence at the exact amount
beside a budget is taken as the bill, which in `as_of`'s month changes no total while the budget has
room, and which `payments` shows. Plan versus actual's rollovers still give bills their share of a
month by amount (above): they say what a month came to, and pay no one occurrence.

**R9a. Settling a month.** A bill can be paid with no transaction to categorize, such as a
transfer or from an account coffer doesn't track, and the safe default of R9 would then
count it until the month ends. A settlement records that one item's occurrences in one
calendar month are `paid` or `skipped`, with who and when, in a table keyed by item and
month. It never changes the item and is deleted with it. Only a recurring bill or a one-off
going out may be settled: a budget is reduced by spending, a goal has links, and income has
no bill to mark paid. An item with settlements can't become a budget or income until they
are deleted, so a settlement never says what its item can't have said.

A settlement doesn't outlive the facts it was made on. A `PATCH` that changes an item's amount,
currency or schedule removes its settlements from the household's current month on, in the same
transaction and audit entry, and its answer shows what is left: nobody said 5,000.00 was paid
when they settled a 100.00 bill, or four weekly payments when they settled one monthly one, and
a one-off moved away and back doesn't bring one back. It errs toward counting, and the member
can settle again. Earlier months stay as history. A change to anything else, such as a name or
category, keeps them, and a patch that changes nothing writes nothing.

`PUT` and `DELETE /api/v1/plan-items/{id}/settlements/{month}` follow R3's rules for a plan
write: `all` at `write`, decided before any lookup, so hidden and missing answer alike;
audited with member and agent; reads show settlements on the item. Both are idempotent: the
same `PUT` again writes nothing and keeps its who and when, another `kind` replaces it, and
a `DELETE` of nothing answers the same. A `PUT`'s month must be in the plan's years, at most
12 calendar months from today's either way, and hold an occurrence of the item. An item holds
at most 60; a `DELETE` may clear any month of the plan's years, so one that has aged out can
make room.

In a forecast a settled item's occurrences in that month count as nothing, in the current
month, last month or a later one in the window: no gross, no gap, no `bill_not_seen` or
`bill_unpaid_last_month`, no payment of its own, so a transaction goes on to the next bill or a
budget, and a settled one-off raises no `goal_one_off_overlap`. One narrowing of that, toward
holding more: a month settled as `paid` says the money was spent, so it is no budget's. For an
occurrence looked at or later in `as_of`'s month, a transaction that fits it and paid nothing else
leaves the pool, up to the bill's amount; with none that fits, such as a charge of 230 on a bill of
200 or a rent paid in two parts, the bill's amount comes off the pool whatever it holds. That caps it
at the bill so that settling can't raise the need, so of that 230 the 30 beyond the bill still
reduces a budget. `skipped` takes nothing. A months floor keeps them, since it says how much to hold
rather than what is owed, so settling can't lower or zero it. `settled` lists each item and
month from last month to the window's end with its amount, so no number silently differs, and settling raises
no caution, so it doesn't make a forecast `provisional` of itself: a person decided. A
what-if reads settlements as they stand and can't add one, and a hypothetical item has none.

Plan versus actual keeps a settled occurrence in `planned`, since it is what was planned; a
row's `settled` says how much of `planned` is, and the report's `settled` which item, month
and kind, so the difference is no surprise. A settled occurrence takes no share of a rollover's
spending, so the rollover and the forecast agree: a settlement says that bill was paid some other
way, so spending in its category wasn't for it. The forecast page says what is settled in plain
words and can settle what a caution asks about (R14); deleting a settlement is API only for now.

**R10. Actuals** count only posted, non-goal-linked `expense` splits on counted
accounts, on or before that account's balance date, since spending newer than the
balance is not in it yet and would count twice in the household's favour. Closed
accounts are left out too: a closed card's debt counts against cash (R16), but its
spending does not lower the need, which errs safe. Goal-linked spending on a counted
account has the same balance-date limit, as does one on a closed card switched into
the forecast, with or without a balance; from an excluded account it does not, since
it never touched counted cash. Pending charges do not lower the need, and money spent from an excluded
account did not come out of counted cash. A known limit: a balance has a date and no
time, so a transaction later on the balance's own date counts as shown.

Money out that a balance may not show raises the caution
`spending_newer_than_balance`, per currency, which makes the status `provisional`:
`total` is posted outflows after the account's balance date, up to `as_of`, and
`pending` is pending outflows dated up to `as_of`, whatever the balance date. A
transaction is one or the other, and pending alone raises it. It covers open accounts
with a balance, an open one without being blocked already, and any closed short-term
liability switched in, with or without a balance, since closing a card settles no debt
its balance does not show. The number does not change, since a provider's balance date
may lag what its balance reflects and guessing could count the money twice or not at
all: the forecast assumes each balance already reflects this money. Where one does not,
as with pending charges on many depository balances, the cash available is overstated
by up to these amounts. A bill past its date is still unpaid until a transaction on or before
the balance date pays it (R9b).

**R10a. Charges that say nothing about what was spent.** An outflow on a transaction
flagged `source_removed` or `split_mismatch` counts neither as a category's spending nor
as a goal's, and one flagged `possible_duplicate` does not count as a goal's. The provider
took the charge back or changed it, so its splits no longer say what happened, and leaving
it out errs toward holding more: a bill it paid is unpaid again, with a caution. An inflow
on such a transaction keeps counting, split by split, because leaving a refund out would
raise net spending, the opposite direction: a reversal the provider revised still reverses
the charge, and a refund the provider removed still counts, since whether it was real is
unknown and counting it holds more. A possible duplicate in a category keeps counting
there, with the caution `possible_duplicates`, but pays no bill (R9b).

**R11. No base or display currency yet.** §6.8's "in the requester's display
currency" waits for FX. A fixed floor carries its own currency.

**R12. What-if** is `POST /api/v1/forecast/what-if`, and a `read` token may call
it. This qualifies §6.3's "`read` (GET only)": scope is decided by the action, and
this one persists nothing.

**R13. Overrides.** `GET /forecast` takes `as_of`, `horizon_months` and
`include_income`. A per-call floor override goes only through what-if, which with
no items also takes `floor` and `stale_after_days`, so a tagged floor is never
flattened into query parameters.

**R14. Settings are API-only; a small forecast page is added.**
`GET /forecast` is session-only and renders the same forecast as the API: cash
available, cash needed, the surplus or the reason there is none, a monthly
breakdown with expected income, and cautions. Its only write action settles a bill a caution asks
about (`bill_not_seen`, `bill_unpaid_last_month`, `bill_payment_unclear`), through R9a's service, for a role that may
write, with the session's CSRF token. It has the same authorization to read as R2, the default
content security policy, no inline script or style, and a link in the navigation.

**R15. Income is off by default.** With it on, a caution fires when projected
cash at a month's end, before an inflow arrives, is below the floor: a later bonus
cannot pay an earlier bill.

**R16. A planned outflow hinted at an uncounted account still counts,** with a
caution. Overstating the need is the safe direction; dropping it silently is not. A
planned inflow hinted at one stays out of the number, with the same caution (`effect`
`left_out`; an outflow's is `still_needed`): the money may never reach counted cash, and a
forecast must not overstate the surplus. A surplus that reads low for it says so. An
inflow with no hint, or hinted at a counted account, counts.
Likewise a closed short-term liability still owing counts against cash,
with a caution, though on its own it is no counted account.

**R17. Item model.** `category` is required on stored items and optional on
hypothetical ones. An outflow needs an `expense` category, an inflow an `income`
one, and `transfer` is refused. A goal must be an outflow. `spending` is set only on a
recurring outflow. An item's type never
changes. Delete is real, and refused with 409 for a goal with linked splits, which
is marked complete instead.

**R18. Schedules clamp to the month's end and never drift.** The k-th occurrence
is `anchor + k × every`, always counted from the anchor. Anchoring on the 31st is
how to say "last day of the month", so §6.7's optional day-of-month semantics get
no field of their own. A yearly item anchored on February 29 falls on February 28
in other years. Occurrences are never after an end date, which is inclusive.

**R19. `as_of`** may be any date in range, with a caution unless it is today, since
what falls due before the month before `as_of`'s is left out; from there a passed bill still
owes what nothing paid (R9b), and a budget counts its whole month in `as_of`'s (R9), while a hypothetical item dated
before `as_of` is left out. Planned inflows, stored or hypothetical, count only from the
day after `as_of`, also in what `excluded_inflows` reports: a balance dated `as_of` may
already hold that day's money, so counting it could count it twice, and if it hasn't
arrived the surplus reads low, the safe direction. The window's end stays where it is, so
a monthly salary paid on `as_of`'s own day counts once less. A balance is the latest on or
before `as_of`, and its age runs to the earlier of `as_of` and today, so a future `as_of`
does not mark a balance dated today as stale. When `as_of` plus the horizon clamps to a shorter month, the
clamped day stays in the window for outflows, so a bill on it is not lost; one
anchored on a month's end may count once more, the safe direction. Inflows count only
up to the unclamped end, so the clamp's extra day never adds income.

**R20. Limits.** Horizon 1 to 60 months, default 3. 500 stored items, 50
hypothetical items. Amounts 1 to 10^15 minor units, a fixed floor from 0. `every`
1 to 999. Schedule dates and `as_of` in the years 1970 to 2200, written exactly
`YYYY-MM-DD`, since a datetime's day depends on its zone. A goal's due date
after its start date. Stale after 1 to 365 days, default 7. Floor months 1 to 60.
"M months of outflows" sums unreduced recurring outflow occurrences in `[as_of,
as_of + M months)`, budgets' earlier days this month not among them, per currency, ending on the clamped day plus one when that
clamps to a shorter month. Totals past `i64` answer 422, never a wrapped
number.

**R21. The plan has no history.** Plan versus actual applies today's active items
to past months, so a plan edit rewrites past months' "planned". "Normalized into
months" means occurrences bucketed where they fall, not amortized.

**R22. Not in this plan:** MCP (§6.12), rules, transfer links, a starter category
set, FX and unbinding a provider account (§8.5, §8.8).

### 8.10 Net worth and FX

Decisions made while planning net worth history and exchange rates (§6.9, §6.10,
plan 4). Where they differ from an earlier section, they govern.

**N1. The forecast's `mixed_currencies` blocker stays.** What a household can invest
is held per currency: euros don't pay a dollar bill without a conversion at a rate not
yet known, a fixed floor is in one currency, and goals stay in their own. A converted
surplus would be one confident number over all of that, the failure §1 fears most, while
each currency's own surplus is true today. A forecast across currencies needs rules of
its own, which is a separate issue. R1's "when FX arrives" no longer applies.

**N2. A token's net worth is the sum of what it can see.** Unlike a forecast (R2), net
worth has a partial answer that is true: the sum of the accounts visible at `balances`.
Nothing marks it partial, since that would reveal hidden accounts. Everything in the
answer comes from visible accounts and household settings alone, so a hidden account's
currency, rate state, age and existence don't show, and a token seeing no counted account
reads exactly like an empty household. Which rates are stored depends on the currencies in
use, so the FX endpoints span every account and need `all` at `balances`.

One thing crosses that line, and it is accepted. A rate is reference data the household
stores once, so a visible currency a hidden account already caused to be fetched has rates
sooner than it would have alone. A row needing a rate means that currency and the display
currency make more than one, so N7 fetches them with or without the hidden account: only
the order differs, and the same rates give the same bytes whoever caused them. Before the first fetch for
that currency, or for as long as an outage lasts or the provider stays off, a token can
compare `not_fetched_yet`, `fetch_failing` or `provider_off` with an amount at an old
`rate_on`. That is one bit about a currency it already sees, and a date: some use of the
currency predates this, and the last fetch was about then. The use could be an account now
closed, a past display currency or a visible account. It shows no amount, count, name or
id, and no other currency. An outage lengthens how long the bit can be read without adding to it,
and hiding a true rate from one token would make its number wrong, so the answer stays. No
token can make this happen: creating an account needs `all` at `write`, and changing the
display currency needs `all` at `balances` (N8).

**N3. A daily table holds one row per account per day.** It holds the balance used, the
date and source of its snapshot, the amount in the display currency, that currency, the
rate date, `rate_leg` and the state of the conversion. `rate_leg` (`held`, `display` or `both`)
says which currency's rate the rate date, or a missing rate, is about: a cross rate goes through
the euro and reports the older of its two dates, and a euro account has only the display
currency's, so a caution names the currency whose rate is old or missing and never the euro.
`manual_rate_leg` says which side, if any, used a rate an admin entered (N16). It is derived:
`balance_snapshots`, `fx_rates`, `fx_manual_rates`
and the display currency are the source of truth, and the table can be dropped and rebuilt.
Names, `include_in_net_worth`, the liquidity class and closing are applied at read, so
changing them needs no rebuild.

A changed snapshot rebuilds its account from that date up to its next snapshot, and a bind
bringing in an unbound account's kept balances rebuilds it from the earliest of them. A new or
corrected rate rebuilds the accounts it converts, from that date. A changed display
currency rebuilds everything. Each happens in the same transaction as the change, unless a
rate or the display currency would rewrite more than 25,000 rows. Then that transaction only
asks for a full rebuild, and until it has run the read answers `rebuild_needed`. One account's
whole history always fits, so a back-dated balance or a bind stays in its own transaction. The
daily job fills days forward up to today. Where a row can't exist yet the read says so
instead of serving an old number: `through` names the last day filled, later dates aren't
answered and `to_date_not_yet_filled` says so when the range's end or today is later, and `rebuild_needed` (set by the migration creating the table, or by a change too large for its own transaction) answers no number
at all until the rebuild has run. `coffer rebuild-net-worth` rebuilds the whole table, and
a test holds that incremental updates match it.

A full rebuild runs in batches of whole accounts in id order, at most 25,000 rows unless one
account alone holds more, each in its own transaction, with a pause between them, so other
writers get the lock whatever the table's size. One process runs it at a time, decided by an
operating system lock on a file beside the database, so two can't fill each other's pauses and
one that died blocks nobody. The daily fill takes as many days as fit in the same limit at a
time, with the same pause, and the table can answer between them, with `through` saying how far
it has come. `rebuild_needed` stays set throughout, so a
read never sees old and new rows together. `rebuilt_up_to` records the last account done and
the day the rebuild runs up to is held in `filled_through`, so a crash or a failure resumes
where the last batch committed. A write between two batches rewrites its rows as usual if its account's turn
has passed, and otherwise leaves them to the rebuild, so none is lost. A display currency
change, or a rate past the limit, starts the rebuild over, since the rows done so far no
longer hold.

Rows start no earlier than 1970-01-01, so a snapshot dated in the distant past can't make
hundreds of thousands of rows, and never run past `through`, so a future-dated snapshot has
no row until its day.

**N4. Each account is rounded on its own.** §6.1's "rounds once, at the end" applies per
account and day: half-even, in `i128`, from the exact rates. Totals sum the rounded rows, so
a breakdown always adds up to its total and a restricted token's sum is the sum of the rows
it sees. A total can differ from converting a one-currency sum in one go by a few minor units.
Exact rates are text in a canonical form, at most 10 digits with at most 6 of them decimals,
so no conversion can pass `i128`.

**N5. Missing and old rates.** A currency's rate for a day is its most recent rate on or
before that day, never a later one. A cross rate goes through EUR and reports the older of
its two dates. With no such rate the row has no amount. A period holding such a row has
`total: null`, names the account in `unconverted`, and the report carries a caution saying
which currency and why (not fetched yet, not published, fetch failing, provider off, none
that early). Sums per currency are always present, so nothing is omitted. A rate more than 7
days older than its day raises a `rate_stale` caution: a discontinued currency would otherwise
be converted at its last rate for ever (the ECB's rouble ends on 2022-03-01). A conversion
past `i64` is treated the same way, as `too_large`. So is a hole: an included account with no
row on a day after its first has no amount, so that period's `total` and its class are null
and `unconverted` names it, with the caution `daily_value_missing`, since a sum without it
would read as money leaving. Its balance can't be in that day's sum per currency.

**N6. The default provider is the ECB's data API.** `RateProvider` is the interface, and
providers answer in units of a currency per one euro. `COFFER_FX_PROVIDER` is `ecb` (the
default) or `none`. `ecb` asks `https://data-api.ecb.europa.eu/service/data/EXR/D.<CCY>.EUR.SP00.A?format=csvdata&detail=dataonly`,
per currency, with `startPeriod` once it holds rates. That is the only host, through
`egress::Egress`. It needs no key and no new crate: the answer is an unquoted CSV of a fixed
shape, read strictly, and one bad row refuses all of it. A row with an empty value is a day
with no rate, which the ECB's series hold on some holidays up to 2012 and which would otherwise
refuse most currencies' first fetch; it yields nothing and passes every other check. Every line
must end, since a row cut off by the connection closing could read as a false rate. No date
may be before 1990-01-01 or the `startPeriod` asked for, or after tomorrow. The ECB can see which currencies a household asks for, an accepted risk.

**N7. A household with one currency contacts nobody.** A currency is needed when it is an
account's or the display currency, EUR aside, and only when those come to more than one
currency. First use of a currency fetches its whole history. After that the job asks every
6 hours from 7 days before its last rate, so a missed day or a correction arrives by itself.
A failure is logged with fixed text, recorded as a fixed code and retried. A warning comes with
the first failure and each change of status or code, and repeats are at `debug`, judged by the
coverage so a restart isn't a change. It never touches
sync, which has its own worker.

Such an answer replaces the days it speaks for, from the start of the window to its own last
row: a stored rate absent there is removed and the rows from that day rewritten, in one
transaction, so a false rate on a weekend or holiday doesn't last. Rates after its last row
stay, since an answer cut at the end of a line looks like a shorter one, and the next answer
to pass them settles them. An empty answer or a failure removes nothing. An empty answer can't
be true of a window holding a rate that was published, so it is logged, rates are kept and only
the time of the attempt moves. On first use an empty answer reads as `not_published`, which is
what it looks like, at the cost of a 24 hour wait. Each attempt commits before its request, so a
store that can't write sends none, and an attempt dated after now, from a clock that ran ahead,
is due.

**N8. The display currency is a household setting,** set by an admin through
`PATCH /api/v1/household/settings` or `/admin/household`, audited, and replaceable but never
unset. A token with grants also needs `all` at `balances` to name one in a patch, whatever it
names: a visible row converts at once if the code's rates are held, so a token held to one
account could otherwise try each code and read from its own report which ones the household
uses (the report answers an amount where a code nobody uses says `not_fetched_yet`). It starts unset and is never guessed, since a guess from the oldest account would
reveal that account's currency to a restricted token. Until it is set a report has sums per
currency, no totals, and the blocker `display_currency_not_set`. This replaces §6.1's base
currency and each member's display currency.

**N9. A closed account counts up to the day it closed.** Its rows go on being filled, and
closing is applied at read. From its closing day on it counts only if its balance is
negative, since a debt doesn't close with the card (R16), with the caution
`closed_account_owing`. Reopening an account restores those days. Each account in a report
says its `closed_on`, so a drop is explained even without its points. When nothing counts in
a range's later periods, the last point stops short of the range, and the caution
`nothing_counted_after_last_point` says from when: it makes the report `provisional`, since a
reader would take the last point as the value now.

**N10. Fetched rates and rows aren't audited.** They are public reference data and derived
values, and an entry per row would bury the log. What changes them is: a snapshot's write and the
display currency are audited as before, and so is a rate entered by hand (N16).

**N11. Sign and age.** A report sums balances as stored. A positive balance on a liability
class raises the caution `liability_balance_positive`, and a counted balance older than the
forecast's `stale_after_days` on the latest point raises `balance_stale`. Neither changes the
number, as in the forecast (R4).

**N12. Periods.** `GET /api/v1/reports/net-worth` takes `from`, `to`, `interval` (`day`,
`week` ending on Sunday, or `month`, the default) and `account_points`. A point is the value
on the last day of its period, or on the last day available, when it says `partial`. At most
1,000 points; beyond that 422. Dates are in the years 1970 to 2200.

**N13. The net worth page.** `/net-worth` is the second page holding financial data, on the
forecast page's rules (R14): session only, calls the same service, computes nothing, the
default content security policy, no script or inline style of its own, and no form. Its chart
is server-rendered SVG drawn in integers, beside a table of the same numbers, and it takes a
range through plain links. `/admin/household` gains a form for the display currency and one
for rates entered by hand (N16), for admins, with the anti-forgery token.

**N14. Derived balances are optional and asked for.** `balance_snapshots.source` gains
`derived`, for balances worked back from a reported balance through posted transactions, on
`cash` and `short_term_liability` accounts only, since an investment moves with no
transaction. Nothing derives them unless a member calls for it on one account, a dry run by
default. They never replace a reported balance, a forecast ignores them, and every report
naming one says so. Unbuilt until the owner asks for it (plan 4, Task 10).

**N15. Not in this plan:** a forecast across currencies (N1),
each member's display currency, separate buckets (#38) and unvested RSUs (#44). Buckets
filter accounts at read and vesting adds accounts or balances, so neither changes the table.

**N16. An admin can enter a rate by hand** (issue #79), since the ECB publishes about 30
currencies and a household holding another would otherwise have no total. It is for one currency
and one day, through `PUT /api/v1/fx/manual-rates/{currency}/{on}` or `/admin/household`, and it
is carried forward like any rate (N5).

The rule beside a fetch: **a fetch never overwrites a manual rate on the same day; a manual rate
wins over a fetched rate on the same day, while a fetched rate on a later day takes over as
usual.** Manual rates live in their own table, `fx_manual_rates`, which no fetch writes, so this
holds whatever a fetch does (N7), and `RateHistory::merged` is the one place the two join. What is
asked of a source still depends on fetched rates alone, as does `fx_coverage`. An admin can remove
one: what it covered falls back on that day's fetched rate, an earlier rate or none.

A wrong rate moves net worth, so nothing about one is silent. It is refused unless it is positive,
in `FxRate`'s form (N4), with a day from 1970 to the household's today, as its transaction
sees the zone chosen, in an ISO 4217 code other than the euro or
the display currency. A rate over 1.5 times from the published rate nearest its day, within 30 days,
or within 2% of that rate's inverse when it is at least 5% away from 1, is refused as
`unusual_rate`, showing both numbers, unless `confirm_unusual` is set, since one entered the wrong
way round misstates net worth without looking wrong: USD at 1.087 written 0.92 is only 1.18 times
away. Within 5% of 1 a rate and its inverse are an ordinary move apart, so that isn't caught. With no published rate
nearby there is nothing to compare it with, and the form's wording is the only guard. A later change of display currency to a code holding a manual rate
goes on using it. Each change is audited with the old and new rate, and the same rate again writes
nothing. It is marked `manual` wherever it is reported: `manual` on `GET /api/v1/fx/rates`, a list
of its own on `GET /api/v1/fx/coverage`, and on a report `manual_rate` on a point, a class and an
account's point, and `manual_rates` on a report and a bucket naming the currency and the stretch.
That is for information and not a caution: for a currency no source publishes every report would
otherwise stay `provisional` with nothing to act on, which teaches people to ignore the word. A
fresh manual rate leaves a report `firm`. `rate_stale` applies after 7 days as to any rate, and
then it is `provisional`. The list of them answers at most 1,000 from an `offset` with a `truncated` flag,
and the page links to the rest, so an older one can always be reached and removed.

The daily rows move as for any rate (N3): those it converts, from its day, in the transaction, or
past 25,000 rows a rebuild. A write needs `admin`, and with grants `all` at `balances` for the
reason N8 gives: a row converts at once, so a restricted token could otherwise try a code and read
from its report whether the household uses it. A day before the first rate is still `rate_missing`,
with `none_that_early`, since a rate never applies to an earlier day.

### 8.11 Accounts for other people and purposes

Decisions made while planning issue #38 and #39 (plan 5). Where they differ from an earlier
section, they govern. In particular B10 narrows R6's "actuals cover every account", B1 adds to
§6.1's kinds, B6 replaces what N15 left open, and B8 adds that a held account is one more
uncounted account for R16. Names in examples are generic.

**B1. Three more kinds.** `education` (a 529 and similar), `custodial` (UTMA, UGMA) and
`charitable` (a donor-advised fund). Charitable giving is its own kind, as the owner decided on
#38: money in a donor-advised fund has already been given away, even before it is granted. A
brokerage account merely earmarked for giving is still the household's, and stays `brokerage`.

**B2. Whose money it is, apart from what it is.** Every account has `belongs_to`:

| Value | Means | By default for |
|---|---|---|
| `household` | The household's own money. | every other kind |
| `someone_else` | Held for someone, or for a purpose beyond the household's own. | `education`, `custodial` |
| `given_away` | No longer the household's, whatever has happened to it since. | `charitable` |

A kind can't carry this alone: a brokerage account held for a child and a 529 are different kinds
with the same answer, and a 529 a parent means to spend on themselves is an `education` account
that is the household's. It says how coffer treats the money and is not a legal statement: who
legally owns a 529 or a custodial account, and how a tax form treats it, is outside it. It
defaults from the kind and follows a change of kind until it is chosen, then stays, as the
liquidity class does (§6.1).

**B3. A class for each kind.** All three default to `invested`, overridable like any class. A
class says what the money is, and `belongs_to` whose. So the forecast, which counts `cash` and
`short_term_liability` only, leaves them out twice over, and a custodial savings account a member
sets to `cash` still stays out (B4).

**B4. One gate, and no contradictory state.** An account that isn't `household` counts in neither
net worth nor the forecast, and can't be made to: `include_in_net_worth` and `include_in_forecast`
are false on it, a CHECK on the table holds that, and asking for `true` answers 422. Overriding
means saying whose money it is: a member who counts a 529 as their own sets `belongs_to` to
`household`, which is audited, and the account then counts like any other. So there is never an
account "held for someone, but counted", and every existing reader of the two flags stays right
without knowing about this. A `household` account keeps both flags as before, for money that is
the household's and that it would rather not count.

Moving an account away from `household` turns both flags off. Moving it back turns on any the same
request doesn't name, as a new account has them, and clears its beneficiary. The answer shows the
result and the audit entry each change.

**B5. A beneficiary is a label, not a person.** `beneficiary` is an optional free-text label, 1 to
64 characters with something visible and no control, format or separator characters other than the
plain space, on an account that isn't `household`, and refused on one that is: a child, or a
purpose such as a family fund. It isn't a member, since a child has no login, and it isn't a record
of its own. A record would need endpoints, a delete rule and a household-wide list of names that a
restricted token must not read, to save retyping a name on two or three accounts. Labels compare
exactly after trimming, so two spellings make two buckets, and the forms offer the labels the
actor can already see. A provider's text never fills it. It may be a child's name, so it is never
logged, its `Debug` is redacted, it is shown only with its account, and goes in the audit log as an account's name does.

**B6. A report has buckets.** `GET /api/v1/reports/net-worth` keeps its shape for the household's
money and adds `buckets`: one per `belongs_to` and exact `beneficiary` among the accounts that
aren't `household`, with no label first, in a fixed order. Each is the same calculation as the
household's over its own accounts: its own `status`, `points`, `accounts`, `blockers` and
`cautions`, closing and rounding as in N4 and N9. A bucket's account is listed only in its bucket,
so the top-level `accounts` and every number beside it are what they would be without it.

No total adds a bucket to the household's or one bucket to another: this is money the household
said isn't its own, and a sum would be one more number that looks like net worth. Nothing about a
bucket touches the household's `status`, so a missing rate on a child's account doesn't make net
worth provisional. A household with only buckets still has `no_counted_accounts`, and its buckets
answer. Rows go on accruing for every account, as flags and classes apply at read (N3), so
`belongs_to` needs no rebuild, and changing it changes the account's whole history at once. An
account given away part way through its life has no date for that; if that ever matters it needs
an effective date of its own.

**B7. A token's view of a bucket follows its grants.** A bucket is made from the accounts the actor
sees at `balances`, and sums those, unmarked, as in N2: a token holding one of a child's two
accounts sees a bucket of one, and a hidden account's label, currency and existence show nowhere.
`belongs_to` and `beneficiary` read with the account, at `balances`, and change with `write` on it.

**B8. Forecast and allocation.** A forecast lists such an account with `not_counted:
"not_household"`. A later target allocation and rebalancing (#3) reads `belongs_to = household`
only, whatever the class: that is why nothing here adds a third flag. A planned outflow hinted at
such an account still counts, with R16's caution; hint a contribution at the account paying it. A
planned inflow hinted at one stays out of the number, with the same caution, since it isn't the
household's income, as it does for any uncounted account (R16); with `include_income` off it
raises nothing.

**B9. Money going in is spending.** A contribution from a counted account to one that isn't the
household's is a real outflow, so it belongs in an `expense` category. It then reads as `actual` in
plan versus actual, pays a planned bill as R9 says, and lowers the surplus, and net worth falls as
the bucket rises, which is what happened. A `transfer` category means money moved within the
household, which this isn't: there it is in no `actual` (R6), though the forecast's cash still
falls and an unpaid bill keeps counting, the safe direction. R17 already refuses a `transfer`
category on a planned item. Nothing checks this, so it is a rule for members and agents, in the
docs. A transfer link out of the household reads as an outflow, changing no number (§8.12 T5).

Money coming back, such as a 529 paying a household's tuition bill into checking, is a refund in
the `expense` category it paid for, or `income`, on the household's side.

**B10. Money inside is not the household's spending.** A transaction on such an account syncs,
matches and can be categorized as usual, but its splits stay out of plan versus actual: a grant from
a donor-advised fund categorized as charity would otherwise count a second time, after the
contribution. R6's "money stays visible" holds through the report's `not_the_households`, per month
and currency. A split linked to a goal stays on the goal's line: a member linked it on purpose, and
a tuition goal paid from a 529 does need less from counted cash (R10). It reduces no budget and
pays no bill, since a forecast's spending is on counted accounts (R10), so a bill paid that way is
settled (R9a).

Link the payment or the contribution, never both. A goal linked on both the household's accounts,
counted or not, and such an account may have its need lowered twice for the same money, once as the contribution and once as
the tuition it paid, and the data can't tell that from two separate payments. So the number doesn't
change, and the caution `goal_linked_both_sides` names the goal and makes the forecast `provisional`.
Each link alone raises nothing. A link newer than its balance is a side already, and a charge R10a
leaves out is none. A charge counts as a side even after its refund, so removing a link is what
clears the caution, which errs toward holding more. A finished goal raises nothing. The rule also
covers one held account funding another, which the forecast can't see or warn about.

**B11. Nothing is reclassified.** Existing accounts stay as they are, `household`, whatever their
name says, since a guess here moves net worth. A member changes an `other_asset` that is really a
529 by patching its `kind`, and `belongs_to` and a defaulted class follow. CHECKs can't change in
place, so the migration rebuilds `accounts` and `provider_accounts`, in one transaction and row
for row.

**B12. A suggested kind is a guess a member confirms.** A provider account's name may suggest a
kind, computed at read and never stored. It is shown when binding and answered as `suggested_kind`.
It is never applied by itself: a bind with no `kind` still falls back only to the provider's own
`kind_hint`, and otherwise answers 422. A provider's name may hold personal text, so nothing about a
suggestion is logged or audited. Only an unambiguous match suggests: a whole word, and two kinds
matching at the same strength suggest nothing. A name holding `hsa`, `h s a` or `health savings`
suggests nothing, whatever else it holds: a health savings account isn't cash, and none of the kinds
is one. `thrift savings plan` is retirement, while another name with `thrift` and `savings` is a
thrift's cash account, so it's `savings`. A form says it's a guess to check, and also names the
kind, as its select shows it, when that kind leaves the account out of net worth. A bind form
refused doesn't put the guess back in its row, which may be what the member rejected: only a
provider's own hint is preselected there. Plaid's subtypes `529`, `education savings account`,
`utma` and `ugma` become `kind_hint`s.

**B12a. A member can dismiss an unbound provider account (#75).** Someone who means to keep one
unbound, such as a custodial or charity account, dismisses it and the forecast stops warning about
it. `provider_accounts.dismissed_at` holds when, and a CHECK keeps it NULL on a bound one. A
dismissed account is still an unbound one in every other way: it stays on `/connections` under its
connection, marked as dismissed, with an undo; sync keeps tracking its last report, holds its records
and keeps its balances exactly as for any unbound account, and writes nothing more; if the provider
drops it, it is treated as an unbound one is. It raises no `unbound_provider_account` caution and
isn't counted among the accounts to set up (the navigation notice), and `suggested_kind` is
unchanged. It can still be bound, by either route, and binding clears the dismissal. Dismissing and
restoring (`POST /provider-accounts/{id}/dismiss` and `/restore`, and forms on the page, on the same
service) need `ManageConnections` and write access to every account, as binding does; an actor who
without write access to every account is refused before any lookup, so a real id and a missing one get the same answer. A dismissal stands even if real money later appears on the account: no caution until a member restores it. Both are audited
(`provider_account.dismiss`, `provider_account.restore`), with the agent named as ever. Asking for
a state it is already in, or for a bound account or a removed connection, is a 409, which also
answers a page or form older than the change.

**B13. Not in this plan:** target allocation (#3), holdings (#42), an account
page for manual accounts (#49), and an effective date for `belongs_to`.

### 8.12 Transfer links

Decisions made for issue #72. Where they differ from an earlier section, they govern; T5 and T6
narrow R6, R9 and R10 for linked money.

**T1. A link joins two transactions.** Exactly two, on different accounts, one money out and one
money in, each in one link at most, held by the table and a trigger. Their amounts may differ, as
for a fee, and their currencies too. A link reports the `difference`, what arrived less what left
(`null` across currencies, since a link doesn't read an exchange rate), and hides nothing. It writes nothing on
either transaction: a leg keeps its category, and its other fields, as they were. API only for now
(#49 tracks the pages).

**T2. Never linked by itself.** As with duplicates (§8.4) and suggested kinds (B12), coffer guesses
and a member or agent confirms. `transfer-candidates` suggests the same amount the other way, same
currency, another account the actor can read, within 5 days, in no link and with at most one split,
nearest first. `GET /transactions?unmatched_transfer=true` lists what is filed in a `transfer`
category with no link. Neither writes anything.

**T3. Access.** Creating or removing a link needs `write` on both accounts, and reading one
`transactions` on both. A token with one side hidden gets the answer a missing transaction or link
gets, and a transaction it can read shows `transfer_link: null`. A refusal such a token gets because
of the link (a split, a goal, a delete, a merge) doesn't word it as a link, and one refused for a goal
it isn't shown doesn't word it as a goal, as R3 has it.
Audited as `transfer_link.create` and `transfer_link.remove`, with the agent as usual, and undone by
removing it or making it again. Nothing in the policy changes.

**T4. What a leg holds.** At most one split, so a link is refused on a split transaction and a leg
can't become one. Part of a split may be spending, and a report can't say which part, so a rule of
counting the rest would be a guess. Remove the link to split it. A goal is refused on a link that is one
movement in a report (T5), either way round, since money moved between counted accounts is still cash (R8). Any
other link keeps its goal, as a contribution out of counted cash may lower one's need (B10). R8's
refusal of a goal on a `transfer` category stands.

**T5. What it does to a report.** A link is *one movement* when both accounts are in counted cash,
the definition the forecast's actuals use (R10): open, `include_in_forecast`, and `cash` or
`short_term_liability`. Only then did the money stay in the cash the forecast adds up. It is one
definition in the store, behind every number here. Flags and classes are read at the time as every
count is (B6), so switching one changes what its links mean, history included. Closing an account
doesn't: a report says what happened, so it counts a closed account for a leg posted before
`closed_at`, up to the UTC date of `closed_at` plus one day, and January and the last sweep before closing keep
their meaning. The forecast reads the current state, in which a closed account isn't counted, so
`one_movement` and `one_movement_in_reports` can differ on such a link. A link also
stops being one while a leg is flagged `source_removed` (T7) or a provider's revision leaves two legs
of one sign.

What moved is the smaller leg, in one currency. The difference stays what its leg is filed as, so a
link of -1,250.00 and +12.50 moves 12.50, and 1,237.50 stays spending, whatever the reason for it. Across
currencies a link doesn't read an exchange rate, though coffer holds them, so all of each leg moved. A
known limit: nothing bounds a link across currencies, and it takes all of each leg out on a person's
word.

| Number | Effect of linking |
|---|---|
| `actual`, `rolling_average`, `rollovers` | What moved leaves, in any category. |
| `uncategorized` | What moved of an uncategorized outflow leaves. Any other link stays on the list until it is filed, since B9 makes it real spending, and a `transfer` category clears it as it does any outflow. |
| a goal's `spent_against` | What moved of a charge leaves, into the line's `left_out`; of a refund, it just isn't counted. Only an account changing after the link can put one there (T4). |
| `moved_between_counted_accounts` (new) | Says what left and what arrived per month and currency, from posted legs, so the money is still visible (R6). |

Any other link, to a brokerage, a loan, a closed account, or across or outside the household's money,
records the movement and changes no other number: B9's contribution stays `actual` and pays its bill,
a monthly auto-invest stays paid, B10's money inside stays in `not_the_households`, and one in a
`transfer` category stays out of `actual` as it was, since a link doesn't correct a category.
`one_movement` says which. Pending legs are in none of this, as in R10.

**T6. What it does to the forecast.** For one movement, read at the time:

| Number | Effect of linking |
|---|---|
| A budget's `reduced_by`, a bill's `unpaid` | What moved no longer reduces or pays. The other side is counted cash, so nothing left it, and the need is really still there: without the link the surplus was overstated by that much, and with it the surplus falls by that. |
| `uncategorized_spending` | What moved isn't in it. |
| `possible_duplicates` | Unchanged: a leg may still be a duplicate. |
| Goal need, `goal_linked_both_sides` | What moved doesn't lower the need or count as a side. |
| Cash, net worth | Unchanged: they read balances. |
| `spending_newer_than_balance` | When both legs are newer than their own balances in one currency, what moved leaves, since then counted cash is right as it stands. With the inflow shown or still pending, it stays: a pending deposit may already be in its balance, and then cash is overstated. That can ask when nothing is wrong, the safe direction. |

A link to any other account changes none of these. Its money left counted cash, so
counting it as spent is what makes the number right: a bill paid by an auto-invest stays paid, where
treating it as a movement would owe the bill again on top of the fallen balance.

**T7. A leg over time.** Only a person ends a link. A link is user state (§8.4):

- Pending to posted is one transaction, so nothing changes.
- A provider's record vanishing doesn't delete a leg, which holds a person's say-so. It stays with its
  link, flagged `source_removed`, and the link stops counting: real spending comes back rather than
  disappears. The flagged leg follows R10a, so a withdrawn charge is in `left_out`, and the other leg
  counts as it is filed. Between counted accounts with a leg from the first of last month on (R9b), the caution
  `transfer_leg_removed` names the link and makes the forecast `provisional`. A pending record taken
  back is in no `spending_newer_than_balance`, link or not. A record attaching again clears the flag
  and the link counts again.
- A merge: the target keeps its link. A duplicate holding one is refused with `merge_conflict` unless
  `discard_duplicate_state` is sent, like its notes, and the end of the link is audited; that also
  needs `write` on the other leg's account. To keep a link, merge into the leg. A target flagged
  `source_removed` holds a record again, so a merge clears that flag too.
- An un-merge leaves the link on the transaction that holds it; the new one starts with none.
- A manual leg can't be deleted until its link is removed: 409, and the reference refuses it should
  anything else try.
- A closed account may hold a leg, and links can still be made and removed on it: the last sweep out
  of an account is often what closed it. The forecast reads such a link as no movement and a report, for
a leg posted before the close, as one (T5).
- A provider revising a leg's amount leaves the link, and `difference` shows what it says now.
