CREATE TABLE IF NOT EXISTS wrapped_exchange_rate_snapshots (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    token_address     TEXT    NOT NULL,
    block_number      INTEGER NOT NULL,
    block_timestamp   INTEGER NOT NULL,
    assets_per_share  TEXT    NOT NULL,
    asset_address     TEXT    NOT NULL DEFAULT '',
    asset_symbol      TEXT    NOT NULL DEFAULT '',
    asset_decimals    INTEGER NOT NULL DEFAULT 0,
    captured_at       TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_wrapped_rate_snapshots_token_block
ON wrapped_exchange_rate_snapshots (token_address, block_number);

CREATE INDEX idx_wrapped_rate_snapshots_token_captured
ON wrapped_exchange_rate_snapshots (token_address, captured_at);
