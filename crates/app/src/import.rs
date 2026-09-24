//! `uwubooru admin import`: a folder of files, with tags from sidecar
//! files, as posts.

use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use clap::Args;
use uwu_core::config::Config;
use uwu_core::import::{Sidecar, is_media, parse_json, parse_rating, parse_txt, sidecar_paths};
use uwu_db::Db;
use uwu_db::site_cache::SiteCache;
use uwu_web::AppState;
use uwu_web::import::{ImportFile, Imported, existing_post, import_file, usable_tags};

#[derive(Args)]
pub struct ImportArgs {
    /// The folder of files to import
    dir: PathBuf,
    /// Who the posts are uploaded by
    #[arg(long)]
    uploader: String,
    /// Rating for files whose sidecar doesn't give one: g, s, q or e
    #[arg(long)]
    rating: Option<String>,
    /// Tags to add to every file, separated by spaces
    #[arg(long, default_value = "")]
    tags: String,
    /// Also import the files in subfolders
    #[arg(short, long)]
    recursive: bool,
    /// Show what would happen, without importing anything
    #[arg(long)]
    dry_run: bool,
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

/// Everything the sidecars next to `path` say, JSON first.
fn read_sidecars(path: &Path) -> Result<Sidecar, String> {
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
            let parsed = parse_json(&text).map_err(|e| format!("{}: {e}", candidate.display()))?;
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
    let uploader = uwu_db::users::by_name(db.primary(), &args.uploader)
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
    let file_key = uwu_db::secrets::get_or_create(
        db.primary(),
        "file_urls",
        uwu_core::tokens::NewToken::generate().hash,
    )
    .await
    .context("could not load the file URL key")?;
    let state = AppState::new(config, db.clone(), site, file_key)?;
    let extra: Vec<String> = args.tags.split_whitespace().map(str::to_owned).collect();

    let mut counts = Counts::default();
    for path in &files {
        let name = path.strip_prefix(&args.dir).unwrap_or(path).display();
        let mut fail = |message: &str| {
            counts.failed += 1;
            println!("failed     {name}: {message}");
        };
        let sidecar = match read_sidecars(path) {
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
        match import_file(&state, &uploader, file).await {
            Ok(Imported::Created(id)) => {
                counts.imported += 1;
                println!("imported   {name} as post #{id}");
            }
            Ok(Imported::Duplicate(id)) => {
                counts.duplicates += 1;
                println!("duplicate  {name}: already post #{id}");
            }
            Err(error) => fail(&error),
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
        let dir = std::env::temp_dir().join(format!("uwu-import-{}", std::process::id()));
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

        let sidecar = read_sidecars(&dir.join("b.png")).unwrap();
        assert_eq!(sidecar.tags, ["cat", "dog"]);
        // The JSON sidecar's rating wins.
        assert_eq!(sidecar.rating, Some(uwu_core::posts::Rating::Explicit));
        assert_eq!(
            read_sidecars(&dir.join("sub/c.webm")).unwrap(),
            Sidecar::default()
        );

        std::fs::write(dir.join("b.json"), "{").unwrap();
        assert!(
            read_sidecars(&dir.join("b.png"))
                .unwrap_err()
                .contains("b.json")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
