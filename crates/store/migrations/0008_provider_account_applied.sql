-- When each provider account's data was last applied in full. A sync asks
-- the provider for records from shortly before the oldest of these among
-- the accounts it reported within its window, so an account that failed
-- or was left out of a few answers is fetched again from where it stopped
-- instead of losing the days in between.
-- NULL until the account is first applied: such an account gets the
-- provider's full first window.
ALTER TABLE provider_accounts ADD COLUMN last_applied_at INTEGER;
