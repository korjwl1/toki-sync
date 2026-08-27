<p align="center">
  <img src="assets/logo.png" alt="toki-sync logo" width="160" />
</p>

<h1 align="center">toki-sync</h1>

<p align="center">
  <b>Self-hosted multi-device Claude Code / Codex usage aggregation server</b><br>
  Receives usage events from <a href="https://github.com/korjwl1/toki">toki</a>, stores them centrally, and serves authenticated query and administration APIs.
</p>

<p align="center">
  <a href="https://hub.docker.com/r/korjwl11/toki-sync"><img src="https://img.shields.io/docker/v/korjwl11/toki-sync?sort=semver&label=Docker%20Hub" alt="Docker Hub" /></a>
  <a href="https://hub.docker.com/r/korjwl11/toki-sync"><img src="https://img.shields.io/docker/pulls/korjwl11/toki-sync" alt="Docker Pulls" /></a>
  <a href="https://hub.docker.com/r/korjwl11/toki-sync"><img src="https://img.shields.io/docker/image-size/korjwl11/toki-sync?sort=semver" alt="Docker Image Size" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue" alt="MIT License" /></a>
</p>

<p align="center">
  <a href="README.ko.md">한국어</a>
</p>

---

## Quick start

No `git clone` needed. Create a `docker-compose.yml` and `.env`, then start.

**1. Create `docker-compose.yml`**

```yaml
services:
  toki-sync-server:
    image: korjwl11/toki-sync:latest
    container_name: toki-sync-server
    restart: unless-stopped
    ports:
      - "9090:9090"   # sync protocol (TCP)
      - "9091:9091"   # admin console + API (HTTP)
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

# Direct ports are plain HTTP/TCP; use only on a trusted local network.
toki settings sync enable --server <your-server-ip> --no-tls
```

Done. Token usage now syncs automatically across all your devices. Do not
publish this direct-port setup to the Internet; terminate HTTP and TCP TLS with
an external reverse proxy for production.

> **Need public TLS?** Use an [existing reverse proxy](docs/deploy-reverse-proxy.md).
> The bundled [DuckDNS/Caddy scenario](docs/deploy-caddy-duckdns.md) currently
> uses an internal CA and is not automatic public TLS.

---

## Docker image

| Field | Value |
|---|---|
| Image | [`korjwl11/toki-sync`](https://hub.docker.com/r/korjwl11/toki-sync) |
| Source package version | `2.2.0` |
| Platforms | `linux/amd64`, `linux/arm64` |

The examples use `latest`. For reproducible deployments, pin a Docker tag that
you have verified exists in the registry; the Cargo package version alone does
not prove that a matching image has been published.

### Standalone (default)

Uses **Fjall** (embedded event store) + **SQLite** (metadata). Zero external dependencies -- just the single container above.

### With ClickHouse (optional)

Starting the ClickHouse container is only half of the switch. Set the event
backend in `config/toki-sync.toml`, then enable the Compose profile:

```toml
[events]
backend = "clickhouse"
clickhouse_url = "http://clickhouse:8123"
```

```bash
docker compose --profile clickhouse up -d
```

This starts ClickHouse alongside toki-sync and directs new event reads/writes to
it. Data is not migrated automatically between Fjall and ClickHouse. Existing
ClickHouse installations also need the upgrade warning in
[`docs/CONFIGURATION.md`](docs/CONFIGURATION.md#clickhouse-upgrade-warning).

### Building from source

The source pins the published `toki-sync-protocol` v1.1.0 tag and does not
require a sibling checkout:

```bash
cargo build --release
docker build -t toki-sync:local .
```

---

## Who is this for?

- **Multiple machines?** Query all synchronized usage from the CLI or [Toki Monitor](https://github.com/korjwl1/toki-monitor).
- **Teams?** Aggregate usage across team members with role-based access.
- **Self-hosted?** Your data stays on your server. No telemetry, no cloud.

---

## How it works

```text
[Device A]  [Device B]  [Device C]
toki daemon  toki daemon  toki daemon
     +-- TCP+TLS (bincode) --+
                              v
                      toki-sync server
                      |-- TCP :9090 (sync protocol)
                      |-- HTTP :9091 (auth + query API + admin console)
                      +-- SQLite (metadata)
                      +-- Fjall (events) or ClickHouse (optional)
```

- **toki daemons** maintain persistent connections, batch events (1,000/batch), zstd-compress, and send with ACK-based flow control; TLS is enabled when the deployment supplies a TLS terminator
- **toki-sync server** authenticates users, stores metadata in SQLite, writes events to the event store
- **Idempotent upsert** uses `(device_id, provider, msg_id)` on current schemas so retransmission does not double-count

---

## Features

- **Multi-device sync** -- TCP binary protocol, zstd compression, ACK flow control, delta-sync on reconnect
- **Device code auth** -- browser-based device code flow, OIDC (Google, GitHub, etc.), password login
- **Admin console** -- user, device, team, registration, OIDC, and scope management
- **Teams** -- aggregate queries across team members with role-based access
- **Dual storage** -- SQLite (zero-config) or PostgreSQL; Fjall (embedded) or ClickHouse (scale)
- **Authenticated query API** -- the supported toki virtual-query subset for usage, cost, events, and windows
- **Security** -- brute force protection and refresh-token rotation; TLS is supplied by Caddy or your reverse proxy

---

## Privacy and security

- **No prompt access** -- only token counts and metadata (model, session ID, project name). Never prompts or responses.
- **TLS by deployment** -- the server listens with plain TCP/HTTP internally. Use the bundled self-signed Caddy profile or a correctly configured external reverse proxy for encrypted public traffic.
- **Per-user data isolation** -- each user can only query their own data.
- **Self-hosted** -- no telemetry, no cloud dependencies.

---

## Documentation

Start with the [deployment guide](docs/deployment.md) to pick a scenario, then refer to the documents below as needed.

| Document | When to read |
|---|---|
| [Deployment guide](docs/deployment.md) | Pick a scenario (A/B/C/D) based on your infra |
| [Architecture and design](docs/DESIGN.md) | Implemented protocol, cursor, storage, limits, and validation status |
| [Configuration reference](docs/CONFIGURATION.md) | All TOML options, defaults, environment variables |
| [HTTP API reference](docs/API.md) | All endpoints, request/response examples, authentication |
| [Custom dashboards](docs/custom-dashboard.md) | Build a custom UI on top of the toki-sync query API |
| [Backup and restore](docs/backup.md) | Volume layout, hot/cold backup, recovery |
| [Troubleshooting](docs/troubleshooting.md) | Diagnose connection, TLS, query, storage, and sync issues |
| [Contributing](CONTRIBUTING.md) | Dev setup, branch naming, commit conventions, DCO |

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

Commercial use is permitted by the MIT license; sponsorship is optional and
supports continued maintenance.

---

## License

[MIT](LICENSE)
