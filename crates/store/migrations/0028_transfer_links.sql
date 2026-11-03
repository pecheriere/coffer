-- The two legs of one transfer (spec §8.12). Only a person makes or ends one, so nothing
-- cascades: the references refuse to delete a leg out from under its link, and whatever would
-- delete one has to end the link first and audit that.
CREATE TABLE transfer_links (
    id                TEXT PRIMARY KEY,
    outflow_id        TEXT NOT NULL UNIQUE REFERENCES transactions (id),
    inflow_id         TEXT NOT NULL UNIQUE REFERENCES transactions (id),
    created_by_member TEXT NOT NULL REFERENCES members (id),
    created_by_agent  TEXT REFERENCES agent_tokens (id),
    created_at        INTEGER NOT NULL,
    CHECK (outflow_id <> inflow_id)
) STRICT;

-- The UNIQUEs hold a transaction to one link on each side, and this across the two sides.
CREATE TRIGGER transfer_links_one_each BEFORE INSERT ON transfer_links
WHEN EXISTS (
    SELECT 1 FROM transfer_links
    WHERE outflow_id = NEW.inflow_id OR inflow_id = NEW.outflow_id
)
BEGIN
    SELECT RAISE(ABORT, 'a transaction is one leg of one transfer link at most');
END;
