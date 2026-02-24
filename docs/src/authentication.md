# Authentication

All endpoints except `/health` require HTTP Basic Authentication.

## Format

The `Authorization` header uses the standard Basic scheme:

```
Authorization: Basic <base64(key_id:secret)>
```

Where `key_id:secret` is your API key ID and secret separated by a colon, Base64-encoded.

## Example

If your key ID is `abc123` and your secret is `mysecret`:

```bash
# Encode credentials
echo -n "abc123:mysecret" | base64
# Output: YWJjMTIzOm15c2VjcmV0

# Use in a request
curl https://api.st0x.io/v1/tokens \
  -H "Authorization: Basic YWJjMTIzOm15c2VjcmV0"
```

Most HTTP client libraries handle Basic Auth natively. For example, with curl's `-u` flag:

```bash
curl -u "abc123:mysecret" https://api.st0x.io/v1/tokens
```

## Error Responses

| Status | Code | When |
|--------|------|------|
| 401 | `UNAUTHORIZED` | Missing or invalid credentials |

```json
{
  "error": {
    "code": "UNAUTHORIZED",
    "message": "Invalid API key or secret"
  }
}
```
