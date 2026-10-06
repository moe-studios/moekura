//! Importing files from disk as posts, for `moekura admin import`. Each
//! file goes through the same checks and steps as an upload.

use std::collections::HashMap;
use std::path::Path;

use moekura_core::posts::Rating;
use moekura_core::tags::parse_input;
use moekura_db::users::User;
use tokio::io::AsyncReadExt;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::upload::{
    TempUpload, TempWriter, UploadError, UploadFields, ingest, media_error, strip_metadata,
};

/// A file to import and what to give its post.
#[derive(Debug, Clone)]
pub struct ImportFile<'a> {
    pub path: &'a Path,
    pub rating: Rating,
    /// As the upload form takes them: `name` or `category:name`.
    pub tags: &'a [String],
    pub source: &'a str,
    pub description: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Imported {
    /// A new post.
    Created(i64),
    /// The file was already this post; nothing changed.
    Duplicate(i64),
}

impl Imported {
    /// The post the file is, new or not.
    pub fn post(self) -> i64 {
        match self {
            Self::Created(id) | Self::Duplicate(id) => id,
        }
    }
}

/// What [`import_file`] made of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub imported: Imported,
    /// The SHA-256 of the file as it is on disk, which sidecars name
    /// parents by. The post's is another when its metadata was removed.
    pub file_sha256: [u8; 32],
}

/// The tags of `tags` that are valid, as one input string, and the ones
/// that aren't, explained. Deprecated tags are refused later, by the
/// upload itself.
pub async fn usable_tags(state: &AppState, tags: &[String]) -> sqlx::Result<(String, Vec<String>)> {
    let categories = moekura_db::tags::categories(state.db.primary()).await?;
    let names: Vec<&str> = categories.iter().map(|c| c.name.as_str()).collect();
    let (valid, invalid) = parse_input(&tags.join(" "), &names);
    let input = valid
        .iter()
        .map(|t| match &t.category {
            Some(category) => format!("{category}:{}", t.name),
            None => t.name.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ");
    Ok((input, invalid.iter().map(ToString::to_string).collect()))
}

/// Copies `path` to scratch space while hashing it, as uploads are
/// received, within the upload size limit.
async fn receive(state: &AppState, path: &Path) -> Result<TempUpload, String> {
    let limit = state.media.config().max_upload_mb * 1024 * 1024;
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("Could not open it: {e}"))?;
    let mut writer = TempWriter::create(&state.work_dir)
        .await
        .map_err(|e| e.to_string())?;
    let mut buffer = vec![0; 256 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|e| format!("Could not read it: {e}"))?;
        if read == 0 {
            break;
        }
        if writer.written() + read as u64 > limit {
            return Err(format!(
                "It's larger than the upload limit of {} MB.",
                state.media.config().max_upload_mb
            ));
        }
        writer
            .write(&buffer[..read])
            .await
            .map_err(|e| e.to_string())?;
    }
    writer.finish().await.map_err(|e| e.to_string())
}

/// Imports one file as a post by `uploader`. `tags` should already be
/// [`usable_tags`]; an invalid one fails the file.
pub async fn import_file(
    state: &AppState,
    uploader: &User,
    file: ImportFile<'_>,
) -> Result<Outcome, String> {
    let received = receive(state, file.path).await?;
    let current = CurrentUser::for_user(uploader.clone(), None, &state.site.get());
    let fields = UploadFields {
        url: String::new(),
        rating: Some(file.rating),
        tags: file.tags.join(" "),
        source: file.source.to_owned(),
        description: file.description.to_owned(),
        ..UploadFields::default()
    };
    let imported = match ingest(state, &current, &received, &fields, false).await {
        Ok(id) => Imported::Created(id),
        Err(UploadError::Duplicate(id)) => Imported::Duplicate(id),
        Err(error) => return Err(error.to_string()),
    };
    Ok(Outcome {
        imported,
        file_sha256: received.sha256,
    })
}

/// Makes the post with the file `parent_sha256` the parent of post
/// `child`, as `uploader`: the post that file became in this import
/// (`imported`, by [`Outcome::file_sha256`]), or else the post whose
/// stored file it is. `None` when no post has that file (yet).
pub async fn link_parent(
    state: &AppState,
    uploader: &User,
    child: i64,
    parent_sha256: &[u8; 32],
    imported: &HashMap<[u8; 32], i64>,
) -> Result<Option<i64>, String> {
    let parent = match imported.get(parent_sha256) {
        Some(&post) => Some(post),
        None => moekura_db::media::post_with_sha256(state.db.primary(), parent_sha256)
            .await
            .map_err(|e| e.to_string())?,
    };
    let Some(parent) = parent else {
        return Ok(None);
    };
    let current = CurrentUser::for_user(uploader.clone(), None, &state.site.get());
    crate::edit::set_parent(state, &current, child, Some(parent))
        .await
        .map_err(|e| UploadError::from(e).to_string())?;
    Ok(Some(parent))
}

/// The post that already has the file at `path`, if any, without
/// importing it: as it is, or as it would be stored, without its
/// metadata, as uploads find duplicates.
pub async fn existing_post(state: &AppState, path: &Path) -> Result<Option<i64>, String> {
    let db = state.db.primary();
    let received = receive(state, path).await?;
    let found = moekura_db::media::post_with_sha256(db, &received.sha256)
        .await
        .map_err(|e| e.to_string())?;
    if found.is_some() {
        return Ok(found);
    }
    let media_type = state
        .media
        .identify(received.path())
        .await
        .map_err(|e| media_error(e).to_string())?;
    let Some(stripped) = strip_metadata(state, &received, media_type)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    moekura_db::media::post_with_sha256(db, &stripped.sha256)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use moekura_core::permissions::SystemRole;
    use moekura_core::posts::PostStatus;
    use sqlx::PgPool;

    use super::*;
    use crate::test_support::{fixture, session_for, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn imports_files_once(pool: PgPool) {
        let state = test_state(&pool).await;
        session_for(&pool, "alice", SystemRole::Member).await;
        let alice = moekura_db::users::by_name(&pool, "alice")
            .await
            .unwrap()
            .unwrap();
        let dir = state.work_dir.join("import-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pic.png");
        std::fs::write(&path, fixture::png(30, 20)).unwrap();

        let (tags, dropped) = usable_tags(
            &state,
            &["cat".into(), "artist:someone".into(), "bad*".into()],
        )
        .await
        .unwrap();
        assert_eq!(tags, "cat artist:someone");
        assert_eq!(dropped.len(), 1);
        let tags = vec![tags];
        let file = ImportFile {
            path: &path,
            rating: Rating::Questionable,
            tags: &tags,
            source: "https://example.com",
            description: "",
        };

        assert_eq!(existing_post(&state, &path).await.unwrap(), None);
        let Imported::Created(id) = import_file(&state, &alice, file.clone())
            .await
            .unwrap()
            .imported
        else {
            panic!("expected a new post");
        };
        let post = moekura_db::posts::by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(post.status, PostStatus::Active);
        assert_eq!(post.uploader_id, Some(alice.id));
        assert_eq!(post.rating, Rating::Questionable);
        assert_eq!(post.tag_ids.len(), 2);
        // The original is untouched; only the scratch copy is removed.
        assert!(path.exists());

        assert_eq!(
            import_file(&state, &alice, file).await.unwrap().imported,
            Imported::Duplicate(id)
        );
        assert_eq!(existing_post(&state, &path).await.unwrap(), Some(id));

        let text = dir.join("notes.png");
        std::fs::write(&text, b"not an image").unwrap();
        let error = import_file(
            &state,
            &alice,
            ImportFile {
                path: &text,
                rating: Rating::General,
                tags: &[],
                source: "",
                description: "",
            },
        )
        .await
        .unwrap_err();
        assert!(error.contains("isn't supported"), "{error}");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn files_stored_without_metadata_are_found_by_what_was_imported(pool: PgPool) {
        let state = test_state(&pool).await;
        session_for(&pool, "alice", SystemRole::Member).await;
        let alice = moekura_db::users::by_name(&pool, "alice")
            .await
            .unwrap()
            .unwrap();
        let dir = state.work_dir.join("import-stripped");
        std::fs::create_dir_all(&dir).unwrap();
        let (parent, child) = (dir.join("parent.png"), dir.join("child.png"));
        std::fs::write(&parent, fixture::png_with_text(30, 20, "secret")).unwrap();
        std::fs::write(&child, fixture::png(20, 30)).unwrap();
        let file = |path| ImportFile {
            path,
            rating: Rating::General,
            tags: &[],
            source: "",
            description: "",
        };

        let imported = import_file(&state, &alice, file(&parent)).await.unwrap();
        let Imported::Created(parent_id) = imported.imported else {
            panic!("expected a new post");
        };
        // Stored without the text, so the post has another hash than the
        // file...
        let by_file = moekura_db::media::post_with_sha256(&pool, &imported.file_sha256)
            .await
            .unwrap();
        assert_eq!(by_file, None);
        // ...which still finds it, as a dry run would.
        assert_eq!(
            existing_post(&state, &parent).await.unwrap(),
            Some(parent_id)
        );

        // A sidecar names the parent by the file it had.
        let child_id = import_file(&state, &alice, file(&child))
            .await
            .unwrap()
            .imported
            .post();
        let posts = HashMap::from([(imported.file_sha256, parent_id)]);
        assert_eq!(
            link_parent(&state, &alice, child_id, &imported.file_sha256, &posts)
                .await
                .unwrap(),
            Some(parent_id)
        );
        let child = moekura_db::posts::by_id(&pool, child_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(child.parent_id, Some(parent_id));
    }
}
