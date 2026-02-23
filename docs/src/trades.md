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
