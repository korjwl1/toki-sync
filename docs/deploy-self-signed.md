# Scenario C: self-signed TLS (IP-only)

For servers without a domain name (e.g., home lab on a local IP). Caddy generates a self-signed certificate automatically.

> Picking the wrong scenario? See [Caddy + DuckDNS](deploy-caddy-duckdns.md) for free domain + Let's Encrypt, [existing reverse proxy](deploy-reverse-proxy.md) if you already run nginx/Traefik, or [local / LAN](deploy-local.md) for development.

---

## Prerequisites

- **Docker** and **Docker Compose v2** installed on your server
- A server with a known IP address (public or LAN)

---

## Step 1: clone and configure

```bash
git clone https://github.com/korjwl1/toki-sync.git
cd toki-sync

cp .env.example .env
cp config/toki-sync.toml.example config/toki-sync.toml
```

Edit `.env` — leave `DUCKDNS_TOKEN` and `TOKI_DOMAIN` unset so Caddy falls back to its self-signed default:

```bash
TOKI_ADMIN_PASSWORD=your-strong-password
JWT_SECRET=$(openssl rand -base64 32)
TOKI_EXTERNAL_URL=https://192.168.1.100
# DUCKDNS_TOKEN is not set
# TOKI_DOMAIN is not set — Caddy uses tls internal (self-signed)
```

The bundled `caddy/Caddyfile` already supports this mode. The relevant lines are:

```caddyfile
{$TOKI_DOMAIN:localhost} {
    tls {$TLS_MODE:internal}

    reverse_proxy toki-sync-server:9091
}
```

When `TOKI_DOMAIN` is unset, the site address defaults to `localhost`, and `TLS_MODE` defaults to `internal` — Caddy generates and serves a self-signed certificate. No edits to the Caddyfile are needed.

---

## Step 2: deploy

**With Caddy** (self-signed mode):

```bash
docker compose --profile caddy up -d
```

**Without Caddy** (expose ports directly):

```bash
docker compose up -d
```

If you skip Caddy, add port mappings to `docker-compose.yml`:

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

## Step 3: connect a device

Clients must use the `--insecure` flag to accept the self-signed certificate:

```bash
toki settings sync enable --server 192.168.1.100 --insecure
toki settings sync status
```
