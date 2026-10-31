-- Connections to data providers, the accounts and transaction records they
-- report (source records), possible duplicates and sync runs. The store
-- parses every value it reads.

CREATE TABLE connections (
    id              TEXT PRIMARY KEY,
    -- The provider's name, validated on read; no CHECK, so providers are
    -- added without a table rebuild.
    provider        TEXT NOT NULL,
    label           TEXT NOT NULL CHECK (length(label) BETWEEN 1 AND 100),
    owner_id        TEXT NOT NULL REFERENCES members (id),
    -- Sealed provider credentials (never in clear); NULL once removed.
    credentials     BLOB,
    -- Opaque provider state, such as a sync cursor.
    provider_state  TEXT,
    status          TEXT NOT NULL
        CHECK (status IN ('active', 'error', 'reauth_required', 'removed')),
    last_error      TEXT,
    last_attempt_at INTEGER,
    last_success_at INTEGER,
    created_at      INTEGER NOT NULL,
    removed_at      INTEGER,
    CHECK ((status = 'removed') = (removed_at IS NOT NULL))
) STRICT;

-- An account as a provider reports it. `account_id` is its binding to a
-- coffer account; while NULL its transaction records are held.
CREATE TABLE provider_accounts (
    id                  TEXT PRIMARY KEY,
    connection_id       TEXT NOT NULL REFERENCES connections (id),
    provider_account_id TEXT NOT NULL CHECK (length(provider_account_id) >= 1),
    name                TEXT NOT NULL,
    institution         TEXT,
    -- The currency as the provider gave it; checked when it is used.
    currency_raw        TEXT NOT NULL,
    kind_hint           TEXT CHECK (kind_hint IN (
        'checking', 'savings', 'credit_card', 'loan', 'mortgage', 'brokerage',
        'retirement', 'equity_comp', 'property', 'vehicle', 'other_asset',
        'other_liability')),
    account_id          TEXT REFERENCES accounts (id),
    -- The last balance as the provider gave it (a decimal text) and its day.
    last_balance_raw    TEXT,
    last_balance_on     TEXT
        CHECK (last_balance_on GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
    error               TEXT,
    first_seen_at       INTEGER NOT NULL,
    last_seen_at        INTEGER NOT NULL,
    UNIQUE (connection_id, provider_account_id),
    CHECK ((last_balance_raw IS NULL) = (last_balance_on IS NULL))
) STRICT;

CREATE INDEX provider_accounts_account_id ON provider_accounts (account_id);

-- A transaction record as a provider reports it, with its imported fields
-- (sanitized to the transaction limits) and fingerprint. `transaction_id`
-- is NULL while its provider account is unbound (held).
CREATE TABLE transaction_sources (
    id                   TEXT PRIMARY KEY,
    connection_id        TEXT NOT NULL REFERENCES connections (id),
    provider_account_ref TEXT NOT NULL REFERENCES provider_accounts (id),
    provider_txn_id      TEXT NOT NULL CHECK (length(provider_txn_id) BETWEEN 1 AND 255),
    transaction_id       TEXT REFERENCES transactions (id),
    posted_on            TEXT NOT NULL
        CHECK (posted_on GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
    transacted_on        TEXT
        CHECK (transacted_on GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
    -- The amount as the provider gave it (a decimal text).
    amount_raw           TEXT NOT NULL CHECK (length(amount_raw) BETWEEN 1 AND 64),
    -- Every record is converted to minor units before it is stored.
    amount_minor         INTEGER NOT NULL,
    description          TEXT NOT NULL CHECK (length(description) BETWEEN 1 AND 500),
    payee                TEXT CHECK (length(payee) BETWEEN 1 AND 200),
    memo                 TEXT CHECK (length(memo) BETWEEN 1 AND 500),
    pending              INTEGER NOT NULL CHECK (pending IN (0, 1)),
    fingerprint          TEXT NOT NULL,
    fingerprint_version  INTEGER NOT NULL CHECK (fingerprint_version >= 1),
    first_seen_at        INTEGER NOT NULL,
    last_seen_at         INTEGER NOT NULL,
    UNIQUE (connection_id, provider_txn_id)
) STRICT;

CREATE INDEX transaction_sources_transaction_id ON transaction_sources (transaction_id);
CREATE INDEX transaction_sources_account_posted
    ON transaction_sources (provider_account_ref, posted_on);

-- Transactions a flagged transaction may duplicate.
CREATE TABLE duplicate_candidates (
    transaction_id TEXT NOT NULL REFERENCES transactions (id) ON DELETE CASCADE,
    candidate_id   TEXT NOT NULL REFERENCES transactions (id) ON DELETE CASCADE,
    PRIMARY KEY (transaction_id, candidate_id),
    CHECK (transaction_id <> candidate_id)
) STRICT;

CREATE INDEX duplicate_candidates_candidate_id ON duplicate_candidates (candidate_id);

CREATE TABLE sync_runs (
    id                  TEXT PRIMARY KEY,
    connection_id       TEXT NOT NULL REFERENCES connections (id),
    trigger             TEXT NOT NULL CHECK (trigger IN ('schedule', 'manual', 'initial')),
    requested_by_member TEXT REFERENCES members (id),
    requested_by_agent  TEXT REFERENCES agent_tokens (id),
    status              TEXT NOT NULL
        CHECK (status IN ('queued', 'running', 'succeeded', 'partial', 'failed')),
    detail              TEXT,
    queued_at           INTEGER NOT NULL,
    started_at          INTEGER,
    finished_at         INTEGER
) STRICT;

CREATE INDEX sync_runs_connection_queued ON sync_runs (connection_id, queued_at);
