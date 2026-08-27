# toki-sync configuration reference

The binary defaults to `./config.toml`; `--config <path>` takes precedence over
`TOKI_SYNC_CONFIG`. The bundled Compose service sets `TOKI_SYNC_CONFIG` to
`/etc/toki-sync/config.toml` and mounts `config/toki-sync.toml` there.
Environment variables are expanded using `${VAR_NAME}` syntax; an unset
referenced variable expands to an empty string.

## Example config

```toml
[server]
# bind = "0.0.0.0"
tcp_port = 9090
http_port = 9091
# external_url = "${TOKI_EXTERNAL_URL}"
# trust_proxy = false
# max_concurrent_writes = 10

[auth]
jwt_secret = "${JWT_SECRET}"
# access_token_ttl_secs = 3600
# refresh_token_ttl_secs = 7776000
# brute_force_max_attempts = 5
# brute_force_window_secs = 300
# brute_force_lockout_secs = 900
# registration_mode = "closed"

[storage]
backend = "sqlite"
db_path = "/data/toki_sync.db"

[events]
backend = "fjall"
fjall_path = "/data/events.fjall"
# dedup_retention_secs = 2592000
# backend = "clickhouse"
# clickhouse_url = "http://clickhouse:8123"

[features]
# max_query_scope = "self"   # "self" | "team" | "all"

[log]
level = "info"
json = true
```

---

## Server section

`[server]` section.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `bind` | string | `0.0.0.0` | Network interface to bind |
| `http_port` | integer | `9091` | HTTP API port (REST, admin console, query endpoint) |
| `tcp_port` | integer | `9090` | TCP sync protocol port (toki daemon connections) |
| `external_url` | string | *(empty)* | Public URL for JWT `iss` and OIDC redirect URI. See below |
| `max_concurrent_writes` | integer | `10` | Maximum parallel event store batch writes |
| `trust_proxy` | boolean | `false` | Trust `X-Forwarded-For` / `X-Real-IP` headers. See below |

#### `external_url`

Used in the JWT `iss` claim and to derive the OIDC redirect URI. Example: `https://sync.example.com`.

#### `max_concurrent_writes`

Limits thundering-herd pressure when many devices sync simultaneously. The server queues additional batch writes once this limit is reached.

#### `trust_proxy`

When `true`, the server reads the client IP from proxy headers for brute force tracking. Only enable when toki-sync sits behind a trusted reverse proxy; otherwise clients can spoof their IP.

---

## Auth section

`[auth]` section.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `jwt_secret` | string | — | **Required.** HS256 signing key. See below |
| `access_token_ttl_secs` | integer | `3600` | Access token lifetime in seconds (default: 1 hour) |
| `refresh_token_ttl_secs` | integer | `7776000` | Refresh token lifetime in seconds (default: 90 days) |
| `brute_force_max_attempts` | integer | `5` | Failed login attempts before lockout |
| `brute_force_window_secs` | integer | `300` | Tracking window (default: 5 minutes) |
| `brute_force_lockout_secs` | integer | `900` | Lockout duration (default: 15 minutes) |
| `registration_mode` | string | `"closed"` | Self-registration policy. See below |
| `oidc_issuer` | string | *(empty)* | OIDC provider URL (e.g., `https://accounts.google.com`) |
| `oidc_client_id` | string | *(empty)* | OIDC client ID from your identity provider |
| `oidc_client_secret` | string | *(empty)* | OIDC client secret |
| `oidc_redirect_uri` | string | *(empty)* | OIDC callback URL (e.g., `https://sync.example.com/auth/callback`) |

#### `jwt_secret`

Use `${JWT_SECRET}` to read from the environment. Generate a strong value with `openssl rand -base64 32`.

#### `registration_mode`

Three policies for self-registration via `POST /register`:

- `"open"` — anyone can register.
- `"approval"` — registration creates a pending account; an admin must approve via `/admin/pending/:id/approve`.
- `"closed"` — only admins can create users via `/admin/users`.

### Brute force protection

Failed login attempts are tracked per IP + username pair. When `brute_force_max_attempts` is exceeded within `brute_force_window_secs`, the pair is locked out for `brute_force_lockout_secs`. The guard applies to `/login`, `/register`, and `/token/refresh`.

### OIDC configuration

To enable OIDC (Google, GitHub, etc.), set all four OIDC fields. The server performs standard OIDC discovery on startup and caches the result with a 1-hour TTL.

```toml
[auth]
jwt_secret = "${JWT_SECRET}"
oidc_issuer = "https://accounts.google.com"
oidc_client_id = "${OIDC_CLIENT_ID}"
oidc_client_secret = "${OIDC_CLIENT_SECRET}"
oidc_redirect_uri = "https://sync.example.com/auth/callback"
```

---

## Storage section

`[storage]` section.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `backend` | string | `sqlite` | Database backend: `sqlite` or `postgres` |
| `sqlite_path` | string | `./data/toki_sync.db` | SQLite database file path. Used when `backend = "sqlite"` |
| `db_path` | string | *(empty)* | Legacy alias for `sqlite_path`. See below |
| `postgres_url` | string | *(empty)* | PostgreSQL connection string. See below |

#### `db_path`

Older configs used `db_path` instead of `sqlite_path`. The two keys coexist for backward compatibility: if `db_path` is set and `sqlite_path` is still at its default, `db_path` is used. New configs should set `sqlite_path` only.

#### `postgres_url`

Used when `backend = "postgres"`. Example: `postgres://user:pass@host/dbname`.

### SQLite vs PostgreSQL

- **SQLite** (default): zero configuration, single-file database. Recommended for personal use and small teams.
- **PostgreSQL**: better concurrency for large teams. Requires an external PostgreSQL server.

```toml
# SQLite (default)
[storage]
backend = "sqlite"
sqlite_path = "/data/toki_sync.db"

# PostgreSQL
[storage]
backend = "postgres"
postgres_url = "postgres://toki:password@db:5432/toki_sync"
```

---

## Events section

`[events]` section.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `backend` | string | `fjall` | Event store backend: `fjall` (embedded, no external dependencies) or `clickhouse` (external ClickHouse server) |
| `fjall_path` | string | `/data/events.fjall` | Fjall database directory path. Used when `backend = "fjall"` |
| `clickhouse_url` | string | *(empty)* | ClickHouse HTTP endpoint. Required when `backend = "clickhouse"` |
| `dedup_retention_secs` | integer | `2592000` | Fjall dedup-index retention in seconds (30 days). Event rows themselves are not deleted |

### Fjall vs ClickHouse

- **Fjall** (default): embedded LSM-tree storage. Current dedup key is `(device_id, provider, msg_id)`.
- **ClickHouse**: external column-oriented database using `ReplacingMergeTree(ts_ms)`. It is implemented, but this repository has no live ClickHouse integration tests.

```toml
# Fjall (default — no external dependencies)
[events]
backend = "fjall"
fjall_path = "/data/events.fjall"

# ClickHouse (requires external ClickHouse server)
[events]
backend = "clickhouse"
clickhouse_url = "http://clickhouse:8123"
```

Set both values above, then run `docker compose --profile clickhouse up -d`.
The profile alone only starts ClickHouse; toki-sync otherwise keeps using Fjall.
Changing backends does not copy existing events.

### ClickHouse upgrade warning

A fresh `toki_events` table uses
`ORDER BY (device_id, provider, msg_id)`. Older deployments may still use
`ORDER BY (device_id, msg_id)`. Startup uses `CREATE TABLE IF NOT EXISTS`, so it
does not rewrite the old sorting key; Claude Code and Codex rows sharing a
device/message ID can then collapse. Check before upgrading:

```sql
SELECT sorting_key
FROM system.tables
WHERE database = currentDatabase() AND name = 'toki_events';
```

There is no automatic `toki_events` sort-key migration in this release. Back
up and plan a table rebuild/full re-sync before using an old table with mixed
providers. The separate `toki_windows.updated_at` migration is automatic, but
has not been tested here against a live ClickHouse instance.

---

## Log section

`[log]` section.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `level` | string | `info` | Log level: `trace`, `debug`, `info`, `warn`, `error` |
| `json` | boolean | `false` | Output logs in JSON format. Recommended for production (structured logging) |

---

## Features section

`[features]` section.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `max_query_scope` | string | `"self"` | Maximum query scope non-admin users may request on `/api/v1/toki/query`. One of `self`, `team`, `all`. Admins always get `all` |

---

## Environment variables

Environment variables are used in two ways:
1. **In TOML**: `${VAR_NAME}` syntax for expanding values inside `toki-sync.toml`
2. **In `.env`**: Docker Compose reads `.env` and injects variables into containers

| Variable | Required | Description |
|----------|----------|-------------|
| `TOKI_ADMIN_PASSWORD` | Initial setup | Creates the built-in `admin` only if absent; later changes do not change its password |
| `JWT_SECRET` | Production | JWT signing key when TOML references `${JWT_SECRET}`. Generate: `openssl rand -base64 32` |
| `TOKI_EXTERNAL_URL` | Deployment-dependent | Expanded only when TOML references it; Compose also passes it to Caddy as `TOKI_DOMAIN` |
| `DUCKDNS_TOKEN` | No current effect | Present in examples/Compose, but the bundled Caddy build and Caddyfile do not use a DuckDNS DNS module |
| `TOKI_VERSION` | No | Docker image tag (default: `latest`) |

### `.env` Example

```bash
# Required
TOKI_ADMIN_PASSWORD=your-strong-password
JWT_SECRET=base64-encoded-secret-here
TOKI_EXTERNAL_URL=https://yourserver.duckdns.org

# Caddy TLS (optional)
DUCKDNS_TOKEN=your-duckdns-token

# Image version (optional)
TOKI_VERSION=2.1.0
```

> **Security**: never commit `.env`. The bundled Caddyfile currently forces its
> internal CA; it does not obtain a public Let's Encrypt/DuckDNS certificate.

---

## Config loading

The server loads configuration in this order:

1. Select `--config`, then `TOKI_SYNC_CONFIG`, then the default `./config.toml`
2. Expand `${VAR_NAME}` placeholders with environment variable values
3. Parse TOML into the configuration struct
4. Apply defaults for any missing fields

If the selected file does not exist, the server uses built-in defaults with
`JWT_SECRET` from the environment, falling back to `change-me-in-production`
with a warning. An existing file must contain `[auth]` and `jwt_secret` after
environment expansion.

## Validation status

The current 116-test suite covers configuration parsing, SQLite-backed HTTP
paths, and Fjall. PostgreSQL and ClickHouse compile but have no live integration
tests here. The repository's Docker source build is also blocked until protocol
v1.1.0 is tagged and the temporary sibling patch is removed. Treat optional
backends and real migrations as unverified until exercised on disposable
instances resembling production.
