-- Exchange rates (spec §8.10). A rate is units of a currency per one euro, as
-- the canonical text of the domain's FxRate, so that comparing two texts
-- compares two rates and a re-fetch of an unchanged rate writes nothing. The
-- euro has no row: it is 1 on every day.
CREATE TABLE fx_rates (
    quote      TEXT NOT NULL
        CHECK (length(quote) = 3 AND quote NOT GLOB '*[^A-Z]*' AND quote <> 'EUR'),
    on_date    TEXT NOT NULL
        CHECK (on_date GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
    rate       TEXT NOT NULL
        CHECK (length(rate) BETWEEN 1 AND 11 AND rate NOT GLOB '*[^0-9.]*'
               AND rate GLOB '*[1-9]*' AND rate NOT GLOB '*.*.*'
               AND rate NOT GLOB '.*' AND rate NOT GLOB '*.'),
    source     TEXT NOT NULL CHECK (source IN ('ecb')),
    fetched_at INTEGER NOT NULL,
    PRIMARY KEY (quote, on_date)
) STRICT, WITHOUT ROWID;

-- How fetching each currency's rates last went. No row means it was never
-- tried. first_on and last_on are the days with a rate; a failure keeps them,
-- and a currency is ready only while it has one.
CREATE TABLE fx_coverage (
    currency        TEXT PRIMARY KEY
        CHECK (length(currency) = 3 AND currency NOT GLOB '*[^A-Z]*' AND currency <> 'EUR'),
    status          TEXT NOT NULL CHECK (status IN ('ready', 'not_published', 'failing')),
    first_on        TEXT
        CHECK (first_on GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
    last_on         TEXT
        CHECK (last_on GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
    last_attempt_at INTEGER NOT NULL,
    last_success_at INTEGER,
    last_error      TEXT CHECK (last_error IN ('unavailable', 'malformed', 'network')),
    CHECK (status <> 'ready' OR last_on IS NOT NULL)
) STRICT;
