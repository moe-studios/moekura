//! The `ml.tag_post` job: running the model on a post's thumbnail and
//! saving what it suggests.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use moekura_core::jobs::TagPost;
use moekura_core::posts::PostStatus;
use moekura_core::tags::TagName;
use moekura_db::media::{self, Variant};
use moekura_db::tag_suggestions::{self, NewResult};
use moekura_jobs::{JobError, Registry};
use moekura_media::{Media, MediaType};
use moekura_storage::{Key, Storage};
use sqlx::PgPool;

use crate::model::Predict;

/// Tag category ids with thresholds of their own.
const CHARACTER: i16 = 4;
/// Suggestions below these confidences aren't kept.
const GENERAL_THRESHOLD: f32 = 0.35;
const CHARACTER_THRESHOLD: f32 = 0.85;

/// What tagging needs.
#[derive(Clone)]
pub struct TaggerJobs {
    pub db: PgPool,
    pub storage: Storage,
    pub media: Media,
    /// Scratch space; each job works in its own subdirectory.
    pub work_dir: PathBuf,
    pub model: Arc<dyn Predict>,
}

impl TaggerJobs {
    pub fn register(self, registry: &mut Registry) {
        registry.register(move |job: TagPost| {
            let jobs = self.clone();
            async move { jobs.tag(job.post_id).await }
        });
    }

    /// Runs the model on the post's image and saves its suggestions,
    /// replacing earlier ones. Safe to repeat.
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

        let work = ScratchDir::create(&self.work_dir, post_id).await?;
        let file = work.path().join(format!("source.{}", variant.format));
        self.storage
            .download(&key, &file)
            .await
            .map_err(|e| JobError::retry(format!("downloading {}: {e}", variant.kind)))?;
        let image = self
            .media
            .rgb_within(&file, variant_type, size, work.path())
            .await
            .map_err(|e| {
                if e.is_internal() {
                    JobError::retry(e)
                } else {
                    JobError::permanent(e)
                }
            })?;
        drop(work);

        let model = self.model.clone();
        let floor = GENERAL_THRESHOLD.min(CHARACTER_THRESHOLD);
        let prediction = tokio::task::spawn_blocking(move || model.predict(&image, floor))
            .await
            .map_err(|e| JobError::retry(format!("the model crashed: {e}")))?
            .map_err(JobError::retry)?;
        let Some((rating, rating_confidence)) = prediction.rating else {
            return Err(JobError::permanent("the model gave no rating"));
        };

        // The model's names, as this site writes them.
        let names: Vec<(TagName, i16, f32)> = prediction
            .tags
            .into_iter()
            .filter_map(|(name, category, score)| {
                TagName::parse(&name)
                    .ok()
                    .map(|name| (name, category, score))
            })
            .collect();
        let predicted: Vec<(&str, i16, f32)> = names
            .iter()
            .map(|(name, category, score)| (name.as_str(), *category, *score))
            .collect();
        let mut tx = self.db.begin().await?;
        let suggestions: Vec<(i32, f32)> = tag_suggestions::site_tags(&mut tx, &predicted)
            .await?
            .into_iter()
            .filter(|(tag, confidence)| *confidence >= threshold(tag.category_id))
            .map(|(tag, confidence)| (tag.id, confidence))
            .collect();
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
        tx.commit().await?;
        if saved {
            tracing::info!(
                post_id,
                suggestions = suggestions.len(),
                rating = rating.code(),
                "post tagged"
            );
        }
        Ok(())
    }
}

fn threshold(category_id: i16) -> f32 {
    if category_id == CHARACTER {
        CHARACTER_THRESHOLD
    } else {
        GENERAL_THRESHOLD
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
    async fn create(root: &Path, post_id: i64) -> Result<Self, JobError> {
        let path = root.join(format!("job-tag-{post_id}-{}", std::process::id()));
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
}
