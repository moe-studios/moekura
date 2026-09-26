//! The tagger's suggestions, on the post edit form.

use minijinja::{Value, context};
use moekura_db::posts::Post;
use moekura_db::tag_suggestions;
use moekura_db::tags::Category;
use sqlx::PgPool;

use crate::AppState;

/// What the edit form shows of the tagger's suggestions: tags the post
/// lacks that are over their category's threshold, best first, and a
/// rating other than the post's. `None` when there is nothing to show.
pub(crate) async fn for_edit_form(
    state: &AppState,
    db: &PgPool,
    post: &Post,
    categories: &[Category],
) -> sqlx::Result<Option<Value>> {
    let Some(result) = tag_suggestions::result(db, post.id).await? else {
        return Ok(state
            .config
            .tagger
            .enabled
            .then(|| context! { pending => true }));
    };
    let settings = &state.site.get().settings.tagger;
    let category = |id: i16| {
        categories
            .iter()
            .find(|c| c.id == id)
            .map_or("general", |c| c.name.as_str())
    };
    let tags: Vec<Value> = tag_suggestions::for_post(db, post.id)
        .await?
        .into_iter()
        .filter(|s| {
            !post.tag_ids.contains(&s.tag_id)
                && s.confidence >= settings.threshold(category(s.category_id))
        })
        .map(|s| {
            context! {
                name => s.name,
                category => category(s.category_id),
                confidence => percent(s.confidence),
            }
        })
        .collect();
    let rating = (result.rating != post.rating).then(|| {
        context! {
            code => result.rating.code(),
            label => result.rating.label(),
            confidence => percent(result.rating_confidence),
        }
    });
    if tags.is_empty() && rating.is_none() {
        return Ok(None);
    }
    Ok(Some(context! {
        pending => false,
        tags => tags,
        rating => rating,
    }))
}

fn percent(confidence: f32) -> u32 {
    (confidence * 100.0).round().clamp(0.0, 100.0) as u32
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use moekura_core::posts::Rating;
    use moekura_db::tag_suggestions::NewResult;
    use moekura_db::tags::{self, WantedTag};
    use sqlx::PgPool;

    use crate::test_support::{TestApp, fixture, session_for, test_config, test_state_with};

    async fn app(pool: &PgPool, tagger: bool) -> TestApp {
        let mut config = test_config();
        config.tagger.enabled = tagger;
        let state = test_state_with(pool, config).await;
        let max = state.config.media.max_upload_mb * 1024 * 1024;
        let routes = crate::edit::routes()
            .merge(crate::posts::routes())
            .merge(crate::upload::routes(max));
        TestApp::new(state, routes)
    }

    async fn upload(app: &TestApp, session: &str, tags: &str) -> i64 {
        let fields = vec![("rating", "g".to_owned()), ("tags", tags.to_owned())];
        let response = app
            .post_multipart(
                "/upload",
                Some(session),
                &fields,
                Some(("a.png", &fixture::png(20, 20))),
            )
            .await;
        response.location.unwrap()["/posts/".len()..]
            .parse()
            .unwrap()
    }

    /// The tagger's verdict: `rating`, and `tags` with confidences.
    async fn suggest(pool: &PgPool, post_id: i64, rating: Rating, suggested: &[(&str, f32)]) {
        let mut conn = pool.acquire().await.unwrap();
        let wanted: Vec<WantedTag<'_>> = suggested
            .iter()
            .map(|(name, _)| WantedTag {
                name,
                category_id: Some(if name.contains("miku") { 4 } else { 0 }),
            })
            .collect();
        let found = tags::ensure(&mut conn, &wanted, false).await.unwrap();
        let suggestions: Vec<(i32, f32)> = suggested
            .iter()
            .map(|(name, confidence)| {
                (
                    found.iter().find(|t| t.name == *name).unwrap().id,
                    *confidence,
                )
            })
            .collect();
        moekura_db::tag_suggestions::save(
            &mut conn,
            &NewResult {
                post_id,
                model: "test",
                rating,
                rating_confidence: 0.7,
                suggestions: &suggestions,
            },
        )
        .await
        .unwrap();
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn edit_form_offers_suggestions(pool: PgPool) {
        let app = app(&pool, true).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let id = upload(&app, &alice, "cat").await;

        // Not tagged yet.
        let page = app.get(&format!("/posts/{id}"), Some(&alice)).await;
        assert!(
            page.body.contains("hasn't looked at this post"),
            "{}",
            page.body
        );

        suggest(
            &pool,
            id,
            Rating::Sensitive,
            &[
                ("cat", 0.99),
                ("whiskers", 0.8),
                ("hatsune_miku", 0.9),
                ("rin", 0.3),
                ("maybe", 0.2),
            ],
        )
        .await;
        let page = app.get(&format!("/posts/{id}"), Some(&alice)).await;
        // Not the tag the post has, nor those under their category's
        // threshold (35%, characters 85%).
        let offered: Vec<&str> = page
            .body
            .match_indices("name=\"add\" value=\"")
            .map(|(at, found)| {
                let rest = &page.body[at + found.len()..];
                &rest[..rest.find('"').unwrap()]
            })
            .collect();
        assert_eq!(offered, ["hatsune_miku", "whiskers"]);
        assert!(page.body.contains("value=\"s\""), "{}", page.body);
        assert!(page.body.contains("70%"));
        // Visitors, who can't edit, see none.
        let page = app.get(&format!("/posts/{id}"), None).await;
        assert!(!page.body.contains("name=\"add\""));

        // One click adds the tag (and keeps the rest of the form).
        let form = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs([
                ("old_tags", "cat"),
                ("tags", "cat"),
                ("rating", "g"),
                ("add", "whiskers"),
            ])
            .finish();
        let response = app
            .post_form(&format!("/posts/{id}/edit"), Some(&alice), &[], &form)
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        let post = moekura_db::posts::by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(post.tag_ids.len(), 2);
        assert_eq!(post.rating, Rating::General);

        // And so does the suggested rating.
        let form = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs([
                ("old_tags", "cat whiskers"),
                ("tags", "cat whiskers"),
                ("rating", "g"),
                ("suggested_rating", "s"),
            ])
            .finish();
        let response = app
            .post_form(&format!("/posts/{id}/edit"), Some(&alice), &[], &form)
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        let post = moekura_db::posts::by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(post.rating, Rating::Sensitive);
        let page = app.get(&format!("/posts/{id}"), Some(&alice)).await;
        assert!(!page.body.contains("name=\"suggested_rating\""));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn nothing_shows_without_the_tagger(pool: PgPool) {
        let app = app(&pool, false).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let id = upload(&app, &alice, "cat").await;
        let page = app.get(&format!("/posts/{id}"), Some(&alice)).await;
        assert!(
            !page.body.contains("class=\"suggestions\""),
            "{}",
            page.body
        );
    }
}
