use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use super::super::http::{AppError, AppState, extract_jwt};
use crate::events::{UserFilter, ServerEvent};

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
    #[serde(default)]
    pub tz: Option<String>,
    #[serde(default)]
    pub start_of_week: Option<String>,
}

// ─── Scope types ─────────────────────────────────────────────────────────────

enum Scope {
    Self_,
    Team(String),
    All,
    Invalid,
}

/// Resolve scope into a UserFilter for EventStore queries.
async fn resolve_user_filter(
    state: &AppState,
    user_id: &str,
    requested_scope: &str,
) -> Result<UserFilter, AppError> {
    let max_scope = state.dynamic_settings.max_query_scope().await;
    let is_admin = state.db.user_is_admin(user_id).await.map_err(AppError::internal)?;

    // Admins bypass max_scope enforcement, but the *requested* scope still
    // narrows the query. An admin asking for `scope=self` should see only
    // their own devices — otherwise device_id grouping leaks every user's
    // devices into the dashboard's filter menu.
    match parse_scope(requested_scope) {
        Scope::Self_ => Ok(UserFilter::Single(user_id.to_string())),
        Scope::Team(team_id) => {
            if !is_admin && max_scope == "self" {
                return Err(AppError::forbidden("team scope not enabled"));
            }
            // Admins can pull any team's data; non-admins must be members.
            if !is_admin {
                let role = state.db.get_team_member_role(&team_id, user_id).await.map_err(AppError::internal)?;
                if role.is_none() {
                    return Err(AppError::forbidden("not a member of this team"));
                }
            }
            let members = state.db.list_team_members(&team_id).await.map_err(AppError::internal)?;
            let user_ids: Vec<String> = members.iter().map(|m| m.user_id.clone()).collect();
            Ok(UserFilter::Multiple(user_ids))
        }
        Scope::All => {
            if !is_admin && max_scope != "all" {
                return Err(AppError::forbidden("global scope not enabled"));
            }
            Ok(UserFilter::All)
        }
        Scope::Invalid => Err(AppError {
            status: StatusCode::BAD_REQUEST,
            message: "invalid scope".into(),
        }),
    }
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

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let start_ts = params.start.as_deref()
        .map(|s| parse_toki_time(s, false))
        .transpose()?
        .unwrap_or(0);
    let end_ts = params.end.as_deref()
        .map(|s| parse_toki_time(s, true))
        .transpose()?
        .unwrap_or(now);

    let parsed = parse_toki_virtual_query(&params.query);
    let is_range = params.step.is_some();

    let step_secs: i64 = params.step.as_deref()
        .map(|s| parse_duration_secs(s))
        .unwrap_or(3600);

    // ── Step validation: protect against memory exhaustion. ──
    // aggregate_events_to_toki_json accumulates into BTreeMap<(bucket, group_key)>.
    // A 30-day range with 1-second step = 2.6M buckets × ~200 bytes = 500MB+.
    // Cap at 2000 buckets (sufficient for any dashboard chart resolution).
    if is_range {
        let range_secs = (end_ts - start_ts).max(1);
        let max_buckets: i64 = 2000;
        let min_step = (range_secs + max_buckets - 1) / max_buckets; // ceil division
        if step_secs < min_step {
            return Err(AppError {
                status: StatusCode::BAD_REQUEST,
                message: format!(
                    "step {}s too small for range {}s (would produce {} buckets, max {}). minimum step: {}s",
                    step_secs, range_secs, range_secs / step_secs, max_buckets, min_step
                ),
            });
        }
        if step_secs > range_secs {
            return Err(AppError {
                status: StatusCode::BAD_REQUEST,
                message: format!("step {}s exceeds range {}s", step_secs, range_secs),
            });
        }
    }

    // ── Query from EventStore (deduped by msg_id) ──
    //
    // EventStore handles dedup: same (device_id, msg_id) → last value only.
    // We query all events in [since, until) for the user, then aggregate
    // with the same bucketing logic as the local daemon.
    let since_ms = start_ts * 1000;
    let until_ms = end_ts * 1000;

    // Build user filter from scope
    let user_filter = resolve_user_filter(&state, &claims.sub, requested_scope).await?;

    let all_events = state.events.query_events(since_ms, until_ms, user_filter)
        .await.map_err(AppError::bad_gateway)?;

    let pricing = state.pricing.read().await.clone();
    let effective_step = if is_range { step_secs } else { (end_ts - start_ts).max(1) };
    let tz: Option<chrono_tz::Tz> = params.tz.as_deref().and_then(|s| {
        match s.parse() {
            Ok(t) => Some(t),
            Err(_) => {
                tracing::warn!("invalid timezone '{s}', falling back to UTC");
                None
            }
        }
    });
    let start_of_week = params.start_of_week.as_deref()
        .and_then(|s| parse_weekday(s));
    let toki_json = aggregate_events_to_toki_json(
        &all_events, effective_step, since_ms, until_ms,
        parsed.is_cost, parsed.is_events, &parsed.group_by, &pricing,
        tz.as_ref(), start_of_week,
    )?;

    Ok((
        StatusCode::OK,
        [("Content-Type", "application/json")],
        toki_json,
    ).into_response())
}

/// Aggregate raw VM data using the exact same logic as the local daemon.
///
/// This is the correct approach: instead of relying on VM's sum_over_time
/// (which has different window semantics), we fetch raw data points and
/// bucket them identically to the local daemon's query engine.
/// Supported `by (...)` labels in the toki virtual query language.
///
/// `From<&str>` falls back to `Model` for any unknown label rather than
/// erroring — preserves the historical "silent fallback" behavior but
/// makes the exhaustive switch in `aggregate_events_to_toki_json` a
/// compile-time check: adding a future label means the compiler points
/// at every place that needs an arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupBy {
    Model,
    Project,
    DeviceId,
}

impl GroupBy {
    fn key<'a>(&self, event: &'a crate::events::ServerEvent) -> &'a String {
        match self {
            GroupBy::Model     => &event.model,
            GroupBy::Project   => &event.project,
            GroupBy::DeviceId  => &event.device_id,
        }
    }
}

impl From<&str> for GroupBy {
    fn from(s: &str) -> Self {
        match s {
            "project"   => GroupBy::Project,
            "device_id" => GroupBy::DeviceId,
            _           => GroupBy::Model,
        }
    }
}

///
/// Aggregate ServerEvents into toki JSON format.
/// Uses the exact same bucketing logic as the local daemon (query.rs).
///
/// Output format matches local daemon's `grouped_to_json` (sink/json.rs):
/// - `{"providers": {"<provider>": [{"period": "<ts>|<group>", "usage_per_models": [...]}]}}`
/// - Period key: ISO timestamp + "|" + group key (model name or project name)
/// - Field names vary by provider (Codex uses cached_input_tokens/reasoning_output_tokens)
/// - Server omits the "information" block that local CLI adds (optional for parsers)
///
/// EventStore has already deduped by msg_id, so each event appears once.
fn aggregate_events_to_toki_json(
    events: &[ServerEvent],
    step_secs: i64,
    since_ms: i64,
    until_ms: i64,
    is_cost: bool,
    is_events: bool,
    group_by: &str,
    pricing: &crate::pricing::PricingTable,
    tz: Option<&chrono_tz::Tz>,
    start_of_week: Option<chrono::Weekday>,
) -> Result<Vec<u8>, AppError> {
    use std::collections::BTreeMap;

    let step_ms = step_secs * 1000;

    // For weekly steps (604800s), compute offset from Unix epoch to align
    // buckets to the desired start_of_week. Unix epoch (1970-01-01) is Thursday.
    // Default start_of_week is Monday.
    let week_offset_ms: i64 = if step_secs == 604800 {
        let sow = start_of_week.unwrap_or(chrono::Weekday::Mon);
        // Days from Thursday (epoch weekday) to desired start_of_week
        let epoch_day = 3i64; // Thursday = 3 (Mon=0)
        let target_day = match sow {
            chrono::Weekday::Mon => 0,
            chrono::Weekday::Tue => 1,
            chrono::Weekday::Wed => 2,
            chrono::Weekday::Thu => 3,
            chrono::Weekday::Fri => 4,
            chrono::Weekday::Sat => 5,
            chrono::Weekday::Sun => 6,
        };
        ((target_day - epoch_day + 7) % 7) * 86400 * 1000
    } else {
        0
    };

    #[derive(Default)]
    struct ModelBucket {
        input: u64, output: u64, cache_create: u64, cache_read: u64,
        usage_total: u64, events: u64, cost_usd: Option<f64>,
        provider: String,
    }

    let mut buckets: BTreeMap<(i64, String), ModelBucket> = BTreeMap::new();
    // Cap bucket cardinality so a `scope=all` + `by (device_id)` query
    // against a fleet of N devices doesn't balloon `buckets` past memory.
    // Steps are already capped at 2000 buckets time-wise; with N devices
    // we'd have up to 2000 * N entries. 50_000 is enough headroom for
    // typical fleets but stops a runaway aggregation cold.
    const MAX_BUCKET_ENTRIES: usize = 50_000;

    for event in events {
        // 1. Scan range check (EventStore already filters, but double-check)
        if event.ts_ms < since_ms || event.ts_ms >= until_ms { continue; }

        // 2. Bucket assignment (with week offset for weekly steps)
        let adjusted = event.ts_ms - week_offset_ms;
        let bucket_ms = (adjusted / step_ms) * step_ms + week_offset_ms;

        // 3. Bucket filter (local daemon's overlap check)
        if bucket_ms + step_ms <= since_ms || bucket_ms >= until_ms { continue; }

        let bucket_sec = bucket_ms / 1000;
        let group_key = GroupBy::from(group_by).key(event);

        let key = (bucket_sec, group_key.clone());
        // Hard cap: refuse to allocate new entries past the limit. Existing
        // buckets still accumulate so the answer for already-seen
        // (bucket, group) pairs is correct; new combinations are dropped.
        if buckets.len() >= MAX_BUCKET_ENTRIES && !buckets.contains_key(&key) {
            continue;
        }
        let entry = buckets.entry(key).or_default();

        // Track provider from first event in bucket
        if entry.provider.is_empty() && !event.provider.is_empty() {
            entry.provider = event.provider.clone();
        }

        // 4. Accumulate
        if is_events {
            entry.events += 1;
        } else {
            entry.input += event.input_tokens;
            entry.output += event.output_tokens;
            entry.cache_create += event.cache_creation_input_tokens;
            entry.cache_read += event.cache_read_input_tokens;
            entry.usage_total += event.usage_total;  // Use pre-computed total
            entry.events += 1;  // always count events alongside tokens
        }
    }

    // Compute cost if needed. Pricing table is keyed by model name; when
    // group_by is not "model" (e.g. "project" or "device_id") the lookup
    // returns None and cost stays absent — the client should aggregate by
    // model when it needs cost.
    if is_cost {
        for ((_, group_key), bucket) in &mut buckets {
            bucket.cost_usd = pricing.cost(group_key, bucket.input, bucket.output,
                bucket.cache_create, bucket.cache_read);
        }
    }

    // Build toki JSON.
    //
    // `buckets` is keyed by `(timestamp, group_key)`. The local-CLI-compatible
    // shape uses the field name `model` for the group key inside each entry
    // regardless of what dimension we grouped on — keeping it consistent
    // means the client doesn't need to switch on the group_by parameter.
    // `period_key` MUST also include the group key, otherwise two events
    // with the same timestamp and same model but different `device_id`
    // collapse into one entry (the original bug this commit fixes).
    let mut periods: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
    for ((bucket_sec, group_key), bucket) in &buckets {
        let ts_str = if let Some(tz) = tz {
            chrono::DateTime::from_timestamp(*bucket_sec, 0)
                .map(|dt| dt.with_timezone(tz).format("%Y-%m-%dT%H:%M:%S").to_string())
                .unwrap_or_default()
        } else {
            chrono::DateTime::from_timestamp(*bucket_sec, 0)
                .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S").to_string())
                .unwrap_or_default()
        };

        let is_codex = bucket.provider == "codex";
        let mut entry = serde_json::json!({
            "model": group_key,
            "input_tokens": bucket.input,
            "output_tokens": bucket.output,
            "total_tokens": bucket.usage_total,
            "events": bucket.events,
        });
        if is_codex {
            entry["cached_input_tokens"] = serde_json::json!(bucket.cache_read);
            entry["reasoning_output_tokens"] = serde_json::json!(bucket.cache_create);
        } else {
            entry["cache_creation_input_tokens"] = serde_json::json!(bucket.cache_create);
            entry["cache_read_input_tokens"] = serde_json::json!(bucket.cache_read);
        }

        if let Some(cost) = bucket.cost_usd.or_else(|| pricing.cost(group_key, bucket.input, bucket.output, bucket.cache_create, bucket.cache_read)) {
            entry["cost_usd"] = serde_json::json!(cost);
        }
        let period_key = format!("{}|{}", ts_str, group_key);
        periods.entry(period_key).or_default().push(entry);
    }

    let data: Vec<serde_json::Value> = periods.into_iter().map(|(period, models)| {
        serde_json::json!({
            "period": period,
            "usage_per_models": models,
        })
    }).collect();

    let provider_name = events.iter()
        .find(|e| !e.provider.is_empty())
        .map(|e| e.provider.as_str())
        .unwrap_or("claude_code");

    let output = serde_json::json!({
        "providers": { provider_name: data }
    });

    serde_json::to_vec(&output)
        .map_err(|e| AppError::internal(anyhow::anyhow!("json serialize: {e}")))
}


/// Parse toki time string: epoch seconds, YYYYMMDD, or YYYYMMDDhhmmss
fn parse_toki_time(s: &str, is_end: bool) -> Result<i64, AppError> {
    // Try epoch seconds first
    if let Ok(ts) = s.parse::<i64>() {
        return Ok(ts);
    }
    // YYYYMMDD
    if s.len() == 8 {
        if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y%m%d") {
            let time = if is_end {
                d.and_hms_opt(23, 59, 59).unwrap()
            } else {
                d.and_hms_opt(0, 0, 0).unwrap()
            };
            return Ok(time.and_utc().timestamp());
        }
    }
    // YYYYMMDDhhmmss
    if s.len() == 14 {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y%m%d%H%M%S") {
            return Ok(dt.and_utc().timestamp());
        }
    }
    Err(AppError {
        status: StatusCode::BAD_REQUEST,
        message: format!("invalid time format: '{s}'"),
    })
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

/// Parse weekday string (mon, tue, ...) to chrono::Weekday.
fn parse_weekday(s: &str) -> Option<chrono::Weekday> {
    match s.to_lowercase().as_str() {
        "mon" | "monday" => Some(chrono::Weekday::Mon),
        "tue" | "tuesday" => Some(chrono::Weekday::Tue),
        "wed" | "wednesday" => Some(chrono::Weekday::Wed),
        "thu" | "thursday" => Some(chrono::Weekday::Thu),
        "fri" | "friday" => Some(chrono::Weekday::Fri),
        "sat" | "saturday" => Some(chrono::Weekday::Sat),
        "sun" | "sunday" => Some(chrono::Weekday::Sun),
        _ => None,
    }
}

// ─── Virtual query parser ───────────────────────────────────────────────────
//
// Parses toki virtual metric queries (usage{}, events{}, cost{}) to extract
// the query type and group-by dimension. No PromQL rewriting needed since
// EventStore handles queries directly.

struct ParsedQuery {
    is_cost: bool,
    is_events: bool,
    group_by: String,
}

fn parse_toki_virtual_query(query: &str) -> ParsedQuery {
    let is_cost = query.contains("cost{") || query.contains("cost[");
    let is_events = query.contains("events{") || query.contains("events[");

    let group_by = {
        let by_re = regex::Regex::new(r"by\s*\(([^)]*)\)").unwrap();
        by_re.captures(query)
            .and_then(|c| c.get(1))
            .map(|m| {
                m.as_str().split(',')
                    .map(|s| s.trim())
                    .find(|s| *s != "type")
                    .unwrap_or("model")
                    .to_string()
            })
            .unwrap_or_else(|| "model".to_string())
    };

    ParsedQuery { is_cost, is_events, group_by }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_parse_virtual_query_usage() {
        let r = parse_toki_virtual_query("sum by (model) (increase(usage{}[1d]))");
        assert!(!r.is_cost);
        assert!(!r.is_events);
        assert_eq!(r.group_by, "model");
    }

    #[test]
    fn test_parse_virtual_query_cost() {
        let r = parse_toki_virtual_query("sum by (model) (increase(cost{}[1d]))");
        assert!(r.is_cost);
        assert!(!r.is_events);
        assert_eq!(r.group_by, "model");
    }

    #[test]
    fn test_parse_virtual_query_events() {
        let r = parse_toki_virtual_query("sum by (model) (increase(events{}[1d]))");
        assert!(!r.is_cost);
        assert!(r.is_events);
        assert_eq!(r.group_by, "model");
    }

    #[test]
    fn test_parse_virtual_query_by_project() {
        let r = parse_toki_virtual_query("sum by (project) (increase(usage{}[1d]))");
        assert_eq!(r.group_by, "project");
    }

    #[test]
    fn test_parse_virtual_query_device_id() {
        let r = parse_toki_virtual_query("sum by (device_id) (increase(events{}[1d]))");
        assert!(!r.is_cost);
        assert!(r.is_events);
        assert_eq!(r.group_by, "device_id");
    }

    // MARK: - Aggregation integration tests
    //
    // These exercise `aggregate_events_to_toki_json` end-to-end so the
    // bug fixed in this commit (period_key collision when two events
    // shared a timestamp + model but differed on the group-by label)
    // stays fixed.

    fn make_event(device_id: &str, model: &str, project: &str, ts_ms: i64, input: u64) -> crate::events::ServerEvent {
        crate::events::ServerEvent {
            device_id: device_id.to_string(),
            user_id: "u1".to_string(),
            msg_id: format!("{device_id}-{model}-{ts_ms}-{input}"),
            ts_ms,
            provider: "claude_code".to_string(),
            model: model.to_string(),
            project: project.to_string(),
            input_tokens: input,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            usage_total: input,
        }
    }

    fn parse_periods(bytes: &[u8]) -> Vec<(String, Vec<serde_json::Value>)> {
        let v: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        let providers = v["providers"].as_object().unwrap();
        let arr = providers.values().next().unwrap().as_array().unwrap();
        arr.iter()
            .map(|p| (
                p["period"].as_str().unwrap().to_string(),
                p["usage_per_models"].as_array().unwrap().clone(),
            ))
            .collect()
    }

    #[test]
    fn test_aggregate_groups_by_device_id_without_collision() {
        // Two devices, identical timestamp + model. Pre-fix this produced
        // ONE entry with the two devices' tokens merged under the model
        // name; post-fix it produces TWO entries keyed by device_id.
        let events = vec![
            make_event("device-a", "claude-3-opus", "/proj", 1700000000_000, 100),
            make_event("device-b", "claude-3-opus", "/proj", 1700000000_000, 250),
        ];
        let pricing = crate::pricing::PricingTable::new(std::collections::HashMap::new());
        let out = aggregate_events_to_toki_json(
            &events, 60, 1700000000_000, 1700000060_000,
            false, false, "device_id", &pricing, None, None,
        ).unwrap();

        let periods = parse_periods(&out);
        assert_eq!(periods.len(), 2, "device_id grouping must split entries by device, got {periods:?}");
        let labels: std::collections::HashSet<String> = periods.iter()
            .flat_map(|(_, entries)| entries.iter().map(|e| e["model"].as_str().unwrap().to_string()))
            .collect();
        assert!(labels.contains("device-a"));
        assert!(labels.contains("device-b"));
    }

    #[test]
    fn test_aggregate_groups_by_model_baseline() {
        // Default group_by=model still works: two devices, two models →
        // each model is its own entry, devices merged.
        let events = vec![
            make_event("device-a", "claude-3-opus", "/proj", 1700000000_000, 100),
            make_event("device-b", "claude-3-opus", "/proj", 1700000000_000, 50),
            make_event("device-a", "claude-3-haiku", "/proj", 1700000000_000, 30),
        ];
        let pricing = crate::pricing::PricingTable::new(std::collections::HashMap::new());
        let out = aggregate_events_to_toki_json(
            &events, 60, 1700000000_000, 1700000060_000,
            false, false, "model", &pricing, None, None,
        ).unwrap();
        let periods = parse_periods(&out);
        assert_eq!(periods.len(), 2, "two distinct models → two entries");
    }

    #[test]
    fn test_aggregate_groups_by_project() {
        let events = vec![
            make_event("device-a", "claude-3-opus", "/proj-x", 1700000000_000, 100),
            make_event("device-a", "claude-3-opus", "/proj-y", 1700000000_000, 50),
        ];
        let pricing = crate::pricing::PricingTable::new(std::collections::HashMap::new());
        let out = aggregate_events_to_toki_json(
            &events, 60, 1700000000_000, 1700000060_000,
            false, false, "project", &pricing, None, None,
        ).unwrap();
        let periods = parse_periods(&out);
        assert_eq!(periods.len(), 2, "two projects, same model → two entries");
    }
}

