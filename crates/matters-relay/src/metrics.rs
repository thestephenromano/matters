//! Redacted operational metrics (#44), in OpenMetrics text for Prometheus.
//!
//! What a relay operator needs to see degraded sync, and nothing about
//! content or who syncs:
//! - requests, by the route template that matched and the status answered;
//!   durable acknowledgements are `PUT …/records/{record}` answered 201,
//!   replays 200, and authorization failures and backpressure have their
//!   own statuses;
//! - request latency, by template;
//! - storage across every route (routes, stored records, stored record
//!   and authority bytes),
//!   refreshed from the database.
//!
//! Labels are drawn only from fixed vocabularies: the API's path templates
//! (`/v0/routes/{route}/records/{record}`, never the route itself) and
//! HTTP status codes. No route, tenant, device, record ID or digest is
//! ever a label value.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{MatchedPath, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::{exponential_buckets, Histogram};
use prometheus_client::registry::Registry;

use crate::Relay;

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct RequestLabels {
    endpoint: String,
    status: u16,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct EndpointLabels {
    endpoint: String,
}

fn latency() -> Histogram {
    // 1 ms to about 33 s.
    Histogram::new(exponential_buckets(0.001, 2.0, 16))
}

/// The relay's metrics.
pub struct Metrics {
    registry: Registry,
    requests: Family<RequestLabels, Counter>,
    latency: Family<EndpointLabels, Histogram, fn() -> Histogram>,
    routes: Gauge,
    stored_records: Gauge,
    stored_bytes: Gauge,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    pub fn new() -> Self {
        let mut registry = Registry::with_prefix("matters_relay");
        let requests = Family::<RequestLabels, Counter>::default();
        registry.register(
            "requests",
            "API requests, by matched path template and status",
            requests.clone(),
        );
        let latency =
            Family::<EndpointLabels, Histogram, fn() -> Histogram>::new_with_constructor(latency);
        registry.register(
            "request_duration_seconds",
            "API request latency, by matched path template",
            latency.clone(),
        );
        let routes = Gauge::default();
        registry.register("routes", "Provisioned routes", routes.clone());
        let stored_records = Gauge::default();
        registry.register(
            "stored_records",
            "Records stored, across every route",
            stored_records.clone(),
        );
        let stored_bytes = Gauge::default();
        registry.register(
            "stored_bytes",
            "Record and authority bytes stored, across every route",
            stored_bytes.clone(),
        );
        Self {
            registry,
            requests,
            latency,
            routes,
            stored_records,
            stored_bytes,
        }
    }

    /// Count one answered request under its path template.
    pub fn observe(&self, endpoint: &str, status: u16, elapsed: Duration) {
        self.requests
            .get_or_create(&RequestLabels {
                endpoint: endpoint.to_owned(),
                status,
            })
            .inc();
        self.latency
            .get_or_create(&EndpointLabels {
                endpoint: endpoint.to_owned(),
            })
            .observe(elapsed.as_secs_f64());
    }

    /// Read storage totals from the database.
    pub async fn refresh(&self, relay: &Relay) -> Result<(), crate::RelayError> {
        let (routes, records, bytes) = relay.totals().await?;
        self.routes.set(routes);
        self.stored_records.set(records);
        self.stored_bytes.set(bytes);
        Ok(())
    }

    /// The OpenMetrics text exposition.
    pub fn encode(&self) -> String {
        let mut out = String::new();
        prometheus_client::encoding::text::encode(&mut out, &self.registry)
            .expect("writing to a String cannot fail");
        out
    }
}

/// Middleware that observes every request the API router answers.
pub async fn track(State(metrics): State<Arc<Metrics>>, request: Request, next: Next) -> Response {
    // The template, never the concrete path: no identifiers in labels.
    let endpoint = request
        .extensions()
        .get::<MatchedPath>()
        .map_or("unmatched", MatchedPath::as_str)
        .to_owned();
    let started = Instant::now();
    let response = next.run(request).await;
    metrics.observe(&endpoint, response.status().as_u16(), started.elapsed());
    response
}

/// The metrics listener's router: `GET /metrics`, refreshing storage
/// totals first.
pub fn router(relay: Relay, metrics: Arc<Metrics>) -> axum::Router {
    axum::Router::new().route(
        "/metrics",
        axum::routing::get(move || {
            let (relay, metrics) = (relay.clone(), metrics.clone());
            async move {
                // Stale totals are better than none when the database is
                // briefly away; readiness reports that.
                let _ = metrics.refresh(&relay).await;
                (
                    [(
                        axum::http::header::CONTENT_TYPE,
                        "application/openmetrics-text; version=1.0.0; charset=utf-8",
                    )],
                    metrics.encode(),
                )
            }
        }),
    )
}
