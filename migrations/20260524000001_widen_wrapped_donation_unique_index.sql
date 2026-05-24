-- Widen the wrapped_donation_events uniqueness key so that a single transaction
-- carrying *multiple* OARV transfers from the same donor (rare, but possible)
-- can land all of them instead of collapsing to one row. The narrow
-- UNIQUE(wrapper_address, tx_hash, donor_address) collides on the second
-- transfer; the wider UNIQUE adds (asset_amount, block_number) so each
-- distinct transfer survives as its own donation row.
--
-- The scanner's idempotence guarantee (re-running the same block range
-- never duplicates rows) still holds: replays of an identical transfer hit
-- every column of the unique tuple and remain a no-op via INSERT OR IGNORE.

-- SQLite can't ALTER a UNIQUE constraint in place; rebuild the table via
-- the standard rename-create-copy-drop dance.
PRAGMA foreign_keys = OFF;

CREATE TABLE wrapped_donation_events_new (
    id                     INTEGER PRIMARY KEY AUTOINCREMENT,
    wrapper_address        TEXT    NOT NULL,
    donor_address          TEXT    NOT NULL,
    asset_amount           TEXT    NOT NULL,
    block_number           INTEGER NOT NULL,
    block_timestamp        INTEGER NOT NULL,
    tx_hash                TEXT    NOT NULL,
    new_assets_per_share   TEXT    NOT NULL,
    captured_at            TEXT    NOT NULL DEFAULT (datetime('now')),
    UNIQUE (wrapper_address, tx_hash, donor_address, asset_amount, block_number)
);

INSERT INTO wrapped_donation_events_new
    (id, wrapper_address, donor_address, asset_amount, block_number,
     block_timestamp, tx_hash, new_assets_per_share, captured_at)
SELECT id, wrapper_address, donor_address, asset_amount, block_number,
       block_timestamp, tx_hash, new_assets_per_share, captured_at
FROM wrapped_donation_events;

DROP TABLE wrapped_donation_events;
ALTER TABLE wrapped_donation_events_new RENAME TO wrapped_donation_events;

CREATE INDEX idx_wrapped_donation_events_wrapper_block
ON wrapped_donation_events (wrapper_address, block_number);

PRAGMA foreign_keys = ON;
