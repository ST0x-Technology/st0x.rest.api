CREATE TABLE IF NOT EXISTS registry_url_history (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    previous_url TEXT NOT NULL,
    new_url      TEXT NOT NULL,
    actor_key_id TEXT NOT NULL,
    actor_label  TEXT NOT NULL DEFAULT '',
    actor_owner  TEXT NOT NULL DEFAULT '',
    changed_at   TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_registry_url_history_changed_at
ON registry_url_history (changed_at);
