//! `moekura admin …`: account and settings management from the shell,
//! for bootstrapping an instance before anyone can log in.

use std::io::{BufRead, IsTerminal};
use std::net::IpAddr;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::Subcommand;
use moekura_core::jobs::ProcessMedia;
use moekura_core::moderation::ActionKind;
use moekura_core::permissions::{Permission, Permissions, Role, SystemRole};
use moekura_db::accounts::{self, NewAccount};
use moekura_db::mod_actions::{self, NewAction};
use moekura_db::users::{self, User, UserStatus};
use moekura_db::{api_keys, bans, invites, remote_imports, roles, settings};
use serde_json::Value;
use sqlx::PgPool;

#[derive(Subcommand)]
pub enum AdminCommand {
    /// Create an account. Prompts for the password, or reads one line from
    /// stdin when it is not a terminal.
    CreateUser {
        name: String,
        /// Role name or built-in key (anonymous, member, …, admin)
        #[arg(long, default_value = "member")]
        role: String,
        #[arg(long, conflicts_with = "bot")]
        email: Option<String>,
        /// A bot account, with no password: it uses API keys made with
        /// `create-api-key` and can't log in on the site
        #[arg(long)]
        bot: bool,
    },
    /// Change a user's role
    SetRole { name: String, role: String },
    /// Make an account the site's owner, who outranks every role (so can
    /// act on admins), in place of the current owner
    SetOwner { name: String },
    /// Make an account a bot, which can't log in on the site and uses API
    /// keys only, or (with --off) a person's again
    SetBot {
        name: String,
        #[arg(long)]
        off: bool,
    },
    /// Lift the network bans covering an address, or overlapping a range,
    /// e.g. when staff are locked out of the site
    LiftNetworkBan {
        /// An address, or a range like 203.0.113.0/24
        network: String,
    },
    /// Create an invite code for registration_mode = invite. The code is
    /// shown once.
    CreateInvite {
        /// How many accounts the code can create
        #[arg(long, default_value_t = 1)]
        uses: u16,
        /// Days until the code expires; never by default
        #[arg(long)]
        expires_days: Option<u32>,
    },
    /// Create an API key for a user, such as a bot. The key is printed
    /// once.
    CreateApiKey {
        /// The user the key acts as
        user: String,
        /// What will use it
        #[arg(long)]
        name: String,
        /// A permission the key may use (view_posts, upload, …); repeat
        /// for more. Without any, the key can do everything the user can.
        #[arg(long = "permission", value_name = "PERMISSION")]
        permissions: Vec<String>,
        /// Days until the key expires; never by default
        #[arg(long)]
        expires_days: Option<u32>,
    },
    /// List a user's API keys
    ApiKeys { user: String },
    /// Revoke one of a user's API keys, by its id from `api-keys`
    RevokeApiKey { user: String, id: i64 },
    /// Regenerate thumbnails and samples, e.g. after changing media settings
    RegenerateMedia {
        /// Every post
        #[arg(long, conflicts_with = "posts")]
        all: bool,
        /// Post ids
        #[arg(required_unless_present = "all")]
        posts: Vec<i64>,
    },
    /// Recompute every tag's post count. Post edits wait while it runs.
    RecountTags,
    /// Queue existing posts for the tagger (`moekura tagger`): those it
    /// hasn't seen, oldest first
    TagBacklog {
        /// Every post, including those it has seen (e.g. after changing
        /// the model or lowering thresholds)
        #[arg(long)]
        all: bool,
        /// Queue at most this many
        #[arg(long)]
        limit: Option<i64>,
    },
    /// Write posts' original files to a folder, each with a JSON sidecar of
    /// its tags, rating, source, description and parent, for `import` here
    /// or on another site. Files already there are kept, so an interrupted
    /// export can be run again.
    Export(crate::export::ExportArgs),
    /// Import a folder of images and videos as posts, with tags from the
    /// sidecar files next to them (pic.png.txt, pic.json, …). Files already
    /// here are skipped, so an interrupted import can be run again.
    Import(crate::import::ImportArgs),
    /// Import posts from another booru through its API: Danbooru (and
    /// other Moekura sites), e621, Gelbooru or Moebooru. Progress is kept,
    /// so running it again carries on where it stopped.
    ImportRemote(crate::import_remote::ImportRemoteArgs),
    /// List the posts `import-remote` couldn't import, and why. Run it
    /// with --retry-failed to try them again.
    ImportRemoteFailures {
        /// Only this site, like https://danbooru.donmai.us
        site: Option<String>,
    },
    /// Time a fixed suite of searches against this database (seed one
    /// first), and report the pages each touched
    Bench {
        /// Timed runs per search
        #[arg(long, default_value_t = 20)]
        runs: usize,
        /// Fail if a search that should be selective reads over half as
        /// many pages as the posts table has
        #[arg(long)]
        check: bool,
        /// Print the query plans of the searches whose names contain this
        #[arg(long)]
        explain: Option<String>,
    },
    /// Time pages and API responses from a running server over HTTP, as
    /// visitors wait for them, with what to load picked from this database.
    /// Fails if any response isn't 200 OK.
    BenchHttp {
        /// The server
        #[arg(long, default_value = "http://localhost:8080")]
        url: String,
        /// Timed requests per page
        #[arg(long, default_value_t = 50)]
        runs: usize,
        /// Requests in flight at once
        #[arg(long, default_value_t = 1)]
        concurrency: usize,
        /// Only pages whose names contain this
        #[arg(long)]
        only: Option<String>,
        /// Fail if a page's p95 is above this many milliseconds
        #[arg(long)]
        check_ms: Option<u64>,
    },
    /// Fill a test database with synthetic posts for load testing. Refuses
    /// to touch a database with real posts unless forced.
    Seed {
        /// How many posts to add
        #[arg(long)]
        posts: u64,
        /// How many distinct tags [default: one per 20 posts, at least 1000]
        #[arg(long)]
        tags: Option<u32>,
        /// Random seed: the same seed and sizes give the same posts
        #[arg(long, default_value_t = 1)]
        seed: u32,
        /// Posts per transaction
        #[arg(long, default_value_t = 50_000)]
        batch: u64,
        /// Seed even though the database has real posts
        #[arg(long)]
        force: bool,
    },
    /// Send a test message through the [mail] settings, to check them
    SendTestMail {
        /// Address to send it to
        to: String,
    },
    /// Show site settings, or change one
    Settings {
        #[command(subcommand)]
        action: Option<SettingsAction>,
    },
}

#[derive(Subcommand)]
pub enum SettingsAction {
    /// Set KEY to VALUE. VALUE is parsed as JSON, falling back to a plain string.
    Set { key: String, value: String },
}

async fn seed_posts(
    db: &PgPool,
    options: &moekura_db::seed::Options,
    force: bool,
) -> anyhow::Result<()> {
    use moekura_db::seed;
    let started = std::time::Instant::now();
    let plan = seed::prepare(db, options, force).await?;
    println!(
        "seeding {} posts with {} tags and {} users",
        options.posts,
        plan.tag_ids.len(),
        plan.user_ids.len()
    );
    let mut done = 0;
    while done < options.posts {
        let to = (done + options.batch_size).min(options.posts);
        seed::batch(db, &plan, options, done, to).await?;
        done = to;
        let secs = started.elapsed().as_secs_f64();
        println!(
            "{done}/{} posts ({:.0} per second)",
            options.posts,
            done as f64 / secs.max(0.001)
        );
    }
    println!("adding aliases and implications, and analyzing");
    seed::finish(db, &plan).await?;
    println!("done in {:.0?}", started.elapsed());
    Ok(())
}

async fn bench(
    db: &PgPool,
    config: &moekura_core::config::SearchConfig,
    runs: usize,
    check: bool,
    explain: Option<&str>,
) -> anyhow::Result<()> {
    use moekura_db::bench;
    use moekura_db::search::Count;
    let posts: i64 = sqlx::query_scalar("SELECT count(*) FROM posts")
        .fetch_one(db)
        .await?;
    let table = bench::posts_pages(db).await?;
    println!("{posts} posts ({table} pages); {runs} runs per search\n");
    println!(
        "{:<22} {:>8} {:>8} {:>8} {:>9} {:<8} {:>9}  query",
        "search", "p50 ms", "p95 ms", "max ms", "pages", "plan", "count"
    );
    let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
    let mut problems = Vec::new();
    for case in bench::suite(db, config).await? {
        let m = bench::measure(db, config, &case, runs).await?;
        let count = match m.count {
            Count::Exact(n) => n.to_string(),
            Count::About(n) => format!("~{n}"),
            Count::AtLeast(n) => format!("{n}+"),
        };
        let page = match case.page {
            moekura_db::search::PageRef::Number(1) => String::new(),
            moekura_db::search::PageRef::Number(n) => format!(" (page {n})"),
            moekura_db::search::PageRef::Before(id) => format!(" (page b{id})"),
            moekura_db::search::PageRef::After(id) => format!(" (page a{id})"),
        };
        println!(
            "{:<22} {:>8.1} {:>8.1} {:>8.1} {:>9} {:<8} {:>9}  {}{page}",
            case.name,
            ms(m.p50),
            ms(m.p95),
            ms(m.max),
            m.pages,
            m.strategy,
            count,
            case.query
        );
        problems.extend(bench::problem(&m, table));
        if explain.is_some_and(|name| case.name.contains(name)) {
            let query = moekura_core::search::Query::parse(&case.query)?;
            let visitor = moekura_db::posts::Visibility {
                hidden_tags: Vec::new(),
                statuses: vec![
                    moekura_core::posts::PostStatus::Active,
                    moekura_core::posts::PostStatus::Flagged,
                ],
                viewer: None,
                ratings: Vec::new(),
                deleted_by_default: false,
            };
            let plan = moekura_db::search::Plan::resolve(db, &query, &visitor, config).await?;
            println!("{}\n", plan.explain_text(db, case.page).await?);
        }
    }
    if check && !problems.is_empty() {
        for problem in &problems {
            eprintln!("{problem}");
        }
        bail!("{} searches read far more than they should", problems.len());
    }
    Ok(())
}

pub async fn run(
    db: &PgPool,
    config: &moekura_core::config::Config,
    command: AdminCommand,
) -> anyhow::Result<()> {
    match command {
        AdminCommand::CreateUser {
            name,
            role,
            bot: true,
            ..
        } => {
            let user = create_bot(db, &name, &role).await?;
            println!("created the bot {} (id {})", user.name, user.id);
        }
        AdminCommand::CreateUser {
            name,
            role,
            email,
            bot: false,
        } => {
            let password = read_password()?;
            let user = create_user(db, &name, &role, email.as_deref(), &password).await?;
            println!("created {} (id {})", user.name, user.id);
            if user.is_owner {
                println!("{} is the site's owner", user.name);
            }
        }
        AdminCommand::SetBot { name, off } => {
            let user = set_bot(db, &name, !off).await?;
            if off {
                println!("{} is no longer a bot", user.name);
            } else {
                println!(
                    "{} is now a bot; their sessions have ended and their feed token is revoked",
                    user.name
                );
            }
        }
        AdminCommand::SetRole { name, role } => {
            let role = set_role(db, &name, &role).await?;
            println!("{name} is now {}", role.name);
        }
        AdminCommand::SetOwner { name } => {
            let user = set_owner(db, &name).await?;
            println!("{} is now the site's owner", user.name);
        }
        AdminCommand::LiftNetworkBan { network } => {
            let lifted = lift_network_bans(db, &network).await?;
            if lifted.is_empty() {
                println!("no network ban in force covers {network}");
            }
            for network in lifted {
                println!("lifted the ban on {network}");
            }
        }
        AdminCommand::CreateInvite { uses, expires_days } => {
            let invite = invites::NewInvite {
                created_by: None,
                max_uses: i32::from(uses.max(1)),
                expires_in: expires_days.map(|d| Duration::from_secs(u64::from(d) * 86_400)),
                note: String::new(),
            };
            println!("{}", invites::create(db, invite).await?);
        }
        AdminCommand::CreateApiKey {
            user,
            name,
            permissions,
            expires_days,
        } => {
            println!(
                "{}",
                create_api_key(db, &user, &name, &permissions, expires_days).await?
            );
        }
        AdminCommand::ApiKeys { user } => {
            let user = find_user(db, &user).await?;
            let keys = api_keys::list(db, user.id).await?;
            if keys.is_empty() {
                println!("{} has no API keys", user.name);
            }
            let day = |t: time::OffsetDateTime| t.date().to_string();
            for key in keys {
                let access = if key.permissions == Permissions::ALL {
                    "everything".to_owned()
                } else {
                    key.permissions
                        .iter()
                        .map(Permission::key)
                        .collect::<Vec<_>>()
                        .join(",")
                };
                println!(
                    "{}\t{}\t{}…\t{access}\tcreated {}\tlast used {}\texpires {}",
                    key.id,
                    key.name,
                    key.prefix,
                    day(key.created_at),
                    key.last_used_at.map_or_else(|| "never".into(), day),
                    key.expires_at.map_or_else(|| "never".into(), day),
                );
            }
        }
        AdminCommand::RevokeApiKey { user, id } => {
            let user = find_user(db, &user).await?;
            if !api_keys::revoke(db, user.id, id).await? {
                bail!("{} has no API key {id}", user.name);
            }
            println!("revoked key {id} of {}", user.name);
        }
        AdminCommand::RegenerateMedia { all, posts } => {
            let queued = regenerate_media(db, (!all).then_some(posts.as_slice())).await?;
            println!("queued {queued} file(s) for processing");
        }
        AdminCommand::ImportRemoteFailures { site } => {
            let site = site.as_deref().map(|s| s.trim_end_matches('/'));
            let failures = remote_imports::failures(db, site).await?;
            if failures.is_empty() {
                println!("no failed posts");
            }
            for failure in failures {
                println!(
                    "{}\t{}\t{}\t{:?}\t{}",
                    failure.site,
                    failure.remote_id,
                    failure.failed_at.date(),
                    failure.query,
                    failure.reason,
                );
            }
        }
        AdminCommand::Export(_) | AdminCommand::Import(_) | AdminCommand::ImportRemote(_) => {
            unreachable!("imports need the whole app; main runs them")
        }
        AdminCommand::Bench {
            runs,
            check,
            explain,
        } => bench(db, &config.search, runs, check, explain.as_deref()).await?,
        AdminCommand::BenchHttp {
            url,
            runs,
            concurrency,
            only,
            check_ms,
        } => {
            crate::bench_http::run(
                db,
                crate::bench_http::Options {
                    url: &url,
                    runs,
                    concurrency,
                    only: only.as_deref(),
                    check: check_ms.map(Duration::from_millis),
                },
            )
            .await?;
        }
        AdminCommand::Seed {
            posts,
            tags,
            seed,
            batch,
            force,
        } => {
            let options = moekura_db::seed::Options {
                posts,
                tags,
                seed,
                batch_size: batch.max(1),
            };
            seed_posts(db, &options, force).await?;
        }
        AdminCommand::TagBacklog { all, limit } => {
            let posts =
                moekura_db::tag_suggestions::backlog(db, all, limit.unwrap_or(i64::MAX)).await?;
            // Batches keep each transaction short on large sites.
            for batch in posts.chunks(1000) {
                let mut tx = db.begin().await?;
                for &post_id in batch {
                    moekura_db::jobs::enqueue(&mut tx, &moekura_core::jobs::TagPost { post_id })
                        .await?;
                }
                tx.commit().await?;
            }
            println!("queued {} post(s) for the tagger", posts.len());
            if !config.tagger.enabled {
                println!(
                    "note: tagger.enabled is off here, so new uploads won't be queued; \
                     run `moekura tagger` to work through these"
                );
            }
        }
        AdminCommand::RecountTags => {
            let fixed = moekura_db::tags::recount(db).await?;
            println!("corrected {fixed} tag count(s)");
        }
        AdminCommand::SendTestMail { to } => {
            if !config.mail.is_enabled() {
                bail!("mail is off: set mail.host and mail.from first");
            }
            let mailer = moekura_jobs::mail::Mailer::new(&config.mail)?;
            let body = format!(
                "This is a test message from Moekura at {}.\n\nIf you can read it, mail works.\n",
                config.server.public_url
            );
            mailer.send(&to, "Moekura test message", &body).await?;
            println!("sent a test message to {to}");
        }
        AdminCommand::Settings { action: None } => {
            for (key, value) in settings::load(db).await?.to_map() {
                println!("{key} = {value}");
            }
        }
        AdminCommand::Settings {
            action: Some(SettingsAction::Set { key, value }),
        } => {
            let value = parse_setting_value(&value);
            let mut tx = db.begin().await?;
            let from = settings::load(&mut *tx).await?.to_map().remove(&key);
            settings::set_in(&mut tx, &key, value.clone()).await?;
            mod_actions::record(
                &mut *tx,
                NewAction::new(None, ActionKind::SettingUpdate).details(serde_json::json!({
                    "key": key, "from": from, "value": value, "via": "cli",
                })),
            )
            .await?;
            tx.commit().await?;
            println!("{key} = {value}");
        }
    }
    Ok(())
}

pub async fn create_user(
    db: &PgPool,
    name: &str,
    role: &str,
    email: Option<&str>,
    password: &str,
) -> anyhow::Result<User> {
    let role = find_role(db, role).await?;
    let account = NewAccount {
        name,
        password,
        email,
        role_id: role.id,
        status: UserStatus::Active,
    };
    let mut user = accounts::create(db, account).await?;
    // The first admin of a new site owns it.
    if role.system == Some(SystemRole::Admin) && !users::owner_exists(db).await? {
        let mut tx = db.begin().await?;
        users::set_owner(&mut tx, user.id).await?;
        tx.commit().await?;
        user.is_owner = true;
    }
    Ok(user)
}

pub async fn create_bot(db: &PgPool, name: &str, role: &str) -> anyhow::Result<User> {
    let role = find_role(db, role).await?;
    Ok(accounts::create_bot(db, name, role.id).await?)
}

/// Marks `name` as a bot or not. Becoming one ends their sessions and
/// revokes their feed token, which a bot couldn't revoke itself; their API
/// keys keep working.
pub async fn set_bot(db: &PgPool, name: &str, is_bot: bool) -> anyhow::Result<User> {
    let user = find_user(db, name).await?;
    let mut tx = db.begin().await?;
    users::set_bot(&mut *tx, user.id, is_bot).await?;
    if is_bot {
        moekura_db::sessions::delete_all_for_user(&mut *tx, user.id).await?;
        moekura_db::feeds::set_token(&mut *tx, user.id, None).await?;
    }
    tx.commit().await?;
    Ok(user)
}

/// Makes `name` the site's owner, in place of whoever was.
pub async fn set_owner(db: &PgPool, name: &str) -> anyhow::Result<User> {
    let user = find_user(db, name).await?;
    if user.is_bot {
        bail!("{name} is a bot; bots can't own the site");
    }
    let mut tx = db.begin().await?;
    users::set_owner(&mut tx, user.id).await?;
    mod_actions::record(
        &mut *tx,
        NewAction::new(None, ActionKind::UserOwner)
            .user(user.id)
            .details(serde_json::json!({ "via": "cli" })),
    )
    .await?;
    tx.commit().await?;
    Ok(user)
}

pub async fn set_role(db: &PgPool, name: &str, role: &str) -> anyhow::Result<Role> {
    let role = find_role(db, role).await?;
    let Some(user) = users::by_name(db, name).await? else {
        bail!("no user named {name}");
    };
    let from = roles::list(db)
        .await?
        .into_iter()
        .find(|r| r.id == user.role_id)
        .map(|r| r.name);
    let mut tx = db.begin().await?;
    users::set_role(&mut *tx, user.id, role.id).await?;
    mod_actions::record(
        &mut *tx,
        NewAction::new(None, ActionKind::UserRole)
            .user(user.id)
            .details(serde_json::json!({ "from": from, "role": role.name, "via": "cli" })),
    )
    .await?;
    tx.commit().await?;
    Ok(role)
}

/// Makes an API key for `user`, limited to `permissions` (by key) unless
/// that's empty, and returns it.
pub async fn create_api_key(
    db: &PgPool,
    user: &str,
    name: &str,
    permissions: &[String],
    expires_days: Option<u32>,
) -> anyhow::Result<String> {
    let user = find_user(db, user).await?;
    let name = name.trim();
    if name.is_empty() || name.chars().count() > api_keys::NAME_MAX_LEN {
        bail!(
            "give the key a name of at most {} characters",
            api_keys::NAME_MAX_LEN
        );
    }
    let permissions = if permissions.is_empty() {
        Permissions::ALL
    } else {
        let role = roles::list(db)
            .await?
            .into_iter()
            .find(|r| r.id == user.role_id)
            .context("the user's role is missing")?;
        let mut chosen = Vec::new();
        for key in permissions {
            let Some(permission) = Permission::ALL.into_iter().find(|p| p.key() == key) else {
                let known: Vec<&str> = Permission::ALL.iter().map(|p| p.key()).collect();
                bail!("no permission {key} (permissions: {})", known.join(", "));
            };
            if !role.can(permission) {
                bail!(
                    "{} ({}) can't {key}, so the key couldn't either",
                    user.name,
                    role.name
                );
            }
            chosen.push(permission);
        }
        Permissions::of(&chosen)
    };
    let expires_at =
        expires_days.map(|d| time::OffsetDateTime::now_utc() + time::Duration::days(i64::from(d)));
    let key = api_keys::create_with_permissions(db, user.id, name, expires_at, permissions).await?;
    tracing::info!(user = %user.name, key = name, "API key created from the shell");
    Ok(key)
}

/// Lifts the network bans in force that cover `text` (an address or a
/// range) or lie within it, and logs each; returns their networks.
pub async fn lift_network_bans(db: &PgPool, text: &str) -> anyhow::Result<Vec<String>> {
    let text = text.trim();
    // A range, or a single address as one.
    let Some(network) = text
        .parse()
        .ok()
        .or_else(|| text.parse::<IpAddr>().ok().map(Into::into))
    else {
        bail!("give an address or a range like 203.0.113.0/24");
    };
    let mut tx = db.begin().await?;
    let lifted: Vec<String> = bans::lift_networks_overlapping(&mut tx, network, None)
        .await?
        .iter()
        .map(ToString::to_string)
        .collect();
    for network in &lifted {
        mod_actions::record(
            &mut *tx,
            NewAction::new(None, ActionKind::IpUnban)
                .details(serde_json::json!({ "network": network, "via": "cli" })),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(lifted)
}

/// Queues processing for the given posts' files (all when `None`).
/// Returns how many were queued.
pub async fn regenerate_media(db: &PgPool, posts: Option<&[i64]>) -> anyhow::Result<usize> {
    let asset_ids = moekura_db::media::asset_ids(db, posts).await?;
    // Batches keep each transaction short on large sites.
    for batch in asset_ids.chunks(1000) {
        let mut tx = db.begin().await?;
        for &asset_id in batch {
            moekura_db::jobs::enqueue(&mut tx, &ProcessMedia { asset_id }).await?;
        }
        tx.commit().await?;
    }
    Ok(asset_ids.len())
}

async fn find_user(db: &PgPool, name: &str) -> anyhow::Result<User> {
    users::by_name(db, name)
        .await?
        .with_context(|| format!("no user named {name}"))
}

/// By display name, then by built-in key, so `admin` works even if the
/// Admin role was renamed.
async fn find_role(db: &PgPool, name: &str) -> anyhow::Result<Role> {
    if let Some(role) = roles::by_name(db, name).await? {
        return Ok(role);
    }
    if let Some(system) = SystemRole::from_key(&name.to_ascii_lowercase()) {
        return Ok(roles::by_system(db, system).await?);
    }
    let known: Vec<String> = roles::list(db).await?.into_iter().map(|r| r.name).collect();
    bail!("no role named {name} (roles: {})", known.join(", "))
}

fn parse_setting_value(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
}

fn read_password() -> anyhow::Result<String> {
    if !std::io::stdin().is_terminal() {
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .context("could not read password from stdin")?;
        return Ok(line.trim_end_matches(['\r', '\n']).to_owned());
    }
    let password = rpassword::prompt_password("Password: ")?;
    if rpassword::prompt_password("Repeat password: ")? != password {
        bail!("passwords do not match");
    }
    Ok(password)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn creates_admins_by_key_or_name(pool: PgPool) {
        let user = create_user(&pool, "catherine", "admin", None, "correct horse")
            .await
            .unwrap();
        let admin = roles::by_system(&pool, SystemRole::Admin).await.unwrap();
        assert_eq!(user.role_id, admin.id);

        // Renamed built-in roles are still found by key.
        sqlx::query("UPDATE roles SET name = 'Owner' WHERE system_key = 'admin'")
            .execute(&pool)
            .await
            .unwrap();
        let user = create_user(&pool, "second", "admin", None, "correct horse")
            .await
            .unwrap();
        assert_eq!(user.role_id, admin.id);
        let user = create_user(&pool, "third", "OWNER", None, "correct horse")
            .await
            .unwrap();
        assert_eq!(user.role_id, admin.id);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn hands_the_site_over(pool: PgPool) {
        let first = create_user(&pool, "first", "admin", None, "correct horse")
            .await
            .unwrap();
        assert!(first.is_owner, "the first admin owns the site");
        let second = create_user(&pool, "second", "admin", None, "correct horse")
            .await
            .unwrap();
        assert!(!second.is_owner);
        set_owner(&pool, "Second").await.unwrap();
        let owners: Vec<String> = sqlx::query_scalar("SELECT name::text FROM users WHERE is_owner")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(owners, ["second"]);
        let logged: String = sqlx::query_scalar("SELECT action FROM mod_actions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(logged, "user.owner");
        assert!(set_owner(&pool, "nobody").await.is_err());
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn reports_unknown_roles_and_users(pool: PgPool) {
        let err = create_user(&pool, "catherine", "wizard", None, "correct horse")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no role named wizard"), "{err}");
        let err = set_role(&pool, "nobody", "admin").await.unwrap_err();
        assert!(err.to_string().contains("no user named nobody"), "{err}");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn changes_roles(pool: PgPool) {
        create_user(&pool, "catherine", "member", None, "correct horse")
            .await
            .unwrap();
        let role = set_role(&pool, "Catherine", "moderator").await.unwrap();
        let user = users::by_name(&pool, "catherine").await.unwrap().unwrap();
        assert_eq!(user.role_id, role.id);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn regenerates_selected_or_all_media(pool: PgPool) {
        // Two posts with files, straight into the tables.
        for n in 1..=2u8 {
            sqlx::query(
                "WITH p AS (INSERT INTO posts (rating) VALUES ('g') RETURNING id)
                 INSERT INTO media_assets (post_id, sha256, md5, media_type, width, height, file_size, storage_key)
                 SELECT id, $1, $2, 'png', 1, 1, 1, 'k' FROM p",
            )
            .bind(vec![n; 32])
            .bind(vec![n; 16])
            .execute(&pool)
            .await
            .unwrap();
        }
        let first_post: i64 = sqlx::query_scalar("SELECT min(id) FROM posts")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            regenerate_media(&pool, Some(&[first_post, 999]))
                .await
                .unwrap(),
            1
        );
        assert_eq!(regenerate_media(&pool, None).await.unwrap(), 2);
        assert_eq!(moekura_db::jobs::counts(&pool).await.unwrap().queued, 3);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn lifts_network_bans_from_the_shell(pool: PgPool) {
        let mut conn = pool.acquire().await.unwrap();
        for range in ["203.0.113.0/24", "198.51.100.0/24"] {
            bans::ban_network(&mut conn, range.parse().unwrap(), "x", None, true, None)
                .await
                .unwrap();
        }
        drop(conn);
        assert!(lift_network_bans(&pool, "nonsense").await.is_err());
        assert_eq!(
            lift_network_bans(&pool, " 203.0.113.7 ").await.unwrap(),
            ["203.0.113.0/24"]
        );
        assert!(
            lift_network_bans(&pool, "203.0.113.7")
                .await
                .unwrap()
                .is_empty()
        );
        let in_force = bans::networks_in_force(&pool).await.unwrap();
        assert_eq!(in_force.len(), 1);
        let logged: serde_json::Value =
            sqlx::query_scalar("SELECT details FROM mod_actions WHERE action = 'network.unban'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(logged["via"], json!("cli"));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn creates_api_keys_from_the_shell(pool: PgPool) {
        let bot = create_user(&pool, "uploader", "member", None, "correct horse")
            .await
            .unwrap();
        let all = create_api_key(&pool, "Uploader", "everything", &[], None)
            .await
            .unwrap();
        let found = api_keys::lookup(&pool, &all).await.unwrap().unwrap();
        assert_eq!(found.user.id, bot.id);
        assert_eq!(found.permissions, Permissions::ALL);

        let some = ["view_posts".to_owned(), "upload".to_owned()];
        let key = create_api_key(&pool, "uploader", "posts", &some, Some(30))
            .await
            .unwrap();
        let found = api_keys::lookup(&pool, &key).await.unwrap().unwrap();
        assert_eq!(
            found.permissions,
            Permissions::of(&[Permission::ViewPosts, Permission::Upload])
        );
        let listed = api_keys::list(&pool, bot.id).await.unwrap();
        assert!(listed[1].expires_at.is_some());

        for (user, name, permissions, error) in [
            ("nobody", "x", vec![], "no user named nobody"),
            ("uploader", " ", vec![], "give the key a name"),
            ("uploader", "x", vec!["fly".to_owned()], "no permission fly"),
            (
                "uploader",
                "x",
                vec!["ban_users".to_owned()],
                "uploader (Member) can't ban_users",
            ),
        ] {
            let err = create_api_key(&pool, user, name, &permissions, None)
                .await
                .unwrap_err();
            assert!(err.to_string().contains(error), "{err}");
        }
        assert_eq!(api_keys::list(&pool, bot.id).await.unwrap().len(), 2);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn makes_bots(pool: PgPool) {
        let bot = create_bot(&pool, "tagbot", "contributor").await.unwrap();
        assert!(bot.is_bot);
        assert!(!users::has_password(&pool, bot.id).await.unwrap());
        assert!(
            users::by_name(&pool, "tagbot")
                .await
                .unwrap()
                .unwrap()
                .is_bot
        );
        assert!(create_bot(&pool, "x", "member").await.is_err());

        let person = create_user(&pool, "catherine", "member", None, "correct horse")
            .await
            .unwrap();
        moekura_db::sessions::create(
            &pool,
            moekura_db::sessions::NewSession {
                user_id: person.id,
                user_agent: None,
                ip: None,
            },
            moekura_db::sessions::Lifetime {
                idle: Duration::from_secs(3600),
                max: Duration::from_secs(3600),
            },
        )
        .await
        .unwrap();
        let key = create_api_key(&pool, "catherine", "script", &[], None)
            .await
            .unwrap();
        let feed = moekura_core::tokens::NewToken::generate();
        moekura_db::feeds::set_token(&pool, person.id, Some(&feed.hash))
            .await
            .unwrap();
        set_bot(&pool, "Catherine", true).await.unwrap();
        assert!(
            !moekura_db::feeds::has_token(&pool, person.id)
                .await
                .unwrap()
        );
        let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(sessions, 0);
        let found = api_keys::lookup(&pool, &key).await.unwrap().unwrap();
        assert!(found.user.is_bot);
        set_bot(&pool, "catherine", false).await.unwrap();
        assert!(
            !users::by_id(&pool, person.id)
                .await
                .unwrap()
                .unwrap()
                .is_bot
        );
    }

    #[test]
    fn setting_values_parse_as_json_or_string() {
        assert_eq!(parse_setting_value("closed"), json!("closed"));
        assert_eq!(parse_setting_value("\"quoted\""), json!("quoted"));
        assert_eq!(parse_setting_value("My Booru"), json!("My Booru"));
        assert_eq!(parse_setting_value("42"), json!(42));
    }
}
