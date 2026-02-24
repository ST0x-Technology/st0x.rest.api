# Order Management

Orders are persistent on-chain strategies that execute over time. The API supports two order types: **DCA** (Dollar Cost Averaging) and **Solver** orders.

The order endpoints return transaction calldata — the API does not execute transactions for you. You receive `to`, `data`, and `value` fields (plus any required token `approvals`) and submit those transactions on-chain yourself, the same pattern as the [Swap Flow](./swap-flow.md).

## Get DCA Order Calldata

```
POST /v1/order/dca
```

Returns calldata to deploy a DCA order that periodically buys a token at a set interval, with optional price bounds.

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

| Field | Type | Description |
|-------|------|-------------|
| `inputToken` | string | Token to spend |
| `outputToken` | string | Token to receive |
| `budgetAmount` | string | Total budget in human-readable units (e.g. `"10000"` for 10,000 USDC) |
| `period` | number | Time between executions |
| `periodUnit` | string | `"days"`, `"hours"`, or `"minutes"` |
| `startIo` | string | Starting IO ratio |
| `floorIo` | string | Minimum acceptable IO ratio |
| `inputVaultId` | string (optional) | Existing vault ID for input token |
| `outputVaultId` | string (optional) | Existing vault ID for output token |

### Response

The response always includes all fields. If approvals are needed, `data` is empty and `approvals` contains the required transactions:

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

Send each approval transaction on-chain, then call the endpoint again. Once approvals are in place, `approvals` is empty and `data` contains the deployment calldata:

```json
{
  "to": "0xOrderbookContractAddress",
  "data": "0xabcdef...",
  "value": "0x0",
  "approvals": []
}
```

## Get Solver Order Calldata

```
POST /v1/order/solver
```

Returns calldata to deploy a solver order that offers tokens at a fixed IO ratio.

### Request

```bash
curl -X POST https://api.st0x.io/v1/order/solver \
  -H "Authorization: Basic <credentials>" \
  -H "Content-Type: application/json" \
  -d '{
    "inputToken": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
    "outputToken": "0x4200000000000000000000000000000000000006",
    "amount": "5000",
    "ioRatio": "2500.0"
  }'
```

| Field | Type | Description |
|-------|------|-------------|
| `inputToken` | string | Token to spend |
| `outputToken` | string | Token to receive |
| `amount` | string | Amount to offer in human-readable units |
| `ioRatio` | string | Fixed input-to-output ratio |
| `inputVaultId` | string (optional) | Existing vault ID for input token |
| `outputVaultId` | string (optional) | Existing vault ID for output token |

The response has the same shape as the DCA response — calldata to execute on-chain.

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
GET /v1/orders/{address}
```

Paginated list of orders for a wallet address.

### Request

```bash
curl "https://api.st0x.io/v1/orders/0xOwnerAddress?page=1&pageSize=10" \
  -H "Authorization: Basic <credentials>"
```

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `page` | number | 1 | Page number |
| `pageSize` | number | 20 | Results per page |

### Response

```json
{
  "orders": [
    {
      "orderHash": "0xabc123...",
      "owner": "0xOwnerAddress",
      "inputToken": { "address": "0x...", "symbol": "USDC", "decimals": 6 },
      "outputToken": { "address": "0x...", "symbol": "WETH", "decimals": 18 },
      "outputVaultBalance": "0.5",
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

Execute each transaction in the `transactions` array sequentially. The `summary` shows what tokens you will receive back.
