//! `moekura admin import-remote`: posts from another booru, through its
//! API.

use std::time::Duration;

use anyhow::{Context, anyhow};
use clap::Args;
use moekura_core::config::Config;
use moekura_core::remote::{Credentials, Kind};
use moekura_db::Db;
use moekura_db::site_cache::SiteCache;
use moekura_web::AppState;
use moekura_web::remote_import::{Options, Outcome, run as import};

#[derive(Args)]
pub struct ImportRemoteArgs {
    /// The site's address, like https://danbooru.donmai.us
    site: String,
    /// What to import: a search in the site's own syntax
    #[arg(default_value = "")]
    tags: String,
    /// Who the posts are uploaded by here
    #[arg(long)]
    uploader: String,
    /// The kind of site: danbooru (also other Moekura sites), e621,
    /// gelbooru or moebooru. Guessed for well-known sites.
    #[arg(long)]
    kind: Option<String>,
    /// Your name (Gelbooru: user id) on the site, if it wants a login
    #[arg(long, default_value = "")]
    login: String,
    /// Your API key on the site
    #[arg(long, default_value = "")]
    api_key: String,
    /// Milliseconds to wait between requests to the site
    #[arg(long, default_value_t = 1000)]
    delay_ms: u64,
    /// Stop after importing this many posts (run again to carry on)
    #[arg(long)]
    limit: Option<u32>,
    /// Also import notes (Danbooru-style sites)
    #[arg(long)]
    notes: bool,
    /// Also import pools (Danbooru-style sites)
    #[arg(long)]
    pools: bool,
    /// Start from the newest posts again instead of where the last run of
    /// this site and search stopped
    #[arg(long)]
    restart: bool,
    /// Allow a site on a private network (another Moekura on your LAN)
    #[arg(long)]
    allow_private_addresses: bool,
}

pub async fn run(config: Config, db: &Db, args: ImportRemoteArgs) -> anyhow::Result<()> {
    let base =
        url::Url::parse(&args.site).with_context(|| format!("{:?} isn't a URL", args.site))?;
    let kind = match &args.kind {
        Some(kind) => Kind::parse(kind).ok_or_else(|| {
            anyhow!("unknown kind {kind:?}: use danbooru, e621, gelbooru or moebooru")
        })?,
        None => base
            .host_str()
            .and_then(Kind::guess)
            .ok_or_else(|| anyhow!("can't tell what kind of site that is; give --kind"))?,
    };
    let uploader = moekura_db::users::by_name(db.primary(), &args.uploader)
        .await?
        .ok_or_else(|| anyhow!("there is no user called {:?}", args.uploader))?;
    let site = SiteCache::load(db.primary())
        .await
        .context("could not load site settings")?;
    let file_key = moekura_db::secrets::get_or_create(
        db.primary(),
        "file_urls",
        moekura_core::tokens::NewToken::generate().hash,
    )
    .await
    .context("could not load the file URL key")?;
    let state = AppState::new(config, db.clone(), site, file_key)?;
    let options = Options {
        kind,
        base: args.site.trim_end_matches('/').to_owned(),
        tags: args.tags.clone(),
        credentials: Credentials {
            login: args.login.clone(),
            api_key: args.api_key.clone(),
        },
        delay: Duration::from_millis(args.delay_ms),
        limit: args.limit,
        notes: args.notes,
        pools: args.pools,
        restart: args.restart,
        allow_private: args.allow_private_addresses,
    };
    let progress = import(&state, &uploader, &options, |outcome| match outcome {
        Outcome::Imported { remote, post } => println!("imported   {remote} as post #{post}"),
        Outcome::Duplicate { remote, post } => {
            println!("duplicate  {remote}: already post #{post}")
        }
        Outcome::NoFile { remote } => {
            println!("skipped    {remote}: the site doesn't show its file")
        }
        Outcome::Failed { remote, reason } => println!("failed     {remote}: {reason}"),
    })
    .await
    .map_err(|e| anyhow!(e))?;
    println!(
        "{} imported, {} already here, {} failed{}",
        progress.imported,
        progress.duplicates,
        progress.failed,
        if progress.finished {
            " (finished)"
        } else {
            "; run again to carry on"
        }
    );
    Ok(())
}
