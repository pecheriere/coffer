-- The currency the household's net worth is shown in (spec §8.10). NULL means
-- none was chosen. It lives in the settings row but is not a forecast setting.
ALTER TABLE household_settings ADD COLUMN display_currency TEXT
    CHECK (length(display_currency) = 3 AND display_currency NOT GLOB '*[^A-Z]*');
