//! EIP-191 signer used to produce gating `SignedContextV1` entries for
//! take-order flows against API-gated strategies.
//!
//! The signed context carries `[recipient, orderHash, expiry, id]` as four
//! bytes32 values. Strategies on the gated-pyth registry entry verify:
//!   - signer == `api-signer` binding,
//!   - recipient == `order-counterparty()`,
//!   - orderHash == `order-hash()`,
//!   - expiry >= `now()`.
//!
//! The `id` (keccak256 of the authenticated API key id) is signed but not
//! asserted on-chain; it is retained purely for off-chain attribution via
//! event indexing.

use alloy::primitives::{keccak256, Address, FixedBytes, B256, U256};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer as AlloySigner;

/// Holds the gating signer key and knows how to produce signed contexts.
///
/// Does **not** derive `Debug`: the inner `PrivateKeySigner` would print
/// the secret through `{:?}`. A manual impl redacts the key material and
/// only surfaces the public address, so any accidental log or panic
/// formatting of a struct containing a `GatingSigner` stays safe.
pub struct GatingSigner {
    inner: PrivateKeySigner,
}

impl std::fmt::Debug for GatingSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatingSigner")
            .field("address", &self.inner.address())
            .field("key", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GatingSignerError {
    #[error("invalid gating signer private key: {0}")]
    InvalidKey(String),
    #[error("signing failed: {0}")]
    SignFailed(String),
}

impl GatingSigner {
    /// Parse a hex private key (with or without `0x` prefix).
    pub fn from_hex_key(private_key: &str) -> Result<Self, GatingSignerError> {
        let key = private_key.strip_prefix("0x").unwrap_or(private_key);
        let inner: PrivateKeySigner =
            key.parse()
                .map_err(|e: alloy::signers::local::LocalSignerError| {
                    GatingSignerError::InvalidKey(e.to_string())
                })?;
        Ok(Self { inner })
    }

    pub fn address(&self) -> Address {
        self.inner.address()
    }

    /// Sign `[recipient, orderHash, expiry, id]` as a four-entry signed
    /// context. The signature is EIP-191 over `keccak256(abi.encodePacked(context))`,
    /// matching `LibContext.build` in the orderbook contract.
    pub async fn sign_gating_context(
        &self,
        recipient: Address,
        order_hash: B256,
        expiry: u64,
        id: B256,
    ) -> Result<SignedGatingContext, GatingSignerError> {
        let context: [FixedBytes<32>; 4] = [
            address_to_b256(recipient),
            order_hash,
            u64_to_b256(expiry),
            id,
        ];

        let mut packed = Vec::with_capacity(context.len() * 32);
        for word in &context {
            packed.extend_from_slice(word.as_slice());
        }
        let hash = keccak256(&packed);
        let signature = self
            .inner
            .sign_message(hash.as_slice())
            .await
            .map_err(|e| GatingSignerError::SignFailed(e.to_string()))?;

        Ok(SignedGatingContext {
            signer: self.address(),
            context: context.to_vec(),
            signature: signature.as_bytes().to_vec(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct SignedGatingContext {
    pub signer: Address,
    pub context: Vec<FixedBytes<32>>,
    pub signature: Vec<u8>,
}

/// Compute the per-key id embedded in gating signatures: `keccak256(key_id)`.
pub fn compute_attribution_id(key_id: &str) -> B256 {
    keccak256(key_id.as_bytes())
}

/// Bundle of runtime gating config held in rocket-managed state.
pub struct GatingState {
    pub signer: GatingSigner,
    pub ttl_seconds: u64,
}

impl GatingState {
    pub fn expiry_from_now(&self) -> u64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        now.saturating_add(self.ttl_seconds)
    }
}

fn address_to_b256(addr: Address) -> B256 {
    let mut out = [0u8; 32];
    out[12..].copy_from_slice(addr.as_slice());
    B256::from(out)
}

fn u64_to_b256(n: u64) -> B256 {
    B256::from(U256::from(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Hardhat account #0 — test only.
    const TEST_KEY: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
    const HARDHAT_0: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

    #[test]
    fn test_address_derivation() {
        let signer = GatingSigner::from_hex_key(TEST_KEY).unwrap();
        assert_eq!(signer.address(), HARDHAT_0.parse::<Address>().unwrap());
    }

    #[test]
    fn test_key_with_0x_prefix() {
        let signer = GatingSigner::from_hex_key(&format!("0x{TEST_KEY}")).unwrap();
        assert_eq!(signer.address(), HARDHAT_0.parse::<Address>().unwrap());
    }

    #[test]
    fn test_invalid_key() {
        let err = GatingSigner::from_hex_key("not-hex").unwrap_err();
        assert!(matches!(err, GatingSignerError::InvalidKey(_)));
    }

    #[tokio::test]
    async fn test_sign_is_deterministic() {
        let signer = GatingSigner::from_hex_key(TEST_KEY).unwrap();
        let recipient = "0x1111111111111111111111111111111111111111"
            .parse::<Address>()
            .unwrap();
        let order_hash = B256::from([0x22u8; 32]);
        let id = compute_attribution_id("test-key-id");

        let a = signer
            .sign_gating_context(recipient, order_hash, 1_700_000_000, id)
            .await
            .unwrap();
        let b = signer
            .sign_gating_context(recipient, order_hash, 1_700_000_000, id)
            .await
            .unwrap();

        assert_eq!(a.signature, b.signature);
        assert_eq!(a.signer, b.signer);
        assert_eq!(a.context, b.context);
        assert_eq!(a.signature.len(), 65);
        assert_eq!(a.context.len(), 4);
    }

    #[tokio::test]
    async fn test_different_inputs_produce_different_sigs() {
        let signer = GatingSigner::from_hex_key(TEST_KEY).unwrap();
        let recipient = "0x1111111111111111111111111111111111111111"
            .parse::<Address>()
            .unwrap();
        let order_hash_a = B256::from([0x22u8; 32]);
        let order_hash_b = B256::from([0x33u8; 32]);
        let id = compute_attribution_id("test-key-id");

        let a = signer
            .sign_gating_context(recipient, order_hash_a, 1_700_000_000, id)
            .await
            .unwrap();
        let b = signer
            .sign_gating_context(recipient, order_hash_b, 1_700_000_000, id)
            .await
            .unwrap();
        assert_ne!(a.signature, b.signature);
    }

    #[test]
    fn test_attribution_id_is_keccak_of_key_id() {
        let id = compute_attribution_id("abc123");
        let expected = keccak256(b"abc123");
        assert_eq!(id, expected);
    }

    #[test]
    fn test_address_to_b256_is_right_aligned() {
        let addr: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();
        let b = address_to_b256(addr);
        // first 12 bytes zero, last 20 bytes = addr
        assert_eq!(&b.as_slice()[..12], &[0u8; 12]);
        assert_eq!(&b.as_slice()[12..], addr.as_slice());
    }
}
