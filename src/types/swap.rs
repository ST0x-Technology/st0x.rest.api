use crate::types::common::Approval;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SwapQuoteRequest {
    #[schema(example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub input_token: String,
    #[schema(example = "0x4200000000000000000000000000000000000006")]
    pub output_token: String,
    #[schema(example = "1000000")]
    pub output_amount: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SwapQuoteResponse {
    #[schema(example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub input_token: String,
    #[schema(example = "0x4200000000000000000000000000000000000006")]
    pub output_token: String,
    #[schema(example = "1000000")]
    pub output_amount: String,
    #[schema(example = "500000000000000")]
    pub estimated_input: String,
    #[schema(example = "0.0005")]
    pub estimated_io_ratio: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SwapCalldataRequest {
    #[schema(example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub input_token: String,
    #[schema(example = "0x4200000000000000000000000000000000000006")]
    pub output_token: String,
    #[schema(example = "1000000")]
    pub output_amount: String,
    #[schema(example = "0.0006")]
    pub maximum_io_ratio: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SwapCalldataResponse {
    #[schema(example = "0xDEF171Fe48CF0115B1d80b88dc8eAB59176FEe57")]
    pub to: String,
    #[schema(example = "0xabcdef...")]
    pub data: String,
    #[schema(example = "0")]
    pub value: String,
    #[schema(example = "500000000000000")]
    pub estimated_input: String,
    pub approvals: Vec<Approval>,
}
