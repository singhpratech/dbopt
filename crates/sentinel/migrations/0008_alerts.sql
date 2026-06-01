-- Fired-alert log. Sentinel was purely passive; this turns it active. After a
-- vitals/waits poll persists, the threshold engine (src/alerts.rs) evaluates the
-- configured rules against the just-captured values and, on a breach, writes one
-- row here. De-dup is handled in the storage layer: an already-firing rule only
-- (re)fires when its breach state changes or after the configured cooldown, so a
-- standing condition can't spam a row every tick.
--
-- Keyed like every other time-series table off (instance_id, fired_at) with
-- fired_at as unix epoch millis. `notified` records whether the webhook POST
-- succeeded so the UI can show delivery status honestly (true / false / no
-- webhook configured).
CREATE TABLE IF NOT EXISTS alerts_fired (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id  INTEGER NOT NULL REFERENCES instances(id),
    fired_at     INTEGER NOT NULL,
    rule_id      TEXT    NOT NULL,
    metric       TEXT    NOT NULL,
    value        REAL    NOT NULL,
    threshold    REAL    NOT NULL,
    severity     TEXT    NOT NULL,
    message      TEXT    NOT NULL,
    notified     INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_alerts_fired_inst_time
    ON alerts_fired(instance_id, fired_at);
-- De-dup lookup: "is this rule already firing for this instance, and when did it
-- last fire" keys on (instance_id, rule_id) ordered by fired_at.
CREATE INDEX IF NOT EXISTS idx_alerts_fired_inst_rule
    ON alerts_fired(instance_id, rule_id, fired_at);
