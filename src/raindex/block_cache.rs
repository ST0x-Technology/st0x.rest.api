use crate::cache::AppCache;
use std::time::Duration;

const BLOCK_NUMBER_CACHE_TTL: Duration = Duration::from_secs(2);
const BLOCK_NUMBER_CACHE_CAPACITY: u64 = 16;

/// Per-chain cache of the latest block number.
///
/// Block time on Base is ~2s, so a 2s TTL gives effectively no staleness while
/// eliminating the per-quote-batch `eth_blockNumber` round-trip the upstream
/// quote library performs when called with `block_number = None`.
pub(crate) type BlockNumberCache = AppCache<u32, u64>;

pub(crate) fn block_number_cache() -> BlockNumberCache {
    AppCache::new(BLOCK_NUMBER_CACHE_CAPACITY, BLOCK_NUMBER_CACHE_TTL)
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum BlockNumberError {
    #[error("no rpc urls configured for chain")]
    NoRpcUrls,
    #[error("rpc transport failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("rpc returned malformed response: {0}")]
    InvalidResponse(String),
}

/// Issue a JSON-RPC `eth_blockNumber` call against the first reachable URL.
///
/// We deliberately use a plain reqwest call rather than threading another
/// alloy/ethers provider through the codebase — this keeps the block-number
/// cache standalone and avoids new transitive deps.
async fn fetch_block_number(rpc_urls: &[String]) -> Result<u64, BlockNumberError> {
    if rpc_urls.is_empty() {
        return Err(BlockNumberError::NoRpcUrls);
    }

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_blockNumber",
        "params": [],
        "id": 1,
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;

    let mut last_err: Option<BlockNumberError> = None;
    for url in rpc_urls {
        match client.post(url).json(&body).send().await {
            Ok(resp) => {
                if !resp.status().is_success() {
                    last_err = Some(BlockNumberError::InvalidResponse(format!(
                        "http status {}",
                        resp.status()
                    )));
                    continue;
                }
                match resp.json::<serde_json::Value>().await {
                    Ok(json) => match parse_block_number(&json) {
                        Ok(b) => return Ok(b),
                        Err(e) => last_err = Some(e),
                    },
                    Err(e) => last_err = Some(BlockNumberError::Transport(e)),
                }
            }
            Err(e) => last_err = Some(BlockNumberError::Transport(e)),
        }
    }

    Err(last_err.unwrap_or(BlockNumberError::NoRpcUrls))
}

fn parse_block_number(json: &serde_json::Value) -> Result<u64, BlockNumberError> {
    let hex = json
        .get("result")
        .and_then(|v| v.as_str())
        .ok_or_else(|| BlockNumberError::InvalidResponse("missing result field".into()))?;
    let trimmed = hex.trim_start_matches("0x");
    u64::from_str_radix(trimmed, 16)
        .map_err(|e| BlockNumberError::InvalidResponse(format!("not a hex u64: {e}")))
}

/// Return the cached block number for the chain, fetching on miss.
///
/// Returns `None` when the fetch fails — callers should fall back to passing
/// `None` to the upstream library so it can perform its own (uncached) lookup.
pub(crate) async fn get_or_fetch_block_number(
    cache: &BlockNumberCache,
    chain_id: u32,
    rpc_urls: &[String],
) -> Option<u64> {
    if let Some(b) = cache.get(&chain_id).await {
        return Some(b);
    }
    match fetch_block_number(rpc_urls).await {
        Ok(b) => {
            cache.insert(chain_id, b).await;
            Some(b)
        }
        Err(e) => {
            tracing::warn!(
                chain_id,
                error = %e,
                "block number fetch failed; quote batch will fetch directly"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn test_parse_block_number_valid() {
        let v = json!({"jsonrpc": "2.0", "id": 1, "result": "0x1a2b3c"});
        assert_eq!(parse_block_number(&v).unwrap(), 0x1a2b3c);
    }

    #[test]
    fn test_parse_block_number_no_prefix() {
        let v = json!({"result": "ff"});
        assert_eq!(parse_block_number(&v).unwrap(), 255);
    }

    #[test]
    fn test_parse_block_number_missing_result() {
        let v = json!({"jsonrpc": "2.0", "id": 1});
        let err = parse_block_number(&v).unwrap_err();
        assert!(matches!(err, BlockNumberError::InvalidResponse(_)));
    }

    #[test]
    fn test_parse_block_number_garbage() {
        let v = json!({"result": "0xZZZ"});
        let err = parse_block_number(&v).unwrap_err();
        assert!(matches!(err, BlockNumberError::InvalidResponse(_)));
    }

    #[rocket::async_test]
    async fn test_fetch_block_number_no_urls() {
        let result = fetch_block_number(&[]).await;
        assert!(matches!(result, Err(BlockNumberError::NoRpcUrls)));
    }

    /// Spawn a tiny HTTP server that responds with the given JSON body to one
    /// POST and then exits. Returns the bound URL.
    async fn one_shot_rpc(response_body: String) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        format!("http://{addr}")
    }

    #[rocket::async_test]
    async fn test_fetch_block_number_success() {
        let url = one_shot_rpc(r#"{"jsonrpc":"2.0","id":1,"result":"0x100"}"#.into()).await;
        let block = fetch_block_number(&[url]).await.unwrap();
        assert_eq!(block, 256);
    }

    #[rocket::async_test]
    async fn test_get_or_fetch_caches_value() {
        let cache: BlockNumberCache = block_number_cache();
        cache.insert(8453, 12345).await;

        let counter = Arc::new(AtomicUsize::new(0));
        // No URLs needed — value is cached.
        let block = get_or_fetch_block_number(&cache, 8453, &[]).await;
        assert_eq!(block, Some(12345));
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[rocket::async_test]
    async fn test_get_or_fetch_returns_none_on_failure() {
        let cache: BlockNumberCache = block_number_cache();
        // 127.0.0.1:1 is reserved; the connection will fail.
        let block = get_or_fetch_block_number(&cache, 8453, &["http://127.0.0.1:1".into()]).await;
        assert_eq!(block, None);
    }
}
