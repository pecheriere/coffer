-- Rates an admin entered (issue #79), units of a currency per one euro as the canonical
-- text of the domain's FxRate. They live apart from fx_rates, which a fetch rewrites, so no
-- fetch can touch one, and a published rate under one is there again when it is removed. On
-- a day with both, the manual rate wins; a later day's rate of either kind takes over.
CREATE TABLE fx_manual_rates (
    quote      TEXT NOT NULL
        CHECK (length(quote) = 3 AND quote NOT GLOB '*[^A-Z]*' AND quote <> 'EUR'),
    on_date    TEXT NOT NULL
        CHECK (on_date GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
    rate       TEXT NOT NULL
        CHECK (length(rate) BETWEEN 1 AND 11 AND rate NOT GLOB '*[^0-9.]*'
               AND rate GLOB '*[1-9]*' AND rate NOT GLOB '*.*.*'
               AND rate NOT GLOB '.*' AND rate NOT GLOB '*.'),
    entered_at INTEGER NOT NULL,
    PRIMARY KEY (quote, on_date)
) STRICT, WITHOUT ROWID;

-- Which side of a conversion used a manual rate, so a row can be marked without reading the
-- rates again. No row can have used one before this table existed.
ALTER TABLE account_daily_values
    ADD COLUMN manual_rate_leg TEXT CHECK (manual_rate_leg IS NULL
        OR (manual_rate_leg IN ('held', 'display', 'both') AND conversion = 'converted'));
