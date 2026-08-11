WITH batch AS (
    SELECT
        chain_id,
        raindex_address,
        trade_id,
        transaction_hash,
        log_index,
        block_number,
        block_timestamp,
        transaction_sender,
        order_hash,
        input_token,
        input_delta,
        output_token,
        output_delta
    FROM derived_trades
    WHERE trade_kind = 'take'
      AND chain_id = ?
      AND raindex_address = ?
      AND block_number >= ?
      AND block_number <= ?
      AND (
          ? = 0
          OR block_number > ?
          OR (block_number = ? AND log_index > ?)
          OR (block_number = ? AND log_index = ? AND trade_id > ?)
      )
    ORDER BY block_number, log_index, trade_id
    LIMIT ?
)
SELECT
    b.*,
    c.context_index,
    c.context_value,
    MAX(CASE WHEN v.value_index = 0 THEN v.value END) AS value_0,
    MAX(CASE WHEN v.value_index = 1 THEN v.value END) AS value_1,
    MAX(CASE WHEN v.value_index = 2 THEN v.value END) AS value_2,
    MAX(CASE WHEN v.value_index = 3 THEN v.value END) AS value_3,
    COUNT(v.value_index) AS value_count
FROM batch b
LEFT JOIN take_order_contexts c
  ON c.chain_id = b.chain_id
 AND c.raindex_address = b.raindex_address
 AND c.transaction_hash = b.transaction_hash
 AND c.log_index = b.log_index
LEFT JOIN context_values v
  ON v.chain_id = c.chain_id
 AND v.raindex_address = c.raindex_address
 AND v.transaction_hash = c.transaction_hash
 AND v.log_index = c.log_index
 AND v.context_index = c.context_index
GROUP BY
    b.chain_id,
    b.raindex_address,
    b.trade_id,
    b.transaction_hash,
    b.log_index,
    b.block_number,
    b.block_timestamp,
    b.transaction_sender,
    b.order_hash,
    b.input_token,
    b.input_delta,
    b.output_token,
    b.output_delta,
    c.context_index,
    c.context_value
ORDER BY b.block_number, b.log_index, b.trade_id, c.context_index
