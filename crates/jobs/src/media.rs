//! The `media.process` job: renditions for an uploaded file.

use std::path::{Path, PathBuf};

use moekura_core::jobs::{ExpireStagedUploads, ProcessMedia, PurgePost, TagPost};
use moekura_core::posts::PostStatus;
use moekura_db::media::{self, Asset, Variant};
use moekura_media::{Media, MediaError, MediaType};
use moekura_storage::{Key, Storage};
use sqlx::PgPool;

use crate::{JobError, Registry};

/// How long a staged upload waits to be made into a post.
pub const STAGED_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// How often unused staged uploads are looked for.
const STAGED_EVERY: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Removes deleted post `post_id`'s files, then the post; returns
/// whether it was purged. Safe to repeat: missing files and a missing
/// post are fine. A post restored in the meantime is left alone.
pub async fn purge_post(db: &PgPool, storage: &Storage, post_id: i64) -> Result<bool, JobError> {
    let Some(post) = moekura_db::posts::by_id(db, post_id).await? else {
        return Ok(false);
    };
    if post.status != PostStatus::Deleted {
        tracing::warn!(post_id, "purge skipped: the post is no longer deleted");
        return Ok(false);
    }
    if let Some(asset) = media::for_post(db, post_id).await? {
        let mut keys = vec![asset.storage_key.clone()];
        keys.extend(
            media::variants(db, asset.id)
                .await?
                .into_iter()
                .map(|v| v.storage_key),
        );
        for key in keys.iter().filter_map(|k| Key::parse(k)) {
            storage
                .delete(&key)
                .await
                .map_err(|e| JobError::retry(format!("deleting {key}: {e}")))?;
        }
    }
    moekura_db::posts::delete(db, post_id).await?;
    tracing::info!(post_id, "post purged");
    Ok(true)
}

/// What media jobs need.
#[derive(Clone)]
pub struct MediaJobs {
    pub db: PgPool,
    pub storage: Storage,
    pub media: Media,
    /// Scratch space; each job works in its own subdirectory.
    pub work_dir: PathBuf,
    /// Queue processed posts for the tagger (`tagger.enabled`).
    pub tag_posts: bool,
}

impl MediaJobs {
    pub fn register(self, registry: &mut Registry) {
        let purger = self.clone();
        registry.register(move |job: ProcessMedia| {
            let jobs = self.clone();
            async move { jobs.process(job.asset_id).await }
        });
        let expirer = purger.clone();
        registry.register(move |job: PurgePost| {
            let jobs = purger.clone();
            async move { jobs.purge(job.post_id).await }
        });
        registry
            .register(move |_: ExpireStagedUploads| {
                let jobs = expirer.clone();
                async move { jobs.expire_staged().await.map(|_| ()) }
            })
            .every::<ExpireStagedUploads>(STAGED_EVERY);
    }

    /// Removes staged uploads nobody made into a post within
    /// [`STAGED_MAX_AGE`], with their stored files when nothing else
    /// uses them. Returns how many files were removed.
    pub async fn expire_staged(&self) -> Result<usize, JobError> {
        let keys = moekura_db::staged_uploads::expire(&self.db, STAGED_MAX_AGE).await?;
        for key in &keys {
            if let Some(key) = Key::parse(key) {
                self.storage
                    .delete(&key)
                    .await
                    .map_err(|e| JobError::retry(format!("deleting {}: {e}", key.as_str())))?;
            }
        }
        if !keys.is_empty() {
            tracing::info!(files = keys.len(), "removed unused staged uploads");
        }
        Ok(keys.len())
    }

    /// Removes a deleted post's files, then the post: see [`purge_post`].
    pub async fn purge(&self, post_id: i64) -> Result<(), JobError> {
        purge_post(&self.db, &self.storage, post_id)
            .await
            .map(|_| ())
    }

    /// Generates thumbnails at every configured size, plus a `sample` for
    /// large still images or a `poster` frame for videos, then marks the
    /// asset processed and, with the tagger on, queues the post for it.
    /// Safe to repeat: every write replaces the last.
    pub async fn process(&self, asset_id: i64) -> Result<(), JobError> {
        let Some(asset) = media::by_id(&self.db, asset_id).await? else {
            // The post was deleted in the meantime.
            return Ok(());
        };
        let media_type: MediaType = asset.media_type.parse().map_err(|()| {
            JobError::permanent(format!("unknown media type `{}`", asset.media_type))
        })?;
        let original_key = Key::parse(&asset.storage_key).ok_or_else(|| {
            JobError::permanent(format!("malformed storage key `{}`", asset.storage_key))
        })?;

        let work = ScratchDir::create(&self.work_dir, asset_id).await?;
        let original = work
            .path()
            .join(format!("original.{}", original_key.extension()));
        self.storage
            .download(&original_key, &original)
            .await
            .map_err(|e| JobError::retry(format!("downloading original: {e}")))?;

        // Metadata comes from the original; failing to read it (a tool
        // choking on odd fields) doesn't stop the renditions.
        match self.media.metadata(&original, media_type).await {
            Ok(metadata) => media::set_metadata(&self.db, asset_id, &metadata).await?,
            Err(error) if error.is_internal() => return Err(media_error(error)),
            Err(error) => tracing::warn!(asset_id, %error, "could not read the file's metadata"),
        }

        // Videos are rendered from a still frame, ugoira from their first
        // frame, and ugoira also get a video to play.
        let (source, source_type) = if media_type == MediaType::Ugoira {
            let (video, first, first_type) = self
                .media
                .ugoira_video(&original, work.path())
                .await
                .map_err(media_error)?;
            self.store_video(&asset, &video).await?;
            (first, first_type)
        } else if media_type.is_video() {
            let duration = asset.duration_ms.and_then(|d| u32::try_from(d).ok());
            let poster = self
                .media
                .video_poster(&original, duration, work.path())
                .await
                .map_err(media_error)?;
            (poster, MediaType::Png)
        } else {
            (original, media_type)
        };

        let config = self.media.config();
        let mut wanted: Vec<(String, u32)> = config
            .thumbnail_sizes
            .iter()
            .map(|&size| (format!("thumb-{size}"), size))
            .collect();
        let longest = asset.width.max(asset.height);
        if media_type.is_video() || media_type == MediaType::Ugoira {
            wanted.push(("poster".to_owned(), config.sample_size));
        } else if asset.frames == 1
            && longest > i32::try_from(config.sample_size).unwrap_or(i32::MAX)
        {
            // Animations keep playing the original instead.
            wanted.push(("sample".to_owned(), config.sample_size));
        }

        for (kind, size) in wanted {
            self.render_variant(&asset, &source, source_type, &kind, size, work.path())
                .await?;
        }
        let phash = self
            .media
            .perceptual_hash(&source, source_type, work.path())
            .await
            .map_err(media_error)?;
        let mut tx = self.db.begin().await?;
        media::mark_processed(&mut *tx, asset_id, Some(phash)).await?;
        if self.tag_posts {
            moekura_db::jobs::enqueue(
                &mut tx,
                &TagPost {
                    post_id: asset.post_id,
                },
            )
            .await?;
        }
        tx.commit().await?;
        tracing::info!(asset_id, post_id = asset.post_id, "media processed");
        Ok(())
    }

    /// Stores an ugoira's video as its `video` variant (WebM, full size).
    async fn store_video(&self, asset: &Asset, video: &Path) -> Result<(), JobError> {
        let key = Key::variant("video", &asset.sha256_hex(), "webm");
        self.storage
            .put_file(&key, video)
            .await
            .map_err(|e| JobError::retry(format!("storing the video: {e}")))?;
        let size = tokio::fs::metadata(video)
            .await
            .map_err(|e| JobError::retry(format!("reading the video: {e}")))?
            .len();
        let variant = Variant {
            asset_id: asset.id,
            kind: "video".to_owned(),
            format: "webm".to_owned(),
            width: asset.width,
            height: asset.height,
            file_size: i64::try_from(size).unwrap_or(i64::MAX),
            storage_key: key.as_str().to_owned(),
        };
        media::save_variant(&self.db, &variant).await?;
        Ok(())
    }

    async fn render_variant(
        &self,
        asset: &Asset,
        source: &Path,
        source_type: MediaType,
        kind: &str,
        size: u32,
        dir: &Path,
    ) -> Result<(), JobError> {
        let format = self.media.variant_format();
        let out = dir.join(format!("{kind}.{format}"));
        let rendition = self
            .media
            .fit_within(source, source_type, size, &out)
            .await
            .map_err(media_error)?;
        let key = Key::variant(kind, &asset.sha256_hex(), format);
        self.storage
            .put_file(&key, &rendition.path)
            .await
            .map_err(|e| JobError::retry(format!("storing {kind}: {e}")))?;
        let variant = Variant {
            asset_id: asset.id,
            kind: kind.to_owned(),
            format: format.to_owned(),
            width: i32::try_from(rendition.width).unwrap_or(i32::MAX),
            height: i32::try_from(rendition.height).unwrap_or(i32::MAX),
            file_size: i64::try_from(rendition.size).unwrap_or(i64::MAX),
            storage_key: key.as_str().to_owned(),
        };
        media::save_variant(&self.db, &variant).await?;
        Ok(())
    }
}

/// A damaged file won't get better; missing tools or timeouts might.
fn media_error(error: MediaError) -> JobError {
    if error.is_internal() {
        JobError::retry(error)
    } else {
        JobError::permanent(error)
    }
}

/// A per-job working directory, removed when dropped.
struct ScratchDir(PathBuf);

impl ScratchDir {
    async fn create(root: &Path, asset_id: i64) -> Result<Self, JobError> {
        let path = root.join(format!("job-media-{asset_id}-{}", std::process::id()));
        // A crashed earlier attempt may have left files behind.
        let _ = tokio::fs::remove_dir_all(&path).await;
        tokio::fs::create_dir_all(&path)
            .await
            .map_err(|e| JobError::retry(format!("creating work directory: {e}")))?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use moekura_core::config::MediaConfig;
    use moekura_core::posts::{PostStatus, Rating};
    use moekura_db::media::NewAsset;
    use moekura_db::posts::{self, NewPost};

    use super::*;

    const HASH: &str = "cafe0123456789abcdef0123456789abcdef0123456789abcdef0123456789ab";

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("moekura-jobs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ffmpeg(out: &Path, args: &[&str]) {
        let status = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(args)
            .arg(out)
            .status()
            .expect("ffmpeg is needed for media job tests");
        assert!(status.success());
    }

    /// Stores `file` as a post's original and returns the job context and
    /// asset id.
    async fn stored_asset(
        pool: &PgPool,
        dir: &Path,
        file: &Path,
        media_type: &str,
        dims: (i32, i32),
    ) -> (MediaJobs, i64) {
        let jobs = MediaJobs {
            db: pool.clone(),
            storage: Storage::local(dir.join("storage")).unwrap(),
            media: Media::new(MediaConfig::default()),
            work_dir: dir.join("work"),
            tag_posts: false,
        };
        let extension = file.extension().unwrap().to_str().unwrap();
        let key = Key::original(HASH, extension);
        jobs.storage.put_file(&key, file).await.unwrap();
        let post_id = posts::insert(
            pool,
            NewPost {
                uploader_id: None,
                rating: Rating::General,
                status: PostStatus::Active,
                source: "",
                description: "",
                tag_ids: &[],
            },
        )
        .await
        .unwrap();
        let sha256: [u8; 32] = hex::decode(HASH).unwrap().try_into().unwrap();
        let asset = NewAsset {
            post_id,
            sha256: &sha256,
            md5: &[0; 16],
            media_type,
            width: dims.0,
            height: dims.1,
            duration_ms: (media_type == "webm").then_some(2000),
            frames: 1,
            has_audio: false,
            file_size: 1,
            storage_key: key.as_str(),
        };
        let asset_id = media::insert(pool, asset).await.unwrap();
        (jobs, asset_id)
    }

    fn summary(variants: &[Variant]) -> Vec<(String, i32, i32)> {
        variants
            .iter()
            .map(|v| (v.kind.clone(), v.width, v.height))
            .collect()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn large_images_get_thumbnails_and_a_sample(pool: PgPool) {
        let dir = scratch("image");
        let png = dir.join("big.png");
        ffmpeg(
            &png,
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=2000x1500:duration=1",
                "-frames:v",
                "1",
            ],
        );
        let (jobs, asset_id) = stored_asset(&pool, &dir, &png, "png", (2000, 1500)).await;

        jobs.process(asset_id).await.unwrap();

        let variants = media::variants(&pool, asset_id).await.unwrap();
        assert_eq!(
            summary(&variants),
            [
                ("sample".to_owned(), 1600, 1200),
                ("thumb-250".to_owned(), 250, 188),
                ("thumb-500".to_owned(), 500, 375),
            ]
        );
        for variant in &variants {
            let key = Key::parse(&variant.storage_key).unwrap();
            assert!(jobs.storage.exists(&key).await.unwrap(), "{key}");
            assert_eq!(variant.format, "webp");
        }
        let processed = media::by_id(&pool, asset_id).await.unwrap().unwrap();
        assert!(processed.processed_at.is_some());
        assert!(processed.phash.is_some());
        let metadata = media::metadata_for_post(&pool, processed.post_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            metadata.get("File:ColorComponents").map(String::as_str),
            Some("3")
        );
        // Scratch space is cleaned up.
        assert_eq!(std::fs::read_dir(dir.join("work")).unwrap().count(), 0);

        // Running again is harmless.
        jobs.process(asset_id).await.unwrap();
        assert_eq!(media::variants(&pool, asset_id).await.unwrap().len(), 3);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn ugoira_get_a_video(pool: PgPool) {
        use std::io::Write;

        let dir = scratch("ugoira");
        let zip_path = dir.join("ugoira.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for n in 0..3 {
            let frame = dir.join(format!("{n}.png"));
            ffmpeg(
                &frame,
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=64x48:duration=1",
                    "-frames:v",
                    "1",
                ],
            );
            writer.start_file(format!("{n:06}.png"), options).unwrap();
            writer.write_all(&std::fs::read(&frame).unwrap()).unwrap();
        }
        writer.finish().unwrap();
        let (jobs, asset_id) = stored_asset(&pool, &dir, &zip_path, "ugoira", (64, 48)).await;

        jobs.process(asset_id).await.unwrap();

        let variants = media::variants(&pool, asset_id).await.unwrap();
        let kinds: Vec<(&str, &str)> = variants
            .iter()
            .map(|v| (v.kind.as_str(), v.format.as_str()))
            .collect();
        assert!(kinds.contains(&("video", "webm")), "{kinds:?}");
        assert!(kinds.contains(&("poster", "webp")), "{kinds:?}");
        assert!(kinds.contains(&("thumb-250", "webp")), "{kinds:?}");
        let metadata = media::metadata_for_post(
            &pool,
            media::by_id(&pool, asset_id)
                .await
                .unwrap()
                .unwrap()
                .post_id,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            metadata.get("Ugoira:FrameCount").map(String::as_str),
            Some("3")
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn processed_posts_are_queued_for_the_tagger(pool: PgPool) {
        let dir = scratch("tagger");
        let png = dir.join("p.png");
        ffmpeg(
            &png,
            &["-f", "lavfi", "-i", "testsrc2=size=64x48", "-frames:v", "1"],
        );
        let (mut jobs, asset_id) = stored_asset(&pool, &dir, &png, "png", (64, 48)).await;
        let queued = async || -> Vec<serde_json::Value> {
            sqlx::query_scalar("SELECT payload FROM jobs WHERE kind = 'ml.tag_post'")
                .fetch_all(&pool)
                .await
                .unwrap()
        };
        jobs.process(asset_id).await.unwrap();
        assert!(queued().await.is_empty());

        jobs.tag_posts = true;
        jobs.process(asset_id).await.unwrap();
        let post_id = media::by_id(&pool, asset_id)
            .await
            .unwrap()
            .unwrap()
            .post_id;
        assert_eq!(queued().await, [serde_json::json!({ "post_id": post_id })]);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn videos_get_thumbnails_and_a_poster(pool: PgPool) {
        let dir = scratch("video");
        let webm = dir.join("clip.webm");
        ffmpeg(
            &webm,
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x240:rate=10:duration=2",
                "-c:v",
                "libvpx-vp9",
            ],
        );
        let (jobs, asset_id) = stored_asset(&pool, &dir, &webm, "webm", (320, 240)).await;

        jobs.process(asset_id).await.unwrap();

        let variants = media::variants(&pool, asset_id).await.unwrap();
        assert_eq!(
            summary(&variants),
            [
                ("poster".to_owned(), 320, 240),
                ("thumb-250".to_owned(), 250, 188),
                ("thumb-500".to_owned(), 320, 240),
            ]
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn purging_removes_files_then_the_post(pool: PgPool) {
        let dir = scratch("purge");
        let png = dir.join("p.png");
        ffmpeg(
            &png,
            &["-f", "lavfi", "-i", "testsrc2=size=64x48", "-frames:v", "1"],
        );
        let (jobs, asset_id) = stored_asset(&pool, &dir, &png, "png", (64, 48)).await;
        jobs.process(asset_id).await.unwrap();
        let asset = media::by_id(&pool, asset_id).await.unwrap().unwrap();
        let mut keys = vec![Key::parse(&asset.storage_key).unwrap()];
        for v in media::variants(&pool, asset_id).await.unwrap() {
            keys.push(Key::parse(&v.storage_key).unwrap());
        }

        // Not deleted: left alone.
        jobs.purge(asset.post_id).await.unwrap();
        assert!(jobs.storage.exists(&keys[0]).await.unwrap());

        sqlx::query("UPDATE posts SET status = 'deleted' WHERE id = $1")
            .bind(asset.post_id)
            .execute(&pool)
            .await
            .unwrap();
        jobs.purge(asset.post_id).await.unwrap();
        for key in &keys {
            assert!(!jobs.storage.exists(key).await.unwrap(), "{key}");
        }
        assert!(
            moekura_db::posts::by_id(&pool, asset.post_id)
                .await
                .unwrap()
                .is_none()
        );
        // Again: nothing left to do.
        jobs.purge(asset.post_id).await.unwrap();
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn failures_are_classified(pool: PgPool) {
        let dir = scratch("failures");
        let broken = dir.join("broken.png");
        std::fs::write(&broken, b"\x89PNG\r\n\x1a\nnot really").unwrap();
        let (jobs, asset_id) = stored_asset(&pool, &dir, &broken, "png", (10, 10)).await;
        assert!(matches!(
            jobs.process(asset_id).await,
            Err(JobError::Permanent(_))
        ));

        // A missing original may reappear (storage outage): retry.
        let key = Key::original(HASH, "png");
        jobs.storage.delete(&key).await.unwrap();
        assert!(matches!(
            jobs.process(asset_id).await,
            Err(JobError::Retry(_))
        ));

        // A deleted post is simply done.
        assert!(jobs.process(asset_id + 1000).await.is_ok());
    }
}
