//! Request counts, latencies and requests in flight for Prometheus, when
//! `telemetry.metrics_bind` is set. Requests are labelled by their route's
//! template (`/posts/{id}`), method and status, so the series are a
//! small, fixed set whatever is requested.

use std::sync::{Arc, OnceLock};
use std::time::Instant;

use axum::extract::{MatchedPath, Request};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;

/// Requests that matched no route (404s, probes for missing files).
const UNMATCHED: &str = "unmatched";

fn method_label(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::HEAD => "HEAD",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::PATCH => "PATCH",
        Method::DELETE => "DELETE",
        Method::OPTIONS => "OPTIONS",
        _ => "other",
    }
}

/// Counts a request as in flight until dropped, even if the client goes
/// away mid-request.
struct InFlight;

impl InFlight {
    fn start() -> Self {
        metrics::gauge!("moekura_http_requests_in_flight").increment(1.0);
        Self
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        metrics::gauge!("moekura_http_requests_in_flight").decrement(1.0);
    }
}

/// Where [`note_route`] leaves the matched route for [`measure`], which
/// wraps the routing (and the CORS and form checks around it), so can't
/// see it itself. Filled in even if the request then times out.
#[derive(Clone, Default)]
struct RouteSlot(Arc<OnceLock<String>>);

/// Middleware on every route (and the fallback): records which route
/// matched.
pub(crate) async fn note_route(request: Request, next: Next) -> Response {
    if let (Some(slot), Some(path)) = (
        request.extensions().get::<RouteSlot>(),
        request.extensions().get::<MatchedPath>(),
    ) {
        let _ = slot.0.set(path.as_str().to_owned());
    }
    next.run(request).await
}

/// Middleware outside all the others: times each request and counts it by
/// route, method and status.
pub(crate) async fn measure(mut request: Request, next: Next) -> Response {
    let slot = RouteSlot::default();
    request.extensions_mut().insert(slot.clone());
    let method = method_label(request.method());
    let started = Instant::now();
    let in_flight = InFlight::start();
    let response = next.run(request).await;
    drop(in_flight);
    let route = slot.0.get().map_or(UNMATCHED, String::as_str).to_owned();
    metrics::counter!(
        "moekura_http_requests_total",
        "method" => method,
        "route" => route.clone(),
        "status" => response.status().as_str().to_owned(),
    )
    .increment(1);
    metrics::histogram!(
        "moekura_http_request_duration_seconds",
        "method" => method,
        "route" => route,
    )
    .record(started.elapsed().as_secs_f64());
    response
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
    use sqlx::PgPool;

    use crate::test_support::{TestApp, test_config, test_state_with};

    /// The process-wide recorder; tests that don't set `metrics_bind`
    /// record nothing over HTTP.
    fn recorder() -> &'static PrometheusHandle {
        static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();
        HANDLE.get_or_init(|| {
            let recorder = PrometheusBuilder::new().build_recorder();
            let handle = recorder.handle();
            metrics::set_global_recorder(recorder).expect("no other recorder in tests");
            handle
        })
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn labels_requests_by_route_template(pool: PgPool) {
        let handle = recorder();
        let mut config = test_config();
        config.telemetry.metrics_bind = Some("127.0.0.1:0".parse().unwrap());
        let state = test_state_with(&pool, config).await;
        let app = TestApp::new(state, crate::posts::routes());

        assert_eq!(app.get("/posts/987654", None).await.status, 404);
        assert_eq!(app.get("/no/such/page/12345", None).await.status, 404);

        let text = handle.render();
        let line = |route: &str| {
            text.lines()
                .find(|l| {
                    l.starts_with("moekura_http_requests_total{")
                        && l.contains(&format!("route=\"{route}\""))
                })
                .map(str::to_owned)
        };
        let posts = line("/posts/{id}").expect("the post route is counted");
        assert!(posts.contains("method=\"GET\""), "{posts}");
        assert!(posts.contains("status=\"404\""), "{posts}");
        assert!(line("unmatched").is_some(), "{text}");
        // Neither the post id nor the missing path becomes a label.
        assert!(!text.contains("987654"), "{text}");
        assert!(!text.contains("12345"), "{text}");
        assert!(text.contains("moekura_http_request_duration_seconds"));
        assert!(text.contains("moekura_http_requests_in_flight 0"), "{text}");
    }
}
