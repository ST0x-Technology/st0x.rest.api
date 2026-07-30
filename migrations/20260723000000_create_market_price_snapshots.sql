CREATE TABLE IF NOT EXISTS market_price_snapshots (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    chain_id INTEGER NOT NULL,
    asset_token_address TEXT NOT NULL,
    quote_token_address TEXT NOT NULL,
    best_bid TEXT NOT NULL,
    best_ask TEXT NOT NULL,
    midpoint TEXT NOT NULL,
    assets_per_share TEXT NOT NULL,
    observed_at INTEGER NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (chain_id, asset_token_address, quote_token_address, observed_at)
);

CREATE INDEX IF NOT EXISTS idx_market_price_snapshots_observed_at
    ON market_price_snapshots (observed_at);

CREATE INDEX IF NOT EXISTS idx_market_price_snapshots_market_observed_at
    ON market_price_snapshots (
        chain_id,
        quote_token_address,
        observed_at DESC,
        asset_token_address
    );
