use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::Deserialize;

use super::super::http::{AppError, AppState, require_admin};

// ─── /admin/users ──────────────────────────────────────────────────────────

pub async fn admin_list_users(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&headers, &state.jwt, &state.db).await?;

    let rows: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT id, username, role, created_at FROM users ORDER BY created_at",
    )
    .fetch_all(&state.db.pool)
    .await
    .map_err(AppError::internal)?;

    let users: Vec<_> = rows.into_iter().map(|(id, username, role, created_at)| {
        serde_json::json!({ "id": id, "username": username, "role": role, "created_at": created_at })
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
    require_admin(&headers, &state.jwt, &state.db).await?;

    if body.password.len() < 8 {
        return Err(AppError { status: StatusCode::UNPROCESSABLE_ENTITY, message: "password must be at least 8 characters".into() });
    }

    let pw = body.password.clone();
    let hash = tokio::task::spawn_blocking(move || bcrypt::hash(&pw, bcrypt::DEFAULT_COST))
        .await
        .map_err(AppError::internal)?
        .map_err(AppError::internal)?;

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();
    let role = body.role.as_deref().unwrap_or("user");
    if role != "user" && role != "admin" {
        return Err(AppError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: "role must be 'user' or 'admin'".into(),
        });
    }

    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, created_at, updated_at) VALUES (?,?,?,?,?,?)",
    )
    .bind(&id)
    .bind(&body.username)
    .bind(&hash)
    .bind(role)
    .bind(now)
    .bind(now)
    .execute(&state.db.pool)
    .await
    .map_err(|e| {
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
    require_admin(&headers, &state.jwt, &state.db).await?;

    // Delete VM series for all user's devices before cascade
    let device_ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM devices WHERE user_id = ?"
    )
    .bind(&user_id)
    .fetch_all(&state.db.pool)
    .await
    .map_err(AppError::internal)?;

    for did in &device_ids {
        if let Err(e) = state.vm.delete_device_series(did).await {
            tracing::warn!("failed to delete VM series for device {did}: {e}");
        }
    }

    let affected = sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(&user_id)
        .execute(&state.db.pool)
        .await
        .map_err(AppError::internal)?
        .rows_affected();

    if affected == 0 {
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
    require_admin(&headers, &state.jwt, &state.db).await?;

    // Verify user exists
    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM users WHERE id = ?")
        .bind(&user_id)
        .fetch_optional(&state.db.pool)
        .await
        .map_err(AppError::internal)?;

    if exists.is_none() {
        return Err(AppError::not_found("user not found"));
    }

    if body.password.len() < 8 {
        return Err(AppError { status: StatusCode::UNPROCESSABLE_ENTITY, message: "password must be at least 8 characters".into() });
    }

    let pw = body.password.clone();
    let new_hash = tokio::task::spawn_blocking(move || bcrypt::hash(&pw, bcrypt::DEFAULT_COST))
        .await
        .map_err(AppError::internal)?
        .map_err(AppError::internal)?;

    let now = chrono::Utc::now().timestamp();
    sqlx::query("UPDATE users SET password_hash = ?, updated_at = ? WHERE id = ?")
        .bind(&new_hash)
        .bind(now)
        .bind(&user_id)
        .execute(&state.db.pool)
        .await
        .map_err(AppError::internal)?;

    // Revoke all refresh tokens -- password change invalidates existing sessions
    sqlx::query("UPDATE refresh_tokens SET revoked = 1 WHERE user_id = ? AND revoked = 0")
        .bind(&user_id)
        .execute(&state.db.pool)
        .await
        .map_err(AppError::internal)?;

    Ok(StatusCode::NO_CONTENT)
}

pub async fn admin_list_devices(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&headers, &state.jwt, &state.db).await?;

    let rows: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT d.id, d.name, u.username, d.last_seen_at FROM devices d
         JOIN users u ON d.user_id = u.id ORDER BY d.last_seen_at DESC",
    )
    .fetch_all(&state.db.pool)
    .await
    .map_err(AppError::internal)?;

    let devices: Vec<_> = rows.into_iter().map(|(id, name, username, last_seen)| {
        serde_json::json!({ "id": id, "name": name, "username": username, "last_seen_at": last_seen })
    }).collect();

    Ok(Json(serde_json::json!({ "devices": devices })))
}

pub async fn admin_delete_device(
    headers: HeaderMap,
    State(state): State<AppState>,
    axum::extract::Path(device_id): axum::extract::Path<String>,
) -> Result<StatusCode, AppError> {
    require_admin(&headers, &state.jwt, &state.db).await?;

    // Delete the device's time-series data from VictoriaMetrics before removing from DB
    if let Err(e) = state.vm.delete_device_series(&device_id).await {
        tracing::warn!("failed to delete VM series for device {device_id}: {e}");
    }

    let affected = sqlx::query("DELETE FROM devices WHERE id = ?")
        .bind(&device_id)
        .execute(&state.db.pool)
        .await
        .map_err(AppError::internal)?
        .rows_affected();

    if affected == 0 {
        return Err(AppError::not_found("device not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}
