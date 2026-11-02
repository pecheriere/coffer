-- How many syncs in a row left the saved sync cursor where it was because
-- this provider account's records could not be stored. A sync that applies
-- the account, or whose failure fetching again cannot fix, sets it back to 0.
ALTER TABLE provider_accounts ADD COLUMN held_syncs INTEGER NOT NULL DEFAULT 0;
-- Whether a member asked for the cursor to move past this account's failing
-- records: a sync then applies them one by one, and those that still fail are
-- passed for good once it moves the cursor. Until a sync does, the request
-- stands.
ALTER TABLE provider_accounts ADD COLUMN hold_released INTEGER NOT NULL DEFAULT 0;
