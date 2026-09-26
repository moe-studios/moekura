//! `moekura tagger`: job workers that only tag posts, with the model
//! loaded once.

use clap::Args;
use moekura_core::config::Config;

#[derive(Debug, Args)]
pub struct TaggerArgs {
    /// Load ONNX Runtime, print its version and exit
    #[arg(long)]
    check: bool,
}

#[cfg(not(feature = "tagger"))]
pub async fn run(_config: Config, _args: TaggerArgs) -> anyhow::Result<()> {
    anyhow::bail!(
        "this build of Moekura has no tagger: build it with `--features tagger`, \
         or use the image tagged -tagger"
    )
}

#[cfg(feature = "tagger")]
pub async fn run(config: Config, args: TaggerArgs) -> anyhow::Result<()> {
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::Context;
    use moekura_jobs::{PoolConfig, Registry};
    use moekura_media::Media;
    use moekura_storage::Storage;
    use moekura_tagger::{Model, Predict, TaggerJobs};
    use tokio_util::sync::CancellationToken;

    let tagger = &config.tagger;
    let runtime = moekura_tagger::load_runtime(tagger.runtime.as_deref())?;
    if args.check {
        println!("ONNX Runtime loaded from {}", runtime.display());
        return Ok(());
    }
    if !tagger.enabled {
        tracing::warn!(
            "tagger.enabled is off, so new uploads aren't queued for tagging; \
             only posts queued with `moekura admin tag-backlog` are tagged"
        );
    }
    crate::check_media_tools(&config).await?;

    let source = tagger
        .source()
        .context("tagger: the model isn't fully configured")?;
    let files = moekura_tagger::ensure(&source, &tagger.model_dir).await?;
    let (name, threads) = (source.name.clone(), tagger.threads);
    let model =
        tokio::task::spawn_blocking(move || Model::load(&name, &files.model, &files.tags, threads))
            .await
            .context("loading the model")??;
    tracing::info!(
        model = model.model_name(),
        input_size = model.input_size(),
        "model loaded"
    );

    let db = crate::connect(&config.database).await?;
    if config.database.auto_migrate {
        crate::migrate(&db).await?;
    }
    let work_dir = config.media.work_dir_or_default();
    std::fs::create_dir_all(&work_dir)
        .with_context(|| format!("could not create {}", work_dir.display()))?;
    let mut registry = Registry::new();
    TaggerJobs {
        db: db.primary().clone(),
        storage: Storage::from_config(&config.storage).context("could not open file storage")?,
        media: Media::new(config.media.clone()),
        work_dir,
        model: Arc::new(model),
    }
    .register(&mut registry);

    let shutdown = CancellationToken::new();
    tokio::spawn(crate::cancel_on_signal(shutdown.clone()));
    let pool_config = PoolConfig::new(
        tagger.workers,
        Duration::from_secs(config.jobs.lock_timeout_secs),
    );
    let workers = tokio::spawn(moekura_jobs::run(
        db.primary().clone(),
        registry,
        pool_config,
        shutdown,
    ));
    crate::wait_for_workers(workers).await;
    db.close().await;
    tracing::info!("shut down");
    Ok(())
}
