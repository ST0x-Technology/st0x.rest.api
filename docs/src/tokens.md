# Tokens

Discover available tokens before making swaps or deploying orders.

## List Tokens

```
GET /v1/tokens
```

Returns all tokens supported on the Base network.

### Request

```bash
curl https://api.st0x.io/v1/tokens \
  -H "Authorization: Basic <credentials>"
```

### Response

```json
{
  "tokens": [
    {
      "address": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
      "symbol": "USDC",
      "name": "USD Coin",
      "ISIN": "US0000000001",
      "decimals": 6
    },
    {
      "address": "0x4200000000000000000000000000000000000006",
      "symbol": "WETH",
      "name": "Wrapped Ether",
      "decimals": 18
    }
  ]
}
```

### Fields

| Field      | Type              | Description                                  |
| ---------- | ----------------- | -------------------------------------------- |
| `address`  | string            | Token contract address on Base               |
| `symbol`   | string            | Token ticker symbol                          |
| `name`     | string            | Full token name                              |
| `ISIN`     | string (optional) | ISIN identifier, omitted when not applicable |
| `decimals` | number            | Token decimal places                         |

Use the `address` field when specifying tokens in swap and order requests.

## Wrapped Token Ratios

```
GET /v1/tokens/wrap-ratio
GET /v1/tokens/wrap-ratio/{address}
```

Returns ERC4626 wrapped-token ratios for registry tokens where
`extensions.category` is `ST0x`. For the single-token endpoint, `{address}` must
be the wrapped token / ERC4626 vault address. Symbol lookup is not supported.

### Batch Response

The batch endpoint returns successful ratios in `data` and per-token failures in
`errors`. One failed token does not fail the whole response.

```json
{
  "data": [
    {
      "shareAddress": "0xff05e1bd696900dc6a52ca35ca61bb1024eda8e2",
      "assetAddress": "0x013b782f402d61aa1004cca95b9f5bb402c9d5fe",
      "assetsPerShare": "1.0",
      "blockNumber": 123456789,
      "blockTimestamp": 1717351200,
      "capturedAt": "1717351201"
    }
  ],
  "errors": [
    {
      "shareAddress": "0x1111111111111111111111111111111111111111",
      "message": "failed to read ERC4626 ratio"
    }
  ]
}
```

### Fields

| Field            | Type           | Description                                                                                           |
| ---------------- | -------------- | ----------------------------------------------------------------------------------------------------- |
| `shareAddress`   | string         | Wrapped wtStock / ERC4626 vault token address                                                         |
| `assetAddress`   | string         | Unwrapped tStock asset address from `extensions.unwrappedAddress`, verified against ERC4626 `asset()` |
| `assetsPerShare` | string         | Assets returned by `convertToAssets(1 * 10^shareDecimals)`                                            |
| `blockNumber`    | number         | Block number used for the ERC4626 batch read                                                          |
| `blockTimestamp` | number or null | Block timestamp when available from the RPC                                                           |
| `capturedAt`     | string         | Unix timestamp when the SDK captured the batch response                                               |
