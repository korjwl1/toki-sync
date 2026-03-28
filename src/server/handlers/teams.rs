use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

use super::super::http::{AppError, AppState, extract_jwt, require_admin};
use super::metrics::{escape_label_value, inject_label_filter, QueryRangeParams};
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

    let teams = state.db.list_teams_with_member_count().await.map_err(AppError::internal)?;

    let team_list: Vec<_> = teams.into_iter().map(|t| {
        serde_json::json!({
            "id": t.id,
            "name": t.name,
            "member_count": t.member_count,
            "created_at": t.created_at,
        })
    }).collect();

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
    // Allow admin or team member
    let claims = extract_jwt(&headers, &state.jwt)?;
    let user_id = claims.sub;
    let is_admin = state.db.user_is_admin(&user_id).await.map_err(AppError::internal)?;
    if !is_admin {
        let role = state.db.get_team_member_role(&team_id, &user_id).await
            .map_err(AppError::internal)?;
        if role.is_none() {
            return Err(AppError::forbidden("must be team member or admin"));
        }
    }

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

    // Inject the team user filter into the query (shared with inject_user_label)
    let injected = inject_label_filter(&params.query, &injection);

    let step = params.step.as_deref().unwrap_or("60s");
    let result = state.vm.query_range(&injected, params.start, params.end, step)
        .await
        .map_err(AppError::bad_gateway)?;

    Ok((
        StatusCode::OK,
        [("Content-Type", "application/json")],
        result,
    ).into_response())
}

