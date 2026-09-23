//! Queries on `media_assets` and `media_variants`.

use sqlx::PgExecutor;
use time::OffsetDateTime;

pub struct NewAsset<'a> {
    pub post_id: i64,
    pub sha256: &'a [u8; 32],
    pub md5: &'a [u8; 16],
    pub media_type: &'a str,
    pub width: i32,
    pub height: i32,
    pub duration_ms: Option<i32>,
    pub frames: i32,
    pub has_audio: bool,
    pub file_size: i64,
    pub storage_key: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Asset {
    pub id: i64,
    pub post_id: i64,
    pub sha256: Vec<u8>,
    pub md5: Vec<u8>,
    pub media_type: String,
    pub width: i32,
    pub height: i32,
    pub duration_ms: Option<i32>,
    pub frames: i32,
    pub has_audio: bool,
    pub file_size: i64,
    pub storage_key: String,
    pub phash: Option<i64>,
    pub processed_at: Option<OffsetDateTime>,
}

impl Asset {
    pub fn sha256_hex(&self) -> String {
        hex::encode(&self.sha256)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InsertAssetError {
    /// The same file is already stored, for this post.
    #[error("already uploaded as post #{0}")]
    Duplicate(i64),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// `SELECT <asset columns> FROM media_assets` followed by `$rest`.
macro_rules! select_assets {
    ($rest:literal) => {
        concat!(
            "SELECT id, post_id, sha256, md5, media_type, width, height, duration_ms, frames, has_audio,
                    file_size, storage_key, phash, processed_at
             FROM media_assets ",
            $rest
        )
    };
}

/// Inserts an asset. A concurrent upload of the same file surfaces as
/// [`InsertAssetError::Duplicate`] (caught by the unique index), which the
/// caller should treat like the duplicate check it did beforehand.
pub async fn insert(db: impl PgExecutor<'_>, asset: NewAsset<'_>) -> Result<i64, InsertAssetError> {
    sqlx::query_scalar(
        "INSERT INTO media_assets
             (post_id, sha256, md5, media_type, width, height, duration_ms, frames, has_audio, file_size, storage_key)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
         RETURNING id",
    )
    .bind(asset.post_id)
    .bind(&asset.sha256[..])
    .bind(&asset.md5[..])
    .bind(asset.media_type)
    .bind(asset.width)
    .bind(asset.height)
    .bind(asset.duration_ms)
    .bind(asset.frames)
    .bind(asset.has_audio)
    .bind(asset.file_size)
    .bind(asset.storage_key)
    .fetch_one(db)
    .await
    .map_err(|error| match crate::users::unique_violation(&error) {
        Some("media_assets_sha256_key") => InsertAssetError::Duplicate(0),
        _ => InsertAssetError::Db(error),
    })
}

/// The post that already has this exact file, if any.
pub async fn post_with_sha256(
    db: impl PgExecutor<'_>,
    sha256: &[u8; 32],
) -> sqlx::Result<Option<i64>> {
    sqlx::query_scalar("SELECT post_id FROM media_assets WHERE sha256 = $1")
        .bind(&sha256[..])
        .fetch_optional(db)
        .await
}

pub async fn by_id(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Option<Asset>> {
    sqlx::query_as(select_assets!("WHERE id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
}

pub async fn for_post(db: impl PgExecutor<'_>, post_id: i64) -> sqlx::Result<Option<Asset>> {
    sqlx::query_as(select_assets!("WHERE post_id = $1"))
        .bind(post_id)
        .fetch_optional(db)
        .await
}

/// The assets of the posts among `post_ids`, in no particular order.
pub async fn for_posts(db: impl PgExecutor<'_>, post_ids: &[i64]) -> sqlx::Result<Vec<Asset>> {
    sqlx::query_as(select_assets!("WHERE post_id = ANY($1)"))
        .bind(post_ids)
        .fetch_all(db)
        .await
}

/// Asset ids for the given posts, or for every post when `None`.
pub async fn asset_ids(
    db: impl PgExecutor<'_>,
    post_ids: Option<&[i64]>,
) -> sqlx::Result<Vec<i64>> {
    sqlx::query_scalar(
        "SELECT id FROM media_assets WHERE $1::bigint[] IS NULL OR post_id = ANY($1) ORDER BY id",
    )
    .bind(post_ids)
    .fetch_all(db)
    .await
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Variant {
    pub asset_id: i64,
    pub kind: String,
    pub format: String,
    pub width: i32,
    pub height: i32,
    pub file_size: i64,
    pub storage_key: String,
}

/// Records a generated rendition, replacing any earlier one of the same
/// kind (reprocessing).
pub async fn save_variant(db: impl PgExecutor<'_>, variant: &Variant) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO media_variants (asset_id, kind, format, width, height, file_size, storage_key)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (asset_id, kind) DO UPDATE
         SET format = EXCLUDED.format, width = EXCLUDED.width, height = EXCLUDED.height,
             file_size = EXCLUDED.file_size, storage_key = EXCLUDED.storage_key",
    )
    .bind(variant.asset_id)
    .bind(&variant.kind)
    .bind(&variant.format)
    .bind(variant.width)
    .bind(variant.height)
    .bind(variant.file_size)
    .bind(&variant.storage_key)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn variants(db: impl PgExecutor<'_>, asset_id: i64) -> sqlx::Result<Vec<Variant>> {
    sqlx::query_as(
        "SELECT asset_id, kind, format, width, height, file_size, storage_key
         FROM media_variants WHERE asset_id = $1 ORDER BY kind",
    )
    .bind(asset_id)
    .fetch_all(db)
    .await
}

/// The variants of all `asset_ids`, by asset and kind.
pub async fn variants_of(db: impl PgExecutor<'_>, asset_ids: &[i64]) -> sqlx::Result<Vec<Variant>> {
    sqlx::query_as(
        "SELECT asset_id, kind, format, width, height, file_size, storage_key
         FROM media_variants WHERE asset_id = ANY($1) ORDER BY asset_id, kind",
    )
    .bind(asset_ids)
    .fetch_all(db)
    .await
}

/// Marks processing finished and stores the perceptual hash, split into
/// the four 16-bit chunks the similarity index uses.
pub async fn mark_processed(
    db: impl PgExecutor<'_>,
    id: i64,
    phash: Option<u64>,
) -> sqlx::Result<()> {
    let chunks = phash.map(phash_chunks);
    sqlx::query(
        "UPDATE media_assets
         SET processed_at = now(), phash = $2, phash_0 = $3, phash_1 = $4, phash_2 = $5, phash_3 = $6
         WHERE id = $1",
    )
    .bind(id)
    .bind(phash.map(|h| h as i64))
    .bind(chunks.map(|c| c[0]))
    .bind(chunks.map(|c| c[1]))
    .bind(chunks.map(|c| c[2]))
    .bind(chunks.map(|c| c[3]))
    .execute(db)
    .await?;
    Ok(())
}

/// Hashes within this many bits are always found: by pigeonhole, a hash
/// differing in at most 3 bits matches at least one 16-bit chunk exactly,
/// and each chunk is indexed.
pub const SIMILAR_MAX_DISTANCE: u32 = 3;

/// A post whose file looks like another.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Similar {
    pub post_id: i64,
    pub distance: i32,
}

/// Posts whose perceptual hash is within `max_distance` bits of `hash`,
/// closest first. Candidates come from the chunk indexes, so results are
/// complete up to [`SIMILAR_MAX_DISTANCE`]; larger distances only find
/// matches that happen to share a chunk.
pub async fn similar(
    db: impl PgExecutor<'_>,
    hash: u64,
    max_distance: u32,
    exclude_post: Option<i64>,
    limit: i64,
) -> sqlx::Result<Vec<Similar>> {
    let [c0, c1, c2, c3] = phash_chunks(hash);
    sqlx::query_as(
        "SELECT post_id, distance FROM (
             SELECT post_id, bit_count((phash # $1)::bit(64))::int AS distance
             FROM media_assets
             WHERE phash IS NOT NULL
               AND (phash_0 = $2 OR phash_1 = $3 OR phash_2 = $4 OR phash_3 = $5)
               AND post_id IS DISTINCT FROM $6
         ) candidates
         WHERE distance <= $7
         ORDER BY distance, post_id DESC
         LIMIT $8",
    )
    .bind(hash as i64)
    .bind(c0)
    .bind(c1)
    .bind(c2)
    .bind(c3)
    .bind(exclude_post)
    .bind(i32::try_from(max_distance).unwrap_or(64))
    .bind(limit)
    .fetch_all(db)
    .await
}

/// A 64-bit hash as four 16-bit values (stored as smallint).
pub fn phash_chunks(hash: u64) -> [i16; 4] {
    [0, 1, 2, 3].map(|i| (hash >> (48 - 16 * i)) as u16 as i16)
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;
    use uwu_core::posts::{PostStatus, Rating};

    use super::*;
    use crate::posts::{self, NewPost};

    async fn post(pool: &PgPool) -> i64 {
        let new = NewPost {
            uploader_id: None,
            rating: Rating::General,
            status: PostStatus::Active,
            source: "",
            description: "",
            tag_ids: &[],
        };
        posts::insert(pool, new).await.unwrap()
    }

    fn asset(post_id: i64, sha256: &[u8; 32]) -> NewAsset<'_> {
        NewAsset {
            post_id,
            sha256,
            md5: &[7; 16],
            media_type: "png",
            width: 640,
            height: 480,
            duration_ms: None,
            frames: 1,
            has_audio: false,
            file_size: 1234,
            storage_key: "original/ab/cd/x.png",
        }
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn stores_assets_and_finds_duplicates(pool: PgPool) {
        let post_id = post(&pool).await;
        let id = insert(&pool, asset(post_id, &[1; 32])).await.unwrap();
        assert_eq!(
            post_with_sha256(&pool, &[1; 32]).await.unwrap(),
            Some(post_id)
        );
        assert_eq!(post_with_sha256(&pool, &[2; 32]).await.unwrap(), None);

        let stored = for_post(&pool, post_id).await.unwrap().unwrap();
        assert_eq!(
            (stored.id, stored.width, stored.processed_at),
            (id, 640, None)
        );
        assert_eq!(stored.sha256_hex(), "01".repeat(32));

        let other = post(&pool).await;
        let err = insert(&pool, asset(other, &[1; 32])).await.unwrap_err();
        assert!(matches!(err, InsertAssetError::Duplicate(_)), "{err:?}");
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn variants_replace_by_kind_and_processing_stores_the_hash(pool: PgPool) {
        let post_id = post(&pool).await;
        let id = insert(&pool, asset(post_id, &[1; 32])).await.unwrap();
        let mut thumb = Variant {
            asset_id: id,
            kind: "thumb-250".into(),
            format: "webp".into(),
            width: 250,
            height: 188,
            file_size: 999,
            storage_key: "thumb-250/ab/cd/x.webp".into(),
        };
        save_variant(&pool, &thumb).await.unwrap();
        thumb.file_size = 555;
        save_variant(&pool, &thumb).await.unwrap();
        assert_eq!(variants(&pool, id).await.unwrap(), vec![thumb]);

        let hash = 0xF00D_0001_8000_FFFFu64;
        mark_processed(&pool, id, Some(hash)).await.unwrap();
        let (stored, chunks): (Option<i64>, Vec<i16>) = sqlx::query_as(
            "SELECT phash, ARRAY[phash_0, phash_1, phash_2, phash_3] FROM media_assets WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stored.map(|h| h as u64), Some(hash));
        assert_eq!(chunks, phash_chunks(hash));
        assert!(
            for_post(&pool, post_id)
                .await
                .unwrap()
                .unwrap()
                .processed_at
                .is_some()
        );
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn finds_similar_hashes_through_the_chunk_index(pool: PgPool) {
        let base = 0x0123_4567_89AB_CDEFu64;
        // 0, 1, 3 and 4 bits away, spread over different chunks; plus an
        // unrelated hash.
        let hashes = [
            base,
            base ^ 1,
            base ^ (1 << 20 | 1 << 40 | 1 << 60),
            base ^ (1 | 1 << 20 | 1 << 40 | 1 << 60),
            !base,
        ];
        let mut posts_by_hash = Vec::new();
        for (n, hash) in hashes.iter().enumerate() {
            let post_id = post(&pool).await;
            let sha = [n as u8; 32];
            let id = insert(&pool, asset(post_id, &sha)).await.unwrap();
            mark_processed(&pool, id, Some(*hash)).await.unwrap();
            posts_by_hash.push(post_id);
        }

        let found = similar(
            &pool,
            base,
            SIMILAR_MAX_DISTANCE,
            Some(posts_by_hash[0]),
            10,
        )
        .await
        .unwrap();
        let summary: Vec<(i64, i32)> = found.iter().map(|s| (s.post_id, s.distance)).collect();
        assert_eq!(summary, [(posts_by_hash[1], 1), (posts_by_hash[2], 3)]);

        // At realistic sizes the chunk indexes do the finding.
        sqlx::query(
            "WITH p AS (
                 INSERT INTO posts (rating) SELECT 'g' FROM generate_series(1, 5000) RETURNING id
             )
             INSERT INTO media_assets
                 (post_id, sha256, md5, media_type, width, height, file_size, storage_key,
                  phash, phash_0, phash_1, phash_2, phash_3, processed_at)
             SELECT id, sha256(id::text::bytea), '\\x00000000000000000000000000000000', 'png', 1, 1, 1, 'k',
                    (random() * 9e18)::bigint,
                    (random() * 65535 - 32768)::smallint, (random() * 65535 - 32768)::smallint,
                    (random() * 65535 - 32768)::smallint, (random() * 65535 - 32768)::smallint, now()
             FROM p",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("ANALYZE media_assets")
            .execute(&pool)
            .await
            .unwrap();
        let plan: Vec<String> = sqlx::query_scalar(
            "EXPLAIN SELECT post_id FROM media_assets
             WHERE phash IS NOT NULL
               AND (phash_0 = 1::int2 OR phash_1 = 2::int2 OR phash_2 = 3::int2 OR phash_3 = 4::int2)",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        for index in ["phash_0", "phash_1", "phash_2", "phash_3"] {
            let name = format!("media_assets_{index}_idx");
            assert!(plan.iter().any(|line| line.contains(&name)), "{plan:#?}");
        }
    }

    #[test]
    fn chunks_split_high_to_low() {
        assert_eq!(phash_chunks(0x0001_0002_0003_0004), [1, 2, 3, 4]);
        assert_eq!(phash_chunks(u64::MAX), [-1, -1, -1, -1]);
    }
}
