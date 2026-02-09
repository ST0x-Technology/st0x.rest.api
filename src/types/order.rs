use crate::types::common::{Approval, TokenRef};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PeriodUnit {
    Days,
    Hours,
    Minutes,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeployDcaOrderRequest {
    #[schema(example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub input_token: String,
    #[schema(example = "0x4200000000000000000000000000000000000006")]
    pub output_token: String,
    #[schema(example = "1000000")]
    pub budget_amount: String,
    #[schema(example = 4)]
    pub period: u32,
    #[schema(example = "hours")]
    pub period_unit: PeriodUnit,
    #[schema(example = "0.0005")]
    pub start_io: String,
    #[schema(example = "0.0003")]
    pub floor_io: String,
    pub input_vault_id: Option<String>,
    pub output_vault_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploySolverOrderRequest {
    #[schema(example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub input_token: String,
    #[schema(example = "0x4200000000000000000000000000000000000006")]
    pub output_token: String,
    #[schema(example = "1000000")]
    pub amount: String,
    #[schema(example = "0.0005")]
    pub ioratio: String,
    pub input_vault_id: Option<String>,
    pub output_vault_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeployOrderResponse {
    #[schema(example = "0xDEF171Fe48CF0115B1d80b88dc8eAB59176FEe57")]
    pub to: String,
    #[schema(example = "0xabcdef...")]
    pub data: String,
    #[schema(example = "0")]
    pub value: String,
    pub approvals: Vec<Approval>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CancelOrderRequest {
    #[schema(example = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890ab")]
    pub order_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CancelTransaction {
    #[schema(example = "0xDEF171Fe48CF0115B1d80b88dc8eAB59176FEe57")]
    pub to: String,
    #[schema(example = "0xabcdef...")]
    pub data: String,
    #[schema(example = "0")]
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenReturn {
    #[schema(example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub token: String,
    #[schema(example = "USDC")]
    pub symbol: String,
    #[schema(example = "1000000")]
    pub amount: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CancelSummary {
    #[schema(example = 2)]
    pub vaults_to_withdraw: u32,
    pub tokens_returned: Vec<TokenReturn>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CancelOrderResponse {
    pub transactions: Vec<CancelTransaction>,
    pub summary: CancelSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum OrderType {
    Dca,
    Solver,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrderDetailsInfo {
    #[serde(rename = "type")]
    #[schema(example = "dca")]
    pub type_: OrderType,
    #[schema(example = "0.0005")]
    pub io_ratio: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrderTradeEntry {
    #[schema(example = "trade-1")]
    pub id: String,
    #[schema(example = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890ab")]
    pub tx_hash: String,
    #[schema(example = "1000000")]
    pub input_amount: String,
    #[schema(example = "500000")]
    pub output_amount: String,
    #[schema(example = 1718452800)]
    pub timestamp: u64,
    #[schema(example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub sender: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrderDetail {
    #[schema(example = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890ab")]
    pub order_hash: String,
    #[schema(example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub owner: String,
    pub order_details: OrderDetailsInfo,
    pub input_token: TokenRef,
    pub output_token: TokenRef,
    #[schema(example = "1")]
    pub input_vault_id: String,
    #[schema(example = "2")]
    pub output_vault_id: String,
    #[schema(example = "1000000")]
    pub input_vault_balance: String,
    #[schema(example = "500000")]
    pub output_vault_balance: String,
    #[schema(example = "0.0005")]
    pub io_ratio: String,
    #[schema(example = 1718452800)]
    pub created_at: u64,
    #[schema(example = "0xorderbook")]
    pub orderbook_id: String,
    pub trades: Vec<OrderTradeEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deploy_dca_order_request_serde() {
        let json = r#"{
            "inputToken": "0xabc",
            "outputToken": "0xdef",
            "budgetAmount": "1000000",
            "period": 4,
            "periodUnit": "hours",
            "startIo": "0.0005",
            "floorIo": "0.0003"
        }"#;
        let req: DeployDcaOrderRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.period_unit, PeriodUnit::Hours);
        assert_eq!(req.period, 4);
        assert_eq!(req.budget_amount, "1000000");
        assert!(req.input_vault_id.is_none());
        assert!(req.output_vault_id.is_none());
    }

    #[test]
    fn test_period_unit_variants() {
        let variants = [
            ("\"minutes\"", PeriodUnit::Minutes),
            ("\"hours\"", PeriodUnit::Hours),
            ("\"days\"", PeriodUnit::Days),
        ];
        for (json, expected) in variants {
            let parsed: PeriodUnit = serde_json::from_str(json).unwrap();
            assert_eq!(parsed, expected);
        }
    }

    #[test]
    fn test_period_unit_rejects_invalid() {
        let result = serde_json::from_str::<PeriodUnit>("\"seconds\"");
        assert!(result.is_err());
        let result = serde_json::from_str::<PeriodUnit>("\"weeks\"");
        assert!(result.is_err());
        let result = serde_json::from_str::<PeriodUnit>("\"months\"");
        assert!(result.is_err());
    }

    #[test]
    fn test_order_details_info_type_rename() {
        let info = OrderDetailsInfo {
            type_: OrderType::Dca,
            io_ratio: "0.0005".into(),
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"type\":\"dca\""));
        assert!(!json.contains("\"type_\""));
    }
}
