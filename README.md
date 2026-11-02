# coffer

coffer is a self-hosted household financial hub: a backend that holds a
family's accounts, balances, transactions and spending plan, and that AI agents
(and later a UI) query to answer questions and help plan. Its motivating
question is periodic: over the next N months, how much cash does the household
actually need (recurring bills, known big expenses, money set aside for later,
plus a safety floor), so that everything above that can be invested.

> [!WARNING]
> **Work in progress.** coffer is pre-alpha and not ready for real use. It has
> members with passkey sign-in, invites, sessions, agent tokens, an audit log,
> a web UI and the container image, plus accounts, balances, categories and
> transactions with SimpleFin and Plaid sync, and per-account limits for agent
> tokens. The spending plan and the forecast are not built yet. The API, the database schema and the configuration will change
> without migration paths between 0.x releases. Do not put financial data you
> care about in it yet.

What exists and what comes next:

- **0.1.0:** identity and access. Passkey-only members (admin and
  member roles), expiring invite links, sessions, revocable agent tokens with
  read/write/admin scopes, an append-only audit log, backups, a container image.
- **0.2.0 (this release):** accounts (manual and linked), daily balances, categories
  and transactions with splits, review and assignment; SimpleFin and Plaid connections
  synced on a schedule and on demand, with duplicate matching and merging;
  agent tokens that can be limited to chosen accounts at a chosen level; a
  typed OpenAPI document of the JSON API.
- **Next:** planned items, sinking funds and the cash forecast; then reports
  and an MCP server for agents. See the
  [issues](https://github.com/pecheriere/coffer/issues) for what is planned.

Configuration is in [docs/configuration.md](docs/configuration.md); to run
coffer in a container, see [docs/container.md](docs/container.md). coffer now
makes outbound requests to the SimpleFin Bridge, and to Plaid when
configured, to sync; which hosts it
reaches and how to restrict them is in
[docs/configuration.md](docs/configuration.md#outbound-network). Agents use
the API as described in [docs/agent-usage.md](docs/agent-usage.md).

## Dependency age

Every crate version in `Cargo.lock` must have been published on crates.io at
least 7 days before it is added, so a compromised or broken release has time to
be noticed and yanked. On each pull request,
[`.github/scripts/crate_age.py`](.github/scripts/crate_age.py) compares
`Cargo.lock` with the base branch's, looks up the publish date of every registry
crate version that is new or changed, and fails the build for one younger than
7 days. Unchanged versions are never looked up, so a pull request that leaves
`Cargo.lock` alone makes no request to crates.io; a lookup that fails fails the
check rather than skipping the crate. The release workflow runs the same check
against the previous release tag, so a tag never ships a version that skipped
it, even one pushed straight to `main`.

The one exception is the oldest release that fixes a security advisory. List it
in [`.github/crate-age-allow.toml`](.github/crate-age-allow.toml), one entry per
crate version, with a reason that names the advisory (RUSTSEC or GHSA):

```toml
[[allow]]
name = "example-crate"
version = "1.2.3"
reason = "Oldest release without RUSTSEC-0000-0000"
```

An entry covers that exact version only. Once its version leaves `Cargo.lock`
the entry is stale and fails the check, so it is removed with the update that
replaces the crate. Run the check locally on the working tree against `main`:

```sh
python3 -I .github/scripts/crate_age.py --base main
```

A version that is too young is fixed by pinning an older one
(`cargo update -p <crate> --precise <version>`), not by an allow entry.

## Fonts

coffer bundles the Fredoka, Nunito and Manrope typefaces, under the SIL Open Font License
1.1. Their licences are in [`crates/server/assets/fonts/`](crates/server/assets/fonts/).

## License

Licensed under either of MIT or Apache-2.0, at your option.
