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
| `read` | Read household data: `me`, `members`, `audit`. |
| `write` | Everything in `read`, plus changes to household data (as features arrive). |
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

`GET /api/v1/openapi.json` needs no token. Every other operation needs
`read`:

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
operation), `invalid_cursor` (400), `internal` (500). `detail`, when
present, names at most the documented parameter at fault and never echoes
what you sent. A problem may carry a `Retry-After` header (seconds): wait
that long before retrying. Request bodies are limited to 256 KiB.
