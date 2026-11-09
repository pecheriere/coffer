-- Plan history (money redesign §10.3): a planned item's amount, currency, direction,
-- category, spending kind and tier become effective-dated versions in their own table, so
-- a past month reads the values in force then. Every item gets one version equal to its
-- old values, effective from the month of its anchor and written on the day it was created.
--
-- planned_items loses those columns, which SQLite can't drop beside the CHECKs that name
-- them, so it is rebuilt (0022's method). Two things need care:
--   * planned_item_settlements cascades from planned_items, and DROP TABLE deletes the
--     rows it refers to first: the settlements are copied aside, and put back.
--   * With foreign keys on, DROP TABLE counts every referring row (transaction_splits.goal_id,
--     the settlements) as a violation; deferred violations are checked at COMMIT, and
--     inserting the parents into a table of the same name uncounts them. A row lost here that
--     anything refers to fails the whole migration.
PRAGMA defer_foreign_keys = ON;

CREATE TABLE planned_item_settlements_held AS SELECT * FROM planned_item_settlements;
CREATE TABLE planned_items_held AS SELECT * FROM planned_items;
DROP TABLE planned_items;

CREATE TABLE planned_items (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    account_hint_id TEXT REFERENCES accounts (id),
    notes TEXT CHECK (length(notes) BETWEEN 1 AND 2000),
    active INTEGER NOT NULL CHECK (active IN (0, 1)),
    item_type TEXT NOT NULL CHECK (item_type IN ('recurring', 'one_off', 'goal')),
    every INTEGER CHECK (every BETWEEN 1 AND 999),
    unit TEXT CHECK (unit IN ('days', 'weeks', 'months', 'years')),
    anchor_on TEXT
        CHECK (anchor_on GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
    end_on TEXT
        CHECK (end_on GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
    one_off_on TEXT
        CHECK (one_off_on GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
    goal_start_on TEXT
        CHECK (goal_start_on GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
    goal_due_on TEXT
        CHECK (goal_due_on GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
    goal_completed INTEGER CHECK (goal_completed IN (0, 1)),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK (
        (item_type = 'recurring'
            AND every IS NOT NULL AND unit IS NOT NULL AND anchor_on IS NOT NULL
            AND one_off_on IS NULL
            AND goal_start_on IS NULL AND goal_due_on IS NULL AND goal_completed IS NULL)
        OR (item_type = 'one_off'
            AND every IS NULL AND unit IS NULL AND anchor_on IS NULL AND end_on IS NULL
            AND one_off_on IS NOT NULL
            AND goal_start_on IS NULL AND goal_due_on IS NULL AND goal_completed IS NULL)
        OR (item_type = 'goal'
            AND every IS NULL AND unit IS NULL AND anchor_on IS NULL AND end_on IS NULL
            AND one_off_on IS NULL
            AND goal_start_on IS NOT NULL AND goal_due_on IS NOT NULL
            AND goal_completed IS NOT NULL)
    )
) STRICT;

CREATE INDEX planned_items_account_hint_id ON planned_items (account_hint_id);

INSERT INTO planned_items (id, name, account_hint_id, notes, active, item_type, every, unit,
    anchor_on, end_on, one_off_on, goal_start_on, goal_due_on, goal_completed, created_at,
    updated_at)
SELECT id, name, account_hint_id, notes, active, item_type, every, unit,
    anchor_on, end_on, one_off_on, goal_start_on, goal_due_on, goal_completed, created_at,
    updated_at
FROM planned_items_held;

INSERT INTO planned_item_settlements SELECT * FROM planned_item_settlements_held;

-- Append-only: a row is never updated or deleted but with its item. The version in force in
-- a month is the row with the highest seq among those effective by then (the first row when
-- none is). `direction` is versioned because `spending` and `tier` depend on it. That a goal
-- saves toward and that only a recurring outflow has a spending kind depend on the item's
-- type, which a CHECK here can't see: triggers below join planned_items for them.
CREATE TABLE planned_item_versions (
    item_id TEXT NOT NULL REFERENCES planned_items (id) ON DELETE CASCADE,
    seq INTEGER NOT NULL CHECK (seq >= 1),
    effective_from TEXT NOT NULL CHECK (effective_from GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]'),
    written_on TEXT NOT NULL
        CHECK (written_on GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
    amount_minor INTEGER NOT NULL CHECK (amount_minor BETWEEN 1 AND 1000000000000000),
    currency TEXT NOT NULL CHECK (length(currency) = 3),
    direction TEXT NOT NULL CHECK (direction IN ('outflow', 'inflow')),
    category_id TEXT NOT NULL REFERENCES categories (id),
    spending TEXT CHECK (spending IN ('bill', 'budget')),
    tier TEXT CHECK (tier IN ('must_pay', 'essentials', 'flexible', 'saving_toward')),
    PRIMARY KEY (item_id, seq),
    CHECK ((direction = 'outflow') = (tier IS NOT NULL)),
    CHECK (direction = 'outflow' OR spending IS NULL)
) STRICT, WITHOUT ROWID;

CREATE INDEX planned_item_versions_category_id ON planned_item_versions (category_id);

INSERT INTO planned_item_versions (item_id, seq, effective_from, written_on, amount_minor,
    currency, direction, category_id, spending, tier)
SELECT id, 1,
    substr(CASE item_type
        WHEN 'recurring' THEN anchor_on
        WHEN 'one_off' THEN one_off_on
        ELSE goal_start_on
    END, 1, 7),
    date(created_at / 1000, 'unixepoch'),
    amount_minor, currency, direction, category_id, spending,
    CASE WHEN direction = 'outflow' THEN COALESCE(tier, CASE
        WHEN item_type = 'goal' THEN 'saving_toward'
        WHEN spending = 'budget' THEN 'flexible'
        ELSE 'must_pay'
    END) END
FROM planned_items_held;

-- What the CHECKs of planned_items said of these columns while they lived there. They run on
-- insert only, as the table is append-only.
CREATE TRIGGER planned_item_versions_spending_fits BEFORE INSERT ON planned_item_versions
WHEN EXISTS (
    SELECT 1 FROM planned_items AS item
    WHERE item.id = NEW.item_id
        AND (NEW.spending IS NOT NULL)
            <> (item.item_type = 'recurring' AND NEW.direction = 'outflow')
)
BEGIN
    SELECT RAISE(ABORT, 'only a recurring outflow has a spending kind, and it always does');
END;

CREATE TRIGGER planned_item_versions_goal_is_outflow BEFORE INSERT ON planned_item_versions
WHEN NEW.direction <> 'outflow'
    AND EXISTS (
        SELECT 1 FROM planned_items AS item
        WHERE item.id = NEW.item_id AND item.item_type = 'goal'
    )
BEGIN
    SELECT RAISE(ABORT, 'a goal is always an outflow');
END;

CREATE TRIGGER planned_item_versions_saving_toward_is_goal BEFORE INSERT ON planned_item_versions
WHEN NEW.tier IS NOT NULL
    AND EXISTS (
        SELECT 1 FROM planned_items AS item
        WHERE item.id = NEW.item_id
            AND (NEW.tier = 'saving_toward') <> (item.item_type = 'goal')
    )
BEGIN
    SELECT RAISE(ABORT, 'the saving_toward tier is a goal''s, and only a goal''s');
END;

CREATE TRIGGER planned_item_versions_no_update BEFORE UPDATE ON planned_item_versions
BEGIN
    SELECT RAISE(ABORT, 'planned_item_versions is append-only');
END;

-- A version goes with its item (ON DELETE CASCADE runs this after the item row is gone) and
-- no other way.
CREATE TRIGGER planned_item_versions_no_delete BEFORE DELETE ON planned_item_versions
WHEN EXISTS (SELECT 1 FROM planned_items WHERE id = OLD.item_id)
BEGIN
    SELECT RAISE(ABORT, 'planned_item_versions is append-only');
END;

DROP TABLE planned_item_settlements_held;
DROP TABLE planned_items_held;
