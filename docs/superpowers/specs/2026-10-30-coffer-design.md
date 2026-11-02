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
  read-only forecast page in §6.4.
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
- **Balances:** each sync writes today's BalanceSnapshot for linked accounts.
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

Server-rendered, no SPA, minimal JS only for WebAuthn ceremonies. Pages:
setup (bootstrap), accept invite / re-enroll, log in, log out, my passkeys,
my agent tokens, SimpleFin connections, and for admins: members and invites.
No financial data pages in v1, except one small read-only forecast page (§8.9,
R14). (Connections get a page because pasting a setup token is a human step.)

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
or error reveals a hidden account.

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
2b), and the FX source (a later plan). HTTPS on port 443 only, no redirects, and
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

### 8.7 Household timezone

Provider timestamps become calendar days in `COFFER_TIMEZONE` (IANA name,
default `UTC`). It stays an environment variable, and household settings do not
take it over (§8.9, R7). The forecast's "today" is the household's day in this
zone.

### 8.8 Deferred

Rules (a §2 goal), transfer links, FX, a starter category set and unbinding a
provider account move to a later plan, as does MCP (§6.12). Household settings
arrive in plan 3, without the time zone (§8.7, §8.9).

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
with its own surplus. Currencies are never netted. When FX arrives `surplus`
fills in and the blocker goes.

**R2. Forecast access.** A forecast, a what-if or a plan-versus-actual report
sums every account's balances and category spending, so it needs an `all` grant
at `transactions` (§8.2); without it the answer is 403 with "this action needs
transactions access to every account". There is no partial forecast: a number
over the visible accounts would be confidently wrong, and marking it partial
would reveal that hidden accounts exist. The refusal is the same whether or not
any account is hidden.

**R3. Planned item access.** A planned item hinting at an account the actor
cannot see at `transactions` answers like a missing item: left out of lists, 404
on `GET`, not 403, which would confirm it exists. A `balances` grant shows only
the account and its balances; an item's name, amount, cadence and notes reveal
much more. Unhinted items are household data, which need `transactions` on at
least one account, named or through an `all` grant, for the same reason. A token
holding only `balances` sees no planned items at all: an empty list, not a 403.
Uniform access always qualifies. Creating,
changing or deleting an item needs `WriteHousehold` and an `all` grant at
`write`, like a category. An unhinted item's name can mention an account: that
is an accepted risk, and hinting the item hides it.

**R4. Status and severity.** A forecast's status is `firm`, `provisional` or
`unavailable`. `blockers` mean no number at all: `mixed_currencies`,
`floor_not_set`, `no_counted_accounts`, `balance_missing`. Everything else is a
`caution`, which keeps the number but makes it not firm, including stale
balances and unbound provider accounts. A counted account without any balance
could hold anything; an unbound account cannot be dismissed yet, so blocking on
it could block forever.

**R5. The floor must be set.** An unset floor blocks the number, because a silent
floor of 0 overstates what can be invested. An explicit fixed floor of 0 is
allowed. A floor of some months of outflows that comes to 0 keeps the number, with
a caution.

**R6. Plan versus actual** is defined by category (§6.9) and needs no transfer
links: actuals are categorized splits, and `transfer` is a category kind.
Uncategorized money is never spending and is reported separately, without
invested and illiquid accounts.

**R7. The time zone stays an environment variable** (§8.7). Everything reads one
zone, and changing it at runtime would raise what happens to days already
assigned, which is a separate question.

**R8. Goal links on splits.** `SplitInput` gains an optional `goal`, whose
currency must equal the transaction's, so goal arithmetic stays exact without
FX. A purchase in another currency cannot be linked until FX exists, which
overstates that goal's need: the safe direction.

**R9. Bills and budgets.** A recurring outflow has a `spending` of `bill` or
`budget`, which replaces §6.8's reduction of every recurring item. Only a budget is
reduced by this month's spending. Each category and currency has one pool: the
month's actual spending, minus planned outflows in that category dated earlier this
month, bills first, floored at 0. It reduces this month's remaining budget
occurrences in date order, and then in the deterministic order items load in, since
applied per item, one purchase would reduce every item in the category. A paid bill
therefore never eats a budget. A bill counts in full until its date passes. Past
it, spending in its category pays recurring bills in date order, and any gap is
still needed, with a caution naming the category and the amount. If it was paid
under another category the forecast shows less to invest, the safe direction. A
budget can simply underspend, so its shortfall is never a gap. Hypothetical items
neither reduce, consume nor owe.

**R10. Actuals** count only posted, non-goal-linked `expense` splits on counted
accounts, on or before that account's balance date, since spending newer than the
balance is not in it yet and would count twice in the household's favour. Pending charges do not lower the need, and money spent from an excluded
account did not come out of counted cash.

**R11. No base or display currency yet.** §6.8's "in the requester's display
currency" waits for FX. A fixed floor carries its own currency.

**R12. What-if** is `POST /api/v1/forecast/what-if`, and a `read` token may call
it. This qualifies §6.3's "`read` (GET only)": scope is decided by the action, and
this one persists nothing.

**R13. Overrides.** `GET /forecast` takes `as_of`, `horizon_months` and
`include_income`. A per-call floor override goes only through what-if, which with
no items also takes `floor` and `stale_after_days`, so a tagged floor is never
flattened into query parameters.

**R14. Settings are API-only; a small read-only forecast page is added.**
`GET /forecast` is session-only and renders the same forecast as the API: cash
available, cash needed, the surplus or the reason there is none, a monthly
breakdown with expected income, and cautions. It has no write action, the same
authorization as R2, the default content security policy, no inline script or
style, and a link in the navigation.

**R15. Income is off by default.** With it on, a caution fires when projected
cash at a month's end, before an inflow arrives, is below the floor: a later bonus
cannot pay an earlier bill.

**R16. A planned item hinted at an uncounted account still counts,** with a
caution. Overstating the need is the safe direction; dropping it silently is not.
For the same reason a closed short-term liability still owing counts against cash,
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
bills between today and `as_of` fall outside the window. A balance is the latest
on or before `as_of`. When `as_of` plus the horizon clamps to a shorter month, the
clamped day stays in the window, so a bill on it is not lost; one anchored on a
month's end may count once more, the safe direction.

**R20. Limits.** Horizon 1 to 60 months, default 3. 500 stored items, 50
hypothetical items. Amounts 1 to 10^15 minor units, a fixed floor from 0. `every`
1 to 999. Schedule dates and `as_of` in the years 1970 to 2200, written exactly
`YYYY-MM-DD`, since a datetime's day depends on its zone. A goal's due date
after its start date. Stale after 1 to 365 days, default 7. Floor months 1 to 60.
"M months of outflows" sums unreduced recurring outflow occurrences in `[as_of,
as_of + M months)`, per currency. Totals past `i64` answer 422, never a wrapped
number.

**R21. The plan has no history.** Plan versus actual applies today's active items
to past months, so a plan edit rewrites past months' "planned". "Normalized into
months" means occurrences bucketed where they fall, not amortized.

**R22. Not in this plan:** MCP (§6.12), rules, transfer links, a starter category
set, FX and unbinding a provider account (§8.5, §8.8).
