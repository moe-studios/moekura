//! Reporting messages, forum posts, users, wiki pages, pools, notes,
//! artists and feedback to staff, and the queue where staff settle the
//! reports. Posts are flagged and comments reported on their own.

use axum::extract::{Path, Query};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::markup;
use moekura_core::moderation::{ActionKind, REASON_MAX_LEN, ReportKind, SNAPSHOT_MAX_LEN};
use moekura_core::permissions::Permission;
use moekura_db::dmails;
use moekura_db::mod_actions::{self, NewAction};
use moekura_db::moderation_reports::{self, NewReport, Report, ReportError};
use moekura_db::users;
use serde::Deserialize;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::ranks::Act;
use crate::templates::url_value;

/// Things with open reports per page of the queue.
const PAGE_SIZE: i64 = 30;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/reports", post(create))
        .route("/moderation/reports", get(queue))
        .route("/moderation/reports/{kind}/{id}/settle", post(settle))
        // Reported messages used to have a page of their own.
        .route(
            "/moderation/dmails",
            get(|| async { Redirect::permanent("/moderation/reports?kind=dmail") }),
        )
}

/// What's reported, as staff see it.
pub(crate) struct Subject {
    /// Who wrote or made it, if anyone.
    pub owner_id: Option<i64>,
    /// Its text now, kept with the report.
    pub text: String,
}

fn snapshot(parts: &[&str]) -> String {
    let text = parts
        .iter()
        .filter(|p| !p.trim().is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("\n\n");
    text.chars().take(SNAPSHOT_MAX_LEN).collect()
}

fn small_id(id: i64) -> Result<i32, AppError> {
    i32::try_from(id).map_err(|_| AppError::NotFound)
}

/// The thing `current` wants to report, if they may see it.
pub(crate) async fn subject(
    state: &AppState,
    current: &CurrentUser,
    kind: ReportKind,
    id: i64,
) -> Result<Subject, AppError> {
    let db = state.db.primary();
    let me = current.user.as_ref().map(|u| u.id);
    Ok(match kind {
        ReportKind::Dmail => {
            // Only the copy they received, by its recipient.
            let d = dmails::by_id(db, me.ok_or(AppError::Unauthorized)?, id)
                .await?
                .filter(|d| d.from_id != Some(d.owner_id))
                .ok_or(AppError::NotFound)?;
            Subject {
                owner_id: d.from_id,
                text: snapshot(&[&d.title, &d.body]),
            }
        }
        ReportKind::ForumPost => {
            let p = moekura_db::forum::post(db, id)
                .await?
                .ok_or(AppError::NotFound)?;
            crate::forum::visible_topic(state, current, p.topic_id).await?;
            if p.is_hidden && !crate::forum::moderates(current) {
                return Err(AppError::NotFound);
            }
            Subject {
                owner_id: p.creator_id,
                text: snapshot(&[&p.body]),
            }
        }
        ReportKind::User => {
            let user = users::by_id(db, id).await?.ok_or(AppError::NotFound)?;
            let profile = users::profile(db, id).await?;
            Subject {
                owner_id: Some(user.id),
                text: snapshot(&[&user.name, &profile.bio]),
            }
        }
        ReportKind::WikiPage => {
            let page = moekura_db::wiki::by_id(db, small_id(id)?)
                .await?
                .ok_or(AppError::NotFound)?;
            Subject {
                owner_id: None,
                text: snapshot(&[&page.title, &page.body]),
            }
        }
        ReportKind::Pool => {
            let pool = moekura_db::pools::by_id(db, small_id(id)?)
                .await?
                .filter(|p| !p.is_deleted || current.can(Permission::DeletePosts))
                .ok_or(AppError::NotFound)?;
            Subject {
                owner_id: None,
                text: snapshot(&[&pool.name, &pool.description]),
            }
        }
        ReportKind::Note => {
            let note = moekura_db::notes::by_id(db, id)
                .await?
                .filter(|n| n.is_active)
                .ok_or(AppError::NotFound)?;
            moekura_db::posts::by_id(db, note.post_id)
                .await?
                .filter(|p| crate::posts::visibility(current).allows(p))
                .ok_or(AppError::NotFound)?;
            Subject {
                owner_id: note.creator_id,
                text: snapshot(&[&note.body]),
            }
        }
        ReportKind::Artist => {
            let artist = moekura_db::artists::by_id(db, small_id(id)?)
                .await?
                .filter(|a| !a.is_deleted)
                .ok_or(AppError::NotFound)?;
            Subject {
                owner_id: None,
                text: snapshot(&[&artist.name, &artist.other_names.join(" ")]),
            }
        }
        ReportKind::UserFeedback => {
            let f = moekura_db::user_feedbacks::by_id(db, id)
                .await?
                .filter(|f| !f.is_deleted)
                .ok_or(AppError::NotFound)?;
            Subject {
                owner_id: f.creator_id,
                text: snapshot(&[&f.category, &f.body]),
            }
        }
    })
}

#[derive(Debug, Default, Deserialize)]
struct ReportForm {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    id: i64,
    #[serde(default)]
    reason: String,
    /// The page to go back to.
    back: Option<String>,
}

async fn create(
    page: Page,
    jar: CookieJar,
    Form(form): Form<ReportForm>,
) -> Result<Response, AppError> {
    let kind = ReportKind::parse(&form.kind).ok_or(AppError::NotFound)?;
    report(page.state(), &page.current, kind, form.id, &form.reason).await?;
    let back = crate::account::safe_next(form.back.as_deref()).to_owned();
    Ok((flash::set(jar, Flash::Reported), Redirect::to(&back)).into_response())
}

/// Reports something to staff, keeping its text as it is now. Messages
/// are reported by their recipients, anything else by those who may flag
/// posts; not one's own, and not the same thing twice while a report is
/// open or soon after one was settled.
pub(crate) async fn report(
    state: &AppState,
    current: &CurrentUser,
    kind: ReportKind,
    id: i64,
    reason: &str,
) -> Result<i64, AppError> {
    if kind != ReportKind::Dmail {
        current.require(Permission::Flag)?;
    }
    let user = current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(AppError::BadRequest("Say what's wrong".into()));
    }
    if reason.chars().count() > REASON_MAX_LEN {
        return Err(AppError::BadRequest(format!(
            "The reason may be at most {REASON_MAX_LEN} characters"
        )));
    }
    let subject = subject(state, current, kind, id).await?;
    if subject.owner_id == Some(user.id) {
        return Err(AppError::BadRequest("You can't report your own".into()));
    }
    // Before the rate limit, so a refused repeat doesn't use it up.
    let refused = |e: ReportError| match e {
        ReportError::Db(e) => AppError::from(e),
        other => AppError::BadRequest(other.to_string()),
    };
    moderation_reports::check_new(state.db.primary(), kind, id, user.id)
        .await
        .map_err(refused)?;
    state.rate_limits.check_report(user.id).await?;
    let mut tx = state.db.primary().begin().await?;
    let report_id = moderation_reports::create(
        &mut tx,
        NewReport {
            kind,
            subject_id: id,
            owner_id: subject.owner_id,
            creator_id: user.id,
            reason,
            snapshot: &subject.text,
        },
    )
    .await
    .map_err(refused)?;
    mod_actions::record(
        &mut *tx,
        log_entry(
            Some(user.id),
            ActionKind::ReportCreate,
            kind,
            id,
            subject.owner_id,
        )
        .reason(reason),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(kind = kind.as_str(), id, user = user.name, "reported");
    Ok(report_id)
}

/// A log entry about reported thing `id` of `kind`, by `owner`.
fn log_entry(
    actor: Option<i64>,
    action: ActionKind,
    kind: ReportKind,
    id: i64,
    owner: Option<i64>,
) -> NewAction<'static> {
    let mut entry = NewAction::new(actor, action);
    if let Some(subject) = kind.subject() {
        entry = entry.subject(subject, id);
    }
    match owner {
        Some(owner) => entry.user(owner),
        None => entry,
    }
}

/// The kinds of report `current` handles.
pub(crate) fn handled_kinds(current: &CurrentUser) -> Vec<ReportKind> {
    ReportKind::ALL
        .into_iter()
        .filter(|k| current.can(k.handled_by()))
        .collect()
}

#[derive(Debug, Default, Deserialize)]
struct QueueQuery {
    #[serde(default)]
    kind: String,
    after: Option<i64>,
}

async fn queue(page: Page, Query(query): Query<QueueQuery>) -> Result<Response, AppError> {
    let mut kinds = handled_kinds(&page.current);
    if kinds.is_empty() {
        page.current.require(Permission::ModerateComments)?;
    }
    if let Some(only) = ReportKind::parse(&query.kind) {
        kinds.retain(|k| *k == only);
    }
    let db = page.state().db.primary();
    let open = moderation_reports::open(db, &kinds, query.after.unwrap_or(0), PAGE_SIZE).await?;
    // Each thing's reports come together, oldest first.
    let mut things: Vec<(&str, i64)> = Vec::new();
    for r in &open {
        if !things.contains(&(r.subject_kind.as_str(), r.subject_id)) {
            things.push((r.subject_kind.as_str(), r.subject_id));
        }
    }
    let mut items: Vec<Value> = Vec::new();
    for (kind, id) in &things {
        let reports: Vec<&Report> = open
            .iter()
            .filter(|r| r.subject_kind == *kind && r.subject_id == *id)
            .collect();
        let first = reports[0];
        items.push(context! {
            kind => kind,
            id => id,
            owner => first.owner_name,
            url => subject_url(db, first).await?.map(|u| url_value(&u)),
            // The text as each report found it, oldest first; repeats
            // left out.
            texts => distinct_snapshots(&reports),
            reports => reports.iter().map(|r| context! {
                by => r.creator_name,
                reason => r.reason,
                when => crate::dates::day(r.created_at),
            }).collect::<Vec<_>>(),
        });
    }
    let more = (things.len() == PAGE_SIZE as usize)
        .then(|| {
            let (kind, id) = things.last()?;
            open.iter()
                .find(|r| r.subject_kind == *kind && r.subject_id == *id)
        })
        .flatten()
        .map(|r| {
            url_value(&format!(
                "/moderation/reports?kind={}&after={}",
                query.kind, r.id
            ))
        });
    Ok(page.render(
        "moderation_reports.html",
        context! {
            items => items,
            kinds => handled_kinds(&page.current).iter().map(|k| k.as_str()).collect::<Vec<_>>(),
            kind => query.kind,
            more_url => more,
        },
    ))
}

/// Where a reported thing is now, for staff, if it still is.
async fn subject_url(db: &sqlx::PgPool, report: &Report) -> Result<Option<String>, AppError> {
    let id = report.subject_id;
    let small = || i32::try_from(id).ok();
    Ok(match ReportKind::parse(&report.subject_kind) {
        None | Some(ReportKind::Dmail) => None,
        Some(ReportKind::ForumPost) => Some(format!("/forum_posts/{id}")),
        Some(ReportKind::User) => report
            .owner_name
            .as_ref()
            .map(|name| crate::user_moderation::url(name)),
        Some(ReportKind::WikiPage) => match small() {
            Some(id) => moekura_db::wiki::by_id(db, id)
                .await?
                .map(|page| markup::wiki_url(&page.title)),
            None => None,
        },
        Some(ReportKind::Pool) => Some(format!("/pools/{id}")),
        Some(ReportKind::Note) => moekura_db::notes::by_id(db, id)
            .await?
            .map(|note| format!("/posts/{}", note.post_id)),
        Some(ReportKind::Artist) => Some(format!("/artists/{id}")),
        Some(ReportKind::UserFeedback) => moekura_db::user_feedbacks::by_id(db, id)
            .await?
            .map(|f| crate::user_feedbacks::list_url(&f.user_name)),
    })
}

fn distinct_snapshots(reports: &[&Report]) -> Vec<Value> {
    let mut seen: Vec<&str> = Vec::new();
    let mut texts = Vec::new();
    for r in reports {
        if !seen.contains(&r.snapshot.as_str()) {
            seen.push(&r.snapshot);
            texts.push(context! {
                when => crate::dates::day(r.created_at),
                html => Value::from_safe_string(markup::render(&r.snapshot)),
            });
        }
    }
    texts
}

#[derive(Debug, Default, Deserialize)]
struct SettleForm {
    /// `uphold` or `dismiss`.
    #[serde(default)]
    decision: String,
    /// How it ended, for the reporters' record and the log.
    #[serde(default)]
    outcome: String,
}

async fn settle(
    page: Page,
    jar: CookieJar,
    Path((kind, id)): Path<(String, i64)>,
    Form(form): Form<SettleForm>,
) -> Result<Response, AppError> {
    let kind = ReportKind::parse(&kind).ok_or(AppError::NotFound)?;
    let upheld = match form.decision.as_str() {
        "uphold" => true,
        "dismiss" => false,
        _ => return Err(AppError::BadRequest("Uphold or dismiss?".into())),
    };
    settle_reports(page.state(), &page.current, kind, id, upheld, &form.outcome).await?;
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to(&format!("/moderation/reports?kind={}", kind.as_str())),
    )
        .into_response())
}

/// Settles the open reports about thing `id` of `kind` as upheld or
/// dismissed, with `outcome`; returns how many there were. Settling
/// reports about one's own work, or that of someone at or above them,
/// is for others.
pub(crate) async fn settle_reports(
    state: &AppState,
    current: &CurrentUser,
    kind: ReportKind,
    id: i64,
    upheld: bool,
    outcome: &str,
) -> Result<u64, AppError> {
    current.require(kind.handled_by())?;
    let outcome = crate::moderation::check_reason(outcome)?;
    let db = state.db.primary();
    let reports = moderation_reports::for_subject(db, kind, id).await?;
    let owner = reports.first().and_then(|r| r.owner_id);
    let act = if upheld { Act::Against } else { Act::For };
    crate::ranks::check_moderate(state, current, owner, act).await?;
    let actor = current.user.as_ref().map(|u| u.id);
    let mut tx = db.begin().await?;
    let settled = moderation_reports::resolve(&mut *tx, kind, id, upheld, actor, outcome).await?;
    if settled == 0 {
        return Err(AppError::BadRequest(
            "There are no open reports about it".into(),
        ));
    }
    mod_actions::record(
        &mut *tx,
        log_entry(actor, ActionKind::ReportSettle, kind, id, owner)
            .reason(outcome)
            .details(serde_json::json!({
                "kind": kind.as_str(),
                "decision": if upheld { "upheld" } else { "dismissed" },
                "reports": settled,
            })),
    )
    .await?;
    tx.commit().await?;
    Ok(settled)
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn reports_keep_what_was_said(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            super::routes()
                .merge(crate::forum::routes())
                .merge(crate::users::routes()),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let janitor = session_for(&pool, "jan", SystemRole::Janitor).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        let general = moekura_db::forum::categories(&pool).await.unwrap()[0].id;
        app.post_form(
            "/forum_topics",
            Some(&bob),
            &[],
            &format!("category={general}&title=Hi&body=You+are+all+idiots"),
        )
        .await;
        let post: i64 = sqlx::query_scalar("SELECT id FROM forum_posts")
            .fetch_one(&pool)
            .await
            .unwrap();
        let report = async |who: &str, form: &str| {
            app.post_form("/reports", Some(who), &[], form).await.status
        };
        let about_post = format!("kind=forum_post&id={post}&reason=rude&back=%2Fforum_topics");
        // Not one's own; not without a reason; not as a visitor.
        assert_eq!(report(&bob, &about_post).await, StatusCode::BAD_REQUEST);
        assert_eq!(
            report(&alice, &format!("kind=forum_post&id={post}&reason=")).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            app.post_form("/reports", None, &[], &about_post)
                .await
                .status,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(report(&alice, &about_post).await, StatusCode::SEE_OTHER);
        // Cleaning it up afterwards doesn't clean up the report.
        app.post_form(
            &format!("/forum_posts/{post}"),
            Some(&bob),
            &[],
            "body=Hello+all",
        )
        .await;
        let queue = app.get("/moderation/reports", Some(&janitor)).await.body;
        assert!(queue.contains("You are all idiots"), "{queue}");
        assert!(queue.contains("“rude”"), "{queue}");

        // Users are reported to those who ban them.
        let bob_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE name = 'bob'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let about_bob = format!("kind=user&id={bob_id}&reason=troll");
        assert_eq!(report(&alice, &about_bob).await, StatusCode::SEE_OTHER);
        let queue = app
            .get("/moderation/reports?kind=user", Some(&janitor))
            .await
            .body;
        assert!(!queue.contains("troll"), "{queue}");
        let queue = app
            .get("/moderation/reports?kind=user", Some(&moderator))
            .await
            .body;
        assert!(queue.contains("troll"), "{queue}");
        // Nobody settles reports about themselves.
        let mods_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE name = 'mod'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            report(&alice, &format!("kind=user&id={mods_id}&reason=unfair")).await,
            StatusCode::SEE_OTHER
        );
        let own = app
            .post_form(
                &format!("/moderation/reports/user/{mods_id}/settle"),
                Some(&moderator),
                &[],
                "decision=dismiss",
            )
            .await;
        assert_eq!(own.status, StatusCode::FORBIDDEN);
        let dismissed = app
            .post_form(
                &format!("/moderation/reports/forum_post/{post}/settle"),
                Some(&janitor),
                &[],
                "decision=dismiss&outcome=edited+since",
            )
            .await;
        assert_eq!(dismissed.status, StatusCode::SEE_OTHER);
        let logged: Vec<String> = sqlx::query_scalar("SELECT action FROM mod_actions ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(
            logged,
            [
                "report.create",
                "report.create",
                "report.create",
                "report.settle"
            ]
        );
    }
}
