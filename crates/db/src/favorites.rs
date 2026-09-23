//! Favorites and votes. Triggers keep `posts.fav_count` and `posts.score`
//! in step with these tables.

use sqlx::PgExecutor;

/// Adds a favorite; false if it was already there.
pub async fn add(db: impl PgExecutor<'_>, user_id: i64, post_id: i64) -> sqlx::Result<bool> {
    let result = sqlx::query(
        "INSERT INTO favorites (user_id, post_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(user_id)
    .bind(post_id)
    .execute(db)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Removes a favorite; false if there was none.
pub async fn remove(db: impl PgExecutor<'_>, user_id: i64, post_id: i64) -> sqlx::Result<bool> {
    let result = sqlx::query("DELETE FROM favorites WHERE user_id = $1 AND post_id = $2")
        .bind(user_id)
        .bind(post_id)
        .execute(db)
        .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn exists(db: impl PgExecutor<'_>, user_id: i64, post_id: i64) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM favorites WHERE user_id = $1 AND post_id = $2)",
    )
    .bind(user_id)
    .bind(post_id)
    .fetch_one(db)
    .await
}

/// How many posts a user has favorited.
pub async fn count_by_user(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT count(*) FROM favorites WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(db)
        .await
}

/// Sets a user's vote on a post: `1`, `-1`, or `0` to take it back.
pub async fn vote(
    db: impl PgExecutor<'_>,
    user_id: i64,
    post_id: i64,
    score: i16,
) -> sqlx::Result<()> {
    if score == 0 {
        sqlx::query("DELETE FROM post_votes WHERE user_id = $1 AND post_id = $2")
            .bind(user_id)
            .bind(post_id)
            .execute(db)
            .await?;
    } else {
        // Only a changed vote touches the row, so the score trigger runs
        // only when the score changes.
        sqlx::query(
            "INSERT INTO post_votes (user_id, post_id, score) VALUES ($1, $2, $3)
             ON CONFLICT (user_id, post_id) DO UPDATE SET score = EXCLUDED.score, created_at = now()
             WHERE post_votes.score <> EXCLUDED.score",
        )
        .bind(user_id)
        .bind(post_id)
        .bind(score.signum())
        .execute(db)
        .await?;
    }
    Ok(())
}

/// A user's vote on a post: `1`, `-1` or `0`.
pub async fn vote_of(db: impl PgExecutor<'_>, user_id: i64, post_id: i64) -> sqlx::Result<i16> {
    let score: Option<i16> =
        sqlx::query_scalar("SELECT score FROM post_votes WHERE user_id = $1 AND post_id = $2")
            .bind(user_id)
            .bind(post_id)
            .fetch_optional(db)
            .await?;
    Ok(score.unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    async fn user(pool: &PgPool, name: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT $1, id FROM roles WHERE system_key = 'member' RETURNING id",
        )
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn counts(pool: &PgPool, post: i64) -> (i32, i32) {
        sqlx::query_as("SELECT fav_count, score FROM posts WHERE id = $1")
            .bind(post)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn counts_follow_favorites_and_votes(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let bob = user(&pool, "bob").await;
        let post: i64 = sqlx::query_scalar("INSERT INTO posts (rating) VALUES ('g') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();

        assert!(add(&pool, alice, post).await.unwrap());
        assert!(!add(&pool, alice, post).await.unwrap());
        assert!(add(&pool, bob, post).await.unwrap());
        assert!(exists(&pool, bob, post).await.unwrap());
        assert!(remove(&pool, bob, post).await.unwrap());
        assert!(!remove(&pool, bob, post).await.unwrap());
        assert_eq!(count_by_user(&pool, alice).await.unwrap(), 1);

        vote(&pool, alice, post, 1).await.unwrap();
        vote(&pool, alice, post, 1).await.unwrap();
        vote(&pool, bob, post, -1).await.unwrap();
        assert_eq!(counts(&pool, post).await, (1, 0));
        vote(&pool, bob, post, 1).await.unwrap();
        assert_eq!(counts(&pool, post).await, (1, 2));
        vote(&pool, alice, post, 0).await.unwrap();
        assert_eq!(counts(&pool, post).await, (1, 1));
        assert_eq!(vote_of(&pool, bob, post).await.unwrap(), 1);
        assert_eq!(vote_of(&pool, alice, post).await.unwrap(), 0);

        // Deleting a user takes their favorites and votes with them.
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(bob)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(counts(&pool, post).await, (1, 0));
        // And deleting the post cascades without tripping the triggers.
        sqlx::query("DELETE FROM posts WHERE id = $1")
            .bind(post)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(count_by_user(&pool, alice).await.unwrap(), 0);
    }
}
