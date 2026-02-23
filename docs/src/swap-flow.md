# Swap Flow

Swapping is a two-step process: get a **quote** to preview pricing, then get **calldata** to build the on-chain transaction.

## Step 1: Get a Quote

```
POST /v1/swap/quote
```

### Request

```bash
curl -X POST https://api.st0x.io/v1/swap/quote \
  -H "Authorization: Basic <credentials>" \
  -H "Content-Type: application/json" \
  -d '{
    "inputToken": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
    "outputToken": "0x4200000000000000000000000000000000000006",
    "outputAmount": "1.0"
  }'
```

| Field | Type | Description |
|-------|------|-------------|
| `inputToken` | string | Address of the token you are selling |
| `outputToken` | string | Address of the token you want to receive |
| `outputAmount` | string | Desired output amount (human-readable, e.g. `"1.0"` for 1 WETH) |

### Response

```json
{
  "inputToken": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
  "outputToken": "0x4200000000000000000000000000000000000006",
  "outputAmount": "1.0",
  "estimatedOutput": "1.0",
  "estimatedInput": "2500.0",
  "estimatedIoRatio": "2500.0"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `estimatedOutput` | string | Expected output amount |
| `estimatedInput` | string | Expected input amount required |
| `estimatedIoRatio` | string | Input-to-output ratio |

The quote reflects current orderbook state. Prices may change between quoting and execution.

## Step 2: Get Calldata

```
POST /v1/swap/calldata
```

### Request

```bash
curl -X POST https://api.st0x.io/v1/swap/calldata \
  -H "Authorization: Basic <credentials>" \
  -H "Content-Type: application/json" \
  -d '{
    "taker": "0xYourWalletAddress",
    "inputToken": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
    "outputToken": "0x4200000000000000000000000000000000000006",
    "outputAmount": "1.0",
    "maximumIoRatio": "2600.0"
  }'
```

| Field | Type | Description |
|-------|------|-------------|
| `taker` | string | Your wallet address that will execute the transaction |
| `inputToken` | string | Address of the token you are selling |
| `outputToken` | string | Address of the token you want to receive |
| `outputAmount` | string | Desired output amount (human-readable) |
| `maximumIoRatio` | string | Maximum acceptable IO ratio (slippage protection) |

Set `maximumIoRatio` slightly above the `estimatedIoRatio` from the quote to allow for price movement.

### Response

```json
{
  "to": "0xOrderbookContractAddress",
  "data": "0x...",
  "value": "0x0",
  "estimatedInput": "2500.0",
  "approvals": [
    {
      "token": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
      "spender": "0xOrderbookContractAddress",
      "amount": "2500.0",
      "symbol": "USDC",
      "approvalData": "0x..."
    }
  ]
}
```

| Field | Type | Description |
|-------|------|-------------|
| `to` | string | Contract address to send the transaction to |
| `data` | string | Encoded transaction calldata |
| `value` | string | Native token value to send (usually `"0x0"`) |
| `estimatedInput` | string | Expected input amount |
| `approvals` | array | Token approvals required before executing |

## Step 3: Handle Approvals

If the `approvals` array is **not empty**, you must send approval transactions before the swap. Each approval grants the orderbook contract permission to spend your tokens.

For each approval:

1. Send a transaction to the `token` address with `approvalData` as calldata
2. Wait for confirmation

## Step 4: Execute the Swap

After approvals are confirmed, send the main transaction using `to`, `data`, and `value` from the calldata response.

## Complete Example

```bash
# 1. Get quote
QUOTE=$(curl -s -X POST https://api.st0x.io/v1/swap/quote \
  -H "Authorization: Basic <credentials>" \
  -H "Content-Type: application/json" \
  -d '{
    "inputToken": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
    "outputToken": "0x4200000000000000000000000000000000000006",
    "outputAmount": "1.0"
  }')

echo "$QUOTE" | jq .estimatedIoRatio

# 2. Get calldata (add some slippage to the IO ratio)
CALLDATA=$(curl -s -X POST https://api.st0x.io/v1/swap/calldata \
  -H "Authorization: Basic <credentials>" \
  -H "Content-Type: application/json" \
  -d '{
    "taker": "0xYourWalletAddress",
    "inputToken": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
    "outputToken": "0x4200000000000000000000000000000000000006",
    "outputAmount": "1.0",
    "maximumIoRatio": "2600.0"
  }')

# 3. Check approvals
echo "$CALLDATA" | jq .approvals

# 4. Send approval transactions (if any), then the main transaction
echo "$CALLDATA" | jq '{to, data, value}'
```
