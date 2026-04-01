# Scenario B: Existing Reverse Proxy

For servers that already have nginx, Traefik, or another proxy handling TLS.

---

## Prerequisites

- **Docker** and **Docker Compose v2** installed on your server
- An existing reverse proxy with valid TLS certificates
- A domain name pointing to your server

---

## Step 1: Clone and configure

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
TOKI_EXTERNAL_URL=https://yourserver.example.com
# DUCKDNS_TOKEN is not needed — your proxy handles TLS
```

---

## Step 2: Expose ports

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

## Step 3: Deploy

```bash
docker compose up -d
```

This starts only toki-sync-server with the embedded Fjall event store (no Caddy).

---

## Step 4: Configure your proxy

Forward two types of traffic to toki-sync:

| Traffic | Upstream |
|---------|----------|
| HTTPS :443 (HTTP) | `http://127.0.0.1:9091` |
| TLS :9090 (TCP stream) | `127.0.0.1:9090` |

### nginx

```nginx
# HTTP API + dashboard
server {
    listen 443 ssl;
    server_name yourserver.example.com;

    ssl_certificate     /path/to/cert.pem;
    ssl_certificate_key /path/to/key.pem;

    location / {
        proxy_pass http://127.0.0.1:9091;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
    }
}

# TCP sync protocol (requires nginx stream module)
stream {
    server {
        listen 9090 ssl;
        ssl_certificate     /path/to/cert.pem;
        ssl_certificate_key /path/to/key.pem;
        proxy_pass 127.0.0.1:9090;
    }
}
```

### Traefik

```yaml
# traefik dynamic config
http:
  routers:
    toki-sync-http:
      rule: "Host(`yourserver.example.com`)"
      service: toki-sync-http
      tls:
        certResolver: letsencrypt
  services:
    toki-sync-http:
      loadBalancer:
        servers:
          - url: "http://127.0.0.1:9091"

tcp:
  routers:
    toki-sync-tcp:
      rule: "HostSNI(`yourserver.example.com`)"
      service: toki-sync-tcp
      tls:
        certResolver: letsencrypt
  services:
    toki-sync-tcp:
      loadBalancer:
        servers:
          - address: "127.0.0.1:9090"
```

---

## Step 5: Connect a device

On any machine with [toki](https://github.com/korjwl1/toki) installed:

```bash
toki settings sync enable --server yourserver.example.com
toki settings sync status
```
