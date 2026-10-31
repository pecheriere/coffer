# AGENTS.md

## Purpose

coffer is a self-hosted household finance hub: it holds a family's accounts,
balances, transactions and spending plan, and AI agents query it to answer
questions and help plan. Design: `docs/superpowers/specs/2026-10-30-coffer-design.md`.

## Crate map

- `coffer-domain` (`crates/domain`): pure domain types (ids, identity, policy,
  audit entry types, money, accounts, transactions, clock). No I/O.
- `coffer-store` (`crates/store`): persistence. The only crate that depends on
  `sqlx`; its public API speaks domain types only and never exposes an `sqlx`
  type, so a different database backend changes only this crate.
- `coffer-server` (`crates/server`): `config` and `secrets`; `auth` (passkey
  ceremonies, identity, sessions, agent tokens); `http` (router, request
  extractors, problem bodies, idempotency keys, `api/` JSON v1, `health`,
  `pages/`); `ledger` (accounts and balances, categories,
  transactions and their splits); `cli` and
  the `coffer` binary (`src/main.rs`). It never imports `sqlx`.

## Commands

```sh
cargo run -p coffer-server -- serve                     # run the server (see docs/configuration.md)
cargo run -p coffer-server -- bootstrap-link            # first-admin setup link
cargo run -p coffer-server -- recover-admin <username>  # re-enroll link for a locked-out admin
cargo run -p coffer-server -- backup <dest>             # consistent database copy, mode 0600
cargo run -p coffer-server -- healthcheck               # probe GET /healthz on COFFER_LISTEN
cargo run -p coffer-server -- openapi > docs/api/openapi.json  # regenerate the OpenAPI snapshot

cargo fmt
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features
cargo deny check
```

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
- Every mutation goes through a `StoreTx` together with its audit entry.
- Every handler authorizes through `coffer_domain::policy::authorize`; a
  `Denied` becomes a problem only through `impl From<Denied> for Problem`.
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

## Security review

Required before merge for changes to auth, sessions, tokens, policy, secrets
or outbound HTTP. Run the `security-review` skill or an equivalent review and
record the findings in the PR.

## Definition of done

Formatting, clippy, tests and `cargo deny check` are all green, and the docs
are updated to match the change.
