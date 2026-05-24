-- Donation events on the OARV side of an ERC4626 wrapper. A donation is a
-- direct asset transfer to the wrapper that bumps assets-per-share without
-- minting new shares. See `src/wrapped_donations.rs` for the detection
-- algorithm (Transfer-to-wrapper minus matched Deposit on the wrapper).
CREATE TABLE IF NOT EXISTS wrapped_donation_events (
    id                     INTEGER PRIMARY KEY AUTOINCREMENT,
    wrapper_address        TEXT    NOT NULL,
    donor_address          TEXT    NOT NULL,
    asset_amount           TEXT    NOT NULL,
    block_number           INTEGER NOT NULL,
    block_timestamp        INTEGER NOT NULL,
    tx_hash                TEXT    NOT NULL,
    new_assets_per_share   TEXT    NOT NULL,
    captured_at            TEXT    NOT NULL DEFAULT (datetime('now')),
    UNIQUE (wrapper_address, tx_hash, donor_address)
);

CREATE INDEX idx_wrapped_donation_events_wrapper_block
ON wrapped_donation_events (wrapper_address, block_number);

-- Per-wrapper scanner cursor — last OARV block whose Transfer logs have been
-- examined for this wrapper. Updated by the scanner, read by both the
-- scanner (resume point) and tests.
CREATE TABLE IF NOT EXISTS wrapped_donation_scan_state (
    wrapper_address     TEXT    PRIMARY KEY,
    last_scanned_block  INTEGER NOT NULL,
    updated_at          TEXT    NOT NULL DEFAULT (datetime('now'))
);
