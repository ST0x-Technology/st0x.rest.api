CREATE TABLE IF NOT EXISTS private_registry_history (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    source_commit     TEXT NOT NULL,
    payload_sha256    TEXT NOT NULL,
    actor_key_id      TEXT NOT NULL,
    actor_label       TEXT NOT NULL DEFAULT '',
    actor_owner       TEXT NOT NULL DEFAULT '',
    validation_status TEXT NOT NULL CHECK (validation_status IN ('success', 'failed')),
    validation_error  TEXT,
    changed_at        TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_private_registry_history_changed_at
ON private_registry_history (changed_at);

CREATE INDEX idx_private_registry_history_success
ON private_registry_history (validation_status, changed_at);
