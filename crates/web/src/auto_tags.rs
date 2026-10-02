//! The site's automatic tags ([`moekura_core::auto_tags`]), applied to a
//! post's tags as an upload or edit saves them.

use moekura_core::auto_tags::{self, FileFacts};
use moekura_core::file_traits::FileTrait;
use moekura_core::settings::SiteSettings;
use moekura_db::media::Asset;
use moekura_db::tags::{self, Tag, WantedTag};
use sqlx::PgConnection;

/// `asset`'s facts, for the rules; `traits` holds its traits read.
pub(crate) fn facts<'a>(asset: &'a Asset, traits: &'a [FileTrait]) -> FileFacts<'a> {
    FileFacts {
        width: asset.width,
        height: asset.height,
        media_type: &asset.media_type,
        frames: asset.frames,
        has_audio: asset.has_audio,
        traits,
    }
}

/// `tags` (a post's, sorted by id) with the site's automatic tags added
/// and removed, for its file (`None` when it has none) and `source`.
/// Unchanged when the site doesn't use them.
pub(crate) async fn with_automatic_tags(
    conn: &mut PgConnection,
    settings: &SiteSettings,
    file: Option<&FileFacts<'_>>,
    source: &str,
    mut tags: Vec<Tag>,
) -> sqlx::Result<Vec<Tag>> {
    let names: Vec<String> = tags.iter().map(|t| t.name.clone()).collect();
    let changes = auto_tags::changes(
        &settings.automatic_tags,
        file,
        source,
        &names,
        settings.request_tags,
    );
    if changes.add.is_empty() && changes.remove.is_empty() {
        return Ok(tags);
    }
    tags.retain(|t| !changes.remove.contains(&t.name));
    if !changes.add.is_empty() {
        let categories = tags::categories(&mut *conn).await?;
        let wanted: Vec<WantedTag<'_>> = changes
            .add
            .iter()
            .map(|(name, rule)| WantedTag {
                name,
                category_id: categories
                    .iter()
                    .find(|c| c.name == rule.category())
                    .map(|c| c.id),
            })
            .collect();
        tags.extend(tags::ensure(conn, &wanted, false).await?);
    }
    tags.sort_by_key(|t| t.id);
    tags.dedup_by_key(|t| t.id);
    Ok(tags)
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, fixture, session_for, test_state};

    async fn names(pool: &PgPool, id: i64) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT t.name FROM posts p JOIN tags t ON t.id = ANY(p.tag_ids)
             WHERE p.id = $1 ORDER BY t.name",
        )
        .bind(id)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn posts_get_tags_from_their_files_and_sources(pool: PgPool) {
        moekura_db::settings::set(
            &pool,
            "automatic_tags",
            serde_json::json!({ "enabled": true, "tags": { "bad_link": "bad_twitter_link" } }),
        )
        .await
        .unwrap();
        let state = test_state(&pool).await;
        let max = state.config.media.max_upload_mb * 1024 * 1024;
        let routes = crate::edit::routes()
            .merge(crate::posts::routes())
            .merge(crate::upload::routes(max));
        let app = TestApp::new(state, routes);
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let fields = vec![
            ("rating", "s".to_owned()),
            ("tags", "cat highres animated".to_owned()),
            (
                "source",
                "https://pbs.twimg.com/media/EBGbJe_U8AA4Ekb.jpg".to_owned(),
            ),
        ];
        let posted = app
            .post_multipart(
                "/upload",
                Some(&alice),
                &fields,
                Some(("a.png", &fixture::png(320, 240))),
            )
            .await;
        assert_eq!(posted.status, StatusCode::SEE_OTHER, "{}", posted.body);
        let id: i64 = posted.location.unwrap()["/posts/".len()..]
            .split('?')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        // Tags the file contradicts went; what it and the source say came.
        assert_eq!(
            names(&pool, id).await,
            ["bad_twitter_link", "cat", "lowres"]
        );
        let category: i16 =
            sqlx::query_scalar("SELECT category_id FROM tags WHERE name = 'lowres'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(category, 5, "meta");

        // Edits keep to the file, and follow the source.
        let edit = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("old_tags", "bad_twitter_link cat lowres")
            .append_pair("tags", "cat dog highres")
            .append_pair("rating", "s")
            .append_pair("source", "https://x.com/someone/status/1")
            .finish();
        let edited = app
            .post_form(&format!("/posts/{id}/edit"), Some(&alice), &[], &edit)
            .await;
        assert_eq!(edited.status, StatusCode::SEE_OTHER, "{}", edited.body);
        assert_eq!(names(&pool, id).await, ["cat", "dog", "lowres"]);
    }
}
