-- A split may count towards a goal (spec §8.9 R8). The reference stops a goal
-- with linked splits from being deleted, which R17 turns into a 409.
ALTER TABLE transaction_splits ADD COLUMN goal_id TEXT REFERENCES planned_items (id);

CREATE INDEX transaction_splits_goal_id ON transaction_splits (goal_id) WHERE goal_id IS NOT NULL;
