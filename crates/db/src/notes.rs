//! Notes on posts and their history (`notes`, `note_versions`). A trigger
//! keeps `posts.note_count` and `posts.last_noted_at` in step.

use moekura_core::notes::NoteBox;
use sqlx::{PgConnection, PgExecutor, PgPool};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Note {
    pub id: i64,
    pub post_id: i64,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub body: String,
    pub is_active: bool,
    pub version: i32,
    pub creator_id: Option<i64>,
    pub updater_name: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

impl Note {
    pub fn note_box(&self) -> NoteBox {
        NoteBox {
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
        }
    }
}

macro_rules! select_notes {
    ($rest:literal) => {
        concat!(
            "SELECT n.id, n.post_id, n.x, n.y, n.width, n.height, n.body, n.is_active,
                    n.version, n.creator_id, u.name::text AS updater_name,
                    n.created_at, n.updated_at
             FROM notes n LEFT JOIN users u ON u.id = n.updater_id ",
            $rest
        )
    };
}

pub async fn by_id(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Option<Note>> {
    sqlx::query_as(select_notes!("WHERE n.id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
}

/// A post's notes, oldest first; deleted ones too if `with_inactive`.
pub async fn for_post(
    db: impl PgExecutor<'_>,
    post_id: i64,
    with_inactive: bool,
) -> sqlx::Result<Vec<Note>> {
    sqlx::query_as(select_notes!(
        "WHERE n.post_id = $1 AND ($2 OR n.is_active) ORDER BY n.id"
    ))
    .bind(post_id)
    .bind(with_inactive)
    .fetch_all(db)
    .await
}

/// Active notes on the posts among `post_ids`, by post, oldest first.
pub async fn for_posts(db: impl PgExecutor<'_>, post_ids: &[i64]) -> sqlx::Result<Vec<Note>> {
    sqlx::query_as(select_notes!(
        "WHERE n.post_id = ANY($1) AND n.is_active ORDER BY n.post_id, n.id"
    ))
    .bind(post_ids)
    .fetch_all(db)
    .await
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    /// Someone else changed the note since the editor loaded `base`.
    #[error("the note changed since version {base}; it is now at version {current}")]
    Conflict { base: i32, current: i32 },
    #[error("no such note")]
    NotFound,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// What a version records.
#[derive(Debug, Clone, PartialEq, Eq)]
struct State {
    note_box: NoteBox,
    body: String,
    is_active: bool,
}

async fn record_version(
    conn: &mut PgConnection,
    id: i64,
    post_id: i64,
    version: i32,
    updater_id: Option<i64>,
    state: &State,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO note_versions
             (note_id, post_id, version, updater_id, x, y, width, height, body, is_active)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(id)
    .bind(post_id)
    .bind(version)
    .bind(updater_id)
    .bind(state.note_box.x)
    .bind(state.note_box.y)
    .bind(state.note_box.width)
    .bind(state.note_box.height)
    .bind(&state.body)
    .bind(state.is_active)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Adds a note (already checked) and returns its id.
pub async fn create(
    db: &PgPool,
    post_id: i64,
    note_box: NoteBox,
    body: &str,
    creator_id: Option<i64>,
) -> sqlx::Result<i64> {
    let mut tx = db.begin().await?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO notes (post_id, x, y, width, height, body, creator_id, updater_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $7) RETURNING id",
    )
    .bind(post_id)
    .bind(note_box.x)
    .bind(note_box.y)
    .bind(note_box.width)
    .bind(note_box.height)
    .bind(body)
    .bind(creator_id)
    .fetch_one(&mut *tx)
    .await?;
    let state = State {
        note_box,
        body: body.to_owned(),
        is_active: true,
    };
    record_version(&mut tx, id, post_id, 1, creator_id, &state).await?;
    tx.commit().await?;
    Ok(id)
}

/// A change to a note; `None` fields stay as they are.
#[derive(Debug, Clone, Default)]
pub struct Changes {
    pub note_box: Option<NoteBox>,
    pub body: Option<String>,
    pub is_active: Option<bool>,
}

/// Changes note `id` and returns its version afterwards. `base` is the
/// version the editor started from (a different current one is a
/// [`SaveError::Conflict`]); `None` changes whatever is there. A change
/// that changes nothing records no version.
pub async fn update(
    db: &PgPool,
    id: i64,
    changes: &Changes,
    updater_id: Option<i64>,
    base: Option<i32>,
) -> Result<i32, SaveError> {
    let mut tx = db.begin().await?;
    let note: Note = sqlx::query_as(select_notes!("WHERE n.id = $1 FOR UPDATE OF n"))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(SaveError::NotFound)?;
    if let Some(base) = base
        && base != note.version
    {
        return Err(SaveError::Conflict {
            base,
            current: note.version,
        });
    }
    let before = State {
        note_box: note.note_box(),
        body: note.body.clone(),
        is_active: note.is_active,
    };
    let after = State {
        note_box: changes.note_box.unwrap_or(before.note_box),
        body: changes.body.clone().unwrap_or_else(|| before.body.clone()),
        is_active: changes.is_active.unwrap_or(before.is_active),
    };
    if after == before {
        return Ok(note.version);
    }
    let version = note.version + 1;
    sqlx::query(
        "UPDATE notes SET x = $2, y = $3, width = $4, height = $5, body = $6, is_active = $7,
                          version = $8, updater_id = $9, updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(after.note_box.x)
    .bind(after.note_box.y)
    .bind(after.note_box.width)
    .bind(after.note_box.height)
    .bind(&after.body)
    .bind(after.is_active)
    .bind(version)
    .bind(updater_id)
    .execute(&mut *tx)
    .await?;
    record_version(&mut tx, id, note.post_id, version, updater_id, &after).await?;
    tx.commit().await?;
    Ok(version)
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Version {
    pub note_id: i64,
    pub post_id: i64,
    pub version: i32,
    pub updater_name: Option<String>,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub body: String,
    pub is_active: bool,
    pub created_at: OffsetDateTime,
}

impl Version {
    /// What restoring this version changes a note to.
    pub fn changes(&self) -> Changes {
        Changes {
            note_box: Some(NoteBox {
                x: self.x,
                y: self.y,
                width: self.width,
                height: self.height,
            }),
            body: Some(self.body.clone()),
            is_active: Some(self.is_active),
        }
    }
}

macro_rules! select_versions {
    ($rest:literal) => {
        concat!(
            "SELECT v.note_id, v.post_id, v.version, u.name::text AS updater_name,
                    v.x, v.y, v.width, v.height, v.body, v.is_active, v.created_at
             FROM note_versions v LEFT JOIN users u ON u.id = v.updater_id ",
            $rest
        )
    };
}

/// The versions of a post's notes, newest first (at most `limit`).
pub async fn versions_for_post(
    db: impl PgExecutor<'_>,
    post_id: i64,
    limit: i64,
) -> sqlx::Result<Vec<Version>> {
    sqlx::query_as(select_versions!(
        "WHERE v.post_id = $1 ORDER BY v.id DESC LIMIT $2"
    ))
    .bind(post_id)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Note versions across the site, newest first, filtered by note, post
/// or updater, older than `before`.
pub async fn versions(
    db: impl PgExecutor<'_>,
    note_id: Option<i64>,
    post_id: Option<i64>,
    updater_id: Option<i64>,
    limit: i64,
) -> sqlx::Result<Vec<Version>> {
    sqlx::query_as(select_versions!(
        "WHERE ($1::bigint IS NULL OR v.note_id = $1)
           AND ($2::bigint IS NULL OR v.post_id = $2)
           AND ($3::bigint IS NULL OR v.updater_id = $3)
         ORDER BY v.id DESC LIMIT $4"
    ))
    .bind(note_id)
    .bind(post_id)
    .bind(updater_id)
    .bind(limit)
    .fetch_all(db)
    .await
}

pub async fn version(
    db: impl PgExecutor<'_>,
    note_id: i64,
    version: i32,
) -> sqlx::Result<Option<Version>> {
    sqlx::query_as(select_versions!("WHERE v.note_id = $1 AND v.version = $2"))
        .bind(note_id)
        .bind(version)
        .fetch_optional(db)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(x: i32, y: i32, width: i32, height: i32) -> NoteBox {
        NoteBox {
            x,
            y,
            width,
            height,
        }
    }

    async fn counts(pool: &PgPool, post: i64) -> (i32, bool) {
        let (count, at): (i32, Option<OffsetDateTime>) =
            sqlx::query_as("SELECT note_count, last_noted_at FROM posts WHERE id = $1")
                .bind(post)
                .fetch_one(pool)
                .await
                .unwrap();
        (count, at.is_some())
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn notes_versions_and_counts(pool: PgPool) {
        let post: i64 = sqlx::query_scalar("INSERT INTO posts (rating) VALUES ('g') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(counts(&pool, post).await, (0, false));
        let id = create(&pool, post, b(1, 2, 30, 40), "Hello", None)
            .await
            .unwrap();
        create(&pool, post, b(5, 5, 5, 5), "Other", None)
            .await
            .unwrap();
        assert_eq!(counts(&pool, post).await, (2, true));

        let moved = Changes {
            note_box: Some(b(10, 20, 30, 40)),
            ..Changes::default()
        };
        assert_eq!(update(&pool, id, &moved, None, Some(1)).await.unwrap(), 2);
        // Unchanged: no version.
        assert_eq!(update(&pool, id, &moved, None, Some(2)).await.unwrap(), 2);
        assert!(matches!(
            update(&pool, id, &moved, None, Some(1)).await,
            Err(SaveError::Conflict {
                base: 1,
                current: 2
            })
        ));
        let deleted = Changes {
            is_active: Some(false),
            ..Changes::default()
        };
        update(&pool, id, &deleted, None, None).await.unwrap();
        assert_eq!(counts(&pool, post).await.0, 1);
        assert_eq!(for_post(&pool, post, false).await.unwrap().len(), 1);
        assert_eq!(for_post(&pool, post, true).await.unwrap().len(), 2);

        // Restore the first version.
        let first = version(&pool, id, 1).await.unwrap().unwrap();
        assert_eq!(
            update(&pool, id, &first.changes(), None, None)
                .await
                .unwrap(),
            4
        );
        let note = by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!((note.note_box(), note.is_active), (b(1, 2, 30, 40), true));
        assert_eq!(counts(&pool, post).await.0, 2);
        let history = versions_for_post(&pool, post, 10).await.unwrap();
        assert_eq!(
            history
                .iter()
                .map(|v| (v.note_id, v.version))
                .collect::<Vec<_>>()[..3],
            [(id, 4), (id, 3), (id, 2)]
        );
        assert_eq!(
            versions(&pool, Some(id), None, None, 10)
                .await
                .unwrap()
                .len(),
            4
        );
        assert_eq!(for_posts(&pool, &[post]).await.unwrap().len(), 2);
    }
}
