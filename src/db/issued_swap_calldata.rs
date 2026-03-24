use crate::db::DbPool;
use crate::types::swap::{SwapCalldataRequest, SwapCalldataResponse};
use alloy::hex::encode_prefixed;
use alloy::primitives::keccak256;

pub(crate) async fn insert(
    pool: &DbPool,
    api_key_id: i64,
    chain_id: u32,
    request: &SwapCalldataRequest,
    response: &SwapCalldataResponse,
) -> Result<(), sqlx::Error> {
    let calldata = encode_prefixed(response.data.as_ref());
    let calldata_hash = encode_prefixed(keccak256(response.data.as_ref()));

    sqlx::query(
        "INSERT INTO issued_swap_calldata (
            api_key_id,
            chain_id,
            taker,
            to_address,
            tx_value,
            calldata,
            calldata_hash,
            input_token,
            output_token,
            output_amount,
            maximum_io_ratio,
            estimated_input
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(api_key_id)
    .bind(chain_id as i64)
    .bind(format!("{:#x}", request.taker))
    .bind(format!("{:#x}", response.to))
    .bind(response.value.to_string())
    .bind(calldata)
    .bind(calldata_hash)
    .bind(format!("{:#x}", request.input_token))
    .bind(format!("{:#x}", request.output_token))
    .bind(&request.output_amount)
    .bind(&request.maximum_io_ratio)
    .bind(&response.estimated_input)
    .execute(pool)
    .await?;

    Ok(())
}
