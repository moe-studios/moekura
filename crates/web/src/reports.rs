//! `/stats`, the site's totals for everyone, and `/reports`, charts of
//! activity over time for staff, by user too. The `stats.refresh` job
//! counts both hourly, so neither page counts anything itself.

use axum::Router;
use axum::extract::Query;
use axum::response::Response;
use axum::routing::get;
use minijinja::context;
use moekura_core::permissions::Permission;
use moekura_db::reports::{self, Metric};
use moekura_db::users;
use serde::Deserialize;
use time::{Date, Duration, OffsetDateTime};

use crate::AppState;
use crate::charts::{self, Bar};
use crate::error::AppError;
use crate::pages::Page;

/// The periods reports cover, in days.
const PERIODS: [i64; 3] = [30, 90, 365];
/// Users listed as most active.
const TOP_USERS: i64 = 20;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/stats", get(stats))
        .route("/reports", get(index))
}

async fn stats(page: Page) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let totals = reports::totals(page.state().reader(&page.current)).await?;
    let counted = totals.iter().map(|(.., at)| *at).min();
    Ok(page.render(
        "stats.html",
        context! {
            totals => totals.iter().map(|(key, value, _)| context! {
                key => key,
                value => value,
            }).collect::<Vec<_>>(),
            counted => counted.map(crate::dates::day),
            can_report => page.current.can(Permission::ViewAuditLog),
        },
    ))
}

#[derive(Debug, Default, Deserialize)]
struct ReportQuery {
    #[serde(default)]
    metric: String,
    days: Option<i64>,
    #[serde(default)]
    user: String,
}

/// `daily` counts as bars: one a day, or one a week over long periods,
/// labelled now and then.
fn chart_bars(daily: &[(Date, i64)], per_bar: usize) -> Vec<Bar> {
    let label_every = (daily.len() / per_bar).div_ceil(6).max(1);
    daily
        .chunks(per_bar)
        .enumerate()
        .map(|(i, days)| {
            let first = days[0].0;
            let value = days.iter().map(|(_, n)| n).sum();
            let span = if days.len() > 1 {
                format!("{first} to {}", days[days.len() - 1].0)
            } else {
                first.to_string()
            };
            Bar {
                label: if i % label_every == 0 {
                    format!("{} {}", &first.month().to_string()[..3], first.day())
                } else {
                    String::new()
                },
                title: format!("{span}: {value}"),
                value,
            }
        })
        .collect()
}

async fn index(page: Page, Query(query): Query<ReportQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewAuditLog)?;
    let db = page.state().reader(&page.current);
    let metric = Metric::parse(query.metric.trim()).unwrap_or(Metric::Uploads);
    let days = query
        .days
        .filter(|d| PERIODS.contains(d))
        .unwrap_or(PERIODS[0]);
    let user = match query.user.trim() {
        "" => None,
        name if metric.by_user() => {
            Some(users::by_name(db, name).await?.ok_or(AppError::NotFound)?)
        }
        _ => None,
    };
    let to = OffsetDateTime::now_utc().date();
    let from = to - Duration::days(days - 1);
    let daily = reports::daily(db, metric, user.as_ref().map(|u| u.id), from, to).await?;
    let total: i64 = daily.iter().map(|(_, n)| n).sum();
    let per_bar = if days > 90 { 7 } else { 1 };
    let top = if metric.by_user() && user.is_none() {
        reports::top_users(db, metric, from, to, TOP_USERS).await?
    } else {
        Vec::new()
    };
    let link = |metric: Metric, days: i64, user: Option<&str>| {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("metric", metric.as_str());
        query.append_pair("days", &days.to_string());
        if let Some(user) = user.filter(|_| metric.by_user()) {
            query.append_pair("user", user);
        }
        crate::templates::url_value(&format!("/reports?{}", query.finish()))
    };
    let user_name = user.as_ref().map(|u| u.name.as_str());
    Ok(page.render(
        "reports.html",
        context! {
            metric => metric.as_str(),
            metrics => Metric::ALL.iter().map(|&m| context! {
                key => m.as_str(),
                url => link(m, days, user_name),
                current => m == metric,
            }).collect::<Vec<_>>(),
            periods => PERIODS.iter().map(|&d| context! {
                days => d,
                url => link(metric, d, user_name),
                current => d == days,
            }).collect::<Vec<_>>(),
            user => user_name,
            all_url => user_name.map(|_| link(metric, days, None)),
            days => days,
            total => total,
            chart => charts::bars(&chart_bars(&daily, per_bar)),
            per_week => per_bar > 1,
            top => top.iter().map(|(name, count)| context! {
                name => name,
                count => count,
                url => link(metric, days, Some(name)),
            }).collect::<Vec<_>>(),
        },
    ))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn stats_and_reports(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, super::routes());
        let member = session_for(&pool, "member", SystemRole::Member).await;
        let moderator = session_for(&pool, "moderator", SystemRole::Moderator).await;
        sqlx::query(
            "INSERT INTO posts (rating, uploader_id) SELECT 'g', id FROM users WHERE name = 'member'",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Nothing until the job counts.
        assert!(
            app.get("/stats", None)
                .await
                .body
                .contains("aren't counted yet")
        );
        let today = time::OffsetDateTime::now_utc().date();
        for metric in moekura_db::reports::Metric::ALL {
            moekura_db::reports::refresh(&pool, metric, today, today)
                .await
                .unwrap();
        }
        moekura_db::reports::refresh_totals(&pool).await.unwrap();
        let stats = app.get("/stats", None).await.body;
        assert!(stats.contains("<dt>Posts</dt><dd>1</dd>"), "{stats}");
        assert!(stats.contains("<dt>Users</dt><dd>2</dd>"), "{stats}");
        assert!(!stats.contains("href=\"/reports\""));

        assert_eq!(
            app.get("/reports", Some(&member)).await.status,
            StatusCode::FORBIDDEN
        );
        let report = app.get("/reports", Some(&moderator)).await.body;
        assert!(report.contains("class=\"bar-chart\""), "{report}");
        assert!(report.contains("1 in the last 30 days"), "{report}");
        assert!(
            report.contains("href=\"/reports?metric=uploads&amp;days=30&amp;user=member\""),
            "{report}"
        );
        let theirs = app
            .get(
                "/reports?metric=uploads&days=365&user=member",
                Some(&moderator),
            )
            .await
            .body;
        assert!(
            theirs.contains("by member") && theirs.contains("by the week"),
            "{theirs}"
        );
        let none = app
            .get("/reports?metric=comments&days=90", Some(&moderator))
            .await
            .body;
        assert!(none.contains("0 in the last 90 days") && !none.contains("bar-chart"));
        assert_eq!(
            app.get("/reports?user=nobody", Some(&moderator))
                .await
                .status,
            StatusCode::NOT_FOUND
        );
    }
}
