# AGENTS.md

## Purpose

coffer is a self-hosted household finance hub: it holds a family's accounts,
balances, transactions and spending plan, and AI agents query it to answer
questions and help plan. Design: `docs/superpowers/specs/2026-10-30-coffer-design.md`.

## Crate map

- `coffer-domain` (`crates/domain`): pure domain types (ids, identity, policy,
  audit entry types, money, transactions, matching, sync run
  triggers and statuses, clock; `accounts`, with whose money an account is
  (`belonging.rs`) and the guess at a kind from a provider's name (`suggest.rs`); `plan` (planned items, schedules, goals,
  forecast settings), `forecast` and `plan_report`, the pure calculations
  behind the cash forecast and plan versus actual; `fx`, exchange rates held
  exactly and conversion through the euro; `transfer_rate`, the check of a transfer link's legs in
  two currencies against a rate, which changes no amount; `net_worth`, the daily values, the
  periods and the report). No I/O.
- `coffer-store` (`crates/store`): persistence. The only crate that depends on
  `sqlx`; its public API speaks domain types only and never exposes an `sqlx`
  type, so a different database backend changes only this crate. It holds
  planned items and their settlements, the household settings (with the
  display currency), exchange rates and their coverage, the daily net worth
  table (`net_worth.rs`, **the only file that writes `account_daily_values`**:
  a snapshot, a rate, the display currency or a time zone that puts today before its
  last day changes it in its own transaction,
  or, past 25,000 rows, asks for a rebuild that runs in short transactions,
  and a test fails for a new writer of `balance_snapshots` until it refreshes the table), and the one read
  transaction that gathers a forecast's or a report's data.
- `coffer-server` (`crates/server`): `config` and `secrets`; `household_time`
  (the household's zone and day, read from its settings at each asking); `auth` (passkey
  ceremonies, identity, sessions, agent tokens); `http` (router, request
  extractors, problem bodies, idempotency keys, `api/` JSON v1, `health`,
  `pages/`, whose `plaid.rs` serves the Plaid Link pages, the only ones
  with a content security policy of their own, whose `forecast.rs` is the
  read-only forecast page, and whose `net_worth.rs` is the read-only net worth page
  and its `chart.rs`, an SVG drawn in integers; `browser_reports/`, the
  unauthenticated endpoint that logs what browsers report); `ledger` (accounts and balances, categories, `plan_items`,
  `settlements` (one month of a planned item marked paid or skipped), the
  household forecast `settings`, the `forecast` and its what-if, the
  `plan_report`, `net_worth` (net worth and the FX endpoints: authorizes, gathers
  what the actor sees at `balances`, and hands it to the domain's report),
  `manual_rates` (exchange rates an admin enters: validated, audited and marked),
  transactions and their splits, merging, un-merging and dismissing possible duplicates, `rules` (keeping them, with `run.rs` trying one and running it by hand in short batches; `coffer_domain::rules::decide` alone says what they fill), `transfer_links` (two
  transactions that are one movement of money); `unseen.rs` holds what a token with grants
  doesn't get of a transaction it can read: duplicate candidates and transfer links with a side in
  accounts it can't read and goal links it can't see); `fx` (the `RateProvider` interface,
  the ECB's data API over `egress`, and the two jobs `serve` starts: fetching the
  rates a household needs and filling the daily table forward); `tasks`
  (supervision shared by the sync scheduler and these jobs: a panic is logged and the task restarts);
  `egress` (the one outbound
  HTTPS client: per-provider host allow-lists, public-address resolver,
  `HTTPS_PROXY` or an operator's relay, no redirects, time and size caps); `vault` (provider
  credentials sealed with XChaCha20-Poly1305 under a key derived from the
  server key, bound to their connection id); `sync` (the provider interface
  and the `ProviderRegistry`, the one place providers are registered; `dating`, which
  keeps a record reported again on its day after a change of zone; the
  `SimpleFin` provider and `sync::plaid` (`client.rs`, Plaid's API over
  `egress`; `provider.rs`, the `Provider` implementation; `kinds.rs`, what
  Plaid says an account is); connections, sync requests and their rate limits,
  and provider account binding; the engine that performs sync runs; the
  provider-agnostic apply step matching provider records onto transactions;
  the scheduler, whose worker performs queued runs and whose ticker queues
  scheduled ones, started by `serve`); `canonical` (changes to
  canonical transactions shared by `sync` and `ledger`: imported fields and
  duplicate candidates); `cli` and
  the `coffer` binary (`src/main.rs`). It never imports `sqlx`.

## Commands

```sh
cargo run -p coffer-server -- serve                     # run the server (see docs/configuration.md)
cargo run -p coffer-server -- bootstrap-link            # first-admin setup link
cargo run -p coffer-server -- recover-admin <username>  # re-enroll link for a locked-out admin
cargo run -p coffer-server -- backup <dest>             # consistent database copy, mode 0600
cargo run -p coffer-server -- rebuild-net-worth         # recompute the daily net worth table
cargo run -p coffer-server -- healthcheck               # probe GET /healthz on COFFER_LISTEN
cargo run -p coffer-server -- openapi > docs/api/openapi.json  # regenerate the OpenAPI snapshot

cargo fmt
scripts/gate.sh                                         # the full gate
scripts/staging.sh up                                   # the staging stack (below)
```

While iterating, test only the crate you change with
`cargo nextest run -p <crate> --all-features` (add `-E 'test(<name>)'` to
filter); run `scripts/gate.sh` once before committing. It runs formatting,
clippy, the tests and doctests, the docs, `cargo deny` and the OpenAPI check,
and stops at the first failure. Worktrees share its lock, so gates run one at a
time, and each worktree keeps its own log at `$(git rev-parse --git-dir)/gate.log`.
It needs `cargo-nextest` and `cargo-deny`.

## Parallel work

Each task running at the same time gets its own git worktree and branch:

```sh
git worktree add -b <branch> ../coffer-<task> origin/main
```

Leave `CARGO_TARGET_DIR` unset, so each worktree keeps its own build output.
Never run two agents that write in one worktree: one's half-finished edit
fails the other's build and checks. A shared build directory makes builds wait
for each other and can hand one worktree a binary built from another's source.
A read-only reviewer may share a worktree if it does not build. Remove a
worktree when its branch has merged.

## Staging

`scripts/staging.sh` runs the real image, built from the checkout, with made-up data
and mock providers, one stack per machine (`staging/`, how to use it in
`CONTRIBUTING.md`). Use it to try something in the running product instead of writing
a throwaway server and seed. What holds it together:

- **The seed goes through the public pages and API** (`staging/seed/`), never a
  committed database, so it keeps working as migrations change. Everything in it is
  made up. Its amounts stay fixed whichever day it is seeded on, so a test can compare
  them with fixed answers; `staging/seed/household.mjs` says how, and a change there
  must keep it true.
- **Each server is a copy of a read-only template**, seeded once for the image,
  migrations, seed and day. Nothing clock-related is added to the image: the seed dates
  everything from the template's day.
- **The mocks stand behind the real egress settings:** `COFFER_EGRESS_RELAY` and a CA
  of the stack's own, on a Docker network with no route out. A scenario is named by the
  request, a SimpleFin setup token or an ECB currency, never set as a mode, so tests
  running side by side don't disturb each other.
- **End-to-end tests (`scripts/staging.sh test`, `staging/e2e/`) cover the image:** real
  HTTP, the real configuration and the egress rules, with each server a new copy and every
  API response checked against `docs/api/openapi.json`. Logic and edge cases stay in
  `crates/*/tests/it/`. Packages keep to the rules below: exact versions at least 7 days
  old, a committed lockfile, `npm ci` and no install scripts.
- **The tests' browser is pinned by hash too:** `staging/e2e/browser.lock.json` holds the
  sha256 of the headless Chromium archive for each platform, checked before it is unpacked
  (`support/install-browser.mjs`), never `playwright install`. When Playwright moves, a
  Dependabot bump fails the e2e job until someone runs
  `node support/install-browser.mjs --update` in `staging/e2e`, which refuses a browser younger
  than 7 days, reviews the versions and commits the file.
- **The seeded passkeys' private keys are public** (`staging/fixtures/passkeys.json`).
  `serve` refuses a database where an admin holds that key unless `COFFER_PUBLIC_URL` is
  localhost (`cli/staging_seed.rs`), and registering those keys is refused away from localhost
  (`auth/staging_fixture.rs`). That is an accident guard, not a security boundary: with a
  localhost URL, a script holding the key signs in from anywhere that can reach the server, so
  keep the seed where only this machine can. No staging flag or backdoor belongs in the image.

## CI and supply chain

- Actions are pinned to the full commit SHA of a release at least 7 days old
  (comment with the tag); `persist-credentials: false` on every checkout;
  `--locked` everywhere. Only GitHub-owned actions and the allow-listed
  third-party ones (dtolnay/rust-toolchain, Swatinem/rust-cache,
  EmbarkStudios/cargo-deny-action, zizmorcore/zizmor-action,
  ossf/scorecard-action, docker/setup-buildx-action,
  docker/build-push-action) may be used. Spectral runs from a digest-pinned
  image built from `.github/spectral/Dockerfile`.
- The Dockerfile pins the Rust base image by digest (the version in
  `rust-toolchain.toml`) and its build tools by version, each a release at
  least 7 days old; Dependabot proposes base image updates.
- `crate-age` refuses `Cargo.lock` versions younger than 7 days (README,
  "Dependency age"). Pin an older version with
  `cargo update -p <crate> --precise <version>`; allow-list entries are only
  for security fixes and must name a RUSTSEC or GHSA id.
- Run `GH_TOKEN=$(gh auth token) uvx zizmor .` before pushing; it must report
  no findings.
- Fix, don't exempt: a failing advisory, lint or zizmor finding is fixed, not
  ignored in `deny.toml`, `#[allow]` or a zizmor config.

## Conventions

- Ids are UUIDv7 newtypes from `coffer_domain::ids`.
- Time comes from a `Clock`; never call `Timestamp::now()` outside `SystemClock`.
- A day is the household's day, from `household_time::HouseholdTime`, read when it is
  asked for. Never keep the zone or read `Config::default_timezone` for one: an admin
  can change it while the server runs (spec §8.7). A writer whose change depends on the
  day asks with `today_seen_by`, under the write lock.
- Every mutation goes through a `StoreTx` together with its audit entry. The one
  exception is fetched exchange rates and the daily net worth rows (spec §8.10 N10): public
  reference data and derived values, written in the transaction of whatever changes
  them. What changes them is audited, and a rate an admin enters is (N16).
- Every handler authorizes through `coffer_domain::policy`: `authorize` for
  household actions, `authorize_account` for anything tied to an account,
  `authorize_every_account` for what spans every account (the rule table in
  the `policy` module docs), with the `AccountAccess` that `AccessResolver`
  resolves (never built by hand). A `Denied` becomes a problem only through
  `impl From<Denied> for Problem`. Whether a planned item exists for an actor
  is decided only by `plan_item_visible`, whose hidden answer is the missing one, and
  whether a connection does only by `connection_visible`.
- A rule only fills what nobody has set, decided only by `coffer_domain::rules::decide`, and
  never reviews. What only a rule chose isn't user state (spec §8.13 U5, U7).
- **An account that isn't the household's is out of every count through its flags,
  held by a CHECK, and anything reading by class alone must also read `belongs_to`**
  (spec §8.11 B4, B8). Net worth reports it in buckets, and nothing adds a bucket to
  anything.
- Money is an integer in minor units with its ISO 4217 currency
  (`coffer_domain::money`); negative is money out of the account, except a
  planned item's amount, a positive magnitude with a `direction`. No
  floating point anywhere on a money path, provider JSON included.
- Outbound HTTP goes only through `egress::Egress`, with a host allow-list.
  Providers are registered only in `ProviderRegistry::from_config`. Every
  provider text passes through `sanitize_provider_message` before it is
  stored or shown. Provider credentials exist in clear only in memory, and
  never in the database, a log, an error, the audit log or a response.
- API routes are added only through `ApiRouter::op` (`http/api/router.rs`)
  with a `#[utoipa::path]`; the application router merges nothing else under
  `/api`. Request, query, path and response types derive `ToSchema` or
  `IntoParams`; no `serde_json::Value` in any of them. Extract with
  `TypedJson`, `TypedQuery` and `TypedPath` (`http/api/extract.rs`), whose
  rejections never echo input. Errors are `Problem`s with a `ProblemCode`.
- After any API change, regenerate the snapshot with
  `cargo run -p coffer-server -- openapi > docs/api/openapi.json`; a test
  compares it with the generated document and CI lints it with Spectral
  (`.spectral.yaml`, findings fixed in code, never by disabling a rule).
- Errors use `thiserror`; no `unwrap`/`expect` outside tests.
- A panic's message never reaches a log or a response: the hook
  (`cli/panic_report.rs`) leaves it out and `http/panic_answer.rs` answers a
  fixed 500. A `tokio` `JoinError`'s text holds that message, so log
  `err.is_panic()` instead of the error.
- The release binary keeps its symbols and line tables so a panic report names
  file and line. CI's `docker-build` job reads both images' binaries with
  `scripts/check-backtrace-lines.sh`, which fails when a known function no
  longer resolves to its file and line (`strip` in `[profile.release]`, a
  linker flag the symbolizer cannot read).
- A page may set its own `Content-Security-Policy` only for Plaid Link
  (`http/pages/plaid.rs`): the default plus the exact hosts Link needs and a
  per-response nonce, never an `unsafe-*` source. Changes there are in the
  threat model's accepted risks.
- A page whose scripts handle amounts or names stays out of `PAGES` in
  `http/browser_reports/fields.rs`, so a browser report's free text from it is
  never logged, and first-party scripts never put page data in an error's
  message. `/connections` stays in it with rejections withheld, since its forms hold a
  beneficiary label. The threat model says why.
- Lints are strict (`unsafe_code` forbidden, clippy `all` and `pedantic` at
  deny). No `#[allow]`/`#[expect]` without a `reason = "..."`.

## Themes and assets

- Static files live in `crates/server/assets/`. `build.rs` serves each at a
  content-hashed URL (`/static/<dir>/<name>.<hash>.<ext>`, cached for a year)
  and rewrites `asset:<path>` references in CSS and the web manifest. Rust
  code names an asset by its generated constant (`assets::CSS_BASE_CSS`).
- A theme is one token file, `assets/themes/<name>.css`: `@font-face` rules
  and one `:root` block, colours as `light-dark(<light>, <dark>)`.
  Components (`assets/css/base.css`) read only tokens: no colour literal or
  font name outside a theme file.
- Adding a theme: the token file (plus its fonts and `ofl-<family>.txt` under
  `assets/fonts/`), then one `Theme` entry in `crates/server/src/themes/mod.rs`.
  The theme tests check every token in both schemes and WCAG AA contrast.
- Fonts and icons come from `tools/regenerate_assets.py` (pinned sources and
  tools); never edit them by hand. Fonts whose copyright header declares a
  Reserved Font Name are not subset.
- The web manifest's `theme_color` and `background_color` are Mochi's
  values for every theme: the manifest is static and cached by the device.

## Testing rules

Tests specify behavior; no tests that restate implementation or exist for
coverage. Integration tests drive the real router with soft passkeys.

Each crate's integration tests build as one test binary: `tests/it/main.rs`
declares one module per area. Add a new test file as `tests/it/<area>.rs` plus
a `mod <area>;` line, never directly under `tests/`, where cargo would build it
as another binary.

`.config/nextest.toml` gives every test a time limit. A test that needs longer
gets its own override there, based on its measured time; don't raise the
default.

## Security review

Required before merge for changes to auth, sessions, tokens, policy, secrets,
the vault, outbound HTTP (`egress`), provider code (`sync`), or the content
security policy and other security headers. Run the `security-review` skill or an equivalent review and
record the findings in the PR.

## Definition of done

Formatting, clippy, tests and `cargo deny check` are all green, and the docs
are updated to match the change.
