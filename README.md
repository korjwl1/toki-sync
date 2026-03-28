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
  <a href="README.ko.md">한국어</a>
</p>

---

## Quick Start

```bash
git clone https://github.com/korjwl1/toki-sync.git
cd toki-sync
cp .env.example .env
cp config/toki-sync.toml.example config/toki-sync.toml
```

Edit `.env`:

```bash
TOKI_ADMIN_PASSWORD=your-strong-password
JWT_SECRET=$(openssl rand -base64 32)
TOKI_EXTERNAL_URL=https://myserver.duckdns.org
DUCKDNS_TOKEN=your-duckdns-token
```

Deploy and connect:

```bash
docker compose --profile caddy up -d

# On any machine with toki installed
toki settings sync enable --server myserver.duckdns.org:9090 --username admin
```

Done. Token usage now syncs automatically.

> First time with DuckDNS? The [Caddy + DuckDNS guide](docs/deploy-caddy-duckdns.md) walks you through every step from signup to verification.

---

## Who is this for?

- **Using AI tools on multiple machines?** See all your token usage in one place — queryable via PromQL, visible in the web dashboard or [Toki Monitor](https://github.com/korjwl1/toki-monitor).

- **Want a team usage dashboard?** Aggregate token usage across team members with team-scoped PromQL queries and role-based access control.

- **Need a self-hosted solution?** One `docker compose up` gives you a complete sync server with automatic TLS, no cloud dependencies, no telemetry.

---

## How it works

```
[Device A]  [Device B]  [Device C]
toki daemon  toki daemon  toki daemon
     +-- TCP+TLS (bincode) --+
                              v
                      toki-sync server
                      |-- TCP :9090 (sync protocol)
                      |-- HTTP :9091 (auth + PromQL proxy + dashboard)
                      +-- SQLite / PostgreSQL
                              |
                      VictoriaMetrics
                      (time-series storage)
```

- **toki daemons** maintain persistent TLS connections, batch events (1,000/batch), zstd-compress, and send with ACK-based flow control
- **toki-sync server** authenticates users, stores metadata in SQLite/PostgreSQL, writes time-series to VictoriaMetrics
- **PromQL proxy** injects per-user labels for data isolation — each user only sees their own data

---

## Features

- **Multi-device sync** — TCP binary protocol with zstd compression, ACK flow control, delta-sync on reconnect
- **JWT authentication** — password-based and OIDC (Google, GitHub, etc.) login flows
- **PromQL proxy** — per-user label injection for data isolation; compatible with toki CLI `--remote` and Toki Monitor
- **Web dashboard** — chart panels, time range picker, device list, team views
- **Teams / organizations** — aggregate queries across team members
- **Dual database backend** — SQLite (default, zero-config) or PostgreSQL (for scale)
- **Docker deployment** — Caddy profile for automatic TLS, or bring your own reverse proxy
- **Brute force protection** — configurable attempt limits, lockout windows, IP-based tracking
- **Refresh token rotation** — secure token refresh with one-time-use rotation

---

## Privacy & Security

- **No prompt access** — only token counts and metadata (model, session ID, project name) are transmitted. Never prompts or responses.
- **TLS everywhere** — all sync traffic is encrypted. Caddy handles certificates automatically via Let's Encrypt.
- **Per-user data isolation** — PromQL proxy injects user labels, so each user can only query their own data.
- **Self-hosted** — your data stays on your server. No telemetry, no cloud dependencies.

---

## Deployment

| Scenario | Guide | Description |
|----------|-------|-------------|
| Caddy + DuckDNS | [Guide](docs/deploy-caddy-duckdns.md) | One-click TLS with free domain (recommended) |
| Existing proxy | [Guide](docs/deploy-reverse-proxy.md) | nginx, Traefik, etc. |
| Self-signed TLS | [Guide](docs/deploy-self-signed.md) | IP-only servers, no domain |
| Local / LAN | [Guide](docs/deploy-local.md) | Development and testing |

See also: [Backup & Restore](docs/backup.md) | [Troubleshooting](docs/troubleshooting.md)

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

## Sponsor

<a href="https://github.com/sponsors/korjwl1">
  <img src="https://img.shields.io/badge/Sponsor-%E2%9D%A4-pink?style=for-the-badge&logo=github" alt="Sponsor" />
</a>

If toki-sync is useful to you, consider sponsoring to support development.

For commercial use in paid products, please sponsor or [reach out](mailto:korjwl1@gmail.com).

---

## License

[FSL-1.1-Apache-2.0](LICENSE)
