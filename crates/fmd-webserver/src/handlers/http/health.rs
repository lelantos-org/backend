//! Liveness only. A static answer with no database round trip, so a probe
//! cannot be the thing that exhausts the pool.

pub async fn health() -> &'static str {
    "ok"
}
