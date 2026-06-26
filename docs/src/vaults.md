# Vaults

Read live orderbook deposit vault balances through the API instead of querying
the Raindex subgraphs directly.

Vault balances are returned as raw token-denominated integer strings. Use the
token `decimals` value to format them for display.

## List Vault Positions

```
GET /v1/vaults
```

Returns deposit vault positions for a wallet address.

### Request

```bash
curl "https://api.st0x.io/v1/vaults?owner=0xYourAddress&page=1&pageSize=100" \
  -H "Authorization: Basic <credentials>"
```

| Parameter         | Type    | Default | Description                               |
| ----------------- | ------- | ------- | ----------------------------------------- |
| `owner`           | string  | -       | Wallet address that owns the vaults       |
| `token`           | string  | -       | Optional token address filter             |
| `hideZeroBalance` | boolean | `false` | Hide vaults whose current balance is zero |
| `page`            | number  | `1`     | Page number                               |
| `pageSize`        | number  | `100`   | Results per page                          |

### Response

```json
{
  "vaults": [
    {
      "id": "0x...",
      "vaultId": "123",
      "owner": "0xYourAddress",
      "token": {
        "address": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
        "name": "USD Coin",
        "symbol": "USDC",
        "decimals": 6
      },
      "balance": "1000000",
      "orderbook": "0xd2938e7c9fe3597f78832ce780feb61945c377d7",
      "ordersAsInput": [{ "orderHash": "0x..." }],
      "ordersAsOutput": [{ "orderHash": "0x..." }]
    }
  ],
  "pagination": {
    "page": 1,
    "pageSize": 100,
    "totalItems": 1,
    "hasMore": false
  }
}
```

The pair of `vaultId` and `token.address` identifies the deposit vault for
client-side withdrawal flows. `orderbook` is the orderbook contract address to
use for transaction calldata.

## Vault Totals

```
GET /v1/vaults/totals
```

Returns non-zero deposit vault balances aggregated by token address.

### Request

```bash
curl https://api.st0x.io/v1/vaults/totals \
  -H "Authorization: Basic <credentials>"
```

### Response

```json
{
  "totals": [
    {
      "token": {
        "address": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
        "symbol": "USDC",
        "decimals": 6
      },
      "totalBalance": "42000000",
      "vaultCount": 42
    }
  ]
}
```

The API does not compute USD values. Use `totalBalance` with token decimals and
your pricing source to calculate UI TVL values.
