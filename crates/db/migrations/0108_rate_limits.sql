-- Rate-limit counters shared by every web server when there's no Valkey
-- (cache.backend = "memory"), so several servers don't each grant their
-- own allowance. Each key keeps the GCRA "theoretical arrival time";
-- losing them in a crash only resets the limits, hence unlogged.
CREATE UNLOGGED TABLE rate_limits (
    key text PRIMARY KEY,
    tat timestamptz NOT NULL
);
