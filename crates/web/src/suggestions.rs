//! The tagger's suggestions, on the post edit form and on the post form
//! of a file being uploaded.

use minijinja::{Value, context};
use moekura_core::jobs::TagStaged;
use moekura_core::post_edit;
use moekura_core::posts::Rating;
use moekura_db::posts::Post;
use moekura_db::tag_suggestions::{self, Suggestion};
use moekura_db::tags::Category;
use sqlx::{PgConnection, PgPool};

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
    let suggested = tag_suggestions::for_post(db, post.id).await?;
    let tags = offered(state, categories, suggested, |s| {
        post.tag_ids.contains(&s.tag_id)
    });
    let rating = (result.rating != post.rating)
        .then(|| rating_context(result.rating, result.rating_confidence));
    Ok(shown(tags, rating))
}

/// Queues staged file `staged_id` for the tagger, when it's on, so its
/// post form can show suggestions.
pub(crate) async fn queue_staged(
    state: &AppState,
    conn: &mut PgConnection,
    staged_id: i64,
) -> sqlx::Result<()> {
    if state.config.tagger.enabled {
        moekura_db::jobs::enqueue(conn, &TagStaged { staged_id }).await?;
    }
    Ok(())
}

/// What the post form of staged file `staged_id` shows of the tagger's
/// suggestions: tags over their category's threshold that aren't in the
/// form's `tags` yet, best first, and a rating other than the form's.
/// Pending until the tagger has seen the file; `None` without the tagger
/// or when there is nothing to show.
pub(crate) async fn for_upload_form(
    state: &AppState,
    db: &PgPool,
    staged_id: i64,
    tags: &str,
    rating: &str,
) -> sqlx::Result<Option<Value>> {
    if !state.config.tagger.enabled {
        return Ok(None);
    }
    let Some(result) = tag_suggestions::staged_result(db, staged_id).await? else {
        return Ok(Some(context! { pending => true }));
    };
    let categories = moekura_db::tags::categories(db).await?;
    let prefixes: Vec<&str> = categories.iter().map(|c| c.name.as_str()).collect();
    let typed = post_edit::parse(tags, &prefixes, &[]).tags;
    let tags = offered(state, &categories, result.suggestions, |s| {
        typed.iter().any(|t| t.name.as_str() == s.name)
    });
    let rating = (result.rating.code() != rating)
        .then(|| rating_context(result.rating, result.rating_confidence));
    Ok(shown(tags, rating))
}

/// `suggested` tags over their category's threshold, leaving out those
/// `has` says are there already, for templates.
fn offered(
    state: &AppState,
    categories: &[Category],
    suggested: Vec<Suggestion>,
    has: impl Fn(&Suggestion) -> bool,
) -> Vec<Value> {
    let settings = &state.site.get().settings.tagger;
    let category = |id: i16| {
        categories
            .iter()
            .find(|c| c.id == id)
            .map_or("general", |c| c.name.as_str())
    };
    suggested
        .into_iter()
        .filter(|s| !has(s) && s.confidence >= settings.threshold(category(s.category_id)))
        .map(|s| {
            context! {
                name => s.name,
                category => category(s.category_id),
                confidence => percent(s.confidence),
            }
        })
        .collect()
}

fn rating_context(rating: Rating, confidence: f32) -> Value {
    context! {
        code => rating.code(),
        label => rating.label(),
        confidence => percent(confidence),
    }
}

/// The suggestions box, unless there is nothing in it.
fn shown(tags: Vec<Value>, rating: Option<Value>) -> Option<Value> {
    if tags.is_empty() && rating.is_none() {
        return None;
    }
    Some(context! {
        pending => false,
        tags => tags,
        rating => rating,
    })
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
            .merge(crate::upload::routes(max))
            .merge(crate::uploads::routes(max));
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
            .split('?')
            .next()
            .unwrap()
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
    async fn upload_form_offers_suggestions(pool: PgPool) {
        let app = app(&pool, true).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let sent = app
            .post_multipart_files(
                "/uploads",
                Some(&alice),
                &[],
                &[("file", "a.png", &fixture::png(20, 20))],
            )
            .await;
        let upload: i64 = sent.location.unwrap()["/uploads/".len()..].parse().unwrap();
        let staged: i64 = sqlx::query_scalar("SELECT id FROM staged_uploads")
            .fetch_one(&pool)
            .await
            .unwrap();
        // Queued for the tagger...
        let queued: Vec<serde_json::Value> =
            sqlx::query_scalar("SELECT payload FROM jobs WHERE kind = 'ml.tag_staged'")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(queued, [serde_json::json!({ "staged_id": staged })]);
        // ...and waited for, without holding up posting.
        let form = format!("/uploads/{upload}/assets/{staged}");
        let page = app.get(&form, Some(&alice)).await.body;
        assert!(
            page.contains("The tagger is looking at this file"),
            "{page}"
        );
        assert!(page.contains(&format!("data-suggestions-poll=\"{form}/suggestions\"")));
        let fragment = app.get(&format!("{form}/suggestions"), Some(&alice)).await;
        assert!(fragment.body.contains("data-suggestions-pending"));
        assert!(!fragment.body.contains("<html"), "{}", fragment.body);
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let other = app.get(&format!("{form}/suggestions"), Some(&bob)).await;
        assert_eq!(other.status, StatusCode::NOT_FOUND);

        // The tagger's verdict.
        let mut conn = pool.acquire().await.unwrap();
        let wanted = ["cat", "whiskers", "maybe"].map(|name| WantedTag {
            name,
            category_id: Some(0),
        });
        let found = tags::ensure(&mut conn, &wanted, false).await.unwrap();
        let id = |name: &str| found.iter().find(|t| t.name == name).unwrap().id;
        let suggested = [(id("cat"), 0.9), (id("whiskers"), 0.8), (id("maybe"), 0.2)];
        moekura_db::tag_suggestions::save_staged(
            &mut *conn,
            staged,
            "test",
            Rating::Sensitive,
            0.7,
            &suggested,
        )
        .await
        .unwrap();
        let offered = |body: &str| -> Vec<String> {
            body.match_indices("name=\"add\" value=\"")
                .map(|(at, found)| {
                    let rest = &body[at + found.len()..];
                    rest[..rest.find('"').unwrap()].to_owned()
                })
                .collect()
        };
        let page = app.get(&form, Some(&alice)).await.body;
        assert!(!page.contains("data-suggestions-poll"), "{page}");
        assert_eq!(offered(&page), ["cat", "whiskers"]);
        assert!(page.contains("value=\"s\""), "{page}");
        let fragment = app.get(&format!("{form}/suggestions"), Some(&alice)).await;
        assert_eq!(offered(&fragment.body), ["cat", "whiskers"]);

        // Without scripts, a click adds the tag to the form, which comes
        // back without it on offer, and posts nothing.
        let fields = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs([("tags", "artist:someone cat"), ("add", "whiskers")])
            .finish();
        let response = app.post_form(&form, Some(&alice), &[], &fields).await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        assert!(
            response
                .body
                .contains(">artist:someone cat whiskers </textarea>"),
            "{}",
            response.body
        );
        assert!(offered(&response.body).is_empty());
        let fields = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs([("tags", "cat"), ("rating", "g"), ("suggested_rating", "s")])
            .finish();
        let response = app.post_form(&form, Some(&alice), &[], &fields).await;
        assert!(
            response.body.contains("value=\"s\" required checked"),
            "{}",
            response.body
        );
        assert!(!response.body.contains("name=\"suggested_rating\""));
        let posts: i64 = sqlx::query_scalar("SELECT count(*) FROM posts")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(posts, 0);

        // Posting it leaves suggestions to the post.
        let fields = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs([("tags", "cat whiskers"), ("rating", "s")])
            .finish();
        let response = app.post_form(&form, Some(&alice), &[], &fields).await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        let page = app.get(&form, Some(&alice)).await.body;
        assert!(!page.contains("class=\"suggestions\""), "{page}");
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
