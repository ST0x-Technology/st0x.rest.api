CREATE INDEX IF NOT EXISTS idx_attributed_trades_volume_group
    ON attributed_trades (api_key_hash, chain_id, input_token, output_token);
