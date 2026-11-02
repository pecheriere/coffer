-- The spending plan's items (spec §6.7, §8.9). Each row's schedule columns are
-- exactly those of its type, and the amount is a positive magnitude with a
-- direction. An item's type never changes. A recurring outflow, and nothing
-- else, says whether it is a bill or a budget.
CREATE TABLE planned_items (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    category_id TEXT NOT NULL REFERENCES categories (id),
    amount_minor INTEGER NOT NULL CHECK (amount_minor BETWEEN 1 AND 1000000000000000),
    currency TEXT NOT NULL CHECK (length(currency) = 3),
    direction TEXT NOT NULL CHECK (direction IN ('outflow', 'inflow')),
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
    spending TEXT CHECK (spending IN ('bill', 'budget')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK ((item_type = 'recurring' AND direction = 'outflow') = (spending IS NOT NULL)),
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
            AND goal_completed IS NOT NULL
            AND direction = 'outflow')
    )
) STRICT;

CREATE INDEX planned_items_category_id ON planned_items (category_id);
CREATE INDEX planned_items_account_hint_id ON planned_items (account_hint_id);
