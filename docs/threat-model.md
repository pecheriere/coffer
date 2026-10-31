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
  In clear they exist only in memory, in buffers wiped after use, and never
  appear in logs, errors, the audit log or responses. A stolen database or
  backup does not reveal them without the server key; losing the server key
  means reconnecting every provider.
- Passkey ceremonies are single use, expire after 120 seconds, and live only
  in memory.
- Sessions have idle and absolute limits; sign-in ends any previous session
  of the browser.
- Authorization is centralized (`policy::authorize`) and applied by every
  handler. An agent token is capped by its scope and by its member's role,
  and the member is re-read on every request, so disabling a member or
  changing a role takes effect immediately.
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
- **No rate limiting beyond ceremony caps.** Brute-forcing a 256-bit token is
  infeasible; password-style guessing does not exist. Network access control
  is the defense against abuse volume.
