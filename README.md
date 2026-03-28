# toki-sync

Multi-device token usage sync server for [toki](https://github.com/korjwl1/toki).

Collects token usage data from toki CLI clients, stores time-series in VictoriaMetrics, and provides a unified view across devices.

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

#### Scenario A: With Caddy (one-click TLS)

Best for fresh servers without an existing reverse proxy. Caddy handles TLS certificates automatically via Let's Encrypt.

```bash
# Add DuckDNS token to .env
echo "DUCKDNS_TOKEN=your-duckdns-token" >> .env

docker compose --profile caddy up -d
```

This starts three containers:
- **toki-sync-server** — sync protocol (TCP :9090) + auth API (HTTP :9091)
- **VictoriaMetrics** — time-series storage (internal only, not exposed)
- **Caddy** — TLS termination, exposes :443 (HTTPS) and :9090 (TLS-wrapped TCP)

#### Scenario B: Without Caddy (existing reverse proxy)

If you already have nginx, Traefik, or another proxy handling TLS:

```bash
docker compose up -d
```

This starts only toki-sync-server and VictoriaMetrics. Configure your existing proxy to forward:

| Traffic | Upstream |
|---|---|
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

## Environment Variables

| Variable | Required | Description |
|---|---|---|
| `TOKI_ADMIN_PASSWORD` | Yes | Admin account password (created on first start) |
| `TOKI_JWT_SECRET` | Yes | JWT signing key. Generate: `openssl rand -base64 32` |
| `TOKI_EXTERNAL_URL` | Yes | Public URL (e.g., `https://yourserver.duckdns.org`) |
| `DUCKDNS_TOKEN` | Caddy only | DuckDNS token for Let's Encrypt DNS challenge |
| `TOKI_VERSION` | No | Image tag (default: `latest`) |

## Configuration

Server configuration lives in `config/toki-sync.toml`. See `config/toki-sync.toml.example` for all options.

Key sections:
- `[server]` — bind address and ports
- `[auth]` — JWT settings, brute-force protection, registration toggle
- `[storage]` — SQLite database path
- `[backend]` — VictoriaMetrics endpoint
- `[log]` — log level and format

Environment variables are expanded in the config file using `${VAR_NAME}` syntax.

## Data Persistence

| Volume | Path | Contents | On loss |
|---|---|---|---|
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

## Client Setup

On each machine running toki:

```bash
# Enable sync
toki sync enable --server https://yourserver.duckdns.org

# Login (uses admin credentials or user account)
toki sync login

# Check sync status
toki sync status
```

The toki daemon will automatically batch and sync token usage data to the server.

## Architecture

```
toki CLI (daemon)
    │
    ├── TCP :9090 ──→ toki-sync-server ──→ VictoriaMetrics (time-series)
    │   (bincode sync protocol)            (internal, :8428)
    │
    └── HTTP :9091 ─→ toki-sync-server
        (auth API, PromQL proxy)
```

## License

FSL-1.1-Apache-2.0
