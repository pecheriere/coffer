-- Appearance: a member's theme and colour scheme, and the household theme.
-- The theme CHECKs mirror ThemeName::parse; whether a theme of that name is
-- registered is the server's concern, so a retired theme may stay stored.

-- NULL means "use the household theme".
ALTER TABLE members ADD COLUMN theme TEXT
    CHECK (theme IS NULL OR (length(theme) BETWEEN 1 AND 32 AND theme NOT GLOB '*[^a-z0-9-]*'));
ALTER TABLE members ADD COLUMN color_scheme TEXT NOT NULL DEFAULT 'system'
    CHECK (color_scheme IN ('system', 'light', 'dark'));

-- At most one row. No row means the registry's default theme.
CREATE TABLE household_appearance (
    id    INTEGER PRIMARY KEY CHECK (id = 1),
    theme TEXT NOT NULL
        CHECK (length(theme) BETWEEN 1 AND 32 AND theme NOT GLOB '*[^a-z0-9-]*')
) STRICT;
