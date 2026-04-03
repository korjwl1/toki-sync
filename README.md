<p align="center">
  <img src="assets/logo.png" alt="toki-sync logo" width="160" />
</p>

<h1 align="center">toki-sync</h1>

<p align="center">
  <b>Self-hosted multi-device token usage sync server for <a href="https://github.com/korjwl1/toki">toki</a></b><br>
  Collects AI tool usage from all your machines, stores events locally, serves a unified dashboard.
</p>

<p align="center">
  <a href="https://hub.docker.com/r/korjwl11/toki-sync"><img src="https://img.shields.io/docker/v/korjwl11/toki-sync?sort=semver&label=Docker%20Hub" alt="Docker Hub" /></a>
  <a href="https://hub.docker.com/r/korjwl11/toki-sync"><img src="https://img.shields.io/docker/pulls/korjwl11/toki-sync" alt="Docker Pulls" /></a>
  <a href="https://hub.docker.com/r/korjwl11/toki-sync"><img src="https://img.shields.io/docker/image-size/korjwl11/toki-sync?sort=semver" alt="Docker Image Size" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-FSL--1.1--Apache--2.0-blue" alt="License" /></a>
</p>

<p align="center">
  <a href="README.ko.md">한국어</a>
</p>

---

## Quick Start

No `git clone` needed. Create a `docker-compose.yml` and `.env`, then start.

**1. Create `docker-compose.yml`**

```yaml
services:
  toki-sync:
    image: korjwl11/toki-sync:latest
    container_name: toki-sync
    restart: unless-stopped
    ports:
      - "9090:9090"   # sync protocol (TCP)
      - "9091:9091"   # web dashboard + API (HTTP)
    environment:
      TOKI_ADMIN_PASSWORD: ${TOKI_ADMIN_PASSWORD}
      JWT_SECRET: ${JWT_SECRET}
    volumes:
      - toki-data:/data

volumes:
  toki-data:
```

**2. Create `.env`**

```bash
TOKI_ADMIN_PASSWORD=change-me-to-a-strong-password
JWT_SECRET=change-me-run-openssl-rand-base64-32
```

**3. Start and connect**

```bash
docker compose up -d

# On any machine with toki installed (opens browser for authentication)
toki settings sync enable --server <your-server-ip-or-domain>
```

Done. Token usage now syncs automatically across all your devices.

> **Want automatic TLS?** See the [Caddy + DuckDNS deployment guide](docs/deploy-caddy-duckdns.md) for HTTPS with a free domain and auto-renewed certificates.

---

## Docker Image

| | |
|---|---|
| **Image** | [`korjwl11/toki-sync`](https://hub.docker.com/r/korjwl11/toki-sync) |
| **Tags** | `latest`, `2.0.0` |
| **Platforms** | `linux/amd64`, `linux/arm64` |

### Standalone (default)

Uses **Fjall** (embedded event store) + **SQLite** (metadata). Zero external dependencies -- just the single container above.

### With ClickHouse (optional)

For high-volume deployments, add the `--profile clickhouse` flag:

```bash
docker compose --profile clickhouse up -d
```

This starts a ClickHouse container alongside toki-sync for scalable event storage. See the full [`docker-compose.yml`](docker-compose.yml) for details.

---

## Who is this for?

- **Multiple machines?** See all your AI token usage in one place -- web dashboard or [Toki Monitor](https://github.com/korjwl1/toki-monitor).
- **Team dashboard?** Aggregate usage across team members with role-based access.
- **Self-hosted?** Your data stays on your server. No telemetry, no cloud.

---

## How it works

```
[Device A]  [Device B]  [Device C]
toki daemon  toki daemon  toki daemon
     +-- TCP+TLS (bincode) --+
                              v
                      toki-sync server
                      |-- TCP :9090 (sync protocol)
                      |-- HTTP :9091 (auth + dashboard)
                      +-- SQLite (metadata)
                      +-- Fjall (events) or ClickHouse (optional)
```

- **toki daemons** maintain persistent TLS connections, batch events (1,000/batch), zstd-compress, and send with ACK-based flow control
- **toki-sync server** authenticates users, stores metadata in SQLite, writes events to the event store
- **Deduplication** via `msg_id` ensures exactly-once delivery across reconnections

---

## Features

- **Multi-device sync** -- TCP binary protocol, zstd compression, ACK flow control, delta-sync on reconnect
- **Device code auth** -- browser-based device code flow, OIDC (Google, GitHub, etc.), password login
- **Web dashboard** -- charts, time range picker, device list, team views
- **Teams** -- aggregate queries across team members with role-based access
- **Dual storage** -- SQLite (zero-config) or PostgreSQL; Fjall (embedded) or ClickHouse (scale)
- **PromQL proxy** (optional) -- per-user label injection for VictoriaMetrics compatibility
- **Security** -- TLS everywhere, brute force protection, refresh token rotation

---

## Privacy & Security

- **No prompt access** -- only token counts and metadata (model, session ID, project name). Never prompts or responses.
- **TLS everywhere** -- all sync traffic encrypted. Caddy handles Let's Encrypt certificates automatically.
- **Per-user data isolation** -- each user can only query their own data.
- **Self-hosted** -- no telemetry, no cloud dependencies.

---

## Deployment Guides

| Scenario | Guide | Description |
|----------|-------|-------------|
| Caddy + DuckDNS | [Guide](docs/deploy-caddy-duckdns.md) | Automatic TLS with free domain (recommended) |
| Existing proxy | [Guide](docs/deploy-reverse-proxy.md) | nginx, Traefik, etc. |
| Self-signed TLS | [Guide](docs/deploy-self-signed.md) | IP-only servers, no domain |
| Local / LAN | [Guide](docs/deploy-local.md) | Development and testing |

See also: [Backup & Restore](docs/backup.md) | [Troubleshooting](docs/troubleshooting.md)

---

## Documentation

| Document | Description |
|----------|-------------|
| [Architecture & Design](docs/DESIGN.md) | Sync protocol, cursor management, security model, scaling |
| [Configuration Reference](docs/CONFIGURATION.md) | All TOML options, defaults, environment variables |
| [HTTP API Reference](docs/API.md) | All endpoints, request/response examples, authentication |

---

## Disconnecting

```bash
toki settings sync disable              # Prompts to delete remote data
toki settings sync disable --delete     # Delete this device's data from server
toki settings sync disable --keep       # Keep remote data, only disable locally
```

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
