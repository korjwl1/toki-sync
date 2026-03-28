<p align="center">
  <img src="assets/logo.png" alt="toki-sync logo" width="160" />
</p>

<h1 align="center">toki-sync</h1>

<p align="center">
  <b>Multi-device token usage sync server</b><br>
  Collects AI tool usage from all your machines, stores time-series in VictoriaMetrics, serves a unified dashboard.
</p>

<p align="center">
  Part of the <a href="https://github.com/korjwl1/toki">toki</a> ecosystem.
</p>

<p align="center">
  <a href="README.ko.md">🇰🇷 한국어</a>
</p>

---

## Table of Contents

- [Quick Start](#quick-start)
- [Architecture](#architecture)
- [Features](#features)
- [Configuration](#configuration)
- [Deployment](#deployment)
- [Client Setup](#client-setup)
- [API Reference](#api-reference)
- [Tech Stack](#tech-stack)
- [License](#license)

---

## Quick Start

### Prerequisites

- Docker and Docker Compose v2
- A domain name (e.g., `yourserver.duckdns.org`) pointing to your server

### 1. Clone and configure

```bash
git clone https://github.com/korjwl1/toki-sync.git
cd toki-sync

cp .env.example .env
cp config/toki-sync.toml.example config/toki-sync.toml
```

Edit `.env` and fill in the required values:

```bash
TOKI_ADMIN_PASSWORD=your-strong-password
TOKI_JWT_SECRET=$(openssl rand -base64 32)
TOKI_EXTERNAL_URL=https://yourserver.duckdns.org
```

### 2. Deploy

```bash
# With Caddy (automatic TLS via Let's Encrypt)
echo "DUCKDNS_TOKEN=your-duckdns-token" >> .env
docker compose --profile caddy up -d

# Without Caddy (bring your own reverse proxy)
docker compose up -d
```

### 3. Connect a device

```bash
# On any machine with toki installed
toki sync enable --server yourserver.duckdns.org:9090 --username admin
toki sync status
```

Done. Token usage now syncs automatically.

---

## Architecture

```
[Device A]  [Device B]  [Device C]
toki daemon  toki daemon  toki daemon
     └── TCP+TLS (bincode) ──┐
                              v
                      toki-sync server
                      ├── TCP :9090 (sync protocol)
                      ├── HTTP :9091 (auth + PromQL proxy + dashboard)
                      └── SQLite / PostgreSQL
                              │
                      VictoriaMetrics
                      (time-series storage)
```

- **TCP :9090** — binary sync protocol. toki daemons maintain persistent TLS connections, batch events (1,000/batch), zstd-compress (>=100 items), and send with ACK-based flow control.
- **HTTP :9091** — REST API for auth, PromQL proxy, device management, admin, teams, and the web dashboard. JWT-authenticated.
- **VictoriaMetrics** — stores all time-series data. Queried via PromQL proxy with per-user label injection for data isolation.

---

## Features

- **Multi-device sync** — TCP binary protocol with zstd compression, ACK flow control, delta-sync on reconnect
- **JWT authentication** — password-based and OIDC (Google, GitHub, etc.) login flows
- **PromQL proxy** — per-user label injection ensures data isolation; compatible with toki CLI `--remote` queries and Toki Monitor server mode
- **Web dashboard** — 4 chart panels, time range picker, device list, team views
- **Teams / organizations** — aggregate queries across team members
- **Dual database backend** — SQLite (default, zero-config) or PostgreSQL (for scale)
- **Docker deployment** — Caddy profile for automatic TLS, or bring your own reverse proxy
- **Brute force protection** — configurable attempt limits, lockout windows, IP-based tracking
- **Refresh token rotation** — secure token refresh with one-time-use rotation
- **Global batch throttling** — configurable concurrent VictoriaMetrics writes to prevent thundering herd

---

## Configuration

Server configuration lives in `config/toki-sync.toml`. Environment variables are expanded using `${VAR_NAME}` syntax.

### `[server]`

| Key | Default | Description |
|-----|---------|-------------|
| `bind` | `0.0.0.0` | Bind address |
| `http_port` | `9091` | HTTP API port |
| `tcp_port` | `9090` | TCP sync protocol port |
| `external_url` | — | Public URL for JWT `iss` and OIDC redirects |
| `max_concurrent_writes` | `10` | Max parallel VictoriaMetrics batch writes |

### `[auth]`

| Key | Default | Description |
|-----|---------|-------------|
| `jwt_secret` | — | **Required.** HS256 signing key |
| `access_token_ttl_secs` | `3600` | Access token lifetime (1h) |
| `refresh_token_ttl_secs` | `2592000` | Refresh token lifetime (30d) |
| `brute_force_max_attempts` | `5` | Failed attempts before lockout |
| `brute_force_window_secs` | `300` | Tracking window (5m) |
| `brute_force_lockout_secs` | `900` | Lockout duration (15m) |
| `allow_registration` | `false` | Allow open self-registration |
| `oidc_issuer` | — | OIDC provider URL (empty = disabled) |
| `oidc_client_id` | — | OIDC client ID |
| `oidc_client_secret` | — | OIDC client secret |
| `oidc_redirect_uri` | — | OIDC callback URL |

### `[storage]`

| Key | Default | Description |
|-----|---------|-------------|
| `backend` | `sqlite` | `sqlite` or `postgres` |
| `sqlite_path` | `./data/toki_sync.db` | SQLite database file path |
| `postgres_url` | — | PostgreSQL connection string |

### `[backend]`

| Key | Default | Description |
|-----|---------|-------------|
| `vm_url` | `http://victoriametrics:8428` | VictoriaMetrics endpoint |

### `[log]`

| Key | Default | Description |
|-----|---------|-------------|
| `level` | `info` | Log level (trace, debug, info, warn, error) |
| `json` | `false` | JSON log format |

### Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `TOKI_ADMIN_PASSWORD` | Yes | Admin account password (created on first start) |
| `TOKI_JWT_SECRET` | Yes | JWT signing key. Generate: `openssl rand -base64 32` |
| `TOKI_EXTERNAL_URL` | Yes | Public URL (e.g., `https://yourserver.duckdns.org`) |
| `DUCKDNS_TOKEN` | Caddy only | DuckDNS token for Let's Encrypt DNS challenge |
| `TOKI_VERSION` | No | Docker image tag (default: `latest`) |

---

## Deployment

### Scenario A: With Caddy (one-click TLS)

Best for fresh servers without an existing reverse proxy. Caddy handles TLS certificates automatically via Let's Encrypt.

```bash
echo "DUCKDNS_TOKEN=your-duckdns-token" >> .env
docker compose --profile caddy up -d
```

This starts three containers:
- **toki-sync-server** — sync protocol (TCP :9090) + auth API (HTTP :9091)
- **VictoriaMetrics** — time-series storage (internal only, not exposed)
- **Caddy** — TLS termination, exposes :443 (HTTPS) and :9090 (TLS-wrapped TCP)

### Scenario B: Without Caddy (existing reverse proxy)

If you already have nginx, Traefik, or another proxy handling TLS:

```bash
docker compose up -d
```

This starts only toki-sync-server and VictoriaMetrics. Configure your existing proxy to forward:

| Traffic | Upstream |
|---------|----------|
| HTTPS :443 | `http://127.0.0.1:9091` |
| TLS :9090 (TCP stream) | `127.0.0.1:9090` |

Example nginx config:

```nginx
# HTTP API
server {
    listen 443 ssl;
    server_name yourserver.example.com;
    location / {
        proxy_pass http://127.0.0.1:9091;
    }
}

# TCP sync (stream module)
stream {
    server {
        listen 9090 ssl;
        proxy_pass 127.0.0.1:9090;
    }
}
```

Note: In Scenario B, add port mappings to `toki-sync-server` in `docker-compose.yml`:

```yaml
ports:
  - "9091:9091"
  - "9090:9090"
```

### Scenario C: Self-signed TLS (IP-only servers)

For servers without a domain name (e.g., home lab on a local IP):

```bash
docker compose up -d
```

Clients connect with the `--insecure` flag to accept the self-signed certificate:

```bash
toki sync enable --server 1.2.3.4:9090 --insecure --username admin
```

### Data Persistence

| Volume | Path | Contents | On loss |
|--------|------|----------|---------|
| `toki-data` | `/data` | SQLite (users, devices, cursors) | Re-login + full re-sync required |
| `vm-data` | `/vm-data` | VictoriaMetrics time-series data | **Unrecoverable** |
| `caddy-data` | `/data` | Let's Encrypt certificates | Auto-reissue (rate limit: 5/week) |

### Backup

`vm-data` is the critical volume. VictoriaMetrics supports hot snapshots:

```bash
# Create snapshot (no downtime)
docker exec victoriametrics wget -qO- http://localhost:8428/snapshot/create

# List snapshots
docker exec victoriametrics wget -qO- http://localhost:8428/snapshot/list
```

For easier backup access, use bind mounts instead of named volumes:

```yaml
volumes:
  - ./data/vm:/vm-data
  - ./data/toki:/data
```

See [VictoriaMetrics backup docs](https://docs.victoriametrics.com/single-server-victoriametrics/#backups) for full details.

---

## Client Setup

On each machine with [toki](https://github.com/korjwl1/toki) installed:

```bash
# Scenario A/B: domain with valid TLS
toki sync enable --server yourserver.duckdns.org:9090 --username admin

# Scenario C: self-signed TLS (IP-only)
toki sync enable --server 1.2.3.4:9090 --insecure --username admin

# Check connection
toki sync status

# List all registered devices
toki sync devices

# Query server data from CLI
toki report query --remote 'sum by (model)(toki_tokens_total)'

# Disable sync
toki sync disable
```

The toki daemon automatically batches and syncs token usage data. On disconnect, events accumulate locally and delta-sync on reconnect.

For a GUI view, use [Toki Monitor](https://github.com/korjwl1/toki-monitor) — toggle Local/Server mode in the dashboard toolbar.

---

## API Reference

All HTTP endpoints are served on port 9091. JWT-authenticated endpoints require `Authorization: Bearer <token>`.

### Public

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/health` | Health check |
| `POST` | `/login` | Authenticate (username + password), returns JWT |
| `POST` | `/register` | Self-register (if `allow_registration` is enabled) |
| `POST` | `/token/refresh` | Refresh access token |
| `POST` | `/auth-method` | Check available auth methods for a username |

### OIDC

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/auth/oidc/authorize` | Initiate OIDC login flow |
| `GET` | `/auth/callback` | OIDC callback handler |

### PromQL Proxy (JWT required)

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/v1/query` | Instant PromQL query (per-user label injection) |
| `GET` | `/api/v1/query_range` | Range PromQL query (per-user label injection) |

### User Self-Service (JWT required)

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/me/devices` | List own devices |
| `DELETE` | `/me/devices/:device_id` | Remove a device |
| `PATCH` | `/me/devices/:device_id/name` | Rename a device |
| `PATCH` | `/me/password` | Change own password |
| `GET` | `/me/teams` | List own team memberships |

### Teams (JWT required)

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/v1/teams/:team_id/query_range` | Aggregated PromQL query for a team |

### Admin (JWT required, admin role)

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/admin/users` | List all users |
| `POST` | `/admin/users` | Create a user |
| `DELETE` | `/admin/users/:user_id` | Delete a user |
| `PATCH` | `/admin/users/:user_id/password` | Change a user's password |
| `GET` | `/admin/devices` | List all devices |
| `DELETE` | `/admin/devices/:device_id` | Delete a device |
| `GET` | `/admin/teams` | List all teams |
| `POST` | `/admin/teams` | Create a team |
| `DELETE` | `/admin/teams/:team_id` | Delete a team |
| `GET` | `/admin/teams/:team_id/members` | List team members |
| `POST` | `/admin/teams/:team_id/members` | Add a team member |
| `DELETE` | `/admin/teams/:team_id/members/:user_id` | Remove a team member |

### Dashboard

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/` | Redirect to `/dashboard` |
| `GET` | `/dashboard` | Web dashboard (HTML) |
| `GET` | `/login` | Login page (HTML) |

---

## Documentation

| Document | Description |
|----------|-------------|
| **[Configuration Reference](docs/CONFIGURATION.md)** | All TOML sections, fields, defaults, environment variables |
| **[HTTP API Reference](docs/API.md)** | All endpoints, request/response examples, authentication |

---

## Tech Stack

| Purpose | Choice | Rationale |
|---------|--------|-----------|
| HTTP framework | axum 0.7 | Async, tower middleware ecosystem |
| Async runtime | tokio | Full-featured async I/O |
| Database | sqlx 0.8 (SQLite + PostgreSQL) | Compile-time query checking, dual backend |
| Time-series | VictoriaMetrics | PromQL-compatible, low resource usage |
| Auth | jsonwebtoken 9 + bcrypt | JWT access/refresh tokens, secure password hashing |
| OIDC | reqwest + manual discovery | Standard OIDC flow without heavy framework |
| Sync protocol | toki-sync-protocol (shared crate) | Wire-compatible types, bincode serialization |
| Compression | zstd 0.13 | Fast batch compression for sync protocol |
| Serialization | bincode (sync), serde_json (API), toml (config) | Binary for performance, JSON for interop |
| Logging | tracing + tracing-subscriber | Structured logging with JSON output option |
| Config | toml 0.8 with `${ENV}` expansion | Simple, human-readable server configuration |

---

## License

[FSL-1.1-Apache-2.0](LICENSE)
