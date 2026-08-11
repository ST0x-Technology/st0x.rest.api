//! REST API attribution contexts embedded in generated `takeOrders4` calldata.
//!
//! These contexts are not used by order Rainlang for authorization. They are
//! signed by the REST API so an off-chain indexer can attribute an executed
//! transaction to the API key that requested its calldata.

use alloy::primitives::{keccak256, Address, Bytes, Signature, B256, U256};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer as AlloySigner;
use rain_orderbook_bindings::IRaindexV6::SignedContextV1;

pub(crate) const ATTRIBUTION_SCHEMA_VERSION: u64 = 1;
pub(crate) const ATTRIBUTION_CONTEXT_WORDS: usize = 4;

/// EIP-191 signer for REST attribution contexts.
///
/// The private key is deliberately omitted from `Debug` output.
pub(crate) struct AttributionSigner {
    inner: PrivateKeySigner,
}

impl std::fmt::Debug for AttributionSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AttributionSigner")
            .field("address", &self.inner.address())
            .field("key", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum AttributionSignerError {
    #[error("invalid attribution signer private key: {0}")]
    InvalidKey(String),
    #[error("attribution signing failed: {0}")]
    SignFailed(String),
}

impl AttributionSigner {
    pub(crate) fn from_hex_key(private_key: &str) -> Result<Self, AttributionSignerError> {
        let key = private_key.strip_prefix("0x").unwrap_or(private_key);
        let inner: PrivateKeySigner =
            key.parse()
                .map_err(|error: alloy::signers::local::LocalSignerError| {
                    AttributionSignerError::InvalidKey(error.to_string())
                })?;
        Ok(Self { inner })
    }

    pub(crate) fn address(&self) -> Address {
        self.inner.address()
    }

    /// Sign the versioned attribution frame:
    ///
    /// `[version, api_key_hash, taker, order_hash]`.
    pub(crate) async fn sign_context(
        &self,
        attribution: &Attribution,
        order_hash: B256,
    ) -> Result<SignedContextV1, AttributionSignerError> {
        let context = attribution.context_for_order(order_hash);
        let hash = attribution_message_hash(&context);
        let signature = self
            .inner
            .sign_message(hash.as_slice())
            .await
            .map_err(|error| AttributionSignerError::SignFailed(error.to_string()))?;

        Ok(SignedContextV1 {
            signer: self.address(),
            context: context.to_vec(),
            signature: Bytes::copy_from_slice(signature.as_bytes().as_slice()),
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Attribution {
    pub(crate) api_key_hash: B256,
    pub(crate) taker: Address,
}

impl Attribution {
    pub(crate) fn context_for_order(&self, order_hash: B256) -> [B256; ATTRIBUTION_CONTEXT_WORDS] {
        [
            u64_to_b256(ATTRIBUTION_SCHEMA_VERSION),
            self.api_key_hash,
            address_to_b256(self.taker),
            order_hash,
        ]
    }
}

pub(crate) struct AttributionState {
    pub(crate) signer: AttributionSigner,
}

impl AttributionState {
    pub(crate) fn new(signer: AttributionSigner) -> Self {
        Self { signer }
    }

    pub(crate) fn for_api_key(&self, key_id: &str, taker: Address) -> Attribution {
        Attribution {
            api_key_hash: compute_api_key_hash(key_id),
            taker,
        }
    }
}

pub(crate) fn compute_api_key_hash(key_id: &str) -> B256 {
    keccak256(key_id.as_bytes())
}

pub(crate) fn attribution_message_hash(context: &[B256; ATTRIBUTION_CONTEXT_WORDS]) -> B256 {
    let mut packed = [0u8; ATTRIBUTION_CONTEXT_WORDS * 32];
    for (target, word) in packed.chunks_exact_mut(32).zip(context) {
        target.copy_from_slice(word.as_slice());
    }
    keccak256(packed)
}

pub(crate) fn address_to_b256(address: Address) -> B256 {
    let mut value = [0u8; 32];
    value[12..].copy_from_slice(address.as_slice());
    B256::from(value)
}

pub(crate) fn u64_to_b256(value: u64) -> B256 {
    B256::from(U256::from(value))
}

pub(crate) fn verify_signed_attribution(
    signed_context: &SignedContextV1,
    expected_signer: Address,
    taker: Address,
    order_hash: B256,
) -> Option<B256> {
    if signed_context.signer != expected_signer {
        return None;
    }
    let context: [B256; ATTRIBUTION_CONTEXT_WORDS] =
        signed_context.context.clone().try_into().ok()?;
    if context[0] != u64_to_b256(ATTRIBUTION_SCHEMA_VERSION)
        || context[2] != address_to_b256(taker)
        || context[3] != order_hash
    {
        return None;
    }
    let signature = Signature::try_from(signed_context.signature.as_ref()).ok()?;
    let hash = attribution_message_hash(&context);
    let recovered = signature.recover_address_from_msg(hash.as_slice()).ok()?;
    (recovered == expected_signer).then_some(context[1])
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    const TEST_KEY: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
    const TEST_ADDRESS: Address = address!("f39Fd6e51aad88F6F4ce6aB8827279cffFb92266");

    fn attribution() -> Attribution {
        Attribution {
            api_key_hash: compute_api_key_hash("customer-key"),
            taker: Address::from([0x22; 20]),
        }
    }

    #[test]
    fn derives_expected_address() {
        let signer = AttributionSigner::from_hex_key(TEST_KEY).unwrap();
        assert_eq!(signer.address(), TEST_ADDRESS);
    }

    #[test]
    fn redacts_private_key_from_debug() {
        let signer = AttributionSigner::from_hex_key(TEST_KEY).unwrap();
        let debug = format!("{signer:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains(TEST_KEY));
    }

    #[tokio::test]
    async fn signs_versioned_attribution_layout() {
        let signer = AttributionSigner::from_hex_key(TEST_KEY).unwrap();
        let attribution = attribution();
        let order_hash = B256::from([0x33; 32]);
        let signed = signer.sign_context(&attribution, order_hash).await.unwrap();

        assert_eq!(signed.signer, signer.address());
        assert_eq!(signed.signature.len(), 65);
        assert_eq!(signed.context.len(), ATTRIBUTION_CONTEXT_WORDS);
        assert_eq!(signed.context[0], u64_to_b256(ATTRIBUTION_SCHEMA_VERSION));
        assert_eq!(signed.context[1], attribution.api_key_hash);
        assert_eq!(signed.context[2], address_to_b256(attribution.taker));
        assert_eq!(signed.context[3], order_hash);

        let context_hash = attribution_message_hash(&attribution.context_for_order(order_hash));
        let signature = Signature::try_from(signed.signature.as_ref()).unwrap();
        assert_eq!(
            signature
                .recover_address_from_msg(context_hash.as_slice())
                .unwrap(),
            signer.address()
        );
        assert_eq!(
            verify_signed_attribution(&signed, signer.address(), attribution.taker, order_hash),
            Some(attribution.api_key_hash)
        );
    }
}
