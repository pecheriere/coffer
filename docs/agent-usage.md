# Using coffer from an AI agent

Agents call the JSON API at `/api/v1` with a bearer token that a household
member creates. The API is described by an OpenAPI 3.1 document served
without a token at `GET /api/v1/openapi.json` (a copy is committed at
`docs/api/openapi.json`); generate a client from it or read it for every
operation, parameter and schema.

## Create a token

1. Sign in to coffer with your passkey and open `/account`.
2. Create an agent token: give it a name (for example `claude`), choose a
   scope and, preferably, an expiry.
3. To limit it to chosen accounts, set the levels on the same form (see
   [Account grants](#account-grants)). Left as they are, "All accounts" at
   `write` and every account at `none`, the token has no grants: it sees
   every account at its scope's level, and `write` is capped by the scope.
   To limit it to chosen accounts you must set "All accounts" to `none`,
   then pick their levels. Picking an account while "All accounts" stays at
   that level or above is refused, because the better level would win and
   the token would still reach every account. `none` everywhere is refused
   too.
4. Copy the token now. It starts with `coffer_` and is shown exactly once;
   coffer keeps only a keyed hash. Revoke it from the same page if it leaks;
   an admin can also see, revoke and change the account access of every
   member's tokens from `/admin/members`.

Tokens act as you: whatever the agent does is attributed to your member and
to that token in the audit log. You can create tokens only from a passkey
session, never with another token.

## Scopes

| Scope | Allows |
|---|---|
| `read` | Read household data: `me`, `members`, `audit`, accounts, categories, transactions, connections, provider accounts and sync runs. |
| `write` | Everything in `read`, plus changes to household data: accounts (creating, changing, recording balances), transactions (creating, reviewing, categorizing, assigning, merging, deleting), categories, connections (connecting, reconnecting, removing, binding provider accounts) and sync (requesting one). Removing or reconnecting another member's connection also needs the `admin` role. |
| `admin` | Everything in `write`, plus household administration. Only admins can create `admin` tokens. |

A token never exceeds its member's role: if the member is demoted or
disabled the token loses power at the next request. Tokens can never create,
list or revoke tokens, nor issue enrollment links; those need a passkey
session.

## Account grants

A token may also be limited to chosen accounts. Each grant gives a level on
one account, or on `all` accounts (including ones created later):

| Level | Allows on the account |
|---|---|
| `balances` | The account and its balances. |
| `transactions` | Also its transactions and their source records. |
| `write` | Also every change to them. |

The token's level on an account is the best grant naming it, capped by its
scope: a `read` token's grants never exceed `transactions`. An account no
grant names is hidden from the token. A token without grants sees every
account at its scope's level, as before.

Grants are set when the token is created and changed from `/account`, under
the token's "Change account access" (a passkey session only: the token's own
member, or an admin). A change applies at the token's next request and is
audited as `token.grants` with the canonical
text before and after (`all=balances,<id>=write`; `before` is `null` for a
token that had no grants). A token that is revoked or expired takes no new
grants (409). Grants can never be emptied, since no grants means uniform
access: to widen a token, grant `all` explicitly. `GET /api/v1/me` reports a
token's access.

- A hidden account, its transactions and its provider accounts answer
  exactly like ones that do not exist: 404 `not_found`, never listed, never
  named in an error (a batch names the failing operation by index only) or
  in another transaction's `duplicate_candidates`.
- A visible account below the level needed is a 403 `forbidden` whose
  detail names the level: "this action needs transactions access to this
  account". `GET /api/v1/transactions` silently leaves out the accounts the
  token sees only at `balances`; naming one in `account=` is the 403.
- Actions that span every account need an `all` grant: reading the audit
  log and a connection's sync runs need `all` at `transactions`; creating
  an account, binding a provider account, requesting a sync, adding,
  reconnecting or removing a connection, and creating or changing a category
  or category group need `all` at `write`. Without it
  they are a 403 whose detail says "this action needs write access to
  every account" (or `transactions`). Unbound provider accounts are listed
  (in `GET /api/v1/provider-accounts` and in connections) only with `all`
  at `transactions`; bound ones follow their account. A connection's
  `last_error` could name any account it serves, so without `all` at
  `transactions` it is `null`; its `status` still shows the problem. Using
  an existing category on a split needs only `write` on the transaction's
  account.

## First call

```sh
curl -H "Authorization: Bearer $COFFER_TOKEN" https://coffer.example/api/v1/me
```

```json
{
  "member": {"id": "...", "username": "alice", "display_name": "Alice", "role": "admin", "disabled_at": null},
  "agent": {"id": "...", "name": "claude", "scope": "read"},
  "access": {"ceiling": "transactions", "all": null, "accounts": [{"account": "...", "level": "balances"}]}
}
```

`access.ceiling` is the most the token's scope allows, `all` its level on
every account (`null` without an `all` grant) and `accounts` the level it
really has on each account a grant names. A token without grants shows
`all` equal to `ceiling` and no `accounts`.

## Endpoints

`GET /api/v1/openapi.json` needs no token. Every other operation needs at
least `read`:

- `GET /api/v1/me` identifies the member and token behind the request, and
  the token's account access.
- `GET /api/v1/members` returns `{"items": [{id, username, display_name, role, disabled_at}]}`.
- `GET /api/v1/audit` returns `{"items": [...], "next": "<cursor>|null"}`, newest
  first. Query parameters: `entity_type`, `entity_id`, `member`, `agent`
  (ids), `from` (inclusive) and `to` (exclusive) as RFC 3339 timestamps
  (percent-encode `+` in offsets as `%2B`), `limit` (clamped to 1..200,
  default 50) and `after` (the previous `next`). These are the only accepted
  parameters; an unknown or repeated parameter is a 400 `invalid`. Each
  event has `id`, `at`, `actor` (`{kind: "member", member, agent}` or
  `{kind: "system", reason}`), `action`, `entity_type`, `entity_id` and `changes`. `changes` is an object
  `{field: {"before": value, "after": value}}`, one entry per field the
  action touched; each value is `null`, a boolean, an integer or a string
  (ids, RFC 3339 timestamps and `YYYY-MM-DD` dates are strings). `before` is
  `null` for a field the action set, `after` is `null` for one it removed.
  Entries recorded before this format (bare `{"from", "to"}` objects) are
  returned in it too.

### Accounts

Amounts are integers in the minor units of the account's ISO 4217 currency
(`{"minor_units": -1234, "currency": "USD"}` is -12.34 USD); negative is
money owed or going out.

- `GET /api/v1/accounts` lists accounts with their latest balance;
  `include_closed=true` adds closed ones. An account's `source` is `manual`
  (kept by hand) or `linked` (bound to a provider account: syncs bring its
  transactions and balances).
- `POST /api/v1/accounts` (`write`) creates a manual account from `name`,
  `kind`, `currency` and optionally `liquidity` (by default the kind's: a
  credit card is a `short_term_liability`), `include_in_net_worth`,
  `include_in_forecast` (both default `true`) and `opening_balance`
  (`{"on": "YYYY-MM-DD", "minor_units": n}`). Answers 201.
- `GET` and `PATCH /api/v1/accounts/{account_id}` read and change an
  account. The currency never changes (sending it is a 400 `invalid`); a
  new `kind` re-defaults the liquidity class unless one was chosen;
  `closed: true` closes the account.
- `GET /api/v1/accounts/{account_id}/balances` pages the daily balances,
  newest first (`from` inclusive, `to` exclusive, `after`, `limit`).
- `PUT /api/v1/accounts/{account_id}/balances/{on}` (`write`) records
  `{"minor_units": n}` as that day's balance. A day holds one balance: the
  last write wins, so a linked account's next sync replaces a manual value
  for the same day.

### Categories

Categories are household-wide, grouped and ordered: groups and the
categories within a group are listed by `position` (0 to 10000; lower
first, ties oldest first). A category's `kind` is `expense`, `income` or
`transfer`. A household starts with no categories.

- `GET /api/v1/categories` returns `{"items": [{id, name, position,
  archived, categories: [{id, group, name, kind, position, archived}]}]}`,
  archived groups and categories included.
- `POST /api/v1/category-groups` (`write`) creates a group from `name` and
  optionally `position` (by default after the last group). Answers 201.
- `PATCH /api/v1/category-groups/{group_id}` (`write`) changes `name`,
  `position` or `archived`.
- `POST /api/v1/categories` (`write`) creates a category from `group`,
  `name`, `kind` and optionally `position` (by default last in its group).
  Answers 201; an unknown `group` is a 422 `invalid`.
- `PATCH /api/v1/categories/{category_id}` (`write`) changes `name`, `kind`,
  `position`, `archived`, or `group` (moving it).

Archiving never deletes: an archived category, or any category of an
archived group, takes no new splits, and the splits it already has stay.
Names are 1 to 100 characters without control characters.

### Transactions

A transaction has a signed `amount` (`{"minor_units": -1234, "currency":
"USD"}`; negative is money out of the account), a `posted_on` day, the
`description` its source gave, and the fields members and agents set:
`payee`, `notes`, `review` (`unreviewed` or `reviewed`), `assignee` (a
member id or `null`) and `splits` (`[{id, category, amount}]`, empty when
uncategorized). `reviewed_by` and `categorized_by` say who reviewed and who
last changed the splits (`{member, agent, system, at}`); `origin` is
`manual` or `imported`.

An imported transaction lists the provider records it came from in
`sources` (`[{id, connection, provider_account, pending, posted_on,
amount_raw, first_seen_at}]`). A sync matches each record to one
transaction: the same record updates it; a posted record replacing a
pending one (even with a tip of up to 20 %) joins the pending one's
transaction, which keeps its id and everything members set; and two
identical purchases stay two transactions. A sync never changes `payee`
once set by hand, `notes`, `review`, `assignee` or `splits`, with one
exception: when the amount changes and there is a single split, the split
follows (attributed to `system:sync`). Flags ask for a look:
`possible_duplicate` (the record may be the same purchase as the
transactions in `duplicate_candidates`; a sync never merges them),
`source_removed` (the provider no longer reports it, and it was kept
because it holds changes members made) and `split_mismatch` (its amount
changed and its several splits no longer sum to it; saving valid splits
clears it).

`amount_raw` is the provider's amount exactly as it wrote it, in the
provider's own sign, while the transaction's `amount` follows coffer's rule
(negative is money out). For SimpleFin the two agree in sign. Plaid reports
money out as positive, so for a Plaid record the sign of `amount_raw` is the
opposite of `amount`: an `amount_raw` of `"12.30"` beside an `amount` of
`-1230` is 12.30 spent, not a refund. Always use `amount` to decide direction;
`amount_raw` is for checking the digits.

- `GET /api/v1/transactions` lists transactions, newest posting day first
  (ties newest first), as `{"items": [...], "next": "<cursor>|null"}`.
  Every parameter given must hold:
  - `account`: 1 to 100 comma-separated account ids. An account that does
    not exist or that the token cannot see is a 404 `not_found`.
  - `from` (inclusive) and `to` (exclusive): `YYYY-MM-DD` posting days.
  - `review`: `reviewed` or `unreviewed`.
  - `assignee`: a member id, or `none` for unassigned transactions.
  - `category`: 1 to 100 comma-separated category ids; a transaction
    matches when one of its splits uses one of them. Unlike `account`, an
    unknown category id is not an error: it matches nothing, so the result
    may be empty.
  - `uncategorized=true`: only transactions without splits
    (`uncategorized=false` does not filter).
  - `pending`: `true` or `false`.
  - `flag`: `possible_duplicate`, `source_removed` or `split_mismatch`.
  - `min_amount` and `max_amount` (both inclusive): the signed amount in
    minor units, so `max_amount=-1` is every outflow.
  - `q`: 1 to 100 characters searched in the description, the payee and the
    notes. It matches text that contains it; letter case is ignored for the
    ASCII letters `A`-`Z` only (`é` does not match `É`), and `%`, `_` and
    `\` match themselves, not as wildcards. Percent-encode `%` as `%25`.
  - `after` (the previous `next`) and `limit` (clamped to 1..200, default
    50).

  An unknown or repeated parameter, or a value that does not parse, is a
  400 `invalid` naming the parameter; a cursor that does not parse is a 400
  `invalid_cursor`.
- `POST /api/v1/transactions` (`write` on the account) creates a manual
  transaction in an open account from `account`, `posted_on`, `minor_units`,
  `description` and optionally `payee`, `memo` and `notes`; it takes the
  account's currency. Answers 201; a closed account, or a linked one, is a
  409 `conflict`.
  Accepts an `Idempotency-Key`.
- `GET /api/v1/transactions/{transaction_id}` reads one transaction.
- `PATCH /api/v1/transactions/{transaction_id}` (`write`) changes any of
  `payee` (`null` restores the source's), `notes` (`null` or `""` clears),
  `review`, `assignee` (an active member, or `null`) and `splits` (`[]`
  uncategorizes; otherwise 1 to 50 non-zero splits on categories that are
  not archived, summing exactly to the amount). A patch that changes nothing
  records nothing.
- `DELETE /api/v1/transactions/{transaction_id}` (`write`) deletes a manual
  transaction (204); an imported one is a 409 `conflict`.
- `POST /api/v1/transactions/batch` (`write`) applies
  `{"operations": [{"id": "<transaction id>", "patch": {...}}]}`: 1 to 200
  patches, each to a different transaction, in order and atomically. It
  answers `{"items": [...]}` in operation order, or, when one operation
  fails, nothing is applied and the problem carries that failure's status
  and code with a `detail` starting `operations[<index>]: ` (zero-based),
  for example `operations[1]: a split names an archived category`. Use it
  to review a page of transactions with the owner in one call: categorize,
  assign and mark them reviewed together.

- `POST /api/v1/transactions/{transaction_id}/merge` (`write`) merges
  `{"duplicate": "<transaction id>"}` into this transaction: the
  duplicate's source records move here and it is deleted. Use the
  transaction holding the notes, splits and review as the target; a
  duplicate with user state of its own is a 409 `merge_conflict` unless
  `discard_duplicate_state` is `true`.
- `POST /api/v1/transactions/{transaction_id}/dismiss-duplicate`
  (`write`) clears `possible_duplicate` when the candidates are distinct
  purchases.
- `POST /api/v1/transactions/{transaction_id}/unmerge` (`write`) moves one
  source record back into a transaction of its own.

### Connections and sync

A connection links coffer to a data provider (SimpleFin or Plaid). It reports
provider accounts; once a provider account is bound to a coffer account,
syncs bring that account's transactions and balances. Binding applies the
records already held and the balance last reported. coffer syncs every
connection on a schedule (`COFFER_SYNC_INTERVAL_HOURS`), and you can ask
for a sync.

- `GET /api/v1/connections` returns `{"items": [...]}`: each connection's
  `status` (`active`, `error`, `reauth_required` or `removed`),
  `last_error` (sanitized), `last_attempt_at`, `last_success_at`,
  `next_sync_at` and the provider accounts the token may see. Credentials
  are never returned.
- Plaid connections appear in `GET /api/v1/connections` like any other
  (`provider` is `plaid`), when the server is configured for Plaid. Connecting
  one is a browser step: the member logs in to the bank through Plaid Link
  from the Connections page, so no API call can create one, and an agent
  that needs one asks the member to do it.
- `POST /api/v1/connections/simplefin` (`write`) connects SimpleFin from
  `{"label": "...", "setup_token": "..."}`; accepts an `Idempotency-Key`.
  The setup token is claimed once and never stored or echoed.
- `POST /api/v1/connections/{connection_id}/sync` (`write`) queues a manual
  sync and answers 202 with the run (`status: "queued"`), attributed to
  your token. Follow it with
  `GET /api/v1/connections/{connection_id}/sync-runs`, which pages the
  connection's runs newest first (`after`, `limit`) with their `status`
  (`queued`, `running`, `succeeded`, `partial` or `failed`) and `detail`.
- `DELETE /api/v1/connections/{connection_id}` (`write`) gives up the
  connection's access at the provider and marks it `removed` (204). Its
  accounts and transactions stay. If the provider cannot be reached, or
  refuses (Plaid keeps billing an Item until it is removed there), it is an
  error and nothing changes: the connection stays and removing can be retried.
  A Plaid connection also answers 409 and stays if the server can no longer
  reach Plaid for it: Plaid is not configured, or its sealed token no longer
  opens. If that is permanent, only a member can forget the connection from
  the Connections page, signed in with a passkey and typing its label: no
  token can, and `DELETE` stays refused. Forgetting erases the sealed token
  without calling Plaid, so the Item keeps billing at Plaid until the member
  removes it in Plaid's dashboard; the audit log records `connection.forget`
  rather than `connection.remove`. Tell the member this instead of retrying.
- `GET /api/v1/provider-accounts` (`unbound=true` for the unbound ones),
  `POST /api/v1/provider-accounts/{provider_account_id}/bind` with
  `{"account": "<id>"}` (an open account in the same currency), and
  `POST /api/v1/provider-accounts/{provider_account_id}/create-account`
  (creates a linked account and binds it) attach what a provider reports to
  the household's accounts.

Sync limits are the provider's: for SimpleFin, at most one requested sync
per hour per connection, scheduled syncs at least two hours apart, and 16
syncs in 24 hours; for Plaid, a requested sync 15 minutes after the last,
scheduled syncs an hour apart, and 48 syncs in 24 hours. A sync request while a run is queued or running is a 409
`sync_in_progress`: wait for that run. A request the limits refuse is a 429
`sync_rate_limited` with a `Retry-After` header in seconds; do not retry
before it has passed.

When a connection's status is `reauth_required`, the provider no longer
accepts its access. For Plaid, the member logs in again from the connection's
Reconnect button in the browser; its accounts stay bound. A sync already
running when they finish no longer changes the status, and the schedule syncs
the connection when it is next due. For SimpleFin, ask the owner for a new setup token and send
it to `POST /api/v1/connections/{connection_id}/simplefin/reconnect` as
`{"setup_token": "..."}`: the connection becomes `active` again and keeps
its provider accounts, bindings and history.

## Review workflow

A typical pass over newly synced transactions with the owner:

1. List what needs a look: `GET /api/v1/transactions?review=unreviewed`,
   and `flag=possible_duplicate` for possible duplicates.
2. Categorize, assign and mark reviewed in batches of up to 200 with
   `POST /api/v1/transactions/batch`; each patch sets `splits`, `assignee`
   and `review: "reviewed"` as needed, and the batch applies atomically.
3. For each flagged duplicate, compare it with its `duplicate_candidates`:
   merge the same purchase with `.../merge`, or dismiss distinct purchases
   with `.../dismiss-duplicate`.

Amounts are always integers in minor units with their currency, and
negative is money out of the account; splits sum exactly to the
transaction's amount. Use an `Idempotency-Key`
on every create so a retry never creates twice.

### Retrying creates

Send an `Idempotency-Key` header (1 to 128 visible ASCII characters) with a
create. A retry with the same key and the same request within 24 hours gets
the first response back instead of creating again. The same key with a
different request is a 422 `idempotency_key_reuse`; while the first request
is still running, a retry is a 409 `idempotency_in_progress` (retry later).
Keys belong to the token that sent them (or to your passkey session): the
same key from another token is a separate request, never a replay. Use a
fresh key per intended create.

## Errors

Errors are `application/problem+json` bodies (RFC 9457):

```json
{
  "type": "urn:coffer:problem:invalid",
  "title": "Invalid request",
  "status": 400,
  "code": "invalid",
  "detail": "the `limit` parameter is not valid"
}
```

`type` is always `urn:coffer:problem:<code>`, and `code` is stable:
`unauthenticated` (401, with `WWW-Authenticate: Bearer`), `forbidden` (403),
`not_found` (404, including `/api`, `/api/v1` and unknown `/api/v1/*` paths),
`method_not_allowed` (405, with an `Allow` header), `invalid` (400: a bad
query or path parameter, or a body that is not valid JSON for the
operation), `invalid` (422: a value the operation does not accept, such as
an empty account name), `conflict` (409: the change does not fit the
current state), `invalid_cursor` (400), `idempotency_key_reuse`
(422), `idempotency_in_progress` (409), `merge_conflict` (409),
`currency_mismatch` (409), `setup_token_invalid` (422), `reauth_required`
(409), `sync_in_progress` (409), `sync_rate_limited` (429, with
`Retry-After`), `provider_error` (502: the provider failed or could not be
reached), `internal` (500). `detail`, when
present, names at most the documented parameter at fault and never echoes
what you sent. A problem may carry a `Retry-After` header (seconds): wait
that long before retrying. Request bodies are limited to 256 KiB.
