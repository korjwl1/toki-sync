# Backup and restore

toki-sync state lives in `toki-data`, optionally `clickhouse-data`, and optionally
`caddy-data`. Clients keep local event history, but event-store loss does **not**
automatically cause a full upload when the relational cursor survives. Back up
the event store and cursor database as one recovery set.

## Data volumes

| Volume | Path | Contents | On loss |
|---|---|---|---|
| `toki-data` | `/data` | SQLite (users, devices, cursors) + Fjall event store | Re-login + full re-sync required |
| `clickhouse-data` | `/var/lib/clickhouse` | ClickHouse events/windows when configured as the backend | Requires cursor reset/full re-sync if restored empty while relational cursors survive |
| `caddy-data` | `/data` | Caddy internal CA/certificate state in the bundled configuration | Clients may need their trust configuration updated |

With Fjall, restore metadata/cursors and events together. With ClickHouse,
restore `toki-data` and `clickhouse-data` from a consistent point. Starting the
ClickHouse profile alone does not select it; `[events].backend` must also be
`clickhouse`.

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

This repository has no tested hot-backup procedure spanning SQLite/Fjall or
PostgreSQL/ClickHouse. Use database-supported coordinated snapshots or stop
writers for a cold backup.

---

## ClickHouse backup (optional backend)

When using ClickHouse as the event store:

```bash
# If clickhouse-backup was installed separately
docker exec toki-clickhouse clickhouse-backup create backup_$(date +%Y%m%d)

# Or export both tables
docker exec toki-clickhouse clickhouse-client \
  --query "SELECT * FROM toki_events FORMAT Native" > toki_events_backup.bin
docker exec toki-clickhouse clickhouse-client \
  --query "SELECT * FROM toki_windows FINAL FORMAT Native" > toki_windows_backup.bin
```

Verify/install `clickhouse-backup` before using the first command. Raw exports
also require a tested schema-and-data restore procedure.

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
