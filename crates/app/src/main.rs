mod admin;
mod bench_http;
mod config;
mod deprecated;
mod export;
mod import;
mod import_remote;
mod tagger;
mod telemetry;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::{CommandFactory, Parser, Subcommand};
use moekura_core::config::{Config, DatabaseConfig};
use moekura_db::Db;
use moekura_db::site_cache::SiteCache;
use moekura_jobs::mail::{MailJobs, Mailer};
use moekura_jobs::media::MediaJobs;
use moekura_jobs::posts::PostJobs;
use moekura_jobs::stats::StatsJobs;
use moekura_jobs::tags::TagJobs;
use moekura_jobs::users::UserJobs;
use moekura_jobs::versions::VersionJobs;
use moekura_jobs::webhooks::WebhookJobs;
use moekura_jobs::{PoolConfig, Registry};
use moekura_media::Media;
use moekura_storage::Storage;
use moekura_web::AppState;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// How long startup keeps retrying an unreachable database, so the app can
/// start alongside Postgres (e.g. in docker compose) without crashing.
const DB_STARTUP_WAIT: Duration = Duration::from_secs(60);

/// How long shutdown waits for running jobs.
const WORKER_SHUTDOWN_GRACE: Duration = Duration::from_secs(60);

#[derive(Parser)]
#[command(name = "moekura", version, about)]
struct Cli {
    /// Config file [default: ./moekura.toml, if present]
    #[arg(short, long, env = "MOEKURA_CONFIG", global = true)]
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
    /// Suggest tags for posts with a machine learning model (see [tagger]
    /// in the configuration; needs the `tagger` build feature)
    Tagger(tagger::TaggerArgs),
    /// Apply pending database migrations, then exit
    Migrate,
    /// Validate the configuration and print the effective settings, with secrets redacted
    CheckConfig,
    /// Print the API's OpenAPI description as JSON
    Openapi,
    /// Manage accounts and site settings
    Admin {
        #[command(subcommand)]
        command: admin::AdminCommand,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    moekura_storage::install_crypto_provider();
    let cli = parse_args();
    // Needs no configuration.
    if let Command::Openapi = cli.command {
        println!("{}", moekura_web::api::openapi().to_pretty_json()?);
        return Ok(());
    }
    let (config, notices) = config::load(cli.config.as_deref())?;
    // Logged once logging is set up; commands without it print them.
    let warn_deprecated = || {
        for notice in &notices {
            tracing::warn!("{notice}");
        }
    };
    if matches!(cli.command, Command::CheckConfig | Command::Admin { .. }) {
        for notice in &notices {
            eprintln!("warning: {notice}");
        }
    }

    match cli.command {
        Command::Openapi => unreachable!("handled before loading the configuration"),
        Command::CheckConfig => {
            print!("{}", effective_settings(&config)?);
            Ok(())
        }
        Command::Migrate => {
            let telemetry = telemetry::init(&config.telemetry, "migrate")?;
            warn_deprecated();
            let result = async {
                let db = connect(&config.database).await?;
                migrate(&db).await?;
                db.close().await;
                Ok(())
            }
            .await;
            telemetry.shutdown().await;
            result
        }
        Command::Admin { command } => {
            // Fails fast rather than retrying: someone is waiting at a shell.
            let db = Db::connect(&config.database)
                .await
                .context("could not connect to the database")?;
            if config.database.auto_migrate {
                migrate(&db).await?;
            }
            let result = match command {
                admin::AdminCommand::Export(args) => export::run(config, &db, args).await,
                admin::AdminCommand::Import(args) => match check_media_tools(&config).await {
                    Ok(()) => import::run(config, &db, args).await,
                    Err(error) => Err(error),
                },
                admin::AdminCommand::ImportRemote(args) => match check_media_tools(&config).await {
                    Ok(()) => import_remote::run(config, &db, args).await,
                    Err(error) => Err(error),
                },
                command => admin::run(db.primary(), &config, command).await,
            };
            db.close().await;
            result
        }
        Command::Serve => {
            let telemetry = telemetry::init(&config.telemetry, "serve")?;
            warn_deprecated();
            let result = serve(config).await;
            telemetry.shutdown().await;
            result
        }
        Command::Worker => {
            let telemetry = telemetry::init(&config.telemetry, "worker")?;
            warn_deprecated();
            let result = worker(config).await;
            telemetry.shutdown().await;
            result
        }
        Command::Tagger(args) => {
            let telemetry = telemetry::init(&config.telemetry, "tagger")?;
            warn_deprecated();
            let result = tagger::run(config, args).await;
            telemetry.shutdown().await;
            result
        }
    }
}

/// The command line, with deprecated spellings rewritten (and warned
/// about) first.
fn parse_args() -> Cli {
    let args: Option<Vec<String>> = std::env::args_os()
        .map(|arg| arg.into_string().ok())
        .collect();
    // Old names are plain ASCII; leave anything else to clap.
    let Some(args) = args else {
        return Cli::parse();
    };
    let (args, warnings) = deprecated::rewrite_args(&Cli::command(), deprecated::CLI_NAMES, args);
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
    Cli::parse_from(args)
}

/// What `check-config` prints: the settings in effect, with credentials
/// masked, as operators paste it into bug reports.
fn effective_settings(config: &Config) -> anyhow::Result<String> {
    // Site settings live in the database; say which ones win.
    Ok(format!(
        "# search.per_page, max_per_page and max_page are defaults: the `pagination`\n\
         # site setting overrides them (see `moekura admin settings`).\n\n{}",
        toml::to_string_pretty(&config.redacted())?
    ))
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
    let shutdown = CancellationToken::new();
    let metrics = match config.telemetry.metrics_bind {
        Some(bind) => Some(
            telemetry::start_metrics(
                bind,
                config.server.connections.clone(),
                db.clone(),
                shutdown.clone(),
            )
            .await?,
        ),
        None => None,
    };

    let site = SiteCache::load(db.primary())
        .await
        .context("could not load site settings")?;
    // Built before the config moves into the web state.
    let workers = config
        .jobs
        .run_in_serve
        .then(|| run_workers(&db, &config))
        .transpose()?;
    let file_key = moekura_db::secrets::get_or_create(
        db.primary(),
        "file_urls",
        moekura_core::tokens::NewToken::generate().hash,
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
    let max_lag = Duration::from_secs(state.config.database.replica_max_lag_secs);
    let background = [
        tokio::spawn(site.listen(db.primary().clone())),
        tokio::spawn(hourly_maintenance(state.clone())),
        tokio::spawn(db.clone().monitor_replicas(max_lag)),
        tokio::spawn(moekura_web::explore::flush_every_minute(state.clone())),
    ];

    tokio::spawn(cancel_on_signal(shutdown.clone()));
    let workers = workers.map(|run| tokio::spawn(run(shutdown.clone())));

    let app = moekura_web::router(state.clone());
    moekura_web::serve(
        listener,
        app,
        &state.config.server.connections,
        shutdown.clone().cancelled_owned(),
    )
    .await?;
    // Views and searches counted since the last minute.
    moekura_web::explore::flush(&state).await;

    if let Some(workers) = workers {
        wait_for_workers(workers, &shutdown).await;
    }
    for task in background {
        task.abort();
    }
    if let Some(metrics) = metrics {
        let _ = metrics.await;
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
    let metrics = match config.telemetry.metrics_bind {
        Some(bind) => Some(
            telemetry::start_metrics(
                bind,
                config.server.connections.clone(),
                db.clone(),
                shutdown.clone(),
            )
            .await?,
        ),
        None => None,
    };
    tokio::spawn(cancel_on_signal(shutdown.clone()));
    let run = run_workers(&db, &config)?;
    wait_for_workers(tokio::spawn(run(shutdown.clone())), &shutdown).await;
    if let Some(metrics) = metrics {
        let _ = metrics.await;
    }
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
        tag_posts: config.tagger.enabled,
    }
    .register(&mut registry);
    PostJobs {
        db: db.primary().clone(),
        storage: Storage::from_config(&config.storage).context("could not open file storage")?,
    }
    .register(&mut registry);
    StatsJobs {
        db: db.primary().clone(),
    }
    .register(&mut registry);
    TagJobs {
        db: db.primary().clone(),
    }
    .register(&mut registry);
    UserJobs {
        db: db.primary().clone(),
    }
    .register(&mut registry);
    VersionJobs {
        db: db.primary().clone(),
    }
    .register(&mut registry);
    WebhookJobs::new(
        db.primary().clone(),
        Duration::from_secs(config.webhooks.timeout_secs),
        config.webhooks.allow_private_addresses,
    )
    .max_concurrent(config.webhooks.max_concurrent)
    .register(&mut registry);
    let mailer = if config.mail.is_enabled() {
        let mailer = Mailer::new(&config.mail).context("mail")?;
        Some(Arc::new(mailer))
    } else {
        None
    };
    MailJobs { mailer }.register(&mut registry);
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
        Box::pin(moekura_jobs::run(pool, registry, pool_config, shutdown))
    })
}

type BoxFuture = std::pin::Pin<Box<dyn Future<Output = ()> + Send>>;

/// Waits for shutdown to be requested, then for running jobs, within
/// reason; any cut short are retried elsewhere once their lock expires.
async fn wait_for_workers(mut workers: tokio::task::JoinHandle<()>, shutdown: &CancellationToken) {
    tokio::select! {
        _ = &mut workers => return,
        () = shutdown.cancelled() => {}
    }
    if tokio::time::timeout(WORKER_SHUTDOWN_GRACE, workers)
        .await
        .is_err()
    {
        tracing::warn!("jobs still running at shutdown; they will be retried");
    }
}

/// Deletes expired sessions, unfinished logins, old view and search
/// counts, old read notifications, scratch files left by requests and
/// idle file transfers, and forgets idle rate-limit counters.
async fn hourly_maintenance(state: AppState) {
    let mut interval = tokio::time::interval(Duration::from_secs(60 * 60));
    loop {
        interval.tick().await;
        state.rate_limits.retain_recent();
        match moekura_db::sessions::prune_expired(state.db.primary()).await {
            Ok(0) => {}
            Ok(removed) => tracing::info!(removed, "pruned expired sessions"),
            Err(error) => tracing::warn!(%error, "could not prune expired sessions"),
        }
        if let Err(error) = moekura_db::two_factor::prune_challenges(state.db.primary()).await {
            tracing::warn!(%error, "could not prune expired two-factor logins");
        }
        if let Err(error) = moekura_db::passkeys::prune_challenges(state.db.primary()).await {
            tracing::warn!(%error, "could not prune expired passkey challenges");
        }
        if let Err(error) = moekura_db::identities::prune_logins(state.db.primary()).await {
            tracing::warn!(%error, "could not prune abandoned single sign-on logins");
        }
        moekura_web::explore::prune(&state).await;
        moekura_web::notifications::prune(&state).await;
        moekura_web::uploads::sweep_work_dir(&state).await;
        moekura_web::transfers::prune(&state).await;
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
        migrations = moekura_db::MIGRATOR.iter().count(),
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

#[cfg(test)]
// `Jail` closures must return `figment::Result`, whose error type is large.
#[allow(clippy::result_large_err)]
mod tests {
    use clap::CommandFactory;
    use figment::Jail;

    use super::*;

    /// Each old spelling leads to a flag or subcommand that exists, and no
    /// longer exists itself.
    #[test]
    fn renamed_cli_names_point_at_current_ones() {
        let root = Cli::command();
        for rename in deprecated::CLI_NAMES {
            let mut words: Vec<&str> = rename.old.split(' ').collect();
            let old = words.pop().expect("not empty");
            let mut command = &root;
            for word in words {
                command = command
                    .find_subcommand(word)
                    .unwrap_or_else(|| panic!("{}: no subcommand {word}", rename.old));
            }
            let exists = |name: &str| match name.strip_prefix("--") {
                Some(long) => command.get_arguments().any(|a| a.get_long() == Some(long)),
                None => command.find_subcommand(name).is_some(),
            };
            assert!(
                exists(rename.new),
                "{}: {} is missing",
                rename.old,
                rename.new
            );
            assert!(!exists(old), "{} still exists", rename.old);
        }
    }

    #[test]
    fn check_config_prints_no_credentials() {
        Jail::expect_with(|jail| {
            jail.create_file(
                config::DEFAULT_PATH,
                r#"
                [database]
                url = "postgres://moekura:s3cr3t-db@db/moekura"
                [auth.captcha]
                provider = "hcaptcha"
                site_key = "public-site-key"
                secret_key = "s3cr3t-captcha"
                [sources.logins."x.com"]
                cookie = "auth_token=s3cr3t-cookie"
                headers = { "x-csrf-token" = "s3cr3t-csrf" }
                [sources.logins."gelbooru.com"]
                query = { user_id = "7", api_key = "s3cr3t-api-key" }
                [telemetry]
                otlp_endpoint = "https://otel:s3cr3t-otlp@collector.example.com"
                "#,
            )?;
            jail.set_env("MOEKURA_MAIL__PASSWORD", "s3cr3t-mail");
            let (config, _) = config::load(None).map_err(|e| format!("{e:#}"))?;
            let printed = effective_settings(&config).map_err(|e| format!("{e:#}"))?;
            assert!(!printed.contains("s3cr3t"), "{printed}");
            assert!(printed.contains("public-site-key"), "{printed}");
            assert!(printed.contains("x-csrf-token"), "{printed}");
            Ok(())
        });
    }
}
