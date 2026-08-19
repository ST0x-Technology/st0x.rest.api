CREATE TABLE IF NOT EXISTS market_price_sampler_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    last_completed_sample_at INTEGER NOT NULL
);
