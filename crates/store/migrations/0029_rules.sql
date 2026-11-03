-- Rules that file or assign transactions nobody has filled yet (spec §8.13), in `position`
-- order, ties oldest first. A referenced category, account or member is never deleted: an
-- archived category or a disabled member only stops that part of the rule.
CREATE TABLE rules (
    id               TEXT PRIMARY KEY,
    match_kind       TEXT NOT NULL CHECK (match_kind IN ('contains', 'starts_with')),
    pattern          TEXT NOT NULL CHECK (length(pattern) BETWEEN 1 AND 200),
    account_id       TEXT REFERENCES accounts (id),
    amount_currency  TEXT CHECK (length(amount_currency) = 3),
    amount_min_minor INTEGER,
    amount_max_minor INTEGER,
    category_id      TEXT REFERENCES categories (id),
    assignee_id      TEXT REFERENCES members (id),
    position         INTEGER NOT NULL CHECK (position BETWEEN 0 AND 10000),
    enabled          INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL,
    CHECK (category_id IS NOT NULL OR assignee_id IS NOT NULL),
    CHECK (
        (amount_currency IS NULL AND amount_min_minor IS NULL AND amount_max_minor IS NULL)
        OR (amount_currency IS NOT NULL
            AND (amount_min_minor IS NOT NULL OR amount_max_minor IS NOT NULL))
    ),
    CHECK (amount_min_minor IS NULL OR amount_max_minor IS NULL
        OR amount_min_minor <= amount_max_minor)
) STRICT;

CREATE INDEX rules_order ON rules (position, id);

-- The rule that chose a transaction's category or assignee. No reference: deleting a rule leaves
-- what it did in place, and the id still says which one did it.
ALTER TABLE transactions ADD COLUMN categorized_by_rule TEXT
    CHECK (categorized_by_rule IS NULL OR categorized_at IS NOT NULL);

CREATE INDEX transactions_categorized_by_rule ON transactions (categorized_by_rule)
    WHERE categorized_by_rule IS NOT NULL;

-- Who last set or cleared the assignee, as for the splits. An empty assignee with these set was
-- cleared by someone, and a rule leaves that alone.
ALTER TABLE transactions ADD COLUMN assigned_by_member TEXT REFERENCES members (id);
ALTER TABLE transactions ADD COLUMN assigned_by_agent TEXT REFERENCES agent_tokens (id);
ALTER TABLE transactions ADD COLUMN assigned_by_system TEXT
    CHECK (length(assigned_by_system) BETWEEN 1 AND 100);
ALTER TABLE transactions ADD COLUMN assigned_by_rule TEXT;
CREATE INDEX transactions_assigned_by_rule ON transactions (assigned_by_rule)
    WHERE assigned_by_rule IS NOT NULL;
ALTER TABLE transactions ADD COLUMN assigned_at INTEGER CHECK (
    (assigned_at IS NULL AND assigned_by_member IS NULL AND assigned_by_agent IS NULL
        AND assigned_by_system IS NULL AND assigned_by_rule IS NULL)
    OR (assigned_at IS NOT NULL AND assigned_by_member IS NOT NULL
        AND assigned_by_system IS NULL)
    OR (assigned_at IS NOT NULL AND assigned_by_system IS NOT NULL
        AND assigned_by_member IS NULL AND assigned_by_agent IS NULL)
);

-- Assignees changed before this are in the audit log, whose last word for each transaction says
-- who. Only a member's edit ever changed one, under this action.
UPDATE transactions
SET assigned_by_member = last_edit.actor_member_id,
    assigned_by_agent = last_edit.actor_agent_id,
    assigned_at = last_edit.at
FROM (
    SELECT entity_id, actor_member_id, actor_agent_id, at, max(id)
    FROM audit_events
    WHERE entity_type = 'transaction'
      AND action = 'transaction.update'
      AND actor_kind = 'member'
      AND json_extract(changes_json, '$.assignee') IS NOT NULL
    GROUP BY entity_id
) AS last_edit
WHERE last_edit.entity_id = transactions.id;
