# Market Prices

The API samples the executable ST0x order book once per minute. For each token,
the market price is the midpoint of the highest bid and lowest ask:

```
midpoint = (bestBid + bestAsk) / 2
```

A sample is stored only when both sides are positive and the book is not
crossed. When the market is closed or one side is unavailable, the most recent
valid sample remains available as a cached price. Samples older than seven days
are deleted.

Price markets are discovered from the active registry. For every configured
network containing ST0x tokens, the token list must contain exactly one token
whose symbol is `USDC`; its registry address is used as that network's quote
token. No chain IDs or quote-token addresses are configured separately in the
service.

The asset unit is the canonical wrapped ST0x share returned in `assetAddress`.
Orders using the underlying asset are converted with the current share's
ERC-4626 assets-per-share ratio. Legacy wrapped shares are converted through
their own ratio into the current canonical share denomination before all orders
are combined. All decimal values are returned as strings so clients can choose
their required precision.

## Latest Prices

Authentication is required; see [Authentication](./authentication.md). Requests
without valid credentials return `401 Unauthorized`.

```
GET /v1/prices?chainId=8453
```

Returns every configured ST0x token on the requested network. If `chainId` is
omitted, prices from all registry networks are returned. Addresses are canonical
lowercase wrapped token addresses. Tokens without a retained sample have
`source: "unavailable"` and null price fields.

The sampler only queries networks with a configured Raindex/orderbook. Tokens on
standalone registry networks are still included in this response, but remain
`unavailable` until that network has an orderbook-backed price source.

```json
{
  "data": [
    {
      "chainId": 8453,
      "assetAddress": "0xfb5b41acdba20a3230f84be995173cfb98b8d6e7",
      "symbol": "wtNVDA",
      "quoteAddress": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
      "bestBid": "123.4",
      "bestAsk": "123.6",
      "midpoint": "123.5",
      "source": "live",
      "observedAt": 1784800000,
      "change24hPercent": "1.42"
    }
  ]
}
```

`source` is:

- `live` when the most recent observation is no more than two sample intervals
  old.
- `cached` when a retained observation exists but has not refreshed.
- `unavailable` when no retained observation exists.

`change24hPercent` is null until a sample at least 24 hours older exists.

## Prices At A Timestamp

This endpoint has the same [authentication](./authentication.md) requirement and
may return `401 Unauthorized`.

```
GET /v1/prices?chainId=8453&at=1784800000
```

Returns the nearest retained observation at or before `at` for every token.
Returned observations use `source: "historical"`. The API does not retrieve or
regenerate prices older than its retention window.

## Token Price History

This endpoint has the same [authentication](./authentication.md) requirement and
may return `401 Unauthorized`.

```
GET /v1/prices/{address}/history
```

`{address}` can use any casing and can be the current wrapped, unwrapped, or
legacy address. The response always identifies the canonical current wrapped
token.

Query parameters:

| Field       | Type   | Default               | Description                                       |
| ----------- | ------ | --------------------- | ------------------------------------------------- |
| `chainId`   | number | only configured chain | Required when the registry has multiple networks  |
| `startTime` | number | retention cutoff      | Inclusive Unix timestamp                          |
| `endTime`   | number | current time          | Inclusive Unix timestamp                          |
| `interval`  | number | sample interval       | Seconds per output bucket; the last point is kept |

Requests before the retention cutoff are clamped to the retained window. For
unusually large configured retention windows, the API raises the effective
interval as needed to cap an inclusive response at 10,081 points (seven days of
minute boundaries) and returns that effective value in `interval`.

```json
{
  "chainId": 8453,
  "assetAddress": "0xfb5b41acdba20a3230f84be995173cfb98b8d6e7",
  "symbol": "wtNVDA",
  "quoteAddress": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
  "startTime": 1784195200,
  "endTime": 1784800000,
  "interval": 900,
  "points": [
    {
      "bestBid": "123.4",
      "bestAsk": "123.6",
      "midpoint": "123.5",
      "observedAt": 1784800000
    }
  ]
}
```
