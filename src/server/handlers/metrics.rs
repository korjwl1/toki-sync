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
    #[serde(default)]
    pub no_cost: bool,
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

    let tz: Option<chrono_tz::Tz> = params.tz.as_deref().and_then(|s| {
        match s.parse() {
            Ok(t) => Some(t),
            Err(_) => {
                tracing::warn!("invalid timezone '{s}', falling back to UTC");
                None
            }
        }
    });
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let start_ts = params.start.as_deref()
        .map(|s| parse_toki_time(s, false, tz.as_ref()))
        .transpose()?
        .unwrap_or(0);
    let end_ts = params.end.as_deref()
        .map(|s| parse_toki_time(s, true, tz.as_ref()))
        .transpose()?
        .unwrap_or(now);
    let start_ms = start_ts.checked_mul(1000).ok_or_else(|| AppError {
        status: StatusCode::BAD_REQUEST,
        message: "start time is out of range".to_string(),
    })?;
    let end_ms = end_ts.checked_mul(1000).ok_or_else(|| AppError {
        status: StatusCode::BAD_REQUEST,
        message: "end time is out of range".to_string(),
    })?;

    // Windows metric: its own branch — one row per window instance, never
    // bucketed, and (v1) strictly scope=self: windows are account-level
    // provider state with no team aggregation yet.
    if params.query.trim() == "windows" {
        if requested_scope != "self" {
            return Err(AppError::forbidden("windows supports scope=self only"));
        }
        let rows = state
            .events
            .query_user_windows(&claims.sub, start_ms)
            .await
            .map_err(AppError::internal)?;
        let mut providers: std::collections::BTreeMap<String, Vec<serde_json::Value>> =
            std::collections::BTreeMap::new();
        for (provider, w) in rows {
            if w.window_end_ms > end_ms {
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

    // A query this backend cannot execute is refused, never approximated: the
    // caller gets the reason (contract Q1) instead of another query's numbers.
    let parsed = parse_toki_virtual_query(&params.query).map_err(unsupported_query_error)?;
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
    let since_ms = start_ms;
    let until_ms = end_ms;

    // Build user filter from scope
    let user_filter = resolve_user_filter(&state, &claims.sub, requested_scope).await?;

    let all_events = state.events.query_events(since_ms, until_ms, user_filter)
        .await.map_err(AppError::bad_gateway)?;
    let pricing = state.pricing.read().await.clone();
    let include_cost = !params.no_cost;

    // Bare `events` is a raw listing in the local query engine. Preserve that
    // contract remotely; bucketed, grouped, summed, or range-step requests
    // continue through the bounded aggregator below.
    if parsed.raw_events && !is_range {
        let toki_json = raw_events_to_toki_json(
            &all_events,
            parsed.provider.as_deref(),
            tz.as_ref(),
            &pricing,
            include_cost,
        )?;
        return Ok((
            StatusCode::OK,
            [("Content-Type", "application/json")],
            toki_json,
        ).into_response());
    }

    let effective_step = if is_range { step_secs } else { (end_ts - start_ts).max(1) };
    let start_of_week = params.start_of_week.as_deref().and_then(parse_weekday);
    let toki_json = aggregate_events_to_toki_json(
        &all_events, effective_step, since_ms, until_ms,
        parsed.is_cost, parsed.is_events, &parsed.group_by,
        parsed.provider.as_deref(), &pricing,
        tz.as_ref(), start_of_week, include_cost,
    )?;

    Ok((
        StatusCode::OK,
        [("Content-Type", "application/json")],
        toki_json,
    ).into_response())
}

/// Render the local query engine's `RawEvent` wire shape, grouped only by the
/// top-level provider schema. Pointer sorting keeps the extra memory bounded
/// without cloning every stored event.
fn raw_events_to_toki_json(
    events: &[ServerEvent],
    provider_filter: Option<&str>,
    tz: Option<&chrono_tz::Tz>,
    pricing: &crate::pricing::PricingTable,
    include_cost: bool,
) -> Result<Vec<u8>, AppError> {
    use std::collections::{BTreeMap, HashMap};

    let mut by_provider: BTreeMap<&str, Vec<&ServerEvent>> = BTreeMap::new();
    for event in events {
        let provider = if event.provider.is_empty() {
            "claude_code"
        } else {
            event.provider.as_str()
        };
        if provider_filter.is_some_and(|want| want != provider) {
            continue;
        }
        by_provider.entry(provider).or_default().push(event);
    }

    let mut providers = serde_json::Map::new();
    let mut price_cache: HashMap<&str, Option<&crate::pricing::ModelPricing>> = HashMap::new();
    for (provider, mut provider_events) in by_provider {
        provider_events.sort_by_key(|event| event.ts_ms);
        let rows: Vec<serde_json::Value> = provider_events
            .into_iter()
            .map(|event| {
                let timestamp = chrono::DateTime::from_timestamp_millis(event.ts_ms)
                    .map(|dt| match tz {
                        Some(tz) => dt
                            .with_timezone(tz)
                            .format("%Y-%m-%dT%H:%M:%S")
                            .to_string(),
                        None => dt.format("%Y-%m-%dT%H:%M:%S").to_string(),
                    })
                    .unwrap_or_default();
                let mut row = serde_json::json!({
                    "timestamp": timestamp,
                    "model": event.model,
                    "session": event.session,
                    "project": event.project,
                    "input_tokens": event.input_tokens,
                    "output_tokens": event.output_tokens,
                    "cache_creation_input_tokens": event.cache_creation_input_tokens,
                    "cache_read_input_tokens": event.cache_read_input_tokens,
                });
                if include_cost {
                    let price = price_cache
                        .entry(event.model.as_str())
                        .or_insert_with(|| pricing.get(&event.model));
                    if let Some(price) = price.as_ref() {
                        row["cost_usd"] = serde_json::json!(price.cost(
                            event.input_tokens,
                            event.output_tokens,
                            event.cache_creation_input_tokens,
                            event.cache_read_input_tokens,
                        ));
                    }
                }
                row
            })
            .collect();
        providers.insert(provider.to_string(), serde_json::Value::Array(rows));
    }
    if providers.is_empty() {
        providers.insert(
            provider_filter.unwrap_or("claude_code").to_string(),
            serde_json::Value::Array(Vec::new()),
        );
    }

    serde_json::to_vec(&serde_json::json!({ "providers": providers }))
        .map_err(|e| AppError::internal(anyhow::anyhow!("json serialize: {e}")))
}

/// Aggregate raw VM data using the exact same logic as the local daemon.
///
/// This is the correct approach: instead of relying on VM's sum_over_time
/// (which has different window semantics), we fetch raw data points and
/// bucket them identically to the local daemon's query engine.
/// Supported `by (...)` labels in the toki virtual query language.
///
/// `From<&str>` falls back to `Model` for any string outside this set, but it
/// is never reached from a request: `parse_toki_virtual_query` rejects an
/// unsupported `by (...)` label before the query runs. The fallback exists so
/// the switch in `aggregate_events_to_toki_json` stays exhaustive — adding a
/// future label means the compiler points at every place that needs an arm.
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
#[allow(clippy::too_many_arguments)] // one aggregation entry point; bundling
// these into a struct would only move the argument list to a type nobody else
// constructs.
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
    // `provider="…"` from the query's label matchers. Previously parsed away
    // and dropped, so a provider-scoped panel silently summed every provider.
    provider_filter: Option<&str>,
    pricing: &crate::pricing::PricingTable,
    tz: Option<&chrono_tz::Tz>,
    start_of_week: Option<chrono::Weekday>,
    include_cost: bool,
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
    // Prices are model-keyed even when the response groups by project/device.
    // Resolve each distinct model once, then accumulate its event cost into
    // the selected bucket. Looking up the final group label as a model made
    // every non-model grouping silently lose cost.
    let mut price_cache: HashMap<&str, Option<&crate::pricing::ModelPricing>> = HashMap::new();
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

        // 3b. Provider matcher. Compared against the same normalized key the
        //     response is bucketed under, so `provider="claude_code"` also
        //     matches events that arrived with an empty provider.
        if provider_filter.is_some_and(|want| want != provider_key) { continue; }

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
            if include_cost {
                let price = price_cache
                    .entry(event.model.as_str())
                    .or_insert_with(|| pricing.get(&event.model));
                if let Some(price) = price.as_ref() {
                    *entry.cost_usd.get_or_insert(0.0) += price.cost(
                        event.input_tokens,
                        event.output_tokens,
                        event.cache_creation_input_tokens,
                        event.cache_read_input_tokens,
                    );
                }
            }
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

        // cost_usd is emitted when at least one event in this usage/cost bucket
        // has a known model price. Raw events are priced in their own branch;
        // aggregated `events` is a count metric and intentionally has no cost.
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
fn parse_toki_time(
    s: &str,
    is_end: bool,
    tz: Option<&chrono_tz::Tz>,
) -> Result<i64, AppError> {
    use chrono::TimeZone;

    // Try epoch seconds first
    if let Ok(ts) = s.parse::<i64>() {
        // Eight- and fourteen-digit values are compact local dates, not Unix
        // seconds; keep parity with the local daemon's detection order.
        if s.len() != 8 && s.len() != 14 {
            return Ok(ts);
        }
    }
    // YYYYMMDD
    if s.len() == 8 {
        if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y%m%d") {
            let time = if is_end {
                d.and_hms_opt(23, 59, 59).unwrap()
            } else {
                d.and_hms_opt(0, 0, 0).unwrap()
            };
            return match tz {
                Some(tz) => tz
                    .from_local_datetime(&time)
                    .single()
                    .map(|dt| dt.timestamp())
                    .ok_or_else(|| AppError {
                        status: StatusCode::BAD_REQUEST,
                        message: format!("ambiguous or invalid local time: '{s}'"),
                    }),
                None => Ok(time.and_utc().timestamp()),
            };
        }
    }
    // YYYYMMDDhhmmss
    if s.len() == 14 {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y%m%d%H%M%S") {
            return match tz {
                Some(tz) => tz
                    .from_local_datetime(&dt)
                    .single()
                    .map(|value| value.timestamp())
                    .ok_or_else(|| AppError {
                        status: StatusCode::BAD_REQUEST,
                        message: format!("ambiguous or invalid local time: '{s}'"),
                    }),
                None => Ok(dt.and_utc().timestamp()),
            };
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
// Parses the subset of the toki virtual query language that this backend can
// actually execute. The query language itself belongs to the daemon
// (`toki/src/query_parser.rs`); this parser does not extend it, it only decides
// whether a query is inside the subset the sync server can answer faithfully.
//
// It REFUSES anything outside that subset instead of approximating it. The
// previous implementation searched the string for `cost{` / `events{` and a
// `by (...)` clause and ignored everything else, so
// `usage{model="claude-opus-4-6"}`, `avg(...)`, `count(...)`, `rate(...)`,
// `by (session)`, `... offset 7d`, `windows{}` and every outright typo were all
// answered with the result of a *different* query — an unfiltered `sum` over
// `usage` grouped by model — with nothing in the response to say so. A number
// that is not the answer to the question is worse than an error, because the
// caller believes it.
//
// Accepted grammar (`?` = optional):
//
//   query      = "sum" by_clause? "(" inner ")" by_clause?
//              | inner by_clause?
//   inner      = "increase" "(" selector ")" | selector
//   selector   = metric filters? bucket?
//   metric     = "usage" | "toki_tokens_total" | "cost" | "events"
//   filters    = "{" (filter ("," filter)*)? "}"
//   filter     = "provider" "=" '"' value '"'
//   bucket     = "[" duration "]"
//   by_clause  = "by" "(" label ("," label)* ")"
//   label      = "model" | "project" | "device_id" | "type"
//
// The bare query `windows` is handled by its own branch in `toki_query` before
// this parser is reached.

struct ParsedQuery {
    is_cost: bool,
    is_events: bool,
    /// True only for the local engine's raw-event form: no bucket, group-by,
    /// aggregation wrapper, or range function.
    raw_events: bool,
    group_by: String,
    /// `provider="…"` label matcher, when present. Honoured by the aggregator:
    /// it was previously parsed away and dropped, which quietly widened every
    /// provider-scoped panel to all providers.
    provider: Option<String>,
}

/// Byte cursor over the query text. Everything the grammar cares about is
/// ASCII; label *values* may not be, so quoted strings are scanned by byte
/// (a `"` byte cannot occur inside a multi-byte UTF-8 sequence).
struct QueryCursor<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> QueryCursor<'a> {
    fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.src.as_bytes().get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(|b| b.is_ascii_whitespace()) {
            self.pos += 1;
        }
    }

    /// Consume `b` if it is the next non-whitespace byte.
    fn eat(&mut self, b: u8) -> bool {
        self.skip_ws();
        if self.peek() == Some(b) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// Consume an identifier (`[A-Za-z0-9_]+`), if one starts here.
    fn ident(&mut self) -> Option<&'a str> {
        self.skip_ws();
        let start = self.pos;
        while self.peek().is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_') {
            self.pos += 1;
        }
        (self.pos > start).then(|| &self.src[start..self.pos])
    }

    fn at_end(&mut self) -> bool {
        self.skip_ws();
        self.pos >= self.src.len()
    }

    /// What is left, for error messages. Bounded so a huge query cannot blow up
    /// the response body.
    fn tail(&self) -> String {
        let rest = self.src[self.pos..].trim();
        let cut = rest.char_indices().nth(40).map_or(rest.len(), |(i, _)| i);
        if cut < rest.len() {
            format!("{}…", &rest[..cut])
        } else {
            rest.to_string()
        }
    }

    /// `"…"` string literal.
    fn quoted(&mut self) -> Result<&'a str, String> {
        self.skip_ws();
        if self.peek() != Some(b'"') {
            return Err(format!("expected a quoted label value, found `{}`", self.tail()));
        }
        self.pos += 1;
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b == b'"' {
                let value = &self.src[start..self.pos];
                self.pos += 1;
                return Ok(value);
            }
            if b == b'\\' {
                return Err("escape sequences in label values are not supported".to_string());
            }
            self.pos += 1;
        }
        Err("unterminated quoted label value".to_string())
    }

    /// `( label, … )` after a `by` keyword that has already been consumed.
    fn by_labels(&mut self) -> Result<Vec<&'a str>, String> {
        if !self.eat(b'(') {
            return Err("`by` must be followed by `(...)`".to_string());
        }
        let mut labels = Vec::new();
        loop {
            if self.eat(b')') {
                break;
            }
            let label = self
                .ident()
                .ok_or_else(|| format!("expected a label name in `by (...)`, found `{}`", self.tail()))?;
            labels.push(label);
            if self.eat(b',') {
                continue;
            }
            if self.eat(b')') {
                break;
            }
            return Err(format!("expected `,` or `)` in `by (...)`, found `{}`", self.tail()));
        }
        if labels.is_empty() {
            return Err("`by ()` needs at least one label".to_string());
        }
        Ok(labels)
    }
}

/// Turn a refusal reason into the 400 the caller sees.
///
/// `AppError`'s `IntoResponse` serialises this as `{"error": "<message>"}`, so
/// the reason is carried verbatim in the response body and a client can show it
/// without interpreting a status code (contract Q2).
fn unsupported_query_error(reason: String) -> AppError {
    AppError {
        status: StatusCode::BAD_REQUEST,
        message: format!("unsupported query: {reason}"),
    }
}

/// Parse a virtual query, or explain why this backend cannot execute it.
///
/// The `Err` string is user-facing: it is returned verbatim in the 400 response
/// body so the caller can show it (see `contracts/query-contract.md` Q1/Q2).
fn parse_toki_virtual_query(query: &str) -> Result<ParsedQuery, String> {
    let mut c = QueryCursor::new(query);
    if c.at_end() {
        return Err("the query is empty".to_string());
    }

    let mut group_labels: Vec<&str> = Vec::new();
    let mut have_by = false;
    let mut open_parens = 0usize;
    let mut has_operator = false;
    let mut has_bucket = false;

    // ── Optional aggregation wrapper: `sum (…)` / `sum by (…) (…)` ──
    let mark = c.pos;
    if let Some(id) = c.ident() {
        // An identifier here is an aggregation only if a parenthesised body
        // follows it — either straight away, or after a `by (...)` clause.
        // `increase` is a range function and is handled one level down, and a
        // metric can carry a *trailing* group-by (`events by (model)`), which
        // is why the by-clause alone does not make this a call.
        c.skip_ws();
        let mut wrapper = c.peek() == Some(b'(') && id != "increase";
        let mut labels: Vec<&str> = Vec::new();

        if !wrapper {
            let by_mark = c.pos;
            if c.ident() == Some("by") {
                // A malformed by-clause is an error wherever it appears, so
                // this propagates instead of silently rewinding.
                let parsed_labels = c.by_labels()?;
                c.skip_ws();
                if c.peek() == Some(b'(') {
                    wrapper = true;
                    labels = parsed_labels;
                } else {
                    c.pos = by_mark;
                }
            } else {
                c.pos = by_mark;
            }
        }

        if wrapper {
            if id != "sum" {
                return Err(format!(
                    "`{id}(...)` is not supported by the sync backend; the only aggregation it computes is `sum`"
                ));
            }
            have_by = !labels.is_empty();
            has_operator = true;
            group_labels = labels;
            if !c.eat(b'(') {
                return Err(format!(
                    "`sum` must be followed by a parenthesised expression, found `{}`",
                    c.tail()
                ));
            }
            open_parens += 1;
        } else {
            c.pos = mark;
        }
    }

    // ── Optional range function: `increase(…)` ──
    let mark = c.pos;
    if let Some(id) = c.ident() {
        c.skip_ws();
        if c.peek() == Some(b'(') {
            if id != "increase" {
                return Err(format!(
                    "function `{id}` is not supported by the sync backend; it only understands `increase`"
                ));
            }
            has_operator = true;
            c.pos += 1;
            open_parens += 1;
        } else {
            c.pos = mark;
        }
    }

    // ── Metric ──
    let metric = c
        .ident()
        .ok_or_else(|| format!("expected a metric name, found `{}`", c.tail()))?;
    let (is_cost, is_events) = match metric {
        // `toki_tokens_total` is the pre-rename spelling of `usage`; saved
        // dashboards still carry it.
        "usage" | "toki_tokens_total" => (false, false),
        "cost" => (true, false),
        "events" => (false, true),
        "windows" => {
            return Err(
                "metric `windows` is only available on the sync backend as the bare query `windows`"
                    .to_string(),
            );
        }
        "sessions" | "projects" => {
            return Err(format!(
                "metric `{metric}` is only available from the local daemon, not the sync backend"
            ));
        }
        other => {
            return Err(format!(
                "unknown metric `{other}`; the sync backend understands `usage`, `cost` and `events`"
            ));
        }
    };

    // ── Optional label matchers ──
    let mut provider: Option<&str> = None;
    if c.eat(b'{') {
        loop {
            if c.eat(b'}') {
                break;
            }
            let key = c
                .ident()
                .ok_or_else(|| format!("expected a label name in `{{...}}`, found `{}`", c.tail()))?;
            c.skip_ws();
            // Only `=` is executable here; the rest would silently match everything.
            let op = match (c.peek(), c.src.as_bytes().get(c.pos + 1).copied()) {
                (Some(b'='), Some(b'~')) => "=~",
                (Some(b'!'), Some(b'~')) => "!~",
                (Some(b'!'), Some(b'=')) => "!=",
                (Some(b'='), _) => "=",
                _ => return Err(format!("expected a label matcher after `{key}`, found `{}`", c.tail())),
            };
            c.pos += op.len();
            if op != "=" {
                return Err(format!(
                    "label matcher `{op}` is not supported by the sync backend; only `=` is"
                ));
            }
            let value = c.quoted()?;
            if key != "provider" {
                return Err(format!(
                    "filter on `{key}` is not supported by the sync backend; it can only filter on `provider`"
                ));
            }
            if value.is_empty() {
                return Err("`provider` filter must not be empty".to_string());
            }
            if provider.is_some_and(|p| p != value) {
                return Err("conflicting `provider` filters".to_string());
            }
            provider = Some(value);

            if c.eat(b',') {
                continue;
            }
            if c.eat(b'}') {
                break;
            }
            return Err(format!("expected `,` or `}}` in `{{...}}`, found `{}`", c.tail()));
        }
    }

    // ── Optional range selector. The bucket width the server aggregates on
    //    comes from the `step` parameter, so this is validated for syntax only.
    if c.eat(b'[') {
        has_bucket = true;
        let start = c.pos;
        while c.peek().is_some_and(|b| b != b']') {
            c.pos += 1;
        }
        let raw = c.src[start..c.pos].trim();
        if !c.eat(b']') {
            return Err("unterminated `[` range selector".to_string());
        }
        if parse_duration_secs(raw).is_err() {
            return Err(format!("invalid range `[{raw}]`"));
        }
    }

    // ── `offset` shifts the window; the server has no way to apply it. ──
    let mark = c.pos;
    if let Some(id) = c.ident() {
        if id == "offset" {
            return Err("`offset` is not supported by the sync backend".to_string());
        }
        c.pos = mark;
    }

    for _ in 0..open_parens {
        if !c.eat(b')') {
            return Err(format!("unbalanced parentheses; expected `)`, found `{}`", c.tail()));
        }
    }

    // ── Trailing group-by (`increase(usage[1d]) by (model)`, legacy toki form) ──
    let mark = c.pos;
    if let Some(id) = c.ident() {
        if id == "by" {
            if have_by {
                return Err("duplicate `by (...)` clause".to_string());
            }
            group_labels = c.by_labels()?;
        } else {
            c.pos = mark;
        }
    }

    if !c.at_end() {
        return Err(format!("unexpected trailing input `{}`", c.tail()));
    }

    // ── Resolve the single grouping dimension the aggregator supports. ──
    let has_group_by = !group_labels.is_empty();
    let mut selected: Option<&str> = None;
    for label in &group_labels {
        match *label {
            // Token-kind split: the aggregator emits input/output/cache fields
            // side by side, so `type` is already in the response and needs no
            // grouping. Skipped, as it always was.
            "type" => {}
            "model" | "project" | "device_id" => {
                if selected.is_some_and(|s| s != *label) {
                    return Err(format!(
                        "the sync backend can group by only one label, got `{}`",
                        group_labels.join(", ")
                    ));
                }
                selected = Some(label);
            }
            other => {
                return Err(format!(
                    "group-by label `{other}` is not supported by the sync backend; use `model`, `project` or `device_id`"
                ));
            }
        }
    }

    Ok(ParsedQuery {
        is_cost,
        is_events,
        raw_events: is_events && !has_operator && !has_bucket && !has_group_by,
        group_by: selected.unwrap_or("model").to_string(),
        provider: provider.map(str::to_string),
    })
}


/// Capability discovery for optional sync features. Older servers 404 here;
/// clients treat only an authoritative 404/2xx as an answer (transient errors
/// must be retried, not latched).
pub async fn capabilities() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "sync_windows_v1": true,
        // The opt-in monitor settings channel under /me/monitor. A monitor
        // pointed at an older server has to find out here: those servers 404
        // the routes, which is indistinguishable from an empty store.
        "monitor_settings_v1": true,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Clients probe this before using optional features, so a flag going
    /// missing is a silent downgrade, not a visible break.
    #[tokio::test]
    async fn capabilities_advertises_the_optional_channels() {
        let axum::Json(caps) = capabilities().await;
        assert_eq!(caps["sync_windows_v1"], true);
        assert_eq!(caps["monitor_settings_v1"], true);
    }

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

    // MARK: - Virtual query parser
    //
    // Two halves, and both matter:
    //
    //   * the `accepted` half pins the queries the backend answered correctly
    //     before this parser existed — it fails if the rewrite narrows the set;
    //   * the `refused` half pins the queries it used to answer with *another
    //     query's* numbers — every one of these returned a `ParsedQuery` under
    //     the old substring scan, so each of these assertions fails against it.

    fn parsed(query: &str) -> ParsedQuery {
        parse_toki_virtual_query(query)
            .unwrap_or_else(|e| panic!("`{query}` must stay accepted, got: {e}"))
    }

    fn refusal(query: &str) -> String {
        match parse_toki_virtual_query(query) {
            Err(reason) => reason,
            Ok(p) => panic!(
                "`{query}` must be refused, but was answered as cost={} events={} group_by={} — \
                 that is the silent-substitute bug",
                p.is_cost, p.is_events, p.group_by
            ),
        }
    }

    #[test]
    fn test_parse_virtual_query_usage() {
        let r = parsed("sum by (model) (increase(usage{}[1d]))");
        assert!(!r.is_cost);
        assert!(!r.is_events);
        assert_eq!(r.group_by, "model");
        assert_eq!(r.provider, None);
    }

    #[test]
    fn test_parse_virtual_query_cost() {
        let r = parsed("sum by (model) (increase(cost{}[1d]))");
        assert!(r.is_cost);
        assert!(!r.is_events);
        assert_eq!(r.group_by, "model");
    }

    #[test]
    fn test_parse_virtual_query_events() {
        let r = parsed("sum by (model) (increase(events{}[1d]))");
        assert!(!r.is_cost);
        assert!(r.is_events);
        assert_eq!(r.group_by, "model");
    }

    #[test]
    fn test_parse_virtual_query_by_project() {
        let r = parsed("sum by (project) (increase(usage{}[1d]))");
        assert_eq!(r.group_by, "project");
    }

    #[test]
    fn test_parse_virtual_query_device_id() {
        let r = parsed("sum by (device_id) (increase(events{}[1d]))");
        assert!(!r.is_cost);
        assert!(r.is_events);
        assert_eq!(r.group_by, "device_id");
    }

    /// Every shape the old scanner answered *correctly* still parses to the
    /// same result. This is the anti-regression net for "do not narrow the
    /// accepted set" — it covers the panel templates the app ships
    /// (`toki_monitor` DashboardConfig / DashboardSettingsSheet), the legacy
    /// trailing-`by` form the daemon accepts, and the bare selector forms.
    #[test]
    fn test_previously_accepted_queries_are_unchanged() {
        // (query, is_cost, is_events, group_by)
        let cases: &[(&str, bool, bool, &str)] = &[
            ("sum by (model) (increase(usage{}[1d]))",       false, false, "model"),
            ("sum by (model) (increase(usage[1h]))",         false, false, "model"),
            ("sum by (model) (increase(cost{}[1d]))",        true,  false, "model"),
            ("sum by (model) (increase(events{}[1d]))",      false, true,  "model"),
            ("sum by (project) (increase(usage{}[1d]))",     false, false, "project"),
            ("sum by (device_id) (increase(usage{}[1d]))",   false, false, "device_id"),
            // `type` was skipped by the old scanner; it still is.
            ("sum by (type) (increase(usage{}[1d]))",        false, false, "model"),
            ("sum by (type, project) (increase(cost[1d]))",  true,  false, "project"),
            // No `by` clause → per-model rows, same as the daemon's default.
            ("sum(increase(usage[1d]))",                     false, false, "model"),
            ("increase(cost{}[1d])",                         true,  false, "model"),
            ("usage{}[1d]",                                  false, false, "model"),
            ("usage",                                        false, false, "model"),
            // Legacy toki form: group-by trails the expression.
            ("increase(usage[1d]) by (project)",             false, false, "project"),
            ("usage[5m] by (model)",                         false, false, "model"),
            ("sum(usage[1d]) by (device_id)",                false, false, "device_id"),
            // Pre-rename spelling still carried by saved dashboards.
            ("sum by (model) (increase(toki_tokens_total{}[1d]))", false, false, "model"),
            // Whitespace variants.
            ("sum  by ( model , type ) ( increase( usage{} [1d] ) )", false, false, "model"),
        ];
        for (query, is_cost, is_events, group_by) in cases {
            let r = parsed(query);
            assert_eq!(r.is_cost, *is_cost, "is_cost for `{query}`");
            assert_eq!(r.is_events, *is_events, "is_events for `{query}`");
            assert_eq!(r.group_by, *group_by, "group_by for `{query}`");
        }
    }

    /// A bare `events` (no `{}` and no `[…]`) used to slip past the substring
    /// scan — it looked for the literal `events{` / `events[` — and was
    /// answered as a *usage* query: token sums where the caller asked for a
    /// call count. The metric is now read as a metric, so the same query
    /// returns what it says.
    #[test]
    fn test_bare_events_metric_is_an_events_query() {
        for query in ["events", "events by (model)", "sum(events)", "sum by (project) (events)"] {
            let r = parsed(query);
            assert!(r.is_events, "`{query}` is an events query");
            assert!(!r.is_cost);
        }
        assert!(parsed("events").raw_events);
        assert!(parsed("events{provider=\"codex\"}").raw_events);
        for query in ["events[1d]", "events by (type)", "sum(events)", "increase(events)"] {
            assert!(!parsed(query).raw_events, "`{query}` must stay aggregated");
        }
    }

    /// The core of this change: a query the backend cannot execute is refused.
    /// Under the old scanner every one of these returned an answer computed
    /// from `sum(usage) by (model)` instead.
    #[test]
    fn test_unrecognised_queries_are_refused() {
        let queries = [
            // Metrics this backend has no branch for.
            "sum by (model) (increase(http_requests_total[5m]))",
            "sum by (model) (increase(sessions[1d]))",
            "sum by (model) (increase(projects[1d]))",
            "sum by (model) (increase(windows[1d]))",
            "usge{}[1d]",                       // typo — used to be silently `usage`
            // Aggregations and functions it does not implement.
            "avg by (model) (increase(usage[1d]))",
            "count by (model) (increase(events[1d]))",
            "max by (model) (increase(usage[1d]))",
            "sum by (model) (rate(usage[5m]))",
            "sum by (model) (sum_over_time(usage[1d]))",
            // Label matchers it cannot apply.
            "sum by (model) (increase(usage{model=\"claude-opus-4-6\"}[1d]))",
            "sum by (model) (increase(usage{project=\"toki\"}[1d]))",
            "sum by (model) (increase(usage{provider=~\"claude.*\"}[1d]))",
            "sum by (model) (increase(usage{provider!=\"codex\"}[1d]))",
            // Modifiers it cannot apply.
            "sum by (model) (increase(usage[1d] offset 7d))",
            // Group-by dimensions it cannot produce.
            "sum by (session) (increase(usage[1d]))",
            "sum by (provider) (increase(usage[1d]))",
            "sum by (model, project) (increase(usage[1d]))",
            "sum by () (increase(usage[1d]))",
            // Not a query at all.
            "",
            "   ",
            "hello world",
            "{}",
            "1 + 1",
            "sum by (model) (increase(usage[1d])",   // unbalanced
            "sum by (model) (increase(usage[nope]))", // unparseable range
        ];
        for query in queries {
            let reason = refusal(query);
            assert!(!reason.is_empty(), "refusal of `{query}` must carry a reason");
        }
    }

    /// The reason has to be readable by the person who wrote the query, which
    /// means naming the part that was rejected — not just "bad request".
    #[test]
    fn test_refusal_reason_names_the_offending_token() {
        let cases = [
            ("sum by (model) (increase(usage{model=\"opus\"}[1d]))", "model"),
            ("sum by (session) (increase(usage[1d]))",               "session"),
            ("avg by (model) (increase(usage[1d]))",                 "avg"),
            ("sum by (model) (rate(usage[5m]))",                     "rate"),
            ("sum by (model) (increase(http_requests_total[5m]))",   "http_requests_total"),
            ("sum by (model) (increase(usage[1d] offset 7d))",       "offset"),
            ("sum by (model) (increase(sessions[1d]))",              "sessions"),
        ];
        for (query, needle) in cases {
            let reason = refusal(query);
            assert!(
                reason.contains(needle),
                "refusal of `{query}` should mention `{needle}`, got: {reason}"
            );
        }
    }

    /// The refusal must reach the client as a 400 with the reason in the body,
    /// not as an empty 200 that looks like "no data".
    #[tokio::test]
    async fn test_refusal_becomes_400_with_reason_in_body() {
        let reason = refusal("sum by (model) (increase(http_requests_total[5m]))");
        let err = unsupported_query_error(reason.clone());
        assert_eq!(err.status, StatusCode::BAD_REQUEST);

        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let message = body["error"].as_str().expect("body must carry an `error` string");
        assert!(message.starts_with("unsupported query: "), "got: {message}");
        assert!(message.contains(&reason), "the reason must survive verbatim, got: {message}");
        assert!(message.contains("http_requests_total"), "got: {message}");
    }

    /// The cursor walks bytes; a query with multi-byte text in it must be
    /// refused with a message, not slice-panic mid-character.
    #[test]
    fn test_non_ascii_query_is_refused_without_panicking() {
        for query in ["사용량", "usage{model=\"프로젝트\"}", "usage[1일]", "usage{} 한글"] {
            let reason = refusal(query);
            assert!(!reason.is_empty());
        }
    }

    #[test]
    fn test_provider_filter_is_parsed() {
        let r = parsed("sum by (model) (increase(usage{provider=\"claude_code\"}[1d]))");
        assert_eq!(r.provider.as_deref(), Some("claude_code"));
        assert_eq!(r.group_by, "model");

        let r = parsed("sum by (project) (increase(cost{provider=\"codex\"}[1d]))");
        assert_eq!(r.provider.as_deref(), Some("codex"));
        assert!(r.is_cost);
    }

    #[test]
    fn test_empty_and_conflicting_provider_filters_are_refused() {
        assert!(refusal("usage{provider=\"\"}").contains("provider"));
        assert!(
            refusal("usage{provider=\"codex\",provider=\"claude_code\"}").contains("conflicting")
        );
    }

    /// The `provider="…"` matcher used to be parsed and then thrown away, so a
    /// panel scoped to one provider was answered with every provider's tokens
    /// summed together. It now narrows the aggregation.
    #[test]
    fn test_provider_filter_narrows_the_aggregation() {
        let mut codex = make_event("d2", "m1", "p", 1_700_000_000_000, 200);
        codex.provider = "codex".to_string();
        // Legacy upload with no provider — normalizes to claude_code, both in
        // the response key and in the matcher.
        let mut legacy = make_event("d3", "m1", "p", 1_700_000_000_000, 300);
        legacy.provider = String::new();
        let events = vec![
            make_event("d1", "m1", "p", 1_700_000_000_000, 100),
            codex,
            legacy,
        ];
        let pricing = crate::pricing::PricingTable::new(std::collections::HashMap::new());

        let totals = |provider_filter: Option<&str>| -> serde_json::Value {
            let out = aggregate_events_to_toki_json(
                &events, 60, 1_700_000_000_000, 1_700_000_060_000,
                false, false, "model", provider_filter, &pricing, None, None, true,
            ).unwrap();
            serde_json::from_slice(&out).unwrap()
        };

        // No matcher: unchanged: both providers are reported.
        let all = totals(None);
        let keys: Vec<&String> = all["providers"].as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["claude_code", "codex"], "unfiltered query keeps every provider");

        let only_codex = totals(Some("codex"));
        let obj = only_codex["providers"].as_object().unwrap();
        assert_eq!(obj.keys().collect::<Vec<_>>(), vec!["codex"], "codex filter drops claude_code");
        let entry = &obj["codex"].as_array().unwrap()[0]["usage_per_models"][0];
        assert_eq!(entry["input_tokens"].as_u64(), Some(200));

        let only_claude = totals(Some("claude_code"));
        let obj = only_claude["providers"].as_object().unwrap();
        assert_eq!(obj.keys().collect::<Vec<_>>(), vec!["claude_code"], "claude filter drops codex");
        let entry = &obj["claude_code"].as_array().unwrap()[0]["usage_per_models"][0];
        assert_eq!(entry["input_tokens"].as_u64(), Some(400), "empty provider counts as claude_code");
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
            session: String::new(),
            input_tokens: input,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            usage_total: input,
        }
    }

    #[test]
    fn test_raw_events_preserve_session_provider_and_timezone() {
        // 2026-08-27 14:30 UTC = 2026-08-27 23:30 KST.
        let ts_ms = chrono::NaiveDate::from_ymd_opt(2026, 8, 27)
            .unwrap()
            .and_hms_opt(14, 30, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let mut event = make_event("device-a", "gpt-test", "/project", ts_ms, 12);
        event.provider = "codex".to_string();
        event.session = "session-1".to_string();
        event.output_tokens = 3;
        event.cache_creation_input_tokens = 2;
        event.cache_read_input_tokens = 4;
        let kst: chrono_tz::Tz = "Asia/Seoul".parse().unwrap();
        let pricing = crate::pricing::PricingTable::new(std::collections::HashMap::from([(
            "gpt-test".to_string(),
            crate::pricing::ModelPricing {
                input_cost_per_token: 1.0,
                output_cost_per_token: 1.0,
                cache_creation_input_token_cost: None,
                cache_read_input_token_cost: None,
            },
        )]));

        let out = raw_events_to_toki_json(
            &[event.clone()],
            Some("codex"),
            Some(&kst),
            &pricing,
            true,
        ).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let rows = value["providers"]["codex"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["timestamp"], "2026-08-27T23:30:00");
        assert_eq!(rows[0]["session"], "session-1");
        assert_eq!(rows[0]["cache_creation_input_tokens"], 2);
        assert_eq!(rows[0]["cost_usd"], 15.0);
        assert!(value["providers"].get("claude_code").is_none());

        let no_cost = raw_events_to_toki_json(
            &[event],
            Some("codex"),
            Some(&kst),
            &pricing,
            false,
        ).unwrap();
        let no_cost: serde_json::Value = serde_json::from_slice(&no_cost).unwrap();
        assert!(no_cost["providers"]["codex"][0].get("cost_usd").is_none());
    }

    #[test]
    fn test_compact_query_dates_use_request_timezone() {
        let kst: chrono_tz::Tz = "Asia/Seoul".parse().unwrap();
        let start = parse_toki_time("20260827", false, Some(&kst)).unwrap();
        let end = parse_toki_time("20260827", true, Some(&kst)).unwrap();
        assert_eq!(
            chrono::DateTime::from_timestamp(start, 0).unwrap().to_rfc3339(),
            "2026-08-26T15:00:00+00:00"
        );
        assert_eq!(
            chrono::DateTime::from_timestamp(end, 0).unwrap().to_rfc3339(),
            "2026-08-27T14:59:59+00:00"
        );
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
            make_event("device-a", "claude-3-opus", "/proj", 1_700_000_000_000, 100),
            make_event("device-b", "claude-3-opus", "/proj", 1_700_000_000_000, 250),
        ];
        let pricing = crate::pricing::PricingTable::new(std::collections::HashMap::new());
        let out = aggregate_events_to_toki_json(
            &events, 60, 1_700_000_000_000, 1_700_000_060_000,
            false, false, "device_id", None, &pricing, None, None, true,
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
            make_event("device-a", "claude-3-opus", "/proj", 1_700_000_000_000, 100),
            make_event("device-b", "claude-3-opus", "/proj", 1_700_000_000_000, 50),
            make_event("device-a", "claude-3-haiku", "/proj", 1_700_000_000_000, 30),
        ];
        let pricing = crate::pricing::PricingTable::new(std::collections::HashMap::new());
        let out = aggregate_events_to_toki_json(
            &events, 60, 1_700_000_000_000, 1_700_000_060_000,
            false, false, "model", None, &pricing, None, None, true,
        ).unwrap();
        let periods = parse_periods(&out);
        assert_eq!(periods.len(), 2, "two distinct models → two entries");
    }

    #[test]
    fn test_aggregate_no_cost_skips_server_pricing() {
        let events = vec![make_event("device-a", "priced-model", "/proj", 1_700_000_000_000, 100)];
        let pricing = crate::pricing::PricingTable::new(std::collections::HashMap::from([(
            "priced-model".to_string(),
            crate::pricing::ModelPricing {
                input_cost_per_token: 1.0,
                output_cost_per_token: 1.0,
                cache_creation_input_token_cost: None,
                cache_read_input_token_cost: None,
            },
        )]));
        let out = aggregate_events_to_toki_json(
            &events, 60, 1_700_000_000_000, 1_700_000_060_000,
            false, false, "model", None, &pricing, None, None, false,
        ).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let model = &value["providers"]["claude_code"][0]["usage_per_models"][0];
        assert!(model.get("cost_usd").is_none());
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
        let mut cc = make_event("device-a", "claude-3-opus", "/proj", 1_700_000_000_000, 100);
        cc.provider = "claude_code".to_string();
        let mut cx = make_event("device-a", "gpt-5", "/proj", 1_700_000_000_000, 200);
        cx.provider = "codex".to_string();

        let pricing = crate::pricing::PricingTable::new(std::collections::HashMap::new());
        let out = aggregate_events_to_toki_json(
            &[cc, cx], 60, 1_700_000_000_000, 1_700_000_060_000,
            false, false, "device_id", None, &pricing, None, None, true,
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
        let mut cc = make_event("device-a", "claude-3-opus", "/proj", 1_700_000_000_000, 100);
        cc.provider = "claude_code".to_string();
        let mut cx = make_event("device-b", "gpt-5", "/proj", 1_700_000_000_000, 200);
        cx.provider = "codex".to_string();

        let pricing = crate::pricing::PricingTable::new(std::collections::HashMap::new());
        let out = aggregate_events_to_toki_json(
            &[cc, cx], 60, 1_700_000_000_000, 1_700_000_060_000,
            false, false, "model", None, &pricing, None, None, true,
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
            make_event("device-a", "claude-3-opus", "/proj-x", 1_700_000_000_000, 100),
            make_event("device-a", "claude-3-opus", "/proj-y", 1_700_000_000_000, 50),
        ];
        let pricing = crate::pricing::PricingTable::new(std::collections::HashMap::from([(
            "claude-3-opus".to_string(),
            crate::pricing::ModelPricing {
                input_cost_per_token: 1.0,
                output_cost_per_token: 0.0,
                cache_creation_input_token_cost: None,
                cache_read_input_token_cost: None,
            },
        )]));
        let out = aggregate_events_to_toki_json(
            &events, 60, 1_700_000_000_000, 1_700_000_060_000,
            false, false, "project", None, &pricing, None, None, true,
        ).unwrap();
        let periods = parse_periods(&out);
        assert_eq!(periods.len(), 2, "two projects, same model → two entries");
        let mut costs: Vec<f64> = periods
            .iter()
            .map(|(_, models)| models[0]["cost_usd"].as_f64().unwrap())
            .collect();
        costs.sort_by(f64::total_cmp);
        assert_eq!(costs, vec![50.0, 100.0], "costs use the event model, not project label");
    }
}
