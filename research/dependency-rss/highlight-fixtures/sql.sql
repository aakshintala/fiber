-- Sessions over budget: total tool wall time per session, slowest first.
CREATE TABLE IF NOT EXISTS tool_call (
    session_id TEXT NOT NULL,
    tool TEXT NOT NULL,
    ms INTEGER NOT NULL CHECK (ms >= 0),
    ok INTEGER NOT NULL DEFAULT 1,
    ts INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_tool_call_session ON tool_call (session_id);

WITH totals AS (
    SELECT
        session_id,
        COUNT(*) AS calls,
        SUM(ms) / 1000.0 AS secs,
        SUM(CASE WHEN ok = 0 THEN 1 ELSE 0 END) AS errors
    FROM tool_call
    WHERE ts >= strftime('%s', 'now', '-7 days')
    GROUP BY session_id
)
SELECT session_id, calls, ROUND(secs, 1) AS secs, errors
FROM totals
WHERE secs > 60
ORDER BY secs DESC
LIMIT 20;

-- Prune raw rows older than the 90-day retention window.
DELETE FROM tool_call
WHERE ts < strftime('%s', 'now', '-90 days');

SELECT COUNT(*) AS remaining FROM tool_call;
VACUUM;

-- Biggest single calls this week, for the slow-call drill-down.
SELECT session_id, tool, ms, datetime(ts, 'unixepoch') AS at
FROM tool_call
WHERE ts >= strftime('%s', 'now', '-7 days')
ORDER BY ms DESC
LIMIT 10;
