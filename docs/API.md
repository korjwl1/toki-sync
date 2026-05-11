# toki-sync HTTP API reference

All HTTP endpoints are served on port 9091 (configurable via `[server].http_port`).

## Authentication

JWT-authenticated endpoints require the `Authorization` header:

```http
Authorization: Bearer <access_token>
```

Access tokens expire after `access_token_ttl_secs` (default: 1 hour). Use the `/token/refresh` endpoint to obtain a new pair.

All error responses follow this format:

```json
{ "error": "error message" }
```

---

## Public endpoints

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

## Device code flow endpoints

The device code flow allows CLI tools to authenticate via browser without passing credentials on the command line.

### `POST /device/code`

Request a device code for CLI authentication.

**Response** `200 OK`

```json
{
  "device_code": "550e8400-e29b-41d4-a716-446655440000",
  "user_code": "WDJB-MJHT",
  "verification_url": "https://sync.example.com/login/device",
  "expires_in": 300,
  "interval": 5
}
```

---

### `POST /device/token`

Poll for device code completion. The CLI polls this endpoint at the specified `interval`.

**Request Body**

```json
{
  "device_code": "550e8400-e29b-41d4-a716-446655440000",
  "device_key": "optional-stable-uuid",
  "device_name": "optional-hostname"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `device_code` | string | Yes | Code returned by `/device/code` |
| `device_key` | string | No | Stable client UUID. If supplied, the device is registered on approval |
| `device_name` | string | No | Human-readable device name (truncated to 64 chars) |

**Response** `200 OK` (authorization complete)

```json
{
  "access_token": "eyJhbGciOi...",
  "refresh_token": "eyJhbGciOi...",
  "token_type": "Bearer",
  "expires_in": 3600
}
```

**Errors**

| Status | Body | Description |
|--------|------|-------------|
| `400` | `{ "error": "authorization_pending" }` | User hasn't approved yet, keep polling |
| `400` | `{ "error": "slow_down", "interval": 10 }` | Client is polling faster than 5s |
| `400` | `{ "error": "expired_token" }` | Device code is unknown or already consumed |
| `410` | `{ "error": "expired_token" }` | Device code has expired |

---

### `POST /device/approve`

Approve a pending device code. Called by the browser after the user submits the `user_code` from a logged-in session. Requires JWT.

**Request Body**

```json
{
  "user_code": "WDJB-MJHT"
}
```

**Response** `204 No Content` on success.

**Errors**

| Status | Message | Description |
|--------|---------|-------------|
| `404` | `invalid or expired code` | Unknown user_code |
| `409` | `code already approved` | Code already consumed |
| `410` | `code expired` | Code expired |

---

## OIDC endpoints

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
- **Browser flow** (no `redirect_uri`): `307 Redirect` to `/admin#access_token=...&refresh_token=...&expires_in=...`

---

## Query (JWT required)

Queries are served directly from the EventStore. The same interface as the local toki daemon's REPORT protocol — toki virtual queries (`usage{}`, `events{}`, `cost{}`) and the daemon's JSON output format.

### `GET /api/v1/toki/query`

Single endpoint covering both instant (stat) and range (chart) queries. When `step` is supplied, results are bucketed; without `step`, a single aggregated result for the full `[start, end)` range is returned.

**Query Parameters**

| Parameter | Required | Description |
|-----------|----------|-------------|
| `query` | Yes | Toki virtual query: `usage{}`, `events{}`, or `cost{}`. Group-by via `by (model)` or `by (project)` |
| `start` | No | Epoch seconds, `YYYYMMDD`, or `YYYYMMDDhhmmss`. Defaults to `0` |
| `end` | No | Same formats as `start`. Defaults to now |
| `step` | No | Bucket size (e.g., `3600`, `1h`, `1d`, `1w`). Omit for instant query |
| `scope` | No | `self` (default), `team:<team_id>`, or `all`. Subject to server's `max_query_scope` |
| `tz` | No | IANA timezone name for bucket formatting (e.g., `Asia/Seoul`). Defaults to UTC |
| `start_of_week` | No | Week start for `step=1w` (`mon`-`sun`). Default `mon` |

Range queries are capped at 2000 buckets per request — the server rejects steps that would exceed this for the given range.

**Response** `200 OK`

```json
{
  "providers": {
    "claude_code": [
      {
        "period": "2026-03-28T00:00:00|claude-opus-4-6",
        "usage_per_models": [
          {
            "model": "claude-opus-4-6",
            "input_tokens": 12345,
            "output_tokens": 6789,
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 0,
            "total_tokens": 19134,
            "events": 42,
            "cost_usd": 0.18
          }
        ]
      }
    ]
  }
}
```

`period` is `<ISO timestamp>|<group key>`. Codex provider entries use `cached_input_tokens` and `reasoning_output_tokens` in place of the cache fields. `cost_usd` is only present for `cost{}` queries or when a pricing entry matches the model.

**Errors**

| Status | Description |
|--------|-------------|
| `400` | Invalid time format, invalid scope, step too small for range, or step > range |
| `403` | Scope not enabled by server (`max_query_scope`) or not a team member |
| `502` | EventStore backend unavailable |

---

## User self-service (JWT required)

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

## Admin endpoints (JWT required, admin role)

All admin endpoints require a JWT from a user with the `admin` role.

### Settings

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/admin/settings` | Get current server settings (registration_mode, OIDC fields, max_query_scope) |
| `PUT` | `/admin/settings/:key` | Update one setting by key |

Allowed `:key` values: `registration_mode`, `oidc_issuer`, `oidc_client_id`, `oidc_client_secret`, `oidc_redirect_uri`, `max_query_scope`.

#### `PUT /admin/settings/:key`

**Request Body**

```json
{ "value": "approval" }
```

**Response** `204 No Content`. Returns `422` if the key is unknown or the value fails validation (`registration_mode` must be `open|approval|closed`; `max_query_scope` must be `self|team|all`).

---

### Pending Users

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/admin/pending` | List users awaiting approval (when `registration_mode = "approval"`) |
| `POST` | `/admin/pending/:id/approve` | Approve a pending registration |
| `POST` | `/admin/pending/:id/reject` | Reject a pending registration |

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

### Users

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/admin/users` | List all users |
| `POST` | `/admin/users` | Create a user |
| `DELETE` | `/admin/users/:user_id` | Delete a user |
| `PATCH` | `/admin/users/:user_id/password` | Change a user's password |
| `PATCH` | `/admin/users/:user_id/active` | Activate or deactivate a user (`{ "active": bool }`) |

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
| `GET` | `/` | Redirects to `/admin` |
| `GET` | `/admin` | Admin dashboard (HTML/JS SPA) |
| `GET` | `/login` | Login page (HTML) |

The dashboard authenticates via JWT stored in browser `localStorage`. After OIDC login, tokens are passed via URL fragment (`#access_token=...`).

---

## TCP sync protocol reference (port 9090)

Port 9090 uses a custom binary protocol (bincode serialization), not HTTP. The protocol is implemented in the `toki-sync-protocol` crate and is not intended for direct use — connect via the toki CLI (`toki settings sync enable`).

| Frame field | Size | Meaning |
|---|---|---|
| Message type | 4 bytes (u32 LE) | Frame type (`AUTH`, `SYNC_BATCH`, etc.) |
| Payload length | 4 bytes (u32 LE) | Payload byte count |
| Payload | N bytes | bincode-encoded message, optionally zstd-compressed |

For the full message-type table, handshake sequence, and design rationale, see [DESIGN.md — Sync Protocol](DESIGN.md#sync-protocol).
