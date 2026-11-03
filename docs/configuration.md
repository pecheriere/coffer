# Configuration

coffer is configured only through environment variables. Invalid values are
rejected at startup with an error that names the variable and never echoes
its value. The `serve`, `bootstrap-link`, `recover-admin` and `backup`
commands read the same configuration; `healthcheck` reads only
`COFFER_LISTEN`. When Plaid is configured, that includes reading the Plaid
secret file, so each of those commands needs it present and readable with the
right mode, even `backup` and `recover-admin`. `healthcheck` and `openapi` do
not read it. The container image and how to run it are described in
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
| `COFFER_TIMEZONE` | `UTC` | An IANA time zone name, such as `America/Los_Angeles`. The household's time zone: provider timestamps become calendar days in it, and it decides which day is today for sync windows and balance dates, for the forecast's default `as_of` and its current month, and for the plan-versus-actual report's current month. It also decides the days of net worth: which day a balance's value starts on, when each new day is filled (shortly after midnight in this zone), the day an account closed and what "today" is for a report. It stays an environment variable, not a household setting: change it and restart the server. The time zone database is built into the binary, so no `/usr/share/zoneinfo` is needed (a system copy is preferred when present). |
| `COFFER_SYNC_INTERVAL_HOURS` | `6` | 1 to 24. How often each provider connection syncs on schedule; a provider's own minimum interval can lengthen it. |
| `COFFER_SIMPLEFIN_HOSTS` | `bridge.simplefin.org,beta-bridge.simplefin.org` | Comma-separated DNS host names (exact match, no wildcards, IP addresses, ports or schemes). The only hosts a SimpleFin setup token's claim URL, and the access URL it yields, may name; a token for any other host is refused before anything is sent. |
| `COFFER_PLAID_CLIENT_ID` | unset | Plaid client id. Set it together with `COFFER_PLAID_SECRET_FILE` to offer Plaid connections, or set neither; one without the other, or an empty value, is refused. With neither set, the other `COFFER_PLAID_*` variables are ignored and the web UI offers no Plaid. |
| `COFFER_PLAID_SECRET_FILE` | unset | Path to a file holding the Plaid secret for the environment in `COFFER_PLAID_ENV`; trailing newlines are removed. It must be a regular file of at most 4096 bytes, not empty, with mode 0600 or stricter (no group or other bit set): any other mode is refused at startup. A symlink is followed and the mode of its target is checked. The secret is read from the file, never from a variable, so it stays out of `docker inspect` and the process environment. |
| `COFFER_PLAID_ENV` | required with Plaid | `sandbox` or `production`; it selects the one Plaid API host coffer may reach (see [Outbound network](#outbound-network)). `development` is refused, since Plaid retired it. |
| `COFFER_PLAID_COUNTRY_CODES` | `US` | Comma-separated ISO 3166-1 alpha-2 codes in capitals, such as `US,CA`: the countries whose institutions Link offers. Only the shape is checked; Plaid refuses a country it does not serve. |
| `HTTPS_PROXY` | unset | Egress proxy for every outbound https request (`https_proxy` is read when `HTTPS_PROXY` is unset; an empty value counts as unset): `http://` or `https://` with a host, optional port and optional `user:password@`, nothing after the host but an optional `/`. coffer sends `CONNECT <host>:443` and never resolves target hosts itself, so the proxy enforces the public-address rule; the proxy's own address may be private. Its credentials never appear in logs or errors. `HTTP_PROXY`, `ALL_PROXY` and `NO_PROXY` are ignored. |
| `COFFER_FX_PROVIDER` | `ecb` | Where exchange rates come from: `ecb`, the European Central Bank's public data API, or `none`, which asks nobody, so reports say rates are missing. Anything else is refused. See [Outbound network](#outbound-network). |
| `COFFER_EGRESS_RELAY` | unset | One IP address, IPv4 or IPv6, of an SNI-forwarding relay you control: no port, no host name, no brackets, no zone. Addresses nothing can connect to are refused (`0.0.0.0/8`, `::`, link-local `fe80::/10`, multicast and broadcast), and so is an empty value; private and loopback addresses are accepted. Every outbound connection goes to this address, on port 443, and coffer resolves nothing; see [Outbound network](#outbound-network). Setting it together with `HTTPS_PROXY` or `https_proxy` is refused. |
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
- Add the client's address as the last entry of `X-Forwarded-For`, as Caddy,
  nginx with `$proxy_add_x_forwarded_for` and Tailscale do. It is read only to
  limit [browser reports](#browser-reports) per client, and never logged.
- Keep `COFFER_LISTEN` on a loopback or private interface that only the proxy
  can reach.

A Tailscale Service is one way to do this: run coffer on the host, expose it
with a Tailscale Service whose name gives `https://coffer.<tailnet>.ts.net`,
and set `COFFER_PUBLIC_URL` to that address. Any other proxy (Caddy, nginx,
Traefik) that meets the list above works the same way.

## Outbound network

coffer is no longer an offline service: to sync provider connections it
makes outbound https requests. It connects only to:

- the hosts in `COFFER_SIMPLEFIN_HOSTS` (by default `bridge.simplefin.org`
  and `beta-bridge.simplefin.org`), on port 443;
- when Plaid is configured, only the host `COFFER_PLAID_ENV` selects,
  `sandbox.plaid.com` or `production.plaid.com`, on port 443. The other
  Plaid environment's host is refused;
- unless `COFFER_FX_PROVIDER` is `none`, the European Central Bank's data API,
  `data-api.ecb.europa.eu`, on port 443, for exchange rates. A request says
  which currency and from which date, and carries no key or credential. The
  name is a CNAME for the ECB's CDN, `data--api-ecb-europa-eu.ax4z.com` on
  2026-11-02, so a firewall that works from DNS answers must allow that too;
  don't pin its address, and re-check the name when you set the rule, since the
  CDN's can change: `dig +short CNAME data-api.ecb.europa.eu`. A firewall that
  allow-lists by SNI needs only `data-api.ecb.europa.eu` on port 443. A household
  with one currency contacts nobody. Nothing breaks while the host isn't allowed:
  syncs and everything else go on, and rates are tried again every hour. Each
  currency logs one warning, then another only when its failure changes, and one
  when it recovers; repeats are at `debug`, and a restart isn't a change. A
  currency never fetched reports `rate_missing` with the reason `fetch_failing`;
  one already held is carried forward at its last rate, with `rate_stale` once that
  is a week old.

Every other destination is refused before a connection is opened. That
covers what coffer's server connects to. Plaid Link, the login dialog, runs in
the member's browser, not in coffer: the browser loads Plaid's script and
frame from `cdn.plaid.com` and calls the configured Plaid API host, so the
device that connects a bank must reach those, and Plaid's own hosts it opens
after that (see Plaid's Link documentation). coffer's server never contacts
`cdn.plaid.com`. coffer widens its content security policy only on the Link
pages, to those two hosts; see the [threat model](threat-model.md). Target
hosts must resolve to public addresses: coffer refuses loopback, private,
link-local and other special-purpose addresses, so a setup token cannot point
it at the local network. Redirects are never followed. `NO_PROXY`,
`HTTP_PROXY` and `ALL_PROXY` are ignored.

To enforce the same allow-list outside coffer, use one of:

- an egress proxy in `HTTPS_PROXY` (or `https_proxy` when `HTTPS_PROXY` is
  unset) that allows `CONNECT` to the hosts above
  (port 443) only. coffer then does not resolve the target itself, so the
  proxy must enforce the public-address rule;
- a firewall or SNI allow-list that lets the coffer host reach only those
  hosts on port 443.
- a relay in `COFFER_EGRESS_RELAY`, for a network with no DNS and no direct
  internet, where outbound TLS goes to a relay that allow-lists by SNI and
  forwards to the real host. An `HTTPS_PROXY` cannot do this job, since such a
  relay does not speak `CONNECT`, and the two settings cannot be combined.

With a relay, every allowed destination connects to that one address, on port
443, without a DNS lookup, and the public-address rule is skipped for that
address only: it may be private. It must be a relay you control, because you
are trusting it to reach the allowed hosts and nothing else, and to enforce
the public-address rule, which coffer can no longer check for the real host.
Nothing else changes. The allow-list is still checked before anything is sent,
https is still required, redirects are still not followed, and TLS is still
verified end to end: the SNI and the certificate check use the real host name,
so the relay sees which host is contacted but not the content, unless it holds
a certificate for that name from a CA the host trusts. So never add the relay's
own CA to the CA bundle coffer uses: that lets the relay read and alter
everything sent to the providers, credentials included. Only this variable can
set the address: a setup token, a provider or a request has no way to reach it.

Provider credentials are sealed under a key derived from the server key, so
losing `secret.key` also means reconnecting every provider connection.

## Plaid

Plaid is optional. To offer it, create a Plaid account, then set
`COFFER_PLAID_CLIENT_ID`, `COFFER_PLAID_ENV` and `COFFER_PLAID_SECRET_FILE`
(the secret for that environment). Members then connect a bank from the
Connections page, in the browser: Plaid Link needs a person to log in to the
bank, so an agent cannot start it. Agents can still list, sync and remove
Plaid connections through the API.

New links also ask Plaid for consent to investments, which is not billed until used, so adding holdings later needs no relink.

Create the secret file so that the secret never appears on a command line or
in shell history: set `umask 077`, then write the file from your secret
manager's output. For a container, see
[container.md](container.md#plaid-secret).

Sync limits for Plaid: a requested sync at least 15 minutes after the last
one, scheduled syncs at least an hour apart, and 48 syncs in 24 hours, per
connection. One pass over Plaid's transaction pages accepts at most 64 MiB of
responses and 100,000 records, a page may hold at most 1,000 records, and an
answer may list at most 1,000 accounts. A SimpleFin answer may list at most
1,000 accounts, 10,000 transactions on one account and 100,000 transactions in
all, and keeps the first 100 of its connection errors (the rest are checked only as valid JSON); one over any of the three counts fails the run and stores nothing from it, and the
bridge's own limit of 90 days keeps an honest household far below them. For any provider, a
fetch that takes more than 10 minutes fails the run, and the next run starts
from the same place. Nothing is stored until the fetch ends, so a first sync
that needs longer fails every time. The limit covers fetching only: storing what
was fetched comes on top and has no time limit. None of these can be configured.

OAuth banks: coffer sets no redirect URI. Plaid documents that OAuth works on
web without one, opening the bank in a popup (a new tab on mobile). The cost
is that people on mobile who open coffer inside a webview, such as the
browser built into a mail or social app, are blocked, because those browsers
often do not support popups. Tell them to open coffer in a normal browser.

## Endpoints

- `GET /healthz` returns `200 ok` when the database answers, otherwise `503`.
  It needs no credentials and reveals nothing else.
- `POST /browser-reports` takes [browser reports](#browser-reports). It needs
  no credentials and always answers an empty `204`.
- `/api/v1/*` is the JSON API (see `agent-usage.md`); everything else is the
  web UI.

## Browser reports

When something fails in a browser, the operator can find it in the log. coffer's
pages report uncaught script errors, unhandled promise rejections, failed passkey
ceremonies and Plaid Link failures, at most 5 per page load, and the browser
reports violations of the content security policy. All of them go to coffer's own
`POST /browser-reports`, never to a third party, and nothing is stored.

Each report kept writes one `WARN` event with the message `browser report
(client-reported, untrusted)`. **Anyone on the network can send one, so treat its
fields as claims.** With `COFFER_LOG_FORMAT=json`, on one line:

```json
{"timestamp":"...","level":"WARN","fields":{"message":"browser report (client-reported, untrusted)","source":"browser","trust":"untrusted","kind":"passkey","page":"/invite/{token}","script":"none","line":0,"column":0,"client_message":"enroll: NotAllowedError: The operation either timed out or was not allowed. [url]"},"target":"coffer_server::http::browser_reports::fields"}
```

| Field | What it holds |
|-------|---------------|
| `kind` | `error`, `unhandled_rejection`, `passkey`, `plaid` or `csp`. |
| `page` | The page's route template, such as `/invite/{token}`, never its real path, query or fragment. A path that is no known page is `other`. |
| `script` | The path of `coffer.js` or `plaid-link.js`, `none` if the report names no script, and `other` for anything else, another origin included. |
| `line`, `column` | Numbers in that script, `0` when unknown. |
| `client_message` | At most 200 characters of letters, numbers, punctuation and symbols, with whitespace reduced to single spaces. A word shaped like a credential (`eyJ` with a dot, `-----BEGIN`) is replaced by `[token]`, and one that looks like an address by `[url]`. For `csp` it is built from fixed values, such as `img-src blocked inline`. It is `[withheld]` from a page that is `other`, for an `error` whose script is not one of coffer's, and for an `unhandled_rejection` from `/connections`. |

To find them in JSON logs:

```sh
docker logs coffer 2>&1 | jq -Rc 'fromjson? | select(.fields.trust == "untrusted") | .fields'
```

In the text format, search for `client-reported`.

A report can't be trusted to be free of what your log alerts look for, despite
the `[token]` rule: if you alert on secrets in logs, ignore events with `trust` =
`untrusted`.

Limits: 5 reports per client and 20 in all per minute; a body of at most 8 KiB.
Only a report that passes every other check spends the limit. What is over a limit
or refused is dropped without a line. At most every 5 minutes,
and only when the next request arrives, one `WARN` event `browser reports dropped`
gives the counts (`over_limit`, `refused`); a restart loses a count not yet logged.
Many `over_limit` drops with reports you can't explain mean something on your
network is sending them.

The one `style-src-elem` violation that Plaid's script causes on each Link page is
expected, so it is neither logged nor counted. Any other violation on those pages
is a finding.

## Panics

A panic is a bug in coffer. The log gets one `ERROR` event with the message
`coffer panicked`, in the configured format, with where it happened and a
backtrace. With `COFFER_LOG_FORMAT=json`, on one line:

```json
{"timestamp":"...","level":"ERROR","fields":{"message":"coffer panicked","location":"crates/server/src/sync/engine/mod.rs:194:18","backtrace":"   0: ...\n  11: run\n             at ./src/crates/server/src/sync/engine/mod.rs:194:18\n ..."},"target":"coffer_server::cli::panic_report"}
```

To find them in JSON logs:

```sh
docker logs coffer 2>&1 | jq -Rc 'fromjson? | select(.fields.message == "coffer panicked") | .fields.location'
```

The panic's own message is left out on purpose, because it can hold anything
and logs carry no financial data. Nothing leaves the host. `coffer`'s own
frames name file and line; a dependency's frame may show only its function.

What happens next depends on where the panic was:

- **In a request:** the client gets a generic 500, a problem under `/api` and a
  plain page elsewhere, and the server goes on. The request's open transaction
  rolls back. An `Idempotency-Key` it had reserved answers
  `idempotency_in_progress` until the reservation lapses, within 5 minutes.
- **In a sync run:** the log also has `a sync run ended abnormally` with its
  id, the run is marked failed, and the next queued run follows. If the
  database is unavailable when the run is marked failed, the log says so and
  the worker tries again, waiting up to a minute between attempts, until it
  works; until then the connection answers that a sync is in progress, and if
  the server stops first the next start marks the run failed. Any other error
  is logged once, the worker goes on, and that run stays marked running until
  the next restart.
- **In the sync scheduler itself:** the log also has
  `a sync scheduler task ended abnormally; starting it again`, and the task
  starts again after a wait of up to a minute.
- **In a command such as `backup`, or before logging is set up:** the same
  location and backtrace go to stderr as plain text, and the exit code is 101.
  So do panics when `RUST_LOG` filters out the event.

## Commands

```sh
coffer serve                    # run the server
coffer bootstrap-link           # print a single-use first-admin link; exit 1 if an admin exists
coffer recover-admin <username> # print a re-enroll link for an admin who lost every passkey
coffer backup <dest>            # consistent copy of the database, mode 0600; refuses an existing file
coffer rebuild-net-worth        # recompute the daily net worth table up to today; prints rows written and last day
coffer healthcheck              # exit 0 if the running server is healthy, 1 otherwise
```

On first start with no admin yet, `serve` also logs the setup link. Every link
issued this way, by `serve` at startup or by `bootstrap-link`, revokes the
earlier unused setup links, so only the newest works: a restart of `serve`
invalidates a link printed before it. Both stop once an admin exists.

Exit codes: 0 success, 1 the command failed, 2 bad usage or configuration.
`serve` shuts down gracefully on SIGTERM or ctrl-c.

`rebuild-net-worth` recomputes the whole daily table from the balances, the
rates and the display currency, with today taken from `COFFER_TIMEZONE`. It is
safe while the server runs: it works in short transactions, so other writes take
turns with it, and reports say `rebuilding` until it ends if the table takes more
than one. One rebuild runs at a time, so it first waits for one the server has
under way, and the server leaves it alone until it ends. If it stops part way, the
running server's job finishes it. A file named after the database with
`.rebuild-lock` added says who is running. It stays there, and deleting it is safe only
while no rebuild runs. Its lock needs a local filesystem, as SQLite's WAL mode already does.
While another rebuild runs the command prints that it is waiting, for as long as that takes,
and the server logs once, at `info`, that it is leaving the rebuild to the other process.
The rows it prints are what it wrote, which can differ from what the table holds if a change
starts it over or a balance arrives while it runs. Opening the database applies this version's migrations, as
`backup` does, so run the same version as the server. It never creates a
database: no database found, or an empty file, is exit code 1. You rarely need
it: the first start after the upgrade that adds the table rebuilds it in the
background while the server already answers, and until then a report says so
instead of showing a number. It is also how to undo a clock that ran ahead: the
table never moves its last day back by itself, and the command resets it to
today and removes the days after. The server warns once a day while that last
day is more than a day past today.

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
passkeys keep working. An older version refuses to start on a database a newer one has
migrated (v0.3.0 on one migrated by a later version, for example), so rolling back needs a
backup taken before the upgrade.
