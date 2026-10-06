# Market Prices

The API samples the executable ST0x order book once per minute. For each token,
the market price is the midpoint of the highest bid and lowest ask:

```
midpoint = (bestBid + bestAsk) / 2
```

A midpoint sample is stored only when both sides are positive and the book is
not crossed. Executable bids and asks are observed independently; an empty
output vault contributes no executable price on that side. When the market is
closed or one side is unavailable, the most recent valid sample remains
available as a cached price. Samples older than seven days are deleted.

Price markets are discovered from the active registry. For every configured
network containing ST0x tokens, the token list must identify exactly one quote
token. A token with `extensions.marketQuote: true` takes precedence; otherwise,
the API falls back to the token whose symbol is `USDC`. This allows networks to
use a non-USDC quote token without configuring chain IDs or quote-token
addresses separately in the service.

The asset unit is the canonical wrapped ST0x share returned in `assetAddress`.
Orders using the underlying asset are converted with the current share's
ERC-4626 assets-per-share ratio. Legacy wrapped shares are converted through
their own ratio into the current canonical share denomination before orders are
combined for display midpoints. The independent `executableBook` includes only
orders whose asset token is the canonical wrapped address, since swap requests
match exact token addresses and do not automatically wrap or migrate underlying
or legacy assets. Variant-only liquidity can therefore contribute a display
midpoint while the canonical executable book remains empty. All decimal values
are returned as strings so clients can choose their required precision.

## Latest Prices

Authentication is required; see [Authentication](./authentication.md). Requests
without valid credentials return `401 Unauthorized`.

```
GET /v2/prices?chainId=8453
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
      "change24hPercent": "1.42",
      "executableBook": {
        "bestBid": "123.4",
        "bestAsk": "123.6",
        "observedAt": 1784800000,
        "source": "live"
      }
    }
  ]
}
```

`source` is:

- `live` when the most recent observation is no more than two sample intervals
  old.
- `cached` when a retained observation exists but has not refreshed.
- `unavailable` when no retained observation exists.

`executableBook` is independent of the retained midpoint fields. Its bid and ask
are nullable, and either side can refresh while the other side has no funded
executable orders. Each successful complete sample replaces both sides, clearing
an earlier side when it disappears. Crossed books publish both sides as null.
The object is null before the first sample, after retention expiry, or until a
new sample after the canonical token changes. It persists across server
restarts. Its `source` is `live` only for a non-future observation no more than
two sample intervals old; otherwise it is `cached`. A live object with two null
sides means no executable side was observed, not a tradable price.

Trading clients should use a fresh positive `executableBook.bestAsk` for a buy
reference and the reciprocal of fresh positive `executableBook.bestBid` for a
sell reference (the API swap IO ratio is input tokens per output token). Both
book prices are quote units per canonical wrapped share; the sell reference is
the reciprocal bid, in wrapped shares per quote unit. Submit these references as
`referenceIoRatio` with `denomination: "wrapped"`. For
`denomination: "unwrapped"`, convert the reference first:
`unwrappedReference = wrappedReference * inputAssetsPerShare / outputAssetsPerShare`
(use 1 for a token without a wrapper). This accounts for each market's actual
spread; the display midpoint is not a directional trade price. Do not use
cached, future-dated, or missing sides as trading references. A book price does
not guarantee a requested trade size can fill, and a directional reference
protects against worsening execution rather than independently validating fair
market value. Normal quote limits and execution slippage still apply.

`change24hPercent` is null until a sample at least 24 hours older exists.

## Prices At A Timestamp

This endpoint has the same [authentication](./authentication.md) requirement and
may return `401 Unauthorized`.

```
GET /v2/prices?chainId=8453&at=1784800000
```

Returns the nearest retained observation at or before `at` for every token.
Returned observations use `source: "historical"` and `executableBook: null`. The
latest executable book is deliberately excluded from historical responses. The
API does not retrieve or regenerate prices older than its retention window.

## Token Price History

This endpoint has the same [authentication](./authentication.md) requirement and
may return `401 Unauthorized`.

```
GET /v2/prices/{address}/history
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
