# Scenario D: Local / LAN (No TLS)

For development or testing on localhost. Not recommended for production.

---

## Prerequisites

- **Docker** and **Docker Compose v2**

---

## Step 1: Clone and configure

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

---

## Step 4: Connect

```bash
toki settings sync enable --server localhost --no-tls
toki settings sync status
```
