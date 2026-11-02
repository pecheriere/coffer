-- Three more kinds, and whose money an account is (spec §8.11). Every existing row stays
-- the household's and nothing is guessed: B11.
--
-- A CHECK can't change in place. With foreign keys on, DROP TABLE counts every child row
-- as a violation, and only inserting parents into a table of the same name uncounts them:
-- the usual rename fails at COMMIT. Deferred violations are checked at COMMIT, so a row
-- lost here that anything refers to fails the whole migration. Nothing cascades from
-- these two tables.
PRAGMA defer_foreign_keys = ON;

CREATE TABLE accounts_held AS SELECT * FROM accounts;
DROP TABLE accounts;
CREATE TABLE accounts (
    id                    TEXT PRIMARY KEY,
    name                  TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    kind                  TEXT NOT NULL CHECK (kind IN (
        'checking', 'savings', 'credit_card', 'loan', 'mortgage', 'brokerage',
        'retirement', 'education', 'custodial', 'charitable', 'equity_comp',
        'property', 'vehicle', 'other_asset', 'other_liability')),
    -- An ISO 4217 code; never changes once the account exists.
    currency              TEXT NOT NULL CHECK (length(currency) = 3),
    liquidity             TEXT NOT NULL CHECK (liquidity IN (
        'cash', 'short_term_liability', 'invested', 'illiquid',
        'long_term_liability')),
    -- 1 once a liquidity class was chosen rather than defaulted from kind.
    liquidity_overridden  INTEGER NOT NULL CHECK (liquidity_overridden IN (0, 1)),
    include_in_net_worth  INTEGER NOT NULL CHECK (include_in_net_worth IN (0, 1)),
    include_in_forecast   INTEGER NOT NULL CHECK (include_in_forecast IN (0, 1)),
    closed_at             INTEGER,
    created_at            INTEGER NOT NULL,
    belongs_to            TEXT NOT NULL DEFAULT 'household'
        CHECK (belongs_to IN ('household', 'someone_else', 'given_away')),
    -- 1 once it was chosen rather than defaulted from kind.
    belongs_to_overridden INTEGER NOT NULL DEFAULT 0 CHECK (belongs_to_overridden IN (0, 1)),
    beneficiary           TEXT CHECK (length(beneficiary) BETWEEN 1 AND 64),
    -- Money that isn't the household's counts nowhere: spec §8.11 B4.
    CHECK (belongs_to = 'household' OR (include_in_net_worth = 0 AND include_in_forecast = 0)),
    CHECK (belongs_to <> 'household' OR beneficiary IS NULL)
) STRICT;
INSERT INTO accounts (id, name, kind, currency, liquidity, liquidity_overridden,
    include_in_net_worth, include_in_forecast, closed_at, created_at)
SELECT id, name, kind, currency, liquidity, liquidity_overridden,
    include_in_net_worth, include_in_forecast, closed_at, created_at FROM accounts_held;
DROP TABLE accounts_held;

CREATE TABLE provider_accounts_held AS SELECT * FROM provider_accounts;
DROP TABLE provider_accounts;
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
        'retirement', 'education', 'custodial', 'charitable', 'equity_comp',
        'property', 'vehicle', 'other_asset', 'other_liability')),
    account_id          TEXT REFERENCES accounts (id),
    -- The last balance as the provider gave it (a decimal text) and its day.
    last_balance_raw    TEXT,
    last_balance_on     TEXT
        CHECK (last_balance_on GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
    error               TEXT,
    first_seen_at       INTEGER NOT NULL,
    last_seen_at        INTEGER NOT NULL,
    -- When its data was last applied in full; NULL until it is first applied (0008).
    last_applied_at     INTEGER,
    -- Syncs in a row that left the saved cursor where it was for this account (0012).
    held_syncs          INTEGER NOT NULL DEFAULT 0,
    -- Whether a member asked for the cursor to move past its failing records (0012).
    hold_released       INTEGER NOT NULL DEFAULT 0,
    -- How many of its records the member agreed to when asking (0012).
    released_records    INTEGER NOT NULL DEFAULT 0,
    -- Whether such a request was withdrawn because something changed after it (0012).
    release_withdrawn   INTEGER NOT NULL DEFAULT 0,
    -- How many records the last sync that held the cursor could not store (0013).
    held_records        INTEGER NOT NULL DEFAULT 0,
    UNIQUE (connection_id, provider_account_id),
    CHECK ((last_balance_raw IS NULL) = (last_balance_on IS NULL))
) STRICT;
INSERT INTO provider_accounts SELECT * FROM provider_accounts_held;
DROP TABLE provider_accounts_held;
CREATE INDEX provider_accounts_account_id ON provider_accounts (account_id);
