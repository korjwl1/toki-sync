# Backup and restore

toki-sync state lives in two Docker volumes: `toki-data` (metadata + Fjall events) and, optionally, `clickhouse-data`. Cold backups (with the server stopped) are simple `tar` archives; data loss is recoverable because clients keep a local event history and will re-sync on reconnect.

## Data volumes

| Volume | Path | Contents | On loss |
|---|---|---|---|
| `toki-data` | `/data` | SQLite (users, devices, cursors) + Fjall event store | Re-login + full re-sync required |
| `clickhouse-data` | `/var/lib/clickhouse` | ClickHouse event data (only with `--profile clickhouse`) | Recoverable via client re-sync |
| `caddy-data` | `/data` | Let's Encrypt certificates | Auto-reissue (Let's Encrypt rate limit: 5/week) |

With the default Fjall backend, `toki-data` contains both metadata and events. With ClickHouse, event data is stored separately in `clickhouse-data`. If lost, clients perform a full re-sync from their local history on reconnect.

---

## Bind mounts (recommended for backups)

For easier backup access, use bind mounts instead of named volumes in `docker-compose.yml`:

```yaml
volumes:
  - ./data/toki:/data
```

---

## Cold backup with `tar`

This works for both the default Fjall backend and a manual file backup of bind mounts.

> The example uses the `caddy` profile. Adjust the `up` command for your deployment:
> - Local / reverse proxy: `docker compose up -d`
> - Self-signed (Caddy): `docker compose --profile caddy up -d`

```bash
# Stop containers to ensure consistency
docker compose down

# Archive the data directory
tar czf toki-sync-backup-$(date +%Y%m%d).tar.gz ./data/

# Restart (adjust profile flag for your deployment)
docker compose --profile caddy up -d
```

For a hot backup while the server runs, copy the Fjall directory directly. Fjall uses an LSM-tree structure that is safe to copy live, but stopping the server is the only way to guarantee full consistency.

---

## ClickHouse backup (optional backend)

When using ClickHouse as the event store:

```bash
# Use clickhouse-backup tool
docker exec toki-clickhouse clickhouse-backup create backup_$(date +%Y%m%d)

# Or use clickhouse-client to export
docker exec toki-clickhouse clickhouse-client \
  --query "SELECT * FROM events FORMAT Native" > events_backup.bin
```

See the [ClickHouse backup documentation](https://clickhouse.com/docs/en/operations/backup) for full details.

---

## VM / VPS disk snapshots

The simplest approach for small deployments:

1. Stop containers: `docker compose down`.
2. Snapshot the entire VM/VPS disk via your cloud provider's console.
3. Restart: `docker compose --profile caddy up -d` (adjust profile for your deployment).

This captures everything — database, event store, and certificates.

---

## Restore

1. Stop containers: `docker compose down`.
2. Replace the data directories with your backup.
3. Restart: `docker compose --profile caddy up -d` (adjust profile for your deployment).
4. Clients reconnect automatically (the toki daemon retries with exponential backoff).
