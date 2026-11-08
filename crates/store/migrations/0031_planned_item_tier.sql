-- A planned outflow's tier (money redesign §4.1). SQLite can't add a CHECK across columns to
-- a table that has rows, and rebuilding planned_items would cascade-delete its settlements, so
-- this CHECK allows NULL and the store refuses an outflow read without a tier.
ALTER TABLE planned_items ADD COLUMN tier TEXT
    CHECK (tier IN ('must_pay', 'essentials', 'flexible', 'saving_toward'))
    CHECK (tier IS NULL
        OR (direction = 'outflow' AND (tier = 'saving_toward') = (item_type = 'goal')));

-- Every outflow takes the default for its kind: a goal saves toward, a budget is flexible,
-- and a bill or a one-off is must pay. Income keeps none.
UPDATE planned_items SET tier = CASE
    WHEN item_type = 'goal' THEN 'saving_toward'
    WHEN spending = 'budget' THEN 'flexible'
    ELSE 'must_pay'
END
WHERE direction = 'outflow';
