mod admin;
mod config;
mod telemetry;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use tokio::net::TcpListener;
use uwuu_core::config::{Config, DatabaseConfig};
use uwuu_db::Db;
use uwuu_db::site_cache::SiteCache;
use uwuu_web::AppState;

/// How long startup keeps retrying an unreachable database, so the app can
/// start alongside Postgres (e.g. in docker compose) without crashing.
const DB_STARTUP_WAIT: Duration = Duration::from_secs(60);

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
    /// Run the HTTP server
    Serve,
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
    let background = [
        tokio::spawn(site.clone().listen(db.primary().clone())),
        tokio::spawn(prune_sessions(db.clone())),
    ];

    let state = AppState::new(config, db.clone(), site)?;
    let app = uwuu_web::router(state);
    uwuu_web::serve(listener, app, shutdown_signal()).await?;

    for task in background {
        task.abort();
    }
    db.close().await;
    tracing::info!("shut down");
    Ok(())
}

/// Deletes expired sessions hourly. Moves to the job queue in M3.
async fn prune_sessions(db: Db) {
    let mut interval = tokio::time::interval(Duration::from_secs(60 * 60));
    loop {
        interval.tick().await;
        match uwuu_db::sessions::prune_expired(db.primary()).await {
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
    tracing::info!("shutdown signal received, finishing in-flight requests");
}
