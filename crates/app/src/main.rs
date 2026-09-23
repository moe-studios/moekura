mod admin;
mod config;
mod telemetry;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use uwu_core::config::{Config, DatabaseConfig};
use uwu_db::Db;
use uwu_db::site_cache::SiteCache;
use uwu_jobs::media::MediaJobs;
use uwu_jobs::tags::TagJobs;
use uwu_jobs::{PoolConfig, Registry};
use uwu_media::Media;
use uwu_storage::Storage;
use uwu_web::AppState;

/// How long startup keeps retrying an unreachable database, so the app can
/// start alongside Postgres (e.g. in docker compose) without crashing.
const DB_STARTUP_WAIT: Duration = Duration::from_secs(60);

/// How long shutdown waits for running jobs.
const WORKER_SHUTDOWN_GRACE: Duration = Duration::from_secs(60);

#[derive(Parser)]
#[command(name = "uwubooru", version, about)]
struct Cli {
    /// Config file [default: ./uwubooru.toml, if present]
    #[arg(short, long, env = "UWU_CONFIG", global = true)]
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
    uwu_storage::install_crypto_provider();
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
    check_media_tools(&config).await?;
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
    // Built before the config moves into the web state.
    let workers = config
        .jobs
        .run_in_serve
        .then(|| run_workers(&db, &config))
        .transpose()?;
    let file_key = uwu_db::secrets::get_or_create(
        db.primary(),
        "file_urls",
        uwu_core::tokens::NewToken::generate().hash,
    )
    .await
    .context("could not load the file URL key")?;
    let state = AppState::new(config, db.clone(), site.clone(), file_key)?;
    if state.is_private() && !state.storage.served_by_app() {
        tracing::warn!(
            "the site is private, but files are served from storage.public_base_url, \
             where anyone with a link can load them; unset it to have files signed and served here"
        );
    }
    let background = [
        tokio::spawn(site.listen(db.primary().clone())),
        tokio::spawn(hourly_maintenance(state.clone())),
    ];

    let shutdown = CancellationToken::new();
    tokio::spawn(cancel_on_signal(shutdown.clone()));
    let workers = workers.map(|run| tokio::spawn(run(shutdown.clone())));

    let app = uwu_web::router(state);
    uwu_web::serve(listener, app, shutdown.clone().cancelled_owned()).await?;

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
    check_media_tools(&config).await?;
    let db = connect(&config.database).await?;
    if config.database.auto_migrate {
        migrate(&db).await?;
    }
    let shutdown = CancellationToken::new();
    tokio::spawn(cancel_on_signal(shutdown.clone()));
    let run = run_workers(&db, &config)?;
    wait_for_workers(tokio::spawn(run(shutdown))).await;
    db.close().await;
    tracing::info!("shut down");
    Ok(())
}

/// Fails early, with an explanation, when vips or ffmpeg is missing, rather
/// than on the first upload.
async fn check_media_tools(config: &Config) -> anyhow::Result<()> {
    let versions = Media::new(config.media.clone())
        .check_tools()
        .await
        .context("media tools are required (see the README for installing vips and ffmpeg)")?;
    tracing::info!(tools = versions.join(", "), "media tools found");
    Ok(())
}

/// Every job type the application knows how to run.
fn job_registry(db: &Db, config: &Config) -> anyhow::Result<Registry> {
    let mut registry = Registry::new();
    let work_dir = config.media.work_dir_or_default();
    std::fs::create_dir_all(&work_dir)
        .with_context(|| format!("could not create {}", work_dir.display()))?;
    MediaJobs {
        db: db.primary().clone(),
        storage: Storage::from_config(&config.storage).context("could not open file storage")?,
        media: Media::new(config.media.clone()),
        work_dir,
    }
    .register(&mut registry);
    TagJobs {
        db: db.primary().clone(),
    }
    .register(&mut registry);
    Ok(registry)
}

/// Prepares a worker pool; call the result with a shutdown token to run it.
fn run_workers(
    db: &Db,
    config: &Config,
) -> anyhow::Result<impl FnOnce(CancellationToken) -> BoxFuture + use<>> {
    let registry = job_registry(db, config)?;
    let pool = db.primary().clone();
    let pool_config = PoolConfig::new(
        config.jobs.workers,
        Duration::from_secs(config.jobs.lock_timeout_secs),
    );
    Ok(move |shutdown| -> BoxFuture {
        Box::pin(uwu_jobs::run(pool, registry, pool_config, shutdown))
    })
}

type BoxFuture = std::pin::Pin<Box<dyn Future<Output = ()> + Send>>;

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
        match uwu_db::sessions::prune_expired(state.db.primary()).await {
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
        migrations = uwu_db::MIGRATOR.iter().count(),
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
