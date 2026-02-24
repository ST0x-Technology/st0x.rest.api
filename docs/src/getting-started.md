# Getting Started

## Prerequisites

1. An API key (contact the st0x team to obtain one)
2. A tool that can make HTTP requests (curl, Postman, or any HTTP client library)

## Base URL

All endpoints are served from:

```
https://api.st0x.io
```

## Quick Test

Verify the API is running:

```bash
curl https://api.st0x.io/health
```

```json
{
  "status": "ok"
}
```

The health endpoint is the only public endpoint — all other requests require authentication.

## First Authenticated Request

Encode your credentials as `key_id:secret` in Base64:

```bash
echo -n "your_key_id:your_secret" | base64
```

Then make an authenticated request to list available tokens:

```bash
curl https://api.st0x.io/v1/tokens \
  -H "Authorization: Basic <base64_credentials>"
```

See [Authentication](./authentication.md) for details on the auth format.

## Typical Workflow

A common integration flow looks like this:

1. **List tokens** — `GET /v1/tokens` to discover available trading pairs
2. **Get a quote** — `POST /v1/swap/quote` to see estimated pricing
3. **Get calldata** — `POST /v1/swap/calldata` to generate the transaction
4. **Handle approvals** — If the response includes `approvals`, send those transactions on-chain, then call the calldata endpoint again
5. **Execute the swap** — Once approvals are in place, the calldata response contains the transaction to submit on-chain

You can also create orders and monitor their trades. Like swaps, the order endpoints return calldata that you execute on-chain yourself:

1. **Get order calldata** — `POST /v1/order/dca`
2. **Handle approvals** — If approvals are returned, send them on-chain, then call the endpoint again to get the deployment calldata
3. **Monitor** — `GET /v1/order/{order_hash}` for status, `GET /v1/trades/{address}` for fills
4. **Cancel** — `POST /v1/order/cancel` to get cancellation calldata, then execute on-chain

Each of these flows is covered in detail in the following sections.
