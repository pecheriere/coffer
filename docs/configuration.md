# Configuration

coffer is configured only through environment variables. Invalid values are
rejected at startup with an error that names the variable and never echoes
its value. The `serve`, `bootstrap-link`, `recover-admin` and `backup`
commands read the same configuration; `healthcheck` reads only
`COFFER_LISTEN`. The container image and how to run it are described in
[container.md](container.md).

| Variable | Default | Constraints |
|---|---|---|
| `COFFER_PUBLIC_URL` | required | The externally visible origin, also the WebAuthn origin: `https://<domain>[:port]` with nothing after the host but an optional `/`. `http://localhost[:port]` is accepted for local development only. Rejected: IP addresses (IPv4 or IPv6, over https too) because passkeys need a domain; a path (coffer cannot be served under a sub-path); user information, a query or a fragment. Changing the domain later invalidates every registered passkey. |
| `COFFER_DATA_DIR` | required | Directory holding `coffer.db` and the server key. Created with mode 0700 if missing; an existing directory that group or others can access (any of mode 0077 set) is refused at startup by every command, with the fix (`chmod 700 <dir>`) in the error. Must be persistent and backed up. |
| `COFFER_LISTEN` | `127.0.0.1:8080` | `host:port` socket address to bind. |
| `COFFER_SECRET_KEY_FILE` | `$COFFER_DATA_DIR/secret.key` | 32-byte server key, created with mode 0600 on first start. A file readable by group or others is refused. Losing it invalidates all sessions, agent tokens and outstanding links, and makes the stored provider credentials unreadable, so every provider connection must be reconnected; passkeys are unaffected. |
| `COFFER_INVITE_TTL_MINUTES` | `30` | 1 to 1440. Lifetime of setup, invite and re-enroll links. |
| `COFFER_SESSION_IDLE_HOURS` | `168` | 1 to 8760. A session ends after this long without use. |
| `COFFER_SESSION_ABSOLUTE_DAYS` | `30` | 1 to 365. A session ends this long after sign-in regardless of use. |
| `COFFER_TIMEZONE` | `UTC` | An IANA time zone name, such as `America/Los_Angeles`. The household's time zone: provider timestamps become calendar days in it, and it decides which day is today for sync windows and balance dates. The time zone database is built into the binary, so no `/usr/share/zoneinfo` is needed (a system copy is preferred when present). |
| `COFFER_SYNC_INTERVAL_HOURS` | `6` | 1 to 24. How often each provider connection syncs on schedule; a provider's own minimum interval can lengthen it. |
| `HTTPS_PROXY` | unset | Egress proxy for every outbound https request (`https_proxy` is read when `HTTPS_PROXY` is unset; an empty value counts as unset): `http://` or `https://` with a host, optional port and optional `user:password@`, nothing after the host but an optional `/`. coffer sends `CONNECT <host>:443` and never resolves target hosts itself, so the proxy enforces the public-address rule; the proxy's own address may be private. Its credentials never appear in logs or errors. `HTTP_PROXY`, `ALL_PROXY` and `NO_PROXY` are ignored. |
| `RUST_LOG` | `info` | A `tracing` filter, for example `info,coffer_server=debug`. An empty value counts as unset; a malformed one stops `serve` at startup (exit code 2). |
| `COFFER_LOG_FORMAT` | text | `json` switches to JSON lines. Logs go to stderr. |

## File permissions

Every `coffer` command sets its umask to 0077 before touching the data
directory, so everything it creates is readable by its owner only. That
matters for the database: SQLite creates `coffer.db` and its `coffer.db-wal`
and `coffer.db-shm` companions itself, with no way to choose their mode, and
they hold credential public keys, token hashes and financial data. Run coffer
as a dedicated user that owns the data directory.

## Reverse proxy and TLS

coffer speaks plain HTTP and must be reached through a TLS-terminating
reverse proxy: passkeys work only in a secure context, and session cookies
are `Secure`. Requirements for the proxy:

- Serve exactly the origin in `COFFER_PUBLIC_URL`.
- Forward the `Origin` header unchanged. coffer rejects any state-changing
  request outside `/api/` whose `Origin` differs from the public origin.
- Do not log request paths under `/setup/` or `/invite/`: they carry
  single-use secrets in the URL. coffer itself never logs request paths.
- Keep `COFFER_LISTEN` on a loopback or private interface that only the proxy
  can reach.

A Tailscale Service is one way to do this: run coffer on the host, expose it
with a Tailscale Service whose name gives `https://coffer.<tailnet>.ts.net`,
and set `COFFER_PUBLIC_URL` to that address. Any other proxy (Caddy, nginx,
Traefik) that meets the list above works the same way.

## Endpoints

- `GET /healthz` returns `200 ok` when the database answers, otherwise `503`.
  It needs no credentials and reveals nothing else.
- `/api/v1/*` is the JSON API (see `agent-usage.md`); everything else is the
  web UI.

## Commands

```sh
coffer serve                    # run the server
coffer bootstrap-link           # print a single-use first-admin link; exit 1 if an admin exists
coffer recover-admin <username> # print a re-enroll link for an admin who lost every passkey
coffer backup <dest>            # consistent copy of the database, mode 0600; refuses an existing file
coffer healthcheck              # exit 0 if the running server is healthy, 1 otherwise
```

Exit codes: 0 success, 1 the command failed, 2 bad usage or configuration.
`serve` shuts down gracefully on SIGTERM or ctrl-c.

`healthcheck` is meant for container health checks and process supervisors.
It sends `GET /healthz` to `COFFER_LISTEN`, connecting to loopback
(`127.0.0.1` or `[::1]`) when that address is unspecified (`0.0.0.0` or
`[::]`), and exits 0 only on a `200` response within 5 seconds. Anything else
(no listener, a `503`, a timeout, an invalid `COFFER_LISTEN`) is exit code 1,
with the reason on stderr; it never uses exit code 2, which Docker reserves
in health checks. It reads no other variable and never touches the data
directory or the server key.

## Backups

`coffer backup` produces a standalone SQLite file. It contains credential
public keys, token hashes and all financial data, so store it as carefully as
the live data directory. The server key (`secret.key`) is not part of the
backup; without it, restored agent tokens and sessions no longer validate and
must be recreated, and provider credentials (sealed under a key derived from
it) cannot be opened, so every provider connection must be reconnected, while
passkeys keep working.
