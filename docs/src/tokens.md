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
[
  {
    "key": "usdc",
    "network": {
      "key": "base",
      "rpcs": [],
      "chainId": 8453,
      "label": "Base",
      "networkId": null,
      "currency": "ETH"
    },
    "address": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
    "decimals": 6,
    "label": "USD Coin",
    "symbol": "USDC",
    "logo-uri": null,
    "extensions": {
      "isin": "US0000000001"
    },
    "name": "USD Coin",
    "isin": "US0000000001"
  },
  {
    "key": "weth",
    "network": {
      "key": "base",
      "rpcs": [],
      "chainId": 8453,
      "label": "Base",
      "networkId": null,
      "currency": "ETH"
    },
    "address": "0x4200000000000000000000000000000000000006",
    "decimals": 18,
    "label": "Wrapped Ether",
    "symbol": "WETH",
    "logo-uri": null,
    "extensions": null,
    "name": "Wrapped Ether",
    "isin": null
  }
]
```

### Fields

| Field | Type | Description |
|-------|------|-------------|
| `key` | string | Token key from the registry settings |
| `address` | string | Token contract address on Base |
| `symbol` | string | Token ticker symbol |
| `name` | string | Full token name |
| `isin` | string or null | ISIN identifier, when available |
| `decimals` | number | Token decimal places |
| `network` | object | Token network metadata with RPC URLs removed |
| `extensions` | object or null | Additional token metadata, when available |

Use the `address` field when specifying tokens in swap and order requests.
