# toki-sync HTTP API Reference

All HTTP endpoints are served on port 9091 (configurable via `[server].http_port`).

## Authentication

JWT-authenticated endpoints require the `Authorization` header:

```
Authorization: Bearer <access_token>
```

Access tokens expire after `access_token_ttl_secs` (default: 1 hour). Use the `/token/refresh` endpoint to obtain a new pair.

All error responses follow this format:

```json
{ "error": "error message" }
```

---

## Public Endpoints

### `GET /health`

Health check.

**Response** `200 OK`

```json
{ "status": "ok" }
```

---

### `POST /login`

Authenticate with username and password. Returns JWT access and refresh tokens.

**Request Body**

```json
{
  "username": "admin",
  "password": "your-password",
  "device_id": "macbook-pro"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `username` | string | Yes | Account username |
| `password` | string | Yes | Account password |
| `device_id` | string | No | Device identifier (included in refresh token for per-device management) |

**Response** `200 OK`

```json
{
  "access_token": "eyJhbGciOi...",
  "refresh_token": "eyJhbGciOi...",
  "token_type": "Bearer",
  "expires_in": 3600
}
```

**Errors**

| Status | Message | Description |
|--------|---------|-------------|
| `401` | `invalid credentials` | Wrong username or password |
| `401` | `this account uses OIDC login` | Password login not available for OIDC accounts |
| `429` | `too many attempts, retry after Ns` | Brute force lockout active |

---

### `POST /register`

Self-register a new user account. Only available when `registration_mode = "open"` or `registration_mode = "approval"` in config.

**Request Body**

```json
{
  "username": "newuser",
  "password": "strong-password"
}
```

| Field | Type | Required | Constraints |
|-------|------|----------|-------------|
| `username` | string | Yes | 3-32 characters, alphanumeric + `_`, `-`, `.` |
| `password` | string | Yes | 8-128 characters |

**Response** `201 Created`

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "username": "newuser"
}
```

**Errors**

| Status | Message | Description |
|--------|---------|-------------|
| `403` | `registration is disabled` | `registration_mode` is `"closed"` |
| `409` | `username already exists` | Duplicate username |
| `422` | `username must be 3-32 characters` | Invalid username length |
| `422` | `password must be 8-128 characters` | Invalid password length |

---

### `POST /token/refresh`

Refresh an access token using a refresh token. Implements one-time-use rotation: the old refresh token is invalidated and a new pair is returned.

**Request Body**

```json
{
  "refresh_token": "eyJhbGciOi...",
  "device_id": "macbook-pro"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `refresh_token` | string | Yes | Current refresh token |
| `device_id` | string | No | Device identifier |

**Response** `200 OK`

```json
{
  "access_token": "eyJhbGciOi...",
  "refresh_token": "eyJhbGciOi...",
  "token_type": "Bearer",
  "expires_in": 3600
}
```

**Errors**

| Status | Message | Description |
|--------|---------|-------------|
| `401` | `invalid or expired refresh token` | Token is expired, already used, or invalid |

---

### `POST /auth-method`

Check available authentication methods for a username. Returns `"password"` or `"oidc"` depending on server configuration.

**Request Body**

```json
{
  "username": "admin"
}
```

**Response** `200 OK` (password auth)

```json
{ "method": "password" }
```

**Response** `200 OK` (OIDC configured)

```json
{
  "method": "oidc",
  "auth_url": "/auth/oidc/authorize?redirect_uri=..."
}
```

---

### `GET /auth/info`

Returns server authentication configuration (registration mode, OIDC availability).

**Response** `200 OK`

```json
{
  "registration_mode": "open",
  "oidc_enabled": true,
  "server_version": "0.2.0"
}
```

---

## Device Code Flow Endpoints

The device code flow allows CLI tools to authenticate via browser without passing credentials on the command line.

### `POST /auth/device/code`

Request a device code for CLI authentication.

**Request Body**

```json
{
  "device_name": "macbook-pro"
}
```

**Response** `200 OK`

```json
{
  "device_code": "GMMhmHCXhWEzkobqIHGG_EnNYYNjPzoysSr99Uy_zNM",
  "user_code": "WDJB-MJHT",
  "verification_uri": "https://sync.example.com/device",
  "expires_in": 900,
  "interval": 5
}
```

---

### `GET /device`

Browser page where the user enters the `user_code` and authenticates.

---

### `POST /auth/device/token`

Poll for device code completion. The CLI polls this endpoint at the specified `interval`.

**Request Body**

```json
{
  "device_code": "GMMhmHCXhWEzkobqIHGG_EnNYYNjPzoysSr99Uy_zNM"
}
```

**Response** `200 OK` (authorization complete)

```json
{
  "access_token": "eyJhbGciOi...",
  "refresh_token": "eyJhbGciOi...",
  "token_type": "Bearer",
  "expires_in": 3600
}
```

**Response** `428 Precondition Required` (authorization pending)

```json
{ "error": "authorization_pending" }
```

**Errors**

| Status | Message | Description |
|--------|---------|-------------|
| `428` | `authorization_pending` | User hasn't authorized yet, keep polling |
| `400` | `expired_token` | Device code has expired |
| `400` | `access_denied` | User denied authorization |

---

### `POST /auth/device/verify`

Server-side endpoint called when the user submits the code in the browser. Requires an authenticated session (the user must be logged in).

**Request Body**

```json
{
  "user_code": "WDJB-MJHT"
}
```

**Response** `200 OK`

```json
{ "verified": true }
```

---

## OIDC Endpoints

These endpoints are only available when OIDC is configured (all `oidc_*` fields set in config).

### `GET /auth/oidc/authorize`

Initiates the OIDC login flow. Redirects the user to the identity provider.

**Query Parameters**

| Parameter | Required | Description |
|-----------|----------|-------------|
| `redirect_uri` | No | Client redirect URI after authentication (CLI flow: must be localhost) |

**Response** `307 Temporary Redirect` to the identity provider's authorization endpoint.

---

### `GET /auth/callback`

OIDC callback handler. Exchanges the authorization code for tokens and creates/finds the user.

**Query Parameters**

| Parameter | Description |
|-----------|-------------|
| `code` | Authorization code from the identity provider |
| `state` | CSRF state token |
| `error` | Error from the identity provider (optional) |

**Response**
- **CLI flow** (localhost `redirect_uri`): `307 Redirect` to `redirect_uri?access_token=...&refresh_token=...&token_type=Bearer&expires_in=...`
- **Browser flow** (no `redirect_uri`): `307 Redirect` to `/dashboard#access_token=...&refresh_token=...&expires_in=...`

---

## PromQL Proxy (JWT required, optional -- requires VictoriaMetrics)

These endpoints proxy PromQL queries to an external VictoriaMetrics instance with per-user label injection for data isolation. Each user can only query their own data.

These endpoints are only available when `[backend].vm_url` is configured in `toki-sync.toml`. Without VictoriaMetrics, these endpoints return an error.

### `GET /api/v1/query`

Instant PromQL query.

**Query Parameters**

| Parameter | Required | Description |
|-----------|----------|-------------|
| `query` | Yes | PromQL expression |
| `time` | No | Evaluation timestamp (RFC3339 or Unix timestamp) |

**Response** `200 OK` — VictoriaMetrics response format (when VM is configured):

```json
{
  "status": "success",
  "data": {
    "resultType": "vector",
    "result": [
      {
        "metric": { "__name__": "toki_tokens_total", "model": "claude-opus-4-6" },
        "value": [1711929600, "12345"]
      }
    ]
  }
}
```

**Response** `503 Service Unavailable` (when VM is not configured):

```json
{ "error": "PromQL proxy not available: VictoriaMetrics not configured" }
```

---

### `GET /api/v1/query_range`

Range PromQL query.

**Query Parameters**

| Parameter | Required | Description |
|-----------|----------|-------------|
| `query` | Yes | PromQL expression |
| `start` | Yes | Start timestamp |
| `end` | Yes | End timestamp |
| `step` | Yes | Query resolution step (e.g., `60s`, `5m`, `1h`) |

**Response** `200 OK` — VictoriaMetrics response format with `resultType: "matrix"` (when VM is configured).

---

## User Self-Service (JWT required)

### `GET /me/devices`

List all devices registered under the authenticated user.

**Response** `200 OK`

```json
[
  {
    "device_id": "550e8400-e29b-...",
    "device_name": "macbook-pro",
    "last_seen": "2026-03-28T10:30:00Z"
  }
]
```

---

### `DELETE /me/devices/:device_id`

Remove a device from the authenticated user's account.

**Response** `200 OK`

```json
{ "deleted": true }
```

---

### `PATCH /me/devices/:device_id/name`

Rename a device.

**Request Body**

```json
{ "name": "work-laptop" }
```

**Response** `200 OK`

```json
{ "updated": true }
```

---

### `PATCH /me/password`

Change the authenticated user's password.

**Request Body**

```json
{
  "current_password": "old-password",
  "new_password": "new-strong-password"
}
```

**Response** `200 OK`

```json
{ "updated": true }
```

---

### `GET /me/teams`

List team memberships for the authenticated user.

**Response** `200 OK`

```json
[
  {
    "team_id": "team-uuid",
    "team_name": "engineering"
  }
]
```

---

## Teams (JWT required)

### `GET /api/v1/teams/:team_id/query_range`

Aggregated PromQL range query across all team members. The server injects a regex label matcher for all users in the team. Requires VictoriaMetrics to be configured.

**Query Parameters** -- same as `/api/v1/query_range`.

**Response** `200 OK` -- VictoriaMetrics response format (when VM is configured).

---

## Admin Endpoints (JWT required, admin role)

All admin endpoints require a JWT from a user with the `admin` role.

### Settings

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/admin/settings` | Get current server settings (registration_mode, etc.) |
| `PATCH` | `/admin/settings` | Update server settings |

#### `PATCH /admin/settings`

**Request Body**

```json
{
  "registration_mode": "approval"
}
```

---

### Pending Users

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/admin/pending` | List users awaiting approval (when `registration_mode = "approval"`) |
| `POST` | `/admin/pending/:user_id/approve` | Approve a pending user |
| `DELETE` | `/admin/pending/:user_id` | Reject a pending user |

---

### Server Info

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/admin/server-info` | Server version, uptime, connected devices count, database stats |

---

### Role Management

| Method | Path | Description |
|--------|------|-------------|
| `PATCH` | `/admin/users/:user_id/role` | Change a user's role (`"admin"` or `"user"`) |

---

### Active Devices

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/admin/active` | List currently connected devices with real-time sync status |

---

### Users

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/admin/users` | List all users |
| `POST` | `/admin/users` | Create a user |
| `DELETE` | `/admin/users/:user_id` | Delete a user |
| `PATCH` | `/admin/users/:user_id/password` | Change a user's password |

#### `POST /admin/users`

**Request Body**

```json
{
  "username": "newuser",
  "password": "strong-password"
}
```

**Response** `201 Created`

```json
{
  "id": "550e8400-e29b-...",
  "username": "newuser"
}
```

#### `PATCH /admin/users/:user_id/password`

**Request Body**

```json
{ "password": "new-password" }
```

### Devices

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/admin/devices` | List all devices across all users |
| `DELETE` | `/admin/devices/:device_id` | Delete any device |

### Teams

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/admin/teams` | List all teams |
| `POST` | `/admin/teams` | Create a team |
| `DELETE` | `/admin/teams/:team_id` | Delete a team |
| `GET` | `/admin/teams/:team_id/members` | List team members |
| `POST` | `/admin/teams/:team_id/members` | Add a team member |
| `DELETE` | `/admin/teams/:team_id/members/:user_id` | Remove a team member |

#### `POST /admin/teams`

**Request Body**

```json
{ "name": "engineering" }
```

#### `POST /admin/teams/:team_id/members`

**Request Body**

```json
{ "user_id": "user-uuid" }
```

---

## Dashboard

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/` | Redirects to `/dashboard` |
| `GET` | `/dashboard` | Web dashboard (HTML/JS SPA) |
| `GET` | `/login` | Login page (HTML) |

The dashboard authenticates via JWT stored in browser `localStorage`. After OIDC login, tokens are passed via URL fragment (`#access_token=...`).

---

## TCP Sync Protocol (Port 9090)

The TCP port is **not** HTTP. It uses a custom binary protocol (bincode serialization) for toki daemon connections:

1. Client connects via TLS
2. Client sends `AuthRequest` (username + JWT or password)
3. Server responds with `AuthResponse` (success + device_id)
4. Client sends batches of `SyncBatch` (events, zstd-compressed if >= 100 items)
5. Server responds with `SyncAck` per batch

This protocol is implemented in the `toki-sync-protocol` shared crate and is not intended for direct use. Use the toki CLI (`toki settings sync enable`) to connect.
