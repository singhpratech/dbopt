-- 0003_logs.sql · durable AI + analysis logs
-- Survives backend restarts and browser cache clears. Indexed for fast recent-N
-- retrieval and per-server filtering.

CREATE TABLE IF NOT EXISTS ai_interactions (
    id              TEXT    PRIMARY KEY,    -- client UUID, idempotent on resend
    occurred_at     TEXT    NOT NULL,       -- ISO8601 UTC
    provider        TEXT    NOT NULL,       -- ollama / web-llm / anthropic / openai / openrouter / azure / bedrock
    model           TEXT    NOT NULL,
    system_prompt   TEXT,                   -- nullable
    user_prompt     TEXT    NOT NULL,
    response        TEXT    NOT NULL,
    status          TEXT    NOT NULL,       -- streaming / ok / error / cancelled
    error_message   TEXT,
    latency_ms      INTEGER,
    tokens_in       INTEGER,
    tokens_out      INTEGER
);
CREATE INDEX IF NOT EXISTS ix_ai_int_occurred ON ai_interactions(occurred_at DESC);
CREATE INDEX IF NOT EXISTS ix_ai_int_provider ON ai_interactions(provider, occurred_at DESC);

CREATE TABLE IF NOT EXISTS analysis_runs (
    id              TEXT    PRIMARY KEY,    -- client UUID
    occurred_at     TEXT    NOT NULL,
    server_name     TEXT,                   -- e.g. "app-sql-01"; null for offline static-only
    database_name   TEXT,                   -- e.g. "sales"; null when none picked
    mode            TEXT    NOT NULL,       -- "adhoc" | "database_scan"
    sql_hash        TEXT,                   -- sha256 first 16 chars; lets us dedup re-analyses
    sql_preview     TEXT,                   -- first 500 chars of the SQL
    server_version  INTEGER,                -- the version flag fed to the analyzer
    findings_total  INTEGER NOT NULL,
    findings_critical INTEGER NOT NULL DEFAULT 0,
    findings_error    INTEGER NOT NULL DEFAULT 0,
    findings_warning  INTEGER NOT NULL DEFAULT 0,
    findings_info     INTEGER NOT NULL DEFAULT 0,
    plan_attached   INTEGER NOT NULL DEFAULT 0,  -- bool: was a plan XML included
    plan_subtree_cost REAL,                 -- estimated cost if available
    plan_op_count   INTEGER,
    duration_ms     INTEGER                 -- wall-clock for the analyze call
);
CREATE INDEX IF NOT EXISTS ix_runs_occurred ON analysis_runs(occurred_at DESC);
CREATE INDEX IF NOT EXISTS ix_runs_server_db ON analysis_runs(server_name, database_name, occurred_at DESC);

CREATE TABLE IF NOT EXISTS analysis_findings (
    run_id          TEXT    NOT NULL REFERENCES analysis_runs(id) ON DELETE CASCADE,
    rule_id         TEXT    NOT NULL,
    severity        TEXT    NOT NULL,
    line_no         INTEGER,
    col_no          INTEGER,
    message         TEXT    NOT NULL,
    recommendation  TEXT
);
CREATE INDEX IF NOT EXISTS ix_findings_run  ON analysis_findings(run_id);
CREATE INDEX IF NOT EXISTS ix_findings_rule ON analysis_findings(rule_id);
