-- Whether the last balance a provider reported for this account was refused
-- because it would have added or changed history at or before the account's
-- latest snapshot. Not a provider's error: coffer refused it, so it stays apart
-- from `error`, which the forecast reads as one.
ALTER TABLE provider_accounts
    ADD COLUMN old_balance_refused INTEGER NOT NULL DEFAULT 0
    CHECK (old_balance_refused IN (0, 1));
