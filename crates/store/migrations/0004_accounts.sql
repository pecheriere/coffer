-- Accounts, their daily balance snapshots, and idempotency keys for creates.
-- The CHECKs mirror the domain types (AccountKind, LiquidityClass,
-- BalanceSource, AccountName); the store parses every value it reads.

CREATE TABLE accounts (
    id                   TEXT PRIMARY KEY,
    name                 TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    kind                 TEXT NOT NULL CHECK (kind IN (
        'checking', 'savings', 'credit_card', 'loan', 'mortgage', 'brokerage',
        'retirement', 'equity_comp', 'property', 'vehicle', 'other_asset',
        'other_liability')),
    -- An ISO 4217 code; never changes once the account exists.
    currency             TEXT NOT NULL CHECK (length(currency) = 3),
    liquidity            TEXT NOT NULL CHECK (liquidity IN (
        'cash', 'short_term_liability', 'invested', 'illiquid',
        'long_term_liability')),
    -- 1 once a liquidity class was chosen rather than defaulted from kind.
    liquidity_overridden INTEGER NOT NULL CHECK (liquidity_overridden IN (0, 1)),
    include_in_net_worth INTEGER NOT NULL CHECK (include_in_net_worth IN (0, 1)),
    include_in_forecast  INTEGER NOT NULL CHECK (include_in_forecast IN (0, 1)),
    closed_at            INTEGER,
    created_at           INTEGER NOT NULL
) STRICT;

-- One snapshot per account per day; the last write for a day wins.
CREATE TABLE balance_snapshots (
    account_id    TEXT NOT NULL REFERENCES accounts (id),
    on_date       TEXT NOT NULL
        CHECK (on_date GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
    balance_minor INTEGER NOT NULL,
    source        TEXT NOT NULL CHECK (source IN ('sync', 'manual')),
    recorded_at   INTEGER NOT NULL,
    PRIMARY KEY (account_id, on_date)
) STRICT;

-- A create made with an Idempotency-Key. `request_hash` is a keyed hash of
-- the request, never the request itself (a body may hold a credential). A
-- `pending` row is completed in the same transaction as the create.
CREATE TABLE idempotency_keys (
    member_id     TEXT NOT NULL REFERENCES members (id),
    key           TEXT NOT NULL CHECK (length(key) BETWEEN 1 AND 128),
    request_hash  BLOB NOT NULL CHECK (length(request_hash) = 32),
    state         TEXT NOT NULL CHECK (state IN ('pending', 'done')),
    status        INTEGER,
    response_json TEXT,
    created_at    INTEGER NOT NULL,
    PRIMARY KEY (member_id, key),
    CHECK (
        (state = 'pending' AND status IS NULL AND response_json IS NULL)
        OR (state = 'done' AND status BETWEEN 100 AND 599 AND response_json IS NOT NULL)
    )
) STRICT;

CREATE INDEX idempotency_keys_created_at ON idempotency_keys (created_at);
