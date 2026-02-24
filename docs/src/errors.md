# Error Handling

All error responses follow a consistent format.

## Error Response Format

```json
{
  "error": {
    "code": "ERROR_CODE",
    "message": "Human-readable description of what went wrong"
  }
}
```

## Error Codes

| HTTP Status | Code | Description |
|-------------|------|-------------|
| 400 | `BAD_REQUEST` | Invalid request body, missing fields, or malformed parameters |
| 401 | `UNAUTHORIZED` | Missing or invalid authentication credentials |
| 403 | `FORBIDDEN` | Valid credentials but insufficient permissions |
| 404 | `NOT_FOUND` | Requested resource does not exist |
| 429 | `RATE_LIMITED` | Too many requests — see [Rate Limiting](./rate-limiting.md) |
| 500 | `INTERNAL_ERROR` | Unexpected server error |

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
  "error": {
    "code": "NOT_FOUND",
    "message": "Order not found"
  }
}
```

### Rate Limited

```json
{
  "error": {
    "code": "RATE_LIMITED",
    "message": "Rate limit exceeded"
  }
}
```

Rate-limited responses include a `Retry-After: 60` header indicating how many seconds to wait.
