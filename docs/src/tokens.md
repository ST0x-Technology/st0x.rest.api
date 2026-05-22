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

## Wrapped Token Exchange Rates

```
GET /v1/tokens/exchange-rates
```

Returns the current ERC4626 `assetsPerShare` rate for every wrapped (`wt*`)
st0x token in the registry. `assetsPerShare` is the multiplier that converts
between wrapped (wtStock) and underlying (tStock) units:

```
tStockAmount = wtStockAmount * assetsPerShare
```

Wrapped tokens are identified by the `extensions.category = "ST0x"` flag in
the registry's token-list metadata.

Rates are cached for 24 hours and persisted to a snapshot history so the
`/v1/trades/*` endpoints can convert past trades using the rate that was
current at each trade's block. See [Trade Monitoring](./trades.md) for the
`denomination=tstock` query toggle.

### Data source

The API reads `assetsPerShare` directly from the ERC4626 vault contract via
`convertToAssets(10^decimals)` against the registry-declared RPC. Neither
the rain.orderbook subgraph nor the st0x.oracle subgraph indexes ERC4626
share/asset accounting, so RPC is the only authoritative source. The first
RPC in the token's network configuration is used; additional RPCs serve as
fallbacks on transport or `eth_call` errors. Each successful read is
persisted to `wrapped_exchange_rate_snapshots` along with the chain head
block number, so historical lookups (the `tstock` denomination toggle) can
find the rate that was live at any past trade's block.

### Request

```bash
curl https://api.st0x.io/v1/tokens/exchange-rates \
  -H "Authorization: Basic <credentials>"
```

### Response

```json
[
  {
    "share": {
      "address": "0xff05e1bd696900dc6a52ca35ca61bb1024eda8e2",
      "symbol": "wtMSTR",
      "decimals": 18
    },
    "asset": {
      "address": "0x013b782f402d61aa1004cca95b9f5bb402c9d5fe",
      "symbol": "tMSTR",
      "decimals": 18
    },
    "assetsPerShare": "1.04",
    "blockNumber": 12345678,
    "blockTimestamp": 1718452800,
    "capturedAt": "2026-05-22 09:32:11"
  }
]
```

### Fields

| Field | Type | Description |
|-------|------|-------------|
| `share` | object | The wrapped ERC4626 share token (`wt*`). |
| `asset` | object | The underlying tStock asset. Address comes from the wrapped token's `extensions.unwrappedAddress` in the registry; `symbol` falls back to stripping the leading `w` from the wrapped symbol when the registry doesn't supply one. |
| `assetsPerShare` | string | Decimal multiplier from share → asset. `1.0` means redemption is 1:1. |
| `blockNumber` | number | Chain head at the moment the rate was read. |
| `blockTimestamp` | number | Unix seconds at observation. |
| `capturedAt` | string | ISO-8601 timestamp recording when the API persisted this snapshot. |

