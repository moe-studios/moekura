//! Importing files from disk as posts, for `moekura admin import`. Each
//! file goes through the same checks and steps as an upload.

use std::path::Path;

use moekura_core::posts::Rating;
use moekura_core::tags::parse_input;
use moekura_db::users::User;
use tokio::io::AsyncReadExt;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::upload::{TempUpload, TempWriter, UploadError, UploadFields, ingest};

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
) -> Result<Imported, String> {
    let received = receive(state, file.path).await?;
    let current = CurrentUser::for_user(uploader.clone(), None, &state.site.get());
    let fields = UploadFields {
        url: String::new(),
        rating: Some(file.rating),
        tags: file.tags.join(" "),
        source: file.source.to_owned(),
        description: file.description.to_owned(),
    };
    match ingest(state, &current, &received, &fields).await {
        Ok(id) => Ok(Imported::Created(id)),
        Err(UploadError::Duplicate(id)) => Ok(Imported::Duplicate(id)),
        Err(error) => Err(error.to_string()),
    }
}

/// The post that already has the file at `path`, if any, without
/// importing it.
pub async fn existing_post(state: &AppState, path: &Path) -> Result<Option<i64>, String> {
    let received = receive(state, path).await?;
    moekura_db::media::post_with_sha256(state.db.primary(), &received.sha256)
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
        let Imported::Created(id) = import_file(&state, &alice, file.clone()).await.unwrap() else {
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
            import_file(&state, &alice, file).await.unwrap(),
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
}
