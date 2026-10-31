-- The shape of audit_events.changes_json. Version 1 (0.1.0 and the design
-- system) is free-form JSON, often {"from", "to"}; version 2 is
-- {field: {"before", "after"}}. Old rows are converted on read and never
-- rewritten: an ALTER fires none of the append-only triggers.
ALTER TABLE audit_events ADD COLUMN changes_version INTEGER NOT NULL DEFAULT 1
    CHECK (changes_version IN (1, 2));
