use crate::error::ApiError;
use crate::types::common::Approval;
use crate::types::order::DeployOrderResponse;
use alloy::primitives::{Bytes, U256};
use alloy::sol_types::SolCall;
use rain_orderbook_bindings::IERC20::approveCall;
use rain_orderbook_js_api::gui::order_operations::DeploymentTransactionArgs;

fn decode_approval_amount(calldata: &Bytes) -> Result<String, ApiError> {
    let decoded = approveCall::abi_decode(calldata).map_err(|e| {
        tracing::error!(error = %e, "failed to decode approval calldata");
        ApiError::Internal("failed to decode approval calldata".into())
    })?;
    Ok(decoded.amount.to_string())
}

pub(super) fn map_deployment_to_response(
    args: DeploymentTransactionArgs,
) -> Result<DeployOrderResponse, ApiError> {
    let approvals = args
        .approvals
        .iter()
        .map(|a| {
            Ok(Approval {
                token: a.token,
                spender: args.orderbook_address,
                symbol: a.symbol.clone(),
                approval_data: a.calldata.clone(),
                amount: decode_approval_amount(&a.calldata)?,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;

    Ok(DeployOrderResponse {
        to: args.orderbook_address,
        data: args.deployment_calldata,
        value: U256::ZERO,
        approvals,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, Address};
    use rain_orderbook_js_api::gui::order_operations::ExtendedApprovalCalldata;

    const ORDERBOOK: Address = address!("1234567890abcdef1234567890abcdef12345678");
    const USDC: Address = address!("833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");

    fn mock_approve_calldata(spender: Address, amount: U256) -> Bytes {
        Bytes::from(approveCall { spender, amount }.abi_encode())
    }

    #[test]
    fn test_decode_approval_amount() {
        let amount = U256::from(1_000_000u64);
        let calldata = mock_approve_calldata(Address::ZERO, amount);
        let result = decode_approval_amount(&calldata).unwrap();
        assert_eq!(result, "1000000");
    }

    #[test]
    fn test_decode_approval_amount_large_value() {
        let amount = U256::from(999_999_999_999_999_999u128);
        let calldata = mock_approve_calldata(Address::ZERO, amount);
        let result = decode_approval_amount(&calldata).unwrap();
        assert_eq!(result, "999999999999999999");
    }

    #[test]
    fn test_decode_approval_amount_invalid_calldata() {
        let calldata = Bytes::from(vec![0xab, 0xcd]);
        let result = decode_approval_amount(&calldata);
        assert!(matches!(result, Err(ApiError::Internal(_))));
    }

    #[test]
    fn test_map_deployment_to_response_no_approvals() {
        let args = DeploymentTransactionArgs {
            approvals: vec![],
            deployment_calldata: Bytes::from(vec![0x01, 0x02]),
            orderbook_address: ORDERBOOK,
            chain_id: 8453,
            emit_meta_call: None,
        };
        let response = map_deployment_to_response(args).unwrap();
        assert_eq!(response.to, ORDERBOOK);
        assert_eq!(response.data, Bytes::from(vec![0x01, 0x02]));
        assert_eq!(response.value, U256::ZERO);
        assert!(response.approvals.is_empty());
    }

    #[test]
    fn test_map_deployment_to_response_with_approvals() {
        let amount = U256::from(500_000u64);
        let calldata = mock_approve_calldata(ORDERBOOK, amount);

        let args = DeploymentTransactionArgs {
            approvals: vec![ExtendedApprovalCalldata {
                token: USDC,
                calldata: calldata.clone(),
                symbol: "USDC".to_string(),
            }],
            deployment_calldata: Bytes::from(vec![0xaa]),
            orderbook_address: ORDERBOOK,
            chain_id: 8453,
            emit_meta_call: None,
        };
        let response = map_deployment_to_response(args).unwrap();
        assert_eq!(response.approvals.len(), 1);

        let approval = &response.approvals[0];
        assert_eq!(approval.token, USDC);
        assert_eq!(approval.spender, ORDERBOOK);
        assert_eq!(approval.symbol, "USDC");
        assert_eq!(approval.amount, "500000");
        assert_eq!(approval.approval_data, calldata);
    }
}
