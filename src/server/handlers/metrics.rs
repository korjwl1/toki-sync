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
}

#[derive(Deserialize)]
pub struct QueryRangeParams {
    pub query: String,
    pub start: i64,
    pub end: i64,
    pub step: Option<String>,
}

pub async fn promql_query(
    headers: HeaderMap,
    State(state): State<AppState>,
    Query(params): Query<QueryParams>,
) -> Result<Response, AppError> {
    let claims = extract_jwt(&headers, &state.jwt)?;
    let injected = inject_user_label(&params.query, &claims.sub);
    let result = state.vm.query(&injected, params.time).await.map_err(AppError::internal)?;
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
    let injected = inject_user_label(&params.query, &claims.sub);
    let step = params.step.as_deref().unwrap_or("60s");
    let result = state.vm.query_range(&injected, params.start, params.end, step)
        .await
        .map_err(AppError::internal)?;
    Ok((
        StatusCode::OK,
        [("Content-Type", "application/json")],
        result,
    ).into_response())
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
pub fn inject_user_label(expr: &str, user_id: &str) -> String {
    let escaped = escape_label_value(user_id);
    let injection = format!("user=\"{escaped}\"");
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
                    result.push_str(&injection);
                } else {
                    result.push_str(&injection);
                    result.push(',');
                }
                continue;
            }
            result.push(chars.next().unwrap());
        }
        return result;
    }

    // ── Path B: no `{` -- inject after bare metric name tokens ───────────
    //
    // A metric name token is `[a-zA-Z_:][a-zA-Z0-9_:]*` that is:
    //   - NOT immediately followed by `(` (which would make it a function name)
    //   - NOT a PromQL keyword (aggregation modifier, binary operator, etc.)
    //   - NOT inside an aggregation-modifier label list `by(...)` / `without(...)`
    //
    // We track parenthesis depth and whether we're inside a modifier label list.
    // Modifier label lists are `(...)` that directly follow `by`, `without`,
    // `on`, or `ignoring` keywords.

    const KEYWORDS: &[&str] = &[
        // aggregation operators (always followed by `(` or modifier keyword)
        "sum", "min", "max", "avg", "count", "stddev", "stdvar",
        "bottomk", "topk", "count_values", "quantile",
        // range/instant functions
        "rate", "irate", "increase", "delta", "idelta",
        "resets", "changes", "deriv", "predict_linear", "holt_winters",
        "label_replace", "label_join", "histogram_quantile",
        "abs", "absent", "ceil", "floor", "round", "clamp_max", "clamp_min",
        "exp", "sqrt", "ln", "log2", "log10",
        "vector", "scalar", "sort", "sort_desc",
        "time", "minute", "hour", "day_of_month", "day_of_week", "month", "year",
        // modifier keywords -- their `(label, ...)` list must not get injected
        "by", "without", "on", "ignoring", "group_left", "group_right",
        // binary-operator keywords
        "and", "or", "unless", "bool", "offset",
    ];

    // Pre-scan: find byte ranges that are inside modifier-label lists.
    // A modifier-label list is `(...)` that immediately follows (with optional
    // whitespace) one of: `by`, `without`, `on`, `ignoring`.
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
                    // skip whitespace
                    let mut j = i;
                    while j < len && (bytes[j] == b' ' || bytes[j] == b'\t') {
                        j += 1;
                    }
                    if j < len && bytes[j] == b'(' {
                        // find matching ')'
                        let open = j;
                        let mut depth = 1usize;
                        let mut k = j + 1;
                        while k < len && depth > 0 {
                            if bytes[k] == b'(' { depth += 1; }
                            else if bytes[k] == b')' { depth -= 1; }
                            k += 1;
                        }
                        skip_range.push((open, k)); // k points past ')'
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
    let mut bracket_depth = 0u32; // depth inside `[...]` range vectors
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

            // Never inject inside `[...]` (duration suffixes like `5m`, `1h`)
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
}
