-- The account grants of an agent token: `level` on one account (its id) or
-- on every account (`all`). A token without rows has uniform access at its
-- scope. Grants go with their token.
CREATE TABLE agent_token_grants (
    token_id TEXT NOT NULL REFERENCES agent_tokens (id) ON DELETE CASCADE,
    target   TEXT NOT NULL CHECK (target = 'all' OR length(target) = 36),
    level    TEXT NOT NULL CHECK (level IN ('balances', 'transactions', 'write')),
    PRIMARY KEY (token_id, target)
) STRICT;
