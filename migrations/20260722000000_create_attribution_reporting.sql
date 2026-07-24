CREATE TABLE IF NOT EXISTS attributed_trades (
    chain_id          INTEGER NOT NULL,
    raindex_address   TEXT    NOT NULL,
    indexed_trade_id  TEXT    NOT NULL,
    transaction_hash  TEXT    NOT NULL,
    log_index         INTEGER NOT NULL,
    block_number      INTEGER NOT NULL,
    block_timestamp   INTEGER NOT NULL,
    order_hash        TEXT    NOT NULL,
    taker             TEXT    NOT NULL,
    api_key_hash      TEXT    NOT NULL,
    api_key_database_id INTEGER,
    api_key_id        TEXT,
    api_key_label     TEXT,
    api_key_owner     TEXT,
    input_token       TEXT    NOT NULL,
    input_amount      TEXT    NOT NULL,
    output_token      TEXT    NOT NULL,
    output_amount     TEXT    NOT NULL,
    created_at        TEXT    NOT NULL DEFAULT (datetime('now')),
    updated_at        TEXT    NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (chain_id, raindex_address, indexed_trade_id)
);

CREATE INDEX idx_attributed_trades_block
    ON attributed_trades (block_number DESC, log_index DESC, indexed_trade_id DESC);
CREATE INDEX idx_attributed_trades_api_key_hash
    ON attributed_trades (
        api_key_hash,
        block_number DESC,
        log_index DESC,
        indexed_trade_id DESC
    );
CREATE INDEX idx_attributed_trades_transaction_hash
    ON attributed_trades (transaction_hash);

CREATE TABLE IF NOT EXISTS attribution_sync_cursors (
    chain_id          INTEGER NOT NULL,
    raindex_address   TEXT    NOT NULL,
    start_block       INTEGER NOT NULL,
    last_block        INTEGER NOT NULL,
    last_log_index    INTEGER NOT NULL,
    last_trade_id     TEXT    NOT NULL,
    updated_at        TEXT    NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (chain_id, raindex_address)
);

CREATE TABLE IF NOT EXISTS attribution_signers (
    address       TEXT PRIMARY KEY,
    first_seen_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS attribution_api_keys (
    api_key_hash        TEXT PRIMARY KEY,
    api_key_database_id INTEGER,
    api_key_id          TEXT NOT NULL,
    api_key_label       TEXT NOT NULL,
    api_key_owner       TEXT NOT NULL,
    created_at          TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at          TEXT NOT NULL DEFAULT (datetime('now'))
);
