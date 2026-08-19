# Order Management

Orders are persistent on-chain strategies that execute over time.

The order endpoints return transaction calldata — the API does not execute
transactions for you. You receive `to`, `data`, and `value` fields (plus any
required token `approvals`) and submit those transactions on-chain yourself, the
same pattern as the [Swap Flow](./swap-flow.md).

## Get DCA Order Calldata

```
POST /v1/order/dca
```

Returns calldata to deploy a DCA order that periodically buys a token at a set
interval, with price bounds.

### Request

```bash
curl -X POST https://api.st0x.io/v1/order/dca \
  -H "Authorization: Basic <credentials>" \
  -H "Content-Type: application/json" \
  -d '{
    "inputToken": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
    "outputToken": "0x4200000000000000000000000000000000000006",
    "budgetAmount": "10000",
    "period": 24,
    "periodUnit": "hours",
    "startIo": "2500.0",
    "floorIo": "2000.0"
  }'
```

| Field           | Type              | Description                                                           |
| --------------- | ----------------- | --------------------------------------------------------------------- |
| `inputToken`    | string            | Token to spend                                                        |
| `outputToken`   | string            | Token to receive                                                      |
| `budgetAmount`  | string            | Total budget in human-readable units (e.g. `"10000"` for 10,000 USDC) |
| `period`        | number            | Time between executions                                               |
| `periodUnit`    | string            | `"days"`, `"hours"`, or `"minutes"`                                   |
| `startIo`       | string            | Starting IO ratio                                                     |
| `floorIo`       | string            | Minimum acceptable IO ratio                                           |
| `inputVaultId`  | string (optional) | Existing vault ID for input token                                     |
| `outputVaultId` | string (optional) | Existing vault ID for output token                                    |

### Response

The response always includes all fields. If approvals are needed, `data` is
empty and `approvals` contains the required transactions:

```json
{
  "to": "0xOrderbookContractAddress",
  "data": "0x",
  "value": "0x0",
  "approvals": [
    {
      "token": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
      "spender": "0xOrderbookContractAddress",
      "amount": "10000",
      "symbol": "USDC",
      "approvalData": "0x..."
    }
  ]
}
```

Send each approval transaction on-chain, then call the endpoint again. Once
approvals are in place, `approvals` is empty and `data` contains the deployment
calldata:

```json
{
  "to": "0xOrderbookContractAddress",
  "data": "0xabcdef...",
  "value": "0x0",
  "approvals": []
}
```

## Get Order Details

```
GET /v1/order/{order_hash}
```

Retrieve the full state of an order including vault balances and trade history.

### Request

```bash
curl https://api.st0x.io/v1/order/0xabc123... \
  -H "Authorization: Basic <credentials>"
```

| Parameter      | Type                     | Default   | Description                                                                                                               |
| -------------- | ------------------------ | --------- | ------------------------------------------------------------------------------------------------------------------------- |
| `denomination` | `wrapped` or `unwrapped` | `wrapped` | Return wrapped token amounts as-is, or normalize wrapped token balances, trade amounts, and IO ratios to unwrapped values |

When `denomination=unwrapped`, order fields are normalized using the current
wrapped exchange rate. Omit the parameter to keep the default wrapped-token
response.

### Response

```json
{
  "orderHash": "0xabc123...",
  "owner": "0xOwnerAddress",
  "orderDetails": {
    "type": "dca",
    "ioRatio": "2500.0"
  },
  "inputToken": {
    "address": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
    "symbol": "USDC",
    "decimals": 6
  },
  "outputToken": {
    "address": "0x4200000000000000000000000000000000000006",
    "symbol": "WETH",
    "decimals": 18
  },
  "inputVaultId": "0x1",
  "outputVaultId": "0x2",
  "inputVaultBalance": "8000.0",
  "outputVaultBalance": "0.5",
  "ioRatio": "2500.0",
  "createdAt": 1708000000,
  "orderbookId": "0xOrderbookAddress",
  "trades": [
    {
      "id": "trade-1",
      "txHash": "0x...",
      "inputAmount": "2000.0",
      "outputAmount": "0.8",
      "timestamp": 1708010000,
      "sender": "0xSolverAddress"
    }
  ]
}
```

## List Orders by Owner

```
GET /v1/orders/owner/{address}
```

Paginated list of orders for a wallet address.

### Request

```bash
curl "https://api.st0x.io/v1/orders/owner/0xOwnerAddress?state=active&page=1&pageSize=10" \
  -H "Authorization: Basic <credentials>"
```

| Parameter      | Type                           | Default   | Description                                                                                               |
| -------------- | ------------------------------ | --------- | --------------------------------------------------------------------------------------------------------- |
| `state`        | `active`, `inactive`, or `all` | `active`  | Filter by current order state                                                                             |
| `page`         | number                         | 1         | Page number                                                                                               |
| `pageSize`     | number                         | 20        | Results per page                                                                                          |
| `denomination` | `wrapped` or `unwrapped`       | `wrapped` | Return wrapped token amounts as-is, or normalize wrapped token balances and IO ratios to unwrapped values |

Use `denomination=unwrapped` to view order balances and IO ratios normalized to
the current unwrapped asset value:

```bash
curl "https://api.st0x.io/v1/orders/owner/0xOwnerAddress?state=active&page=1&pageSize=10&denomination=unwrapped" \
  -H "Authorization: Basic <credentials>"
```

### Response

```json
{
  "orders": [
    {
      "orderHash": "0xabc123...",
      "owner": "0xOwnerAddress",
      "chainId": 8453,
      "orderBytes": "0x...",
      "active": true,
      "removedAt": null,
      "orderType": "limit",
      "inputToken": { "address": "0x...", "symbol": "USDC", "decimals": 6 },
      "outputToken": { "address": "0x...", "symbol": "WETH", "decimals": 18 },
      "outputVaultBalance": "0.5",
      "maxOutput": "0.25",
      "ioRatio": "2500.0",
      "createdAt": 1708000000,
      "orderbookId": "0xOrderbookAddress"
    }
  ],
  "pagination": {
    "page": 1,
    "pageSize": 10,
    "totalOrders": 25,
    "totalPages": 3,
    "hasMore": true
  }
}
```

`maxOutput` is the quote-derived executable output amount for the listed order.
It is `null` when quote data is unavailable.

`orderType` is one of `limit`, `dca`, `dynamic-spread`, or `custom`.

When `state=inactive`, orders are returned without live quote data: `ioRatio` is
`"-"`, `maxOutput` is `null`, and `outputVaultBalance` is `"0"`. `chainId`,
`orderBytes`, token refs, `orderType`, `active`, and `removedAt` remain
populated when available.

## List Orders by Token

```
GET /v1/orders/token/{address}
```

Paginated list of orders for a token address.

### Request

```bash
curl "https://api.st0x.io/v1/orders/token/0xTokenAddress?state=all&side=output&page=1&pageSize=10" \
  -H "Authorization: Basic <credentials>"
```

| Parameter  | Type                           | Default  | Description                          |
| ---------- | ------------------------------ | -------- | ------------------------------------ |
| `state`    | `active`, `inactive`, or `all` | `active` | Filter by current order state        |
| `side`     | `input` or `output`            | all      | Match token as an input/output token |
| `page`     | number                         | 1        | Page number                          |
| `pageSize` | number                         | 20       | Results per page                     |

The response shape is the same as list orders by owner.

## Query Orders by Token Set

```
POST /v1/orders/query
```

Queries one network and one canonical token set through a single indexed SDK
query. This is the preferred contract for network-wide orderbook views.

```bash
curl -X POST https://api.st0x.io/v1/orders/query \
  -H "Authorization: Basic <credentials>" \
  -H "Content-Type: application/json" \
  -d '{
    "chainId": 8453,
    "tokenAddresses": [
      "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
      "0x4200000000000000000000000000000000000006"
    ],
    "state": "active",
    "side": "output",
    "page": 1,
    "pageSize": 50,
    "denomination": "wrapped"
  }'
```

| Field              | Type                           | Default   | Bounds and behavior                                                               |
| ------------------ | ------------------------------ | --------- | --------------------------------------------------------------------------------- |
| `chainId`          | number                         | required  | Must have a Raindex/orderbook in the active registry                              |
| `tokenAddresses`   | string[]                       | `[]`      | At most 64; normalized and deduplicated; required unless `orderHash` is supplied  |
| `ownerAddresses`   | string[]                       | `[]`      | Optional SDK owner filter; at most 64; normalized and deduplicated                |
| `raindexAddresses` | string[]                       | `[]`      | Optional SDK contract filter; at most 64; normalized and deduplicated             |
| `orderHash`        | string                         | -         | Optional exact hash. The pinned SDK supports one exact order hash, not a hash set |
| `state`            | `active`, `inactive`, or `all` | `active`  | Same state semantics as the existing order list routes                            |
| `side`             | `input` or `output`            | all sides | Match the canonical token set on the selected side                                |
| `page`             | number                         | 1         | 1 through 1000                                                                    |
| `pageSize`         | number                         | 20        | 1 through 50                                                                      |
| `denomination`     | `wrapped` or `unwrapped`       | `wrapped` | Same amount and IO-ratio denomination semantics as the existing order list routes |

The response is `OrdersListResponse`, the same stable REST-owned shape used by
the existing list routes. Results are deduplicated by network, Raindex contract,
and order hash, then ordered by `createdAt` descending with order hash and
Raindex contract tie-breakers.

Canonical address sets share the short-lived response cache regardless of input
order, duplicate entries, or address case. Concurrent identical cold requests
share one computation. The endpoint only caches complete responses: an indexed
query, denomination conversion, or live quote failure fails the whole request
and is not cached.

## List Orders by Transaction

```
GET /v1/orders/tx/{tx_hash}
```

Get all orders created in a specific transaction.

### Request

```bash
curl https://api.st0x.io/v1/orders/tx/0xTxHash... \
  -H "Authorization: Basic <credentials>"
```

### Response

```json
{
  "txHash": "0xTxHash...",
  "blockNumber": 12345678,
  "timestamp": 1708000000,
  "orders": [
    {
      "orderHash": "0xabc123...",
      "owner": "0xOwnerAddress",
      "orderbookId": "0xOrderbookAddress",
      "inputToken": { "address": "0x...", "symbol": "USDC", "decimals": 6 },
      "outputToken": { "address": "0x...", "symbol": "WETH", "decimals": 18 }
    }
  ]
}
```

## Cancel an Order

```
POST /v1/order/cancel
```

Returns calldata for cancelling an order and withdrawing from its vaults.

### Request

```bash
curl -X POST https://api.st0x.io/v1/order/cancel \
  -H "Authorization: Basic <credentials>" \
  -H "Content-Type: application/json" \
  -d '{
    "orderHash": "0xabc123..."
  }'
```

### Response

```json
{
  "transactions": [
    {
      "to": "0xOrderbookAddress",
      "data": "0x...",
      "value": "0x0"
    },
    {
      "to": "0xOrderbookAddress",
      "data": "0x...",
      "value": "0x0"
    }
  ],
  "summary": {
    "vaultsToWithdraw": 2,
    "tokensReturned": [
      { "token": "0x...", "symbol": "USDC", "amount": "8000.0" },
      { "token": "0x...", "symbol": "WETH", "amount": "0.5" }
    ]
  }
}
```

Execute each transaction in the `transactions` array sequentially. The `summary`
shows what tokens you will receive back.
