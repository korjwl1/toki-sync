use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::Deserialize;

use super::super::http::{AppError, AppState, extract_jwt};

// --- /me/devices -----------------------------------------------------------

pub async fn me_devices(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = extract_jwt(&headers, &state.jwt)?;
    let rows = state.db.list_user_devices(&claims.sub).await.map_err(AppError::internal)?;

    let devices: Vec<_> = rows.into_iter().map(|d| {
        serde_json::json!({ "id": d.id, "name": d.name, "device_key": d.device_key, "last_seen_at": d.last_seen_at })
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

    let renamed = state.db.rename_device(&device_id, &claims.sub, &name).await.map_err(AppError::internal)?;
    if !renamed {
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
    let belongs = state.db.device_belongs_to_user(&device_id, &claims.sub).await.map_err(AppError::internal)?;
    if !belongs {
        return Err(AppError::not_found("device not found"));
    }

    // Ownership confirmed -- safe to delete VM data
    if let Err(e) = state.vm.delete_device_series(&device_id).await {
        tracing::warn!("failed to delete VM series for device {device_id}: {e}");
    }

    state.db.delete_user_device(&device_id, &claims.sub).await.map_err(AppError::internal)?;

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

    let user = state.db.get_user_by_id(&claims.sub).await.map_err(AppError::internal)?;
    let user = user.ok_or_else(|| AppError::not_found("user not found"))?;

    let cur = body.current_password.clone();
    let hash = user.password_hash;
    let valid = tokio::task::spawn_blocking(move || bcrypt::verify(&cur, &hash))
        .await
        .map_err(AppError::internal)?
        .map_err(AppError::internal)?;

    if !valid {
        return Err(AppError::unauthorized("current password incorrect"));
    }

    if body.new_password.len() < 8 || body.new_password.len() > 128 {
        return Err(AppError { status: StatusCode::UNPROCESSABLE_ENTITY, message: "password must be 8-128 characters".into() });
    }

    let new_pw = body.new_password.clone();
    let new_hash = tokio::task::spawn_blocking(move || {
        bcrypt::hash(&new_pw, bcrypt::DEFAULT_COST)
    })
    .await
    .map_err(AppError::internal)?
    .map_err(AppError::internal)?;

    state.db.update_password(&claims.sub, &new_hash).await.map_err(AppError::internal)?;

    // Revoke all refresh tokens -- password change invalidates existing sessions
    state.db.revoke_user_refresh_tokens(&claims.sub).await.map_err(AppError::internal)?;

    Ok(StatusCode::NO_CONTENT)
}
