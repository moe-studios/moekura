//! Logs, and when configured, Prometheus metrics and OpenTelemetry traces.

use std::collections::BTreeSet;
use std::io::IsTerminal;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, anyhow};
use axum::Router;
use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::response::IntoResponse;
use axum::routing::get;
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use moekura_core::config::{ConnectionConfig, LogFormat, TelemetryConfig};
use moekura_db::Db;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// What [`init`] started, to be shut down at exit so buffered traces are
/// sent.
#[derive(Default)]
pub struct Telemetry {
    traces: Option<SdkTracerProvider>,
}

impl Telemetry {
    /// Sends the traces still buffered and stops the exporter.
    pub async fn shutdown(self) {
        let Some(traces) = self.traces else { return };
        // Blocks until the batch is sent; off the async threads.
        let stopped = tokio::task::spawn_blocking(move || traces.shutdown()).await;
        if let Ok(Err(error)) = stopped {
            eprintln!("could not send the last traces: {error}");
        }
    }
}

/// Installs the global `tracing` subscriber, with OTLP export when
/// `otlp_endpoint` is set. `RUST_LOG` overrides the configured filter.
/// `role` (`serve`, `worker`, …) is recorded on exported traces.
pub fn init(config: &TelemetryConfig, role: &str) -> anyhow::Result<Telemetry> {
    let filter = match EnvFilter::try_from_default_env() {
        Ok(filter) => filter,
        Err(_) => EnvFilter::try_new(&config.log_filter)
            .map_err(|e| anyhow!("invalid telemetry.log_filter: {e}"))?,
    };
    let fmt = tracing_subscriber::fmt::layer().with_ansi(std::io::stdout().is_terminal());
    let fmt = match config.log_format {
        LogFormat::Text => fmt.boxed(),
        LogFormat::Json => fmt.json().flatten_event(true).boxed(),
    };
    let traces = config
        .otlp_endpoint
        .as_ref()
        .map(|endpoint| tracer_provider(config, endpoint, role))
        .transpose()?;
    let otel = traces
        .as_ref()
        .map(|provider| tracing_opentelemetry::layer().with_tracer(provider.tracer("moekura")));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt)
        .with(otel)
        .try_init()
        .map_err(|e| anyhow!(e))?;
    Ok(Telemetry { traces })
}

/// Batches spans and sends them to `endpoint` over OTLP/HTTP (protobuf).
fn tracer_provider(
    config: &TelemetryConfig,
    endpoint: &url::Url,
    role: &str,
) -> anyhow::Result<SdkTracerProvider> {
    // Below the endpoint's path, whether or not it ends in a slash.
    let mut traces = endpoint.clone();
    traces.set_path(&format!(
        "{}/v1/traces",
        endpoint.path().trim_end_matches('/')
    ));
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(traces.as_str())
        .with_timeout(Duration::from_secs(10))
        .with_http_client(OtlpClient::new()?)
        .build()
        .context("could not set up OTLP export")?;
    let resource = opentelemetry_sdk::Resource::builder()
        .with_service_name(config.service_name.clone())
        .with_attribute(opentelemetry::KeyValue::new(
            "moekura.role",
            role.to_owned(),
        ))
        .with_attribute(opentelemetry::KeyValue::new(
            "service.version",
            env!("CARGO_PKG_VERSION"),
        ))
        .build();
    Ok(SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
            config.otlp_sample_ratio,
        ))))
        .with_resource(resource)
        .build())
}

/// Sends OTLP requests with the reqwest client the rest of the app uses.
/// The batch exporter runs on its own thread, outside the async runtime,
/// so requests are handed to the runtime and awaited from there.
#[derive(Debug)]
struct OtlpClient {
    client: reqwest::Client,
    runtime: tokio::runtime::Handle,
}

impl OtlpClient {
    fn new() -> anyhow::Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .context("could not build the OTLP HTTP client")?,
            runtime: tokio::runtime::Handle::current(),
        })
    }
}

#[async_trait::async_trait]
impl opentelemetry_http::HttpClient for OtlpClient {
    async fn send_bytes(
        &self,
        request: axum::http::Request<bytes::Bytes>,
    ) -> Result<axum::http::Response<bytes::Bytes>, opentelemetry_http::HttpError> {
        let request = reqwest::Request::try_from(request.map(reqwest::Body::from))?;
        let client = self.client.clone();
        let sent = self.runtime.spawn(async move {
            let response = client.execute(request).await?;
            let mut answer = axum::http::Response::builder().status(response.status());
            for (name, value) in response.headers() {
                answer = answer.header(name, value);
            }
            let body = response.bytes().await?;
            Ok::<_, opentelemetry_http::HttpError>(answer.body(body)?)
        });
        sent.await?
    }
}

// ---- metrics -----------------------------------------------------------------

/// Bucket bounds, in seconds, for request and job durations.
const REQUEST_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];
const JOB_BUCKETS: &[f64] = &[0.01, 0.1, 0.5, 1.0, 5.0, 15.0, 60.0, 300.0, 1800.0];

/// Installs the Prometheus recorder, so the `metrics` calls throughout
/// start counting. Before this, they cost next to nothing.
fn install_metrics() -> anyhow::Result<PrometheusHandle> {
    PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full("moekura_http_request_duration_seconds".into()),
            REQUEST_BUCKETS,
        )?
        .set_buckets_for_metric(
            Matcher::Full("moekura_job_duration_seconds".into()),
            JOB_BUCKETS,
        )?
        .install_recorder()
        .context("could not install the metrics recorder")
}

struct Scrape {
    handle: PrometheusHandle,
    db: Db,
    /// Job kinds reported so far, so a kind whose jobs are all gone
    /// reads zero rather than its last count.
    kinds: Mutex<BTreeSet<String>>,
}

/// Installs the recorder and serves `/metrics` on `bind` until
/// `shutdown`: what the recorder counted, plus the database pools and the
/// job queue as they are at the time of the scrape. Connections are
/// limited as the site's are.
pub async fn start_metrics(
    bind: SocketAddr,
    limits: ConnectionConfig,
    db: Db,
    shutdown: CancellationToken,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("could not bind telemetry.metrics_bind {bind}"))?;
    let handle = install_metrics()?;
    tracing::info!(addr = %listener.local_addr()?, "serving metrics at /metrics");
    let app = metrics_router(handle, db);
    Ok(tokio::spawn(async move {
        let served = moekura_web::serve(listener, app, &limits, shutdown.cancelled_owned()).await;
        if let Err(error) = served {
            tracing::warn!(%error, "the metrics listener stopped");
        }
    }))
}

fn metrics_router(handle: PrometheusHandle, db: Db) -> Router {
    Router::new()
        .route("/metrics", get(scrape))
        .with_state(Arc::new(Scrape {
            handle,
            db,
            kinds: Mutex::default(),
        }))
}

async fn scrape(State(scrape): State<Arc<Scrape>>) -> impl IntoResponse {
    record_pools(&scrape.db);
    record_queue(&scrape).await;
    (
        [(CONTENT_TYPE, "text/plain; version=0.0.4")],
        scrape.handle.render(),
    )
}

/// Connections open and idle in each pool, and their limit.
fn record_pools(db: &Db) {
    let pools = std::iter::once(("primary".to_owned(), db.primary())).chain(
        db.replicas()
            .iter()
            .enumerate()
            .map(|(i, pool)| (format!("replica_{i}"), pool)),
    );
    for (name, pool) in pools {
        let open = f64::from(pool.size());
        let idle = pool.num_idle() as f64;
        metrics::gauge!("moekura_db_connections", "pool" => name.clone(), "state" => "idle")
            .set(idle);
        metrics::gauge!("moekura_db_connections", "pool" => name.clone(), "state" => "in_use")
            .set(open - idle);
        metrics::gauge!("moekura_db_connections_max", "pool" => name)
            .set(f64::from(pool.options().get_max_connections()));
    }
}

/// Jobs by kind and state, and how long the oldest due job has waited.
async fn record_queue(scrape: &Scrape) {
    let health = tokio::time::timeout(
        Duration::from_secs(5),
        moekura_db::jobs::queue_health(scrape.db.primary()),
    )
    .await;
    let health = match health {
        Ok(Ok(health)) => health,
        Ok(Err(error)) => {
            tracing::warn!(%error, "could not read the job queue for metrics");
            return;
        }
        Err(_) => {
            tracing::warn!("reading the job queue for metrics timed out");
            return;
        }
    };
    let mut kinds = scrape.kinds.lock().unwrap_or_else(|e| e.into_inner());
    let gone: Vec<String> = kinds
        .iter()
        .filter(|kind| !health.iter().any(|h| &h.kind == *kind))
        .cloned()
        .collect();
    for kind in gone {
        set_queue(&kind, [0; 4], 0.0);
    }
    for kind in health {
        set_queue(
            &kind.kind,
            [kind.ready, kind.scheduled, kind.running, kind.dead],
            kind.oldest_ready_secs,
        );
        kinds.insert(kind.kind);
    }
}

fn set_queue(kind: &str, counts: [i64; 4], oldest_ready_secs: f64) {
    for (state, count) in ["ready", "scheduled", "running", "dead"]
        .into_iter()
        .zip(counts)
    {
        metrics::gauge!("moekura_jobs", "kind" => kind.to_owned(), "state" => state)
            .set(count as f64);
    }
    metrics::gauge!("moekura_jobs_oldest_ready_seconds", "kind" => kind.to_owned())
        .set(oldest_ready_secs);
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use sqlx::PgPool;
    use tower::ServiceExt;

    use super::*;

    async fn get_metrics(app: &Router) -> String {
        let response = app
            .clone()
            .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(body.to_vec()).unwrap()
    }

    fn gauge(text: &str, prefix: &str) -> f64 {
        let line = text
            .lines()
            .find(|l| l.starts_with(prefix))
            .unwrap_or_else(|| panic!("no {prefix} in\n{text}"));
        line.rsplit(' ').next().unwrap().parse().unwrap()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn scrapes_pools_and_queue(pool: PgPool) {
        sqlx::query(
            "INSERT INTO jobs (kind, run_at) VALUES \
             ('thumbnail', now() - interval '30 seconds'), \
             ('thumbnail', now() + interval '1 hour')",
        )
        .execute(&pool)
        .await
        .unwrap();
        // The only test here that installs the global recorder.
        let app = metrics_router(
            install_metrics().unwrap(),
            Db::from_pools(pool.clone(), vec![]),
        );

        let text = get_metrics(&app).await;
        let ready = r#"moekura_jobs{kind="thumbnail",state="ready"}"#;
        assert_eq!(gauge(&text, ready), 1.0);
        assert_eq!(
            gauge(&text, r#"moekura_jobs{kind="thumbnail",state="scheduled"}"#),
            1.0
        );
        assert!(
            gauge(
                &text,
                r#"moekura_jobs_oldest_ready_seconds{kind="thumbnail"}"#
            ) >= 29.0
        );
        assert!(gauge(&text, r#"moekura_db_connections_max{pool="primary"}"#) >= 1.0);
        assert!(text.contains(r#"moekura_db_connections{pool="primary",state="in_use"}"#));

        // A kind with no jobs left reads zero, not its last count.
        sqlx::query("DELETE FROM jobs")
            .execute(&pool)
            .await
            .unwrap();
        let text = get_metrics(&app).await;
        assert_eq!(gauge(&text, ready), 0.0);
        assert_eq!(
            gauge(
                &text,
                r#"moekura_jobs_oldest_ready_seconds{kind="thumbnail"}"#
            ),
            0.0
        );
    }

    #[tokio::test]
    async fn exports_spans_over_otlp_http() {
        use axum::routing::post;
        use opentelemetry::trace::{Span as _, Tracer as _};

        moekura_storage::install_crypto_provider();
        let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
        let collector = Router::new().route(
            "/collector/v1/traces",
            post(
                move |headers: axum::http::HeaderMap, body: bytes::Bytes| async move {
                    let _ = sent.send((headers, body));
                    ""
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, collector).await });

        let config = TelemetryConfig {
            otlp_endpoint: Some(format!("http://{addr}/collector").parse().unwrap()),
            ..TelemetryConfig::default()
        };
        let provider =
            tracer_provider(&config, config.otlp_endpoint.as_ref().unwrap(), "worker").unwrap();
        let mut span = provider.tracer("moekura").start("render thumbnail");
        span.end();
        Telemetry {
            traces: Some(provider),
        }
        .shutdown()
        .await;

        let (headers, body) = tokio::time::timeout(Duration::from_secs(5), received.recv())
            .await
            .expect("the span was sent")
            .unwrap();
        assert_eq!(headers[CONTENT_TYPE], "application/x-protobuf");
        let body = String::from_utf8_lossy(&body);
        assert!(body.contains("render thumbnail"), "{body}");
        assert!(body.contains("moekura.role"), "{body}");
    }
}
