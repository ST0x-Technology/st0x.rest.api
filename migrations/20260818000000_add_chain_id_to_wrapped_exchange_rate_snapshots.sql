ALTER TABLE wrapped_exchange_rate_snapshots
    RENAME TO wrapped_exchange_rate_snapshots_legacy;

CREATE TABLE wrapped_exchange_rate_snapshots (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    chain_id INTEGER NOT NULL,
    share_token_address TEXT NOT NULL,
    asset_token_address TEXT NOT NULL,
    assets_per_share TEXT NOT NULL,
    block_number INTEGER NOT NULL,
    block_timestamp INTEGER,
    captured_at TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (chain_id, share_token_address, block_number)
);

-- Snapshots written before multi-network support came from the Base-only service.
INSERT INTO wrapped_exchange_rate_snapshots (
    id,
    chain_id,
    share_token_address,
    asset_token_address,
    assets_per_share,
    block_number,
    block_timestamp,
    captured_at,
    created_at
)
SELECT
    id,
    8453,
    share_token_address,
    asset_token_address,
    assets_per_share,
    block_number,
    block_timestamp,
    captured_at,
    created_at
FROM wrapped_exchange_rate_snapshots_legacy;

DROP TABLE wrapped_exchange_rate_snapshots_legacy;

CREATE INDEX idx_wrapped_exchange_rate_snapshots_share_captured_at
    ON wrapped_exchange_rate_snapshots (chain_id, share_token_address, captured_at);

CREATE INDEX idx_wrapped_exchange_rate_snapshots_share_block_number
    ON wrapped_exchange_rate_snapshots (chain_id, share_token_address, block_number);

CREATE INDEX idx_wrapped_exchange_rate_snapshots_asset_captured_at
    ON wrapped_exchange_rate_snapshots (chain_id, asset_token_address, captured_at);
