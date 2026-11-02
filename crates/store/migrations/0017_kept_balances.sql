-- The balance a provider reported for an account not bound yet, one row per
-- provider account per day, the last write for a day wins. A snapshot needs an
-- account and providers only report today's balance, so this is what binding
-- turns into history; the bind removes the rows it reads. The currency is the
-- one the amount was reported in: a provider's can change while unbound.
CREATE TABLE kept_balances (
    provider_account_id TEXT NOT NULL REFERENCES provider_accounts (id),
    on_date             TEXT NOT NULL
        CHECK (on_date GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
    balance_minor       INTEGER NOT NULL,
    currency            TEXT NOT NULL CHECK (length(currency) = 3),
    recorded_at         INTEGER NOT NULL,
    PRIMARY KEY (provider_account_id, on_date)
) STRICT;
