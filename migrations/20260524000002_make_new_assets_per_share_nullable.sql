-- Make `wrapped_donation_events.new_assets_per_share` nullable so the
-- scanner can record a donation row even when `convertToAssets` at the
-- donation's historical block can't be read (e.g. the RPC has pruned that
-- block, or a transient network error). Without this, a failed rate fetch
-- meant the donation was permanently lost: the scanner skipped the row
-- but the cursor advanced past its block.
--
-- The route layer lazily backfills any rows with a NULL rate on the next
-- history request, capped at 25 attempts per request to keep latency sane.
-- Until backfill succeeds, the API serialises the field as JSON `null`.
--
-- SQLite can't ALTER a NOT NULL constraint in place, so we use the same
-- rename-create-copy-drop dance as the previous unique-index migration.
PRAGMA foreign_keys = OFF;

CREATE TABLE wrapped_donation_events_new (
    id                     INTEGER PRIMARY KEY AUTOINCREMENT,
    wrapper_address        TEXT    NOT NULL,
    donor_address          TEXT    NOT NULL,
    asset_amount           TEXT    NOT NULL,
    block_number           INTEGER NOT NULL,
    block_timestamp        INTEGER NOT NULL,
    tx_hash                TEXT    NOT NULL,
    new_assets_per_share   TEXT,
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
