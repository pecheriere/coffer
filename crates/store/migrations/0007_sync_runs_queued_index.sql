-- The worker's lookup of the oldest queued run (`next_queued_run`): only
-- queued rows, in the order it reads them.
CREATE INDEX sync_runs_queued ON sync_runs (queued_at, id) WHERE status = 'queued';
