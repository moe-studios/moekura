//! Artist entries, their URLs and history (`artists`, `artist_urls`,
//! `artist_versions`).

use moekura_core::artists::{ArtistUrl, normalize_url, url_prefixes};
use moekura_core::sites::encoded_url;
use sqlx::{PgConnection, PgExecutor, PgPool};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Artist {
    pub id: i32,
    pub name: String,
    pub group_name: String,
    pub other_names: Vec<String>,
    pub is_banned: bool,
    pub is_deleted: bool,
    pub version: i32,
    pub updater_name: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// `SELECT <artist columns> FROM artists a LEFT JOIN users u` followed by
/// `$rest`.
macro_rules! select_artists {
    ($rest:literal) => {
        concat!(
            "SELECT a.id, a.name::text AS name, a.group_name, a.other_names, a.is_banned,
                    a.is_deleted, a.version, u.name::text AS updater_name, a.created_at,
                    a.updated_at
             FROM artists a LEFT JOIN users u ON u.id = a.updater_id ",
            $rest
        )
    };
}

pub async fn by_id(db: impl PgExecutor<'_>, id: i32) -> sqlx::Result<Option<Artist>> {
    sqlx::query_as(select_artists!("WHERE a.id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
}

/// The entry for the artist tag `name` (normalised).
pub async fn by_name(db: impl PgExecutor<'_>, name: &str) -> sqlx::Result<Option<Artist>> {
    sqlx::query_as(select_artists!("WHERE a.name = $1"))
        .bind(name)
        .fetch_optional(db)
        .await
}

/// The entries for any of the tags `names`, by name.
pub async fn by_names(db: impl PgExecutor<'_>, names: &[String]) -> sqlx::Result<Vec<Artist>> {
    sqlx::query_as(select_artists!("WHERE a.name = ANY($1) ORDER BY a.name"))
        .bind(names)
        .fetch_all(db)
        .await
}

pub async fn by_ids(db: impl PgExecutor<'_>, ids: &[i32]) -> sqlx::Result<Vec<Artist>> {
    sqlx::query_as(select_artists!("WHERE a.id = ANY($1) ORDER BY a.id"))
        .bind(ids)
        .fetch_all(db)
        .await
}

/// A URL of an artist's, as stored.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct StoredUrl {
    pub id: i64,
    pub artist_id: i32,
    pub url: String,
    pub normalized_url: String,
    pub is_active: bool,
    pub created_at: OffsetDateTime,
}

/// The URLs of artists `ids`, each artist's in order.
pub async fn urls(db: impl PgExecutor<'_>, ids: &[i32]) -> sqlx::Result<Vec<StoredUrl>> {
    sqlx::query_as(
        "SELECT id, artist_id, url, normalized_url, is_active, created_at FROM artist_urls
         WHERE artist_id = ANY($1) ORDER BY artist_id, position",
    )
    .bind(ids)
    .fetch_all(db)
    .await
}

/// Which artists [`list`] returns; every field narrows it.
#[derive(Debug, Clone, Default)]
pub struct Filter<'a> {
    /// Matches the name, any other name or the group name: a prefix, or a
    /// pattern with `*` wildcards (other and group names regardless of
    /// case). Empty for all.
    pub name: &'a str,
    /// The artist has a URL that this URL, or a page under it, matches.
    pub url: &'a str,
    pub banned: Option<bool>,
    pub with_deleted: bool,
    pub ids: Option<&'a [i32]>,
}

/// Artists matching `filter`, most recently changed first.
pub async fn list(
    db: impl PgExecutor<'_>,
    filter: &Filter<'_>,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<Artist>> {
    let like = crate::tags::like_pattern(filter.name);
    let url_prefixes = if filter.url.trim().is_empty() {
        None
    } else {
        Some(url_prefixes(filter.url))
    };
    sqlx::query_as(select_artists!(
        "WHERE ($1 = '%'
                OR a.name LIKE $1
                OR lower(a.group_name) LIKE lower($1)
                OR EXISTS (SELECT 1 FROM unnest(a.other_names) n WHERE lower(n) LIKE lower($1)))
           AND ($2::text[] IS NULL OR EXISTS (
                SELECT 1 FROM artist_urls au
                WHERE au.artist_id = a.id AND au.normalized_url = ANY($2)))
           AND ($3::boolean IS NULL OR a.is_banned = $3)
           AND ($4 OR NOT a.is_deleted)
           AND ($5::int[] IS NULL OR a.id = ANY($5))
         ORDER BY a.updated_at DESC, a.id DESC OFFSET $6 LIMIT $7"
    ))
    .bind(like)
    .bind(url_prefixes)
    .bind(filter.banned)
    .bind(filter.with_deleted)
    .bind(filter.ids)
    .bind(offset)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// The artists (not deleted) that `url` belongs to: those with that URL,
/// or else with the longest page above it (see
/// [`moekura_core::artists::url_prefixes`]).
pub async fn find_by_url(db: impl PgExecutor<'_>, url: &str) -> sqlx::Result<Vec<Artist>> {
    let prefixes = url_prefixes(url);
    if prefixes.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as(select_artists!(
        "JOIN (SELECT au.artist_id, max(length(au.normalized_url)) AS matched
               FROM artist_urls au WHERE au.normalized_url = ANY($1)
               GROUP BY au.artist_id) m ON m.artist_id = a.id
         WHERE NOT a.is_deleted
           AND m.matched = (SELECT max(length(normalized_url)) FROM artist_urls au
                            JOIN artists b ON b.id = au.artist_id
                            WHERE au.normalized_url = ANY($1) AND NOT b.is_deleted)
         ORDER BY a.name"
    ))
    .bind(prefixes)
    .fetch_all(db)
    .await
}

/// The ids of the tags of banned artists (not deleted).
pub async fn banned_tag_ids(db: impl PgExecutor<'_>) -> sqlx::Result<Vec<i32>> {
    sqlx::query_scalar(
        "SELECT t.id FROM artists a JOIN tags t ON t.name = a.name
         WHERE a.is_banned AND NOT a.is_deleted ORDER BY t.id",
    )
    .fetch_all(db)
    .await
}

/// The names of banned artists (not deleted) among the tags `tag_ids`.
pub async fn banned_among_tags(
    db: impl PgExecutor<'_>,
    tag_ids: &[i32],
) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT a.name::text FROM artists a JOIN tags t ON t.name = a.name
         WHERE t.id = ANY($1) AND a.is_banned AND NOT a.is_deleted ORDER BY a.name",
    )
    .bind(tag_ids)
    .fetch_all(db)
    .await
}

/// An artist as saved: what [`save`] writes and each version records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contents {
    /// A normalised tag name.
    pub name: String,
    pub group_name: String,
    /// Normalised like wiki other names.
    pub other_names: Vec<String>,
    pub urls: Vec<ArtistUrl>,
    pub is_banned: bool,
    pub is_deleted: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    /// Someone else saved the artist since the editor loaded `base`.
    #[error("the artist changed since version {base}; it is now at version {current}")]
    Conflict { base: i32, current: i32 },
    #[error("there is already an artist entry with that name")]
    NameTaken,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

fn db_error(e: sqlx::Error) -> SaveError {
    match &e {
        sqlx::Error::Database(db) if db.constraint() == Some("artists_name_idx") => {
            SaveError::NameTaken
        }
        _ => SaveError::Db(e),
    }
}

/// Creates an artist and returns its id.
pub async fn create(
    db: &PgPool,
    contents: &Contents,
    updater_id: Option<i64>,
) -> Result<i32, SaveError> {
    let mut tx = db.begin().await?;
    let id: i32 = sqlx::query_scalar(
        "INSERT INTO artists (name, group_name, other_names, is_banned, is_deleted, updater_id)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(&contents.name)
    .bind(&contents.group_name)
    .bind(&contents.other_names)
    .bind(contents.is_banned)
    .bind(contents.is_deleted)
    .bind(updater_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(db_error)?;
    write_urls(&mut tx, id, &contents.urls).await?;
    record_version(&mut tx, id, 1, updater_id, contents).await?;
    if contents.is_banned {
        announce_bans(&mut tx).await?;
    }
    tx.commit().await?;
    Ok(id)
}

/// Saves `contents` as artist `id` and returns its version afterwards.
///
/// `base` is the version the editor started from; a different current
/// version is a [`SaveError::Conflict`]. `None` saves over whatever is
/// there. Saving what's already there records no new version.
pub async fn save(
    db: &PgPool,
    id: i32,
    contents: &Contents,
    updater_id: Option<i64>,
    base: Option<i32>,
) -> Result<i32, SaveError> {
    let mut tx = db.begin().await?;
    let (version,): (i32,) = sqlx::query_as("SELECT version FROM artists WHERE id = $1 FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(SaveError::Db(sqlx::Error::RowNotFound))?;
    if let Some(base) = base
        && base != version
    {
        return Err(SaveError::Conflict {
            base,
            current: version,
        });
    }
    let before = current_contents(&mut tx, id).await?;
    if before.as_ref() == Some(contents) {
        return Ok(version);
    }
    sqlx::query(
        "UPDATE artists SET name = $2, group_name = $3, other_names = $4, is_banned = $5,
                            is_deleted = $6, version = $7, updater_id = $8, updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(&contents.name)
    .bind(&contents.group_name)
    .bind(&contents.other_names)
    .bind(contents.is_banned)
    .bind(contents.is_deleted)
    .bind(version + 1)
    .bind(updater_id)
    .execute(&mut *tx)
    .await
    .map_err(db_error)?;
    write_urls(&mut tx, id, &contents.urls).await?;
    record_version(&mut tx, id, version + 1, updater_id, contents).await?;
    if contents.is_banned || before.is_some_and(|b| b.is_banned) {
        announce_bans(&mut tx).await?;
    }
    tx.commit().await?;
    Ok(version + 1)
}

/// An artist's contents now, as [`save`] compares them.
pub async fn contents(db: &PgPool, id: i32) -> sqlx::Result<Option<Contents>> {
    let mut conn = db.acquire().await?;
    current_contents(&mut conn, id).await
}

async fn current_contents(conn: &mut PgConnection, id: i32) -> sqlx::Result<Option<Contents>> {
    let row: Option<(String, String, Vec<String>, bool, bool)> = sqlx::query_as(
        "SELECT name::text, group_name, other_names, is_banned, is_deleted FROM artists
         WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((name, group_name, other_names, is_banned, is_deleted)) = row else {
        return Ok(None);
    };
    let urls = urls(&mut *conn, &[id])
        .await?
        .into_iter()
        .map(|u| ArtistUrl {
            url: u.url,
            is_active: u.is_active,
        })
        .collect();
    Ok(Some(Contents {
        name,
        group_name,
        other_names,
        urls,
        is_banned,
        is_deleted,
    }))
}

/// Tells every node to reload the banned artists' tags, once the
/// transaction commits.
async fn announce_bans(conn: &mut PgConnection) -> sqlx::Result<()> {
    sqlx::query("SELECT pg_notify($1, 'artists')")
        .bind(crate::site_cache::CHANNEL)
        .execute(conn)
        .await?;
    Ok(())
}

/// Recomputes every artist URL's normalized form with the current rules
/// ([`normalize_url`]), a batch at a time, encoding what a stored URL
/// can't hold raw ([`encoded_url`]: quotes, angle brackets, spaces) on
/// the way. Returns how many changed.
pub async fn renormalize_urls(db: &PgPool) -> sqlx::Result<u64> {
    let mut changed = 0;
    let mut after = 0_i64;
    loop {
        let batch: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT id, url, normalized_url FROM artist_urls WHERE id > $1 ORDER BY id LIMIT 1000",
        )
        .bind(after)
        .fetch_all(db)
        .await?;
        let Some((last, _, _)) = batch.last() else {
            break;
        };
        after = *last;
        let mut ids = Vec::new();
        let mut urls = Vec::new();
        let mut normalized = Vec::new();
        for (id, url, old) in batch {
            let encoded = encoded_url(&url);
            let new = normalize_url(&encoded).unwrap_or_else(|| encoded.clone());
            if encoded != url || new != old {
                ids.push(id);
                urls.push(encoded);
                normalized.push(new);
            }
        }
        if ids.is_empty() {
            continue;
        }
        changed += sqlx::query(
            "UPDATE artist_urls au SET url = n.url, normalized_url = n.normalized
             FROM unnest($1::bigint[], $2::text[], $3::text[]) AS n(id, url, normalized)
             WHERE au.id = n.id",
        )
        .bind(ids)
        .bind(urls)
        .bind(normalized)
        .execute(db)
        .await?
        .rows_affected();
    }
    Ok(changed)
}

async fn write_urls(conn: &mut PgConnection, id: i32, urls: &[ArtistUrl]) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM artist_urls WHERE artist_id = $1")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    if urls.is_empty() {
        return Ok(());
    }
    let positions: Vec<i32> = (0..urls.len() as i32).collect();
    let raw: Vec<&str> = urls.iter().map(|u| u.url.as_str()).collect();
    let normalized: Vec<String> = urls
        .iter()
        .map(|u| normalize_url(&u.url).unwrap_or_else(|| u.url.clone()))
        .collect();
    let active: Vec<bool> = urls.iter().map(|u| u.is_active).collect();
    sqlx::query(
        "INSERT INTO artist_urls (artist_id, position, url, normalized_url, is_active)
         SELECT $1, * FROM unnest($2::int[], $3::text[], $4::text[], $5::bool[])",
    )
    .bind(id)
    .bind(positions)
    .bind(raw)
    .bind(normalized)
    .bind(active)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// How a version stores a URL: inactive ones start with `-`.
fn version_url(url: &ArtistUrl) -> String {
    if url.is_active {
        url.url.clone()
    } else {
        format!("-{}", url.url)
    }
}

async fn record_version(
    conn: &mut PgConnection,
    id: i32,
    version: i32,
    updater_id: Option<i64>,
    contents: &Contents,
) -> sqlx::Result<()> {
    let urls: Vec<String> = contents.urls.iter().map(version_url).collect();
    sqlx::query(
        "INSERT INTO artist_versions (artist_id, version, updater_id, name, group_name,
                                      other_names, urls, is_banned, is_deleted)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(version)
    .bind(updater_id)
    .bind(&contents.name)
    .bind(&contents.group_name)
    .bind(&contents.other_names)
    .bind(urls)
    .bind(contents.is_banned)
    .bind(contents.is_deleted)
    .execute(conn)
    .await?;
    Ok(())
}

/// A version, with the one before it for showing what changed.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Version {
    pub id: i64,
    pub artist_id: i32,
    pub version: i32,
    pub updater_id: Option<i64>,
    pub updater_name: Option<String>,
    pub name: String,
    pub group_name: String,
    pub other_names: Vec<String>,
    /// Inactive ones start with `-`.
    pub urls: Vec<String>,
    pub is_banned: bool,
    pub is_deleted: bool,
    pub created_at: OffsetDateTime,
    pub previous_name: Option<String>,
    pub previous_group_name: Option<String>,
    pub previous_other_names: Option<Vec<String>>,
    pub previous_urls: Option<Vec<String>>,
    pub previous_is_banned: Option<bool>,
    pub previous_is_deleted: Option<bool>,
}

/// Which versions [`versions`] returns.
#[derive(Debug, Clone, Copy, Default)]
pub struct VersionFilter {
    pub artist_id: Option<i32>,
    pub updater_id: Option<i64>,
    /// Older than this version id.
    pub before: Option<i64>,
    /// Versions to skip.
    pub offset: i64,
}

/// Versions, newest first.
pub async fn versions(
    db: impl PgExecutor<'_>,
    filter: VersionFilter,
    limit: i64,
) -> sqlx::Result<Vec<Version>> {
    sqlx::query_as(
        "SELECT v.id, v.artist_id, v.version, v.updater_id, u.name::text AS updater_name,
                v.name, v.group_name, v.other_names, v.urls, v.is_banned, v.is_deleted,
                v.created_at,
                p.name AS previous_name, p.group_name AS previous_group_name,
                p.other_names AS previous_other_names, p.urls AS previous_urls,
                p.is_banned AS previous_is_banned, p.is_deleted AS previous_is_deleted
         FROM artist_versions v
         LEFT JOIN artist_versions p ON p.artist_id = v.artist_id AND p.version = v.version - 1
         LEFT JOIN users u ON u.id = v.updater_id
         WHERE ($1::int IS NULL OR v.artist_id = $1)
           AND ($2::bigint IS NULL OR v.updater_id = $2)
           AND ($3::bigint IS NULL OR v.id < $3)
         ORDER BY v.id DESC OFFSET $5 LIMIT $4",
    )
    .bind(filter.artist_id)
    .bind(filter.updater_id)
    .bind(filter.before)
    .bind(limit)
    .bind(filter.offset)
    .fetch_all(db)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str, urls: &str) -> Contents {
        Contents {
            name: name.into(),
            group_name: String::new(),
            other_names: vec!["ネコ".into()],
            urls: ArtistUrl::parse_list(urls).unwrap(),
            is_banned: false,
            is_deleted: false,
        }
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn renormalizes_old_urls(pool: PgPool) {
        let id = create(&pool, &sample("sa_dui", "https://example.com/a"), None)
            .await
            .unwrap();
        // Saved before profiles were made canonical.
        sqlx::query(
            "UPDATE artist_urls SET url = 'https://www.artstation.com/artist/sa-dui',
             normalized_url = 'artstation.com/artist/sa-dui' WHERE artist_id = $1",
        )
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
        assert!(
            find_by_url(&pool, "https://sa-dui.artstation.com")
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(renormalize_urls(&pool).await.unwrap(), 1);
        assert_eq!(renormalize_urls(&pool).await.unwrap(), 0);
        let found = find_by_url(&pool, "https://sa-dui.artstation.com")
            .await
            .unwrap();
        assert_eq!(found.iter().map(|a| a.id).collect::<Vec<_>>(), [id]);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn renormalizing_encodes_raw_urls(pool: PgPool) {
        let id = create(&pool, &sample("cat_artist", "https://example.com/a"), None)
            .await
            .unwrap();
        // Saved while canonical forms kept decoded quotes and brackets.
        sqlx::query(
            "UPDATE artist_urls SET url = 'https://misskey.io/@a\"><b>''c',
             normalized_url = 'misskey.io/@a%22%3E%3Cb%3E''c' WHERE artist_id = $1",
        )
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(renormalize_urls(&pool).await.unwrap(), 1);
        assert_eq!(renormalize_urls(&pool).await.unwrap(), 0);
        let stored = urls(&pool, &[id]).await.unwrap();
        assert_eq!(stored[0].url, "https://misskey.io/@a%22%3E%3Cb%3E%27c");
        assert_eq!(stored[0].normalized_url, "misskey.io/@a%22%3E%3Cb%3E%27c");
        let found = find_by_url(&pool, "https://misskey.io/@a%22%3E%3Cb%3E'c")
            .await
            .unwrap();
        assert_eq!(found.iter().map(|a| a.id).collect::<Vec<_>>(), [id]);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn saves_with_history_and_finds_by_url(pool: PgPool) {
        let cat = create(
            &pool,
            &sample("cat_artist", "https://twitter.com/cat pixiv.net/users/1"),
            None,
        )
        .await
        .unwrap();
        let dog = create(&pool, &sample("dog_artist", "twitter.com/dog"), None)
            .await
            .unwrap();
        assert!(matches!(
            create(&pool, &sample("cat_artist", ""), None).await,
            Err(SaveError::NameTaken)
        ));

        let found = |url: &'static str| {
            let pool = pool.clone();
            async move {
                find_by_url(&pool, url)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|a| a.id)
                    .collect::<Vec<_>>()
            }
        };
        assert_eq!(found("https://x.com/cat/status/5").await, [cat]);
        assert_eq!(found("https://www.pixiv.net/users/1/artworks").await, [cat]);
        assert_eq!(found("https://twitter.com/").await, Vec::<i32>::new());
        assert_eq!(found("https://twitter.com/doggo").await, Vec::<i32>::new());

        let mut changed = sample("cat_artist", "-https://twitter.com/cat");
        changed.group_name = "Cats".into();
        assert_eq!(save(&pool, cat, &changed, None, Some(1)).await.unwrap(), 2);
        assert_eq!(save(&pool, cat, &changed, None, Some(2)).await.unwrap(), 2);
        assert!(matches!(
            save(&pool, cat, &changed, None, Some(1)).await,
            Err(SaveError::Conflict { .. })
        ));
        assert_eq!(contents_of(&pool, cat).await, changed);
        // Inactive URLs still find the artist.
        assert_eq!(found("twitter.com/cat").await, [cat]);

        let history = versions(
            &pool,
            VersionFilter {
                artist_id: Some(cat),
                ..VersionFilter::default()
            },
            10,
        )
        .await
        .unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].urls, ["-https://x.com/cat"]);
        assert_eq!(
            history[0].previous_urls.as_deref(),
            Some(
                &[
                    "https://x.com/cat".to_owned(),
                    "https://www.pixiv.net/users/1".to_owned()
                ][..]
            )
        );

        let listed = |filter: Filter<'static>| {
            let pool = pool.clone();
            async move {
                list(&pool, &filter, 0, 10)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|a| a.id)
                    .collect::<Vec<_>>()
            }
        };
        assert_eq!(
            listed(Filter {
                name: "cats",
                ..Filter::default()
            })
            .await,
            [cat]
        );
        assert_eq!(
            listed(Filter {
                name: "ネ*",
                ..Filter::default()
            })
            .await,
            [cat, dog]
        );
        assert_eq!(
            listed(Filter {
                url: "https://twitter.com/dog/status/1",
                ..Filter::default()
            })
            .await,
            [dog]
        );

        sqlx::query("INSERT INTO tags (name) VALUES ('dog_artist')")
            .execute(&pool)
            .await
            .unwrap();
        assert!(banned_tag_ids(&pool).await.unwrap().is_empty());
        let mut banned = sample("dog_artist", "twitter.com/dog");
        banned.is_banned = true;
        save(&pool, dog, &banned, None, None).await.unwrap();
        assert_eq!(banned_tag_ids(&pool).await.unwrap().len(), 1);
    }

    async fn contents_of(pool: &PgPool, id: i32) -> Contents {
        super::contents(pool, id).await.unwrap().unwrap()
    }
}
