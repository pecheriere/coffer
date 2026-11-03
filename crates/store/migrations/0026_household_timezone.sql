-- The household's time zone (spec §8.7). NULL means none was chosen, and the server's
-- COFFER_TIMEZONE applies. Days already stored are never rewritten when it changes. The
-- service accepts only known IANA names; this only keeps out what can't be one.
ALTER TABLE household_settings ADD COLUMN timezone TEXT
    CHECK (length(timezone) BETWEEN 1 AND 64);

-- The chosen zone each of a provider record's two days was worked out in; NULL means
-- COFFER_TIMEZONE, which every record before these columns was. A record reported again
-- keeps a day unless its instant moved in that zone, so a change of zone can't move money.
ALTER TABLE transaction_sources ADD COLUMN dated_in TEXT
    CHECK (length(dated_in) BETWEEN 1 AND 64);
ALTER TABLE transaction_sources ADD COLUMN transacted_in TEXT
    CHECK (length(transacted_in) BETWEEN 1 AND 64);
