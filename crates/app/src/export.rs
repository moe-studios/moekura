//! `moekura admin export`: posts' original files, each with a JSON sidecar
//! of its tags, rating, source, description and parent, in a folder
//! `moekura admin import` reads back (here or on another site).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use clap::Args;
use moekura_core::config::Config;
use moekura_core::import::Export;
use moekura_core::posts::PostStatus;
use moekura_db::Db;
use moekura_db::posts::Visibility;
use moekura_db::search::{PageRef, Plan};
use moekura_storage::{Key, Storage};
use serde_json::Value;

/// Posts looked up at a time.
const BATCH: u32 = 200;

#[derive(Args)]
pub struct ExportArgs {
    /// The folder to write to; made if missing
    dir: PathBuf,
    /// Only posts matching this search, as on the site (in id order: no
    /// `order:`)
    #[arg(long, default_value = "")]
    tags: String,
    /// Also deleted posts, unless the search says which statuses
    #[arg(long)]
    include_deleted: bool,
    /// Show what would be written, without writing anything
    #[arg(long)]
    dry_run: bool,
}

/// Every status: an export is the admin's, not a visitor's.
fn everything(include_deleted: bool) -> Visibility {
    Visibility {
        statuses: vec![
            PostStatus::Pending,
            PostStatus::Active,
            PostStatus::Flagged,
            PostStatus::Deleted,
        ],
        viewer: None,
        ratings: Vec::new(),
        deleted_by_default: include_deleted,
        hidden_tags: Vec::new(),
    }
}

/// `0000000123.png`: zero-padded, so a folder lists (and imports) oldest
/// first, parents before their children.
fn file_name(post_id: i64, key: &Key) -> String {
    format!("{post_id:010}.{}", key.extension())
}

/// What happened to one post.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Written,
    /// The file was there already (an earlier run); the sidecar is
    /// rewritten in case the post changed.
    Kept,
}

#[derive(Default)]
struct Counts {
    written: usize,
    kept: usize,
    failed: usize,
}

pub async fn run(config: Config, db: &Db, args: ExportArgs) -> anyhow::Result<()> {
    let storage = Storage::from_config(&config.storage).context("could not open file storage")?;
    let pool = db.primary();
    let query = moekura_core::search::Query::parse(args.tags.trim())
        .map_err(|e| anyhow::anyhow!("--tags: {e}"))?;
    let mut paged = query.clone();
    paged.limit = Some(BATCH);
    let mut search = config.search.clone();
    search.max_per_page = search.max_per_page.max(BATCH);
    let plan = Plan::resolve(pool, &paged, &everything(args.include_deleted), &search)
        .await
        .map_err(|e| anyhow::anyhow!("--tags: {e}"))?;
    if !plan.supports_keyset() {
        bail!("--tags: exports go in post order; leave out order:");
    }
    if !args.dry_run {
        tokio::fs::create_dir_all(&args.dir)
            .await
            .with_context(|| format!("could not make {}", args.dir.display()))?;
    }
    let categories: HashMap<i16, String> = moekura_db::tags::categories(pool)
        .await?
        .into_iter()
        .map(|c| (c.id, c.name))
        .collect();

    let mut counts = Counts::default();
    let mut page = PageRef::default();
    loop {
        let ids = plan
            .ids(pool, page)
            .await
            .map_err(|e| anyhow::anyhow!("search: {e}"))?;
        let Some(&last) = ids.last() else { break };
        page = PageRef::Before(last);
        for id in ids {
            match export_post(pool, &storage, &categories, &args, id).await {
                Ok(Some((name, Outcome::Written))) => {
                    counts.written += 1;
                    println!(
                        "{} {name}",
                        if args.dry_run {
                            "would write"
                        } else {
                            "wrote      "
                        }
                    );
                }
                Ok(Some((name, Outcome::Kept))) => {
                    counts.kept += 1;
                    println!("kept        {name}");
                }
                // Gone since the search, or without a file.
                Ok(None) => {}
                Err(error) => {
                    counts.failed += 1;
                    println!("failed      post #{id}: {error:#}");
                }
            }
        }
    }
    let verb = if args.dry_run {
        "would be written"
    } else {
        "written"
    };
    println!(
        "\n{} {verb}, {} already there, {} failed",
        counts.written, counts.kept, counts.failed
    );
    if counts.failed > 0 {
        bail!(
            "{} post{} could not be exported",
            counts.failed,
            if counts.failed == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

/// Writes post `id`'s file (unless it's there already) and its sidecar.
async fn export_post(
    pool: &sqlx::PgPool,
    storage: &Storage,
    categories: &HashMap<i16, String>,
    args: &ExportArgs,
    id: i64,
) -> anyhow::Result<Option<(String, Outcome)>> {
    let (Some(post), Some(asset)) = (
        moekura_db::posts::by_id(pool, id).await?,
        moekura_db::media::for_post(pool, id).await?,
    ) else {
        return Ok(None);
    };
    let key = Key::parse(&asset.storage_key)
        .with_context(|| format!("bad storage key {}", asset.storage_key))?;
    let name = file_name(post.id, &key);
    let path = args.dir.join(&name);
    let sidecar = sidecar(pool, categories, &post, &asset).await?;
    let kept = tokio::fs::metadata(&path)
        .await
        .is_ok_and(|m| m.len() == u64::try_from(asset.file_size).unwrap_or(u64::MAX));
    if args.dry_run {
        return Ok(Some((
            name,
            if kept {
                Outcome::Kept
            } else {
                Outcome::Written
            },
        )));
    }
    if !kept {
        // Under another name until complete, so a stopped export leaves
        // no half file to be kept next time.
        let partial = args.dir.join(format!("{name}.part"));
        storage
            .download(&key, &partial)
            .await
            .with_context(|| format!("could not read {}", asset.storage_key))?;
        tokio::fs::rename(&partial, &path).await?;
    }
    let json = serde_json::to_string_pretty(&sidecar)?;
    write_if_changed(&args.dir.join(format!("{name}.json")), json.as_bytes()).await?;
    Ok(Some((
        name,
        if kept {
            Outcome::Kept
        } else {
            Outcome::Written
        },
    )))
}

/// A post's sidecar: its tags by category, and how to find its parent.
async fn sidecar(
    pool: &sqlx::PgPool,
    categories: &HashMap<i16, String>,
    post: &moekura_db::posts::Post,
    asset: &moekura_db::media::Asset,
) -> anyhow::Result<Export> {
    let mut tags = serde_json::Map::new();
    let mut found = moekura_db::tags::by_ids(pool, &post.tag_ids).await?;
    found.sort_by(|a, b| a.name.cmp(&b.name));
    for tag in found {
        let category = categories
            .get(&tag.category_id)
            .cloned()
            .unwrap_or_else(|| "general".to_owned());
        if let Value::Array(list) = tags
            .entry(category)
            .or_insert_with(|| Value::Array(Vec::new()))
        {
            list.push(Value::String(tag.name));
        }
    }
    let parent_sha256 = match post.parent_id {
        Some(parent) => moekura_db::media::for_post(pool, parent)
            .await?
            .map(|a| a.sha256_hex()),
        None => None,
    };
    Ok(Export {
        id: post.id,
        status: post.status.as_str().to_owned(),
        rating: post.rating.code().to_owned(),
        tags,
        source: post.source.clone(),
        description: post.description.clone(),
        parent_id: post.parent_id,
        parent_sha256,
        sha256: asset.sha256_hex(),
        md5: asset.md5.iter().map(|b| format!("{b:02x}")).collect(),
        media_type: asset.media_type.clone(),
    })
}

async fn write_if_changed(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if tokio::fs::read(path).await.is_ok_and(|old| old == bytes) {
        return Ok(());
    }
    tokio::fs::write(path, bytes).await
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use moekura_db::site_cache::SiteCache;
    use moekura_web::AppState;
    use moekura_web::import::{ImportFile, Imported, import_file};
    use sqlx::PgPool;

    use super::*;

    fn png(dir: &Path, name: &str, size: u32) -> PathBuf {
        let path = dir.join(name);
        let status = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
            ])
            .arg(format!("testsrc2=size={size}x{size}:duration=1"))
            .args(["-frames:v", "1"])
            .arg(&path)
            .status()
            .expect("ffmpeg is needed for export tests");
        assert!(status.success());
        path
    }

    fn sha256(path: &Path) -> Vec<u8> {
        use sha2::Digest;
        sha2::Sha256::digest(std::fs::read(path).unwrap()).to_vec()
    }

    struct Found {
        tags: Vec<String>,
        rating: String,
        source: String,
        description: String,
        parent_sha256: Option<Vec<u8>>,
    }

    /// The post with the file `sha256`, as category:name tags.
    async fn found(pool: &PgPool, sha256: &[u8]) -> Found {
        sqlx::query_as::<_, (Vec<String>, String, String, String, Option<Vec<u8>>)>(
            "SELECT array(SELECT c.name || ':' || t.name FROM tags t
                          JOIN tag_categories c ON c.id = t.category_id
                          WHERE t.id = ANY(p.tag_ids) ORDER BY t.name),
                    p.rating::text, p.source, p.description,
                    (SELECT pa.sha256 FROM media_assets pa WHERE pa.post_id = p.parent_id)
             FROM posts p JOIN media_assets a ON a.post_id = p.id WHERE a.sha256 = $1",
        )
        .bind(sha256)
        .fetch_one(pool)
        .await
        .map(|(tags, rating, source, description, parent_sha256)| Found {
            tags,
            rating,
            source,
            description,
            parent_sha256,
        })
        .unwrap()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn round_trips_through_import(pool: PgPool) {
        let root = std::env::temp_dir().join(format!("moekura-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let files = root.join("files");
        std::fs::create_dir_all(&files).unwrap();
        let mut config = Config::default();
        config.storage.path = root.join("storage");
        config.media.work_dir = Some(root.join("work"));
        let db = Db::from_pools(pool.clone(), vec![]);
        let state = AppState::new(
            config.clone(),
            db.clone(),
            SiteCache::load(&pool).await.unwrap(),
            [7; 32],
        )
        .unwrap();
        sqlx::query("INSERT INTO tag_categories (id, name, label, position) VALUES (6, 'species', 'Species', 5)")
            .execute(&pool)
            .await
            .unwrap();
        let admin = crate::admin::create_user(&pool, "boss", "admin", None, "correct horse")
            .await
            .unwrap();

        let parent_file = png(&files, "parent.png", 40);
        let child_file = png(&files, "child.png", 44);
        let mut made = Vec::new();
        for (path, tags, rating) in [
            (
                &parent_file,
                vec![
                    "artist:someone".to_owned(),
                    "species:cat".to_owned(),
                    "sky".to_owned(),
                ],
                moekura_core::posts::Rating::Questionable,
            ),
            (
                &child_file,
                vec!["sky".to_owned()],
                moekura_core::posts::Rating::General,
            ),
        ] {
            let file = ImportFile {
                path,
                rating,
                tags: &tags,
                source: "https://example.com/art",
                description: "a test pattern",
            };
            let Imported::Created(id) = import_file(&state, &admin, file).await.unwrap().imported
            else {
                panic!("not created");
            };
            made.push(id);
        }
        sqlx::query("UPDATE posts SET parent_id = $1 WHERE id = $2")
            .bind(made[0])
            .bind(made[1])
            .execute(&pool)
            .await
            .unwrap();

        let out = root.join("export");
        let export = |dry_run| ExportArgs {
            dir: out.clone(),
            tags: String::new(),
            include_deleted: false,
            dry_run,
        };
        run(config.clone(), &db, export(false)).await.unwrap();
        let mut names: Vec<String> = std::fs::read_dir(&out)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        let first = format!("{:010}.png", made[0]);
        assert_eq!(names[0], first);
        assert_eq!(names.len(), 4, "{names:?}");
        let exported: Export = serde_json::from_str(
            &std::fs::read_to_string(out.join(format!("{first}.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(exported.tags["species"], serde_json::json!(["cat"]));
        // The bytes as uploaded.
        assert_eq!(sha256(&out.join(&first)), sha256(&parent_file));
        // Again: nothing to download.
        run(config.clone(), &db, export(false)).await.unwrap();

        let before_parent = found(&pool, &sha256(&parent_file)).await;
        let before_child = found(&pool, &sha256(&child_file)).await;
        assert_eq!(before_child.parent_sha256, Some(sha256(&parent_file)));
        sqlx::query("DELETE FROM media_assets")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM posts")
            .execute(&pool)
            .await
            .unwrap();

        crate::import::run(
            config,
            &db,
            crate::import::ImportArgs {
                dir: out.clone(),
                uploader: "boss".into(),
                rating: None,
                tags: String::new(),
                recursive: false,
                dry_run: false,
            },
        )
        .await
        .unwrap();
        for (before, file) in [(before_parent, &parent_file), (before_child, &child_file)] {
            let after = found(&pool, &sha256(file)).await;
            assert_eq!(after.tags, before.tags);
            assert_eq!(
                (after.rating, after.source, after.description),
                (before.rating, before.source, before.description)
            );
            assert_eq!(after.parent_sha256, before.parent_sha256);
        }
        assert_eq!(
            found(&pool, &sha256(&parent_file)).await.tags,
            ["species:cat", "general:sky", "artist:someone"]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
