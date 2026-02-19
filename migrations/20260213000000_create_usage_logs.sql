CREATE TABLE IF NOT EXISTS usage_logs (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    api_key_id INTEGER NOT NULL,
    method     TEXT    NOT NULL,
    path       TEXT    NOT NULL,
    status_code INTEGER NOT NULL,
    latency_ms REAL    NOT NULL,
    created_at TEXT    NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (api_key_id) REFERENCES api_keys(id) ON DELETE CASCADE
);
CREATE INDEX idx_usage_logs_api_key_id_created ON usage_logs (api_key_id, created_at);
