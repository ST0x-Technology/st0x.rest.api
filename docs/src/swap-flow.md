# Swap Flow

Swapping is a two-step process: get a **quote** to preview pricing, then get
**calldata** to build the on-chain transaction.

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
    "outputAmount": "1.0",
    "denomination": "wrapped"
  }'
```

| Field          | Type   | Description                                                                                                                                                                   |
| -------------- | ------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `inputToken`   | string | Address of the token you are selling                                                                                                                                          |
| `outputToken`  | string | Address of the token you want to receive                                                                                                                                      |
| `outputAmount` | string | Desired output amount (human-readable, e.g. `"1.0"` for 1 WETH)                                                                                                               |
| `denomination` | string | Optional. `"wrapped"` (default) returns orderbook-denominated values. `"unwrapped"` returns normalized display values for wrapped ST0x/ERC4626 tokens after quote simulation. |

### Response

```json
{
  "inputToken": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
  "outputToken": "0x4200000000000000000000000000000000000006",
  "outputAmount": "1.0",
  "denomination": "wrapped",
  "estimatedOutput": "1.0",
  "estimatedInput": "2500.0",
  "estimatedIoRatio": "2500.0"
}
```

| Field              | Type   | Description                                                                       |
| ------------------ | ------ | --------------------------------------------------------------------------------- |
| `denomination`     | string | Denomination used for `estimatedOutput`, `estimatedInput`, and `estimatedIoRatio` |
| `estimatedOutput`  | string | Expected output amount                                                            |
| `estimatedInput`   | string | Expected input amount required                                                    |
| `estimatedIoRatio` | string | Input-to-output ratio                                                             |

The quote reflects current orderbook state. Prices may change between quoting
and execution.

When `denomination` is omitted or set to `"wrapped"`, quote values use the
wrapped/orderbook token units required by the swap endpoints. When
`denomination` is `"unwrapped"`, the API still simulates against the
wrapped/orderbook `inputToken` and `outputToken` addresses, then normalizes only
the displayed `estimatedInput`, `estimatedOutput`, and `estimatedIoRatio` for
wrapped ST0x/ERC4626 tokens. `outputAmount` remains in `outputToken` units and
is not pre-converted.

Do not pass unwrapped-normalized quote values into other endpoints unless those
endpoints explicitly support `denomination=unwrapped` and you call them that
way. `/v1/swap/calldata` supports the same `denomination` field, but the swap
still purchases wrapped/orderbook tokens.

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
    "maximumIoRatio": "2600.0",
    "denomination": "wrapped"
  }'
```

| Field            | Type   | Description                                                                                                                                                                     |
| ---------------- | ------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `taker`          | string | Your wallet address that will execute the transaction                                                                                                                           |
| `inputToken`     | string | Wrapped/orderbook token address you are selling                                                                                                                                 |
| `outputToken`    | string | Wrapped/orderbook token address you want to receive                                                                                                                             |
| `outputAmount`   | string | Desired output amount in the selected `denomination`                                                                                                                            |
| `maximumIoRatio` | string | Maximum acceptable IO ratio in the selected `denomination`                                                                                                                      |
| `denomination`   | string | Optional. `"wrapped"` (default) uses orderbook units. `"unwrapped"` interprets `outputAmount` and `maximumIoRatio` as unwrapped display values for wrapped ST0x/ERC4626 tokens. |

Set `maximumIoRatio` slightly above the `estimatedIoRatio` from the quote to
allow for price movement.

For calldata, `denomination=unwrapped` only changes how numeric fields are
interpreted and displayed. The API converts `outputAmount` and `maximumIoRatio`
to wrapped/orderbook units before generating calldata. Clients must still pass
the wrapped/orderbook token addresses for `inputToken` and `outputToken`; the
endpoint does not translate unwrapped asset addresses.

### Response

The response always includes all fields, but the content depends on whether your
`taker` address has sufficient token approvals.

**If approvals are needed**, `data` is empty and `approvals` contains the
required transactions:

```json
{
  "to": "0xOrderbookContractAddress",
  "data": "0x",
  "value": "0x0",
  "estimatedInput": "2500.0",
  "denomination": "wrapped",
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

**If approvals are already in place**, `approvals` is empty and `data` contains
the swap calldata:

```json
{
  "to": "0xOrderbookContractAddress",
  "data": "0xabcdef...",
  "value": "0x0",
  "estimatedInput": "2500.0",
  "denomination": "wrapped",
  "approvals": []
}
```

| Field            | Type   | Description                                                                        |
| ---------------- | ------ | ---------------------------------------------------------------------------------- |
| `to`             | string | Contract address to send the transaction to                                        |
| `data`           | string | Encoded transaction calldata — empty (`"0x"`) when approvals are needed            |
| `value`          | string | Native token value to send (usually `"0x0"`)                                       |
| `estimatedInput` | string | Expected input amount in the requested `denomination`                              |
| `denomination`   | string | Denomination used for `estimatedInput`                                             |
| `approvals`      | array  | Token approvals needed — if non-empty, approve first then call this endpoint again |

Approval entries always describe the actual on-chain approval requirements in
wrapped/orderbook token units. They are not converted or relabeled when
`denomination=unwrapped`.

## Step 3: Handle Approvals

If the `approvals` array is **not empty**, send the approval transactions first:

1. For each approval, send a transaction to the `token` address with
   `approvalData` as calldata
2. Wait for confirmation
3. **Call the calldata endpoint again** — with approvals in place, the response
   will now contain the swap calldata

## Step 4: Execute the Swap

Once you receive a response with an empty `approvals` array, send the main
transaction using `to`, `data`, and `value`.

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

# 3. Check if approvals are needed
#    The first response only contains approvals — "data" will be empty ("0x").
#    You must send the approval transactions on-chain first, then call
#    the calldata endpoint again to get the actual swap calldata.
APPROVALS=$(echo "$CALLDATA" | jq '.approvals')
if [ "$APPROVALS" != "[]" ]; then
  # Send each approval transaction on-chain...
  # (use approvalData from each entry as calldata to the token address)

  # Now call the calldata endpoint again — this time approvals are in place
  # and the response will contain the swap calldata in "data"
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
fi

# 4. Execute the swap transaction using to, data, and value from the response
echo "$CALLDATA" | jq '{to, data, value}'
```
