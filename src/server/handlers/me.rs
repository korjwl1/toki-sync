use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::Deserialize;

use super::super::http::{AppError, AppState, extract_jwt};

// ─── /me/devices ────────────────────────────────────────────────────────────

pub async fn me_devices(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = extract_jwt(&headers, &state.jwt)?;
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT id, name, last_seen_at FROM devices WHERE user_id = ? ORDER BY last_seen_at DESC",
    )
    .bind(&claims.sub)
    .fetch_all(&state.db.pool)
    .await
    .map_err(AppError::internal)?;

    let devices: Vec<_> = rows.into_iter().map(|(id, name, last_seen)| {
        serde_json::json!({ "id": id, "name": name, "last_seen_at": last_seen })
    }).collect();

    Ok(Json(serde_json::json!({ "devices": devices })))
}

#[derive(Deserialize)]
pub struct RenameDeviceRequest {
    pub name: String,
}

pub async fn me_rename_device(
    headers: HeaderMap,
    State(state): State<AppState>,
    axum::extract::Path(device_id): axum::extract::Path<String>,
    Json(body): Json<RenameDeviceRequest>,
) -> Result<StatusCode, AppError> {
    let claims = extract_jwt(&headers, &state.jwt)?;

    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError { status: StatusCode::UNPROCESSABLE_ENTITY, message: "name must not be empty".into() });
    }

    let affected = sqlx::query(
        "UPDATE devices SET name = ? WHERE id = ? AND user_id = ?",
    )
    .bind(&name)
    .bind(&device_id)
    .bind(&claims.sub)
    .execute(&state.db.pool)
    .await
    .map_err(AppError::internal)?
    .rows_affected();

    if affected == 0 {
        return Err(AppError::not_found("device not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn me_delete_device(
    headers: HeaderMap,
    State(state): State<AppState>,
    axum::extract::Path(device_id): axum::extract::Path<String>,
) -> Result<StatusCode, AppError> {
    let claims = extract_jwt(&headers, &state.jwt)?;

    // Verify ownership first
    let exists: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM devices WHERE id = ? AND user_id = ?"
    )
    .bind(&device_id)
    .bind(&claims.sub)
    .fetch_one(&state.db.pool)
    .await
    .map_err(AppError::internal)?;

    if !exists {
        return Err(AppError::not_found("device not found"));
    }

    // Ownership confirmed -- safe to delete VM data
    if let Err(e) = state.vm.delete_device_series(&device_id).await {
        tracing::warn!("failed to delete VM series for device {device_id}: {e}");
    }

    sqlx::query("DELETE FROM devices WHERE id = ? AND user_id = ?")
        .bind(&device_id)
        .bind(&claims.sub)
        .execute(&state.db.pool)
        .await
        .map_err(AppError::internal)?;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

pub async fn me_change_password(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<ChangePasswordRequest>,
) -> Result<StatusCode, AppError> {
    let claims = extract_jwt(&headers, &state.jwt)?;

    let row: Option<(String,)> = sqlx::query_as("SELECT password_hash FROM users WHERE id = ?")
        .bind(&claims.sub)
        .fetch_optional(&state.db.pool)
        .await
        .map_err(AppError::internal)?;

    let (hash,) = row.ok_or_else(|| AppError::not_found("user not found"))?;

    let cur = body.current_password.clone();
    let valid = tokio::task::spawn_blocking(move || bcrypt::verify(&cur, &hash))
        .await
        .map_err(AppError::internal)?
        .map_err(AppError::internal)?;

    if !valid {
        return Err(AppError::unauthorized("current password incorrect"));
    }

    if body.new_password.len() < 8 {
        return Err(AppError { status: StatusCode::UNPROCESSABLE_ENTITY, message: "password must be at least 8 characters".into() });
    }

    let new_pw = body.new_password.clone();
    let new_hash = tokio::task::spawn_blocking(move || {
        bcrypt::hash(&new_pw, bcrypt::DEFAULT_COST)
    })
    .await
    .map_err(AppError::internal)?
    .map_err(AppError::internal)?;

    let now = chrono::Utc::now().timestamp();
    sqlx::query("UPDATE users SET password_hash = ?, updated_at = ? WHERE id = ?")
        .bind(&new_hash)
        .bind(now)
        .bind(&claims.sub)
        .execute(&state.db.pool)
        .await
        .map_err(AppError::internal)?;

    // Revoke all refresh tokens -- password change invalidates existing sessions
    sqlx::query("UPDATE refresh_tokens SET revoked = 1 WHERE user_id = ? AND revoked = 0")
        .bind(&claims.sub)
        .execute(&state.db.pool)
        .await
        .map_err(AppError::internal)?;

    Ok(StatusCode::NO_CONTENT)
}
