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
   Anything on that network can reach the sign-in and invite pages, and
   `POST /browser-reports`, which writes to the log.
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
- **A panic's message is never logged or answered.** It can hold anything,
  so the panic log has only the source location and a backtrace of code
  locations, and a client gets a fixed 500. No crash reports leave the host.
  The release binary keeps its symbols and line tables for those backtraces,
  which reveals nothing the public source does not. Accepted risk: panic events
  are not rate limited, at about 8 KB each, so a client that could cause a panic
  could fill the logs; that needs a client-reachable panic, and a review of this
  code found none.
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
  and the token kept, so the Item is never orphaned while still billing.
  When Plaid is merely not configured, the token still works: configure Plaid
  again and retry; nothing can be forgotten. When the sealed token itself
  cannot be opened and that is permanent (the server key is truly lost), the
  member can instead forget the connection from the Connections page. The
  action is offered only for a connection whose Plaid token cannot be opened,
  needs a passkey session (an agent token cannot do it, and the API's `DELETE`
  stays refused), the same role as removal and the anti-forgery token, and
  asks for the connection's label to be typed. The page warns that a wrong key
  or a restored backup should be fixed first, since forgetting cannot be
  undone and removal then works normally. Forgetting marks the connection
  removed and erases the sealed token without calling Plaid. `/item/remove`
  needs the token, so coffer can no longer end billing for the Item; the page
  says so and tells the member to contact Plaid or check their Plaid
  dashboard. It is audited as `connection.forget` with `given_up_at_provider`
  false and the reason, never as
  `connection.remove`. Plaid's `error_message` and request id are deliberately not
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
  - A planned item hinted at an account the token holds below `transactions`
    answers like a missing one, and a split on a visible account linked to such a
    goal reads `goal: null`: its id would confirm a hidden item exists, tell when
    it was made (ids are UUIDv7) and which transactions share it. Naming it is
    refused like a missing goal even on the transaction holding the link, and a
    patch keeps a link it could not see while a split of that amount is sent, so
    its answer and whether it writes are those of a split with no link. What is
    left: such a patch dividing the money into other amounts drops the link,
    which the token cannot know. The audit log records it.
  - A safety floor is a figure of the whole household's plan, and a fixed one's
    currency could show money the token isn't granted. So
    `GET /api/v1/household/settings` and a patch's answer say `floor: null` below
    `all` at `transactions`, the access a forecast needs, exactly as with none set,
    and a restricted `admin` token that may replace the floor can't confirm a guess
    at it by the answer. The other settings (whether income counts, the default
    horizon, when a balance is stale) read the same for every token: they say how
    a forecast is worked out, not what the household holds or plans.
  - A forecast, a what-if and a plan-versus-actual report sum every account's
    balances and category spending, so each needs `all` at `transactions` before
    anything is read (spec §8.9 R2). There is no partial answer: a number over the
    visible accounts would be confidently wrong, and marking it partial would
    reveal that hidden accounts exist. The refusal is the same 403 whether or not
    any account is hidden, and a what-if, which carries items and settings of its
    own, is checked the same way, so it can't be used to probe an account. A
    what-if is a `POST` that persists nothing (no item, setting, audit entry or
    idempotency key), so a `read` token may call it.
  - The plan is shared by every account, as categories are. Creating, changing,
    deleting or settling a planned item needs `all` at `write`, decided before the
    item is looked up. Reading one needs
    `transactions` on its hinted account, or, for an item with no hint, on at least
    one account (R3), so a token holding only `balances` sees an empty list, not a
    403, and one hinted at an account the token can't read is a 404, never a 403
    that would confirm it exists. A balances grant shows only the account and its
    balances; an item's name, amount, cadence and notes show more.
  - Whatever spans every account needs an `all` grant at `transactions` to
    read (the audit log, unbound provider accounts, a connection's sync
    runs, whose counts and errors cover all its accounts) or at `write` to
    change (creating an account, binding a provider account, requesting a
    sync, adding, reconnecting or removing a connection, creating or
    changing a category or group), and is refused with a 403
    that names no account otherwise.
  - Connections list only the provider accounts the token may see.
  - Balances reported for an unbound provider account are kept apart from the
    accounts' snapshots (`kept_balances`), and no route, page, report or audit
    entry reads them. Only the bind does, which needs `all` at `write`; it
    turns them into the account's snapshots, in its transaction, and deletes
    them. A removed or forgotten connection's go in the same transaction as
    the removal. They do stay in a backup, like everything else in the database.
    Reconnecting a connection keeps them, as it keeps held records: new
    credentials may be a different login at the provider, and what the old one
    reported stays until a bind brings it in or the connection is removed.
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
  which a browser never attaches on its own. `/browser-reports` is exempt, and
  applies the rule below.
- **Browser reports only reach the log.** `POST /browser-reports` lets a browser
  say that a script, a passkey ceremony or the content security policy failed. It
  needs no credentials, since sign-in failures matter most, so anyone on the
  network can call it. It stores nothing, reads no session and sends nothing on.
  - *Every answer is the same empty 204*, so its content does not say whether a
    report was kept. Other methods get the router's 405. Its timing can differ: a
    request refused before the body is read is answered at once, so on a direct,
    unbuffered connection a client sending part of a body can tell. That reveals
    only rules this public source states.
  - *Refused early.* Only the exact content types `application/json` and
    `application/csp-report` are read. A declared or real body over 8 KiB, far
    below the general 64 KiB, is refused before any parsing. Only a report that
    passes every check then spends the limit of 5 per client and 20 in all per
    minute, so another site's page or junk cannot silence a real browser, and the
    size limit bounds what refusing them costs. The per client limit uses the last
    `X-Forwarded-For` entry, which the proxy adds; the global limit holds whatever
    a client claims.
  - *Forged lines.* The event has a fixed shape and says it is client-reported and
    untrusted. `kind`, `page` and `script` come from fixed lists, and a CSP report's
    text is built from fixed values, so none of them holds client text. The only
    client text is the message of a script report, which keeps only letters,
    numbers, punctuation and symbols, with whitespace reduced to single spaces, and
    which the log formats quote or encode. So a variation selector or other
    invisible character cannot hide text from a reader while an agent reading the
    log sees it. A test sends CR, LF, ANSI escapes and JSON-breaking
    text through the real binary in both formats.
  - *Tokens in URLs.* A page is logged only as its route template, such as
    `/invite/{token}`, and a path outside the list as `other`, so no query,
    fragment or link token is logged. A message loses any word that looks like an
    address, and a blocked address becomes a class, such as `other`.
  - *Cross-site requests.* A session is not used, so there is nothing to forge
    on a member's behalf; what a forged report could do is write to the log. Both
    content types need a CORS preflight from another site, which this route
    refuses, so a cross-site script cannot post them. A script report must carry
    the public `Origin`. A CSP report may have that, `null` or none, as browsers
    differ, but not another origin, and must be about a document of the public
    origin, since another site's policy can name this endpoint. None of this stops
    a program on the network: `Origin` is only a claim there, which is why the
    limits and allow-lists do not depend on it.
- **Referrer policy:** every response sets `Referrer-Policy: same-origin`.
  Other sites receive no referrer, so setup and invite tokens in the path
  never leave coffer. It is not `no-referrer` because under that policy
  browsers send `Origin: null` on plain form POSTs, which the origin rule
  refuses; `same-origin` lets them send the real origin, and the rule stays
  strict (it never accepts `null`).
- **Two pages hold financial data.** `/forecast` shows the household's totals,
  monthly figures and the names in its cautions (spec §6.4, §8.9 R14), and
  `/net-worth` shows what the accounts a member can see come to, period by period,
  with each account's name, balance and rate date (§8.10 N13). Each takes the session
  cookie only, never a bearer token, calls the same service as its API endpoint
  (`GET /api/v1/forecast`, `GET /api/v1/reports/net-worth`), so with the same access
  (R2, N2), and has no form or other write action, no inline script or style, and only
  the default content security policy: the net worth chart is SVG drawn on the server,
  with classes only. Neither is in the browser reports' table of pages, so a report
  from one has its message withheld and names no page. Like every page they are sent
  with `Cache-Control: no-store`, so neither the browser's nor an intermediary's cache
  keeps them, and `Referrer-Policy: same-origin`, so following a link off one tells
  another site nothing. The back button after signing out depends on that `no-store`:
  signing out ends the session on the server but can't reach into the browser, so it is
  `no-store` that makes the browser ask again and be sent to sign in rather than redraw
  the figures from a stored copy.
- Every mutation is written with its audit entry in one transaction; entries
  record the member and the agent token involved.

## Accepted residual risks

- **A released account applies one record per write transaction.** After a
  member releases a held cursor, the sync takes the write lock once per record
  of that account's delta, up to the 100,000 records a fetch may hold. Others
  can write between records, but a large delta can keep a sync busy for a long
  time. Only an owner or admin can start it, and only for a held cursor.
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
  any other request path (a browser report logs only a route template), and
  proxies must not log `/setup/` or `/invite/`.
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
- **A provider chooses the day of a balance it reports for an unbound account.**
  coffer keeps one row per provider account per day, about 135 bytes, so an
  honest provider costs about 50 KB per unbound account per year, kept until the
  account is bound or its connection removed, with no retention job. A day more
  than 31 days back is not kept, since a bank's date can lag but no further, so
  one account can never hold more than 32 rows plus one per day after that.
  Nothing dedupes ids within an answer, so a provider that repeats an id with
  different days writes up to 32 rows for it in one sync, and one that invents
  ids adds accounts as well. Either way a sync writes at most one row per entry
  of its answer, at most 1,000 entries: 16,000 rows or about 2 MB a day through
  SimpleFin's 16 syncs, 48,000 rows or about 6.5 MB through Plaid's 48. Whatever
  it reports within those days becomes history at the bind, as it would for a
  bound account, but it can't change a snapshot the account already has. The
  limit applies to keeping and to the bind's use of the last report, not to a
  bound account's balance, whose day is still the provider's to choose.
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
  fails the fetch as it is read, and so does a page of more than 1,000 records,
  counted before any of them is converted. Any of these failures applies nothing
  and keeps the cursor. Nothing commits until a fetch ends, so an honest first
  sync that takes over 10 minutes, or a real history above the 64 MiB, would
  fail every run of that connection.
- **What the limits leave open.**
  - The worker runs one sync at a time, so a compromised or failing provider can
    still delay other connections by up to 10 minutes per fetch, within the
    budget above: 8 hours in 24 of fetching for one Plaid connection. Applying
    comes on top of that and has no deadline.
  - Memory in one fetch goes above the 64 MiB. Parsing adds overhead, and the
    account list and the page being read add up to 16 MiB each. A SimpleFin
    answer is held whole, up to 16 MiB, and is read with the same bound as
    Plaid's pages: at most 1,000 accounts, 10,000 transactions on one account
    and 100,000 in all. Each list holds one entry past its limit, the rest
    is skipped but must still be valid JSON, and an answer over a limit fails
    the run with a fixed text before any entry is converted, so nothing is
    stored from it. Each account is read once, and the count comes from the
    value that is converted, so an account or answer written as a JSON array
    is held to the same limits. The `errors` list of an answer keeps its first
    100 entries; the rest are checked only as valid JSON. The kept errors are
    sanitized whole, and the text stored is cut to 500 characters.
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
  records that still fail. The form carries the provider accounts and the
  record count the page showed, and a release is refused if the accounts
  holding the cursor or their count are no longer exactly those. The sync then
  applies those accounts' records one by one and gives up only the ones that
  fail twice; the others are stored. An account that is itself refused (for
  instance one that cannot be read) is different: if the refusal is the one
  recorded when the member released, none of its records can be tried, so all
  of them are given up untried, and the page says so before the member
  releases. A failure that says nothing about the record (a busy, locked or
  damaged database, a failing or full disk, no connection in time) gives up
  nothing: the hold and the release stay for a later sync. A refusal other than
  the recorded one, or a fetch with more records for the account than the
  member agreed to, takes the release back (audited as
  `connection.release_withdrawn`, with a fixed reason): the account stays held,
  the page says so, and the member has to look again and release again. A
  record given up is lost for good, only counted as skipped, so the page says
  so plainly, with how many records the last fetch could not store. The records
  are given up, counted and audited (`connection.cursor_passed`, under the sync
  actor, with their count) only in the transaction that really moves the
  cursor; if another account still blocks it, the release waits, and a
  completed fetch without an account ends that account's hold and release. A
  waiting request never covers more records than the member agreed to. It is
  checked by count: a fetch of the same size or smaller may hold a record the
  member did not see, when others in the delta were netted away. The request
  needs the anti-forgery token, write access to every account and leaves an
  audit entry with the record count and each account's recorded error,
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
  without it the token sees only that the status says something is wrong. It also
  reads the forecast settings other than the floor, and the planned items without
  an account hint if it holds `transactions` on any account (below).
- **An unhinted planned item's name is household data.** A name such as
  "Payment on the savings card" names an account to every token that sees the item,
  and an item with no `account_hint` is visible to any token holding `transactions`
  on at least one account. Hinting the item at that account hides it from tokens
  that can't read the account, but coffer can't tell which words in a name are
  sensitive, so it hides nothing by itself. The docs tell agents not to put an
  account's name in an unhinted item.
- **Reshaping a split can drop a goal link the token can't see.** A token that
  can't see a goal reads `goal: null` on the split linked to it, and sending
  back what it read keeps the link and writes nothing. Dividing that split into
  other amounts drops the link, because there is no way to carry it over; the
  token can't know it did. The audit log records the split list before and
  after, with the goal's id, and a member can link it again. Until then the goal
  looks less spent, and the money counts as its category's spending instead.
- **A rate already held shows through a restricted token's net worth.** Rates are
  stored once for the household. A token with an account in a currency a hidden
  account already caused to be fetched gets an amount where, alone, it would read
  `not_fetched_yet` until the first fetch, and `fetch_failing` or `provider_off`
  for as long as an outage lasts or the provider stays off. Its `rate_on` then shows
  about when the last fetch was. That is one bit about a currency the token already
  sees, plus that date, and it can't tell a hidden account from a closed one or a
  past display currency; it shows no amount, count, name or id, and no other
  currency. No token can bring it about, since creating an account needs `all` at
  `write` and changing the display currency needs `all` at `balances` (spec §8.10
  N2, N8). An operator who keeps the provider off for a long time keeps the window
  open that long. Timing of any answer was not examined.
- **No rate limiting beyond ceremony caps and browser reports.** Brute-forcing a
  256-bit token is infeasible; password-style guessing does not exist. Network
  access control is the defense against abuse volume.
- **Anyone on the network can write browser reports to the log.**
  - A report can say anything within its fixed shape, so an event is a claim, not
    evidence. It can send an operator after a failure that never happened, and a
    message is free text of up to 200 characters that a reader could take for
    instructions. coffer logs nothing about who sent it.
  - The limit bounds log growth to 20 events a minute of about 1 KB each, about
    29 MB a day at worst, plus one count every 5 minutes. A device that can do
    that is already on the household's network, and the log's rotation is the
    operator's.
  - Someone who keeps the global limit spent hides other browsers' reports, though
    the `over_limit` count shows that it is happening. The limits are per fixed
    minute, so up to twice the limit can arrive across the turn of a minute.
  - Without a proxy that adds `X-Forwarded-For`, a client can choose its entry and
    escape the per client limit, and only the global one holds. With a proxy that
    adds none, all clients share one limit.
- **A browser report's message can hold data from the page.** A browser's error
  text can quote what a script was working on, and it reaches the log, up to 200
  characters. So a message is kept only if it comes from coffer's own code, which
  must never put an amount or a name in an error's message: `coffer.js` reports
  only a browser's text for an error and the name and text of an `Error`, and for a
  refused request only the status, not the server's text; `plaid-link.js` reports
  fixed texts and Plaid's error code, never Plaid's message. An `error` whose script
  is not one of coffer's, such as a password manager's, is logged with its message
  `[withheld]`. A message is also kept only from a page in the list in
  `http/browser_reports/fields.rs`. Some of those pages show names or provider
  accounts, which is safe only because coffer's code never reads them into an
  error. **The forecast page shows amounts, so it is not in that list:** a report from
  any page not listed has `page` `other` and its message `[withheld]`, so a new
  page is safe until someone decides otherwise.
  What is left:
  - An `unhandled_rejection` names no script, so one from another script on a
    listed page keeps its message, which could hold page content. Withholding them
    all would lose the rejections of coffer's own code.
  - A word without a `/`, such as a bare token, is not recognized as an address.
    coffer's own code puts no token in an error's message.
- **A report can't be trusted to be free of what log alerts look for.** A message
  word shaped like a credential (`eyJ` with a dot, or `-----BEGIN`) is replaced by
  `[token]`, so that anyone on the network cannot set off the operator's alert on
  secrets in logs with a few bytes. Other shapes pass. An alert on secrets in logs
  should ignore events with `trust` = `untrusted`.
- **No time limit on reading a request's body.** A client that sends a report's body
  slowly holds a connection and a task for as long as it likes, as on every other
  POST route. It holds at most 8 KiB and writes nothing to the log. The network
  boundary and the proxy's own timeouts are the mitigation.
- **The expected Plaid violation hides its twin.** A `style-src-elem` violation for
  inline content on a Link page is taken as Plaid's start-up check and dropped,
  uncounted. A second such violation on the page, the sign of a real change in
  Plaid's script, looks the same to the server, so only a browser's console shows
  it; anyone on the network can also send one, which does nothing. Link pages load
  rarely, so the check alone would not flood the log; it is dropped to keep every
  logged violation a finding.
