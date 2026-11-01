-- Idempotency keys belong to the credential that sent them: a member's
-- session (`session`) or one of their agent tokens (its id). Tokens of one
-- member may see different accounts, so one never replays another's
-- response. Existing keys cannot be attributed to a credential and are
-- dropped; a key lives 24 hours, so only a retry spanning the upgrade is
-- performed anew.
DROP TABLE idempotency_keys;

CREATE TABLE idempotency_keys (
    member_id     TEXT NOT NULL REFERENCES members (id),
    credential    TEXT NOT NULL CHECK (credential = 'session' OR length(credential) = 36),
    key           TEXT NOT NULL CHECK (length(key) BETWEEN 1 AND 128),
    request_hash  BLOB NOT NULL CHECK (length(request_hash) = 32),
    state         TEXT NOT NULL CHECK (state IN ('pending', 'done')),
    status        INTEGER,
    response_json TEXT,
    created_at    INTEGER NOT NULL,
    PRIMARY KEY (member_id, credential, key),
    CHECK (
        (state = 'pending' AND status IS NULL AND response_json IS NULL)
        OR (state = 'done' AND status BETWEEN 100 AND 599 AND response_json IS NOT NULL)
    )
) STRICT;

CREATE INDEX idempotency_keys_created_at ON idempotency_keys (created_at);
