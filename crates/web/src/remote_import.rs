//! Importing posts from other boorus through their APIs, for `moekura
//! admin import-remote`. Each file goes through the same checks and
//! steps as an upload; see [`moekura_core::remote`] for the sites.

use std::time::Duration;

use moekura_core::notes::NoteBox;
use moekura_core::pools::{Category, PoolName};
use moekura_core::remote::{self, Credentials, Cursor, Kind, RemotePost};
use moekura_db::remote_imports::{self, State};
use moekura_db::users::User;
use moekura_db::{media, notes, pools};

use crate::AppState;
use crate::auth::CurrentUser;
use crate::fetch::Fetcher;
use crate::upload::{TempWriter, UploadError, UploadFields, ingest};

/// What to import, and how.
#[derive(Debug, Clone)]
pub struct Options {
    pub kind: Kind,
    /// The site's base URL, like `https://danbooru.donmai.us`.
    pub base: String,
    pub tags: String,
    pub credentials: Credentials,
    /// Pause between requests to the site.
    pub delay: Duration,
    /// Stop after importing this many posts.
    pub limit: Option<u32>,
    pub notes: bool,
    pub pools: bool,
    /// Start from the newest posts again, rather than where the last run
    /// stopped.
    pub restart: bool,
    /// Allow the site to be on a private network (another Moekura on the
    /// LAN).
    pub allow_private: bool,
}

/// What happened to one post, for the progress report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Imported {
        remote: i64,
        post: i64,
    },
    Duplicate {
        remote: i64,
        post: i64,
    },
    /// The site doesn't show the file (hidden, or needs a login).
    NoFile {
        remote: i64,
    },
    Failed {
        remote: i64,
        reason: String,
    },
}

/// Talks to the other site.
struct Remote<'a> {
    options: &'a Options,
    client: reqwest::Client,
    fetcher: Fetcher,
}

impl Remote<'_> {
    async fn get(&self, url: &str) -> Result<String, String> {
        tokio::time::sleep(self.options.delay).await;
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("couldn't reach {url}: {e}"))?;
        let status = response.status();
        let body = response.text().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!(
                "{url} answered {status}: {}",
                body.chars().take(200).collect::<String>()
            ));
        }
        Ok(body)
    }
}

/// Runs an import, calling `report` for each post, and returns the
/// counts so far. Progress is saved after every page, so a stopped
/// import carries on from there next time.
pub async fn run(
    state: &AppState,
    uploader: &User,
    options: &Options,
    mut report: impl FnMut(&Outcome),
) -> Result<State, String> {
    let db = state.db.primary();
    let site = options.base.trim_end_matches('/').to_owned();
    let saved = remote_imports::state(db, &site, &options.tags)
        .await
        .map_err(|e| e.to_string())?;
    let mut progress = match saved {
        Some(saved) if !options.restart => saved,
        _ => State::default(),
    };
    if progress.finished && !options.restart {
        return Ok(progress);
    }
    let remote = Remote {
        options,
        client: moekura_net::client(
            Duration::from_secs(60),
            options.allow_private,
            reqwest::redirect::Policy::limited(5),
        ),
        fetcher: Fetcher::new(Duration::from_secs(300), options.allow_private),
    };
    let current = CurrentUser::for_user(uploader.clone(), None, &state.site.get());
    let mut cursor = progress.cursor.unwrap_or(Cursor::Start);
    let mut imported_now = 0;
    loop {
        let url = remote::page_url(
            options.kind,
            &site,
            &options.tags,
            cursor,
            &options.credentials,
        );
        let body = remote.get(&url).await?;
        let posts = remote::parse_page(options.kind, &site, &body)?;
        for post in &posts {
            let outcome = import_one(state, &remote, &current, &site, post).await;
            match &outcome {
                Outcome::Imported { .. } => {
                    progress.imported += 1;
                    imported_now += 1;
                }
                Outcome::Duplicate { .. } => progress.duplicates += 1,
                Outcome::NoFile { .. } | Outcome::Failed { .. } => progress.failed += 1,
            }
            report(&outcome);
            if options.limit.is_some_and(|limit| imported_now >= limit) {
                break;
            }
        }
        let next = remote::next_cursor(options.kind, cursor, &posts);
        let stop = options.limit.is_some_and(|limit| imported_now >= limit);
        match next {
            // Stopping early: this page again next time (done posts are
            // duplicates by then).
            Some(_) if stop => {}
            Some(next) => cursor = next,
            None => progress.finished = true,
        }
        progress.cursor = Some(cursor);
        remote_imports::save(db, &site, &options.tags, &progress)
            .await
            .map_err(|e| e.to_string())?;
        if stop || progress.finished {
            return Ok(progress);
        }
    }
}

async fn import_one(
    state: &AppState,
    remote: &Remote<'_>,
    current: &CurrentUser,
    site: &str,
    post: &RemotePost,
) -> Outcome {
    let id = post.id;
    let db = state.db.primary();
    let failed = |reason: String| Outcome::Failed { remote: id, reason };
    // Already here: from an earlier run, or uploaded some other way.
    match remote_imports::local_post(db, site, id).await {
        Ok(Some(local)) => {
            return Outcome::Duplicate {
                remote: id,
                post: local,
            };
        }
        Ok(None) => {}
        Err(e) => return failed(e.to_string()),
    }
    let md5: Option<[u8; 16]> = post
        .md5
        .as_deref()
        .and_then(|m| hex::decode(m).ok())
        .and_then(|b| b.try_into().ok());
    if let Some(md5) = md5 {
        match media::post_with_md5(db, &md5).await {
            Ok(Some(local)) => {
                record(state, current, site, post, local).await;
                return Outcome::Duplicate {
                    remote: id,
                    post: local,
                };
            }
            Ok(None) => {}
            Err(e) => return failed(e.to_string()),
        }
    }
    let Some(file_url) = &post.file_url else {
        return Outcome::NoFile { remote: id };
    };
    let Ok(url) = url::Url::parse(file_url) else {
        return failed(format!("bad file URL {file_url}"));
    };
    tokio::time::sleep(remote.options.delay).await;
    let writer = match TempWriter::create(&state.work_dir).await {
        Ok(writer) => writer,
        Err(e) => return failed(e.to_string()),
    };
    let limit = state.media.config().max_upload_mb * 1024 * 1024;
    let file = match remote.fetcher.fetch(&url, writer, limit).await {
        Ok(file) => file,
        Err(e) => return failed(e.to_string()),
    };
    let tags = match crate::import::usable_tags(state, &post.tags).await {
        Ok((tags, _invalid)) => tags,
        Err(e) => return failed(e.to_string()),
    };
    let fields = UploadFields {
        url: String::new(),
        rating: Some(post.rating),
        tags,
        source: post.source.clone(),
        description: String::new(),
    };
    let local = match ingest(state, current, &file, &fields).await {
        Ok(local) => local,
        Err(UploadError::Duplicate(local)) => {
            record(state, current, site, post, local).await;
            return Outcome::Duplicate {
                remote: id,
                post: local,
            };
        }
        Err(e) => return failed(e.to_string()),
    };
    record(state, current, site, post, local).await;
    if remote.options.notes
        && post.has_notes
        && remote.options.kind.has_notes_and_pools()
        && let Err(e) = import_notes(state, remote, current, site, post.id, local).await
    {
        tracing::warn!(remote = id, error = e, "notes not imported");
    }
    if remote.options.pools
        && remote.options.kind.has_notes_and_pools()
        && let Err(e) = import_pools(state, remote, current, site, post.id).await
    {
        tracing::warn!(remote = id, error = e, "pools not imported");
    }
    Outcome::Imported {
        remote: id,
        post: local,
    }
}

/// Remembers the local post, linking parents and children.
async fn record(
    state: &AppState,
    current: &CurrentUser,
    site: &str,
    post: &RemotePost,
    local: i64,
) {
    let updater = current.user.as_ref().map(|u| u.id);
    if let Err(error) = remote_imports::record(
        state.db.primary(),
        site,
        post.id,
        local,
        post.parent_id,
        updater,
    )
    .await
    {
        tracing::warn!(%error, remote = post.id, "could not record an imported post");
    }
}

async fn import_notes(
    state: &AppState,
    remote: &Remote<'_>,
    current: &CurrentUser,
    site: &str,
    remote_id: i64,
    local: i64,
) -> Result<(), String> {
    let db = state.db.primary();
    let body = remote.get(&remote::notes_url(site, remote_id)).await?;
    let asset = media::for_post(db, local)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no file")?;
    let creator = current.user.as_ref().map(|u| u.id);
    for note in remote::parse_notes(&body)? {
        let note_box = NoteBox {
            x: note.x,
            y: note.y,
            width: note.width,
            height: note.height,
        };
        let Ok(note_box) = note_box.fit(asset.width, asset.height) else {
            continue;
        };
        let Some(text) = moekura_core::notes::clean_body(&note.body) else {
            continue;
        };
        notes::create(db, local, note_box, &text, creator)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Puts the post in the site's pools that have it, by name, creating
/// them as needed, in the other site's order.
async fn import_pools(
    state: &AppState,
    remote: &Remote<'_>,
    current: &CurrentUser,
    site: &str,
    remote_id: i64,
) -> Result<(), String> {
    let db = state.db.primary();
    let body = remote.get(&remote::pools_url(site, remote_id)).await?;
    let updater = current.user.as_ref().map(|u| u.id);
    for pool in remote::parse_pools(&body)? {
        let Ok(name) = PoolName::parse(&pool.name) else {
            continue;
        };
        // The other site's posts that are here, in its order.
        let mut ordered = Vec::new();
        for id in &pool.post_ids {
            if let Some(local) = remote_imports::local_post(db, site, *id)
                .await
                .map_err(|e| e.to_string())?
            {
                ordered.push(local);
            }
        }
        let category = Category::parse(&pool.category)
            .unwrap_or_default()
            .as_str()
            .to_owned();
        match pools::by_name(db, name.as_str())
            .await
            .map_err(|e| e.to_string())?
        {
            Some(existing) => {
                let mut contents = pools::contents(db, existing.id)
                    .await
                    .map_err(|e| e.to_string())?
                    .ok_or("the pool vanished")?;
                // Posts added here some other way stay, after.
                let others: Vec<i64> = contents
                    .post_ids
                    .iter()
                    .copied()
                    .filter(|id| !ordered.contains(id))
                    .collect();
                contents.post_ids = ordered.into_iter().chain(others).collect();
                pools::save(db, existing.id, &contents, updater, None)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            None => {
                let contents = pools::Contents {
                    name: name.as_str().to_owned(),
                    description: pool.description.clone(),
                    category,
                    is_deleted: false,
                    post_ids: ordered,
                };
                pools::create(db, &contents, updater)
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use axum::Router;
    use axum::extract::Query;
    use axum::routing::get;
    use moekura_core::permissions::SystemRole;
    use serde_json::{Value, json};
    use sqlx::PgPool;

    use super::*;
    use crate::test_support::{fixture, session_for, test_state};

    /// A tiny Danbooru: posts 3 (parent 2), 2 and 1, newest first, two to
    /// a page; post 3 has a note, posts 2 and 3 are a pool.
    async fn fake_danbooru() -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let post = move |id: i64, parent: Option<i64>| {
            json!({
                "id": id,
                "file_url": format!("{base}/files/{id}.png"),
                "rating": "g",
                "tag_string_general": "cat",
                "tag_string_artist": "someone",
                "source": "",
                "parent_id": parent,
                "last_noted_at": if id == 3 { json!("2026-01-01T00:00:00Z") } else { json!(null) },
            })
        };
        let app = Router::new()
            .route(
                "/posts.json",
                get(move |Query(q): Query<std::collections::HashMap<String, String>>| {
                    let post = post.clone();
                    async move {
                        let before: i64 = q
                            .get("page")
                            .and_then(|p| p.strip_prefix('b'))
                            .and_then(|p| p.parse().ok())
                            .unwrap_or(i64::MAX);
                        let all = [post(3, Some(2)), post(2, None), post(1, None)];
                        let page: Vec<Value> = all
                            .into_iter()
                            .filter(|p| p["id"].as_i64().unwrap() < before)
                            .take(2)
                            .collect();
                        axum::Json(page)
                    }
                }),
            )
            .route(
                "/files/{name}",
                get(|axum::extract::Path(name): axum::extract::Path<String>| async move {
                    let width = 20 + name.trim_end_matches(".png").parse::<u32>().unwrap() * 4;
                    fixture::png(width, 20)
                }),
            )
            .route(
                "/notes.json",
                get(|| async {
                    axum::Json(json!([{ "x": 1, "y": 1, "width": 5, "height": 5, "body": "Hi", "is_active": true }]))
                }),
            )
            .route(
                "/pools.json",
                get(|| async {
                    axum::Json(json!([{ "name": "Cats", "description": "", "category": "series", "post_ids": [2, 3] }]))
                }),
            );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        addr
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn imports_resumes_and_links(pool: PgPool) {
        let addr = fake_danbooru().await;
        let state = test_state(&pool).await;
        session_for(&pool, "importer", SystemRole::Admin).await;
        let uploader = moekura_db::users::by_name(&pool, "importer")
            .await
            .unwrap()
            .unwrap();
        let options = Options {
            kind: Kind::Danbooru,
            base: format!("http://{addr}/"),
            tags: "cat".into(),
            credentials: Credentials::default(),
            delay: Duration::ZERO,
            limit: Some(1),
            notes: true,
            pools: true,
            restart: false,
            allow_private: true,
        };
        let mut seen = Vec::new();
        let first = run(&state, &uploader, &options, |o| seen.push(o.clone()))
            .await
            .unwrap();
        assert_eq!((first.imported, first.finished), (1, false));
        assert!(matches!(seen[0], Outcome::Imported { remote: 3, .. }));

        let options = Options {
            limit: None,
            ..options
        };
        let done = run(&state, &uploader, &options, |o| seen.push(o.clone()))
            .await
            .unwrap();
        assert_eq!(
            (done.imported, done.duplicates, done.finished),
            (3, 1, true)
        );

        let site = format!("http://{addr}");
        let local = |remote: i64| {
            let pool = pool.clone();
            let site = site.clone();
            async move {
                moekura_db::remote_imports::local_post(&pool, &site, remote)
                    .await
                    .unwrap()
                    .unwrap()
            }
        };
        let (three, two) = (local(3).await, local(2).await);
        let post = moekura_db::posts::by_id(&pool, three)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(post.parent_id, Some(two), "linked once the parent arrived");
        assert_eq!(post.source, format!("http://{addr}/posts/3"));
        let artist = moekura_db::tags::by_name(&pool, "someone")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(artist.category_id, 1);
        assert_eq!(
            moekura_db::notes::for_post(&pool, three, false)
                .await
                .unwrap()
                .len(),
            1
        );
        let cats = moekura_db::pools::by_name(&pool, "Cats")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            moekura_db::pools::post_ids(&pool, cats.id).await.unwrap(),
            [two, three]
        );

        // Finished: nothing to do, unless started over.
        let again = run(&state, &uploader, &options, |_| {}).await.unwrap();
        assert_eq!(again.imported, 3);
        let over = run(
            &state,
            &uploader,
            &Options {
                restart: true,
                ..options
            },
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!((over.imported, over.duplicates), (0, 3));
    }
}
