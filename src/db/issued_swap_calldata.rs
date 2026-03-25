use crate::db::DbPool;

pub(crate) struct NewIssuedSwapCalldata {
    pub api_key_id: i64,
    pub key_id: String,
    pub label: String,
    pub owner: String,
    pub chain_id: i64,
    pub taker: String,
    pub to_address: String,
    pub tx_value: String,
    pub calldata: String,
    pub calldata_hash: String,
    pub input_token: String,
    pub output_token: String,
    pub output_amount: String,
    pub maximum_io_ratio: String,
    pub estimated_input: String,
}

pub(crate) async fn insert(
    pool: &DbPool,
    record: &NewIssuedSwapCalldata,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO issued_swap_calldata (
            api_key_id,
            key_id,
            label,
            owner,
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
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(record.api_key_id)
    .bind(&record.key_id)
    .bind(&record.label)
    .bind(&record.owner)
    .bind(record.chain_id)
    .bind(&record.taker)
    .bind(&record.to_address)
    .bind(&record.tx_value)
    .bind(&record.calldata)
    .bind(&record.calldata_hash)
    .bind(&record.input_token)
    .bind(&record.output_token)
    .bind(&record.output_amount)
    .bind(&record.maximum_io_ratio)
    .bind(&record.estimated_input)
    .execute(pool)
    .await?;

    Ok(())
}
