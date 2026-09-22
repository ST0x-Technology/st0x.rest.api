# Introduction

The st0x API powers the [st0x platform](https://platform.st0x.io/), providing
programmatic access to swap, trade, and manage tokenized assets across the
networks configured in the live registry. Through the API you can:

- Discover available tokens
- Get swap quotes and generate transaction calldata
- Deploy and manage DCA orders
- Monitor trades

**Base URL:** `https://api.st0x.io`

**Full API Reference:** The interactive Swagger UI is available at
[https://api.st0x.io/swagger/](https://api.st0x.io/swagger/) with complete
request/response schemas for every endpoint.

This guide focuses on the typical workflows and how the endpoints connect.

Responses identify their network with `chainId`. V2 list endpoints accept it as
an optional filter; address- and transaction-scoped endpoints use it to select
the network. Existing V1 requests that omit it retain their Base behavior. V3
swap endpoints require it and never infer a network.
