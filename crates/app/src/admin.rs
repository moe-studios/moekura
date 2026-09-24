//! `uwubooru admin …`: account and settings management from the shell,
//! for bootstrapping an instance before anyone can log in.

use std::io::{BufRead, IsTerminal};
use std::time::Duration;

use anyhow::{Context, bail};
use clap::Subcommand;
use serde_json::Value;
use sqlx::PgPool;
use uwu_core::jobs::ProcessMedia;
use uwu_core::moderation::ActionKind;
use uwu_core::permissions::{Role, SystemRole};
use uwu_db::accounts::{self, NewAccount};
use uwu_db::mod_actions::{self, NewAction};
use uwu_db::users::{self, User, UserStatus};
use uwu_db::{invites, roles, settings};

#[derive(Subcommand)]
pub enum AdminCommand {
    /// Create an account. Prompts for the password, or reads one line from
    /// stdin when it is not a terminal.
    CreateUser {
        name: String,
        /// Role name or built-in key (anonymous, member, …, admin)
        #[arg(long, default_value = "member")]
        role: String,
        #[arg(long)]
        email: Option<String>,
    },
    /// Change a user's role
    SetRole { name: String, role: String },
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
    /// Import a folder of images and videos as posts, with tags from the
    /// sidecar files next to them (pic.png.txt, pic.json, …). Files already
    /// here are skipped, so an interrupted import can be run again.
    Import(crate::import::ImportArgs),
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
    options: &uwu_db::seed::Options,
    force: bool,
) -> anyhow::Result<()> {
    use uwu_db::seed;
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
    config: &uwu_core::config::SearchConfig,
    runs: usize,
    check: bool,
    explain: Option<&str>,
) -> anyhow::Result<()> {
    use uwu_db::bench;
    use uwu_db::search::Count;
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
            uwu_db::search::PageRef::Number(1) => String::new(),
            uwu_db::search::PageRef::Number(n) => format!(" (page {n})"),
            uwu_db::search::PageRef::Before(id) => format!(" (page b{id})"),
            uwu_db::search::PageRef::After(id) => format!(" (page a{id})"),
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
            let query = uwu_core::search::Query::parse(&case.query)?;
            let visitor = uwu_db::posts::Visibility {
                statuses: vec![
                    uwu_core::posts::PostStatus::Active,
                    uwu_core::posts::PostStatus::Flagged,
                ],
                viewer: None,
            };
            let plan = uwu_db::search::Plan::resolve(db, &query, &visitor, config).await?;
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
    config: &uwu_core::config::Config,
    command: AdminCommand,
) -> anyhow::Result<()> {
    match command {
        AdminCommand::CreateUser { name, role, email } => {
            let password = read_password()?;
            let user = create_user(db, &name, &role, email.as_deref(), &password).await?;
            println!("created {} (id {})", user.name, user.id);
        }
        AdminCommand::SetRole { name, role } => {
            let role = set_role(db, &name, &role).await?;
            println!("{name} is now {}", role.name);
        }
        AdminCommand::CreateInvite { uses, expires_days } => {
            let invite = invites::NewInvite {
                created_by: None,
                max_uses: i32::from(uses.max(1)),
                expires_in: expires_days.map(|d| Duration::from_secs(u64::from(d) * 86_400)),
            };
            println!("{}", invites::create(db, invite).await?);
        }
        AdminCommand::RegenerateMedia { all, posts } => {
            let queued = regenerate_media(db, (!all).then_some(posts.as_slice())).await?;
            println!("queued {queued} file(s) for processing");
        }
        AdminCommand::Import(_) => unreachable!("imports need the whole app; main runs them"),
        AdminCommand::Bench {
            runs,
            check,
            explain,
        } => bench(db, &config.search, runs, check, explain.as_deref()).await?,
        AdminCommand::Seed {
            posts,
            tags,
            seed,
            batch,
            force,
        } => {
            let options = uwu_db::seed::Options {
                posts,
                tags,
                seed,
                batch_size: batch.max(1),
            };
            seed_posts(db, &options, force).await?;
        }
        AdminCommand::RecountTags => {
            let fixed = uwu_db::tags::recount(db).await?;
            println!("corrected {fixed} tag count(s)");
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
            settings::set(db, &key, value.clone()).await?;
            mod_actions::record(
                db,
                NewAction::new(None, ActionKind::SettingUpdate)
                    .details(serde_json::json!({ "key": key, "value": value, "via": "cli" })),
            )
            .await?;
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
    Ok(accounts::create(db, account).await?)
}

pub async fn set_role(db: &PgPool, name: &str, role: &str) -> anyhow::Result<Role> {
    let role = find_role(db, role).await?;
    let Some(user) = users::by_name(db, name).await? else {
        bail!("no user named {name}");
    };
    users::set_role(db, user.id, role.id).await?;
    mod_actions::record(
        db,
        NewAction::new(None, ActionKind::UserRole)
            .user(user.id)
            .details(serde_json::json!({ "role": role.name, "via": "cli" })),
    )
    .await?;
    Ok(role)
}

/// Queues processing for the given posts' files (all when `None`).
/// Returns how many were queued.
pub async fn regenerate_media(db: &PgPool, posts: Option<&[i64]>) -> anyhow::Result<usize> {
    let asset_ids = uwu_db::media::asset_ids(db, posts).await?;
    // Batches keep each transaction short on large sites.
    for batch in asset_ids.chunks(1000) {
        let mut tx = db.begin().await?;
        for &asset_id in batch {
            uwu_db::jobs::enqueue(&mut tx, &ProcessMedia { asset_id }).await?;
        }
        tx.commit().await?;
    }
    Ok(asset_ids.len())
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

    #[sqlx::test(migrator = "uwu_db::MIGRATOR")]
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

    #[sqlx::test(migrator = "uwu_db::MIGRATOR")]
    async fn reports_unknown_roles_and_users(pool: PgPool) {
        let err = create_user(&pool, "catherine", "wizard", None, "correct horse")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no role named wizard"), "{err}");
        let err = set_role(&pool, "nobody", "admin").await.unwrap_err();
        assert!(err.to_string().contains("no user named nobody"), "{err}");
    }

    #[sqlx::test(migrator = "uwu_db::MIGRATOR")]
    async fn changes_roles(pool: PgPool) {
        create_user(&pool, "catherine", "member", None, "correct horse")
            .await
            .unwrap();
        let role = set_role(&pool, "Catherine", "moderator").await.unwrap();
        let user = users::by_name(&pool, "catherine").await.unwrap().unwrap();
        assert_eq!(user.role_id, role.id);
    }

    #[sqlx::test(migrator = "uwu_db::MIGRATOR")]
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
        assert_eq!(uwu_db::jobs::counts(&pool).await.unwrap().queued, 3);
    }

    #[test]
    fn setting_values_parse_as_json_or_string() {
        assert_eq!(parse_setting_value("closed"), json!("closed"));
        assert_eq!(parse_setting_value("\"quoted\""), json!("quoted"));
        assert_eq!(parse_setting_value("My Booru"), json!("My Booru"));
        assert_eq!(parse_setting_value("42"), json!(42));
    }
}
