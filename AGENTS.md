# AGENTS.md

## Purpose

coffer is a self-hosted household finance hub: it holds a family's accounts,
balances, transactions and spending plan, and AI agents query it to answer
questions and help plan. Design: `docs/superpowers/specs/2026-10-30-coffer-design.md`.

## Crate map

- `coffer-domain` (`crates/domain`): pure domain types (ids, identity, policy,
  audit entry types, clock). No I/O.
- `coffer-store` (`crates/store`): persistence. The only crate that depends on
  `sqlx`; its public API speaks domain types only and never exposes an `sqlx`
  type, so a different database backend changes only this crate.
- `coffer-server` (`crates/server`): `config` and `secrets`; `auth` (passkey
  ceremonies, identity, sessions, agent tokens); `http` (router, request
  extractors, problem bodies, `api/` JSON v1, `health`, `pages/`); `cli` and
  the `coffer` binary (`src/main.rs`). It never imports `sqlx`.

## Commands

```sh
cargo run -p coffer-server -- serve                     # run the server (see docs/configuration.md)
cargo run -p coffer-server -- bootstrap-link            # first-admin setup link
cargo run -p coffer-server -- recover-admin <username>  # re-enroll link for a locked-out admin
cargo run -p coffer-server -- backup <dest>             # consistent database copy, mode 0600
cargo run -p coffer-server -- healthcheck               # probe GET /healthz on COFFER_LISTEN

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
  docker/build-push-action) may be used.
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
- Every handler authorizes through `coffer_domain::policy::authorize`.
- Errors use `thiserror`; no `unwrap`/`expect` outside tests.
- Lints are strict (`unsafe_code` forbidden, clippy `all` and `pedantic` at
  deny). No `#[allow]`/`#[expect]` without a `reason = "..."`.

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
