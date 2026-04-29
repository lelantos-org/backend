# explorer-webserver

HTTP API for explorer queries (public chain data). Axum + Postgres. Must not depend on `fmd-crypto` (CI gate enforces).

## Run

```bash
DATABASE_URL=postgres://... cargo run -p explorer-webserver
```

## Env

| Var | Required | Default | Notes |
|-----|----------|---------|-------|
| `DATABASE_URL` | yes | — | Postgres URL |
| `EXPLORER_BIND_ADDR` | no | `0.0.0.0:3002` | Listen address |
| `CACHE_TTL_S` | no | `30` | Response cache TTL (seconds) |

## OpenAPI

`utoipa` + Swagger UI mounted by `build_router`. Open the bind address in a browser to inspect routes.
