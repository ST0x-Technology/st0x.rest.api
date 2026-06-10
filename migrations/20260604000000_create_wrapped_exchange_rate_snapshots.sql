CREATE TABLE IF NOT EXISTS wrapped_exchange_rate_snapshots (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    share_token_address TEXT NOT NULL,
    asset_token_address TEXT NOT NULL,
    assets_per_share TEXT NOT NULL,
    block_number INTEGER NOT NULL,
    block_timestamp INTEGER,
    captured_at TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (share_token_address, block_number)
);

CREATE INDEX IF NOT EXISTS idx_wrapped_exchange_rate_snapshots_share_captured_at
    ON wrapped_exchange_rate_snapshots (share_token_address, captured_at);

CREATE INDEX IF NOT EXISTS idx_wrapped_exchange_rate_snapshots_share_block_number
    ON wrapped_exchange_rate_snapshots (share_token_address, block_number);

CREATE INDEX IF NOT EXISTS idx_wrapped_exchange_rate_snapshots_asset_captured_at
    ON wrapped_exchange_rate_snapshots (asset_token_address, captured_at);
