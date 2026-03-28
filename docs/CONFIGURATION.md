# toki-sync Configuration Reference

Server configuration lives in `config/toki-sync.toml`. Environment variables are expanded using `${VAR_NAME}` syntax — if the variable is unset, it expands to an empty string.

## Example

```toml
[server]
# bind = "0.0.0.0"
tcp_port = 9090
http_port = 9091

[auth]
jwt_secret = "${JWT_SECRET}"
# access_token_ttl_secs = 3600
# refresh_token_ttl_secs = 2592000
# brute_force_max_attempts = 5
# brute_force_window_secs = 300
# brute_force_lockout_secs = 900
# allow_registration = false

[storage]
db_path = "/data/toki_sync.db"

[backend]
vm_url = "http://victoriametrics:8428"

[log]
level = "info"
json = true
```

---

## `[server]`

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `bind` | string | `0.0.0.0` | Network interface to bind |
| `http_port` | integer | `9091` | HTTP API port (REST, dashboard, PromQL proxy) |
| `tcp_port` | integer | `9090` | TCP sync protocol port (toki daemon connections) |
| `external_url` | string | *(empty)* | Public URL used for JWT `iss` claim and OIDC redirect URI derivation. Example: `https://sync.example.com` |
| `max_concurrent_writes` | integer | `10` | Maximum parallel VictoriaMetrics batch writes. Limits thundering-herd pressure when many devices sync simultaneously |

---

## `[auth]`

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `jwt_secret` | string | — | **Required.** HS256 signing key for JWT tokens. Use `${JWT_SECRET}` to read from environment. Generate with `openssl rand -base64 32` |
| `access_token_ttl_secs` | integer | `3600` | Access token lifetime in seconds (default: 1 hour) |
| `refresh_token_ttl_secs` | integer | `2592000` | Refresh token lifetime in seconds (default: 30 days) |
| `brute_force_max_attempts` | integer | `5` | Maximum failed login attempts before lockout |
| `brute_force_window_secs` | integer | `300` | Time window for tracking failed attempts (default: 5 minutes) |
| `brute_force_lockout_secs` | integer | `900` | Lockout duration after max attempts exceeded (default: 15 minutes) |
| `allow_registration` | boolean | `false` | Allow self-registration via `POST /register`. When `false`, only admins can create users |
| `oidc_issuer` | string | *(empty)* | OIDC provider URL (e.g., `https://accounts.google.com`). Empty = OIDC disabled |
| `oidc_client_id` | string | *(empty)* | OIDC client ID from your identity provider |
| `oidc_client_secret` | string | *(empty)* | OIDC client secret |
| `oidc_redirect_uri` | string | *(empty)* | OIDC callback URL (e.g., `https://sync.example.com/auth/callback`) |

### Brute Force Protection

Brute force protection tracks failed login attempts per IP + username combination. When `brute_force_max_attempts` is exceeded within `brute_force_window_secs`, the IP+username pair is locked out for `brute_force_lockout_secs`. This applies to `/login`, `/register`, and `/token/refresh` endpoints.

### OIDC Configuration

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

## `[storage]`

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `backend` | string | `sqlite` | Database backend: `sqlite` or `postgres` |
| `sqlite_path` | string | `./data/toki_sync.db` | SQLite database file path. Used when `backend = "sqlite"` |
| `db_path` | string | *(empty)* | Legacy alias for `sqlite_path` (backward compatible). If set and `sqlite_path` is default, this value is used |
| `postgres_url` | string | *(empty)* | PostgreSQL connection string. Used when `backend = "postgres"`. Example: `postgres://user:pass@host/dbname` |

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

## `[backend]`

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `vm_url` | string | `http://victoriametrics:8428` | VictoriaMetrics HTTP endpoint. The server writes time-series data here and proxies PromQL queries through it |

In Docker Compose, VictoriaMetrics runs as a service named `victoriametrics`, so the default URL works out of the box. If running outside Docker, adjust to the actual VictoriaMetrics address.

---

## `[log]`

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `level` | string | `info` | Log level: `trace`, `debug`, `info`, `warn`, `error` |
| `json` | boolean | `false` | Output logs in JSON format. Recommended for production (structured logging) |

---

## Environment Variables

Environment variables are used in two ways:
1. **In TOML**: `${VAR_NAME}` syntax for expanding values inside `toki-sync.toml`
2. **In `.env`**: Docker Compose reads `.env` and injects variables into containers

| Variable | Required | Description |
|----------|----------|-------------|
| `TOKI_ADMIN_PASSWORD` | Yes | Admin account password. Created automatically on first server start |
| `JWT_SECRET` | Yes | JWT signing key. Generate: `openssl rand -base64 32` |
| `TOKI_EXTERNAL_URL` | Yes | Public URL (e.g., `https://yourserver.duckdns.org`). Used for JWT `iss` and OIDC redirects |
| `DUCKDNS_TOKEN` | Caddy profile only | DuckDNS API token for Let's Encrypt DNS-01 challenge |
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
TOKI_VERSION=0.1.0
```

> **Security**: never commit `.env` to version control. The `.env.example` file is provided as a template.

---

## Config Loading

The server loads configuration in this order:

1. Read `config/toki-sync.toml` (or the path specified by the `--config` flag)
2. Expand `${VAR_NAME}` placeholders with environment variable values
3. Parse TOML into the configuration struct
4. Apply defaults for any missing fields

If the config file does not exist, the server uses built-in defaults with `JWT_SECRET` read from the environment (falling back to `change-me-in-production` if unset).
