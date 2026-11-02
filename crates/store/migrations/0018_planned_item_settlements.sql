-- One planned item's occurrences in one calendar month are settled: paid some
-- way the forecast can't see, or skipped (spec §8.9 R9a). It never changes the
-- item, and goes with it. Only a recurring bill or a one-off outflow has any,
-- which the ledger keeps true.
CREATE TABLE planned_item_settlements (
    item_id TEXT NOT NULL REFERENCES planned_items (id) ON DELETE CASCADE,
    month TEXT NOT NULL CHECK (month GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]'),
    kind TEXT NOT NULL CHECK (kind IN ('paid', 'skipped')),
    settled_by_member TEXT NOT NULL REFERENCES members (id),
    settled_by_agent TEXT REFERENCES agent_tokens (id),
    settled_at INTEGER NOT NULL,
    PRIMARY KEY (item_id, month)
) STRICT, WITHOUT ROWID;
