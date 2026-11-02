# Threat model

## Assets

- Household financial data (accounts, balances, transactions, plan).
- Credentials: passkey public keys, session cookies, agent tokens, provider
  credentials (a SimpleFin access URL, a Plaid access token), the server key.
- The audit log, whose value is that it is trustworthy.

## Trust boundaries

1. **Tailnet or private network to coffer.** coffer is meant to be reachable
   only from the household's private network (for example a tailnet) through a
   TLS reverse proxy. It is not designed to face the public internet.
   Anything on that network can reach the sign-in and invite pages.
2. **Browser to coffer.** Authenticated by a passkey (WebAuthn, user
   verification required) that yields a session cookie (`HttpOnly; Secure;
   SameSite=Strict`). State-changing page requests need the anti-forgery token
   and an `Origin` equal to the public origin.
3. **Agent to coffer.** Authenticated by a bearer token on `/api/v1` only.
4. **Host operator to coffer.** Whoever controls the host and data directory.
5. **coffer to data providers.** coffer makes outbound https requests to
   provider hosts (the SimpleFin Bridge, and Plaid when configured) with the
   connection's credentials, and treats every answer as untrusted input: amounts are
   parsed as decimal text into integer minor units, texts are sanitized and
   bounded before they are stored, and provider error texts are reduced to
   fixed messages. The provider is trusted only for the data it reports.

## Design

- Tokens (sessions, invites, agent tokens) are 256-bit random values. The
  database stores only a keyed BLAKE3 hash, domain-separated per token kind,
  so a stolen database or backup cannot be replayed without the server key.
  Secrets are shown once and never logged; errors never echo credentials.
- **Provider credentials are stored only sealed.** They are encrypted with
  XChaCha20-Poly1305 (random 24-byte nonce per seal) under a key derived from
  the server key with BLAKE3 `derive_key` (context `coffer 2026-10 provider
  credentials v1`). The connection id and the leading version byte are bound
  as associated data, so a sealed value copied to another connection's row
  does not open, and relabelling its version (to steer it to another key)
  fails authentication, as does any altered byte. The version byte allows a
  dedicated key later.
  In clear they exist only in memory, in buffers wiped after use (except
  the short-lived copies listed under accepted residual risks), and never
  appear in logs, errors, the audit log or responses. A stolen database or
  backup does not reveal them without the server key; losing the server key
  means reconnecting every provider.
- **Provider URLs cannot point coffer inward.** A SimpleFin setup token is
  the base64 of a claim URL that a member pastes; the claim URL and the
  access URL it returns must be `https` to a host in
  `COFFER_SIMPLEFIN_HOSTS`, checked before anything is sent. The egress
  client follows no redirect and, without a proxy, resolves the host itself
  and connects to public addresses only (no loopback, private, link-local or
  other special-purpose address). With an egress proxy (`HTTPS_PROXY`),
  coffer sends `CONNECT` to the allowed host and does not resolve it, so the
  proxy must enforce the public-address rule; the proxy's own address may be
  private. With an egress relay (`COFFER_EGRESS_RELAY`), coffer resolves
  nothing and connects to that one operator-configured address, which may be
  private; only the environment can set it, never a setup token, a provider
  or a request. The operator trusts the relay to forward to the allowed hosts
  only and to apply the public-address rule, which coffer cannot check any
  more; the relay learns which hosts coffer contacts, and when, and it can
  drop or delay every sync. coffer does not trust it with anything else: the
  allow-list, https and the ban on
  redirects are unchanged, and TLS is verified against the real host name, so
  a relay cannot read or alter the traffic unless it holds a certificate for
  that name from a CA the host trusts. `HTTP_PROXY`, `ALL_PROXY` and `NO_PROXY` are ignored, so the
  environment cannot quietly route around the allow-list. Setup tokens, claim and access URLs and provider response
  bodies never reach an error, a log line, the audit log or a response:
  coffer's own provider error texts are fixed, with at most a status code.
  Errors the bridge itself reports beside its data are shown after
  sanitizing (user information in URLs is redacted); any that holds the
  connection's access user or password is withheld and replaced by a fixed
  text.
- **Plaid.** Plaid connects through Plaid Link in the member's browser;
  coffer's server receives a one-time public token, exchanges it for an
  access token and keeps only that token, sealed like every provider
  credential (a small JSON document, bound to the connection id). The client
  secret is read from a file that must be mode 0600 or stricter, and lives in
  memory only. The server may reach only the configured Plaid host. On
  disconnect coffer calls Plaid's `/item/remove`, since Plaid bills an Item
  until it is removed. A removal that fails keeps the connection, so the
  member can retry; only `ITEM_NOT_FOUND` (the Item is already gone) counts as
  removed, and an Item that needs a new login is not removed until it has been
  reconnected. If coffer cannot reach Plaid to remove it, because Plaid is no
  longer configured or the sealed token no longer opens, the removal is refused
  and the token kept, so the Item is never orphaned while still billing. Plaid's `error_message` and request id are deliberately not
  kept: its free text can quote credentials, so coffer keeps only Plaid's error
  type and code, which are restricted to capitals, digits and underscores.
- Passkey ceremonies are single use, expire after 120 seconds, and live only
  in memory.
- Sessions have idle and absolute limits; sign-in ends any previous session
  of the browser.
- Authorization is centralized (`policy::authorize`) and applied by every
  handler. An agent token is capped by its scope and by its member's role,
  and the member is re-read on every request, so disabling a member or
  changing a role takes effect immediately.
- **Account grants narrow an agent token.** A token may carry grants: a
  level (`balances`, `transactions`, `write`) on chosen accounts, or on
  `all` accounts including ones created later. Its level on an account is
  the best grant naming it, capped by its scope (a `read` token never
  writes); an account no grant names is hidden. A token without grants,
  and every session, has uniform access at its scope. Grants are read on
  every request from the store by the one access resolver; a grant that no
  longer parses fails the request rather than being dropped, since
  dropping the last one would widen the token. Grants change only from a
  passkey session, by the token's member or an admin (anyone else's token
  answers like a missing one), and each change is audited with the grants
  before and after. The list can never be emptied, for the same reason: a
  token is widened only by an explicit `all` grant, which the audit log
  shows. On `/account` and, for an admin, `/admin/members`, a form that selects no level at all is refused
  rather than read as no restriction, and so is an account set at or below
  "All accounts", which would look restricted and not be. The side channels are
  closed:
  - A hidden account, its transactions and the provider account bound to
    it answer exactly like missing ones (the same 404 body), in single
    reads, list filters, batch operations (the error names the operation's
    index, never an id), merges and creates. Lists and their cursors are
    filtered in the query, so hidden rows neither appear nor shorten a page.
    As defence in depth, a transaction's duplicate candidates leave out those
    in an account the token cannot read: the store refuses a candidate
    pair across accounts, so none should exist.
  - Whatever spans every account needs an `all` grant at `transactions` to
    read (the audit log, unbound provider accounts, a connection's sync
    runs, whose counts and errors cover all its accounts) or at `write` to
    change (creating an account, binding a provider account, requesting a
    sync, adding, reconnecting or removing a connection, creating or
    changing a category or group), and is refused with a 403
    that names no account otherwise.
  - Connections list only the provider accounts the token may see.
  - Idempotency keys belong to the credential that sent them, so a token
    never receives a stored response another token (or the member's
    session) earned, and a create is authorized before its key is
    reserved, so a refused request leaves nothing behind.
- **Agent tokens can never create, list or revoke tokens, nor issue
  enrollment links.** Those actions require a passkey session, so a leaked
  token cannot be used to mint more access or to enroll a new passkey.
- **Origin rule:** every state-changing request outside `/api/` must carry an
  `Origin` equal to the public origin; it is enforced once, router-wide, so
  a new page route cannot forget it. `/api/` accepts bearer tokens only,
  which a browser never attaches on its own.
- **Referrer policy:** every response sets `Referrer-Policy: same-origin`.
  Other sites receive no referrer, so setup and invite tokens in the path
  never leave coffer. It is not `no-referrer` because under that policy
  browsers send `Origin: null` on plain form POSTs, which the origin rule
  refuses; `same-origin` lets them send the real origin, and the rule stays
  strict (it never accepts `null`).
- Every mutation is written with its audit entry in one transaction; entries
  record the member and the agent token involved.

## Accepted residual risks

- **Usernames are not secret.** Anyone on the network can learn whether a
  username exists by calling login begin. Sign-in itself reveals nothing
  further and fails identically for unknown users and bad assertions.
- **Pending sign-ins can be cancelled by flooding.** A device that knows a
  username can evict that member's pending sign-in ceremonies (at most four
  are kept per member) by repeatedly calling login begin. This is a nuisance,
  not a bypass: the member retries. The network boundary is the mitigation.
- **The bootstrap link appears in the server logs once.** When no admin
  exists, `coffer serve` logs a single-use setup link at startup (and
  `coffer bootstrap-link` prints one); it expires after the invite TTL and
  dies when the first admin is created. Anyone able to read the logs in that
  window could claim the first admin account, so run the first start on a
  private network and register the first admin promptly. coffer never logs
  any other request path, and proxies must not log `/setup/` or `/invite/`.
- **Host access is the root of trust.** Whoever can run `coffer` against the
  data directory can create admin re-enroll links (`coffer recover-admin`),
  read the database and key, and take backups. coffer does not defend
  against its own host; protect the host, the data directory (mode 0700) and
  backups (mode 0600).
- **A compromised SimpleFin Bridge can feed false data.** A bridge that is
  compromised, or impersonated by someone who obtained a valid certificate
  for its host, can report false accounts, balances and transactions, which
  coffer imports. It cannot make coffer reach any other host (the allow-list
  holds for every URL, redirects are never followed) and it never receives
  more than the connection's own credentials. Members review imported
  transactions, and removing the connection stops further imports.
- **Syncs are bounded by the provider's limits.** Manual and scheduled syncs
  are refused beyond the provider's minimum interval and daily budget, so a
  leaked `write` token can start at most that budget of sync runs (16 in 24
  hours for SimpleFin, 48 for Plaid) per connection. One run can make several
  provider requests: for Plaid, up to 601 when it pages and restarts as often as
  allowed, if it fits the limits below. Removals are not budgeted.
- **A member's Link pages are bounded too.** The two Link pages and the public
  token exchange share a budget of 20 requests per member in any 10 minutes,
  each one call to Plaid (an exchange whose result cannot be stored also gives
  the new Item up again). Beyond it they answer 429 with `Retry-After`
  and call nothing. A failed call counts; a request refused for any other
  reason does not. The budget is kept in memory, so a restart resets it, and
  agent tokens cannot reach these routes.
- **A sync's fetch is bounded in time and memory.** Every provider's fetch stops
  after 10 minutes, and a Plaid pass after 64 MiB of responses, on top of 16 MiB
  and 60 seconds per response. An answer listing more than 1,000 Plaid accounts
  fails the fetch as it is read. Any of these failures applies nothing and keeps
  the cursor. Nothing commits until a fetch ends, so an honest first sync that
  takes over 10 minutes, or a real history above the 64 MiB, would fail every
  run of that connection.
- **What the limits leave open.**
  - The worker runs one sync at a time, so a compromised or failing provider can
    still delay other connections by up to 10 minutes per fetch, within the
    budget above: 8 hours in 24 of fetching for one Plaid connection. Applying
    comes on top of that and has no deadline.
  - Memory in one fetch goes above the 64 MiB. Parsing adds overhead, and the
    account list and the page being read add up to 16 MiB each. A 16 MiB page of
    tiny malformed records expands much further while it is converted: by
    calculation over 500 MiB, until the record limit refuses it (issue #37). A
    SimpleFin answer has no limit on its accounts and expands the same way.
- **Some Plaid records are passed by for good and only counted.** A record with
  an id too long to store is skipped, and longer texts are cut. A record whose
  `account_id` is too long, or that names no account, belongs to no account coffer
  can report. If an earlier page of the same fetch named that record, its account
  counts it as skipped. Otherwise it is only counted among the run's skipped
  records and noted on the connection. The cursor moves past it, so it is not fetched again,
  and if it said a stored transaction was removed or changed, that transaction
  stays as it was. A provider can use this to hide a change from coffer, which
  it could also do by not reporting it.
- **Plaid's script runs on the Link page.** Link needs Plaid's script from
  `https://cdn.plaid.com/link/v2/stable/link-initialize.js`. Plaid serves it
  from that one URL and changes it in place, so coffer cannot pin a version or
  use subresource integrity, and it may not be bundled or hosted by coffer.
  The script runs with the page's DOM and origin: it can read the page's link
  token and anti-forgery token, and make same-origin requests as the signed-in
  member. The page holds no financial data, and a link token and a public
  token each work once, but the CSRF token lets script act as the member for
  that session. This is accepted: Plaid is already trusted with the bank
  logins and the access token. The exposure is limited to the Link pages,
  which alone carry a policy that allows the script: `script-src` adds that
  one URL, `frame-src` `https://cdn.plaid.com`, and `connect-src` the
  configured Plaid host. Every other page keeps `script-src 'self'` and
  `style-src 'self'`.
- **The Link policy differs from Plaid's documented ones.** Plaid's
  documentation lists `'unsafe-inline'` for scripts and styles. coffer's policy
  has no `unsafe-*` source. Plaid's script adds a stylesheet, which Link's
  mobile overlay needs for its layout, and marks it with the nonce of its own
  tag; so each Link response carries a fresh random nonce on that script tag
  and `style-src 'self' 'nonce-...'`. Plaid's script checks at start-up
  whether inline styles are supported, which produces exactly one
  `style-src-elem` violation in the browser console: that one is expected, and
  any other is a finding. Because Plaid may change its script, a future change
  could break Link under this policy; that would be a finding to fix, not a reason to add `'unsafe-inline'`.
- **Plaid's data is imported with care, not trusted.** A balance with more
  decimals than its currency allows is rounded half-even to minor units, with
  the exact text kept on the provider account; one too large to store is
  dropped and the account shows a note. Transaction amounts are never rounded:
  a record whose amount does not convert exactly is skipped and only counted.
  A held cursor protects against loss: when an account's records keep failing
  to store, coffer does not advance the Plaid sync cursor, so nothing is
  lost, and every sync fetches those records again until they apply. A balance
  Plaid sends in a form coffer cannot read (an exponent such as `1e3`) marks
  that account unreadable and holds the cursor the same way. The cost is
  repeated work, and a hold that lasts. After three syncs in a row, the
  connection says so, with a fixed text and the provider accounts that cause
  it (never a provider text), in the API (`held_cursor`) and on the
  Connections page; as with `last_error`, only an actor who sees every account
  gets it. The way out is a button on the page: the connection's owner or an
  admin, signed in with a passkey, lets the next sync move the cursor past the
  records that still fail. The form carries the provider accounts the page
  showed, and a release is refused if the accounts holding the cursor are no
  longer exactly those. The sync then applies those accounts' records one by
  one and gives up only the ones that fail twice; the others are stored. A
  record given up is lost for good, only counted as skipped, so the page says
  so plainly, with how many records the last fetch could not store. The
  records are given up, counted and audited (`connection.cursor_passed`, under
  the sync actor, with their count) only in the transaction that really moves
  the cursor; if another account still blocks it, the release waits, and a
  completed fetch without an account ends that account's hold and release, so
  an old request never passes records that arrive later. The request needs the
  anti-forgery token, write access to every account and leaves an audit entry
  naming the provider accounts. An agent token cannot do it, whatever its
  scope: an agent able to
  discard records could hide a transaction from the household. A provider's
  account id that is empty, holds a control character or is over-long is
  refused when a Plaid answer is read, as a record of no account, so it is
  counted as skipped and cannot hold the cursor for good.
- **Some copies of provider secrets are not wiped.** While a SimpleFin
  setup token is claimed and while a sync fetches, the claim URL, the access
  URL and its user name pass through `url::Url` and `String` buffers (the
  parsed URL, the user name used for basic auth, the text the JSON parser
  produces and the percent-decoded password before it becomes a
  `SecretString`). These copies live only for the duration of the call and
  are freed, not zeroed. Wiping them would mean parsing URLs by hand instead
  of with `url::Url`, trading a well-tested parser for a home-made one on
  input an attacker can shape; reading them needs access to the server's
  memory, which already exposes the server key. The same applies to Plaid: each
  request body holds the client secret and the access token in a buffer handed
  to the HTTP client, response bodies grow through buffers freed unwiped before
  they are wrapped, and the link token is an ordinary string in the rendered
  page.
- **A member can re-widen their own token.** Grants narrow what an agent
  can do, not what its member can do: from a passkey session the member (or
  an admin) can grant their token `all` again. The change needs a passkey,
  never a token, and the audit log records the grants before and after.
- **A restricted token still sees household-wide data.** Grants limit
  accounts, not the rest of the household: a token with grants still reads
  members, categories and connections (label, status, owner). Its changes
  to what is shared stop there: changing categories, or adding a connection
  (whose first sync queues work on accounts not yet granted), needs `all` at
  `write`. A connection's last error, the provider's sanitized text, could
  name an account the token cannot see, so it needs `all` at `transactions`;
  without it the token sees only that the status says something is wrong.
- **No rate limiting beyond ceremony caps.** Brute-forcing a 256-bit token is
  infeasible; password-style guessing does not exist. Network access control
  is the defense against abuse volume.
