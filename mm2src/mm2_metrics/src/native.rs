use super::*;
use common::executor::{spawn, Timer};
use common::log::{LogArc, Tag};
use gstuff::Constructible;
use hdrhistogram::Histogram;
use itertools::Itertools;
use serde_json as json;
use std::collections::HashMap;
use std::fmt::Write;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Increment counter if an MmArc is not dropped yet and metrics system is initialized already.
#[macro_export]
macro_rules! mm_counter {
    ($metrics:expr, $name:expr, $value:expr) => {{
        if let Some(mut sink) = $crate::TrySink::try_sink(&$metrics) {
            sink.increment_counter($name, $value);
        }
    }};
    ($metrics:expr, $name:expr, $value:expr, $($label_key:expr => $label_val:expr),+) => {{
        if let Some(mut sink) = $crate::TrySink::try_sink(&$metrics) {
            let labels = vec![$($crate::MetricLabel::new($label_key, $label_val)),+];
            sink.increment_counter_with_labels($name, $value, labels);
        }
    }};
}

/// Update gauge if an MmArc is not dropped yet and metrics system is initialized already.
#[macro_export]
macro_rules! mm_gauge {
    ($metrics:expr, $name:expr, $value:expr) => {{
        if let Some(mut sink) = $crate::TrySink::try_sink(&$metrics) {
            sink.update_gauge($name, $value);
        }
    }};

    ($metrics:expr, $name:expr, $value:expr, $($label_key:expr => $label_val:expr),+) => {{
        if let Some(mut sink) = $crate::TrySink::try_sink(&$metrics) {
            let labels = vec![$($crate::MetricLabel::new($label_key, $label_val)),+];
            sink.update_gauge_with_labels($name, $value, labels);
        }
    }};
}

/// Pass new timing value if an MmArc is not dropped yet and metrics system is initialized already.
#[macro_export]
macro_rules! mm_timing {
    ($metrics:expr, $name:expr, $start:expr, $end:expr) => {{
        if let Some(mut sink) = $crate::TrySink::try_sink(&$metrics) {
            sink.record_timing($name, $start, $end);
        }
    }};

    ($metrics:expr, $name:expr, $start:expr, $end:expr, $($label_key:expr => $label_val:expr),+) => {{
        if let Some(mut sink) = $crate::TrySink::try_sink(&$metrics) {
            let labels = vec![$($crate::MetricLabel::new($label_key, $label_val)),+];
            sink.record_timing_with_labels($name, $start, $end, labels);
        }
    }};
}

/// Default quantiles are "min" and "max".
const QUANTILES: &[f64] = &[0.0, 1.0];

/// Significant figures used for the timing histograms (see `Histogram::new`).
const HIST_SIGFIG: u8 = 3;

/// A single metric label (key/value pair) attached to a metric sample.
///
/// Replaces the former `metrics_core::Label`; kept as a first-class public type
/// because the `mm_counter!`/`mm_gauge!`/`mm_timing!` macros construct it directly.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MetricLabel {
    key: String,
    value: String,
}

impl MetricLabel {
    pub fn new<K: Into<String>, V: Into<String>>(key: K, value: V) -> MetricLabel {
        MetricLabel {
            key: key.into(),
            value: value.into(),
        }
    }

    pub fn key(&self) -> &str { &self.key }

    pub fn value(&self) -> &str { &self.value }
}

/// Identifies a metric by its name and the (ordered) set of labels attached to it.
#[derive(Clone, Eq, Hash, PartialEq)]
struct MetricKey {
    name: String,
    labels: Vec<MetricLabel>,
}

impl MetricKey {
    fn labels_map(&self) -> HashMap<String, String> {
        self.labels
            .iter()
            .map(|label| (label.key.clone(), label.value.clone()))
            .collect()
    }

    fn tags(&self) -> Vec<Tag> {
        self.labels
            .iter()
            .map(|label| Tag {
                key: label.key.clone(),
                val: Some(label.value.clone()),
            })
            .collect()
    }
}

/// In-memory metric storage for a single `Metrics` instance.
#[derive(Default)]
struct RegistryInner {
    counters: HashMap<MetricKey, u64>,
    gauges: HashMap<MetricKey, i64>,
    histograms: HashMap<MetricKey, Histogram<u64>>,
}

/// A per-instance metrics registry. Cheap to share via `Arc`; the monotonic
/// `start` instant provides the clock used by `Sink::now`.
struct Registry {
    inner: Mutex<RegistryInner>,
    start: Instant,
}

impl Default for Registry {
    fn default() -> Self {
        Registry {
            inner: Mutex::new(RegistryInner::default()),
            start: Instant::now(),
        }
    }
}

impl Registry {
    fn collect_json(&self) -> MetricsJson {
        let inner = self.inner.lock().expect("metrics registry poisoned");
        let mut metrics = Vec::new();

        for (key, value) in inner.counters.iter() {
            metrics.push(MetricType::Counter {
                key: key.name.clone(),
                labels: key.labels_map(),
                value: *value,
            });
        }

        for (key, value) in inner.gauges.iter() {
            metrics.push(MetricType::Gauge {
                key: key.name.clone(),
                labels: key.labels_map(),
                value: *value,
            });
        }

        for (key, hist) in inner.histograms.iter() {
            let mut quantiles = hist_at_quantiles(hist, QUANTILES);
            quantiles.insert("count".into(), hist.len());
            metrics.push(MetricType::Histogram {
                key: key.name.clone(),
                labels: key.labels_map(),
                quantiles,
            });
        }

        MetricsJson { metrics }
    }

    /// Render the collected metrics in Prometheus text exposition format.
    fn collect_prometheus(&self) -> String {
        let inner = self.inner.lock().expect("metrics registry poisoned");
        let mut out = String::new();

        // Group by sanitized metric name so each `# TYPE` line is emitted once.
        let mut counters: HashMap<String, Vec<(&MetricKey, u64)>> = HashMap::new();
        for (key, value) in inner.counters.iter() {
            counters.entry(sanitize(&key.name)).or_default().push((key, *value));
        }
        for name in counters.keys().sorted() {
            let _ = writeln!(out, "# TYPE {} counter", name);
            for (key, value) in counters[name].iter().sorted_by(|a, b| a.0.labels.cmp(&b.0.labels)) {
                let _ = writeln!(out, "{}{} {}", name, render_labels(&key.labels, &[]), value);
            }
        }

        let mut gauges: HashMap<String, Vec<(&MetricKey, i64)>> = HashMap::new();
        for (key, value) in inner.gauges.iter() {
            gauges.entry(sanitize(&key.name)).or_default().push((key, *value));
        }
        for name in gauges.keys().sorted() {
            let _ = writeln!(out, "# TYPE {} gauge", name);
            for (key, value) in gauges[name].iter().sorted_by(|a, b| a.0.labels.cmp(&b.0.labels)) {
                let _ = writeln!(out, "{}{} {}", name, render_labels(&key.labels, &[]), value);
            }
        }

        let mut histograms: HashMap<String, Vec<(&MetricKey, &Histogram<u64>)>> = HashMap::new();
        for (key, hist) in inner.histograms.iter() {
            histograms.entry(sanitize(&key.name)).or_default().push((key, hist));
        }
        for name in histograms.keys().sorted() {
            let _ = writeln!(out, "# TYPE {} summary", name);
            for (key, hist) in histograms[name].iter().sorted_by(|a, b| a.0.labels.cmp(&b.0.labels)) {
                for &q in QUANTILES {
                    let quantile = format!("{}", q);
                    let value = hist.value_at_quantile(q);
                    let labels = render_labels(&key.labels, &[("quantile", &quantile)]);
                    let _ = writeln!(out, "{}{} {}", name, labels, value);
                }
                let base = render_labels(&key.labels, &[]);
                let _ = writeln!(out, "{}_count{} {}", name, base, hist.len());
            }
        }

        out
    }
}

/// Sanitizes a metric name into a Prometheus-valid identifier (`[a-zA-Z0-9_:]`).
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == ':' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Renders a Prometheus label set `{k="v",...}`, optionally with extra labels
/// appended (used to add the `quantile` label to histogram samples). Returns an
/// empty string when there are no labels at all.
fn render_labels(labels: &[MetricLabel], extra: &[(&str, &str)]) -> String {
    if labels.is_empty() && extra.is_empty() {
        return String::new();
    }
    let mut parts = Vec::with_capacity(labels.len() + extra.len());
    for label in labels {
        parts.push(format!(
            "{}=\"{}\"",
            sanitize(&label.key),
            escape_label_value(&label.value)
        ));
    }
    for (k, v) in extra {
        parts.push(format!("{}=\"{}\"", sanitize(k), escape_label_value(v)));
    }
    format!("{{{}}}", parts.join(","))
}

fn escape_label_value(value: &str) -> String { value.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n") }

/// Values of a histogram at the requested quantiles, keyed by a human label
/// (`min` for 0.0, `max` for 1.0, `p<NN>` otherwise).
fn hist_at_quantiles(hist: &Histogram<u64>, quantiles: &[f64]) -> HashMap<String, u64> {
    quantiles
        .iter()
        .map(|q| (quantile_label(*q), hist.value_at_quantile(*q)))
        .collect()
}

fn quantile_label(q: f64) -> String {
    if q == 0.0 {
        "min".to_string()
    } else if q == 1.0 {
        "max".to_string()
    } else {
        format!("p{}", (q * 100.0) as u64)
    }
}

/// Handle for sending metric samples into a `Registry`.
///
/// Cloneable and cheap; mirrors the sink API the `mm_*` macros expect.
#[derive(Clone)]
pub struct Sink {
    registry: Arc<Registry>,
}

impl Sink {
    /// Monotonically increasing timestamp in nanoseconds since registry creation,
    /// used to bracket timing measurements passed to `record_timing`.
    pub fn now(&self) -> u64 { self.registry.start.elapsed().as_nanos() as u64 }

    pub fn increment_counter(&mut self, name: &str, value: u64) {
        self.increment_counter_with_labels(name, value, Vec::new())
    }

    pub fn increment_counter_with_labels(&mut self, name: &str, value: u64, labels: Vec<MetricLabel>) {
        let key = MetricKey {
            name: name.to_string(),
            labels,
        };
        let mut inner = self.registry.inner.lock().expect("metrics registry poisoned");
        *inner.counters.entry(key).or_insert(0) += value;
    }

    pub fn update_gauge(&mut self, name: &str, value: i64) { self.update_gauge_with_labels(name, value, Vec::new()) }

    pub fn update_gauge_with_labels(&mut self, name: &str, value: i64, labels: Vec<MetricLabel>) {
        let key = MetricKey {
            name: name.to_string(),
            labels,
        };
        let mut inner = self.registry.inner.lock().expect("metrics registry poisoned");
        inner.gauges.insert(key, value);
    }

    pub fn record_timing(&mut self, name: &str, start: u64, end: u64) {
        self.record_timing_with_labels(name, start, end, Vec::new())
    }

    pub fn record_timing_with_labels(&mut self, name: &str, start: u64, end: u64, labels: Vec<MetricLabel>) {
        let value = end.saturating_sub(start);
        let key = MetricKey {
            name: name.to_string(),
            labels,
        };
        let mut inner = self.registry.inner.lock().expect("metrics registry poisoned");
        let hist = inner
            .histograms
            .entry(key)
            .or_insert_with(|| Histogram::new(HIST_SIGFIG).expect("HIST_SIGFIG is a valid significant-figures value"));
        if let Err(err) = hist.record(value) {
            log!("failed to record timing value: "(err));
        }
    }
}

pub struct Clock {
    sink: Sink,
}

impl From<Sink> for Clock {
    fn from(sink: Sink) -> Self { Clock { sink } }
}

impl ClockOps for Clock {
    fn now(&self) -> u64 { self.sink.now() }
}

pub trait TrySink {
    fn try_sink(&self) -> Option<Sink>;
}

impl TrySink for MetricsArc {
    fn try_sink(&self) -> Option<Sink> { self.0.sink().ok() }
}

impl TrySink for MetricsWeak {
    fn try_sink(&self) -> Option<Sink> {
        let metrics = MetricsArc::from_weak(self)?;
        metrics.0.sink().ok()
    }
}

#[derive(Default)]
pub struct Metrics {
    /// The metric registry. Can be initialized only once.
    registry: Constructible<Arc<Registry>>,
}

impl MetricsOps for Metrics {
    fn init(&self) -> Result<(), String> {
        if self.registry.is_some() {
            return ERR!("metrics system is initialized already");
        }

        let _ = try_s!(self.registry.pin(Arc::new(Registry::default())));

        Ok(())
    }

    fn init_with_dashboard(&self, log_state: LogWeak, record_interval: f64) -> Result<(), String> {
        self.init()?;

        let registry = self.registry.as_option().unwrap().clone();
        let exporter = TagExporter { log_state, registry };

        spawn(exporter.run(record_interval));

        Ok(())
    }

    fn clock(&self) -> Result<Clock, String> { self.sink().map(Clock::from) }

    fn collect_json(&self) -> Result<Json, String> {
        let registry = try_s!(self.try_registry());
        json::to_value(registry.collect_json()).map_err(|err| ERRL!("{}", err))
    }
}

impl Metrics {
    fn try_registry(&self) -> Result<&Arc<Registry>, String> {
        self.registry
            .as_option()
            .ok_or("metrics system is not initialized yet".into())
    }

    fn sink(&self) -> Result<Sink, String> {
        let registry = self.try_registry()?.clone();
        Ok(Sink { registry })
    }

    /// Collect the metrics in Prometheus format.
    pub fn collect_prometheus_format(&self) -> Result<String, String> {
        let registry = try_s!(self.try_registry());
        Ok(registry.collect_prometheus())
    }
}

/// Exports metrics to the log using `log::Status` in Tag format, on an interval.
struct TagExporter {
    /// Using a weak reference by default in order to avoid circular references and leaks.
    log_state: LogWeak,
    /// The registry to snapshot on each turn.
    registry: Arc<Registry>,
}

impl TagExporter {
    /// Run endless async loop.
    async fn run(self, interval: f64) {
        loop {
            Timer::sleep(interval).await;
            self.turn();
        }
    }

    /// Observe metrics and histograms and record them into the log in Tag format.
    fn turn(&self) {
        let log_state = match LogArc::from_weak(&self.log_state) {
            Some(x) => x,
            // MmCtx is dropped already
            _ => return,
        };

        log!(">>>>>>>>>> DEX metrics <<<<<<<<<");

        let inner = self.registry.inner.lock().expect("metrics registry poisoned");

        // Group counters and gauges that share the same label set into a single
        // `name=value ...` message tagged with those labels.
        let mut grouped: HashMap<Vec<MetricLabel>, Vec<(String, String)>> = HashMap::new();
        for (key, value) in inner.counters.iter() {
            grouped
                .entry(key.labels.clone())
                .or_default()
                .push((key.name.clone(), value.to_string()));
        }
        for (key, value) in inner.gauges.iter() {
            grouped
                .entry(key.labels.clone())
                .or_default()
                .push((key.name.clone(), value.to_string()));
        }

        for (labels, name_values) in grouped.iter() {
            let tags = labels_to_tags(labels);
            let message = name_values
                .iter()
                .sorted()
                .map(|(name, value)| format!("{}={}", name, value))
                .join(" ");
            log_state.log_deref_tags("", tags, &message);
        }

        for (key, hist) in inner.histograms.iter() {
            let tags = key.tags();
            let message = format!("{}: {}", key.name, hist_to_message(hist, QUANTILES));
            log_state.log_deref_tags("", tags, &message);
        }
    }
}

fn labels_to_tags(labels: &[MetricLabel]) -> Vec<Tag> {
    labels
        .iter()
        .map(|label| Tag {
            key: label.key.clone(),
            val: Some(label.value.clone()),
        })
        .collect()
}

fn hist_to_message(hist: &Histogram<u64>, quantiles: &[f64]) -> String {
    let fmt_quantiles = quantiles
        .iter()
        .map(|q| format!("{}={}", quantile_label(*q), hist.value_at_quantile(*q)))
        .join(" ");
    if fmt_quantiles.is_empty() {
        format!("count={}", hist.len())
    } else {
        format!("count={} {}", hist.len(), fmt_quantiles)
    }
}

pub mod prometheus {
    use super::*;
    use futures::future::{Future, FutureExt};
    use hyper::http::{self, header, Request, Response, StatusCode};
    use hyper::service::{make_service_fn, service_fn};
    use hyper::{Body, Server};
    use std::convert::Infallible;
    use std::net::SocketAddr;

    #[derive(Clone)]
    pub struct PrometheusCredentials {
        pub userpass: String,
    }

    pub fn spawn_prometheus_exporter(
        metrics: MetricsWeak,
        address: SocketAddr,
        shutdown_detector: impl Future<Output = ()> + 'static + Send,
        credentials: Option<PrometheusCredentials>,
    ) -> Result<(), String> {
        let make_svc = make_service_fn(move |_conn| {
            let metrics = metrics.clone();
            let credentials = credentials.clone();
            futures::future::ready(Ok::<_, Infallible>(service_fn(move |req| {
                futures::future::ready(scrape_handle(req, metrics.clone(), credentials.clone()))
            })))
        });

        let server = try_s!(Server::try_bind(&address))
            .http1_half_close(false) // https://github.com/hyperium/hyper/issues/1764
            .serve(make_svc)
            .with_graceful_shutdown(shutdown_detector);

        let server = server.then(|r| {
            if let Err(err) = r {
                log!((err));
            };
            futures::future::ready(())
        });

        spawn(server);
        Ok(())
    }

    fn scrape_handle(
        req: Request<Body>,
        metrics: MetricsWeak,
        credentials: Option<PrometheusCredentials>,
    ) -> Result<Response<Body>, http::Error> {
        fn on_error(status: StatusCode, error: String) -> Result<Response<Body>, http::Error> {
            log!((error));
            Response::builder().status(status).body(Body::empty()).map_err(|err| {
                log!((err));
                err
            })
        }

        if req.uri() != "/metrics" {
            return on_error(
                StatusCode::BAD_REQUEST,
                ERRL!("Warning Prometheus: unexpected URI {}", req.uri()),
            );
        }

        if let Some(credentials) = credentials {
            if let Err(err) = check_auth_credentials(&req, credentials) {
                return on_error(StatusCode::UNAUTHORIZED, err);
            }
        }

        let metrics = match MetricsArc::from_weak(&metrics) {
            Some(m) => m,
            _ => {
                return on_error(
                    StatusCode::BAD_REQUEST,
                    ERRL!("Warning Prometheus: metrics system unavailable"),
                )
            },
        };

        let body = match metrics.0.collect_prometheus_format() {
            Ok(body) => Body::from(body),
            _ => {
                return on_error(
                    StatusCode::BAD_REQUEST,
                    ERRL!("Warning Prometheus: metrics system is not initialized yet"),
                )
            },
        };

        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(body)
            .map_err(|err| {
                log!((err));
                err
            })
    }

    fn check_auth_credentials(req: &Request<Body>, expected: PrometheusCredentials) -> Result<(), String> {
        let header_value = req
            .headers()
            .get(header::AUTHORIZATION)
            .ok_or(ERRL!("Warning Prometheus: authorization required"))
            .and_then(|header| Ok(try_s!(header.to_str())))?;

        let expected = format!("Basic {}", base64::encode_config(&expected.userpass, base64::URL_SAFE));

        if header_value != expected {
            return Err(format!("Warning Prometheus: invalid credentials: {}", header_value));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::log::LogState;

    #[test]
    fn test_initialization() {
        let log_state = LogArc::new(LogState::in_memory());
        let metrics = MetricsArc::new();

        // metrics system is not initialized yet
        assert!(metrics.try_sink().is_none());

        metrics.init().unwrap();
        assert!(metrics.init().is_err());
        assert!(metrics.init_with_dashboard(log_state.weak(), 1.).is_err());

        assert!(metrics.try_sink().is_some());
    }

    #[test]
    fn test_collect_json() {
        let metrics = MetricsArc::new();

        metrics.init().unwrap();

        mm_counter!(metrics, "rpc.traffic.tx", 62, "coin" => "BTC");
        mm_counter!(metrics, "rpc.traffic.rx", 105, "coin" => "BTC");

        mm_counter!(metrics, "rpc.traffic.tx", 30, "coin" => "BTC");
        mm_counter!(metrics, "rpc.traffic.rx", 44, "coin" => "BTC");

        mm_counter!(metrics, "rpc.traffic.tx", 54, "coin" => "KMD");
        mm_counter!(metrics, "rpc.traffic.rx", 158, "coin" => "KMD");

        mm_gauge!(metrics, "rpc.connection.count", 3, "coin" => "KMD");

        // gauge takes the latest value for a given label set
        mm_gauge!(metrics, "rpc.connection.count", 5, "coin" => "KMD");

        let expected = json::json!({
            "metrics": [
                {
                    "key": "rpc.traffic.tx",
                    "labels": { "coin": "BTC" },
                    "type": "counter",
                    "value": 92
                },
                {
                    "key": "rpc.traffic.rx",
                    "labels": { "coin": "BTC" },
                    "type": "counter",
                    "value": 149
                },
                {
                    "key": "rpc.traffic.tx",
                    "labels": { "coin": "KMD" },
                    "type": "counter",
                    "value": 54
                },
                {
                    "key": "rpc.traffic.rx",
                    "labels": { "coin": "KMD" },
                    "type": "counter",
                    "value": 158
                },
                {
                    "key": "rpc.connection.count",
                    "labels": { "coin": "KMD" },
                    "type": "gauge",
                    "value": 5
                }
            ]
        });

        let mut actual = metrics.collect_json().unwrap();

        let actual = actual["metrics"].as_array_mut().unwrap();
        for expected in expected["metrics"].as_array().unwrap() {
            let index = actual
                .iter()
                .position(|metric| metric == expected)
                .unwrap_or_else(|| panic!("Couldn't find expected metric: {:?} in {:?}", expected, actual));
            actual.remove(index);
        }

        assert!(
            actual.is_empty(),
            "More metrics collected than expected. Excess metrics: {:?}",
            actual
        );
    }
}
