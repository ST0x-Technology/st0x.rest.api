CREATE INDEX IF NOT EXISTS idx_wrapped_exchange_rate_snapshots_share_block_timestamp
    ON wrapped_exchange_rate_snapshots (
        chain_id,
        share_token_address,
        block_timestamp DESC,
        block_number DESC,
        captured_at DESC
    );
