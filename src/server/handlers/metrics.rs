use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use super::super::http::{AppError, AppState, authenticate};
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
    let claims = authenticate(&state, &headers).await?;
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

    // Windows metric: its own branch — one row per window instance, never
    // bucketed, and (v1) strictly scope=self: windows are account-level
    // provider state with no team aggregation yet.
    if params.query.trim() == "windows" {
        if requested_scope != "self" {
            return Err(AppError::forbidden("windows supports scope=self only"));
        }
        let rows = state
            .events
            .query_user_windows(&claims.sub, start_ts * 1000)
            .await
            .map_err(AppError::internal)?;
        let mut providers: std::collections::BTreeMap<String, Vec<serde_json::Value>> =
            std::collections::BTreeMap::new();
        for (provider, w) in rows {
            if w.window_end_ms > end_ts * 1000 {
                continue;
            }
            // Same field names as the local WindowRow wire shape.
            providers.entry(provider).or_default().push(serde_json::json!({
                "kind": match w.window_kind { 0 => "session", 1 => "weekly", _ => "unknown" },
                "limit_id": w.limit_id,
                "account": w.account,
                "window_end_ms": w.window_end_ms,
                "raw_resets_at_ms": w.raw_resets_at_ms,
                "window_minutes": w.window_minutes,
                "peak_pct": (w.peak_pct_x100 as f64) / 100.0,
                "last_pct": (w.last_pct_x100 as f64) / 100.0,
                "observed_ts_ms": w.observed_ts_ms,
                "first_seen_ms": w.first_seen_ms,
                // Derived: a device that uploaded an open snapshot and went
                // permanently offline never sends the finalize — a passed
                // reset IS final regardless (mirrors the client's grace).
                // Compared against WALL CLOCK: end_ts is caller-supplied and a
                // date string rounds up to 23:59:59, which would report a
                // still-open window (peak still climbing) as final.
                "finalized": w.finalized || w.raw_resets_at_ms + 180_000 < (now * 1000),
                "maxed_out": w.maxed_out,
                "limit_reached_kind": w.limit_reached_kind,
                "time_to_100_ms": w.time_to_100_ms,
                "active_ms": w.active_ms,
                "last_sample_gap_ms": w.last_sample_gap_ms,
                "sampled_active_fraction": w.sampled_active_fraction,
                "n_samples": w.n_samples,
                "plan": w.plan,
            }));
        }
        for list in providers.values_mut() {
            list.sort_by_key(|v| v["window_end_ms"].as_i64().unwrap_or(0));
        }
        let body = serde_json::json!({ "schema": 1, "windows": providers });
        return Ok(axum::Json(body).into_response());
    }

    let parsed = parse_toki_virtual_query(&params.query);
    let is_range = params.step.is_some();

    let step_secs: i64 = params.step.as_deref()
        .map(parse_duration_secs)
        .transpose()?
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

/// Compute the bucket-start (epoch seconds) an event falls into.
///
/// Whole-day-multiple steps (86400s × N, e.g. 1d/2d/30d) floor to the *local*
/// calendar boundary in `tz`, matching the local CLI's tz-aware day buckets
/// (toki `query.rs` bucket_start_ms): the day index (days since the
/// 1970-01-01 local-midnight anchor) is floored by `div_euclid(step_days)`.
/// The week granularity (604800s) is the one exception — with a `tz` it aligns
/// to the configured `start_of_week` local midnight rather than the epoch anchor,
/// so a non-Monday start_of_week is honoured. Non-whole-day steps (hour/minute,
/// 27h, …) and any step when no tz is given stay epoch/UTC-aligned — including
/// weekly with no tz, which is plain `(ts/step)*step` (the local CLI has no
/// start_of_week to apply without a timezone).
fn bucket_start_sec(
    ts_ms: i64,
    step_secs: i64,
    tz: Option<&chrono_tz::Tz>,
    start_of_week: chrono::Weekday,
) -> i64 {
    let step_ms = step_secs * 1000;
    let is_week = step_secs == 604800;
    let whole_day_multiple = step_secs >= 86400 && step_secs % 86400 == 0;

    // Tz-aware calendar flooring for week and whole-day-multiple granularities.
    if let (true, Some(tz)) = (is_week || whole_day_multiple, tz) {
        use chrono::{Datelike, Duration, NaiveDate, TimeZone};
        if let Some(local) = chrono::DateTime::from_timestamp_millis(ts_ms).map(|dt| dt.with_timezone(tz)) {
            let date = local.date_naive();
            let start_date = if is_week {
                // Align to start_of_week local midnight (honours the setting).
                let back = (date.weekday().num_days_from_monday() as i64
                    - start_of_week.num_days_from_monday() as i64 + 7) % 7;
                date - Duration::days(back)
            } else {
                // Floor the day index by step_days, anchored at 1970-01-01.
                let step_days = step_secs / 86400;
                let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
                let day_index = (date - epoch).num_days();
                epoch + Duration::days(day_index.div_euclid(step_days) * step_days)
            };
            if let Some(midnight) = start_date.and_hms_opt(0, 0, 0) {
                // Resolve local midnight to its UTC instant. The bucket must always
                // anchor to the start of the local day (never an epoch fallback), so
                // the two DST edge cases mirror the local CLI (query.rs
                // bucket_start_ms) bit-for-bit:
                //   * fall-back (local midnight happens twice) → the earlier instant.
                //   * spring-forward gap (local midnight never exists) → advance in
                //     1-minute steps to the first local time that does exist, i.e.
                //     the first valid instant of that local day. Bounded to one day
                //     of steps so a pathological tz can never loop forever.
                let resolved = match tz.from_local_datetime(&midnight) {
                    chrono::LocalResult::Single(dt) => Some(dt),
                    chrono::LocalResult::Ambiguous(earlier, _) => Some(earlier),
                    chrono::LocalResult::None => {
                        let mut candidate = midnight;
                        let mut hit = None;
                        for _ in 0..24 * 60 {
                            candidate += Duration::minutes(1);
                            if let Some(dt) = tz.from_local_datetime(&candidate).earliest() {
                                hit = Some(dt);
                                break;
                            }
                        }
                        hit
                    }
                };
                if let Some(dt) = resolved {
                    return dt.timestamp();
                }
            }
        }
        // Fall through to epoch alignment only if timestamp conversion itself
        // failed (not a DST edge case, which is handled above).
    }

    // Non-whole-day step, or any step without a tz (incl. weekly): epoch-aligned.
    // The canonical rule does not apply start_of_week without a timezone, so
    // no-tz weekly is plain epoch-aligned (matches toki bucket_start_ms).
    (ts_ms / step_ms) * step_ms / 1000
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
    // cost_usd is now always computed (see the cost block below), so the query
    // being a cost{} query no longer gates it. Kept for call-site symmetry.
    _is_cost: bool,
    is_events: bool,
    group_by: &str,
    pricing: &crate::pricing::PricingTable,
    tz: Option<&chrono_tz::Tz>,
    start_of_week: Option<chrono::Weekday>,
) -> Result<Vec<u8>, AppError> {
    use std::collections::{BTreeMap, HashMap};

    let step_ms = step_secs * 1000;
    // start_of_week defaults to Monday; only consulted for weekly steps.
    let sow = start_of_week.unwrap_or(chrono::Weekday::Mon);

    #[derive(Default)]
    struct ModelBucket {
        input: u64, output: u64, cache_create: u64, cache_read: u64,
        usage_total: u64, events: u64, cost_usd: Option<f64>,
        provider: String,
    }

    // Keyed by (bucket_sec, normalized_provider, group_key). The provider MUST
    // be part of the key: without it, claude_code and codex events that share a
    // bucket AND group key (e.g. `by (device_id)` on a device that runs both)
    // collapse into one bucket and are emitted under whichever provider was seen
    // first — with that provider's token field names. Empty provider normalizes
    // to "claude_code" to match the downstream output default.
    let mut buckets: BTreeMap<(i64, String, String), ModelBucket> = BTreeMap::new();
    // Cap bucket cardinality so a `scope=all` + `by (device_id)` query
    // against a fleet of N devices doesn't balloon `buckets` past memory.
    // Steps are already capped at 2000 buckets time-wise; with N devices
    // we'd have up to 2000 * N entries. 50_000 is enough headroom for
    // typical fleets but stops a runaway aggregation cold.
    const MAX_BUCKET_ENTRIES: usize = 50_000;
    // Parse the group-by dimension once — it's constant for the whole query.
    let group_dim = GroupBy::from(group_by);
    // Set when the bucket cap forces us to drop new (bucket, group) combinations
    // so the response can flag partial data.
    let mut truncated = false;

    for event in events {
        // 1. Scan range check (EventStore already filters, but double-check)
        if event.ts_ms < since_ms || event.ts_ms >= until_ms { continue; }

        // 2. Bucket assignment. Day/week granularities floor to the request
        //    timezone's local calendar boundary (matching the local CLI's
        //    tz-aware day/week buckets); sub-day steps stay epoch-aligned.
        let bucket_sec = bucket_start_sec(event.ts_ms, step_secs, tz, sow);
        let bucket_ms = bucket_sec * 1000;

        // 3. Bucket filter (overlap check). step_ms is the nominal bucket width;
        //    DST-length days differ by ±1h, which only nudges range-edge buckets.
        if bucket_ms + step_ms <= since_ms || bucket_ms >= until_ms { continue; }

        let group_key = group_dim.key(event);
        let provider_key = if event.provider.is_empty() { "claude_code" } else { event.provider.as_str() };

        let key = (bucket_sec, provider_key.to_string(), group_key.clone());
        // Hard cap: refuse to allocate new entries past the limit. Existing
        // buckets still accumulate so the answer for already-seen
        // (bucket, group) pairs is correct; new combinations are dropped and
        // the response is flagged truncated.
        if buckets.len() >= MAX_BUCKET_ENTRIES && !buckets.contains_key(&key) {
            truncated = true;
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

    // Compute per-bucket cost. cost_usd is part of the response contract for
    // usage{} (and events{}) responses too, not just cost{} — the toki CLI and
    // the dashboard read it from usage_per_models — so it is always computed,
    // gated only by whether the model is priced. The pricing table is keyed by
    // model name, so project/device_id group keys resolve to None and cost stays
    // absent. Memoize the per-model ModelPricing lookup so a model spanning many
    // time buckets is hashed once (this repeated lookup was the real waste, not
    // the field).
    {
        let mut price_cache: HashMap<String, Option<crate::pricing::ModelPricing>> = HashMap::new();
        for ((_, _, group_key), bucket) in &mut buckets {
            if !price_cache.contains_key(group_key) {
                price_cache.insert(group_key.clone(), pricing.get(group_key).cloned());
            }
            bucket.cost_usd = price_cache[group_key].as_ref().map(|p| {
                p.cost(bucket.input, bucket.output, bucket.cache_create, bucket.cache_read)
            });
        }
    }

    // Build toki JSON, grouped by provider.
    //
    // `buckets` is keyed by `(timestamp, group_key)`. The local-CLI-compatible
    // shape uses the field name `model` for the group key inside each entry
    // regardless of what dimension we grouped on — keeping it consistent
    // means the client doesn't need to switch on the group_by parameter.
    // `period_key` MUST also include the group key, otherwise two events
    // with the same timestamp and same model but different `device_id`
    // collapse into one entry.
    //
    // A mixed-provider query (scope=all/team spanning claude_code + codex) emits
    // each provider's periods under its own top-level key — mirroring the
    // single-provider shape `{"providers": {"<provider>": [...]}}` — rather than
    // collapsing every provider's data under the first one seen.
    let mut per_provider: BTreeMap<String, BTreeMap<String, Vec<serde_json::Value>>> = BTreeMap::new();
    for ((bucket_sec, _provider_key, group_key), bucket) in &buckets {
        let ts_str = if let Some(tz) = tz {
            chrono::DateTime::from_timestamp(*bucket_sec, 0)
                .map(|dt| dt.with_timezone(tz).format("%Y-%m-%dT%H:%M:%S").to_string())
                .unwrap_or_default()
        } else {
            chrono::DateTime::from_timestamp(*bucket_sec, 0)
                .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S").to_string())
                .unwrap_or_default()
        };

        let provider = if bucket.provider.is_empty() { "claude_code" } else { bucket.provider.as_str() };
        let is_codex = provider == "codex";
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

        // cost_usd is emitted whenever the model is priced, for every query
        // type (see the cost computation above).
        if let Some(cost) = bucket.cost_usd {
            entry["cost_usd"] = serde_json::json!(cost);
        }
        let period_key = format!("{}|{}", ts_str, group_key);
        per_provider
            .entry(provider.to_string())
            .or_default()
            .entry(period_key)
            .or_default()
            .push(entry);
    }

    let mut providers_json = serde_json::Map::new();
    for (provider, periods) in per_provider {
        let data: Vec<serde_json::Value> = periods.into_iter().map(|(period, models)| {
            serde_json::json!({
                "period": period,
                "usage_per_models": models,
            })
        }).collect();
        providers_json.insert(provider, serde_json::Value::Array(data));
    }

    // Preserve the historical shape: an empty result still carries a
    // `claude_code` key so clients that index it directly don't choke.
    if providers_json.is_empty() {
        providers_json.insert("claude_code".to_string(), serde_json::Value::Array(Vec::new()));
    }

    let mut output = serde_json::json!({ "providers": providers_json });
    if truncated {
        tracing::warn!(
            "toki query result truncated at {MAX_BUCKET_ENTRIES} (bucket,group) entries; \
             some combinations were dropped"
        );
        output["truncated"] = serde_json::json!(true);
    }

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

/// Parse duration string: "86400", "86400s", "24h", "1d", "1h30m" → seconds.
///
/// Returns a 400 error on unparseable input, unknown unit suffixes, trailing
/// digits without a unit, or a non-positive result, rather than silently
/// defaulting — a bad `step` should surface as an error, not a wrong bucket size.
fn parse_duration_secs(s: &str) -> Result<i64, AppError> {
    let invalid = || AppError {
        status: StatusCode::BAD_REQUEST,
        message: format!("invalid duration: '{s}'"),
    };

    // Plain number of seconds.
    if let Ok(n) = s.parse::<i64>() {
        return if n > 0 { Ok(n) } else { Err(invalid()) };
    }
    // Bare "<digits>s" — strip exactly one trailing unit, and only if the rest
    // is all digits (so "3600ss" falls through to the strict compound parser
    // below and is rejected there, rather than being trimmed to 3600).
    if let Some(num) = s.strip_suffix('s') {
        if !num.is_empty() && num.bytes().all(|b| b.is_ascii_digit()) {
            let n: i64 = num.parse().map_err(|_| invalid())?;
            return if n > 0 { Ok(n) } else { Err(invalid()) };
        }
    }

    // Compound units: repeated <digits><unit> (e.g. 1h30m). Every unit must be
    // preceded by digits, and all arithmetic is checked so a huge value errors
    // rather than wrapping.
    let mut total = 0i64;
    let mut num_buf = String::new();
    let mut saw_unit = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            num_buf.push(c);
        } else {
            // A unit with no preceding digits ("1hms", "hm") is malformed.
            if num_buf.is_empty() {
                return Err(invalid());
            }
            let n: i64 = num_buf.parse().map_err(|_| invalid())?;
            num_buf.clear();
            let unit_secs: i64 = match c {
                'd' => 86400,
                'h' => 3600,
                'm' => 60,
                's' => 1,
                'w' => 604800,
                'y' => 31536000,
                _ => return Err(invalid()),
            };
            total = n.checked_mul(unit_secs)
                .and_then(|v| total.checked_add(v))
                .ok_or_else(invalid)?;
            saw_unit = true;
        }
    }
    // Trailing digits with no unit, no unit at all, or a non-positive total.
    if !num_buf.is_empty() || !saw_unit || total <= 0 {
        return Err(invalid());
    }
    Ok(total)
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


/// Capability discovery for optional sync features. Older servers 404 here;
/// clients treat only an authoritative 404/2xx as an answer (transient errors
/// must be retried, not latched).
pub async fn capabilities() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "sync_windows_v1": true,
    }))
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
    fn test_parse_duration_secs_valid() {
        assert_eq!(parse_duration_secs("3600").unwrap(), 3600);
        assert_eq!(parse_duration_secs("3600s").unwrap(), 3600);
        assert_eq!(parse_duration_secs("24h").unwrap(), 86400);
        assert_eq!(parse_duration_secs("1d").unwrap(), 86400);
        assert_eq!(parse_duration_secs("1h30m").unwrap(), 5400);
        assert_eq!(parse_duration_secs("1w").unwrap(), 604800);
    }

    #[test]
    fn test_parse_duration_secs_malformed_units() {
        // Previously accepted by trim_end_matches / no digit-before-unit check.
        assert!(parse_duration_secs("3600ss").is_err(), "double 's' suffix must error");
        assert!(parse_duration_secs("1hms").is_err(), "units with no preceding digits");
        assert!(parse_duration_secs("hms").is_err());
        assert!(parse_duration_secs("s").is_err());
        // Checked arithmetic: an astronomically large value errors, not wraps.
        assert!(parse_duration_secs("100000000000000000y").is_err());
        // Valid compound still parses.
        assert_eq!(parse_duration_secs("2w").unwrap(), 1209600);
        assert_eq!(parse_duration_secs("1h30m").unwrap(), 5400);
    }

    #[test]
    fn test_aggregate_same_group_different_provider_not_merged() {
        // A device that runs both claude_code and codex, grouped by device_id:
        // both events share (bucket, device_id). Pre-fix they merged into one
        // bucket under the first provider seen (with its token field names);
        // post-fix the provider is part of the aggregation key, so each provider
        // gets its own entry with its own field names.
        let mut cc = make_event("device-a", "claude-3-opus", "/proj", 1700000000_000, 100);
        cc.provider = "claude_code".to_string();
        let mut cx = make_event("device-a", "gpt-5", "/proj", 1700000000_000, 200);
        cx.provider = "codex".to_string();

        let pricing = crate::pricing::PricingTable::new(std::collections::HashMap::new());
        let out = aggregate_events_to_toki_json(
            &[cc, cx], 60, 1700000000_000, 1700000060_000,
            false, false, "device_id", &pricing, None, None,
        ).unwrap();

        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let providers = v["providers"].as_object().unwrap();
        assert!(providers.contains_key("claude_code"), "missing claude_code: {providers:?}");
        assert!(providers.contains_key("codex"), "missing codex: {providers:?}");

        let cc_entry = &providers["claude_code"].as_array().unwrap()[0]["usage_per_models"][0];
        assert_eq!(cc_entry["model"].as_str().unwrap(), "device-a");
        assert_eq!(cc_entry["input_tokens"].as_u64().unwrap(), 100);
        assert!(cc_entry.get("cache_creation_input_tokens").is_some(), "claude_code field names");

        let cx_entry = &providers["codex"].as_array().unwrap()[0]["usage_per_models"][0];
        assert_eq!(cx_entry["input_tokens"].as_u64().unwrap(), 200);
        assert!(cx_entry.get("reasoning_output_tokens").is_some(), "codex field names");
    }

    #[test]
    fn test_parse_duration_secs_invalid() {
        // Previously these silently fell back to 3600; now they error.
        assert!(parse_duration_secs("0").is_err());
        assert!(parse_duration_secs("").is_err());
        assert!(parse_duration_secs("abc").is_err());
        assert!(parse_duration_secs("10x").is_err());   // unknown unit
        assert!(parse_duration_secs("1h30").is_err());  // trailing digits, no unit
        assert!(parse_duration_secs("-5").is_err());    // non-positive
    }

    #[test]
    fn test_bucket_start_sec_day_is_local_midnight() {
        // A UTC-evening event that is already the next day in KST must bucket to
        // the local (KST) midnight of its local date, matching the local CLI's
        // tz-aware day buckets — not the UTC midnight.
        let kst: chrono_tz::Tz = "Asia/Seoul".parse().unwrap();
        let ts_ms = chrono::NaiveDate::from_ymd_opt(2023, 11, 6).unwrap()
            .and_hms_opt(20, 0, 0).unwrap().and_utc().timestamp() * 1000; // 2023-11-07 05:00 KST

        let b = bucket_start_sec(ts_ms, 86400, Some(&kst), chrono::Weekday::Mon);
        let local = chrono::DateTime::from_timestamp(b, 0).unwrap().with_timezone(&kst);
        assert_eq!(local.format("%Y-%m-%d %H:%M:%S").to_string(), "2023-11-07 00:00:00");

        // UTC-aligned bucketing would land it on the previous (Nov 6) day.
        let b_utc = bucket_start_sec(ts_ms, 86400, None, chrono::Weekday::Mon);
        assert_ne!(b, b_utc, "tz-aware day bucket must differ from UTC bucket here");
    }

    #[test]
    fn test_bucket_start_sec_week_local_start_of_week() {
        use chrono::Datelike;
        // Weekly buckets floor to the start_of_week local midnight.
        let kst: chrono_tz::Tz = "Asia/Seoul".parse().unwrap();
        // 2023-11-08 is a Wednesday; Monday-start week begins 2023-11-06.
        let ts_ms = chrono::NaiveDate::from_ymd_opt(2023, 11, 8).unwrap()
            .and_hms_opt(3, 0, 0).unwrap().and_utc().timestamp() * 1000;

        let b = bucket_start_sec(ts_ms, 604800, Some(&kst), chrono::Weekday::Mon);
        let local = chrono::DateTime::from_timestamp(b, 0).unwrap().with_timezone(&kst);
        assert_eq!(local.format("%Y-%m-%d %H:%M:%S").to_string(), "2023-11-06 00:00:00");
        assert_eq!(local.weekday(), chrono::Weekday::Mon);
    }

    #[test]
    fn test_bucket_start_sec_2d_epoch_anchored_local() {
        use chrono::{Duration, NaiveDate, TimeZone};
        // A whole-day multiple (2d) floors the local day index by 2, anchored at
        // the 1970-01-01 local midnight — and the bucket start is local midnight.
        let kst: chrono_tz::Tz = "Asia/Seoul".parse().unwrap();
        let ts_ms = NaiveDate::from_ymd_opt(2023, 11, 9).unwrap()
            .and_hms_opt(3, 0, 0).unwrap().and_utc().timestamp() * 1000;

        let b = bucket_start_sec(ts_ms, 172800, Some(&kst), chrono::Weekday::Mon);
        let local = chrono::DateTime::from_timestamp(b, 0).unwrap().with_timezone(&kst);
        assert_eq!(local.time().to_string(), "00:00:00", "bucket must start at local midnight");

        // Independently reproduce the epoch-anchored floored date.
        let date = chrono::DateTime::from_timestamp_millis(ts_ms).unwrap().with_timezone(&kst).date_naive();
        let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
        let di = (date - epoch).num_days();
        let expect_date = epoch + Duration::days(di.div_euclid(2) * 2);
        let expect = kst.from_local_datetime(&expect_date.and_hms_opt(0, 0, 0).unwrap()).unwrap().timestamp();
        assert_eq!(b, expect);
        assert!(b <= ts_ms / 1000 && ts_ms / 1000 < b + 172800 + 3600, "event within its 2-day window");
    }

    #[test]
    fn test_bucket_start_sec_kst_cross_parity() {
        // Locks server bucketing to the canonical local-CLI rule
        // (toki bucket_start_ms, fix-toki 888ec2a test_bucket_start_ms_kst_cross_parity).
        // Event: 2026-03-11T05:00:00Z (= KST 03-11 14:00 Wed). start_of_week=Monday.
        let kst: chrono_tz::Tz = "Asia/Seoul".parse().unwrap();
        let ts_ms = chrono::NaiveDate::from_ymd_opt(2026, 3, 11).unwrap()
            .and_hms_opt(5, 0, 0).unwrap().and_utc().timestamp() * 1000;
        let mon = chrono::Weekday::Mon;
        let cases: [(&str, i64, Option<&chrono_tz::Tz>, i64); 7] = [
            ("1d tz",    86400,   Some(&kst), 1773154800), // KST 03-11 00:00
            ("2d tz",    172800,  Some(&kst), 1773068400), // KST 03-10 00:00
            ("1w tz",    604800,  Some(&kst), 1772982000), // KST 03-09 00:00 (Mon)
            ("30d tz",   2592000, Some(&kst), 1772895600), // KST 03-08 00:00
            ("27h",      97200,   Some(&kst), 1773122400), // epoch-aligned
            ("1d no-tz", 86400,   None,       1773187200), // UTC 03-11 00:00
            ("1w no-tz", 604800,  None,       1772668800), // UTC 03-05 00:00 (pure epoch)
        ];
        for (label, step, tz, expect) in cases {
            assert_eq!(bucket_start_sec(ts_ms, step, tz, mon), expect, "parity mismatch: {label}");
        }
    }

    #[test]
    fn test_bucket_start_sec_non_whole_day_falls_back_epoch() {
        // 27h is not a whole-day multiple → epoch-aligned even with a tz.
        let kst: chrono_tz::Tz = "Asia/Seoul".parse().unwrap();
        let step_secs = 27 * 3600; // 97200
        let ts_ms = chrono::NaiveDate::from_ymd_opt(2023, 11, 9).unwrap()
            .and_hms_opt(3, 0, 0).unwrap().and_utc().timestamp() * 1000;

        let b = bucket_start_sec(ts_ms, step_secs, Some(&kst), chrono::Weekday::Mon);
        let expect = (ts_ms / (step_secs * 1000)) * (step_secs * 1000) / 1000;
        assert_eq!(b, expect, "non-whole-day step must stay epoch-aligned");
    }

    #[test]
    fn test_bucket_start_sec_spring_forward_gap_anchors_to_first_valid_instant() {
        // Mirrors toki query.rs
        // test_bucket_start_ms_spring_forward_gap_anchors_to_first_valid_instant.
        // America/Sao_Paulo springs forward 2018-11-04 00:00 -> 01:00, so local
        // midnight never exists; the day bucket must anchor to the first valid
        // local instant (01:00 = 2018-11-04T03:00:00Z), never an epoch fallback.
        let sp: chrono_tz::Tz = "America/Sao_Paulo".parse().unwrap();
        let ts_ms = chrono::NaiveDate::from_ymd_opt(2018, 11, 4).unwrap()
            .and_hms_opt(13, 0, 0).unwrap().and_utc().timestamp() * 1000;

        let b = bucket_start_sec(ts_ms, 86400, Some(&sp), chrono::Weekday::Mon);
        assert_eq!(
            b, 1_541_300_400,
            "gap-day bucket must anchor to first valid local instant (2018-11-04T03:00Z)"
        );
    }

    #[test]
    fn test_aggregate_splits_by_provider() {
        // A mixed-provider result must place each provider's data under its own
        // key, not merge codex under claude_code.
        let mut cc = make_event("device-a", "claude-3-opus", "/proj", 1700000000_000, 100);
        cc.provider = "claude_code".to_string();
        let mut cx = make_event("device-b", "gpt-5", "/proj", 1700000000_000, 200);
        cx.provider = "codex".to_string();

        let pricing = crate::pricing::PricingTable::new(std::collections::HashMap::new());
        let out = aggregate_events_to_toki_json(
            &[cc, cx], 60, 1700000000_000, 1700000060_000,
            false, false, "model", &pricing, None, None,
        ).unwrap();

        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let providers = v["providers"].as_object().unwrap();
        assert!(providers.contains_key("claude_code"), "missing claude_code key: {providers:?}");
        assert!(providers.contains_key("codex"), "missing codex key: {providers:?}");
        // codex entries carry codex-specific token field names
        let codex_entry = &providers["codex"].as_array().unwrap()[0]["usage_per_models"][0];
        assert!(codex_entry.get("reasoning_output_tokens").is_some());
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

