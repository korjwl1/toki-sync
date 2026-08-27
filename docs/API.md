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
  "oidc_enabled": true
}
```

The public auth-info response does not expose the server version. Admins can
read the Cargo package version from `GET /admin/server-info`.

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

Queries are served directly from the EventStore. This is a deliberately limited
subset of toki's virtual-query language, not a general PromQL endpoint.

### `GET /api/v1/toki/query`

Single endpoint covering both instant (stat) and range (chart) queries. When `step` is supplied, results are bucketed; without `step`, a single aggregated result for the full `[start, end)` range is returned.

**Query Parameters**

| Parameter | Required | Description |
|-----------|----------|-------------|
| `query` | Yes | `usage`/`toki_tokens_total`, `cost`, `events`, or bare `windows`; optional `sum`, `increase`, provider equality filter, and one of `model`, `project`, `device_id` groupings |
| `start` | No | Epoch seconds, 13-digit epoch milliseconds, `YYYYMMDD`, or `YYYYMMDDhhmmss`. Defaults to `0` |
| `end` | No | Same formats as `start`. Defaults to now |
| `step` | No | Bucket size (e.g., `3600`, `1h`, `1d`, `1w`). Omit for instant query |
| `scope` | No | `self` (default), `team:<team_id>`, or `all`. Subject to server's `max_query_scope` |
| `tz` | No | IANA timezone name for bucket formatting (e.g., `Asia/Seoul`). Defaults to UTC |
| `start_of_week` | No | Week start for `step=1w` (`mon`-`sun`). Default `mon` |
| `no_cost` | No | Boolean query value; when true, suppress cost calculation |

The server uses a half-open `[start, end)` range. A date-only `end=YYYYMMDD`
is converted to the following local midnight, so the full date is included.
Numeric second/millisecond and `YYYYMMDDhhmmss` end bounds are exact and
exclusive. RFC 3339 and dashed dates are currently rejected with `400`; the
remote API does not yet accept every time spelling understood by local toki.

Range queries are capped at 2,000 time buckets. Only `provider="value"` with
the `=` operator is accepted as a label filter. Regex/negative matchers,
`offset`, `avg`, `count`, `sessions`, `projects`, and arbitrary PromQL are
rejected rather than approximated. Bare `windows` supports `scope=self` only.

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

`period` is `<ISO timestamp>|<group key>`. Codex provider entries use
`cached_input_tokens` and `reasoning_output_tokens` in place of the Claude cache
fields. Unless `no_cost=true`, usage/cost buckets and raw events include
`cost_usd` where the model has a known price; aggregated `events` is a count and
does not include cost.

### Query ceilings and partial results

- A request reads at most 200,000 events. This is a hard ceiling, not pagination.
- Bare raw `events` responses add top-level `"truncated": true` when that event
  ceiling is reached. Direct API clients must inspect it; the current toki CLI
  adapter discards this top-level flag.
- Aggregated queries are subject to the same 200,000-input ceiling, but the
  current response does **not** report when that input scan was cut off. Totals
  can therefore be partial. Narrow the time range until pagination or explicit
  aggregate truncation propagation is implemented.
- Aggregation additionally caps distinct `(bucket, group)` combinations at
  50,000 and reports `"truncated": true` when that cap is reached.
- The ClickHouse adapter buffers its `JSONEachRow` result through ureq's 10 MiB
  string reader. A wide raw query can fail with `502` before reaching 200,000
  events. The server does not currently provide a cursor or response-size-safe
  streaming path.
- Fjall team scans visit members sequentially. At the event ceiling, earlier
  members can consume the full allowance, so a truncated team result is not a
  globally time-ordered sample.

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
{
  "devices": [
    {
      "id": "550e8400-e29b-...",
      "name": "macbook-pro",
      "device_key": "stable-client-key",
      "last_seen_at": 1774693800
    }
  ]
}
```

---

### `DELETE /me/devices/:device_id`

Remove a device from the authenticated user's account.

**Response** `204 No Content`.

---

### `PATCH /me/devices/:device_id/name`

Rename a device.

**Request Body**

```json
{ "name": "work-laptop" }
```

**Response** `204 No Content`.

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

**Response** `204 No Content`. All of the user's refresh tokens are revoked.

---

### `GET /me/teams`

List team memberships for the authenticated user.

**Response** `200 OK`

```json
{
  "teams": [
    {
      "team_id": "team-uuid",
      "team_name": "engineering",
      "role": "member"
    }
  ]
}
```

---

## Monitor settings sync (JWT required)

An **opt-in** channel for toki_monitor's own configuration and dashboard
definitions. It is deliberately separate from the TCP sync protocol: that
protocol carries toki's collected usage data and only that. A toki user who does
not run the monitor never touches these endpoints.

Payloads are **opaque**. The server stores the exact bytes it is given and hands
the same bytes back — it never parses a dashboard, so there is no second
server-side idea of what a dashboard is to drift out of step with the monitor's.
`value` is therefore a **string**, not a JSON object: stringify your own
structure and it round-trips byte for byte.

Probe `GET /api/v1/capabilities` for `monitor_settings_v1` before using these —
an older server 404s the routes, which looks the same as an empty store.

**Keys** are 1–128 characters of ASCII letters, digits, `.`, `_`, `-`, `:`
(e.g. `dashboard:main`, `prefs.theme`). Anything else is `422`.

**Limits**

| Limit | Value | Exceeded |
|-------|-------|----------|
| Request body | 2 MiB | `413` (refused before the body is buffered) |
| One `value` | 256 KiB | `413` |
| Entries per user | 512 | `507` |
| Bytes per user | 8 MiB | `507` |
| Writes per user | 60 / 60s | `429` with `Retry-After` |

Reads are not rate limited: a client told to back off still has to be able to
read the current state. `PUT` and `DELETE` both count against the write budget.

### `GET /me/monitor/index`

What this user has stored, **without** the payloads, plus quota headroom. Compare
`version` against what you already hold and fetch only the entries that moved.

**Response** `200 OK`

```json
{
  "delete_cas": true,
  "entries": [
    { "key": "dashboard:main", "version": 3, "updated_at": 1756200000, "size_bytes": 4210 }
  ],
  "quota": {
    "max_entries": 512,
    "max_value_bytes": 262144,
    "max_total_bytes": 8388608,
    "used_entries": 1,
    "used_bytes": 4210
  }
}
```

### `GET /me/monitor/settings`

Everything this user has stored, payloads included.

**Response** `200 OK`

```json
{
  "entries": [
    { "key": "dashboard:main", "value": "{\"panels\":[]}", "version": 3, "updated_at": 1756200000 }
  ]
}
```

### `GET /me/monitor/settings/:key`

One entry.

**Response** `200 OK`

```json
{ "key": "dashboard:main", "value": "{\"panels\":[]}", "version": 3, "updated_at": 1756200000 }
```

`404` if this user has no entry under that key.

### `PUT /me/monitor/settings/:key`

Store or replace one entry.

**Request**

```json
{ "value": "{\"panels\":[]}", "if_version": 3 }
```

`if_version` is optional. When present the write only lands if the stored
version matches; `0` means "expect no entry" (create-only).

**Response** `200 OK`

```json
{
  "key": "dashboard:main",
  "version": 4,
  "updated_at": 1756200100,
  "previous_version": 3,
  "created": false
}
```

**Concurrent writes.** Two devices editing the same entry is the expected case,
not an error. The rule is **last-write-wins on the server clock**, the same rule
the window merge uses — but a client is never left guessing whether it won:

- Wrote **with** `if_version` and lost → `409 Conflict`, nothing written:

  ```json
  {
    "error": "version conflict: the stored entry moved since if_version was read",
    "key": "dashboard:main",
    "current_version": 5,
    "current_updated_at": 1756200090
  }
  ```

  Re-fetch, merge, retry against `current_version`.

- Wrote **without** `if_version` → the write always lands, but
  `previous_version` says what it replaced. If that is not the version you
  fetched, another device wrote in between and you just overwrote it.
  `previous_version` is `null` (and `created` is `true`) for a new entry.

`version` starts at 1 and increments per write. Deleting and re-creating a key
starts over at 1, so version 1 always means "this entry is new".

**Errors**

| Status | When |
|--------|------|
| `413 Payload Too Large` | `value` over 256 KiB, or a request body over 2 MiB |
| `422 Unprocessable Entity` | key outside the allowed shape |
| `429 Too Many Requests` | write budget exhausted; body carries `retry_after` |
| `507 Insufficient Storage` | per-user quota; body carries `quota` (`entries` \| `bytes`), `used`, `limit` |

### `DELETE /me/monitor/settings/:key`

Delete one entry. The row is removed, not tombstoned.

Optional query parameter `if_version=<n>` performs compare-and-swap deletion.
If the stored version differs, the server returns the same `409` version detail
shape as a conditional `PUT`. Probe the index's `delete_cas` flag before using
this with an older server.

**Response** `204 No Content`, or `404` if this user has no entry under that key.

Deleting an account cascades to its monitor settings, so an abandoned account
leaves nothing behind.

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
| `GET` | `/admin/server-info` | Registration/OIDC state, relational storage backend, and Cargo package version |

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
| `GET` | `/admin` | Administration console (HTML/JS SPA) |
| `GET` | `/login` | Login page (HTML) |

The built-in page manages users, devices, teams, registration, OIDC, and query
scope. It is not a token-usage chart dashboard. It authenticates via JWT stored
in browser `localStorage`; after OIDC login, tokens are passed via URL fragment.

---

## Capability discovery

`GET /api/v1/capabilities` is public and currently returns:

```json
{ "sync_windows_v1": true, "monitor_settings_v1": true }
```

Clients must probe this before sending optional protocol messages or using the
monitor settings channel. Older servers return `404`.

---

## TCP sync protocol reference (port 9090)

Port 9090 uses a custom binary protocol (bincode serialization), not HTTP. The
protocol is implemented in `toki-sync-protocol` and is not intended for direct
use. The source pins the published protocol v1.1.0 tag.

| Frame field | Size | Meaning |
|---|---|---|
| Message type | 4 bytes (u32 LE) | Frame type (`AUTH`, `SYNC_BATCH`, etc.) |
| Payload length | 4 bytes (u32 LE) | Payload byte count |
| Payload | N bytes | bincode-encoded message, optionally zstd-compressed |

For the full message-type table, handshake sequence, and design rationale, see [DESIGN.md — Sync Protocol](DESIGN.md#sync-protocol).
