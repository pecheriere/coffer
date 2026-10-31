# Using coffer from an AI agent

Agents call the JSON API at `/api/v1` with a bearer token that a household
member creates.

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
  "member": {"id": "...", "username": "alice", "display_name": "Alice", "role": "admin"},
  "agent": {"id": "...", "name": "claude", "scope": "read"}
}
```

## Endpoints (all need `read`)

- `GET /api/v1/me` identifies the member and token behind the request.
- `GET /api/v1/members` returns `{"items": [{id, username, display_name, role, disabled_at}]}`.
- `GET /api/v1/audit` returns `{"items": [...], "next": "<cursor>|null"}`, newest
  first. Query parameters: `entity_type`, `entity_id`, `member`, `agent`
  (ids), `from` (inclusive) and `to` (exclusive) as RFC 3339 timestamps
  (percent-encode `+` in offsets as `%2B`), `limit` (clamped to 1..200,
  default 50) and `after` (the previous `next`). These are the only accepted
  parameters; an unknown or repeated parameter is a 400 `invalid`. Each event has `id`, `at`,
  `actor` (`{kind: "member", member, agent}` or `{kind: "system", reason}`),
  `action`, `entity_type`, `entity_id` and `changes`.

## Errors

Errors are `application/problem+json` with a stable `code`:
`unauthenticated` (401, with `WWW-Authenticate: Bearer`), `forbidden` (403),
`not_found` (404, including `/api`, `/api/v1` and unknown `/api/v1/*` paths),
`method_not_allowed` (405, with an `Allow` header; the API is read-only here), `invalid` (400, bad
query parameter), `invalid_cursor` (400), `internal` (500). Error bodies never
echo what you sent.
