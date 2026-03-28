use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

use super::super::http::{AppError, AppState, extract_jwt, require_admin};
use super::metrics::{escape_label_value, QueryRangeParams};
use crate::metrics::backend::MetricsBackend;

// ─── Admin: team management ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateTeamRequest {
    pub name: String,
}

pub async fn admin_create_team(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<CreateTeamRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    require_admin(&headers, &state.jwt, &*state.db).await?;

    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError { status: StatusCode::UNPROCESSABLE_ENTITY, message: "team name must not be empty".into() });
    }

    let id = uuid::Uuid::new_v4().to_string();
    state.db.create_team(&id, &name).await.map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            AppError::conflict("team name already exists")
        } else {
            AppError::internal(e)
        }
    })?;

    Ok((StatusCode::CREATED, Json(serde_json::json!({ "id": id, "name": name }))))
}

pub async fn admin_list_teams(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&headers, &state.jwt, &*state.db).await?;

    let teams = state.db.list_teams().await.map_err(AppError::internal)?;

    // For each team, get member count
    let mut team_list = Vec::with_capacity(teams.len());
    for t in teams {
        let members = state.db.list_team_members(&t.id).await.map_err(AppError::internal)?;
        team_list.push(serde_json::json!({
            "id": t.id,
            "name": t.name,
            "member_count": members.len(),
            "created_at": t.created_at,
        }));
    }

    Ok(Json(serde_json::json!({ "teams": team_list })))
}

pub async fn admin_delete_team(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(team_id): Path<String>,
) -> Result<StatusCode, AppError> {
    require_admin(&headers, &state.jwt, &*state.db).await?;

    let deleted = state.db.delete_team(&team_id).await.map_err(AppError::internal)?;
    if !deleted {
        return Err(AppError::not_found("team not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

// ─── Admin: team member management ──────────────────────────────────────────

#[derive(Deserialize)]
pub struct AddMemberRequest {
    pub user_id: String,
    pub role: Option<String>,
}

pub async fn admin_add_team_member(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(team_id): Path<String>,
    Json(body): Json<AddMemberRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    require_admin(&headers, &state.jwt, &*state.db).await?;

    // Verify team exists
    let team = state.db.get_team(&team_id).await.map_err(AppError::internal)?;
    if team.is_none() {
        return Err(AppError::not_found("team not found"));
    }

    // Verify user exists
    let user = state.db.get_user_by_id(&body.user_id).await.map_err(AppError::internal)?;
    if user.is_none() {
        return Err(AppError::not_found("user not found"));
    }

    let role = body.role.as_deref().unwrap_or("member");
    if role != "owner" && role != "admin" && role != "member" {
        return Err(AppError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: "role must be 'owner', 'admin', or 'member'".into(),
        });
    }

    state.db.add_team_member(&team_id, &body.user_id, role).await.map_err(|e| {
        if e.to_string().contains("UNIQUE") || e.to_string().contains("PRIMARY") {
            AppError::conflict("user is already a team member")
        } else {
            AppError::internal(e)
        }
    })?;

    Ok((StatusCode::CREATED, Json(serde_json::json!({ "team_id": team_id, "user_id": body.user_id, "role": role }))))
}

pub async fn admin_remove_team_member(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path((team_id, user_id)): Path<(String, String)>,
) -> Result<StatusCode, AppError> {
    require_admin(&headers, &state.jwt, &*state.db).await?;

    let removed = state.db.remove_team_member(&team_id, &user_id).await.map_err(AppError::internal)?;
    if !removed {
        return Err(AppError::not_found("team member not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn admin_list_team_members(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(team_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&headers, &state.jwt, &*state.db).await?;

    // Verify team exists
    let team = state.db.get_team(&team_id).await.map_err(AppError::internal)?;
    if team.is_none() {
        return Err(AppError::not_found("team not found"));
    }

    let members = state.db.list_team_members(&team_id).await.map_err(AppError::internal)?;
    let member_list: Vec<_> = members.into_iter().map(|m| {
        serde_json::json!({
            "user_id": m.user_id,
            "username": m.username,
            "role": m.role,
            "joined_at": m.joined_at,
        })
    }).collect();

    Ok(Json(serde_json::json!({ "members": member_list })))
}

// ─── User self-service: my teams ────────────────────────────────────────────

pub async fn me_teams(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = extract_jwt(&headers, &state.jwt)?;
    let teams = state.db.list_user_teams(&claims.sub).await.map_err(AppError::internal)?;

    let team_list: Vec<_> = teams.into_iter().map(|t| {
        serde_json::json!({
            "team_id": t.team_id,
            "team_name": t.team_name,
            "role": t.role,
        })
    }).collect();

    Ok(Json(serde_json::json!({ "teams": team_list })))
}

// ─── Team usage aggregation (PromQL proxy) ──────────────────────────────────

pub async fn team_query_range(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(team_id): Path<String>,
    Query(params): Query<QueryRangeParams>,
) -> Result<Response, AppError> {
    // Require admin for team-level queries
    require_admin(&headers, &state.jwt, &*state.db).await?;

    // Get all user_ids in the team
    let members = state.db.list_team_members(&team_id).await.map_err(AppError::internal)?;
    if members.is_empty() {
        return Err(AppError::not_found("team not found or has no members"));
    }

    // Build regex label matcher: user=~"user1|user2|user3"
    let user_regex = members.iter()
        .map(|m| escape_label_value(&m.user_id))
        .collect::<Vec<_>>()
        .join("|");
    let injection = format!("user=~\"{user_regex}\"");

    // Inject the team user filter into the query
    let injected = inject_team_label(&params.query, &injection);

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

/// Inject a team user filter (regex label matcher) into a PromQL expression.
/// Works similarly to inject_user_label but uses a pre-built injection string.
fn inject_team_label(expr: &str, injection: &str) -> String {
    let selector = format!("{{{injection}}}");

    // Path A: expression already has `{...}` selectors
    if expr.contains('{') {
        let mut result = String::with_capacity(expr.len() + injection.len() + 10);
        let mut chars = expr.chars().peekable();

        while let Some(&ch) = chars.peek() {
            if ch == '"' || ch == '\'' || ch == '`' {
                let quote = ch;
                result.push(chars.next().unwrap());
                while let Some(&c) = chars.peek() {
                    result.push(chars.next().unwrap());
                    if c == '\\' {
                        if chars.peek().is_some() {
                            result.push(chars.next().unwrap());
                        }
                    } else if c == quote {
                        break;
                    }
                }
                continue;
            }
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

    // Path B: no `{` -- inject after bare metric name tokens
    // Reuse the same keyword/skip logic from metrics.rs
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
                let is_fn = next == Some(b'(');
                let is_kw = KEYWORDS.contains(&tok);
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
