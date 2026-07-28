# Error Handling

All error responses follow a consistent format.

## Error Response Format

```json
{
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "error": {
    "code": "ERROR_CODE",
    "message": "Human-readable description of what went wrong"
  }
}
```

`code` is a stable machine-readable identifier. Once published, a code is never
reused for another meaning. `request_id` identifies one concrete request and is
also returned in the `X-Request-Id` response header and included in structured
request logs.

API consumers should branch on `code`, not `message`. Messages may be reworded
without changing the code's meaning and should not be displayed directly to end
users. Domain-coded server failures use static public summaries; detailed
dependency errors remain in structured logs.

## Error Codes

| HTTP Status | Code                   | Description                                                   |
| ----------- | ---------------------- | ------------------------------------------------------------- |
| 202         | `NOT_YET_INDEXED`      | The requested transaction has not been indexed yet            |
| 400         | `BAD_REQUEST`          | Invalid request body, missing fields, or malformed parameters |
| 401         | `UNAUTHORIZED`         | Missing or invalid authentication credentials                 |
| 403         | `FORBIDDEN`            | Authenticated caller does not have permission                 |
| 404         | `NOT_FOUND`            | Requested resource does not exist                             |
| 422         | `UNPROCESSABLE_ENTITY` | Request body could not be parsed                              |
| 429         | `RATE_LIMITED`         | Too many requests — see [Rate Limiting](./rate-limiting.md)   |
| 500         | `INTERNAL_ERROR`       | Unexpected server error                                       |

### Reserved trade-flow codes

The following stable codes are reserved by the error contract. Individual trade
endpoints begin emitting them as their failure boundaries are migrated;
consumers must continue to handle the generic fallback codes during rollout.

| HTTP Status | Code                     | Description                                    |
| ----------- | ------------------------ | ---------------------------------------------- |
| 400         | `SWAP_UNSUPPORTED_TOKEN` | One or both swap tokens are unsupported        |
| 400         | `SWAP_PREFLIGHT_FAILED`  | Swap preflight rejected the proposed execution |
| 404         | `SWAP_NO_LIQUIDITY`      | No executable liquidity is available           |
| 500         | `SWAP_QUOTE_FAILED`      | Quote generation failed unexpectedly           |
| 500         | `SWAP_CALLDATA_FAILED`   | Calldata generation failed unexpectedly        |
| 502         | `ORDERS_QUERY_FAILED`    | The order source could not serve the request   |
| 503         | `UPSTREAM_UNAVAILABLE`   | A required upstream dependency is unavailable  |

Domain codes are assigned at the boundary where the failure is understood.
Internal dependency errors are logged with the same code and `request_id`; raw
dependency details are not returned in domain-coded response bodies.

## Examples

### Bad Request

```bash
curl -X POST https://api.st0x.io/v1/swap/quote \
  -H "Authorization: Basic <credentials>" \
  -H "Content-Type: application/json" \
  -d '{}'
```

```json
{
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "error": {
    "code": "BAD_REQUEST",
    "message": "Missing required field: inputToken"
  }
}
```

### Not Found

```bash
curl https://api.st0x.io/v1/order/0xinvalidhash \
  -H "Authorization: Basic <credentials>"
```

```json
{
  "request_id": "550e8400-e29b-41d4-a716-446655440001",
  "error": {
    "code": "NOT_FOUND",
    "message": "Order not found"
  }
}
```

### Rate Limited

```json
{
  "request_id": "550e8400-e29b-41d4-a716-446655440002",
  "error": {
    "code": "RATE_LIMITED",
    "message": "Rate limit exceeded"
  }
}
```

Rate-limited responses include a `Retry-After: 60` header indicating how many
seconds to wait.
