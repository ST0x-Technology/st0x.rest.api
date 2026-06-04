//! Adapter that implements `rain_orderbook_common::take_orders::SignedContextInjector`
//! by producing a single gating `SignedContextV1` per order, signed by the
//! API's gating key.
//!
//! The injector is constructed per request: the authenticated key id defines
//! the attribution `id`, and the TTL config defines the `expiry`. The
//! counterparty (taker) comes from rain.orderbook's candidate-building loop
//! as the on-chain take-order counterparty.

use crate::auth::AuthenticatedKey;
use crate::signing::{compute_attribution_id, GatingSigner, GatingState, SignedGatingContext};
use alloy::primitives::{keccak256, Address, Bytes, B256};
use alloy::sol_types::SolValue;
use async_trait::async_trait;
use rain_orderbook_bindings::IRaindexV6::{OrderV4, SignedContextV1};
use rain_orderbook_common::take_orders::SignedContextInjector;

pub struct ApiGatingInjector<'a> {
    pub signer: &'a GatingSigner,
    pub expiry: u64,
    pub attribution_id: B256,
}

impl<'a> ApiGatingInjector<'a> {
    /// Build the request-scoped injector from rocket-managed gating state and
    /// the authenticated key id. Encapsulates expiry computation and
    /// `id = keccak256(key_id)` derivation so route handlers don't repeat it.
    pub fn for_request(state: &'a GatingState, key: &AuthenticatedKey) -> Self {
        Self {
            signer: &state.signer,
            expiry: state.expiry_from_now(),
            attribution_id: compute_attribution_id(&key.key_id),
        }
    }
}

#[async_trait]
impl SignedContextInjector for ApiGatingInjector<'_> {
    async fn contexts_for(
        &self,
        order: &OrderV4,
        _input_io_index: u32,
        _output_io_index: u32,
        counterparty: Address,
    ) -> Vec<SignedContextV1> {
        let order_hash = keccak256(order.abi_encode());
        match self
            .signer
            .sign_gating_context(counterparty, order_hash, self.expiry, self.attribution_id)
            .await
        {
            Ok(signed) => vec![to_signed_context_v1(signed)],
            Err(e) => {
                tracing::error!(
                    error = %e,
                    %counterparty,
                    %order_hash,
                    attribution_id = %self.attribution_id,
                    "gating signer failed to sign context; order will fail on-chain gating"
                );
                vec![]
            }
        }
    }
}

fn to_signed_context_v1(signed: SignedGatingContext) -> SignedContextV1 {
    SignedContextV1 {
        signer: signed.signer,
        context: signed.context,
        signature: Bytes::from(signed.signature),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signing::compute_attribution_id;
    use alloy::primitives::U256;
    use rain_orderbook_bindings::IRaindexV6::{EvaluableV4, IOV2};

    const TEST_KEY: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

    fn dummy_order() -> OrderV4 {
        OrderV4 {
            owner: Address::from([1u8; 20]),
            nonce: U256::from(1).into(),
            evaluable: EvaluableV4 {
                interpreter: Address::from([2u8; 20]),
                store: Address::from([3u8; 20]),
                bytecode: Bytes::from(vec![0x01, 0x02]),
            },
            validInputs: vec![IOV2 {
                token: Address::from([4u8; 20]),
                vaultId: U256::from(100).into(),
            }],
            validOutputs: vec![IOV2 {
                token: Address::from([5u8; 20]),
                vaultId: U256::from(200).into(),
            }],
        }
    }

    #[tokio::test]
    async fn test_produces_single_signed_context_with_derived_hash() {
        let signer = GatingSigner::from_hex_key(TEST_KEY).unwrap();
        let injector = ApiGatingInjector {
            signer: &signer,
            expiry: 1_700_000_000,
            attribution_id: compute_attribution_id("my-key-id"),
        };
        let order = dummy_order();
        let counterparty = Address::from([7u8; 20]);

        let contexts = injector.contexts_for(&order, 0, 0, counterparty).await;
        assert_eq!(contexts.len(), 1);
        let ctx = &contexts[0];
        assert_eq!(ctx.signer, signer.address());
        assert_eq!(ctx.context.len(), 4);
        assert_eq!(ctx.signature.len(), 65);

        // Row 1 must be the canonical order hash.
        let expected_hash = keccak256(order.abi_encode());
        assert_eq!(ctx.context[1], expected_hash);
    }
}
