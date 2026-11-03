# Contributing

Humans and agents follow the same rules; see `AGENTS.md` for conventions.

## Setup

Install [rustup](https://rustup.rs); the pinned toolchain in
`rust-toolchain.toml` is fetched automatically. Install `cargo-deny` and
[cargo-nextest](https://nexte.st) with
`cargo install cargo-deny cargo-nextest --locked`.

## Checks

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo nextest run --workspace --all-features --locked
cargo test --doc --workspace --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
cargo deny check
```

All six must pass (run `cargo fmt` to fix formatting); CI runs them on
every pull request.

## Fast local loop

While iterating, run only the crate you are changing with nextest, which runs
each test in its own process:

```sh
cargo nextest run -p coffer-server --all-features            # one crate
cargo nextest run -p coffer-store --all-features -E 'test(idempotency)'  # a filter
```

Before committing, run the checks above unchanged (`scripts/gate.sh` runs them
all). nextest is the test runner in the gate and in CI, so a test passes or
fails the same in both. It does not run doctests, which run separately with
`cargo test --doc`.

A compiler cache such as [sccache](https://github.com/mozilla/sccache)
(`RUSTC_WRAPPER=sccache`) speeds up rebuilds after branch switches. It is
optional and local only; CI does not use it.

For work in parallel, use one git worktree per task; see "Parallel work" in
`AGENTS.md`.

## Staging

A staging stack on your Mac, or any machine with Docker and `openssl`: the real image,
built from your checkout, with a made-up household, on `http://localhost:18080`.

```sh
scripts/staging.sh up      # build, seed if need be, and serve
scripts/staging.sh token   # the seeded admin's agent token, for the API
scripts/staging.sh link    # a link to enroll a passkey of your own as the admin
scripts/staging.sh reset   # back to the seeded state
scripts/staging.sh down    # remove all of it
```

It needs no credentials and reaches nothing but its two mock providers, which stand
in for the SimpleFin bridge and the ECB on a Docker network with no route out. Ports
are bound to `127.0.0.1`. One stack runs at a time on a machine, so `up` refuses while
another checkout's is running.

The household has three years of history in four currencies: a current account, savers,
a brokerage, a home, a mortgage, a car loan, a card from a SimpleFin connection, a closed
account, bills, a budget, a goal, and accounts held for others. A second SimpleFin account
is left unbound. The members are `alex`, an admin, and `sam`, each with a passkey whose
private key is in `staging/fixtures/passkeys.json`. Those keys are made up and public, so
`coffer serve` refuses that data unless `COFFER_PUBLIC_URL` is on localhost. That only guards
against an accident: anyone who can reach such a server can sign in as its admin with a script,
so never publish its port or put it behind a proxy. The stack binds `127.0.0.1` for this reason.

### End-to-end tests

```sh
scripts/staging.sh test                       # all of them, with stack and templates if need be
scripts/staging.sh test api/sync.spec.mjs     # arguments go to Playwright
scripts/staging.sh test --project=ui          # the pages only
```

CI's `e2e` job runs them on every pull request, on the image `docker-build` made, and keeps
what a failing test saw as an artifact. They need Node 24 or later on the machine, with npm
11.10 or later, the first to read `min-release-age` in `staging/e2e/.npmrc`, and run from
`staging/e2e/` in
[Playwright](https://playwright.dev): `npm ci` installs what its lockfile says, with no
install scripts, and the browser comes from `browser.lock.json`, its archive's hash checked
(about 150 MB in `staging/e2e/.browser`). Needs `unzip`. Each test gets its own server on a new copy of the template, and every
API response is checked against `docs/api/openapi.json`, whichever test made it, a field the
document doesn't name included. One run goes at a time in a checkout. They cover
what the in-process tests in `crates/*/tests/it/` can't, the built image, the real
configuration and the egress rules, and logic and edge cases stay there. A server that
panics or logs a mock credential fails the test using it.

The UI tests sign in with Chromium's virtual authenticator, holding either a new passkey or
the seed's. They set up a first admin on an empty server, make a token, bind an account, and
look at the forecast and net worth at phone and desktop widths, in light and dark. Any console
error, uncaught script error or browser report in the server's log, a content security
policy violation included, fails the test it happened in.

The known answers are the same on any day, which is checked on today and on templates
seeded as of the first of this month and the last of the previous one, asked about by date
through the forecast's `as_of` and net worth's `to`.

### Dates

Everything is dated from the day of the seed, and rebuilt on a new day, or when the
image, the migrations, the seed or the mocks change. `scripts/staging.sh template <day>`
seeds as of an earlier day, and `start <port> <day>` serves a throwaway copy of it, on a
port from 18081 to 18099, until `stop <port>`.

To see a failure, make it happen. Add a SimpleFin connection with the base64 of
`https://bridge.simplefin.org/claim/<scenario>/<any new name>` as its setup token, where
the scenario is `healthy`, `outage`, `cut-off`, `bad-json`, `revoked` or `rate-limited`. Add
an account in `NOK` for an ECB outage, `SEK` for a cut-off answer, or `DKK` for a currency
it doesn't publish; rates are fetched within a minute.

## Static assets

Files under `crates/server/assets` are served at content-hashed URLs, so
they are never renamed by hand. Fonts and icons are generated by
`tools/regenerate_assets.py` and are never edited by hand; change the
source and rerun the tool. `icons` needs resvg 0.48.1:

```sh
cargo install resvg --version 0.48.1 --locked
```

CI runs `icons` and fails if `crates/server/assets/icons` differs from what it
commits.

`fonts` needs `uv`, which installs the hash-pinned fontTools of
`tools/requirements-fonts.txt` into a throwaway environment.

## Pull requests

- Keep a change focused and explain the why in the description.
- Add tests that specify the new behavior; update the docs.
- Changes touching auth, sessions, tokens, policy, secrets or outbound HTTP
  need a security review recorded in the PR.

## Commit style

Use [conventional commits](https://www.conventionalcommits.org), for example
`feat: add invite expiry`. A DCO sign-off is not required.
