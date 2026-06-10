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

| Parameter      | Type                     | Default   | Description                                                                                                                  |
| -------------- | ------------------------ | --------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `page`         | number                   | 1         | Page number                                                                                                                  |
| `pageSize`     | number                   | 20        | Results per page                                                                                                             |
| `startTime`    | number                   | -         | Filter: only trades after this Unix timestamp                                                                                |
| `endTime`      | number                   | -         | Filter: only trades before this Unix timestamp                                                                               |
| `denomination` | `wrapped` or `unwrapped` | `wrapped` | Return wrapped token amounts as-is, or normalize observed wrapped token amounts and IO ratios to their unwrapped asset value |

When `denomination=unwrapped`, amount and IO ratio fields are normalized from
the wrapped token value using the current wrapped exchange rate. This is a
current-rate normalized view, not a historically exact reconstruction of the
rate at the trade block. Do not combine unwrapped-normalized trade values with
wrapped-denomination values from endpoints that were called without
`denomination=unwrapped`; the calculations will differ.

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

Detailed breakdown of all trades within a specific transaction, including
per-trade request/result and aggregate totals.

### Request

```bash
curl "https://api.st0x.io/v1/trades/tx/0xTxHash...?denomination=unwrapped" \
  -H "Authorization: Basic <credentials>"
```

| Parameter      | Type                     | Default   | Description                                                                                                                           |
| -------------- | ------------------------ | --------- | ------------------------------------------------------------------------------------------------------------------------------------- |
| `denomination` | `wrapped` or `unwrapped` | `wrapped` | Return wrapped token amounts as-is, or normalize observed wrapped token amounts, IO ratios, and totals to their unwrapped asset value |

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

Historical-rate conversion for older trades will be handled separately once the
exchange-rate history is incorporated into the trade response path.

## Other Trade Queries

The same denomination behavior is supported by the other trade endpoints:

```text
GET /v1/trades/token/{address}?denomination=unwrapped
GET /v1/trades/taker/{address}?denomination=unwrapped
POST /v1/trades/query
```

For `POST /v1/trades/query`, include `"denomination": "unwrapped"` in the JSON
body with `orderHashes`, `startTime`, and `endTime`.
