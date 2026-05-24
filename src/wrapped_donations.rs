//! Detection and persistence of ERC4626 "donation" events on wrapped tStock
//! tokens.
//!
//! A donation is a direct asset transfer to the wrapper that increases
//! `assetsPerShare` without a corresponding `Deposit` (i.e. without minting
//! new wrapper shares). The detection algorithm is:
//!
//! 1. For a wrapper, find its OARV asset address (caller passes it in — see
//!    [`crate::wrapped_rates::extract_asset_hint`] for the registry-side
//!    derivation).
//! 2. From `last_scanned_block + 1` up to the chain head, fetch
//!    `Transfer(*, wrapper, *)` logs on the OARV contract (chunked by
//!    [`SCAN_CHUNK_BLOCKS`] per `eth_getLogs` call to stay under provider
//!    limits).
//! 3. For each Transfer, look at the same tx's `Deposit(sender, owner,
//!    assets, shares)` events on the wrapper. If the Transfer's `value`
//!    matches the Deposit's `assets`, it's a regular deposit — skip.
//!    Otherwise, classify as a donation.
//! 4. For each donation, read `convertToAssets(10^decimals)` at the donation
//!    block via [`crate::wrapped_rates::IERC4626Rate`] to compute the new
//!    `assetsPerShare`.
//! 5. Persist via [`crate::db::wrapped_donations::insert_event`] and bump
//!    [`crate::db::wrapped_donations::update_scan_state`].
//!
//! ## Shape
//!
//! The module is split into a **pure core** (parsing/correlation) and a
//! **thin RPC shell** ([`scan_for_wrapper`]). Tests cover the core
//! exhaustively; the shell is exercised only at runtime. This keeps unit
//! tests fast and deterministic — mocking the alloy provider stack adds
//! more scaffolding than it returns in confidence.

use crate::db::wrapped_donations as db_donations;
use crate::db::wrapped_rates as db_rates;
use crate::db::DbPool;
use crate::denomination::{format_ratio_from_u256, scale_one};
use crate::error::ApiError;
use crate::rpc::try_with_providers;
use crate::wrapped_rates::{extract_asset_hint, IERC4626Rate};
use alloy::primitives::{keccak256, Address, B256, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::eth::{BlockNumberOrTag, Filter, Log};
use rain_orderbook_app_settings::token::TokenCfg;
use std::collections::{HashMap, HashSet};

/// Stay below most Base RPC providers' 10_000-block `eth_getLogs` cap with
/// some headroom for retries.
pub(crate) const SCAN_CHUNK_BLOCKS: u64 = 5_000;

/// Hard cap on how far the inline scan walks per request before bailing
/// out. With Base at ~2s block time this is ~28h of activity — past that
/// we'd rather return whatever's stored and warn than block the endpoint.
pub(crate) const MAX_INLINE_SCAN_BLOCKS: u64 = 50_000;

/// Number of blocks to look back when seeding a brand-new wrapper that has
/// neither a scan-cursor nor a stored snapshot. Avoids walking Base from
/// genesis on the first call against a freshly registered token.
const SEED_LOOKBACK_BLOCKS: u64 = 50_000;

/// How many blocks to lag behind the chain head before treating logs as
/// final. Base reorgs are rare and shallow (single-block in practice); 32
/// is generous insurance against a deeper one invalidating a freshly
/// detected donation. Donations land in the response on the next request
/// after this delay, which is fine: rates are cached for 24h anyway.
pub(crate) const REORG_CONFIRMATION_BLOCKS: u64 = 32;

/// Compute the inclusive `to_block` for a scan window, given the chain head
/// and the cursor. Returns `None` when there is nothing safe to scan yet
/// (i.e. `current_head - REORG_CONFIRMATION_BLOCKS <= from_block`). The
/// returned value is additionally capped at
/// `from_block + MAX_INLINE_SCAN_BLOCKS` so the route stays responsive.
///
/// Split out as a pure function so reorg-lag math can be unit-tested
/// without spinning up an RPC.
pub(crate) fn safe_to_block(current_head: u64, from_block: u64) -> Option<u64> {
    let safe_head = current_head.saturating_sub(REORG_CONFIRMATION_BLOCKS);
    if safe_head <= from_block {
        return None;
    }
    let inline_cap = from_block.saturating_add(MAX_INLINE_SCAN_BLOCKS);
    Some(safe_head.min(inline_cap))
}

/// `keccak256("Transfer(address,address,uint256)")`. Computed at runtime to
/// avoid a hardcoded byte literal that could silently drift from the actual
/// event signature.
fn transfer_topic() -> B256 {
    keccak256(b"Transfer(address,address,uint256)")
}

/// `keccak256("Deposit(address,address,uint256,uint256)")` — the ERC4626
/// standard event emitted whenever wrapper shares are minted.
fn deposit_topic() -> B256 {
    keccak256(b"Deposit(address,address,uint256,uint256)")
}

/// Decoded `Transfer(from, to, value)` event coming off the OARV.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedTransfer {
    pub from: Address,
    pub to: Address,
    pub value: U256,
    pub block_number: u64,
    pub tx_hash: B256,
}

/// Decoded `Deposit(sender, owner, assets, shares)` event coming off the
/// wrapper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedDeposit {
    #[allow(dead_code)] // surfaced for future logging; not used in correlation.
    pub sender: Address,
    #[allow(dead_code)]
    pub owner: Address,
    pub assets: U256,
    #[allow(dead_code)]
    pub shares: U256,
    pub tx_hash: B256,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum DecodeError {
    #[error("log missing topic at index {0}")]
    MissingTopic(usize),
    #[error("log missing block_number")]
    MissingBlockNumber,
    #[error("log missing transaction_hash")]
    MissingTxHash,
    #[error("data payload has unexpected length: got {got}, expected {expected}")]
    BadDataLength { got: usize, expected: usize },
}

/// Decode a `Transfer(from, to, value)` log emitted by the OARV. The OARV
/// uses the standard ERC20 signature: three indexed topics
/// (topic0=sig, topic1=from, topic2=to) plus the value packed into 32 bytes
/// of data.
pub(crate) fn decode_transfer_log(log: &Log) -> Result<DecodedTransfer, DecodeError> {
    let topics = log.inner.topics();
    let from_topic = topics.get(1).ok_or(DecodeError::MissingTopic(1))?;
    let to_topic = topics.get(2).ok_or(DecodeError::MissingTopic(2))?;
    let data = log.inner.data.data.as_ref();
    if data.len() != 32 {
        return Err(DecodeError::BadDataLength {
            got: data.len(),
            expected: 32,
        });
    }
    let block_number = log.block_number.ok_or(DecodeError::MissingBlockNumber)?;
    let tx_hash = log.transaction_hash.ok_or(DecodeError::MissingTxHash)?;
    Ok(DecodedTransfer {
        from: Address::from_word(*from_topic),
        to: Address::from_word(*to_topic),
        value: U256::from_be_slice(data),
        block_number,
        tx_hash,
    })
}

/// Decode a `Deposit(sender, owner, assets, shares)` log emitted by the
/// ERC4626 wrapper. `sender` and `owner` are indexed; `assets` and `shares`
/// are packed back-to-back in the data payload (32 bytes each).
pub(crate) fn decode_deposit_log(log: &Log) -> Result<DecodedDeposit, DecodeError> {
    let topics = log.inner.topics();
    let sender_topic = topics.get(1).ok_or(DecodeError::MissingTopic(1))?;
    let owner_topic = topics.get(2).ok_or(DecodeError::MissingTopic(2))?;
    let data = log.inner.data.data.as_ref();
    if data.len() != 64 {
        return Err(DecodeError::BadDataLength {
            got: data.len(),
            expected: 64,
        });
    }
    let tx_hash = log.transaction_hash.ok_or(DecodeError::MissingTxHash)?;
    Ok(DecodedDeposit {
        sender: Address::from_word(*sender_topic),
        owner: Address::from_word(*owner_topic),
        assets: U256::from_be_slice(&data[..32]),
        shares: U256::from_be_slice(&data[32..]),
        tx_hash,
    })
}

/// Given the OARV transfers landing on the wrapper and the wrapper-side
/// Deposit events from the same block range, return only the transfers
/// that cannot be matched to a Deposit — i.e. true donations.
///
/// Matching key is `(tx_hash, assets_amount)`. A single deposit may "consume"
/// at most one transfer with the same amount — if a tx happens to contain
/// two transfers of the same value plus one Deposit, the second transfer is
/// still a donation. We model this by counting matches per key and
/// decrementing the count as we consume them.
pub(crate) fn correlate_transfers_to_deposits(
    transfers: Vec<DecodedTransfer>,
    deposits: &[DecodedDeposit],
) -> Vec<DecodedTransfer> {
    // Build a multiset of (tx_hash, assets) keys from the deposits.
    let mut deposit_counts: HashMap<(B256, U256), usize> = HashMap::new();
    for d in deposits {
        *deposit_counts.entry((d.tx_hash, d.assets)).or_insert(0) += 1;
    }

    let mut donations = Vec::new();
    for t in transfers {
        let key = (t.tx_hash, t.value);
        match deposit_counts.get_mut(&key) {
            Some(count) if *count > 0 => {
                *count -= 1;
                // Matched a deposit — normal user mint, not a donation.
            }
            _ => {
                donations.push(t);
            }
        }
    }
    donations
}

/// Driver entry point. Returns the number of newly inserted donation rows.
///
/// `wrapper_token` is the ERC4626 share token (e.g. `wtMSTR`).
/// `oarv_address` is the underlying OARV asset address — caller pulls it
/// from the registry (`extract_asset_hint`) or a prior snapshot.
///
/// Failure modes:
/// - missing RPCs for the wrapper's network -> [`ApiError::Internal`]
/// - chain-head lookup fails -> [`ApiError::Internal`]
/// - per-chunk `eth_getLogs` failure is logged and the scan stops at the
///   last fully-processed chunk (cursor is bumped to that height); the
///   caller still sees `Ok(<inserts_so_far>)`.
///
/// Idempotence: the DB layer ignores duplicate inserts via
/// `INSERT OR IGNORE` on the widened unique index, so re-running the same
/// range never duplicates rows.
pub(crate) async fn scan_for_wrapper(
    pool: &DbPool,
    wrapper_token: &TokenCfg,
    oarv_address: Address,
) -> Result<u64, ApiError> {
    let wrapper_address = wrapper_token.address;

    if oarv_address == Address::ZERO {
        tracing::warn!(
            wrapper = ?wrapper_address,
            "skipping donation scan: OARV address unknown for wrapper"
        );
        return Ok(0);
    }

    let rpcs: Vec<url::Url> = wrapper_token.network.rpcs.clone();
    if rpcs.is_empty() {
        tracing::warn!(
            wrapper = ?wrapper_address,
            "skipping donation scan: no rpc urls configured for network"
        );
        return Ok(0);
    }

    // Pick the first RPC that responds to `eth_blockNumber` and use it for
    // the rest of the scan. Within a single scan we keep the same provider
    // so the chunk-by-chunk fallback story stays simple: if mid-scan calls
    // fail we stop, bump the cursor to whatever completed, and the next
    // request retries the rest (possibly via a different RPC).
    let (current_head, rpc) = match try_with_providers(&rpcs, |rpc| async move {
        let provider = ProviderBuilder::new().connect_http(rpc.clone());
        let head = provider
            .get_block_number()
            .await
            .map_err(|e| e.to_string())?;
        Ok::<(u64, url::Url), String>((head, rpc.clone()))
    })
    .await
    {
        Ok(pair) => pair,
        Err(msg) => {
            tracing::error!(
                wrapper = ?wrapper_address,
                error = %msg,
                "donation scan: failed to fetch chain head from any rpc"
            );
            return Err(ApiError::Internal(
                "donation scan: chain head lookup failed".into(),
            ));
        }
    };
    let provider = ProviderBuilder::new().connect_http(rpc.clone());

    let last_scanned = db_donations::get_scan_state(pool, wrapper_address)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, wrapper = ?wrapper_address, "donation scan: scan-state read failed");
            ApiError::Internal("donation scan: scan-state read failed".into())
        })?;

    let from_block = match last_scanned {
        Some(b) if b >= 0 => (b as u64).saturating_add(1),
        _ => {
            // No cursor yet — seed from the earliest stored snapshot, or
            // (worst case) `head - SEED_LOOKBACK_BLOCKS` so we don't walk
            // Base from genesis.
            match db_rates::get_earliest_for_token(pool, wrapper_address).await {
                Ok(Some(snapshot)) if snapshot.block_number > 0 => snapshot.block_number as u64,
                Ok(_) => current_head.saturating_sub(SEED_LOOKBACK_BLOCKS),
                Err(e) => {
                    tracing::error!(error = %e, wrapper = ?wrapper_address, "donation scan: earliest-snapshot lookup failed");
                    return Err(ApiError::Internal(
                        "donation scan: snapshot seed lookup failed".into(),
                    ));
                }
            }
        }
    };

    // Lag behind the chain head by REORG_CONFIRMATION_BLOCKS so a shallow
    // reorg can't invalidate a freshly persisted donation. If we don't have
    // a safe range yet, log and return 0 without bumping the cursor —
    // subsequent requests will pick the work up once enough blocks land.
    let to_block = match safe_to_block(current_head, from_block) {
        Some(b) => b,
        None => {
            tracing::info!(
                wrapper = ?wrapper_address,
                from_block,
                current_head,
                reorg_lag = REORG_CONFIRMATION_BLOCKS,
                "donation scan: nothing safe to scan yet (within reorg confirmation lag)"
            );
            return Ok(0);
        }
    };

    if to_block.saturating_sub(from_block) >= MAX_INLINE_SCAN_BLOCKS {
        tracing::warn!(
            wrapper = ?wrapper_address,
            from_block,
            current_head,
            capped_to = to_block,
            "donation scan: range exceeds MAX_INLINE_SCAN_BLOCKS; scanning a partial window"
        );
    }

    tracing::info!(
        wrapper = ?wrapper_address,
        oarv = ?oarv_address,
        from_block,
        to_block,
        "donation scan: starting"
    );

    let share_decimals = wrapper_token.decimals.unwrap_or(18);
    let one_share = scale_one(share_decimals);
    let (_asset_addr, _asset_sym, asset_decimals_hint) = extract_asset_hint(wrapper_token);
    let effective_asset_decimals = if asset_decimals_hint == 0 {
        share_decimals
    } else {
        asset_decimals_hint
    };

    let transfer_topic = transfer_topic();
    let deposit_topic = deposit_topic();
    let wrapper_topic = wrapper_address.into_word();

    let mut block_ts_cache: HashMap<u64, u64> = HashMap::new();
    let mut inserts: u64 = 0;
    let mut chunk_start = from_block;
    let mut last_completed = from_block.saturating_sub(1);

    while chunk_start <= to_block {
        let chunk_end = chunk_start
            .saturating_add(SCAN_CHUNK_BLOCKS.saturating_sub(1))
            .min(to_block);

        // ---- OARV Transfer logs (filtered to `to == wrapper`) ----
        let transfers_filter = Filter::new()
            .address(oarv_address)
            .from_block(BlockNumberOrTag::Number(chunk_start))
            .to_block(BlockNumberOrTag::Number(chunk_end))
            .event_signature(transfer_topic)
            .topic2(wrapper_topic);

        let transfer_logs = match provider.get_logs(&transfers_filter).await {
            Ok(logs) => logs,
            Err(e) => {
                tracing::warn!(
                    wrapper = ?wrapper_address,
                    chunk_start,
                    chunk_end,
                    error = %e,
                    "donation scan: transfer-log fetch failed; stopping early"
                );
                break;
            }
        };

        if transfer_logs.is_empty() {
            // Fast path: no OARV transfers to the wrapper in this chunk —
            // skip the Deposit query entirely.
            last_completed = chunk_end;
            chunk_start = chunk_end.saturating_add(1);
            continue;
        }

        // ---- Wrapper Deposit logs in the same range ----
        let deposits_filter = Filter::new()
            .address(wrapper_address)
            .from_block(BlockNumberOrTag::Number(chunk_start))
            .to_block(BlockNumberOrTag::Number(chunk_end))
            .event_signature(deposit_topic);

        let deposit_logs = match provider.get_logs(&deposits_filter).await {
            Ok(logs) => logs,
            Err(e) => {
                tracing::warn!(
                    wrapper = ?wrapper_address,
                    chunk_start,
                    chunk_end,
                    error = %e,
                    "donation scan: deposit-log fetch failed; stopping early"
                );
                break;
            }
        };

        let decoded_transfers: Vec<DecodedTransfer> = transfer_logs
            .iter()
            .filter_map(|log| match decode_transfer_log(log) {
                Ok(t) => Some(t),
                Err(e) => {
                    tracing::warn!(error = %e, "donation scan: skipping malformed transfer log");
                    None
                }
            })
            .collect();
        let decoded_deposits: Vec<DecodedDeposit> = deposit_logs
            .iter()
            .filter_map(|log| match decode_deposit_log(log) {
                Ok(d) => Some(d),
                Err(e) => {
                    tracing::warn!(error = %e, "donation scan: skipping malformed deposit log");
                    None
                }
            })
            .collect();

        let donations = correlate_transfers_to_deposits(decoded_transfers, &decoded_deposits);

        if donations.is_empty() {
            last_completed = chunk_end;
            chunk_start = chunk_end.saturating_add(1);
            continue;
        }

        tracing::info!(
            wrapper = ?wrapper_address,
            chunk_start,
            chunk_end,
            donation_count = donations.len(),
            "donation scan: detected donations in chunk"
        );

        // Collect block numbers we need timestamps for, dedup, then fetch.
        let needed_blocks: HashSet<u64> = donations.iter().map(|d| d.block_number).collect();
        for bn in needed_blocks {
            if block_ts_cache.contains_key(&bn) {
                continue;
            }
            match provider
                .get_block_by_number(BlockNumberOrTag::Number(bn))
                .await
            {
                Ok(Some(block)) => {
                    use alloy::consensus::BlockHeader;
                    let ts = block.header.timestamp();
                    block_ts_cache.insert(bn, ts);
                }
                Ok(None) => {
                    tracing::warn!(
                        block = bn,
                        "donation scan: block not found when fetching timestamp; recording 0"
                    );
                    block_ts_cache.insert(bn, 0);
                }
                Err(e) => {
                    tracing::warn!(block = bn, error = %e, "donation scan: block fetch failed; recording 0");
                    block_ts_cache.insert(bn, 0);
                }
            }
        }

        // For each donation: compute new assetsPerShare at its block,
        // persist. If the rate read fails (RPC pruned that block, transient
        // error, etc.) we still persist the row with a NULL rate so the
        // cursor can safely advance — the route layer lazily backfills the
        // value on the next history request. Without this, a failed rate
        // fetch lost the donation forever once the cursor moved past.
        let contract = IERC4626Rate::new(wrapper_address, &provider);
        for donation in donations {
            let rate = match contract
                .convertToAssets(one_share)
                .block(BlockNumberOrTag::Number(donation.block_number).into())
                .call()
                .await
            {
                Ok(a) => Some(format_ratio_from_u256(a, effective_asset_decimals)),
                Err(e) => {
                    tracing::warn!(
                        wrapper = ?wrapper_address,
                        block = donation.block_number,
                        tx_hash = ?donation.tx_hash,
                        error = %e,
                        "donation scan: convertToAssets failed; persisting donation with null rate for later backfill"
                    );
                    None
                }
            };

            let asset_amount = format_ratio_from_u256(donation.value, effective_asset_decimals);
            let block_timestamp = block_ts_cache
                .get(&donation.block_number)
                .copied()
                .unwrap_or(0);

            let new_event = db_donations::NewDonationEvent {
                wrapper_address,
                donor_address: donation.from,
                asset_amount: &asset_amount,
                block_number: donation.block_number,
                block_timestamp,
                tx_hash: donation.tx_hash,
                new_assets_per_share: rate.as_deref(),
            };
            if let Err(e) = db_donations::insert_event(pool, &new_event).await {
                tracing::error!(
                    error = %e,
                    wrapper = ?wrapper_address,
                    tx_hash = ?donation.tx_hash,
                    "donation scan: insert failed; continuing"
                );
                continue;
            }
            inserts += 1;
        }

        last_completed = chunk_end;
        chunk_start = chunk_end.saturating_add(1);
    }

    // Bump the cursor to whatever we actually finished. If everything failed
    // on the first chunk `last_completed` stays at `from_block - 1` (or 0
    // on saturate); we still write it so subsequent scans don't re-do work
    // that succeeded earlier.
    if last_completed >= from_block.saturating_sub(1) {
        if let Err(e) = db_donations::update_scan_state(pool, wrapper_address, last_completed).await
        {
            tracing::error!(
                error = %e,
                wrapper = ?wrapper_address,
                "donation scan: scan-state update failed; next call will redo this range"
            );
        }
    }

    tracing::info!(
        wrapper = ?wrapper_address,
        inserts,
        last_completed,
        "donation scan: complete"
    );
    Ok(inserts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{Bytes, LogData};

    fn addr(byte: u8) -> Address {
        let mut buf = [0u8; 20];
        buf[19] = byte;
        Address::from(buf)
    }

    fn tx(byte: u8) -> B256 {
        let mut buf = [0u8; 32];
        buf[31] = byte;
        B256::from(buf)
    }

    /// Build a `Log` whose inner consensus log carries the supplied topics
    /// and data, plus the metadata fields we read in the decoders. Mirrors
    /// what alloy hands us back from `eth_getLogs`.
    fn make_log(topics: Vec<B256>, data: Vec<u8>, block_number: u64, tx_hash: B256) -> Log {
        let log_data = LogData::new_unchecked(topics, Bytes::from(data));
        let inner = alloy::primitives::Log {
            address: Address::ZERO,
            data: log_data,
        };
        Log {
            inner,
            block_hash: None,
            block_number: Some(block_number),
            block_timestamp: None,
            transaction_hash: Some(tx_hash),
            transaction_index: None,
            log_index: None,
            removed: false,
        }
    }

    fn u256_to_word(value: U256) -> Vec<u8> {
        value.to_be_bytes::<32>().to_vec()
    }

    #[test]
    fn test_decode_transfer_log_happy_path() {
        let from = addr(1);
        let to = addr(2);
        let value = U256::from(12_345u64);
        let log = make_log(
            vec![transfer_topic(), from.into_word(), to.into_word()],
            u256_to_word(value),
            100,
            tx(7),
        );

        let decoded = decode_transfer_log(&log).expect("decode");
        assert_eq!(decoded.from, from);
        assert_eq!(decoded.to, to);
        assert_eq!(decoded.value, value);
        assert_eq!(decoded.block_number, 100);
        assert_eq!(decoded.tx_hash, tx(7));
    }

    #[test]
    fn test_decode_transfer_log_missing_topic() {
        let log = make_log(vec![transfer_topic()], u256_to_word(U256::ZERO), 1, tx(1));
        let err = decode_transfer_log(&log).unwrap_err();
        assert!(matches!(err, DecodeError::MissingTopic(_)));
    }

    #[test]
    fn test_decode_transfer_log_bad_data_length() {
        let log = make_log(
            vec![transfer_topic(), addr(1).into_word(), addr(2).into_word()],
            vec![0u8; 16], // wrong size
            1,
            tx(1),
        );
        let err = decode_transfer_log(&log).unwrap_err();
        assert!(matches!(err, DecodeError::BadDataLength { .. }));
    }

    #[test]
    fn test_decode_deposit_log_happy_path() {
        let sender = addr(3);
        let owner = addr(4);
        let assets = U256::from(1_000_000u64);
        let shares = U256::from(999_000u64);
        let mut data = u256_to_word(assets);
        data.extend(u256_to_word(shares));
        let log = make_log(
            vec![deposit_topic(), sender.into_word(), owner.into_word()],
            data,
            200,
            tx(9),
        );

        let decoded = decode_deposit_log(&log).expect("decode");
        assert_eq!(decoded.sender, sender);
        assert_eq!(decoded.owner, owner);
        assert_eq!(decoded.assets, assets);
        assert_eq!(decoded.shares, shares);
        assert_eq!(decoded.tx_hash, tx(9));
    }

    #[test]
    fn test_decode_deposit_log_bad_data_length() {
        let log = make_log(
            vec![deposit_topic(), addr(1).into_word(), addr(2).into_word()],
            vec![0u8; 32], // should be 64
            1,
            tx(1),
        );
        let err = decode_deposit_log(&log).unwrap_err();
        assert!(matches!(err, DecodeError::BadDataLength { .. }));
    }

    fn t(tx_byte: u8, from_byte: u8, value: u64, block: u64) -> DecodedTransfer {
        DecodedTransfer {
            from: addr(from_byte),
            to: addr(255),
            value: U256::from(value),
            block_number: block,
            tx_hash: tx(tx_byte),
        }
    }

    fn d(tx_byte: u8, assets: u64) -> DecodedDeposit {
        DecodedDeposit {
            sender: addr(0),
            owner: addr(0),
            assets: U256::from(assets),
            shares: U256::from(assets),
            tx_hash: tx(tx_byte),
        }
    }

    #[test]
    fn test_correlate_no_deposits_means_all_donations() {
        let transfers = vec![t(1, 10, 100, 1), t(2, 11, 200, 2)];
        let donations = correlate_transfers_to_deposits(transfers.clone(), &[]);
        assert_eq!(donations, transfers);
    }

    #[test]
    fn test_correlate_matched_deposit_excluded() {
        // tx1 transfer with value=100 matches tx1 deposit with assets=100.
        let transfers = vec![t(1, 10, 100, 1)];
        let deposits = vec![d(1, 100)];
        let donations = correlate_transfers_to_deposits(transfers, &deposits);
        assert!(donations.is_empty(), "matched deposit should hide transfer");
    }

    #[test]
    fn test_correlate_donation_alongside_unrelated_deposit() {
        // Block has a normal deposit (tx1, value=100) AND a separate donation
        // (tx2, value=50). Only tx2 should surface.
        let transfers = vec![t(1, 10, 100, 1), t(2, 11, 50, 1)];
        let deposits = vec![d(1, 100)];
        let donations = correlate_transfers_to_deposits(transfers, &deposits);
        assert_eq!(donations.len(), 1);
        assert_eq!(donations[0].tx_hash, tx(2));
        assert_eq!(donations[0].value, U256::from(50u64));
    }

    #[test]
    fn test_correlate_two_transfers_one_deposit_same_tx_one_is_donation() {
        // Same tx contains two equal-value transfers but only ONE Deposit
        // (rare but possible — e.g. a router forwarding leftover funds back
        // to the wrapper alongside a real mint). The deposit consumes one
        // transfer; the second transfer should surface as a donation.
        let transfers = vec![t(1, 10, 100, 1), t(1, 11, 100, 1)];
        let deposits = vec![d(1, 100)];
        let donations = correlate_transfers_to_deposits(transfers, &deposits);
        assert_eq!(donations.len(), 1);
        assert_eq!(donations[0].value, U256::from(100u64));
    }

    #[test]
    fn test_correlate_mismatched_amount_is_donation() {
        // tx1 has a deposit for assets=100, but the transfer in tx1 is
        // value=99 (e.g. a rounding-driven dust top-up). The amounts don't
        // match so the transfer is treated as a donation.
        let transfers = vec![t(1, 10, 99, 1)];
        let deposits = vec![d(1, 100)];
        let donations = correlate_transfers_to_deposits(transfers, &deposits);
        assert_eq!(donations.len(), 1);
        assert_eq!(donations[0].value, U256::from(99u64));
    }

    #[test]
    fn test_safe_to_block_returns_none_within_reorg_window() {
        // current_head=1000, from_block=970, reorg=32 → safe_head=968
        // 968 <= 970 → None (nothing safe to scan yet).
        assert_eq!(safe_to_block(1000, 970), None);
    }

    #[test]
    fn test_safe_to_block_caps_at_safe_head() {
        // current_head=1000, from_block=900 → safe_head=968.
        // Window (68 blocks) is well under MAX_INLINE_SCAN_BLOCKS, so the
        // returned value is exactly safe_head.
        assert_eq!(safe_to_block(1000, 900), Some(968));
    }

    #[test]
    fn test_safe_to_block_caps_at_inline_max() {
        // safe_head far past from_block + MAX_INLINE_SCAN_BLOCKS → cap.
        let from_block = 100u64;
        let current_head = from_block + MAX_INLINE_SCAN_BLOCKS + REORG_CONFIRMATION_BLOCKS + 10_000;
        let got = safe_to_block(current_head, from_block).expect("safe range exists");
        assert_eq!(got, from_block + MAX_INLINE_SCAN_BLOCKS);
    }

    #[test]
    fn test_safe_to_block_handles_genesis_head() {
        // current_head smaller than the reorg lag — saturating_sub keeps it
        // at 0, which is by definition <= from_block, so we return None.
        assert_eq!(safe_to_block(10, 0), None);
    }
}
