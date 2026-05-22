# Trade Monitoring

Track trade execution for your orders.

## Trades by Address

```
GET /v1/trades/{address}
```

Paginated list of trades associated with a wallet address.

### Request

```bash
curl "https://api.st0x.io/v1/trades/0xYourAddress?page=1&pageSize=20" \
  -H "Authorization: Basic <credentials>"
```

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `page` | number | 1 | Page number |
| `pageSize` | number | 20 | Results per page |
| `startTime` | number | - | Filter: only trades after this Unix timestamp |
| `endTime` | number | - | Filter: only trades before this Unix timestamp |
| `denomination` | string | `wtstock` | Set to `tstock` to express amounts and IO ratios in underlying tStock units. See [Denomination toggle](#denomination-toggle). |

### Response

```json
{
  "trades": [
    {
      "txHash": "0x...",
      "inputAmount": "2000.0",
      "outputAmount": "0.8",
      "inputToken": { "address": "0x...", "symbol": "USDC", "decimals": 6 },
      "outputToken": { "address": "0x...", "symbol": "WETH", "decimals": 18 },
      "orderHash": "0xabc123...",
      "timestamp": 1708010000,
      "blockNumber": 12345678
    }
  ],
  "pagination": {
    "page": 1,
    "pageSize": 20,
    "totalTrades": 42,
    "totalPages": 3,
    "hasMore": true
  }
}
```

### Time Filtering

To get trades within a specific window:

```bash
curl "https://api.st0x.io/v1/trades/0xYourAddress?startTime=1708000000&endTime=1708100000" \
  -H "Authorization: Basic <credentials>"
```

## Trades by Transaction

```
GET /v1/trades/tx/{tx_hash}
```

Detailed breakdown of all trades within a specific transaction, including per-trade request/result and aggregate totals.

### Request

```bash
curl https://api.st0x.io/v1/trades/tx/0xTxHash... \
  -H "Authorization: Basic <credentials>"
```

### Response

```json
{
  "txHash": "0xTxHash...",
  "blockNumber": 12345678,
  "timestamp": 1708010000,
  "sender": "0xSolverAddress",
  "trades": [
    {
      "orderHash": "0xabc123...",
      "orderOwner": "0xOwnerAddress",
      "request": {
        "inputToken": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
        "outputToken": "0x4200000000000000000000000000000000000006",
        "maximumInput": "3000.0",
        "maximumIoRatio": "2600.0"
      },
      "result": {
        "inputAmount": "2500.0",
        "outputAmount": "1.0",
        "actualIoRatio": "2500.0"
      }
    }
  ],
  "totals": {
    "totalInputAmount": "2500.0",
    "totalOutputAmount": "1.0",
    "averageIoRatio": "2500.0"
  }
}
```

The `totals` field aggregates across all trades in the transaction.

## Denomination toggle

All `/v1/trades/*` GET endpoints accept a `denomination` query parameter:

| Value | Meaning |
|-------|---------|
| `wtstock` *(default)* | Amounts and IO ratios are returned exactly as recorded on chain — the wrapped (`wt*`) share token amounts. Backwards-compatible; preserves prior behavior. |
| `tstock` | Wrapped-side amounts and IO ratios are scaled by the assets-per-share rate from the wrapped exchange rate snapshot ≤ each trade's block number, so amounts read in underlying tStock units. |

When `denomination=tstock`:

- For each trade, the API looks up the `wrapped_exchange_rate_snapshots`
  row whose `block_number` is the greatest value ≤ the trade's block. If no
  qualifying snapshot exists for a wrapped token, that side is left as-is
  and `assetsPerShare` is omitted on that row.
- Token addresses, symbols, and decimals in `inputToken`/`outputToken`
  remain pointing at the wrapped share token. Callers should pair the
  returned `assetsPerShare` (when present) with [`/v1/tokens/exchange-rates`](./tokens.md#wrapped-token-exchange-rates)
  if they need underlying-asset metadata.
- For `GET /v1/trades/tx/{tx_hash}` the per-trade `request.maximumIoRatio`
  and `result.actualIoRatio` are recomputed in tStock terms, and the
  response-level `totals` are recomputed against the adjusted amounts.

### Example

```bash
curl "https://api.st0x.io/v1/trades/0xYourAddress?denomination=tstock" \
  -H "Authorization: Basic <credentials>"
```

```json
{
  "trades": [
    {
      "txHash": "0x...",
      "inputAmount": "10.4",
      "outputAmount": "-50.0",
      "inputToken": { "address": "0x...", "symbol": "wtMSTR", "decimals": 18 },
      "outputToken": { "address": "0x...", "symbol": "USDC", "decimals": 6 },
      "orderHash": "0xabc123...",
      "timestamp": 1708010000,
      "blockNumber": 12345678,
      "denomination": "tstock",
      "assetsPerShare": "1.04"
    }
  ]
}
```

### Historical accuracy

The conversion uses the *most recent* snapshot whose `block_number` is
`≤ trade.block_number`. Snapshot cadence is currently best-effort:

- Each call to `GET /v1/tokens/exchange-rates` refreshes any token whose
  most recent snapshot is older than 24 hours and persists a new row. The
  refresh dials the token's registry-configured RPC and reads
  `convertToAssets(10^decimals)` on the ERC4626 vault.
- Until the API has observed the chain for a token for the first time, no
  historical snapshot exists. For trades that predate the first snapshot,
  the `tstock` toggle leaves the row in raw wtStock amounts and omits the
  `assetsPerShare` field so the caller knows no conversion was applied.
  Hitting `/v1/tokens/exchange-rates` once after deploy is enough to seed
  the table for every wrapped token in the registry.

The same `denomination` toggle is accepted by `/v1/trades/tx/{tx_hash}`,
`/v1/trades/token/{address}`, and `/v1/trades/taker/{address}`. The
`POST /v1/trades/batch` endpoint currently returns amounts only in
`wtstock` (no per-trade token metadata is exposed for conversion).

