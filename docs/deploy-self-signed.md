# Scenario C: self-signed TLS (IP-only)

For servers without a domain name (e.g., home lab on a local IP). Caddy generates a self-signed certificate automatically.

> For trusted public TLS use an [existing reverse proxy](deploy-reverse-proxy.md).
> The bundled [DuckDNS scenario](deploy-caddy-duckdns.md) is not turnkey public
> TLS in this revision.

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

Edit `.env`. Compose passes `TOKI_EXTERNAL_URL` to Caddy as `TOKI_DOMAIN`; the
Caddyfile's default `TLS_MODE=internal` makes the resulting certificate
internal-CA/self-signed:

```bash
TOKI_ADMIN_PASSWORD=your-strong-password
JWT_SECRET=$(openssl rand -base64 32)
TOKI_EXTERNAL_URL=https://192.168.1.100
# DUCKDNS_TOKEN is not set
# TOKI_DOMAIN is populated from TOKI_EXTERNAL_URL by Compose
```

The bundled `caddy/Caddyfile` already supports this mode. The relevant lines are:

```caddyfile
{$TOKI_DOMAIN:localhost} {
    tls {$TLS_MODE:internal}

    reverse_proxy toki-sync-server:9091
}
```

Because `TLS_MODE` is unset, it defaults to `internal`; Caddy generates an
internal-CA certificate for the configured IP/host. `DUCKDNS_TOKEN` has no
effect in this revision.

---

## Step 2: deploy

**With Caddy** (self-signed mode):

```bash
docker compose pull toki-sync-server
docker compose build caddy
docker compose --profile caddy up -d --no-build
```

**Without Caddy** (expose ports directly):

```bash
docker compose pull toki-sync-server
docker compose up -d --no-build
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
