use anyhow::Result;

/// A single time-series data point to be written to the metrics backend.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct MetricPoint {
    /// Prometheus-style metric name (e.g. "toki_token_usage_total")
    pub name: String,
    /// Label set as (key, value) pairs. All values must be pre-escaped.
    pub labels: Vec<(String, String)>,
    /// Metric value
    pub value: f64,
    /// Unix milliseconds timestamp
    pub timestamp_ms: i64,
}

/// A batch of metric points for bulk write.
#[allow(dead_code)]
pub type MetricBatch = Vec<MetricPoint>;

/// Abstraction over a time-series metrics backend (e.g. VictoriaMetrics).
pub trait MetricsBackend: Send + Sync {
    /// Write a batch of metric points. Implementations must be idempotent when
    /// the same timestamp+label combination is written multiple times.
    #[allow(dead_code)]
    fn write_batch(&self, batch: &MetricBatch) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Execute a PromQL instant query. Returns raw backend response bytes.
    fn query(&self, expr: &str, time: Option<i64>) -> impl std::future::Future<Output = Result<bytes::Bytes>> + Send;

    /// Execute a PromQL range query. Returns raw backend response bytes.
    fn query_range(
        &self,
        expr: &str,
        start: i64,
        end: i64,
        step: &str,
    ) -> impl std::future::Future<Output = Result<bytes::Bytes>> + Send;
}
