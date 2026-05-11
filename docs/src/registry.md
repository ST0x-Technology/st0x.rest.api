# Registry

The registry metadata identifies the token and orderbook configuration currently active in the API.
Private registry artifacts are secret-bearing, so the API returns source metadata and the artifact
SHA-256 instead of returning the raw registry `data:` URI.

## Get Registry Metadata

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
  "registry_type": "private_artifact",
  "source_commit": "fb6b06ea12c941157000d60621184d2f99b55f71",
  "payload_sha256": "9f...",
  "changed_at": "2026-05-11 10:30:00"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `registry_type` | string | `private_artifact` for uploaded private artifacts or `public_url` for legacy public URL configuration |
| `source_commit` | string \| null | 40-character source commit SHA that produced the active artifact |
| `payload_sha256` | string \| null | SHA-256 of the uploaded registry artifact |
| `changed_at` | string \| null | Time the artifact was accepted |
