//! The `ml.tag_post` job: running the model on a post's thumbnail and
//! saving what it suggests; and `ml.tag_staged`, the same for a file
//! waiting to be posted, for its post form.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use moekura_core::jobs::{TagPost, TagStaged};
use moekura_core::posts::{PostStatus, Rating};
use moekura_core::tagger::TaggerSettings;
use moekura_core::tags::TagName;
use moekura_db::media::{self, Variant};
use moekura_db::staged_uploads::{self, Status};
use moekura_db::tag_suggestions::{self, AccountError, NewResult};
use moekura_jobs::{JobError, Registry};
use moekura_media::{Media, MediaType, RgbImage};
use moekura_storage::{Key, Storage};
use sqlx::{PgConnection, PgPool};

use crate::model::Predict;

/// What tagging needs.
#[derive(Clone)]
pub struct TaggerJobs {
    pub db: PgPool,
    pub storage: Storage,
    pub media: Media,
    /// Scratch space; each job works in its own subdirectory.
    pub work_dir: PathBuf,
    pub model: Arc<dyn Predict>,
    /// Who tags applied automatically are credited to (`tagger.account`).
    pub account: String,
}

impl TaggerJobs {
    pub fn register(self, registry: &mut Registry) {
        let jobs = self.clone();
        registry.register(move |job: TagPost| {
            let jobs = jobs.clone();
            async move { jobs.tag(job.post_id).await }
        });
        registry.register(move |job: TagStaged| {
            let jobs = self.clone();
            async move { jobs.tag_staged(job.staged_id).await }
        });
    }

    /// Runs the model on the post's image and saves its suggestions,
    /// replacing earlier ones, then applies the most confident ones if the
    /// site settings say so. Safe to repeat.
    pub async fn tag(&self, post_id: i64) -> Result<(), JobError> {
        let Some(post) = moekura_db::posts::by_id(&self.db, post_id).await? else {
            return Ok(());
        };
        if post.status == PostStatus::Deleted {
            return Ok(());
        }
        let Some(asset) = media::for_post(&self.db, post_id).await? else {
            return Ok(());
        };
        if asset.processed_at.is_none() {
            return Err(JobError::retry("the post's thumbnails aren't ready yet"));
        }
        let size = self.model.input_size();
        let variants = media::variants(&self.db, asset.id).await?;
        let variant = pick_variant(&variants, size)
            .ok_or_else(|| JobError::permanent("the post has no thumbnails"))?;
        let key = Key::parse(&variant.storage_key).ok_or_else(|| {
            JobError::permanent(format!("malformed storage key `{}`", variant.storage_key))
        })?;
        let variant_type: MediaType = variant
            .format
            .parse()
            .map_err(|()| JobError::permanent(format!("unknown format `{}`", variant.format)))?;

        let work = ScratchDir::create(&self.work_dir, &format!("tag-{post_id}")).await?;
        let file = work.path().join(format!("source.{}", variant.format));
        self.storage
            .download(&key, &file)
            .await
            .map_err(|e| JobError::retry(format!("downloading {}: {e}", variant.kind)))?;
        let image = self.scale(&file, variant_type, work.path()).await?;
        drop(work);

        let settings = moekura_db::settings::load(&self.db).await?.tagger;
        let tagger = if settings.auto_apply {
            let account = tag_suggestions::tagger_account(&self.db, &self.account)
                .await
                .map_err(|e| match e {
                    AccountError::HasPassword(_) => JobError::permanent(e),
                    AccountError::Db(e) => e.into(),
                })?;
            Some(account)
        } else {
            None
        };
        let (rating, rating_confidence, names) = self.predict(image, &settings).await?;
        let mut tx = self.db.begin().await?;
        let suggestions = site_suggestions(&mut tx, &names, &settings).await?;

        let saved = tag_suggestions::save(
            &mut tx,
            &NewResult {
                post_id,
                model: self.model.model_name(),
                rating,
                rating_confidence,
                suggestions: &suggestions,
            },
        )
        .await?;
        let mut applied = false;
        if let Some(tagger) = tagger.filter(|_| saved) {
            let sure = settings.auto_threshold();
            let tags: Vec<i32> = suggestions
                .iter()
                .filter(|(_, confidence)| *confidence >= sure)
                .map(|(id, _)| *id)
                .collect();
            let rating = (settings.auto_rating && rating_confidence >= sure).then_some(rating);
            if !tags.is_empty() || rating.is_some() {
                applied = tag_suggestions::apply(&mut tx, post_id, tagger, &tags, rating).await?;
            }
        }
        tx.commit().await?;
        if saved {
            tracing::info!(
                post_id,
                suggestions = suggestions.len(),
                rating = rating.code(),
                applied,
                "post tagged"
            );
        }
        Ok(())
    }

    /// Runs the model on staged upload `staged_id`'s file and saves its
    /// suggestions for the post form, unless the file was posted (the
    /// post gets its own) or is gone. Safe to repeat.
    pub async fn tag_staged(&self, staged_id: i64) -> Result<(), JobError> {
        let Some(staged) = staged_uploads::by_id(&self.db, staged_id).await? else {
            return Ok(());
        };
        if staged.status != Status::Ready || staged.post_id.is_some() {
            return Ok(());
        }
        let (Some(stored), Some(media_type)) = (&staged.storage_key, &staged.media_type) else {
            return Ok(());
        };
        let key = Key::parse(stored)
            .ok_or_else(|| JobError::permanent(format!("malformed storage key `{stored}`")))?;
        let media_type: MediaType = media_type
            .parse()
            .map_err(|()| JobError::permanent(format!("unknown media type `{media_type}`")))?;

        let work = ScratchDir::create(&self.work_dir, &format!("tag-staged-{staged_id}")).await?;
        let file = work.path().join(format!("original.{}", key.extension()));
        self.storage
            .download(&key, &file)
            .await
            .map_err(|e| JobError::retry(format!("downloading the file: {e}")))?;
        let duration = staged.duration_ms.and_then(|d| u32::try_from(d).ok());
        let (still, still_type) = self
            .media
            .still(&file, media_type, duration, work.path())
            .await
            .map_err(media_error)?;
        let image = self.scale(&still, still_type, work.path()).await?;
        drop(work);

        let settings = moekura_db::settings::load(&self.db).await?.tagger;
        let (rating, rating_confidence, names) = self.predict(image, &settings).await?;
        let mut tx = self.db.begin().await?;
        let suggestions = site_suggestions(&mut tx, &names, &settings).await?;
        let saved = tag_suggestions::save_staged(
            &mut *tx,
            staged_id,
            self.model.model_name(),
            rating,
            rating_confidence,
            &suggestions,
        )
        .await?;
        tx.commit().await?;
        if saved {
            tracing::info!(
                staged_id,
                suggestions = suggestions.len(),
                rating = rating.code(),
                "staged upload tagged"
            );
        }
        Ok(())
    }

    /// `file` scaled for the model.
    async fn scale(
        &self,
        file: &Path,
        media_type: MediaType,
        work: &Path,
    ) -> Result<RgbImage, JobError> {
        self.media
            .rgb_within(file, media_type, self.model.input_size(), work)
            .await
            .map_err(media_error)
    }

    /// The model's rating of `image`, with its confidence, and the tags it
    /// finds at least as likely as the lowest threshold, named as this
    /// site writes them, with their category and confidence.
    async fn predict(
        &self,
        image: RgbImage,
        settings: &TaggerSettings,
    ) -> Result<(Rating, f32, Vec<(TagName, i16, f32)>), JobError> {
        let model = self.model.clone();
        let floor = settings.lowest_threshold();
        let prediction = tokio::task::spawn_blocking(move || model.predict(&image, floor))
            .await
            .map_err(|e| JobError::retry(format!("the model crashed: {e}")))?
            .map_err(JobError::retry)?;
        let Some((rating, rating_confidence)) = prediction.rating else {
            return Err(JobError::permanent("the model gave no rating"));
        };
        let names = prediction
            .tags
            .into_iter()
            .filter_map(|(name, category, score)| {
                TagName::parse(&name)
                    .ok()
                    .map(|name| (name, category, score))
            })
            .collect();
        Ok((rating, rating_confidence, names))
    }
}

/// The site's tags for what the model found, created if missing, with
/// their confidence, keeping those over their category's threshold.
async fn site_suggestions(
    conn: &mut PgConnection,
    names: &[(TagName, i16, f32)],
    settings: &TaggerSettings,
) -> Result<Vec<(i32, f32)>, JobError> {
    let categories = moekura_db::tags::categories(&mut *conn).await?;
    let threshold = |category_id: i16| {
        let name = categories
            .iter()
            .find(|c| c.id == category_id)
            .map_or("general", |c| c.name.as_str());
        settings.threshold(name)
    };
    let predicted: Vec<(&str, i16, f32)> = names
        .iter()
        .map(|(name, category, score)| (name.as_str(), *category, *score))
        .collect();
    Ok(tag_suggestions::site_tags(conn, &predicted)
        .await?
        .into_iter()
        .filter(|(tag, confidence)| *confidence >= threshold(tag.category_id))
        .map(|(tag, confidence)| (tag.id, confidence))
        .collect())
}

/// Media errors as job errors: ours are worth retrying, the file's aren't.
fn media_error(error: moekura_media::MediaError) -> JobError {
    if error.is_internal() {
        JobError::retry(error)
    } else {
        JobError::permanent(error)
    }
}

/// The smallest rendition at least `size` on its longest side, or else
/// the largest one: less to download and scale than the original, which
/// may be a video.
fn pick_variant(variants: &[Variant], size: u32) -> Option<&Variant> {
    let longest = |v: &Variant| u32::try_from(v.width.max(v.height)).unwrap_or(0);
    variants
        .iter()
        .filter(|v| longest(v) >= size)
        .min_by_key(|v| longest(v))
        .or_else(|| variants.iter().max_by_key(|v| longest(v)))
}

/// A per-job working directory, removed when dropped.
struct ScratchDir(PathBuf);

impl ScratchDir {
    async fn create(root: &Path, name: &str) -> Result<Self, JobError> {
        let path = root.join(format!("job-{name}-{}", std::process::id()));
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
pub(crate) mod tests {
    use moekura_core::posts::Rating;
    use moekura_core::tagger::Prediction;

    use super::*;

    fn variant(kind: &str, width: i32, height: i32) -> Variant {
        Variant {
            asset_id: 1,
            kind: kind.into(),
            format: "webp".into(),
            width,
            height,
            file_size: 1,
            storage_key: String::new(),
        }
    }

    #[test]
    fn picks_the_smallest_big_enough_rendition() {
        let variants = [
            variant("sample", 1600, 1200),
            variant("thumb-250", 250, 188),
            variant("thumb-500", 500, 375),
        ];
        assert_eq!(pick_variant(&variants, 448).unwrap().kind, "thumb-500");
        assert_eq!(pick_variant(&variants, 600).unwrap().kind, "sample");
        assert_eq!(pick_variant(&variants, 2000).unwrap().kind, "sample");
        assert!(pick_variant(&[], 448).is_none());
    }

    /// Suggests fixed tags for any image, and says what size it was given.
    pub(crate) struct Fixed {
        pub seen: std::sync::Mutex<Vec<(u32, u32)>>,
    }

    impl Predict for Fixed {
        fn model_name(&self) -> &str {
            "fixed"
        }

        fn input_size(&self) -> u32 {
            64
        }

        fn predict(
            &self,
            image: &moekura_media::RgbImage,
            floor: f32,
        ) -> Result<Prediction, crate::TaggerError> {
            self.seen.lock().unwrap().push((image.width, image.height));
            let tags = [
                ("1girl", 0, 0.95),
                ("Solo", 0, 0.5),
                ("hatsune_miku", 4, 0.9),
                ("kagamine_rin", 4, 0.6),
                ("-_-", 0, 0.9),
                ("faint", 0, 0.2),
            ];
            Ok(Prediction {
                rating: Some((Rating::Sensitive, 0.8)),
                tags: tags
                    .into_iter()
                    .filter(|t| t.2 >= floor)
                    .map(|(name, category, score)| (name.to_owned(), category, score))
                    .collect(),
            })
        }
    }

    /// A post whose only rendition is a 100×50 red PNG, and jobs tagging
    /// with `model`.
    pub(crate) async fn processed_post(
        pool: &PgPool,
        dir: &Path,
        model: Arc<dyn Predict>,
    ) -> (TaggerJobs, i64) {
        use moekura_core::config::MediaConfig;
        use moekura_db::media::NewAsset;
        use moekura_db::posts::{self, NewPost};

        let storage = Storage::local(dir.join("storage")).unwrap();
        let png = dir.join("thumb.png");
        let status = std::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg("color=c=red:size=100x50")
            .args(["-frames:v", "1"])
            .arg(&png)
            .status()
            .expect("ffmpeg is needed for tagger tests");
        assert!(status.success());
        let hash = "ab".repeat(32);
        let original = Key::original(&hash, "png");
        storage.put_file(&original, &png).await.unwrap();
        let thumb = Key::variant("thumb-250", &hash, "png");
        storage.put_file(&thumb, &png).await.unwrap();

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
        let sha256 = [0xab; 32];
        let asset_id = media::insert(
            pool,
            NewAsset {
                post_id,
                sha256: &sha256,
                md5: &[0; 16],
                media_type: "png",
                width: 100,
                height: 50,
                duration_ms: None,
                frames: 1,
                has_audio: false,
                file_size: 1,
                storage_key: original.as_str(),
            },
        )
        .await
        .unwrap();
        media::save_variant(
            pool,
            &Variant {
                asset_id,
                kind: "thumb-250".into(),
                format: "png".into(),
                width: 100,
                height: 50,
                file_size: 1,
                storage_key: thumb.as_str().to_owned(),
            },
        )
        .await
        .unwrap();
        media::mark_processed(pool, asset_id, None).await.unwrap();
        let jobs = TaggerJobs {
            db: pool.clone(),
            storage,
            media: Media::new(MediaConfig::default()),
            work_dir: dir.join("work"),
            model,
            account: "tagger".into(),
        };
        (jobs, post_id)
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("moekura-tagger-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn saves_suggestions_above_their_categorys_threshold(pool: PgPool) {
        let dir = scratch("job");
        let fixed = Arc::new(Fixed {
            seen: Default::default(),
        });
        let (jobs, post_id) = processed_post(&pool, &dir, fixed.clone()).await;
        jobs.tag(post_id).await.unwrap();

        let suggestions: Vec<(String, i16)> = tag_suggestions::for_post(&pool, post_id)
            .await
            .unwrap()
            .into_iter()
            .map(|s| (s.name, s.category_id))
            .collect();
        // kagamine_rin is below the character threshold, faint below the
        // floor, and -_- isn't a valid tag name here.
        assert_eq!(
            suggestions,
            [
                ("1girl".to_owned(), 0),
                ("hatsune_miku".to_owned(), 4),
                ("solo".to_owned(), 0)
            ]
        );
        let result = tag_suggestions::result(&pool, post_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (result.model.as_str(), result.rating),
            ("fixed", Rating::Sensitive)
        );
        // Scaled to fit the model's input.
        assert_eq!(*fixed.seen.lock().unwrap(), [(64, 32)]);
        assert_eq!(std::fs::read_dir(dir.join("work")).unwrap().count(), 0);

        // Deleted posts are skipped; missing ones are done.
        sqlx::query("UPDATE posts SET status = 'deleted' WHERE id = $1")
            .bind(post_id)
            .execute(&pool)
            .await
            .unwrap();
        jobs.tag(post_id).await.unwrap();
        jobs.tag(post_id + 100).await.unwrap();
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn tags_staged_files_until_posted(pool: PgPool) {
        let dir = scratch("staged");
        let fixed = Arc::new(Fixed {
            seen: Default::default(),
        });
        // The post's original, a 100×50 PNG, staged again.
        let (jobs, post_id) = processed_post(&pool, &dir, fixed.clone()).await;
        let asset = media::for_post(&pool, post_id).await.unwrap().unwrap();
        let uploader: i64 = sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT 'alice', id FROM roles LIMIT 1 RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let upload = staged_uploads::create_upload(&pool, uploader, "", "")
            .await
            .unwrap();
        let slot = staged_uploads::Slot {
            upload_id: upload,
            uploader_id: uploader,
            position: 0,
            file_name: "a.png",
            source: "",
        };
        let staged = staged_uploads::create(
            &pool,
            slot,
            staged_uploads::StoredFile {
                sha256: &[1; 32],
                md5: &[1; 16],
                media_type: "png",
                width: 100,
                height: 50,
                duration_ms: None,
                frames: 1,
                has_audio: false,
                file_size: 1,
                storage_key: &asset.storage_key,
                phash: None,
                pixel_hash: None,
                traits: &[],
            },
        )
        .await
        .unwrap();
        jobs.tag_staged(staged).await.unwrap();

        let result = tag_suggestions::staged_result(&pool, staged)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.rating, Rating::Sensitive);
        let names: Vec<&str> = result.suggestions.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["1girl", "hatsune_miku", "solo"]);
        assert_eq!(*fixed.seen.lock().unwrap(), [(64, 32)]);

        // Once posted, the post is tagged instead.
        sqlx::query("DELETE FROM staged_tagger_results")
            .execute(&pool)
            .await
            .unwrap();
        staged_uploads::used(&pool, staged, post_id).await.unwrap();
        jobs.tag_staged(staged).await.unwrap();
        assert!(
            tag_suggestions::staged_result(&pool, staged)
                .await
                .unwrap()
                .is_none()
        );
        jobs.tag_staged(staged + 100).await.unwrap();
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn waits_for_thumbnails(pool: PgPool) {
        let dir = scratch("unprocessed");
        let fixed = Arc::new(Fixed {
            seen: Default::default(),
        });
        let (jobs, post_id) = processed_post(&pool, &dir, fixed).await;
        sqlx::query("UPDATE media_assets SET processed_at = NULL")
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(jobs.tag(post_id).await, Err(JobError::Retry(_))));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn follows_the_site_settings(pool: PgPool) {
        let dir = scratch("settings");
        let fixed = Arc::new(Fixed {
            seen: Default::default(),
        });
        let (jobs, post_id) = processed_post(&pool, &dir, fixed).await;
        moekura_db::settings::set(
            &pool,
            "tagger",
            serde_json::json!({
                "thresholds": { "general": 60, "character": 50 },
                "auto_apply": true,
                "auto_threshold": 90,
                "auto_rating": true,
            }),
        )
        .await
        .unwrap();
        jobs.tag(post_id).await.unwrap();

        let suggestions: Vec<String> = tag_suggestions::for_post(&pool, post_id)
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(suggestions, ["1girl", "hatsune_miku", "kagamine_rin"]);
        // At least 90% sure: applied by the tagger. The rating, 80% sure,
        // stays.
        let post = moekura_db::posts::by_id(&pool, post_id)
            .await
            .unwrap()
            .unwrap();
        let names: Vec<String> = moekura_db::tags::by_ids(&pool, &post.tag_ids)
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"1girl".to_owned()) && names.contains(&"hatsune_miku".to_owned()));
        assert_eq!(post.rating, Rating::General);
        let versions = moekura_db::post_versions::list(&pool, post_id)
            .await
            .unwrap();
        assert_eq!(versions[0].updater_name.as_deref(), Some("tagger"));

        // An account someone can log in to isn't used.
        sqlx::query("UPDATE users SET password_hash = 'x' WHERE name = 'tagger'")
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            jobs.tag(post_id).await,
            Err(JobError::Permanent(_))
        ));
    }
}
