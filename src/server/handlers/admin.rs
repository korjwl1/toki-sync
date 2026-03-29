use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::Deserialize;

use super::super::http::{AppError, AppState, require_admin, validate_username};

// --- /admin/users ----------------------------------------------------------

pub async fn admin_list_users(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&headers, &state.jwt, &*state.db).await?;

    let rows = state.db.list_users().await.map_err(AppError::internal)?;

    let users: Vec<_> = rows.into_iter().map(|u| {
        serde_json::json!({ "id": u.id, "username": u.username, "role": u.role, "created_at": u.created_at, "active": u.active })
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

    // Protect built-in admin account from deletion
    if let Ok(Some(user)) = state.db.get_user_by_id(&user_id).await {
        if user.username == "admin" {
            return Err(AppError::forbidden("the built-in admin account cannot be deleted, only deactivated"));
        }
    }

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

// --- /admin/users/:id/role ------------------------------------------------

#[derive(Deserialize)]
pub struct AdminChangeRoleRequest {
    pub role: String,
}

pub async fn admin_change_user_role(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(body): Json<AdminChangeRoleRequest>,
) -> Result<StatusCode, AppError> {
    require_admin(&headers, &state.jwt, &*state.db).await?;

    if body.role != "user" && body.role != "admin" {
        return Err(AppError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: "role must be 'user' or 'admin'".into(),
        });
    }

    let updated = state.db.update_user_role(&user_id, &body.role).await.map_err(AppError::internal)?;
    if !updated {
        return Err(AppError::not_found("user not found"));
    }
    Ok(StatusCode::NO_CONTENT)
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

// --- /admin/pending -------------------------------------------------------

pub async fn admin_list_pending(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&headers, &state.jwt, &*state.db).await?;

    let rows = state.db.list_pending_registrations().await.map_err(AppError::internal)?;

    let pending: Vec<_> = rows.into_iter().map(|p| {
        serde_json::json!({ "id": p.id, "username": p.username, "requested_at": p.requested_at })
    }).collect();

    Ok(Json(serde_json::json!({ "pending": pending })))
}

pub async fn admin_approve_pending(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(pending_id): Path<String>,
) -> Result<StatusCode, AppError> {
    require_admin(&headers, &state.jwt, &*state.db).await?;

    let approved = state.db.approve_registration(&pending_id).await.map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            AppError::conflict("username already exists")
        } else {
            AppError::internal(e)
        }
    })?;
    if !approved {
        return Err(AppError::not_found("pending registration not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn admin_reject_pending(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(pending_id): Path<String>,
) -> Result<StatusCode, AppError> {
    require_admin(&headers, &state.jwt, &*state.db).await?;

    let rejected = state.db.reject_registration(&pending_id).await.map_err(AppError::internal)?;
    if !rejected {
        return Err(AppError::not_found("pending registration not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

// --- /admin/server-info ---------------------------------------------------

pub async fn admin_server_info(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&headers, &state.jwt, &*state.db).await?;

    let ds = &state.dynamic_settings;
    let reg_mode = ds.registration_mode().await;
    let oidc_issuer = ds.oidc_issuer().await;
    let oidc_client_id = ds.oidc_client_id().await;
    let oidc_redirect_uri = ds.oidc_redirect_uri().await;

    Ok(Json(serde_json::json!({
        "registration_mode": reg_mode,
        "oidc_enabled": !oidc_issuer.is_empty(),
        "oidc_issuer": if oidc_issuer.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(oidc_issuer.clone()) },
        "oidc_client_id": if oidc_client_id.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(oidc_client_id) },
        "oidc_redirect_uri": if oidc_redirect_uri.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(oidc_redirect_uri) },
        "storage_backend": state.storage_backend,
        "version": env!("CARGO_PKG_VERSION"),
    })))
}

// --- /admin/settings -------------------------------------------------------

pub async fn admin_list_settings(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&headers, &state.jwt, &*state.db).await?;

    let ds = &state.dynamic_settings;
    Ok(Json(serde_json::json!({
        "registration_mode": ds.registration_mode().await,
        "oidc_issuer": ds.oidc_issuer().await,
        "oidc_client_id": ds.oidc_client_id().await,
        "oidc_client_secret": ds.oidc_client_secret().await,
        "oidc_redirect_uri": ds.oidc_redirect_uri().await,
        "max_query_scope": ds.max_query_scope().await,
    })))
}

#[derive(Deserialize)]
pub struct UpdateSettingRequest {
    pub value: String,
}

const ALLOWED_SETTINGS: &[&str] = &[
    "registration_mode",
    "oidc_issuer",
    "oidc_client_id",
    "oidc_client_secret",
    "oidc_redirect_uri",
    "max_query_scope",
];

pub async fn admin_update_setting(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(key): Path<String>,
    Json(body): Json<UpdateSettingRequest>,
) -> Result<StatusCode, AppError> {
    require_admin(&headers, &state.jwt, &*state.db).await?;

    if !ALLOWED_SETTINGS.contains(&key.as_str()) {
        return Err(AppError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: format!("unknown setting: {key}"),
        });
    }

    // Validate registration_mode values
    if key == "registration_mode" && !["open", "approval", "closed"].contains(&body.value.as_str()) {
        return Err(AppError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: "registration_mode must be 'open', 'approval', or 'closed'".into(),
        });
    }

    // Validate max_query_scope values
    if key == "max_query_scope" && !["self", "team", "all"].contains(&body.value.as_str()) {
        return Err(AppError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: "max_query_scope must be 'self', 'team', or 'all'".into(),
        });
    }

    state.db.set_server_setting(&key, &body.value).await.map_err(AppError::internal)?;

    // Invalidate OIDC discovery cache when any OIDC setting changes
    if key.starts_with("oidc_") {
        let mut cache = state.oidc_discovery_cache.write().await;
        *cache = None;
    }

    Ok(StatusCode::NO_CONTENT)
}

// --- /admin/users/:id/active -----------------------------------------------

#[derive(Deserialize)]
pub struct SetActiveRequest {
    pub active: bool,
}

pub async fn admin_set_user_active(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(body): Json<SetActiveRequest>,
) -> Result<StatusCode, AppError> {
    let admin_id = require_admin(&headers, &state.jwt, &*state.db).await?;

    // Cannot deactivate yourself
    if !body.active && user_id == admin_id {
        return Err(AppError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: "cannot deactivate your own account".into(),
        });
    }

    let user = state.db.get_user_by_id(&user_id).await.map_err(AppError::internal)?;
    let user = match user {
        Some(u) => u,
        None => return Err(AppError::not_found("user not found")),
    };

    // When deactivating the built-in admin, ensure at least one other active admin exists
    if !body.active && user.username == "admin" {
        let other_admins = state.db.count_active_admins_except("admin").await.map_err(AppError::internal)?;
        if other_admins == 0 {
            return Err(AppError {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                message: "cannot deactivate admin: no other admin accounts".into(),
            });
        }
    }

    let updated = state.db.set_user_active(&user_id, body.active).await.map_err(AppError::internal)?;
    if !updated {
        return Err(AppError::not_found("user not found"));
    }

    // If deactivating, revoke all their refresh tokens
    if !body.active {
        state.db.revoke_user_refresh_tokens(&user_id).await.map_err(AppError::internal)?;
    }

    Ok(StatusCode::NO_CONTENT)
}
