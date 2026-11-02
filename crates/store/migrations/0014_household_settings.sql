-- Household forecast settings (spec §6.8, §8.9). At most one row; no row
-- means ForecastSettings::default(). The CHECKs mirror the domain types.
-- No floor leaves all four floor columns NULL, which differs from a fixed
-- floor of 0.
-- (IS, not =: a CHECK that evaluates to NULL passes.)
CREATE TABLE household_settings (
    id                     INTEGER PRIMARY KEY CHECK (id = 1),
    floor_kind             TEXT
        CHECK (floor_kind IN ('fixed', 'months_of_outflows')),
    floor_minor            INTEGER
        CHECK (floor_minor BETWEEN 0 AND 1000000000000000),
    floor_currency         TEXT
        CHECK (length(floor_currency) = 3 AND floor_currency NOT GLOB '*[^A-Z]*'),
    floor_months           INTEGER
        CHECK (floor_months BETWEEN 1 AND 60),
    include_income         INTEGER NOT NULL CHECK (include_income IN (0, 1)),
    default_horizon_months INTEGER NOT NULL
        CHECK (default_horizon_months BETWEEN 1 AND 60),
    stale_after_days       INTEGER NOT NULL
        CHECK (stale_after_days BETWEEN 1 AND 365),
    CHECK (
        (floor_kind IS NULL
            AND floor_minor IS NULL AND floor_currency IS NULL AND floor_months IS NULL)
        OR (floor_kind IS 'fixed'
            AND floor_minor IS NOT NULL AND floor_currency IS NOT NULL
            AND floor_months IS NULL)
        OR (floor_kind IS 'months_of_outflows'
            AND floor_months IS NOT NULL
            AND floor_minor IS NULL AND floor_currency IS NULL)
    )
) STRICT;
