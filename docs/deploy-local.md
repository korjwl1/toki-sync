# Scenario D: local / LAN (no TLS)

For development or testing on localhost. Not recommended for production.

> Different setup? See [Caddy + DuckDNS](deploy-caddy-duckdns.md) for production with auto TLS, [existing reverse proxy](deploy-reverse-proxy.md) if you already run one, or [self-signed TLS](deploy-self-signed.md) for IP-only servers.

---

## Prerequisites

- **Docker** and **Docker Compose v2**

---

## Step 1: clone and configure

```bash
git clone https://github.com/korjwl1/toki-sync.git
cd toki-sync

cp .env.example .env
cp config/toki-sync.toml.example config/toki-sync.toml
```

Edit `.env` for local use:

```bash
TOKI_ADMIN_PASSWORD=dev-password
JWT_SECRET=dev-secret-change-in-production
TOKI_EXTERNAL_URL=http://localhost:9091
```

---

## Step 2: expose ports

Add port mappings to `docker-compose.yml` under `toki-sync-server`:

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

## Step 3: deploy

```bash
docker compose up -d
```

---

## Step 4: connect

```bash
toki settings sync enable --server localhost --no-tls
toki settings sync status
```
