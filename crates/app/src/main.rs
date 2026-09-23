mod admin;
mod config;
mod telemetry;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use uwuu_core::config::{Config, DatabaseConfig, JobsConfig};
use uwuu_db::Db;
use uwuu_db::site_cache::SiteCache;
use uwuu_jobs::{PoolConfig, Registry};
use uwuu_web::AppState;

/// How long startup keeps retrying an unreachable database, so the app can
/// start alongside Postgres (e.g. in docker compose) without crashing.
const DB_STARTUP_WAIT: Duration = Duration::from_secs(60);

/// How long shutdown waits for running jobs.
const WORKER_SHUTDOWN_GRACE: Duration = Duration::from_secs(60);

#[derive(Parser)]
#[command(name = "uwuubooru", version, about)]
struct Cli {
    /// Config file [default: ./uwuubooru.toml, if present]
    #[arg(short, long, env = "UWUU_CONFIG", global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the HTTP server (and job workers, unless jobs.run_in_serve is off)
    Serve,
    /// Run job workers only
    Worker,
    /// Apply pending database migrations, then exit
    Migrate,
    /// Validate the configuration and print the effective settings, with secrets redacted
    CheckConfig,
    /// Manage accounts and site settings
    Admin {
        #[command(subcommand)]
        command: admin::AdminCommand,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    uwuu_storage::install_crypto_provider();
    let cli = Cli::parse();
    let config = config::load(cli.config.as_deref())?;

    match cli.command {
        Command::CheckConfig => {
            print!("{}", toml::to_string_pretty(&config.redacted())?);
            Ok(())
        }
        Command::Migrate => {
            telemetry::init(&config.telemetry)?;
            let db = connect(&config.database).await?;
            migrate(&db).await?;
            db.close().await;
            Ok(())
        }
        Command::Admin { command } => {
            // Fails fast rather than retrying: someone is waiting at a shell.
            let db = Db::connect(&config.database)
                .await
                .context("could not connect to the database")?;
            if config.database.auto_migrate {
                migrate(&db).await?;
            }
            let result = admin::run(db.primary(), command).await;
            db.close().await;
            result
        }
        Command::Serve => {
            telemetry::init(&config.telemetry)?;
            serve(config).await
        }
        Command::Worker => {
            telemetry::init(&config.telemetry)?;
            worker(config).await
        }
    }
}

async fn serve(config: Config) -> anyhow::Result<()> {
    let db = connect(&config.database).await?;
    if config.database.auto_migrate {
        migrate(&db).await?;
    }

    let listener = TcpListener::bind(config.server.bind)
        .await
        .with_context(|| format!("could not bind {}", config.server.bind))?;
    tracing::info!(addr = %listener.local_addr()?, "listening");

    let site = SiteCache::load(db.primary())
        .await
        .context("could not load site settings")?;
    let config_jobs = config.jobs.clone();
    let state = AppState::new(config, db.clone(), site.clone())?;
    let background = [
        tokio::spawn(site.listen(db.primary().clone())),
        tokio::spawn(hourly_maintenance(state.clone())),
    ];

    let shutdown = CancellationToken::new();
    tokio::spawn(cancel_on_signal(shutdown.clone()));
    let workers = config_jobs
        .run_in_serve
        .then(|| tokio::spawn(run_workers(&db, &config_jobs, shutdown.clone())));

    let app = uwuu_web::router(state);
    uwuu_web::serve(listener, app, shutdown.clone().cancelled_owned()).await?;

    if let Some(workers) = workers {
        wait_for_workers(workers).await;
    }
    for task in background {
        task.abort();
    }
    db.close().await;
    tracing::info!("shut down");
    Ok(())
}

async fn worker(config: Config) -> anyhow::Result<()> {
    let db = connect(&config.database).await?;
    if config.database.auto_migrate {
        migrate(&db).await?;
    }
    let shutdown = CancellationToken::new();
    tokio::spawn(cancel_on_signal(shutdown.clone()));
    wait_for_workers(tokio::spawn(run_workers(&db, &config.jobs, shutdown))).await;
    db.close().await;
    tracing::info!("shut down");
    Ok(())
}

/// Every job type the application knows how to run.
fn job_registry() -> Registry {
    Registry::new()
}

fn run_workers(
    db: &Db,
    config: &JobsConfig,
    shutdown: CancellationToken,
) -> impl Future<Output = ()> + use<> {
    let pool_config = PoolConfig::new(
        config.workers,
        Duration::from_secs(config.lock_timeout_secs),
    );
    uwuu_jobs::run(db.primary().clone(), job_registry(), pool_config, shutdown)
}

/// Waits for running jobs after shutdown was requested, within reason; any
/// cut short are retried elsewhere once their lock expires.
async fn wait_for_workers(workers: tokio::task::JoinHandle<()>) {
    if tokio::time::timeout(WORKER_SHUTDOWN_GRACE, workers)
        .await
        .is_err()
    {
        tracing::warn!("jobs still running at shutdown; they will be retried");
    }
}

/// Deletes expired sessions and forgets idle rate-limit counters. Moves to
/// the job queue in M3.
async fn hourly_maintenance(state: AppState) {
    let mut interval = tokio::time::interval(Duration::from_secs(60 * 60));
    loop {
        interval.tick().await;
        state.rate_limits.retain_recent();
        match uwuu_db::sessions::prune_expired(state.db.primary()).await {
            Ok(0) => {}
            Ok(removed) => tracing::info!(removed, "pruned expired sessions"),
            Err(error) => tracing::warn!(%error, "could not prune expired sessions"),
        }
    }
}

async fn connect(config: &DatabaseConfig) -> anyhow::Result<Db> {
    let deadline = tokio::time::Instant::now() + DB_STARTUP_WAIT;
    let mut delay = Duration::from_millis(500);
    loop {
        match Db::connect(config).await {
            Ok(db) => return Ok(db),
            // A malformed URL or option will not fix itself.
            Err(error @ sqlx::Error::Configuration(_)) => {
                return Err(error).context("invalid database configuration");
            }
            Err(error) if tokio::time::Instant::now() + delay < deadline => {
                tracing::warn!(%error, retry_in = ?delay, "database not reachable yet");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(5));
            }
            Err(error) => return Err(error).context("could not connect to the database"),
        }
    }
}

async fn migrate(db: &Db) -> anyhow::Result<()> {
    db.migrate().await.context("database migration failed")?;
    tracing::info!(
        migrations = uwuu_db::MIGRATOR.iter().count(),
        "database schema is up to date"
    );
    Ok(())
}

async fn cancel_on_signal(token: CancellationToken) {
    shutdown_signal().await;
    token.cancel();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to listen for Ctrl+C");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to listen for SIGTERM")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
    tracing::info!("shutdown signal received, finishing in-flight work");
}
