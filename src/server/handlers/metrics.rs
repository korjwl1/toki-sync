use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use super::super::http::{AppError, AppState, extract_jwt};
use crate::metrics::backend::MetricsBackend;

// ─── PromQL proxy ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct QueryParams {
    pub query: String,
    pub time: Option<i64>,
    pub scope: Option<String>,
}

#[derive(Deserialize)]
pub struct QueryRangeParams {
    pub query: String,
    pub start: i64,
    pub end: i64,
    pub step: Option<String>,
    pub scope: Option<String>,
}

/// Toki query params — same interface as local daemon REPORT protocol.
/// Query is toki PromQL (usage{}, events{}, cost{}), start/end are epoch seconds or date strings.
/// With step: range query returning time-bucketed results (for charts).
/// Without step: instant query returning single aggregated result (for stat panels).
#[derive(Deserialize)]
pub struct TokiQueryParams {
    pub query: String,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
    #[serde(default)]
    pub step: Option<String>,
    pub scope: Option<String>,
}

// ─── Scope types ─────────────────────────────────────────────────────────────

enum Scope {
    Self_,
    Team(String),
    All,
    Invalid,
}

fn parse_scope(s: &str) -> Scope {
    match s {
        "self" => Scope::Self_,
        "all" => Scope::All,
        s if s.starts_with("team:") => {
            let id = s.strip_prefix("team:").unwrap_or("");
            if id.is_empty() { Scope::Invalid } else { Scope::Team(id.to_string()) }
        }
        _ => Scope::Invalid,
    }
}

/// Resolve the scope parameter into an injected PromQL query.
/// Returns the rewritten query string with appropriate label filters.
async fn resolve_scope(
    state: &AppState,
    query: &str,
    user_id: &str,
    requested_scope: &str,
) -> Result<String, AppError> {
    let max_scope = state.dynamic_settings.max_query_scope().await;
    let is_admin = state.db.user_is_admin(user_id).await.map_err(AppError::internal)?;

    if is_admin {
        // Admin bypasses all restrictions
        return Ok(query.to_string());
    }

    match parse_scope(requested_scope) {
        Scope::Self_ => {
            // Always allowed
            Ok(inject_label_filter(query, &format!("user=\"{}\"", escape_label_value(user_id))))
        }
        Scope::Team(team_id) => {
            // Check: max_scope must be "team" or "all"
            if max_scope == "self" {
                return Err(AppError::forbidden("team scope not enabled by server administrator"));
            }
            // Check: user must be member of this team
            let role = state.db.get_team_member_role(&team_id, user_id).await.map_err(AppError::internal)?;
            if role.is_none() {
                return Err(AppError::forbidden("not a member of this team"));
            }
            // Get team member IDs and build regex
            let members = state.db.list_team_members(&team_id).await.map_err(AppError::internal)?;
            let user_ids: Vec<String> = members.iter().map(|m| escape_label_value(&m.user_id)).collect();
            let regex = user_ids.join("|");
            Ok(inject_label_filter(query, &format!("user=~\"{}\"", regex)))
        }
        Scope::All => {
            // Check: max_scope must be "all"
            if max_scope != "all" {
                return Err(AppError::forbidden("global scope not enabled by server administrator"));
            }
            // No label injection -- full access
            Ok(query.to_string())
        }
        Scope::Invalid => {
            Err(AppError {
                status: StatusCode::BAD_REQUEST,
                message: "invalid scope: use 'self', 'team:<id>', or 'all'".into(),
            })
        }
    }
}

pub async fn promql_query(
    headers: HeaderMap,
    State(state): State<AppState>,
    Query(params): Query<QueryParams>,
) -> Result<Response, AppError> {
    let claims = extract_jwt(&headers, &state.jwt)?;
    let requested_scope = params.scope.as_deref().unwrap_or("self");
    let scoped = resolve_scope(&state, &params.query, &claims.sub, requested_scope).await?;

    let rewritten = rewrite_toki_query(&scoped);
    let result = if rewritten.needs_cost_compute {
        // cost{} → fetch per-type tokens from VM, apply pricing server-side
        let vm_result = state.vm.query(&rewritten.vm_query, params.time).await.map_err(AppError::bad_gateway)?;
        let computed = compute_cost_from_vm_response(&vm_result, &state.pricing, &rewritten.original_query)?;
        bytes::Bytes::from(computed)
    } else {
        state.vm.query(&rewritten.vm_query, params.time).await.map_err(AppError::bad_gateway)?
    };

    Ok((
        StatusCode::OK,
        [("Content-Type", "application/json")],
        result,
    ).into_response())
}

pub async fn promql_query_range(
    headers: HeaderMap,
    State(state): State<AppState>,
    Query(params): Query<QueryRangeParams>,
) -> Result<Response, AppError> {
    let claims = extract_jwt(&headers, &state.jwt)?;
    let requested_scope = params.scope.as_deref().unwrap_or("self");
    let scoped = resolve_scope(&state, &params.query, &claims.sub, requested_scope).await?;

    let rewritten = rewrite_toki_query(&scoped);
    let step = params.step.as_deref().unwrap_or("60s");
    let result = if rewritten.needs_cost_compute {
        let vm_result = state.vm.query_range(&rewritten.vm_query, params.start, params.end, step)
            .await.map_err(AppError::bad_gateway)?;
        let computed = compute_cost_from_vm_response(&vm_result, &state.pricing, &rewritten.original_query)?;
        bytes::Bytes::from(computed)
    } else {
        state.vm.query_range(&rewritten.vm_query, params.start, params.end, step)
            .await.map_err(AppError::bad_gateway)?
    };

    Ok((
        StatusCode::OK,
        [("Content-Type", "application/json")],
        result,
    ).into_response())
}

/// Toki query endpoint: returns toki-format JSON identical to `toki query --output-format json`.
///
/// With step param: range query → time-bucketed results (chart panels)
/// Without step: instant query → single aggregated result (stat panels)
///
/// Response format matches local CLI exactly:
/// ```json
/// {"providers": {"claude_code": [{"period": "...", "usage_per_models": [{...}]}]}}
/// ```
pub async fn toki_query(
    headers: HeaderMap,
    State(state): State<AppState>,
    Query(params): Query<TokiQueryParams>,
) -> Result<Response, AppError> {
    let claims = extract_jwt(&headers, &state.jwt)?;
    let requested_scope = params.scope.as_deref().unwrap_or("self");
    let scoped = resolve_scope(&state, &params.query, &claims.sub, requested_scope).await?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let start_ts = params.start.as_deref()
        .map(|s| parse_toki_time(s, false))
        .unwrap_or(0);
    let end_ts = params.end.as_deref()
        .map(|s| parse_toki_time(s, true))
        .unwrap_or(now);

    let rewritten = rewrite_toki_query(&scoped);
    let is_range = params.step.is_some();

    let step_secs: i64 = params.step.as_deref()
        .map(|s| parse_duration_secs(s))
        .unwrap_or(3600);

    // Query VM at hourly granularity with sum_over_time[3600s] wrapper.
    // Each eval point covers exactly one hour bucket (no stale data repetition).
    // Server re-buckets hourly results to requested step using floor(ts/step)*step.
    let vm_bytes = if is_range {
        // Replace user's range vector with [3600s] for hourly granularity
        let hourly_query = replace_range_vector(&rewritten.vm_query, 3600);
        state.vm.query_range(&hourly_query, start_ts, end_ts, "3600")
            .await.map_err(AppError::bad_gateway)?
    } else {
        // Instant: single value covering start→end
        let range_secs = (end_ts - start_ts).max(1);
        let vm_query = if rewritten.vm_query.contains('(') {
            replace_range_vector(&rewritten.vm_query, range_secs)
        } else {
            let range_str = format!("{}s", range_secs);
            format!("sum by (model) (sum_over_time({}[{}]))", rewritten.vm_query, range_str)
        };
        state.vm.query(&vm_query, Some(end_ts))
            .await.map_err(AppError::bad_gateway)?
    };

    // Convert VM Prometheus JSON → toki JSON format
    let step_secs: i64 = params.step.as_deref()
        .map(|s| parse_duration_secs(s))
        .unwrap_or(3600);
    let toki_json = vm_response_to_toki_json(
        &vm_bytes, &state.pricing, rewritten.needs_cost_compute, rewritten.is_events, step_secs,
    )?;

    Ok((
        StatusCode::OK,
        [("Content-Type", "application/json")],
        toki_json,
    ).into_response())
}

/// Convert Prometheus JSON response to toki-format JSON.
/// Output matches `toki query --output-format json` exactly.
fn vm_response_to_toki_json(
    vm_bytes: &[u8],
    pricing: &crate::pricing::PricingTable,
    is_cost: bool,
    is_events: bool,
    step_secs: i64,
) -> Result<Vec<u8>, AppError> {
    let vm: serde_json::Value = serde_json::from_slice(vm_bytes)
        .map_err(|e| AppError::internal(anyhow::anyhow!("invalid VM response: {e}")))?;

    if vm["status"].as_str() != Some("success") {
        return Ok(vm_bytes.to_vec());
    }

    let result_type = vm["data"]["resultType"].as_str().unwrap_or("vector");
    let results = vm["data"]["result"].as_array()
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("no result array")))?;

    // Collect per-(timestamp, model) token data
    use std::collections::BTreeMap;

    struct ModelBucket {
        input: u64, output: u64, cache_create: u64, cache_read: u64,
        events: u64, cost_usd: Option<f64>,
    }
    impl Default for ModelBucket {
        fn default() -> Self {
            ModelBucket { input: 0, output: 0, cache_create: 0, cache_read: 0, events: 0, cost_usd: None }
        }
    }

    // key: (timestamp_str, model) → ModelBucket
    let mut buckets: BTreeMap<(String, String), ModelBucket> = BTreeMap::new();

    for r in results {
        let metric = r["metric"].as_object().unwrap_or(&serde_json::Map::new()).clone();
        let model = metric.get("model")
            .or_else(|| metric.get("project"))
            .or_else(|| metric.get("session"))
            .and_then(|v| v.as_str())
            .unwrap_or("(total)")
            .to_string();
        let token_type = metric.get("type").and_then(|v| v.as_str());
        let toki_metric = metric.get("__toki_metric__").and_then(|v| v.as_str());

        let points: Vec<(String, f64)> = if result_type == "matrix" {
            r["values"].as_array().map(|vals| {
                vals.iter().filter_map(|pair| {
                    let raw_ts = pair[0].as_f64()? as i64;
                    // Raw hourly data point timestamp = hour start.
                    // Floor to requested step, matching local daemon's
                    // floor(event_ts / step) * step bucketing.
                    let floored = (raw_ts / step_secs) * step_secs;
                    let ts = chrono::DateTime::from_timestamp(floored, 0)
                        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S").to_string())?;
                    let val: f64 = pair[1].as_str()?.parse().ok()?;
                    Some((ts, val))
                }).collect()
            }).unwrap_or_default()
        } else {
            // vector: single value
            let ts = r["value"][0].as_f64().map(|t| {
                chrono::DateTime::from_timestamp(t as i64, 0)
                    .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S").to_string())
                    .unwrap_or_default()
            }).unwrap_or_default();
            let val: f64 = r["value"][1].as_str().and_then(|s| s.parse().ok()).unwrap_or(0.0);
            vec![(ts, val)]
        };

        for (ts, val) in points {
            let bucket = buckets.entry((ts, model.clone())).or_default();
            if toki_metric == Some("cost") || is_cost {
                // Cost query: compute from per-type tokens
                if let Some(tt) = token_type {
                    let uval = val.round() as u64;
                    match tt {
                        "input" => bucket.input += uval,
                        "output" => bucket.output += uval,
                        "cache_create" => bucket.cache_create += uval,
                        "cache_read" => bucket.cache_read += uval,
                        _ => {}
                    }
                } else {
                    // Already computed cost value
                    *bucket.cost_usd.get_or_insert(0.0) += val;
                }
            } else if is_events {
                // Events query: value is event count
                let uval = val.round() as u64;
                bucket.events += uval;
            } else {
                // Usage query: per-type token breakdown
                let uval = val.round() as u64;
                match token_type {
                    Some("input") => bucket.input += uval,
                    Some("output") => bucket.output += uval,
                    Some("cache_create") => bucket.cache_create += uval,
                    Some("cache_read") => bucket.cache_read += uval,
                    _ => bucket.input += uval,
                }
            }
        }
    }

    // For cost queries: compute cost from tokens if not already computed
    if is_cost {
        for ((_, model), bucket) in &mut buckets {
            if bucket.cost_usd.is_none() {
                bucket.cost_usd = pricing.cost(model, bucket.input, bucket.output,
                    bucket.cache_create, bucket.cache_read);
            }
        }
    }

    // Build toki JSON: group by period
    let mut periods: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
    for ((ts, model), bucket) in &buckets {
        let total = bucket.input + bucket.output + bucket.cache_create + bucket.cache_read;
        let mut entry = serde_json::json!({
            "model": model,
            "input_tokens": bucket.input,
            "output_tokens": bucket.output,
            "cache_creation_input_tokens": bucket.cache_create,
            "cache_read_input_tokens": bucket.cache_read,
            "total_tokens": total,
            "events": bucket.events,
        });
        if let Some(cost) = bucket.cost_usd.or_else(|| pricing.cost(model, bucket.input, bucket.output, bucket.cache_create, bucket.cache_read)) {
            entry["cost_usd"] = serde_json::json!(cost);
        }
        let period_key = format!("{}|{}", ts, model);
        periods.entry(period_key).or_default().push(entry);
    }

    let data: Vec<serde_json::Value> = periods.into_iter().map(|(period, models)| {
        serde_json::json!({
            "period": period,
            "usage_per_models": models,
        })
    }).collect();

    let output = serde_json::json!({
        "providers": {
            "claude_code": data,
        }
    });

    serde_json::to_vec(&output)
        .map_err(|e| AppError::internal(anyhow::anyhow!("json serialize: {e}")))
}

/// Parse toki time string: epoch seconds, YYYYMMDD, or YYYYMMDDhhmmss
fn parse_toki_time(s: &str, is_end: bool) -> i64 {
    // Try epoch seconds first
    if let Ok(ts) = s.parse::<i64>() {
        return ts;
    }
    // YYYYMMDD
    if s.len() == 8 {
        if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y%m%d") {
            let time = if is_end {
                d.and_hms_opt(23, 59, 59).unwrap()
            } else {
                d.and_hms_opt(0, 0, 0).unwrap()
            };
            return time.and_utc().timestamp();
        }
    }
    // YYYYMMDDhhmmss
    if s.len() == 14 {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y%m%d%H%M%S") {
            return dt.and_utc().timestamp();
        }
    }
    0
}

/// Replace range vector durations [Xd/h/m/s/w/y] with [Ns] where N=range_secs.
/// Parse duration string: "86400", "86400s", "24h", "1d", "1h30m" → seconds.
fn parse_duration_secs(s: &str) -> i64 {
    // Try plain number (seconds)
    if let Ok(n) = s.parse::<i64>() { return n; }
    if let Ok(n) = s.trim_end_matches('s').parse::<i64>() { return n; }

    let mut total = 0i64;
    let mut num_buf = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            num_buf.push(c);
        } else {
            let n: i64 = num_buf.parse().unwrap_or(0);
            num_buf.clear();
            match c {
                'd' => total += n * 86400,
                'h' => total += n * 3600,
                'm' => total += n * 60,
                's' => total += n,
                'w' => total += n * 604800,
                'y' => total += n * 31536000,
                _ => {}
            }
        }
    }
    if total == 0 { 3600 } else { total }
}

/// Extract the bare metric selector from a PromQL expression.
/// "sum by (model, type) (sum_over_time(toki_tokens_total[3600s]))" → "toki_tokens_total"
/// This lets us query raw data points and aggregate server-side.
fn extract_metric_selector(query: &str) -> String {
    // Find metric name: toki_*
    if let Some(start) = query.find("toki_") {
        let end = query[start..].find(|c: char| !c.is_alphanumeric() && c != '_')
            .map(|i| start + i).unwrap_or(query.len());
        let metric = &query[start..end];
        // Append any label filter {..} that follows
        let rest = &query[end..];
        if rest.starts_with('{') {
            if let Some(close) = rest.find('}') {
                return format!("{}{}", metric, &rest[..=close]);
            }
        }
        return metric.to_string();
    }
    query.to_string()
}

fn replace_range_vector(query: &str, range_secs: i64) -> String {
    let replacement = format!("{}s", range_secs);
    let mut result = String::with_capacity(query.len());
    let mut chars = query.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '[' {
            let mut content = String::new();
            while let Some(&nc) = chars.peek() {
                if nc == ']' { chars.next(); break; }
                content.push(chars.next().unwrap());
            }
            let is_duration = !content.is_empty()
                && content.chars().last().map_or(false, |c| "smhdwy".contains(c))
                && content.chars().all(|c| c.is_ascii_digit() || "smhdwy".contains(c));
            if is_duration {
                result.push_str(&format!("[{}]", replacement));
            } else {
                result.push_str(&format!("[{}]", content));
            }
        } else {
            result.push(c);
        }
    }
    result
}

// ─── Toki PromQL → VM PromQL translation ─────────────────────────────────────
//
// Virtual metrics: usage{}, events{}, cost{}
// These don't exist in VM — translated here.
//
// usage{}  → toki_usage_total (pre-summed tokens, no type dimension)
// events{} → toki_events_total (1 per API call)
// cost{}   → server-side compute: fetch toki_tokens_total per-type, multiply by pricing
//
// Standard PromQL (toki_tokens_total, toki_events_total, etc.) passes through as-is.

struct RewriteResult {
    vm_query: String,
    original_query: String,
    needs_cost_compute: bool,
    is_events: bool,
}

fn rewrite_toki_query(query: &str) -> RewriteResult {
    use regex::Regex;

    let is_cost = query.contains("cost{") || query.contains("cost[");
    let is_events = query.contains("events{") || query.contains("events[");
    let is_usage = query.contains("usage{") || query.contains("usage[");

    // Standard PromQL pass-through (no virtual metric)
    if !is_cost && !is_events && !is_usage {
        return RewriteResult {
            vm_query: query.to_string(),
            original_query: query.to_string(),
            needs_cost_compute: false,
            is_events: false,
        };
    }

    let mut result = query.to_string();

    if is_cost {
        // cost{} → fetch per-type tokens for server-side pricing computation.
        // Rewrite to toki_tokens_total with type in by() so we get per-type breakdown.
        let re = Regex::new(r"cost\{([^}]*)\}").unwrap();
        result = re.replace_all(&result, "toki_tokens_total{$1}").to_string();
        let re2 = Regex::new(r"toki_tokens_total\{\}").unwrap();
        result = re2.replace_all(&result, "toki_tokens_total").to_string();
        let re3 = Regex::new(r"cost\[").unwrap();
        result = re3.replace_all(&result, "toki_tokens_total[").to_string();

        // Inject `type` into by() for per-type breakdown (needed for pricing)
        let by_re = Regex::new(r"by\s*\(([^)]*)\)").unwrap();
        result = by_re.replace_all(&result, |caps: &regex::Captures| {
            let inner = &caps[1];
            if inner.contains("type") {
                format!("by ({})", inner)
            } else {
                format!("by ({}, type)", inner)
            }
        }).to_string();
    } else if is_events {
        // events{} → toki_events_total (1:1)
        let re = Regex::new(r"events\{([^}]*)\}").unwrap();
        result = re.replace_all(&result, "toki_events_total{$1}").to_string();
        let re2 = Regex::new(r"toki_events_total\{\}").unwrap();
        result = re2.replace_all(&result, "toki_events_total").to_string();
        let re3 = Regex::new(r"events\[").unwrap();
        result = re3.replace_all(&result, "toki_events_total[").to_string();
    } else {
        // usage{} → toki_tokens_total with type injection for per-type breakdown
        let re = Regex::new(r"usage\{([^}]*)\}").unwrap();
        result = re.replace_all(&result, "toki_tokens_total{$1}").to_string();
        let re2 = Regex::new(r"toki_tokens_total\{\}").unwrap();
        result = re2.replace_all(&result, "toki_tokens_total").to_string();
        let re3 = Regex::new(r"usage\[").unwrap();
        result = re3.replace_all(&result, "toki_tokens_total[").to_string();

        // Inject type into by() for per-type breakdown
        let by_re = Regex::new(r"by\s*\(([^)]*)\)").unwrap();
        result = by_re.replace_all(&result, |caps: &regex::Captures| {
            let inner = &caps[1];
            if inner.contains("type") {
                format!("by ({})", inner)
            } else {
                format!("by ({}, type)", inner)
            }
        }).to_string();
    }

    // increase() → sum_over_time() (VM stores gauge data, not counters)
    let inc_re = Regex::new(r"increase\(").unwrap();
    result = inc_re.replace_all(&result, "sum_over_time(").to_string();

    RewriteResult {
        vm_query: result,
        original_query: query.to_string(),
        needs_cost_compute: is_cost,
        is_events,
    }
}

/// Compute cost from VM per-type token response.
///
/// VM returns series with `type` and `model` labels. We:
/// 1. Parse the Prometheus JSON response
/// 2. Group by all labels EXCEPT `type`
/// 3. For each group, multiply each type's value by its pricing rate
/// 4. Sum into a single cost value per group
/// 5. Return a Prometheus-format JSON response with `type` label removed
fn compute_cost_from_vm_response(
    vm_bytes: &[u8],
    pricing: &crate::pricing::PricingTable,
    _original_query: &str,
) -> Result<Vec<u8>, AppError> {
    let vm_resp: serde_json::Value = serde_json::from_slice(vm_bytes)
        .map_err(|e| AppError::internal(anyhow::anyhow!("invalid VM response: {e}")))?;

    let status = vm_resp["status"].as_str().unwrap_or("error");
    if status != "success" {
        // Pass through error responses
        return Ok(vm_bytes.to_vec());
    }

    let result_type = vm_resp["data"]["resultType"].as_str().unwrap_or("vector");
    let results = match vm_resp["data"]["result"].as_array() {
        Some(r) => r,
        None => return Ok(vm_bytes.to_vec()),
    };

    // Group results by (all labels except type) → accumulate cost
    // Key: sorted label pairs (excluding type), Value: (cost, values/value for time series)
    use std::collections::BTreeMap;

    match result_type {
        "vector" => {
            // Instant query: each result has { metric: {}, value: [ts, "val"] }
            let mut cost_map: BTreeMap<String, (BTreeMap<String, String>, f64, serde_json::Value)> = BTreeMap::new();

            for r in results {
                let metric = r["metric"].as_object().unwrap_or(&serde_json::Map::new()).clone();
                let model = metric.get("model").and_then(|v| v.as_str()).unwrap_or("");
                let token_type = metric.get("type").and_then(|v| v.as_str()).unwrap_or("");
                let val: f64 = r["value"][1].as_str().and_then(|s| s.parse().ok()).unwrap_or(0.0);

                // Build group key (all labels except type and msg)
                let mut group_labels: BTreeMap<String, String> = BTreeMap::new();
                for (k, v) in &metric {
                    if k != "type" && k != "msg" && k != "__name__" {
                        group_labels.insert(k.clone(), v.as_str().unwrap_or("").to_string());
                    }
                }
                let group_key = format!("{:?}", group_labels);

                // Look up per-token-type price for this model
                let token_cost = pricing.get(model).map(|p| {
                    match token_type {
                        "input" => val * p.input_cost_per_token,
                        "output" => val * p.output_cost_per_token,
                        "cache_create" => val * p.cache_creation_input_token_cost.unwrap_or(0.0),
                        "cache_read" => val * p.cache_read_input_token_cost.unwrap_or(0.0),
                        _ => 0.0,
                    }
                }).unwrap_or(0.0);

                let ts = r["value"][0].clone();
                let entry = cost_map.entry(group_key).or_insert_with(|| (group_labels.clone(), 0.0, ts));
                entry.1 += token_cost;
            }

            // Build response — add __toki_metric__: "cost" so clients know this is USD, not tokens
            let result_arr: Vec<serde_json::Value> = cost_map.values().map(|(labels, cost, ts)| {
                let mut metric: serde_json::Map<String, serde_json::Value> = labels.iter()
                    .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                    .collect();
                metric.insert("__toki_metric__".to_string(), serde_json::Value::String("cost".to_string()));
                serde_json::json!({
                    "metric": metric,
                    "value": [ts, format!("{}", cost)],
                })
            }).collect();

            let resp = serde_json::json!({
                "status": "success",
                "data": { "resultType": "vector", "result": result_arr }
            });
            serde_json::to_vec(&resp).map_err(|e| AppError::internal(anyhow::anyhow!("json serialize: {e}")))
        }
        "matrix" => {
            // Range query: each result has { metric: {}, values: [[ts, "val"], ...] }
            // Group by labels, and for each timestamp multiply token value by price
            let mut cost_map: BTreeMap<String, (BTreeMap<String, String>, BTreeMap<String, f64>)> = BTreeMap::new();
            // key → (labels, ts_string → accumulated_cost)

            for r in results {
                let metric = r["metric"].as_object().unwrap_or(&serde_json::Map::new()).clone();
                let model = metric.get("model").and_then(|v| v.as_str()).unwrap_or("");
                let token_type = metric.get("type").and_then(|v| v.as_str()).unwrap_or("");

                let mut group_labels: BTreeMap<String, String> = BTreeMap::new();
                for (k, v) in &metric {
                    if k != "type" && k != "msg" && k != "__name__" {
                        group_labels.insert(k.clone(), v.as_str().unwrap_or("").to_string());
                    }
                }
                let group_key = format!("{:?}", group_labels);

                let price_fn = pricing.get(model).map(|p| {
                    match token_type {
                        "input" => p.input_cost_per_token,
                        "output" => p.output_cost_per_token,
                        "cache_create" => p.cache_creation_input_token_cost.unwrap_or(0.0),
                        "cache_read" => p.cache_read_input_token_cost.unwrap_or(0.0),
                        _ => 0.0,
                    }
                }).unwrap_or(0.0);

                let values = r["values"].as_array();
                if let Some(vals) = values {
                    let entry = cost_map.entry(group_key).or_insert_with(|| (group_labels.clone(), BTreeMap::new()));
                    for point in vals {
                        let ts_key = point[0].to_string(); // preserves exact timestamp
                        let val: f64 = point[1].as_str().and_then(|s| s.parse().ok()).unwrap_or(0.0);
                        *entry.1.entry(ts_key).or_insert(0.0) += val * price_fn;
                    }
                }
            }

            let result_arr: Vec<serde_json::Value> = cost_map.values().map(|(labels, ts_costs)| {
                let mut metric: serde_json::Map<String, serde_json::Value> = labels.iter()
                    .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                    .collect();
                metric.insert("__toki_metric__".to_string(), serde_json::Value::String("cost".to_string()));
                let values: Vec<serde_json::Value> = ts_costs.iter().map(|(ts, cost)| {
                    let ts_val: serde_json::Value = serde_json::from_str(ts).unwrap_or(serde_json::Value::Null);
                    serde_json::json!([ts_val, format!("{}", cost)])
                }).collect();
                serde_json::json!({
                    "metric": metric,
                    "values": values,
                })
            }).collect();

            let resp = serde_json::json!({
                "status": "success",
                "data": { "resultType": "matrix", "result": result_arr }
            });
            serde_json::to_vec(&resp).map_err(|e| AppError::internal(anyhow::anyhow!("json serialize: {e}")))
        }
        _ => Ok(vm_bytes.to_vec()),
    }
}

// ─── Label injection ─────────────────────────────────────────────────────────

/// Escape a PromQL label value: backslash and double-quote must be escaped.
pub fn escape_label_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"'  => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c    => out.push(c),
        }
    }
    out
}

/// Inject `user="<escaped>"` into a PromQL expression by rewriting the
/// metric selector. This prevents a user from querying another user's data.
///
/// Strategy: find the first `{` in each metric selector and insert the label
/// before the first existing label. If no `{` exists, find each bare metric
/// name token and inject `{user="..."}` after it.
///
/// NOTE: This is a string-level transformation, not a full PromQL parse.
/// Handles the common patterns used by toki-monitor and toki CLI:
///   - bare metric:              `toki_tokens_total`
///   - aggregation argument:     `sum(toki_tokens_total)`
///   - range vector:             `rate(toki_tokens_total[5m])`
///   - aggregation with groupby: `sum by (model)(toki_tokens_total)`
///
/// All injected values are escaped; no string interpolation from user input.
#[allow(dead_code)]
pub fn inject_user_label(expr: &str, user_id: &str) -> String {
    let escaped = escape_label_value(user_id);
    inject_label_filter(expr, &format!("user=\"{escaped}\""))
}

/// Inject an arbitrary label filter (e.g. `user="alice"` or `user=~"a|b"`) into
/// a PromQL expression by rewriting metric selectors. This is the shared core
/// used by both `inject_user_label` and team query injection.
///
/// Strategy: find the first `{` in each metric selector and insert the label
/// before the first existing label. If no `{` exists, find each bare metric
/// name token and inject `{<injection>}` after it.
///
/// NOTE: This is a string-level transformation, not a full PromQL parse.
/// Handles the common patterns used by toki-monitor and toki CLI.
pub fn inject_label_filter(expr: &str, injection: &str) -> String {
    let selector = format!("{{{injection}}}");

    // ── Path A: expression already has `{...}` selectors ──────────────────
    if expr.contains('{') {
        let mut result = String::with_capacity(expr.len() + injection.len() + 10);
        let mut chars = expr.chars().peekable();

        while let Some(&ch) = chars.peek() {
            // Skip string literals
            if ch == '"' || ch == '\'' || ch == '`' {
                let quote = ch;
                result.push(chars.next().unwrap());
                while let Some(&c) = chars.peek() {
                    result.push(chars.next().unwrap());
                    if c == '\\' {
                        if let Some(&_next) = chars.peek() {
                            result.push(chars.next().unwrap());
                        }
                    } else if c == quote {
                        break;
                    }
                }
                continue;
            }
            // Inject into `{` label selectors
            if ch == '{' {
                result.push(chars.next().unwrap());
                if chars.peek() == Some(&'}') {
                    result.push_str(injection);
                } else {
                    result.push_str(injection);
                    result.push(',');
                }
                continue;
            }
            result.push(chars.next().unwrap());
        }
        return result;
    }

    // ── Path B: no `{` -- inject after bare metric name tokens ───────────

    const KEYWORDS: &[&str] = &[
        "sum", "min", "max", "avg", "count", "stddev", "stdvar",
        "bottomk", "topk", "count_values", "quantile",
        "rate", "irate", "increase", "delta", "idelta",
        "resets", "changes", "deriv", "predict_linear", "holt_winters",
        "label_replace", "label_join", "histogram_quantile",
        "abs", "absent", "ceil", "floor", "round", "clamp_max", "clamp_min",
        "exp", "sqrt", "ln", "log2", "log10",
        "vector", "scalar", "sort", "sort_desc",
        "time", "minute", "hour", "day_of_month", "day_of_week", "month", "year",
        "by", "without", "on", "ignoring", "group_left", "group_right",
        "and", "or", "unless", "bool", "offset",
    ];

    let bytes = expr.as_bytes();
    let len = bytes.len();
    let mut skip_range: Vec<(usize, usize)> = Vec::new();
    {
        let modifier_kw: &[&str] = &["by", "without", "on", "ignoring"];
        let mut i = 0;
        while i < len {
            if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
                let start = i;
                while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let tok = &expr[start..i];
                if modifier_kw.contains(&tok) {
                    let mut j = i;
                    while j < len && (bytes[j] == b' ' || bytes[j] == b'\t') {
                        j += 1;
                    }
                    if j < len && bytes[j] == b'(' {
                        let mut depth = 1usize;
                        let mut k = j + 1;
                        while k < len && depth > 0 {
                            if bytes[k] == b'(' { depth += 1; }
                            else if bytes[k] == b')' { depth -= 1; }
                            k += 1;
                        }
                        skip_range.push((j, k));
                    }
                }
            } else {
                i += 1;
            }
        }
    }

    let in_skip_range = |pos: usize| skip_range.iter().any(|&(a, b)| pos >= a && pos < b);

    let mut result = String::with_capacity(expr.len() + selector.len() * 2);
    let mut i = 0;
    let mut bracket_depth = 0u32;
    while i < len {
        let b = bytes[i];
        if b == b'[' {
            bracket_depth += 1;
            result.push(b as char);
            i += 1;
        } else if b == b']' {
            bracket_depth = bracket_depth.saturating_sub(1);
            result.push(b as char);
            i += 1;
        } else if b.is_ascii_alphabetic() || b == b'_' || b == b':' {
            let start = i;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b':') {
                i += 1;
            }
            let tok = &expr[start..i];
            result.push_str(tok);

            if bracket_depth == 0 {
                let next = bytes.get(i).copied();
                let is_fn   = next == Some(b'(');
                let is_kw   = KEYWORDS.contains(&tok);
                let in_skip = in_skip_range(start);

                if !is_fn && !is_kw && !in_skip {
                    result.push_str(&selector);
                }
            }
        } else {
            result.push(b as char);
            i += 1;
        }
    }

    result
}

// ─── Label injection tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_backslash() {
        assert_eq!(escape_label_value("a\\b"), "a\\\\b");
    }

    #[test]
    fn test_escape_double_quote() {
        assert_eq!(escape_label_value("a\"b"), "a\\\"b");
    }

    #[test]
    fn test_escape_newline() {
        assert_eq!(escape_label_value("a\nb"), "a\\nb");
    }

    #[test]
    fn test_escape_no_special_chars() {
        assert_eq!(escape_label_value("user-123"), "user-123");
    }

    #[test]
    fn test_inject_bare_metric() {
        // No selector -> append {user="..."}
        let result = inject_user_label("up", "alice");
        assert!(result.contains("user=\"alice\""), "got: {result}");
        assert!(result.starts_with("up{"), "got: {result}");
    }

    #[test]
    fn test_inject_empty_selector() {
        let result = inject_user_label("toki_usage{}", "bob");
        assert!(result.contains("user=\"bob\""), "got: {result}");
        assert!(!result.contains(",}"), "should not have trailing comma: {result}");
    }

    #[test]
    fn test_inject_existing_labels() {
        let result = inject_user_label("toki_usage{model=\"gpt-4\"}", "carol");
        assert!(result.contains("user=\"carol\""), "got: {result}");
        assert!(result.contains("model=\"gpt-4\""), "existing label preserved: {result}");
    }

    #[test]
    fn test_injection_attempt_escaped() {
        // Attacker tries to inject: attacker",user="victim
        // After escape: attacker\",user=\"victim
        // This is safe: PromQL treats \" as escaped quote inside the string,
        // so there is no separate user="victim" label.
        let malicious_user = "attacker\",user=\"victim";
        let result = inject_user_label("metric{}", malicious_user);
        // Escaped quote must appear -- the " is not raw in output
        assert!(result.contains("\\\""), "quote must be escaped, got: {result}");
        // Must NOT contain an unescaped standalone label like ,user="victim"
        // i.e. after the escaped quote there must not be a bare: ,user="
        assert!(!result.contains(",user=\"victim\""), "standalone injection label must not appear, got: {result}");
    }

    #[test]
    fn test_injection_backslash_escaped() {
        let user = "user\\admin";
        let result = inject_user_label("m{}", user);
        assert!(result.contains("user\\\\admin"), "backslash must be doubled, got: {result}");
    }

    #[test]
    fn test_inject_range_query() {
        // rate(metric[5m]) -- bare metric inside function, label before `[`
        let result = inject_user_label("rate(toki_tokens_total[5m])", "dave");
        assert_eq!(result, "rate(toki_tokens_total{user=\"dave\"}[5m])", "got: {result}");
    }

    #[test]
    fn test_inject_aggregation_no_selector() {
        // sum(metric) -- metric inside aggregation, no existing selector
        let result = inject_user_label("sum(toki_tokens_total)", "eve");
        assert_eq!(result, "sum(toki_tokens_total{user=\"eve\"})", "got: {result}");
    }

    #[test]
    fn test_inject_aggregation_with_by() {
        // sum by (model)(metric) -- `model` inside `by(...)` must NOT get injected
        let result = inject_user_label("sum by (model)(toki_tokens_total)", "frank");
        assert!(result.contains("toki_tokens_total{user=\"frank\"}"), "metric must be injected, got: {result}");
        // `model` inside `by(...)` must not get a selector appended
        assert!(!result.contains("model{"), "label in by() must not be injected, got: {result}");
    }

    #[test]
    fn test_inject_increase_range() {
        // increase(metric[1h]) -- similar to rate
        let result = inject_user_label("increase(toki_tokens_total[1h])", "grace");
        assert_eq!(result, "increase(toki_tokens_total{user=\"grace\"}[1h])", "got: {result}");
    }

    #[test]
    fn test_inject_nested_sum_by_increase() {
        // toki-monitor primary pattern: sum by (model) (increase(metric[step]))
        let result = inject_user_label(
            "sum by (model) (increase(toki_tokens_total[15m]))", "hana");
        assert_eq!(
            result,
            "sum by (model) (increase(toki_tokens_total{user=\"hana\"}[15m]))",
            "got: {result}",
        );
        // model inside by(...) must NOT get a selector
        assert!(!result.contains("model{"), "label in by() must not be injected, got: {result}");
    }

    #[test]
    fn test_inject_nested_sum_by_increase_with_filter() {
        // toki-monitor with provider filter: Path A (expr contains `{`)
        let result = inject_user_label(
            "sum by (model) (increase(toki_tokens_total{provider=\"claude_code\"}[1h]))", "ivan");
        assert!(result.contains("user=\"ivan\""), "got: {result}");
        assert!(result.contains("provider=\"claude_code\""), "existing label preserved, got: {result}");
        // injection must be inside the `{...}`, not appended at end
        assert!(result.contains("toki_tokens_total{"), "got: {result}");
        assert!(!result.contains("}[1h]){"), "selector must not appear after closing paren, got: {result}");
    }

    #[test]
    fn test_inject_multi_label_by() {
        // sum by (model, provider)(metric) -- multi-label modifier list
        let result = inject_user_label(
            "sum by (model, provider) (increase(toki_tokens_total[1h]))", "judy");
        assert!(result.contains("toki_tokens_total{user=\"judy\"}"), "got: {result}");
        assert!(!result.contains("model{"), "model label must not be injected, got: {result}");
        assert!(!result.contains("provider{"), "provider label must not be injected, got: {result}");
    }

    #[test]
    fn test_inject_without_clause() {
        // sum without (session)(metric)
        let result = inject_user_label(
            "sum without (session) (increase(toki_tokens_total[1h]))", "ken");
        assert!(result.contains("toki_tokens_total{user=\"ken\"}"), "got: {result}");
        assert!(!result.contains("session{"), "session label must not be injected, got: {result}");
    }

    #[test]
    fn test_inject_trailing_by_syntax() {
        // sum(increase(metric[1h])) by (model) -- alternative PromQL syntax (by after closing paren)
        let result = inject_user_label(
            "sum(increase(toki_tokens_total[1h])) by (model)", "leo");
        assert!(result.contains("toki_tokens_total{user=\"leo\"}"), "got: {result}");
        assert!(!result.contains("model{"), "model label must not be injected, got: {result}");
    }

    // ─── Scope parsing tests ────────────────────────────────────────────────

    #[test]
    fn test_parse_scope_self() {
        assert!(matches!(parse_scope("self"), Scope::Self_));
    }

    #[test]
    fn test_parse_scope_all() {
        assert!(matches!(parse_scope("all"), Scope::All));
    }

    #[test]
    fn test_parse_scope_team() {
        match parse_scope("team:abc-123") {
            Scope::Team(id) => assert_eq!(id, "abc-123"),
            _ => panic!("expected Scope::Team"),
        }
    }

    #[test]
    fn test_parse_scope_team_empty_id() {
        assert!(matches!(parse_scope("team:"), Scope::Invalid));
    }

    #[test]
    fn test_parse_scope_invalid() {
        assert!(matches!(parse_scope("foo"), Scope::Invalid));
        assert!(matches!(parse_scope(""), Scope::Invalid));
    }

    // ─── Toki PromQL rewrite tests ──────────────────────────────────────

    #[test]
    fn test_rewrite_usage_basic() {
        let r = rewrite_toki_query("usage{}");
        assert_eq!(r.vm_query, "toki_tokens_total");
        assert!(!r.needs_cost_compute);
        assert!(!r.is_events);
    }

    #[test]
    fn test_rewrite_usage_with_labels() {
        let r = rewrite_toki_query("usage{provider=\"claude_code\"}");
        assert!(r.vm_query.contains("toki_tokens_total{provider=\"claude_code\"}"), "got: {}", r.vm_query);
    }

    #[test]
    fn test_rewrite_usage_sum_by_model() {
        let r = rewrite_toki_query("sum by (model) (increase(usage{}[1d]))");
        assert!(r.vm_query.contains("toki_tokens_total"), "got: {}", r.vm_query);
        assert!(r.vm_query.contains("sum_over_time("), "got: {}", r.vm_query);
        assert!(!r.vm_query.contains("increase("), "got: {}", r.vm_query);
        // usage now uses toki_tokens_total with type injection for per-type breakdown
        assert!(r.vm_query.contains(", type)"), "type injected, got: {}", r.vm_query);
    }

    #[test]
    fn test_rewrite_events() {
        let r = rewrite_toki_query("sum by (model) (increase(events{}[1d]))");
        assert!(r.vm_query.contains("toki_events_total"), "got: {}", r.vm_query);
        assert!(r.vm_query.contains("sum_over_time("), "got: {}", r.vm_query);
        assert!(!r.needs_cost_compute);
    }

    #[test]
    fn test_rewrite_cost() {
        let r = rewrite_toki_query("sum by (model) (increase(cost{}[1d]))");
        assert!(r.needs_cost_compute);
        // cost fetches per-type token data for pricing computation
        assert!(r.vm_query.contains("toki_tokens_total"), "got: {}", r.vm_query);
        assert!(r.vm_query.contains("by (model, type)"), "type injected for cost, got: {}", r.vm_query);
        assert!(r.vm_query.contains("sum_over_time("), "got: {}", r.vm_query);
    }

    #[test]
    fn test_rewrite_passthrough_vm_native() {
        let q = "sum_over_time(toki_tokens_total{user=\"abc\"}[1h])";
        let r = rewrite_toki_query(q);
        assert_eq!(r.vm_query, q);
        assert!(!r.needs_cost_compute);
    }

    #[test]
    fn test_compute_cost_vector() {
        // Simulate VM response for cost query: per-type token sums
        let vm_response = serde_json::json!({
            "status": "success",
            "data": {
                "resultType": "vector",
                "result": [
                    { "metric": { "model": "test-model", "type": "input" }, "value": [1000, "1000"] },
                    { "metric": { "model": "test-model", "type": "output" }, "value": [1000, "500"] },
                ]
            }
        });
        let vm_bytes = serde_json::to_vec(&vm_response).unwrap();

        let mut prices = std::collections::HashMap::new();
        prices.insert("test-model".to_string(), crate::pricing::ModelPricing {
            input_cost_per_token: 0.00001,
            output_cost_per_token: 0.00003,
            cache_creation_input_token_cost: None,
            cache_read_input_token_cost: None,
        });
        let pricing = crate::pricing::PricingTable::new(prices);

        let result = compute_cost_from_vm_response(&vm_bytes, &pricing, "cost{}").unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&result).unwrap();

        assert_eq!(parsed["status"], "success");
        let data = &parsed["data"]["result"];
        assert_eq!(data.as_array().unwrap().len(), 1); // grouped by model (type removed)
        let cost_str = data[0]["value"][1].as_str().unwrap();
        let cost: f64 = cost_str.parse().unwrap();
        // 1000 * 0.00001 + 500 * 0.00003 = 0.01 + 0.015 = 0.025
        assert!((cost - 0.025).abs() < 1e-10, "got: {cost}");
    }
}

#[cfg(test)]
mod toki_json_tests {
    use super::*;

    #[test]
    fn test_vm_response_daily_bucket() {
        // Simulates VM query_range with step=86400, aligned_start=03-24T00:00.
        // Eval points are daily boundaries. sum_over_time[86400s] at each eval
        // covers the previous 24h.
        let vm = serde_json::json!({
            "status": "success",
            "data": {
                "resultType": "matrix",
                "result": [
                    {
                        "metric": {"model": "opus", "type": "input"},
                        "values": [
                            [1774310400, "300"],  // eval=03-24T00:00 → bucket=03-23
                            [1774396800, "500"],  // eval=03-25T00:00 → bucket=03-24
                        ]
                    }
                ]
            }
        });
        let pricing = crate::pricing::PricingTable::new(std::collections::HashMap::new());
        let result = vm_response_to_toki_json(
            &serde_json::to_vec(&vm).unwrap(),
            &pricing, false, false, 86400
        ).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&result).unwrap();
        let items = parsed["providers"]["claude_code"].as_array().unwrap();
        
        // eval=03-24 → bucket=03-23 (eval-step), eval=03-25 → bucket=03-24
        let mut found = std::collections::HashMap::new();
        for item in items {
            let period = item["period"].as_str().unwrap();
            let day = &period[..10];
            for m in item["usage_per_models"].as_array().unwrap() {
                let input = m["input_tokens"].as_u64().unwrap();
                *found.entry(day.to_string()).or_insert(0u64) += input;
            }
        }
        eprintln!("found: {:?}", found);
        assert_eq!(found.get("2026-03-23"), Some(&300u64), "03-23 should have 300 (eval 03-24)");
        assert_eq!(found.get("2026-03-24"), Some(&500u64), "03-24 should have 500 (eval 03-25)");
    }
}
