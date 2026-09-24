//! The moderation audit log.

use moekura_core::moderation::ActionKind;
use serde_json::Value;
use sqlx::PgExecutor;
use time::OffsetDateTime;

/// An action to record.
#[derive(Debug, Clone)]
pub struct NewAction<'a> {
    /// `None` for the command line.
    pub actor_id: Option<i64>,
    pub kind: ActionKind,
    pub post_id: Option<i64>,
    pub user_id: Option<i64>,
    pub reason: &'a str,
    pub details: Value,
}

impl<'a> NewAction<'a> {
    pub fn new(actor_id: Option<i64>, kind: ActionKind) -> Self {
        Self {
            actor_id,
            kind,
            post_id: None,
            user_id: None,
            reason: "",
            details: Value::Object(Default::default()),
        }
    }

    pub fn post(mut self, post_id: i64) -> Self {
        self.post_id = Some(post_id);
        self
    }

    pub fn user(mut self, user_id: i64) -> Self {
        self.user_id = Some(user_id);
        self
    }

    pub fn reason(mut self, reason: &'a str) -> Self {
        self.reason = reason;
        self
    }

    pub fn details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }
}

/// Records an action; call it in the transaction making the change.
pub async fn record(db: impl PgExecutor<'_>, action: NewAction<'_>) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO mod_actions (actor_id, action, post_id, user_id, reason, details)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(action.actor_id)
    .bind(action.kind.as_str())
    .bind(action.post_id)
    .bind(action.user_id)
    .bind(action.reason)
    .bind(&action.details)
    .execute(db)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Entry {
    pub id: i64,
    pub actor_name: Option<String>,
    pub action: String,
    pub post_id: Option<i64>,
    pub user_id: Option<i64>,
    pub user_name: Option<String>,
    pub reason: String,
    pub details: Value,
    pub created_at: OffsetDateTime,
}

/// What to show of the log.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub action: Option<ActionKind>,
    pub actor_id: Option<i64>,
    pub post_id: Option<i64>,
    pub user_id: Option<i64>,
    /// Entries older than this id (keyset pagination).
    pub before: Option<i64>,
}

/// Newest first.
pub async fn list(
    db: impl PgExecutor<'_>,
    filter: &Filter,
    limit: i64,
) -> sqlx::Result<Vec<Entry>> {
    sqlx::query_as(
        "SELECT m.id, a.name::text AS actor_name, m.action, m.post_id, m.user_id,
                u.name::text AS user_name, m.reason, m.details, m.created_at
         FROM mod_actions m
         LEFT JOIN users a ON a.id = m.actor_id
         LEFT JOIN users u ON u.id = m.user_id
         WHERE ($1::text IS NULL OR m.action = $1)
           AND ($2::bigint IS NULL OR m.actor_id = $2)
           AND ($3::bigint IS NULL OR m.post_id = $3)
           AND ($4::bigint IS NULL OR m.user_id = $4)
           AND ($5::bigint IS NULL OR m.id < $5)
         ORDER BY m.id DESC LIMIT $6",
    )
    .bind(filter.action.map(ActionKind::as_str))
    .bind(filter.actor_id)
    .bind(filter.post_id)
    .bind(filter.user_id)
    .bind(filter.before)
    .bind(limit)
    .fetch_all(db)
    .await
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sqlx::PgPool;

    use super::*;

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn records_and_filters(pool: PgPool) {
        let moderator: i64 = sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT 'mod', id FROM roles WHERE system_key = 'moderator' RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        record(
            &pool,
            NewAction::new(Some(moderator), ActionKind::PostDelete)
                .post(7)
                .reason("duplicate"),
        )
        .await
        .unwrap();
        record(
            &pool,
            NewAction::new(None, ActionKind::SettingUpdate)
                .details(json!({ "key": "site_name", "value": "x" })),
        )
        .await
        .unwrap();

        let all = list(&pool, &Filter::default(), 10).await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].action, "setting.update");
        assert_eq!(all[0].actor_name, None);
        assert_eq!(all[1].actor_name.as_deref(), Some("mod"));
        let deletes = Filter {
            action: Some(ActionKind::PostDelete),
            ..Filter::default()
        };
        assert_eq!(
            list(&pool, &deletes, 10).await.unwrap()[0].reason,
            "duplicate"
        );
        let older = Filter {
            before: Some(all[0].id),
            ..Filter::default()
        };
        assert_eq!(list(&pool, &older, 10).await.unwrap().len(), 1);
        let by_post = Filter {
            post_id: Some(8),
            ..Filter::default()
        };
        assert!(list(&pool, &by_post, 10).await.unwrap().is_empty());
    }
}
