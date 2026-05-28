-- 0004_query_text.sql · capture the SQL text behind Query Store rows
-- Before this, query_store_snapshot stored only an opaque query_id/plan_id, so
-- the dashboard could show "query 42 ran 14×" but never *what* query 42 was.
-- These columns are nullable so rows captured by older builds stay valid.

ALTER TABLE query_store_snapshot ADD COLUMN query_sql_text   TEXT;
ALTER TABLE query_store_snapshot ADD COLUMN last_execution_ms INTEGER;
