CREATE TABLE IF NOT EXISTS attributed_swap_txs (
    tx_hash                  TEXT    PRIMARY KEY,
    issued_swap_calldata_id  INTEGER NOT NULL,
    api_key_id               INTEGER NOT NULL,
    chain_id                 INTEGER NOT NULL,
    sender                   TEXT    NOT NULL,
    to_address               TEXT    NOT NULL,
    block_number             INTEGER NOT NULL,
    block_timestamp          INTEGER NOT NULL,
    input_token              TEXT    NOT NULL,
    output_token             TEXT    NOT NULL,
    total_input_amount       TEXT    NOT NULL,
    total_output_amount      TEXT    NOT NULL,
    matched_at               TEXT    NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (issued_swap_calldata_id) REFERENCES issued_swap_calldata(id) ON DELETE CASCADE,
    FOREIGN KEY (api_key_id) REFERENCES api_keys(id) ON DELETE CASCADE
);

CREATE INDEX idx_attributed_swap_txs_api_key_id_timestamp
    ON attributed_swap_txs (api_key_id, block_timestamp);
CREATE INDEX idx_attributed_swap_txs_chain_id_timestamp
    ON attributed_swap_txs (chain_id, block_timestamp);

CREATE TABLE IF NOT EXISTS swap_report_runs (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    report_name      TEXT    NOT NULL,
    start_time       INTEGER NOT NULL,
    end_time         INTEGER NOT NULL,
    matched_count    INTEGER NOT NULL,
    unmatched_count  INTEGER NOT NULL,
    ambiguous_count  INTEGER NOT NULL,
    completed_at     TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_swap_report_runs_name_completed
    ON swap_report_runs (report_name, completed_at);
