//! Formal warnings staff give users (`user_warnings`), on their own or
//! about something they wrote.

use moekura_core::moderation::SubjectKind;
use sqlx::PgExecutor;
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Warning {
    pub id: i64,
    pub user_id: i64,
    pub creator_name: Option<String>,
    pub reason: String,
    pub subject_kind: Option<String>,
    pub subject_id: Option<i64>,
    pub created_at: OffsetDateTime,
}

/// Warns `user_id`, by `creator_id`, about `subject` if given (a comment
/// or forum post); returns the warning's id.
pub async fn create(
    db: impl PgExecutor<'_>,
    user_id: i64,
    creator_id: Option<i64>,
    reason: &str,
    subject: Option<(SubjectKind, i64)>,
) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "INSERT INTO user_warnings (user_id, creator_id, reason, subject_kind, subject_id)
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(user_id)
    .bind(creator_id)
    .bind(reason)
    .bind(subject.map(|(kind, _)| kind.as_str()))
    .bind(subject.map(|(_, id)| id))
    .fetch_one(db)
    .await
}

/// `user_id`'s warnings, newest first.
pub async fn for_user(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<Vec<Warning>> {
    sqlx::query_as(
        "SELECT w.id, w.user_id, c.name::text AS creator_name, w.reason, w.subject_kind,
                w.subject_id, w.created_at
         FROM user_warnings w LEFT JOIN users c ON c.id = w.creator_id
         WHERE w.user_id = $1 ORDER BY w.id DESC",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}

/// Which of `ids` (of `kind`) someone was warned about.
pub async fn warned_among(
    db: impl PgExecutor<'_>,
    kind: SubjectKind,
    ids: &[i64],
) -> sqlx::Result<Vec<i64>> {
    sqlx::query_scalar(
        "SELECT DISTINCT subject_id FROM user_warnings
         WHERE subject_kind = $1 AND subject_id = ANY($2)",
    )
    .bind(kind.as_str())
    .bind(ids)
    .fetch_all(db)
    .await
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn warnings_about_what_was_written(pool: PgPool) {
        let alice: i64 = sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT 'alice', id FROM roles WHERE system_key = 'member' RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        create(
            &pool,
            alice,
            None,
            "be nice",
            Some((SubjectKind::Comment, 5)),
        )
        .await
        .unwrap();
        create(&pool, alice, None, "spam", None).await.unwrap();
        let found = for_user(&pool, alice).await.unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].reason, "spam");
        assert_eq!(
            warned_among(&pool, SubjectKind::Comment, &[4, 5])
                .await
                .unwrap(),
            [5]
        );
        assert!(
            warned_among(&pool, SubjectKind::ForumPost, &[5])
                .await
                .unwrap()
                .is_empty()
        );
    }
}
