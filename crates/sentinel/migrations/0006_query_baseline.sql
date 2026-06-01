-- Durable rolling performance baseline, one row per (instance_id, query_id).
--
-- The original z-score regression detector only ever looked inside the current
-- query window, so a restart (or a window that didn't reach back far enough)
-- erased all "what is normal for this query" history. This table holds a
-- running mean + variance per query that is UPDATED every poll and PERSISTED
-- across windows and process restarts, giving a stable baseline to judge the
-- latest sample against.
--
-- We keep Welford's online accumulators so we never have to re-read the whole
-- history to update: `count` samples, running `mean` (duration-per-execution,
-- ms), and `m2` (the sum of squared deviations from the mean). Sample variance
-- is `m2 / (count - 1)`; stddev is its square root. `last_value_ms` is the most
-- recently observed sample, `last_updated_ms` powers stale-baseline pruning.
CREATE TABLE IF NOT EXISTS query_baseline (
    instance_id     INTEGER NOT NULL REFERENCES instances(id),
    query_id        INTEGER NOT NULL,
    count           INTEGER NOT NULL DEFAULT 0,
    mean            REAL    NOT NULL DEFAULT 0,
    m2              REAL    NOT NULL DEFAULT 0,
    last_value_ms   REAL    NOT NULL DEFAULT 0,
    last_updated_ms INTEGER NOT NULL,
    PRIMARY KEY (instance_id, query_id)
);
CREATE INDEX IF NOT EXISTS idx_qb_updated
    ON query_baseline(last_updated_ms);
