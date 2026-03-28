use anyhow::{Context, Result};
use bytes::Bytes;

use super::backend::{MetricBatch, MetricsBackend};

pub struct VictoriaMetrics {
    base_url: String,
    client: ureq::Agent,
}

impl VictoriaMetrics {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            client: ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(10))
                .build(),
        }
    }

    /// Delete all VM series matching a provider label (used on schema mismatch reset).
    pub async fn delete_user_series(&self, provider: &str) -> Result<()> {
        let selector = format!("{{provider=\"{provider}\"}}");
        let url = format!(
            "{}/api/v1/admin/tsdb/delete_series?match[]={}",
            self.base_url,
            urlencoding::encode(&selector),
        );
        let client = self.client.clone();
        let base_url = self.base_url.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = client.post(&url).call() {
                tracing::warn!("VM delete_series failed for {base_url}: {e}");
            }
        })
        .await
        .context("spawn_blocking panicked")?;
        Ok(())
    }

    #[allow(dead_code)]
    fn format_prometheus_text(batch: &MetricBatch) -> String {
        let mut out = String::new();
        for pt in batch {
            // metric_name{label="value",...} value timestamp_ms
            out.push_str(&pt.name);
            if !pt.labels.is_empty() {
                out.push('{');
                for (i, (k, v)) in pt.labels.iter().enumerate() {
                    if i > 0 { out.push(','); }
                    out.push_str(k);
                    out.push_str("=\"");
                    out.push_str(v); // values are already escaped
                    out.push('"');
                }
                out.push('}');
            }
            out.push(' ');
            out.push_str(&pt.value.to_string());
            out.push(' ');
            out.push_str(&pt.timestamp_ms.to_string());
            out.push('\n');
        }
        out
    }
}

impl MetricsBackend for VictoriaMetrics {
    async fn write_batch(&self, batch: &MetricBatch) -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let body = Self::format_prometheus_text(batch);
        let url = format!("{}/api/v1/import/prometheus", self.base_url);

        let base_url = self.base_url.clone();
        let client = self.client.clone();
        let body_clone = body.clone();

        tokio::task::spawn_blocking(move || {
            client
                .post(&url)
                .set("Content-Type", "text/plain")
                .send_string(&body_clone)
                .with_context(|| format!("VM write_batch failed: {base_url}"))
        })
        .await
        .context("spawn_blocking panicked")??;

        Ok(())
    }

    async fn query(&self, expr: &str, time: Option<i64>) -> Result<Bytes> {
        let mut url = format!("{}/api/v1/query?query={}", self.base_url,
            urlencoding::encode(expr));
        if let Some(t) = time {
            url.push_str(&format!("&time={t}"));
        }

        let base_url = self.base_url.clone();
        let client = self.client.clone();
        let url_clone = url.clone();

        let body = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
            let resp = client.get(&url_clone)
                .call()
                .with_context(|| format!("VM query failed: {base_url}"))?;
            let mut buf = Vec::new();
            resp.into_reader().read_to_end(&mut buf)
                .context("reading VM response")?;
            Ok(buf)
        })
        .await
        .context("spawn_blocking panicked")??;

        Ok(Bytes::from(body))
    }

    async fn query_range(&self, expr: &str, start: i64, end: i64, step: &str) -> Result<Bytes> {
        let url = format!(
            "{}/api/v1/query_range?query={}&start={}&end={}&step={}",
            self.base_url,
            urlencoding::encode(expr),
            start,
            end,
            step,
        );

        let base_url = self.base_url.clone();
        let client = self.client.clone();

        let body = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
            let resp = client.get(&url)
                .call()
                .with_context(|| format!("VM query_range failed: {base_url}"))?;
            let mut buf = Vec::new();
            resp.into_reader().read_to_end(&mut buf)
                .context("reading VM response")?;
            Ok(buf)
        })
        .await
        .context("spawn_blocking panicked")??;

        Ok(Bytes::from(body))
    }
}
