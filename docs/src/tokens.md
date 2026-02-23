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

| Field | Type | Description |
|-------|------|-------------|
| `address` | string | Token contract address on Base |
| `symbol` | string | Token ticker symbol |
| `name` | string | Full token name |
| `ISIN` | string (optional) | ISIN identifier, omitted when not applicable |
| `decimals` | number | Token decimal places |

Use the `address` field when specifying tokens in swap and order requests.
