-- A member's decision to leave a provider account unbound: it raises no forecast caution and
-- no notice that accounts wait to be set up. NULL while it is not dismissed. Binding the
-- account clears it, and a bound account is never dismissed.
ALTER TABLE provider_accounts ADD COLUMN dismissed_at INTEGER
    CHECK (dismissed_at IS NULL OR account_id IS NULL);
