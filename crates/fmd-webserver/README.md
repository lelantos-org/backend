# fmd-webserver

HTTP API for FMD clients (clue retrieval, indexer status). Axum + Postgres. Depends on `fmd-crypto`.

## Run

```bash
DATABASE_URL=postgres://... cargo run -p fmd-webserver
```

## Env

| Var | Required | Default | Notes |
|-----|----------|---------|-------|
| `DATABASE_URL` | yes | — | Postgres URL |
| `BIND_ADDR` | no | `0.0.0.0:3001` | Listen address |
| `INDEXER_LAG_WARN_BLOCKS` | no | `50` | Health endpoint warns if indexer lags by more than N blocks |

## OpenAPI

`utoipa` + Swagger UI mounted by `build_router`.
