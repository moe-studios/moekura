//! Warnings about incomplete tagging, shown on the post page after an
//! upload or edit (`?check=1`) without stopping the save: no artist,
//! copyright or character tag, few general tags, tags no other post has
//! (a typo?), and category prefixes that couldn't apply to an existing
//! tag (`kept=`, found when saving).
//!
//! Also the site's optional request tags (`request_tags`): `artist_request`
//! while a post has no artist tag, `tagme` while it has few general tags.

use minijinja::{Value, context};
use moekura_core::posts::PostStatus;
use moekura_core::tags::parse_input;
use moekura_db::posts::Post;
use moekura_db::tags::{self, Category, Tag, WantedTag};
use sqlx::{PgConnection, PgPool};

use crate::templates::search_url;

/// General tags a well-tagged post has at least.
pub(crate) const MIN_GENERAL_TAGS: usize = 10;

/// Added while a post has no artist tag, if the site wants.
pub(crate) const ARTIST_REQUEST: &str = "artist_request";
/// Added while a post has fewer than [`MIN_GENERAL_TAGS`] general tags.
pub(crate) const TAGME: &str = "tagme";

fn category_id(categories: &[Category], name: &str) -> Option<i16> {
    categories.iter().find(|c| c.name == name).map(|c| c.id)
}

/// Tags typed with a category prefix (in `input`) that post `post` has
/// in another category: existing tags keep theirs unless the editor may
/// manage tags. As `name:category` words, for the `kept` parameter.
pub(crate) async fn kept_categories(
    db: &PgPool,
    input: &str,
    post_id: i64,
) -> sqlx::Result<Vec<String>> {
    let categories = tags::categories(db).await?;
    let names: Vec<&str> = categories.iter().map(|c| c.name.as_str()).collect();
    let (typed, _) = parse_input(input, &names);
    let Some(post) = moekura_db::posts::by_id(db, post_id).await? else {
        return Ok(Vec::new());
    };
    let on_post = tags::by_ids(db, &post.tag_ids).await?;
    Ok(typed
        .into_iter()
        .filter_map(|t| {
            let category = t.category?;
            let wanted = category_id(&categories, &category)?;
            let tag = on_post.iter().find(|p| p.name == t.name.as_str())?;
            (tag.category_id != wanted).then(|| format!("{}:{category}", tag.name))
        })
        .collect())
}

/// The query string for the post page after a save: `check=1`, and
/// `kept` when some prefixes didn't apply.
pub(crate) fn check_query(kept: &[String]) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("check", "1");
    if !kept.is_empty() {
        query.append_pair("kept", &kept.join(" "));
    }
    query.finish()
}

/// The warnings for `post`, with its tags; `kept` is the page's `kept`
/// parameter, trusted only as far as the post bears it out.
pub(crate) fn warnings(
    post: &Post,
    post_tags: &[Tag],
    categories: &[Category],
    kept: &str,
) -> Vec<Value> {
    let mut out = Vec::new();
    let has = |name: &str| {
        category_id(categories, name).is_none_or(|id| post_tags.iter().any(|t| t.category_id == id))
    };
    for (name, what) in [
        ("artist", "an artist tag, or artist_unknown"),
        ("copyright", "a copyright tag, or original"),
        ("character", "a character tag, if anyone appears in it"),
    ] {
        if !has(name) {
            out.push(context! { text => format!("It has no {name} tag: add {what}.") });
        }
    }
    let general = category_id(categories, "general").map_or(0, |id| {
        post_tags
            .iter()
            .filter(|t| t.category_id == id && t.name != TAGME)
            .count()
    });
    if general < MIN_GENERAL_TAGS {
        out.push(context! {
            text => format!(
                "It has {general} general tag{}; well-tagged posts have at least {MIN_GENERAL_TAGS}.",
                if general == 1 { "" } else { "s" }
            ),
        });
    }
    // Only this post has them (or none yet, when it isn't counted).
    let counted = matches!(post.status, PostStatus::Active | PostStatus::Flagged);
    let mut new: Vec<&Tag> = post_tags
        .iter()
        .filter(|t| t.post_count <= i32::from(counted))
        .collect();
    new.sort_by(|a, b| a.name.cmp(&b.name));
    if !new.is_empty() {
        out.push(context! {
            text => "No other post has these tags yet; check their spelling:",
            tags => new.iter().map(|t| context! {
                name => t.name,
                url => Value::from_safe_string(search_url(&t.name)),
            }).collect::<Vec<_>>(),
        });
    }
    for word in kept.split_whitespace() {
        let Some((name, wanted)) = word.rsplit_once(':') else {
            continue;
        };
        let Some(tag) = post_tags.iter().find(|t| t.name == name) else {
            continue;
        };
        let Some(asked) = categories.iter().find(|c| c.name == wanted) else {
            continue;
        };
        if tag.category_id == asked.id {
            continue;
        }
        let actual = categories
            .iter()
            .find(|c| c.id == tag.category_id)
            .map_or("general", |c| c.name.as_str());
        out.push(context! {
            text => format!(
                "{name} is already a{} {actual} tag, so {wanted}: didn't change it; ask someone who manages tags if it's wrong.",
                if actual.starts_with(['a', 'e', 'i', 'o', 'u']) { "n" } else { "" }
            ),
        });
    }
    out
}

/// `tags` (a post's, sorted by id) with the request tags the site wants:
/// [`ARTIST_REQUEST`] while there's no artist tag and [`TAGME`] while
/// there are few general tags, each taken off again once it no longer
/// applies. Unchanged when the site doesn't use request tags.
pub(crate) async fn with_request_tags(
    conn: &mut PgConnection,
    enabled: bool,
    mut tags: Vec<Tag>,
) -> sqlx::Result<Vec<Tag>> {
    if !enabled {
        return Ok(tags);
    }
    let categories = tags::categories(&mut *conn).await?;
    let artist = category_id(&categories, "artist");
    let general = category_id(&categories, "general");
    let meta = category_id(&categories, "meta");
    let requests = [ARTIST_REQUEST, TAGME];
    let wanted = [
        !tags.iter().any(|t| Some(t.category_id) == artist),
        tags.iter()
            .filter(|t| Some(t.category_id) == general && !requests.contains(&t.name.as_str()))
            .count()
            < MIN_GENERAL_TAGS,
    ];
    for (name, wanted) in requests.into_iter().zip(wanted) {
        let present = tags.iter().any(|t| t.name == name);
        if wanted && !present {
            let found = tags::ensure(
                conn,
                &[WantedTag {
                    name,
                    category_id: meta,
                }],
                false,
            )
            .await?;
            tags.extend(found);
        } else if !wanted && present {
            tags.retain(|t| t.name != name);
        }
    }
    tags.sort_by_key(|t| t.id);
    Ok(tags)
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, fixture, session_for, test_state};

    async fn app(pool: &PgPool) -> TestApp {
        let state = test_state(pool).await;
        let max = state.config.media.max_upload_mb * 1024 * 1024;
        let routes = crate::edit::routes()
            .merge(crate::posts::routes())
            .merge(crate::upload::routes(max));
        TestApp::new(state, routes)
    }

    async fn upload(app: &TestApp, session: &str, width: u32, tags: &str) -> String {
        let fields = vec![("rating", "s".to_owned()), ("tags", tags.to_owned())];
        let response = app
            .post_multipart(
                "/upload",
                Some(session),
                &fields,
                Some(("a.png", &fixture::png(width, 20))),
            )
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        response.location.unwrap()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn warns_after_saving(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        upload(&app, &alice, 20, "cat artist:someone").await;
        // `cat` is an existing general tag; the prefix can't move it.
        let location = upload(&app, &alice, 24, "artist:cat catt").await;
        assert!(
            location.contains("?check=1&kept=cat%3Aartist"),
            "{location}"
        );
        let page = app.get(&location, Some(&alice)).await.body;
        for warning in [
            "no artist tag",
            "no copyright tag",
            "no character tag",
            "It has 2 general tags",
            "check their spelling",
            ">catt</a>",
            "cat is already a general tag, so artist: didn&#x27;t change it",
        ] {
            assert!(page.contains(warning), "{warning}: {page}");
        }
        assert!(!page.contains(">cat</a>, "), "cat is on another post");
        // Only after saving, and only for editors.
        let id = location.split('?').next().unwrap();
        assert!(
            !app.get(id, Some(&alice))
                .await
                .body
                .contains("tag-warnings")
        );
        assert!(!app.get(&location, None).await.body.contains("tag-warnings"));
        // A made-up `kept` doesn't show.
        let forged = format!("{id}?check=1&kept=dog%3Aartist+catt%3Anope+catt%3Ageneral");
        assert!(
            !app.get(&forged, Some(&alice))
                .await
                .body
                .contains("didn&#x27;t")
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn request_tags_come_and_go(pool: PgPool) {
        moekura_db::settings::set(&pool, "request_tags", serde_json::json!(true))
            .await
            .unwrap();
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let location = upload(&app, &alice, 20, "cat").await;
        let id: i64 = location["/posts/".len()..]
            .split('?')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let names = || async {
            sqlx::query_scalar::<_, String>(
                "SELECT t.name FROM posts p JOIN tags t ON t.id = ANY(p.tag_ids)
                 WHERE p.id = $1 ORDER BY t.name",
            )
            .bind(id)
            .fetch_all(&pool)
            .await
            .unwrap()
        };
        assert_eq!(names().await, ["artist_request", "cat", "tagme"]);
        let meta: i16 = sqlx::query_scalar("SELECT category_id FROM tags WHERE name = 'tagme'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(meta, 5);
        let many: Vec<String> = (0..10).map(|i| format!("g{i}")).collect();
        let edit = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("old_tags", "artist_request cat tagme")
            .append_pair(
                "tags",
                &format!("artist_request cat tagme artist:someone {}", many.join(" ")),
            )
            .append_pair("rating", "s")
            .finish();
        let response = app
            .post_form(&format!("/posts/{id}/edit"), Some(&alice), &[], &edit)
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        let now = names().await;
        assert!(
            !now.contains(&"tagme".to_owned()) && !now.contains(&"artist_request".to_owned()),
            "{now:?}"
        );
    }
}
