CREATE TABLE IF NOT EXISTS issued_swap_calldata (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    api_key_id       INTEGER NOT NULL,
    chain_id         INTEGER NOT NULL,
    taker            TEXT    NOT NULL,
    to_address       TEXT    NOT NULL,
    tx_value         TEXT    NOT NULL,
    calldata         TEXT    NOT NULL,
    calldata_hash    TEXT    NOT NULL,
    input_token      TEXT    NOT NULL,
    output_token     TEXT    NOT NULL,
    output_amount    TEXT    NOT NULL,
    maximum_io_ratio TEXT    NOT NULL,
    estimated_input  TEXT    NOT NULL,
    created_at       TEXT    NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (api_key_id) REFERENCES api_keys(id) ON DELETE CASCADE
);

CREATE INDEX idx_issued_swap_calldata_api_key_id_created
    ON issued_swap_calldata (api_key_id, created_at);
CREATE INDEX idx_issued_swap_calldata_calldata_hash_created
    ON issued_swap_calldata (calldata_hash, created_at);
CREATE INDEX idx_issued_swap_calldata_taker_created
    ON issued_swap_calldata (taker, created_at);
