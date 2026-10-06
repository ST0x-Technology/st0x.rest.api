CREATE TABLE market_books (
    chain_id INTEGER NOT NULL,
    asset_token_address TEXT NOT NULL,
    quote_token_address TEXT NOT NULL,
    best_bid TEXT,
    best_ask TEXT,
    assets_per_share TEXT NOT NULL,
    observed_at INTEGER NOT NULL,
    PRIMARY KEY (chain_id, asset_token_address, quote_token_address)
);
