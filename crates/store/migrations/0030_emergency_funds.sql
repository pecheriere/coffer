-- The emergency fund (money redesign §4.4, §10.4): at most one target per currency, a fixed
-- amount in it or some months of its planned outflows. No row: none set for that currency.
-- The CHECKs mirror coffer_domain::plan::EmergencyFundTarget.
-- (IS, not =: a CHECK that evaluates to NULL passes.)
CREATE TABLE emergency_funds (
    currency TEXT PRIMARY KEY
        CHECK (length(currency) = 3 AND currency NOT GLOB '*[^A-Z]*'),
    kind     TEXT NOT NULL CHECK (kind IN ('fixed', 'months_of_outflows')),
    minor    INTEGER CHECK (minor BETWEEN 0 AND 1000000000000000),
    months   INTEGER CHECK (months BETWEEN 1 AND 60),
    CHECK ((kind IS 'fixed' AND minor IS NOT NULL AND months IS NULL)
        OR (kind IS 'months_of_outflows' AND months IS NOT NULL AND minor IS NULL))
) STRICT, WITHOUT ROWID;

-- The old floor: a fixed one is its currency's target.
INSERT INTO emergency_funds (currency, kind, minor)
SELECT floor_currency, 'fixed', floor_minor FROM household_settings
WHERE floor_kind = 'fixed';

-- A months floor applied to every currency; it becomes a target for each one in use: open
-- accounts counted in the forecast, active outflows and the display currency.
INSERT INTO emergency_funds (currency, kind, months)
SELECT used.currency, 'months_of_outflows', s.floor_months
FROM household_settings AS s,
     (SELECT currency FROM accounts WHERE include_in_forecast = 1 AND closed_at IS NULL
      UNION SELECT currency FROM planned_items WHERE direction = 'outflow' AND active = 1
      UNION SELECT display_currency FROM household_settings WHERE display_currency IS NOT NULL)
     AS used
WHERE s.floor_kind = 'months_of_outflows';

-- household_settings without the floor. Nothing references it, so the plain rebuild is safe.
-- The display currency's and the time zone's CHECKs are 0020's and 0026's, verbatim.
CREATE TABLE household_settings_new (
    id                     INTEGER PRIMARY KEY CHECK (id = 1),
    include_income         INTEGER NOT NULL CHECK (include_income IN (0, 1)),
    default_horizon_months INTEGER NOT NULL
        CHECK (default_horizon_months BETWEEN 1 AND 60),
    stale_after_days       INTEGER NOT NULL
        CHECK (stale_after_days BETWEEN 1 AND 365),
    display_currency       TEXT
        CHECK (length(display_currency) = 3 AND display_currency NOT GLOB '*[^A-Z]*'),
    timezone               TEXT
        CHECK (length(timezone) BETWEEN 1 AND 64)
) STRICT;
INSERT INTO household_settings_new
    (id, include_income, default_horizon_months, stale_after_days, display_currency, timezone)
SELECT id, include_income, default_horizon_months, stale_after_days, display_currency, timezone
FROM household_settings;
DROP TABLE household_settings;
ALTER TABLE household_settings_new RENAME TO household_settings;
