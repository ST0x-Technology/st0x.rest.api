# Registry

The registry URL points to the token and orderbook configuration used by the API.

## Get Registry URL

```
GET /registry
```

### Request

```bash
curl https://api.st0x.io/registry \
  -H "Authorization: Basic <credentials>"
```

### Response

```json
{
  "registry_url": "https://raw.githubusercontent.com/..."
}
```

| Field | Type | Description |
|-------|------|-------------|
| `registry_url` | string | URL of the active registry configuration |
