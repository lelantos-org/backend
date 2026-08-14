# risk-webserver

Internal address screening API: "is this address banned / high risk?" Axum + Postgres, backed by the `screened_addresses` table.

**Read-only.** There is no write endpoint, which is what makes running it unauthenticated behind the gateway acceptable — network reach cannot be used to *remove* a sanctioned address. Screening is **fail-closed**: if the table cannot be read the request fails with 500 rather than reporting the address as clean.

Unlike the other webservers this one runs migrations at startup — no indexer touches `screened_addresses`, so nothing else would create it.

## Run

```bash
DATABASE_URL=postgres://... cargo run -p risk-webserver
```

## Env

| Var | Required | Default | Notes |
|-----|----------|---------|-------|
| `DATABASE_URL` | yes | — | Postgres URL |
| `RISK_BIND_ADDR` | no | `0.0.0.0:3004` | Listen address |
| `CACHE_TTL_S` | no | `60` | Verdict cache TTL (seconds) |

## Routes

| Route | Notes |
|-------|-------|
| `GET /health` | version + commit |
| `POST /v1/screen` | `{chain, address}` → `{chain, address, risk, blocked, matches}` |
| `POST /v1/screen/batch` | `{chain, addresses}`, max 100, verdicts in request order |
| `GET /v1/entries` | `?chain=&source=&limit=&offset=` — list contents |

Screening is POST rather than GET-with-path-param even though it is a read: `TraceLayer` records the request URI, so an address in the URL would be copied into access logs.

`risk` is one of `banned | high | medium | low | none`, the max across all matching rows; `blocked` is true for `banned` and `high`.

## Populating the list

Migration `2026-08-14-000021_seed_ofac_sdn_addresses` seeds 100 OFAC SDN EVM addresses (`source = 'ofac_sdn'`). That is a point-in-time snapshot and nothing refreshes it — see the migration header.

Beyond that, by SQL — there is no write API.

```sql
INSERT INTO screened_addresses (chain, address, risk, source, reason)
VALUES ('evm', '0x000000000000000000000000000000000000dead', 'high', 'internal', 'manual review');
```

⚠️ Rows must store the **normalized** form that `domain::address::normalize` produces — `chain` lowercased, and for `chain = 'evm'` the address as lowercase `0x`-hex. Lookups are exact `=` matches, so a row inserted with a checksummed (mixed-case) EVM address will never be found.

A new row becomes visible only once the cached verdict expires, up to `CACHE_TTL_S` later, on each replica independently.

## OpenAPI

`utoipa` + Swagger UI mounted by `build_router`, at `/swagger-ui`.
