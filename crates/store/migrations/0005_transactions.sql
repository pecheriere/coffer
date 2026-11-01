-- Categories and canonical transactions with their splits. The CHECKs
-- mirror the domain types (CategoryKind, TransactionOrigin, ReviewStatus,
-- the text limits and the split rule); the store parses every value it reads.

CREATE TABLE category_groups (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    position    INTEGER NOT NULL CHECK (position BETWEEN 0 AND 10000),
    archived_at INTEGER,
    created_at  INTEGER NOT NULL
) STRICT;

CREATE TABLE categories (
    id          TEXT PRIMARY KEY,
    group_id    TEXT NOT NULL REFERENCES category_groups (id),
    name        TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    kind        TEXT NOT NULL CHECK (kind IN ('expense', 'income', 'transfer')),
    position    INTEGER NOT NULL CHECK (position BETWEEN 0 AND 10000),
    archived_at INTEGER,
    created_at  INTEGER NOT NULL
) STRICT;

CREATE INDEX categories_group_id ON categories (group_id);

CREATE TABLE transactions (
    id                      TEXT PRIMARY KEY,
    account_id              TEXT NOT NULL REFERENCES accounts (id),
    origin                  TEXT NOT NULL CHECK (origin IN ('manual', 'imported')),
    posted_on               TEXT NOT NULL
        CHECK (posted_on GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
    transacted_on           TEXT
        CHECK (transacted_on GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
    pending                 INTEGER NOT NULL CHECK (pending IN (0, 1)),
    amount_minor            INTEGER NOT NULL,
    -- The account's currency when the transaction was created; an
    -- account's currency never changes.
    currency                TEXT NOT NULL CHECK (length(currency) = 3),
    raw_description         TEXT NOT NULL CHECK (length(raw_description) BETWEEN 1 AND 500),
    -- The payee the source gave; `payee` is what members see. While
    -- `payee_edited` is 0, `payee` follows `imported_payee`.
    imported_payee          TEXT CHECK (length(imported_payee) BETWEEN 1 AND 200),
    payee                   TEXT CHECK (length(payee) BETWEEN 1 AND 200),
    payee_edited            INTEGER NOT NULL CHECK (payee_edited IN (0, 1)),
    memo                    TEXT CHECK (length(memo) BETWEEN 1 AND 500),
    -- Empty notes are stored as NULL.
    notes                   TEXT CHECK (length(notes) BETWEEN 1 AND 2000),
    review                  TEXT NOT NULL CHECK (review IN ('unreviewed', 'reviewed')),
    assignee_id             TEXT REFERENCES members (id),
    -- Who marked the transaction reviewed: set exactly while it is.
    reviewed_by_member      TEXT REFERENCES members (id),
    reviewed_by_agent       TEXT REFERENCES agent_tokens (id),
    reviewed_at             INTEGER,
    -- Who last changed the splits: a member (possibly through an agent) or
    -- the system with its reason.
    categorized_by_member   TEXT REFERENCES members (id),
    categorized_by_agent    TEXT REFERENCES agent_tokens (id),
    categorized_by_system   TEXT CHECK (length(categorized_by_system) BETWEEN 1 AND 100),
    categorized_at          INTEGER,
    flag_possible_duplicate INTEGER NOT NULL CHECK (flag_possible_duplicate IN (0, 1)),
    flag_source_removed     INTEGER NOT NULL CHECK (flag_source_removed IN (0, 1)),
    flag_split_mismatch     INTEGER NOT NULL CHECK (flag_split_mismatch IN (0, 1)),
    created_at              INTEGER NOT NULL,
    updated_at              INTEGER NOT NULL,
    CHECK (payee_edited = 1 OR payee IS imported_payee),
    CHECK (
        (review = 'unreviewed' AND reviewed_at IS NULL
            AND reviewed_by_member IS NULL AND reviewed_by_agent IS NULL)
        OR (review = 'reviewed' AND reviewed_at IS NOT NULL
            AND reviewed_by_member IS NOT NULL)
    ),
    CHECK (
        (categorized_at IS NULL AND categorized_by_member IS NULL
            AND categorized_by_agent IS NULL AND categorized_by_system IS NULL)
        OR (categorized_at IS NOT NULL AND categorized_by_member IS NOT NULL
            AND categorized_by_system IS NULL)
        OR (categorized_at IS NOT NULL AND categorized_by_system IS NOT NULL
            AND categorized_by_member IS NULL AND categorized_by_agent IS NULL)
    )
) STRICT;

CREATE INDEX transactions_account_posted ON transactions (account_id, posted_on);
CREATE INDEX transactions_review_assignee ON transactions (review, assignee_id);
-- The order of every transaction listing and its cursor.
CREATE INDEX transactions_posted_id ON transactions (posted_on DESC, id DESC);

-- The categorized parts of a transaction, in `position` order. They sum to
-- the transaction's amount unless it is flagged `split_mismatch`.
CREATE TABLE transaction_splits (
    id             TEXT PRIMARY KEY,
    transaction_id TEXT NOT NULL REFERENCES transactions (id) ON DELETE CASCADE,
    category_id    TEXT NOT NULL REFERENCES categories (id),
    minor_units    INTEGER NOT NULL CHECK (minor_units <> 0),
    position       INTEGER NOT NULL CHECK (position BETWEEN 0 AND 49),
    UNIQUE (transaction_id, position)
) STRICT;

CREATE INDEX transaction_splits_category_id ON transaction_splits (category_id);
