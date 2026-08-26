use anyhow::{Context, Result, anyhow};
use database::DbPool;
use database::advisory::{ChainLock, MIGRATE_KEY};
use ingester::adapters::{DynRpc, HttpRpc};
use ingester::app::config::{ChainConfig, IngesterConfig, redact_url};
use ingester::app::state::WorkerDeps;
use ingester::build_info;
use ingester::domain::error::IngesterError;
use ingester::handlers::worker::{self, WorkerExit};
use ingester::repositories::{
    PostgresAtomicWriteRepo, PostgresChainStateRepo, PostgresRawEventRepo,
};
use ingester::services::backfill::BackfillService;
use ingester::services::ingest::IngestService;
use ingester::services::reorg::ReorgService;
use ingester::services::retry::{Policy, is_retryable};
use shared::shutdown::{self, Shutdown};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

/// How long a replica waits between attempts at the migration lock.
const MIGRATE_LOCK_POLL: Duration = Duration::from_secs(1);
/// Give up on the migration lock rather than hang a deploy indefinitely.
const MIGRATE_LOCK_ATTEMPTS: u32 = 120;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    shared::tracing_init::init();
    info!(
        version = build_info::PKG_VERSION,
        commit = build_info::GIT_SHA,
        "ingester starting"
    );

    let cfg = load_config()?;

    shared::metrics::init_addr(&cfg.metrics_addr)?;

    migrate(&cfg.database_url).await?;

    let pool_cfg = database::PoolCfg::indexer();
    let pool = database::build_pool(&cfg.database_url, pool_cfg)
        .await
        .context("build pool")?;
    info!(pool_size = pool_cfg.max_size, "db pool built");

    let (trigger, shutdown) = shutdown::channel();
    tokio::spawn(shutdown::watch_signals(trigger));

    let mut workers = Vec::new();
    for chain_cfg in cfg.chains {
        let chain_id = chain_cfg.chain_id;
        info!(chain_id, rpc_url = %redact_url(&chain_cfg.rpc_url), "spawning worker");
        let deps = build_deps(&pool, chain_cfg, &cfg.database_url)?;
        let shutdown = shutdown.clone();
        workers.push((
            chain_id,
            tokio::spawn(async move { supervise(deps, shutdown).await }),
        ));
    }

    await_workers(workers).await
}

fn load_config() -> Result<IngesterConfig> {
    let mut cfg: IngesterConfig = shared::config::load_toml("INGESTER_CONFIG", "ingester.toml")
        .context("load ingester config")?;
    cfg.apply_env_overlay().context("apply env overlay")?;
    // Validated before anything is spawned, so a bad address or a zero chunk size
    // fails the process now rather than after a standby has waited out a chain
    // lock.
    cfg.validate().context("validate ingester config")?;
    info!(chains = cfg.chains.len(), "config loaded");
    Ok(cfg)
}

/// Wire one chain's repositories, services and provider together.
fn build_deps(pool: &DbPool, cfg: ChainConfig, database_url: &str) -> Result<WorkerDeps> {
    let rpc: DynRpc = HttpRpc::build(
        &cfg.rpc_url,
        Duration::from_millis(cfg.rpc_timeout_ms),
        Duration::from_millis(cfg.rpc_connect_timeout_ms),
        cfg.meta_concurrency,
    )?;
    let writes = Arc::new(PostgresAtomicWriteRepo::new(pool.clone()));
    let raw_events = Arc::new(PostgresRawEventRepo::new(pool.clone()));
    let chain_state = Arc::new(PostgresChainStateRepo::new(pool.clone()));
    let ingest = Arc::new(IngestService::new(
        writes.clone(),
        raw_events.clone(),
        chain_state.clone(),
    ));
    let reorg = Arc::new(ReorgService::new(writes, raw_events, chain_state.clone()));
    let backfill = Arc::new(BackfillService::new(rpc.clone(), ingest.clone()));

    Ok(WorkerDeps {
        cfg,
        rpc,
        chain_state,
        ingest,
        reorg,
        backfill,
        database_url: database_url.to_string(),
    })
}

/// Wait for every chain worker, then report whether any of them failed.
///
/// A dead chain is a failed process: exiting 0 after every worker gave up would
/// present a stalled ingester as healthy to its orchestrator.
async fn await_workers(
    workers: Vec<(i64, tokio::task::JoinHandle<Result<(), IngesterError>>)>,
) -> Result<()> {
    let mut failed = Vec::new();
    for (chain_id, handle) in workers {
        match handle.await {
            Ok(Ok(())) => info!(chain_id, "worker stopped cleanly"),
            Ok(Err(e)) => {
                error!(chain_id, "worker exited: {}", e);
                failed.push(chain_id);
            }
            Err(e) => {
                error!(chain_id, "worker task panicked: {}", e);
                failed.push(chain_id);
            }
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(anyhow!("chain workers failed: {:?}", failed))
    }
}

/// Run migrations under an advisory lock so concurrent replicas serialise.
///
/// `diesel_migrations` takes no lock of its own, so replicas booting together
/// would otherwise apply the same migration concurrently.
async fn migrate(database_url: &str) -> Result<()> {
    let lock = acquire_migrate_lock(database_url)
        .await?
        .ok_or_else(|| anyhow!("timed out waiting for the migration lock"))?;

    info!("running migrations");
    let url = database_url.to_string();
    let result = tokio::task::spawn_blocking(move || database::migrate::run(&url)).await;
    // Held until here: dropping the lock closes its connection.
    drop(lock);
    result?.context("migrations")?;
    info!("migrations complete");
    Ok(())
}

async fn acquire_migrate_lock(database_url: &str) -> Result<Option<ChainLock>> {
    for attempt in 0..MIGRATE_LOCK_ATTEMPTS {
        match ChainLock::try_acquire(database_url, MIGRATE_KEY).await {
            Ok(Some(lock)) => return Ok(Some(lock)),
            Ok(None) => {
                if attempt == 0 {
                    info!("another replica is migrating; waiting");
                }
                tokio::time::sleep(MIGRATE_LOCK_POLL).await;
            }
            Err(e) => return Err(anyhow!("migration lock: {}", e)),
        }
    }
    Ok(None)
}

/// Keep one chain's worker alive across recoverable failures.
///
/// A worker that loses its advisory lock is restarted rather than abandoned: it
/// returns to standby and retakes the chain when the lock frees. That is not a
/// failure and does not consume a restart.
async fn supervise(deps: WorkerDeps, mut shutdown: Shutdown) -> Result<(), IngesterError> {
    let chain_id = deps.cfg.chain_id;
    let policy = Policy::WORKER_RESTART;
    let mut restarts: u32 = 0;
    loop {
        match worker::run(deps.clone(), shutdown.clone()).await {
            Ok(WorkerExit::Shutdown) => return Ok(()),
            Ok(WorkerExit::LockLost) => {
                warn!(chain_id, "lock lost; returning to standby");
            }
            Err(e) if !is_retryable(&e) => return Err(e),
            Err(e) => {
                restarts += 1;
                if restarts >= policy.max_attempts {
                    error!(chain_id, restarts, "worker exhausted restarts: {}", e);
                    return Err(e);
                }
                let delay = policy.delay(restarts - 1);
                warn!(
                    chain_id,
                    restarts,
                    delay_ms = delay.as_millis() as u64,
                    "worker failed; restarting: {}",
                    e
                );
                tokio::select! {
                    () = tokio::time::sleep(delay) => {}
                    () = shutdown.recv() => return Ok(()),
                }
            }
        }
    }
}
