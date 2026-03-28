use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::Deserialize;

use super::super::http::{AppError, AppState, require_admin};

fn validate_username(username: &str) -> Result<(), AppError> {
    if username.len() < 3 || username.len() > 32 {
        return Err(AppError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: "username must be 3-32 characters".into(),
        });
    }
    if !username.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.') {
        return Err(AppError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: "username may only contain letters, digits, _, -, .".into(),
        });
    }
    Ok(())
}

// --- /admin/users ----------------------------------------------------------

pub async fn admin_list_users(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&headers, &state.jwt, &*state.db).await?;

    let rows = state.db.list_users().await.map_err(AppError::internal)?;

    let users: Vec<_> = rows.into_iter().map(|u| {
        serde_json::json!({ "id": u.id, "username": u.username, "role": u.role, "created_at": u.created_at })
    }).collect();

    Ok(Json(serde_json::json!({ "users": users })))
}

#[derive(Deserialize)]
pub struct CreateUserRequest {
    pub username: String,
    pub password: String,
    pub role: Option<String>,
}

pub async fn admin_create_user(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    require_admin(&headers, &state.jwt, &*state.db).await?;

    validate_username(&body.username)?;

    if body.password.len() < 8 || body.password.len() > 128 {
        return Err(AppError { status: StatusCode::UNPROCESSABLE_ENTITY, message: "password must be 8-128 characters".into() });
    }

    let pw = body.password.clone();
    let hash = tokio::task::spawn_blocking(move || bcrypt::hash(&pw, bcrypt::DEFAULT_COST))
        .await
        .map_err(AppError::internal)?
        .map_err(AppError::internal)?;

    let id = uuid::Uuid::new_v4().to_string();
    let role = body.role.as_deref().unwrap_or("user");
    if role != "user" && role != "admin" {
        return Err(AppError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: "role must be 'user' or 'admin'".into(),
        });
    }

    let new_user = crate::db::models::NewUser {
        id: id.clone(),
        username: body.username.clone(),
        password_hash: hash,
        role: role.to_string(),
    };
    state.db.create_user(&new_user).await.map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            AppError::conflict("username already exists")
        } else {
            AppError::internal(e)
        }
    })?;

    Ok((StatusCode::CREATED, Json(serde_json::json!({ "id": id, "username": body.username, "role": role }))))
}

pub async fn admin_delete_user(
    headers: HeaderMap,
    State(state): State<AppState>,
    axum::extract::Path(user_id): axum::extract::Path<String>,
) -> Result<StatusCode, AppError> {
    require_admin(&headers, &state.jwt, &*state.db).await?;

    // Delete VM series for all user's devices before cascade
    let device_ids = state.db.get_user_device_ids(&user_id).await.map_err(AppError::internal)?;

    for did in &device_ids {
        if let Err(e) = state.vm.delete_device_series(did).await {
            tracing::warn!("failed to delete VM series for device {did}: {e}");
        }
    }

    let deleted = state.db.delete_user(&user_id).await.map_err(AppError::internal)?;
    if !deleted {
        return Err(AppError::not_found("user not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct AdminChangePasswordRequest {
    pub password: String,
}

pub async fn admin_change_user_password(
    headers: HeaderMap,
    State(state): State<AppState>,
    axum::extract::Path(user_id): axum::extract::Path<String>,
    Json(body): Json<AdminChangePasswordRequest>,
) -> Result<StatusCode, AppError> {
    require_admin(&headers, &state.jwt, &*state.db).await?;

    // Verify user exists
    let user = state.db.get_user_by_id(&user_id).await.map_err(AppError::internal)?;
    if user.is_none() {
        return Err(AppError::not_found("user not found"));
    }

    if body.password.len() < 8 || body.password.len() > 128 {
        return Err(AppError { status: StatusCode::UNPROCESSABLE_ENTITY, message: "password must be 8-128 characters".into() });
    }

    let pw = body.password.clone();
    let new_hash = tokio::task::spawn_blocking(move || bcrypt::hash(&pw, bcrypt::DEFAULT_COST))
        .await
        .map_err(AppError::internal)?
        .map_err(AppError::internal)?;

    state.db.update_password(&user_id, &new_hash).await.map_err(AppError::internal)?;

    // Revoke all refresh tokens -- password change invalidates existing sessions
    state.db.revoke_user_refresh_tokens(&user_id).await.map_err(AppError::internal)?;

    Ok(StatusCode::NO_CONTENT)
}

pub async fn admin_list_devices(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&headers, &state.jwt, &*state.db).await?;

    let rows = state.db.list_all_devices().await.map_err(AppError::internal)?;

    let devices: Vec<_> = rows.into_iter().map(|d| {
        serde_json::json!({ "id": d.id, "name": d.name, "username": d.username, "last_seen_at": d.last_seen_at })
    }).collect();

    Ok(Json(serde_json::json!({ "devices": devices })))
}

pub async fn admin_delete_device(
    headers: HeaderMap,
    State(state): State<AppState>,
    axum::extract::Path(device_id): axum::extract::Path<String>,
) -> Result<StatusCode, AppError> {
    require_admin(&headers, &state.jwt, &*state.db).await?;

    // Delete the device's time-series data from VictoriaMetrics before removing from DB
    if let Err(e) = state.vm.delete_device_series(&device_id).await {
        tracing::warn!("failed to delete VM series for device {device_id}: {e}");
    }

    let deleted = state.db.delete_device(&device_id).await.map_err(AppError::internal)?;
    if !deleted {
        return Err(AppError::not_found("device not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}
