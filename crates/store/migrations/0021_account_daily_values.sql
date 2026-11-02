-- One row per account per day (spec §8.10 N3): the balance used, and what it comes
-- to in the display currency. Derived from balance_snapshots, fx_rates and the
-- display currency: it can be dropped and rebuilt. rate_leg says which side of a
-- conversion a rate is about: the side of rate_on, the older of the two, or the
-- side with no rate on or before the day.
CREATE TABLE account_daily_values (
    account_id       TEXT NOT NULL REFERENCES accounts (id),
    on_date          TEXT NOT NULL
        CHECK (on_date GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
    balance_minor    INTEGER NOT NULL,
    balance_on       TEXT NOT NULL
        CHECK (balance_on GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
               AND balance_on <= on_date),
    balance_source   TEXT NOT NULL CHECK (balance_source IN ('sync', 'manual')),
    display_currency TEXT
        CHECK (length(display_currency) = 3 AND display_currency NOT GLOB '*[^A-Z]*'),
    display_minor    INTEGER,
    rate_on          TEXT
        CHECK (rate_on GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
               AND rate_on <= on_date),
    rate_leg         TEXT CHECK (rate_leg IN ('held', 'display', 'both')),
    conversion       TEXT NOT NULL CHECK (conversion IN
        ('same', 'converted', 'missing', 'too_large', 'no_display_currency')),
    CHECK ((display_minor IS NOT NULL) = (conversion IN ('same', 'converted'))),
    CHECK ((rate_on IS NOT NULL) = (conversion = 'converted')),
    CHECK ((rate_leg IS NOT NULL) = (conversion IN ('converted', 'missing'))),
    CHECK ((display_currency IS NULL) = (conversion = 'no_display_currency')),
    PRIMARY KEY (account_id, on_date)
) STRICT, WITHOUT ROWID;

-- filled_through is the last day the table holds. Until a rebuild has run there
-- is no such day, and rebuild_needed answers no number at all.
CREATE TABLE net_worth_state (
    id             INTEGER PRIMARY KEY CHECK (id = 1),
    filled_through TEXT
        CHECK (filled_through GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
    rebuild_needed INTEGER NOT NULL CHECK (rebuild_needed IN (0, 1)),
    CHECK (rebuild_needed = 1 OR filled_through IS NOT NULL)
) STRICT;
INSERT INTO net_worth_state (id, filled_through, rebuild_needed) VALUES (1, NULL, 1);
