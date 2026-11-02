-- How many records of this provider account the last sync that left the
-- sync cursor in place fetched and could not store, so the Connections page
-- can say how much a release would pass.
ALTER TABLE provider_accounts ADD COLUMN held_records INTEGER NOT NULL DEFAULT 0;
