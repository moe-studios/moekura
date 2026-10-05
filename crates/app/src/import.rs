//! `moekura admin import`: a folder of files, with tags from sidecar
//! files, as posts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use clap::Args;
use moekura_core::config::Config;
use moekura_core::import::{
    Sidecar, is_media, parse_json_for, parse_rating, parse_txt, sidecar_paths,
};
use moekura_db::Db;
use moekura_db::site_cache::SiteCache;
use moekura_web::AppState;
use moekura_web::import::{
    ImportFile, Imported, existing_post, import_file, link_parent, usable_tags,
};

#[derive(Args)]
pub struct ImportArgs {
    /// The folder of files to import
    pub(crate) dir: PathBuf,
    /// Who the posts are uploaded by
    #[arg(long)]
    pub(crate) uploader: String,
    /// Rating for files whose sidecar doesn't give one: g, s, q or e
    #[arg(long)]
    pub(crate) rating: Option<String>,
    /// Tags to add to every file, separated by spaces
    #[arg(long, default_value = "")]
    pub(crate) tags: String,
    /// Also import the files in subfolders
    #[arg(short, long)]
    pub(crate) recursive: bool,
    /// Show what would happen, without importing anything
    #[arg(long)]
    pub(crate) dry_run: bool,
}

/// The files to import in `dir`, by path. Hidden files and folders are
/// skipped.
fn collect(dir: &Path, recursive: bool) -> std::io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            if recursive {
                found.extend(collect(&path, true)?);
            }
        } else if is_media(&path) {
            found.push(path);
        }
    }
    found.sort();
    Ok(found)
}

/// Everything the sidecars next to `path` say, JSON first. Lists by
/// category keep the site's `categories`.
fn read_sidecars(path: &Path, categories: &[&str]) -> Result<Sidecar, String> {
    let mut sidecar = Sidecar::default();
    let (mut json_seen, mut txt_seen) = (false, false);
    for candidate in sidecar_paths(path) {
        let is_json = candidate.extension().is_some_and(|e| e == "json");
        if (is_json && json_seen) || (!is_json && txt_seen) || !candidate.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&candidate)
            .map_err(|e| format!("Could not read {}: {e}", candidate.display()))?;
        if is_json {
            json_seen = true;
            let parsed = parse_json_for(&text, categories)
                .map_err(|e| format!("{}: {e}", candidate.display()))?;
            sidecar.merge(parsed);
        } else {
            txt_seen = true;
            sidecar.merge(parse_txt(&text));
        }
    }
    Ok(sidecar)
}

#[derive(Default)]
struct Counts {
    imported: usize,
    duplicates: usize,
    failed: usize,
}

pub async fn run(config: Config, db: &Db, args: ImportArgs) -> anyhow::Result<()> {
    let default_rating = match &args.rating {
        None => None,
        Some(text) => Some(
            parse_rating(text)
                .ok_or_else(|| anyhow!("unknown rating {text:?}: use g, s, q or e"))?,
        ),
    };
    let uploader = moekura_db::users::by_name(db.primary(), &args.uploader)
        .await?
        .ok_or_else(|| anyhow!("there is no user called {:?}", args.uploader))?;
    let files = collect(&args.dir, args.recursive)
        .with_context(|| format!("could not read {}", args.dir.display()))?;
    if files.is_empty() {
        println!("no images or videos found in {}", args.dir.display());
        return Ok(());
    }

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
    let extra: Vec<String> = args.tags.split_whitespace().map(str::to_owned).collect();
    let categories = moekura_db::tags::categories(db.primary()).await?;
    let category_names: Vec<&str> = categories.iter().map(|c| c.name.as_str()).collect();

    let mut counts = Counts::default();
    // Posts and their parents' files, linked once every file is in.
    let mut parents: Vec<(i64, String, [u8; 32])> = Vec::new();
    // The post each file became, by the file's own SHA-256, which is what
    // sidecars name parents by: a post stored without metadata has
    // another.
    let mut posts: HashMap<[u8; 32], i64> = HashMap::new();
    for path in &files {
        let name = path.strip_prefix(&args.dir).unwrap_or(path).display();
        let mut fail = |message: &str| {
            counts.failed += 1;
            println!("failed     {name}: {message}");
        };
        let sidecar = match read_sidecars(path, &category_names) {
            Ok(sidecar) => sidecar,
            Err(error) => {
                fail(&error);
                continue;
            }
        };
        let Some(rating) = sidecar.rating.or(default_rating) else {
            fail("No rating: give --rating, or put one in its sidecar.");
            continue;
        };
        let mut tags = sidecar.tags;
        tags.extend(extra.iter().cloned());
        let (tags, invalid) = usable_tags(&state, &tags).await?;
        for problem in invalid {
            println!("           {name}: left out a tag: {problem}");
        }
        if args.dry_run {
            match existing_post(&state, path).await {
                Ok(Some(id)) => {
                    counts.duplicates += 1;
                    println!("duplicate  {name}: already post #{id}");
                }
                Ok(None) => {
                    counts.imported += 1;
                    println!("would add  {name} ({rating}): {tags}");
                }
                Err(error) => fail(&error),
            }
            continue;
        }
        let file = ImportFile {
            path,
            rating,
            tags: std::slice::from_ref(&tags),
            source: sidecar.source.as_deref().unwrap_or_default(),
            description: sidecar.description.as_deref().unwrap_or_default(),
        };
        let outcome = match import_file(&state, &uploader, file).await {
            Ok(outcome) => outcome,
            Err(error) => {
                fail(&error);
                continue;
            }
        };
        posts.insert(outcome.file_sha256, outcome.imported.post());
        match outcome.imported {
            Imported::Created(id) => {
                counts.imported += 1;
                println!("imported   {name} as post #{id}");
                if let Some(parent) = sidecar.parent_sha256 {
                    parents.push((id, name.to_string(), parent));
                }
            }
            Imported::Duplicate(id) => {
                counts.duplicates += 1;
                println!("duplicate  {name}: already post #{id}");
            }
        }
    }
    for (child, name, parent) in &parents {
        match link_parent(&state, &uploader, *child, parent, &posts).await {
            Ok(Some(parent)) => println!("parent     {name}: post #{parent}"),
            Ok(None) => println!("           {name}: its parent's file isn't here"),
            Err(error) => println!("           {name}: parent not set: {error}"),
        }
    }

    let verb = if args.dry_run {
        "would be imported"
    } else {
        "imported"
    };
    println!(
        "\n{} {verb}, {} already here, {} failed",
        counts.imported, counts.duplicates, counts.failed
    );
    if !args.dry_run && counts.imported > 0 {
        println!("thumbnails are made by the job workers (serve or worker)");
    }
    if counts.failed > 0 {
        bail!(
            "{} file{} could not be imported",
            counts.failed,
            if counts.failed == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_media_and_reads_sidecars() {
        let dir = std::env::temp_dir().join(format!("moekura-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        for name in [
            "b.png",
            "a.JPG",
            "a.JPG.txt",
            "notes.txt",
            "sub/c.webm",
            ".hidden/d.png",
        ] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        std::fs::write(dir.join("b.json"), r#"{"tags": ["cat"], "rating": "e"}"#).unwrap();
        std::fs::write(dir.join("b.png.txt"), "dog\nrating:g").unwrap();

        assert_eq!(
            collect(&dir, false).unwrap(),
            [dir.join("a.JPG"), dir.join("b.png")]
        );
        assert_eq!(collect(&dir, true).unwrap().len(), 3);

        let sidecar = read_sidecars(&dir.join("b.png"), &[]).unwrap();
        assert_eq!(sidecar.tags, ["cat", "dog"]);
        // The JSON sidecar's rating wins.
        assert_eq!(sidecar.rating, Some(moekura_core::posts::Rating::Explicit));
        assert_eq!(
            read_sidecars(&dir.join("sub/c.webm"), &[]).unwrap(),
            Sidecar::default()
        );

        std::fs::write(dir.join("b.json"), "{").unwrap();
        assert!(
            read_sidecars(&dir.join("b.png"), &[])
                .unwrap_err()
                .contains("b.json")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A JPEG made by ffmpeg, which writes a comment into it.
    fn jpeg(path: &Path, size: u32) {
        let status = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
            .arg("-i")
            .arg(format!("testsrc2=size={size}x{size}:duration=1"))
            .args(["-frames:v", "1"])
            .arg(path)
            .status()
            .expect("ffmpeg is needed for import tests");
        assert!(status.success());
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn finds_parents_by_the_files_they_were_imported_from(pool: sqlx::PgPool) {
        use sha2::Digest;

        let root =
            std::env::temp_dir().join(format!("moekura-import-parents-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("files");
        std::fs::create_dir_all(&dir).unwrap();
        let mut config = Config::default();
        config.storage.path = root.join("storage");
        config.media.work_dir = Some(root.join("work"));
        let db = Db::from_pools(pool.clone(), vec![]);
        crate::admin::create_user(&pool, "boss", "admin", None, "correct horse")
            .await
            .unwrap();
        // As an export from a site that keeps originals as uploaded has
        // them: the parent's comment is removed here, so its post gets
        // another SHA-256 than the one its child's sidecar names.
        let parent = dir.join("a.jpg");
        jpeg(&parent, 40);
        jpeg(&dir.join("b.jpg"), 44);
        let parent_sha256: String = sha2::Sha256::digest(std::fs::read(&parent).unwrap())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        std::fs::write(dir.join("a.jpg.json"), r#"{"rating": "s"}"#).unwrap();
        std::fs::write(
            dir.join("b.jpg.json"),
            format!(r#"{{"rating": "g", "parent_sha256": "{parent_sha256}"}}"#),
        )
        .unwrap();

        let args = ImportArgs {
            dir: dir.clone(),
            uploader: "boss".into(),
            rating: None,
            tags: String::new(),
            recursive: false,
            dry_run: false,
        };
        run(config, &db, args).await.unwrap();
        let posts: Vec<(i64, Option<i64>)> =
            sqlx::query_as("SELECT id, parent_id FROM posts ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(posts.len(), 2, "{posts:?}");
        assert_eq!(posts[0].1, None);
        assert_eq!(posts[1].1, Some(posts[0].0), "the child has its parent");
        let unchanged: Option<i64> =
            sqlx::query_scalar("SELECT post_id FROM media_assets WHERE sha256 = decode($1, 'hex')")
                .bind(&parent_sha256)
                .fetch_optional(&pool)
                .await
                .unwrap();
        assert_eq!(unchanged, None, "the parent was stored without its comment");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
