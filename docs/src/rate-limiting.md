# Rate Limiting

The API enforces two independent rate limits to ensure fair usage.

## Limits

| Limit | Scope | Description |
|-------|-------|-------------|
| **Global** | All requests combined | Total requests per minute across all API keys |
| **Per-key** | Individual API key | Requests per minute for a single API key |

Both limits use a 60-second sliding window.

## Response Headers

Every authenticated response includes rate limit headers for your API key:

```
X-RateLimit-Limit: 60
X-RateLimit-Remaining: 45
X-RateLimit-Reset: 1708010060
```

| Header | Description |
|--------|-------------|
| `X-RateLimit-Limit` | Maximum requests allowed per window |
| `X-RateLimit-Remaining` | Requests remaining in the current window |
| `X-RateLimit-Reset` | Unix timestamp when the window resets |

## When Rate Limited

If you exceed either limit, you receive a `429` response:

```json
{
  "error": {
    "code": "RATE_LIMITED",
    "message": "Rate limit exceeded"
  }
}
```

The response includes a `Retry-After` header:

```
Retry-After: 60
```

## Best Practices

- Monitor the `X-RateLimit-Remaining` header and back off as it approaches zero
- Implement exponential backoff on `429` responses
- Cache quote responses when possible to reduce unnecessary calls
- Batch monitoring queries rather than polling individual orders frequently
