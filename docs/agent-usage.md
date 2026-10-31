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
3. Copy the token now. It starts with `coffer_` and is shown exactly once;
   coffer keeps only a keyed hash. Revoke it from the same page if it leaks;
   an admin can also see and revoke every member's tokens from
   `/admin/members`.

Tokens act as you: whatever the agent does is attributed to your member and
to that token in the audit log. You can create tokens only from a passkey
session, never with another token.

## Scopes

| Scope | Allows |
|---|---|
| `read` | Read household data: `me`, `members`, `audit`, accounts, categories and transactions. |
| `write` | Everything in `read`, plus changes to household data: creating and changing accounts, recording balances, creating and changing categories, and creating, reviewing, categorizing and deleting transactions. |
| `admin` | Everything in `write`, plus household administration. Only admins can create `admin` tokens. |

A token never exceeds its member's role: if the member is demoted or
disabled the token loses power at the next request. Tokens can never create,
list or revoke tokens, nor issue enrollment links; those need a passkey
session.

## First call

```sh
curl -H "Authorization: Bearer $COFFER_TOKEN" https://coffer.example/api/v1/me
```

```json
{
  "member": {"id": "...", "username": "alice", "display_name": "Alice", "role": "admin", "disabled_at": null},
  "agent": {"id": "...", "name": "claude", "scope": "read"}
}
```

## Endpoints

`GET /api/v1/openapi.json` needs no token. Every other operation needs at
least `read`:

- `GET /api/v1/me` identifies the member and token behind the request.
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
  `include_closed=true` adds closed ones.
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

- `GET /api/v1/transactions` lists transactions, newest posting day first
  (ties newest first), as `{"items": [...], "next": "<cursor>|null"}`.
  Every parameter given must hold:
  - `account`: 1 to 100 comma-separated account ids. An account that does
    not exist or that the token cannot see is a 404 `not_found`.
  - `from` (inclusive) and `to` (exclusive): `YYYY-MM-DD` posting days.
  - `review`: `reviewed` or `unreviewed`.
  - `assignee`: a member id, or `none` for unassigned transactions.
  - `category`: 1 to 100 comma-separated category ids; a transaction
    matches when one of its splits uses one of them.
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
  account's currency. Answers 201; a closed account is a 409 `conflict`.
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

### Retrying creates

Send an `Idempotency-Key` header (1 to 128 visible ASCII characters) with a
create. A retry with the same key and the same request within 24 hours gets
the first response back instead of creating again. The same key with a
different request is a 422 `idempotency_key_reuse`; while the first request
is still running, a retry is a 409 `idempotency_in_progress` (retry later).
Keys belong to your member, so use a fresh key per intended create.

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
(422), `idempotency_in_progress` (409), `internal` (500). `detail`, when
present, names at most the documented parameter at fault and never echoes
what you sent. A problem may carry a `Retry-After` header (seconds): wait
that long before retrying. Request bodies are limited to 256 KiB.
