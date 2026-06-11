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

## Token Proofs

```
GET /v1/tokens/{address}/proofs
```

Returns the raw proof data for a supported ST0x token. `{address}` can be the
current wrapped token address, the token's `extensions.unwrappedAddress`, or its
`extensions.legacyAddress` when one is present. The response always normalizes
`address` to the current wrapped token address from the registry.

The API returns raw hex strings from the configured subgraphs. It does not strip
Rain metadata prefixes, decode CBOR, or parse schema hashes.

### Request

```bash
curl https://api.st0x.io/v1/tokens/0xff05e1bd696900dc6a52ca35ca61bb1024eda8e2/proofs \
  -H "Authorization: Basic <credentials>"
```

### Response

```json
{
  "address": "0xff05e1bd696900dc6a52ca35ca61bb1024eda8e2",
  "metadata": [
    {
      "id": "0xmeta-id",
      "meta": "0x...",
      "sender": "0xsender",
      "subject": "0x000000000000000000000000ff05e1bd696900dc6a52ca35ca61bb1024eda8e2",
      "metaHash": "0xhash"
    }
  ],
  "schemas": [
    {
      "id": "vault-info-id",
      "information": "0x...",
      "timestamp": 1717351200
    }
  ],
  "receipts": [
    {
      "id": "receipt-info-id",
      "receiptId": "1",
      "txHash": "0xtxhash",
      "type": "deposit",
      "information": "0x...",
      "timestamp": 1717351200
    }
  ]
}
```

### Fields

| Field         | Type   | Description                                                                  |
| ------------- | ------ | ---------------------------------------------------------------------------- |
| `address`     | string | Current wrapped ST0x token address from the registry                         |
| `metadata`    | array  | Raw `metaV1S` rows for the wrapped token subject from the metaboard subgraph |
| `schemas`     | array  | Raw `receiptVaultInformations` rows from the SFT subgraph                    |
| `receipts`    | array  | Flattened deposit and withdraw receipt information rows                      |
| `type`        | string | Receipt event type: `deposit` or `withdraw`                                  |
| `information` | string | Raw hex information field from the subgraph                                  |
| `timestamp`   | number | Subgraph timestamp parsed as a number                                        |

### Not Found Responses

The endpoint returns `404` when:

- The address is not a supported ST0x token, unwrapped address, or legacy
  address.
- The SFT subgraph has no vault for the normalized wrapped token address.

When the vault exists, empty subgraph collections are returned as empty arrays.

### Registry Configuration

Proofs use the Raindex YAML loaded from the active Dotrain registry at API
startup. The API expects the SFT subgraph key to be `sft-{network}` under
`subgraphs`, for example `subgraphs.sft-base`. The metadata source uses the
network key under `metaboards`, for example `metaboards.base`.
