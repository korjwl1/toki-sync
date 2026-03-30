# Scenario C: Self-signed TLS (IP-only)

For servers without a domain name (e.g., home lab on a local IP). Caddy generates a self-signed certificate automatically.

---

## Prerequisites

- **Docker** and **Docker Compose v2** installed on your server
- A server with a known IP address (public or LAN)

---

## Step 1: Clone and configure

```bash
git clone https://github.com/korjwl1/toki-sync.git
cd toki-sync

cp .env.example .env
cp config/toki-sync.toml.example config/toki-sync.toml
```

Edit `.env` — leave `DUCKDNS_TOKEN` empty:

```bash
TOKI_ADMIN_PASSWORD=your-strong-password
JWT_SECRET=$(openssl rand -base64 32)
TOKI_EXTERNAL_URL=https://192.168.1.100
# DUCKDNS_TOKEN is not set — Caddy will use tls internal (self-signed)
```

---

## Step 2: Deploy

**With Caddy** (self-signed mode):

```bash
docker compose --profile caddy up -d
```

> Note: Your Caddyfile must be configured to use `tls internal` when no DuckDNS token is provided. See `caddy/Caddyfile` for the template logic.

**Without Caddy** (expose ports directly):

```bash
docker compose up -d
```

Add port mappings to `docker-compose.yml`:

```yaml
services:
  toki-sync-server:
    ports:
      - "9091:9091"
      - "9090:9090"
    networks:
      - internal
      - external
```

---

## Step 3: Connect a device

Clients must use the `--insecure` flag to accept the self-signed certificate:

```bash
toki settings sync enable --server 192.168.1.100 --insecure
toki settings sync status
```
